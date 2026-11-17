//! [`AdmissionStore`] on the hub database (Plan.md §3.5).

use async_trait::async_trait;
use threadlane_core::admission::{AdmissionStore, AdmittedKey, KeyRole};
use threadlane_core::store::{StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use turso::Row;

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::row::{Cols, params};

const KEY_FIELDS: &str = "endpoint_id, name, role, admitted_at, revoked_at";

fn decode_key(row: &Row) -> StoreResult<AdmittedKey> {
    let c = Cols(row);
    Ok(AdmittedKey {
        endpoint_id: c.text(0)?,
        name: c.text(1)?,
        role: c.parse(2)?,
        admitted_at: c.ts(3)?,
        revoked_at: c.opt_ts(4)?,
    })
}

#[async_trait]
impl AdmissionStore for HubDb {
    async fn add_join_code(
        &self,
        secret_hash: &str,
        role: KeyRole,
        expires_at: Timestamp,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        exec(
            tx.conn(),
            "INSERT INTO join_codes (secret_hash, role, expires_at) VALUES (?1, ?2, ?3)",
            params![secret_hash, role.as_str(), expires_at.as_millis()],
        )
        .await?;
        tx.commit().await
    }

    async fn open_join_codes(&self, now: Timestamp) -> StoreResult<Vec<String>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            "SELECT secret_hash FROM join_codes WHERE used_at IS NULL AND expires_at > ?1",
            params![now.as_millis()],
            |row| Cols(row).text(0),
        )
        .await
    }

    async fn redeem_join_code(
        &self,
        secret_hash: &str,
        endpoint_id: &str,
        now: Timestamp,
    ) -> StoreResult<AdmittedKey> {
        let tx = self.db.begin().await?;
        let role: Option<KeyRole> = query_opt(
            tx.conn(),
            "UPDATE join_codes SET used_at = ?1
             WHERE secret_hash = ?2 AND used_at IS NULL AND expires_at > ?1
             RETURNING role",
            params![now.as_millis(), secret_hash],
            |row| Cols(row).parse(0),
        )
        .await?;
        let Some(role) = role else {
            return Err(StoreError::Conflict(
                "the join code is used, expired, or unknown".into(),
            ));
        };
        // Rows are never deleted, so a count gives a name no key has had.
        let taken = query_one(
            tx.conn(),
            "SELECT COUNT(*) FROM admitted_keys WHERE role = ?1",
            params![role.as_str()],
            |row| Cols(row).i64(0),
        )
        .await?;
        let admitted = query_one(
            tx.conn(),
            &format!(
                "INSERT INTO admitted_keys (endpoint_id, role, name, admitted_at)
                 VALUES (?1, ?2, ?3, ?4) RETURNING {KEY_FIELDS}"
            ),
            params![
                endpoint_id,
                role.as_str(),
                format!("{role}-{}", taken + 1),
                now.as_millis(),
            ],
            decode_key,
        )
        .await?;
        tx.commit().await?;
        Ok(admitted)
    }

    async fn admitted_key(&self, endpoint_id: &str) -> StoreResult<Option<AdmittedKey>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!("SELECT {KEY_FIELDS} FROM admitted_keys WHERE endpoint_id = ?1"),
            params![endpoint_id],
            decode_key,
        )
        .await
    }

    async fn admitted_keys(&self) -> StoreResult<Vec<AdmittedKey>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!("SELECT {KEY_FIELDS} FROM admitted_keys ORDER BY admitted_at, name"),
            (),
            decode_key,
        )
        .await
    }

    async fn revoke_key(&self, name: &str, now: Timestamp) -> StoreResult<AdmittedKey> {
        let tx = self.db.begin().await?;
        let revoked = query_opt(
            tx.conn(),
            &format!(
                "UPDATE admitted_keys SET revoked_at = ?1
                 WHERE name = ?2 AND revoked_at IS NULL RETURNING {KEY_FIELDS}"
            ),
            params![now.as_millis(), name],
            decode_key,
        )
        .await?
        .ok_or_else(|| StoreError::not_found("admitted key", name))?;
        tx.commit().await?;
        Ok(revoked)
    }
}
