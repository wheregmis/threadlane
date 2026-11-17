//! [`HubCore`]: the hub's roles over one database (docs/milestone-1.md §9).

use std::future::Future;
use std::sync::Arc;

use threadlane_core::adapters::ChannelAdapter;
use threadlane_core::client::HubApi;
use threadlane_core::ids::{LaneId, MachineId};
use threadlane_core::link::HubLink;
use threadlane_core::runtime::{StepHook, WakeTarget, Waker};
use threadlane_core::store::{InboxStore, MachineStore};
use threadlane_core::time::Clock;
use threadlane_core::tools::{PermissionPolicy, ToolRegistry};
use threadlane_store::HubDb;

use crate::defaults::ensure_defaults;
use crate::dispatcher::ClientChannel;
use crate::error::HubError;
use crate::handler::ClientApi;
use crate::link::LocalLink;
use crate::settings::HubSettings;
use crate::tools::{HubTools, TierPolicy};
use crate::wake::WakeHub;

#[derive(Debug, Clone)]
pub(crate) struct Inner {
    pub(crate) db: HubDb,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) hook: Arc<dyn StepHook>,
    pub(crate) settings: HubSettings,
    pub(crate) wake: Arc<WakeHub>,
    pub(crate) tools: Arc<dyn ToolRegistry>,
    pub(crate) policy: Arc<dyn PermissionPolicy>,
    pub(crate) channels: Vec<Arc<dyn ChannelAdapter>>,
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
    /// Builds the hub with the built-in tools, the milestone-1 permission
    /// policy, and the `client` channel, after making sure the defaults
    /// exist (a lead agent, lead bindings, the hub's machine row).
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
        let tools: Arc<dyn ToolRegistry> = Arc::new(HubTools::new(
            db.clone(),
            Arc::clone(&clock),
            Arc::clone(&wake) as Arc<dyn Waker>,
        ));
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                clock,
                hook,
                settings,
                wake,
                tools,
                policy: Arc::new(TierPolicy::default()),
                channels: vec![Arc::new(ClientChannel)],
            }),
        })
    }

    /// The same hub with another tool registry. Combine it with
    /// [`HubCore::tools`] through [`crate::CombinedTools`] to add tools
    /// rather than replace them. Links made earlier keep the old registry.
    #[must_use]
    pub fn with_tools(self, tools: Arc<dyn ToolRegistry>) -> Self {
        self.map_inner(|inner| inner.tools = tools)
    }

    /// The same hub with another permission policy.
    #[must_use]
    pub fn with_policy(self, policy: Arc<dyn PermissionPolicy>) -> Self {
        self.map_inner(|inner| inner.policy = policy)
    }

    /// The same hub with another set of channel adapters.
    #[must_use]
    pub fn with_channels(self, channels: Vec<Arc<dyn ChannelAdapter>>) -> Self {
        self.map_inner(|inner| inner.channels = channels)
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
        Ok(expired)
    }

    /// Runs the sweep loop and the dispatcher loop until `shutdown`
    /// completes. Both run every `poll`; the dispatcher also runs on every
    /// dispatcher wake hint. Store errors are logged and retried on the next
    /// pass.
    ///
    /// # Errors
    ///
    /// [`HubError::Killed`] at an injected crash.
    pub async fn run(&self, shutdown: impl Future<Output = ()> + Send) -> Result<(), HubError> {
        tokio::select! {
            () = shutdown => Ok(()),
            result = self.sweep_loop() => result,
            result = self.dispatch_loop() => result,
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
            match self.dispatch_once().await {
                Ok(0) => {}
                Ok(sent) => tracing::debug!(sent, "dispatched messages"),
                Err(HubError::Killed(killed)) => return Err(killed.into()),
                Err(error) => tracing::error!(%error, "dispatcher pass failed"),
            }
            tokio::select! {
                () = self.inner.wake.dispatcher_woken() => {}
                () = self.inner.clock.sleep(self.inner.settings.poll) => {}
            }
        }
    }
}
