use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;
use threadlane_protocol::harness::SessionDiagnostics;
use threadlane_protocol::permission::*;
use threadlane_protocol::session::*;
use threadlane_runtime::harness::SessionStore;
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

    pub async fn list_session_infos(
        &self,
        req: ListSessionsRequest,
    ) -> Result<Vec<SessionInfo>, String> {
        let work_dir = Path::new(&req.project_path);
        let sessions_dir = work_dir.join(".threadlane/sessions");
        let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
            return Ok(Vec::new());
        };

        let canonical_work_dir =
            std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
        let mut sessions = Vec::new();

        for entry in entries.flatten() {
            let path = std::fs::canonicalize(entry.path()).unwrap_or_else(|_| entry.path());
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl")
                || path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".harness.jsonl"))
            {
                continue;
            }

            let id = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "session".into());

            let (runtime_work_dir, is_worktree, stub_branch) =
                match threadlane_runtime::harness::JsonlStore::open_read_only(&path) {
                    Ok(store) => {
                        let facts = store.facts();
                        let is_worktree = facts
                            .get("is_worktree")
                            .is_some_and(|value| value == "true");
                        let work_dir = effective_session_work_dir(&canonical_work_dir, &id, &facts);
                        (work_dir, is_worktree, facts.get("git_branch").cloned())
                    }
                    Err(_) => (canonical_work_dir.clone(), false, None),
                };

            let session_file =
                resolve_session_transcript_file(&path, &runtime_work_dir, &id, is_worktree);

            let (title, health, git_branch) =
                match threadlane_runtime::harness::JsonlStore::open_read_only(&session_file) {
                    Ok(store) => (
                        extract_session_title(&store, &id),
                        SessionHealth::Healthy,
                        store.facts().get("git_branch").cloned().or(stub_branch),
                    ),
                    Err(_) => (
                        "Unreadable session".to_string(),
                        SessionHealth::Warning,
                        stub_branch,
                    ),
                };

            let updated_at = file_mtime(&session_file);
            let worktree_available = !is_worktree || runtime_work_dir.is_dir();

            sessions.push(SessionInfo {
                id,
                title,
                work_dir: canonical_work_dir.clone(),
                runtime_work_dir,
                session_file,
                updated_at,
                health,
                git_branch,
                is_worktree,
                worktree_available,
            });
        }

        sessions.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.title.cmp(&b.title))
        });
        Ok(sessions)
    }

    pub async fn hydrate_session(
        &self,
        req: HydrateSessionRequest,
    ) -> Result<HydrateSessionResponse, String> {
        let session_file = Path::new(&req.session_file);
        if !session_file.exists() {
            return Err(format!("Session file not found: {}", req.session_file));
        }

        let store = threadlane_runtime::harness::JsonlStore::open_read_only(session_file)
            .map_err(|e| format!("Failed to open session JSONL: {e}"))?;

        let diagnostics_internal =
            threadlane_runtime::harness::project_session_diagnostics(&store, "main")
                .map_err(|e| e.to_string())?;

        let diagnostics = convert_diagnostics(diagnostics_internal);
        let (trajectory, metrics, token_usage, context_window) =
            project_trajectory_from_store(&store);
        let subagents = project_subagents_from_store(&store);
        let messages = compute_session_messages_from_file(session_file)?;

        let plan = threadlane_protocol::SessionPlan {
            explanation: store.plan().explanation,
            items: store
                .plan()
                .items
                .into_iter()
                .map(|item| threadlane_protocol::PlanItem {
                    step: item.step,
                    status: match item.status {
                        threadlane_runtime::types::PlanItemStatus::Pending => {
                            threadlane_protocol::PlanItemStatus::Pending
                        }
                        threadlane_runtime::types::PlanItemStatus::InProgress => {
                            threadlane_protocol::PlanItemStatus::InProgress
                        }
                        threadlane_runtime::types::PlanItemStatus::Completed => {
                            threadlane_protocol::PlanItemStatus::Completed
                        }
                    },
                })
                .collect(),
        };

        Ok(HydrateSessionResponse {
            messages,
            plan,
            trajectory,
            subagents,
            diagnostics,
            metrics,
            token_usage,
            context_window,
        })
    }

    pub async fn archive_session(&self, req: ArchiveSessionRequest) -> Result<(), String> {
        let project_dir = Path::new(&req.project_path);
        let session_file = project_dir
            .join(".threadlane/sessions")
            .join(format!("{}.jsonl", req.session_id));
        let archive_dir = project_dir.join(".threadlane/archive");
        let _ = std::fs::create_dir_all(&archive_dir);
        let target = archive_dir.join(format!("{}.jsonl", req.session_id));
        if session_file.exists() {
            std::fs::rename(&session_file, target)
                .map_err(|e| format!("Failed to archive session: {e}"))?;
        }
        Ok(())
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
        }
        Ok(())
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
                PermissionDecision::AllowOnce => threadlane_session::PermissionDecision::AllowOnce,
                PermissionDecision::AllowAlways => threadlane_session::PermissionDecision::AllowAlways,
                PermissionDecision::Deny | PermissionDecision::DenyWithReason { .. } => {
                    threadlane_session::PermissionDecision::Deny
                }
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

// ── Helpers ─────────────────────────────────────────────────────────────────

fn file_mtime(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn effective_session_work_dir(
    canonical_work_dir: &Path,
    id: &str,
    facts: &std::collections::BTreeMap<String, String>,
) -> PathBuf {
    if facts
        .get("is_worktree")
        .is_some_and(|value| value == "true")
    {
        facts
            .get("worktree_path")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let inferred = canonical_work_dir.join(".threadlane/worktrees").join(id);
                if inferred.exists() {
                    inferred
                } else {
                    canonical_work_dir.to_path_buf()
                }
            })
    } else {
        canonical_work_dir.to_path_buf()
    }
}

fn resolve_session_transcript_file(
    stub_file: &Path,
    runtime_work_dir: &Path,
    session_id: &str,
    is_worktree: bool,
) -> PathBuf {
    let worktree_file = runtime_work_dir
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    if is_worktree && worktree_file.is_file() {
        worktree_file
    } else {
        stub_file.to_path_buf()
    }
}

fn extract_session_title(store: &impl SessionStore, fallback_id: &str) -> String {
    if let Some(name) = store.name() {
        if !name.trim().is_empty() {
            return name;
        }
    }
    let messages = {
        let active = store.active_branch_messages("main");
        if active.is_empty() {
            store.get_persisted_messages()
        } else {
            active
        }
    };

    for msg in &messages {
        match msg {
            threadlane_runtime::types::AgentMessage::User { content }
            | threadlane_runtime::types::AgentMessage::UserWithImages { content, .. } => {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    let first_line = trimmed.lines().next().unwrap_or(trimmed);
                    let mut char_count = 0;
                    let mut result = String::new();
                    for ch in first_line.chars() {
                        if char_count >= 40 {
                            result.push('…');
                            break;
                        }
                        result.push(ch);
                        char_count += 1;
                    }
                    return result;
                }
            }
            _ => {}
        }
    }
    fallback_id.to_string()
}

fn compute_session_messages_from_file(
    session_file: &Path,
) -> Result<Vec<ChatMessageInfo>, String> {
    use threadlane_runtime::harness::{read_transcript_page, TranscriptItem};

    let mut cursor = None;
    let mut pages = Vec::new();
    loop {
        let page =
            read_transcript_page(session_file, cursor, 40).map_err(|error| error.to_string())?;
        let has_older = page.has_older;
        cursor = page.next_cursor;
        pages.push(page.items);
        if !has_older {
            break;
        }
    }
    pages.reverse();
    let items = pages.into_iter().flatten().collect::<Vec<_>>();
    let mut rows = Vec::new();
    let mut messages = Vec::new();
    let mut segment_start = 0usize;
    let flush = |messages: &mut Vec<threadlane_runtime::types::AgentMessage>,
                 rows: &mut Vec<ChatMessageInfo>,
                 start| {
        for (index, mut row) in project_agent_messages_to_protocol(std::mem::take(messages))
            .into_iter()
            .enumerate()
        {
            row.id = format!("history-{start}-{index}-{}", row.id);
            rows.push(row);
        }
    };
    for (item_index, item) in items.into_iter().enumerate() {
        match item {
            TranscriptItem::Message(message) => {
                if messages.is_empty() {
                    segment_start = item_index;
                }
                messages.push(message);
            }
            TranscriptItem::ContextCompacted(marker) => {
                flush(&mut messages, &mut rows, segment_start);
                rows.push(ChatMessageInfo {
                    id: format!("history-context-{}", marker.seq),
                    role: MessageRole::ContextMarker,
                    content: format!(
                        "Context compacted · {} → {}",
                        marker.pre_tokens,
                        marker.post_tokens,
                    ),
                    tool_activities: Vec::new(),
                    streaming: false,
                    reasoning_content: None,
                    reasoning_expanded: false,
                });
            }
        }
    }
    flush(&mut messages, &mut rows, segment_start);
    Ok(rows)
}

fn project_agent_messages_to_protocol(
    agent_messages: Vec<threadlane_runtime::types::AgentMessage>,
) -> Vec<ChatMessageInfo> {
    threadlane_runtime::harness::project_chat_messages(&agent_messages)
        .into_iter()
        .map(|msg| ChatMessageInfo {
            id: msg.id,
            role: match msg.role {
                threadlane_runtime::harness::UiMessageRole::User => MessageRole::User,
                threadlane_runtime::harness::UiMessageRole::Assistant => MessageRole::Assistant,
                threadlane_runtime::harness::UiMessageRole::System => MessageRole::System,
                threadlane_runtime::harness::UiMessageRole::Advisor(sev) => {
                    MessageRole::Advisor(match sev {
                        threadlane_runtime::types::AdvisorSeverity::Aside => {
                            threadlane_protocol::AdvisorSeverity::Aside
                        }
                        threadlane_runtime::types::AdvisorSeverity::Concern => {
                            threadlane_protocol::AdvisorSeverity::Concern
                        }
                        threadlane_runtime::types::AdvisorSeverity::Blocker => {
                            threadlane_protocol::AdvisorSeverity::Blocker
                        }
                    })
                }
                threadlane_runtime::harness::UiMessageRole::Error => MessageRole::Error,
            },
            content: msg.content,
            tool_activities: msg
                .tool_activities
                .into_iter()
                .map(|act| ToolActivityInfo {
                    id: act.id,
                    category: act.category,
                    title: act.title,
                    display_summary: act.summary,
                    detail: act.detail,
                    is_expanded: false,
                })
                .collect(),
            streaming: false,
            reasoning_content: msg.reasoning_content,
            reasoning_expanded: false,
        })
        .collect()
}

fn convert_diagnostics(
    d: threadlane_runtime::harness::SessionDiagnostics,
) -> SessionDiagnostics {
    SessionDiagnostics {
        total_turns: d.recovery.iter().map(|r| r.attempts as usize).sum(),
        total_tokens: 0,
        input_tokens: 0,
        output_tokens: 0,
        model_context: Vec::new(),
        durable_events: d
            .durable_events
            .into_iter()
            .map(|e| threadlane_protocol::harness::DurableEventRecord {
                id: e.id,
                seq: e.seq,
                lane: e.lane,
                run_id: e.run_id,
                turn: e.turn,
                kind: match e.kind {
                    threadlane_runtime::harness::DurableEventKind::Entry { role, parent_id } => {
                        threadlane_protocol::harness::DurableEventKind::Entry { role, parent_id }
                    }
                    threadlane_runtime::harness::DurableEventKind::Record => {
                        threadlane_protocol::harness::DurableEventKind::Record
                    }
                },
            })
            .collect(),
        recovery: Vec::new(),
    }
}

fn project_trajectory_from_store(
    store: &threadlane_runtime::harness::JsonlStore,
) -> (
    Vec<TrajectoryEntry>,
    SessionMetricsInfo,
    TokenUsageSummary,
    Option<ContextWindowInfo>,
) {
    let mut trajectory: Vec<TrajectoryEntry> = Vec::new();
    let mut metrics = SessionMetricsInfo::default();
    let mut durable_usage = TokenUsageSummary::default();

    for record in store.records() {
        use threadlane_runtime::harness::Record;
        let entry = match record {
            Record::OperationStarted {
                seq,
                lane,
                id,
                intent,
                ..
            } => Some(TrajectoryEntry {
                seq: Some(*seq),
                run_id: Some(id.clone()),
                turn: None,
                request: None,
                category: "Operation".into(),
                summary: format!("{intent:?} started"),
                detail: String::new(),
                lane: Some(lane.clone()),
                correlation_id: None,
                diagnostics: TrajectoryDiagnostics::default(),
            }),
            Record::OperationFinished {
                seq,
                lane,
                run_id,
                outcome,
                error,
                ..
            } => Some(TrajectoryEntry {
                seq: Some(*seq),
                run_id: Some(run_id.clone()),
                turn: None,
                request: None,
                category: "Operation".into(),
                summary: format!("Operation {outcome:?}"),
                detail: error.clone().unwrap_or_default(),
                lane: Some(lane.clone()),
                correlation_id: None,
                diagnostics: TrajectoryDiagnostics::default(),
            }),
            Record::StepAttempt {
                seq,
                lane,
                run_id,
                attempt,
                ..
            } => {
                metrics.turns = metrics.turns.saturating_add(1);
                Some(TrajectoryEntry {
                    seq: Some(*seq),
                    run_id: Some(run_id.clone()),
                    turn: Some(*attempt),
                    request: None,
                    category: "Step".into(),
                    summary: format!("Step {attempt} started"),
                    detail: format!("lane {}", lane.as_str()),
                    lane: Some(lane.clone()),
                    correlation_id: None,
                    diagnostics: TrajectoryDiagnostics::default(),
                })
            }
            Record::Usage {
                seq,
                lane,
                run_id,
                attempt,
                cause,
                usage,
                ..
            } => {
                let usage_proto = TokenUsageSummary {
                    input_tokens: u64::from(usage.input_tokens),
                    output_tokens: u64::from(usage.output_tokens),
                    cache_read_tokens: u64::from(usage.cache_read_tokens),
                    cache_write_tokens: u64::from(usage.cache_write_tokens),
                    total_tokens: u64::from(usage.total_tokens),
                };
                if *cause == threadlane_runtime::harness::UsageCause::Provider {
                    metrics.accumulate_usage(&usage_proto);
                    durable_usage.accumulate(&usage_proto);
                }
                Some(TrajectoryEntry {
                    seq: Some(*seq),
                    run_id: run_id.clone(),
                    turn: *attempt,
                    request: None,
                    category: "Usage".into(),
                    summary: format!("Usage: {} total tokens ({cause:?})", usage.total_tokens),
                    detail: format!(
                        "input: {}, output: {}, cache read: {}, cache write: {}",
                        usage.input_tokens,
                        usage.output_tokens,
                        usage.cache_read_tokens,
                        usage.cache_write_tokens
                    ),
                    lane: Some(lane.clone()),
                    correlation_id: None,
                    diagnostics: TrajectoryDiagnostics::default(),
                })
            }
            _ => None,
        };
        if let Some(entry) = entry {
            trajectory.push(entry);
        }
    }

    (trajectory, metrics, durable_usage, None)
}

fn project_subagents_from_store(
    store: &impl SessionStore,
) -> Vec<SubagentActivityInfo> {
    use threadlane_runtime::harness::Record;

    let mut rows = Vec::new();
    for lane in store.lanes().into_iter().filter(|lane| lane != "main") {
        let has_subagent_lifecycle = store.records().iter().any(|record| {
            matches!(
                record,
                Record::SubagentLifecycle { subagent_lane, .. }
                    if subagent_lane.as_str() == lane
            )
        });
        if !has_subagent_lifecycle {
            continue;
        }

        rows.push(SubagentActivityInfo {
            batch_run_id: 0,
            task_index: rows.len(),
            journal_run_id: None,
            lane: Some(lane.clone()),
            agent: lane.clone(),
            task: format!("Subagent lane {lane}"),
            model: None,
            status: SubagentActivityStatus::Completed,
            messages: Vec::new(),
            error: None,
        });
    }
    rows
}

fn uuid_v4_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos)
}

fn chrono_iso_now() -> String {
    let now = std::time::SystemTime::now();
    let datetime: chrono::DateTime<chrono::Utc> = now.into();
    datetime.to_rfc3339()
}
