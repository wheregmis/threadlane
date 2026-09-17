use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender as Sender;

use threadlane_runtime::harness::{JsonlStore, SessionStore};
use threadlane_coding_agent::credentials::provider_client_for;
use threadlane_protocol::{AgentEvent, ImageAttachment, ReasoningEffort};

use threadlane_coding_agent::controller::SessionRuntime;
use crate::ChatStreamEvent;

pub fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    Ok(threadlane_provider::exec::get_runtime())
}

pub(crate) fn execute_prompt(
    runtime: Arc<SessionRuntime>,
    work_dir: PathBuf,
    session_id: String,
    text: String,
    images: Vec<ImageAttachment>,
    reasoning_effort: ReasoningEffort,
    stream_tx: Sender<ChatStreamEvent>,
    pending_acp: Vec<(String, String)>,
) -> Result<(), String> {
    // Turn-driving policy lives on the controller; this adapter only maps the
    // engine sinks onto chat stream events.
    let event_session_id = session_id.clone();
    let event_tx = stream_tx.clone();
    let on_agent_event = move |event| {
        let _ = event_tx.send(ChatStreamEvent::Agent {
            session_id: event_session_id.clone(),
            event,
        });
    };
    let output_session_id = session_id.clone();
    let output_tx = stream_tx.clone();
    let on_output_text = move |output: String| {
        let _ = output_tx.send(ChatStreamEvent::Agent {
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
    let acp_source = Arc::downgrade(&runtime);
    let on_acp_options = move |options, error, failed_config| {
        let _ = acp_tx.send(ChatStreamEvent::AcpConfigOptions {
            session_id: acp_session_id.clone(),
            source: acp_source.clone(),
            options,
            error,
            failed_config,
        });
    };
    let finished_file = runtime.session_file.clone();
    let on_finished = move || {
        let _ = stream_tx.send(ChatStreamEvent::Finished {
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
pub(crate) fn load_acp_config_options(
    runtime: Arc<SessionRuntime>,
    session_id: String,
    stream_tx: Sender<ChatStreamEvent>,
) -> Result<(), String> {
    spawn_acp_config_task(runtime, session_id, stream_tx, |runtime| async move {
        runtime.acp_config_options().await
    })
}

/// Applies one of the agent's own settings and reports what it holds afterwards.
pub(crate) fn set_acp_config_option(
    runtime: Arc<SessionRuntime>,
    session_id: String,
    config_id: String,
    value: String,
    stream_tx: Sender<ChatStreamEvent>,
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
    stream_tx: Sender<ChatStreamEvent>,
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
        let source = Arc::downgrade(&runtime);
        let (options, error) = match operation(runtime).await {
            Ok(options) => (options, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let _ = stream_tx.send(ChatStreamEvent::AcpConfigOptions {
            session_id,
            source,
            options,
            error,
            failed_config: None,
        });
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn maybe_generate_session_title(
    session_file: PathBuf,
    session_id: String,
    submitted_prompt: String,
    api_key: String,
    account_id: Option<String>,
    model: String,
    work_dir: PathBuf,
    stream_tx: Sender<ChatStreamEvent>,
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
        let _ = stream_tx.send(ChatStreamEvent::TitleGenerated {
            session_id,
            session_file,
        });
    });
}

    use threadlane_runtime::titles::normalize_session_title;

pub(crate) fn cancel_prompt(
    runtime: Arc<SessionRuntime>,
    session_id: String,
    stream_tx: Sender<ChatStreamEvent>,
) -> Result<(), String> {
    runtime.cancel()?;
    runtime.finish_generation(Some("Generation cancelled".into()));
    let _ = stream_tx.send(ChatStreamEvent::Agent {
        session_id: session_id.clone(),
        event: AgentEvent::AgentError {
            error: "Generation cancelled".into(),
        },
    });
    let _ = stream_tx.send(ChatStreamEvent::Finished {
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
