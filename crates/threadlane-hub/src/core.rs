//! [`HubCore`]: the hub's roles over one database (docs/milestone-1.md §9).

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use threadlane_core::adapters::ChannelAdapter;
use threadlane_core::client::HubApi;
use threadlane_core::github::GithubSource;
use threadlane_core::ids::{LaneId, MachineId, ProjectId, TaskId};
use threadlane_core::link::HubLink;
use threadlane_core::model::RunStatus;
use threadlane_core::runtime::{StepHook, WakeTarget, Waker};
use threadlane_core::store::{InboxStore, MachineStore, OrgStore, SessionStore, TaskStore};
use threadlane_core::time::{Clock, Timestamp};
use threadlane_core::tools::ToolRegistry;
use threadlane_core::workspace::{WorkspaceRemoval, Workspaces, task_ref};
use threadlane_runner::GitWorkspaces;
use threadlane_store::HubDb;
use threadlane_transport_iroh::{EndpointAddr, Gate};

use crate::defaults::ensure_defaults;
use crate::dispatcher::ClientChannel;
use crate::error::HubError;
use crate::handler::ClientApi;
use crate::link::LocalLink;
use crate::pairing::HubGate;
use crate::settings::HubSettings;
use crate::tools::HubTools;
use crate::wake::WakeHub;

#[derive(Debug, Clone)]
pub(crate) struct Inner {
    pub(crate) db: HubDb,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) hook: Arc<dyn StepHook>,
    pub(crate) settings: HubSettings,
    pub(crate) wake: Arc<WakeHub>,
    pub(crate) tools: Arc<dyn ToolRegistry>,
    pub(crate) dispatch: Arc<tokio::sync::Mutex<()>>,
    pub(crate) channels: Vec<Arc<dyn ChannelAdapter>>,
    /// How clients reach this hub over iroh, for join codes; `None` while
    /// the hub does not serve iroh.
    pub(crate) iroh: Option<EndpointAddr>,
    /// The local drivers for permission questions, in preference order.
    pub(crate) backends: Vec<Arc<dyn threadlane_core::backend::AgentBackend>>,
    /// Where the GitHub poller reads; `None` runs no poller.
    pub(crate) github: Option<Arc<dyn GithubSource>>,
    // Serialize cursor readers independently of the project-deletion lock.
    pub(crate) github_poll: Arc<tokio::sync::Mutex<()>>,
    pub(crate) workspaces: Option<PathBuf>,
    pub(crate) workspace_files: Option<Arc<dyn Workspaces>>,
    pub(crate) project_removal: Arc<tokio::sync::Mutex<()>>,
    /// Ended tasks whose worktrees were kept, and when.
    pub(crate) kept_worktrees: Arc<tokio::sync::Mutex<HashMap<TaskId, Timestamp>>>,
    /// When this hub process started, shown to every run (#652).
    pub(crate) started_at: Timestamp,
}

/// The hub of one space: handler, placement, begin-run, tool gateway,
/// dispatcher, and lease sweep over one [`HubDb`].
///
/// Cheap to clone; clones share the wake mechanism. Everything durable is in
/// the database, so a fresh `HubCore` on the same database continues where a
/// previous one stopped.
#[derive(Debug, Clone)]
pub struct HubCore {
    pub(crate) inner: Arc<Inner>,
}

impl HubCore {
    /// Builds the hub with the built-in tools and the `client` channel, after making sure the defaults
    /// exist (a lead agent, lead bindings, the hub's machine row, dreaming where unset).
    ///
    /// # Errors
    ///
    /// Fails if the defaults cannot be written.
    pub async fn new(
        db: HubDb,
        clock: Arc<dyn Clock>,
        hook: Arc<dyn StepHook>,
        settings: HubSettings,
    ) -> Result<Self, HubError> {
        ensure_defaults(&db).await?;
        let wake = Arc::new(WakeHub::default());
        let tools: Arc<dyn ToolRegistry> = Arc::new(
            HubTools::new(
                db.clone(),
                Arc::clone(&clock),
                Arc::clone(&wake) as Arc<dyn Waker>,
                settings.reviewer.clone(),
            )
            .with_default_models(settings.default_models.clone()),
        );
        let started_at = clock.now();
        let core = Self {
            inner: Arc::new(Inner {
                started_at,
                db,
                clock,
                hook,
                settings,
                wake,
                tools,
                dispatch: Arc::default(),
                channels: vec![Arc::new(ClientChannel)],
                iroh: None,
                github: None,
                github_poll: Arc::default(),
                workspaces: None,
                workspace_files: None,
                project_removal: Arc::default(),
                kept_worktrees: Arc::default(),
                backends: Vec::new(),
            }),
        };
        core.ensure_dreaming().await?;
        Ok(core)
    }

    /// Attaches local workspace files and shares their lock with the embedded runner.
    #[must_use]
    pub fn with_workspaces(self, root: PathBuf) -> Self {
        self.map_inner(|inner| {
            inner.workspace_files = Some(Arc::new(GitWorkspaces::new(root.clone())));
            inner.workspaces = Some(root);
        })
    }

    pub(crate) async fn cleanup_project(
        &self,
        id: ProjectId,
        removal: &WorkspaceRemoval,
    ) -> Vec<String> {
        let leftovers = match &self.inner.workspace_files {
            Some(files) => files.remove(removal).await,
            None => Vec::new(),
        };
        if leftovers.is_empty()
            && let Err(error) = self.inner.db.finish_project_cleanup(id).await
        {
            tracing::error!(%error, "could not acknowledge project workspace cleanup");
        }
        leftovers
    }

    /// The same hub with another tool registry. Combine it with
    /// [`HubCore::tools`] through [`crate::CombinedTools`] to add tools
    /// rather than replace them. Links made earlier keep the old registry.
    #[must_use]
    pub fn with_tools(self, tools: Arc<dyn ToolRegistry>) -> Self {
        self.map_inner(|inner| inner.tools = tools)
    }

    /// The same hub with another set of channel adapters.
    #[must_use]
    pub fn with_channels(self, channels: Vec<Arc<dyn ChannelAdapter>>) -> Self {
        self.map_inner(|inner| inner.channels = channels)
    }

    /// The same hub, issuing join codes that reach it at `addr` over iroh.
    #[must_use]
    pub fn with_iroh(self, addr: EndpointAddr) -> Self {
        self.map_inner(|inner| inner.iroh = Some(addr))
    }

    /// The same hub, polling GitHub through `source` while its machine
    /// reports `gh` logged in (Plan.md §6.6).
    #[must_use]
    pub fn with_github(self, source: Arc<dyn GithubSource>) -> Self {
        self.map_inner(|inner| inner.github = Some(source))
    }

    /// Attaches the hub machine's backends for screening and permission review.
    #[must_use]
    pub fn with_backends(
        self,
        backends: Vec<Arc<dyn threadlane_core::backend::AgentBackend>>,
    ) -> Self {
        self.map_inner(|inner| inner.backends = backends)
    }

    fn map_inner(self, change: impl FnOnce(&mut Inner)) -> Self {
        let mut inner = Arc::unwrap_or_clone(self.inner);
        change(&mut inner);
        Self {
            inner: Arc::new(inner),
        }
    }

    /// The tool registry runs are given.
    #[must_use]
    pub fn tools(&self) -> Arc<dyn ToolRegistry> {
        Arc::clone(&self.inner.tools)
    }

    /// The hub's wake mechanism, shared by every clone.
    #[must_use]
    pub fn waker(&self) -> Arc<dyn Waker> {
        Arc::clone(&self.inner.wake) as Arc<dyn Waker>
    }

    /// The database this hub works on.
    #[must_use]
    pub fn db(&self) -> &HubDb {
        &self.inner.db
    }

    /// The settings this hub runs with.
    #[must_use]
    pub fn settings(&self) -> &HubSettings {
        &self.inner.settings
    }

    /// The client interface (Plan.md §11.2).
    #[must_use]
    pub fn api(&self) -> Arc<dyn HubApi> {
        Arc::new(ClientApi::new(self.clone()))
    }

    /// Who may connect over iroh: the admitted keys, joining, revocation.
    #[must_use]
    pub fn gate(&self) -> Arc<dyn Gate> {
        Arc::new(HubGate::new(self.clone()))
    }

    /// The link a runner on `machine` talks to the hub through. It receives
    /// every nudge sent from now on.
    #[must_use]
    pub fn link(&self, machine: MachineId) -> Arc<dyn HubLink> {
        Arc::new(LocalLink::new(self.clone(), machine))
    }

    /// The machine row of the hub's own runner.
    ///
    /// # Errors
    ///
    /// A store error, or [`HubError::NoHubMachine`].
    pub async fn local_machine(&self) -> Result<MachineId, HubError> {
        self.inner
            .db
            .machines()
            .await?
            .into_iter()
            .find(|machine| machine.is_hub)
            .map(|machine| machine.id)
            .ok_or(HubError::NoHubMachine)
    }

    /// Removes the worktrees of ended tasks with no active run once nothing
    /// in them would be lost (see [`Workspaces::remove_finished`]). A task
    /// whose worktrees were kept is checked again after `worktree_cleanup`,
    /// or sooner when it runs again.
    async fn remove_idle_worktrees(&self, now: Timestamp) -> Result<(), HubError> {
        let inner = &self.inner;
        let (Some(recheck), Some(root), Some(files)) = (
            inner.settings.worktree_cleanup,
            &inner.workspaces,
            &inner.workspace_files,
        ) else {
            return Ok(());
        };
        let Ok(mut entries) = tokio::fs::read_dir(root.join("worktrees")).await else {
            return Ok(());
        };
        let mut candidate_ids = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            if let Some(id) = name
                .to_str()
                .and_then(|name| task_ref(&serde_json::Value::String(name.to_owned())))
            {
                candidate_ids.push(id);
            }
        }
        for id in candidate_ids {
            let _removal = inner.project_removal.lock().await;
            let Some(task) = inner.db.task(id).await? else {
                continue;
            };
            if !task.status.is_terminal() {
                continue;
            }
            let runs = inner.db.lane_runs(task.lane_id).await?;
            if runs.iter().any(|run| run.status == RunStatus::Active) {
                continue;
            }
            let last_run = runs
                .iter()
                .filter_map(|run| run.ended_at)
                .max()
                .unwrap_or(task.created_at);
            let kept = inner.kept_worktrees.lock().await.get(&id).copied();
            if kept.is_some_and(|kept| kept >= last_run && now.saturating_since(kept) < recheck) {
                continue;
            }
            let repositories: Vec<_> = inner
                .db
                .repositories(task.project_id)
                .await?
                .into_iter()
                .filter(|repository| task.repositories.contains(&repository.id))
                .collect();
            match files.remove_finished(task.id, &repositories).await {
                Ok(None) => {
                    inner.kept_worktrees.lock().await.remove(&id);
                    tracing::info!(task = %task.id, "removed an ended task's worktrees");
                }
                Ok(Some(reason)) => {
                    inner.kept_worktrees.lock().await.insert(id, now);
                    tracing::info!(task = %task.id, %reason, "kept an ended task's worktrees");
                }
                Err(error) => {
                    inner.kept_worktrees.lock().await.insert(id, now);
                    tracing::warn!(task = %task.id, %error, "could not remove an ended task's worktrees");
                }
            }
        }
        Ok(())
    }

    /// Releases expired leases (their claimed events return to pending),
    /// sends the account notices that are due (Plan.md §5.3), and nudges
    /// runners if any lane is ready. Returns the released lanes.
    ///
    /// # Errors
    ///
    /// A store error.
    pub async fn sweep_once(&self) -> Result<Vec<LaneId>, HubError> {
        let now = self.inner.clock.now();
        let expired = self.inner.db.expire_leases(now).await?;
        // A cooldown may have ended, or the space may have run out of
        // accounts without a notice yet (a crash in between).
        self.notice_accounts().await?;
        if !expired.is_empty() || !self.inner.db.ready_lanes(now).await?.is_empty() {
            self.inner.wake.wake(WakeTarget::Runners);
        }
        // Keep the lease sweep independent of GitHub polling when there is no cleanup.
        let cleanups = self.inner.db.project_cleanups().await?;
        if !cleanups.is_empty() {
            let _removal = self.inner.project_removal.lock().await;
            for (id, removal) in cleanups {
                self.cleanup_project(id, &removal).await;
            }
        }
        self.remove_idle_worktrees(now).await?;
        Ok(expired)
    }

    /// Runs the sweep loop, the dispatcher loop, the scheduler, and, with a
    /// GitHub source, the GitHub poller until `shutdown` completes. The
    /// first two run every `poll`, the dispatcher also on every dispatcher
    /// wake hint; the scheduler every `scheduler`; the poller at the
    /// interval GitHub asks for. Store errors are logged and retried on the
    /// next pass.
    ///
    /// # Errors
    ///
    /// [`HubError::Killed`] at an injected crash.
    pub async fn run(&self, shutdown: impl Future<Output = ()> + Send) -> Result<(), HubError> {
        tokio::select! {
            () = shutdown => Ok(()),
            result = self.sweep_loop() => result,
            result = self.dispatch_loop() => result,
            result = self.schedule_loop() => result,
            result = self.github_loop() => result,
        }
    }

    async fn schedule_loop(&self) -> Result<(), HubError> {
        loop {
            match self.schedule_once().await {
                Ok(0) => {}
                Ok(lanes) => tracing::debug!(lanes, "fired timers"),
                Err(HubError::Killed(killed)) => return Err(killed.into()),
                Err(error) => tracing::error!(%error, "scheduler pass failed"),
            }
            self.inner.clock.sleep(self.inner.settings.scheduler).await;
        }
    }

    async fn sweep_loop(&self) -> Result<(), HubError> {
        loop {
            match self.sweep_once().await {
                Ok(lanes) if !lanes.is_empty() => {
                    tracing::info!(?lanes, "released expired leases");
                }
                Ok(_) => {}
                Err(HubError::Killed(killed)) => return Err(killed.into()),
                Err(error) => tracing::error!(%error, "lease sweep failed"),
            }
            // Wake when the next cooldown ends, so that work resumes then
            // rather than on the next poll.
            let poll = self.inner.clock.now() + self.inner.settings.poll;
            let wake_at = match self.next_reset().await {
                Ok(Some(reset)) => reset.min(poll),
                Ok(None) => poll,
                Err(error) => {
                    tracing::error!(%error, "could not read the accounts");
                    poll
                }
            };
            self.inner.clock.sleep_until(wake_at).await;
        }
    }

    async fn dispatch_loop(&self) -> Result<(), HubError> {
        loop {
            let started_at = self.inner.clock.now();
            match self.dispatch_pass().await {
                Ok((0, false)) => {}
                Ok((sent, full_batch)) => {
                    if sent > 0 {
                        tracing::debug!(sent, "dispatched messages");
                    }
                    if full_batch {
                        continue;
                    }
                }
                Err(HubError::Killed(killed)) => return Err(killed.into()),
                Err(error) => tracing::error!(%error, "dispatcher pass failed"),
            }
            let poll = self.inner.clock.now() + self.inner.settings.poll;
            let deadline = match self.inner.db.next_delivery_after(started_at).await {
                Ok(Some(next)) => next.min(poll),
                Ok(None) => poll,
                Err(error) => {
                    tracing::error!(%error, "could not read the next delivery deadline");
                    poll
                }
            };
            tokio::select! {
                () = self.inner.wake.dispatcher_woken() => {}
                () = self.inner.clock.sleep_until(deadline) => {}
            }
        }
    }
}
