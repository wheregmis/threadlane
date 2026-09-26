//! Application-owned automation service. One actor serializes storage and dispatch.
use crate::ChatStreamEvent;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use threadlane_automation::{now, Definition, Run, RunStatus, Snapshot, Store};
use threadlane_coding_agent::automation_tool::{install_creator, CreationRequest};
use threadlane_coding_agent::{
    automation::{durable_status, prepare, record_outcome, PreparedRun},
    controller::{SessionController, SessionStatus},
};
use threadlane_protocol::{AgentEvent, PermissionRequest, QuestionRequest};
use threadlane_runtime::harness::{JsonlStore, SessionStore};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

#[derive(Clone, Default)]
pub struct Projection {
    pub snapshot: Snapshot,
    pub error: Option<String>,
    pub active_runtime: Option<Arc<SessionController>>,
    pub permissions: HashMap<String, PermissionRequest>,
    pub questions: HashMap<String, QuestionRequest>,
    pub notification: Option<(String, String)>,
}
pub enum Command {
    Save(Definition),
    SetEnabled(String, bool),
    Delete(String),
    RunNow(String),
    Cancel(String),
    Review(String),
    Resolved {
        session_id: String,
        request_id: String,
    },
}
type Request = (Command, oneshot::Sender<Result<(), String>>);
pub struct AutomationService {
    commands: mpsc::UnboundedSender<Request>,
    pub projection: watch::Receiver<Projection>,
    events: broadcast::Sender<ChatStreamEvent>,
    chat_commands: mpsc::UnboundedSender<CreationRequest>,
}
impl AutomationService {
    pub fn shared() -> Arc<Self> {
        static SERVICE: OnceLock<Arc<AutomationService>> = OnceLock::new();
        SERVICE
            .get_or_init(|| {
                let service =
                    Self::start(threadlane_project::global_threadlane_dir().join("automations"));
                install_creator(service.chat_commands.clone());
                service
            })
            .clone()
    }
    fn start(root: PathBuf) -> Arc<Self> {
        let (commands, receiver) = mpsc::unbounded_channel();
        let (chat_commands, chat_receiver) = mpsc::unbounded_channel();
        let (projection_tx, projection) = watch::channel(Projection::default());
        let (events, _) = broadcast::channel(2048);
        let service = Arc::new(Self {
            commands,
            projection,
            events: events.clone(),
            chat_commands,
        });
        threadlane_provider::exec::get_runtime().spawn(async move {
            let store = Store::open(&root);
            match store {
                Ok(store) => {
                    Actor::new(store, projection_tx, events)
                        .run(receiver, chat_receiver)
                        .await
                }
                Err(error) => {
                    projection_tx.send_replace(Projection {
                        error: Some(error),
                        ..Default::default()
                    });
                }
            }
        });
        service
    }
    pub fn subscribe(&self) -> broadcast::Receiver<ChatStreamEvent> {
        self.events.subscribe()
    }
    pub async fn command(&self, command: Command) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.commands.send((command, tx)).map_err(|_| {
            "Automation service is unavailable; reopen Threadlane after resolving the storage error"
        })?;
        rx.await.map_err(|_| "Automation service stopped")?
    }
    pub fn resolved(&self, session_id: String, request_id: String) {
        let (tx, _) = oneshot::channel();
        let _ = self.commands.send((
            Command::Resolved {
                session_id,
                request_id,
            },
            tx,
        ));
    }
}
enum Event {
    Prepared(String, Result<PreparedRun, String>),
    Agent(String, AgentEvent),
    Output(String, String),
    Finished(String),
}
struct Active {
    run: Run,
    runtime: Option<Arc<SessionController>>,
    elapsed: Duration,
    cancellation: Option<(RunStatus, String)>,
    completion: Option<(RunStatus, Option<String>)>,
    error: Option<String>,
}
struct Actor {
    store: Store,
    projection: Projection,
    updates: watch::Sender<Projection>,
    events: broadcast::Sender<ChatStreamEvent>,
    tx: mpsc::UnboundedSender<Event>,
    rx: mpsc::UnboundedReceiver<Event>,
    active: Option<Active>,
}
impl Actor {
    fn new(
        store: Store,
        updates: watch::Sender<Projection>,
        events: broadcast::Sender<ChatStreamEvent>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            store,
            updates,
            events,
            tx,
            rx,
            active: None,
            projection: Projection::default(),
        }
    }
    fn publish(&mut self) {
        let current = self.updates.borrow();
        let runtime = self.active.as_ref().and_then(|a| a.runtime.clone());
        if current.snapshot.revision == self.store.snapshot().revision
            && current.error == self.projection.error
            && current.active_runtime.as_ref().map(|r| r.session_file())
                == runtime.as_ref().map(|r| r.session_file())
        {
            return;
        }
        drop(current);
        self.projection.snapshot = self.store.snapshot().clone();
        self.projection.active_runtime = self.active.as_ref().and_then(|a| a.runtime.clone());
        self.updates.send_replace(self.projection.clone());
    }
    fn recover(&mut self) -> Result<(), String> {
        let runs = self.store.snapshot().runs.clone();
        for run in runs
            .into_iter()
            .filter(|r| r.status.active() && r.status != RunStatus::Queued)
        {
            let outcome = run
                .session_file
                .as_ref()
                .and_then(|p| JsonlStore::open_read_only(p).ok())
                .and_then(|s| s.facts().get("automation_outcome").cloned());
            let status = match outcome.as_deref() {
                Some("succeeded") => RunStatus::Succeeded,
                Some("failed") => RunStatus::Failed,
                Some("cancelled") => RunStatus::Cancelled,
                Some("interrupted") => RunStatus::Interrupted,
                _ => run
                    .session_file
                    .as_ref()
                    .and_then(|p| durable_status(p))
                    .unwrap_or(RunStatus::Interrupted),
            };
            self.store.update_run(
                &run.id,
                status,
                (status != RunStatus::Succeeded).then(||
                    "Recovered after Threadlane stopped. Review the chat before running again."
                        .into()
                ),
                None,
                now(),
            )?;
        }
        Ok(())
    }
    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Request>,
        mut chat_commands: mpsc::UnboundedReceiver<CreationRequest>,
    ) {
        if let Err(error) = self.recover() {
            self.projection.error = Some(error);
            self.publish();
            return;
        }
        self.publish();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last = Instant::now();
        loop {
            let result = tokio::select! {
                Some(request) = chat_commands.recv() => {
                    let result = self.create_from_chat(request.definition);
                    let error = result.as_ref().err().cloned();
                    let _ = request.reply.send(result);
                    error.map_or(Ok(()), Err)
                }
                Some((command, reply)) = commands.recv() => {
                    let result = self.command(command);
                    let _ = reply.send(result.clone());
                    result
                }
                Some(event) = self.rx.recv() => self.event(event),
                _ = tick.tick() => {
                    let elapsed = last.elapsed(); last = Instant::now();
                    if self.projection.permissions.is_empty() && self.projection.questions.is_empty() {
                        if let Some(active) = &mut self.active {
                            active.elapsed += elapsed;
                            if active.elapsed >= Duration::from_secs(3600) && active.cancellation.is_none() {
                                active.cancellation = Some((RunStatus::Failed, "Execution exceeded one hour".into()));
                                if let Some(runtime) = &active.runtime { let _ = runtime.cancel(); }
                            }
                        }
                    }
                    self.schedule()
                }
            };
            if let Err(error) = result {
                self.projection.error = Some(error);
            }
            self.publish();
        }
    }
    fn command(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::Save(d) => {
                if !threadlane_project::load_project_registry()
                    .iter()
                    .any(|p| p.path == d.project)
                {
                    return Err("Choose an attached project".into());
                }
                self.store.save(d, now())?;
            }
            Command::SetEnabled(id, enabled) => self.store.set_enabled(&id, enabled, now())?,
            Command::Delete(id) => self.store.delete(&id)?,
            Command::RunNow(id) => {
                self.store.enqueue(&id, false, now())?;
            }
            Command::Review(id) => self.store.mark_reviewed(&id)?,
            Command::Cancel(id) => {
                if let Some(active) = self.active.as_mut().filter(|a| a.run.id == id) {
                    active.cancellation = Some((RunStatus::Cancelled, "Cancelled by user".into()));
                    if let Some(runtime) = &active.runtime {
                        runtime.cancel()?;
                    }
                } else {
                    self.store
                        .update_run(&id, RunStatus::Cancelled, None, None, now())?;
                }
            }
            Command::Resolved {
                session_id,
                request_id,
            } => {
                if self
                    .active
                    .as_ref()
                    .is_some_and(|a| a.run.session_id == session_id)
                {
                    if self
                        .projection
                        .permissions
                        .get(&session_id)
                        .is_some_and(|r| r.id == request_id)
                    {
                        self.projection.permissions.remove(&session_id);
                    }
                    if self
                        .projection
                        .questions
                        .get(&session_id)
                        .is_some_and(|r| r.id == request_id)
                    {
                        self.projection.questions.remove(&session_id);
                    }
                    if self.projection.permissions.is_empty()
                        && self.projection.questions.is_empty()
                    {
                        let id = self.active.as_ref().unwrap().run.id.clone();
                        self.store
                            .update_run(&id, RunStatus::Running, None, None, now())?;
                    }
                }
            }
        }
        self.projection.error = None;
        self.schedule()
    }
    fn create_from_chat(&mut self, definition: Definition) -> Result<Definition, String> {
        if !threadlane_project::load_project_registry()
            .iter()
            .any(|p| p.path == definition.project)
        {
            return Err("Choose an attached project".into());
        }
        // A missing picker entry is not authoritative while live discovery is cold.
        // Definition validation still rejects empty IDs and unsupported ACP models.
        let saved = self.store.create_once(definition, now())?;
        self.projection.error = None;
        Ok(saved)
    }
    fn schedule(&mut self) -> Result<(), String> {
        if let Some((id, (status, error))) = self
            .active
            .as_ref()
            .and_then(|a| a.completion.clone().map(|c| (a.run.id.clone(), c)))
        {
            self.finish(&id, status, error)?;
        }
        let at = now();
        let due: Vec<_> = self
            .store
            .snapshot()
            .definitions
            .iter()
            .filter(|d| {
                d.enabled
                    && d.next_at.is_some_and(|t| t <= at)
                    && !self
                        .store
                        .snapshot()
                        .runs
                        .iter()
                        .any(|r| r.definition.id == d.id && r.status.active())
            })
            .map(|d| d.id.clone())
            .collect();
        for id in due {
            self.store.enqueue(&id, true, at)?;
        }
        if self.active.is_none() {
            if let Some(run) = self
                .store
                .snapshot()
                .runs
                .iter()
                .find(|r| r.status == RunStatus::Queued)
                .cloned()
            {
                let stub = run
                    .definition
                    .project
                    .join(".threadlane/sessions")
                    .join(format!("{}.jsonl", run.session_id));
                self.store
                    .update_run(&run.id, RunStatus::Starting, None, Some(stub), at)?;
                self.active = Some(Active {
                    run: run.clone(),
                    runtime: None,
                    elapsed: Duration::ZERO,
                    cancellation: None,
                    completion: None,
                    error: None,
                });
                let tx = self.tx.clone();
                threadlane_provider::exec::get_runtime().spawn(async move {
                    let id = run.id.clone();
                    let model = run.definition.model.clone();
                    // Picker caches may be empty at startup or after discovery failures.
                    // Preserve the saved model; its provider is authoritative at execution.
                    let result = prepare(run).await;
                    if result.is_ok() {
                        // Await discovery before the first provider turn (also populates live
                        // runtime mappings). A failed/empty refresh is not proof of removal.
                        if model.starts_with("opencode-go/") {
                            threadlane_ui_catalog::refresh_discovered_models().await;
                        } else if model.starts_with("antigravity/") {
                            threadlane_ui_catalog::refresh_antigravity_models().await;
                        } else {
                            threadlane_ui_catalog::refresh_openai_models().await;
                        }
                    }
                    let _ = tx.send(Event::Prepared(id, result));
                });
            }
        }
        Ok(())
    }
    fn event(&mut self, event: Event) -> Result<(), String> {
        match event {
            Event::Prepared(id, result) => {
                let prepared = match result {
                    Ok(p) => p,
                    Err(error) => {
                        let (status, reason) = self
                            .active
                            .as_ref()
                            .filter(|a| a.run.id == id)
                            .and_then(|a| a.cancellation.clone())
                            .unwrap_or((RunStatus::Failed, error));
                        return self.finish(&id, status, Some(reason));
                    }
                };
                let active = self
                    .active
                    .as_mut()
                    .filter(|a| a.run.id == id)
                    .ok_or("Stale automation preparation")?;
                active.runtime = Some(prepared.runtime.clone());
                if let Err(error) = self.store.update_run(
                    &id,
                    RunStatus::Running,
                    None,
                    Some(prepared.runtime.session_file().into()),
                    now(),
                ) {
                    active.completion = Some((RunStatus::Failed, Some(error.clone())));
                    return Err(error);
                }
                if let Some((status, reason)) = active.cancellation.clone() {
                    return self.finish(&id, status, Some(reason));
                }
                let tx = self.tx.clone();
                let tx_output = tx.clone();
                let tx_done = tx.clone();
                let agent_id = id.clone();
                let output_id = id.clone();
                let done_id = id.clone();
                let result = prepared.runtime.spawn_interactive_turn(
                    active.run.definition.prompt.clone(),
                    vec![],
                    prepared.effort,
                    prepared.work_dir,
                    vec![],
                    move |e| {
                        let _ = tx.send(Event::Agent(agent_id.clone(), e));
                    },
                    move |text| {
                        let _ = tx_output.send(Event::Output(output_id.clone(), text));
                    },
                    |_, _, _| {},
                    move || {
                        let _ = tx_done.send(Event::Finished(done_id.clone()));
                    },
                );
                if let Err(error) = result {
                    return self.finish(&id, RunStatus::Failed, Some(error));
                }
            }
            Event::Agent(id, event) => {
                let Some(active) = self.active.as_mut().filter(|a| a.run.id == id) else {
                    return Ok(());
                };
                let session_id = active.run.session_id.clone();
                let waiting = match &event {
                    AgentEvent::PermissionRequested { request } => {
                        self.projection
                            .permissions
                            .insert(session_id.clone(), request.clone());
                        Some(RunStatus::WaitingPermission)
                    }
                    AgentEvent::QuestionRequested { request } => {
                        self.projection
                            .questions
                            .insert(session_id.clone(), request.clone());
                        Some(RunStatus::WaitingAnswer)
                    }
                    AgentEvent::AgentError { error } => {
                        active.error = Some(error.clone());
                        None
                    }
                    _ => None,
                };
                if let Some(status) = waiting {
                    self.store.update_run(&id, status, None, None, now())?;
                    self.projection.notification = Some((
                        format!("{id}-{:?}", status),
                        format!("{}: {}", active.run.definition.name, status.label()),
                    ));
                }
                let _ = self
                    .events
                    .send(ChatStreamEvent::Agent { session_id, event });
            }
            Event::Output(id, text) => {
                if let Some(active) = self.active.as_ref().filter(|a| a.run.id == id) {
                    let _ = self.events.send(ChatStreamEvent::Agent {
                        session_id: active.run.session_id.clone(),
                        event: AgentEvent::MessageUpdate {
                            text_delta: Some(text),
                            reasoning_delta: None,
                            tool_call_name: None,
                        },
                    });
                }
            }
            Event::Finished(id) => {
                let Some(active) = self.active.as_ref().filter(|a| a.run.id == id) else {
                    return Ok(());
                };
                let runtime = active.runtime.as_ref().unwrap();
                let error = active.error.clone().or_else(|| match runtime.status() {
                    SessionStatus::Error(e) => Some(e),
                    _ => None,
                });
                let durable = durable_status(runtime.session_file());
                let status = if let Some((status, _)) = &active.cancellation {
                    *status
                } else if durable == Some(RunStatus::Cancelled) {
                    RunStatus::Cancelled
                } else if error.is_some() {
                    RunStatus::Failed
                } else {
                    durable.unwrap_or(RunStatus::Interrupted)
                };
                let error = active
                    .cancellation
                    .as_ref()
                    .map(|(_, reason)| reason.clone())
                    .or(error);
                self.finish(&id, status, error)?;
            }
        }
        Ok(())
    }
    fn finish(&mut self, id: &str, status: RunStatus, error: Option<String>) -> Result<(), String> {
        let Some(active) = self.active.as_mut().filter(|a| a.run.id == id) else {
            return Ok(());
        };
        active.completion = Some((status, error.clone()));
        let mut error = error;
        if let Some(runtime) = &active.runtime {
            if let Err(write_error) = record_outcome(
                runtime,
                match status {
                    RunStatus::Succeeded => "succeeded",
                    RunStatus::Cancelled => "cancelled",
                    RunStatus::Interrupted => "interrupted",
                    _ => "failed",
                },
            ) {
                let message = format!("Could not record outcome: {write_error}");
                error = Some(match error {
                    Some(error) => format!("{error}; {message}"),
                    None => message,
                });
            }
        }
        self.store.update_run(id, status, error, None, now())?;
        if let Some(runtime) = &active.runtime {
            let _ = self.events.send(ChatStreamEvent::Finished {
                session_id: active.run.session_id.clone(),
                session_file: runtime.session_file().into(),
            });
        }
        if active.run.definition.notify_all
            || matches!(status, RunStatus::Failed | RunStatus::Interrupted)
        {
            self.projection.notification = Some((
                format!("{id}-finished"),
                format!("{}: {}", active.run.definition.name, status.label()),
            ));
        }
        self.projection.permissions.clear();
        self.projection.questions.clear();
        self.active = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_automation::Schedule;
    use threadlane_protocol::{
        DeferredResponse, ProviderPort, RuntimeRequest, RuntimeStreamEvent, RuntimeUsage,
    };

    #[derive(Default)]
    struct Provider(std::sync::atomic::AtomicBool);
    #[async_trait::async_trait]
    impl ProviderPort for Provider {
        async fn stream_request(
            &self,
            request: RuntimeRequest,
            events: mpsc::Sender<RuntimeStreamEvent>,
        ) {
            assert!(format!("{:?}", request.messages).contains("automation test prompt"));
            if !self.0.swap(true, std::sync::atomic::Ordering::SeqCst) {
                events.send(RuntimeStreamEvent::Finished { tool_calls: vec![threadlane_protocol::RuntimeToolCall {
                    id: "automation-question".into(), r#type: "function".into(),
                    function: threadlane_protocol::RuntimeToolCallFunction { name: "ask_question".into(), arguments: r#"{"questions":[{"question":"Continue?","options":["Yes","No"]}]}"#.into() }, thought_signature: None,
                }], usage: RuntimeUsage::default() }).await.unwrap();
                return;
            }
            events
                .send(RuntimeStreamEvent::ContentToken("Automation result".into()))
                .await
                .unwrap();
            events
                .send(RuntimeStreamEvent::Finished {
                    tool_calls: vec![],
                    usage: RuntimeUsage::default(),
                })
                .await
                .unwrap();
        }
        async fn fetch_deferred(&self, _: &str, _: &str) -> Result<DeferredResponse, String> {
            Ok(DeferredResponse::Pending)
        }
        async fn cancel_deferred(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn provider_kind(&self, _: &str) -> &'static str {
            "test"
        }
    }
    fn definition(root: &std::path::Path) -> Definition {
        Definition {
            id: "automation-test".into(),
            revision: 0,
            name: "Test".into(),
            prompt: "automation test prompt".into(),
            project: root.into(),
            model: "gpt-4o".into(),
            effort: "medium".into(),
            worktree: false,
            schedule: Schedule::Manual,
            enabled: false,
            notify_all: false,
            anchor: 0,
            next_at: None,
            failures: 0,
            paused_reason: None,
        }
    }
    fn make_actor(root: &std::path::Path) -> Actor {
        let store = Store::open(root).unwrap();
        let (updates, _) = watch::channel(Projection::default());
        let (events, _) = broadcast::channel(32);
        Actor::new(store, updates, events)
    }
    #[tokio::test]
    async fn automation_uses_controller_and_durable_transcript_then_recovers() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let mut actor = make_actor(&root.join("automations"));
        actor.store.save(definition(&root), now()).unwrap();
        let id = actor
            .store
            .enqueue("automation-test", false, now())
            .unwrap();
        let run = actor.store.snapshot().runs[0].clone();
        let session = root
            .join(".threadlane/sessions")
            .join(format!("{}.jsonl", run.session_id));
        let options = crate::projection::coding_agent_options(
            root.clone(),
            session.clone(),
            "gpt-4o".into(),
            Default::default(),
            Default::default(),
        );
        let runtime = tokio::task::spawn_blocking(move || {
            threadlane_coding_agent::controller::test_support::session_controller_with_provider(
                options,
                Arc::new(Provider::default()),
            )
        })
        .await
        .unwrap();
        actor.active = Some(Active {
            run,
            runtime: None,
            elapsed: Duration::ZERO,
            cancellation: None,
            completion: None,
            error: None,
        });
        actor
            .event(Event::Prepared(
                id.clone(),
                Ok(PreparedRun {
                    runtime,
                    work_dir: root.clone(),
                    effort: Default::default(),
                }),
            ))
            .unwrap();
        let mut answered = false;
        tokio::time::timeout(Duration::from_secs(10), async {
            while actor.active.is_some() {
                let event = actor.rx.recv().await.unwrap();
                actor.event(event).unwrap();
                if let Some((session_id, question)) = actor
                    .projection
                    .questions
                    .iter()
                    .next()
                    .map(|(s, q)| (s.clone(), q.clone()))
                {
                    assert_eq!(
                        actor.store.snapshot().runs[0].status,
                        RunStatus::WaitingAnswer
                    );
                    let runtime = actor.active.as_ref().unwrap().runtime.as_ref().unwrap();
                    assert!(runtime.is_generating());
                    let answer = threadlane_protocol::QuestionAnswer {
                        request_id: question.id.clone(),
                        dismissed: false,
                        answers: vec![threadlane_protocol::QuestionItemAnswer {
                            question_id: question.questions[0].id.clone(),
                            selected: vec!["Yes".into()],
                            custom_text: None,
                        }],
                    };
                    assert!(runtime.resolve_question(&question.id, answer));
                    actor
                        .command(Command::Resolved {
                            session_id,
                            request_id: question.id,
                        })
                        .unwrap();
                    answered = true;
                }
            }
        })
        .await
        .unwrap();
        assert!(
            answered,
            "the real ask_question tool must wait for an explicit answer"
        );
        assert_eq!(actor.store.snapshot().runs[0].status, RunStatus::Succeeded);
        let store = JsonlStore::open_read_only(&session).unwrap();
        assert!(store
            .entries()
            .iter()
            .any(|e| format!("{:?}", e.message).contains("automation test prompt")));
        assert_eq!(
            store.facts().get("automation_outcome").map(String::as_str),
            Some("succeeded")
        );
        drop(actor);
        let mut recovered = make_actor(&root.join("automations"));
        recovered.recover().unwrap();
        assert_eq!(recovered.store.snapshot().runs.len(), 1);
        assert_eq!(
            recovered.store.snapshot().runs[0].status,
            RunStatus::Succeeded
        );
    }

    #[test]
    fn recovery_interrupts_uncertain_dispatch_but_keeps_queued_work() {
        let temp = tempfile::tempdir().unwrap();
        let mut service = make_actor(temp.path());
        service.store.save(definition(temp.path()), 0).unwrap();
        let id = service.store.enqueue("automation-test", false, 1).unwrap();
        service
            .store
            .update_run(&id, RunStatus::Starting, None, None, 1)
            .unwrap();
        service.recover().unwrap();
        assert_eq!(
            service.store.snapshot().runs[0].status,
            RunStatus::Interrupted
        );
        service.store.enqueue("automation-test", false, 2).unwrap();
        service.recover().unwrap();
        assert_eq!(service.store.snapshot().runs[1].status, RunStatus::Queued);
    }

    #[tokio::test]
    async fn outcome_write_failure_releases_dispatch_and_successful_recovery_has_no_error() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let mut actor = make_actor(&root.join("automations"));
        actor.store.save(definition(&root), 0).unwrap();
        let id = actor.store.enqueue("automation-test", false, 1).unwrap();
        let session = root.join("session.jsonl");
        let options = crate::projection::coding_agent_options(
            root.clone(), session.clone(), "gpt-4o".into(),
            Default::default(), Default::default(),
        );
        let runtime = tokio::task::spawn_blocking(move || {
            threadlane_coding_agent::controller::test_support::session_controller_with_provider(
                options, Arc::new(Provider::default()),
            )
        }).await.unwrap();
        actor.store.update_run(&id, RunStatus::Running, None, Some(session.clone()), 1).unwrap();
        record_outcome(&runtime, "succeeded").unwrap();
        actor.recover().unwrap();
        assert_eq!(actor.store.snapshot().runs[0].status, RunStatus::Succeeded);
        assert!(actor.store.snapshot().runs[0].error.is_none());

        let id = actor.store.enqueue("automation-test", false, 2).unwrap();
        actor.active = Some(Active {
            run: actor.store.snapshot().runs[1].clone(),
            runtime: Some(runtime), elapsed: Duration::ZERO,
            cancellation: None, completion: None, error: None,
        });
        // A broken transcript must not prevent the independent automation store from settling.
        std::fs::rename(&session, root.join("saved-session.jsonl")).unwrap();
        std::fs::create_dir(&session).unwrap();
        actor.finish(&id, RunStatus::Failed, Some("Provider failed".into())).unwrap();
        assert!(actor.active.is_none());
        let error = actor.store.snapshot().runs[1].error.as_deref().unwrap();
        assert!(error.starts_with("Provider failed; Could not record outcome:"));
        actor.store.enqueue("automation-test", false, 3).unwrap();
        actor.schedule().unwrap();
        assert!(actor.active.is_some());
    }

    #[tokio::test]
    async fn cold_picker_cache_does_not_reject_a_saved_live_only_model() {
        let temp = tempfile::tempdir().unwrap();
        let mut actor = make_actor(&temp.path().join("automations"));
        let mut definition = definition(temp.path());
        definition.model = "opencode-go/live-only-regression-model".into();
        definition.enabled = true;
        definition.schedule = Schedule::Interval { minutes: 1 };
        actor.store.save(definition, now() - 120).unwrap();
        actor.schedule().unwrap();
        let event = tokio::time::timeout(Duration::from_secs(10), actor.rx.recv())
            .await
            .unwrap()
            .unwrap();
        // This unattached temporary project stops preparation before credentials/provider use.
        // Reaching that check proves cache absence did not fail the occurrence first.
        let Event::Prepared(_, Err(error)) = event else {
            panic!("expected project validation")
        };
        assert_eq!(error, "Attach this automation's project before running it");
        assert_eq!(actor.store.snapshot().runs[0].status, RunStatus::Starting);
        assert_eq!(actor.store.snapshot().definitions[0].failures, 0);
    }
    #[test]
    fn automation_navigation_preserves_chat_and_project_scope() {
        let mut state = crate::AppState::load_from_registry(vec![]);
        state.active_session_id = Some("original".into());
        state.active_work_dir = Some(PathBuf::from("/project"));
        state.sidebar_project_filter = Some(PathBuf::from("/filter"));
        crate::controller::dispatch(&mut state, crate::actions::AppAction::OpenAutomations);
        assert_eq!(state.workspace_page, crate::WorkspacePage::Automations);
        assert_eq!(state.active_session_id.as_deref(), Some("original"));
        assert_eq!(state.active_work_dir, Some(PathBuf::from("/project")));
        assert_eq!(state.sidebar_project_filter, Some(PathBuf::from("/filter")));
    }

    #[test]
    fn completed_run_refresh_preserves_questions_from_a_later_chat_turn() {
        let temp = tempfile::tempdir().unwrap();
        let mut actor = make_actor(temp.path());
        actor.store.save(definition(temp.path()), 0).unwrap();
        let id = actor.store.enqueue("automation-test", false, 1).unwrap();
        actor
            .store
            .update_run(&id, RunStatus::Succeeded, None, None, 2)
            .unwrap();
        let session_id = actor.store.snapshot().runs[0].session_id.clone();
        let mut state = crate::AppState::load_from_registry(vec![]);
        state.pending_questions.insert(
            session_id.clone(),
            QuestionRequest {
                id: "later-turn-question".into(),
                questions: vec![],
            },
        );
        state.apply_automation_projection(Projection {
            snapshot: actor.store.snapshot().clone(),
            ..Default::default()
        });
        assert_eq!(
            state.pending_questions[&session_id].id,
            "later-turn-question"
        );
    }
}
