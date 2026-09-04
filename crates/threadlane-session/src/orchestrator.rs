//! Entrypoint Prewalk Orchestration (inspired by Oh My Pi).
//!
//! Uses a fast model intent classifier to distinguish casual conversation / queries
//! from actionable coding tasks. For actionable tasks, engages the Frontier Architect
//! protocol via system instructions (without polluting the user message bubble), and
//! seamlessly hands off to the target fast model upon landing the pivotal first code edit.

use std::sync::Arc;
use threadlane_protocol::ProviderPort;
use threadlane_runtime::{OrchestratorMode, ReasoningEffort};

#[derive(Debug)]
pub(crate) struct PrewalkState {
    pub(crate) target_model: String,
    pub(crate) target_reasoning: Option<ReasoningEffort>,
    pub(crate) started_at: std::time::Instant,
}

pub const ARCHITECT_PROTOCOL_HEADER: &str =
    "[ARCHITECT PROTOCOL: Frontier Architect -> Fast Model Handoff]";
pub const ARCHITECT_PROTOCOL_FOOTER: &str = "[END ARCHITECT PROTOCOL]";

/// Orchestrator decision for an incoming prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrchestratorDecision {
    /// Execute directly with active model without Prewalk.
    DirectExecution,
    /// Engage Prewalk: frontier architect explores and lands first edit via system directive, then hands off to fast model.
    EngagePrewalk {
        fast_model: String,
        fast_reasoning: Option<ReasoningEffort>,
        architect_system_directive: String,
    },
}

pub struct Orchestrator;

impl Orchestrator {
    pub async fn evaluate(
        prompt: &str,
        mode: OrchestratorMode,
        active_model: &str,
        fast_model: &str,
        fast_reasoning: Option<ReasoningEffort>,
        provider_client: Option<Arc<dyn ProviderPort>>,
    ) -> OrchestratorDecision {
        // If the fast model is identical to the active model, or orchestration is off, run directly.
        if active_model == fast_model || mode == OrchestratorMode::Off {
            return OrchestratorDecision::DirectExecution;
        }

        let task_prompt = prompt.trim();
        if task_prompt.is_empty() {
            return OrchestratorDecision::DirectExecution;
        }

        // In Auto mode, query the model to determine whether this is an actionable coding task or a query.
        if mode == OrchestratorMode::Auto {
            let is_task = if let Some(client) = provider_client {
                classify_task_intent_with_model(client, fast_model, fast_reasoning, task_prompt)
                    .await
            } else {
                false
            };

            if !is_task {
                return OrchestratorDecision::DirectExecution;
            }
        }

        let architect_system_directive = build_architect_directive(fast_model);

        OrchestratorDecision::EngagePrewalk {
            fast_model: fast_model.to_string(),
            fast_reasoning,
            architect_system_directive,
        }
    }
}

fn parse_classifier_label(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case("task")
}

/// LLM-based intent classifier using the configured fast model.
pub async fn classify_task_intent_with_model(
    provider_client: Arc<dyn ProviderPort>,
    fast_model: &str,
    reasoning_effort: Option<ReasoningEffort>,
    prompt: &str,
) -> bool {
    use std::time::Duration;
    use threadlane_protocol::{RuntimeRequest, RuntimeStreamEvent};
    use tokio::sync::mpsc;

    let (tx, mut rx) = mpsc::channel(16);
    let request = RuntimeRequest {
        model: fast_model.to_string(),
        messages: serde_json::json!([
            {
                "role": "system",
                "content": "You are a prompt intent classifier. Determine whether the user's input is a general query, explanation request, greeting, conversation, or discussion ('query'), or an actionable coding, implementation, refactoring, bug-fixing, or codebase editing task ('task'). Respond with ONLY 'query' or 'task'."
            },
            {
                "role": "user",
                "content": prompt
            }
        ]),
        tools: serde_json::json!([]),
        prompt_cache_key: None,
        reasoning_effort: reasoning_effort.map(|effort| effort.label().to_ascii_lowercase()),
    };

    let started_at = std::time::Instant::now();
    let request_task = tokio::spawn(async move {
        provider_client.stream_request(request, tx).await;
    });

    let receive_result = tokio::time::timeout(Duration::from_millis(2500), async {
        let mut text = String::new();
        while let Some(event) = rx.recv().await {
            match event {
                RuntimeStreamEvent::ContentToken(token) => {
                    text.push_str(&token);
                    match text.trim().to_ascii_lowercase().as_str() {
                        "task" => return true,
                        "query" => return false,
                        _ => {}
                    }
                }
                RuntimeStreamEvent::Finished { .. } | RuntimeStreamEvent::Error(_) => break,
                _ => {}
            }
        }
        parse_classifier_label(&text)
    })
    .await;
    request_task.abort();

    let (is_task, success) = match receive_result {
        Ok(is_task) => (is_task, true),
        Err(_) => (false, false),
    };
    log::info!(
        "orchestrator classification_ms={} decision={} success={success}",
        started_at.elapsed().as_millis(),
        if is_task { "task" } else { "query" },
    );
    is_task
}

pub fn build_architect_directive(fast_model: &str) -> String {
    let handoff_tool = crate::coding_agent::capabilities::PREWALK_HANDOFF_TOOL_NAME;
    format!(
        "\n\n{ARCHITECT_PROTOCOL_HEADER}\n\
         Target Fast Model: {fast_model}\n\
         Inspect the relevant code, make one foundational working change, and verify it.\n\
         Then call `{handoff_tool}` to hand the remaining work to {fast_model}.\n\
         {ARCHITECT_PROTOCOL_FOOTER}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::pending;
    use std::sync::atomic::{AtomicBool, Ordering};
    use threadlane_protocol::{DeferredResponse, RuntimeRequest, RuntimeStreamEvent};

    struct HangingClassifier {
        cancelled: Arc<AtomicBool>,
    }

    struct RecordingClassifier {
        effort: Arc<std::sync::Mutex<Option<Option<String>>>>,
    }

    struct CancellationGuard(Arc<AtomicBool>);

    impl Drop for CancellationGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl ProviderPort for HangingClassifier {
        async fn stream_request(
            &self,
            _request: RuntimeRequest,
            events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
        ) {
            let _guard = CancellationGuard(self.cancelled.clone());
            events
                .send(RuntimeStreamEvent::ContentToken("task".into()))
                .await
                .unwrap();
            pending::<()>().await;
        }

        async fn fetch_deferred(
            &self,
            _model: &str,
            _handle_id: &str,
        ) -> Result<DeferredResponse, String> {
            unreachable!()
        }

        async fn cancel_deferred(&self, _model: &str, _handle_id: &str) -> Result<(), String> {
            unreachable!()
        }

        fn provider_kind(&self, _model: &str) -> &'static str {
            "test"
        }
    }

    #[async_trait::async_trait]
    impl ProviderPort for RecordingClassifier {
        async fn stream_request(
            &self,
            request: RuntimeRequest,
            events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
        ) {
            *self.effort.lock().unwrap() = Some(request.reasoning_effort);
            let _ = events
                .send(RuntimeStreamEvent::ContentToken("query".into()))
                .await;
        }

        async fn fetch_deferred(
            &self,
            _model: &str,
            _handle_id: &str,
        ) -> Result<DeferredResponse, String> {
            unreachable!()
        }

        async fn cancel_deferred(&self, _model: &str, _handle_id: &str) -> Result<(), String> {
            unreachable!()
        }

        fn provider_kind(&self, _model: &str) -> &'static str {
            "test"
        }
    }

    #[test]
    fn classifier_label_must_be_exact() {
        assert!(parse_classifier_label(" task\n"));
        assert!(!parse_classifier_label("query"));
        assert!(!parse_classifier_label("not a task"));
        assert!(!parse_classifier_label("task because this needs an edit"));
    }

    #[test]
    fn architect_directive_uses_explicit_handoff_signal() {
        let directive = build_architect_directive("fast-model");

        assert!(directive.contains(crate::coding_agent::capabilities::PREWALK_HANDOFF_TOOL_NAME));
        assert!(directive.contains("verify"));
        assert!(!directive.contains("read_file"));
        assert!(!directive.contains("CONCISE CHECKLIST"));
    }

    #[tokio::test]
    async fn classifier_returns_and_cancels_after_complete_label() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let client = Arc::new(HangingClassifier {
            cancelled: cancelled.clone(),
        });

        let is_task = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            classify_task_intent_with_model(client, "fast", None, "fix it"),
        )
        .await
        .expect("classifier should not wait for stream closure");

        assert!(is_task);
        tokio::task::yield_now().await;
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn classifier_forwards_fast_model_reasoning_effort() {
        let effort = Arc::new(std::sync::Mutex::new(None));
        let client = Arc::new(RecordingClassifier {
            effort: effort.clone(),
        });

        classify_task_intent_with_model(
            client,
            "antigravity/gemini-3.7-flash",
            Some(ReasoningEffort::Medium),
            "hello",
        )
        .await;

        assert_eq!(*effort.lock().unwrap(), Some(Some("medium".into())));
    }

    #[tokio::test]
    async fn test_orchestrator_decision_off_or_same_model() {
        let decision = Orchestrator::evaluate(
            "fix the bug in lib.rs",
            OrchestratorMode::Off,
            "gemini-pro",
            "gemini-flash",
            None,
            None,
        )
        .await;
        assert_eq!(decision, OrchestratorDecision::DirectExecution);

        let decision_same = Orchestrator::evaluate(
            "fix the bug in lib.rs",
            OrchestratorMode::Auto,
            "gemini-flash",
            "gemini-flash",
            None,
            None,
        )
        .await;
        assert_eq!(decision_same, OrchestratorDecision::DirectExecution);

        let decision_empty = Orchestrator::evaluate(
            "   ",
            OrchestratorMode::Auto,
            "gemini-pro",
            "gemini-flash",
            None,
            None,
        )
        .await;
        assert_eq!(decision_empty, OrchestratorDecision::DirectExecution);
    }

    #[tokio::test]
    async fn test_orchestrator_decision_always_mode() {
        let decision = Orchestrator::evaluate(
            "Fix the concurrency bug in session store",
            OrchestratorMode::Always,
            "gemini-pro",
            "gemini-flash",
            Some(ReasoningEffort::Low),
            None,
        )
        .await;

        match decision {
            OrchestratorDecision::EngagePrewalk {
                fast_model,
                fast_reasoning,
                architect_system_directive,
            } => {
                assert_eq!(fast_model, "gemini-flash");
                assert_eq!(fast_reasoning, Some(ReasoningEffort::Low));
                assert!(architect_system_directive.contains(ARCHITECT_PROTOCOL_HEADER));
                assert!(architect_system_directive.contains("Target Fast Model: gemini-flash"));
            }
            _ => panic!("expected EngagePrewalk"),
        }
    }
}
