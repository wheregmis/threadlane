//! Pairing (Plan.md §3.5, §11.2): join codes, the admitted keys, revoking,
//! and the [`Gate`] the iroh transport asks before serving a key.

use std::fmt::Write as _;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use threadlane_core::admission::{AdmissionStore, KeyRole};
use threadlane_core::client::{ApiError, PairingReply, PairingRequest};
use threadlane_core::store::StoreError;
use threadlane_transport_iroh::{EndpointId, Gate, JoinCode};
use tokio::sync::watch;

use crate::core::HubCore;
use crate::error::api_error;

/// How long a join code stays valid.
pub const JOIN_CODE_TTL: Duration = Duration::from_secs(10 * 60);

/// The one answer to every refused join secret, so a refusal says nothing
/// about which codes exist.
const NOT_VALID: &str = "the join code is not valid; ask the hub for a new one";

/// Answers a [`PairingRequest`].
pub(crate) async fn handle(
    core: &HubCore,
    request: PairingRequest,
) -> Result<PairingReply, ApiError> {
    let inner = &core.inner;
    match request {
        PairingRequest::IssueJoinCode => {
            let addr = inner.iroh.clone().ok_or_else(|| {
                ApiError::Invalid("this hub does not serve iroh; see [iroh] in hub.toml".into())
            })?;
            let code = JoinCode::new(addr);
            let expires_at = inner.clock.now() + JOIN_CODE_TTL;
            inner
                .db
                .add_join_code(&secret_hash(&code.secret), KeyRole::Client, expires_at)
                .await
                .map_err(api_error)?;
            Ok(PairingReply::JoinCode {
                code: code.to_string(),
                expires_at,
            })
        }
        PairingRequest::Clients => Ok(PairingReply::Clients {
            clients: inner.db.admitted_keys().await.map_err(api_error)?,
        }),
        PairingRequest::Revoke { name } => {
            let client = inner
                .db
                .revoke_key(name.trim(), inner.clock.now())
                .await
                .map_err(|error| match error {
                    StoreError::NotFound { .. } => {
                        ApiError::NotFound(format!("no admitted client named {:?}", name.trim()))
                    }
                    other => api_error(other),
                })?;
            inner.wake.revoked();
            tracing::info!(name = %client.name, "revoked a client key");
            Ok(PairingReply::Revoked { client })
        }
        other => Err(ApiError::Invalid(format!("unsupported request {other:?}"))),
    }
}

/// The hub's [`Gate`]: its admitted list and join codes.
#[derive(Debug, Clone)]
pub(crate) struct HubGate {
    core: HubCore,
}

impl HubGate {
    pub(crate) fn new(core: HubCore) -> Self {
        Self { core }
    }
}

#[async_trait]
impl Gate for HubGate {
    async fn is_admitted(&self, key: EndpointId) -> bool {
        match self.core.inner.db.admitted_key(&key.to_string()).await {
            Ok(Some(admitted)) => admitted.is_active() && admitted.role == KeyRole::Client,
            Ok(None) => false,
            Err(error) => {
                tracing::error!(%error, "cannot read the admitted keys; refusing the connection");
                false
            }
        }
    }

    async fn join(&self, key: EndpointId, secret: &[u8; 32]) -> Result<(), ApiError> {
        let inner = &self.core.inner;
        let now = inner.clock.now();
        let presented = secret_hash(secret);
        let open = inner.db.open_join_codes(now).await.map_err(api_error)?;
        let Some(matching) = open.iter().find(|hash| constant_time_eq(hash, &presented)) else {
            tracing::info!(key = %key.fmt_short(), "refused a join secret");
            return Err(ApiError::Invalid(NOT_VALID.into()));
        };
        let admitted = inner
            .db
            .redeem_join_code(matching, &key.to_string(), now)
            .await
            .map_err(|error| match error {
                // Used meanwhile, expired meanwhile, or a key already known.
                StoreError::Conflict(_) => ApiError::Invalid(NOT_VALID.into()),
                other => api_error(other),
            })?;
        tracing::info!(name = %admitted.name, key = %key.fmt_short(), "admitted a client key");
        Ok(())
    }

    fn revocations(&self) -> watch::Receiver<u64> {
        self.core.inner.wake.revocations()
    }
}

/// What the store keeps of a join secret: its SHA-256, in hex.
fn secret_hash(secret: &[u8; 32]) -> String {
    let mut hash = String::with_capacity(64);
    for byte in Sha256::digest(secret).iter() {
        // Writing to a String cannot fail.
        let _ = write!(hash, "{byte:02x}");
    }
    hash
}

/// Compares without stopping at the first difference.
fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0_u8, |difference, (x, y)| difference | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_secret_hash_is_hex_sha256_and_compares_equal_only_to_itself() {
        let hash = secret_hash(&[0; 32]);
        assert_eq!(
            hash,
            "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"
        );
        assert!(constant_time_eq(&hash, &secret_hash(&[0; 32])));
        assert!(!constant_time_eq(&hash, &secret_hash(&[1; 32])));
        assert!(!constant_time_eq(&hash, &hash[1..]));
    }
}
