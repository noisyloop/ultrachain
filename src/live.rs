//! UNTESTED SKETCH of live execution: Jupiter /swap builds the transaction,
//! we sign it with the wallet key and submit it to an RPC.
//!
//! Known gaps before this should touch real money:
//! - fills are recorded at the quoted amount, not the actual balance change
//!   (read the post-transaction token balances instead);
//! - no handling of blockhash expiry beyond the confirm timeout;
//! - the engine's cash is the paper balance, not the wallet's.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};

use crate::config::{JupiterConfig, LiveConfig};
use crate::jupiter::Quote;

pub struct LiveExecutor {
    http: reqwest::Client,
    jupiter: JupiterConfig,
    cfg: LiveConfig,
    key: SigningKey,
}

impl LiveExecutor {
    pub fn new(jupiter: JupiterConfig, cfg: LiveConfig) -> Result<Self> {
        let key = load_keypair(&cfg.keypair_path)?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        tracing::warn!(wallet = %bs58::encode(key.verifying_key().as_bytes()).into_string(), "LIVE executor armed");
        Ok(Self {
            http,
            jupiter,
            cfg,
            key,
        })
    }

    pub fn pubkey(&self) -> String {
        bs58::encode(self.key.verifying_key().as_bytes()).into_string()
    }

    /// Execute the swap described by `quote`; returns the transaction signature.
    pub async fn execute(&self, quote: &Quote) -> Result<String> {
        let prioritization = match self.cfg.priority_fee_lamports {
            Some(l) => json!(l),
            None => json!("auto"),
        };
        let url = format!("{}/swap", self.jupiter.base_url.trim_end_matches('/'));
        let mut req = self.http.post(&url).json(&json!({
            "quoteResponse": quote.raw,
            "userPublicKey": self.pubkey(),
            "wrapAndUnwrapSol": true,
            "dynamicComputeUnitLimit": true,
            "prioritizationFeeLamports": prioritization,
        }));
        if let Some(key) = &self.jupiter.api_key {
            req = req.header("x-api-key", key);
        }
        let resp: Value = req.send().await?.error_for_status()?.json().await?;
        let tx_b64 = resp["swapTransaction"]
            .as_str()
            .context("jupiter /swap: no swapTransaction")?;
        let tx = base64::engine::general_purpose::STANDARD.decode(tx_b64)?;
        let signed = sign_transaction(&tx, &self.key)?;
        let sig = self.send(&signed).await?;
        self.confirm(&sig).await?;
        Ok(sig)
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let out: Value = self
            .http
            .post(&self.cfg.rpc_url)
            .json(&body)
            .send()
            .await?
            .json()
            .await?;
        if let Some(err) = out.get("error") {
            bail!("{method}: {err}");
        }
        Ok(out["result"].clone())
    }

    async fn send(&self, tx: &[u8]) -> Result<String> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(tx);
        let res = self
            .rpc(
                "sendTransaction",
                json!([b64, {"encoding": "base64", "maxRetries": 3}]),
            )
            .await?;
        res.as_str()
            .map(str::to_owned)
            .context("sendTransaction returned no signature")
    }

    async fn confirm(&self, sig: &str) -> Result<()> {
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(self.cfg.confirm_timeout_secs);
        while tokio::time::Instant::now() < deadline {
            let res = self.rpc("getSignatureStatuses", json!([[sig]])).await?;
            let status = &res["value"][0];
            if !status.is_null() {
                if !status["err"].is_null() {
                    bail!("transaction {sig} failed: {}", status["err"]);
                }
                if matches!(
                    status["confirmationStatus"].as_str(),
                    Some("confirmed" | "finalized")
                ) {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        bail!(
            "transaction {sig} not confirmed within {}s",
            self.cfg.confirm_timeout_secs
        )
    }
}

/// Read a Solana JSON keypair file (64 bytes: secret key then public key).
pub fn load_keypair(path: &Path) -> Result<SigningKey> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let bytes: Vec<u8> =
        serde_json::from_str(&raw).context("keypair file must be a JSON byte array")?;
    ensure!(
        bytes.len() == 64,
        "expected 64-byte keypair, got {}",
        bytes.len()
    );
    let key = SigningKey::from_bytes(bytes[..32].try_into().unwrap());
    ensure!(
        key.verifying_key().as_bytes() == &bytes[32..],
        "keypair public half doesn't match its secret"
    );
    Ok(key)
}

/// Read a compact-u16 ("shortvec") length; returns (value, bytes consumed).
fn shortvec(b: &[u8]) -> Result<(usize, usize)> {
    let mut val = 0usize;
    for (i, byte) in b.iter().take(3).enumerate() {
        val |= ((byte & 0x7f) as usize) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok((val, i + 1));
        }
    }
    bail!("bad shortvec")
}

/// Sign a serialized (legacy or v0) transaction as its fee payer: the
/// message is everything after the signature array, and the fee payer is
/// the first account key and the first signature slot.
pub fn sign_transaction(tx: &[u8], key: &SigningKey) -> Result<Vec<u8>> {
    let (n_sigs, len) = shortvec(tx)?;
    ensure!(n_sigs >= 1, "transaction has no signature slots");
    let msg_start = len + 64 * n_sigs;
    ensure!(tx.len() > msg_start, "truncated transaction");
    let msg = &tx[msg_start..];

    // v0 messages start with 0x80; then a 3-byte header and the account keys.
    let mut p = if msg[0] & 0x80 != 0 { 1 } else { 0 };
    p += 3;
    let (n_keys, l) = shortvec(&msg[p..])?;
    p += l;
    ensure!(
        n_keys >= 1 && msg.len() >= p + 32,
        "message has no account keys"
    );
    ensure!(
        &msg[p..p + 32] == key.verifying_key().as_bytes(),
        "fee payer in the transaction is not our wallet"
    );

    let sig = key.sign(msg);
    let mut out = tx.to_vec();
    out[len..len + 64].copy_from_slice(&sig.to_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;

    #[test]
    fn signs_v0_transaction_as_fee_payer() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let payer = key.verifying_key();
        // 1 empty signature slot + a minimal v0 message: prefix, header,
        // 2 account keys (payer first), blockhash, no instructions, no ALTs.
        let mut msg = vec![0x80, 1, 0, 1, 2];
        msg.extend_from_slice(payer.as_bytes());
        msg.extend_from_slice(&[9u8; 32]);
        msg.extend_from_slice(&[3u8; 32]);
        msg.extend_from_slice(&[0, 0]);
        let mut tx = vec![1u8];
        tx.extend_from_slice(&[0u8; 64]);
        tx.extend_from_slice(&msg);

        let signed = sign_transaction(&tx, &key).unwrap();
        let sig = ed25519_dalek::Signature::from_bytes(signed[1..65].try_into().unwrap());
        assert!(payer.verify(&msg, &sig).is_ok());
        assert_eq!(&signed[65..], &msg[..]);

        let other = SigningKey::from_bytes(&[8u8; 32]);
        assert!(sign_transaction(&tx, &other).is_err());
    }

    #[test]
    fn shortvec_decodes_multibyte() {
        assert_eq!(shortvec(&[0x05]).unwrap(), (5, 1));
        assert_eq!(shortvec(&[0x80, 0x01]).unwrap(), (128, 2));
    }
}
