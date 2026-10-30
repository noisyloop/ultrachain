use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Quote real routes through Jupiter, but only record "would-be" fills.
    Paper,
    /// Sign and submit swaps. Not implemented in this build; refused at startup.
    Live,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub mode: Mode,
    /// Seconds between engine ticks (signal poll + position marking).
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,
    /// Directory for the trade ledger and persisted state.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// Starting paper balance, in quote-token units (e.g. USDC).
    pub starting_balance: f64,
    pub quote_token: Token,
    #[serde(default)]
    pub tokens: Vec<Token>,
    pub jupiter: JupiterConfig,
    pub signals: SignalConfig,
    pub risk: RiskConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Token {
    pub symbol: String,
    pub mint: String,
    pub decimals: u8,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JupiterConfig {
    #[serde(default = "default_jupiter_url")]
    pub base_url: String,
    /// Optional key for the authenticated api.jup.ag tier. Falls back to the
    /// JUPITER_API_KEY environment variable.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_slippage_bps")]
    pub slippage_bps: u16,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SignalConfig {
    /// Tail a JSON-lines file the tracker appends to.
    File { path: PathBuf },
    /// Poll an HTTP endpoint that returns a JSON array of signals.
    Http { url: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    /// Max quote-token value committed to a single position at entry.
    pub max_position_size: f64,
    /// Max number of simultaneously open positions.
    #[serde(default = "default_max_open_positions")]
    pub max_open_positions: usize,
    /// Close a position when its exit value is this % below cost (e.g. 8.0).
    pub stop_loss_pct: f64,
    /// Optional: close a position when its exit value is this % above cost.
    #[serde(default)]
    pub take_profit_pct: Option<f64>,
    /// Halt new entries for the rest of the UTC day once realized + unrealized
    /// PnL for the day drops to -daily_loss_cap.
    pub daily_loss_cap: f64,
    /// Also close every open position when the daily cap is hit.
    #[serde(default = "default_true")]
    pub flatten_on_daily_cap: bool,
    /// Reject quotes whose price impact exceeds this %.
    #[serde(default = "default_max_price_impact_pct")]
    pub max_price_impact_pct: f64,
    /// Ignore signals older than this many seconds.
    #[serde(default = "default_max_signal_age_secs")]
    pub max_signal_age_secs: i64,
    /// Flat per-swap cost estimate (network + priority fees) in quote-token
    /// units, deducted from paper fills so results aren't flattering.
    #[serde(default)]
    pub est_fee_per_swap: f64,
}

fn default_tick_secs() -> u64 {
    15
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("data")
}
fn default_jupiter_url() -> String {
    "https://lite-api.jup.ag/swap/v1".into()
}
fn default_slippage_bps() -> u16 {
    50
}
fn default_timeout_secs() -> u64 {
    10
}
fn default_max_open_positions() -> usize {
    3
}
fn default_true() -> bool {
    true
}
fn default_max_price_impact_pct() -> f64 {
    1.0
}
fn default_max_signal_age_secs() -> i64 {
    120
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&raw).context("parsing config")?;
        if cfg.jupiter.api_key.is_none() {
            cfg.jupiter.api_key = std::env::var("JUPITER_API_KEY").ok().filter(|k| !k.is_empty());
        }
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        let r = &self.risk;
        if r.max_position_size <= 0.0 {
            bail!("risk.max_position_size must be > 0");
        }
        if !(0.0 < r.stop_loss_pct && r.stop_loss_pct < 100.0) {
            bail!("risk.stop_loss_pct must be between 0 and 100");
        }
        if r.daily_loss_cap <= 0.0 {
            bail!("risk.daily_loss_cap must be > 0");
        }
        if r.max_open_positions == 0 {
            bail!("risk.max_open_positions must be >= 1");
        }
        if let Some(tp) = r.take_profit_pct
            && tp <= 0.0
        {
            bail!("risk.take_profit_pct must be > 0");
        }
        if self.starting_balance <= 0.0 {
            bail!("starting_balance must be > 0");
        }
        Ok(())
    }

    pub fn token_by_mint(&self, mint: &str) -> Option<&Token> {
        self.tokens.iter().find(|t| t.mint == mint)
    }

    /// Resolve a signal's token reference, which may be a mint or a symbol.
    pub fn resolve_token(&self, token: &str) -> Option<&Token> {
        self.token_by_mint(token)
            .or_else(|| self.tokens.iter().find(|t| t.symbol.eq_ignore_ascii_case(token)))
    }
}

impl Token {
    pub fn to_base_units(&self, ui_amount: f64) -> u64 {
        (ui_amount * 10f64.powi(self.decimals as i32)).floor() as u64
    }

    pub fn to_ui(&self, base_units: u64) -> f64 {
        base_units as f64 / 10f64.powi(self.decimals as i32)
    }
}
