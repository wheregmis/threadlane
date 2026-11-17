//! Daemon-owned session core.
//!
//! [`DaemonCore`] is the in-process owner of everything a standalone
//! `threadlane-daemon` process would hold: the live `SessionRuntime` map
//! (keyed by session file, addressed by `session_id`), pending worktree
//! setups, session/model configuration, and the `SessionEvent` journal that
//! fans out to every attached client. `AppState` embeds it today through
//! `LocalDaemon`; the daemon binary wraps it behind the WebSocket transport
//! that `RemoteDaemon` speaks — the same `dispatch(SessionCommand)` entry
//! point serves both.
//!
//! Event flow: producers (chat turns, ACP tasks, worktree setup, hydration)
//! send into the ingest channel; a pump on the shared Tokio reactor appends
//! each event to a bounded journal and broadcasts it to subscribers. A
//! reconnecting client replays the journal tail and then tails live events —
//! the attach-mid-run semantics the wire contract promises.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::{broadcast, mpsc};

use threadlane_coding_agent::controller::SessionRuntime;
use threadlane_protocol::browser::BrowserBridge;
use threadlane_protocol::daemon::{
    CommandResponse, ProjectInfo, SessionCommand, SessionEvent, SessionHydrationRequest,
    SessionInfo, SessionSnapshot, TerminalEvent, WorktreeSetup,
};
use threadlane_protocol::editor_lsp::{
    EditorLspOperation, EditorLspRequest, EditorLspResponse, MAX_EDITOR_LSP_BYTES,
};
use threadlane_protocol::orchestration::ModelRoles;
use threadlane_protocol::ReasoningEffort;
use threadlane_runtime::harness::SessionStore;

use crate::discovery::canonical_session_file;
use crate::projection::{
    compute_full_session_projection, compute_session_messages_for_hydration, coding_agent_options,
};

/// How many recent events a reconnecting client can replay. Sized to a long
/// busy session: tail events are deltas, so a full snapshot is fetched via
/// `GetSessionSnapshot` for anything older.
const JOURNAL_CAPACITY: usize = 4096;
/// Broadcast lag headroom per subscriber before `Lagged` drops events.
const BROADCAST_CAPACITY: usize = 1024;

fn parse_editor_server_document_version(
    response: &serde_json::Value,
    is_close: bool,
) -> Result<Option<i32>, String> {
    match response.get("server_document_version") {
        Some(serde_json::Value::Null) if is_close => Ok(None),
        Some(serde_json::Value::Number(version)) if !is_close => {
            let version = version
                .as_i64()
                .ok_or("Editor LSP server document version must be an integer")?;
            i32::try_from(version)
                .map(Some)
                .map_err(|_| "Editor LSP server document version is out of range".into())
        }
        Some(serde_json::Value::Null) => {
            Err("Editor LSP response omitted its server document version".into())
        }
        Some(serde_json::Value::Number(_)) => {
            Err("Editor LSP close response must not include a server document version".into())
        }
        _ => Err("Editor LSP response has an invalid server document version".into()),
    }
}

/// Where a session's runtime was built to execute. `work_dir` is the
/// effective execution directory (the worktree for worktree sessions).
#[derive(Clone, Debug)]
pub struct SessionIdentity {
    pub session_file: PathBuf,
    pub work_dir: PathBuf,
}

/// The session-owning half of the daemon, transport-agnostic.
pub struct DaemonCore {
    /// Live runtimes keyed by session file (the canonical identity path).
    runtimes: Mutex<HashMap<PathBuf, Arc<SessionRuntime>>>,
    /// Per-session gates serialize construction without coupling unrelated sessions.
    runtime_construction: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
    /// `session_id` → file/work_dir, populated at registration and hydration.
    identities: Mutex<HashMap<String, SessionIdentity>>,
    /// Work dirs the host attached, fed by AppState/`AddProject` so remote
    /// clients can enumerate projects without journal archaeology.
    attached_projects: Mutex<BTreeSet<PathBuf>>,
    /// In-flight worktree preparations, for `CancelWorktreeSetup`.
    worktree_setups: Mutex<HashMap<String, WorktreeSetup>>,
    /// Daemon-hosted PTYs, addressed by client-chosen terminal ids.
    terminals: crate::terminal::TerminalManager,
    /// Refcounted filesystem watchers for `WatchProject`/`UnwatchProject`,
    /// feeding the ephemeral `WorkspaceChanged` stream.
    project_watchers: crate::project_io::ProjectWatchers,
    /// Events producers write into; pumped onto the journal + broadcast.
    ingest_tx: mpsc::UnboundedSender<SessionEvent>,
    /// `(seq, event)` pairs: the pump assigns a monotonic journal sequence
    /// so transports can offer cursor-based replay (`?since=` on attach).
    broadcast_tx: broadcast::Sender<(u64, SessionEvent)>,
    journal: Arc<Mutex<VecDeque<(u64, SessionEvent)>>>,
    event_seq: Arc<AtomicU64>,
    /// Host-provided browser bridge resolved at runtime construction — the
    /// desktop embeds a live bridge; a standalone daemon serves
    /// `BrowserBridge::unavailable()`.
    browser_bridge: RwLock<BrowserBridge>,
    /// Model selection shared by new/lazy runtime constructions, seeded by
    /// the host and kept current by `SetModel`.
    model: RwLock<String>,
    model_roles: RwLock<ModelRoles>,
    effort: RwLock<ReasoningEffort>,
}

impl DaemonCore {
    /// Start the core and its event pump on the shared Tokio reactor.
    pub fn new() -> Result<Arc<Self>, String> {
        let (ingest_tx, mut ingest_rx) = mpsc::unbounded_channel::<SessionEvent>();
        let (broadcast_tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let journal: Arc<Mutex<VecDeque<(u64, SessionEvent)>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let event_seq: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        {
            let broadcast_tx = broadcast_tx.clone();
            let journal = journal.clone();
            let event_seq = event_seq.clone();
            crate::chat::executor()?.spawn(async move {
                while let Some(event) = ingest_rx.recv().await {
                    let seq = event_seq.fetch_add(1, Ordering::SeqCst) + 1;
                    // Terminal output is high-volume and per-spawn ephemeral:
                    // journaled frames would evict session history during
                    // floods and replay garbage into live emulators on
                    // reconnect. Lifecycle frames (Exited/Resized) stay
                    // journaled — they are rare and reconcile client state.
                    let journalable = !matches!(
                        &event,
                        SessionEvent::TerminalEvent {
                            event: TerminalEvent::Output { .. }
                        } | SessionEvent::WorkspaceChanged { .. }
                    );
                    let mut journal = journal.lock().expect("daemon journal poisoned");
                    if journalable {
                        journal.push_back((seq, event.clone()));
                        while journal.len() > JOURNAL_CAPACITY {
                            journal.pop_front();
                        }
                    }
                    drop(journal);
                    // Slow subscribers drop via Lagged rather than blocking
                    // the whole daemon on one client's backlog.
                    let _ = broadcast_tx.send((seq, event));
                }
            });
        }
        let core = Arc::new(Self {
            runtimes: Mutex::new(HashMap::new()),
            runtime_construction: Mutex::new(HashMap::new()),
            identities: Mutex::new(HashMap::new()),
            attached_projects: Mutex::new(BTreeSet::new()),
            worktree_setups: Mutex::new(HashMap::new()),
            terminals: crate::terminal::TerminalManager::default(),
            project_watchers: crate::project_io::ProjectWatchers::default(),
            ingest_tx,
            broadcast_tx,
            journal,
            event_seq,
            browser_bridge: RwLock::new(BrowserBridge::unavailable()),
            model: RwLock::new(String::new()),
            model_roles: RwLock::new(ModelRoles::default()),
            effort: RwLock::new(ReasoningEffort::default()),
        });
        core.start_automation_bridge();
        Ok(core)
    }

    /// Bridge the process-local automation service into this core: the
    /// service's agent-event feed joins the shared broadcast (so clients
    /// attached through the transport see run output), its projection watch
    /// channel becomes journaled `AutomationChanged` events, and a live
    /// run's runtime registers here so `Answer*`/`CancelRun` commands
    /// resolve against it. This is the wiring the desktop host applied
    /// per-client; doing it once at the core serves local and remote
    /// clients alike.
    fn start_automation_bridge(self: &Arc<Self>) {
        let service = crate::automation::AutomationService::shared();
        let mut events = service.subscribe();
        let this = Arc::downgrade(self);
        if let Ok(executor) = crate::chat::executor() {
            executor.spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            let Some(this) = this.upgrade() else {
                                break;
                            };
                            if this.ingest_tx.send(event).is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });
        }
        let mut projections = service.projection.clone();
        // The process-wide service must not keep a closed host's runtimes
        // (and their extension ownership leases) alive while awaiting updates.
        let this = Arc::downgrade(self);
        if let Ok(executor) = crate::chat::executor() {
            executor.spawn(async move {
                loop {
                    if projections.changed().await.is_err() {
                        break;
                    }
                    let Some(this) = this.upgrade() else {
                        break;
                    };
                    let projection = projections.borrow_and_update().clone();
                    if let Some(runtime) = &projection.active_runtime {
                        let session_file = runtime.session_file().to_path_buf();
                        if this.runtime_for_file(&session_file).is_none() {
                            // Canonical layout nests session files as
                            // `<project>/.threadlane/sessions/<id>.jsonl`,
                            // so ancestor(3) is the project root — or the
                            // linked worktree for worktree runs, which
                            // `project_dir_for` maps back to its project.
                            let work_dir = session_file
                                .ancestors()
                                .nth(3)
                                .map(|dir| Self::project_dir_for(dir))
                                .unwrap_or_default();
                            let session_id = Self::session_id_for_file(&session_file)
                                .unwrap_or_default();
                            this.register_runtime(
                                &session_id,
                                work_dir,
                                session_file,
                                runtime.clone(),
                            );
                        }
                    }
                    let wire = Self::automation_projection_wire(&projection);
                    if this
                        .ingest_tx
                        .send(SessionEvent::AutomationChanged { projection: wire })
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
    }

    /// The service's process-local projection as it crosses the wire:
    /// `active_runtime` (an in-process handle) becomes the run's session id.
    /// Also used by local UI hosts to share client-side request reconciliation.
    pub fn automation_projection_wire(
        projection: &crate::automation::Projection,
    ) -> threadlane_protocol::automation::AutomationProjection {
        threadlane_protocol::automation::AutomationProjection {
            snapshot: projection.snapshot.clone(),
            permissions: projection.permissions.clone(),
            questions: projection.questions.clone(),
            question_queues: Some(projection.question_queues.clone()),
            active_session_id: projection
                .active_runtime
                .as_ref()
                .and_then(|runtime| Self::session_id_for_file(runtime.session_file())),
            error: projection.error.clone(),
        }
    }

    /// The host (desktop shell, or the standalone binary's own browser
    /// surface once one exists) installs the bridge every lazily-built
    /// runtime shares.
    pub fn set_browser_bridge(&self, bridge: BrowserBridge) {
        *self.browser_bridge.write().expect("browser bridge poisoned") = bridge;
    }

    /// Seed the model/effort/roles a host already selected (AppState embeds
    /// the core after restoring its own persisted selection).
    pub fn seed_config(&self, model: String, roles: ModelRoles, effort: ReasoningEffort) {
        *self.model.write().expect("model poisoned") = model;
        *self.model_roles.write().expect("model roles poisoned") = roles;
        *self.effort.write().expect("effort poisoned") = effort;
    }

    /// The host records an attached project; thin clients enumerate these
    /// via `SessionCommand::GetProjects`.
    pub fn attach_project(&self, work_dir: PathBuf) {
        self.attached_projects
            .lock()
            .expect("attached_projects poisoned")
            .insert(work_dir);
    }

    /// The host records a detached project.
    pub fn detach_project(&self, work_dir: &Path) {
        self.attached_projects
            .lock()
            .expect("attached_projects poisoned")
            .remove(work_dir);
    }

    fn attached_project_dirs(&self) -> Vec<PathBuf> {
        self.attached_projects
            .lock()
            .expect("attached_projects poisoned")
            .iter()
            .cloned()
            .collect()
    }

    /// Channel producers write `SessionEvent`s into (turns, ACP tasks,
    /// worktree setup). The pump owns every subscriber downstream.
    pub fn event_sender(&self) -> mpsc::UnboundedSender<SessionEvent> {
        self.ingest_tx.clone()
    }

    /// A live event subscription (journal replay is the caller's choice —
    /// [`Self::subscribe_with_tail`] bundles both for attach flows).
    pub fn subscribe(&self) -> broadcast::Receiver<(u64, SessionEvent)> {
        self.broadcast_tx.subscribe()
    }

    /// Attach semantics: replay the bounded journal tail newer than `since`
    /// (a client's last-seen journal sequence; `0` replays everything), then
    /// tail live. The broadcast subscription is created while the journal is
    /// locked, so an event can never fall between the tail snapshot and the
    /// live feed — the tail boundary is exactly-once.
    pub fn subscribe_with_tail(
        &self,
        since: u64,
    ) -> (Vec<(u64, SessionEvent)>, broadcast::Receiver<(u64, SessionEvent)>) {
        let journal = self.journal.lock().expect("daemon journal poisoned");
        let receiver = self.broadcast_tx.subscribe();
        let tail: Vec<(u64, SessionEvent)> = journal
            .iter()
            .filter(|(seq, _)| *seq > since)
            .cloned()
            .collect();
        (tail, receiver)
    }

    /// All journaled events — used by transports that must bridge a `Lagged`
    /// gap into a `DaemonError` for the client.
    pub fn journal_tail(&self) -> Vec<(u64, SessionEvent)> {
        self.journal
            .lock()
            .expect("daemon journal poisoned")
            .iter()
            .cloned()
            .collect()
    }

    // -- runtime registry -------------------------------------------------

    /// Register a runtime under its session identity so commands addressed
    /// by `session_id` resolve.
    pub fn register_runtime(
        &self,
        session_id: &str,
        work_dir: PathBuf,
        session_file: PathBuf,
        runtime: Arc<SessionRuntime>,
    ) -> Arc<SessionRuntime> {
        // Registration is publication only: callers may already have performed
        // expensive construction, so never wait on a construction gate here.
        let runtime = {
            let mut runtimes = self.runtimes.lock().expect("runtimes poisoned");
            runtimes
                .entry(session_file.clone())
                .or_insert_with(|| runtime.clone())
                .clone()
        };
        self.identities.lock().expect("identities poisoned").insert(
            session_id.to_string(),
            SessionIdentity { session_file, work_dir: Self::project_dir_for(&work_dir) },
        );
        runtime
    }

    fn construction_gate(&self, session_file: &Path) -> Arc<Mutex<()>> {
        self.runtime_construction
            .lock()
            .expect("runtime construction registry poisoned")
            .entry(session_file.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn prune_construction_gate(&self, session_file: &Path, gate: &Arc<Mutex<()>>) {
        let mut gates = self.runtime_construction.lock().expect("runtime construction registry poisoned");
        if Arc::strong_count(gate) == 2
            && gates.get(session_file).is_some_and(|current| Arc::ptr_eq(current, gate))
        {
            gates.remove(session_file);
        }
    }

    /// Serialized check-and-create boundary shared by desktop and paired clients.
    /// Call on a large-stack worker; construction loads WASI extensions.
    pub fn get_or_create_runtime(
        &self,
        session_id: &str,
        work_dir: PathBuf,
        session_file: PathBuf,
        options: threadlane_coding_agent::CodingAgentOptions,
        prepared: Option<Arc<SessionRuntime>>,
    ) -> Arc<SessionRuntime> {
        let gate = self.construction_gate(&session_file);
        let _construction = gate.lock().expect("runtime construction poisoned");
        if let Some(runtime) = self.runtime_for_file(&session_file) {
            drop(_construction);
            self.prune_construction_gate(&session_file, &gate);
            return runtime;
        }
        let runtime = prepared.unwrap_or_else(|| SessionRuntime::new(options));
        let runtime = {
            let mut runtimes = self.runtimes.lock().expect("runtimes poisoned");
            runtimes
                .entry(session_file.clone())
                .or_insert_with(|| runtime.clone())
                .clone()
        };
        self.identities.lock().expect("identities poisoned").insert(
            session_id.to_string(),
            SessionIdentity {
                session_file: session_file.clone(),
                work_dir: Self::project_dir_for(&work_dir),
            },
        );
        drop(_construction);
        self.prune_construction_gate(&session_file, &gate);
        runtime
    }

    /// Start construction immediately on the shared large-stack blocking pool.
    pub fn get_or_create_runtime_async(
        self: Arc<Self>,
        session_id: String,
        work_dir: PathBuf,
        session_file: PathBuf,
        options: threadlane_coding_agent::CodingAgentOptions,
        prepared: Option<Arc<SessionRuntime>>,
    ) -> tokio::task::JoinHandle<Arc<SessionRuntime>> {
        threadlane_provider::exec::get_runtime().spawn_blocking(move || {
            self.get_or_create_runtime(&session_id, work_dir, session_file, options, prepared)
        })
    }

    pub fn runtime_for_file(&self, session_file: &Path) -> Option<Arc<SessionRuntime>> {
        self.runtimes
            .lock()
            .expect("runtimes poisoned")
            .get(session_file)
            .cloned()
    }

    /// Snapshot of `(session_file, runtime)` pairs for status sweeps.
    pub fn runtimes(&self) -> Vec<(PathBuf, Arc<SessionRuntime>)> {
        self.runtimes
            .lock()
            .expect("runtimes poisoned")
            .iter()
            .map(|(file, runtime)| (file.clone(), runtime.clone()))
            .collect()
    }

    /// Drop a runtime so the next access rebuilds it with current config
    /// (model/effort/mode switching works by invalidation).
    pub fn drop_runtime(&self, session_file: &Path) -> Option<Arc<SessionRuntime>> {
        // Invalidation also ends the prepared handoff; otherwise the next
        // prompt could resurrect the old model from the mailbox.
        if let Some(session_id) = Self::session_id_for_file(session_file) {
            let _ = crate::runtimes::take_prepared_runtime(&session_id);
        }
        self.runtimes
            .lock()
            .expect("runtimes poisoned")
            .remove(session_file)
    }

    /// Remove a just-constructed runtime only if it is still the exact idle
    /// instance returned to the cancelled setup and no other client retained it.
    pub fn release_cancelled_runtime(
        &self,
        session_file: &Path,
        runtime: &Arc<SessionRuntime>,
    ) {
        let mut runtimes = self.runtimes.lock().expect("runtimes poisoned");
        if runtimes
            .get(session_file)
            .is_some_and(|current| Arc::ptr_eq(current, runtime))
            && !runtime.is_generating()
            && Arc::strong_count(runtime) == 2
        {
            runtimes.remove(session_file);
        }
    }

    /// `session_id` ↔ file: canonical layout is `<work_dir>/.threadlane/
    /// sessions/<session_id>.jsonl`, so the stem is always the id.
    pub fn session_id_for_file(session_file: &Path) -> Option<String> {
        session_file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
    }

    fn identity(&self, session_id: &str) -> Option<SessionIdentity> {
        self.identities
            .lock()
            .expect("identities poisoned")
            .get(session_id)
            .cloned()
    }

    /// Record where a session lives without attaching a runtime (hydration
    /// projection and deletion need it before/without construction).
    pub fn register_identity(&self, session_id: &str, work_dir: PathBuf, session_file: PathBuf) {
        self.identities
            .lock()
            .expect("identities poisoned")
            .insert(session_id.to_string(), SessionIdentity { session_file, work_dir });
    }

    /// Resolve `session_id` to a live runtime when one is registered.
    pub fn runtime_for_session(&self, session_id: &str) -> Option<Arc<SessionRuntime>> {
        let runtimes = self.runtimes.lock().expect("runtimes poisoned");
        if let Some(identity) = self.identity(session_id) {
            if let Some(runtime) = runtimes.get(&identity.session_file) {
                return Some(runtime.clone());
            }
        }
        // Fallback for runtimes registered before their identity (or by a
        // host that only knows the file): the canonical stem is the id.
        runtimes
            .iter()
            .find(|(file, _)| Self::session_id_for_file(file).as_deref() == Some(session_id))
            .map(|(_, runtime)| runtime.clone())
    }

    /// `runtime_for_session` confined to one project: an identity or
    /// session file registered under another `work_dir` cannot satisfy
    /// the lookup, so same-named sessions in two attached projects do
    /// not cross-resolve. `SessionIdentity.work_dir` is the project root,
    /// so the caller's effective checkout is normalized the same way
    /// registration is.
    pub fn runtime_for_session_in(
        &self,
        session_id: &str,
        work_dir: &Path,
    ) -> Option<Arc<SessionRuntime>> {
        let project_dir = Self::project_dir_for(work_dir);
        let runtimes = self.runtimes.lock().expect("runtimes poisoned");
        if let Some(identity) = self.identity(session_id) {
            if identity.work_dir == project_dir {
                if let Some(runtime) = runtimes.get(&identity.session_file) {
                    return Some(runtime.clone());
                }
            }
        }
        // Canonical layout first, then a scan confined to session files
        // inside the project (worktree transcripts nest under its root).
        if let Some(runtime) = runtimes.get(&canonical_session_file(work_dir, session_id)) {
            return Some(runtime.clone());
        }
        runtimes
            .iter()
            .find(|(file, _)| {
                Self::session_id_for_file(file).as_deref() == Some(session_id)
                    && file.starts_with(&project_dir)
            })
            .map(|(_, runtime)| runtime.clone())
    }

    /// Kill a hosted terminal whose owning client went away. Shares the
    /// `TerminalClose` path: kill now, entry reaped on reader EOF so the
    /// trailing `Exited` still orders after the last output.
    pub(crate) fn close_terminal(&self, terminal_id: &str) {
        self.terminals.close(terminal_id);
    }

    /// Resolve `session_id` to a runtime, constructing one lazily when the
    /// session is known but idle. Construction goes through the shared
    /// blocking pool — wasmi needs real stacks.
    pub async fn ensure_runtime(
        self: &Arc<Self>,
        session_id: &str,
        work_dir: &Path,
    ) -> Result<Arc<SessionRuntime>, String> {
        // A prepared runtime may already be registered; always drain its
        // handoff before the fast path so model invalidation cannot resurrect it.
        let prepared = crate::runtimes::take_prepared_runtime(session_id);
        if let Some(runtime) = self.runtime_for_session(session_id) {
            return Ok(runtime);
        }
        let session_file = self
            .identity(session_id)
            .map(|identity| identity.session_file)
            .or_else(|| {
                prepared
                    .as_ref()
                    .map(|runtime| runtime.session_file.clone())
            })
            .unwrap_or_else(|| canonical_session_file(work_dir, session_id));
        let options = coding_agent_options(
            work_dir.to_path_buf(),
            session_file.clone(),
            self.model.read().expect("model poisoned").clone(),
            self.model_roles
                .read()
                .expect("model roles poisoned")
                .clone(),
            self.browser_bridge
                .read()
                .expect("browser bridge poisoned")
                .clone(),
        );
        Arc::clone(self)
            .get_or_create_runtime_async(
                session_id.to_owned(),
                Self::project_dir_for(work_dir),
                session_file,
                options,
                prepared,
            )
            .await
            .map_err(|error| format!("session runtime construction failed: {error}"))
    }

    /// Construct a runtime with explicit hydration options rather than the
    /// core's current selection (hydration pins the session's own model).
    async fn hydrate_runtime(
        self: &Arc<Self>,
        request: &SessionHydrationRequest,
    ) -> Result<Option<Arc<SessionRuntime>>, String> {
        let Some(options) = &request.runtime_options else {
            return Ok(None);
        };
        self.register_identity(
            &request.session_id,
            Self::project_dir_for(&options.work_dir),
            request.session_file.clone(),
        );
        let agent_options = coding_agent_options(
            options.work_dir.clone(),
            request.session_file.clone(),
            options.model.clone(),
            options.model_roles.clone(),
            self.browser_bridge
                .read()
                .expect("browser bridge poisoned")
                .clone(),
        );
        let runtime = Arc::clone(self)
            .get_or_create_runtime_async(
                request.session_id.clone(),
                Self::project_dir_for(&options.work_dir),
                request.session_file.clone(),
                agent_options,
                None,
            )
            .await
            .map_err(|error| format!("session runtime construction failed: {error}"))?;
        Ok(Some(runtime))
    }

    // -- command surface --------------------------------------------------

    /// Execute one [`SessionCommand`]. Errors are also surfaced to clients
    /// as `SessionEvent::DaemonError` so remote callers see failures the
    /// transport cannot return. The returned [`CommandResponse`] is what a
    /// `CommandRequest` caller receives; fire-and-forget callers ignore it.
    pub async fn dispatch(
        self: &Arc<Self>,
        command: SessionCommand,
    ) -> Result<CommandResponse, String> {
        self.dispatch_with_request_id(command, None).await
    }

    /// [`Self::dispatch`] for a `CommandRequest`: the caller's
    /// `request_id` is recorded on the journaled
    /// `SessionEvent::QueuedEntryCancelled` so a requester that lost its
    /// reply to a disconnect can correlate the cancellation on replay.
    pub async fn dispatch_with_request_id(
        self: &Arc<Self>,
        command: SessionCommand,
        request_id: Option<u64>,
    ) -> Result<CommandResponse, String> {
        // Payload commands return their error to the requester in the
        // `CommandResponse`; echoing it again as a broadcast `DaemonError`
        // would surface an expected request-scoped failure (e.g. a
        // `FileInventory` "not a repository") as a global daemon fault.
        let reports_via_response = matches!(
            command,
            SessionCommand::BeginSession { .. }
                | SessionCommand::GetComposerOptions { .. }
                | SessionCommand::SearchProjectFiles { .. }
                | SessionCommand::ValidateSearchTarget { .. }
                | SessionCommand::ListProjectFiles { .. }
                | SessionCommand::ReadProjectFile { .. }
                | SessionCommand::ReadProjectFileVersioned { .. }
                | SessionCommand::WriteProjectFileGuarded { .. }
                | SessionCommand::ProjectFileExists { .. }
                | SessionCommand::EditorLsp { .. }
                | SessionCommand::GitRequest { .. }
                | SessionCommand::GitHubRequest { .. }
                | SessionCommand::AutomationRequest { .. }
        );
        let result = self.dispatch_inner(command, request_id).await;
        if let Err(error) = &result {
            if !reports_via_response {
                let _ = self.ingest_tx.send(SessionEvent::DaemonError {
                    session_id: None,
                    message: error.clone(),
                });
            }
        }
        result
    }

    async fn dispatch_inner(
        self: &Arc<Self>,
        command: SessionCommand,
        request_id: Option<u64>,
    ) -> Result<CommandResponse, String> {
        if let SessionCommand::EditorLsp { request } = &command {
            return Ok(CommandResponse::EditorLsp {
                result: self.execute_editor_lsp_request(request).await,
            });
        }
        // The one command with a return payload short-circuits here; the
        // rest are fire-and-forget effects that answer `Ack` to a request.
        if let SessionCommand::CancelQueuedMessage {
            session_id,
            entry_id,
            work_dir,
        } = &command
        {
            let runtime = match work_dir.as_deref() {
                Some(work_dir) => self.runtime_for_session_in(session_id, work_dir),
                None => self.runtime_for_session(session_id),
            }
            .ok_or_else(|| format!("no live runtime for session {session_id}"))?;
            return runtime
                .work_handle
                .cancel_queued_entry(entry_id)
                .map(|(text, images)| {
                    // The point-to-point reply is gone for good when the
                    // socket dies mid-flight; the journaled copy is how a
                    // reconnected requester still recovers the payload —
                    // and how every other client learns the entry left.
                    let _ = self.ingest_tx.send(SessionEvent::QueuedEntryCancelled {
                        session_id: session_id.clone(),
                        entry_id: entry_id.clone(),
                        request_id,
                        text: text.clone(),
                        images: images.clone(),
                    });
                    CommandResponse::CancelledQueuedMessage {
                        session_id: session_id.clone(),
                        entry_id: entry_id.clone(),
                        text,
                        images,
                    }
                });
        }
        // Project-io commands with a return payload resolve here; all are
        // blocking filesystem/Git work.
        match &command {
            SessionCommand::BeginSession { work_dir } => {
                if !self.attached_project_dirs().contains(work_dir) {
                    return Err("Choose an attached project to start a session".into());
                }
                let id = format!(
                    "mobile-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|e| e.to_string())?
                        .as_nanos()
                );
                let session_file = canonical_session_file(work_dir, &id);
                self.register_identity(&id, work_dir.clone(), session_file.clone());
                return Ok(CommandResponse::SessionDraft {
                    session: SessionInfo {
                        id,
                        title: "New session".into(),
                        work_dir: work_dir.clone(),
                        runtime_work_dir: work_dir.clone(),
                        session_file,
                        ..SessionInfo::default()
                    },
                });
            }
            SessionCommand::GetComposerOptions {
                work_dir,
                session_id,
            } => {
                let facts = session_id
                    .as_deref()
                    .and_then(|id| self.identity(id))
                    .and_then(|identity| {
                        threadlane_runtime::harness::JsonlStore::open_read_only(
                            &identity.session_file,
                        )
                        .ok()
                    })
                    .map(|store| store.facts());
                let models = crate::catalog::available_models_for_project(Some(work_dir))
                    .into_iter()
                    .map(|model| threadlane_protocol::daemon::ComposerModel {
                        efforts: if crate::catalog::supports_reasoning(&model.id, Some(work_dir)) {
                            crate::catalog::efforts_for_model(&model.id, Some(work_dir))
                        } else {
                            Vec::new()
                        },
                        id: model.id,
                        label: model.label,
                    })
                    .collect();
                return Ok(CommandResponse::ComposerOptions {
                    models,
                    model: facts
                        .as_ref()
                        .and_then(|facts| facts.get("model"))
                        .cloned()
                        .unwrap_or_else(|| self.model.read().expect("model poisoned").clone()),
                    effort: facts
                        .as_ref()
                        .and_then(|facts| facts.get("reasoning_effort"))
                        .and_then(|value| ReasoningEffort::from_label(value))
                        .unwrap_or_else(|| *self.effort.read().expect("effort poisoned")),
                    mode: threadlane_project::subagent_settings::load(work_dir).orchestrator_mode,
                });
            }
            SessionCommand::SearchProjectFiles { work_dir, query } => {
                // Bound simultaneous blocking scans even across connected clients.
                static SEARCH_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
                let permit = SEARCH_SLOTS.try_acquire().map_err(|_| "Search busy; retry shortly")?;
                let work_dir = work_dir.clone();
                let query = query.clone();
                let result = run_blocking_io(move || {
                    let _permit = permit;
                    crate::file_search::search(&work_dir, &query)
                }).await??;
                return Ok(CommandResponse::FileSearch { result });
            }
            SessionCommand::ValidateSearchTarget { work_dir, path } => {
                let work_dir = work_dir.clone();
                let path = path.clone();
                run_blocking_io(move || crate::file_search::validate_target(&work_dir, &path)).await??;
                return Ok(CommandResponse::Ack);
            }
            SessionCommand::ListProjectFiles { work_dir, limit } => {
                let work_dir = work_dir.clone();
                let limit = *limit;
                let nodes = run_blocking_io(move || {
                    threadlane_project::files::scan_project_tree(&work_dir, limit)
                })
                .await?;
                return Ok(CommandResponse::ProjectFiles { nodes });
            }
            SessionCommand::ReadProjectFile { work_dir, path } => {
                let work_dir = work_dir.clone();
                let path = path.clone();
                let content = run_blocking_io(move || {
                    threadlane_project::files::read_project_file(&work_dir, &path)
                })
                .await??;
                return Ok(CommandResponse::FileContent { content });
            }
            SessionCommand::ReadProjectFileVersioned { work_dir, path } => {
                let work_dir = work_dir.clone();
                let path = path.clone();
                let result = match run_blocking_io(move || {
                    threadlane_project::files::read_project_file_versioned(&work_dir, &path)
                })
                .await
                {
                    Ok(result) => result,
                    Err(message) => Err(threadlane_protocol::daemon::ProjectFileError::Io {
                        message,
                    }),
                };
                return Ok(CommandResponse::VersionedFile { result });
            }
            SessionCommand::WriteProjectFileGuarded {
                work_dir,
                path,
                content,
                expected_version,
            } => {
                let work_dir = work_dir.clone();
                let path = path.clone();
                let content = content.clone();
                let expected_version = expected_version.clone();
                let result = match run_blocking_io(move || {
                    threadlane_project::files::write_project_file_guarded(
                        &work_dir,
                        &path,
                        &content,
                        &expected_version,
                    )
                })
                .await
                {
                    Ok(result) => result,
                    Err(message) => Err(threadlane_protocol::daemon::ProjectFileError::Io {
                        message,
                    }),
                };
                return Ok(CommandResponse::GuardedFileWrite { result });
            }
            SessionCommand::ProjectFileExists { work_dir, path } => {
                let work_dir = work_dir.clone();
                let path = path.clone();
                let exists = run_blocking_io(move || {
                    threadlane_project::files::project_file_exists(&work_dir, &path)
                })
                .await??;
                return Ok(CommandResponse::FileExists { exists });
            }
            SessionCommand::GitRequest { work_dir, operation } => {
                let work_dir = work_dir.clone();
                let operation = operation.clone();
                let response = run_blocking_io(move || {
                    crate::project_io::run_git_operation(&work_dir, &operation)
                })
                .await??;
                return Ok(CommandResponse::Git { response });
            }
            SessionCommand::GitHubRequest { work_dir, operation } => {
                let work_dir = work_dir.clone();
                let operation = operation.clone();
                let response = run_blocking_io(move || {
                    crate::project_io::run_github_operation(&work_dir, &operation)
                })
                .await??;
                return Ok(CommandResponse::GitHub { response });
            }
            SessionCommand::AutomationRequest { command } => {
                use threadlane_protocol::automation::AutomationCommand as AutomationWire;
                let service = crate::automation::AutomationService::shared();
                match command {
                    AutomationWire::GetSnapshot => {}
                    AutomationWire::Save { definition } => {
                        service
                            .command(crate::automation::Command::Save(definition.clone()))
                            .await?;
                    }
                    AutomationWire::SetEnabled { id, enabled } => {
                        service
                            .command(crate::automation::Command::SetEnabled(
                                id.clone(),
                                *enabled,
                            ))
                            .await?;
                    }
                    AutomationWire::Delete { id } => {
                        service
                            .command(crate::automation::Command::Delete(id.clone()))
                            .await?;
                    }
                    AutomationWire::DeleteRun { id } => {
                        service
                            .command(crate::automation::Command::DeleteRun(id.clone()))
                            .await?;
                    }
                    AutomationWire::RunNow { id } => {
                        service
                            .command(crate::automation::Command::RunNow(id.clone()))
                            .await?;
                    }
                    AutomationWire::Cancel { id } => {
                        service
                            .command(crate::automation::Command::Cancel(id.clone()))
                            .await?;
                    }
                    AutomationWire::Review { id } => {
                        service
                            .command(crate::automation::Command::Review(id.clone()))
                            .await?;
                    }
                }
                // Mutations answer with the post-command projection — the
                // actor has applied the command when `command` resolves,
                // though its watch update can lag a tick, so read fresh
                // rather than replaying `borrow()`.
                // `service.command` resolves inside the actor loop just
                // before its `publish()`, so a bounded wait on `changed()`
                // lands the post-command revision without a fixed sleep;
                // a no-op publish (unchanged projection) falls through on
                // the timeout to the still-current value.
                let projection = {
                    let mut receiver = service.projection.clone();
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(100),
                        receiver.changed(),
                    )
                    .await;
                    let snapshot = receiver.borrow().clone();
                    snapshot
                };
                return Ok(CommandResponse::Automation {
                    response: threadlane_protocol::automation::AutomationResponse::Projection {
                        projection: Self::automation_projection_wire(&projection),
                    },
                });
            }
            _ => {}
        }
        self.dispatch_effect(command)
            .await
            .map(|_| CommandResponse::Ack)
    }

    async fn execute_editor_lsp_request(
        &self,
        request: &EditorLspRequest,
    ) -> Result<EditorLspResponse, String> {
        if request.text.len() > MAX_EDITOR_LSP_BYTES {
            return Err("Editor LSP buffer snapshot exceeds the 1 MiB limit".into());
        }
        let encoded_request =
            serde_json::to_vec(request).map_err(|error| error.to_string())?;
        if encoded_request.len() > MAX_EDITOR_LSP_BYTES {
            return Err("Editor LSP request exceeds the 1 MiB limit".into());
        }
        let runtime = self
            .runtime_for_session_in(&request.session_id, &request.work_dir)
            .ok_or_else(|| "activate a session in this checkout".to_string())?;
        if request
            .expected_runtime_id
            .is_some_and(|expected| expected != runtime.instance_id())
        {
            return Err("Editor LSP request targets a stale session runtime".into());
        }
        let requested_work_dir = request
            .work_dir
            .canonicalize()
            .map_err(|error| format!("Editor LSP checkout does not resolve: {error}"))?;
        let runtime_work_dir = runtime
            .editor_lsp_work_dir()
            .canonicalize()
            .map_err(|error| format!("Session runtime checkout does not resolve: {error}"))?;
        if requested_work_dir != runtime_work_dir {
            return Err("Editor LSP checkout does not match the active session runtime".into());
        }
        threadlane_project::files::resolve_project_path(&requested_work_dir, &request.path)?;

        let extension_response = runtime.execute_editor_lsp(request).await?;
        if serde_json::to_vec(&extension_response)
            .map_or(true, |bytes| bytes.len() > MAX_EDITOR_LSP_BYTES)
        {
            return Err("Editor LSP extension response exceeds the 1 MiB limit".into());
        }
        let current_runtime = self.runtime_for_session_in(&request.session_id, &request.work_dir);
        if !current_runtime
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &runtime))
        {
            return Err("Session runtime changed while the editor LSP request was running".into());
        }
        let server = extension_response
            .get("server")
            .and_then(serde_json::Value::as_str)
            .filter(|server| !server.is_empty())
            .ok_or("Editor LSP extension response is missing its server identity")?
            .to_owned();
        let result = extension_response
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let diagnostics = extension_response
            .get("diagnostics")
            .and_then(serde_json::Value::as_array)
            .cloned();
        let is_close = matches!(&request.operation, EditorLspOperation::Close);
        let server_document_version =
            parse_editor_server_document_version(&extension_response, is_close)?;
        let response = EditorLspResponse {
            document_id: request.document_id,
            version: request.version,
            runtime_id: runtime.instance_id(),
            server,
            server_document_version,
            result,
            diagnostics,
        };
        if serde_json::to_vec(&response)
            .map_or(true, |bytes| bytes.len() > MAX_EDITOR_LSP_BYTES)
        {
            return Err("Editor LSP response exceeds the 1 MiB limit".into());
        }
        Ok(response)
    }

    async fn dispatch_effect(self: &Arc<Self>, command: SessionCommand) -> Result<(), String> {
        match command {
            SessionCommand::SubmitPrompt {
                session_id,
                work_dir,
                text,
                images,
                effort,
                acp_config,
                model,
            } => {
                if let Some(model) = model {
                    *self.model.write().expect("model poisoned") = model;
                }
                let runtime = self.ensure_runtime(&session_id, &work_dir).await?;
                // `None` preserves the daemon's current effort so a client
                // without an effort control can't clobber another's choice.
                if let Some(effort) = effort {
                    *self.effort.write().expect("effort poisoned") = effort;
                }
                let effort = *self.effort.read().expect("effort poisoned");
                if runtime.is_generating() {
                    // Queue a follow-up rather than erroring — a busy turn
                    // picks it up when it settles. Remote clients get the
                    // durable entry id through FollowUpQueued so their
                    // optimistic echo can grow steer/cancel controls.
                    let entry_id = runtime
                        .work_handle
                        .try_queue_follow_up_with_images(text, images)
                        .map_err(|_| "session is busy and the follow-up queue is full".to_string())?;
                    let _ = self.ingest_tx.send(SessionEvent::FollowUpQueued {
                        session_id,
                        entry_id,
                    });
                    Ok(())
                } else {
                    crate::chat::execute_prompt(
                        runtime,
                        work_dir,
                        session_id,
                        text,
                        images,
                        effort,
                        self.ingest_tx.clone(),
                        acp_config,
                    )
                }
            }
            SessionCommand::CancelRun { session_id } => {
                let runtime = self
                    .runtime_for_session(&session_id)
                    .ok_or_else(|| format!("no live runtime for session {session_id}"))?;
                crate::chat::cancel_prompt(runtime, session_id, self.ingest_tx.clone())
            }
            SessionCommand::AnswerPermission {
                session_id,
                request_id,
                decision,
            } => {
                let runtime = self
                    .runtime_for_session(&session_id)
                    .ok_or_else(|| format!("no live runtime for session {session_id}"))?;
                if runtime.resolve_permission(&request_id, decision) {
                    crate::automation::AutomationService::shared()
                        .resolved(session_id, request_id);
                    Ok(())
                } else {
                    Err(format!("permission request {request_id} is no longer pending"))
                }
            }
            SessionCommand::AnswerQuestion { session_id, answer } => {
                let runtime = self
                    .runtime_for_session(&session_id)
                    .ok_or_else(|| format!("no live runtime for session {session_id}"))?;
                let request_id = answer.request_id.clone();
                if runtime.resolve_question(&request_id, answer) {
                    crate::automation::AutomationService::shared()
                        .resolved(session_id, request_id);
                    Ok(())
                } else {
                    Err(format!("question request {request_id} is no longer pending"))
                }
            }
            SessionCommand::SetModel { session_id, model } => {
                self.switch_runtime_fact(&session_id, "model", &model, "changing models")?;
                *self.model.write().expect("model poisoned") = model;
                Ok(())
            }
            SessionCommand::SetReasoningEffort {
                session_id,
                effort,
            } => {
                self.switch_runtime_fact(
                    &session_id,
                    "reasoning_effort",
                    effort.label(),
                    "changing reasoning effort",
                )?;
                *self.effort.write().expect("effort poisoned") = effort;
                Ok(())
            }
            SessionCommand::SetModelRoles { session_id, roles } => {
                *self.model_roles.write().expect("model roles poisoned") = roles.clone();
                if let Some(runtime) = self.runtime_for_session(&session_id) {
                    runtime.set_model_roles(roles).await;
                }
                Ok(())
            }
            SessionCommand::SetOrchestratorMode { session_id, mode } => {
                let identity = self
                    .identity(&session_id)
                    .ok_or_else(|| format!("unknown session {session_id}"))?;
                let mut settings =
                    threadlane_project::subagent_settings::load(&identity.work_dir);
                settings.orchestrator_mode = mode;
                threadlane_project::subagent_settings::save(&identity.work_dir, &settings)
                    .map_err(|error| format!("could not save orchestrator mode: {error}"))?;
                if let Some(runtime) = self.runtime_for_session(&session_id) {
                    if runtime.is_generating() {
                        // Persisted settings apply to the next turn; keep the
                        // live runtime rather than dropping it mid-run.
                        return Ok(());
                    }
                    self.drop_runtime(&identity.session_file);
                }
                Ok(())
            }
            SessionCommand::LoadAcpConfigOptions { session_id } => {
                let runtime = self
                    .runtime_for_session(&session_id)
                    .ok_or_else(|| format!("no live runtime for session {session_id}"))?;
                crate::chat::load_acp_config_options(
                    runtime,
                    session_id,
                    self.ingest_tx.clone(),
                )
            }
            SessionCommand::SetAcpConfigOption {
                session_id,
                config_id,
                value,
            } => {
                let runtime = self
                    .runtime_for_session(&session_id)
                    .ok_or_else(|| format!("no live runtime for session {session_id}"))?;
                crate::chat::set_acp_config_option(
                    runtime,
                    session_id,
                    config_id,
                    value,
                    self.ingest_tx.clone(),
                )
            }
            SessionCommand::HydrateSession { request } => self.hydrate_session(request).await,
            SessionCommand::PrepareWorktree { setup } => {
                crate::worktree_setup::persist_request(&setup)?;
                // Options carry the *project* dir: the setup flow moves
                // execution into the worktree as it creates it.
                let options = coding_agent_options(
                    setup.project.clone(),
                    setup.session_file.clone(),
                    setup.model.clone(),
                    self.model_roles.read().expect("model roles poisoned").clone(),
                    self.browser_bridge
                        .read()
                        .expect("browser bridge poisoned")
                        .clone(),
                );
                self.worktree_setups
                    .lock()
                    .expect("worktree setups poisoned")
                    .insert(setup.session_id.clone(), setup.clone());
                crate::worktree_setup::start(self.clone(), setup, options, self.ingest_tx.clone())
            }
            SessionCommand::CancelWorktreeSetup { session_id } => {
                let setup = self
                    .worktree_setups
                    .lock()
                    .expect("worktree setups poisoned")
                    .remove(&session_id);
                if let Some(setup) = setup {
                    if let Some(runtime) = crate::runtimes::cancel_prepared_runtime(
                        &session_id,
                        &setup.cancelled,
                    ) {
                        self.release_cancelled_runtime(&setup.session_file, &runtime);
                    }
                } else {
                    // A completed setup may already have left the registry.
                    let _ = crate::runtimes::take_prepared_runtime(&session_id);
                }
                Ok(())
            }
            SessionCommand::DeleteSession {
                session_id,
                session_file,
                delete_worktree,
            } => self.delete_session(&session_id, &session_file, delete_worktree),
            SessionCommand::AddProject { work_dir } => {
                self.attach_project(work_dir.clone());
                let sessions = crate::discovery::discover_sessions_in_project(&work_dir);
                let name = work_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string();
                let _ = self.ingest_tx.send(SessionEvent::ProjectChanged {
                    project: ProjectInfo {
                        name,
                        work_dir,
                        sessions,
                        is_expanded: true,
                    },
                });
                Ok(())
            }
            SessionCommand::RemoveProject { work_dir } => {
                self.detach_project(&work_dir);
                let _ = self.ingest_tx.send(SessionEvent::ProjectChanged {
                    project: ProjectInfo {
                        name: work_dir
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default()
                            .to_string(),
                        work_dir,
                        sessions: Vec::new(),
                        is_expanded: false,
                    },
                });
                Ok(())
            }
            SessionCommand::RefreshCatalog { work_dir } => {
                crate::chat::executor()?.spawn(async move {
                    crate::catalog::refresh_discovered_models().await;
                    crate::catalog::refresh_acp_models(work_dir).await;
                });
                Ok(())
            }
            SessionCommand::TerminalOpen {
                terminal_id,
                cwd,
                cols,
                rows,
            } => match self
                .terminals
                .open(&terminal_id, &cwd, cols, rows, self.ingest_tx.clone())
            {
                Ok(()) => Ok(()),
                // Scope the failure to the owning terminal — a global
                // DaemonError would leave the waiting view blank forever.
                Err(message) => {
                    let _ = self.ingest_tx.send(SessionEvent::TerminalEvent {
                        event: TerminalEvent::Failed {
                            terminal_id,
                            message,
                        },
                    });
                    Ok(())
                }
            },
            SessionCommand::TerminalClose { terminal_id } => {
                self.terminals.close(&terminal_id);
                Ok(())
            }
            SessionCommand::TerminalInput { terminal_id, data } => {
                self.terminals.input(&terminal_id, &data)
            }
            SessionCommand::TerminalResize {
                terminal_id,
                cols,
                rows,
            } => self
                .terminals
                .resize(&terminal_id, cols, rows, &self.ingest_tx),
            SessionCommand::SteerMessage {
                session_id,
                text,
                images,
            } => {
                let runtime = self
                    .runtime_for_session(&session_id)
                    .ok_or_else(|| format!("no live runtime for session {session_id}"))?;
                runtime
                    .work_handle
                    .queue_steer_with_images(text, images)
                    .map(|_| ())
            }
            SessionCommand::SteerQueuedMessage {
                session_id,
                entry_id,
            } => {
                let runtime = self
                    .runtime_for_session(&session_id)
                    .ok_or_else(|| format!("no live runtime for session {session_id}"))?;
                runtime.work_handle.steer_queued_entry(&entry_id)
            }
            SessionCommand::CancelQueuedMessage { .. } => {
                unreachable!("payload commands are handled in dispatch_inner")
            }
            SessionCommand::BeginSession { .. } | SessionCommand::GetComposerOptions { .. } => {
                unreachable!("payload commands are handled in dispatch_inner")
            }
            SessionCommand::GetProjects => {
                for work_dir in self.attached_project_dirs() {
                    let sessions = crate::discovery::discover_sessions_in_project(&work_dir);
                    let name = work_dir
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_string();
                    let _ = self.ingest_tx.send(SessionEvent::ProjectChanged {
                        project: ProjectInfo {
                            name,
                            work_dir,
                            sessions,
                            is_expanded: true,
                        },
                    });
                }
                Ok(())
            }
            SessionCommand::GetProjectState { work_dir } => {
                let sessions = crate::discovery::discover_sessions_in_project(&work_dir);
                let name = work_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string();
                let _ = self.ingest_tx.send(SessionEvent::ProjectChanged {
                    project: ProjectInfo {
                        name,
                        work_dir,
                        sessions,
                        is_expanded: true,
                    },
                });
                Ok(())
            }
            SessionCommand::GetSessionSnapshot { session_id } => {
                self.emit_session_snapshot(&session_id)
            }
            SessionCommand::WriteProjectFile {
                work_dir,
                path,
                content,
            } => {
                run_blocking_io(move || {
                    threadlane_project::files::write_project_file(&work_dir, &path, &content)
                })
                .await?
            }
            SessionCommand::WatchProject { work_dir } => self
                .project_watchers
                .watch(work_dir, self.ingest_tx.clone()),
            SessionCommand::UnwatchProject { work_dir } => {
                self.project_watchers.unwatch(&work_dir)
            }
            SessionCommand::GetWorktreeBases { work_dir } => {
                // Answered through the journaled `WorktreeBases` event —
                // same path the embedded host's direct call used to take.
                let ingest_tx = self.ingest_tx.clone();
                let emit = move || {
                    let result = threadlane_git::worktree_bases(&work_dir)
                        .map_err(|error| error.to_string());
                    let _ = ingest_tx.send(SessionEvent::WorktreeBases {
                        project: work_dir,
                        result,
                    });
                };
                if tokio::runtime::Handle::try_current().is_ok() {
                    crate::chat::executor()?.spawn_blocking(emit);
                } else {
                    emit();
                }
                Ok(())
            }
            // Payload commands are answered inside `dispatch_inner` and
            // never reach the effect path.
            SessionCommand::SearchProjectFiles { .. }
            | SessionCommand::ValidateSearchTarget { .. }
            | SessionCommand::ListProjectFiles { .. }
            | SessionCommand::EditorLsp { .. }
            | SessionCommand::ReadProjectFile { .. }
            | SessionCommand::ReadProjectFileVersioned { .. }
            | SessionCommand::WriteProjectFileGuarded { .. }
            | SessionCommand::ProjectFileExists { .. }
            | SessionCommand::GitRequest { .. }
            | SessionCommand::GitHubRequest { .. }
            | SessionCommand::AutomationRequest { .. } => {
                Err("payload command bypassed response dispatch".into())
            }
        }
    }

    /// `set_fact` on the session's live agent, then drop the runtime so the
    /// next access rebuilds it (credentials and providers re-resolve).
    /// The owning *project* directory for a runtime's working directory.
    ///
    /// Commands carry the effective checkout (`SubmitPrompt.work_dir`,
    /// `HydrationRuntimeOptions.work_dir`) which is the worktree for
    /// worktree sessions; `SessionIdentity.work_dir` is the project root,
    /// which stub discovery and session deletion search. Worktrees live
    /// canonically at `<project>/.threadlane/worktrees/<name>`.
    fn project_dir_for(work_dir: &Path) -> PathBuf {
        let mut segments = work_dir.components().collect::<Vec<_>>();
        // Walk up to a `.threadlane/worktrees` parent, if any.
        while let Some(last) = segments.last() {
            if last.as_os_str() == "worktrees" {
                segments.pop();
                if segments.last().is_some_and(|seg| seg.as_os_str() == ".threadlane") {
                    segments.pop();
                    return segments.iter().collect();
                }
                break;
            }
            segments.pop();
        }
        work_dir.to_path_buf()
    }

    fn switch_runtime_fact(
        &self,
        session_id: &str,
        fact: &str,
        value: &str,
        action: &str,
    ) -> Result<(), String> {
        let Some(identity) = self.identity(session_id) else {
            // No attached session: the new selection is already stored and
            // applies to whatever runtime is built next.
            return Ok(());
        };
        let Some(runtime) = self.runtime_for_file(&identity.session_file) else {
            // Thin clients inspect transcripts without constructing a runtime.
            // Persist settings through the same locked harness journal.
            if identity.session_file.exists() {
                return threadlane_coding_agent::harness::CodingSessionHarness::append_fact_to_path(
                    &identity.session_file,
                    "main",
                    fact,
                    value,
                    None,
                );
            }
            return Ok(());
        };
        if runtime.is_generating() {
            return Err(format!("stop the current turn before {action}"));
        }
        if let Some(error) = runtime.harness_error() {
            return Err(error.to_string());
        }
        match runtime.agent.try_lock() {
            Ok(mut agent) => agent
                .set_fact(fact, value)
                .map_err(|error| format!("could not switch: {error}"))?,
            Err(_) => {
                return Err("agent settings are still loading; try again shortly".to_string())
            }
        }
        self.drop_runtime(&identity.session_file);
        Ok(())
    }

    /// Project the durable transcript and emit `SessionSnapshot` (attach
    /// mid-run: snapshot first, live tail continues after it).
    fn emit_session_snapshot(&self, session_id: &str) -> Result<(), String> {
        let identity = self
            .identity(session_id)
            .ok_or_else(|| format!("unknown session {session_id}"))?;
        let snapshot = self.build_snapshot(session_id, &identity)?;
        let _ = self.ingest_tx.send(SessionEvent::SessionSnapshot {
            session_id: session_id.to_string(),
            snapshot: Box::new(snapshot),
        });
        Ok(())
    }

    fn build_snapshot(
        &self,
        session_id: &str,
        identity: &SessionIdentity,
    ) -> Result<SessionSnapshot, String> {
        let session = crate::discovery::discover_sessions_in_project(&identity.work_dir)
            .into_iter()
            .find(|session| session.id == session_id)
            .unwrap_or_else(|| SessionInfo {
                id: session_id.to_string(),
                session_file: identity.session_file.clone(),
                work_dir: identity.work_dir.clone(),
                runtime_work_dir: identity.work_dir.clone(),
                ..SessionInfo::default()
            });
        // Diagnostics and token efficiency stay daemon-local; the wire
        // snapshot carries the fields a remote client can render.
        let projection = compute_full_session_projection(&identity.session_file)
            .map_err(|error| format!("could not project session {session_id}: {error}"))?;
        let messages =
            compute_session_messages_for_hydration(&identity.session_file).unwrap_or_default();
        Ok(SessionSnapshot {
            session,
            messages,
            trajectory: projection.trajectory,
            subagents: projection.subagents,
            plan: projection.plan,
            metrics: projection.metrics,
            token_usage: projection.token_usage,
            context_window: projection.context_window,
            run_timing: projection.run_timing,
        })
    }

    /// `HydrateSession`: register identity, (re)build the runtime when
    /// `runtime_options` asks for it, then answer with a snapshot.
    async fn hydrate_session(
        self: &Arc<Self>,
        request: SessionHydrationRequest,
    ) -> Result<(), String> {
        let identity = self.identity(&request.session_id);
        let work_dir = request
            .runtime_options
            .as_ref()
            .map(|options| Self::project_dir_for(&options.work_dir))
            .or_else(|| identity.as_ref().map(|identity| identity.work_dir.clone()))
            // The canonical layout puts the transcript inside the project's
            // `.threadlane/sessions/`; fall back to its owning root.
            .or_else(|| {
                request
                    .session_file
                    .parent()
                    .and_then(|sessions| sessions.parent())
                    .and_then(|threadlane| threadlane.parent())
                    .map(Self::project_dir_for)
            })
            .ok_or_else(|| {
                format!(
                    "cannot resolve work dir for session {}",
                    request.session_id
                )
            })?;
        self.register_identity(
            &request.session_id,
            work_dir.clone(),
            request.session_file.clone(),
        );
        self.hydrate_runtime(&request).await?;
        let resolved = SessionIdentity {
            session_file: request.session_file.clone(),
            work_dir,
        };
        // A projection failure must surface as DaemonError rather than an
        // empty snapshot — a client cannot match a default snapshot to the
        // session it asked for and would hang on the loading row.
        let snapshot = self.build_snapshot(&request.session_id, &resolved)?;
        let _ = self.ingest_tx.send(SessionEvent::SessionSnapshot {
            session_id: request.session_id,
            snapshot: Box::new(snapshot),
        });
        Ok(())
    }

    /// `DeleteSession`: archive the transcript, drop the runtime, remove
    /// the session file, and — when requested and clean — its worktree.
    fn delete_session(
        &self,
        session_id: &str,
        session_file: &Path,
        delete_worktree: bool,
    ) -> Result<(), String> {
        if let Some(runtime) = self.runtime_for_file(session_file) {
            if runtime.is_generating() {
                return Err("stop the running generation before deleting this session".into());
            }
        }
        let identity = self.identity(session_id);
        let work_dir = identity
            .as_ref()
            .map(|identity| identity.work_dir.clone())
            .or_else(|| {
                session_file
                    .parent()
                    .and_then(|sessions| sessions.parent())
                    .and_then(|threadlane| threadlane.parent())
                    .map(|root| root.to_path_buf())
            })
            .ok_or_else(|| format!("cannot resolve work dir for session {session_id}"))?;

        // Archive the transcript first: deletion destroys the JSONL, and a
        // failed delete must never lose history silently.
        let archive_dir = work_dir.join(".threadlane/sessions/archive");
        std::fs::create_dir_all(&archive_dir).map_err(|error| error.to_string())?;
        let file_name = session_file
            .file_name()
            .ok_or_else(|| "session file has no file name".to_string())?;
        if session_file.exists() {
            std::fs::copy(session_file, archive_dir.join(file_name))
                .map_err(|error| error.to_string())?;
        }
        if delete_worktree {
            if let Some(worktree_dir) =
                Self::session_worktree_dir(&work_dir, session_id)
            {
                // Another session may share the checkout; refuse to delete it.
                let shared = crate::discovery::discover_session_stubs_in_project(&work_dir)
                    .iter()
                    .any(|session| {
                        session.id != session_id
                            && session.is_worktree
                            && session.runtime_work_dir == worktree_dir
                    });
                if shared {
                    return Err(
                        "this worktree is used by another session; keep the worktree".into(),
                    );
                }
                if worktree_dir.exists() {
                    // Untracked Threadlane bookkeeping inside the checkout
                    // never blocks deletion; every other change does.
                    let dirty = threadlane_git::inspect(&worktree_dir)
                        .map_err(|error| error.to_string())?
                        .files
                        .iter()
                        .any(|file| {
                            !(file.is_untracked() && file.path.starts_with(".threadlane/"))
                        });
                    if dirty {
                        return Err(
                            "commit or discard worktree changes before deleting this session"
                                .into(),
                        );
                    }
                    threadlane_git::remove_worktree(&work_dir, &worktree_dir, true)
                        .map_err(|error| error.to_string())?;
                    threadlane_tools::remove_worktree_cargo_target_dir(&worktree_dir);
                    if let Err(error) = threadlane_git::prune_worktrees(&work_dir) {
                        tracing::warn!("worktree prune failed: {error}");
                    }
                }
            }
        }
        self.drop_runtime(session_file);
        Self::remove_file_if_present(&canonical_session_file(&work_dir, session_id))?;
        Self::remove_file_if_present(session_file)?;
        self.identities
            .lock()
            .expect("identities poisoned")
            .remove(session_id);
        // Acknowledge on the event stream: clients defer their persisted
        // cleanup (seen watermark, pins) until the delete is confirmed,
        // since a rejection above must never lose that data.
        let _ = self.ingest_tx.send(SessionEvent::SessionRemoved {
            session_id: session_id.to_string(),
            session_file: session_file.to_path_buf(),
        });
        Ok(())
    }

    fn remove_file_if_present(path: &Path) -> Result<(), String> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// The worktree checkout a session owns, from its durable stub facts —
    /// the same resolution the desktop uses for session_worktree_path.
    fn session_worktree_dir(work_dir: &Path, session_id: &str) -> Option<PathBuf> {
        let stub = canonical_session_file(work_dir, session_id);
        let store = threadlane_runtime::harness::JsonlStore::open_read_only(&stub).ok()?;
        let facts = store.facts();
        if !facts
            .get("is_worktree")
            .is_some_and(|value| value == "true")
        {
            return None;
        }
        let canonical_work_dir =
            std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
        Some(crate::discovery::effective_session_work_dir(
            &canonical_work_dir,
            session_id,
            &facts,
        ))
    }

    /// Track (or clear) an in-flight worktree setup so cancel requests land.
    pub fn track_worktree_setup(&self, session_id: &str, setup: Option<WorktreeSetup>) {
        let mut setups = self.worktree_setups.lock().expect("worktree setups poisoned");
        match setup {
            Some(setup) => {
                setups.insert(session_id.to_string(), setup);
            }
            None => {
                setups.remove(session_id);
            }
        }
    }

    /// Stages still advertised for a session's worktree preparation.
    pub fn worktree_setup(&self, session_id: &str) -> Option<WorktreeSetup> {
        self.worktree_setups
            .lock()
            .expect("worktree setups poisoned")
            .get(session_id)
            .cloned()
    }
}

impl std::fmt::Debug for DaemonCore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DaemonCore")
            .field("runtimes", &self.runtimes.lock().map(|map| map.len()))
            .finish_non_exhaustive()
    }
}

/// Run blocking filesystem/Git work on the shared reactor's blocking
/// pool when a Tokio runtime is driving the dispatch (the WebSocket
/// server path), or inline when it is not: an in-process `LocalDaemon`
/// polled from a non-Tokio executor has no runtime to hop onto, and its
/// caller's thread is already a blocking-safe place.
async fn run_blocking_io<R: Send + 'static>(
    work: impl FnOnce() -> R + Send + 'static,
) -> Result<R, String> {
    if tokio::runtime::Handle::try_current().is_ok() {
        crate::chat::executor()?
            .spawn_blocking(work)
            .await
            .map_err(|error| error.to_string())
    } else {
        Ok(work())
    }
}

#[cfg(test)]
mod composer_tests {
    use super::{canonical_session_file, DaemonCore};
    use threadlane_protocol::daemon::{CommandResponse, SessionCommand};
    use threadlane_protocol::{OrchestratorMode, ReasoningEffort};

    #[tokio::test]
    async fn desktop_and_phone_resume_share_one_runtime_owner() {
        use std::sync::Arc;
        use threadlane_protocol::daemon::{HydrationRuntimeOptions, SessionHydrationRequest};

        let project = tempfile::tempdir().unwrap();
        let work_dir = project.path().canonicalize().unwrap();
        let session_id = "shared-resume";
        let session_file = canonical_session_file(&work_dir, session_id);
        let worktree = work_dir.join(".threadlane/worktrees/shared-resume");
        std::fs::create_dir_all(&worktree).unwrap();
        let core = DaemonCore::new().unwrap();
        let options = |runtime_work_dir: std::path::PathBuf| super::coding_agent_options(
            runtime_work_dir, session_file.clone(), "test/model".into(),
            Default::default(), threadlane_protocol::browser::BrowserBridge::unavailable(),
        );
        // Worktree preparation and desktop/mobile hydration can overlap before
        // either publishes its runtime. Construction must be shared at entry.
        let desktop = core.clone().get_or_create_runtime_async(
            session_id.into(),
            work_dir.clone(),
            session_file.clone(),
            options(work_dir.clone()),
            None,
        );
        let worktree_setup = core.clone().get_or_create_runtime_async(
            session_id.into(),
            worktree.clone(),
            session_file.clone(),
            options(worktree.clone()),
            None,
        );
        let desktop = desktop.await.unwrap();
        let worktree_runtime = worktree_setup.await.unwrap();
        assert!(Arc::ptr_eq(&desktop, &worktree_runtime));
        assert_eq!(core.identity(session_id).unwrap().work_dir, work_dir);
        let phone = core
            .clone()
            .get_or_create_runtime_async(
                session_id.into(),
                worktree.clone(),
                session_file.clone(),
                options(worktree),
                None,
            )
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&desktop, &phone));
        let active = std::sync::atomic::AtomicBool::new(false);
        assert!(crate::runtimes::park_prepared_runtime_if_active(
            session_id.into(), desktop.clone(), &active));
        let resumed = core.ensure_runtime(session_id, &work_dir).await.unwrap();
        assert!(crate::runtimes::take_prepared_runtime(session_id).is_none());
        assert!(Arc::ptr_eq(&desktop, &resumed));
        let hydrated = core.hydrate_runtime(&SessionHydrationRequest {
            session_id: session_id.into(), session_file, reload_messages: true,
            runtime_options: Some(HydrationRuntimeOptions {
                work_dir, model: "test/model".into(), model_roles: Default::default(),
            }),
        }).await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&desktop, &hydrated));
        assert_eq!(core.runtimes().len(), 1);
    }

    #[tokio::test]
    async fn registration_does_not_wait_for_unrelated_construction() {
        use std::sync::mpsc;
        use std::time::Duration;

        let project = tempfile::tempdir().unwrap();
        let work_dir = project.path().canonicalize().unwrap();
        let core = DaemonCore::new().unwrap();
        let session_file = work_dir.join("registered.jsonl");
        let runtime = core
            .clone()
            .get_or_create_runtime_async(
                "constructed".into(),
                work_dir.clone(),
                session_file.clone(),
                super::coding_agent_options(
                    work_dir.clone(),
                    session_file,
                    "test/model".into(),
                    Default::default(),
                    threadlane_protocol::browser::BrowserBridge::unavailable(),
                ),
                None,
            )
            .await
            .unwrap();
        let gate = core.construction_gate(&work_dir.join("being-built.jsonl"));
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let gate_thread = std::thread::spawn(move || {
            let _guard = gate.lock().unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        });
        locked_rx.recv().unwrap();
        let (registered_tx, registered_rx) = mpsc::channel();
        let register_core = core.clone();
        let registered_runtime = runtime.clone();
        let registered_file = work_dir.join("unrelated.jsonl");
        let register_thread = std::thread::spawn(move || {
            register_core.register_runtime(
                "unrelated",
                work_dir.clone(),
                registered_file,
                registered_runtime,
            );
            registered_tx.send(()).unwrap();
        });
        registered_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("registration waited for an unrelated construction gate");
        release_tx.send(()).unwrap();
        register_thread.join().unwrap();
        gate_thread.join().unwrap();
    }

    #[tokio::test]
    async fn cancelled_preparation_releases_only_unshared_owner() {
        use std::sync::{Arc, atomic::AtomicBool};
        let project = tempfile::tempdir().unwrap();
        let work_dir = project.path().canonicalize().unwrap();
        let file = canonical_session_file(&work_dir, "cancelled-owner");
        let core = DaemonCore::new().unwrap();
        let runtime = core.clone().get_or_create_runtime_async(
            "cancelled-owner".into(), work_dir.clone(), file.clone(),
            super::coding_agent_options(work_dir.clone(), file.clone(), "test/model".into(),
                Default::default(), threadlane_protocol::browser::BrowserBridge::unavailable()), None,
        ).await.unwrap();
        assert!(core.runtime_construction.lock().unwrap().is_empty());
        let checkout = work_dir.join(".threadlane/worktrees/cancelled-owner");
        core.register_runtime("cancelled-owner", checkout, file.clone(), runtime.clone());
        assert_eq!(core.identity("cancelled-owner").unwrap().work_dir, work_dir);
        let cancelled = AtomicBool::new(false);
        assert!(crate::runtimes::park_prepared_runtime_if_active(
            "cancelled-owner".into(), runtime.clone(), &cancelled));
        let parked = crate::runtimes::cancel_prepared_runtime("cancelled-owner", &cancelled).unwrap();
        assert!(Arc::ptr_eq(&runtime, &parked));
        drop(parked);
        assert!(!crate::runtimes::park_prepared_runtime_if_active(
            "cancelled-owner".into(), runtime.clone(), &cancelled));
        let other_client = runtime.clone();
        core.release_cancelled_runtime(&file, &runtime);
        assert!(core.runtime_for_file(&file).is_some());
        drop(other_client);
        core.release_cancelled_runtime(&file, &runtime);
        assert!(core.runtime_for_file(&file).is_none());
        assert!(crate::runtimes::take_prepared_runtime("cancelled-owner").is_none());
        core.register_runtime("cancelled-owner", work_dir.clone(), file.clone(), runtime.clone());
        let active = AtomicBool::new(false);
        assert!(crate::runtimes::park_prepared_runtime_if_active(
            "cancelled-owner".into(), runtime.clone(), &active));
        core.drop_runtime(&file);
        assert!(crate::runtimes::take_prepared_runtime("cancelled-owner").is_none());

    }

    #[test]
    fn automation_bridge_does_not_retain_its_host_core() {
        let core = DaemonCore::new().unwrap();
        let weak = std::sync::Arc::downgrade(&core);
        drop(core);
        assert!(weak.upgrade().is_none(), "automation bridge retained its host");
    }

    /// Historical worktree hydration resolves the owning project without constructing a runtime.
    #[tokio::test]
    async fn remote_worktree_hydration_keeps_the_canonical_project_without_a_runtime() {
        use threadlane_coding_agent::harness::CodingSessionHarness;
        use threadlane_protocol::daemon::{SessionEvent, SessionHydrationRequest};

        let root = tempfile::tempdir().unwrap();
        let project = root.path().to_path_buf();
        let session_id = "automation-history";
        let worktree = project.join(".threadlane/worktrees").join(session_id);
        let transcript = canonical_session_file(&worktree, session_id);
        let stub = canonical_session_file(&project, session_id);
        for path in [&stub, &transcript] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            for (key, value) in [
                ("name", "Historical automation".to_string()),
                ("is_worktree", "true".to_string()),
                ("worktree_path", worktree.to_string_lossy().into_owned()),
            ] {
                CodingSessionHarness::append_fact_to_path(path, "main", key, &value, None).unwrap();
            }
        }
        let core = DaemonCore::new().unwrap();
        let mut events = core.subscribe();
        core.dispatch(SessionCommand::HydrateSession {
            request: SessionHydrationRequest {
                session_id: session_id.into(),
                session_file: transcript.clone(),
                reload_messages: true,
                runtime_options: None,
            },
        })
        .await
        .unwrap();
        let snapshot = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let SessionEvent::SessionSnapshot {
                    session_id: id,
                    snapshot,
                } = events.recv().await.unwrap().1
                {
                    if id == session_id {
                        break snapshot;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(snapshot.session.work_dir, project);
        assert_eq!(snapshot.session.runtime_work_dir, worktree);
        assert_eq!(snapshot.session.session_file, transcript);
        assert_eq!(core.identity(session_id).unwrap().work_dir, project);
        assert!(core.runtimes().is_empty());
    }

    #[tokio::test]
    async fn drafts_require_attached_projects_and_composer_settings_are_acknowledged() {
        let project = tempfile::tempdir().unwrap();
        let work_dir = project.path().to_path_buf();
        let core = DaemonCore::new().unwrap();
        assert!(core
            .dispatch(SessionCommand::BeginSession {
                work_dir: work_dir.clone()
            })
            .await
            .is_err());
        core.attach_project(work_dir.clone());
        core.seed_config(
            "test/model".into(),
            Default::default(),
            ReasoningEffort::High,
        );
        let CommandResponse::SessionDraft { session } = core
            .dispatch(SessionCommand::BeginSession {
                work_dir: work_dir.clone(),
            })
            .await
            .unwrap()
        else {
            panic!("draft response")
        };
        assert_eq!(
            session.session_file,
            canonical_session_file(&work_dir, &session.id)
        );
        assert!(
            !session.session_file.exists(),
            "a draft must not create a transcript"
        );
        core.dispatch(SessionCommand::SetOrchestratorMode {
            session_id: session.id.clone(),
            mode: OrchestratorMode::Fusion,
        })
        .await
        .unwrap();
        core.dispatch(SessionCommand::SetModel {
            session_id: session.id.clone(),
            model: "test/changed".into(),
        })
        .await
        .unwrap();
        threadlane_coding_agent::harness::CodingSessionHarness::append_fact_to_path(
            &session.session_file,
            "main",
            "model",
            "test/saved",
            None,
        )
        .unwrap();
        core.dispatch(SessionCommand::SetModel {
            session_id: session.id.clone(),
            model: "test/restored".into(),
        })
        .await
        .unwrap();
        let response = core
            .dispatch(SessionCommand::GetComposerOptions {
                work_dir,
                session_id: Some(session.id),
            })
            .await
            .unwrap();
        let CommandResponse::ComposerOptions {
            ref model,
            effort,
            mode,
            ..
        } = response
        else {
            panic!("composer response")
        };
        assert_eq!(model, "test/restored");
        assert_eq!(effort, ReasoningEffort::High);
        assert_eq!(mode, OrchestratorMode::Fusion);
        let encoded = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<CommandResponse>(&encoded).unwrap(),
            response
        );
    }

    #[tokio::test]
    async fn guarded_file_commands_return_typed_point_to_point_replies() {
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join("src.rs");
        std::fs::write(&path, "original").unwrap();
        let core = DaemonCore::new().unwrap();
        let read = core
            .dispatch(SessionCommand::ReadProjectFileVersioned {
                work_dir: project.path().to_path_buf(),
                path: "src.rs".into(),
            })
            .await
            .unwrap();
        let CommandResponse::VersionedFile {
            result: Ok(versioned),
        } = read
        else {
            panic!("typed versioned-file reply")
        };
        assert_eq!(versioned.content, "original");

        let changed = core
            .dispatch(SessionCommand::WriteProjectFileGuarded {
                work_dir: project.path().to_path_buf(),
                path: "src.rs".into(),
                content: "replacement".into(),
                expected_version: "sha256:stale".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            changed,
            CommandResponse::GuardedFileWrite {
                result: Err(threadlane_protocol::daemon::ProjectFileError::Changed),
            }
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");

        let written = core
            .dispatch(SessionCommand::WriteProjectFileGuarded {
                work_dir: project.path().to_path_buf(),
                path: "src.rs".into(),
                content: "replacement".into(),
                expected_version: versioned.version,
            })
            .await
            .unwrap();
        let CommandResponse::GuardedFileWrite { result: Ok(version) } = written else {
            panic!("typed guarded-write reply")
        };
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement");

        std::fs::remove_file(&path).unwrap();
        let deleted = core
            .dispatch(SessionCommand::WriteProjectFileGuarded {
                work_dir: project.path().to_path_buf(),
                path: "src.rs".into(),
                content: "must not recreate".into(),
                expected_version: version,
            })
            .await
            .unwrap();
        assert_eq!(
            deleted,
            CommandResponse::GuardedFileWrite {
                result: Err(threadlane_protocol::daemon::ProjectFileError::Deleted),
            }
        );
        assert!(!path.exists());
    }
}

#[cfg(test)]
mod editor_lsp_tests {
    use super::{parse_editor_server_document_version, DaemonCore};
    use threadlane_protocol::daemon::{CommandResponse, SessionCommand, SessionEvent};
    use threadlane_protocol::editor_lsp::{
        EditorLspOperation, EditorLspPosition, EditorLspRequest, MAX_EDITOR_LSP_BYTES,
    };

    fn request(session_id: &str, work_dir: std::path::PathBuf) -> EditorLspRequest {
        EditorLspRequest {
            session_id: session_id.into(),
            work_dir,
            path: "src/main.rs".into(),
            document_id: 1,
            version: 1,
            expected_runtime_id: None,
            text: "fn main() {}".into(),
            position: EditorLspPosition::default(),
            operation: EditorLspOperation::Hover,
        }
    }

    #[test]
    fn server_document_version_is_a_bounded_integer_and_absent_only_on_close() {
        assert_eq!(
            parse_editor_server_document_version(
                &serde_json::json!({"server_document_version":42}),
                false,
            )
            .unwrap(),
            Some(42)
        );
        assert_eq!(
            parse_editor_server_document_version(
                &serde_json::json!({"server_document_version":null}),
                true,
            )
            .unwrap(),
            None
        );
        assert!(
            parse_editor_server_document_version(
                &serde_json::json!({"server_document_version":2147483648u64}),
                false,
            )
            .is_err()
        );
        assert!(
            parse_editor_server_document_version(
                &serde_json::json!({"server_document_version":1.5}),
                false,
            )
            .is_err()
        );
        assert!(
            parse_editor_server_document_version(
                &serde_json::json!({"server_document_version":null}),
                false,
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn editor_lsp_errors_are_request_scoped_without_journal_events() {
        let project = tempfile::tempdir().unwrap();
        let core = DaemonCore::new().unwrap();
        let response = core
            .dispatch(SessionCommand::EditorLsp {
                request: request("inactive-editor-session", project.path().to_path_buf()),
            })
            .await
            .unwrap();
        assert!(matches!(
            response,
            CommandResponse::EditorLsp {
                result: Err(error)
            } if error == "activate a session in this checkout"
        ));
        assert!(!core.journal_tail().iter().any(|(_, event)| matches!(
            event,
            SessionEvent::DaemonError { message, .. }
                if message == "activate a session in this checkout"
        )));
    }

    #[tokio::test]
    async fn editor_lsp_rejects_oversize_snapshots_before_runtime_lookup() {
        let project = tempfile::tempdir().unwrap();
        let mut request = request("inactive-editor-session", project.path().to_path_buf());
        request.text = "x".repeat(MAX_EDITOR_LSP_BYTES + 1);
        let core = DaemonCore::new().unwrap();
        let error = core.execute_editor_lsp_request(&request).await.unwrap_err();
        assert!(error.contains("1 MiB limit"));
    }

    #[tokio::test]
    async fn editor_lsp_rejects_a_project_root_for_a_worktree_runtime() {
        let project = tempfile::tempdir().unwrap();
        let work_dir = project.path().canonicalize().unwrap();
        let worktree = work_dir.join(".threadlane/worktrees/editor-session");
        std::fs::create_dir_all(&worktree).unwrap();
        let session_file = super::canonical_session_file(&work_dir, "editor-session");
        let core = DaemonCore::new().unwrap();
        let runtime = core
            .clone()
            .get_or_create_runtime_async(
                "editor-session".into(),
                worktree.clone(),
                session_file.clone(),
                super::coding_agent_options(
                    worktree.clone(),
                    session_file,
                    "test/model".into(),
                    Default::default(),
                    threadlane_protocol::browser::BrowserBridge::unavailable(),
                ),
                None,
            )
            .await
            .unwrap();

        let mut stale = request("editor-session", worktree.clone());
        stale.expected_runtime_id = Some(runtime.instance_id().saturating_add(1));
        let error = core.execute_editor_lsp_request(&stale).await.unwrap_err();
        assert_eq!(error, "Editor LSP request targets a stale session runtime");

        let error = core
            .execute_editor_lsp_request(&request("editor-session", work_dir))
            .await
            .unwrap_err();
        assert_eq!(
            error,
            "Editor LSP checkout does not match the active session runtime"
        );
    }

    #[test]
    fn editor_lsp_paths_must_be_confined_relative_paths() {
        let project = tempfile::tempdir().unwrap();
        assert!(
            threadlane_project::files::resolve_project_path(project.path(), "src/main.rs")
                .is_ok()
        );
        assert!(
            threadlane_project::files::resolve_project_path(project.path(), "../outside.rs")
                .is_err()
        );
        assert!(threadlane_project::files::resolve_project_path(project.path(), "").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let outside = tempfile::tempdir().unwrap();
            let outside_file = outside.path().join("source.rs");
            std::fs::write(&outside_file, "fn outside() {}").unwrap();
            symlink(&outside_file, project.path().join("escape.rs")).unwrap();
            assert!(
                threadlane_project::files::resolve_project_path(project.path(), "escape.rs")
                    .is_err()
            );
        }
    }
}
