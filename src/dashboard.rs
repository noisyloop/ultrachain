//! Self-contained HTML dashboard for a paper-trading ledger
//! (`report --html out.html`). The page embeds the data as JSON and draws
//! its charts with inline SVG, so it opens offline with no server.

use serde_json::json;

use crate::ledger::Event;
use crate::report::{Summary, verdict};

const TEMPLATE: &str = include_str!("dashboard.html");
const MAX_EQUITY_POINTS: usize = 600;
const RECENT_TRADES: usize = 10;

pub fn render(events: &[Event], s: &Summary) -> String {
    let equity: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Equity { ts, equity, .. } => Some((ts.timestamp_millis(), *equity)),
            _ => None,
        })
        .collect();
    // Thin long runs evenly, always keeping the last point.
    let step = equity.len().div_ceil(MAX_EQUITY_POINTS).max(1);
    let mut thinned: Vec<_> = equity.iter().step_by(step).copied().collect();
    if let Some(last) = equity.last()
        && thinned.last() != Some(last)
    {
        thinned.push(*last);
    }

    let trades: Vec<_> = events
        .iter()
        .rev()
        .filter_map(|e| match e {
            Event::Close {
                ts,
                symbol,
                reason,
                pnl,
                pnl_pct,
                held_secs,
                ..
            } => Some(json!({
                "ts": ts.to_rfc3339(), "symbol": symbol, "reason": reason,
                "pnl": pnl, "pnl_pct": pnl_pct, "held_secs": held_secs,
            })),
            _ => None,
        })
        .take(RECENT_TRADES)
        .collect();

    let net = s.gross_profit - s.gross_loss;
    let data = json!({
        "first_ts": s.first_ts.map(|t| t.to_rfc3339()),
        "last_ts": s.last_ts.map(|t| t.to_rfc3339()),
        "start_equity": s.start_equity,
        "end_equity": s.end_equity,
        "trades": s.trades,
        "opened": s.open_events,
        "wins": s.wins,
        "net": net,
        "fees": s.fees,
        "profit_factor": (s.gross_loss > 0.0).then(|| s.gross_profit / s.gross_loss),
        "max_drawdown": s.max_drawdown,
        "max_drawdown_pct": s.max_drawdown_pct,
        "halts": s.halts,
        "equity": thinned,
        "by_day": s.by_day.iter().map(|(d, p)| (d.to_string(), *p)).collect::<Vec<_>>(),
        "bench_return_pct": s.bench_return_pct(),
        "by_token": s.by_token.iter().map(|(t, (n, p))| json!({"token": t, "n": n, "pnl": p})).collect::<Vec<_>>(),
        "by_reason": s.by_reason.iter().map(|(r, (n, p))| json!({"reason": r, "n": n, "pnl": p})).collect::<Vec<_>>(),
        "rejects": s.rejects.iter().map(|(r, n)| json!({"reason": r, "n": n})).collect::<Vec<_>>(),
        "recent": trades,
        "verdict": verdict(s),
    });
    // `</` can't appear inside the inline <script>; escape it.
    let blob = data.to_string().replace("</", "<\\/");
    TEMPLATE.replace("/*__DATA__*/null", &blob)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::summarize;
    use chrono::{TimeZone, Utc};

    #[test]
    fn embeds_data_and_escapes_script_close() {
        let ts = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let evs = vec![
            Event::Equity {
                ts,
                cash: 1000.0,
                equity: 1000.0,
                day_pnl: 0.0,
                open_positions: 0,
                benchmark_price: None,
            },
            Event::Close {
                ts,
                symbol: "</script>".into(),
                mint: "X".into(),
                reason: "signal".into(),
                qty: 1.0,
                proceeds: 101.0,
                fee: 0.0,
                cost: 100.0,
                pnl: 1.0,
                pnl_pct: 1.0,
                price: 1.0,
                price_impact_pct: 0.0,
                held_secs: 60,
                route: String::new(),
            },
        ];
        let html = render(&evs, &summarize(&evs));
        assert!(!html.contains("/*__DATA__*/"));
        assert!(html.contains(r#""trades":1"#));
        assert!(!html.contains("\"</script>\""));
    }
}
