use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::time::{SystemTime, UNIX_EPOCH};
use threadlane_acp::AcpConfigOption;
use threadlane_protocol::{
    AgentEvent, ImageAttachment, OrchestratorMode, ReasoningEffort, SessionPlan,
    SubagentProgressUpdate,
};
use threadlane_runtime::harness::{EventPayload, HarnessEvent, JsonlStore, SessionStore};

use threadlane_coding_agent::controller::{
    SchedulerSupervisorEvent, SchedulerSupervisorHandle, SessionRuntime,
};
use threadlane_project::load_project_registry;

#[cfg(test)]
use threadlane_protocol::TokenUsage;
use crate::discovery::*;
use crate::projection::*;
use crate::session_snooze::{unix_now, SessionSnooze, SnoozeRecord};
pub use crate::types::*;
use threadlane_runtime::harness::{tool_activity_display_summary, tool_activity_summary};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveCloseWork {
    pub identity: String,
    pub title: String,
    pub project: String,
    pub status: String,
}

pub fn active_automation_status(status: threadlane_automation::RunStatus) -> Option<&'static str> {
    use threadlane_automation::RunStatus;
    match status {
        RunStatus::Starting => Some("Starting"),
        RunStatus::Running => Some("Running"),
        RunStatus::WaitingPermission => Some("Needs permission"),
        RunStatus::WaitingAnswer => Some("Needs an answer"),
        RunStatus::Queued | RunStatus::Succeeded | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Interrupted => None,
    }
}

pub fn close_work_needs_refresh(current: &[String], disclosed: &[String]) -> bool {
    current.iter().any(|identity| !disclosed.contains(identity))
}

struct CompletionAlertContext {
    client: Arc<dyn threadlane_client::DaemonClient>,
    connection_epoch: u64,
    inventory_generation: u64,
    remote: bool,
    connected: bool,
}

pub struct AppState {
    pub completion_alerts: crate::completion_alerts::CompletionAlerts,
    pub completion_alerts_loaded: bool,
    pub completion_alerts_saving: bool,
    pub completion_alerts_error: Option<String>,
    pub completion_alerts_retry: Option<bool>,
    pub completion_alert_test_requested: bool,
    completion_alert_context: Option<CompletionAlertContext>,
    pub client: threadlane_client::ClientState,
    pub automation_service: Option<Arc<crate::automation::AutomationService>>,
    pub automations: crate::automation::Projection,
    pub is_new_task: bool,
    pub draft_work_mode: WorkMode,
    pub draft_worktree_base: Option<String>,
    pub draft_worktree_bases: Vec<String>,
    worktree_setups: HashMap<String, crate::worktree_setup::WorktreeSetup>,
    pub(crate) available_models: Vec<threadlane_daemon::catalog::ModelOption>,
    composer_text: String,
    /// Bumped whenever an out-of-band mutation (issue create/close, label
    /// edit) changes GitHub list contents. The GitHub view observes this
    /// and refetches; dialogs cannot reach the view entity directly.
    pub github_list_revision: u64,
    /// Composer inserts requested from other surfaces (browser annotations).
    /// The chat view drains these into the composer input on its next model
    /// observation, preserving whatever the user already typed.
    pub requested_composer_inserts: Vec<RequestedComposerInsert>,
    trajectory_by_session: HashMap<SessionProjectionKey, Vec<TrajectoryEntry>>,
    subagents_by_session: HashMap<SessionProjectionKey, Vec<SubagentActivityInfo>>,
    trajectory_revision: u64,
    trajectory_epoch: u64,
    diagnostics_revision: u64,
    diagnostics_by_session:
        HashMap<SessionProjectionKey, threadlane_runtime::harness::SessionDiagnostics>,
    token_efficiency_by_session:
        HashMap<SessionProjectionKey, threadlane_runtime::harness::TokenEfficiencyReport>,
    /// Settings each ACP session's agent exposes, keyed by session id.
    ///
    /// Keyed by session rather than by model id because two sessions on the
    /// same configured agent can hold different settings.
    acp_config_options: HashMap<SessionProjectionKey, Vec<AcpConfigOption>>,
    /// Pending ACP `config_id -> value` selections made before a session
    /// exists (New task), keyed by agent id.
    ///
    /// The picker is usable before the first turn, but applying a setting
    /// requires a live agent runtime. Selections made there are stored here,
    /// shown optimistically via the launch-time cache, and applied to the
    /// next runtime before its first turn.
    pending_acp_config: HashMap<String, HashMap<String, String>>,
    stashed_prompts: HashMap<String, String>,
    pub pending_hydrations: Vec<SessionHydrationRequest>,
    in_flight_hydrations: HashMap<SessionProjectionKey, usize>,
    /// Live lifecycle status seen since remote selection, scoped to the
    /// connection epoch so reconnect snapshots can restore missed changes.
    remote_live_status: Option<(SessionProjectionKey, u64)>,
    /// Project metadata received in the current transport epoch. A connected
    /// socket alone does not mean its cached inventory is safe to navigate.
    remote_inventory: Option<(Arc<dyn threadlane_client::DaemonClient>, u64, HashSet<PathBuf>)>,
    pub git_statuses: HashMap<PathBuf, threadlane_git::GitStatus>,
    pub git_prs: HashMap<(PathBuf, String), Option<threadlane_git::GitHubPrInfo>>,
    pub auto_address_pr_reviews_enabled: bool,
    /// Persistent PR review tracking per project, loaded on demand and cached.
    pub(crate) pr_review_tracking: HashMap<PathBuf, threadlane_git::PrReviewTrackingStore>,

    pub selected_model: String,
    model_roles: threadlane_runtime::ModelRoles,
    pub reasoning_effort: ReasoningEffort,
    /// Session orchestration mode shown in the composer Mode dropdown.
    /// Persisted per project in `.threadlane/subagents.json`; the live
    /// session runtime reads it when it is (re)built.
    pub orchestrator_mode: OrchestratorMode,
    pub workspace_page: WorkspacePage,
    /// GitHub navigation shared by the sidebar and the GitHub screen.
    pub github_tab: GitHubTab,
    pub openai_key: String,
    pub opencode_key: String,
    pub auth_status_msg: Option<String>,
    pub update_status: threadlane_updater::UpdateStatus,
    pub requested_editor_target: Option<RequestedEditorTarget>,
    /// Files-panel "Open in Panel" requests as `(project, relative_path)`;
    /// `RightPanelView` takes the request and opens the editable document.
    pub requested_panel_document: Option<(PathBuf, String)>,
    pub requested_github_issue: Option<(PathBuf, u64)>,
    pub requested_composer_prompt: Option<String>,
    pub requested_terminal_command: Option<String>,
    pub requested_terminal_work_dir: Option<PathBuf>,
    stream_tx: tokio::sync::mpsc::UnboundedSender<SessionEvent>,
    pub stream_rx: Option<tokio::sync::mpsc::UnboundedReceiver<SessionEvent>>,
    session_refresh_tx: Sender<(u64, PathBuf)>,
    pub session_refresh_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<(u64, PathBuf, Vec<SessionInfo>)>>,
    /// Monotonic generation for session discovery refreshes. Bumped whenever
    /// `recreate_active_worktree` replaces a checkout so late results captured
    /// before the recreation cannot overwrite the fresh sessions.
    session_refresh_generation: u64,
    /// Daemon-owned session state (runtimes, identities, setup tracking,
    /// event journal) embedded in-process via `LocalDaemon`. In remote mode
    /// it stays empty — sessions live in the attached daemon process.
    pub daemon_core: Arc<threadlane_daemon::core::DaemonCore>,
    /// The `SessionCommand`/`SessionEvent` boundary the UI talks through:
    /// `LocalDaemon` in-process by default, `RemoteDaemon` over WebSocket
    /// when `THREADLANE_DAEMON_URL` is set.
    pub daemon_client: Arc<dyn threadlane_client::DaemonClient>,
    /// True when `daemon_client` is remote: runtime handles are then
    /// process-remote and only the command/event surface can reach them.
    pub daemon_remote: bool,
    /// Live LAN pairing listener while "share with mobile" is on; dropping
    /// it disconnects every attached thin client.
    pub pairing: Option<threadlane_daemon::pairing::PairingServer>,
    /// The last async pairing-start failure, shown by the pairing dialog.
    pub pairing_error: Option<String>,
    /// Pairing startup/restore in progress, independent of any dialog view.
    pub pairing_starting: bool,
    pairing_generation: u64,
    pairing_restore_allowed: bool,
    pairing_remove_all_pending: bool,
    /// Ordered channel for `SessionCommand::Terminal*` commands — they must
    /// not reorder relative to each other (TerminalOpen before its Input),
    /// so they go through one forward loop rather than a task per command.
    terminal_command_tx: tokio::sync::mpsc::UnboundedSender<SessionCommand>,
    /// Daemon-hosted PTY frames routed by terminal_id to terminal views.
    terminal_event_tx: tokio::sync::broadcast::Sender<TerminalEvent>,
    /// Remote deletes awaiting the daemon's `SessionRemoved` ack:
    /// `session_id` → project dir. Persisted cleanup (seen watermark,
    /// pins, pending prompts) runs only on the ack — a rejected delete
    /// must leave that data intact when the session row returns.
    pending_remote_deletes: HashMap<String, PathBuf>,
    /// Queued-message cancels awaiting the daemon's confirmation:
    /// request_id → intent. `drain_chat_stream` resolves them when the
    /// `CommandResult` reply (or the journaled `QueuedEntryCancelled`)
    /// lands; the echo row stays until then so the UI never claims a
    /// removal the peer may not have performed.
    pub(crate) pending_queued_cancels: HashMap<u64, PendingQueuedCancel>,
    scheduler_handles: HashMap<PathBuf, (std::sync::Weak<SessionRuntime>, SchedulerSupervisorHandle)>,
    scheduler_results:
        HashMap<PathBuf, tokio::sync::mpsc::UnboundedReceiver<SchedulerSupervisorEvent>>,
    deferred_stream_events: HashMap<String, Vec<SessionEvent>>,
    /// Bridge to the embedded browser panel. The channel is created with the
    /// app; the first constructed right panel claims the receiver and pumps
    /// agent browser commands into the live view.
    pub browser_bridge: threadlane_protocol::browser::BrowserBridge,
    /// Whether the computer-use mirror popup is currently open. Set when the
    /// popup opens and cleared by its close button; guards duplicate popups.
    pub mirror_open: bool,
    /// Seen computer trigger ids (permission requests and tool activities)
    /// so the mirror opens once per new activity, not per pump tick.
    mirror_seen: HashSet<String>,
    /// Per-project `session_seen.json` stores — the acknowledged newest-run
    /// watermarks behind the sidebar "New result" marker. Keyed by canonical
    /// project dir so a worktree transcript's relocation never loses it.
    session_seen: HashMap<PathBuf, crate::session_seen::SessionSeenStore>,
    /// Single serialized background writer for every session_seen file.
    session_seen_writer: crate::session_seen::SessionSeenWriter,
    /// Per-project `session_snooze.json` stores — the sidebar Snoozed
    /// group's absolute-deadline records. Same canonical-project keying as
    /// the seen stores so a worktree session and its stub share a snooze.
    session_snooze: HashMap<PathBuf, crate::session_snooze::SessionSnoozeStore>,
    /// Dedicated serialized writer for snooze files — same machinery as
    /// `session_seen_writer`, kept as a second instance so result channels
    /// stay unambiguous.
    session_snooze_writer: crate::session_snooze::SessionSnoozeWriter,
    /// Completion token captured before the active session's latest
    /// transcript load. Cleared when acknowledged by the chat surface and
    /// replaced by each applied load; never populated by a failed load.
    presented_completion: Option<(SessionProjectionKey, RunCompletionToken)>,
    /// One-shot flag so a failed session_seen write surfaces exactly once.
    session_seen_save_failed: bool,
    /// One-shot flag so a failed session_snooze write surfaces exactly
    /// once; the affected row stays unhidden and unsaved.
    session_snooze_save_failed: bool,
    /// False until the first automation projection has been applied — its
    /// runs are restored history and must not be registered as new in-app
    /// sessions.
    automation_runs_restored: bool,
}

// Existing desktop feature APIs retain field access while the session views move.
// The fields live only in ClientState; desktop services remain in AppState.
impl std::ops::Deref for AppState {
    type Target = threadlane_client::ClientState;
    fn deref(&self) -> &Self::Target { &self.client }
}
impl std::ops::DerefMut for AppState {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.client }
}

/// A queued-message cancel awaiting its `CommandResult` reply (or the
/// journaled `QueuedEntryCancelled`): which echo it belongs to, whether
/// the staged content goes back into the composer (edit) or is discarded
/// (plain remove), and whether the echo already put the staged text back
/// in the composer (then the reply only needs to deliver the images the
/// echo could not carry). `work_dir` pins the project that queued the
/// message so the restored payload is scoped to `(work_dir, session_id)`,
/// matching how composer drafts are keyed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingQueuedCancel {
    pub session_id: String,
    pub work_dir: Option<PathBuf>,
    pub entry_id: String,
    pub restore: bool,
    pub text_restored: bool,
}

/// Handle terminal views use to reach daemon-hosted PTYs. `command()`
/// forwards `SessionCommand::Terminal*` to the attached daemon in send
/// order; `events()` yields the frames the daemon streams back, routed by
/// `terminal_id`. The same surface serves the embedded core and a remote
/// daemon.
#[derive(Clone)]
pub struct TerminalBus {
    commands: tokio::sync::mpsc::UnboundedSender<SessionCommand>,
    events: tokio::sync::broadcast::Sender<TerminalEvent>,
}

impl TerminalBus {
    pub fn command(&self, command: SessionCommand) {
        let _ = self.commands.send(command);
    }

    pub fn events(&self) -> tokio::sync::broadcast::Receiver<TerminalEvent> {
        self.events.subscribe()
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::load()
    }
}

fn persist_pinned_sessions(work_dir: &Path, pinned_ids: &[&str]) -> Result<(), String> {
    let threadlane_dir = work_dir.join(".threadlane");
    std::fs::create_dir_all(&threadlane_dir).map_err(|e| e.to_string())?;
    let path = threadlane_dir.join("pinned_sessions.json");
    let temporary_path = threadlane_dir.join(format!("pinned_sessions.{}.tmp", std::process::id()));
    let json = serde_json::to_string_pretty(pinned_ids).map_err(|e| e.to_string())?;
    std::fs::write(&temporary_path, json).map_err(|e| e.to_string())?;
    std::fs::rename(temporary_path, path).map_err(|e| e.to_string())
}

fn load_pinned_sessions_from_dir(work_dir: &Path, pins: &mut HashSet<(PathBuf, String)>) {
    let pinned_file = work_dir.join(".threadlane/pinned_sessions.json");
    if let Ok(content) = std::fs::read_to_string(&pinned_file) {
        if let Ok(ids) = serde_json::from_str::<Vec<String>>(&content) {
            pins.retain(|(w, _)| w != work_dir);
            for id in ids {
                pins.insert((work_dir.to_path_buf(), id));
            }
        }
    }
}

impl AppState {
    pub(crate) fn load_pinned_sessions(&mut self, work_dir: &Path) {
        load_pinned_sessions_from_dir(work_dir, &mut self.client.pinned_sessions);
    }

    pub fn toggle_pinned_session(
        &mut self,
        work_dir: PathBuf,
        session_id: String,
    ) -> Result<(), String> {
        let key = (work_dir.clone(), session_id.clone());
        let is_currently_pinned = self.client.pinned_sessions.contains(&key);
        let project_pinned: Vec<&str> = if is_currently_pinned {
            self.client.pinned_sessions
                .iter()
                .filter(|(w, id)| *w == work_dir && id.as_str() != session_id)
                .map(|(_, id)| id.as_str())
                .collect()
        } else {
            self.client.pinned_sessions
                .iter()
                .filter(|(w, _)| *w == work_dir)
                .map(|(_, id)| id.as_str())
                .chain(std::iter::once(session_id.as_str()))
                .collect()
        };

        persist_pinned_sessions(&work_dir, &project_pinned)?;

        if is_currently_pinned {
            self.client.pinned_sessions.remove(&key);
        } else {
            self.client.pinned_sessions.insert(key);
        }
        Ok(())
    }

    pub fn is_session_pinned(&self, work_dir: &Path, session_id: &str) -> bool {
        self.client.pinned_sessions
            .contains(&(work_dir.to_path_buf(), session_id.to_string()))
    }

    /// Store key for `session_seen` maps: the canonical project dir so a
    /// worktree session and its stub share one acknowledgment watermark.
    fn session_seen_key(work_dir: &Path) -> PathBuf {
        std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf())
    }

    fn session_seen_store_for(
        &mut self,
        work_dir: &Path,
    ) -> &mut crate::session_seen::SessionSeenStore {
        let key = Self::session_seen_key(work_dir);
        self.session_seen
            .entry(key)
            .or_insert_with(|| crate::session_seen::SessionSeenStore::load(work_dir))
    }

    /// Hands any dirty store for `work_dir` to the serialized writer. A
    /// dead writer re-marks the store so the next mutation retries instead
    /// of spinning per frame.
    fn flush_session_seen(&mut self, work_dir: &Path) {
        let key = Self::session_seen_key(work_dir);
        let Some(store) = self.session_seen.get_mut(&key) else {
            return;
        };
        let Some(json) = store.take_dirty_json() else {
            return;
        };
        let path = store.path().to_path_buf();
        if !self.session_seen_writer.submit(key.clone(), path, 0, json) {
            tracing::warn!("session_seen writer is gone; keeping {} dirty", key.display());
            if let Some(store) = self.session_seen.get_mut(&key) {
                store.mark_dirty();
            }
        }
    }

    /// True while a session carries a confirmed successful main-lane Run
    /// completion newer than its acknowledged watermark — the sidebar "New
    /// result" marker.
    pub fn session_has_unseen_result(&self, session: &SessionInfo) -> bool {
        // A project whose store has not loaded yet cannot claim unseen
        // results — baselining happens with the next successful discovery.
        self.session_seen
            .get(&Self::session_seen_key(&session.work_dir))
            .is_some_and(|store| store.has_unseen(session))
    }

    /// Registers a session created in-app (before its first prompt runs) so
    /// discovery's baseline cannot retroactively mark its first result seen.
    pub fn register_session_seen(&mut self, work_dir: &Path, session_id: &str) {
        self.session_seen_store_for(work_dir).register(session_id);
        self.flush_session_seen(work_dir);
    }

    pub fn sync_completion_alert_context(&mut self) {
        let epoch = self.daemon_client.file_search_connection_epoch();
        let connected = self.daemon_client.is_connected();
        let connection_changed = self.completion_alert_context.as_ref().is_some_and(|previous| {
            !Arc::ptr_eq(&previous.client, &self.daemon_client)
                || previous.connection_epoch != epoch
                || previous.remote != self.daemon_remote
                || previous.connected != connected
        });
        if connection_changed {
            self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
        }
        let changed = self.completion_alert_context.as_ref().is_none_or(|previous| {
            !Arc::ptr_eq(&previous.client, &self.daemon_client)
                || previous.connection_epoch != epoch
                || previous.inventory_generation != self.session_refresh_generation
                || previous.remote != self.daemon_remote
                || previous.connected != connected
        });
        if changed {
            self.completion_alerts.reset();
            self.completion_alert_context = Some(CompletionAlertContext {
                client: self.daemon_client.clone(),
                connection_epoch: epoch,
                inventory_generation: self.session_refresh_generation,
                remote: self.daemon_remote,
                connected,
            });
            if connection_changed && connected && !self.daemon_remote && self.completion_alerts.is_enabled() {
                for project in &self.client.projects { self.request_session_refresh(&project.work_dir); }
            }
        }
        self.completion_alerts.retain_projects(&self.client.projects.iter().map(|project| project.work_dir.clone()).collect());
    }

    pub fn set_completion_alerts_enabled(&mut self, enabled: bool) {
        let changed = self.completion_alerts.is_enabled() != enabled;
        if changed && enabled {
            self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
        }
        self.sync_completion_alert_context();
        self.completion_alerts.set_enabled(enabled);
        self.completion_alert_test_requested = false;
        if enabled && changed && !self.daemon_remote {
            for project in &self.client.projects { self.request_session_refresh(&project.work_dir); }
        }
    }

    fn register_interactive_completion_alert(&mut self, project: &Path, id: &str) {
        self.sync_completion_alert_context();
        if !self.daemon_remote {
            self.completion_alerts.register(crate::completion_alerts::CompletionAlertIdentity {
                project: project.into(), session_id: id.into(), session_file: canonical_session_file(project, id),
            });
        }
    }

    /// First-confirmed-discovery baseline: sessions the store has no record
    /// of inherit their current completion as already seen. `Unknown`
    /// summaries and already-tracked sessions are left alone, so a transient
    /// discovery failure or stub parse never resets a watermark.
    fn baseline_session_seen(&mut self, sessions: &[SessionInfo]) {
        let mut dirty_dirs = Vec::new();
        for session in sessions {
            if session.completion_summary == SessionCompletionSummary::Unknown {
                continue;
            }
            let store = self.session_seen_store_for(&session.work_dir);
            if store.is_tracked(&session.id) {
                continue;
            }
            store.baseline(session);
            dirty_dirs.push(session.work_dir.clone());
        }
        for work_dir in dirty_dirs {
            self.flush_session_seen(&work_dir);
        }
    }

    /// Acknowledges the completion token the active session's last applied
    /// transcript load captured — invoked by the chat surface only once the
    /// transcript is presented at its tail in the foreground window.
    /// Session-id + session-file guards keep a stale presentation from
    /// acknowledging a different session's token; the write is journal-seq
    /// monotonic, so a completion newer than the presented one can never be
    /// acknowledged by it.
    pub fn acknowledge_presented_completion(&mut self) -> bool {
        let Some((key, token)) = self.presented_completion.clone() else {
            return false;
        };
        if self.client.active_session_id.as_deref() != Some(key.session_id.as_str()) {
            return false;
        }
        let Some(session) = self.active_session_info() else {
            return false;
        };
        if session.session_file != key.session_file {
            return false;
        }
        let work_dir = session.work_dir.clone();
        let advanced = self
            .session_seen_store_for(&work_dir)
            .acknowledge(&key.session_id, &token);
        if advanced {
            self.flush_session_seen(&work_dir);
        }
        // The presented token is consumed whether it advanced the watermark
        // or was already covered by a newer one; a later applied load
        // replaces it.
        self.presented_completion = None;
        advanced
    }

    /// The store map key for a project. `session_seen_key` canonicalizes
    /// when the directory exists, but a store created while the project
    /// dir was missing lives under the raw path — honor whichever key an
    /// existing store uses so a later canonicalization cannot fork it.
    fn session_snooze_key(&self, work_dir: &Path) -> PathBuf {
        let key = Self::session_seen_key(work_dir);
        if self.session_snooze.contains_key(work_dir)
            && !self.session_snooze.contains_key(&key)
        {
            work_dir.to_path_buf()
        } else {
            key
        }
    }

    fn session_snooze_store_for(
        &mut self,
        work_dir: &Path,
    ) -> &mut crate::session_snooze::SessionSnoozeStore {
        let key = self.session_snooze_key(work_dir);
        self.session_snooze
            .entry(key)
            .or_insert_with(|| crate::session_snooze::SessionSnoozeStore::load(work_dir, unix_now()))
    }

    /// Hands any dirty snooze store for `work_dir` to the serialized
    /// writer. The revision captured by `take_dirty_json` rides the job so a
    /// stale acknowledgment can never confirm a newer record.
    fn flush_session_snooze(&mut self, work_dir: &Path) {
        let key = self.session_snooze_key(work_dir);
        let Some(store) = self.session_snooze.get_mut(&key) else {
            return;
        };
        let Some((json, revision)) = store.take_dirty_json() else {
            return;
        };
        let path = store.path().to_path_buf();
        if !self
            .session_snooze_writer
            .submit(key.clone(), path, revision, json)
        {
            tracing::warn!("session_snooze writer is gone; keeping {} dirty", key.display());
            if let Some(store) = self.session_snooze.get_mut(&key) {
                store.write_submit_failed(revision);
            }
        }
    }

    fn session_for_store_key(&self, key: &Path, session_id: &str) -> Option<&SessionInfo> {
        self.client.projects
            .iter()
            .flat_map(|project| project.sessions.iter())
            .find(|session| {
                session.id == session_id
                    && (Self::session_seen_key(&session.work_dir) == key
                        || session.work_dir == *key)
            })
    }

    /// Whether a confirmed snooze ended: new Working/Needs you state, an
    /// active scheduled run, or a confirmed completion newer than the
    /// captured baseline. `Unknown`/`None` summaries are not new work — a
    /// transient parse failure or Git refresh never ends a snooze.
    fn snooze_ended_for(&self, session: &SessionInfo, record: &SnoozeRecord) -> bool {
        if matches!(
            self.session_attention(session),
            SessionAttention::Working | SessionAttention::NeedsYou
        ) {
            return true;
        }
        if self
            .daemon_core
            .runtime_for_file(&session.session_file)
            .is_some_and(|runtime| runtime.scheduled_work_active())
        {
            return true;
        }
        match &session.completion_summary {
            SessionCompletionSummary::Latest(token) => match &record.baseline {
                Some(baseline) => token.seq > baseline.seq,
                None => true,
            },
            _ => false,
        }
    }

    /// The row-facing snooze state. Fail-open: expired or unwritten
    /// (pending-hidden ineligible) metadata never hides a session —
    /// pending records report `pending: true` and the row stays put.
    pub fn session_snooze(&self, work_dir: &Path, session_id: &str) -> Option<SessionSnooze> {
        let store = self.session_snooze.get(&self.session_snooze_key(work_dir))?;
        let (record, pending) = store.record(session_id)?;
        if record.wake_at <= unix_now() {
            return None;
        }
        Some(SessionSnooze {
            wake_at: record.wake_at,
            pending,
            save_failed: store.save_failed(),
        })
    }

    /// Whether a new snooze may be recorded for this session. Only
    /// confirmed Ready/Idle local sessions qualify; the `Err` text is the
    /// reason shown by the disabled menu item.
    pub fn session_snooze_eligibility(&self, session: &SessionInfo) -> Result<(), String> {
        if self.daemon_remote {
            return Err("Snooze is local-only for now".into());
        }
        if self.worktree_setups.contains_key(&session.id) {
            return Err("Session is preparing its worktree".into());
        }
        if session.completion_summary == SessionCompletionSummary::Unknown {
            return Err("Session state is still loading".into());
        }
        match self.session_attention(session) {
            SessionAttention::Working => return Err("Session is still working".into()),
            SessionAttention::NeedsYou => return Err("Session needs you".into()),
            _ => {}
        }
        if self
            .daemon_core
            .runtime_for_file(&session.session_file)
            .is_some_and(|runtime| runtime.scheduled_work_active())
        {
            return Err("A scheduled run is active".into());
        }
        Ok(())
    }

    /// Records a snooze after revalidating the exact project/session: the
    /// deadline is computed now (elapsed hours survive sleep and restart)
    /// and the completion baseline is captured so newer work ends it.
    pub fn snooze_session(
        &mut self,
        work_dir: &Path,
        session_id: &str,
        duration_secs: u64,
    ) -> Result<(), String> {
        let session = self.client.projects
            .iter()
            .flat_map(|project| project.sessions.iter())
            .find(|session| session.id == session_id && session.work_dir == work_dir)
            .cloned()
            .ok_or_else(|| "Session was not found".to_string())?;
        self.session_snooze_eligibility(&session)?;
        let baseline = match &session.completion_summary {
            SessionCompletionSummary::Latest(token) => Some(token.clone()),
            _ => None,
        };
        let record = SnoozeRecord {
            wake_at: unix_now() + duration_secs,
            baseline,
        };
        self.session_snooze_store_for(work_dir)
            .snooze(session_id, record);
        self.flush_session_snooze(work_dir);
        Ok(())
    }

    /// Unsnooze is fail-open: the record disappears immediately and the row
    /// rejoins its pin/attention/date grouping; the deletion persists in
    /// the background.
    pub fn unsnooze_session(&mut self, work_dir: &Path, session_id: &str) {
        if self
            .session_snooze_store_for(work_dir)
            .remove(session_id)
        {
            self.flush_session_snooze(work_dir);
        }
    }

    /// Retries persistence for a record whose save failed — the same
    /// deadline and baseline, never a recomputed snooze.
    pub fn retry_snooze_save(&mut self, work_dir: &Path, session_id: &str) {
        if self
            .session_snooze
            .get(&self.session_snooze_key(work_dir))
            .is_some_and(|store| store.record(session_id).is_some())
        {
            self.flush_session_snooze(work_dir);
        }
    }

    /// Drops records that expired or ended: deadline passed, new
    /// Working/Needs you state, or a confirmed completion newer than the
    /// captured baseline. Sessions missing from discovery keep their
    /// records — only confirmed removal prunes them. Returns true when
    /// visible state changed.
    pub fn reconcile_session_snoozes(&mut self) -> bool {
        let now = unix_now();
        let mut changed = false;
        let keys: Vec<PathBuf> = self.session_snooze.keys().cloned().collect();
        for key in keys {
            let mut dropped = Vec::new();
            if let Some(store) = self.session_snooze.get(&key) {
                for (session_id, record) in store.records() {
                    let drop = record.wake_at <= now
                        || self
                            .session_for_store_key(&key, session_id)
                            .is_some_and(|session| self.snooze_ended_for(session, record));
                    if drop {
                        dropped.push(session_id.clone());
                    }
                }
            }
            if dropped.is_empty() {
                continue;
            }
            let Some(store) = self.session_snooze.get_mut(&key) else {
                continue;
            };
            for session_id in dropped {
                store.remove(&session_id);
            }
            let work_dir = store.work_dir().to_path_buf();
            self.flush_session_snooze(&work_dir);
            changed = true;
        }
        changed
    }

    /// Earliest snooze deadline across every project's store — the
    /// sidebar's single wake-up task re-arms against it.
    pub fn next_snooze_deadline(&self) -> Option<u64> {
        self.session_snooze
            .values()
            .filter_map(crate::session_snooze::SessionSnoozeStore::next_deadline)
            .min()
    }

    /// Every snooze record keyed for fingerprinting: `(work_dir,
    /// session_id, wake_at, pending, save_failed)` — a change to any of
    /// them rebuilds the sidebar rows.
    pub fn session_snooze_entries(&self) -> Vec<(PathBuf, String, u64, bool, bool)> {
        let mut entries = Vec::new();
        for store in self.session_snooze.values() {
            for (session_id, record) in store.records() {
                entries.push((
                    store.work_dir().to_path_buf(),
                    session_id.clone(),
                    record.wake_at,
                    store
                        .record(session_id)
                        .is_some_and(|(_, pending)| pending),
                    store.save_failed(),
                ));
            }
        }
        entries.sort();
        entries
    }

    /// Observes serialized session_snooze writes: a confirmation may let
    /// pending records hide, a failure leaves them pending (unhidden) and
    /// surfaces once, and either way the acknowledged state is reconciled
    /// so a late ack never re-hides a session whose work resumed.
    pub fn drain_session_snooze_write_results(&mut self) -> bool {
        let mut changed = false;
        while let Some(result) = self.session_snooze_writer.try_recv_result() {
            // Match on the submitted key verbatim — it is already the store
            // map key. Re-canonicalizing here can produce a different path
            // when the write itself just created the directory.
            let Some(store) = self.session_snooze.get_mut(&result.work_dir) else {
                continue;
            };
            match store.apply_write_result(result.generation, result.error.clone()) {
                crate::session_snooze::SnoozeWriteOutcome::Confirmed => {
                    if self.session_snooze_save_failed {
                        self.session_snooze_save_failed = false;
                    }
                    changed = true;
                }
                crate::session_snooze::SnoozeWriteOutcome::Stale => {}
                crate::session_snooze::SnoozeWriteOutcome::Failed => {
                    if !self.session_snooze_save_failed {
                        self.session_snooze_save_failed = true;
                        self.client.session_status = Some(
                            "Could not save snooze — the session stays visible. Retry from its actions menu."
                                .into(),
                        );
                    }
                    changed = true;
                }
            }
        }
        // A failed deletion leaves no row behind to offer a retry — the
        // drained-or-idle loop resubmits it automatically, or the stale
        // on-disk record would hide the session again on the next load.
        // Stores that still hold records surface the failure on a visible
        // row, whose menu retry rewrites the whole file anyway.
        let orphaned_keys: Vec<PathBuf> = self
            .session_snooze
            .iter()
            .filter(|(_, store)| store.failed_delete_dirty())
            .map(|(key, _)| key.clone())
            .collect();
        for key in orphaned_keys {
            self.flush_session_snooze(&key);
            changed = true;
        }
        if changed {
            changed |= self.reconcile_session_snoozes();
        }
        changed
    }

    /// Canonical project dir owning `session_file` — the sessions list when
    /// known, else the `.threadlane/sessions/<file>` parent chain.
    fn session_work_dir_for_file(&self, session_file: &Path) -> Option<PathBuf> {
        self.client.projects
            .iter()
            .flat_map(|project| project.sessions.iter())
            .find(|session| session.session_file == session_file)
            .map(|session| session.work_dir.clone())
            .or_else(|| {
                session_file
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
            })
    }

    pub fn issue_branch_name(number: u64, title: &str, suffix: &str) -> String {
        threadlane_protocol::repo::issue_branch_name(number, title, suffix)
    }

    pub fn load() -> Self {
        Self::load_from_registry(load_project_registry())
    }

    pub fn active_git_work_dir(&self) -> Option<PathBuf> {
        let work_dir = self.client.active_work_dir.as_ref()?;
        let Some(session_id) = self.client.active_session_id.as_ref() else {
            return Some(work_dir.clone());
        };
        let session = self.client.projects
            .iter()
            .find(|project| project.work_dir == *work_dir)
            .and_then(|project| {
                project
                    .sessions
                    .iter()
                    .find(|session| session.id == *session_id)
            });

        match session {
            Some(session) if session.worktree_available => Some(session.runtime_work_dir.clone()),
            Some(_) => None,
            None => None,
        }
    }

    pub fn active_worktree_unavailable(&self) -> bool {
        if self.active_worktree_setup().is_some() {
            return false;
        }
        let (Some(work_dir), Some(session_id)) = (
            self.client.active_work_dir.as_ref(),
            self.client.active_session_id.as_ref(),
        ) else {
            return false;
        };
        self.client.projects
            .iter()
            .find(|project| project.work_dir == *work_dir)
            .and_then(|project| {
                project
                    .sessions
                    .iter()
                    .find(|session| session.id == *session_id)
            })
            .is_some_and(|session| session.is_worktree && !session.worktree_available)
    }

    /// Canonical attached-project directory used as the terminal group key.
    /// Shells for every session (including worktree sessions) in one project
    /// share a group so switching sessions retains running terminals; the
    /// per-session checkout from `active_git_work_dir` is only the cwd for
    /// newly created shells.
    pub fn terminal_group_key(&self) -> Option<PathBuf> {
        self.client.active_work_dir.clone()
    }

    pub(crate) fn load_from_registry(registry_projects: Vec<AttachedProject>) -> Self {
        Self::load_from_registry_inner(registry_projects, true)
    }

    fn load_from_registry_inner(
        registry_projects: Vec<AttachedProject>,
        load_host_state: bool,
    ) -> Self {
        #[cfg(not(test))]
        let mut registry_projects = registry_projects;
        #[cfg(not(test))]
        registry_projects.retain(|project| is_attachable_project_root(&project.path));
        #[cfg(not(test))]
        if load_host_state
            && registry_projects.is_empty()
            && !threadlane_project::global_threadlane_dir()
                .join("projects.json")
                .exists()
        {
            if let Ok(curr) = std::env::current_dir().and_then(std::fs::canonicalize) {
                if is_attachable_project_root(&curr) {
                    let project = AttachedProject::from_path(curr);
                    registry_projects.push(project.clone());
                    if let Err(error) =
                        threadlane_project::save_project_registry(&registry_projects)
                    {
                        tracing::warn!("failed to persist project registry: {error}");
                    }
                }
            }
        }

        let mut project_infos = Vec::new();
        let mut active_work_dir = None;
        let mut active_session_id = None;
        let mut active_session_file = None;
        let mut active_runtime_work_dir = None;
        let mut active_project_index = 0;
        for index in 1..registry_projects.len() {
            if registry_projects[index].last_opened_at
                > registry_projects[active_project_index].last_opened_at
            {
                active_project_index = index;
            }
        }

        let mut pinned_sessions = HashSet::new();
        for p in &registry_projects {
            load_pinned_sessions_from_dir(&p.path, &mut pinned_sessions);
        }

        for (i, p) in registry_projects.iter().enumerate() {
            let sessions = discover_session_stubs_in_project(&p.path);
            let is_active = i == active_project_index;

            if is_active {
                active_work_dir = Some(p.path.clone());
                if let Some(target_session) = p
                    .last_session_id
                    .as_deref()
                    .and_then(|id| sessions.iter().find(|s| s.id == id))
                    .or_else(|| sessions.first())
                {
                    active_session_id = Some(target_session.id.clone());
                    active_session_file = Some(target_session.session_file.clone());
                    active_runtime_work_dir = Some(target_session.runtime_work_dir.clone());
                }
            }

            project_infos.push(ProjectInfo {
                name: p.name.clone(),
                work_dir: p.path.clone(),
                sessions,
                is_expanded: true,
            });
        }
        for project in &project_infos {
            load_pinned_sessions_from_dir(&project.work_dir, &mut pinned_sessions);
            for session in &project.sessions {
                if session.work_dir != project.work_dir {
                    load_pinned_sessions_from_dir(&session.work_dir, &mut pinned_sessions);
                }
            }
        }
        let openai_key = threadlane_auth::openai_auth::load_openai_api_key()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .unwrap_or_default();
        let opencode_key =
            threadlane_coding_agent::credentials::opencode_api_key().unwrap_or_default();

        let (stream_tx, stream_rx) = tokio::sync::mpsc::unbounded_channel();
        let (session_refresh_tx, session_refresh_requests) = mpsc::channel::<(u64, PathBuf)>();
        let (session_refresh_results_tx, session_refresh_rx) =
            tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let mut discovery_cache = SessionDiscoveryCache::default();
            while let Ok((generation, work_dir)) = session_refresh_requests.recv() {
                let sessions = discover_sessions_in_project_cached(&work_dir, &mut discovery_cache);
                if session_refresh_results_tx
                    .send((generation, work_dir, sessions))
                    .is_err()
                {
                    break;
                }
            }
        });
        for project in &project_infos {
            let _ = session_refresh_tx.send((0, project.work_dir.clone()));
        }
        let selected_model =
            threadlane_daemon::catalog::default_model_for_project(active_work_dir.as_deref())
                .unwrap_or_default();

        let model_roles = threadlane_runtime::ModelRoles::default();
        let reasoning_effort = ReasoningEffort::default();
        let browser_bridge = threadlane_protocol::browser::BrowserBridge::channel();
        let daemon_core = threadlane_daemon::core::DaemonCore::new()
            .unwrap_or_else(|error| panic!("could not start daemon core: {error}"));
        daemon_core.set_browser_bridge(browser_bridge.clone());
        daemon_core.seed_config(
            selected_model.clone(),
            model_roles.clone(),
            reasoning_effort,
        );
        // `THREADLANE_DAEMON_URL` points the app at a running
        // threadlane-daemon instead of the embedded in-process core —
        // the same DaemonClient surface either way.
        let (daemon_client, daemon_remote): (
            Arc<dyn threadlane_client::DaemonClient>,
            bool,
        ) = match load_host_state.then(|| std::env::var("THREADLANE_DAEMON_URL").ok()).flatten() {
            Some(url) => (
                threadlane_client::RemoteDaemon::connect(
                    url,
                    std::env::var("THREADLANE_DAEMON_TOKEN").ok(),
                ),
                true,
            ),
            None => (
                threadlane_client::LocalDaemon::new(daemon_core.clone()),
                false,
            ),
        };
        let (terminal_command_tx, mut terminal_command_rx) =
            tokio::sync::mpsc::unbounded_channel::<SessionCommand>();
        let (terminal_event_tx, _) = tokio::sync::broadcast::channel::<TerminalEvent>(4096);
        // Daemon events flow into the UI stream: in local mode this is one
        // extra hop through the core's journal/broadcast, in remote mode it
        // is the whole event path. The view drains stream_rx unchanged.
        // Isolated UI fixtures feed state on the deterministic GPUI test
        // scheduler. A live Tokio event forwarder would wake that scheduler
        // from another thread, even when no fixture session is running.
        if let Some(executor) = load_host_state.then(crate::chat::executor).and_then(Result::ok) {
            let mut daemon_events = daemon_client.subscribe();
            {
                let stream_tx = stream_tx.clone();
                executor.spawn(async move {
                    while let Some(event) = daemon_events.recv().await {
                        if stream_tx.send(event).is_err() {
                            break;
                        }
                    }
                });
            }
            // Terminal commands forward serially so Open/Input/Resize keep
            // client order end to end; failures surface as DaemonError.
            {
                let client = daemon_client.clone();
                let error_tx = stream_tx.clone();
                executor.spawn(async move {
                    while let Some(command) = terminal_command_rx.recv().await {
                        if let Err(error) = client.command(command).await {
                            let _ = error_tx.send(SessionEvent::DaemonError {
                                session_id: None,
                                message: error,
                            });
                        }
                    }
                });
            }
        }
        let session_status = active_session_id
            .as_ref()
            .map(|_| "Loading session…".to_string());
        let messages = match (active_work_dir.as_ref(), active_session_file.as_ref()) {
            (Some(_), Some(_)) => Vec::new(),
            _ => Vec::new(),
        };

        let available_models =
            threadlane_daemon::catalog::available_models_for_project(active_work_dir.as_deref());

        let orchestrator_mode = active_work_dir
            .as_deref()
            .map(threadlane_project::subagent_settings::load)
            .map(|settings| settings.orchestrator_mode)
            .unwrap_or_default();

        let mut state = Self {
            completion_alerts: Default::default(),
            completion_alerts_loaded: !load_host_state,
            completion_alerts_saving: false,
            completion_alerts_error: None,
            completion_alerts_retry: None,
            completion_alert_test_requested: false,
            completion_alert_context: None,
            client: threadlane_client::ClientState {
                projects: project_infos,
                active_work_dir,
                active_session_id: active_session_id.clone(),
                pinned_sessions,
                messages: Arc::new(messages),
                session_status,
                ..Default::default()
            },
            is_new_task: active_session_id.is_none(),
            draft_work_mode: WorkMode::Local,
            draft_worktree_base: None,
            draft_worktree_bases: Vec::new(),
            worktree_setups: HashMap::new(),
            available_models,
            composer_text: String::new(),
            github_list_revision: 0,
            requested_composer_inserts: Vec::new(),
            trajectory_by_session: HashMap::new(),
            subagents_by_session: HashMap::new(),
            trajectory_revision: 0,
            trajectory_epoch: 0,
            diagnostics_revision: 0,
            diagnostics_by_session: HashMap::new(),
            token_efficiency_by_session: HashMap::new(),
            acp_config_options: HashMap::new(),
            pending_acp_config: HashMap::new(),
            stashed_prompts: HashMap::new(),
            selected_model,
            model_roles,
            reasoning_effort,
            orchestrator_mode,
            workspace_page: WorkspacePage::Chat,
            github_tab: GitHubTab::default(),
            automation_service: None,
            automations: Default::default(),
            openai_key,
            opencode_key,
            auth_status_msg: None,
            update_status: threadlane_updater::UpdateStatus::Idle,
            requested_editor_target: None,
            requested_panel_document: None,
            requested_github_issue: None,
            requested_composer_prompt: None,
            requested_terminal_command: None,
            requested_terminal_work_dir: None,
            stream_tx,
            stream_rx: Some(stream_rx),
            session_refresh_tx,
            session_refresh_rx: Some(session_refresh_rx),
            session_refresh_generation: 0,
            daemon_core,
            daemon_client,
            daemon_remote,
            pairing: None,
            pairing_error: None,
            pairing_starting: false,
            pairing_generation: 0,
            pairing_restore_allowed: load_host_state,
            pairing_remove_all_pending: false,
            terminal_command_tx,
            terminal_event_tx,
            pending_remote_deletes: HashMap::new(),
            pending_queued_cancels: HashMap::new(),
            scheduler_handles: HashMap::new(),
            scheduler_results: HashMap::new(),
            deferred_stream_events: HashMap::new(),
            browser_bridge,
            mirror_open: false,
            mirror_seen: HashSet::new(),
            session_seen: HashMap::new(),
            session_seen_writer: crate::session_seen::SessionSeenWriter::spawn(),
            session_snooze: HashMap::new(),
            session_snooze_writer: crate::session_snooze::SessionSnoozeWriter::spawn(),
            presented_completion: None,
            session_seen_save_failed: false,
            session_snooze_save_failed: false,
            automation_runs_restored: false,
            pending_hydrations: Vec::new(),
            in_flight_hydrations: HashMap::new(),
            remote_live_status: None,
            remote_inventory: None,
            git_statuses: HashMap::new(),
            git_prs: HashMap::new(),
            auto_address_pr_reviews_enabled: threadlane_git::load_auto_address_pr_reviews_enabled(),
            pr_review_tracking: HashMap::new(),
        };
        // Load each project's seen-store once so discovery refreshes can
        // baseline against persisted watermarks instead of the empty map.
        let store_dirs: Vec<PathBuf> = state
            .projects
            .iter()
            .map(|project| project.work_dir.clone())
            .collect();
        for work_dir in store_dirs {
            state.session_seen_store_for(&work_dir);
            state.session_snooze_store_for(&work_dir);
            state.daemon_core.attach_project(work_dir);
        }
        // Startup reconcile: deadlines that passed while the app slept and
        // work that resumed since are dropped before the first row renders,
        // so expired or ended records never hide a session.
        state.reconcile_session_snoozes();
        if let (Some(session_id), Some(session_file)) = (
            state.active_session_id.clone(),
            active_session_file.as_deref(),
        ) {
            state.pending_hydrations.push(SessionHydrationRequest {
                session_id,
                session_file: session_file.to_path_buf(),
                reload_messages: true,
                runtime_options: active_runtime_work_dir.map(|work_dir| {
                    HydrationRuntimeOptions {
                        work_dir,
                        model: state.selected_model.clone(),
                        model_roles: state.model_roles.clone(),
                    }
                }),
            });
        }
        if let Some(setup) = state
            .active_session_info()
            .and_then(crate::worktree_setup::recover)
        {
            state
                .pending_hydrations
                .retain(|p| p.session_id != setup.session_id);
            state
                .worktree_setups
                .insert(setup.session_id.clone(), setup);
            state.session_status = None;
        }
        state
    }

    pub fn active_close_work(&self) -> Vec<ActiveCloseWork> {
        let mut work = Vec::new();
        for setup in self.worktree_setups.values().filter(|s| s.error.is_none()) {
            work.push(ActiveCloseWork {
                identity: setup.session_file.display().to_string(),
                title: threadlane_runtime::titles::normalize_session_title(&setup.text),
                project: setup.project.display().to_string(),
                status: "Preparing worktree".into(),
            });
        }
        let mut session_files = HashSet::new();
        for (session_file, runtime) in self.daemon_core.runtimes() {
            let active = runtime.is_generating()
                || runtime.scheduled_work_active()
                || matches!(
                    runtime.status(),
                    threadlane_coding_agent::controller::SessionStatus::Working
                );
            let session = self.client.projects.iter().flat_map(|project| &project.sessions).find(|session| session.session_file == *session_file);
            let session_id = session.map(|session| session.id.as_str());
            let permission = session_id.is_some_and(|id| self.client.pending_permissions.contains_key(id));
            let question = session_id.is_some_and(|id| self.client.pending_questions.contains_key(id));
            if !active && !permission && !question { continue; }
            let project = session.and_then(|session| self.client.projects.iter().find(|project| project.sessions.iter().any(|item| item.session_file == session.session_file)));
            work.push(ActiveCloseWork {
                identity: session_file.display().to_string(),
                title: session.map(|session| if session.title.trim().is_empty() { "Untitled session".into() } else { session.title.clone() }).unwrap_or_else(|| "Active session".into()),
                project: project.map(|project| project.name.clone()).unwrap_or_else(|| session_file.parent().unwrap_or(Path::new("")).display().to_string()),
                status: if permission { "Needs permission" } else if question { "Needs an answer" } else { "Running" }.into(),
            });
            session_files.insert(session_file.clone());
        }
        for run in &self.automations.snapshot.runs {
            let Some(status) = active_automation_status(run.status) else { continue; };
            if run.session_file.as_ref().is_some_and(|file| session_files.contains(file)) { continue; }
            let session = run.session_file.as_ref().and_then(|file| self.client.projects.iter().flat_map(|project| &project.sessions).find(|session| session.session_file == *file));
            work.push(ActiveCloseWork {
                identity: run.session_file.as_ref().map(|file| file.display().to_string()).unwrap_or_else(|| format!("automation:{}", run.id)),
                title: session.map(|session| session.title.clone()).filter(|title| !title.trim().is_empty()).unwrap_or_else(|| run.definition.name.clone()),
                project: self.client.projects.iter().find(|project| project.work_dir == run.definition.project).map(|project| project.name.clone()).unwrap_or_else(|| run.definition.project.display().to_string()),
                status: status.into(),
            });
        }
        work.sort_by(|a, b| a.identity.cmp(&b.identity));
        work
    }
    pub(crate) fn messages_mut(&mut self) -> &mut Vec<ChatMessageInfo> {
        Arc::make_mut(&mut self.client.messages)
    }

    pub fn available_models(&self) -> &[threadlane_daemon::catalog::ModelOption] {
        &self.available_models
    }

    /// Test support: construct local state without loading the user's project
    /// registry, restoring their active session, connecting a remote daemon,
    /// or forwarding live daemon events into the deterministic UI scheduler.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn for_tests() -> Self {
        Self::load_from_registry_inner(Vec::new(), false)
    }

    /// Test support: seed the model picker's catalog directly.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn test_set_available_models(
        &mut self,
        models: Vec<threadlane_daemon::catalog::ModelOption>,
    ) {
        self.available_models = models;
    }

    /// Test support: seed live ACP config options under the active session's
    /// projection key, the slot `active_acp_config_options` reads first.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn test_set_acp_config_options(&mut self, options: Vec<AcpConfigOption>) {
        if let Some(key) = self.active_session_projection_key() {
            self.acp_config_options.insert(key, options);
        }
    }

    /// Test support: put a worktree setup in flight for the active session
    /// so model/agent mutations are refused.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn test_start_worktree_setup(&mut self) {
        use threadlane_protocol::daemon::{SetupStage, WorktreeSetup};
        let Some(session_id) = self.client.active_session_id.clone() else {
            return;
        };
        self.worktree_setups.insert(
            session_id.clone(),
            WorktreeSetup {
                project: self.client.active_work_dir.clone().unwrap_or_default(),
                session_id,
                session_file: PathBuf::new(),
                worktree: PathBuf::new(),
                base: String::new(),
                stage: SetupStage::Creating,
                branch: None,
                error: None,
                cancelled: Default::default(),
                text: String::new(),
                images: Vec::new(),
                model: String::new(),
                effort: ReasoningEffort::default(),
                acp_config: Vec::new(),
            },
        );
    }

    /// Test support: read a pending New-task agent setting, the slot
    /// `set_acp_config_option` writes when no session runtime exists yet.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn test_pending_acp_config(&self, agent_id: &str, config_id: &str) -> Option<String> {
        self.pending_acp_config
            .get(agent_id)?
            .get(config_id)
            .cloned()
    }

    /// Subscribe to the local service only when this desktop owns the embedded daemon.
    pub fn start_automations(
        &mut self,
    ) -> Option<tokio::sync::watch::Receiver<crate::automation::Projection>> {
        if self.daemon_remote {
            // Remote projections arrive through the daemon event stream;
            // a local watch would overwrite them with this host's store.
            return None;
        }
        let service = crate::automation::AutomationService::shared();
        // The daemon core's automation bridge forwards the service's agent
        // events into the same broadcast `daemon_client.subscribe()` feeds
        // `stream_tx` — subscribing here too would apply each twice.
        let updates = service.projection.clone();
        self.automation_service = Some(service);
        self.apply_automation_projection(updates.borrow().clone());
        Some(updates)
    }

    /// Apply a local service snapshot while retaining runtime registration and discovery behavior.
    pub fn apply_automation_projection(&mut self, projection: crate::automation::Projection) {
        if self.daemon_remote {
            return;
        }
        if let Some(runtime) = &projection.active_runtime {
            let path = runtime.session_file().to_path_buf();
            if self.daemon_core.runtime_for_file(&path).is_none() {
                let work_dir = path
                    .ancestors()
                    .nth(3)
                    .map(|root| root.to_path_buf())
                    .unwrap_or_default();
                self.register_session_runtime(work_dir, path, runtime.clone());
            }
        }
        let previous: HashMap<_, _> = self.automations.snapshot.runs.iter()
            .map(|run| (&run.id, run)).collect();
        let changed: HashSet<_> = projection.snapshot.runs.iter()
            .filter(|run| previous.get(&run.id)
                .is_none_or(|old| old.status != run.status || old.session_file != run.session_file))
            .map(|run| &run.definition.project).collect();
        for project in changed {
            self.request_session_refresh(project);
        }
        // Sessions an automation created during this app's lifetime are
        // registered before their first run completes so a fast background
        // finish still earns a New result marker. Runs present in the very
        // first applied projection are restored history — discovery's
        // baseline decides them instead.
        let to_register: Vec<(PathBuf, String)> = if self.automation_runs_restored {
            projection
                .snapshot
                .runs
                .iter()
                .filter(|run| !previous.contains_key(&run.id))
                .map(|run| (run.definition.project.clone(), run.session_id.clone()))
                .collect()
        } else {
            Vec::new()
        };
        self.automation_runs_restored = true;
        for (project, session_id) in to_register {
            self.register_session_seen(&project, &session_id);
        }
        self.client.apply_event(SessionEvent::AutomationChanged {
            projection: threadlane_daemon::core::DaemonCore::automation_projection_wire(&projection),
        });
        self.automations = projection;
    }

    /// Open a run using its owning daemon identity, without probing remote paths on this host.
    pub fn open_automation_run(&mut self, id: &str) -> Result<(), String> {
        let run = self
            .automations
            .snapshot
            .runs
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or("Run no longer exists")?;
        if self.daemon_remote {
            let session_file = run.session_file.clone().ok_or("This run has no chat yet")?;
            // The run projection supplies daemon-owned identities. Seed a
            // lightweight row until hydration returns the full SessionInfo;
            // neither the project nor the transcript need exist on this host.
            let project = match self
                .client
                .projects
                .iter()
                .position(|p| p.work_dir == run.definition.project)
            {
                Some(index) => &mut self.client.projects[index],
                None => {
                    self.client.projects.push(ProjectInfo {
                        name: run
                            .definition
                            .project
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        work_dir: run.definition.project.clone(),
                        sessions: Vec::new(),
                        is_expanded: true,
                    });
                    self.client.projects.last_mut().unwrap()
                }
            };
            if !project
                .sessions
                .iter()
                .any(|session| session.id == run.session_id && session.session_file == session_file)
            {
                project
                    .sessions
                    .retain(|session| session.id != run.session_id);
                project.sessions.push(SessionInfo {
                    id: run.session_id.clone(),
                    title: format!("{} · automation", run.definition.name),
                    work_dir: run.definition.project.clone(),
                    runtime_work_dir: session_file
                        .ancestors()
                        .nth(3)
                        .unwrap_or(&run.definition.project)
                        .to_path_buf(),
                    session_file,
                    health: if run.status.active() {
                        SessionHealth::Working
                    } else {
                        SessionHealth::Healthy
                    },
                    is_worktree: run.definition.worktree,
                    worktree_available: true,
                    ..Default::default()
                });
            }
            self.selected_model = run.definition.model.clone();
            self.reasoning_effort =
                ReasoningEffort::from_label(&run.definition.effort).unwrap_or_default();
            self.select_session(run.definition.project, run.session_id);
            return Ok(());
        }
        if run.session_file.as_ref().is_none_or(|path| !path.exists()) {
            return Err("This run has no chat yet".into());
        }
        let project = self
            .client
            .projects
            .iter_mut()
            .find(|p| p.work_dir == run.definition.project)
            .ok_or("Attach this run's project to open its chat")?;
        project.sessions = discover_session_stubs_in_project(&project.work_dir);
        self.select_session(run.definition.project, run.session_id);
        self.workspace_page = WorkspacePage::Chat;
        Ok(())
    }

    pub fn refresh_available_models(&mut self) {
        self.available_models =
            threadlane_daemon::catalog::available_models_for_project(self.client.active_work_dir.as_deref());
        if self.selected_model.is_empty() {
            self.selected_model = self
                .available_models
                .first()
                .map(|model| model.id.clone())
                .unwrap_or_default();
        }
        // Refreshing the catalog also runs during session switches, before
        // hydration restores the destination session's settings. Do not use
        // the settings setter here: it would persist the previous session's
        // reasoning effort into the newly active session.
        self.reasoning_effort = threadlane_provider::model_registry::effective_effort(
            &self.selected_model,
            self.reasoning_effort,
            self.client.active_work_dir.as_deref(),
        );
    }

    pub fn set_auto_address_pr_reviews_enabled(&mut self, enabled: bool) -> Result<(), String> {
        threadlane_git::save_auto_address_pr_reviews_enabled(enabled)?;
        self.auto_address_pr_reviews_enabled = enabled;
        Ok(())
    }

    #[cfg(test)]
    fn current_session_token_usage(&self) -> TokenUsage {
        if let Some(key) = self.active_session_projection_key() {
            if let Some(usage) = self.client.session_token_usage.get(&key) {
                return usage.clone();
            }
        }
        let chars: usize = self.client.messages.iter().map(|m| m.content.len()).sum();
        let approx_tokens = (chars / 4) as u32;
        TokenUsage {
            total_tokens: approx_tokens,
            input_tokens: approx_tokens,
            ..Default::default()
        }
    }

    pub fn stash_prompt(&mut self, session_id: &str, text: String) {
        if !text.trim().is_empty() {
            self.stashed_prompts.insert(session_id.to_string(), text);
        }
    }

    pub fn pop_stashed_prompt(&mut self, session_id: &str) -> Option<String> {
        self.stashed_prompts.remove(session_id)
    }

    pub fn get_stashed_prompt(&self, session_id: &str) -> Option<&String> {
        self.stashed_prompts.get(session_id)
    }

    pub fn clear_stashed_prompt(&mut self, session_id: &str) {
        self.stashed_prompts.remove(session_id);
    }

    fn invalidate_idle_runtimes(&mut self) {
        let idle = self
            .daemon_core
            .runtimes()
            .iter()
            .filter(|(_, runtime)| !runtime.is_generating())
            .map(|(session_file, _)| session_file.clone())
            .collect::<Vec<_>>();
        for session_file in idle {
            self.drop_session_runtime(&session_file);
        }
    }

    fn drop_session_runtime(&mut self, session_file: &Path) {
        self.scheduler_handles.remove(session_file);
        self.scheduler_results.remove(session_file);
        self.daemon_core.drop_runtime(session_file);
    }

    pub fn invalidate_capability_runtimes(&mut self) {
        self.invalidate_idle_runtimes();
    }

    pub(crate) fn save_openai_key(&mut self, key: String) -> Result<(), String> {
        let key = key.trim().to_string();
        if !key.is_empty() {
            threadlane_auth::openai_auth::save_openai_api_key(&key)?;
            self.openai_key = key;
            self.auth_status_msg = Some("OpenAI API key saved successfully!".into());
        } else {
            if let Err(error) = threadlane_auth::openai_auth::remove_credentials() {
                tracing::warn!("failed to remove OpenAI credentials: {error}");
                self.auth_status_msg =
                    Some(format!("OpenAI API key removal may be incomplete: {error}"));
            } else {
                self.auth_status_msg = Some("OpenAI API key removed.".into());
            }
            self.openai_key.clear();
        }
        self.invalidate_idle_runtimes();
        self.reconcile_selected_model();
        Ok(())
    }

    pub(crate) fn save_opencode_key(&mut self, key: String) -> Result<(), String> {
        let key = key.trim().to_string();
        if !key.is_empty() {
            threadlane_auth::opencode_auth::save_opencode_api_key(&key)?;
            self.opencode_key = key;
            self.auth_status_msg = Some("Opencode API key saved successfully!".into());
        } else {
            if let Err(error) = threadlane_auth::opencode_auth::clear_opencode_api_key() {
                tracing::warn!("failed to remove Opencode API key: {error}");
                self.auth_status_msg = Some(format!(
                    "Opencode API key removal may be incomplete: {error}"
                ));
            } else {
                self.auth_status_msg = Some("Opencode API key removed.".into());
            }
            self.opencode_key.clear();
        }
        self.invalidate_idle_runtimes();
        self.reconcile_selected_model();
        Ok(())
    }

    pub fn reconcile_selected_model(&mut self) {
        self.refresh_available_models();
        if !self
            .available_models
            .iter()
            .any(|model| model.id == self.selected_model)
        {
            self.selected_model = self
                .available_models
                .first()
                .map(|model| model.id.clone())
                .unwrap_or_default();
        }
        self.set_reasoning_effort(self.reasoning_effort);
        self.invalidate_idle_runtimes();
    }

    pub(crate) fn set_selected_model(&mut self, model: String) {
        if self.active_worktree_setup().is_some() {
            self.client.session_status =
                Some("Cancel worktree setup before changing agent settings".into());
            return;
        }
        if !self.available_models.iter().any(|m| m.id == model) {
            return;
        }
        if self.daemon_remote {
            self.selected_model = model.clone();
            self.dispatch_command(SessionCommand::SetModel {
                session_id: self.client.active_session_id.clone().unwrap_or_default(),
                model,
            });
            return;
        }
        if let Some((runtime, _)) = self.active_session_runtime() {
            if runtime.model() == model && self.selected_model == model {
                return;
            }
            if runtime.is_generating() {
                self.client.session_status = Some("Stop the current turn before changing models".into());
                return;
            }
            let result = if let Some(error) = runtime.harness_error() {
                Err(error.to_string())
            } else if let Ok(mut agent) = runtime.agent.try_lock() {
                // Reuse the canonical model fact used by /model. Rebuilding
                // afterwards also refreshes the selected provider's credentials.
                agent.set_fact("model", &model)
            } else {
                Err("Agent settings are still loading. Try changing models again shortly.".into())
            };
            if let Err(error) = result {
                self.client.session_status = Some(format!("Could not switch models: {error}"));
                return;
            }
            self.drop_session_runtime(&runtime.session_file);
        } else if self.selected_model == model {
            return;
        }
        self.selected_model = model.clone();
        self.set_reasoning_effort(self.reasoning_effort);
        self.auth_status_msg = Some(format!("Model switched to {model}"));
        if self.client.session_status.as_deref().is_some_and(|status| {
            status == "Stop the current turn before changing models"
                || status.starts_with("Could not switch models:")
        }) {
            self.client.session_status = None;
        }
        if let Some(key) = self.active_session_projection_key() {
            self.acp_config_options.remove(&key);
        }
        // Install the rebuilt runtime before a pending hydration can restore
        // its older selection. The next prompt and picker share this runtime.
        self.active_session_runtime();
        self.request_acp_config_options();
    }

    pub fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        if self.active_worktree_setup().is_some() {
            self.client.session_status =
                Some("Cancel worktree setup before changing agent settings".into());
            return;
        }
        let effort = threadlane_provider::model_registry::effective_effort(
            &self.selected_model,
            effort,
            self.client.active_work_dir.as_deref(),
        );
        if self.daemon_remote {
            self.reasoning_effort = effort;
            self.dispatch_command(SessionCommand::SetReasoningEffort {
                session_id: self.client.active_session_id.clone().unwrap_or_default(),
                effort,
            });
            return;
        }
        if let Some((runtime, _)) = self.active_session_runtime() {
            if runtime.is_generating() {
                if self.reasoning_effort != effort {
                    self.reasoning_effort = effort;
                    self.client.session_status =
                        Some("Reasoning effort changed; it will apply to the next turn".into());
                }
                return;
            }
            let result = if let Some(error) = runtime.harness_error() {
                Err(error.to_string())
            } else if let Ok(mut agent) = runtime.agent.try_lock() {
                agent.set_fact("reasoning_effort", effort.label())
            } else {
                Err("Agent settings are still loading. Try changing reasoning effort again shortly."
                    .into())
            };
            if let Err(error) = result {
                self.client.session_status = Some(format!("Could not switch reasoning effort: {error}"));
                return;
            }
            self.drop_session_runtime(&runtime.session_file);
        } else if self.reasoning_effort == effort {
            return;
        }
        self.reasoning_effort = effort;
        self.active_session_runtime();
    }

    /// Switch the session orchestration mode shown in the composer Mode
    /// dropdown. Persists per project in `.threadlane/subagents.json` and
    /// rebuilds the live session runtime so the next turn routes through the
    /// new mode; mirrors `set_selected_model`.
    pub fn set_orchestrator_mode(&mut self, mode: OrchestratorMode) {
        if self.active_worktree_setup().is_some() {
            self.client.session_status =
                Some("Cancel worktree setup before changing agent settings".into());
            return;
        }
        let Some(work_dir) = self.client.active_work_dir.clone() else {
            return;
        };
        if self.orchestrator_mode == mode {
            return;
        }
        if self.daemon_remote {
            self.orchestrator_mode = mode;
            // With a live session the daemon persists the setting and
            // rebuilds the runtime; without one it stays as the draft
            // default and is saved with the next write.
            if let Some(session_id) = self.client.active_session_id.clone() {
                self.dispatch_command(SessionCommand::SetOrchestratorMode { session_id, mode });
            } else {
                let mut settings = threadlane_project::subagent_settings::load(&work_dir);
                settings.orchestrator_mode = mode;
                let _ = threadlane_project::subagent_settings::save(&work_dir, &settings);
            }
            return;
        }
        let mut settings = threadlane_project::subagent_settings::load(&work_dir);
        settings.orchestrator_mode = mode;
        if let Err(error) = threadlane_project::subagent_settings::save(&work_dir, &settings) {
            self.client.session_status = Some(format!("Could not switch mode: {error}"));
            return;
        }
        self.orchestrator_mode = mode;
        // Rebuild only a live runtime; a missing one is constructed on
        // demand from the saved settings by hydration or the next prompt.
        let Some(session_id) = self.client.active_session_id.clone() else {
            return;
        };
        let session_file = self.session_file(&work_dir, &session_id);
        let Some(runtime) = self.daemon_core.runtime_for_file(&session_file) else {
            return;
        };
        if runtime.is_generating() {
            self.client.session_status = Some("Mode changed; it will apply to the next turn".into());
            return;
        }
        self.drop_session_runtime(&session_file);
        self.active_session_runtime();
    }

    /// Re-read the orchestration mode from the active project's stored
    /// settings. Called after session or project switches so the composer
    /// dropdown never shows a stale project's mode.
    fn refresh_orchestrator_mode(&mut self) {
        if let Some(work_dir) = self.client.active_work_dir.as_deref() {
            self.orchestrator_mode =
                threadlane_project::subagent_settings::load(work_dir).orchestrator_mode;
        }
    }

    pub(crate) fn open_settings(&mut self) {
        self.workspace_page = WorkspacePage::Settings;
        self.auth_status_msg = None;
    }

    pub(crate) fn open_github(&mut self) {
        self.workspace_page = WorkspacePage::GitHub;
    }

    pub(crate) fn open_github_issue(&mut self, work_dir: PathBuf, number: u64) {
        self.workspace_page = WorkspacePage::GitHub;
        self.github_tab = GitHubTab::Issues;
        self.requested_github_issue = Some((work_dir, number));
    }

    pub(crate) fn close_github(&mut self) {
        self.workspace_page = WorkspacePage::Chat;
    }

    pub(crate) fn close_settings(&mut self) {
        self.workspace_page = WorkspacePage::Chat;
        self.auth_status_msg = None;
    }

    /// Refresh session metadata on the owning host, never the remote client's disk.
    fn request_session_refresh(&self, work_dir: &Path) {
        if self.daemon_remote {
            self.dispatch_command(SessionCommand::GetProjectState {
                work_dir: work_dir.to_path_buf(),
            });
            return;
        }
        let _ = self.session_refresh_tx.send((
            self.session_refresh_generation,
            work_dir.to_path_buf(),
        ));
    }

    /// Remote navigation must wait for authoritative metadata after reconnect.
    pub fn session_inventory_available(&self, work_dir: &Path) -> bool {
        if !self.daemon_client.is_connected() { return false; }
        if !self.daemon_remote { return true; }
        self.remote_inventory.as_ref().is_some_and(|(client, epoch, projects)| {
            Arc::ptr_eq(client, &self.daemon_client)
                && *epoch == self.daemon_client.file_search_connection_epoch()
                && projects.contains(work_dir)
        })
    }

    /// Apply a current local discovery result; remote metadata arrives through ProjectChanged.
    pub fn apply_session_refresh(
        &mut self,
        work_dir: PathBuf,
        sessions: Vec<SessionInfo>,
        generation: u64,
    ) -> bool {
        self.sync_completion_alert_context();
        if self.daemon_remote || generation != self.session_refresh_generation {
            return false;
        }
        if !self.client.projects.iter().any(|project| project.work_dir == work_dir) {
            return false;
        }
        let automation_files = self.automations.snapshot.runs.iter().filter_map(|run| run.session_file.clone()).collect();
        self.completion_alerts.observe(&work_dir, &sessions, &automation_files);
        // Sessions first confirmed by this discovery pass inherit their
        // current completion as seen — history predating the marker is never
        // reported as a new result.
        self.baseline_session_seen(&sessions);
        self.load_pinned_sessions(&work_dir);
        for session in &sessions {
            if session.work_dir != work_dir {
                self.load_pinned_sessions(&session.work_dir);
            }
        }
        let active_project = self.client.active_work_dir.as_ref() == Some(&work_dir);
        let active_session_id = self.client.active_session_id.clone();
        let selected_session_missing = {
            let Some(project) = self.client.projects
                .iter_mut()
                .find(|project| project.work_dir == work_dir)
            else {
                return false;
            };
            project.sessions = sessions;
            active_project
                && active_session_id.as_ref().is_some_and(|session_id| {
                    !project.sessions.iter().any(|session| session.id == *session_id)
                })
        };
        if selected_session_missing {
            self.client.active_session_id = None;
        }
        // Discovery may confirm a newer completion or a resumed state
        // that ends a snooze recorded before the app was last open.
        self.reconcile_session_snoozes();
        true
    }

    /// Recreates the missing worktree checkout for the active session.
    ///
    /// This runs synchronously on the UI thread: the blocking portion is two
    /// short Git subprocesses (`prune_worktrees`, `create_worktree`) plus one
    /// uncached discovery pass, all user-initiated and infrequent, so moving it
    /// to the background executor would add a plan/apply round-trip for little
    /// gain. The generation bump below keeps that tradeoff safe: any refresh
    /// result captured before the new checkout is discarded by
    /// `apply_session_refresh`.
    pub(crate) fn recreate_active_worktree(&mut self) -> Result<(), String> {
        let work_dir = self.client.active_work_dir.clone().ok_or("No active project")?;
        let session_id = self.client.active_session_id.clone().ok_or("No active session")?;
        let session = self.client.projects
            .iter()
            .find(|project| project.work_dir == work_dir)
            .and_then(|project| {
                project
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
            })
            .cloned()
            .ok_or("Active session was not found")?;
        if !session.is_worktree {
            return Err("The active session does not use a worktree".into());
        }
        if session.worktree_available {
            return Err("The active worktree is already available".into());
        }
        let stub = JsonlStore::open_read_only(canonical_session_file(&work_dir, &session_id))
            .map_err(|error| error.to_string())?;
        let expected_path =
            crate::discovery::effective_session_work_dir(&work_dir, &session_id, &stub.facts());
        if session.runtime_work_dir != expected_path {
            return Err("The recorded worktree path is not safe to recreate".into());
        }
        let branch = session
            .git_branch
            .as_deref()
            .ok_or("The session has no recorded Git branch")?;
        let branches = threadlane_git::inspect(&work_dir)
            .map_err(|error| format!("Could not inspect project branches: {error}"))?
            .branches;
        if !branches.iter().any(|candidate| candidate == branch) {
            return Err(format!("Branch '{branch}' no longer exists in the project"));
        }
        threadlane_git::prune_worktrees(&work_dir)
            .map_err(|error| format!("Could not prune stale worktrees: {error}"))?;
        threadlane_git::create_worktree(&work_dir, &session.runtime_work_dir, branch)
            .map_err(|error| format!("Could not recreate worktree: {error}"))?;

        let sessions = discover_sessions_in_project(&work_dir);
        self.load_pinned_sessions(&work_dir);
        let recreated = sessions
            .iter()
            .any(|candidate| candidate.id == session_id && candidate.worktree_available);
        // Invalidate refresh results captured before the new checkout existed;
        // the background worker recomputes `worktree_available` per pass (see
        // `discover_sessions_in_project_cached`) so the follow-up refresh
        // cannot reuse the stale unavailable entry either.
        self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
        if let Some(project) = self.client.projects
            .iter_mut()
            .find(|project| project.work_dir == work_dir)
        {
            project.sessions = sessions;
        }
        if !recreated {
            return Err("Worktree was recreated, but the session could not be rediscovered".into());
        }
        self.select_session(work_dir.clone(), session_id);
        self.request_session_refresh(&work_dir);
        self.client.session_status = None;
        Ok(())
    }

    fn refresh_active_session(&mut self) {
        if let (Some(work_dir), Some(session_id)) = (
            &self.client.active_work_dir.clone(),
            &self.client.active_session_id.clone(),
        ) {
            let session_file = self.session_file(work_dir, session_id);
            let is_generating = self
                .daemon_core
                .runtime_for_file(&session_file)
                .is_some_and(|runtime| runtime.is_generating());
            if !is_generating {
                self.pending_hydrations.push(SessionHydrationRequest {
                    session_id: session_id.clone(),
                    session_file: session_file.clone(),
                    reload_messages: true,
                    runtime_options: None,
                });
            }
            self.request_session_refresh(work_dir);
        }
    }

    pub(crate) fn begin_new_task(&mut self) {
        self.workspace_page = WorkspacePage::Chat;
        if let Some(project_work_dir) = self.client.active_session_id.as_ref().and_then(|session_id| {
            self.client.projects.iter().find_map(|project| {
                project
                    .sessions
                    .iter()
                    .any(|session| &session.id == session_id)
                    .then(|| project.work_dir.clone())
            })
        }) {
            self.client.active_work_dir = Some(project_work_dir);
        }
        self.client.active_session_id = None;
        self.is_new_task = true;
        self.draft_work_mode = WorkMode::Local;
        self.draft_worktree_base = None;
        self.draft_worktree_bases.clear();
        self.client.messages = Arc::new(Vec::new());
        self.client.active_plan = SessionPlan::default();
        self.client.is_generating = false;
        self.client.session_status = None;
        if self.client.active_work_dir.is_none() {
            self.client.active_work_dir = self.client.projects
                .first()
                .map(|project| project.work_dir.clone());
        }
        self.refresh_orchestrator_mode();
    }

    pub fn set_work_mode(&mut self, mode: WorkMode) {
        self.draft_work_mode = mode;
        if mode == WorkMode::Worktree {
            let Some(project) = self.client.active_work_dir.clone() else {
                return;
            };
            if self.daemon_remote {
                if self.daemon_client.supports_project_io() {
                    // The daemon enumerates bases on its own filesystem
                    // and answers through the journaled `WorktreeBases`
                    // event — the only correct answer for a remote host.
                    self.dispatch_command(SessionCommand::GetWorktreeBases {
                        work_dir: project,
                    });
                } else {
                    // A pre-3 remote daemon cannot answer; reporting the
                    // limitation beats listing the client's own checkout.
                    let _ = self.stream_tx.send(SessionEvent::WorktreeBases {
                        project,
                        result: Err(
                            crate::project_io::UNSUPPORTED_PROJECT_IO.to_string(),
                        ),
                    });
                }
                return;
            }
            match crate::chat::executor() {
                Ok(runtime) => {
                    let tx = self.stream_tx.clone();
                    runtime.spawn_blocking(move || {
                        let result =
                            threadlane_git::worktree_bases(&project).map_err(|e| e.to_string());
                        let _ = tx.send(SessionEvent::WorktreeBases { project, result });
                    });
                }
                Err(error) => self.client.session_status = Some(error),
            }
        }
    }

    pub(crate) fn set_sidebar_project_filter(&mut self, work_dir: Option<PathBuf>) {
        self.client.sidebar_project_filter = work_dir.filter(|candidate| {
            self.client.projects
                .iter()
                .any(|project| project.work_dir == *candidate)
        });
    }

    fn persist_project_selection(&self, work_dir: &Path, session_id: Option<&str>) {
        if let Err(error) = threadlane_project::select_project(work_dir, session_id) {
            tracing::warn!("Failed to persist selected project: {error}");
        }
    }

    pub(crate) fn select_draft_project(&mut self, work_dir: PathBuf) {
        if self.client.projects
            .iter()
            .any(|project| project.work_dir == work_dir)
        {
            self.client.active_work_dir = Some(work_dir.clone());
            self.client.active_session_id = None;
            self.is_new_task = true;
            self.draft_work_mode = WorkMode::Local;
            self.draft_worktree_base = None;
            self.draft_worktree_bases.clear();
            self.client.messages = Arc::new(Vec::new());
            self.client.active_plan = SessionPlan::default();
            self.client.is_generating = false;
            self.client.session_status = None;
            self.persist_project_selection(&work_dir, None);
            self.refresh_available_models();
            self.refresh_orchestrator_mode();
            self.request_session_refresh(&work_dir);
        }
    }

    pub fn request_open_file(&mut self, relative_path: String) {
        self.request_open_file_at_line(relative_path, None);
    }

    pub fn request_open_file_at_line(&mut self, relative_path: String, line: Option<usize>) {
        let Some((root, relative)) = self.resolve_workspace_file(&relative_path) else {
            return;
        };
        self.requested_editor_target = Some(RequestedEditorTarget::File {
            project: root,
            path: relative,
            line,
        });
    }

    /// Requests that `relative_path` open as an editable document inside the
    /// right panel's Files host — the second file-editor surface. Shares the
    /// checkout validation of `request_open_file_at_line`; `RightPanelView`
    /// takes the request and loads the buffer.
    pub fn request_open_panel_file(&mut self, relative_path: String) {
        if let Some((root, relative)) = self.resolve_workspace_file(&relative_path) {
            self.requested_panel_document = Some((root, relative));
        }
    }

    /// Validates `relative_path` inside the active checkout and returns the
    /// checkout root plus normalized workspace-relative path, publishing any
    /// failure to `client.session_status`. A remote checkout's filesystem
    /// lives behind the daemon, so its path cannot canonicalize on the UI
    /// host: containment is checked lexically and the daemon-backed project
    /// I/O read enforces it at open time.
    fn resolve_workspace_file(&mut self, relative_path: &str) -> Option<(PathBuf, String)> {
        let root = self.active_git_work_dir()?;
        if !root.exists() {
            let Some(relative) = lexically_normalized_relative(relative_path) else {
                self.client.session_status =
                    Some("File is outside the workspace".to_string());
                return None;
            };
            return Some((root, relative));
        }
        let path = match threadlane_tools::validate_path_in_workspace(relative_path, &root) {
            Ok(path) => path,
            Err(error) => {
                self.client.session_status = Some(error);
                return None;
            }
        };
        let canonical_root = match root.canonicalize() {
            Ok(root) => root,
            Err(error) => {
                self.client.session_status = Some(format!("Invalid workspace root: {error}"));
                return None;
            }
        };
        let relative = match path.strip_prefix(&canonical_root) {
            Ok(relative) => relative,
            Err(error) => {
                self.client.session_status =
                    Some(format!("File is outside the workspace: {error}"));
                return None;
            }
        };
        Some((root, relative.to_string_lossy().into_owned()))
    }

    pub fn request_open_diff(&mut self, project: PathBuf, relative_path: String, content: String) {
        self.requested_editor_target = Some(RequestedEditorTarget::Diff {
            project,
            path: relative_path,
            content,
        });
    }

    pub fn request_composer_prompt(&mut self, prompt: String) {
        self.requested_composer_prompt = Some(prompt);
    }

    pub(crate) fn request_run_terminal_command(&mut self, command: String) {
        self.requested_terminal_command = Some(command);
    }

    pub(crate) fn request_open_terminal(&mut self, work_dir: PathBuf) {
        self.requested_terminal_work_dir = Some(work_dir);
    }

    pub(crate) fn select_session(&mut self, work_dir: PathBuf, session_id: String) {
        self.select_session_with_persistence(work_dir, session_id, true)
    }

    /// Select and hydrate a session; remote sessions never use local settings or runtime recovery.
    fn select_session_with_persistence(
        &mut self,
        work_dir: PathBuf,
        session_id: String,
        persist_selection: bool,
    ) {
        self.workspace_page = WorkspacePage::Chat;
        if self.daemon_remote {
            let session = self
                .client
                .projects
                .iter()
                .find(|project| project.work_dir == work_dir)
                .and_then(|project| {
                    project
                        .sessions
                        .iter()
                        .find(|session| session.id == session_id)
                })
                .cloned();
            let Some(session) = session else {
                self.client.session_status = Some("Remote session metadata is unavailable".into());
                return;
            };
            self.client.select_session(&session);
            self.remote_live_status = None;
            self.client.session_status = Some("Loading session…".into());
            self.is_new_task = false;
            self.pending_hydrations.retain(|pending| {
                pending.session_id != session.id || pending.session_file != session.session_file
            });
            self.pending_hydrations.push(SessionHydrationRequest {
                session_id: session.id,
                session_file: session.session_file,
                reload_messages: true,
                // Opening a transcript must not rebuild an automation's
                // live runtime with settings from the client machine.
                runtime_options: None,
            });
            return;
        }
        let session = self.client.projects
            .iter()
            .find(|project| project.work_dir == work_dir)
            .and_then(|project| {
                project
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
            });
        let session_file = session
            .map(|session| session.session_file.clone())
            .unwrap_or_else(|| self.session_file(&work_dir, &session_id));
        let runtime_work_dir = session
            .map(|session| session.runtime_work_dir.clone())
            .unwrap_or_else(|| work_dir.clone());
        self.client.active_work_dir = Some(work_dir.clone());
        self.client.active_session_id = Some(session_id.clone());
        self.is_new_task = false;
        self.refresh_orchestrator_mode();
        let project_work_dir = self.client.projects
            .iter()
            .find(|project| {
                project
                    .sessions
                    .iter()
                    .any(|session| session.id == session_id && session.work_dir == work_dir)
            })
            .map(|project| project.work_dir.as_path())
            .unwrap_or(&work_dir);
        if persist_selection {
            self.persist_project_selection(project_work_dir, Some(&session_id));
        }
        self.refresh_available_models();
        self.client.messages = Arc::new(Vec::new());
        self.client.active_plan = SessionPlan::default();
        // Switching to a session that is still generating must keep the
        // generating state: hydration preserves in-flight streaming rows only
        // while it is set, and the composer stays gated on it.
        self.client.is_generating = self
            .daemon_core
            .runtime_for_file(&session_file)
            .is_some_and(|runtime| runtime.is_generating());
        self.client.session_status = Some("Loading session…".into());
        if !self.worktree_setups.contains_key(&session_id) {
            if let Some(setup) = self
                .active_session_info()
                .and_then(crate::worktree_setup::recover)
            {
                self.worktree_setups.insert(session_id.clone(), setup);
            }
        }
        if let Some(setup) = self.worktree_setups.get(&session_id) {
            self.client.is_generating = setup.error.is_none();
            let text = if setup.images.is_empty() {
                setup.text.clone()
            } else {
                format!(
                    "{}\n[{} image attachment(s)]",
                    setup.text,
                    setup.images.len()
                )
            };
            let pending_id = format!("pending-user-{session_id}-{}", self.client.messages.len());
            self.push_optimistic_follow_up(&session_id, text, pending_id);
            self.client.session_status = None;
            return;
        }
        let request = SessionHydrationRequest {
            session_id,
            session_file,
            reload_messages: true,
            runtime_options: Some(HydrationRuntimeOptions {
                work_dir: runtime_work_dir,
                model: self.selected_model.clone(),
                model_roles: self.model_roles.clone(),
            }),
        };
        self.drain_chat_stream(Vec::new());
        self.pending_hydrations.retain(|pending| {
            pending.session_id != request.session_id || pending.session_file != request.session_file
        });
        self.pending_hydrations.push(request);
    }

    /// Snapshot the source and return blocking work without doing journal I/O.
    /// Execute the returned closure off the foreground thread, then deliver its
    /// result to `finish_session_fork` on the foreground thread.
    pub fn prepare_session_fork(
        &self,
        work_dir: PathBuf,
        session_id: String,
    ) -> Result<impl FnOnce() -> Result<(String, Vec<SessionInfo>), String> + Send + 'static, String>
    {
        let source = self.client.projects
            .iter()
            .find(|project| project.work_dir == work_dir)
            .and_then(|project| {
                project
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
            })
            .cloned()
            .ok_or("Session was not found")?;
        if self.worktree_setups.contains_key(&session_id) {
            return Err("Finish or cancel worktree setup before forking this session".into());
        }
        if self
            .daemon_core
            .runtime_for_file(&source.session_file)
            .is_some_and(|runtime| runtime.is_generating())
        {
            return Err("Stop the running generation before forking this session".into());
        }
        Ok(move || {
            if !source.runtime_work_dir.is_dir() {
                return Err("Recreate the missing worktree before forking this session".into());
            }
            let id = format!(
                "session_{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let destination = canonical_session_file(&work_dir, &id);
            threadlane_coding_agent::harness::CodingSessionHarness::fork_to_path(
                &source.session_file,
                &destination,
            )?;
            if source.is_worktree {
                let result = (|| {
                    use threadlane_coding_agent::harness::CodingSessionHarness;
                    let owner = source
                        .runtime_work_dir
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or("Worktree has no valid owner")?;
                    for (key, value) in [
                        ("is_worktree", "true"),
                        ("worktree_owner", owner),
                        (
                            "worktree_path",
                            source
                                .runtime_work_dir
                                .to_str()
                                .ok_or("Worktree path is not UTF-8")?,
                        ),
                    ] {
                        CodingSessionHarness::append_fact_to_path(
                            &destination,
                            "main",
                            key,
                            value,
                            None,
                        )?;
                    }
                    Ok::<(), String>(())
                })();
                if let Err(error) = result {
                    Self::remove_file_if_present(&destination)?;
                    return Err(error);
                }
            }
            Ok((id, discover_sessions_in_project(&work_dir)))
        })
    }

    pub fn finish_session_fork(
        &mut self,
        work_dir: PathBuf,
        id: String,
        sessions: Vec<SessionInfo>,
    ) {
        let Some(project) = self.client.projects
            .iter_mut()
            .find(|project| project.work_dir == work_dir)
        else {
            return;
        };
        // Only merge the newly created session: the background snapshot may
        // predate unrelated session additions/removals on the foreground.
        if let Some(session) = sessions.into_iter().find(|session| session.id == id) {
            project.sessions.retain(|session| session.id != id);
            project.sessions.insert(0, session);
        }
        self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
        self.sync_completion_alert_context();
        if let Some(session) = self.client.projects.iter()
            .find(|project| project.work_dir == work_dir)
            .and_then(|project| project.sessions.iter().find(|session| session.id == id))
            .cloned()
        {
            self.baseline_session_seen(std::slice::from_ref(&session));
            if !self.daemon_remote {
                self.completion_alerts.baseline_interactive(
                    crate::completion_alerts::CompletionAlertIdentity::new(&work_dir, &session),
                    &session.completion_summary,
                );
            }
        }
        self.select_session(work_dir.clone(), id);
        self.request_session_refresh(&work_dir);
    }

    #[cfg(test)]
    pub(crate) fn fork_session(
        &mut self,
        work_dir: PathBuf,
        session_id: String,
    ) -> Result<String, String> {
        let work = self.prepare_session_fork(work_dir.clone(), session_id)?;
        let (id, sessions) = work()?;
        self.finish_session_fork(work_dir, id.clone(), sessions);
        Ok(id)
    }

    fn ensure_worktree_not_shared(&self, work_dir: &Path, session_id: &str) -> Result<(), String> {
        if let Some(checkout) = self.session_worktree_path(work_dir, session_id) {
            if discover_session_stubs_in_project(work_dir)
                .iter()
                .any(|session| {
                    session.id != session_id
                        && session.is_worktree
                        && session.runtime_work_dir == checkout
                })
            {
                return Err("This worktree is used by another session. Keep the worktree when archiving or deleting this session.".into());
            }
        }
        Ok(())
    }

    pub(crate) fn settle_session(
        &mut self,
        work_dir: PathBuf,
        session_id: String,
        delete_worktree: bool,
    ) -> Result<(), String> {
        if delete_worktree {
            self.ensure_worktree_not_shared(&work_dir, &session_id)?;
        }
        if self.worktree_setups.contains_key(&session_id) {
            return Err("Cancel worktree setup before archiving or deleting this session".into());
        }
        let session_file = self.session_file(&work_dir, &session_id);
        if self.daemon_remote {
            // The daemon archives the transcript, applies the shared-worktree
            // and dirtiness guards, and drops the runtime. Persisted cleanup
            // waits for `SessionRemoved`: a rejected delete arrives as
            // DaemonError and the session row must come back with its pin
            // and seen watermark intact.
            self.pending_remote_deletes
                .insert(session_id.clone(), work_dir);
            self.dispatch_command(SessionCommand::DeleteSession {
                session_id,
                session_file,
                delete_worktree,
            });
            return Ok(());
        }
        if self
            .daemon_core
            .runtime_for_file(&session_file)
            .is_some_and(|runtime| runtime.is_generating())
        {
            return Err("Stop the running generation before archiving this session".into());
        }
        let archive_dir = work_dir.join(".threadlane/sessions/archive");
        std::fs::create_dir_all(&archive_dir).map_err(|error| error.to_string())?;
        let file_name = session_file
            .file_name()
            .ok_or_else(|| "Session file has no file name".to_string())?;
        let archive_file = archive_dir.join(file_name);
        if let Some(worktree_dir) = self.session_worktree_path(&work_dir, &session_id) {
            if delete_worktree && worktree_dir.exists() {
                // Untracked Threadlane bookkeeping (session transcripts,
                // previews) lives inside the worktree by design and must
                // never block archiving; every other change does.
                let dirty = threadlane_git::inspect(&worktree_dir)
                    .map_err(|error| error.to_string())?
                    .files
                    .iter()
                    .any(|file| !(file.is_untracked() && file.path.starts_with(".threadlane/")));
                if dirty {
                    return Err("Commit or discard worktree changes before archiving".into());
                }
                std::fs::copy(&session_file, &archive_file).map_err(|error| error.to_string())?;
                // The transcript is archived above; drop the worktree-local
                // bookkeeping dir so `git worktree remove` (non-force) has
                // nothing app-owned left to trip on. Anything else untracked
                // was already rejected by the dirtiness check.
                let worktree_threadlane = worktree_dir.join(".threadlane");
                if worktree_threadlane.exists() {
                    if let Err(error) = std::fs::remove_dir_all(&worktree_threadlane) {
                        if let Err(error) = Self::remove_file_if_present(&archive_file) {
                            tracing::warn!("session teardown rollback failed: {error}");
                        }
                        return Err(error.to_string());
                    }
                }
                if let Err(error) = threadlane_git::remove_worktree(&work_dir, &worktree_dir, false)
                {
                    if let Err(cleanup_error) = Self::remove_file_if_present(&archive_file) {
                        tracing::warn!("session teardown rollback failed: {cleanup_error}");
                    }
                    return Err(error.to_string());
                }
                threadlane_tools::remove_worktree_cargo_target_dir(&worktree_dir);
                let stub = canonical_session_file(&work_dir, &session_id);
                Self::remove_file_if_present(&stub)?;
                if let Err(error) = threadlane_git::prune_worktrees(&work_dir) {
                    tracing::warn!("worktree prune failed: {error}");
                }
            } else {
                if session_file.exists() {
                    if std::fs::rename(&session_file, &archive_file).is_err() {
                        std::fs::copy(&session_file, &archive_file)
                            .map_err(|error| error.to_string())?;
                        if let Err(error) = Self::remove_file_if_present(&session_file) {
                            tracing::warn!("session teardown rollback failed: {error}");
                        }
                    }
                }
                let stub = canonical_session_file(&work_dir, &session_id);
                Self::remove_file_if_present(&stub)?;
                if delete_worktree {
                    if let Err(error) = threadlane_git::prune_worktrees(&work_dir) {
                        tracing::warn!("worktree prune failed: {error}");
                    }
                }
            }
        } else {
            std::fs::rename(&session_file, archive_file).map_err(|error| error.to_string())?;
        }
        self.finish_session_removal(&work_dir, &session_id);
        Ok(())
    }

    pub(crate) fn remove_session(
        &mut self,
        work_dir: PathBuf,
        session_id: String,
        delete_worktree: bool,
    ) -> Result<(), String> {
        if delete_worktree {
            self.ensure_worktree_not_shared(&work_dir, &session_id)?;
        }
        if self.worktree_setups.contains_key(&session_id) {
            return Err("Cancel worktree setup before archiving or deleting this session".into());
        }
        let session_file = self.session_file(&work_dir, &session_id);
        if self.daemon_remote {
            // As in settle_session: the session's persisted cleanup waits
            // for the daemon's `SessionRemoved` ack.
            self.pending_remote_deletes
                .insert(session_id.clone(), work_dir);
            self.dispatch_command(SessionCommand::DeleteSession {
                session_id,
                session_file,
                delete_worktree,
            });
            return Ok(());
        }
        if self
            .daemon_core
            .runtime_for_file(&session_file)
            .is_some_and(|runtime| runtime.is_generating())
        {
            return Err("Stop the running generation before deleting this session".into());
        }
        // Archive the transcript first: deletion destroys the JSONL, and a
        // failed delete must never lose history silently.
        let archive_dir = work_dir.join(".threadlane/sessions/archive");
        std::fs::create_dir_all(&archive_dir).map_err(|error| error.to_string())?;
        let file_name = session_file
            .file_name()
            .ok_or_else(|| "Session file has no file name".to_string())?;
        let archive_file = archive_dir.join(file_name);
        if session_file.exists() {
            std::fs::copy(&session_file, &archive_file).map_err(|error| error.to_string())?;
        }
        if let Some(worktree_dir) = self.session_worktree_path(&work_dir, &session_id) {
            if delete_worktree && worktree_dir.exists() {
                // Same dirtiness guard as archiving: untracked app-owned
                // bookkeeping never blocks, every other change refuses the
                // destroy rather than eating uncommitted work.
                let dirty = threadlane_git::inspect(&worktree_dir)
                    .map_err(|error| error.to_string())?
                    .files
                    .iter()
                    .any(|file| !(file.is_untracked() && file.path.starts_with(".threadlane/")));
                if dirty {
                    return Err(
                        "Commit or discard worktree changes before deleting this session".into(),
                    );
                }
                threadlane_git::remove_worktree(&work_dir, &worktree_dir, true)
                    .map_err(|error| error.to_string())?;
                threadlane_tools::remove_worktree_cargo_target_dir(&worktree_dir);
                if let Err(error) = threadlane_git::prune_worktrees(&work_dir) {
                    tracing::warn!("worktree prune failed: {error}");
                }
            }
            Self::remove_file_if_present(&canonical_session_file(&work_dir, &session_id))?;
            Self::remove_file_if_present(&session_file)?;
            if delete_worktree {
                if let Err(error) = threadlane_git::prune_worktrees(&work_dir) {
                    tracing::warn!("worktree prune failed: {error}");
                }
            }
        } else {
            std::fs::remove_file(session_file).map_err(|error| error.to_string())?;
        }
        self.finish_session_removal(&work_dir, &session_id);
        Ok(())
    }

    pub fn ensure_session_runtime(
        &mut self,
        work_dir: PathBuf,
        session_file: PathBuf,
    ) -> Arc<SessionRuntime> {
        if let Some(runtime) = self.daemon_core.runtime_for_file(&session_file) {
            return self.register_session_runtime(work_dir, session_file, runtime);
        }
        // WASI construction needs a larger stack than GPUI's worker stacks.
        let options = coding_agent_options(
            work_dir.clone(),
            session_file.clone(),
            self.selected_model.clone(),
            self.model_roles.clone(),
            self.browser_bridge.clone(),
        );
        let session_id = threadlane_daemon::core::DaemonCore::session_id_for_file(&session_file)
            .unwrap_or_default();
        let core = self.daemon_core.clone();
        let runtime_work_dir = work_dir.clone();
        let runtime_file = session_file.clone();
        let runtime = std::thread::Builder::new()
            .name("session-runtime-construct".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || core.get_or_create_runtime(
                &session_id, runtime_work_dir, runtime_file, options, None,
            ))
            .expect("failed to spawn session runtime constructor")
            .join()
            .expect("session runtime construction panicked");
        self.register_session_runtime(work_dir, session_file, runtime)
    }

    pub fn register_session_runtime(
        &mut self,
        work_dir: PathBuf,
        session_file: PathBuf,
        runtime: Arc<SessionRuntime>,
    ) -> Arc<SessionRuntime> {
        let session_id = threadlane_daemon::core::DaemonCore::session_id_for_file(&session_file)
            .unwrap_or_default();
        let runtime = self.daemon_core.register_runtime(
            &session_id, work_dir, session_file.clone(), runtime,
        );
        let same_runtime = self.scheduler_handles.get(&session_file)
            .and_then(|(owner, _)| owner.upgrade())
            .is_some_and(|owner| Arc::ptr_eq(&owner, &runtime));
        if !same_runtime {
            self.scheduler_handles.remove(&session_file);
            self.scheduler_results.remove(&session_file);
            if let Ok((handle, results)) = runtime.start_scheduler_supervisor_with_results() {
                self.scheduler_handles.insert(session_file.clone(), (Arc::downgrade(&runtime), handle));
                self.scheduler_results.insert(session_file.clone(), results);
            }
        }
        runtime
    }

    pub fn resolve_active_permission(
        &mut self,
        request_id: &str,
        decision: threadlane_permission::PermissionDecision,
    ) -> bool {
        let Some(session_id) = self.client.active_session_id.clone() else {
            return false;
        };
        let Some(request) = self.client.pending_permissions.get(&session_id) else {
            return false;
        };
        if request.id != request_id
            || (decision == threadlane_permission::PermissionDecision::AllowAlways
                && !request
                    .scopes
                    .contains(&threadlane_protocol::PermissionScope::Always))
            || (decision == threadlane_permission::PermissionDecision::AllowSession
                && !request
                    .scopes
                    .contains(&threadlane_protocol::PermissionScope::Session))
        {
            return false;
        }
        let Some(work_dir) = self.client.active_work_dir.clone() else {
            return false;
        };
        let session_file = self.session_file(&work_dir, &session_id);
        let resolved = if self.daemon_remote {
            self.dispatch_command(SessionCommand::AnswerPermission {
                session_id: session_id.clone(),
                request_id: request_id.to_string(),
                decision,
            });
            // Dispatch is fire-and-forget; the daemon reports a stale
            // request as DaemonError, so clearing optimistically is safe.
            true
        } else {
            self.daemon_core
                .runtime_for_file(&session_file)
                .is_some_and(|runtime| runtime.resolve_permission(request_id, decision))
        };
        if resolved {
            self.client.pending_permissions.remove(&session_id);
            if let Some(service) = &self.automation_service { service.resolved(session_id.clone(), request_id.into()); }
        }
        resolved
    }

    /// Releases a pending `ask_question` request without an answer.
    ///
    /// Explicit dismiss path for the question card's Dismiss button. The
    /// request stays pending until the user answers or dismisses, so the
    /// turn blocks waiting instead of silently continuing on a guess.
    pub fn resolve_active_question(&mut self, request_id: &str) -> bool {
        let Some(session_id) = self.client.active_session_id.clone() else {
            return false;
        };
        let Some(work_dir) = self.client.active_work_dir.clone() else {
            return false;
        };
        let session_file = self.session_file(&work_dir, &session_id);
        let answer = threadlane_protocol::QuestionAnswer::dismissed(request_id);
        let resolved = if self.daemon_remote {
            self.dispatch_command(SessionCommand::AnswerQuestion {
                session_id: session_id.clone(),
                answer,
            });
            true
        } else {
            self.daemon_core
                .runtime_for_file(&session_file)
                .is_some_and(|runtime| runtime.resolve_question(request_id, answer))
        };
        if resolved {
            self.client.pop_question(&session_id);
            if let Some(service) = &self.automation_service { service.resolved(session_id.clone(), request_id.into()); }
        }
        resolved
    }

    /// Resolves a pending `ask_question` request with the user's answers.
    /// Returns false when no runtime holds the request (stale UI).
    pub fn resolve_active_question_answer(
        &mut self,
        request_id: &str,
        answer: threadlane_protocol::QuestionAnswer,
    ) -> bool {
        let Some(session_id) = self.client.active_session_id.clone() else {
            return false;
        };
        let Some(work_dir) = self.client.active_work_dir.clone() else {
            return false;
        };
        let session_file = self.session_file(&work_dir, &session_id);
        let resolved = if self.daemon_remote {
            self.dispatch_command(SessionCommand::AnswerQuestion {
                session_id: session_id.clone(),
                answer,
            });
            true
        } else {
            self.daemon_core
                .runtime_for_file(&session_file)
                .is_some_and(|runtime| runtime.resolve_question(request_id, answer))
        };
        if resolved {
            self.client.pop_question(&session_id);
            if let Some(service) = &self.automation_service { service.resolved(session_id.clone(), request_id.into()); }
        }
        resolved
    }

    fn session_file(&self, work_dir: &Path, session_id: &str) -> PathBuf {
        self.client.projects
            .iter()
            .flat_map(|project| project.sessions.iter())
            .find(|session| {
                session.id == session_id
                    && (session.work_dir == work_dir || session.session_file.starts_with(work_dir))
            })
            .map(|session| session.session_file.clone())
            .unwrap_or_else(|| canonical_session_file(work_dir, session_id))
    }

    fn session_runtime_work_dir(&self, work_dir: &Path, session_id: &str) -> PathBuf {
        self.client.projects
            .iter()
            .find(|project| project.work_dir == work_dir)
            .and_then(|project| {
                project
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
            })
            .map(|session| session.runtime_work_dir.clone())
            .unwrap_or_else(|| work_dir.to_path_buf())
    }

    fn canonical_worktree_dir(work_dir: &Path, session_id: &str) -> PathBuf {
        work_dir.join(".threadlane/worktrees").join(session_id)
    }

    fn session_worktree_path(&self, work_dir: &Path, session_id: &str) -> Option<PathBuf> {
        if let Some(path) = self.client.projects
            .iter()
            .find(|project| project.work_dir == work_dir)
            .and_then(|project| {
                project
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id && session.is_worktree)
            })
            .map(|session| session.runtime_work_dir.clone())
        {
            return Some(path);
        }
        let stub = canonical_session_file(work_dir, session_id);
        let store = JsonlStore::open_read_only(&stub).ok()?;
        let facts = store.facts();
        if facts
            .get("is_worktree")
            .is_some_and(|value| value == "true")
        {
            let canonical_work_dir =
                std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
            Some(crate::discovery::effective_session_work_dir(
                &canonical_work_dir,
                session_id,
                &facts,
            ))
        } else {
            None
        }
    }

    fn remove_file_if_present(path: &Path) -> Result<(), String> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn projection_key(session_id: &str, session_file: &Path) -> SessionProjectionKey {
        SessionProjectionKey {
            session_id: session_id.to_owned(),
            session_file: session_file.to_path_buf(),
        }
    }

    fn session_projection_key(&self, work_dir: &Path, session_id: &str) -> SessionProjectionKey {
        Self::projection_key(session_id, &self.session_file(work_dir, session_id))
    }

    fn active_session_projection_key(&self) -> Option<SessionProjectionKey> {
        let work_dir = self.client.active_work_dir.as_deref()?;
        let session_id = self.client.active_session_id.as_deref()?;
        Some(self.session_projection_key(work_dir, session_id))
    }

    pub fn active_session_matches(&self, session_id: &str, session_file: &Path) -> bool {
        self.active_session_projection_key()
            .is_some_and(|active| active == Self::projection_key(session_id, session_file))
    }

    pub fn active_session_is_loading(&self) -> bool {
        self.pending_hydrations.iter().any(|pending| {
            pending.reload_messages
                && self.active_session_matches(&pending.session_id, &pending.session_file)
        }) || self
            .active_session_projection_key()
            .is_some_and(|key| self.in_flight_hydrations.contains_key(&key))
    }
    pub fn active_session_info(&self) -> Option<&SessionInfo> {
        let work_dir = self.client.active_work_dir.as_ref()?;
        let session_id = self.client.active_session_id.as_deref()?;
        self.client.projects
            .iter()
            .find(|project| &project.work_dir == work_dir)
            .and_then(|project| {
                project
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
            })
    }

    pub fn active_session_attention(&self) -> Option<SessionAttention> {
        self.active_session_info()
            .map(|session| self.session_attention(session))
    }

    pub fn take_pending_hydrations(&mut self) -> Vec<SessionHydrationRequest> {
        let requests = std::mem::take(&mut self.pending_hydrations);
        for request in requests.iter().filter(|request| request.reload_messages) {
            *self
                .in_flight_hydrations
                .entry(Self::projection_key(
                    &request.session_id,
                    &request.session_file,
                ))
                .or_default() += 1;
        }
        requests
    }

    pub fn finish_session_hydration(&mut self, session_id: &str, session_file: &Path) {
        let key = Self::projection_key(session_id, session_file);
        if let Some(count) = self.in_flight_hydrations.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                self.in_flight_hydrations.remove(&key);
            }
        }
    }

    fn finish_session_removal(&mut self, work_dir: &Path, session_id: &str) {
        let session_file = self.session_file(work_dir, session_id);
        self.drop_session_runtime(&session_file);
        self.client.pending_permissions.remove(session_id);
        self.client.pending_questions.remove(session_id);
        self.deferred_stream_events.remove(session_id);
        self.client.pending_composer_messages.remove(session_id);
        if self.presented_completion
            .as_ref()
            .is_some_and(|(key, _)| key.session_id == session_id)
        {
            self.presented_completion = None;
        }
        // Deleted sessions may drop their seen watermark; sessions that only
        // disappear from discovery keep it.
        self.session_seen_store_for(work_dir).prune(session_id);
        self.flush_session_seen(work_dir);
        if self.session_snooze_store_for(work_dir).remove(session_id) {
            self.flush_session_snooze(work_dir);
        }
        let pin_key = (work_dir.to_path_buf(), session_id.to_string());
        let was_pinned = self.client.pinned_sessions.contains(&pin_key);
        let mut pin_error = None;
        if was_pinned {
            let project_pinned: Vec<&str> = self.client.pinned_sessions
                .iter()
                .filter(|(w, id)| *w == work_dir && id.as_str() != session_id)
                .map(|(_, id)| id.as_str())
                .collect();
            if let Err(error) = persist_pinned_sessions(work_dir, &project_pinned) {
                tracing::warn!("failed to update pinned sessions during removal: {error}");
                pin_error = Some(error);
            } else {
                self.client.pinned_sessions.remove(&pin_key);
            }
        }
        self.acp_config_options
            .remove(&Self::projection_key(session_id, &session_file));
        let sessions = discover_sessions_in_project(work_dir);
        self.baseline_session_seen(&sessions);
        if let Some(project) = self.client.projects
            .iter_mut()
            .find(|project| project.work_dir == work_dir)
        {
            project.sessions = sessions;
            self.load_pinned_sessions(work_dir);
        }

        let removed_active = self.client.active_work_dir.as_deref() == Some(work_dir)
            && self.client.active_session_id.as_deref() == Some(session_id);
        if !removed_active {
            if let Some(error) = pin_error {
                self.client.session_status = Some(format!("Failed to update pinned sessions: {error}"));
            }
            return;
        }

        self.client.active_session_id = None;
        self.is_new_task = true;
        self.client.messages = Arc::new(Vec::new());
        self.client.active_plan = SessionPlan::default();
        self.client.is_generating = false;
        self.client.session_status = pin_error
            .as_ref()
            .map(|error| format!("Failed to update pinned sessions: {error}"));
        let next_session = self.client.projects
            .iter()
            .flat_map(|project| project.sessions.iter())
            .next()
            .map(|session| (session.work_dir.clone(), session.id.clone()));
        if let Some((next_work_dir, next_session_id)) = next_session {
            let _ = self.select_session(next_work_dir, next_session_id);
            if let Some(error) = pin_error {
                self.client.session_status = Some(format!("Failed to update pinned sessions: {error}"));
            }
        }
    }

    pub fn session_is_generating(&self, session_file: &Path) -> bool {
        if self
            .worktree_setups
            .values()
            .any(|s| s.session_file == session_file && s.error.is_none())
        {
            return true;
        }
        self.daemon_core
            .runtime_for_file(session_file)
            .is_some_and(|runtime| runtime.is_generating())
    }

    /// Resolve a PR task within its project or exact checkout, preferring the
    /// active available task. Live checkout state supersedes saved branch metadata.
    pub fn linked_pr_session(&self, work_dir: &Path, branch: &str) -> Option<&SessionInfo> {
        if branch.is_empty() {
            return None;
        }
        self.client.projects
            .iter()
            .flat_map(|project| &project.sessions)
            .filter(|session| {
                (session.work_dir == work_dir || session.runtime_work_dir == work_dir)
                    && match self.git_statuses.get(&session.runtime_work_dir) {
                        Some(status) => !status.detached && status.branch.as_deref() == Some(branch),
                        None => session.git_branch.as_deref() == Some(branch),
                    }
            })
            .min_by_key(|session| {
                let active = self.client.active_work_dir.as_ref() == Some(&session.work_dir)
                    && self.client.active_session_id.as_deref() == Some(session.id.as_str());
                (!session.worktree_available, !active)
            })
    }

    /// Auto-address new PR review feedback for an open PR.
    ///
    /// Only active (non-archived) sessions reach this path: archived sessions
    /// live under `.threadlane/sessions/archive/` and are excluded from
    /// discovery, so `sync_session_prs` never polls them. Returns the queued
    /// prompt when a new agent turn was started.
    pub fn auto_address_pr_reviews(
        &mut self,
        work_dir: PathBuf,
        branch: String,
        pr: &threadlane_git::GitHubPrInfo,
    ) -> Option<String> {
        if !self.auto_address_pr_reviews_enabled {
            return None;
        }
        if !pr.state.eq_ignore_ascii_case("open") {
            return None;
        }
        let session = self.linked_pr_session(&work_dir, &branch)?;
        if !session.worktree_available {
            return None;
        }
        let work_dir = session.work_dir.clone();
        let session_id = session.id.clone();
        let session_file = session.session_file.clone();
        let runtime_work_dir = session.runtime_work_dir.clone();
        // Guard against overwriting uncommitted user work when agent is not generating.
        if !self.session_is_generating(&session_file) {
            if let Some(status) = self.git_statuses.get(&runtime_work_dir) {
                if status.has_changes {
                    return None;
                }
            }
        }

        let feedback_items = threadlane_git::collect_actionable_pr_feedback(pr);
        if feedback_items.is_empty() {
            return None;
        }

        let current_store = self
            .pr_review_tracking
            .entry(work_dir.clone())
            .or_insert_with(|| threadlane_git::load_pr_review_tracking(&work_dir))
            .clone();
        let mut candidate_store = current_store;

        let new_items = match threadlane_git::check_and_record_fresh_feedback(
            &mut candidate_store,
            &branch,
            &feedback_items,
        ) {
            threadlane_git::FeedbackSyncResult::UpToDate => return None,
            threadlane_git::FeedbackSyncResult::NewFeedback(items) => items,
        };

        let prompt = threadlane_git::build_auto_address_prompt(pr.number, &branch, &new_items);
        let runtime = self.ensure_session_runtime(runtime_work_dir.clone(), session_file);
        if runtime.is_generating() {
            // An active turn will pick the queued follow-up up via
            // `run_scheduled_agent_work`; queueing alone never starts a run.
            if runtime
                .work_handle
                .try_queue_follow_up_with_images(prompt.clone(), Vec::new())
                .is_err()
            {
                return None;
            }
        } else {
            let model = runtime.model().to_owned();
            let reasoning_effort = self.reasoning_effort;
            let (api_key, _) = threadlane_coding_agent::credentials::provider_credentials(&model);
            if api_key.is_empty() && !threadlane_acp_engine::is_acp_model(&model) {
                return None;
            }
            let pending_acp = threadlane_acp_engine::acp_agent_id(&model)
                .map(|agent_id| self.take_pending_acp_config(agent_id))
                .unwrap_or_default();
            if crate::chat::execute_prompt(
                runtime,
                runtime_work_dir,
                session_id.clone(),
                prompt.clone(),
                Vec::new(),
                reasoning_effort,
                self.stream_tx.clone(),
                pending_acp,
            )
            .is_err()
            {
                return None;
            }
            if self.client.active_session_id.as_deref() == Some(&session_id) {
                self.client.is_generating = true;
                self.client.session_status = Some("Working…".into());
            }
        }
        self.pr_review_tracking
            .insert(work_dir.clone(), candidate_store);
        if let Some(store) = self.pr_review_tracking.get(&work_dir) {
            if let Err(error) = threadlane_git::save_pr_review_tracking(&work_dir, store) {
                tracing::warn!("failed to persist PR review tracking: {error}");
            }
        }
        let echo_id = format!("pr-review-{session_id}-{}", self.client.messages.len());
        self.push_optimistic_follow_up(&session_id, prompt.clone(), echo_id);
        Some(prompt)
    }

    /// Manually address actionable review feedback for a PR on demand.
    ///
    /// Unlike auto-addressing, this processes all current actionable review comments,
    /// starts the linked session immediately, and marks feedback seen only after dispatch succeeds.
    pub fn address_pr_reviews_manual(
        &mut self,
        work_dir: PathBuf,
        branch: String,
        pr: &threadlane_git::GitHubPrInfo,
    ) -> Result<String, String> {
        let feedback_items = threadlane_git::collect_actionable_pr_feedback(pr);
        if feedback_items.is_empty() {
            return Err("No actionable review feedback found on this PR.".into());
        }

        let prompt = threadlane_git::build_auto_address_prompt(pr.number, &branch, &feedback_items);

        let session = self
            .linked_pr_session(&work_dir, &branch)
            .ok_or_else(|| "No active task is linked to this pull request branch.".to_string())?;
        if !session.worktree_available {
            return Err(
                "The linked task’s checkout is missing. Restore it before addressing reviews.".into(),
            );
        }
        let work_dir = session.work_dir.clone();
        let session_id = session.id.clone();
        if self.client.active_work_dir.as_ref() != Some(&work_dir)
            || self.client.active_session_id.as_deref() != Some(session_id.as_str())
        {
            self.select_session(work_dir.clone(), session_id);
        }
        let model = self.selected_model.clone();
        let (api_key, _) = threadlane_coding_agent::credentials::provider_credentials(&model);
        if api_key.is_empty() && !threadlane_acp_engine::is_acp_model(&model) {
            return Err(format!(
                "No API key configured for model `{model}`. Open Settings and save the provider credential."
            ));
        }
        self.send_prompt(prompt.clone())?;

        let store = self
            .pr_review_tracking
            .entry(work_dir.clone())
            .or_insert_with(|| threadlane_git::load_pr_review_tracking(&work_dir));
        threadlane_git::mark_feedback_seen(store, &branch, &feedback_items);
        if let Err(error) = threadlane_git::save_pr_review_tracking(&work_dir, store) {
            tracing::warn!("failed to persist PR review tracking: {error}");
        }
        Ok(prompt)
    }

    pub fn session_attention(&self, session: &SessionInfo) -> SessionAttention {
        if let Some(setup) = self.worktree_setups.get(&session.id) {
            return if setup.error.is_some() {
                SessionAttention::NeedsYou
            } else {
                SessionAttention::Working
            };
        }
        let runtime = self.daemon_core.runtime_for_file(&session.session_file);
        let runtime_status = runtime.as_ref().map(|runtime| runtime.status());
        let is_active = self.client.active_work_dir.as_ref() == Some(&session.work_dir)
            && self.client.active_session_id.as_deref() == Some(session.id.as_str());
        let git_status = self
            .git_statuses
            .get(&session.runtime_work_dir)
            .or_else(|| {
                (!session.is_worktree)
                    .then(|| self.git_statuses.get(&session.work_dir))
                    .flatten()
            });
        let linked_pr = session
            .git_branch
            .as_ref()
            .and_then(|branch| {
                self.git_prs
                    .get(&(session.work_dir.clone(), branch.clone()))
            })
            .and_then(Option::as_ref)
            .or_else(|| {
                is_active
                    .then(|| git_status.and_then(|status| status.pr.as_ref()))
                    .flatten()
            });
        let linked_pr_is_active = linked_pr.is_some_and(|pr| {
            !pr.state.eq_ignore_ascii_case("merged")
                && !pr.state.eq_ignore_ascii_case("closed")
                && (pr.is_draft
                    || pr.state.eq_ignore_ascii_case("open")
                    || pr.state.eq_ignore_ascii_case("draft"))
        });
        // A checkout's git status is shared by local sessions, so expose
        // actionable work to every session that points at that checkout. A
        // known completed PR still owns its session and must not be revived
        // by stale changes left in the shared checkout.
        let actionable_git_work = git_status
            .is_some_and(|status| status.has_changes || status.ahead > 0 || status.pr_ready);
        let branch_is_actionable = session.git_branch.is_some()
            && (linked_pr_is_active || (linked_pr.is_none() && actionable_git_work));
        let deferred_work = self
            .deferred_stream_events
            .get(&session.id)
            .is_some_and(|events| {
                events
                    .iter()
                    .any(|event| matches!(event, SessionEvent::Scheduled { .. }))
            });
        let ready_work =
            deferred_work || branch_is_actionable || (linked_pr.is_none() && actionable_git_work);
        let deferred_failure = self
            .deferred_stream_events
            .get(&session.id)
            .is_some_and(|events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        SessionEvent::Scheduled {
                            result: Some(Err(_)),
                            ..
                        }
                    )
                })
            });
        derive_session_attention(
            self.client.pending_permissions.contains_key(&session.id)
                || self.client.pending_questions.contains_key(&session.id)
                || deferred_failure,
            &session.health,
            runtime_status.as_ref(),
            runtime.is_some_and(|runtime| runtime.is_generating())
                || (is_active && self.client.is_generating),
            ready_work,
        )
    }

    pub(crate) fn toggle_project_expanded(&mut self, work_dir: &Path) {
        if let Some(proj) = self.client.projects.iter_mut().find(|p| p.work_dir == work_dir) {
            proj.is_expanded = !proj.is_expanded;
        }
    }

    pub(crate) fn toggle_tool_activity(&mut self, tool_call_id: &str) {
        if let Some(activity) = self
            .messages_mut()
            .iter_mut()
            .flat_map(|message| message.tool_activities.iter_mut())
            .find(|activity| activity.id == tool_call_id)
        {
            activity.is_expanded = !activity.is_expanded;
        }
    }

    pub fn project_removal_disabled_reason(&self, work_dir: &Path) -> Option<String> {
        if self.daemon_remote {
            return Some("Remove projects from the computer hosting this workspace.".into());
        }
        let project = self
            .client
            .projects
            .iter()
            .find(|project| project.work_dir == work_dir)?;
        // Runtime files retain their canonical project ownership even when a
        // discovery refresh temporarily drops all sidebar session rows.
        let live_runtime = self.daemon_core.runtimes().iter().any(|(file, runtime)| {
            let belongs = file.starts_with(work_dir.join(".threadlane"))
                || project
                    .sessions
                    .iter()
                    .any(|session| session.session_file == *file);
            let id = threadlane_daemon::core::DaemonCore::session_id_for_file(file);
            belongs
                && (runtime.is_generating()
                    || runtime.scheduled_work_active()
                    || matches!(
                        runtime.status(),
                        threadlane_coding_agent::controller::SessionStatus::Working
                    )
                    || id.as_ref().is_some_and(|id| {
                        self.client.pending_permissions.contains_key(id)
                            || self.client.pending_questions.contains_key(id)
                    }))
        });
        if live_runtime
            || self
                .worktree_setups
                .values()
                .any(|setup| setup.project == work_dir && setup.error.is_none())
            || self.automations.snapshot.runs.iter().any(|run| {
                run.definition.project == work_dir && active_automation_status(run.status).is_some()
            })
        {
            return Some("Finish or stop active work before removing this project.".into());
        }
        None
    }

    /// Persist first: a failed registry write must leave the workspace unchanged.
    pub fn remove_project(&mut self, work_dir: &Path) -> Result<(), String> {
        if let Some(reason) = self.project_removal_disabled_reason(work_dir) {
            return Err(reason);
        }
        let Some(project) = self
            .client
            .projects
            .iter()
            .find(|project| project.work_dir == work_dir)
        else {
            return Ok(());
        };
        let removing_active = self.client.active_work_dir.as_deref() == Some(work_dir)
            || project
                .sessions
                .iter()
                .any(|session| self.client.active_session_id.as_ref() == Some(&session.id));
        threadlane_project::unregister_project(work_dir)?;
        self.pending_hydrations.retain(|pending| {
            !project
                .sessions
                .iter()
                .any(|session| session.session_file == pending.session_file)
        });
        self.daemon_core.detach_project(work_dir);
        self.client
            .projects
            .retain(|project| project.work_dir != work_dir);
        if self.client.sidebar_project_filter.as_deref() == Some(work_dir) {
            self.client.sidebar_project_filter = None;
        }
        if removing_active {
            self.client.active_work_dir = None;
            self.client.active_session_id = None;
            let page = self.workspace_page;
            self.begin_new_task();
            self.workspace_page = page;
            self.refresh_available_models();
        }
        Ok(())
    }
    pub(crate) fn attach_project(&mut self, raw_path: PathBuf) -> Result<(), String> {
        let canonical = std::fs::canonicalize(&raw_path).map_err(|e| e.to_string())?;
        if !canonical.is_dir() {
            return Err("Selected path is not a directory".into());
        }

        let record = threadlane_project::register_project(&canonical)?;

        let discovered_sessions = discover_sessions_in_project(&canonical);
        // Attaching a project counts as its first successful discovery pass:
        // pre-existing history is baselined so the sidebar never reports
        // already-known results as new.
        self.baseline_session_seen(&discovered_sessions);
        let session_to_restore = record
            .last_session_id
            .filter(|session_id| {
                discovered_sessions
                    .iter()
                    .any(|session| session.id == *session_id)
            })
            .or_else(|| {
                discovered_sessions
                    .first()
                    .map(|session| session.id.clone())
            });

        self.load_pinned_sessions(&canonical);
        for session in &discovered_sessions {
            if session.work_dir != canonical {
                self.load_pinned_sessions(&session.work_dir);
            }
        }
        if let Some(project) = self.client.projects
            .iter_mut()
            .find(|project| project.work_dir == canonical)
        {
            project.name = record.name;
            project.sessions = discovered_sessions;
            project.is_expanded = true;
        } else {
            self.client.projects.push(ProjectInfo {
                name: record.name,
                sessions: discovered_sessions,
                work_dir: canonical.clone(),
                is_expanded: true,
            });
        }
        self.daemon_core.attach_project(canonical.clone());

        if let Some(session_id) = session_to_restore {
            self.select_session(canonical, session_id);
        } else {
            self.client.active_work_dir = Some(canonical);
            self.client.active_session_id = None;
            self.is_new_task = true;
            self.client.messages = Arc::new(Vec::new());
            self.client.active_plan = SessionPlan::default();
            self.client.is_generating = false;
            self.client.session_status = None;
            self.refresh_available_models();
            self.refresh_orchestrator_mode();
        }
        Ok(())
    }

    pub fn active_worktree_setup(&self) -> Option<&crate::worktree_setup::WorktreeSetup> {
        self.client.active_session_id
            .as_ref()
            .and_then(|id| self.worktree_setups.get(id))
    }

    fn start_worktree_task(
        &mut self,
        text: String,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        use crate::worktree_setup::{SetupStage, WorktreeSetup};
        let project = self.client.active_work_dir
            .clone()
            .ok_or("Select a project first")?;
        let base = self
            .draft_worktree_base
            .clone()
            .ok_or("Wait for base branches to load, then select a base")?;
        let model = self.selected_model.clone();
        let (key, _) = threadlane_coding_agent::credentials::provider_credentials(&model);
        if key.is_empty() && !threadlane_acp_engine::is_acp_model(&model) {
            return Err("Configure a model provider before preparing a worktree".into());
        }
        let id = format!(
            "session_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let setup = WorktreeSetup {
            session_file: project
                .join(".threadlane/sessions")
                .join(format!("{id}.jsonl")),
            worktree: Self::canonical_worktree_dir(&project, &id),
            project: project.clone(),
            session_id: id.clone(),
            base,
            branch: None,
            stage: SetupStage::Naming,
            error: None,
            text,
            images,
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            model: model.clone(),
            effort: self.reasoning_effort,
            acp_config: threadlane_acp_engine::acp_agent_id(&model)
                .map(|agent| self.take_pending_acp_config(agent))
                .unwrap_or_default(),
        };
        if self.daemon_remote {
            self.dispatch_command(SessionCommand::PrepareWorktree {
                setup: setup.clone(),
            });
        } else {
            crate::worktree_setup::persist_request(&setup)?;
            let options = coding_agent_options(
                project.clone(),
                setup.session_file.clone(),
                model,
                self.model_roles.clone(),
                self.browser_bridge.clone(),
            );
            crate::worktree_setup::start(
                self.daemon_core.clone(),
                setup.clone(),
                options,
                self.stream_tx.clone(),
            )?;
        }
        if let Some(info) = self.client.projects.iter_mut().find(|p| p.work_dir == project) {
            info.sessions.insert(
                0,
                SessionInfo {
                    id: id.clone(),
                    title: threadlane_runtime::titles::normalize_session_title(&setup.text),
                    work_dir: project.clone(),
                    runtime_work_dir: setup.worktree.clone(),
                    session_file: setup.session_file.clone(),
                    updated_at: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    health: SessionHealth::Working,
                    git_branch: None,
                    github_issue: None,
                    is_worktree: true,
                    worktree_available: false,
                    completion_summary: SessionCompletionSummary::Unknown,
                },
            );
        }
        self.register_session_seen(&project, &id);
        self.register_interactive_completion_alert(&project, &id);
        self.worktree_setups.insert(id.clone(), setup);
        self.select_session(project, id);
        self.composer_text.clear();
        Ok(())
    }

    pub fn retry_worktree_setup(&mut self) {
        let Some(mut setup) = self
            .active_worktree_setup()
            .filter(|s| s.error.is_some())
            .cloned()
        else {
            return;
        };
        setup.error = None;
        setup.stage = crate::worktree_setup::SetupStage::Naming;
        if self.daemon_remote {
            self.dispatch_command(SessionCommand::PrepareWorktree {
                setup: setup.clone(),
            });
            self.worktree_setups.insert(setup.session_id.clone(), setup);
            self.client.is_generating = true;
            return;
        }
        let options = coding_agent_options(
            setup.project.clone(),
            setup.session_file.clone(),
            setup.model.clone(),
            self.model_roles.clone(),
            self.browser_bridge.clone(),
        );
        match crate::worktree_setup::start(
                self.daemon_core.clone(),
                setup.clone(),
                options,
                self.stream_tx.clone(),
            ) {
            Ok(()) => {
                self.worktree_setups.insert(setup.session_id.clone(), setup);
                self.client.is_generating = true;
            }
            Err(error) => self.client.session_status = Some(error),
        }
    }

    fn cleanup_cancelled_worktree(&mut self, setup: &crate::worktree_setup::WorktreeSetup) {
        if let Some(runtime) = self.daemon_core.runtime_for_file(&setup.session_file) {
            self.daemon_core.release_cancelled_runtime(&setup.session_file, &runtime);
            if self.daemon_core.runtime_for_file(&setup.session_file).is_some() {
                self.worktree_setups.remove(&setup.session_id);
                self.client.session_status = Some("Cancelled setup; retained checkout used by another client".into());
                self.request_session_refresh(&setup.project);
                return;
            }
        }
        self.scheduler_handles.remove(&setup.session_file);
        self.scheduler_results.remove(&setup.session_file);
        self.worktree_setups.remove(&setup.session_id);
        match crate::worktree_setup::cleanup_cancelled(setup) {
            Ok(()) => {
                if let Some(project) = self.client.projects
                    .iter_mut()
                    .find(|p| p.work_dir == setup.project)
                {
                    project.sessions.retain(|s| s.id != setup.session_id);
                }
            }
            Err(error) => {
                self.client.session_status = Some(format!("Could not clean up cancelled setup: {error}"));
            }
        }
        self.request_session_refresh(&setup.project);
    }

    fn finish_worktree_setup(
        &mut self,
        id: &str,
        result: Result<SessionInfo, String>,
    ) {
        let Some(setup) = self.worktree_setups.get(id).cloned() else {
            return;
        };
        let active = self.client.active_session_id.as_deref() == Some(id);
        if setup.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            drop(result);
            // Serialize cancellation with producer publication. The returned
            // handle is the exact prepared runtime and is safe to release only
            // when no other client has retained it.
            if let Some(runtime) = crate::runtimes::cancel_prepared_runtime(
                id,
                &setup.cancelled,
            ) {
                self.daemon_core
                    .release_cancelled_runtime(&setup.session_file, &runtime);
            }
            if active {
                self.client.is_generating = false;
                self.client.session_status = Some("Worktree setup cancelled".into());
            }
            self.cleanup_cancelled_worktree(&setup);
            return;
        }
        let result = result.and_then(|session| {
            if let Some(project) = self.client.projects
                .iter_mut()
                .find(|p| p.work_dir == setup.project)
            {
                project.sessions.retain(|s| s.id != id);
                project.sessions.insert(0, session.clone());
            }
            if self.daemon_remote {
                // The daemon parked the prepared runtime under the session id
                // and owns the worktree checkout; the first prompt resolves it.
                self.dispatch_command(SessionCommand::SubmitPrompt {
                    session_id: id.to_string(),
                    work_dir: setup.worktree.clone(),
                    text: setup.text.clone(),
                    images: setup.images.clone(),
                    effort: Some(setup.effort),
                    acp_config: setup.acp_config.clone(),
                    model: Some(setup.model.clone()),
                });
                return Ok(());
            }
            let prepared = crate::runtimes::take_prepared_runtime(id).ok_or_else(|| {
                "Prepared session runtime was already claimed".to_string()
            })?;
            let runtime = self.register_session_runtime(
                setup.worktree.clone(),
                session.session_file,
                prepared,
            );
            crate::chat::execute_prompt(
                runtime,
                setup.worktree.clone(),
                id.to_string(),
                setup.text.clone(),
                setup.images.clone(),
                setup.effort,
                self.stream_tx.clone(),
                setup.acp_config.clone(),
            )
        });
        match result {
            Ok(()) => {
                crate::worktree_setup::clear_request(&setup);
                self.worktree_setups.remove(id);
                if active {
                    self.client.is_generating = true;
                    self.client.session_status = Some("Working…".into());
                }
                self.request_session_refresh(&setup.project);
            }
            Err(error) => {
                if let Some(setup) = self.worktree_setups.get_mut(id) {
                    setup.error = Some(error);
                }
                if active {
                    self.client.is_generating = false;
                    self.client.session_status = None;
                }
            }
        }
    }

    fn create_new_session(&mut self) -> Result<String, String> {
        let Some(work_dir) = self.client.active_work_dir.clone() else {
            return Err("No active project directory".into());
        };
        let sessions_dir = work_dir.join(".threadlane/sessions");
        std::fs::create_dir_all(&sessions_dir).map_err(|e| e.to_string())?;

        let now_nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let session_id = format!("session_{now_nanos}");
        let session_file = sessions_dir.join(format!("{session_id}.jsonl"));

        self.register_session_seen(&work_dir, &session_id);
        self.register_interactive_completion_alert(&work_dir, &session_id);

        threadlane_coding_agent::harness::CodingSessionHarness::append_fact_to_path(
            &session_file,
            "main",
            "reasoning_effort",
            self.reasoning_effort.label(),
            None,
        )
        .map_err(|error| format!("failed to persist reasoning effort: {error}"))?;

        let sessions = discover_sessions_in_project(&work_dir);
        self.baseline_session_seen(&sessions);
        if let Some(project) = self.client.projects
            .iter_mut()
            .find(|project| project.work_dir == work_dir)
        {
            project.sessions = sessions;
            self.load_pinned_sessions(&work_dir);
        }
        let _ = self.select_session(work_dir, session_id.clone());
        self.is_new_task = false;
        Ok(session_id)
    }

    pub fn start_issue_work(
        &mut self,
        work_dir: PathBuf,
        issue: threadlane_git::GitHubIssueRef,
        title: String,
    ) -> Result<String, String> {
        self.start_issue_work_with_options(
            work_dir,
            issue,
            title,
            self.selected_model.clone(),
            self.reasoning_effort,
            self.orchestrator_mode,
        )
    }

    pub fn start_issue_work_with_options(
        &mut self,
        work_dir: PathBuf,
        issue: threadlane_git::GitHubIssueRef,
        title: String,
        model: String,
        effort: ReasoningEffort,
        orchestrator_mode: OrchestratorMode,
    ) -> Result<String, String> {
        let (api_key, _) = threadlane_coding_agent::credentials::provider_credentials(&model);
        if api_key.is_empty() && !threadlane_acp_engine::is_acp_model(&model) {
            return Err(format!(
                "Connect the provider for `{model}` in Settings before starting the task."
            ));
        }
        self.start_issue_work_with_prompt(
            work_dir,
            issue,
            title,
            model,
            effort,
            orchestrator_mode,
            |state, prompt| state.send_prompt(prompt),
        )
    }

    fn start_issue_work_with_prompt<F>(
        &mut self,
        work_dir: PathBuf,
        issue: threadlane_git::GitHubIssueRef,
        title: String,
        model: String,
        effort: ReasoningEffort,
        orchestrator_mode: OrchestratorMode,
        accept_prompt: F,
    ) -> Result<String, String>
    where
        F: FnOnce(&mut Self, String) -> Result<(), String>,
    {
        if model.trim().is_empty() {
            return Err("Choose a model before starting the task.".into());
        }
        if self.daemon_remote {
            // Issue work creates the worktree and `.threadlane/` session
            // files inline below — that lifecycle is not daemon-side yet,
            // so a remote attachment cannot run it. Fail clearly rather
            // than touching the client's own filesystem.
            return Err(
                "GitHub issue work is not yet supported on remote daemons".into(),
            );
        }
        let work_dir = std::fs::canonicalize(work_dir).map_err(|error| error.to_string())?;
        let effort =
            threadlane_provider::model_registry::effective_effort(&model, effort, Some(&work_dir));
        if !threadlane_git::is_git_repo(&work_dir) {
            return Err("GitHub issue work requires a Git repository".into());
        }
        if threadlane_git::list_commits(&work_dir, 1)
            .map_err(|error| error.to_string())?
            .is_empty()
        {
            return Err("GitHub issue work requires an initial commit".into());
        }
        if !self.client.projects
            .iter()
            .any(|project| project.work_dir == work_dir)
        {
            return Err("GitHub issue work requires an attached project".into());
        }

        let session_id = format!(
            "session_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let suffix = session_id.rsplit('_').next().unwrap_or(&session_id);
        let branch = Self::issue_branch_name(
            issue.number,
            &title,
            &suffix[suffix.len().saturating_sub(6)..],
        );
        let session_file = canonical_session_file(&work_dir, &session_id);
        let worktree_dir = Self::canonical_worktree_dir(&work_dir, &session_id);
        if worktree_dir.exists() || session_file.exists() {
            return Err("Generated issue session path already exists".into());
        }
        self.register_session_seen(&work_dir, &session_id);
        self.register_interactive_completion_alert(&work_dir, &session_id);

        let cleanup = |work_dir: &Path, worktree_dir: &Path, session_file: &Path| {
            // Best-effort rollback of exactly what this setup created
            // (pre-existence is checked above, so nothing here is user
            // work); failures warn instead of masking the primary error.
            if let Err(error) = threadlane_git::remove_worktree(work_dir, worktree_dir, true) {
                tracing::warn!("issue setup rollback: worktree remove failed: {error}");
            }
            if let Err(error) = std::fs::remove_dir_all(worktree_dir) {
                tracing::warn!("issue setup rollback: worktree dir remove failed: {error}");
            }
            threadlane_tools::remove_worktree_cargo_target_dir(worktree_dir);
            if let Err(error) = Self::remove_file_if_present(session_file) {
                tracing::warn!("issue setup rollback: session file remove failed: {error}");
            }
        };
        if let Err(error) = threadlane_git::create_worktree(&work_dir, &worktree_dir, &branch) {
            cleanup(&work_dir, &worktree_dir, &session_file);
            return Err(error.to_string());
        }
        if let Err(error) = std::fs::create_dir_all(
            session_file
                .parent()
                .expect("issue session file has a parent"),
        ) {
            cleanup(&work_dir, &worktree_dir, &session_file);
            return Err(error.to_string());
        }

        let github_issue = match serde_json::to_string(&issue) {
            Ok(value) => value,
            Err(error) => {
                cleanup(&work_dir, &worktree_dir, &session_file);
                return Err(error.to_string());
            }
        };
        for (key, value) in [
            ("is_worktree", "true".to_string()),
            ("worktree_path", worktree_dir.to_string_lossy().to_string()),
            ("git_branch", branch.clone()),
            ("github_issue", github_issue),
            ("model", model.clone()),
            ("reasoning_effort", effort.label().to_string()),
            (
                "orchestrator_mode",
                serde_json::to_string(&orchestrator_mode)
                    .map(|mode| mode.trim_matches('"').to_string())
                    .unwrap_or_else(|_| "normal".to_string()),
            ),
            ("name", format!("#{} {title}", issue.number)),
        ] {
            if let Err(error) =
                threadlane_coding_agent::harness::CodingSessionHarness::append_fact_to_path(
                    &session_file,
                    "main",
                    key,
                    &value,
                    None,
                )
            {
                cleanup(&work_dir, &worktree_dir, &session_file);
                return Err(format!("failed to persist issue metadata: {error}"));
            }
        }

        let sessions = discover_sessions_in_project(&work_dir);
        self.baseline_session_seen(&sessions);
        if let Some(project) = self.client.projects
            .iter_mut()
            .find(|project| project.work_dir == work_dir)
        {
            project.sessions = sessions;
        }
        let selection = IssueWorkSelection::capture(self);
        self.selected_model = model.clone();
        self.reasoning_effort = effort;
        // Persist the dialog's mode choice to the project before the new
        // session's runtime is constructed so the first turn already routes
        // through it; the composer dropdown refreshes from the same store.
        let previous_mode =
            threadlane_project::subagent_settings::load(&work_dir).orchestrator_mode;
        if previous_mode != orchestrator_mode {
            let mut settings = threadlane_project::subagent_settings::load(&work_dir);
            settings.orchestrator_mode = orchestrator_mode;
            if let Err(error) =
                threadlane_project::subagent_settings::save(&work_dir, &settings)
            {
                selection.restore(self);
                return Err(format!("Could not save session mode: {error}"));
            }
        }
        self.orchestrator_mode = orchestrator_mode;
        self.select_session_with_persistence(work_dir.clone(), session_id.clone(), false);
        let prompt = threadlane_prompt::git_workflow::issue_task_prompt(
            &issue.url,
            issue.number,
            threadlane_acp_engine::is_acp_model(&model),
        );
        if let Err(error) = accept_prompt(self, prompt) {
            cleanup(&work_dir, &worktree_dir, &session_file);
            self.drop_session_runtime(&session_file);
            if previous_mode != orchestrator_mode {
                let mut settings = threadlane_project::subagent_settings::load(&work_dir);
                settings.orchestrator_mode = previous_mode;
                let _ = threadlane_project::subagent_settings::save(&work_dir, &settings);
            }
            let sessions = discover_sessions_in_project(&work_dir);
            self.baseline_session_seen(&sessions);
            if let Some(project) = self.client.projects
                .iter_mut()
                .find(|project| project.work_dir == work_dir)
            {
                project.sessions = sessions;
            }
            selection.restore(self);
            return Err(error);
        }
        self.persist_project_selection(&work_dir, Some(&session_id));
        Ok(session_id)
    }

    /// Hydrates trajectory, token usage, and metrics projections from durable harness records.
    #[cfg(test)]
    fn hydrate_session_projection(
        &mut self,
        session_id: &str,
        session_file: &Path,
    ) -> Result<(), String> {
        let result = compute_full_session_projection(session_file)?;
        let key = Self::projection_key(session_id, session_file);
        self.apply_run_timing(&key, result.run_timing);
        if let Some(diagnostics) = result.diagnostics {
            self.diagnostics_by_session.insert(key.clone(), diagnostics);
            self.diagnostics_revision = self.diagnostics_revision.wrapping_add(1);
        }
        self.trajectory_by_session
            .insert(key.clone(), result.trajectory);
        self.trajectory_epoch = self.trajectory_epoch.wrapping_add(1);
        self.subagents_by_session
            .insert(key.clone(), result.subagents);
        self.trajectory_revision = self.trajectory_revision.wrapping_add(1);
        self.client.session_metrics.insert(key.clone(), result.metrics);
        if let Some(token_efficiency) = result.token_efficiency {
            self.token_efficiency_by_session
                .insert(key.clone(), token_efficiency);
        }
        if let Some(context_window) = result.context_window {
            self.client.context_windows.insert(key.clone(), context_window);
        } else {
            self.client.context_windows.remove(&key);
        }
        self.client.session_token_usage.insert(key, result.token_usage);
        Ok(())
    }

    /// Applies a completed background projection if its session remains active.
    pub fn session_status_for_file(&self, session_file: &Path) -> Option<String> {
        self.daemon_core.runtime_for_file(session_file).and_then(|runtime| {
            threadlane_coding_agent::controller::runtime_status_text(runtime.status())
        })
    }

    pub fn apply_session_messages(
        &mut self,
        session_id: &str,
        session_file: &Path,
        mut messages: Vec<ChatMessageInfo>,
        presented_completion: Option<RunCompletionToken>,
    ) {
        if self.active_session_matches(session_id, session_file) {
            // Session creation queues hydration before the first prompt is
            // persisted by CodingAgent. Keep the optimistic user row while
            // that initial, still-empty projection is applied. Once the
            // durable transcript contains the prompt, the pending row is
            // naturally replaced by the persisted message.
            let optimistic_messages: Vec<_> = self.client.messages
                .iter()
                .filter(|message| {
                    ((message.id.starts_with("pending-user-") && message.role == MessageRole::User)
                        || (self.client.is_generating
                            && message.id.starts_with("streaming-")
                            && message.role == MessageRole::Assistant))
                        && !messages.iter().any(|hydrated| {
                            hydrated.role == message.role && hydrated.content == message.content
                        })
                })
                .cloned()
                .collect();
            messages.extend(optimistic_messages);
            self.client.messages = Arc::new(messages);
            // Only a successfully applied load may carry its captured token —
            // failed loads retain the marker, and the later full projection
            // can never acknowledge a newer token it observed afterward.
            self.presented_completion = presented_completion.map(|token| {
                (Self::projection_key(session_id, session_file), token)
            });
        }
    }
}

fn is_attachable_project_root(path: &Path) -> bool {
    path.parent().is_some()
}

/// Merge live-recorded trajectory entries over a fresh file projection.
/// Live entries carry `seq: None`; file entries carry durable sequence
/// numbers. Tool entries deduplicate on `correlation_id` (the tool call id
/// both sides record); other live entries deduplicate on category+summary.
/// Surviving live entries ran after the snapshot, so they append at the end.
pub fn merge_live_trajectory(
    fresh: Vec<TrajectoryEntry>,
    live: &[TrajectoryEntry],
) -> Vec<TrajectoryEntry> {
    let fresh_correlations: HashSet<String> = fresh
        .iter()
        .filter_map(|entry| entry.correlation_id.clone())
        .collect();
    let fresh_summaries: HashSet<(String, String)> = fresh
        .iter()
        .map(|entry| (entry.category.clone(), entry.summary.clone()))
        .collect();
    let mut merged = fresh;
    for entry in live {
        if entry.seq.is_some() {
            continue;
        }
        let covered = match entry.correlation_id.as_deref() {
            Some(correlation) => fresh_correlations.contains(correlation),
            None => fresh_summaries.contains(&(entry.category.clone(), entry.summary.clone())),
        };
        if !covered {
            merged.push(entry.clone());
        }
    }
    merged
}

/// Merge live subagent activity over a fresh file projection. Hydrated rows do
/// not retain the runtime batch identity, so prefer the durable child run id
/// when both sides have one and fall back to (batch run id, task index) only
/// for purely live rows.
pub fn merge_live_subagents(
    fresh: Vec<SubagentActivityInfo>,
    live: &[SubagentActivityInfo],
) -> Vec<SubagentActivityInfo> {
    let mut merged = fresh;
    for activity in live {
        let covered = merged.iter().any(|entry| {
            match (
                entry.journal_run_id.as_deref(),
                activity.journal_run_id.as_deref(),
            ) {
                (Some(entry_run_id), Some(activity_run_id)) => entry_run_id == activity_run_id,
                _ => {
                    entry.batch_run_id == activity.batch_run_id
                        && entry.task_index == activity.task_index
                }
            }
        });
        if !covered {
            merged.push(activity.clone());
        }
    }
    merged
}

impl AppState {
    /// Apply identity-matched projections, preserving live activity that arrived after the snapshot.
    pub fn apply_session_hydration(
        &mut self,
        session_id: &str,
        session_file: &Path,
        result: SessionProjectionResult,
    ) {
        if !self.active_session_matches(session_id, session_file) {
            return;
        }
        let key = Self::projection_key(session_id, session_file);
        self.client.active_plan = result.plan;
        self.apply_run_timing(&key, result.run_timing);
        // Hydration snapshots lag live execution: the file is parsed in the
        // background while tool/subagent events keep arriving, and deferred
        // replay on session switch already consumed its queue into these maps.
        // While the runtime is still generating, a wholesale replace would
        // drop that live activity, so merge it back over the fresh snapshot.
        let generating = if self.daemon_remote {
            self.client.is_generating
        } else {
            self.daemon_core
                .runtime_for_file(session_file)
                .is_some_and(|runtime| runtime.is_generating())
        };
        if generating {
            let live_trajectory = self.trajectory_by_session.remove(&key).unwrap_or_default();
            self.trajectory_by_session.insert(
                key.clone(),
                merge_live_trajectory(result.trajectory, &live_trajectory),
            );
            let live_subagents = self.subagents_by_session.remove(&key).unwrap_or_default();
            self.subagents_by_session.insert(
                key.clone(),
                merge_live_subagents(result.subagents, &live_subagents),
            );
        } else {
            self.trajectory_by_session
                .insert(key.clone(), result.trajectory);
            self.subagents_by_session
                .insert(key.clone(), result.subagents);
        }
        self.trajectory_epoch = self.trajectory_epoch.wrapping_add(1);
        self.trajectory_revision = self.trajectory_revision.wrapping_add(1);
        if let Some(diagnostics) = result.diagnostics {
            self.diagnostics_by_session.insert(key.clone(), diagnostics);
            self.diagnostics_revision = self.diagnostics_revision.wrapping_add(1);
        }
        self.client.session_metrics.insert(key.clone(), result.metrics);
        if let Some(token_efficiency) = result.token_efficiency {
            self.token_efficiency_by_session
                .insert(key.clone(), token_efficiency);
        }
        if let Some(context_window) = result.context_window {
            self.client.context_windows.insert(key.clone(), context_window);
        } else {
            self.client.context_windows.remove(&key);
        }
        self.client.session_token_usage.insert(key, result.token_usage);
    }

    /// Applies a daemon wire snapshot (attach mid-run: snapshot first, live
    /// tail after). Wire snapshots cannot carry the daemon-local diagnostics
    /// and token-efficiency panels, so whatever the UI already computed is
    /// preserved rather than blanked.
    fn apply_session_snapshot(
        &mut self,
        session_id: &str,
        snapshot: threadlane_protocol::daemon::SessionSnapshot,
    ) -> bool {
        let session_file = snapshot.session.session_file.clone();
        if snapshot.session.id != session_id
            || (self.daemon_remote
                && !self.client.projects.iter().any(|project| {
                    project.work_dir == snapshot.session.work_dir
                        && project.sessions.iter().any(|session| {
                            session.id == session_id && session.session_file == session_file
                        })
                }))
        {
            return false;
        }
        self.finish_session_hydration(session_id, &session_file);
        if !self.active_session_matches(session_id, &session_file)
            || (self.daemon_remote
                && self.client.active_work_dir.as_ref() != Some(&snapshot.session.work_dir))
        {
            return false;
        }
        let presented_completion = if self.daemon_remote {
            // Replace a run-projection stub with the daemon's full metadata.
            // Remote completion watermarks must not scan/write this host's
            // filesystem using a daemon-owned transcript path.
            let keep_live_status = self
                .remote_live_status
                .as_ref()
                .is_some_and(|(key, epoch)| {
                    key == &Self::projection_key(session_id, &session_file)
                        && *epoch == self.daemon_client.file_search_connection_epoch()
                });
            let mut session_info = snapshot.session.clone();
            if keep_live_status {
                if self.client.is_generating {
                    session_info.health = SessionHealth::Working;
                } else if matches!(session_info.health, SessionHealth::Working) {
                    session_info.health = SessionHealth::Healthy;
                }
            } else {
                self.client.is_generating = matches!(session_info.health, SessionHealth::Working);
            }
            if !keep_live_status
                || matches!(
                    self.client.session_status.as_deref(),
                    Some("Loading session…" | "Reconciling session…")
                )
            {
                self.client.session_status = None;
            }
            if let Some(session) = self
                .client
                .projects
                .iter_mut()
                .filter(|project| project.work_dir == snapshot.session.work_dir)
                .flat_map(|project| project.sessions.iter_mut())
                .find(|session| session.id == session_id && session.session_file == session_file)
            {
                *session = session_info;
            }
            None
        } else {
            compute_latest_run_completion(&session_file).ok().flatten()
        };
        self.apply_session_messages(
            session_id,
            &session_file,
            snapshot.messages,
            presented_completion,
        );
        self.apply_session_hydration(
            session_id,
            &session_file,
            crate::types::SessionProjectionResult {
                run_timing: snapshot.run_timing,
                plan: snapshot.plan,
                trajectory: snapshot.trajectory,
                subagents: snapshot.subagents,
                diagnostics: None,
                metrics: snapshot.metrics,
                token_efficiency: None,
                token_usage: snapshot.token_usage,
                context_window: snapshot.context_window,
            },
        );
        true
    }

    /// Terminal views use this to reach daemon-hosted PTYs: commands are
    /// forwarded to the attached daemon in send order; frames stream back
    /// on `events()` routed by terminal_id.
    pub fn terminal_bus(&self) -> TerminalBus {
        TerminalBus {
            commands: self.terminal_command_tx.clone(),
            events: self.terminal_event_tx.clone(),
        }
    }

    /// Begin sharing this embedded daemon with thin clients on the LAN.
    /// The bind runs on the shared Tokio executor; its result is committed
    /// through [`finish_pairing_start`](Self::finish_pairing_start).
    pub fn start_pairing(
        &mut self,
    ) -> Result<
        tokio::task::JoinHandle<Result<threadlane_daemon::pairing::PairingServer, String>>,
        String,
    > {
        self.pairing_error = None;
        if self.pairing.is_some() {
            return Err("device pairing is already running".to_string());
        }
        if self.pairing_starting {
            return Err("device pairing is already starting".to_string());
        }
        if self.pairing_remove_all_pending {
            return Err("device sharing removal is still in progress".to_string());
        }
        if !self.pairing_restore_allowed {
            return Err("device pairing is unavailable in isolated test state".to_string());
        }
        if self.daemon_remote {
            // Sessions live in the attached daemon process; re-serving the
            // local empty core would show a paired client nothing.
            return Err(
                "device pairing needs the embedded session core (unset THREADLANE_DAEMON_URL)"
                    .to_string(),
            );
        }
        let executor = crate::chat::executor()?;
        self.pairing_generation = self.pairing_generation.wrapping_add(1);
        self.pairing_starting = true;
        self.pairing_remove_all_pending = false;
        Ok(executor.spawn(threadlane_daemon::pairing::PairingServer::start(
            self.daemon_core.clone(),
        )))
    }

    pub fn pairing_generation(&self) -> u64 {
        self.pairing_generation
    }

    pub fn pairing_restore_task(
        &mut self,
    ) -> Option<(
        u64,
        tokio::task::JoinHandle<Result<Option<threadlane_daemon::pairing::PairingServer>, String>>,
    )> {
        if !self.pairing_restore_allowed
            || self.daemon_remote
            || self.pairing.is_some()
            || self.pairing_starting
            || self.pairing_remove_all_pending
        {
            return None;
        }
        let executor = match crate::chat::executor() {
            Ok(executor) => executor,
            Err(error) => {
                self.pairing_error = Some(error);
                return None;
            }
        };
        self.pairing_generation = self.pairing_generation.wrapping_add(1);
        self.pairing_starting = true;
        self.pairing_error = None;
        let generation = self.pairing_generation;
        Some((
            generation,
            executor.spawn(threadlane_daemon::pairing::PairingServer::restore(
                self.daemon_core.clone(),
            )),
        ))
    }

    pub fn finish_pairing_start(
        &mut self,
        generation: u64,
        result: Result<threadlane_daemon::pairing::PairingServer, String>,
    ) {
        if generation != self.pairing_generation {
            if self.pairing_remove_all_pending {
                if let Ok(server) = result {
                    if let Ok(executor) = crate::chat::executor() {
                        executor.spawn(async move {
                            if let Err(error) = server.remove_all().await {
                                tracing::error!("could not revoke stale pairing start: {error}");
                            }
                        });
                    }
                }
            }
            return;
        }
        self.pairing_starting = false;
        match result {
            Ok(server) => self.pairing = Some(server),
            Err(error) => self.pairing_error = Some(error),
        }
    }

    pub fn finish_pairing_restore(
        &mut self,
        generation: u64,
        result: Result<Option<threadlane_daemon::pairing::PairingServer>, String>,
    ) {
        if generation != self.pairing_generation {
            if self.pairing_remove_all_pending {
                if let Ok(Some(server)) = result {
                    if let Ok(executor) = crate::chat::executor() {
                        executor.spawn(async move {
                            if let Err(error) = server.remove_all().await {
                                tracing::error!("could not revoke stale pairing restore: {error}");
                            }
                        });
                    }
                }
            }
            return;
        }
        self.pairing_starting = false;
        match result {
            Ok(Some(server)) => self.pairing = Some(server),
            Ok(None) => {}
            Err(error) => self.pairing_error = Some(error),
        }
    }

    pub fn pending_pairing_invitation(&self) -> Option<threadlane_daemon::pairing::PairingInfo> {
        self.pairing
            .as_ref()
            .and_then(|server| server.pending_invitation())
    }

    pub fn paired_devices(&self) -> Vec<threadlane_daemon::pairing::PairedDevice> {
        self.pairing
            .as_ref()
            .map_or_else(Vec::new, |server| server.devices())
    }

    pub fn begin_pairing(&mut self) -> Result<threadlane_daemon::pairing::PairingInfo, String> {
        self.pairing
            .as_mut()
            .ok_or_else(|| "device sharing is not running".to_string())?
            .begin_pairing()
    }

    pub fn remove_paired_device(&mut self, id: &str) -> Result<(), String> {
        self.pairing
            .as_mut()
            .ok_or_else(|| "device sharing is not running".to_string())?
            .remove_device(id)
    }

    /// Explicitly stop sharing and revoke all remembered devices.
    pub fn remove_all_pairing(
        &mut self,
    ) -> Result<
        (
            u64,
            tokio::task::JoinHandle<Result<(), String>>,
        ),
        String,
    > {
        if self.daemon_remote {
            return Err("device pairing needs the embedded session core".to_string());
        }
        if self.pairing_remove_all_pending {
            return Err("device sharing removal is already in progress".to_string());
        }
        if self.pairing_starting {
            return Err("device pairing is still starting".to_string());
        }
        let executor = crate::chat::executor()?;
        self.pairing_generation = self.pairing_generation.wrapping_add(1);
        let generation = self.pairing_generation;
        self.pairing_error = None;
        self.pairing_remove_all_pending = true;
        let server = self.pairing.take();
        let task = executor.spawn(async move {
            if let Some(server) = server {
                server.remove_all().await
            } else {
                threadlane_daemon::pairing::PairingServer::remove_all_saved().await
            }
        });
        Ok((generation, task))
    }

    pub fn finish_remove_all_pairing(
        &mut self,
        generation: u64,
        result: Result<(), String>,
    ) {
        if generation != self.pairing_generation {
            return;
        }
        self.pairing_remove_all_pending = false;
        if let Err(error) = result {
            self.pairing_error = Some(error);
        }
    }

    /// Stop accepting clients while preserving enabled state and trust.
    pub fn stop_pairing(&mut self) {
        self.pairing_generation = self.pairing_generation.wrapping_add(1);
        self.pairing_starting = false;
        self.pairing = None;
    }

    /// Sends a `SessionCommand` through the attached `DaemonClient` —
    /// fire-and-forget; failures arrive as `SessionEvent::DaemonError`.
    /// A command the client rejects outright (e.g. disconnected remote)
    /// also surfaces through the event stream rather than vanishing into
    /// a log line, since callers optimistically enter generating state.
    pub fn dispatch_command(&self, command: SessionCommand) {
        let client = self.daemon_client.clone();
        let stream_tx = self.stream_tx.clone();
        if let Ok(executor) = crate::chat::executor() {
            executor.spawn(async move {
                if let Err(error) = client.command(command).await {
                    tracing::warn!("daemon command failed: {error}");
                    let _ = stream_tx.send(SessionEvent::DaemonError {
                        session_id: None,
                        message: error,
                    });
                }
            });
        }
    }

    /// Sends a `CommandRequest` through the attached `DaemonClient` and
    /// routes the reply back into the stream as `SessionEvent::CommandResult`,
    /// so reply handling lives in `drain_chat_stream` like every other
    /// daemon-sourced outcome. A send/transport failure synthesizes the
    /// `Err` reply so any pending intent registered under `request_id`
    /// still resolves.
    fn dispatch_command_request(&self, request: CommandRequest) {
        let client = self.daemon_client.clone();
        let stream_tx = self.stream_tx.clone();
        if let Ok(executor) = crate::chat::executor() {
            executor.spawn(async move {
                let request_id = request.request_id;
                let result = client.command_request(request).await;
                let _ = stream_tx.send(SessionEvent::CommandResult {
                    request_id,
                    result,
                });
            });
        }
    }

    fn record_subagent_activity(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::SubagentQueued {
                run_id,
                task_index,
                agent,
                task,
            } => {
                let Some(subagents) = self.active_subagents_mut() else {
                    return;
                };
                if subagents.iter().any(|subagent| {
                    subagent.batch_run_id == *run_id && subagent.task_index == *task_index
                }) {
                    return;
                }
                subagents.push(SubagentActivityInfo {
                    batch_run_id: *run_id,
                    task_index: *task_index,
                    journal_run_id: None,
                    lane: None,
                    agent: agent.clone(),
                    task: task.clone(),
                    model: None,
                    status: SubagentActivityStatus::Queued,
                    messages: Vec::new(),
                    error: None,
                    isolation: None,
                });
            }
            AgentEvent::SubagentStarted {
                run_id,
                task_index,
                journal_run_id,
                lane,
                agent,
                task,
                model,
                isolation,
            } => {
                let Some(subagents) = self.active_subagents_mut() else {
                    return;
                };
                if let Some(subagent) = subagents.iter_mut().find(|subagent| {
                    subagent.batch_run_id == *run_id && subagent.task_index == *task_index
                }) {
                    subagent.journal_run_id = Some(journal_run_id.clone());
                    subagent.lane = Some(lane.clone());
                    subagent.agent = agent.clone();
                    subagent.task = task.clone();
                    subagent.model = Some(model.clone());
                    subagent.status = SubagentActivityStatus::Running;
                    subagent.isolation = isolation.clone();
                }
            }
            AgentEvent::SubagentUpdate {
                run_id,
                task_index,
                journal_run_id,
                lane,
                update,
            } => {
                let Some(subagents) = self.active_subagents_mut() else {
                    return;
                };
                let Some(subagent) = subagents.iter_mut().find(|subagent| {
                    subagent.batch_run_id == *run_id && subagent.task_index == *task_index
                }) else {
                    return;
                };
                subagent.journal_run_id = Some(journal_run_id.clone());
                subagent.lane = Some(lane.clone());
                subagent.status = SubagentActivityStatus::Running;
                match update {
                    SubagentProgressUpdate::TextDelta { delta } => {
                        if let Some(message) = subagent.messages.last_mut().filter(|message| {
                            message.role == MessageRole::Assistant
                                && message.streaming
                                && message.tool_activities.is_empty()
                        }) {
                            message.content.push_str(delta);
                        } else {
                            subagent.messages.push(ChatMessageInfo {
                                id: format!(
                                    "subagent-{journal_run_id}-{}",
                                    subagent.messages.len()
                                ),
                                role: MessageRole::Assistant,
                                content: delta.clone(),
                                tool_activities: Vec::new(),
                                streaming: true,
                                reasoning_content: None,
                                reasoning_expanded: false,
                                retry_prompt: None,
                            });
                        }
                    }
                    SubagentProgressUpdate::ReasoningDelta { delta } => {
                        if let Some(message) = subagent.messages.last_mut().filter(|message| {
                            message.role == MessageRole::Assistant && message.streaming
                        }) {
                            match &mut message.reasoning_content {
                                Some(reasoning) => reasoning.push_str(delta),
                                None => message.reasoning_content = Some(delta.clone()),
                            }
                        } else {
                            subagent.messages.push(ChatMessageInfo {
                                id: format!(
                                    "subagent-{journal_run_id}-{}",
                                    subagent.messages.len()
                                ),
                                role: MessageRole::Assistant,
                                content: String::new(),
                                tool_activities: Vec::new(),
                                streaming: true,
                                reasoning_content: Some(delta.clone()),
                                reasoning_expanded: false,
                                retry_prompt: None,
                            });
                        }
                    }
                    SubagentProgressUpdate::ToolStarted {
                        tool_call_id,
                        name,
                        arguments,
                    } => {
                        let activity = ToolActivityInfo {
                            id: tool_call_id.clone(),
                            category: "Working".into(),
                            title: name.clone(),
                            display_summary: tool_activity_display_summary(&tool_activity_summary(
                                name, arguments,
                            )),
                            detail: arguments.clone(),
                            arguments: arguments.clone(),
                            is_expanded: false,
                        };
                        if let Some(message) = subagent.messages.last_mut().filter(|message| {
                            message.role == MessageRole::Assistant && message.content.is_empty()
                        }) {
                            message.tool_activities.push(activity);
                        } else {
                            subagent.messages.push(ChatMessageInfo {
                                id: format!(
                                    "subagent-{journal_run_id}-{}",
                                    subagent.messages.len()
                                ),
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
                    SubagentProgressUpdate::ToolUpdated {
                        tool_call_id,
                        partial_result,
                    } => {
                        if let Some(activity) = subagent
                            .messages
                            .iter_mut()
                            .rev()
                            .flat_map(|message| message.tool_activities.iter_mut().rev())
                            .find(|activity| activity.id == *tool_call_id)
                        {
                            activity.detail = partial_result.clone();
                        }
                    }
                    SubagentProgressUpdate::ToolFinished {
                        tool_call_id,
                        result,
                        ..
                    } => {
                        if let Some(activity) = subagent
                            .messages
                            .iter_mut()
                            .rev()
                            .flat_map(|message| message.tool_activities.iter_mut().rev())
                            .find(|activity| activity.id == *tool_call_id)
                        {
                            activity.category = if result.is_error {
                                "Error".into()
                            } else {
                                "Completed".into()
                            };
                            activity.detail = result.content.clone();
                        }
                    }
                    SubagentProgressUpdate::Usage { .. } => {}
                    SubagentProgressUpdate::Error { error } => {
                        subagent.error = Some(error.clone());
                    }
                }
            }
            AgentEvent::SubagentFinished {
                run_id,
                task_index,
                succeeded,
                error,
                ..
            } => {
                let Some(subagents) = self.active_subagents_mut() else {
                    return;
                };
                if let Some(subagent) = subagents.iter_mut().find(|subagent| {
                    subagent.batch_run_id == *run_id && subagent.task_index == *task_index
                }) {
                    subagent.status = if *succeeded {
                        SubagentActivityStatus::Completed
                    } else {
                        SubagentActivityStatus::Failed
                    };
                    subagent.error = error.clone();
                    for message in &mut subagent.messages {
                        message.streaming = false;
                    }
                }
            }
            _ => {}
        }
    }

    fn record_trajectory(&mut self, session_id: &str, event: &AgentEvent) {
        let entry = match event {
            // Provider/tool-loop turn boundaries are ephemeral and have no
            // durable record, so they are intentionally excluded from the
            // canonical trajectory projection.
            AgentEvent::TurnStart { .. } | AgentEvent::TurnEnd { .. } => None,
            AgentEvent::ToolExecutionStart {
                name, arguments, ..
            } => Some(("Tool", format!("{name} running"), arguments.clone(), None)),
            AgentEvent::ToolExecutionEnd { name, result, .. } => Some((
                "Tool",
                format!(
                    "{name} {}",
                    if result.is_error {
                        "failed"
                    } else {
                        "finished"
                    }
                ),
                result.content.clone(),
                None,
            )),
            AgentEvent::SubagentQueued {
                task_index,
                agent,
                task,
                ..
            } => Some((
                "Subagent",
                format!("{agent} queued"),
                format!("Task {task_index}: {task}"),
                Some(agent.clone()),
            )),
            AgentEvent::SubagentStarted {
                journal_run_id,
                task_index,
                model,
                ..
            } => Some((
                "Subagent",
                format!("Subagent {task_index} started"),
                // The model names the lane's driver at a glance: frontier
                // main-model lanes vs. cheap Fusion sidekick lanes.
                format!("{model} · {journal_run_id}"),
                Some(journal_run_id.clone()),
            )),
            AgentEvent::SubagentFinished {
                journal_run_id,
                task_index,
                succeeded,
                error,
                ..
            } => Some((
                "Subagent",
                format!(
                    "Subagent {task_index} {}",
                    if *succeeded { "finished" } else { "failed" }
                ),
                error.clone().unwrap_or_else(|| journal_run_id.clone()),
                Some(journal_run_id.clone()),
            )),
            AgentEvent::SubagentRecovery {
                run_id,
                status,
                detail,
            } => Some((
                "Recovery",
                format!("{status:?}"),
                detail.clone().unwrap_or_else(|| run_id.clone()),
                Some(run_id.clone()),
            )),
            AgentEvent::AgentError { error } => {
                Some(("Error", "Agent error".into(), error.clone(), None))
            }
            AgentEvent::StreamRuleTriggered {
                rule_name,
                reminder,
                ..
            } => Some((
                "Rule",
                format!("{rule_name} triggered"),
                reminder.clone(),
                None,
            )),
            AgentEvent::FusionUpdate { model, message } => {
                Some(("Router", format!("Fusion → {model}"), message.clone(), None))
            }
            _ => None,
        };
        if let Some((category, summary, detail, lane)) = entry {
            let Some(key) = self
                .active_session_projection_key()
                .filter(|key| key.session_id == session_id)
            else {
                return;
            };
            self.trajectory_by_session
                .entry(key)
                .or_default()
                .push(TrajectoryEntry {
                    seq: None,
                    run_id: lane.clone(),
                    turn: None,
                    request: None,
                    category: category.into(),
                    summary,
                    detail,
                    lane,
                    correlation_id: match event {
                        AgentEvent::ToolExecutionStart { tool_call_id, .. }
                        | AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                            Some(tool_call_id.clone())
                        }
                        _ => None,
                    },
                    diagnostics: TrajectoryDiagnostics::default(),
                });
            self.trajectory_revision = self.trajectory_revision.wrapping_add(1);
        }
    }

    pub fn active_model_context_diagnostics(&self) -> Vec<TrajectoryEntry> {
        let Some(projection) = self
            .active_session_projection_key()
            .and_then(|key| self.diagnostics_by_session.get(&key))
        else {
            return Vec::new();
        };
        threadlane_daemon::projection::project_model_context_diagnostics(projection)
    }

    pub fn active_durable_event_diagnostics(&self) -> Vec<TrajectoryEntry> {
        let Some(projection) = self
            .active_session_projection_key()
            .and_then(|key| self.diagnostics_by_session.get(&key))
        else {
            return Vec::new();
        };
        threadlane_daemon::projection::project_durable_event_diagnostics(projection)
    }

    pub fn active_recovery_diagnostics(&self) -> Vec<TrajectoryEntry> {
        let Some(projection) = self
            .active_session_projection_key()
            .and_then(|key| self.diagnostics_by_session.get(&key))
        else {
            return Vec::new();
        };
        threadlane_daemon::projection::project_recovery_diagnostics(&projection.recovery)
    }

    pub fn active_trajectory(&self) -> &[TrajectoryEntry] {
        self.active_session_projection_key()
            .and_then(|key| self.trajectory_by_session.get(&key))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn trajectory_revision(&self) -> u64 {
        self.trajectory_revision
    }

    pub fn trajectory_epoch(&self) -> u64 {
        self.trajectory_epoch
    }

    pub fn diagnostics_revision(&self) -> u64 {
        self.diagnostics_revision
    }

    pub fn session_trajectory(&self, session_id: &str) -> &[TrajectoryEntry] {
        let key = self.client.active_work_dir
            .as_deref()
            .map(|work_dir| self.session_projection_key(work_dir, session_id));
        key.as_ref()
            .and_then(|key| self.trajectory_by_session.get(key))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn active_subagents(&self) -> &[SubagentActivityInfo] {
        self.active_session_projection_key()
            .and_then(|key| self.subagents_by_session.get(&key))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn active_subagents_mut(&mut self) -> Option<&mut Vec<SubagentActivityInfo>> {
        let key = self.active_session_projection_key()?;
        Some(self.subagents_by_session.entry(key).or_default())
    }

    pub fn active_session_metrics(&self) -> SessionMetricsInfo {
        self.active_session_projection_key()
            .and_then(|key| self.client.session_metrics.get(&key))
            .cloned()
            .unwrap_or_default()
    }

    pub fn active_token_efficiency(
        &self,
    ) -> Option<&threadlane_runtime::harness::TokenEfficiencyReport> {
        self.active_session_projection_key()
            .and_then(|key| self.token_efficiency_by_session.get(&key))
    }

    /// Asks the selected external agent what settings it offers.
    ///
    /// A no-op for a provider model, which has no agent to ask.
    fn request_acp_config_options(&mut self) {
        if !threadlane_acp_engine::is_acp_model(&self.selected_model) {
            return;
        }
        if self.daemon_remote {
            if let Some(session_id) = self.client.active_session_id.clone() {
                self.dispatch_command(SessionCommand::LoadAcpConfigOptions { session_id });
            }
            return;
        }
        let Some((runtime, session_id)) = self.active_session_runtime() else {
            return;
        };
        // A refusal here is not worth interrupting the user: this is a
        // background question, and the picker simply stays as it was.
        if let Err(error) =
            crate::chat::load_acp_config_options(runtime, session_id, self.stream_tx.clone())
        {
            tracing::debug!("Could not load ACP agent settings: {error}");
        }
    }

    /// Applies one of the selected external agent's settings.
    pub(crate) fn set_acp_config_option(&mut self, config_id: String, value: String) {
        if self.active_worktree_setup().is_some() {
            self.client.session_status =
                Some("Cancel worktree setup before changing agent settings".into());
            return;
        }
        if self.daemon_remote {
            if let Some(session_id) = self.client.active_session_id.clone() {
                self.dispatch_command(SessionCommand::SetAcpConfigOption {
                    session_id,
                    config_id,
                    value,
                });
                return;
            }
        }
        let Some((runtime, session_id)) = self.active_session_runtime() else {
            // No session yet (New task): remember the choice, show it
            // optimistically via the launch-time cache, and apply it to the
            // next runtime before its first turn. Without this the picker
            // silently keeps the agent's default (e.g. DeepSeek) no matter
            // what the user clicks.
            let Some(agent_id) =
                threadlane_acp_engine::acp_agent_id(&self.selected_model).map(str::to_string)
            else {
                self.client.session_status = Some("Open a session before changing agent settings".into());
                return;
            };
            // Refuse values the agent does not offer when the cache knows
            // them; an unknown cache (empty) still stores optimistically.
            let cached = threadlane_daemon::catalog::cached_acp_config_options(&agent_id);
            if !cached.is_empty() {
                let known = cached
                    .iter()
                    .find(|option| option.id == config_id)
                    .is_some_and(|option| option.has_choice(&value));
                if !known {
                    self.client.session_status = Some(format!("This agent does not offer '{value}'"));
                    return;
                }
            }
            self.pending_acp_config
                .entry(agent_id)
                .or_default()
                .insert(config_id, value);
            if self.client.session_status
                .as_deref()
                .is_some_and(|status| status == "Open a session before changing agent settings")
            {
                self.client.session_status = None;
            }
            return;
        };
        // A refusal here *is* worth surfacing: the user picked something and
        // it did not take effect.
        if let Err(error) = crate::chat::set_acp_config_option(
            runtime,
            session_id,
            config_id.clone(),
            value.clone(),
            self.stream_tx.clone(),
        ) {
            self.client.session_status = Some(error);
            return;
        }
        // A live session now owns the setting; drop any New-task pending for
        // the same agent so a later draft does not re-apply a stale choice.
        if let Some(agent_id) = threadlane_acp_engine::acp_agent_id(&self.selected_model) {
            if let Some(pending) = self.pending_acp_config.get_mut(agent_id) {
                pending.remove(&config_id);
                if pending.is_empty() {
                    self.pending_acp_config.remove(agent_id);
                }
            }
        }
    }

    /// Takes pending ACP `config_id -> value` selections for `agent_id`,
    /// clearing them so they apply exactly once to the next runtime.
    pub(crate) fn take_pending_acp_config(&mut self, agent_id: &str) -> Vec<(String, String)> {
        self.pending_acp_config
            .remove(agent_id)
            .map(|map| map.into_iter().collect())
            .unwrap_or_default()
    }

    /// The active session's runtime, creating it if this is its first use.
    ///
    /// Settings are held by the agent inside the runtime, so reaching them
    /// means having one — the same runtime a turn would use, so asking about
    /// settings and then sending a prompt talk to one agent, not two.
    fn active_session_runtime(&mut self) -> Option<(Arc<SessionRuntime>, String)> {
        if self.active_worktree_setup().is_some() {
            return None;
        }
        let work_dir = self.client.active_work_dir.clone()?;
        let session_id = self.client.active_session_id.clone()?;
        let session_file = self.session_file(&work_dir, &session_id);
        let runtime_work_dir = self.session_runtime_work_dir(&work_dir, &session_id);
        let runtime = self.ensure_session_runtime(runtime_work_dir, session_file);
        Some((runtime, session_id))
    }

    /// Settings the active session's external agent exposes.
    ///
    /// Falls back to the launch-time cache when this session's engine has
    /// not connected yet, so the picker offers the agent's models before the
    /// first spawn. An engine that connected and found nothing stays empty:
    /// only "never asked" falls back, never "asked and empty".
    /// Pending New-task selections override the cached current value so the
    /// picker and status bar show what will run, not the agent default.
    pub fn active_acp_config_options(&self) -> Vec<AcpConfigOption> {
        if !threadlane_acp_engine::is_acp_model(&self.selected_model) {
            return Vec::new();
        }
        if let Some(options) = self
            .active_session_projection_key()
            .and_then(|key| self.acp_config_options.get(&key))
        {
            return options.clone();
        }
        let agent_id = threadlane_acp_engine::acp_agent_id(&self.selected_model);
        let cached = agent_id
            .map(threadlane_daemon::catalog::cached_acp_config_options)
            .unwrap_or_default();
        match agent_id.and_then(|id| self.pending_acp_config.get(id)) {
            Some(pending) => threadlane_acp::apply_pending_config_values(cached, pending),
            None => cached,
        }
    }

    /// Model the active session's external agent reports it is running.
    ///
    /// Derived from the same settings the picker shows, so the status bar and
    /// the picker can never disagree about what is running.
    pub fn active_acp_model_label(&self) -> Option<String> {
        threadlane_acp::config_option_for(
            &self.active_acp_config_options(),
            threadlane_acp::ACP_CONFIG_CATEGORY_MODEL,
        )
        .and_then(AcpConfigOption::current_detail_label)
    }

    pub fn active_context_window(&self) -> Option<&ContextWindowInfo> {
        self.active_session_projection_key()
            .and_then(|key| self.client.context_windows.get(&key))
    }

    pub fn active_run_elapsed_seconds(&self) -> Option<u64> {
        let key = self.active_session_projection_key()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        self.client.run_timings
            .get(&key)?
            .elapsed_seconds(u64::try_from(now.as_millis()).ok()?, self.client.is_generating)
    }

    fn apply_run_timing(&mut self, key: &SessionProjectionKey, timing: Option<RunTiming>) {
        let Some(mut timing) = timing else { return };
        if let Some(current) = self.client.run_timings.get(key) {
            if timing.source_seq < current.source_seq {
                return;
            }
            // A late snapshot of the previous run must not revive its timer
            // after the user has submitted another prompt.
            timing.suppressed = current.suppressed && timing.start_seq <= current.start_seq;
        }
        self.client.run_timings.insert(key.clone(), timing);
    }

    /// Applies a durable runtime event to the session projection.
    ///
    /// Record-backed events remain authoritative for the session projection and
    /// are intentionally left for the existing journal hydration path.
    pub fn apply_durable_event(&mut self, session_id: &str, event: HarnessEvent) -> bool {
        match event.payload() {
            EventPayload::Agent(agent_event) => {
                self.drain_chat_stream(vec![SessionEvent::Agent {
                    session_id: session_id.to_owned(),
                    event: agent_event.clone(),
                }])
            }
            EventPayload::Fault(error) => self.drain_chat_stream(vec![SessionEvent::Agent {
                session_id: session_id.to_owned(),
                event: AgentEvent::AgentError {
                    error: error.clone(),
                },
            }]),
            _ => false,
        }
    }

    /// Creates a durable event cursor for a session runtime.
    pub fn subscribe_durable_events(
        &self,
        runtime: &SessionRuntime,
    ) -> Result<threadlane_runtime::harness::Subscription, threadlane_runtime::harness::EventError>
    {
        runtime.subscribe_durable_events()
    }
    /// Polls a runtime subscription and applies any durable agent events.
    ///
    /// The caller owns the cursor so subscriptions can be kept alongside the
    /// runtime that created them and recovered independently after a gap.
    pub fn poll_durable_events(
        &mut self,
        session_id: &str,
        runtime: &SessionRuntime,
        subscription: &mut threadlane_runtime::harness::Subscription,
    ) -> Result<bool, threadlane_runtime::harness::EventError> {
        let events = runtime.poll_durable_events(subscription)?;
        let mut changed = false;
        for event in events {
            changed |= self.apply_durable_event(session_id, event);
        }
        Ok(changed)
    }
    /// Observes serialized session_seen writes: a failed store is re-marked
    /// dirty so the next mutation retries the full write, and the warning
    /// surfaces once. A later success clears the flag. Returns true when
    /// visible state changed.
    pub fn drain_session_seen_write_results(&mut self) -> bool {
        let mut changed = false;
        while let Some(result) = self.session_seen_writer.try_recv_result() {
            match result.error {
                Some(error) => {
                    tracing::warn!(
                        "session_seen write failed for {}: {error}",
                        result.work_dir.display()
                    );
                    let key = Self::session_seen_key(&result.work_dir);
                    if let Some(store) = self.session_seen.get_mut(&key) {
                        store.mark_dirty();
                    }
                    if !self.session_seen_save_failed {
                        self.session_seen_save_failed = true;
                        self.client.session_status = Some(
                            "Could not save read state — the New result marker may return after restart."
                                .into(),
                        );
                    }
                    changed = true;
                }
                None => {
                    if self.session_seen_save_failed {
                        self.session_seen_save_failed = false;
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    pub fn drain_chat_stream(&mut self, mut events: Vec<SessionEvent>) -> bool {
        let seen_changed = self.drain_session_seen_write_results();
        let snooze_changed = self.drain_session_snooze_write_results();
        let scheduler_files: Vec<PathBuf> = self.scheduler_results.keys().cloned().collect();
        for session_file in scheduler_files {
            if let Some(receiver) = self.scheduler_results.get_mut(&session_file) {
                while let Ok(update) = receiver.try_recv() {
                    let session_id = self.client.projects
                        .iter()
                        .flat_map(|project| project.sessions.iter())
                        .find(|session| session.session_file == session_file)
                        .map(|session| session.id.clone())
                        .or_else(|| {
                            session_file
                                .file_stem()
                                .and_then(|stem| stem.to_str())
                                .map(str::to_owned)
                        })
                        .unwrap_or_else(|| session_file.to_string_lossy().into_owned());
                    events.push(match update {
                        SchedulerSupervisorEvent::Agent(event) => {
                            SessionEvent::Agent { session_id, event }
                        }
                        SchedulerSupervisorEvent::Completed(result) => SessionEvent::Scheduled {
                            session_id,
                            session_file: session_file.clone(),
                            result,
                        },
                    });
                }
            }
        }
        let active_session_id = self.client.active_session_id.clone();
        let deferred = active_session_id
            .as_ref()
            .and_then(|session_id| self.deferred_stream_events.remove(session_id))
            .unwrap_or_default()
            .into_iter();
        let mut changed = seen_changed | snooze_changed;

        for (is_new, event) in deferred
            .map(|event| (false, event))
            .chain(events.into_iter().map(|event| (true, event)))
        {
            // Deferred events predate this selection and cannot make a
            // subsequently requested hydration snapshot look stale.
            if self.daemon_remote && is_new {
                let lifecycle = match &event {
                    SessionEvent::Agent {
                        session_id,
                        event: AgentEvent::AgentStart | AgentEvent::AgentEnd { .. } | AgentEvent::AgentError { .. },
                    } => Some((session_id, None)),
                    SessionEvent::Finished { session_id, session_file }
                    | SessionEvent::Scheduled { session_id, session_file, .. } => {
                        Some((session_id, Some(session_file)))
                    }
                    _ => None,
                };
                if let Some((session_id, file)) = lifecycle {
                    if let Some(key) = self.active_session_projection_key().filter(|key| {
                        key.session_id == *session_id
                            && file.is_none_or(|file| *file == key.session_file)
                    }) {
                        self.remote_live_status =
                            Some((key, self.daemon_client.file_search_connection_epoch()));
                    }
                }
            }
            match event {
                SessionEvent::WorktreeBases { project, result } => {
                    if self.is_new_task && self.client.active_work_dir.as_ref() == Some(&project) {
                        match result {
                            Ok((default, branches)) => {
                                if !self
                                    .draft_worktree_base
                                    .as_ref()
                                    .is_some_and(|base| branches.contains(base))
                                {
                                    self.draft_worktree_base = Some(default);
                                }
                                self.draft_worktree_bases = branches;
                            }
                            Err(error) => {
                                self.client.session_status =
                                    Some(format!("Could not load base branches: {error}"))
                            }
                        }
                        changed = true;
                    }
                }
                SessionEvent::WorktreeProgress {
                    session_id,
                    stage,
                    branch,
                } => {
                    if let Some(setup) = self.worktree_setups.get_mut(&session_id) {
                        setup.stage = stage;
                        if branch.is_some() {
                            setup.branch = branch;
                        }
                        changed = true;
                    }
                }
                SessionEvent::WorktreePrepared { session_id, result } => {
                    self.finish_worktree_setup(&session_id, result);
                    changed = true;
                }
                SessionEvent::Agent { session_id, event }
                    if self.client.active_session_id.as_deref() == Some(&session_id) =>
                {
                    if self.daemon_remote {
                        match &event {
                            AgentEvent::AgentStart => {
                                self.client.is_generating = true;
                                self.client.session_status = None;
                                changed = true;
                            }
                            AgentEvent::AgentEnd { .. } => {
                                self.client.is_generating = false;
                                changed = true;
                            }
                            _ => {}
                        }
                    }
                    if matches!(&event, AgentEvent::AgentStart) {
                        if let Some(key) = self.active_session_projection_key() {
                            self.pending_hydrations.push(SessionHydrationRequest {
                                session_id: key.session_id,
                                session_file: key.session_file,
                                reload_messages: false,
                                runtime_options: None,
                            });
                        }
                    }
                    self.record_trajectory(&session_id, &event);
                    self.record_subagent_activity(&event);
                    // Metrics are best-effort: an event without a projection
                    // key (session switched mid-pump) still flows through
                    // the updates below, it just skips usage accounting.
                    if let Some(key) = self.active_session_projection_key() {
                        let metrics = self.client.session_metrics.entry(key.clone()).or_default();
                        match &event {
                            AgentEvent::AgentStart | AgentEvent::SubagentStarted { .. } => {
                                metrics.turns = metrics.turns.saturating_add(1)
                            }
                            AgentEvent::ToolExecutionStart { .. }
                            | AgentEvent::SubagentUpdate {
                                update: SubagentProgressUpdate::ToolStarted { .. },
                                ..
                            } => metrics.tool_calls = metrics.tool_calls.saturating_add(1),
                            AgentEvent::AgentEnd { usage }
                            | AgentEvent::SubagentUpdate {
                                update: SubagentProgressUpdate::Usage { usage },
                                ..
                            } => metrics.accumulate_usage(usage),
                            _ => {}
                        }
                    }
                    changed |= self.client.apply_agent_event(&session_id, event);
                }
                SessionEvent::Scheduled {
                    session_id,
                    session_file,
                    result,
                } => {
                    let active_file = self
                        .active_session_projection_key()
                        .map(|key| key.session_file);
                    if active_file.as_ref() != Some(&session_file) {
                        changed = true;
                        // A background completion must still reach the
                        // sidebar's New result marker; the deferred event
                        // only replays when the session is opened.
                        if let Some(work_dir) = self.session_work_dir_for_file(&session_file) {
                            self.request_session_refresh(&work_dir);
                        }
                        self.deferred_stream_events
                            .entry(session_id.clone())
                            .or_default()
                            .push(SessionEvent::Scheduled {
                                session_id,
                                session_file,
                                result,
                            });
                        continue;
                    }
                    changed = true;
                    self.client.is_generating = false;
                    let successful = !matches!(&result, Some(Err(_)));
                    let (role, content) = match result {
                        Some(Ok(content)) => (MessageRole::Assistant, content),
                        Some(Err(error)) => (MessageRole::Error, error),
                        None => (MessageRole::System, "Scheduled work completed".into()),
                    };
                    let message_id = format!("scheduled-{}", self.client.messages.len());
                    let status = if role == MessageRole::Error {
                        content.clone()
                    } else {
                        "Scheduled work completed".into()
                    };
                    self.messages_mut().push(ChatMessageInfo {
                        id: message_id,
                        role,
                        content,
                        tool_activities: Vec::new(),
                        streaming: false,
                        reasoning_content: None,
                        reasoning_expanded: false,
                        retry_prompt: None,
                    });
                    self.client.session_status = Some(status);
                    // A successful scheduled completion must also capture its
                    // presented-completion token via a real transcript load;
                    // the inline append above cannot acknowledge the marker.
                    if successful {
                        self.pending_hydrations.push(SessionHydrationRequest {
                            session_id: session_id.clone(),
                            session_file: session_file.clone(),
                            reload_messages: true,
                            runtime_options: None,
                        });
                    }
                    if let Some(work_dir) = self.session_work_dir_for_file(&session_file) {
                        self.request_session_refresh(&work_dir);
                    }
                }
                SessionEvent::Finished {
                    session_id,
                    session_file,
                } => {
                    self.client.pending_permissions.remove(&session_id);
                    self.client.pending_questions.remove(&session_id);
                    self.client.queued_questions.remove(&session_id);
                    if self.client.active_session_id.as_deref() != Some(&session_id) {
                        changed = true;
                        // A background completion must still reach the
                        // sidebar's New result marker; the deferred event
                        // only replays when the session is opened.
                        if let Some(work_dir) = self.session_work_dir_for_file(&session_file) {
                            self.request_session_refresh(&work_dir);
                        }
                        self.deferred_stream_events
                            .entry(session_id.clone())
                            .or_default()
                            .push(SessionEvent::Finished {
                                session_id,
                                session_file,
                            });
                        continue;
                    }
                    changed = true;
                    self.client.is_generating = false;
                    if let Some(subagents) = self.active_subagents_mut() {
                        for subagent in subagents.iter_mut().filter(|subagent| {
                            matches!(
                                subagent.status,
                                SubagentActivityStatus::Queued | SubagentActivityStatus::Running
                            )
                        }) {
                            subagent.status = SubagentActivityStatus::Cancelled;
                            if subagent.error.is_none() {
                                subagent.error =
                                    Some("Parent generation stopped before completion.".into());
                            }
                            for message in &mut subagent.messages {
                                message.streaming = false;
                            }
                        }
                    }
                    self.client.session_status = Some("Reconciling session…".into());
                    self.pending_hydrations.push(SessionHydrationRequest {
                        session_id: session_id.clone(),
                        session_file: session_file.clone(),
                        reload_messages: true,
                        runtime_options: None,
                    });
                    let runtime_is_stale =
                        self.daemon_core
                            .runtime_for_file(&session_file)
                            .is_some_and(|runtime| {
                                !runtime.is_generating()
                                    && (runtime.selected_model != self.selected_model
                                        || runtime.orchestrator_mode != self.orchestrator_mode)
                            });
                    if runtime_is_stale {
                        self.drop_session_runtime(&session_file);
                    }
                    if let Some(work_dir) = self.session_work_dir_for_file(&session_file) {
                        self.request_session_refresh(&work_dir);
                    }
                }
                SessionEvent::AcpConfigOptions {
                    session_id,
                    session_file,
                    runtime_instance,
                    options,
                    error,
                    failed_config,
                } => {
                    // Apply options only while the producing runtime instance
                    // is still the registered one — the wire-clean stand-in
                    // for the old `Weak<SessionRuntime>` identity check.
                    // Remote mode keeps no local runtimes to compare against;
                    // the daemon's own ordering is the staleness guard there.
                    if !self.daemon_remote {
                        let Some(current_instance) = self
                            .daemon_core
                            .runtime_for_file(&session_file)
                            .map(|runtime| runtime.instance_id())
                        else {
                            continue;
                        };
                        if current_instance != runtime_instance {
                            continue;
                        }
                    }
                    let is_active = self.active_session_matches(&session_id, &session_file);
                    if let Some(error) = error {
                        if let (Some((config_id, value)), Some(agent_id)) = (
                            failed_config,
                            threadlane_acp_engine::acp_agent_id(&self.selected_model),
                        ) {
                            self.pending_acp_config
                                .entry(agent_id.to_string())
                                .or_default()
                                .insert(config_id, value);
                        }
                        if is_active {
                            self.client.session_status = Some(error);
                            changed = true;
                        }
                        continue;
                    }
                    let key = Self::projection_key(&session_id, &session_file);
                    if options.is_empty() {
                        if self.acp_config_options.remove(&key).is_some() && is_active {
                            changed = true;
                        }
                    } else if self.acp_config_options.get(&key) != Some(&options) {
                        self.acp_config_options.insert(key, options);
                        if is_active {
                            changed = true;
                        }
                    }
                }
                SessionEvent::TitleGenerated {
                    session_id,
                    session_file,
                } => {
                    if let Some(work_dir) = session_file
                        .parent()
                        .and_then(Path::parent)
                        .and_then(Path::parent)
                    {
                        self.request_session_refresh(work_dir);
                    }
                    if self.client.active_session_id.as_deref() == Some(&session_id) {
                        changed = true;
                        self.refresh_active_session();
                    }
                }
                SessionEvent::Agent { session_id, event } => {
                    match &event {
                        AgentEvent::PermissionRequested { request } => {
                            self.client.pending_permissions
                                .insert(session_id.clone(), request.clone());
                            changed = true;
                        }
                        AgentEvent::AgentStart | AgentEvent::AgentError { .. } => changed = true,
                        _ => {}
                    }
                    self.deferred_stream_events
                        .entry(session_id.clone())
                        .or_default()
                        .push(SessionEvent::Agent { session_id, event });
                }
                // Daemon-hosted PTY frames route to the terminal that owns
                // the id; project deltas arrive via the file watcher.
                SessionEvent::TerminalEvent { event } => {
                    let _ = self.terminal_event_tx.send(event);
                }
                SessionEvent::FollowUpQueued {
                    session_id,
                    entry_id,
                } => {
                    self.bind_queued_echo(&session_id, &entry_id);
                    changed = true;
                }
                SessionEvent::CommandResult { request_id, result } => {
                    match result {
                        Ok(CommandResponse::CancelledQueuedMessage {
                            session_id,
                            entry_id,
                            text,
                            images,
                        }) => {
                            changed |= self.settle_queued_cancel(
                                request_id,
                                &session_id,
                                &entry_id,
                                text,
                                images,
                            );
                        }
                        Ok(_) => {
                            self.pending_queued_cancels.remove(&request_id);
                        }
                        Err(error) => {
                            if let Some(pending) =
                                self.pending_queued_cancels.remove(&request_id)
                            {
                                if error
                                    == threadlane_coding_agent::scheduler::QUEUED_ENTRY_NOT_PENDING
                                {
                                    // Authoritative: the entry already
                                    // left the queue — consumed into the
                                    // running turn or cancelled by
                                    // someone else — so the retained row
                                    // is stale, not truthful. Reconcile
                                    // it; the text already handed back
                                    // for an edit stays in the composer.
                                    self.remove_queued_echo(
                                        &pending.session_id,
                                        &pending.entry_id,
                                    );
                                    self.client.session_status =
                                        Some(error.clone());
                                } else {
                                    // The echo was never removed — the
                                    // row still truthfully shows the
                                    // entry as queued.
                                    self.client.session_status = Some(format!(
                                        "Could not remove queued message: {error}"
                                    ));
                                }
                                changed = true;
                            }
                            tracing::warn!("daemon request {request_id} failed: {error}");
                        }
                    }
                }
                // A queued entry left the daemon's queue: resolves a
                // parked intent when its request_id is ours (the journal
                // replay of a cancellation whose reply was lost to a
                // disconnect), and drops the retained echo either way —
                // including cancels issued by other clients.
                SessionEvent::QueuedEntryCancelled {
                    session_id,
                    entry_id,
                    request_id,
                    text,
                    images,
                } => {
                    if let Some(request_id) = request_id {
                        changed |= self.settle_queued_cancel(
                            request_id,
                            &session_id,
                            &entry_id,
                            text,
                            images,
                        );
                    } else {
                        changed |= self.remove_queued_echo(&session_id, &entry_id);
                    }
                }
                SessionEvent::ProjectChanged { mut project } => {
                    if self.daemon_remote {
                        let epoch = self.daemon_client.file_search_connection_epoch();
                        if !self.remote_inventory.as_ref().is_some_and(|(client, previous, _)| {
                            Arc::ptr_eq(client, &self.daemon_client) && *previous == epoch
                        }) {
                            self.remote_inventory = Some((self.daemon_client.clone(), epoch, HashSet::new()));
                        }
                        if let Some((_, _, projects)) = &mut self.remote_inventory {
                            projects.insert(project.work_dir.clone());
                        }
                        // A project snapshot can predate the run we just
                        // opened. Keep its exact in-flight identity until
                        // the matching session hydration supplies metadata.
                        if self.active_session_is_loading() {
                            if let Some(active) = self.active_session_info().filter(|session| session.work_dir == project.work_dir) {
                                if !project.sessions.iter().any(|session| session.id == active.id && session.session_file == active.session_file) {
                                    project.sessions.retain(|session| session.id != active.id);
                                    project.sessions.push(active.clone());
                                }
                            }
                        }
                        self.client.apply_event(SessionEvent::ProjectChanged { project });
                        changed = true;
                    }
                }
                SessionEvent::AutomationChanged { projection } => {
                    // Local mode uses the service watch, including its
                    // runtime handle. Remote mode has only the wire event.
                    if self.daemon_remote {
                        self.automations = crate::automation::Projection {
                            snapshot: projection.snapshot.clone(),
                            permissions: projection.permissions.clone(),
                            questions: projection.questions.clone(),
                            question_queues: projection.question_queues.clone().unwrap_or_default(),
                            error: projection.error.clone(),
                            ..Default::default()
                        };
                        self.client.apply_event(SessionEvent::AutomationChanged { projection });
                        changed = true;
                    }
                }
                SessionEvent::WorkspaceChanged { .. } => {
                    // Files/git surfaces hold their own daemon
                    // subscription and refresh off this event; AppState
                    // itself keeps no filesystem mirror to update.
                }
                SessionEvent::SessionSnapshot {
                    session_id,
                    snapshot,
                } => {
                    if self.apply_session_snapshot(&session_id, *snapshot) {
                        changed = true;
                    }
                }
                SessionEvent::DaemonError { message, .. } => {
                    tracing::warn!("daemon error: {message}");
                    // Dispatch failures surface here — the command path has
                    // no return channel on the wire.
                    self.client.session_status = Some(format!("Daemon: {message}"));
                    changed = true;
                }
                SessionEvent::SessionRemoved {
                    session_id,
                    session_file,
                } => {
                    // The daemon confirmed the delete — only now is the
                    // session's persisted data safe to drop.
                    let work_dir = self
                        .pending_remote_deletes
                        .remove(&session_id)
                        .or_else(|| self.session_work_dir_for_file(&session_file));
                    if let Some(work_dir) = work_dir {
                        self.finish_session_removal(&work_dir, &session_id);
                        changed = true;
                    }
                }
            }
        }
        // Stream events can flip a session to Working/Needs you — a snooze
        // must end on new work, never re-hide after it is answered.
        changed |= self.reconcile_session_snoozes();
        changed
    }

    /// True once per unseen computer-use trigger: a pending `computer`
    /// approval request, or fresh `computer_*` tool activity in the visible
    /// transcript. The chat pump uses this to open the mirror popup exactly
    /// once per new activity instead of once per pump tick.
    pub fn take_computer_mirror_trigger(&mut self) -> bool {
        // Bound de-duplication to requests and activities still observable in
        // state; completed/evicted activities must not leak one key forever.
        let mut visible = HashSet::new();
        for id in self.client.pending_permissions.keys() {
            visible.insert(format!("permission:{id}"));
        }
        for message in self.client.messages.iter() {
            for activity in message.tool_activities.iter() {
                if activity.title.starts_with("computer_") {
                    visible.insert(format!("tool:{}", activity.id));
                }
            }
        }
        self.mirror_seen.retain(|key| visible.contains(key));

        for (id, request) in &self.client.pending_permissions {
            if request.capability == "computer"
                && self.mirror_seen.insert(format!("permission:{id}"))
            {
                return true;
            }
        }
        let mut fresh = false;
        for message in self.client.messages.iter() {
            for activity in message.tool_activities.iter() {
                if activity.title.starts_with("computer_")
                    && self.mirror_seen.insert(format!("tool:{}", activity.id))
                {
                    fresh = true;
                }
            }
        }
        fresh
    }

    pub fn active_pending_composer_message(&self) -> Option<&str> {
        self.client.active_session_id
            .as_ref()
            .and_then(|session_id| self.client.pending_composer_messages.get(session_id))
            .map(|message| message.text.as_str())
    }

    pub(crate) fn stage_busy_message(
        &mut self,
        text: String,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        let session_id = self.client.active_session_id
            .clone()
            .ok_or_else(|| "No active session".to_string())?;
        if !self.client.is_generating {
            return Err("The session is no longer generating".into());
        }
        self.client.pending_composer_messages
            .insert(session_id, PendingComposerMessage { text, images });
        Ok(())
    }

    pub(crate) fn queue_pending_message(&mut self) -> Result<(), String> {
        if self.daemon_remote {
            let (text, images, session_id) = {
                let session_id = self.client.active_session_id
                    .clone()
                    .ok_or_else(|| "No active session".to_string())?;
                let pending = self.client.pending_composer_messages
                    .get(&session_id)
                    .cloned()
                    .ok_or_else(|| "No pending composer message".to_string())?;
                (pending.text, pending.images, session_id)
            };
            self.client.pending_composer_messages.remove(&session_id);
            // The daemon queues a follow-up itself when the session is still
            // generating (SubmitPrompt's generating branch).
            self.dispatch_command(SessionCommand::SubmitPrompt {
                session_id: session_id.clone(),
                work_dir: self.client.active_work_dir.clone().unwrap_or_default(),
                text: text.clone(),
                images,
                effort: Some(self.reasoning_effort),
                acp_config: Vec::new(),
                model: Some(self.selected_model.clone()),
            });
            self.push_optimistic_follow_up(&session_id, text, format!("queued-user-{session_id}"));
            self.client.session_status = Some("Message queued…".into());
            return Ok(());
        }
        let (runtime, session_id, text, images) = self.pending_runtime_message()?;
        let entry_id = runtime
            .work_handle
            .try_queue_follow_up_with_images(text.clone(), images)?;
        self.client.pending_composer_messages.remove(&session_id);
        let echo_id = format!("queued-user-{session_id}-{entry_id}");
        self.push_optimistic_follow_up(&session_id, text, echo_id);
        self.client.session_status = Some("Message queued…".into());
        Ok(())
    }

    pub(crate) fn steer_pending_message(&mut self) -> Result<(), String> {
        if self.daemon_remote {
            let (text, images, session_id) = {
                let session_id = self.client.active_session_id
                    .clone()
                    .ok_or_else(|| "No active session".to_string())?;
                let pending = self.client.pending_composer_messages
                    .get(&session_id)
                    .cloned()
                    .ok_or_else(|| "No pending composer message".to_string())?;
                (pending.text, pending.images, session_id)
            };
            self.client.pending_composer_messages.remove(&session_id);
            self.dispatch_command(SessionCommand::SteerMessage {
                session_id: session_id.clone(),
                text: text.clone(),
                images,
            });
            let echo_id = format!("steered-user-{session_id}-{}", self.client.messages.len());
            self.push_optimistic_follow_up(&session_id, text, echo_id);
            self.client.session_status = Some("Steering current turn…".into());
            return Ok(());
        }
        let (runtime, session_id, text, images) = self.pending_runtime_message()?;
        runtime
            .work_handle
            .queue_steer_with_images(text.clone(), images)?;
        self.client.pending_composer_messages.remove(&session_id);
        let echo_id = format!("steered-user-{session_id}-{}", self.client.messages.len());
        self.push_optimistic_follow_up(&session_id, text, echo_id);
        self.client.session_status = Some("Steering current turn…".into());
        Ok(())
    }

    /// Drop a still-pending queued follow-up, returning its staged content and
    /// images so the caller can restore them to the composer or discard them.
    ///
    /// Remote mode waits for the daemon's confirmation before claiming the
    /// removal: the echo row stays until the `CommandResult` reply or the
    /// journaled `QueuedEntryCancelled` lands.
    pub fn cancel_queued_message(
        &mut self,
        entry_id: &str,
    ) -> Result<(String, Vec<ImageAttachment>), String> {
        self.cancel_queued_message_inner(entry_id, false)
    }

    /// Cancel a queued follow-up to take its staged content back into the
    /// composer (the queued row's edit button). Remote mode issues a
    /// `CommandRequest` for the cancel: the staged text returns from the
    /// optimistic echo immediately, and the reply's staged content lands
    /// in `requested_composer_inserts` when the removal is confirmed.
    /// Against a pre-envelope daemon the request frame can never be
    /// answered, so the edit degrades to a bare cancel — the entry leaves
    /// the queue but its images are unrecoverable there.
    pub fn edit_queued_message(
        &mut self,
        entry_id: &str,
    ) -> Result<(String, Vec<ImageAttachment>), String> {
        self.cancel_queued_message_inner(entry_id, true)
    }

    /// True while a queued-message cancel for `entry_id` is awaiting the
    /// daemon's confirmation — the row renders its unconfirmed state and
    /// declines further steer/edit/remove actions until resolved.
    pub fn queued_removal_pending(&self, session_id: &str, entry_id: &str) -> bool {
        self.pending_queued_cancels
            .values()
            .any(|pending| pending.session_id == session_id && pending.entry_id == entry_id)
    }

    fn cancel_queued_message_inner(
        &mut self,
        entry_id: &str,
        restore: bool,
    ) -> Result<(String, Vec<ImageAttachment>), String> {
        if self.daemon_remote {
            let session_id = self.client.active_session_id
                .clone()
                .ok_or_else(|| "No active session".to_string())?;
            let echo_id = format!("queued-user-{session_id}-{entry_id}");
            // The staged text comes back from the optimistic echo now; the
            // echo holds no images, so a restore asks the daemon's reply
            // for the full staged content.
            let staged_text = self.client.messages
                .iter()
                .find(|message| message.id == echo_id)
                .map(|message| message.content.clone())
                .unwrap_or_default();
            if self.daemon_client.supports_command_requests() {
                // Shared allocator: panel project-io requests mint ids
                // against the same `RemoteDaemon` waiter map.
                let request_id = threadlane_client::next_request_id();
                self.pending_queued_cancels.insert(
                    request_id,
                    PendingQueuedCancel {
                        session_id: session_id.clone(),
                        work_dir: self.client.active_work_dir.clone(),
                        entry_id: entry_id.to_string(),
                        restore,
                        text_restored: restore && !staged_text.is_empty(),
                    },
                );
                self.dispatch_command_request(CommandRequest {
                    request_id,
                    command: SessionCommand::CancelQueuedMessage {
                        session_id: session_id.clone(),
                        entry_id: entry_id.to_string(),
                        work_dir: self.client.active_work_dir.clone(),
                    },
                });
                // The echo stays until the reply or the journaled
                // cancellation confirms the entry left the queue —
                // claiming removal first is how a daemon that never
                // dispatched the command left the UI lying (issue #349).
                self.client.session_status = Some("Removing queued message…".into());
                return Ok((staged_text, Vec::new()));
            }
            // A pre-envelope daemon cannot decode a `CommandRequest` — the
            // frame is rejected as undecodable and the dispatch never
            // runs. The bare `CancelQueuedMessage` variant predates the
            // envelope and still cancels there; it just cannot report the
            // staged payload back (an edit keeps the echo's text, and the
            // images are lost). Dispatch failures still surface as a
            // broadcast `DaemonError`.
            self.dispatch_command(SessionCommand::CancelQueuedMessage {
                session_id,
                entry_id: entry_id.to_string(),
                work_dir: self.client.active_work_dir.clone(),
            });
            self.remove_queued_echo_by_id(&echo_id);
            self.client.session_status = Some("Queued message removed".into());
            return Ok((staged_text, Vec::new()));
        }
        let (runtime, session_id) = self.active_runtime()?;
        let staged = runtime.work_handle.cancel_queued_entry(entry_id)?;
        let echo_id = format!("queued-user-{session_id}-{entry_id}");
        let mut messages = (*self.client.messages).clone();
        if messages.iter().any(|message| message.id == echo_id) {
            messages.retain(|message| message.id != echo_id);
            self.client.messages = messages.into();
        }
        self.client.session_status = Some("Queued message removed".into());
        Ok(staged)
    }

    /// Drop the optimistic `queued-user-{session}-{entry}` echo. The row
    /// is retained across a pending cancel so the queue only stops
    /// showing the entry once the daemon confirms it left.
    fn remove_queued_echo_by_id(&mut self, echo_id: &str) -> bool {
        if !self.client.messages.iter().any(|message| message.id == echo_id) {
            return false;
        }
        let mut messages = (*self.client.messages).clone();
        messages.retain(|message| message.id != echo_id);
        self.client.messages = messages.into();
        true
    }

    fn remove_queued_echo(&mut self, session_id: &str, entry_id: &str) -> bool {
        self.remove_queued_echo_by_id(&format!("queued-user-{session_id}-{entry_id}"))
    }

    /// Resolve a parked queued-cancel intent with the staged payload a
    /// `CommandResult` reply or a journaled `QueuedEntryCancelled` event
    /// carried: removes the retained echo and, for the edit path, queues
    /// the content as a composer insert scoped to the session that queued
    /// the message — if another session is on screen the insert waits for
    /// it to come back rather than landing in a foreign draft.
    ///
    /// The echo goes away even when no intent matches (a cancel issued by
    /// another client, or one whose intent already resolved): the event
    /// is authoritative that the entry left the queue.
    fn settle_queued_cancel(
        &mut self,
        request_id: u64,
        session_id: &str,
        entry_id: &str,
        text: String,
        images: Vec<ImageAttachment>,
    ) -> bool {
        let mut changed = self.remove_queued_echo(session_id, entry_id);
        // A `QueuedEntryCancelled` may carry a request_id issued by a
        // different client; only consume the intent when session and
        // entry corroborate it is ours.
        let matches = self
            .pending_queued_cancels
            .get(&request_id)
            .is_some_and(|pending| {
                pending.session_id == session_id && pending.entry_id == entry_id
            });
        if !matches {
            return changed;
        }
        let pending = self
            .pending_queued_cancels
            .remove(&request_id)
            .expect("intent presence checked");
        if pending.restore {
            self.requested_composer_inserts
                .push(RequestedComposerInsert {
                    text: if pending.text_restored {
                        String::new()
                    } else {
                        text
                    },
                    images,
                    session_id: Some(session_id.to_string()),
                    work_dir: pending.work_dir,
                });
        }
        self.client.session_status = Some("Queued message removed".into());
        changed = true;
        changed
    }

    /// Re-route a still-pending queued follow-up into the live steer queue so
    /// it reaches the model during the current turn instead of after it.
    pub fn steer_queued_message(&mut self, entry_id: &str) -> Result<(), String> {
        if self.daemon_remote {
            let session_id = self.client.active_session_id
                .clone()
                .ok_or_else(|| "No active session".to_string())?;
            self.dispatch_command(SessionCommand::SteerQueuedMessage {
                session_id: session_id.clone(),
                entry_id: entry_id.to_string(),
            });
            let queued_id = format!("queued-user-{session_id}-{entry_id}");
            let mut messages = (*self.client.messages).clone();
            if let Some(message) = messages.iter_mut().find(|message| message.id == queued_id) {
                message.id = format!("steered-user-{session_id}-{entry_id}");
                self.client.messages = messages.into();
            }
            self.client.session_status = Some("Steering current turn…".into());
            return Ok(());
        }
        let (runtime, session_id) = self.active_runtime()?;
        runtime.work_handle.steer_queued_entry(entry_id)?;
        let queued_id = format!("queued-user-{session_id}-{entry_id}");
        let mut messages = (*self.client.messages).clone();
        if let Some(message) = messages.iter_mut().find(|message| message.id == queued_id) {
            message.id = format!("steered-user-{session_id}-{entry_id}");
            self.client.messages = messages.into();
        }
        self.client.session_status = Some("Steering current turn…".into());
        Ok(())
    }

    pub(crate) fn dismiss_pending_message(&mut self) {
        if let Some(session_id) = self.client.active_session_id.as_ref() {
            self.client.pending_composer_messages.remove(session_id);
        }
    }

    fn active_runtime(&self) -> Result<(Arc<SessionRuntime>, String), String> {
        let session_id = self.client.active_session_id
            .clone()
            .ok_or_else(|| "No active session".to_string())?;
        let work_dir = self.client.active_work_dir
            .as_ref()
            .ok_or_else(|| "No active project".to_string())?;
        let session_file = self.session_file(work_dir, &session_id);
        let runtime = self
            .daemon_core
            .runtime_for_file(&session_file)
            .ok_or_else(|| "Session runtime is unavailable".to_string())?;
        Ok((runtime, session_id))
    }

    fn pending_runtime_message(
        &self,
    ) -> Result<(Arc<SessionRuntime>, String, String, Vec<ImageAttachment>), String> {
        let (runtime, session_id) = self.active_runtime()?;
        let pending = self.client.pending_composer_messages
            .get(&session_id)
            .cloned()
            .ok_or_else(|| "No pending composer message".to_string())?;
        Ok((runtime, session_id, pending.text, pending.images))
    }

    /// Bind the daemon's durable queue entry id to the optimistic
    /// `queued-user-{session}` echo: `queued_entry_id` only recognizes the
    /// `{session}-{entry}` form, so until this lands the row renders no
    /// steer/edit/remove controls.
    fn bind_queued_echo(&mut self, session_id: &str, entry_id: &str) {
        let pending_id = format!("queued-user-{session_id}");
        let bound_id = format!("queued-user-{session_id}-{entry_id}");
        let mut messages = (*self.client.messages).clone();
        if let Some(message) = messages.iter_mut().find(|message| message.id == pending_id) {
            message.id = bound_id;
            self.client.messages = messages.into();
        }
    }

    fn push_optimistic_follow_up(&mut self, session_id: &str, text: String, id: String) {
        if self.client.active_session_id.as_deref() == Some(session_id) {
            self.messages_mut().push(ChatMessageInfo {
                id,
                role: MessageRole::User,
                content: text,
                tool_activities: Vec::new(),
                streaming: false,
                reasoning_content: None,
                reasoning_expanded: false,
                retry_prompt: None,
            });
        }
    }

    pub(crate) fn send_prompt(&mut self, text: String) -> Result<(), String> {
        self.send_prompt_with_images(text, Vec::new())
    }

    pub(crate) fn send_prompt_with_images(
        &mut self,
        text: String,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        let text = text.trim().to_string();
        if text.is_empty() && images.is_empty() {
            return Ok(());
        }

        if self.active_worktree_setup().is_some() {
            return Err("Finish or cancel worktree setup before sending another message".into());
        }
        if self.client.active_session_id.is_none() && self.draft_work_mode == WorkMode::Worktree {
            return self.start_worktree_task(text, images);
        }
        if self.client.active_session_id.is_none() || self.client.active_work_dir.is_none() {
            self.create_new_session()?;
        }

        let (work_dir, session_id) =
            match (self.client.active_work_dir.clone(), self.client.active_session_id.clone()) {
                (Some(w), Some(s)) => (w, s),
                _ => return Err("Failed to ensure active session".into()),
            };
        let session_file = self.session_file(&work_dir, &session_id);
        let runtime_work_dir = self.session_runtime_work_dir(&work_dir, &session_id);
        if self
            .daemon_core
            .runtime_for_file(&session_file)
            .is_some_and(|runtime| runtime.is_generating())
        {
            return Err("A generation is already running for this session".into());
        }

        // Resolve credentials using the same provider routing as the runtime and title task.
        let model = self.selected_model.clone();
        let (api_key, account_id) =
            threadlane_coding_agent::credentials::provider_credentials(&model);

        // An external ACP agent authenticates itself — Claude Code uses its own
        // CLI login — so it has no Threadlane provider credential to check, and
        // gating it on one blocks every ACP turn before it starts.
        if api_key.is_empty() && !threadlane_acp_engine::is_acp_model(&model) {
            // Reject before accepting/clearing the submission. The controller
            // restores its exact payload to the session-scoped composer draft.
            return Err(format!(
                "No API key configured for model `{model}`. Open Settings and save the provider credential."
            ));
        }

        // New-task ACP picks have no session to apply to yet; they wait here
        // and are applied inside the turn task before generation starts, so
        // the first turn runs the model the picker shows.
        let pending_acp = threadlane_acp_engine::acp_agent_id(&model)
            .map(|agent_id| self.take_pending_acp_config(agent_id))
            .unwrap_or_default();
        if self.daemon_remote {
            self.dispatch_command(SessionCommand::SubmitPrompt {
                session_id: session_id.clone(),
                work_dir: runtime_work_dir.clone(),
                text: text.clone(),
                images: images.clone(),
                effort: Some(self.reasoning_effort),
                acp_config: pending_acp,
                model: Some(self.selected_model.clone()),
            });
        } else {
            let runtime =
                self.ensure_session_runtime(runtime_work_dir.clone(), session_file.clone());
            crate::chat::execute_prompt(
                runtime,
                runtime_work_dir,
                session_id.clone(),
                text.clone(),
                images.clone(),
                self.reasoning_effort,
                self.stream_tx.clone(),
                pending_acp,
            )?;
        }
        let prompt_detail = if images.is_empty() {
            text.clone()
        } else if text.is_empty() {
            format!("[{} image attachment(s)]", images.len())
        } else {
            format!("{text}\n[{} image attachment(s)]", images.len())
        };
        self.trajectory_by_session
            .entry(Self::projection_key(&session_id, &session_file))
            .or_default()
            .push(TrajectoryEntry {
                seq: None,
                run_id: None,
                turn: None,
                request: None,
                category: "Input".into(),
                summary: "User input".into(),
                detail: prompt_detail.clone(),
                lane: Some("main".into()),
                correlation_id: None,
                diagnostics: TrajectoryDiagnostics::default(),
            });
        self.trajectory_revision = self.trajectory_revision.wrapping_add(1);
        if !threadlane_provider::router::is_antigravity_model(&model) {
            crate::chat::maybe_generate_session_title(
                session_file,
                session_id.clone(),
                text.clone(),
                api_key,
                account_id,
                model,
                work_dir.clone(),
                self.stream_tx.clone(),
            );
        }

        // Present the accepted prompt immediately. CodingAgent owns durable
        // persistence; writing it directly here would duplicate it.
        let new_len = self.client.messages.len();
        self.messages_mut().push(ChatMessageInfo {
            id: format!("pending-user-{session_id}-{new_len}"),
            role: MessageRole::User,
            content: prompt_detail,
            tool_activities: Vec::new(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
            retry_prompt: None,
        });

        self.client.is_generating = true;
        self.client.session_status = Some("Working…".into());
        if let Some(timing) = self
            .active_session_projection_key()
            .and_then(|key| self.client.run_timings.get_mut(&key))
        {
            timing.suppressed = true;
        }

        // Refresh project sessions without blocking the UI thread.
        self.request_session_refresh(&work_dir);
        self.composer_text.clear();
        Ok(())
    }

    pub(crate) fn cancel_generation(&mut self) -> Result<(), String> {
        if let Some(id) = self.client.active_session_id.clone() {
            if let Some(setup) = self.worktree_setups.remove(&id) {
                if self.daemon_remote {
                    self.dispatch_command(SessionCommand::CancelWorktreeSetup {
                        session_id: id.clone(),
                    });
                }
                crate::worktree_setup::clear_request(&setup);
                setup
                    .cancelled
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                // Keep the entry until the worker acknowledges cancellation so archive/delete
                // cannot race a Git subprocess or runtime construction still writing metadata.
                if setup.error.is_none() {
                    self.worktree_setups.insert(id, setup.clone());
                }
                self.client.is_generating = false;
                self.begin_new_task();
                self.draft_worktree_base = Some(setup.base.clone());
                self.set_work_mode(WorkMode::Worktree);
                if setup.error.is_some() {
                    self.cleanup_cancelled_worktree(&setup);
                }
                self.requested_composer_inserts
                    .push(RequestedComposerInsert {
                        text: setup.text,
                        images: setup.images,
                        session_id: None,
                        work_dir: None,
                    });
                return Ok(());
            }
        }
        let (Some(work_dir), Some(session_id)) = (
            self.client.active_work_dir.as_ref(),
            self.client.active_session_id.as_ref(),
        ) else {
            return Ok(());
        };
        if self.daemon_remote {
            self.dispatch_command(SessionCommand::CancelRun {
                session_id: session_id.clone(),
            });
            self.client.is_generating = false;
            self.client.session_status = Some("Generation cancelled".into());
            return Ok(());
        }
        let session_file = self.session_file(work_dir, session_id);
        let Some(runtime) = self.daemon_core.runtime_for_file(&session_file) else {
            return Ok(());
        };
        crate::chat::cancel_prompt(runtime, session_id.clone(), self.stream_tx.clone())?;
        self.client.is_generating = false;
        self.client.session_status = Some("Generation cancelled".into());
        Ok(())
    }
}


#[path = "tests.rs"]
#[cfg(test)]
mod tests;

/// Normalizes a workspace-relative path without touching the filesystem:
/// resolves `.`/`..` lexically, rejects absolute paths, empty results, and
/// `..` components that would escape the workspace. Used for checkouts that
/// only exist on a remote daemon's filesystem.
fn lexically_normalized_relative(relative_path: &str) -> Option<String> {
    let path = std::path::Path::new(relative_path);
    if path.is_absolute() {
        return None;
    }
    let mut normalized: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop()?;
            }
            component => normalized.push(component.as_os_str().to_owned()),
        }
    }
    if normalized.is_empty() {
        return None;
    }
    Some(
        normalized
            .iter()
            .collect::<PathBuf>()
            .to_string_lossy()
            .into_owned(),
    )
}
