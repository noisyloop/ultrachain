//! Paper portfolio state, persisted to disk so a multi-week run survives
//! restarts.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::signal::SeenIds;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    pub mint: String,
    /// Token amount held, in base units.
    pub qty: u64,
    /// Quote-token spent to open, including the entry fee.
    pub cost: f64,
    pub opened_at: DateTime<Utc>,
    pub signal_id: String,
    /// Most recent quoted exit value (gross of exit fee).
    pub last_value: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct State {
    pub cash: f64,
    pub positions: BTreeMap<String, Position>,
    pub day: NaiveDate,
    pub day_start_equity: f64,
    /// Entries blocked for the rest of `day` because the daily cap was hit.
    pub halted: bool,
    pub seen: SeenIds,
}

impl State {
    pub fn new(starting_balance: f64, now: DateTime<Utc>) -> Self {
        Self {
            cash: starting_balance,
            positions: BTreeMap::new(),
            day: now.date_naive(),
            day_start_equity: starting_balance,
            halted: false,
            seen: SeenIds::default(),
        }
    }

    /// Cash plus the last quoted value of every open position.
    pub fn equity(&self) -> f64 {
        self.cash + self.positions.values().map(|p| p.last_value).sum::<f64>()
    }

    pub fn day_pnl(&self) -> f64 {
        self.equity() - self.day_start_equity
    }

    /// Roll over to a new UTC day: reset the daily baseline and lift a halt.
    /// Returns true if the day changed.
    pub fn roll_day(&mut self, now: DateTime<Utc>) -> bool {
        let today = now.date_naive();
        if today == self.day {
            return false;
        }
        self.day = today;
        self.day_start_equity = self.equity();
        self.halted = false;
        true
    }

    pub fn load_or_new(path: &Path, starting_balance: f64, now: DateTime<Utc>) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw).context("parsing state file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::new(starting_balance, now))
            }
            Err(e) => Err(e).context("reading state file"),
        }
    }

    /// Atomic write: temp file + rename, so a crash never leaves half a file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn roll_day_resets_baseline_and_halt() {
        let t0 = Utc.with_ymd_and_hms(2026, 10, 30, 23, 0, 0).unwrap();
        let mut s = State::new(1_000.0, t0);
        s.cash = 940.0;
        s.halted = true;
        assert!(!s.roll_day(t0 + chrono::Duration::minutes(30)));
        assert!(s.halted);
        assert!(s.roll_day(t0 + chrono::Duration::hours(2)));
        assert!(!s.halted);
        assert_eq!(s.day_start_equity, 940.0);
        assert_eq!(s.day_pnl(), 0.0);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut s = State::new(500.0, Utc::now());
        s.seen.insert("x");
        s.save(&path).unwrap();
        let mut loaded = State::load_or_new(&path, 1.0, Utc::now()).unwrap();
        assert_eq!(loaded.cash, 500.0);
        assert!(!loaded.seen.insert("x"));
    }
}
