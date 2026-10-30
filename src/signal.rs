//! Tracker signal ingestion.
//!
//! The tracker emits one JSON object per signal:
//!
//! ```json
//! {"id": "sig-123", "ts": "2026-10-30T12:00:00Z", "token": "BONK", "side": "buy"}
//! ```
//!
//! `token` may be a mint address or a symbol from the config's token list.
//! `id` is used for de-duplication, so re-delivered signals are ignored.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub id: String,
    pub ts: DateTime<Utc>,
    pub token: String,
    pub side: Side,
    /// Optional tracker confidence in [0, 1]; scales entry size when present.
    #[serde(default)]
    pub strength: Option<f64>,
}

pub trait SignalSource {
    /// Return signals that have not been returned before.
    async fn poll(&mut self) -> Result<Vec<Signal>>;
}

/// Tails a JSON-lines file, remembering the byte offset between polls.
pub struct FileSource {
    path: PathBuf,
    offset: u64,
    partial: String,
}

impl FileSource {
    pub fn new(path: PathBuf) -> Self {
        Self { path, offset: 0, partial: String::new() }
    }
}

impl SignalSource for FileSource {
    async fn poll(&mut self) -> Result<Vec<Signal>> {
        let mut file = match tokio::fs::File::open(&self.path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e).context("opening signal file"),
        };
        let len = file.metadata().await?.len();
        if len < self.offset {
            // Truncated or rotated: start over.
            self.offset = 0;
            self.partial.clear();
        }
        file.seek(std::io::SeekFrom::Start(self.offset)).await?;
        let mut buf = String::new();
        let n = file.read_to_string(&mut buf).await? as u64;
        self.offset += n;

        self.partial.push_str(&buf);
        let mut out = Vec::new();
        // Only consume complete lines; keep a trailing partial line for later.
        while let Some(nl) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=nl).collect();
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Signal>(line) {
                Ok(s) => out.push(s),
                Err(e) => tracing::warn!("skipping malformed signal line: {e}: {line}"),
            }
        }
        Ok(out)
    }
}

/// Polls an HTTP endpoint returning a JSON array of signals. The endpoint may
/// return the same signals repeatedly; ids are de-duplicated by the engine.
pub struct HttpSource {
    http: reqwest::Client,
    url: String,
}

impl HttpSource {
    pub fn new(url: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self { http, url })
    }
}

impl SignalSource for HttpSource {
    async fn poll(&mut self) -> Result<Vec<Signal>> {
        let resp = self.http.get(&self.url).send().await.context("polling tracker")?;
        let resp = resp.error_for_status().context("tracker returned error")?;
        resp.json().await.context("decoding tracker signals")
    }
}

pub enum AnySource {
    File(FileSource),
    Http(HttpSource),
}

impl SignalSource for AnySource {
    async fn poll(&mut self) -> Result<Vec<Signal>> {
        match self {
            AnySource::File(s) => s.poll().await,
            AnySource::Http(s) => s.poll().await,
        }
    }
}

/// Remembers which signal ids have already been acted on.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SeenIds {
    ids: HashSet<String>,
}

impl SeenIds {
    /// Returns true if the id was not seen before.
    pub fn insert(&mut self, id: &str) -> bool {
        self.ids.insert(id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn file_source_reads_only_new_complete_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signals.jsonl");
        let mut src = FileSource::new(path.clone());
        assert!(src.poll().await.unwrap().is_empty());

        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"id":"a","ts":"2026-10-30T00:00:00Z","token":"SOL","side":"buy"}}"#).unwrap();
        write!(f, r#"{{"id":"b","ts":"2026-10-30T00:00:01Z","#).unwrap();
        f.flush().unwrap();

        let got = src.poll().await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "a");

        writeln!(f, r#""token":"SOL","side":"sell"}}"#).unwrap();
        f.flush().unwrap();
        let got = src.poll().await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "b");
        assert_eq!(got[0].side, Side::Sell);

        assert!(src.poll().await.unwrap().is_empty());
    }
}
