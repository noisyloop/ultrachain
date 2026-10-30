//! The trading loop: ingest signals, price swaps through Jupiter, enforce
//! risk limits, fill (on paper, or live via `LiveExecutor`), log everything.

use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::config::{Config, Token};
use crate::jupiter::{Quote, Quoter};
use crate::ledger::{Event, Ledger};
use crate::live::LiveExecutor;
use crate::notify::Notifier;
use crate::risk::{self, ExitReason};
use crate::signal::{Side, Signal, SignalSource};
use crate::state::{Position, State};

const EQUITY_SNAPSHOT_SECS: i64 = 300;

pub struct Engine<Q, S> {
    cfg: Config,
    quoter: Q,
    source: S,
    pub state: State,
    ledger: Ledger,
    state_path: PathBuf,
    last_snapshot: Option<DateTime<Utc>>,
    notifier: Notifier,
    live: Option<LiveExecutor>,
    /// Some position's price is older than `max_mark_staleness_secs`.
    quotes_stale: bool,
    benchmark_price: Option<f64>,
}

impl<Q: Quoter, S: SignalSource> Engine<Q, S> {
    pub fn new(cfg: Config, quoter: Q, source: S, now: DateTime<Utc>) -> Result<Self> {
        std::fs::create_dir_all(&cfg.data_dir)?;
        let state_path = cfg.data_dir.join("state.json");
        let state = State::load_or_new(&state_path, cfg.starting_balance, now)?;
        let ledger = Ledger::open(&cfg.data_dir.join("ledger.jsonl"))?;
        tracing::info!(
            cash = state.cash,
            positions = state.positions.len(),
            ledger = %ledger.path().display(),
            "paper engine ready"
        );
        Ok(Self {
            cfg,
            quoter,
            source,
            state,
            ledger,
            state_path,
            last_snapshot: None,
            notifier: Notifier::disabled(),
            live: None,
            quotes_stale: false,
            benchmark_price: None,
        })
    }

    pub fn with_notifier(mut self, n: Notifier) -> Self {
        self.notifier = n;
        self
    }

    pub fn with_live(mut self, live: LiveExecutor) -> Self {
        self.live = Some(live);
        self
    }

    fn mode_label(&self) -> &'static str {
        if self.live.is_some() { "LIVE" } else { "PAPER" }
    }

    pub async fn tick(&mut self, now: DateTime<Utc>) -> Result<()> {
        if self.state.roll_day(now) {
            tracing::info!(day = %self.state.day, equity = self.state.equity(), "new UTC day");
        }

        // Refresh the benchmark first so every snapshot this tick has it.
        if self.benchmark_price.is_none() || self.snapshot_due(now) {
            self.refresh_benchmark().await;
        }

        // 1. Re-mark open positions and enforce stop loss / take profit.
        self.mark_and_exit(now).await?;

        // 2. Daily loss cap on realized + unrealized PnL.
        self.enforce_daily_cap(now).await?;

        // 3. New signals.
        match self.source.poll().await {
            Ok(signals) => {
                for sig in signals {
                    if self.state.seen.insert(&sig.id) {
                        self.handle_signal(sig, now).await?;
                    }
                }
            }
            Err(e) => tracing::warn!("signal poll failed: {e:#}"),
        }

        self.check_staleness(now).await;
        self.snapshot(now, false)?;
        self.state.save(&self.state_path)?;
        Ok(())
    }

    async fn mark_and_exit(&mut self, now: DateTime<Utc>) -> Result<()> {
        let mints: Vec<String> = self.state.positions.keys().cloned().collect();
        for mint in mints {
            let pos = &self.state.positions[&mint];
            let quote_mint = self.cfg.quote_token.mint.clone();
            match self.quoter.quote(&mint, &quote_mint, pos.qty).await {
                Ok(q) => {
                    let value = self.cfg.quote_token.to_ui(q.out_amount);
                    let pos = self.state.positions.get_mut(&mint).unwrap();
                    pos.last_value = value;
                    pos.last_marked = Some(now);
                    if let Some(reason) = risk::exit_trigger(pos, value, &self.cfg.risk) {
                        self.close(&mint, reason, now).await?;
                    }
                }
                // Keep the previous mark. Note the stop can't fire while
                // quotes are failing; this is logged loudly.
                Err(e) => tracing::warn!(%mint, "failed to mark position: {e:#}"),
            }
        }
        Ok(())
    }

    async fn enforce_daily_cap(&mut self, now: DateTime<Utc>) -> Result<()> {
        let day_pnl = self.state.day_pnl();
        if self.state.halted || !risk::daily_cap_hit(day_pnl, &self.cfg.risk) {
            return Ok(());
        }
        self.state.halted = true;
        self.notifier
            .alert(&format!(
                "Daily loss cap hit ({day_pnl:.2} <= -{:.2}); entries halted until 00:00 UTC",
                self.cfg.risk.daily_loss_cap
            ))
            .await;
        self.ledger.record(&Event::Halt {
            ts: now,
            day_pnl,
            cap: self.cfg.risk.daily_loss_cap,
        })?;
        if self.cfg.risk.flatten_on_daily_cap {
            let mints: Vec<String> = self.state.positions.keys().cloned().collect();
            for mint in mints {
                self.close(&mint, ExitReason::DailyCap, now).await?;
            }
        }
        self.snapshot(now, true)
    }

    async fn handle_signal(&mut self, sig: Signal, now: DateTime<Utc>) -> Result<()> {
        let age = (now - sig.ts).num_seconds();
        if age > self.cfg.risk.max_signal_age_secs {
            return self.reject(&sig, now, format!("stale signal ({age}s old)"));
        }
        let Some(token) = self.cfg.resolve_token(&sig.token).cloned() else {
            return self.reject(&sig, now, "unknown token".into());
        };
        match sig.side {
            Side::Buy => self.open(&sig, &token, now).await,
            Side::Sell => {
                if self.state.positions.contains_key(&token.mint) {
                    self.close(&token.mint, ExitReason::Signal, now).await
                } else {
                    self.reject(&sig, now, "sell signal but no position".into())
                }
            }
        }
    }

    async fn open(&mut self, sig: &Signal, token: &Token, now: DateTime<Utc>) -> Result<()> {
        if self.quotes_stale {
            return self.reject(sig, now, "prices stale; entries paused".into());
        }
        let size = match risk::entry_size(&self.state, &self.cfg.risk, &token.mint, sig.strength) {
            Ok(s) => s,
            Err(r) => return self.reject(sig, now, r.to_string()),
        };
        let qt = self.cfg.quote_token.clone();
        let amount = qt.to_base_units(size);
        let q = match self.quoter.quote(&qt.mint, &token.mint, amount).await {
            Ok(q) => q,
            Err(e) => return self.reject(sig, now, format!("quote failed: {e:#}")),
        };
        if let Err(r) = risk::check_price_impact(&self.cfg.risk, q.price_impact_pct) {
            return self.reject(sig, now, r.to_string());
        }
        if q.out_amount == 0 {
            return self.reject(sig, now, "quote returned zero output".into());
        }
        let out_amount = match self.fill(&q).await {
            Ok(out) => out,
            Err(reason) => return self.reject(sig, now, reason),
        };

        let spend = qt.to_ui(q.in_amount);
        let fee = self.cfg.risk.est_fee_per_swap;
        let qty_ui = token.to_ui(out_amount);
        self.state.cash -= spend + fee;
        self.state.positions.insert(
            token.mint.clone(),
            Position {
                symbol: token.symbol.clone(),
                mint: token.mint.clone(),
                qty: out_amount,
                cost: spend + fee,
                opened_at: now,
                signal_id: sig.id.clone(),
                last_value: spend,
                last_marked: Some(now),
            },
        );
        tracing::info!(mode = self.mode_label(), symbol = %token.symbol, spend, qty = qty_ui, route = %q.route, "BUY");
        self.ledger.record(&Event::Open {
            ts: now,
            signal_id: sig.id.clone(),
            symbol: token.symbol.clone(),
            mint: token.mint.clone(),
            spend,
            fee,
            qty: qty_ui,
            price: spend / qty_ui,
            price_impact_pct: q.price_impact_pct,
            route: q.route,
        })?;
        self.snapshot(now, true)
    }

    /// Exits are never blocked by risk checks: getting out always wins.
    async fn close(&mut self, mint: &str, reason: ExitReason, now: DateTime<Utc>) -> Result<()> {
        let Some(pos) = self.state.positions.get(mint).cloned() else {
            return Ok(());
        };
        let qt = self.cfg.quote_token.clone();
        let q = match self.quoter.quote(mint, &qt.mint, pos.qty).await {
            Ok(q) => q,
            Err(e) => {
                // Position stays open; we'll retry next tick.
                tracing::error!(symbol = %pos.symbol, ?reason, "exit quote failed: {e:#}");
                return Ok(());
            }
        };
        let out_amount = match self.fill(&q).await {
            Ok(out) => out,
            Err(reason) => {
                tracing::error!(symbol = %pos.symbol, ?reason, "exit fill failed; retrying next tick: {reason}");
                return Ok(());
            }
        };
        let fee = self.cfg.risk.est_fee_per_swap;
        let proceeds = qt.to_ui(out_amount) - fee;
        let pnl = proceeds - pos.cost;
        let qty_ui = self
            .cfg
            .token_by_mint(mint)
            .map(|t| t.to_ui(pos.qty))
            .unwrap_or(pos.qty as f64);
        self.state.cash += proceeds;
        self.state.positions.remove(mint);
        tracing::info!(mode = self.mode_label(), symbol = %pos.symbol, ?reason, pnl, "SELL");
        self.ledger.record(&Event::Close {
            ts: now,
            symbol: pos.symbol.clone(),
            mint: mint.to_string(),
            reason: serde_json::to_value(reason)?
                .as_str()
                .unwrap_or("?")
                .to_string(),
            qty: qty_ui,
            proceeds,
            fee,
            cost: pos.cost,
            pnl,
            pnl_pct: pnl / pos.cost * 100.0,
            price: (proceeds + fee) / qty_ui,
            price_impact_pct: q.price_impact_pct,
            held_secs: (now - pos.opened_at).num_seconds(),
            route: q.route,
        })?;
        self.snapshot(now, true)
    }

    fn reject(&mut self, sig: &Signal, now: DateTime<Utc>, reason: String) -> Result<()> {
        tracing::info!(id = %sig.id, token = %sig.token, %reason, "signal rejected");
        self.ledger.record(&Event::Reject {
            ts: now,
            signal_id: sig.id.clone(),
            token: sig.token.clone(),
            side: format!("{:?}", sig.side).to_lowercase(),
            reason,
        })
    }

    /// Fill a quoted swap. Paper: maybe fail, then shave `haircut_bps`.
    /// Live: submit it and wait for confirmation. Returns the output amount.
    async fn fill(&mut self, q: &Quote) -> Result<u64, String> {
        if let Some(live) = &self.live {
            return match live.execute(q).await {
                // TODO: use the actual balance change, not the quote.
                Ok(sig) => {
                    tracing::info!(%sig, "live swap confirmed");
                    Ok(q.out_amount)
                }
                Err(e) => {
                    let msg = format!("live swap failed: {e:#}");
                    self.notifier.alert(&msg).await;
                    Err(msg)
                }
            };
        }
        let p = &self.cfg.paper;
        if p.fail_rate > 0.0 && rand::random::<f64>() < p.fail_rate {
            return Err("simulated transaction failure".into());
        }
        let keep = 1.0 - p.haircut_bps as f64 / 10_000.0;
        Ok((q.out_amount as f64 * keep) as u64)
    }

    /// Pause entries (and alert once) while any position's price is stale,
    /// because its stop loss can't fire until quotes work again.
    async fn check_staleness(&mut self, now: DateTime<Utc>) {
        let limit = self.cfg.risk.max_mark_staleness_secs;
        let stale: Vec<String> = self
            .state
            .positions
            .values()
            .filter(|p| (now - p.last_marked.unwrap_or(p.opened_at)).num_seconds() > limit)
            .map(|p| p.symbol.clone())
            .collect();
        match (stale.is_empty(), self.quotes_stale) {
            (false, false) => {
                self.quotes_stale = true;
                self.notifier
                    .alert(&format!(
                        "No price for {} in over {limit}s; stop losses can't fire. New entries paused.",
                        stale.join(", ")
                    ))
                    .await;
            }
            (true, true) => {
                self.quotes_stale = false;
                self.notifier
                    .alert("Prices are fresh again; entries resumed.")
                    .await;
            }
            _ => {}
        }
    }

    async fn refresh_benchmark(&mut self) {
        let Some(token) = self
            .cfg
            .benchmark
            .as_ref()
            .and_then(|b| self.cfg.resolve_token(b))
        else {
            return;
        };
        let (mint, one) = (token.mint.clone(), token.to_base_units(1.0));
        let qt = self.cfg.quote_token.clone();
        match self.quoter.quote(&mint, &qt.mint, one).await {
            Ok(q) => self.benchmark_price = Some(qt.to_ui(q.out_amount)),
            Err(e) => tracing::warn!("benchmark quote failed: {e:#}"),
        }
    }

    fn snapshot_due(&self, now: DateTime<Utc>) -> bool {
        self.last_snapshot
            .is_none_or(|t| (now - t).num_seconds() >= EQUITY_SNAPSHOT_SECS)
    }

    fn snapshot(&mut self, now: DateTime<Utc>, force: bool) -> Result<()> {
        let due = self.snapshot_due(now);
        if !force && !due {
            return Ok(());
        }
        self.last_snapshot = Some(now);
        self.ledger.record(&Event::Equity {
            ts: now,
            cash: self.state.cash,
            equity: self.state.equity(),
            day_pnl: self.state.day_pnl(),
            open_positions: self.state.positions.len(),
            benchmark_price: self.benchmark_price,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use crate::jupiter::Quote;
    use crate::ledger;
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};

    const USDC: &str = "USDC_MINT";
    const BONK: &str = "BONK_MINT";

    /// Prices are quote-token per UI token; no impact.
    struct MockQuoter {
        prices: RefCell<HashMap<String, f64>>,
        down: std::cell::Cell<bool>,
    }

    impl MockQuoter {
        fn set(&self, mint: &str, price: f64) {
            self.prices.borrow_mut().insert(mint.into(), price);
        }
    }

    impl Quoter for &MockQuoter {
        async fn quote(&self, input: &str, output: &str, amount: u64) -> Result<Quote> {
            anyhow::ensure!(!self.down.get(), "quotes down");
            let p = self.prices.borrow();
            // USDC has 6 decimals, BONK has 5 in this test.
            let out = if input == USDC {
                (amount as f64 / 1e6 / p[output] * 1e5) as u64
            } else {
                (amount as f64 / 1e5 * p[input] * 1e6) as u64
            };
            Ok(Quote {
                in_amount: amount,
                out_amount: out,
                price_impact_pct: 0.1,
                route: "Mock".into(),
                raw: serde_json::Value::Null,
            })
        }
    }

    struct QueueSource(VecDeque<Vec<Signal>>);

    impl SignalSource for QueueSource {
        async fn poll(&mut self) -> Result<Vec<Signal>> {
            Ok(self.0.pop_front().unwrap_or_default())
        }
    }

    fn cfg(dir: &std::path::Path) -> Config {
        Config {
            mode: Mode::Paper,
            tick_secs: 1,
            data_dir: dir.to_path_buf(),
            starting_balance: 1_000.0,
            quote_token: Token {
                symbol: "USDC".into(),
                mint: USDC.into(),
                decimals: 6,
            },
            tokens: vec![Token {
                symbol: "BONK".into(),
                mint: BONK.into(),
                decimals: 5,
            }],
            jupiter: JupiterConfig {
                base_url: String::new(),
                api_key: None,
                slippage_bps: 50,
                timeout_secs: 1,
            },
            signals: SignalConfig::Http { url: String::new() },
            risk: RiskConfig {
                max_position_size: 100.0,
                max_open_positions: 3,
                stop_loss_pct: 10.0,
                take_profit_pct: None,
                daily_loss_cap: 30.0,
                flatten_on_daily_cap: true,
                max_price_impact_pct: 1.0,
                max_signal_age_secs: 60,
                max_mark_staleness_secs: 120,
                est_fee_per_swap: 0.0,
            },
            benchmark: None,
            paper: PaperConfig::default(),
            alerts: AlertConfig::default(),
            live: None,
        }
    }

    fn sig(id: &str, side: Side, ts: DateTime<Utc>) -> Signal {
        Signal {
            id: id.into(),
            ts,
            token: "BONK".into(),
            side,
            strength: None,
        }
    }

    #[tokio::test]
    async fn buy_then_stop_loss_then_halt() {
        let dir = tempfile::tempdir().unwrap();
        let q = MockQuoter {
            prices: RefCell::new(HashMap::new()),
            down: Default::default(),
        };
        q.set(BONK, 0.5);
        let t0 = Utc::now();
        let src = QueueSource(VecDeque::from(vec![
            vec![sig("s1", Side::Buy, t0)],
            vec![],
            vec![sig("s1", Side::Buy, t0)], // duplicate: ignored
        ]));
        let mut eng = Engine::new(cfg(dir.path()), &q, src, t0).unwrap();

        eng.tick(t0).await.unwrap();
        assert_eq!(eng.state.positions.len(), 1);
        assert!((eng.state.cash - 900.0).abs() < 1e-6);

        // Price drops 35%: stop loss closes for -35, which also breaches the
        // 30 daily cap.
        q.set(BONK, 0.325);
        eng.tick(t0 + chrono::Duration::seconds(1)).await.unwrap();
        assert!(eng.state.positions.is_empty());
        assert!(eng.state.halted);
        assert!((eng.state.cash - 965.0).abs() < 1e-3);

        eng.tick(t0 + chrono::Duration::seconds(2)).await.unwrap();
        assert!(eng.state.positions.is_empty());

        let events = ledger::read_all(&dir.path().join("ledger.jsonl")).unwrap();
        let close = events.iter().find_map(|e| match e {
            Event::Close { reason, pnl, .. } => Some((reason.clone(), *pnl)),
            _ => None,
        });
        let (reason, pnl) = close.unwrap();
        assert_eq!(reason, "stop_loss");
        assert!((pnl + 35.0).abs() < 1e-3);
        assert!(events.iter().any(|e| matches!(e, Event::Halt { .. })));
    }

    #[tokio::test]
    async fn rejects_stale_unknown_and_unheld_sells() {
        let dir = tempfile::tempdir().unwrap();
        let q = MockQuoter {
            prices: RefCell::new(HashMap::new()),
            down: Default::default(),
        };
        q.set(BONK, 1.0);
        let t0 = Utc::now();
        let mut unknown = sig("s3", Side::Buy, t0);
        unknown.token = "NOPE".into();
        let src = QueueSource(VecDeque::from(vec![vec![
            sig("s1", Side::Buy, t0 - chrono::Duration::seconds(600)),
            sig("s2", Side::Sell, t0),
            unknown,
        ]]));
        let mut eng = Engine::new(cfg(dir.path()), &q, src, t0).unwrap();
        eng.tick(t0).await.unwrap();
        assert!(eng.state.positions.is_empty());
        let events = ledger::read_all(&dir.path().join("ledger.jsonl")).unwrap();
        let rejects = events
            .iter()
            .filter(|e| matches!(e, Event::Reject { .. }))
            .count();
        assert_eq!(rejects, 3);
    }

    #[tokio::test]
    async fn sell_signal_closes_with_profit_and_state_persists() {
        let dir = tempfile::tempdir().unwrap();
        let q = MockQuoter {
            prices: RefCell::new(HashMap::new()),
            down: Default::default(),
        };
        q.set(BONK, 1.0);
        let t0 = Utc::now();
        let src = QueueSource(VecDeque::from(vec![
            vec![sig("b", Side::Buy, t0)],
            vec![sig("s", Side::Sell, t0 + chrono::Duration::seconds(5))],
        ]));
        let mut eng = Engine::new(cfg(dir.path()), &q, src, t0).unwrap();
        eng.tick(t0).await.unwrap();
        q.set(BONK, 1.1);
        eng.tick(t0 + chrono::Duration::seconds(5)).await.unwrap();
        assert!(eng.state.positions.is_empty());
        assert!((eng.state.cash - 1_010.0).abs() < 1e-3);
        drop(eng);

        let reloaded = State::load_or_new(&dir.path().join("state.json"), 0.0, t0).unwrap();
        assert!((reloaded.cash - 1_010.0).abs() < 1e-3);
    }

    #[tokio::test]
    async fn stale_prices_pause_entries_until_quotes_recover() {
        let dir = tempfile::tempdir().unwrap();
        let q = MockQuoter {
            prices: RefCell::new(HashMap::new()),
            down: Default::default(),
        };
        q.set(BONK, 1.0);
        let t0 = Utc::now();
        let mut other = sig("s2", Side::Buy, t0 + chrono::Duration::seconds(200));
        other.token = "BONK".into();
        let src = QueueSource(VecDeque::from(vec![vec![sig("s1", Side::Buy, t0)], vec![]]));
        let mut eng = Engine::new(cfg(dir.path()), &q, src, t0).unwrap();
        eng.tick(t0).await.unwrap();
        assert_eq!(eng.state.positions.len(), 1);

        q.down.set(true);
        eng.tick(t0 + chrono::Duration::seconds(200)).await.unwrap();
        assert!(eng.quotes_stale);
        eng.state.positions.clear();
        assert!(
            eng.open(&other, &cfg(dir.path()).tokens[0], t0)
                .await
                .is_ok()
        );
        assert!(
            eng.state.positions.is_empty(),
            "entry must be refused while stale"
        );

        q.down.set(false);
        eng.check_staleness(t0 + chrono::Duration::seconds(201))
            .await;
        assert!(!eng.quotes_stale);
    }

    #[tokio::test]
    async fn paper_haircut_reduces_fills() {
        let dir = tempfile::tempdir().unwrap();
        let q = MockQuoter {
            prices: RefCell::new(HashMap::new()),
            down: Default::default(),
        };
        q.set(BONK, 1.0);
        let mut c = cfg(dir.path());
        c.paper.haircut_bps = 100;
        let t0 = Utc::now();
        let src = QueueSource(VecDeque::from(vec![vec![sig("s1", Side::Buy, t0)]]));
        let mut eng = Engine::new(c, &q, src, t0).unwrap();
        eng.tick(t0).await.unwrap();
        // 100 USDC at 1.0 = 100 BONK = 10_000_000 base units, minus 1%.
        assert_eq!(eng.state.positions[BONK].qty, 9_900_000);
    }
}
