use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender as Sender;

use threadlane_protocol::{
    GenerateTitleRequest, ImageAttachment, ReasoningEffort, SendPromptRequest, SessionEvent,
};

use crate::services::sessions::SessionRuntime;
use crate::state::ChatStreamEvent;

/// Returns the shared Tokio runtime used for all daemon-client calls in GPUI.
/// There is exactly one runtime for all daemon I/O; callers in other service
/// modules obtain it through this function.
pub(crate) fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    let rt = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to initialize GPUI async runtime")
    });
    Ok(rt)
}

pub(crate) fn execute_prompt(
    runtime: Arc<SessionRuntime>,
    session_id: String,
    text: String,
    images: Vec<ImageAttachment>,
    reasoning_effort: ReasoningEffort,
    stream_tx: Sender<ChatStreamEvent>,
) -> Result<(), String> {
    runtime.begin_generation();
    let executor = match executor() {
        Ok(executor) => executor,
        Err(error) => {
            runtime.finish_generation();
            return Err(error);
        }
    };

    let task_runtime = runtime.clone();
    let task_session_id = session_id.clone();
    let task_stream_tx = stream_tx.clone();

    executor.spawn(async move {
        let client = match crate::services::daemon_client::get_daemon_client().await {
            Ok(client) => client,
            Err(e) => {
                task_runtime.finish_generation();
                let _ = task_stream_tx.send(ChatStreamEvent::Agent {
                    session_id: task_session_id.clone(),
                    event: SessionEvent::Error { message: e },
                });
                let _ = task_stream_tx.send(ChatStreamEvent::Finished {
                    session_id: task_session_id,
                    session_file: task_runtime.session_file.clone(),
                });
                return;
            }
        };

        let mut events = client.subscribe_session_events();
        if let Err(e) = client.subscribe_session(&task_session_id).await {
            task_runtime.finish_generation();
            let _ = task_stream_tx.send(ChatStreamEvent::Agent {
                session_id: task_session_id.clone(),
                event: SessionEvent::Error { message: e },
            });
            let _ = task_stream_tx.send(ChatStreamEvent::Finished {
                session_id: task_session_id,
                session_file: task_runtime.session_file.clone(),
            });
            return;
        }
        let prompt_res = client
            .send_prompt(SendPromptRequest {
                session_id: task_session_id.clone(),
                prompt: text,
                images,
                reasoning_effort: Some(reasoning_effort),
            })
            .await;

        if let Err(e) = prompt_res {
            task_runtime.finish_generation();
            let _ = task_stream_tx.send(ChatStreamEvent::Agent {
                session_id: task_session_id.clone(),
                event: SessionEvent::Error { message: e },
            });
            let _ = task_stream_tx.send(ChatStreamEvent::Finished {
                session_id: task_session_id,
                session_file: task_runtime.session_file.clone(),
            });
            return;
        }

        while let Ok(event) = events.recv().await {
            let is_terminal = matches!(
                event,
                SessionEvent::SessionCompleted { .. } | SessionEvent::Error { .. }
            );
            let _ = task_stream_tx.send(ChatStreamEvent::Agent {
                session_id: task_session_id.clone(),
                event: event.clone(),
            });
            if is_terminal {
                break;
            }
        }

        task_runtime.finish_generation();
        let _ = task_stream_tx.send(ChatStreamEvent::Finished {
            session_id: task_session_id,
            session_file: task_runtime.session_file.clone(),
        });
    });

    Ok(())
}

pub(crate) fn cancel_prompt(
    runtime: Arc<SessionRuntime>,
    session_id: String,
    stream_tx: Sender<ChatStreamEvent>,
) -> Result<(), String> {
    runtime.finish_generation();
    let task_session_id = session_id.clone();
    let task_file = runtime.session_file.clone();
    if let Ok(executor) = executor() {
        executor.spawn(async move {
            if let Ok(client) = crate::services::daemon_client::get_daemon_client().await {
                let _ = client.cancel_run(&task_session_id).await;
            }
        });
    }
    let _ = stream_tx.send(ChatStreamEvent::Agent {
        session_id: session_id.clone(),
        event: SessionEvent::Error {
            message: "Generation cancelled".into(),
        },
    });
    let _ = stream_tx.send(ChatStreamEvent::Finished {
        session_id,
        session_file: task_file,
    });
    Ok(())
}

/// Generate a session title via the daemon and notify the UI on completion.
pub(crate) fn maybe_generate_session_title(
    session_file: PathBuf,
    session_id: String,
    submitted_prompt: String,
    work_dir: PathBuf,
    stream_tx: Sender<ChatStreamEvent>,
) {
    if let Ok(executor) = executor() {
        let tx = stream_tx.clone();
        let file = session_file.clone();
        let sid = session_id.clone();
        let prompt = submitted_prompt.clone();
        let project = work_dir.to_string_lossy().to_string();

        executor.spawn(async move {
            if let Ok(client) = crate::services::daemon_client::get_daemon_client().await {
                let _ = client
                    .generate_title(GenerateTitleRequest {
                        session_id: sid.clone(),
                        project_path: project,
                        prompt,
                    })
                    .await;
            }
            let _ = tx.send(ChatStreamEvent::TitleGenerated {
                session_id: sid,
                session_file: file,
            });
        });
    } else {
        let _ = stream_tx.send(ChatStreamEvent::TitleGenerated {
            session_id,
            session_file,
        });
    }
}

/// Normalise an LLM-generated title. Delegates to the shared protocol helper.
#[cfg(test)]
pub fn normalize_session_title(value: &str) -> String {
    threadlane_protocol::normalize_session_title(value)
}

pub fn load_acp_config_options(
    _runtime: Arc<SessionRuntime>,
    _session_id: String,
    _tx: tokio::sync::mpsc::UnboundedSender<ChatStreamEvent>,
) -> Result<(), String> {
    Ok(())
}

pub fn set_acp_config_option(
    _runtime: Arc<SessionRuntime>,
    _session_id: String,
    _config_id: String,
    _value: String,
    _tx: tokio::sync::mpsc::UnboundedSender<ChatStreamEvent>,
) -> Result<(), String> {
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
