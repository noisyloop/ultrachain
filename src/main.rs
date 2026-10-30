mod config;
mod dashboard;
mod engine;
mod jupiter;
mod ledger;
mod live;
mod notify;
mod report;
mod risk;
mod signal;
mod state;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use crate::config::{Config, Mode, SignalConfig};
use crate::engine::Engine;
use crate::jupiter::JupiterClient;
use crate::live::LiveExecutor;
use crate::notify::Notifier;
use crate::signal::{AnySource, FileSource, HttpSource};

#[derive(Parser)]
#[command(about = "Ultrachain signal-following trading bot (paper by default)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the trading loop.
    Run {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
        /// Required together with mode = "live" to trade real funds.
        #[arg(long)]
        live: bool,
    },
    /// Summarize the paper-trading ledger.
    Report {
        #[arg(short, long, default_value = "data/ledger.jsonl")]
        ledger: PathBuf,
        /// Also write a self-contained HTML dashboard to this path.
        #[arg(long)]
        html: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    match Cli::parse().cmd {
        Cmd::Run { config, live } => run(Config::load(&config)?, live).await,
        Cmd::Report { ledger, html } => {
            let events = ledger::read_all(&ledger)?;
            let summary = report::summarize(&events);
            print!("{}", report::render(&summary));
            if let Some(path) = html {
                std::fs::write(&path, dashboard::render(&events, &summary))?;
                println!("\nDashboard written to {}", path.display());
            }
            Ok(())
        }
    }
}

async fn run(cfg: Config, live_flag: bool) -> Result<()> {
    let live = match (cfg.mode, live_flag) {
        (Mode::Paper, false) => None,
        (Mode::Paper, true) => bail!("--live given but config has mode = \"paper\""),
        (Mode::Live, false) => bail!(
            "config has mode = \"live\": pass --live as well to trade real funds. \
             Live execution is an untested sketch; paper trade first and review `report`."
        ),
        (Mode::Live, true) => Some(LiveExecutor::new(
            cfg.jupiter.clone(),
            cfg.live.clone().expect("validated"),
        )?),
    };
    let quoter = JupiterClient::new(cfg.jupiter.clone())?;
    let source = match &cfg.signals {
        SignalConfig::File { path } => AnySource::File(FileSource::new(path.clone())),
        SignalConfig::Http { url } => AnySource::Http(HttpSource::new(url.clone())?),
    };
    let tick = Duration::from_secs(cfg.tick_secs);
    let notifier = Notifier::new(cfg.alerts.webhook_url.clone());
    let mut engine = Engine::new(cfg, quoter, source, chrono::Utc::now())?.with_notifier(notifier);
    if let Some(live) = live {
        engine = engine.with_live(live);
    }

    let mut interval = tokio::time::interval(tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(e) = engine.tick(chrono::Utc::now()).await {
                    tracing::error!("tick failed: {e:#}");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!(equity = engine.state.equity(), "shutting down");
                return Ok(());
            }
        }
    }
}
