use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender as Sender;

use threadlane_provider::ProviderClient;
use threadlane_session::harness::{JsonlStore, SessionStore};
use threadlane_session::{AgentEvent, ImageAttachment, ReasoningEffort};

use crate::services::sessions::SessionRuntime;
use crate::state::ChatStreamEvent;

pub(crate) fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    Ok(threadlane_runtime::get_runtime())
}

struct RunCleanup {
    runtime: Arc<SessionRuntime>,
    registration_id: u64,
    session_id: String,
    stream_tx: Sender<ChatStreamEvent>,
    error: Option<String>,
}

impl Drop for RunCleanup {
    fn drop(&mut self) {
        self.runtime
            .cancellation
            .finish_active_run(self.registration_id);
        self.runtime.finish_generation(self.error.clone());
        let _ = self.stream_tx.send(ChatStreamEvent::Finished {
            session_id: self.session_id.clone(),
            session_file: self.runtime.session_file.clone(),
        });
    }
}

pub(crate) fn execute_prompt(
    runtime: Arc<SessionRuntime>,
    session_id: String,
    text: String,
    images: Vec<ImageAttachment>,
    reasoning_effort: ReasoningEffort,
    stream_tx: Sender<ChatStreamEvent>,
) -> Result<(), String> {
    runtime.begin_generation()?;
    let executor = match executor() {
        Ok(executor) => executor,
        Err(error) => {
            runtime.finish_generation(Some(error.clone()));
            return Err(error);
        }
    };

    let task_runtime = runtime.clone();
    let task_session_id = session_id.clone();
    let task_stream_tx = stream_tx.clone();
    let (registration_tx, registration_rx) = tokio::sync::oneshot::channel();
    let task = executor.spawn(async move {
        let Ok(registration_id) = registration_rx.await else {
            task_runtime.finish_generation(Some("Generation registration failed".into()));
            return;
        };

        let turn_span = tracing::info_span!("chat.turn", session_id = %task_session_id);
        tracing::info!(parent: &turn_span, "starting chat turn");
        let mut cleanup = RunCleanup {
            runtime: task_runtime.clone(),
            registration_id,
            session_id: task_session_id.clone(),
            stream_tx: task_stream_tx.clone(),
            error: None,
        };
        let mut agent = task_runtime.agent.lock().await;
        agent.set_reasoning_effort(reasoning_effort).await;
        let mut events = agent.subscribe();
        let run_error = {
            let run = agent.handle_input_with_images(&text, images);
            tokio::pin!(run);
            let mut run_error = None;
            let mut saw_agent_error = false;

            loop {
                tokio::select! {
                    result = &mut run => {
                        match result {
                            Some(Ok(output)) if !output.is_empty() => {
                                let _ = task_stream_tx.send(ChatStreamEvent::Agent {
                                    session_id: task_session_id.clone(),
                                    event: AgentEvent::MessageUpdate {
                                        text_delta: Some(output),
                                        reasoning_delta: None,
                                        tool_call_name: None,
                                    },
                                });
                            }
                            Some(Err(error)) => {
                                tracing::error!(error = %error, "chat turn failed");
                                run_error = Some(error);
                            }
                            _ => {}
                        }
                        while let Ok(event) = events.try_recv() {
                            saw_agent_error |= matches!(event, AgentEvent::AgentError { .. });
                            let _ = task_stream_tx.send(ChatStreamEvent::Agent {
                                session_id: task_session_id.clone(),
                                event,
                            });
                        }
                        if let Some(error) = run_error.as_ref().filter(|_| !saw_agent_error) {
                            let _ = task_stream_tx.send(ChatStreamEvent::Agent {
                                session_id: task_session_id.clone(),
                                event: AgentEvent::AgentError { error: error.clone() },
                            });
                        }
                        break;
                    }
                    event = events.recv() => {
                        match event {
                            Ok(event) => {
                                saw_agent_error |= matches!(event, AgentEvent::AgentError { .. });
                                let _ = task_stream_tx.send(ChatStreamEvent::Agent {
                                    session_id: task_session_id.clone(),
                                    event,
                                });
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
            run_error
        };

        tracing::info!(error = ?run_error, "chat turn finished");
        // Read after the turn because an external agent defines its own
        // settings and only reports them once a session is open. This is the
        // free path: the turn already connected, so nothing is started here.
        let acp_options = agent.acp_user_config_options();
        if !acp_options.is_empty() {
            let _ = task_stream_tx.send(ChatStreamEvent::AcpConfigOptions {
                session_id: task_session_id.clone(),
                options: acp_options,
                error: None,
            });
        }
        drop(agent);
        cleanup.error = run_error;
    });

    let registration_id = match runtime.cancellation.track_active_run(task.abort_handle()) {
        Ok(id) => id,
        Err(error) => {
            task.abort();
            runtime.finish_generation(Some(error.clone()));
            return Err(error);
        }
    };
    if registration_tx.send(registration_id).is_err() {
        runtime.cancellation.finish_active_run(registration_id);
        let error = "Generation task stopped before registration".to_string();
        runtime.finish_generation(Some(error.clone()));
        return Err(error);
    }
    Ok(())
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
    Fut: std::future::Future<Output = Result<Vec<threadlane_session::AcpConfigOption>, String>>
        + Send,
{
    if runtime.is_generating() {
        return Err("Stop the current turn before changing the agent's settings".into());
    }
    executor()?.spawn(async move {
        let (options, error) = match operation(runtime).await {
            Ok(options) => (options, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let _ = stream_tx.send(ChatStreamEvent::AcpConfigOptions {
            session_id,
            options,
            error,
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
            let raw = match threadlane_session::acp_agent_id(&model) {
                Some(agent_id) => {
                    threadlane_session::acp_runtime::generate_title(
                        threadlane_session::default_global_threadlane_dir(),
                        work_dir,
                        agent_id,
                        &submitted_prompt,
                    )
                    .await?
                }
                None => {
                    ProviderClient::new(api_key, account_id)
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

fn normalize_session_title(value: &str) -> String {
    let mut title = value.trim().to_string();
    loop {
        let before = title.clone();
        if title
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("title:"))
        {
            title = title[6..].trim().to_string();
        }
        let quoted = ((title.starts_with('"') && title.ends_with('"'))
            || (title.starts_with('\'') && title.ends_with('\'')))
            && title.len() >= 2;
        if quoted {
            title = title[1..title.len() - 1].trim().to_string();
        }
        if title == before {
            break;
        }
    }

    let mut collapsed = String::with_capacity(title.len());
    let mut previous_was_space = true;
    for character in title.chars() {
        if character.is_whitespace() {
            if !previous_was_space {
                collapsed.push(' ');
                previous_was_space = true;
            }
        } else {
            collapsed.push(character);
            previous_was_space = false;
        }
    }
    if collapsed.ends_with(' ') {
        collapsed.pop();
    }
    collapsed.chars().take(42).collect()
}

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
    use super::normalize_session_title;

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
