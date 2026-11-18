//! The GitHub poller (Plan.md §6.6, docs/milestone-8.md §3): one per hub,
//! pulling through a [`GithubSource`] and following the source contract of
//! Plan.md §6.3: normalize, route, insert deduplicated by the service's own
//! id, wake.
//!
//! Routing (Plan.md §4.2): an item about a task's branch or pull request goes
//! to that task's lane as work you requested, since it continues the task
//! (§2.3); anything else about a repository registered in this hub goes to
//! its project's lead lane, not requested, so under On request it is recorded
//! and starts no run (decision 7); an item about any other repository is
//! dropped and creates no event (invariant 19).
//!
//! A task's work is what was performed for it: the outcome of each
//! approved `publish_branch` or `open_pull_request` names the repository,
//! the branch and the pull request's URL (`crate::publish`), and the
//! arguments of each approved `comment` on a pull request or `merge` name
//! the repository and the pull request's number, whose branch the source
//! looks up.

use std::collections::BTreeSet;
use std::time::Duration;

use threadlane_core::forge::{COMMENT, MERGE};
use threadlane_core::github::{
    DEFAULT_POLL_INTERVAL, GITHUB_SOURCE, GithubItem, PullTarget, normalize_remote,
    pull_request_number,
};
use threadlane_core::ids::{LaneId, ProjectId};
use threadlane_core::model::{EventKind, Inserted, NewEvent, Trust};
use threadlane_core::runtime::steps::github::{
    AFTER_CURSOR, AFTER_FETCH, AFTER_INSERT, AFTER_WAKE,
};
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{
    DecisionStore, InboxStore, MachineStore, OrgStore, StoreError, TaskStore,
};
use threadlane_store::HubDb;

use crate::core::HubCore;
use crate::error::HubError;

/// The cursor of the notifications feed in `source_cursors`.
const FEED_CURSOR: &str = "github:notifications";
/// The host whose repositories the source reads.
const GITHUB_HOST: &str = "github.com/";

/// What one poller pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GithubPass {
    /// How many events it inserted.
    pub inserted: usize,
    /// How long to wait before the next pass: the interval the service
    /// asked for, or [`DEFAULT_POLL_INTERVAL`].
    pub next_in: Duration,
}

/// A repository registered in this hub, by its normalized remote.
#[derive(Debug)]
struct Registered {
    key: String,
    project: ProjectId,
    lead_lane: LaneId,
}

/// One active task's branch, and pull request once opened or acted on, in
/// one repository.
#[derive(Debug)]
struct Work {
    project: ProjectId,
    lane: LaneId,
    target: PullTarget,
}

/// Where items go in this hub, as its stores say now.
#[derive(Debug, Default)]
struct Routes {
    registered: Vec<Registered>,
    work: Vec<Work>,
}

/// The normalized remote of a repository GitHub names `owner/name`.
fn github_key(repository: &str) -> Option<String> {
    normalize_remote(&format!("https://{GITHUB_HOST}{repository}"))
}

impl Routes {
    /// Reads every registered repository and every active task's work.
    async fn read(db: &HubDb) -> Result<Self, StoreError> {
        let mut routes = Self::default();
        for project in db.projects().await? {
            let repositories = db.repositories(project.id).await?;
            routes
                .registered
                .extend(repositories.iter().filter_map(|repository| {
                    Some(Registered {
                        key: normalize_remote(&repository.remote_url)?,
                        project: project.id,
                        lead_lane: project.lead_lane_id,
                    })
                }));
            for task in db.tasks(project.id).await? {
                if task.status.is_terminal() {
                    continue;
                }
                for decision in db.lane_decisions(task.lane_id).await? {
                    let Some(outcome) = decision.outcome.as_ref() else {
                        continue;
                    };
                    if outcome["performed"] != true {
                        continue;
                    }
                    let arguments = &decision.arguments;
                    let names_pull_request = decision.tool == MERGE
                        || (decision.tool == COMMENT && arguments["target"] == "pull_request");
                    let (name, branch, number) =
                        match (outcome["repository"].as_str(), outcome["branch"].as_str()) {
                            (Some(name), Some(branch)) => (
                                name,
                                branch,
                                outcome["pull_request"]
                                    .as_str()
                                    .and_then(pull_request_number),
                            ),
                            _ if names_pull_request => {
                                let (Some(name), Some(number)) = (
                                    arguments["repository"].as_str(),
                                    arguments["number"].as_u64(),
                                ) else {
                                    continue;
                                };
                                (name, "", Some(number))
                            }
                            _ => continue,
                        };
                    let Some(repository) = repositories
                        .iter()
                        .find(|repository| {
                            repository.name == name && task.repositories.contains(&repository.id)
                        })
                        .and_then(|repository| normalize_remote(&repository.remote_url))
                        .and_then(|key| key.strip_prefix(GITHUB_HOST).map(str::to_owned))
                    else {
                        continue;
                    };
                    match routes.work.iter_mut().find(|work| {
                        work.lane == task.lane_id
                            && work.target.repository == repository
                            && ((!branch.is_empty() && work.target.branch == branch)
                                || (number.is_some() && work.target.number == number))
                    }) {
                        Some(work) => {
                            work.target.number = work.target.number.or(number);
                            if work.target.branch.is_empty() {
                                branch.clone_into(&mut work.target.branch);
                            }
                        }
                        None => routes.work.push(Work {
                            project: project.id,
                            lane: task.lane_id,
                            target: PullTarget {
                                repository,
                                branch: branch.to_owned(),
                                number,
                            },
                        }),
                    }
                }
            }
        }
        Ok(routes)
    }

    /// Where a feed item goes: `(project, lane, requested)`, or `None` when
    /// its repository is not registered here.
    fn route(&self, item: &GithubItem) -> Option<(ProjectId, LaneId, bool)> {
        let key = github_key(&item.repository)?;
        let registered = self.registered.iter().find(|repo| repo.key == key)?;
        let task = self.work.iter().find(|work| {
            github_key(&work.target.repository).as_deref() == Some(key.as_str())
                && ((item.pull_request.is_some() && item.pull_request == work.target.number)
                    || item.branch.as_deref() == Some(work.target.branch.as_str()))
        });
        Some(match task {
            Some(work) => (work.project, work.lane, true),
            None => (registered.project, registered.lead_lane, false),
        })
    }
}

impl HubCore {
    /// One poller pass (Plan.md §6.6), if this hub has a GitHub source and
    /// its machine reports `gh` logged in: the notifications feed since its
    /// cursor, then each active task's branch and pull request since theirs.
    /// Every item is inserted once however often it is pulled, cursors
    /// are saved after their items, and the lanes that got events are woken.
    ///
    /// # Errors
    ///
    /// A store error, or [`HubError::Killed`] at an injected crash. A
    /// failed call to GitHub is logged and retried on the next pass.
    pub async fn poll_github_once(&self) -> Result<GithubPass, HubError> {
        let inner = &self.inner;
        let mut pass = GithubPass {
            inserted: 0,
            next_in: DEFAULT_POLL_INTERVAL,
        };
        let Some(source) = inner.github.as_ref() else {
            return Ok(pass);
        };
        let logged_in = inner
            .db
            .machines()
            .await?
            .iter()
            .any(|machine| machine.is_hub && machine.gh_login);
        if !logged_in {
            return Ok(pass);
        }
        let _removal = inner.project_removal.lock().await;
        let routes = Routes::read(&inner.db).await?;
        let mut lanes = BTreeSet::new();

        let cursor = inner.db.source_cursor(FEED_CURSOR).await?;
        match source.notifications(cursor.as_deref()).await {
            Ok(feed) => {
                inner.hook.reach(AFTER_FETCH)?;
                if let Some(interval) = feed.poll_interval {
                    pass.next_in = interval.max(Duration::from_secs(1));
                }
                for item in &feed.items {
                    if let Some((project, lane, requested)) = routes.route(item) {
                        pass.inserted += self
                            .insert(project, lane, requested, item, &mut lanes)
                            .await?;
                    }
                    inner.hook.reach(AFTER_INSERT)?;
                }
                if let Some(next) = feed.cursor.filter(|next| cursor.as_ref() != Some(next)) {
                    inner
                        .db
                        .set_source_cursor(FEED_CURSOR, &next, inner.clock.now())
                        .await?;
                    inner.hook.reach(AFTER_CURSOR)?;
                }
            }
            Err(error) => tracing::warn!(%error, "could not read the GitHub notifications"),
        }

        for work in &routes.work {
            let key = match (work.target.branch.as_str(), work.target.number) {
                ("", Some(number)) => format!("github:{}#{number}", work.target.repository),
                (branch, _) => format!("github:{}@{branch}", work.target.repository),
            };
            let since = inner.db.source_cursor(&key).await?;
            let items = match source.pull_request(&work.target, since.as_deref()).await {
                Ok(items) => items,
                Err(error) => {
                    tracing::warn!(%error, target = ?work.target, "could not read a task's work on GitHub");
                    continue;
                }
            };
            inner.hook.reach(AFTER_FETCH)?;
            for item in &items {
                pass.inserted += self
                    .insert(work.project, work.lane, true, item, &mut lanes)
                    .await?;
                inner.hook.reach(AFTER_INSERT)?;
            }
            let latest = items.iter().map(|item| item.at.as_str()).max();
            if let Some(latest) = latest.filter(|latest| since.as_deref() < Some(*latest)) {
                inner
                    .db
                    .set_source_cursor(&key, latest, inner.clock.now())
                    .await?;
                inner.hook.reach(AFTER_CURSOR)?;
            }
        }

        for lane in &lanes {
            inner.wake.wake(WakeTarget::Lane(*lane));
        }
        inner.hook.reach(AFTER_WAKE)?;
        Ok(pass)
    }

    /// Inserts one item as an `outside_event` of the GitHub source. Returns
    /// 1 if it is new, 0 if it was there already.
    async fn insert(
        &self,
        project: ProjectId,
        lane: LaneId,
        requested: bool,
        item: &GithubItem,
        lanes: &mut BTreeSet<LaneId>,
    ) -> Result<usize, HubError> {
        let payload = serde_json::to_value(item)
            .map_err(|error| StoreError::Invalid(format!("a GitHub item: {error}")))?;
        let inserted = self
            .inner
            .db
            .insert_event(&NewEvent {
                project_id: project,
                lane_id: lane,
                source: GITHUB_SOURCE.to_owned(),
                source_event_id: item.id.clone(),
                kind: EventKind::OutsideEvent,
                payload,
                trust: Trust::External,
                requested,
                session_ref: None,
                decision_ref: None,
                reply_to_ref: None,
                received_at: self.inner.clock.now(),
            })
            .await?;
        Ok(match inserted {
            Inserted::New(_) => {
                lanes.insert(lane);
                1
            }
            Inserted::Duplicate(_) => 0,
        })
    }

    pub(crate) async fn github_loop(&self) -> Result<(), HubError> {
        if self.inner.github.is_none() {
            return std::future::pending().await;
        }
        loop {
            let wait = match self.poll_github_once().await {
                Ok(pass) => {
                    if pass.inserted > 0 {
                        tracing::debug!(inserted = pass.inserted, "pulled GitHub events");
                    }
                    pass.next_in
                }
                Err(HubError::Killed(killed)) => return Err(killed.into()),
                Err(error) => {
                    tracing::error!(%error, "GitHub poller pass failed");
                    DEFAULT_POLL_INTERVAL
                }
            };
            self.inner.clock.sleep(wait).await;
        }
    }
}
