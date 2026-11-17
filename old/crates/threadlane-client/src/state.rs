//! Shared, GPUI-free projection owned by each desktop or mobile client.
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};
use threadlane_protocol::daemon::*;
use threadlane_protocol::projection::{
    adapt_agent_event, tool_activity_display_summary, tool_activity_summary, ChatAgentUpdate,
};
use threadlane_protocol::{AgentEvent, SessionPlan, TokenUsage};

#[derive(Default)]
pub struct ClientState {
    pub composer_drafts: HashMap<(Option<PathBuf>, Option<String>), ComposerDraft>,
    pub projects: Vec<ProjectInfo>,
    pub active_work_dir: Option<PathBuf>,
    pub active_session_id: Option<String>,
    /// Presentation-only filter; `None` shows sessions across all projects.
    pub sidebar_project_filter: Option<PathBuf>,
    pub search_query: String,
    pub pinned_sessions: HashSet<(PathBuf, String)>,
    pub messages: Arc<Vec<ChatMessageInfo>>,
    pub active_plan: SessionPlan,
    pub is_generating: bool,
    pub session_status: Option<String>,
    pub pending_composer_messages: HashMap<String, PendingComposerMessage>,
    pub session_token_usage: HashMap<SessionProjectionKey, TokenUsage>,
    pub session_metrics: HashMap<SessionProjectionKey, SessionMetricsInfo>,
    pub context_windows: HashMap<SessionProjectionKey, ContextWindowInfo>,
    pub run_timings: HashMap<SessionProjectionKey, RunTiming>,
    pub pending_permissions: HashMap<String, threadlane_protocol::PermissionRequest>,
    pub pending_questions: HashMap<String, threadlane_protocol::QuestionRequest>,
    /// Further requests stay queued behind the request currently shown.
    pub queued_questions: HashMap<String, Vec<threadlane_protocol::QuestionRequest>>,
    /// Latest automation-store projection (`SessionEvent::AutomationChanged`,
    /// wire ≥5). Default empty on pre-5 daemons, which never emit it.
    pub automation: threadlane_protocol::automation::AutomationProjection,
    /// Permission/question entries the automation projection contributed
    /// to the pending/queued request surfaces — tracked so the next
    /// projection can retract only those (a request resolved elsewhere
    /// leaves no Agent event).
    pub automation_permissions: HashMap<String, threadlane_protocol::PermissionRequest>,
    pub automation_questions: HashMap<String, threadlane_protocol::QuestionRequest>,
}
impl ClientState {
    pub fn messages_mut(&mut self) -> &mut Vec<ChatMessageInfo> {
        Arc::make_mut(&mut self.messages)
    }
    pub fn active_session_projection_key(&self) -> Option<SessionProjectionKey> {
        let id = self.active_session_id.as_ref()?;
        self.projects
            .iter()
            .filter(|p| self.active_work_dir.as_ref() == Some(&p.work_dir))
            .flat_map(|p| &p.sessions)
            .find(|s| &s.id == id)
            .map(|s| SessionProjectionKey {
                session_id: id.clone(),
                session_file: s.session_file.clone(),
            })
    }
}

impl ClientState {
    /// Apply a live chat event. The owner notifies observers after the batch.
    pub fn apply_agent_event(&mut self, session_id: &str, event: AgentEvent) -> bool {
        if matches!(event, AgentEvent::TurnStart { .. }) {
            if let Some(message) = self
                .messages_mut()
                .last_mut()
                .filter(|message| message.role == MessageRole::Assistant)
            {
                message.streaming = false;
            }
            return true;
        }
        if let AgentEvent::MessageUpdate {
            text_delta: Some(text),
            reasoning_delta: Some(reasoning),
            tool_call_name,
        } = event
        {
            let text_changed = self.apply_agent_event(
                session_id,
                AgentEvent::MessageUpdate {
                    text_delta: Some(text),
                    reasoning_delta: None,
                    tool_call_name,
                },
            );
            return self.apply_agent_event(
                session_id,
                AgentEvent::MessageUpdate {
                    text_delta: None,
                    reasoning_delta: Some(reasoning),
                    tool_call_name: None,
                },
            ) | text_changed;
        }
        let mut changed = false;
        match adapt_agent_event(event) {
            ChatAgentUpdate::TextDelta(delta) => {
                changed = true;
                if let Some(message) = self.messages_mut().last_mut().filter(|message| {
                    let id = message.id.as_str();
                    let stream_id = id.strip_prefix("streaming-");
                    message.role == MessageRole::Assistant
                        && stream_id.is_some_and(|stream_id| {
                            stream_id
                                .strip_prefix(session_id)
                                .is_some_and(|suffix| suffix.starts_with('-'))
                        })
                        && message.tool_activities.is_empty()
                }) {
                    message.content.push_str(&delta);
                } else {
                    let new_len = self.messages.len();
                    self.messages_mut().push(ChatMessageInfo {
                        id: format!("streaming-{session_id}-{new_len}"),
                        role: MessageRole::Assistant,
                        content: delta,
                        tool_activities: Vec::new(),
                        streaming: true,
                        reasoning_content: None,
                        reasoning_expanded: false,
                        retry_prompt: None,
                    });
                }
            }
            ChatAgentUpdate::ReasoningDelta(delta) => {
                changed = true;
                if let Some(message) = self
                    .messages_mut()
                    .last_mut()
                    .filter(|m| m.role == MessageRole::Assistant && m.streaming)
                {
                    match &mut message.reasoning_content {
                        Some(content) => content.push_str(&delta),
                        None => message.reasoning_content = Some(delta),
                    }
                } else {
                    let segment = self.messages.len();
                    self.messages_mut().push(ChatMessageInfo {
                        id: format!("streaming-{session_id}-{segment}"),
                        role: MessageRole::Assistant,
                        content: String::new(),
                        tool_activities: Vec::new(),
                        streaming: true,
                        reasoning_content: Some(delta),
                        reasoning_expanded: false,
                        retry_prompt: None,
                    });
                }
            }
            ChatAgentUpdate::ToolStarted {
                tool_call_id,
                name,
                arguments,
            } => {
                changed = true;
                let summary = tool_activity_summary(&name, &arguments);
                let display_summary = tool_activity_display_summary(&summary);
                let activity = ToolActivityInfo {
                    id: tool_call_id,
                    category: "Working".into(),
                    display_summary,
                    title: name,
                    detail: arguments.clone(),
                    arguments,
                    is_expanded: false,
                };
                if let Some(message) = self.messages_mut().last_mut().filter(|message| {
                    message.role == MessageRole::Assistant && message.content.is_empty()
                }) {
                    message.tool_activities.push(activity);
                } else {
                    let new_len = self.messages.len();
                    self.messages_mut().push(ChatMessageInfo {
                        id: format!("streaming-{session_id}-{new_len}"),
                        role: MessageRole::Assistant,
                        content: String::new(),
                        tool_activities: vec![activity],
                        streaming: true,
                        reasoning_content: None,
                        reasoning_expanded: false,
                        retry_prompt: None,
                    });
                }
            }
            ChatAgentUpdate::ToolUpdated {
                tool_call_id,
                partial_result,
            } => {
                changed = true;
                if let Some(activity) = self
                    .messages_mut()
                    .iter_mut()
                    .rev()
                    .flat_map(|message| message.tool_activities.iter_mut().rev())
                    .find(|activity| activity.id == tool_call_id)
                {
                    activity.detail = partial_result;
                }
            }
            ChatAgentUpdate::ToolFinished {
                tool_call_id,
                content,
                is_error,
            } => {
                changed = true;
                if let Some(activity) = self
                    .messages_mut()
                    .iter_mut()
                    .rev()
                    .flat_map(|message| message.tool_activities.iter_mut().rev())
                    .find(|activity| activity.id == tool_call_id)
                {
                    activity.category = if is_error {
                        "Error".into()
                    } else {
                        "Completed".into()
                    };
                    activity.detail = content;
                }
            }
            ChatAgentUpdate::PlanUpdated(plan) => {
                changed = true;
                self.active_plan = plan;
            }
            ChatAgentUpdate::Usage(usage) => {
                // Best-effort like the metrics above: usage
                // without a projection key is dropped, never
                // panicked on.
                if let Some(key) = self.active_session_projection_key() {
                    let entry = self.session_token_usage.entry(key.clone()).or_default();
                    entry.accumulate(&usage);
                }
            }
            ChatAgentUpdate::PermissionRequested(request) => {
                changed = true;
                self.pending_permissions
                    .insert(session_id.to_owned(), request);
            }
            ChatAgentUpdate::QuestionRequested(request) => {
                if self
                    .pending_questions
                    .get(session_id)
                    .is_some_and(|q| q.id == request.id)
                    || self
                        .queued_questions
                        .get(session_id)
                        .is_some_and(|qs| qs.iter().any(|q| q.id == request.id))
                {
                    return false;
                }
                if self.pending_questions.contains_key(session_id) {
                    self.queued_questions
                        .entry(session_id.to_owned())
                        .or_default()
                        .push(request);
                    return true;
                }
                changed = true;
                let summary = request
                    .questions
                    .iter()
                    .map(|item| {
                        let options = if item.options.is_empty() {
                            String::new()
                        } else {
                            format!(" [{}]", item.options.join(" / "))
                        };
                        format!("• {}: {}{}", item.header, item.question, options)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.messages_mut().push(ChatMessageInfo {
                                id: format!("question-notice-{}", request.id),
                                role: MessageRole::System,
                                content: format!(
                                    "The model asked a question — answer it below so the run can continue.\n{summary}"
                                ),
                                tool_activities: Vec::new(),
                                streaming: false,
                                reasoning_content: None,
                                reasoning_expanded: false,
                                retry_prompt: None,
                            });
                // Keep the request pending until the user answers
                // or dismisses it in the question card. Never
                // auto-resolve: the turn must block on the answer.
                self.pending_questions
                    .insert(session_id.to_owned(), request.clone());
            }
            ChatAgentUpdate::Error(error) => {
                changed = true;
                self.messages_mut().push(ChatMessageInfo {
                    id: format!("stream-error-{session_id}"),
                    role: MessageRole::Error,
                    content: error.clone(),
                    tool_activities: Vec::new(),
                    streaming: false,
                    reasoning_content: None,
                    reasoning_expanded: false,
                    retry_prompt: None,
                });
                self.is_generating = false;
                self.session_status = Some(error);
            }
            ChatAgentUpdate::Ignore => {}
        }
        changed
    }
}

impl ClientState {
    pub fn select_session(&mut self, session: &SessionInfo) {
        self.active_work_dir = Some(session.work_dir.clone());
        self.active_session_id = Some(session.id.clone());
        self.messages = Arc::new(Vec::new());
        self.active_plan = SessionPlan::default();
        self.is_generating = matches!(session.health, SessionHealth::Working);
        self.session_status = None;
    }
    pub fn pop_question(&mut self, session_id: &str) {
        self.pending_questions.remove(session_id);
        if let Some(queue) = self.queued_questions.get_mut(session_id) {
            if !queue.is_empty() {
                self.pending_questions
                    .insert(session_id.to_owned(), queue.remove(0));
            }
            if queue.is_empty() {
                self.queued_questions.remove(session_id);
            }
        }
    }

    /// Merge projected questions into the existing FIFO. Complete queues are
    /// authoritative (including their order); legacy latest-only projections
    /// cannot distinguish parallel questions from resolved replacements.
    fn reconcile_automation_questions(
        &mut self,
        projection: &threadlane_protocol::automation::AutomationProjection,
    ) {
        let sessions: HashSet<_> = self
            .automation_questions
            .keys()
            .chain(projection.questions.keys())
            .chain(
                self.automation
                    .question_queues
                    .iter()
                    .flat_map(|queues| queues.keys()),
            )
            .chain(
                projection
                    .question_queues
                    .iter()
                    .flat_map(|queues| queues.keys()),
            )
            .chain(self.automation.active_session_id.iter())
            .chain(projection.active_session_id.iter())
            .cloned()
            .collect();
        for session_id in sessions {
            let pending = self.pending_questions.remove(&session_id);
            let queued = self
                .queued_questions
                .remove(&session_id)
                .unwrap_or_default();
            let mut requests = if let Some(queues) = &projection.question_queues {
                // A first/coalesced projection can already omit a resolved
                // Agent request we never saw in a previous projection. Trust
                // the complete snapshot rather than keeping that stale head.
                queues.get(&session_id).cloned().unwrap_or_default()
            } else {
                let resolved = self
                    .automation_questions
                    .get(&session_id)
                    .filter(|_| !projection.questions.contains_key(&session_id));
                let mut requests: Vec<_> = pending
                    .into_iter()
                    .chain(queued)
                    .filter(|request| resolved.is_none_or(|old| old.id != request.id))
                    .collect();
                if let Some(current) = projection.questions.get(&session_id) {
                    if let Some(existing) = requests.iter_mut().find(|q| q.id == current.id) {
                        *existing = current.clone();
                    } else {
                        requests.push(current.clone());
                    }
                }
                requests
            };
            let mut seen = HashSet::new();
            requests.retain(|request| seen.insert(request.id.clone()));
            if !requests.is_empty() {
                self.pending_questions
                    .insert(session_id.clone(), requests.remove(0));
            }
            if !requests.is_empty() {
                self.queued_questions.insert(session_id, requests);
            }
        }
    }

    /// Wire projection only. Follow-up commands are executed by the client owner.
    pub fn apply_event(&mut self, event: SessionEvent) -> Vec<SessionCommand> {
        let mut commands = Vec::new();
        match event {
            SessionEvent::ProjectChanged { project } => {
                if let Some(existing) = self
                    .projects
                    .iter_mut()
                    .find(|p| p.work_dir == project.work_dir)
                {
                    *existing = project;
                } else {
                    self.projects.push(project);
                }
            }
            SessionEvent::SessionSnapshot {
                session_id,
                snapshot,
            } if self.active_session_projection_key().is_some_and(|key| {
                key.session_id == session_id
                    && snapshot.session.id == session_id
                    && key.session_file == snapshot.session.session_file
            }) =>
            {
                let key = SessionProjectionKey {
                    session_id,
                    session_file: snapshot.session.session_file.clone(),
                };
                self.messages = Arc::new(snapshot.messages);
                self.active_plan = snapshot.plan;
                self.is_generating = matches!(snapshot.session.health, SessionHealth::Working);
                self.session_metrics.insert(key.clone(), snapshot.metrics);
                self.session_token_usage
                    .insert(key.clone(), snapshot.token_usage);
                if let Some(context) = snapshot.context_window {
                    self.context_windows.insert(key.clone(), context);
                } else {
                    self.context_windows.remove(&key);
                }
                if let Some(timing) = snapshot.run_timing {
                    self.run_timings.insert(key, timing);
                }
                self.session_status = None;
                for project in &mut self.projects {
                    if let Some(info) = project
                        .sessions
                        .iter_mut()
                        .find(|s| s.id == snapshot.session.id)
                    {
                        *info = snapshot.session.clone();
                    }
                }
            }
            SessionEvent::Agent { session_id, event } => {
                if self.active_session_id.as_deref() == Some(&session_id) {
                    match &event {
                        AgentEvent::AgentStart => self.is_generating = true,
                        AgentEvent::AgentEnd { .. } => self.is_generating = false,
                        _ => {}
                    }
                    self.apply_agent_event(&session_id, event);
                } else {
                    match event {
                        AgentEvent::PermissionRequested { request } => {
                            self.pending_permissions.insert(session_id, request);
                        }
                        AgentEvent::QuestionRequested { request } => {
                            if !self
                                .pending_questions
                                .get(&session_id)
                                .is_some_and(|q| q.id == request.id)
                            {
                                if self.pending_questions.contains_key(&session_id) {
                                    let queue =
                                        self.queued_questions.entry(session_id).or_default();
                                    if !queue.iter().any(|q| q.id == request.id) {
                                        queue.push(request);
                                    }
                                } else {
                                    self.pending_questions.insert(session_id, request);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            SessionEvent::Finished { session_id, .. } => {
                self.pending_permissions.remove(&session_id);
                self.pending_questions.remove(&session_id);
                self.queued_questions.remove(&session_id);
                if self.active_session_id.as_deref() == Some(&session_id) {
                    self.is_generating = false;
                    for message in self.messages_mut() {
                        message.streaming = false;
                    }
                    commands.push(SessionCommand::GetSessionSnapshot { session_id });
                }
            }
            SessionEvent::TitleGenerated { session_id, .. } => {
                commands.push(SessionCommand::GetSessionSnapshot { session_id });
            }
            SessionEvent::SessionRemoved { session_id, .. } => {
                for project in &mut self.projects {
                    project.sessions.retain(|s| s.id != session_id);
                }
                self.pending_permissions.remove(&session_id);
                self.pending_questions.remove(&session_id);
                self.queued_questions.remove(&session_id);
                if self.active_session_id.as_deref() == Some(&session_id) {
                    self.active_session_id = None;
                    self.messages = Arc::new(Vec::new());
                }
            }
            SessionEvent::AutomationChanged { projection } => {
                // Retract requests an earlier projection contributed and
                // this one no longer lists (a resolution came through a
                // command reply, not an Agent event), then upsert the
                // store's current request surface.
                for (session_id, old) in &self.automation_permissions {
                    if !projection.permissions.contains_key(session_id)
                        && self
                            .pending_permissions
                            .get(session_id)
                            .is_some_and(|p| p.id == old.id)
                    {
                        self.pending_permissions.remove(session_id);
                    }
                }
                self.reconcile_automation_questions(&projection);
                self.pending_permissions
                    .extend(projection.permissions.clone());
                self.automation_permissions = projection.permissions.clone();
                self.automation_questions = projection.questions.clone();
                self.automation = projection;
            }
            SessionEvent::DaemonError {
                session_id,
                message,
            } if session_id.is_none() || session_id == self.active_session_id => {
                self.session_status = Some(message);
            }
            _ => {}
        }
        commands
    }
}

#[derive(Default, Clone)]
pub struct ComposerDraft {
    pub text: String,
    pub images: Vec<threadlane_protocol::ImageAttachment>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_protocol::{AgentToolResult, QuestionRequest};
    fn session(id: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            title: id.into(),
            work_dir: "/project".into(),
            runtime_work_dir: "/project/worktree".into(),
            session_file: format!("/project/{id}.jsonl").into(),
            updated_at: 0,
            health: SessionHealth::Healthy,
            git_branch: None,
            github_issue: None,
            is_worktree: true,
            worktree_available: true,
            completion_summary: SessionCompletionSummary::Unknown,
        }
    }
    fn state() -> ClientState {
        let session = session("one");
        let mut state = ClientState::default();
        state.projects.push(ProjectInfo {
            name: "project".into(),
            work_dir: "/project".into(),
            sessions: vec![session.clone(), super::tests::session("two")],
            is_expanded: true,
        });
        state.select_session(&session);
        state
    }
    #[test]
    fn streaming_preserves_both_deltas_and_tool_identity() {
        let mut state = state();
        state.apply_agent_event(
            "one",
            AgentEvent::MessageUpdate {
                text_delta: Some("hello".into()),
                reasoning_delta: Some("thinking".into()),
                tool_call_name: None,
            },
        );
        assert_eq!(state.messages[0].content, "hello");
        assert_eq!(
            state.messages[0].reasoning_content.as_deref(),
            Some("thinking")
        );
        for id in ["first", "second"] {
            state.apply_agent_event(
                "one",
                AgentEvent::ToolExecutionStart {
                    tool_call_id: id.into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.rs"}"#.into(),
                },
            );
        }
        state.apply_agent_event(
            "one",
            AgentEvent::ToolExecutionEnd {
                tool_call_id: "first".into(),
                name: "read_file".into(),
                result: AgentToolResult {
                    tool_call_id: "first".into(),
                    name: "read_file".into(),
                    content: "file content".into(),
                    is_error: false,
                    terminate: false,
                    images: Vec::new(),
                },
            },
        );
        let tools = &state.messages[1].tool_activities;
        assert_eq!(tools[0].detail, "file content");
        assert_eq!(tools[0].category, "Completed");
        assert_eq!(tools[1].category, "Working");
        assert_eq!(tools[0].display_summary, "Read · a.rs");
        assert_eq!(tools[1].display_summary, "Read · a.rs");
    }
    #[test]
    fn background_questions_survive_navigation_and_replay_without_duplicates() {
        let mut state = state();
        for id in ["q1", "q2", "q2"] {
            state.apply_event(SessionEvent::Agent {
                session_id: "two".into(),
                event: AgentEvent::QuestionRequested {
                    request: QuestionRequest {
                        id: id.into(),
                        questions: Vec::new(),
                    },
                },
            });
        }
        state.select_session(&session("two"));
        assert_eq!(state.pending_questions["two"].id, "q1");
        state.pop_question("two");
        assert_eq!(state.pending_questions["two"].id, "q2");
        state.pop_question("two");
        assert!(!state.pending_questions.contains_key("two"));
        assert!(!state.queued_questions.contains_key("two"));
    }

    /// Build a minimal request with a stable identity for reconciliation tests.
    fn question(id: &str) -> QuestionRequest {
        QuestionRequest {
            id: id.into(),
            questions: Vec::new(),
        }
    }

    /// Apply a legacy latest-only projection, including an empty retraction.
    fn project_question(state: &mut ClientState, session_id: &str, id: Option<&str>) {
        let mut projection = threadlane_protocol::automation::AutomationProjection::default();
        if let Some(id) = id {
            projection.questions.insert(session_id.into(), question(id));
        }
        state.apply_event(SessionEvent::AutomationChanged { projection });
    }

    /// Deliver a request through the production Agent-event path.
    fn emit_question(state: &mut ClientState, session_id: &str, id: &str) {
        state.apply_event(SessionEvent::Agent {
            session_id: session_id.into(),
            event: AgentEvent::QuestionRequested {
                request: question(id),
            },
        });
    }

    /// Apply a complete authoritative queue while retaining the legacy latest entry.
    fn project_question_queue(state: &mut ClientState, session_id: &str, ids: &[&str]) {
        let questions: Vec<_> = ids.iter().map(|id| question(id)).collect();
        let mut projection = threadlane_protocol::automation::AutomationProjection {
            question_queues: Some(HashMap::new()),
            ..Default::default()
        };
        if let Some(last) = questions.last() {
            projection.questions.insert(session_id.into(), last.clone());
            projection
                .question_queues
                .as_mut()
                .unwrap()
                .insert(session_id.into(), questions);
        }
        state.apply_event(SessionEvent::AutomationChanged { projection });
    }

    /// An unchanged latest question must not hide resolution of an earlier queued request.
    #[test]
    fn automation_complete_queue_reconciles_an_earlier_question_answered_on_another_client() {
        let mut state = state();
        emit_question(&mut state, "one", "first");
        emit_question(&mut state, "one", "second");
        project_question_queue(&mut state, "one", &["first", "second"]);
        project_question_queue(&mut state, "one", &["first", "second"]);
        assert_eq!(state.pending_questions["one"].id, "first");
        assert_eq!(state.queued_questions["one"], vec![question("second")]);

        // The latest-only map still says "second" after another client
        // answers "first". The full queue identifies the exact resolution.
        project_question_queue(&mut state, "one", &["second"]);
        assert_eq!(state.pending_questions["one"].id, "second");
        assert!(!state.queued_questions.contains_key("one"));
        project_question_queue(&mut state, "one", &[]);
        assert!(!state.pending_questions.contains_key("one"));
    }

    /// A coalesced replacement snapshot retires the resolved request without an empty intermediate state.
    #[test]
    fn automation_complete_queue_handles_coalesced_resolution_and_new_question() {
        let mut state = state();
        project_question_queue(&mut state, "one", &["first"]);
        emit_question(&mut state, "one", "second");
        // A watch consumer may never see the intermediate empty projection.
        project_question_queue(&mut state, "one", &["second"]);
        assert_eq!(state.pending_questions["one"].id, "second");
        assert!(!state.queued_questions.contains_key("one"));
    }

    /// The first complete snapshot can retire requests observed only through Agent events.
    #[test]
    fn automation_complete_queue_removes_requests_resolved_before_the_first_projection() {
        let mut state = state();
        emit_question(&mut state, "one", "first");
        emit_question(&mut state, "one", "second");
        // The watch coalesced the projection containing the first request,
        // which another client has already answered.
        project_question_queue(&mut state, "one", &["second"]);
        assert_eq!(state.pending_questions["one"].id, "second");
        assert!(!state.queued_questions.contains_key("one"));
    }

    /// A reconnect snapshot restores FIFO order even when only the later Agent event was replayed.
    #[test]
    fn automation_complete_queue_restores_snapshot_order_after_partial_agent_replay() {
        let mut state = state();
        emit_question(&mut state, "one", "second");
        project_question_queue(&mut state, "one", &["first", "second"]);
        assert_eq!(state.pending_questions["one"].id, "first");
        assert_eq!(state.queued_questions["one"], vec![question("second")]);
    }

    /// An authoritative empty queue clears the active automation without touching unrelated sessions.
    #[test]
    fn automation_complete_empty_queue_clears_unprojected_requests_for_the_active_run_only() {
        let mut state = state();
        emit_question(&mut state, "one", "resolved");
        emit_question(&mut state, "two", "unrelated");
        state.apply_event(SessionEvent::AutomationChanged {
            projection: threadlane_protocol::automation::AutomationProjection {
                active_session_id: Some("one".into()),
                question_queues: Some(HashMap::new()),
                ..Default::default()
            },
        });
        assert!(!state.pending_questions.contains_key("one"));
        assert_eq!(state.pending_questions["two"].id, "unrelated");
    }

    /// Complete snapshots can restore and resolve queues without replaying their Agent events.
    #[test]
    fn automation_complete_queue_restores_pending_questions_without_agent_replay() {
        let mut state = state();
        project_question_queue(&mut state, "two", &["first", "second", "third"]);
        emit_question(&mut state, "two", "second");
        project_question_queue(&mut state, "two", &["first", "third"]);
        assert_eq!(state.pending_questions["two"].id, "first");
        assert_eq!(state.queued_questions["two"], vec![question("third")]);
        state.pop_question("two");
        assert_eq!(state.pending_questions["two"].id, "third");
        state.pop_question("two");
        assert!(!state.pending_questions.contains_key("two"));
        assert!(!state.queued_questions.contains_key("two"));
    }

    /// An upgraded daemon can authoritatively replace a request first seen through a legacy payload.
    #[test]
    fn automation_complete_queue_reconciles_a_previous_legacy_projection() {
        let mut state = state();
        project_question(&mut state, "one", Some("first"));
        project_question_queue(&mut state, "one", &["second"]);
        assert_eq!(state.pending_questions["one"].id, "second");
        assert!(!state.queued_questions.contains_key("one"));
    }

    /// Legacy projections preserve the visible question and deduplicate both active/background replay.
    #[test]
    fn automation_projection_preserves_question_fifo_without_replaying_duplicates() {
        // Exercise both active and background Agent-event insertion paths.
        for session_id in ["one", "two"] {
            let mut state = state();
            emit_question(&mut state, session_id, "first");
            emit_question(&mut state, session_id, "second");
            project_question(&mut state, session_id, Some("second"));
            project_question(&mut state, session_id, Some("second"));
            emit_question(&mut state, session_id, "second");

            assert_eq!(state.pending_questions[session_id].id, "first");
            assert_eq!(state.queued_questions[session_id], vec![question("second")]);
            state.pop_question(session_id);
            assert_eq!(state.pending_questions[session_id].id, "second");
            state.pop_question(session_id);
            assert!(!state.pending_questions.contains_key(session_id));
            assert!(!state.queued_questions.contains_key(session_id));
        }
    }

    /// A newer legacy question joins the FIFO rather than replacing an unanswered predecessor.
    #[test]
    fn automation_projection_appends_new_questions_and_preserves_earlier_projected_requests() {
        let mut state = state();
        project_question(&mut state, "one", Some("first"));
        emit_question(&mut state, "one", "second");
        project_question(&mut state, "one", Some("third"));
        emit_question(&mut state, "one", "third");

        assert_eq!(state.pending_questions["one"].id, "first");
        assert_eq!(
            state.queued_questions["one"],
            vec![question("second"), question("third")]
        );
        for id in ["first", "second", "third"] {
            assert_eq!(state.pending_questions["one"].id, id);
            state.pop_question("one");
        }
        assert!(!state.pending_questions.contains_key("one"));
        assert!(!state.queued_questions.contains_key("one"));
    }

    /// Legacy retraction removes only its contributed queued ID and preserves other requests.
    #[test]
    fn automation_projection_retracts_resolved_queued_request_without_dropping_other_questions() {
        let mut state = state();
        for id in ["first", "second", "third"] {
            emit_question(&mut state, "one", id);
        }
        emit_question(&mut state, "two", "unrelated");
        project_question(&mut state, "one", Some("second"));
        project_question(&mut state, "one", None);

        assert_eq!(state.pending_questions["one"].id, "first");
        assert_eq!(state.queued_questions["one"], vec![question("third")]);
        assert_eq!(state.pending_questions["two"].id, "unrelated");
        state.pop_question("one");
        assert_eq!(state.pending_questions["one"].id, "third");
        state.pop_question("one");
        assert!(!state.pending_questions.contains_key("one"));
        assert!(!state.queued_questions.contains_key("one"));
    }

    /// Resolving the visible projected request promotes the next unanswered question exactly once.
    #[test]
    fn automation_projection_promotes_next_question_when_visible_request_resolves_elsewhere() {
        let mut state = state();
        emit_question(&mut state, "one", "first");
        emit_question(&mut state, "one", "second");
        project_question(&mut state, "one", Some("first"));
        project_question(&mut state, "one", None);
        project_question(&mut state, "one", None);

        assert_eq!(state.pending_questions["one"].id, "second");
        assert!(!state.queued_questions.contains_key("one"));
    }

    /// Reconciliation repairs duplicate IDs across both request surfaces before promotion.
    #[test]
    fn automation_projection_deduplicates_pending_and_queued_questions_before_promotion() {
        let mut state = state();
        state
            .pending_questions
            .insert("one".into(), question("first"));
        state.queued_questions.insert(
            "one".into(),
            vec![question("first"), question("second"), question("second")],
        );
        project_question(&mut state, "one", Some("first"));
        assert_eq!(state.queued_questions["one"], vec![question("second")]);
        project_question(&mut state, "one", None);
        assert_eq!(state.pending_questions["one"].id, "second");
        state.pop_question("one");
        assert!(!state.pending_questions.contains_key("one"));
        assert!(!state.queued_questions.contains_key("one"));
    }

    /// A late retraction cannot remove a newer Agent request with a different identity.
    #[test]
    fn automation_projection_removes_only_the_last_contributed_question_id() {
        let mut state = state();
        project_question(&mut state, "one", Some("resolved"));
        state.pop_question("one");
        emit_question(&mut state, "one", "newer");
        project_question(&mut state, "one", None);

        assert_eq!(state.pending_questions["one"].id, "newer");
        assert!(!state.queued_questions.contains_key("one"));
    }

    #[test]
    fn snapshot_and_background_stream_cannot_replace_another_session() {
        let mut state = state();
        let snapshot = SessionSnapshot {
            session: session("two"),
            messages: Vec::new(),
            trajectory: Vec::new(),
            subagents: Vec::new(),
            plan: SessionPlan::default(),
            metrics: SessionMetricsInfo::default(),
            token_usage: TokenUsage::default(),
            context_window: None,
            run_timing: None,
        };
        state.apply_event(SessionEvent::Agent {
            session_id: "one".into(),
            event: AgentEvent::MessageUpdate {
                text_delta: Some("keep".into()),
                reasoning_delta: None,
                tool_call_name: None,
            },
        });
        let mut stale_snapshot = snapshot.clone();
        stale_snapshot.session = session("one");
        stale_snapshot.session.session_file = "/other/one.jsonl".into();
        state.apply_event(SessionEvent::SessionSnapshot {
            session_id: "one".into(),
            snapshot: Box::new(stale_snapshot),
        });
        state.apply_event(SessionEvent::SessionSnapshot {
            session_id: "two".into(),
            snapshot: Box::new(snapshot),
        });
        state.apply_event(SessionEvent::Agent {
            session_id: "two".into(),
            event: AgentEvent::MessageUpdate {
                text_delta: Some("other".into()),
                reasoning_delta: None,
                tool_call_name: None,
            },
        });
        assert_eq!(state.messages[0].content, "keep");
        assert!(state
            .apply_event(SessionEvent::Finished {
                session_id: "two".into(),
                session_file: session("two").session_file
            })
            .is_empty());
        let commands = state.apply_event(SessionEvent::Finished {
            session_id: "one".into(),
            session_file: session("one").session_file,
        });
        assert!(
            matches!(&commands[..], [SessionCommand::GetSessionSnapshot { session_id }] if session_id == "one")
        );
        assert!(!state.messages[0].streaming);
    }
    #[test]
    fn drafts_remain_scoped_to_project_and_session() {
        let mut state = state();
        let key = (
            state.active_work_dir.clone(),
            state.active_session_id.clone(),
        );
        state.composer_drafts.insert(
            key.clone(),
            ComposerDraft {
                text: "unsent".into(),
                images: Vec::new(),
            },
        );
        state.select_session(&session("two"));
        assert_eq!(state.composer_drafts[&key].text, "unsent");
        state.apply_event(SessionEvent::SessionRemoved {
            session_id: "two".into(),
            session_file: session("two").session_file,
        });
        assert_eq!(state.composer_drafts[&key].text, "unsent");
    }
}
