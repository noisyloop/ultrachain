//! Summarize a paper-trading ledger: does the strategy actually make money?

use std::collections::BTreeMap;
use std::fmt::Write;

use chrono::NaiveDate;

use crate::ledger::Event;

#[derive(Debug, Default)]
pub struct Summary {
    pub trades: usize,
    pub wins: usize,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub fees: f64,
    pub by_reason: BTreeMap<String, (usize, f64)>,
    pub by_day: BTreeMap<NaiveDate, f64>,
    pub rejects: BTreeMap<String, usize>,
    pub halts: usize,
    pub start_equity: Option<f64>,
    pub end_equity: Option<f64>,
    pub max_drawdown: f64,
    pub max_drawdown_pct: f64,
    pub first_ts: Option<chrono::DateTime<chrono::Utc>>,
    pub last_ts: Option<chrono::DateTime<chrono::Utc>>,
    pub open_events: usize,
    pub by_token: BTreeMap<String, (usize, f64)>,
    /// Benchmark token price at the first and last snapshot that had one.
    pub bench_start: Option<f64>,
    pub bench_end: Option<f64>,
}

impl Summary {
    /// Buy-and-hold return of the benchmark over the run, in %.
    pub fn bench_return_pct(&self) -> Option<f64> {
        Some((self.bench_end? - self.bench_start?) / self.bench_start? * 100.0)
    }

    pub fn return_pct(&self) -> Option<f64> {
        Some((self.end_equity? - self.start_equity?) / self.start_equity? * 100.0)
    }
}

pub fn summarize(events: &[Event]) -> Summary {
    let mut s = Summary::default();
    let mut peak = f64::MIN;
    for ev in events {
        match ev {
            Event::Open { fee, .. } => {
                s.open_events += 1;
                s.fees += fee;
            }
            Event::Close {
                ts,
                symbol,
                reason,
                pnl,
                fee,
                ..
            } => {
                let t = s.by_token.entry(symbol.clone()).or_default();
                t.0 += 1;
                t.1 += pnl;
                s.trades += 1;
                s.fees += fee;
                if *pnl > 0.0 {
                    s.wins += 1;
                    s.gross_profit += pnl;
                } else {
                    s.gross_loss += -pnl;
                }
                let r = s.by_reason.entry(reason.clone()).or_default();
                r.0 += 1;
                r.1 += pnl;
                *s.by_day.entry(ts.date_naive()).or_default() += pnl;
            }
            Event::Reject { reason, .. } => {
                // Group "quote failed: <details>" etc. by their prefix.
                let key = reason
                    .split([':', '('])
                    .next()
                    .unwrap_or(reason)
                    .trim()
                    .to_string();
                *s.rejects.entry(key).or_default() += 1;
            }
            Event::Halt { .. } => s.halts += 1,
            Event::Equity {
                ts,
                equity,
                benchmark_price,
                ..
            } => {
                if let Some(b) = benchmark_price {
                    s.bench_start.get_or_insert(*b);
                    s.bench_end = Some(*b);
                }
                s.start_equity.get_or_insert(*equity);
                s.end_equity = Some(*equity);
                s.first_ts.get_or_insert(*ts);
                s.last_ts = Some(*ts);
                peak = peak.max(*equity);
                let dd = peak - equity;
                if dd > s.max_drawdown {
                    s.max_drawdown = dd;
                    s.max_drawdown_pct = dd / peak * 100.0;
                }
            }
        }
    }
    s
}

pub fn render(s: &Summary) -> String {
    let mut o = String::new();
    let net = s.gross_profit - s.gross_loss;
    let _ = writeln!(o, "=== Paper trading report ===");
    if let (Some(a), Some(b)) = (s.first_ts, s.last_ts) {
        let days = (b - a).num_hours() as f64 / 24.0;
        let _ = writeln!(
            o,
            "Period:          {} -> {} ({days:.1} days)",
            a.format("%F %R"),
            b.format("%F %R")
        );
    }
    if let (Some(start), Some(end)) = (s.start_equity, s.end_equity) {
        let _ = writeln!(
            o,
            "Equity:          {start:.2} -> {end:.2} ({:+.2}%)",
            (end - start) / start * 100.0
        );
    }
    if let (Some(b), Some(r)) = (s.bench_return_pct(), s.return_pct()) {
        let _ = writeln!(
            o,
            "Buy & hold:      {b:+.2}% for the benchmark vs {r:+.2}% for the strategy"
        );
    }
    let _ = writeln!(
        o,
        "Closed trades:   {} (opened {})",
        s.trades, s.open_events
    );
    if s.trades > 0 {
        let losses = s.trades - s.wins;
        let _ = writeln!(
            o,
            "Win rate:        {:.1}%",
            s.wins as f64 / s.trades as f64 * 100.0
        );
        let _ = writeln!(
            o,
            "Realized PnL:    {net:+.2} (fees included: {:.2})",
            s.fees
        );
        let _ = writeln!(
            o,
            "Expectancy:      {:+.3} per trade",
            net / s.trades as f64
        );
        if s.wins > 0 {
            let _ = writeln!(o, "Avg win:         {:+.2}", s.gross_profit / s.wins as f64);
        }
        if losses > 0 {
            let _ = writeln!(o, "Avg loss:        {:+.2}", -s.gross_loss / losses as f64);
        }
        if s.gross_loss > 0.0 {
            let _ = writeln!(o, "Profit factor:   {:.2}", s.gross_profit / s.gross_loss);
        }
    }
    let _ = writeln!(
        o,
        "Max drawdown:    {:.2} ({:.2}%)",
        s.max_drawdown, s.max_drawdown_pct
    );
    let _ = writeln!(o, "Daily-cap halts: {}", s.halts);

    if !s.by_reason.is_empty() {
        let _ = writeln!(o, "\nExits by reason:");
        for (r, (n, pnl)) in &s.by_reason {
            let _ = writeln!(o, "  {r:<12} {n:>5} trades  {pnl:+10.2}");
        }
    }
    if !s.by_token.is_empty() {
        let _ = writeln!(o, "\nBy token:");
        for (t, (n, pnl)) in &s.by_token {
            let _ = writeln!(o, "  {t:<12} {n:>5} trades  {pnl:+10.2}");
        }
    }
    if !s.by_day.is_empty() {
        let green = s.by_day.values().filter(|p| **p > 0.0).count();
        let _ = writeln!(
            o,
            "\nDaily realized PnL ({green}/{} green days):",
            s.by_day.len()
        );
        for (d, pnl) in &s.by_day {
            let _ = writeln!(o, "  {d}  {pnl:+10.2}");
        }
    }
    if !s.rejects.is_empty() {
        let _ = writeln!(o, "\nRejected signals:");
        for (r, n) in &s.rejects {
            let _ = writeln!(o, "  {n:>5}  {r}");
        }
    }

    let _ = writeln!(o, "\nVerdict: {}", verdict(s));
    o
}

pub fn verdict(s: &Summary) -> String {
    let net = s.gross_profit - s.gross_loss;
    if s.trades < 30 {
        return format!(
            "only {} closed trades - too few to tell skill from luck. Keep paper trading.",
            s.trades
        );
    }
    if net <= 0.0 {
        return "strategy lost money after fees. Do not go live.".into();
    }
    let pf = if s.gross_loss > 0.0 {
        s.gross_profit / s.gross_loss
    } else {
        f64::INFINITY
    };
    if pf < 1.2 {
        format!(
            "marginally profitable (profit factor {pf:.2}); real slippage and latency will likely erase it."
        )
    } else {
        format!(
            "profitable on paper (profit factor {pf:.2}). Paper fills are optimistic: \
             if you go live, start with a fraction of max_position_size."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn close(day: u32, pnl: f64, reason: &str) -> Event {
        Event::Close {
            ts: Utc.with_ymd_and_hms(2026, 10, day, 12, 0, 0).unwrap(),
            symbol: "X".into(),
            mint: "X".into(),
            reason: reason.into(),
            qty: 1.0,
            proceeds: 100.0 + pnl,
            fee: 0.01,
            cost: 100.0,
            pnl,
            pnl_pct: pnl,
            price: 1.0,
            price_impact_pct: 0.0,
            held_secs: 60,
            route: String::new(),
        }
    }

    fn equity(min: u32, equity: f64) -> Event {
        Event::Equity {
            ts: Utc.with_ymd_and_hms(2026, 10, 1, 0, min, 0).unwrap(),
            cash: equity,
            equity,
            day_pnl: 0.0,
            open_positions: 0,
            benchmark_price: None,
        }
    }

    #[test]
    fn computes_stats_and_drawdown() {
        let evs = vec![
            equity(0, 1000.0),
            equity(1, 1100.0),
            equity(2, 990.0),
            equity(3, 1050.0),
            close(1, 10.0, "signal"),
            close(1, -5.0, "stop_loss"),
            close(2, 20.0, "signal"),
        ];
        let s = summarize(&evs);
        assert_eq!(s.trades, 3);
        assert_eq!(s.wins, 2);
        assert!((s.gross_profit - 30.0).abs() < 1e-9);
        assert!((s.gross_loss - 5.0).abs() < 1e-9);
        assert!((s.max_drawdown - 110.0).abs() < 1e-9);
        assert_eq!(s.by_day.len(), 2);
        assert_eq!(s.by_reason["signal"].0, 2);
        assert!(render(&s).contains("too few"));
    }
}
