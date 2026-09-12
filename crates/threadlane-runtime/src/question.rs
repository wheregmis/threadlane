//! Model-initiated clarifying questions (issue #40: Ask Questions).
//!
//! Canonical home for the question manager, handle, and `ask_question` tool
//! executor, previously defined in `threadlane_session::question`. It
//! depends only on runtime types (`AgentEvent`, `QuestionRequest`,
//! `ToolExecutor`), so it lives with the execution engine. Re-exported
//! through `threadlane_session::question` for compatibility; new code should
//! import `threadlane_runtime::question` directly.
//!
//! The `ask_question` tool gives the model a structured way to ask the user
//! for decisions instead of guessing. It mirrors the existing permission
//! flow: the executor publishes [`AgentEvent::QuestionRequested`] and blocks
//! on a one-shot responder until a surface adapter (GPUI, daemon client)
//! resolves it through [`QuestionHandle::resolve`].
//!
//! Safeguards (matching the permission default-deny posture):
//! - Unattended sessions (non-interactive, e.g. supervisor `/task`) fail fast
//!   with an explanatory error instead of blocking forever.
//! - A request with no event listener also fails fast.
//! - A dropped or dismissed request resolves as an error, never as consent.

use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use crate::{
    AgentEvent, AgentToolDefinition, QuestionAnswer, QuestionItem, QuestionRequest, ToolExecutor,
};
use tokio::sync::{broadcast, oneshot};

pub const ASK_QUESTION_TOOL_NAME: &str = "ask_question";
const MAX_QUESTIONS: usize = 4;
const MAX_OPTIONS: usize = 6;
const MAX_TEXT_CHARS: usize = 500;

/// Cloneable handle used to ask questions and resolve pending requests.
#[derive(Clone)]
pub struct QuestionHandle {
    inner: Arc<QuestionManagerInner>,
}

struct QuestionManagerInner {
    interactive: AtomicBool,
    next_id: AtomicU64,
    pending: Mutex<HashMap<String, oneshot::Sender<QuestionAnswer>>>,
}

pub struct QuestionManager {
    handle: QuestionHandle,
}

impl QuestionManager {
    pub fn new() -> Self {
        Self {
            handle: QuestionHandle {
                inner: Arc::new(QuestionManagerInner {
                    interactive: AtomicBool::new(false),
                    next_id: AtomicU64::new(1),
                    pending: Mutex::new(HashMap::new()),
                }),
            },
        }
    }

    pub fn handle(&self) -> QuestionHandle {
        self.handle.clone()
    }
}

impl QuestionHandle {
    pub fn set_interactive(&self, interactive: bool) {
        self.inner.interactive.store(interactive, Ordering::SeqCst);
    }

    /// Resolves a pending question request. Returns false when no request
    /// with that id is pending.
    pub fn resolve(&self, request_id: &str, answer: QuestionAnswer) -> bool {
        let sender = self
            .inner
            .pending
            .lock()
            .map(|mut pending| pending.remove(request_id))
            .unwrap_or(None);
        match sender {
            Some(sender) => sender.send(answer).is_ok(),
            None => false,
        }
    }

    /// Publishes a [`QuestionRequest`] and waits for the user's answer.
    pub async fn ask(
        &self,
        event_tx: &broadcast::Sender<AgentEvent>,
        questions: Vec<QuestionItem>,
    ) -> Result<QuestionAnswer, String> {
        if !self.inner.interactive.load(Ordering::SeqCst) {
            return Err("ask_question is unavailable: this session has no interactive UI attached. Ask the question in plain text instead.".into());
        }
        let request_id = format!("q_{}", self.inner.next_id.fetch_add(1, Ordering::SeqCst));
        let (sender, receiver) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .map(|mut pending| pending.insert(request_id.clone(), sender))
            .map_err(|_| "Question state is unavailable".to_string())?;
        let request = QuestionRequest {
            id: request_id.clone(),
            questions,
        };
        if event_tx
            .send(AgentEvent::QuestionRequested { request })
            .is_err()
        {
            self.inner
                .pending
                .lock()
                .map(|mut pending| pending.remove(&request_id))
                .ok();
            return Err(
                "ask_question is unavailable: no event listener is attached. Ask the question in plain text instead.".into(),
            );
        }
        let answer = receiver.await.map_err(|_| {
            "The question was dismissed before an answer arrived. Continue with your best judgment or ask again in plain text.".to_string()
        });
        self.inner
            .pending
            .lock()
            .map(|mut pending| pending.remove(&request_id))
            .ok();
        answer
    }
}

#[derive(Deserialize)]
struct AskQuestionArgs {
    #[serde(default)]
    questions: Vec<AskQuestionItemArgs>,
}

#[derive(Deserialize)]
struct AskQuestionItemArgs {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    header: Option<String>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    allow_custom: bool,
}

fn parse_ask_question(args: &str) -> Result<Vec<QuestionItem>, String> {
    let parsed: AskQuestionArgs = serde_json::from_str(args)
        .map_err(|error| format!("Invalid ask_question arguments: {error}"))?;
    if parsed.questions.is_empty() {
        return Err("ask_question requires at least one question".into());
    }
    if parsed.questions.len() > MAX_QUESTIONS {
        return Err(format!(
            "ask_question accepts at most {MAX_QUESTIONS} questions per call"
        ));
    }
    let mut questions = Vec::with_capacity(parsed.questions.len());
    for (index, item) in parsed.questions.into_iter().enumerate() {
        let question = item.question.unwrap_or_default().trim().to_string();
        if question.is_empty() {
            return Err(format!(
                "Question {} requires a non-empty `question`",
                index + 1
            ));
        }
        if question.chars().count() > MAX_TEXT_CHARS {
            return Err(format!(
                "Question {} may contain at most {MAX_TEXT_CHARS} characters",
                index + 1
            ));
        }
        if item.options.len() > MAX_OPTIONS {
            return Err(format!(
                "Question {} accepts at most {MAX_OPTIONS} options",
                index + 1
            ));
        }
        let header = item.header.unwrap_or_default().trim().to_string();
        questions.push(QuestionItem {
            id: item.id.unwrap_or_else(|| format!("q{}", index + 1)),
            header: if header.is_empty() {
                format!("Question {}", index + 1)
            } else {
                header
            },
            question,
            options: item.options,
            allow_custom: item.allow_custom,
        });
    }
    Ok(questions)
}

fn format_answer(answer: &QuestionAnswer) -> String {
    if answer.dismissed || answer.answers.is_empty() {
        return "The user dismissed the questions without answering. Continue with your best judgment or ask again in plain text.".into();
    }
    let mut lines = Vec::with_capacity(answer.answers.len());
    for item in &answer.answers {
        let mut line = format!("{}: ", item.question_id);
        if item.selected.is_empty() {
            line.push_str("(no option selected)");
        } else {
            line.push_str(&item.selected.join(", "));
        }
        if let Some(custom) = &item.custom_text {
            if !custom.trim().is_empty() {
                line.push_str(&format!(" (note: {custom})"));
            }
        }
        lines.push(line);
    }
    format!("User answers:\n{}", lines.join("\n"))
}

pub struct AskQuestionToolExecutor {
    handle: QuestionHandle,
    event_tx: broadcast::Sender<AgentEvent>,
}

impl AskQuestionToolExecutor {
    pub fn new(handle: QuestionHandle, event_tx: broadcast::Sender<AgentEvent>) -> Self {
        Self { handle, event_tx }
    }
}

#[async_trait]
impl ToolExecutor for AskQuestionToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.host.ask_question"
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        vec![AgentToolDefinition::new(
            ASK_QUESTION_TOOL_NAME,
            "Ask the user up to 4 clarifying questions and wait for their answers. Use this when a decision materially affects the outcome (scope, approach, ambiguous requirements) instead of guessing. Keep questions short and offer 2-4 concrete options each.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_QUESTIONS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Stable id referenced in the answer (defaults to q1, q2, ...)" },
                                "header": { "type": "string", "description": "Short label shown in the prompt UI" },
                                "question": { "type": "string", "description": "The full question text" },
                                "options": {
                                    "type": "array",
                                    "maxItems": MAX_OPTIONS,
                                    "items": { "type": "string" },
                                    "description": "Concrete answer options for the user to pick from"
                                },
                                "allow_custom": { "type": "boolean", "description": "Whether the user may answer with free text" }
                            },
                            "required": ["question"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["questions"],
                "additionalProperties": false
            }),
        )]
        .into()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        if name != ASK_QUESTION_TOOL_NAME {
            return None;
        }
        let questions = match parse_ask_question(args) {
            Ok(questions) => questions,
            Err(error) => return Some(Err(error)),
        };
        match self.handle.ask(&self.event_tx, questions).await {
            Ok(answer) => Some(Ok(format_answer(&answer))),
            Err(error) => Some(Err(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_channel() -> broadcast::Sender<AgentEvent> {
        broadcast::channel(16).0
    }

    #[test]
    fn parse_rejects_empty_and_oversized_requests() {
        assert!(parse_ask_question(r#"{"questions": []}"#).is_err());
        assert!(parse_ask_question(r#"{"questions": [{"question": ""}]}"#).is_err());
        assert!(parse_ask_question("not json").is_err());
        let five = serde_json::json!({"questions": [{"question": "a"},{"question": "b"},{"question": "c"},{"question": "d"},{"question": "e"}]});
        assert!(parse_ask_question(&five.to_string()).is_err());
        let many_options = serde_json::json!({"questions": [{"question": "pick", "options": ["1","2","3","4","5","6","7"]}]});
        assert!(parse_ask_question(&many_options.to_string()).is_err());
    }

    #[test]
    fn parse_fills_ids_and_headers() {
        let items = parse_ask_question(r#"{"questions": [{"question": "Which scope?"}]}"#).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "q1");
        assert_eq!(items[0].header, "Question 1");
    }

    #[tokio::test]
    async fn unattended_ask_fails_fast_without_an_event() {
        let manager = QuestionManager::new();
        let event_tx = test_channel();
        let mut events = event_tx.subscribe();
        let error = manager
            .handle()
            .ask(
                &event_tx,
                vec![QuestionItem {
                    id: "q1".into(),
                    header: "Scope".into(),
                    question: "Which scope?".into(),
                    options: vec![],
                    allow_custom: true,
                }],
            )
            .await
            .expect_err("unattended ask must fail");
        assert!(error.contains("no interactive UI"), "{error}");
        assert!(events.try_recv().is_err(), "no event is broadcast");
    }

    #[tokio::test]
    async fn interactive_ask_resolves_through_the_handle() {
        let manager = QuestionManager::new();
        let handle = manager.handle();
        handle.set_interactive(true);
        let event_tx = test_channel();
        let mut events = event_tx.subscribe();
        let ask = tokio::spawn({
            let handle = handle.clone();
            let event_tx = event_tx.clone();
            async move {
                handle
                    .ask(
                        &event_tx,
                        vec![QuestionItem {
                            id: "q1".into(),
                            header: "Scope".into(),
                            question: "Which scope?".into(),
                            options: vec!["a".into(), "b".into()],
                            allow_custom: false,
                        }],
                    )
                    .await
            }
        });
        let AgentEvent::QuestionRequested { request } =
            tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
                .await
                .expect("event arrives")
                .expect("channel open")
        else {
            panic!("expected a question request event");
        };
        assert_eq!(request.questions.len(), 1);
        let resolved = handle.resolve(
            &request.id,
            QuestionAnswer {
                request_id: request.id.clone(),
                answers: vec![crate::QuestionItemAnswer {
                    question_id: "q1".into(),
                    selected: vec!["a".into()],
                    custom_text: None,
                }],
                dismissed: false,
            },
        );
        assert!(resolved);
        let answer = ask.await.expect("task completes").expect("answer");
        assert!(!answer.dismissed);
        assert_eq!(answer.answers[0].selected, vec!["a".to_string()]);
        assert!(!handle.resolve(&request.id, QuestionAnswer::dismissed(&request.id)));
    }

    #[tokio::test]
    async fn executor_rejects_unknown_tools_and_bad_args() {
        let manager = QuestionManager::new();
        let executor = AskQuestionToolExecutor::new(manager.handle(), test_channel());
        assert!(executor.execute_tool("read_file", "{}").await.is_none());
        let result = executor
            .execute_tool(ASK_QUESTION_TOOL_NAME, r#"{"questions": []}"#)
            .await
            .expect("ask_question is handled");
        assert!(result.is_err());
    }

    #[test]
    fn dismissed_answer_formats_as_guidance() {
        let text = format_answer(&QuestionAnswer::dismissed("q_9"));
        assert!(text.contains("dismissed"), "{text}");
    }
}
