use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use threadlane_protocol::permission::*;
use threadlane_protocol::session::*;
use threadlane_session::system_prompt::SystemPromptConfig;
use threadlane_session::{CodingAgent, CodingAgentOptions};
use tokio::sync::broadcast;
use tracing::info;

struct ActiveSession {
    agent: CodingAgent,
    summary: SessionSummary,
    broadcaster: broadcast::Sender<SessionEvent>,
}

#[derive(Clone)]
pub struct SessionService {
    sessions: Arc<tokio::sync::Mutex<HashMap<String, ActiveSession>>>,
}

impl Default for SessionService {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionService {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    pub async fn create_session(
        &self,
        req: CreateSessionRequest,
    ) -> Result<SessionSummary, String> {
        let session_id = req
            .session_id
            .unwrap_or_else(|| format!("sess_{}", uuid_v4_like()));
        let project_path = PathBuf::from(&req.project_path);
        let model = req
            .model
            .unwrap_or_else(|| "antigravity/gemini-3.7-flash".to_string());
        let title = req.title.unwrap_or_else(|| "New Session".to_string());
        let now = chrono_iso_now();

        // Build session file path
        let sessions_dir = project_path.join(".threadlane").join("sessions");
        let _ = std::fs::create_dir_all(&sessions_dir);
        let session_file = sessions_dir.join(format!("{session_id}.jsonl"));

        let options = CodingAgentOptions {
            api_key: String::new(),
            account_id: None,
            model: model.clone(),
            work_dir: project_path.clone(),
            session_file: Some(session_file),
            system_prompt: SystemPromptConfig::default(),
            agent_config: None,
            coding_config: None,
        };

        let agent = CodingAgent::new(options);
        let (broadcaster, _) = broadcast::channel(1024);

        let summary = SessionSummary {
            session_id: session_id.clone(),
            project_path: req.project_path,
            title,
            model,
            created_at: now.clone(),
            updated_at: now,
            is_active: true,
        };

        let mut lock = self.sessions.lock().await;
        lock.insert(
            session_id.clone(),
            ActiveSession {
                agent,
                summary: summary.clone(),
                broadcaster,
            },
        );

        Ok(summary)
    }

    pub async fn list_sessions(
        &self,
        req: ListSessionsRequest,
    ) -> Result<Vec<SessionSummary>, String> {
        let lock = self.sessions.lock().await;
        let mut list: Vec<SessionSummary> = lock
            .values()
            .filter(|s| s.summary.project_path == req.project_path)
            .map(|s| s.summary.clone())
            .collect();

        // Also discover persisted sessions on disk
        let sessions_dir = Path::new(&req.project_path)
            .join(".threadlane")
            .join("sessions");
        if let Ok(entries) = std::fs::read_dir(sessions_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if !list.iter().any(|s| s.session_id == stem) {
                            list.push(SessionSummary {
                                session_id: stem.to_string(),
                                project_path: req.project_path.clone(),
                                title: format!("Session {stem}"),
                                model: "antigravity/gemini-3.7-flash".to_string(),
                                created_at: chrono_iso_now(),
                                updated_at: chrono_iso_now(),
                                is_active: false,
                            });
                        }
                    }
                }
            }
        }

        Ok(list)
    }

    pub async fn get_session(&self, req: GetSessionRequest) -> Result<SessionDetail, String> {
        let lock = self.sessions.lock().await;
        if let Some(session) = lock.get(&req.session_id) {
            Ok(SessionDetail {
                summary: session.summary.clone(),
                messages: Vec::new(),
                plan: None,
                latest_sequence: 0,
            })
        } else {
            Err(format!("Session '{}' not found", req.session_id))
        }
    }

    pub async fn delete_session(&self, req: DeleteSessionRequest) -> Result<(), String> {
        let mut lock = self.sessions.lock().await;
        if let Some(session) = lock.remove(&req.session_id) {
            if let Some(ref path) = session.agent.session_file {
                let _ = std::fs::remove_file(path);
            }
            Ok(())
        } else {
            Err(format!("Session '{}' not found", req.session_id))
        }
    }

    pub async fn subscribe_session(
        &self,
        session_id: &str,
    ) -> Result<broadcast::Receiver<SessionEvent>, String> {
        let lock = self.sessions.lock().await;
        if let Some(session) = lock.get(session_id) {
            Ok(session.broadcaster.subscribe())
        } else {
            Err(format!("Session '{session_id}' not found"))
        }
    }

    pub async fn send_prompt(
        &self,
        req: SendPromptRequest,
    ) -> Result<PromptAcceptedResponse, String> {
        let mut lock = self.sessions.lock().await;
        let session = lock
            .get_mut(&req.session_id)
            .ok_or_else(|| format!("Session '{}' not found", req.session_id))?;

        let run_id = format!("run_{}", uuid_v4_like());
        let session_id = req.session_id.clone();
        let prompt = req.prompt;
        let event_broadcaster = session.broadcaster.clone();

        // Subscribe to internal agent events
        let mut agent_events = session.agent.subscribe();

        // Forward agent events to protocol session events
        let event_broadcaster_clone = event_broadcaster.clone();
        let session_id_clone = session_id.clone();
        tokio::spawn(async move {
            while let Ok(agent_event) = agent_events.recv().await {
                let session_event = match agent_event {
                    threadlane_runtime::AgentEvent::AgentStart => {
                        SessionEvent::SessionStarted {
                            session_id: session_id_clone.clone(),
                        }
                    }
                    threadlane_runtime::AgentEvent::TurnStart { turn_number } => {
                        SessionEvent::TurnStarted { turn_number }
                    }
                    threadlane_runtime::AgentEvent::MessageUpdate {
                        text_delta,
                        reasoning_delta,
                        ..
                    } => {
                        if let Some(delta) = text_delta {
                            SessionEvent::TokenDelta { delta }
                        } else if let Some(delta) = reasoning_delta {
                            SessionEvent::ReasoningDelta { delta }
                        } else {
                            continue;
                        }
                    }
                    threadlane_runtime::AgentEvent::ToolExecutionStart {
                        tool_call_id,
                        name,
                        arguments,
                    } => SessionEvent::ToolCallStarted {
                        tool_call_id,
                        name,
                        arguments,
                    },
                    threadlane_runtime::AgentEvent::ToolExecutionUpdate {
                        tool_call_id,
                        partial_result,
                    } => SessionEvent::ToolCallUpdated {
                        tool_call_id,
                        partial_result,
                    },
                    threadlane_runtime::AgentEvent::ToolExecutionEnd {
                        tool_call_id,
                        name,
                        result,
                    } => SessionEvent::ToolCallFinished {
                        tool_call_id,
                        name,
                        result: ToolResultPayload {
                            content: result.content,
                            is_error: result.is_error,
                        },
                    },
                    threadlane_runtime::AgentEvent::PermissionRequested { request } => {
                        SessionEvent::PermissionRequested {
                            request: PermissionRequest {
                                id: request.id,
                                session_id: Some(session_id_clone.clone()),
                                capability: request.capability,
                                title: request.title,
                                detail: request.detail,
                                scopes: request
                                    .scopes
                                    .into_iter()
                                    .map(|s| match s {
                                        threadlane_runtime::PermissionScope::Once => {
                                            PermissionScope::Once
                                        }
                                        threadlane_runtime::PermissionScope::Always => {
                                            PermissionScope::Always
                                        }
                                    })
                                    .collect(),
                                options: Vec::new(),
                            },
                        }
                    }
                    threadlane_runtime::AgentEvent::AgentError { error } => {
                        SessionEvent::Error { message: error }
                    }
                    _ => continue,
                };

                let _ = event_broadcaster_clone.send(session_event);
            }
        });

        // Execute prompt on agent
        let handle = session.agent.work_handle();
        tokio::spawn(async move {
            info!("Running prompt for session {session_id}");
            let _ = handle.try_queue_follow_up_with_images(&prompt, Vec::new());
        });

        Ok(PromptAcceptedResponse {
            session_id: req.session_id,
            sequence: 1,
            run_id,
        })
    }

    pub async fn submit_permission_decision(
        &self,
        req: SubmitPermissionRequest,
    ) -> Result<(), String> {
        let lock = self.sessions.lock().await;
        let mut resolved = false;

        for session in lock.values() {
            let decision = match req.decision {
                PermissionDecision::Allow { scope } => match scope {
                    PermissionScope::Once => threadlane_session::PermissionDecision::AllowOnce,
                    PermissionScope::Always => threadlane_session::PermissionDecision::AllowAlways,
                },
                PermissionDecision::Deny { .. } => threadlane_session::PermissionDecision::Deny,
                PermissionDecision::AllowWithModifications { .. } => {
                    threadlane_session::PermissionDecision::AllowOnce
                }
            };

            if session
                .agent
                .permission_handle()
                .resolve(&req.request_id, decision)
            {
                resolved = true;
                break;
            }
        }

        if resolved {
            Ok(())
        } else {
            Err(format!("Permission request '{}' not found", req.request_id))
        }
    }

    pub async fn cancel_run(&self, req: CancelRunRequest) -> Result<(), String> {
        let lock = self.sessions.lock().await;
        if let Some(session) = lock.get(&req.session_id) {
            let _ = session.agent.cancellation_handle().cancel();
            Ok(())
        } else {
            Err(format!("Session '{}' not found", req.session_id))
        }
    }

    pub async fn set_model(&self, req: SetSessionModelRequest) -> Result<(), String> {
        let mut lock = self.sessions.lock().await;
        if let Some(session) = lock.get_mut(&req.session_id) {
            session.summary.model = req.model.clone();
            Ok(())
        } else {
            Err(format!("Session '{}' not found", req.session_id))
        }
    }
}

fn uuid_v4_like() -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos)
}

fn chrono_iso_now() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now();
    let datetime: chrono::DateTime<chrono::Utc> = now.into();
    datetime.to_rfc3339()
}
