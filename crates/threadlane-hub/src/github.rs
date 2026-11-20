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

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use threadlane_core::forge::{CLOSE_PULL_REQUEST, COMMENT, MARK_READY, MERGE};
use threadlane_core::github::{
    DEFAULT_POLL_INTERVAL, Feed, GITHUB_SOURCE, GithubItem, GithubKind, GithubSource, PullTarget,
    normalize_remote, pull_request_number,
};
use threadlane_core::ids::{LaneId, ProjectId};
use threadlane_core::model::{EventKind, Inserted, NewEvent, TaskStatus, Trust};
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// The work of tasks that have ended, routed to their project's lead
    /// lane. It is read only when the feed reports activity on it.
    ended: Vec<Work>,
}

#[derive(Default)]
struct LeadActivity {
    retry: bool,
    items: Vec<GithubItem>,
    /// Save the observed PR head only after delivering the activity.
    cursor: Option<(String, String)>,
}

/// The normalized remote of a repository GitHub names `owner/name`.
fn github_key(repository: &str) -> Option<String> {
    normalize_remote(&format!("https://{GITHUB_HOST}{repository}"))
}

impl Work {
    fn cursor_key(&self) -> String {
        match (self.target.branch.as_str(), self.target.number) {
            ("", Some(number)) => format!("github:{}#{number}", self.target.repository),
            (branch, _) => format!("github:{}@{branch}", self.target.repository),
        }
    }

    fn shares_target(&self, other: &Self) -> bool {
        self.target.repository == other.target.repository
            && ((!self.target.branch.is_empty() && self.target.branch == other.target.branch)
                || (self.target.number.is_some() && self.target.number == other.target.number))
    }

    /// Whether `item`, in the repository of normalized remote `key`, is
    /// about this work's pull request or branch.
    fn is_about(&self, item: &GithubItem, key: &str) -> bool {
        github_key(&self.target.repository).as_deref() == Some(key)
            && ((item.pull_request.is_some() && item.pull_request == self.target.number)
                || item.branch.as_deref() == Some(self.target.branch.as_str()))
    }
}

/// Whether an item about an ended task's work needs anyone: a check that
/// passed does not.
fn needs_attention(item: &GithubItem) -> bool {
    item.kind != GithubKind::CheckResult
        || ["failure", "timed_out", "action_required", "startup_failure"]
            .iter()
            .any(|conclusion| item.title.ends_with(&format!(": {conclusion}")))
}

impl Routes {
    /// The ended task's work `item` is about, unless a running task has
    /// taken that work over.
    fn ended_for(&self, item: &GithubItem) -> Option<&Work> {
        let key = github_key(&item.repository)?;
        if self.work.iter().any(|work| work.is_about(item, &key)) {
            return None;
        }
        self.ended.iter().find(|work| work.is_about(item, &key))
    }

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
                // A cancelled task's work was given up; a finished one's pull
                // request still gets checks and reviews, which go to the lead.
                if task.status == TaskStatus::Cancelled {
                    continue;
                }
                let (list, lane) = if task.status.is_terminal() {
                    (&mut routes.ended, project.lead_lane_id)
                } else {
                    (&mut routes.work, task.lane_id)
                };
                for decision in db.lane_decisions(task.lane_id).await? {
                    let Some(outcome) = decision.outcome.as_ref() else {
                        continue;
                    };
                    if outcome["performed"] != true {
                        continue;
                    }
                    let arguments = &decision.arguments;
                    let names_pull_request = matches!(
                        decision.tool.as_str(),
                        MERGE | MARK_READY | CLOSE_PULL_REQUEST
                    ) || (decision.tool == COMMENT
                        && arguments["target"] == "pull_request");
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
                    match list.iter_mut().find(|work| {
                        work.lane == lane
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
                        None => list.push(Work {
                            project: project.id,
                            lane,
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

    /// A fetched target can still be applied to its exact route, or to its
    /// own project's lead if it finished while the request was outstanding.
    fn revalidate(&self, work: &Work, was_ended: bool) -> Option<(&Work, bool)> {
        if !was_ended && let Some(active) = self.work.iter().find(|active| *active == work) {
            return Some((active, false));
        }
        // Do not notify the lead if an active task took over the same work.
        if self.work.iter().any(|active| active.shares_target(work)) {
            return None;
        }
        self.ended
            .iter()
            .find(|ended| ended.project == work.project && ended.target == work.target)
            .map(|ended| (ended, true))
    }

    /// Where a feed item goes: `(project, lane, requested)`, or `None` when
    /// its repository is not registered here.
    fn route(&self, item: &GithubItem) -> Option<(ProjectId, LaneId, bool)> {
        let key = github_key(&item.repository)?;
        let registered = self.registered.iter().find(|repo| repo.key == key)?;
        let task = self.work.iter().find(|work| work.is_about(item, &key));
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
        let _poll = inner.github_poll.lock().await;
        let cursor = inner.db.source_cursor(FEED_CURSOR).await?;
        let feed = match source.notifications(cursor.as_deref()).await {
            Ok(feed) => {
                inner.hook.reach(AFTER_FETCH)?;
                if let Some(interval) = feed.poll_interval {
                    pass.next_in = interval.max(Duration::from_secs(1));
                }
                Some(feed)
            }
            Err(error) => {
                tracing::warn!(%error, "could not read the GitHub notifications");
                None
            }
        };

        let activity = match &feed {
            Some(feed) => self.feed_activity(source.as_ref(), feed).await?,
            None => HashMap::new(),
        };

        let mut lanes = BTreeSet::new();
        let followed = {
            let _removal = inner.project_removal.lock().await;
            let routes = Routes::read(&inner.db).await?;
            let refresh = if let Some(feed) = feed {
                self.apply_github_feed(
                    &routes,
                    feed,
                    activity,
                    cursor.as_deref(),
                    &mut pass,
                    &mut lanes,
                )
                .await?
            } else {
                Vec::new()
            };
            routes
                .work
                .into_iter()
                .map(|work| (work, false))
                .chain(refresh.into_iter().map(|work| (work, true)))
                .collect::<Vec<_>>()
        };
        pass.inserted += self
            .refresh_github_work(source.as_ref(), followed, &mut lanes)
            .await?;

        for lane in &lanes {
            inner.wake.wake(WakeTarget::Lane(*lane));
        }
        inner.hook.reach(AFTER_WAKE)?;
        Ok(pass)
    }

    async fn apply_github_feed(
        &self,
        routes: &Routes,
        feed: Feed,
        mut activity: HashMap<String, LeadActivity>,
        cursor: Option<&str>,
        pass: &mut GithubPass,
        lanes: &mut BTreeSet<LaneId>,
    ) -> Result<Vec<Work>, HubError> {
        let inner = &self.inner;
        let mut refresh = Vec::new();
        let mut retry = false;
        for item in &feed.items {
            let ended = routes.ended_for(item);
            if let Some(work) = ended
                && !refresh.contains(work)
            {
                refresh.push(work.clone());
            }
            if let Some((project, lane, requested)) = routes.route(item) {
                if let Some(resolved) = activity.get_mut(&item.id).filter(|_| !requested) {
                    retry |= resolved.retry;
                    for mut detail in resolved.items.drain(..) {
                        let is_detail = detail.id != item.id;
                        if ended.is_some()
                            && (!needs_attention(&detail)
                                || (item.kind == GithubKind::CheckResult
                                    && detail.kind == GithubKind::PullRequestUpdate))
                        {
                            continue;
                        }
                        if resolved.retry && !is_detail && detail.kind == item.kind {
                            detail.id = format!("unresolved:{}", item.id);
                        } else if ended.is_none() && is_detail {
                            // Keep an unclaimed PR's detail available to a task assigned it later.
                            detail.id =
                                format!("notified:{}:{}:{}", detail.id, detail.at, detail.title);
                        }
                        let (project, lane, requested) = ended
                            .filter(|_| is_detail)
                            .map_or((project, lane, false), |work| {
                                (work.project, work.lane, true)
                            });
                        pass.inserted += self
                            .insert(project, lane, requested, &detail, lanes)
                            .await?;
                    }
                    if let Some((key, value)) = resolved.cursor.take() {
                        inner
                            .db
                            .set_source_cursor(&key, &value, inner.clock.now())
                            .await?;
                    }
                } else {
                    pass.inserted += self.insert(project, lane, requested, item, lanes).await?;
                }
            }
            inner.hook.reach(AFTER_INSERT)?;
        }
        if !retry && let Some(next) = feed.cursor.filter(|next| cursor != Some(next.as_str())) {
            inner
                .db
                .set_source_cursor(FEED_CURSOR, &next, inner.clock.now())
                .await?;
            inner.hook.reach(AFTER_CURSOR)?;
        }
        Ok(refresh)
    }

    async fn refresh_github_work(
        &self,
        source: &dyn GithubSource,
        followed: Vec<(Work, bool)>,
        lanes: &mut BTreeSet<LaneId>,
    ) -> Result<usize, HubError> {
        let inner = &self.inner;
        let mut fetched = Vec::new();
        for (work, ended) in followed {
            let key = work.cursor_key();
            let since = inner.db.source_cursor(&key).await?;
            match source.pull_request(&work.target, since.as_deref()).await {
                Ok(items) => {
                    inner.hook.reach(AFTER_FETCH)?;
                    if !items.is_empty() {
                        fetched.push((work, ended, key, items));
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, target = ?work.target, "could not read a task's work on GitHub");
                }
            }
        }
        if fetched.is_empty() {
            return Ok(0);
        }
        let _removal = inner.project_removal.lock().await;
        // A target may have been deleted, cancelled, or changed while GitHub was
        // queried. Do not write events or advance that target's cursor until
        // its current route has been verified.
        let current = Routes::read(&inner.db).await?;
        let mut inserted = 0;
        for (work, ended, key, items) in fetched {
            let Some((work, ended)) = current.revalidate(&work, ended) else {
                continue;
            };
            // Earlier results in this pass may share the same cursor key.
            let since = inner.db.source_cursor(&key).await?;
            for item in &items {
                if since.as_deref() > Some(item.at.as_str()) || (ended && !needs_attention(item)) {
                    continue;
                }
                inserted += self
                    .insert(work.project, work.lane, true, item, lanes)
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
        Ok(inserted)
    }

    /// What each new notification about a pull request reports for the
    /// lead (see [`GithubSource::activity`]), by notification id. Read
    /// before the project-removal lock, as GitHub can be slow; the routes
    /// are read again under it. A notification already inserted is not read
    /// again once its thread cursor is saved.
    async fn feed_activity(
        &self,
        source: &dyn GithubSource,
        feed: &Feed,
    ) -> Result<HashMap<String, LeadActivity>, HubError> {
        let inner = &self.inner;
        let routes = Routes::read(&inner.db).await?;
        let mut activity = HashMap::new();
        let mut observed = HashMap::new();
        for item in &feed.items {
            let for_lead = matches!(routes.route(item), Some((_, _, false)));
            if !for_lead || item.pull_request.is_none() || activity.contains_key(&item.id) {
                continue;
            }
            // shortcut: one cursor per observed PR thread; prune alongside event history
            // if the hub adds retention.
            let thread = item.id.strip_suffix(&item.at).unwrap_or(&item.id);
            let key = format!("github:lead:{thread}");
            let previous = match observed.get(&key) {
                Some(value) => Some(String::clone(value)),
                None => inner.db.source_cursor(&key).await?,
            };
            let previous = previous.as_deref().and_then(|s| s.split_once('\n'));
            let mut resolved = LeadActivity::default();
            if previous.is_some_and(|(at, _)| at >= item.at.as_str()) {
                activity.insert(item.id.clone(), resolved);
                continue;
            }
            if let Err(error) = source.activity(item, &mut resolved.items).await {
                tracing::warn!(%error, id = item.id, "could not read all pull request activity");
                resolved.retry = true;
                if resolved.items.is_empty() && previous.is_none() {
                    resolved.items.push(item.clone());
                }
            }
            if let Some(head) = resolved.items.first_mut()
                && matches!(head.kind, GithubKind::PullRequestUpdate | GithubKind::Push)
            {
                // Recover the original observation if events survived a crash or a partial read.
                if let Some(payload) = inner
                    .db
                    .source_event_payload(GITHUB_SOURCE, &item.id)
                    .await?
                {
                    *head = serde_json::from_value(payload).map_err(|error| {
                        StoreError::Invalid(format!("a stored GitHub item: {error}"))
                    })?;
                }
                let summary = format!("{}\n{}", head.title, head.body);
                // An incomplete read observed the head but must still retry this timestamp.
                let completed_at = if resolved.retry {
                    previous.map_or("", |(at, _)| at)
                } else {
                    &item.at
                };
                let value = format!("{completed_at}\n{summary}");
                observed.insert(key.clone(), value.clone());
                resolved.cursor = Some((key, value));
                if previous.is_some_and(|(_, last)| last == summary) {
                    resolved.items.remove(0);
                } else {
                    if previous.is_some_and(|(_, last)| last.lines().next() != Some(&head.title)) {
                        head.kind = GithubKind::Push;
                    }
                    resolved.items.rotate_left(1);
                }
            }
            activity.insert(item.id.clone(), resolved);
        }
        Ok(activity)
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
