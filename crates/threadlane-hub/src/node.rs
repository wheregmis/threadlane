//! The hub process: its data directory, lock, database, key, transports, and
//! roles.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use threadlane_backend_antigravity::AntigravityBackend;
use threadlane_backend_claude::ClaudeBackend;
use threadlane_backend_codex::CodexBackend;
use threadlane_core::admission::AdmissionStore;
use threadlane_core::client::{PairingReply, PairingRequest};
use threadlane_core::runtime::NoHooks;
use threadlane_core::store::OrgStore;
use threadlane_core::time::{Clock, SystemClock};
use threadlane_runner::{GitForge, GitWorkspaces, Runner};
use threadlane_store::HubDb;
use tokio::sync::watch;

use crate::DirLock;
use crate::config::{DataDir, HubConfig};
use crate::core::HubCore;
use crate::gh::GhSource;
use crate::handler::ClientApi;
use crate::updater::HubUpdates;

/// An opened hub: its configuration, lock, and stores.
#[derive(Debug)]
pub struct Hub {
    /// What `hub.toml` said.
    pub config: HubConfig,
    /// Where the hub keeps its files.
    pub data_dir: DataDir,
    /// The hub's stores.
    pub db: HubDb,
    /// The real clock.
    pub clock: Arc<dyn Clock>,
    _lock: DirLock,
}

impl Hub {
    /// Creates the data directory if needed, locks it, and opens the database.
    ///
    /// # Errors
    ///
    /// Fails if the directory is locked by another process, the configuration
    /// is invalid, or the database cannot be opened or migrated.
    pub async fn open(data_dir: DataDir, config: HubConfig) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir.root())
            .with_context(|| format!("creating {}", data_dir.root().display()))?;
        let lock = DirLock::acquire(&data_dir)?;
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let db = HubDb::open(&data_dir.database(), &config.bootstrap(), clock.clone())
            .await
            .with_context(|| format!("opening {}", data_dir.database().display()))?;
        Ok(Self {
            config,
            data_dir,
            db,
            clock,
            _lock: lock,
        })
    }

    /// Checks the database and logs what this hub holds.
    ///
    /// # Errors
    ///
    /// Fails if the integrity check reports a problem.
    pub async fn check(&self) -> anyhow::Result<()> {
        let problems = self.db.integrity_check().await?;
        anyhow::ensure!(problems.is_empty(), "integrity check failed: {problems:?}");
        let space = self.db.space().await?;
        let projects = self.db.projects().await?;
        tracing::info!(
            space = %space.name,
            ceiling = %space.mode_ceiling,
            projects = projects.len(),
            schema = self.db.schema_version().await?,
            data_dir = %self.data_dir.root().display(),
            "hub database is healthy"
        );
        Ok(())
    }

    /// Builds the hub's roles over the opened database.
    ///
    /// # Errors
    ///
    /// Fails if the defaults cannot be written.
    pub async fn core(&self) -> anyhow::Result<HubCore> {
        HubCore::new(
            self.db.clone(),
            Arc::clone(&self.clock),
            Arc::new(NoHooks),
            self.config.settings(),
        )
        .await
        .map(|core| {
            core.with_workspaces(self.data_dir.workspaces())
                .with_github(Arc::new(GhSource::with_gh(&self.config.github.gh)))
                .with_backends(vec![
                    Arc::new(ClaudeBackend::new(
                        self.config.backends.claude.claude_config(),
                    )),
                    Arc::new(CodexBackend::new(self.config.backends.codex.codex_config())),
                    Arc::new(AntigravityBackend::new(
                        self.config.backends.antigravity.program.clone(),
                    )),
                ])
        })
        .context("starting the hub's roles")
    }

    async fn deliver_reviews(&self) -> anyhow::Result<()> {
        let forge = GitForge::with_gh(&self.config.github.gh);
        loop {
            let reviews = self.db.pending_github_reviews().await?;
            for review in reviews {
                if forge
                    .publish_review(
                        &review.pull_url,
                        &review.revision,
                        &review.body,
                        &review.delivery_key,
                    )
                    .await
                    .is_ok()
                {
                    self.db.delivered_github_review(&review).await?;
                } else {
                    tracing::warn!(url = %review.pull_url, submission = %review.submission, "GitHub review delivery failed; will retry");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    }

    /// Serves the client interface on `<data dir>/hub.sock` and, unless
    /// `[iroh]` turns it off, over iroh with the key in `<data dir>/hub.key`
    /// (created on first start). Runs the hub's roles until `shutdown`
    /// completes. While no client is admitted, prints a join code first.
    ///
    /// # Errors
    ///
    /// Fails if the socket or the endpoint cannot be bound, the key cannot be
    /// read or created, or a role stops with an error.
    pub async fn run(&self, shutdown: impl Future<Output = ()> + Send) -> anyhow::Result<()> {
        self.serve(shutdown, None).await
    }

    /// Runs the packaged hub and returns the new executable after an update.
    ///
    /// # Errors
    ///
    /// Returns the same startup and shutdown errors as [Self::run].
    pub async fn run_updatable(
        &self,
        shutdown: impl Future<Output = ()> + Send,
    ) -> anyhow::Result<Option<PathBuf>> {
        if !threadlane_updater::Component::Hub.available() {
            self.run(shutdown).await?;
            return Ok(None);
        }
        let (restart, mut installed) = watch::channel(None);
        let updates = Arc::new(HubUpdates::new(restart));
        self.serve(
            async {
                tokio::select! {
                    () = shutdown => {}
                    _ = installed.changed() => {}
                }
            },
            Some(updates),
        )
        .await?;
        Ok(installed.borrow().clone())
    }

    async fn serve(
        &self,
        shutdown: impl Future<Output = ()> + Send,
        updates: Option<Arc<HubUpdates>>,
    ) -> anyhow::Result<()> {
        let mut core = self.core().await?;
        let iroh = &self.config.iroh;
        let endpoint = if iroh.enabled {
            let key_file = self.data_dir.key();
            let secret = threadlane_transport_iroh::load_or_create_key(&key_file)
                .with_context(|| format!("reading the hub's key {}", key_file.display()))?;
            let endpoint =
                threadlane_transport_iroh::hub_endpoint(secret, iroh.bind, iroh.local_discovery)
                    .await?;
            core = core.with_iroh(threadlane_transport_iroh::hub_addr(&endpoint));
            tracing::info!(id = %endpoint.id(), addrs = ?endpoint.bound_sockets(), "serving iroh");
            print_first_join_code(&core).await?;
            Some(endpoint)
        } else {
            None
        };
        let (stop, stopped) = watch::channel(false);
        let socket = self.data_dir.socket();
        let mut api = ClientApi::new(core.clone());
        api.updates = updates;
        let api = Arc::new(api);
        let work = async {
            tokio::try_join!(
                async {
                    tokio::select! {
                        result = self.deliver_reviews() => result,
                        () = until_stopped(stopped.clone()) => anyhow::Ok(()),
                    }
                },
                async {
                    if let Some(endpoint) = endpoint {
                        threadlane_transport_iroh::serve(
                            endpoint,
                            api.clone(),
                            core.gate(),
                            until_stopped(stopped.clone()),
                        )
                        .await;
                    }
                    anyhow::Ok(())
                },
                async {
                    threadlane_transport_local::serve(
                        &socket,
                        api.clone(),
                        until_stopped(stopped.clone()),
                    )
                    .await
                    .with_context(|| format!("serving {}", socket.display()))
                },
                async {
                    core.run(until_stopped(stopped.clone()))
                        .await
                        .context("hub roles stopped")
                },
                async {
                    let result = start_embedded_runner(
                        &core,
                        &self.config,
                        &self.data_dir,
                        Arc::clone(&self.clock),
                        until_stopped(stopped.clone()),
                    )
                    .await;
                    // A hub without its runner would accept messages nobody
                    // runs, so the runner ending early ends the hub.
                    let result = result.and_then(|()| {
                        anyhow::ensure!(
                            *stopped.borrow(),
                            "the embedded runner stopped before the hub did"
                        );
                        Ok(())
                    });
                    if let Err(error) = &result {
                        tracing::error!(
                            error = format!("{error:#}"),
                            "embedded runner failed; stopping the hub"
                        );
                    }
                    result
                },
            )
            .map(|_| ())
        };
        tokio::pin!(work);
        tracing::info!(socket = %socket.display(), "hub running");
        tokio::select! {
            result = &mut work => return result,
            () = shutdown => {}
        }
        tracing::info!("hub stopping");
        // Every receiver is alive inside `work`, so sending cannot fail.
        let _ = stop.send(true);
        work.await
    }
}

/// Prints a join code on standard output while no client is admitted, for
/// pairing the first one (Plan.md §3.5). Not logged: logs keep no secrets.
async fn print_first_join_code(core: &HubCore) -> anyhow::Result<()> {
    let keys = core.db().admitted_keys().await?;
    if keys.iter().any(|key| key.is_active()) {
        return Ok(());
    }
    let reply = core
        .api()
        .pairing(PairingRequest::IssueJoinCode)
        .await
        .context("issuing the first join code")?;
    if let PairingReply::JoinCode { code, .. } = reply {
        println!(
            "No client is paired with this hub yet. Within {} minutes, on the new client run:\n\n    threadlane hub join <name> {code}\n",
            crate::JOIN_CODE_TTL.as_secs() / 60
        );
    }
    Ok(())
}

async fn until_stopped(mut stopped: watch::Receiver<bool>) {
    // An error means the sender is gone, which also means stop.
    let _ = stopped.wait_for(|stop| *stop).await;
}

/// Runs the hub's own runner (docs/milestone-1.md §9 "Binary") on Claude
/// and Codex, with one working directory per lane under `<data dir>/lanes` and task
/// worktrees under `<data dir>/workspaces`, publishing through `git` and
/// `gh` ([`GitForge`]), until
/// `shutdown` completes. It stops its runs and releases their leases on the
/// way out.
async fn start_embedded_runner(
    core: &HubCore,
    config: &HubConfig,
    data_dir: &DataDir,
    clock: Arc<dyn Clock>,
    shutdown: impl Future<Output = ()> + Send,
) -> anyhow::Result<()> {
    let machine = core
        .local_machine()
        .await
        .context("finding the hub's own machine")?;
    let claude = config.backends.claude.claude_config();
    let codex = config.backends.codex.codex_config();
    let runner_config = config.runner_config(data_dir.lanes());
    tracing::info!(
        claude = %claude.program.display(),
        codex = %codex.program.display(),
        antigravity = %config.backends.antigravity.program.display(),
        lanes_dir = %runner_config.lanes_dir.display(),
        max_parallel = runner_config.max_parallel,
        "starting the embedded runner"
    );
    let runner = Runner::new(
        core.link(machine),
        core.inner.backends.clone(),
        core.inner
            .workspace_files
            .clone()
            .unwrap_or_else(|| Arc::new(GitWorkspaces::new(data_dir.workspaces()))),
        Arc::new(GitForge::with_gh(&config.github.gh)),
        clock,
        Arc::new(NoHooks),
        runner_config,
    );
    runner
        .run(shutdown)
        .await
        .context("the embedded runner stopped")
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use threadlane_core::client::{HubApi, OrgReply, OrgRequest, UserMessage};
    use threadlane_core::model::{EventStatus, RunOutcome, RunStatus};
    use threadlane_core::store::{InboxStore, SessionStore};
    use threadlane_transport_local::LocalClient;

    use super::*;

    /// A configuration whose `claude` is `program` and whose `codex` and
    /// `gh` do not exist, so no test runs the real ones, and whose iroh
    /// endpoint stays on loopback with no discovery.
    fn config_with_claude(program: &Path) -> HubConfig {
        let codex = program.with_file_name("no-codex-here");
        HubConfig::parse(&format!(
            "[iroh]\nbind = \"127.0.0.1:0\"\nlocal_discovery = false\n\
             [github]\ngh = \"/nonexistent/gh\"\n\
             [runner]\ndebounce_secs = 0\n[backends.claude]\nprogram = {:?}\n[backends.codex]\nprogram = {:?}\n[backends.antigravity]\nprogram = \"/nonexistent/agy\"",
            program.display().to_string(),
            codex.display().to_string()
        ))
        .unwrap()
    }

    /// Asks `socket` for the hub's info until it answers.
    async fn wait_for_socket(socket: &Path) -> threadlane_core::client::HubInfo {
        let client = LocalClient::new(socket);
        for _ in 0..500 {
            if let Ok(info) = client.info().await {
                return info;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the hub never answered on {}", socket.display());
    }

    #[tokio::test]
    async fn a_second_process_cannot_open_the_same_hub() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = DataDir::new(dir.path().join("work"));
        let hub = Hub::open(data_dir.clone(), HubConfig::default())
            .await
            .unwrap();
        hub.check().await.unwrap();
        let second = Hub::open(data_dir.clone(), HubConfig::default()).await;
        assert!(second.is_err(), "the lock keeps a second hub out");
        drop(hub);
        Hub::open(data_dir, HubConfig::default())
            .await
            .expect("the lock is released when the hub closes");
    }

    #[tokio::test]
    async fn two_hubs_on_one_computer_share_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let work = Hub::open(
            DataDir::new(dir.path().join("work")),
            HubConfig::parse("[space]\nname = \"Work\"").unwrap(),
        )
        .await
        .unwrap();
        let personal = Hub::open(
            DataDir::new(dir.path().join("personal")),
            HubConfig::parse("[space]\nname = \"Personal\"").unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(work.db.space().await.unwrap().name, "Work");
        assert_eq!(personal.db.space().await.unwrap().name, "Personal");
        assert_ne!(work.data_dir.database(), personal.data_dir.database());

        // What happens at one hub is invisible at the other (invariant 17).
        let work_api = work.core().await.unwrap().api();
        let personal_api = personal.core().await.unwrap().api();
        work_api
            .org(OrgRequest::CreateProject {
                name: "Alpha".into(),
                description: String::new(),
            })
            .await
            .unwrap();
        work_api
            .org(OrgRequest::CreateAgent {
                name: "Reviewer".into(),
                role: "reviewer".into(),
                instructions: String::new(),
                backends: Vec::new(),
            })
            .await
            .unwrap();
        work_api
            .send_message(&UserMessage {
                project: Some("Alpha".into()),
                text: "hello".into(),
                images: Vec::new(),
                client_message_id: "m-1".into(),
                task: None,
            })
            .await
            .unwrap();
        assert_eq!(work_api.projects().await.unwrap().len(), 2);

        let projects = personal_api.projects().await.unwrap();
        assert_eq!(projects.len(), 1);
        assert!(projects[0].is_general);
        let lane = personal.db.general_project().await.unwrap().lead_lane_id;
        assert!(personal.db.pending_events(lane).await.unwrap().is_empty());
        assert!(
            personal_api
                .feed(0, Duration::ZERO)
                .await
                .unwrap()
                .is_empty()
        );
        let Ok(OrgReply::Agents { agents }) = personal_api.org(OrgRequest::Agents).await else {
            panic!("expected agents");
        };
        assert_eq!(agents.len(), 1, "only the default lead");
        let missing = personal_api
            .send_message(&UserMessage {
                project: Some("Alpha".into()),
                text: "hello".into(),
                images: Vec::new(),
                client_message_id: "m-1".into(),
                task: None,
            })
            .await;
        assert!(
            matches!(missing, Err(threadlane_core::client::ApiError::NotFound(_))),
            "{missing:?}"
        );

        let work_files = files_under(work.data_dir.root());
        let personal_files = files_under(personal.data_dir.root());
        assert!(!work_files.is_empty() && !personal_files.is_empty());
        assert!(
            work_files.is_disjoint(&personal_files),
            "shared: {:?}",
            work_files.intersection(&personal_files).collect::<Vec<_>>()
        );
    }

    /// Every file and directory under `root`, resolved to its real path.
    fn files_under(root: &Path) -> std::collections::BTreeSet<PathBuf> {
        let mut found = std::collections::BTreeSet::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path.clone());
                }
                found.insert(path.canonicalize().unwrap());
            }
        }
        found
    }

    #[tokio::test]
    async fn run_serves_the_socket_until_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_with_claude(&dir.path().join("no-claude-here"));
        let hub = Hub::open(DataDir::new(dir.path()), config).await.unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let socket = hub.data_dir.socket();
        let running = async {
            hub.run(async {
                let _ = stopped.await;
            })
            .await
        };
        let client = async {
            let info = wait_for_socket(&socket).await;
            stop.send(()).unwrap();
            info
        };
        let (result, info) = tokio::join!(running, client);
        result.expect("a missing claude is not an error; it is reported unusable");
        assert_eq!(info.space, "Space");
        assert!(!socket.exists(), "the socket is removed on shutdown");
    }

    #[tokio::test]
    async fn a_runner_that_cannot_start_stops_the_hub() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_with_claude(&dir.path().join("no-claude-here"));
        // A file where the lanes directory belongs: the runner cannot start.
        std::fs::write(dir.path().join(crate::config::LANES_DIR), "").unwrap();
        let hub = Hub::open(DataDir::new(dir.path()), config).await.unwrap();
        // Never asked to stop: the runner's failure alone must end the hub.
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            hub.run(std::future::pending::<()>()),
        )
        .await
        .expect("the hub stops on its own");
        let error = format!("{:#}", result.expect_err("the hub fails"));
        assert!(error.contains("embedded runner"), "{error}");
    }

    /// A fake `claude`: answers the probe as logged in, and runs as a lead
    /// that says nothing and gives a reason.
    #[cfg(unix)]
    fn fake_claude(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let script = r#"#!/bin/sh
case "$1" in
  --version) echo "2.1.325 (Claude Code)"; exit 0 ;;
  auth) echo '{"loggedIn": true}'; exit 0 ;;
esac
IFS= read -r prompt
echo '{"type":"system","subtype":"init","session_id":"fake-session","tools":[],"mcp_servers":[{"name":"threadlane","status":"connected"}]}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Nothing needs saying.","session_id":"fake-session","num_turns":1,"total_cost_usd":0,"usage":{}}'
"#;
        let program = dir.join("claude");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        program
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_embedded_runner_works_a_message_through_claude() {
        let tools = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config = config_with_claude(&fake_claude(tools.path()));
        let hub = Hub::open(DataDir::new(dir.path()), config).await.unwrap();
        let db = hub.db.clone();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let socket = hub.data_dir.socket();
        let running = async {
            hub.run(async {
                let _ = stopped.await;
            })
            .await
        };
        let client = async {
            wait_for_socket(&socket).await;
            let accepted = LocalClient::new(&socket)
                .send_message(&UserMessage {
                    project: None,
                    text: "Anything new?".into(),
                    images: Vec::new(),
                    client_message_id: "m-1".into(),
                    task: None,
                })
                .await
                .unwrap();
            let mut event = None;
            for _ in 0..3_000 {
                let current = db.event(accepted.event_id).await.unwrap().unwrap();
                if current.status == EventStatus::Done {
                    event = Some(current);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            stop.send(()).unwrap();
            event
        };
        let (result, event) = tokio::join!(running, client);
        result.unwrap();
        let event = event.expect("the embedded runner ran the message");
        let run = db
            .run(event.claimed_by_run.expect("claimed by a run"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(run.status, RunStatus::Committed);
        // The lead's final text is its answer.
        assert_eq!(run.outcome, Some(RunOutcome::Sent));
        assert_eq!(run.rationale.as_deref(), Some("Nothing needs saying."));
        let lane = db.lane(event.lane_id).await.unwrap().unwrap();
        assert_eq!(lane.lease_owner, None, "the lane was released");
        assert!(
            dir.path()
                .join(crate::config::LANES_DIR)
                .join(lane.id.to_string())
                .is_dir(),
            "the run worked in <data dir>/lanes/<lane id>"
        );
    }
}
