//! Model-managed session plans, canonical in `threadlane-plan`.
//!
//! Validation, the in-memory store, and the `update_plan` tool executor live
//! in the leaf crate and persist through the injectable [`PlanJournal`]
//! trait. This module keeps the historical `threadlane_runtime::plan::…`
//! paths working, adapts the session JSONL as the journal
//! ([`JsonlPlanJournal`]), and pins the file-backed behavior with an
//! integration test. New code should import `threadlane_plan` directly.

pub use threadlane_plan::{PlanJournal, SessionPlanStore, UpdatePlanToolExecutor};

use crate::harness::JsonlStore;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use threadlane_protocol::SessionPlan;

/// JSONL-backed [`PlanJournal`]: opens the session file on every update and
/// appends the plan fact, matching the previous inline behavior.
pub struct JsonlPlanJournal {
    path: PathBuf,
}

impl JsonlPlanJournal {
    pub(crate) fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
}

impl PlanJournal for JsonlPlanJournal {
    fn append_plan(&mut self, plan: &SessionPlan) -> Result<(), String> {
        let mut store = JsonlStore::open(&self.path)
            .map_err(|error| format!("Failed to open session for plan update: {error}"))?;
        store
            .append_plan(plan)
            .map_err(|error| format!("Failed to persist session plan: {error}"))?;
        Ok(())
    }
}

/// Builds a file-backed plan store, preserving the previous
/// `SessionPlanStore::new(plan, session_file)` call shape for session code.
pub fn session_plan_store(plan: SessionPlan, session_file: Option<PathBuf>) -> SessionPlanStore {
    SessionPlanStore::new(
        plan,
        session_file.map(|path| {
            Arc::new(Mutex::new(JsonlPlanJournal::new(path))) as Arc<Mutex<dyn PlanJournal>>
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::SessionStore;
    use threadlane_protocol::{AgentEvent, ToolExecutor};
    use std::time::Duration;

    #[tokio::test]
    async fn file_backed_execution_persists_a_readable_plan_fact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(4);
        let store = session_plan_store(Default::default(), Some(path.clone()));
        let executor = UpdatePlanToolExecutor::new(store.clone(), event_tx);

        executor
            .execute_tool(
                "update_plan",
                r#"{"plan":[{"step":"Inspect","status":"completed"}]}"#,
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            JsonlStore::open_read_only(&path).unwrap().plan(),
            store.current(),
            "the journaled plan fact must match the live plan"
        );
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
                .await
                .unwrap()
                .unwrap(),
            AgentEvent::PlanUpdated { plan } if plan == store.current()
        ));
    }
}
