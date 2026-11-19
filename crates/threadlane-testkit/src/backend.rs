//! A backend that follows a script instead of calling a model (Plan.md §16).
//!
//! It models native sessions the way the Claude driver sees them
//! (docs/milestone-1.md §1 and §5): it remembers the sessions it has started,
//! a `Start` on a known id falls back to resuming it, and a `Resume` of an
//! unknown id ends the run with [`BackendEvent::SessionMissing`] alone.

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use threadlane_core::backend::{
    AgentBackend, AskRequest, BackendError, BackendEvent, BackendRun, NativeSession, Probe,
    RunRequest,
};
use threadlane_core::model::BackendKind;
use threadlane_core::time::{Clock, Sleep, Timestamp};
use threadlane_core::tools::ToolEndpoint;

/// One step of a scripted run.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    unpredictable_function_pointer_comparisons,
    reason = "two scripts are equal only if they name the same argument functions"
)]
pub enum ScriptStep {
    /// Emit assistant text.
    Text(String),
    /// Emit the tool use, then call one of the system's tools through the
    /// run's tool endpoint and emit the result. A [`ToolError`] becomes an
    /// error result and the script continues, as with a real backend.
    ///
    /// [`ToolError`]: threadlane_core::tools::ToolError
    CallTool {
        call_id: Option<String>,
        name: String,
        arguments: Json,
    },
    /// [`ScriptStep::CallTool`] with arguments made from the run's prompt,
    /// for a script shared by runs that must each name their own thing,
    /// such as a task's own progress file.
    CallToolFor {
        call_id: Option<String>,
        name: String,
        arguments: fn(&str) -> Json,
    },
    /// Wait on the run's clock.
    Wait(Duration),
    /// Open a turn boundary where the runner can supply a user message.
    TurnBoundary,
    /// Emit any event verbatim. A terminal event ends the run.
    Emit(BackendEvent),
    /// Finish with this final text, which becomes the run's rationale.
    Finish(Option<String>),
    /// Fail with this error.
    Fail(String),
    /// Report a usage limit.
    UsageLimit(Option<Timestamp>),
    /// Never finish; only [`BackendRun::stop`] or a dropped run ends it.
    Hang,
}

/// A whole scripted run. A script that runs out of steps finishes silently.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Script {
    steps: Vec<ScriptStep>,
}

impl Script {
    /// An empty script, which finishes silently.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a step.
    #[must_use]
    pub fn then(mut self, step: ScriptStep) -> Self {
        self.steps.push(step);
        self
    }

    /// Appends assistant text.
    #[must_use]
    pub fn text(self, text: impl Into<String>) -> Self {
        self.then(ScriptStep::Text(text.into()))
    }

    /// Appends a call to one of the system's tools.
    #[must_use]
    pub fn call(
        self,
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments: Json,
    ) -> Self {
        self.then(ScriptStep::CallTool {
            call_id: Some(call_id.into()),
            name: name.into(),
            arguments,
        })
    }

    /// Appends a call to one of the system's tools whose arguments
    /// `arguments` makes from the run's prompt.
    #[must_use]
    pub fn call_for(
        self,
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments: fn(&str) -> Json,
    ) -> Self {
        self.then(ScriptStep::CallToolFor {
            call_id: Some(call_id.into()),
            name: name.into(),
            arguments,
        })
    }

    /// Appends a wait on the run's clock.
    #[must_use]
    pub fn wait(self, duration: Duration) -> Self {
        self.then(ScriptStep::Wait(duration))
    }

    /// Ends the script by finishing with a rationale.
    #[must_use]
    pub fn finish(self, final_text: impl Into<String>) -> Self {
        self.then(ScriptStep::Finish(Some(final_text.into())))
    }

    /// Ends the script with a failure.
    #[must_use]
    pub fn fail(self, error: impl Into<String>) -> Self {
        self.then(ScriptStep::Fail(error.into()))
    }
}

/// What the scripted backend did with a run's [`NativeSession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOutcome {
    /// `Start` with an unknown id: the session was created.
    Created,
    /// `Resume` of a known id, or `Start` on a known id (which falls back to
    /// resuming it, as the Claude driver does on "already in use").
    Resumed,
    /// `Resume` of an unknown id: the run emitted only
    /// [`BackendEvent::SessionMissing`] and consumed no script.
    Missing,
}

impl SessionOutcome {
    /// Whether the run got a session (created or resumed).
    #[must_use]
    pub fn found(self) -> bool {
        self != Self::Missing
    }
}

/// A run request as the scripted backend received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedRun {
    /// The session the request asked to start or resume.
    pub session: NativeSession,
    /// What happened to that session; [`SessionOutcome::Missing`] runs are
    /// recorded too.
    pub session_outcome: SessionOutcome,
    pub working_dir: PathBuf,
    pub prompt: String,
    pub appended_instructions: String,
    pub skills: Vec<String>,
    pub model: Option<String>,
    pub deny_rules: Vec<String>,
    pub remove_builtin_tools: bool,
    pub wall_time_limit: Duration,
    pub max_turns: Option<u32>,
    /// Names of the tools the request's endpoint offered; empty if none.
    pub tools: Vec<String>,
}

impl ReceivedRun {
    /// The native session id the request named.
    #[must_use]
    pub fn session_id(&self) -> &str {
        match &self.session {
            NativeSession::Start { id } | NativeSession::Resume { id } => id,
        }
    }
}

#[derive(Debug)]
struct State {
    probe: Probe,
    scripts: VecDeque<Script>,
    /// Scripts for runs whose prompt contains the text, first match wins.
    routes: Vec<(String, Script)>,
    default_script: Option<Script>,
    questions: Vec<String>,
    answers: VecDeque<Result<Json, BackendError>>,
    received: Vec<ReceivedRun>,
    /// Native session ids this backend knows, in the order they were created.
    sessions: Vec<String>,
    /// What [`AgentBackend::installed_skills`] answers.
    skills: Result<Vec<String>, BackendError>,
}

impl State {
    fn knows(&self, id: &str) -> bool {
        self.sessions.iter().any(|known| known == id)
    }

    fn next_script(&mut self, prompt: &str) -> Option<Script> {
        self.scripts.pop_front().or_else(|| {
            self.routes
                .iter()
                .find(|(needle, _)| prompt.contains(needle.as_str()))
                .map(|(_, script)| script.clone())
                .or_else(|| self.default_script.clone())
        })
    }
}

/// An [`AgentBackend`] that plays [`Script`]s, one per run.
///
/// Queued scripts ([`ScriptedBackend::push`]) are used in order; when the
/// queue is empty the first script routed to text in the run's prompt
/// ([`ScriptedBackend::route`]) is used, else the default script
/// ([`ScriptedBackend::set_default`]). With neither, [`AgentBackend::run`] fails, so a test notices an
/// unexpected run.
#[derive(Debug, Clone)]
pub struct ScriptedBackend {
    kind: BackendKind,
    clock: Arc<dyn Clock>,
    state: Arc<Mutex<State>>,
}

impl ScriptedBackend {
    /// A logged-in backend with no scripts and no sessions. Waits in scripts
    /// run on `clock`.
    #[must_use]
    pub fn new(kind: BackendKind, clock: Arc<dyn Clock>) -> Self {
        Self {
            kind,
            clock,
            state: Arc::new(Mutex::new(State {
                probe: Probe {
                    installed: true,
                    version: Some("scripted".into()),
                    logged_in: true,
                    account_label: Some(format!("{kind}-scripted")),
                },
                scripts: VecDeque::new(),
                routes: Vec::new(),
                default_script: None,
                answers: VecDeque::new(),
                questions: Vec::new(),
                received: Vec::new(),
                sessions: Vec::new(),
                skills: Ok(Vec::new()),
            })),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("scripted backend lock poisoned")
    }

    /// Queues the script for the next run. Queued scripts take priority over
    /// the default.
    pub fn push(&self, script: Script) {
        self.state().scripts.push_back(script);
    }

    /// Uses `script` for every run that finds the queue empty and whose
    /// prompt contains `needle`, unless an earlier route matches. A run that
    /// is repeated after a crash gets the same script again, whichever lane
    /// it is in and in whatever order the lanes run. A route for the same
    /// text again replaces the earlier one, in its place.
    pub fn route(&self, needle: impl Into<String>, script: Script) {
        let needle = needle.into();
        let mut state = self.state();
        match state.routes.iter_mut().find(|(known, _)| *known == needle) {
            Some(route) => route.1 = script,
            None => state.routes.push((needle, script)),
        }
    }

    /// Uses `script` for every run that finds the queue empty.
    pub fn set_default(&self, script: Script) {
        self.state().default_script = Some(script);
    }

    /// Removes the default script, so a run with an empty queue fails again.
    pub fn clear_default(&self) {
        self.state().default_script = None;
    }

    /// The one-shot questions received in order.
    pub fn questions(&self) -> Vec<String> {
        self.state().questions.clone()
    }

    /// Queues the answer for the next one-shot question.
    pub fn push_answer(&self, answer: Result<Json, BackendError>) {
        self.state().answers.push_back(answer);
    }

    /// Replaces what [`AgentBackend::installed_skills`] answers.
    pub fn set_skills(&self, skills: Result<Vec<String>, BackendError>) {
        self.state().skills = skills;
    }

    /// Replaces what [`AgentBackend::probe`] reports.
    pub fn set_probe(&self, probe: Probe) {
        self.state().probe = probe;
    }

    /// Makes the backend know session `id`, as if an earlier run had created it.
    pub fn add_session(&self, id: impl Into<String>) {
        let id = id.into();
        let mut state = self.state();
        if !state.knows(&id) {
            state.sessions.push(id);
        }
    }

    /// Simulates the native session `id` being deleted on this machine: a
    /// later `Resume` of it ends with [`BackendEvent::SessionMissing`].
    /// Returns whether the session was known.
    pub fn forget_session(&self, id: &str) -> bool {
        let mut state = self.state();
        let before = state.sessions.len();
        state.sessions.retain(|known| known != id);
        state.sessions.len() != before
    }

    /// Native session ids this backend knows, in the order they were created.
    #[must_use]
    pub fn sessions(&self) -> Vec<String> {
        self.state().sessions.clone()
    }

    /// Every run received so far, including those that ended in
    /// [`BackendEvent::SessionMissing`]. Runs refused for lack of a script
    /// are not recorded.
    #[must_use]
    pub fn received(&self) -> Vec<ReceivedRun> {
        self.state().received.clone()
    }

    /// Queued scripts not yet used (the default script is not counted).
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.state().scripts.len()
    }
}

#[async_trait]
impl AgentBackend for ScriptedBackend {
    fn kind(&self) -> BackendKind {
        self.kind
    }

    async fn installed_skills(
        &self,
        _cwd: Option<&std::path::Path>,
    ) -> Result<Vec<String>, BackendError> {
        self.state().skills.clone()
    }

    async fn probe(&self) -> Result<Probe, BackendError> {
        Ok(self.state().probe.clone())
    }

    async fn run(&self, request: RunRequest) -> Result<Box<dyn BackendRun>, BackendError> {
        // Ask the endpoint before taking the lock: it is foreign code.
        let tools = request
            .tools
            .as_ref()
            .map(|endpoint| {
                endpoint
                    .definitions()
                    .into_iter()
                    .map(|definition| definition.name)
                    .collect()
            })
            .unwrap_or_default();

        let script = {
            let mut state = self.state();
            let (outcome, script) = match &request.session {
                NativeSession::Resume { id } if !state.knows(id) => (SessionOutcome::Missing, None),
                NativeSession::Start { id } | NativeSession::Resume { id } => {
                    let script = state.next_script(&request.prompt).ok_or_else(|| {
                        BackendError::Failed(
                            "scripted backend: no script queued for this run".into(),
                        )
                    })?;
                    if state.knows(id) {
                        (SessionOutcome::Resumed, Some(script))
                    } else {
                        state.sessions.push(id.clone());
                        (SessionOutcome::Created, Some(script))
                    }
                }
            };
            state.received.push(ReceivedRun {
                session: request.session.clone(),
                session_outcome: outcome,
                working_dir: request.working_dir.clone(),
                prompt: request.prompt.clone(),
                appended_instructions: request.appended_instructions.clone(),
                skills: request.skills.clone(),
                model: request.model.clone(),
                deny_rules: request.deny_rules.clone(),
                remove_builtin_tools: request.remove_builtin_tools,
                wall_time_limit: request.wall_time_limit,
                max_turns: request.max_turns,
                tools,
            });
            script
        };

        let run = match script {
            None => ScriptedRun {
                prompt: String::new(),
                steps: VecDeque::new(),
                buffered: VecDeque::from([BackendEvent::SessionMissing]),
                tools: None,
                clock: self.clock.clone(),
                pending: Pending::Idle,
                ended: true,
            },
            Some(script) => {
                let native_session_id = match request.session {
                    NativeSession::Start { id } | NativeSession::Resume { id } => id,
                };
                ScriptedRun {
                    prompt: request.prompt,
                    steps: script.steps.into(),
                    buffered: VecDeque::from([BackendEvent::SessionStarted { native_session_id }]),
                    tools: request.tools,
                    clock: self.clock.clone(),
                    pending: Pending::Idle,
                    ended: false,
                }
            }
        };
        Ok(Box::new(run))
    }

    async fn ask(&self, request: AskRequest) -> Result<Json, BackendError> {
        let mut state = self.state();
        state.questions.push(request.question);
        state.answers.pop_front().unwrap_or_else(|| {
            Err(BackendError::Failed(
                "scripted backend: no answer queued".into(),
            ))
        })
    }
}

/// A tool call in flight: resolves to the result's output and `is_error`.
type ToolCall = Pin<Box<dyn Future<Output = (Json, bool)> + Send>>;

/// A step that is under way. Kept in the run so that dropping a
/// `next_event` future (for example when the runner races it against its
/// wall-time limit) does not lose or skip the step.
enum Pending {
    Idle,
    Wait(Sleep),
    Call {
        call_id: Option<String>,
        call: ToolCall,
    },
    Hang,
}

struct ScriptedRun {
    prompt: String,
    steps: VecDeque<ScriptStep>,
    buffered: VecDeque<BackendEvent>,
    tools: Option<Arc<dyn ToolEndpoint>>,
    clock: Arc<dyn Clock>,
    pending: Pending,
    ended: bool,
}

impl std::fmt::Debug for ScriptedRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pending = match self.pending {
            Pending::Idle => "idle",
            Pending::Wait(_) => "wait",
            Pending::Call { .. } => "call",
            Pending::Hang => "hang",
        };
        f.debug_struct("ScriptedRun")
            .field("steps_left", &self.steps.len())
            .field("pending", &pending)
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl ScriptedRun {
    fn end_with(&mut self, event: BackendEvent) -> Option<BackendEvent> {
        self.ended = true;
        Some(event)
    }

    fn start_call(&self, name: String, call_id: Option<String>, arguments: Json) -> ToolCall {
        let Some(tools) = self.tools.clone() else {
            return Box::pin(std::future::ready((
                json!({ "error": "no tool server" }),
                true,
            )));
        };
        Box::pin(async move {
            match tools.call(call_id.as_deref(), &name, arguments).await {
                Ok(output) => (output, false),
                Err(error) => (json!({ "error": error.to_string() }), true),
            }
        })
    }
}

#[async_trait]
impl BackendRun for ScriptedRun {
    async fn next_event(&mut self) -> Option<BackendEvent> {
        loop {
            if let Some(event) = self.buffered.pop_front() {
                return Some(event);
            }
            if self.ended {
                return None;
            }
            match &mut self.pending {
                Pending::Idle => {}
                Pending::Wait(sleep) => {
                    sleep.await;
                    self.pending = Pending::Idle;
                    continue;
                }
                Pending::Call { call_id, call } => {
                    let call_id = call_id.clone();
                    let (output, is_error) = call.await;
                    self.pending = Pending::Idle;
                    return Some(BackendEvent::ToolResult {
                        call_id,
                        output,
                        is_error,
                    });
                }
                Pending::Hang => std::future::pending::<()>().await,
            }
            let Some(step) = self.steps.pop_front() else {
                return self.end_with(BackendEvent::Finished {
                    final_text: None,
                    usage: json!({}),
                });
            };
            match step {
                ScriptStep::Text(text) => return Some(BackendEvent::Text { text }),
                ScriptStep::TurnBoundary => return Some(BackendEvent::TurnBoundary),
                ScriptStep::Emit(event) => {
                    if event.is_terminal() {
                        return self.end_with(event);
                    }
                    return Some(event);
                }
                ScriptStep::CallToolFor {
                    call_id,
                    name,
                    arguments,
                } => {
                    let arguments = arguments(&self.prompt);
                    self.steps.push_front(ScriptStep::CallTool {
                        call_id,
                        name,
                        arguments,
                    });
                }
                ScriptStep::CallTool {
                    call_id,
                    name,
                    arguments,
                } => {
                    let call = self.start_call(name.clone(), call_id.clone(), arguments.clone());
                    self.pending = Pending::Call {
                        call_id: call_id.clone(),
                        call,
                    };
                    return Some(BackendEvent::ToolUse {
                        call_id,
                        name,
                        input: arguments,
                    });
                }
                ScriptStep::Wait(duration) => {
                    self.pending = Pending::Wait(self.clock.sleep(duration));
                }
                ScriptStep::Finish(final_text) => {
                    return self.end_with(BackendEvent::Finished {
                        final_text,
                        usage: json!({}),
                    });
                }
                ScriptStep::Fail(error) => return self.end_with(BackendEvent::Failed { error }),
                ScriptStep::UsageLimit(resets_at) => {
                    return self.end_with(BackendEvent::UsageLimit { resets_at });
                }
                ScriptStep::Hang => self.pending = Pending::Hang,
            }
        }
    }

    async fn fold_in(&mut self, message: &str) -> Result<bool, BackendError> {
        self.prompt.push_str("\n\n");
        self.prompt.push_str(message);
        Ok(true)
    }

    async fn stop(&mut self) -> Result<(), BackendError> {
        self.steps.clear();
        self.buffered.clear();
        self.pending = Pending::Idle;
        self.ended = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use threadlane_core::tools::{Tier, ToolDefinition, ToolError};

    use super::*;
    use crate::FakeClock;
    use crate::fakes::{FakeTools, ToolCallRecord};

    /// Long enough to be sure a run is blocked, short enough to keep tests fast.
    const BLOCKED: Duration = Duration::from_millis(50);

    fn backend() -> (FakeClock, ScriptedBackend) {
        let clock = FakeClock::at_default_start();
        let backend = ScriptedBackend::new(BackendKind::Claude, Arc::new(clock.clone()));
        (clock, backend)
    }

    fn start(id: &str) -> NativeSession {
        NativeSession::Start { id: id.into() }
    }

    fn resume(id: &str) -> NativeSession {
        NativeSession::Resume { id: id.into() }
    }

    fn request(session: NativeSession) -> RunRequest {
        RunRequest {
            working_dir: PathBuf::from("/lanes/lane-1"),
            prompt: "hello".into(),
            images: Vec::new(),
            skills: Vec::new(),
            appended_instructions: "lead rules".into(),
            model: Some("haiku".into()),
            session,
            tools: None,
            deny_rules: vec!["Bash(rm:*)".into()],
            remove_builtin_tools: true,
            wall_time_limit: Duration::from_secs(600),
            max_turns: Some(40),
        }
    }

    async fn drain(run: &mut Box<dyn BackendRun>) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        while let Some(event) = run.next_event().await {
            events.push(event);
        }
        events
    }

    async fn play(backend: &ScriptedBackend, request: RunRequest) -> Vec<BackendEvent> {
        let mut run = backend.run(request).await.expect("run starts");
        drain(&mut run).await
    }

    fn started(id: &str) -> BackendEvent {
        BackendEvent::SessionStarted {
            native_session_id: id.into(),
        }
    }

    fn text(text: &str) -> BackendEvent {
        BackendEvent::Text { text: text.into() }
    }

    fn finished(final_text: &str) -> BackendEvent {
        BackendEvent::Finished {
            final_text: Some(final_text.into()),
            usage: json!({}),
        }
    }

    fn send_message_tool() -> ToolDefinition {
        ToolDefinition {
            name: "send_message".into(),
            description: "Sends a message to the user.".into(),
            input_schema: json!({ "type": "object" }),
            tier: Tier::Local,
            safe_to_repeat: false,
        }
    }

    #[tokio::test]
    async fn start_creates_the_session_and_resume_continues_it() {
        let (_clock, backend) = backend();
        backend.push(Script::new().text("one").finish("first"));
        backend.push(Script::new().finish("second"));

        let events = play(&backend, request(start("s1"))).await;
        assert_eq!(events, vec![started("s1"), text("one"), finished("first")]);
        assert_eq!(backend.sessions(), vec!["s1".to_owned()]);

        let events = play(&backend, request(resume("s1"))).await;
        assert_eq!(events, vec![started("s1"), finished("second")]);
        assert_eq!(backend.sessions(), vec!["s1".to_owned()]);

        let received = backend.received();
        assert_eq!(received.len(), 2);
        assert_eq!(received[0].session, start("s1"));
        assert_eq!(received[0].session_outcome, SessionOutcome::Created);
        assert_eq!(received[1].session, resume("s1"));
        assert_eq!(received[1].session_outcome, SessionOutcome::Resumed);
        assert!(received.iter().all(|run| run.session_outcome.found()));
        assert_eq!(backend.remaining(), 0);
    }

    #[tokio::test]
    async fn resuming_an_unknown_session_reports_only_session_missing() {
        let (_clock, backend) = backend();
        backend.push(Script::new().text("never played"));

        let events = play(&backend, request(resume("gone"))).await;
        assert_eq!(events, vec![BackendEvent::SessionMissing]);
        assert_eq!(backend.remaining(), 1, "the script is still queued");
        assert!(backend.sessions().is_empty());

        let received = backend.received();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].session, resume("gone"));
        assert_eq!(received[0].session_outcome, SessionOutcome::Missing);
        assert!(!received[0].session_outcome.found());
    }

    #[tokio::test]
    async fn a_routed_script_plays_for_prompts_with_its_text_after_the_queue() {
        let (_clock, backend) = backend();
        backend.set_default(Script::new().finish("default"));
        backend.route("task", Script::new().finish("task"));
        backend.route(
            "ask",
            Script::new().finish("never: an earlier route matches"),
        );
        let with_prompt = |id: &str, prompt: &str| RunRequest {
            prompt: prompt.into(),
            ..request(start(id))
        };

        let events = play(&backend, with_prompt("s1", "a task to ask about")).await;
        assert_eq!(events, vec![started("s1"), finished("task")]);
        let events = play(&backend, with_prompt("s2", "a message")).await;
        assert_eq!(events, vec![started("s2"), finished("default")]);
        backend.push(Script::new().finish("queued"));
        let events = play(&backend, with_prompt("s3", "a task")).await;
        assert_eq!(events, vec![started("s3"), finished("queued")]);
    }

    #[tokio::test]
    async fn resuming_an_unknown_session_needs_no_script() {
        let (_clock, backend) = backend();
        let events = play(&backend, request(resume("gone"))).await;
        assert_eq!(events, vec![BackendEvent::SessionMissing]);
    }

    #[tokio::test]
    async fn start_on_a_known_id_resumes_it() {
        let (_clock, backend) = backend();
        backend.set_default(Script::new().finish("done"));

        play(&backend, request(start("s1"))).await;
        let events = play(&backend, request(start("s1"))).await;
        assert_eq!(events, vec![started("s1"), finished("done")]);
        assert_eq!(backend.sessions(), vec!["s1".to_owned()]);
        assert_eq!(
            backend.received()[1].session_outcome,
            SessionOutcome::Resumed
        );

        backend.add_session("s2");
        let events = play(&backend, request(start("s2"))).await;
        assert_eq!(events, vec![started("s2"), finished("done")]);
        assert_eq!(
            backend.received()[2].session_outcome,
            SessionOutcome::Resumed
        );
    }

    #[tokio::test]
    async fn a_forgotten_session_is_missing_on_resume() {
        let (_clock, backend) = backend();
        backend.set_default(Script::new());
        play(&backend, request(start("s1"))).await;
        play(&backend, request(start("s2"))).await;
        assert_eq!(backend.sessions(), vec!["s1".to_owned(), "s2".to_owned()]);

        assert!(backend.forget_session("s1"));
        assert!(!backend.forget_session("s1"));
        assert_eq!(backend.sessions(), vec!["s2".to_owned()]);

        let events = play(&backend, request(resume("s1"))).await;
        assert_eq!(events, vec![BackendEvent::SessionMissing]);
        let events = play(&backend, request(resume("s2"))).await;
        assert_eq!(events.first(), Some(&started("s2")));
    }

    #[tokio::test]
    async fn the_default_script_plays_whenever_the_queue_is_empty() {
        let (_clock, backend) = backend();
        backend.set_default(Script::new().text("default").finish("ok"));

        for id in ["a", "b"] {
            let events = play(&backend, request(start(id))).await;
            assert_eq!(events, vec![started(id), text("default"), finished("ok")]);
        }
        assert_eq!(backend.remaining(), 0);

        backend.clear_default();
        let error = backend
            .run(request(start("c")))
            .await
            .expect_err("no script left");
        assert!(matches!(error, BackendError::Failed(_)));
    }

    #[tokio::test]
    async fn queued_scripts_take_priority_over_the_default() {
        let (_clock, backend) = backend();
        backend.set_default(Script::new().text("default"));
        backend.push(Script::new().text("queued"));

        let first = play(&backend, request(start("s1"))).await;
        assert_eq!(first[1], text("queued"));
        let second = play(&backend, request(resume("s1"))).await;
        assert_eq!(second[1], text("default"));
    }

    #[tokio::test]
    async fn a_run_with_no_script_fails_and_changes_nothing() {
        let (_clock, backend) = backend();
        let error = backend
            .run(request(start("s1")))
            .await
            .expect_err("no script queued");
        assert!(matches!(error, BackendError::Failed(_)));
        assert!(backend.received().is_empty());
        assert!(backend.sessions().is_empty());
    }

    #[tokio::test]
    async fn the_request_is_recorded() {
        let (_clock, backend) = backend();
        backend.push(Script::new());
        play(&backend, request(start("s1"))).await;

        let tools = FakeTools::new();
        tools.register(send_message_tool(), Ok(json!({})));
        backend.push(Script::new());
        let mut with_tools = request(resume("s1"));
        with_tools.tools = Some(Arc::new(tools));
        with_tools.remove_builtin_tools = false;
        play(&backend, with_tools).await;

        let received = backend.received();
        assert_eq!(
            received[0],
            ReceivedRun {
                session: start("s1"),
                session_outcome: SessionOutcome::Created,
                working_dir: PathBuf::from("/lanes/lane-1"),
                prompt: "hello".into(),
                skills: Vec::new(),
                appended_instructions: "lead rules".into(),
                model: Some("haiku".into()),
                deny_rules: vec!["Bash(rm:*)".into()],
                remove_builtin_tools: true,
                wall_time_limit: Duration::from_secs(600),
                max_turns: Some(40),
                tools: Vec::new(),
            }
        );
        assert_eq!(received[0].session_id(), "s1");
        assert_eq!(received[1].tools, vec!["send_message".to_owned()]);
        assert!(!received[1].remove_builtin_tools);
    }

    #[tokio::test]
    async fn a_tool_call_reaches_the_endpoint_and_its_result_is_emitted() {
        let (_clock, backend) = backend();
        let tools = FakeTools::new();
        tools.register(send_message_tool(), Ok(json!({ "outbox_id": 7 })));
        backend.push(
            Script::new()
                .call("toolu_1", "send_message", json!({ "text": "hi" }))
                .finish("sent"),
        );
        let mut request = request(start("s1"));
        request.tools = Some(Arc::new(tools.clone()));

        let mut run = backend.run(request).await.expect("run starts");
        assert_eq!(run.next_event().await, Some(started("s1")));
        assert_eq!(
            run.next_event().await,
            Some(BackendEvent::ToolUse {
                call_id: Some("toolu_1".into()),
                name: "send_message".into(),
                input: json!({ "text": "hi" }),
            })
        );
        assert!(
            tools.calls().is_empty(),
            "the use is reported before the call"
        );
        assert_eq!(
            run.next_event().await,
            Some(BackendEvent::ToolResult {
                call_id: Some("toolu_1".into()),
                output: json!({ "outbox_id": 7 }),
                is_error: false,
            })
        );
        assert_eq!(
            tools.calls(),
            vec![ToolCallRecord {
                call_id: Some("toolu_1".into()),
                name: "send_message".into(),
                arguments: json!({ "text": "hi" }),
            }]
        );
        assert_eq!(drain(&mut run).await, vec![finished("sent")]);
    }

    #[tokio::test]
    async fn a_tool_error_becomes_an_error_result_and_the_script_continues() {
        let (_clock, backend) = backend();
        let tools = FakeTools::new();
        tools.register(send_message_tool(), Err(ToolError::LeaseLost));
        backend.push(
            Script::new()
                .call("toolu_1", "send_message", json!({ "text": "hi" }))
                .text("after")
                .finish("done"),
        );
        let mut request = request(start("s1"));
        request.tools = Some(Arc::new(tools));

        let events = play(&backend, request).await;
        assert_eq!(
            events[2],
            BackendEvent::ToolResult {
                call_id: Some("toolu_1".into()),
                output: json!({ "error": ToolError::LeaseLost.to_string() }),
                is_error: true,
            }
        );
        assert_eq!(events[3..], [text("after"), finished("done")]);
    }

    #[tokio::test]
    async fn a_tool_call_without_an_endpoint_is_an_error_result() {
        let (_clock, backend) = backend();
        backend.push(Script::new().call("toolu_1", "send_message", json!({})));
        let events = play(&backend, request(start("s1"))).await;
        assert!(matches!(
            &events[2],
            BackendEvent::ToolResult { is_error: true, .. }
        ));
    }

    #[tokio::test]
    async fn stop_ends_a_hanging_run() {
        let (_clock, backend) = backend();
        backend.push(Script::new().then(ScriptStep::Hang).finish("never"));
        let mut run = backend.run(request(start("s1"))).await.expect("run starts");
        assert_eq!(run.next_event().await, Some(started("s1")));

        // Polling again after a dropped `next_event` keeps hanging.
        for _ in 0..2 {
            let polled = tokio::time::timeout(BLOCKED, run.next_event()).await;
            assert!(polled.is_err(), "a hanging run produces nothing");
        }

        run.stop().await.expect("stop succeeds");
        assert_eq!(run.next_event().await, None);
    }

    #[tokio::test]
    async fn waits_run_on_the_fake_clock() {
        let (clock, backend) = backend();
        backend.push(
            Script::new()
                .wait(Duration::from_secs(10))
                .text("after")
                .finish("done"),
        );
        let mut run = backend.run(request(start("s1"))).await.expect("run starts");
        assert_eq!(run.next_event().await, Some(started("s1")));

        let polled = tokio::time::timeout(BLOCKED, run.next_event()).await;
        assert!(polled.is_err(), "the run waits for the fake clock");
        assert_eq!(clock.sleepers(), 1);

        clock.advance(Duration::from_secs(9));
        let polled = tokio::time::timeout(BLOCKED, run.next_event()).await;
        assert!(polled.is_err(), "the deadline survives a dropped poll");

        clock.advance(Duration::from_secs(1));
        assert_eq!(run.next_event().await, Some(text("after")));
        assert_eq!(clock.sleepers(), 0);
        assert_eq!(drain(&mut run).await, vec![finished("done")]);
    }
}
