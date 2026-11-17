//! Model-managed session plans (`update_plan` tool).
//!
//! Argument validation, the in-memory plan store, and the tool executor
//! depend only on `threadlane-protocol` contracts — never on the execution
//! engine. Durable persistence goes through the injectable [`PlanJournal`]
//! trait, which the engine implements with its session JSONL (see
//! `threadlane_runtime::plan::JsonlPlanJournal`). Import `threadlane_plan`
//! directly.

use async_trait::async_trait;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use threadlane_protocol::{
    AgentEvent, AgentToolDefinition, PlanItem, PlanItemStatus, SessionPlan, ToolExecutor,
};
use tokio::sync::broadcast;

const UPDATE_PLAN_TOOL_NAME: &str = "update_plan";
const MAX_PLAN_ITEMS: usize = 20;
const MAX_STEP_CHARS: usize = 200;
const MAX_EXPLANATION_CHARS: usize = 500;

#[derive(Deserialize)]
struct UpdatePlanArgs {
    #[serde(default)]
    explanation: Option<String>,
    plan: Vec<PlanItem>,
}

fn parse_update_plan(args: &str) -> Result<SessionPlan, String> {
    let args: UpdatePlanArgs = serde_json::from_str(args)
        .map_err(|error| format!("Invalid update_plan arguments: {error}"))?;
    if args.plan.len() > MAX_PLAN_ITEMS {
        return Err(format!("A plan may contain at most {MAX_PLAN_ITEMS} items"));
    }
    if args
        .explanation
        .as_ref()
        .is_some_and(|value| value.chars().count() > MAX_EXPLANATION_CHARS)
    {
        return Err(format!(
            "The plan explanation may contain at most {MAX_EXPLANATION_CHARS} characters"
        ));
    }

    let mut in_progress = 0;
    let mut items = Vec::with_capacity(args.plan.len());
    for mut item in args.plan {
        item.step = item.step.trim().to_string();
        if item.step.is_empty() {
            return Err("Each plan item requires a non-empty step".into());
        }
        if item.step.chars().count() > MAX_STEP_CHARS {
            return Err(format!(
                "Each plan step may contain at most {MAX_STEP_CHARS} characters"
            ));
        }
        if item.status == PlanItemStatus::InProgress {
            in_progress += 1;
        }
        items.push(item);
    }
    if in_progress > 1 {
        return Err("A plan may contain at most one in_progress item".into());
    }

    Ok(SessionPlan {
        explanation: args
            .explanation
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        items,
    })
}

/// Durable sink for plan updates. Implemented by the engine with its session
/// journal; tests use in-memory fakes. Synchronous because journal appends
/// are fast and the executor is already async at the tool boundary.
pub trait PlanJournal: Send + Sync {
    fn append_plan(&mut self, plan: &SessionPlan) -> Result<(), String>;
}

#[derive(Clone)]
pub struct SessionPlanStore {
    inner: Arc<Mutex<SessionPlanState>>,
}

struct SessionPlanState {
    plan: SessionPlan,
    journal: Option<Arc<Mutex<dyn PlanJournal>>>,
}

impl SessionPlanStore {
    pub fn new(plan: SessionPlan, journal: Option<Arc<Mutex<dyn PlanJournal>>>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionPlanState { plan, journal })),
        }
    }

    #[allow(dead_code)]
    pub fn current(&self) -> SessionPlan {
        self.inner.lock().unwrap().plan.clone()
    }

    fn replace(&self, plan: SessionPlan) -> Result<(), String> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| "Session plan state is unavailable".to_string())?;
        if let Some(journal) = &state.journal {
            journal
                .lock()
                .map_err(|_| "Session plan journal is unavailable".to_string())?
                .append_plan(&plan)?;
        }
        state.plan = plan;
        Ok(())
    }
}

pub struct UpdatePlanToolExecutor {
    store: SessionPlanStore,
    event_tx: broadcast::Sender<AgentEvent>,
}

impl UpdatePlanToolExecutor {
    pub fn new(store: SessionPlanStore, event_tx: broadcast::Sender<AgentEvent>) -> Self {
        Self { store, event_tx }
    }
}

#[async_trait]
impl ToolExecutor for UpdatePlanToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.host.update_plan"
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        vec![AgentToolDefinition::new(
            UPDATE_PLAN_TOOL_NAME,
            "Replace the current session plan. Use this tool at the start of multi-step work and after every meaningful milestone: mark the current step in_progress, mark it completed immediately when it succeeds, and set the next step in_progress. Use an empty plan to clear it.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "explanation": {
                        "type": "string",
                        "maxLength": MAX_EXPLANATION_CHARS
                    },
                    "plan": {
                        "type": "array",
                        "maxItems": MAX_PLAN_ITEMS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "step": {
                                    "type": "string",
                                    "minLength": 1,
                                    "maxLength": MAX_STEP_CHARS
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["step", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["plan"],
                "additionalProperties": false
            }),
        )]
        .into()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        if name != UPDATE_PLAN_TOOL_NAME {
            return None;
        }
        let plan = match parse_update_plan(args) {
            Ok(plan) => plan,
            Err(error) => return Some(Err(error)),
        };
        if let Err(error) = self.store.replace(plan.clone()) {
            return Some(Err(error));
        }
        let _ = self
            .event_tx
            .send(AgentEvent::PlanUpdated { plan: plan.clone() });
        Some(Ok(format!(
            "Plan updated with {} item(s).",
            plan.items.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_a_complete_replacement_plan() {
        let plan = parse_update_plan(
            r#"{
                "explanation":"Implement in order",
                "plan":[
                    {"step":"Inspect","status":"completed"},
                    {"step":"Implement","status":"in_progress"},
                    {"step":"Verify","status":"pending"}
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(plan.items.len(), 3);
        assert_eq!(plan.items[1].status, PlanItemStatus::InProgress);
    }

    #[test]
    fn rejects_invalid_plan_updates() {
        for (payload, expected) in [
            (r#"{"plan":[{"step":" ","status":"pending"}]}"#, "non-empty"),
            (
                r#"{"plan":[{"step":"a","status":"in_progress"},{"step":"b","status":"in_progress"}]}"#,
                "one in_progress",
            ),
            (
                &format!(
                    r#"{{"plan":[{{"step":"{}","status":"pending"}}]}}"#,
                    "x".repeat(201)
                ),
                "200 characters",
            ),
        ] {
            assert!(parse_update_plan(payload).unwrap_err().contains(expected));
        }
    }

    struct MemoryJournal {
        plans: Vec<SessionPlan>,
        fail: bool,
    }

    impl PlanJournal for MemoryJournal {
        fn append_plan(&mut self, plan: &SessionPlan) -> Result<(), String> {
            if self.fail {
                return Err("journal unavailable".to_string());
            }
            self.plans.push(plan.clone());
            Ok(())
        }
    }

    fn memory_store(fail: bool) -> (SessionPlanStore, Arc<Mutex<MemoryJournal>>) {
        let journal = Arc::new(Mutex::new(MemoryJournal {
            plans: Vec::new(),
            fail,
        }));
        let store = SessionPlanStore::new(
            Default::default(),
            Some(journal.clone() as Arc<Mutex<dyn PlanJournal>>),
        );
        (store, journal)
    }

    #[tokio::test]
    async fn successful_execution_persists_and_emits_plan_updated() {
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(4);
        let (store, journal) = memory_store(false);
        let executor = UpdatePlanToolExecutor::new(store.clone(), event_tx);

        let result = executor
            .execute_tool(
                UPDATE_PLAN_TOOL_NAME,
                r#"{"plan":[{"step":"Inspect","status":"completed"}]}"#,
            )
            .await
            .unwrap();

        assert!(result.is_ok());
        assert_eq!(journal.lock().unwrap().plans.len(), 1);
        assert_eq!(
            journal.lock().unwrap().plans[0],
            store.current(),
            "journaled plan must match the live plan"
        );
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
                .await
                .unwrap()
                .unwrap(),
            AgentEvent::PlanUpdated { plan } if plan == store.current()
        ));
    }

    #[tokio::test]
    async fn persistence_failure_keeps_the_previous_plan() {
        let previous =
            parse_update_plan(r#"{"plan":[{"step":"Keep me","status":"in_progress"}]}"#).unwrap();
        let (event_tx, _) = tokio::sync::broadcast::channel(4);
        let journal = Arc::new(Mutex::new(MemoryJournal {
            plans: Vec::new(),
            fail: true,
        }));
        let store = SessionPlanStore::new(
            previous.clone(),
            Some(journal as Arc<Mutex<dyn PlanJournal>>),
        );
        let executor = UpdatePlanToolExecutor::new(store.clone(), event_tx);

        let result = executor
            .execute_tool(
                UPDATE_PLAN_TOOL_NAME,
                r#"{"plan":[{"step":"Replace me","status":"completed"}]}"#,
            )
            .await
            .unwrap();

        assert!(result.is_err());
        assert_eq!(store.current(), previous);
    }
}
