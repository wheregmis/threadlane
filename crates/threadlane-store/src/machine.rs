//! [`MachineStore`] on the hub database (Plan.md §5.3).

use async_trait::async_trait;
use threadlane_core::ids::MachineId;
use threadlane_core::model::{
    Account, BackendKind, Machine, MachineReport, MachineStatus, NewMachine,
};
use threadlane_core::store::{MachineStore, StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use turso::Row;

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::row::{Cols, json_text, opt_ts, params};

const MACHINE_FIELDS: &str = "id, name, endpoint_id, is_hub, trust, status, last_seen,
    capabilities, backends, gh_login, max_parallel, load";

fn decode_machine(row: &Row) -> StoreResult<Machine> {
    let c = Cols(row);
    Ok(Machine {
        id: c.id(0)?,
        name: c.text(1)?,
        endpoint_id: c.text(2)?,
        is_hub: c.bool(3)?,
        trust: c.parse(4)?,
        status: c.parse(5)?,
        last_seen: c.opt_ts(6)?,
        capabilities: c.json(7)?,
        backends: c.json(8)?,
        gh_login: c.bool(9)?,
        max_parallel: c.u32(10)?,
        load: c.u32(11)?,
    })
}

const ACCOUNT_FIELDS: &str = "id, label, backend, max_parallel, cooldown_until";

fn decode_account(row: &Row) -> StoreResult<Account> {
    let c = Cols(row);
    Ok(Account {
        id: c.id(0)?,
        label: c.text(1)?,
        backend: c.parse(2)?,
        max_parallel: c.opt_u32(3)?,
        cooldown_until: c.opt_ts(4)?,
    })
}

#[async_trait]
impl MachineStore for HubDb {
    async fn register_machine(&self, machine: &NewMachine) -> StoreResult<Machine> {
        let tx = self.db.begin().await?;
        let created = query_one(
            tx.conn(),
            &format!(
                "INSERT INTO machines (name, endpoint_id, is_hub, trust, status, max_parallel)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING {MACHINE_FIELDS}"
            ),
            params![
                machine.name.as_str(),
                machine.endpoint_id.as_str(),
                machine.is_hub,
                machine.trust.as_str(),
                MachineStatus::Offline.as_str(),
                i64::from(machine.max_parallel),
            ],
            decode_machine,
        )
        .await?;
        tx.commit().await?;
        Ok(created)
    }

    async fn machine(&self, id: MachineId) -> StoreResult<Option<Machine>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!("SELECT {MACHINE_FIELDS} FROM machines WHERE id = ?1"),
            params![id.get()],
            decode_machine,
        )
        .await
    }

    async fn machine_by_endpoint(&self, endpoint_id: &str) -> StoreResult<Option<Machine>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!("SELECT {MACHINE_FIELDS} FROM machines WHERE endpoint_id = ?1"),
            params![endpoint_id],
            decode_machine,
        )
        .await
    }

    async fn machines(&self) -> StoreResult<Vec<Machine>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!("SELECT {MACHINE_FIELDS} FROM machines ORDER BY id"),
            (),
            decode_machine,
        )
        .await
    }

    async fn report(
        &self,
        id: MachineId,
        report: &MachineReport,
        now: Timestamp,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let changed = exec(
            tx.conn(),
            "UPDATE machines SET capabilities = ?1, backends = ?2, gh_login = ?3, load = ?4,
                 last_seen = ?5,
                 status = CASE WHEN status = 'offline' THEN 'online' ELSE status END
             WHERE id = ?6 AND status != 'revoked'",
            params![
                json_text(&report.capabilities)?,
                json_text(&report.backends)?,
                report.gh_login,
                i64::from(report.load),
                now.as_millis(),
                id.get(),
            ],
        )
        .await?;
        if changed != 1 {
            return Err(StoreError::Conflict(format!(
                "machine {id} is unknown or revoked"
            )));
        }
        tx.commit().await
    }

    async fn set_machine_status(&self, id: MachineId, status: MachineStatus) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let changed = exec(
            tx.conn(),
            "UPDATE machines SET status = ?1 WHERE id = ?2",
            params![status.as_str(), id.get()],
        )
        .await?;
        if changed != 1 {
            return Err(StoreError::not_found("machine", id));
        }
        tx.commit().await
    }

    async fn ensure_account(&self, label: &str, backend: BackendKind) -> StoreResult<Account> {
        let tx = self.db.begin().await?;
        let existing = query_opt(
            tx.conn(),
            &format!("SELECT {ACCOUNT_FIELDS} FROM accounts WHERE label = ?1"),
            params![label],
            decode_account,
        )
        .await?;
        let account = match existing {
            Some(account) if account.backend == backend => account,
            Some(account) => {
                return Err(StoreError::Conflict(format!(
                    "account {label} is a {} account, not {backend}",
                    account.backend
                )));
            }
            None => {
                query_one(
                    tx.conn(),
                    &format!(
                        "INSERT INTO accounts (label, backend) VALUES (?1, ?2) RETURNING {ACCOUNT_FIELDS}"
                    ),
                    params![label, backend.as_str()],
                    decode_account,
                )
                .await?
            }
        };
        tx.commit().await?;
        Ok(account)
    }

    async fn accounts(&self) -> StoreResult<Vec<Account>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!("SELECT {ACCOUNT_FIELDS} FROM accounts ORDER BY id"),
            (),
            decode_account,
        )
        .await
    }

    async fn set_cooldown(&self, label: &str, until: Option<Timestamp>) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let changed = exec(
            tx.conn(),
            "UPDATE accounts SET cooldown_until = ?1 WHERE label = ?2",
            params![opt_ts(until), label],
        )
        .await?;
        if changed != 1 {
            return Err(StoreError::not_found("account", label));
        }
        tx.commit().await
    }
}
