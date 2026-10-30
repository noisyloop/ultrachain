//! Append-only JSON-lines log of every would-be trade and risk event. This is
//! the source of truth for `report`.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Open {
        ts: DateTime<Utc>,
        signal_id: String,
        symbol: String,
        mint: String,
        /// Quote token spent on the swap (excluding fee).
        spend: f64,
        fee: f64,
        /// Token received, UI units.
        qty: f64,
        /// Effective quote-per-token price of the fill.
        price: f64,
        price_impact_pct: f64,
        route: String,
    },
    Close {
        ts: DateTime<Utc>,
        symbol: String,
        mint: String,
        reason: String,
        qty: f64,
        /// Quote token received, net of fee.
        proceeds: f64,
        fee: f64,
        cost: f64,
        pnl: f64,
        pnl_pct: f64,
        price: f64,
        price_impact_pct: f64,
        held_secs: i64,
        route: String,
    },
    Reject {
        ts: DateTime<Utc>,
        signal_id: String,
        token: String,
        side: String,
        reason: String,
    },
    Halt {
        ts: DateTime<Utc>,
        day_pnl: f64,
        cap: f64,
    },
    Equity {
        ts: DateTime<Utc>,
        cash: f64,
        equity: f64,
        day_pnl: f64,
        open_positions: usize,
    },
}

pub struct Ledger {
    path: PathBuf,
    file: File,
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening ledger {}", path.display()))?;
        Ok(Self { path: path.to_path_buf(), file })
    }

    pub fn record(&mut self, ev: &Event) -> Result<()> {
        let mut line = serde_json::to_vec(ev)?;
        line.push(b'\n');
        self.file.write_all(&line).context("writing ledger")?;
        self.file.flush()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn read_all(path: &Path) -> Result<Vec<Event>> {
    let f = File::open(path).with_context(|| format!("opening ledger {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let ev = serde_json::from_str(&line)
            .with_context(|| format!("ledger line {}", i + 1))?;
        out.push(ev);
    }
    Ok(out)
}
