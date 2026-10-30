//! Minimal Jupiter Swap API client (quote endpoint only).
//!
//! Paper mode never builds or signs a transaction: it asks Jupiter for the
//! route it *would* take and records the quoted output as the fill.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::config::JupiterConfig;

#[derive(Debug, Clone)]
pub struct Quote {
    pub in_amount: u64,
    pub out_amount: u64,
    pub price_impact_pct: f64,
    /// Human-readable route, e.g. "Raydium>Orca".
    pub route: String,
}

/// Anything that can price an exact-in swap. Implemented by the Jupiter
/// client and by test doubles.
pub trait Quoter {
    async fn quote(&self, input_mint: &str, output_mint: &str, amount: u64) -> Result<Quote>;
}

pub struct JupiterClient {
    http: reqwest::Client,
    cfg: JupiterConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuoteResponse {
    in_amount: String,
    out_amount: String,
    #[serde(default)]
    price_impact_pct: Option<String>,
    #[serde(default)]
    route_plan: Vec<RoutePlanStep>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoutePlanStep {
    swap_info: SwapInfo,
}

#[derive(Deserialize)]
struct SwapInfo {
    #[serde(default)]
    label: Option<String>,
}

impl JupiterClient {
    pub fn new(cfg: JupiterConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()?;
        Ok(Self { http, cfg })
    }
}

impl Quoter for JupiterClient {
    async fn quote(&self, input_mint: &str, output_mint: &str, amount: u64) -> Result<Quote> {
        if amount == 0 {
            bail!("quote amount is zero");
        }
        let url = format!("{}/quote", self.cfg.base_url.trim_end_matches('/'));
        let mut req = self.http.get(&url).query(&[
            ("inputMint", input_mint.to_string()),
            ("outputMint", output_mint.to_string()),
            ("amount", amount.to_string()),
            ("slippageBps", self.cfg.slippage_bps.to_string()),
            ("swapMode", "ExactIn".to_string()),
        ]);
        if let Some(key) = &self.cfg.api_key {
            req = req.header("x-api-key", key);
        }
        let resp = req.send().await.context("jupiter quote request")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("jupiter quote failed ({status}): {body}");
        }
        let q: QuoteResponse = resp.json().await.context("decoding jupiter quote")?;
        parse_quote(q)
    }
}

fn parse_quote(q: QuoteResponse) -> Result<Quote> {
    let route = q
        .route_plan
        .iter()
        .map(|s| s.swap_info.label.clone().unwrap_or_else(|| "?".into()))
        .collect::<Vec<_>>()
        .join(">");
    Ok(Quote {
        in_amount: q.in_amount.parse().context("inAmount")?,
        out_amount: q.out_amount.parse().context("outAmount")?,
        // Jupiter reports impact as a fraction ("0.0012" == 0.12%).
        price_impact_pct: q
            .price_impact_pct
            .as_deref()
            .unwrap_or("0")
            .parse::<f64>()
            .context("priceImpactPct")?
            * 100.0,
        route,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quote_response() {
        let body = r#"{
            "inputMint": "EPjFWdd5AufqSSqeM2qN1xDybapC8G4wEGGkZwyTDt1v",
            "inAmount": "100000000",
            "outputMint": "So11111111111111111111111111111111111111112",
            "outAmount": "612345678",
            "otherAmountThreshold": "609283950",
            "swapMode": "ExactIn",
            "slippageBps": 50,
            "priceImpactPct": "0.0012",
            "routePlan": [
                {"swapInfo": {"label": "Raydium"}, "percent": 100},
                {"swapInfo": {"label": "Orca"}, "percent": 100}
            ]
        }"#;
        let q = parse_quote(serde_json::from_str(body).unwrap()).unwrap();
        assert_eq!(q.in_amount, 100_000_000);
        assert_eq!(q.out_amount, 612_345_678);
        assert!((q.price_impact_pct - 0.12).abs() < 1e-9);
        assert_eq!(q.route, "Raydium>Orca");
    }
}
