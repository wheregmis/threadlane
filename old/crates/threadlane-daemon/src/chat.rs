use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender as Sender;

use threadlane_runtime::harness::{JsonlStore, SessionStore};
use threadlane_coding_agent::credentials::provider_client_for;
use threadlane_protocol::{AgentEvent, ImageAttachment, ReasoningEffort};

use threadlane_coding_agent::controller::SessionRuntime;
use crate::SessionEvent;

pub fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    threadlane_provider::exec::try_get_runtime()
}

pub fn execute_prompt(
    runtime: Arc<SessionRuntime>,
    work_dir: PathBuf,
    session_id: String,
    text: String,
    images: Vec<ImageAttachment>,
    reasoning_effort: ReasoningEffort,
    stream_tx: Sender<SessionEvent>,
    pending_acp: Vec<(String, String)>,
) -> Result<(), String> {
    // Turn-driving policy lives on the controller; this adapter only maps the
    // engine sinks onto chat stream events.
    let event_session_id = session_id.clone();
    let event_tx = stream_tx.clone();
    let on_agent_event = move |event| {
        let _ = event_tx.send(SessionEvent::Agent {
            session_id: event_session_id.clone(),
            event,
        });
    };
    let output_session_id = session_id.clone();
    let output_tx = stream_tx.clone();
    let on_output_text = move |output: String| {
        let _ = output_tx.send(SessionEvent::Agent {
            session_id: output_session_id.clone(),
            event: AgentEvent::MessageUpdate {
                text_delta: Some(output),
                reasoning_delta: None,
                tool_call_name: None,
            },
        });
    };
    let acp_session_id = session_id.clone();
    let acp_tx = stream_tx.clone();
    let acp_session_file = runtime.session_file.clone();
    let acp_runtime_instance = runtime.instance_id();
    let on_acp_options = move |options, error, failed_config| {
        let _ = acp_tx.send(SessionEvent::AcpConfigOptions {
            session_id: acp_session_id.clone(),
            session_file: acp_session_file.clone(),
            runtime_instance: acp_runtime_instance,
            options,
            error,
            failed_config,
        });
    };
    let finished_file = runtime.session_file.clone();
    let on_finished = move || {
        let _ = stream_tx.send(SessionEvent::Finished {
            session_id: session_id.clone(),
            session_file: finished_file.clone(),
        });
    };
    runtime.spawn_interactive_turn(
        text,
        images,
        reasoning_effort,
        work_dir,
        pending_acp,
        on_agent_event,
        on_output_text,
        on_acp_options,
        on_finished,
    )
}

/// Asks the session's external agent what settings it offers.
///
/// Starting the agent is the point: it reports its settings on `session/new`,
/// so opening the picker before the first turn is the only way to find out what
/// it offers. After a turn they arrive free from `execute_prompt`.
pub fn load_acp_config_options(
    runtime: Arc<SessionRuntime>,
    session_id: String,
    stream_tx: Sender<SessionEvent>,
) -> Result<(), String> {
    spawn_acp_config_task(runtime, session_id, stream_tx, |runtime| async move {
        runtime.acp_config_options().await
    })
}

/// Applies one of the agent's own settings and reports what it holds afterwards.
pub fn set_acp_config_option(
    runtime: Arc<SessionRuntime>,
    session_id: String,
    config_id: String,
    value: String,
    stream_tx: Sender<SessionEvent>,
) -> Result<(), String> {
    spawn_acp_config_task(runtime, session_id, stream_tx, move |runtime| async move {
        runtime.set_acp_config_option(&config_id, &value).await
    })
}

/// Runs one agent-settings operation off the UI thread.
///
/// Refuses while a turn is running rather than awaiting the lock: a turn holds
/// the agent for its whole duration, so waiting would hang the picker with no
/// feedback instead of failing.
fn spawn_acp_config_task<F, Fut>(
    runtime: Arc<SessionRuntime>,
    session_id: String,
    stream_tx: Sender<SessionEvent>,
    operation: F,
) -> Result<(), String>
where
    F: FnOnce(Arc<SessionRuntime>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<Vec<threadlane_acp::AcpConfigOption>, String>>
        + Send,
{
    if runtime.is_generating() {
        return Err("Stop the current turn before changing the agent's settings".into());
    }
    executor()?.spawn(async move {
        let session_file = runtime.session_file.clone();
        let runtime_instance = runtime.instance_id();
        let (options, error) = match operation(runtime).await {
            Ok(options) => (options, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let _ = stream_tx.send(SessionEvent::AcpConfigOptions {
            session_id,
            session_file,
            runtime_instance,
            options,
            error,
            failed_config: None,
        });
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn maybe_generate_session_title(
    session_file: PathBuf,
    session_id: String,
    submitted_prompt: String,
    api_key: String,
    account_id: Option<String>,
    model: String,
    work_dir: PathBuf,
    stream_tx: Sender<SessionEvent>,
) {
    let mut store = match JsonlStore::open(&session_file) {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(
                "unable to load session {} for automatic title generation ({}): {}",
                session_id,
                session_file.display(),
                error
            );
            return;
        }
    };
    if store.has_name() || submitted_prompt.trim().is_empty() {
        return;
    }
    match store.mark_title_attempted() {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            tracing::warn!(
                "unable to persist automatic title attempt for session {}: {}",
                session_id,
                error
            );
            return;
        }
    }

    let Ok(executor) = executor() else {
        return;
    };
    executor.spawn(async move {
        let result = async {
            // Title generation follows the selected model. Routing an ACP
            // model through ProviderClient would fall through to OpenAI and
            // fail with a 401, because an ACP agent has no provider key.
            let raw = match threadlane_acp_engine::acp_agent_id(&model) {
                Some(agent_id) => {
                    threadlane_acp_engine::generate_title(
                        threadlane_project::default_global_threadlane_dir(),
                        work_dir,
                        agent_id,
                        &submitted_prompt,
                    )
                    .await?
                }
                None => {
                    provider_client_for(api_key, account_id)
                        .generate_title(&model, &submitted_prompt)
                        .await?
                }
            };
            let title = normalize_session_title(&raw);
            if title.is_empty() {
                return Err("title normalization produced an empty title".to_string());
            }
            let mut store = JsonlStore::open(&session_file)
                .map_err(|error| format!("reload failed: {error}"))?;
            if store.has_name() {
                return Err("session was named while title generation was running".to_string());
            }
            store
                .set_name(&title)
                .map_err(|error| format!("persistence failed: {error}"))
        }
        .await;

        if let Err(error) = result {
            tracing::warn!(
                "automatic title generation failed for session {}: {}",
                session_id,
                error
            );
            return;
        }
        let _ = stream_tx.send(SessionEvent::TitleGenerated {
            session_id,
            session_file,
        });
    });
}

    use threadlane_runtime::titles::normalize_session_title;

pub fn cancel_prompt(
    runtime: Arc<SessionRuntime>,
    session_id: String,
    stream_tx: Sender<SessionEvent>,
) -> Result<(), String> {
    runtime.cancel()?;
    runtime.finish_generation(None);
    let _ = stream_tx.send(SessionEvent::Agent {
        session_id: session_id.clone(),
        event: AgentEvent::AgentError {
            error: "Generation cancelled".into(),
        },
    });
    let _ = stream_tx.send(SessionEvent::Finished {
        session_id,
        session_file: runtime.session_file.clone(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
use threadlane_runtime::titles::normalize_session_title;

    #[test]
    fn title_normalization_matches_native_behavior() {
        assert_eq!(
            normalize_session_title("  \"Title:   Wire automatic titles  \" "),
            "Wire automatic titles"
        );
        assert_eq!(
            normalize_session_title(
                "A title that is deliberately much longer than forty-two characters"
            ),
            "A title that is deliberately much longer t"
        );
    }
}

/// A side request: never adds messages or permits tool execution.
pub async fn generate_text(
    model: String,
    work_dir: PathBuf,
    instruction: String,
    prompt: String,
) -> Result<String, String> {
    if model.trim().is_empty() {
        return Err("Select a model before regenerating text.".into());
    }
    if let Some(agent_id) = threadlane_acp_engine::acp_agent_id(&model) {
        threadlane_acp_engine::generate_text(
            threadlane_project::default_global_threadlane_dir(),
            work_dir,
            agent_id,
            &instruction,
            &prompt,
        )
        .await
    } else {
        let (key, account) = threadlane_coding_agent::credentials::provider_credentials(&model);
        provider_client_for(key, account)
            .generate_text(&model, &instruction, &prompt)
            .await
    }
}

pub fn conversation_title_prompt(messages: &[crate::ChatMessageInfo]) -> String {
    let text = messages
        .iter()
        .filter(|m| {
            matches!(
                m.role,
                crate::MessageRole::User | crate::MessageRole::Assistant
            )
        })
        .map(|m| format!("{:?}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n\n");
    // Keep recent conversation, not reasoning or tool output.
    let skip = text.chars().count().saturating_sub(24_000);
    text.chars().skip(skip).collect()
}

pub fn persist_regenerated_title(
    session_file: &std::path::Path,
    raw: &str,
) -> Result<String, String> {
    let title = normalize_session_title(raw);
    if title.is_empty() {
        return Err("The model returned an empty title.".into());
    }
    if !session_file.is_file() {
        return Err("The session no longer exists.".into());
    }
    let mut store = JsonlStore::open(session_file).map_err(|e| e.to_string())?;
    store.set_name(&title).map_err(|e| e.to_string())?;
    Ok(title)
}

#[cfg(test)]
mod regeneration_tests {
    use super::{conversation_title_prompt, persist_regenerated_title};
    use crate::{ChatMessageInfo, MessageRole};
    use threadlane_runtime::harness::{JsonlStore, SessionStore};

    fn message(role: MessageRole, content: String) -> ChatMessageInfo {
        ChatMessageInfo {
            id: "message".into(),
            role,
            content,
            tool_activities: vec![],
            streaming: false,
            reasoning_content: Some("private reasoning".into()),
            reasoning_expanded: false,
            retry_prompt: None,
        }
    }

    #[test]
    fn regeneration_context_keeps_recent_conversation_and_excludes_internal_messages() {
        let messages = vec![
            message(MessageRole::User, "é".repeat(30_000)),
            message(MessageRole::System, "hidden system data".into()),
            message(MessageRole::Assistant, "Latest answer".into()),
            message(MessageRole::User, "Latest request".into()),
        ];
        let prompt = conversation_title_prompt(&messages);
        assert_eq!(prompt.chars().count(), 24_000);
        assert!(prompt.contains("Assistant: Latest answer"));
        assert!(prompt.ends_with("User: Latest request"));
        assert!(!prompt.contains("hidden system data"));
        assert!(!prompt.contains("private reasoning"));
    }

    #[test]
    fn regenerated_title_replaces_existing_name_but_rejects_empty_or_deleted_session() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();
        store.set_name("Old title").unwrap();
        drop(store);
        let title = persist_regenerated_title(&path, "New title").unwrap();
        assert_eq!(
            JsonlStore::open_read_only(&path).unwrap().name(),
            Some(title)
        );
        assert!(persist_regenerated_title(&path, "  ").is_err());
        assert_eq!(
            JsonlStore::open_read_only(&path).unwrap().name().as_deref(),
            Some("New title")
        );
        std::fs::remove_file(&path).unwrap();
        assert!(persist_regenerated_title(&path, "Should not recreate").is_err());
        assert!(!path.exists());
    }
}
