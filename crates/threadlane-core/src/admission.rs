//! Who may connect to a hub (Plan.md §3.5 "identity is the key").
//!
//! A hub keeps a durable list of admitted transport keys. A key gets on the
//! list by presenting the secret of a one-time join code; it comes off by
//! being revoked. The store keeps only a hash of each join secret.

use std::fmt;
use std::str::FromStr;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::store::StoreResult;
use crate::time::Timestamp;

/// What an admitted key may do. Machines join in milestone 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum KeyRole {
    /// A desktop or command-line client: the client interface.
    Client,
}

impl KeyRole {
    /// The value stored in the database and used on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
        }
    }
}

impl fmt::Display for KeyRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for KeyRole {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "client" => Ok(Self::Client),
            other => Err(format!("unknown KeyRole value {other:?}")),
        }
    }
}

/// One key on a hub's admitted list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedKey {
    /// The node's public key, as iroh prints it (hex).
    pub endpoint_id: String,
    /// The name the hub gave it, unique in the space, for example `client-2`.
    pub name: String,
    pub role: KeyRole,
    pub admitted_at: Timestamp,
    /// When it was revoked. A revoked key is never admitted again.
    pub revoked_at: Option<Timestamp>,
}

impl AdmittedKey {
    /// Whether the key may connect now.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// Join codes and admitted keys (Plan.md §3.5).
#[async_trait]
pub trait AdmissionStore: Send + Sync {
    /// Records a join code by the hash of its secret.
    async fn add_join_code(
        &self,
        secret_hash: &str,
        role: KeyRole,
        expires_at: Timestamp,
    ) -> StoreResult<()>;
    /// The secret hashes of join codes neither used nor expired at `now`.
    async fn open_join_codes(&self, now: Timestamp) -> StoreResult<Vec<String>>;
    /// Uses the join code once and admits `endpoint_id` under a new name, in
    /// one transaction. A conflict if the code is used or expired, or the key
    /// is already known (admitted or revoked).
    async fn redeem_join_code(
        &self,
        secret_hash: &str,
        endpoint_id: &str,
        now: Timestamp,
    ) -> StoreResult<AdmittedKey>;
    async fn admitted_key(&self, endpoint_id: &str) -> StoreResult<Option<AdmittedKey>>;
    /// Every key ever admitted, revoked ones included, oldest first.
    async fn admitted_keys(&self) -> StoreResult<Vec<AdmittedKey>>;
    /// Revokes the active key of this name. Not found if there is none.
    async fn revoke_key(&self, name: &str, now: Timestamp) -> StoreResult<AdmittedKey>;
}
