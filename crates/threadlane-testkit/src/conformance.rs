//! Backend conformance (Plan.md §16): one suite, written once against
//! [`AgentBackend`], that every driver runs against a fake of its tool in
//! `cargo test` and against the real tool in an ignored test.
//!
//! It checks each row of Plan.md §5.7 the system relies on: an unattended
//! run with full access, the event stream (with [`BackendEvent::ToolUse`]),
//! the system's tools reachable, resume by id, each of [`DENY_RULES`]
//! refusing its command, and a usage limit reported as
//! [`BackendEvent::UsageLimit`], the form the runner acts on.
//!
//! The prompts ask a real agent to do something observable; a fake reads
//! the same prompts and answers as the real tool would, according to how
//! the driver started it.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use threadlane_core::backend::{
    AgentBackend, BackendEvent, BackendRun, DENY_RULES, NativeSession, RunRequest,
};
use threadlane_core::tools::{Tier, ToolDefinition, ToolEndpoint, ToolError};

use crate::git::bare_repository;

/// What the agent is asked to send with `send_message`.
pub const SENT: &str = "hi";
/// The file the agent is asked to create with its own shell.
pub const FILE: &str = "conformance.txt";
/// The first run's prompt: the system's tool, then full access.
pub const TOOLS_PROMPT: &str = "Do these three things, in order. 1. Call the send_message tool exactly once with the text \"hi\". 2. Using your shell, create a file named conformance.txt containing the word ok in the current directory. 3. Reply with the single word: done";
/// The resumed run's prompt.
pub const RESUME_PROMPT: &str = "Reply with the single word: ok";
/// A prompt only a fake answers, with the usage-limit report of its tool.
pub const USAGE_LIMIT_PROMPT: &str =
    "Threadlane conformance: report that the usage limit is reached.";
/// A session id no backend has.
pub const UNKNOWN_SESSION: &str = "5e1d0000-0000-4000-8000-00000000c0de";

/// Output a refused command would have printed had it run: `gh ... --help`
/// prints `USAGE`, a dry-run push of a new branch `[new branch]`.
const RAN: [&str; 2] = ["USAGE", "[new branch]"];

/// The prompt that asks for one shell command, quoted in backticks.
#[must_use]
pub fn command_prompt(command: &str) -> String {
    format!(
        "Run exactly this shell command, once, and nothing else: `{command}`. Then reply with the single word: done"
    )
}

/// A `send_message` tool that records its calls.
#[derive(Debug, Default)]
pub struct Recorder {
    calls: Mutex<Vec<(Option<String>, String, Json)>>,
}

impl Recorder {
    /// Each call: its call id, tool name, and arguments.
    #[must_use]
    pub fn calls(&self) -> Vec<(Option<String>, String, Json)> {
        self.calls.lock().expect("lock poisoned").clone()
    }
}

#[async_trait]
impl ToolEndpoint for Recorder {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "send_message".to_owned(),
            description: "Sends a chat message to the user.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {"text": {"type": "string", "description": "The message"}},
                "required": ["text"],
            }),
            tier: Tier::Local,
            safe_to_repeat: false,
        }]
    }

    async fn call(
        &self,
        call_id: Option<&str>,
        name: &str,
        arguments: Json,
    ) -> Result<Json, ToolError> {
        self.calls.lock().expect("lock poisoned").push((
            call_id.map(str::to_owned),
            name.to_owned(),
            arguments,
        ));
        Ok(json!({"sent": true}))
    }
}

/// One driver under test.
#[derive(Debug, Clone)]
pub struct Conformance {
    pub backend: Arc<dyn AgentBackend>,
    pub model: Option<String>,
    /// A fresh id for [`NativeSession::Start`]; a backend that picks its own
    /// ignores it.
    pub new_session_id: fn() -> String,
    /// Bounds each run.
    pub limit: Duration,
}

impl Conformance {
    fn request(&self, work: &Path, prompt: &str, session: NativeSession) -> RunRequest {
        RunRequest {
            working_dir: work.to_owned(),
            prompt: prompt.to_owned(),
            images: Vec::new(),
            appended_instructions: String::new(),
            model: self.model.clone(),
            session,
            tools: None,
            // Started as every task run is (Plan.md §9.2).
            deny_rules: DENY_RULES.iter().map(|rule| (*rule).to_owned()).collect(),
            remove_builtin_tools: false,
            wall_time_limit: self.limit,
            max_turns: Some(8),
        }
    }

    fn start(&self) -> NativeSession {
        NativeSession::Start {
            id: (self.new_session_id)(),
        }
    }

    async fn events(&self, request: RunRequest) -> Vec<BackendEvent> {
        let mut run = self.backend.run(request).await.expect("the run starts");
        let events = collect(run.as_mut(), self.limit).await;
        drop(run);
        let terminal = events.iter().filter(|event| event.is_terminal()).count();
        assert_eq!(terminal, 1, "exactly one terminal event: {events:#?}");
        assert!(
            events.last().is_some_and(BackendEvent::is_terminal),
            "{events:#?}"
        );
        events
    }

    /// Unattended run with full access, the event stream, the system's tools
    /// reachable, resume by id, and a missing session reported as such.
    ///
    /// # Panics
    ///
    /// When a row does not hold.
    pub async fn run_with_tools_then_resume(&self, work: &Path) {
        let recorder = Arc::new(Recorder::default());
        let mut first = self.request(work, TOOLS_PROMPT, self.start());
        first.tools = Some(recorder.clone());
        let events = self.events(first).await;

        let Some(BackendEvent::SessionStarted {
            native_session_id: session,
        }) = events.first().cloned()
        else {
            panic!("the stream starts with the session: {events:#?}");
        };
        let call_id = events
            .iter()
            .find_map(|event| match event {
                BackendEvent::ToolUse { call_id, name, .. } if name == "send_message" => {
                    Some(call_id.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("a send_message ToolUse, by its bare name: {events:#?}"));
        assert!(
            events.iter().any(|event| matches!(
                event,
                BackendEvent::ToolResult { call_id: id, is_error: false, .. } if *id == call_id
            )),
            "its result: {events:#?}"
        );
        let calls = recorder.calls();
        assert_eq!(
            calls.len(),
            1,
            "the system's tool was reached once: {calls:?}"
        );
        assert_eq!(calls[0].1, "send_message");
        assert_eq!(calls[0].2["text"], SENT);
        assert!(
            matches!(events.last(), Some(BackendEvent::Finished { .. })),
            "{events:#?}"
        );
        assert!(
            work.join(FILE).is_file(),
            "full access: the agent wrote {FILE} without asking: {events:#?}"
        );

        let resumed = self
            .events(self.request(
                work,
                RESUME_PROMPT,
                NativeSession::Resume {
                    id: session.clone(),
                },
            ))
            .await;
        assert_eq!(
            resumed.first(),
            Some(&BackendEvent::SessionStarted {
                native_session_id: session
            }),
            "the same session, resumed: {resumed:#?}"
        );
        assert!(
            matches!(resumed.last(), Some(BackendEvent::Finished { .. })),
            "{resumed:#?}"
        );

        let missing = self
            .events(self.request(
                work,
                RESUME_PROMPT,
                NativeSession::Resume {
                    id: UNKNOWN_SESSION.to_owned(),
                },
            ))
            .await;
        assert_eq!(missing, vec![BackendEvent::SessionMissing]);
    }

    /// Each deny rule refuses its command: asked to run it, the agent's
    /// command does not take effect. Every command is harmless had it run:
    /// a dry-run push to a local bare repository, `gh ... --help`.
    ///
    /// # Panics
    ///
    /// When a command ran.
    pub async fn each_deny_rule_refuses_its_command(&self, work: &Path) {
        let remote = bare_repository(work, "conformance");
        let repository = work.join("conformance-seed");
        for rule in DENY_RULES {
            let command = if *rule == "git push" {
                format!(
                    "git push --dry-run {} HEAD:refs/heads/conformance",
                    remote.display()
                )
            } else {
                format!("{rule} --help")
            };
            let events = self
                .events(self.request(&repository, &command_prompt(&command), self.start()))
                .await;
            for event in &events {
                if let BackendEvent::ToolResult { output, .. } = event {
                    let output = output.to_string();
                    assert!(
                        !RAN.iter().any(|ran| output.contains(ran)),
                        "{rule}: `{command}` ran: {events:#?}"
                    );
                }
            }
        }
    }

    /// The tool's usage-limit report becomes [`BackendEvent::UsageLimit`].
    /// Only a fake can be made to report one.
    ///
    /// # Panics
    ///
    /// When it does not.
    pub async fn usage_limit_is_reported(&self, work: &Path) {
        let events = self
            .events(self.request(work, USAGE_LIMIT_PROMPT, self.start()))
            .await;
        assert!(
            matches!(events.last(), Some(BackendEvent::UsageLimit { .. })),
            "{events:#?}"
        );
    }
}

async fn collect(run: &mut dyn BackendRun, limit: Duration) -> Vec<BackendEvent> {
    let mut events = Vec::new();
    let collecting = async {
        while let Some(event) = run.next_event().await {
            eprintln!("event: {event:?}");
            events.push(event);
        }
    };
    tokio::time::timeout(limit, collecting)
        .await
        .expect("the run ends within the limit");
    events
}
