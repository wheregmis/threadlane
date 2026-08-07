use crate::login::LoginState;
use threadlane_agent::{AgentEvent, AgentMessage, ReasoningEffort, SessionPlan};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionMode {
    Command,
    Model,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionState {
    pub visible: bool,
    pub candidates: Vec<String>,
    pub selected: usize,
    pub mode: Option<CompletionMode>,
}

#[derive(Debug)]
pub struct AppState {
    pub model: String,
    pub reasoning_effort: ReasoningEffort,
    pub work_dir: String,
    pub messages: Vec<TranscriptMessage>,
    pub composer: String,
    pub streaming: Option<StreamingMessage>,
    pub activities: Vec<ActivityItem>,
    pub plan: Option<SessionPlan>,
    pub status: RunStatus,
    pub scroll: u16,
    pub follow_tail: bool,
    pub completion: CompletionState,
    pub login: Option<LoginState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamingMessage {
    pub role: String,
    pub text: String,
    pub reasoning: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityItem {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub status: ActivityStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Idle,
    Ready,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub enum MessageType {
    User,
    Assistant,
    Thinking,
    ToolCall(String),
    Error,
}

#[derive(Clone, Debug)]
pub struct TranscriptMessage {
    pub msg_type: MessageType,
    pub content: String,
}

impl AppState {
    pub fn new(model: String, work_dir: String) -> Self {
        Self {
            model,
            reasoning_effort: ReasoningEffort::default(),
            work_dir,
            messages: vec![TranscriptMessage {
                msg_type: MessageType::Assistant,
                content:
                    "Welcome to Threadlane CLI! Type your prompt below and press Enter to submit."
                        .into(),
            }],
            composer: String::new(),
            streaming: None,
            activities: Vec::new(),
            plan: None,
            status: RunStatus::Ready,
            scroll: 0,
            follow_tail: true,
            completion: CompletionState::default(),
            login: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_state() -> Self {
        Self::new("test-model".into(), "/tmp/work".into())
    }

    #[cfg(test)]
    pub(crate) fn test_state_generating() -> Self {
        let mut state = Self::test_state();
        state.begin_generation();
        state
    }

    #[cfg(test)]
    pub(crate) fn test_state_with_plan(count: usize) -> Self {
        let mut state = Self::test_state();
        state.plan = Some(SessionPlan {
            explanation: None,
            items: (0..count)
                .map(|_| threadlane_agent::PlanItem {
                    step: "step".into(),
                    status: threadlane_agent::PlanItemStatus::Pending,
                })
                .collect(),
        });
        state
    }

    pub fn streaming_text(&self) -> &str {
        self.streaming
            .as_ref()
            .map_or("", |message| message.text.as_str())
    }

    pub fn begin_generation(&mut self) {
        self.status = RunStatus::Running;
        self.follow_tail = true;
        self.scroll = 0;
    }

    pub fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_add(1);
        self.follow_tail = false;
    }

    pub fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
        self.follow_tail = self.scroll == 0;
    }

    pub fn show_completion(&mut self, mode: CompletionMode, candidates: Vec<String>) {
        if candidates.is_empty() {
            self.close_completion();
            return;
        }
        self.completion.visible = true;
        self.completion.candidates = candidates;
        self.completion.selected = 0;
        self.completion.mode = Some(mode);
    }

    pub fn close_completion(&mut self) {
        self.completion = CompletionState::default();
    }

    pub fn open_login(&mut self) {
        self.close_completion();
        self.login = Some(LoginState::new());
    }

    pub fn close_login(&mut self) {
        self.login = None;
    }

    pub fn select_next_completion(&mut self) {
        let count = self.completion.candidates.len();
        if count > 0 {
            self.completion.selected = (self.completion.selected + 1) % count;
        }
    }

    pub fn select_previous_completion(&mut self) {
        let count = self.completion.candidates.len();
        if count > 0 {
            self.completion.selected = (self.completion.selected + count - 1) % count;
        }
    }
}

const ACTIVITY_LIMIT: usize = 240;

fn truncate(value: impl AsRef<str>) -> String {
    let value = value.as_ref();
    if value.chars().count() <= ACTIVITY_LIMIT {
        value.into()
    } else {
        format!(
            "{}…",
            value.chars().take(ACTIVITY_LIMIT - 1).collect::<String>()
        )
    }
}

fn activity<'a>(state: &'a mut AppState, id: &str) -> Option<&'a mut ActivityItem> {
    state.activities.iter_mut().find(|item| item.id == id)
}

fn cancellation(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("cancel") || error.contains("abort")
}

pub fn reduce_agent_event(state: &mut AppState, event: AgentEvent) {
    match event {
        AgentEvent::AgentStart => state.begin_generation(),
        AgentEvent::AgentEnd { .. } => {
            if matches!(state.status, RunStatus::Running | RunStatus::Ready) {
                state.status = RunStatus::Succeeded
            }
        }
        AgentEvent::MessageStart { role } => {
            state.begin_generation();
            state.streaming = Some(StreamingMessage {
                role,
                text: String::new(),
                reasoning: String::new(),
            });
        }
        AgentEvent::MessageUpdate {
            text_delta,
            reasoning_delta,
            ..
        } => {
            let streaming = state.streaming.get_or_insert_with(|| StreamingMessage {
                role: "assistant".into(),
                text: String::new(),
                reasoning: String::new(),
            });
            if let Some(delta) = text_delta {
                streaming.text.push_str(&delta);
            }
            if let Some(delta) = reasoning_delta {
                streaming.reasoning.push_str(&delta);
            }
        }
        AgentEvent::MessageEnd { message } => {
            if let AgentMessage::Assistant { content, .. } = message {
                let content = content.unwrap_or_else(|| state.streaming_text().to_string());
                if !content.is_empty() {
                    state.messages.push(TranscriptMessage {
                        msg_type: MessageType::Assistant,
                        content,
                    });
                }
            }
            state.streaming = None;
        }
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            name,
            arguments,
        } => state.activities.push(ActivityItem {
            id: tool_call_id,
            name,
            detail: truncate(arguments),
            status: ActivityStatus::Running,
        }),
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            partial_result,
        } => {
            if let Some(item) = activity(state, &tool_call_id) {
                item.detail = truncate(partial_result);
            }
        }
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            name,
            result,
        } => {
            if let Some(item) = activity(state, &tool_call_id) {
                item.name = name;
                item.detail = truncate(&result.content);
                item.status = if result.is_error {
                    ActivityStatus::Failed
                } else {
                    ActivityStatus::Succeeded
                };
            }
        }
        AgentEvent::SubagentQueued {
            run_id,
            task_index,
            agent,
            task,
        } => state.activities.push(ActivityItem {
            id: format!("subagent:{run_id}:{task_index}"),
            name: agent,
            detail: truncate(task),
            status: ActivityStatus::Queued,
        }),
        AgentEvent::SubagentStarted {
            run_id,
            task_index,
            journal_run_id,
        } => {
            let queued_id = format!("subagent:{run_id}:{task_index}");
            if let Some(item) = activity(state, &queued_id) {
                item.id = journal_run_id;
                item.status = ActivityStatus::Running;
            }
        }
        AgentEvent::SubagentFinished {
            run_id,
            task_index,
            journal_run_id,
            succeeded,
            error,
        } => {
            let queued_id = format!("subagent:{run_id}:{task_index}");
            let id = if state
                .activities
                .iter()
                .any(|item| item.id == journal_run_id)
            {
                journal_run_id.clone()
            } else {
                queued_id
            };
            if let Some(item) = activity(state, &id) {
                item.id = journal_run_id;
                item.detail = truncate(error.as_deref().unwrap_or("Completed"));
                item.status = if succeeded {
                    ActivityStatus::Succeeded
                } else if error.as_deref().is_some_and(cancellation) {
                    ActivityStatus::Cancelled
                } else {
                    ActivityStatus::Failed
                };
            }
        }
        AgentEvent::PlanUpdated { plan } => state.plan = Some(plan),
        AgentEvent::AgentError { error } => {
            let cancelled = cancellation(&error);
            state.status = if cancelled {
                RunStatus::Cancelled
            } else {
                RunStatus::Failed
            };
            if cancelled {
                if let Some(streaming) = state.streaming.take() {
                    if !streaming.text.is_empty() {
                        state.messages.push(TranscriptMessage {
                            msg_type: MessageType::Assistant,
                            content: streaming.text,
                        });
                    }
                }
            } else {
                state.streaming = None;
            }
            state.messages.push(TranscriptMessage {
                msg_type: MessageType::Error,
                content: error,
            });
        }
        AgentEvent::TurnStart { .. }
        | AgentEvent::TurnEnd { .. }
        | AgentEvent::SubagentRecovery { .. }
        | AgentEvent::StreamRuleTriggered { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_agent::{AgentToolResult, PlanItem, PlanItemStatus};

    fn test_tool_result() -> AgentToolResult {
        serde_json::from_value(serde_json::json!({"tool_call_id":"tool-1","name":"read","content":"ok","is_error":false,"terminate":false})).unwrap()
    }

    fn test_plan() -> SessionPlan {
        SessionPlan {
            explanation: Some("Do the work".into()),
            items: vec![PlanItem {
                step: "Implement it".into(),
                status: PlanItemStatus::InProgress,
            }],
        }
    }

    #[test]
    fn message_updates_append_to_one_streaming_assistant() {
        let mut state = AppState::test_state();
        reduce_agent_event(
            &mut state,
            AgentEvent::MessageStart {
                role: "assistant".into(),
            },
        );
        reduce_agent_event(
            &mut state,
            AgentEvent::MessageUpdate {
                text_delta: Some("hel".into()),
                reasoning_delta: None,
                tool_call_name: None,
            },
        );
        reduce_agent_event(
            &mut state,
            AgentEvent::MessageUpdate {
                text_delta: Some("lo".into()),
                reasoning_delta: None,
                tool_call_name: None,
            },
        );
        assert_eq!(state.streaming_text(), "hello");
    }

    #[test]
    fn cancellation_commits_partial_streaming_assistant_text() {
        let mut state = AppState::test_state();
        reduce_agent_event(
            &mut state,
            AgentEvent::MessageStart {
                role: "assistant".into(),
            },
        );
        reduce_agent_event(
            &mut state,
            AgentEvent::MessageUpdate {
                text_delta: Some("partial".into()),
                reasoning_delta: None,
                tool_call_name: None,
            },
        );
        reduce_agent_event(
            &mut state,
            AgentEvent::AgentError {
                error: "Generation cancelled".into(),
            },
        );

        assert!(state.streaming.is_none());
        assert_eq!(
            state.messages.last().unwrap().content,
            "Generation cancelled"
        );
        assert_eq!(state.messages[state.messages.len() - 2].content, "partial");
        assert_eq!(state.status, RunStatus::Cancelled);
    }

    #[test]
    fn tool_lifecycle_replaces_activity_status() {
        let mut state = AppState::test_state();
        reduce_agent_event(
            &mut state,
            AgentEvent::ToolExecutionStart {
                tool_call_id: "tool-1".into(),
                name: "read".into(),
                arguments: "{}".into(),
            },
        );
        assert_eq!(state.activities[0].status, ActivityStatus::Running);
        reduce_agent_event(
            &mut state,
            AgentEvent::ToolExecutionEnd {
                tool_call_id: "tool-1".into(),
                name: "read".into(),
                result: test_tool_result(),
            },
        );
        assert_eq!(state.activities[0].status, ActivityStatus::Succeeded);
    }

    #[test]
    fn reducer_updates_errors_plan_subagents_and_cancellation() {
        let mut state = AppState::test_state();
        reduce_agent_event(&mut state, AgentEvent::PlanUpdated { plan: test_plan() });
        assert_eq!(
            state.plan.as_ref().unwrap().items[0].status,
            PlanItemStatus::InProgress
        );
        reduce_agent_event(
            &mut state,
            AgentEvent::SubagentQueued {
                run_id: 1,
                task_index: 0,
                agent: "worker".into(),
                task: "inspect".into(),
            },
        );
        assert_eq!(state.activities[0].status, ActivityStatus::Queued);
        reduce_agent_event(
            &mut state,
            AgentEvent::SubagentStarted {
                run_id: 1,
                task_index: 0,
                journal_run_id: "journal-1".into(),
            },
        );
        assert_eq!(state.activities[0].status, ActivityStatus::Running);
        reduce_agent_event(
            &mut state,
            AgentEvent::SubagentFinished {
                run_id: 1,
                task_index: 0,
                journal_run_id: "journal-1".into(),
                succeeded: false,
                error: Some("cancelled".into()),
            },
        );
        assert_eq!(state.activities[0].status, ActivityStatus::Cancelled);
        reduce_agent_event(
            &mut state,
            AgentEvent::AgentError {
                error: "boom".into(),
            },
        );
        assert_eq!(state.status, RunStatus::Failed);
        assert!(matches!(
            state.messages.last().unwrap().msg_type,
            MessageType::Error
        ));
    }

    #[test]
    fn agent_lifecycle_updates_run_status() {
        let mut state = AppState::test_state();
        reduce_agent_event(&mut state, AgentEvent::AgentStart);
        assert_eq!(state.status, RunStatus::Running);
        reduce_agent_event(
            &mut state,
            AgentEvent::AgentEnd {
                usage: Default::default(),
            },
        );
        assert_eq!(state.status, RunStatus::Succeeded);
    }

    #[test]
    fn completion_selection_wraps_in_both_directions() {
        let mut state = AppState::test_state();
        state.show_completion(
            CompletionMode::Command,
            vec!["/model".into(), "/models".into(), "/help".into()],
        );

        assert_eq!(state.completion.selected, 0);

        state.select_previous_completion();
        assert_eq!(state.completion.selected, 2);

        state.select_next_completion();
        assert_eq!(state.completion.selected, 0);

        state.select_next_completion();
        assert_eq!(state.completion.selected, 1);
    }

    #[test]
    fn closing_completion_clears_candidates_and_mode() {
        let mut state = AppState::test_state();
        state.show_completion(CompletionMode::Model, vec!["gpt-4o".into(), "gpt-5".into()]);

        state.close_completion();

        assert!(!state.completion.visible);
        assert!(state.completion.candidates.is_empty());
        assert_eq!(state.completion.selected, 0);
        assert!(state.completion.mode.is_none());
    }

    #[test]
    fn test_app_state_initialization() {
        let state = AppState::new("gpt-4o".into(), "/tmp/work".into());
        assert_eq!(state.model, "gpt-4o");
        assert_eq!(state.work_dir, "/tmp/work");
        assert!(state.composer.is_empty());
        assert_eq!(state.status, RunStatus::Ready);
        assert_eq!(state.messages.len(), 1);
    }

    #[test]
    fn test_message_types() {
        let message = TranscriptMessage {
            msg_type: MessageType::Error,
            content: "Test Error".into(),
        };
        assert!(matches!(message.msg_type, MessageType::Error));
    }
}
