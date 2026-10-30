//! Best-effort alerts to a Slack/Discord-style incoming webhook. Failures to
//! deliver are logged, never fatal: an alert outage must not stop trading
//! logic (or its stop losses).

use std::time::Duration;

pub struct Notifier {
    http: reqwest::Client,
    url: Option<String>,
}

impl Notifier {
    pub fn new(url: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("building http client");
        Self { http, url }
    }

    pub fn disabled() -> Self {
        Self::new(None)
    }

    pub async fn alert(&self, msg: &str) {
        tracing::warn!(alert = msg);
        let Some(url) = &self.url else { return };
        // `text` for Slack, `content` for Discord; each ignores the other.
        let body = serde_json::json!({ "text": msg, "content": msg });
        if let Err(e) = self.http.post(url).json(&body).send().await {
            tracing::error!("alert delivery failed: {e:#}");
        }
    }
}
