mod config;
mod engine;
mod jupiter;
mod ledger;
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
use crate::signal::{AnySource, FileSource, HttpSource};

#[derive(Parser)]
#[command(about = "Ultrachain signal-following trading bot (paper mode)")]
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
    },
    /// Summarize the paper-trading ledger.
    Report {
        #[arg(short, long, default_value = "data/ledger.jsonl")]
        ledger: PathBuf,
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
        Cmd::Run { config } => run(Config::load(&config)?).await,
        Cmd::Report { ledger } => {
            let events = ledger::read_all(&ledger)?;
            print!("{}", report::render(&report::summarize(&events)));
            Ok(())
        }
    }
}

async fn run(cfg: Config) -> Result<()> {
    if cfg.mode == Mode::Live {
        bail!(
            "live mode is not implemented: paper trade first and review `report`. \
             Set mode = \"paper\" in the config."
        );
    }
    let quoter = JupiterClient::new(cfg.jupiter.clone())?;
    let source = match &cfg.signals {
        SignalConfig::File { path } => AnySource::File(FileSource::new(path.clone())),
        SignalConfig::Http { url } => AnySource::Http(HttpSource::new(url.clone())?),
    };
    let tick = Duration::from_secs(cfg.tick_secs);
    let mut engine = Engine::new(cfg, quoter, source, chrono::Utc::now())?;

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
