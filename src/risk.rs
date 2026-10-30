//! Hard risk limits. Everything here is a pure function of config + state so
//! it can be tested without a network.

use std::fmt;

use serde::Serialize;

use crate::config::RiskConfig;
use crate::state::{Position, State};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reject {
    DailyCapHalted,
    MaxOpenPositions,
    AlreadyHolding,
    InsufficientBalance,
    PriceImpactTooHigh { pct: f64 },
}

impl fmt::Display for Reject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reject::DailyCapHalted => {
                write!(f, "daily loss cap hit; entries halted until next UTC day")
            }
            Reject::MaxOpenPositions => write!(f, "max open positions reached"),
            Reject::AlreadyHolding => write!(f, "already holding this token"),
            Reject::InsufficientBalance => write!(f, "insufficient balance"),
            Reject::PriceImpactTooHigh { pct } => write!(f, "price impact {pct:.3}% too high"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    Signal,
    StopLoss,
    TakeProfit,
    DailyCap,
}

/// How much quote token to commit to a new position, or why not to enter.
///
/// `strength` (tracker confidence) scales the size down, never up: the
/// result is always <= `max_position_size`.
pub fn entry_size(
    state: &State,
    cfg: &RiskConfig,
    mint: &str,
    strength: Option<f64>,
) -> Result<f64, Reject> {
    if state.halted {
        return Err(Reject::DailyCapHalted);
    }
    if state.positions.contains_key(mint) {
        return Err(Reject::AlreadyHolding);
    }
    if state.positions.len() >= cfg.max_open_positions {
        return Err(Reject::MaxOpenPositions);
    }
    let scale = strength.unwrap_or(1.0).clamp(0.0, 1.0);
    let wanted = cfg.max_position_size * scale;
    // Leave room for the entry fee and the eventual exit fee.
    let available = state.cash - 2.0 * cfg.est_fee_per_swap;
    let size = wanted.min(available);
    // Don't open dust positions just because cash is nearly gone.
    if size <= 0.0 || size < 0.1 * wanted {
        return Err(Reject::InsufficientBalance);
    }
    Ok(size)
}

pub fn check_price_impact(cfg: &RiskConfig, pct: f64) -> Result<(), Reject> {
    if pct > cfg.max_price_impact_pct {
        Err(Reject::PriceImpactTooHigh { pct })
    } else {
        Ok(())
    }
}

/// PnL of a position as a % of its cost, if it were sold for `exit_value`
/// (the quoted quote-token proceeds) right now, net of the exit fee.
pub fn position_pnl_pct(pos: &Position, exit_value: f64, cfg: &RiskConfig) -> f64 {
    (exit_value - cfg.est_fee_per_swap - pos.cost) / pos.cost * 100.0
}

/// Whether a position must be closed given its current quoted exit value.
pub fn exit_trigger(pos: &Position, exit_value: f64, cfg: &RiskConfig) -> Option<ExitReason> {
    let pnl = position_pnl_pct(pos, exit_value, cfg);
    if pnl <= -cfg.stop_loss_pct {
        return Some(ExitReason::StopLoss);
    }
    if let Some(tp) = cfg.take_profit_pct
        && pnl >= tp
    {
        return Some(ExitReason::TakeProfit);
    }
    None
}

/// Daily PnL is equity now minus equity at the start of the UTC day, so it
/// covers both realized and unrealized losses.
pub fn daily_cap_hit(day_pnl: f64, cfg: &RiskConfig) -> bool {
    day_pnl <= -cfg.daily_loss_cap
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn cfg() -> RiskConfig {
        RiskConfig {
            max_position_size: 100.0,
            max_open_positions: 2,
            stop_loss_pct: 8.0,
            take_profit_pct: Some(20.0),
            daily_loss_cap: 50.0,
            flatten_on_daily_cap: true,
            max_price_impact_pct: 1.0,
            max_signal_age_secs: 120,
            est_fee_per_swap: 0.0,
        }
    }

    fn pos(mint: &str, cost: f64) -> Position {
        Position {
            symbol: mint.into(),
            mint: mint.into(),
            qty: 1_000,
            cost,
            opened_at: Utc::now(),
            signal_id: "s".into(),
            last_value: cost,
        }
    }

    #[test]
    fn caps_size_at_max_position() {
        let s = State::new(1_000.0, Utc::now());
        assert_eq!(entry_size(&s, &cfg(), "A", None), Ok(100.0));
        assert_eq!(entry_size(&s, &cfg(), "A", Some(0.5)), Ok(50.0));
        // Strength above 1 can't inflate size past the cap.
        assert_eq!(entry_size(&s, &cfg(), "A", Some(3.0)), Ok(100.0));
    }

    #[test]
    fn size_limited_by_cash_and_fees() {
        let mut c = cfg();
        c.est_fee_per_swap = 1.0;
        let s = State::new(60.0, Utc::now());
        assert_eq!(entry_size(&s, &c, "A", None), Ok(58.0));
        let s = State::new(5.0, Utc::now());
        assert_eq!(
            entry_size(&s, &c, "A", None),
            Err(Reject::InsufficientBalance)
        );
    }

    #[test]
    fn rejects_when_halted_full_or_holding() {
        let mut s = State::new(1_000.0, Utc::now());
        s.positions.insert("A".into(), pos("A", 100.0));
        assert_eq!(
            entry_size(&s, &cfg(), "A", None),
            Err(Reject::AlreadyHolding)
        );
        s.positions.insert("B".into(), pos("B", 100.0));
        assert_eq!(
            entry_size(&s, &cfg(), "C", None),
            Err(Reject::MaxOpenPositions)
        );
        s.positions.clear();
        s.halted = true;
        assert_eq!(
            entry_size(&s, &cfg(), "C", None),
            Err(Reject::DailyCapHalted)
        );
    }

    #[test]
    fn stop_loss_and_take_profit() {
        let p = pos("A", 100.0);
        assert_eq!(exit_trigger(&p, 95.0, &cfg()), None);
        assert_eq!(exit_trigger(&p, 92.0, &cfg()), Some(ExitReason::StopLoss));
        assert_eq!(exit_trigger(&p, 80.0, &cfg()), Some(ExitReason::StopLoss));
        assert_eq!(exit_trigger(&p, 119.0, &cfg()), None);
        assert_eq!(
            exit_trigger(&p, 120.0, &cfg()),
            Some(ExitReason::TakeProfit)
        );
    }

    #[test]
    fn stop_loss_accounts_for_exit_fee() {
        let mut c = cfg();
        c.est_fee_per_swap = 1.0;
        // 93 gross - 1 fee = 92 net = -8%.
        assert_eq!(
            exit_trigger(&pos("A", 100.0), 93.0, &c),
            Some(ExitReason::StopLoss)
        );
    }

    #[test]
    fn daily_cap() {
        assert!(!daily_cap_hit(-49.99, &cfg()));
        assert!(daily_cap_hit(-50.0, &cfg()));
    }

    #[test]
    fn price_impact() {
        assert!(check_price_impact(&cfg(), 0.5).is_ok());
        assert!(check_price_impact(&cfg(), 1.5).is_err());
    }
}
