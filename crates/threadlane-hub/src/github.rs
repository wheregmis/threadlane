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

use threadlane_core::client::{ApiError, PullRequestFiles, TaskCheckRun, TaskPullRequest};
use threadlane_core::forge::{
    BATCH_MAY_BE_READY, BATCH_MAY_HAVE_CHANGED, CLOSE_PULL_REQUEST, COMMENT, EDIT_PULL_REQUEST,
    MARK_READY, MERGE, REQUEST_REVIEW, RESOLVE_REVIEW_THREAD,
};
use threadlane_core::github::{
    DEFAULT_POLL_INTERVAL, Feed, GITHUB_SOURCE, GithubItem, GithubKind, GithubSource,
    PullRequestState, PullTarget, normalize_remote, pull_request_number,
};
use threadlane_core::ids::{LaneId, ProjectId, TaskId};
use threadlane_core::model::{
    Decision, EventKind, Inserted, NewEvent, Project, Repository, Task, TaskStatus, Trust,
};
use threadlane_core::runtime::steps::github::{
    AFTER_CURSOR, AFTER_FETCH, AFTER_INSERT, AFTER_WAKE,
};
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{
    DecisionStore, InboxStore, MachineStore, OrgStore, StoreError, TaskStore,
};
use threadlane_core::time::Timestamp;
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
    task: TaskId,
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

/// What the desktop is told about pull request `number` of `repository`.
fn task_pull_request(
    repository: &str,
    number: u64,
    state: &PullRequestState,
    refreshed_at: Timestamp,
) -> TaskPullRequest {
    TaskPullRequest {
        repository: repository.to_owned(),
        number,
        url: state.url.clone(),
        status: state.status(),
        head_sha: state.head_sha.clone(),
        checks: state
            .checks
            .iter()
            .map(|check| TaskCheckRun {
                name: check.name.clone(),
                status: check.status(),
            })
            .collect(),
        mergeable_state: state.mergeable_state.clone(),
        conflicted: state.conflicted(),
        refreshed_at,
    }
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

/// The pull requests an approved batch `merge` or `mark_ready` handled, in
/// order: all of them when it completed, else those before the one it
/// stopped at, as the runner's error names it ("batch stopped at #N"). The
/// stopped one counts too when the error flags it
/// ([`BATCH_MAY_HAVE_CHANGED`]): it may have merged before its state read
/// failed or timed out, and the task page's status read shows which; a
/// timed-out mark ready flags it with [`BATCH_MAY_BE_READY`]. Empty for any other
/// call.
fn batch_handled(
    tool: &str,
    arguments: &serde_json::Value,
    outcome: &serde_json::Value,
) -> Vec<u64> {
    let listed: Vec<u64> = match tool {
        MERGE => arguments["pulls"].as_array().map(|pulls| {
            pulls
                .iter()
                .filter_map(|pull| pull["number"].as_u64())
                .collect()
        }),
        MARK_READY => arguments["numbers"].as_array().map(|numbers| {
            numbers
                .iter()
                .filter_map(serde_json::Value::as_u64)
                .collect()
        }),
        _ => None,
    }
    .unwrap_or_default();
    if outcome["performed"] == true {
        return listed;
    }
    // The runner's error starts with the batch header, after the hub's optional "forge: ".
    let error = outcome["error"].as_str().unwrap_or_default();
    let Some(rest) = error
        .strip_prefix("forge: ")
        .unwrap_or(error)
        .strip_prefix("batch stopped at #")
    else {
        return Vec::new();
    };
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (stopped, after) = rest.split_at(digits);
    let stopped = stopped.parse::<u64>().ok();
    match stopped.and_then(|stopped| listed.iter().position(|n| *n == stopped)) {
        Some(at) => {
            // Only at its place in the header, so provider detail later in the error can't fake it.
            let changed = match tool {
                MERGE => arguments["pulls"][at]["expected_head"]
                    .as_str()
                    .is_some_and(|head| {
                        after.starts_with(&format!(
                            " (expected head {head}{BATCH_MAY_HAVE_CHANGED})"
                        ))
                    }),
                _ => after.starts_with(&format!(" ({BATCH_MAY_BE_READY})")),
            };
            listed.into_iter().take(at + usize::from(changed)).collect()
        }
        _ => Vec::new(),
    }
}

/// Adds to `list` the work `task`'s recorded `decisions` performed, in the
/// project's `repositories`.
fn push_task_work(
    project: &Project,
    repositories: &[Repository],
    task: &Task,
    decisions: &[Decision],
    list: &mut Vec<Work>,
) {
    let lane = if task.status.is_terminal() {
        project.lead_lane_id
    } else {
        task.lane_id
    };
    for decision in decisions {
        let Some(outcome) = decision.outcome.as_ref() else {
            continue;
        };
        let arguments = &decision.arguments;
        let batch = batch_handled(&decision.tool, arguments, outcome);
        if outcome["performed"] != true && batch.is_empty() {
            continue;
        }
        let names_pull_request = matches!(
            decision.tool.as_str(),
            MERGE
                | MARK_READY
                | CLOSE_PULL_REQUEST
                | EDIT_PULL_REQUEST
                | RESOLVE_REVIEW_THREAD
                | REQUEST_REVIEW
        ) || (decision.tool == COMMENT
            && arguments["target"] == "pull_request");
        let name = arguments["repository"].as_str().unwrap_or_default();
        // Dispatch runs a batch on any repository of the project, attached or not.
        let any_registered = !batch.is_empty();
        let targets: Vec<(&str, &str, Option<u64>)> = if !batch.is_empty() {
            batch.into_iter().map(|n| (name, "", Some(n))).collect()
        } else {
            match (outcome["repository"].as_str(), outcome["branch"].as_str()) {
                (Some(name), Some(branch)) => vec![(
                    name,
                    branch,
                    outcome["pull_request"]
                        .as_str()
                        .and_then(pull_request_number),
                )],
                _ if names_pull_request => {
                    match (
                        arguments["repository"].as_str(),
                        arguments["number"].as_u64(),
                    ) {
                        (Some(name), Some(number)) => vec![(name, "", Some(number))],
                        _ => continue,
                    }
                }
                _ => continue,
            }
        };
        for (name, branch, number) in targets {
            let Some(repository) = repositories
                .iter()
                .find(|repository| {
                    repository.name == name
                        && (any_registered || task.repositories.contains(&repository.id))
                })
                .and_then(|repository| normalize_remote(&repository.remote_url))
                .and_then(|key| key.strip_prefix(GITHUB_HOST).map(str::to_owned))
            else {
                continue;
            };
            match list.iter_mut().find(|work| {
                work.task == task.id
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
                    task: task.id,
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
        let mut decisions_by_lane: HashMap<LaneId, Vec<_>> = HashMap::new();
        for decision in db.decisions_with_outcome().await? {
            decisions_by_lane
                .entry(decision.lane_id)
                .or_default()
                .push(decision);
        }
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
                let Some(decisions) = decisions_by_lane.get(&task.lane_id) else {
                    continue;
                };
                let list = if task.status.is_terminal() {
                    &mut routes.ended
                } else {
                    &mut routes.work
                };
                push_task_work(&project, &repositories, &task, decisions, list);
            }
        }
        Ok(routes)
    }

    /// The work of one task, read from its own lane and project only.
    async fn of_task(db: &HubDb, id: TaskId) -> Result<Vec<Work>, StoreError> {
        let mut work = Vec::new();
        let Some(task) = db.task(id).await? else {
            return Ok(work);
        };
        if task.status == TaskStatus::Cancelled {
            return Ok(work);
        }
        let Some(project) = db.project(task.project_id).await? else {
            return Ok(work);
        };
        let repositories = db.repositories(project.id).await?;
        let decisions = db.lane_decisions(task.lane_id).await?;
        push_task_work(&project, &repositories, &task, &decisions, &mut work);
        Ok(work)
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
        self.refresh_pull_status(source.as_ref(), &followed, false)
            .await;
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
                let old = since.as_deref() > Some(item.at.as_str());
                if (old && !crate::gh::passed_check(item)) || (ended && !needs_attention(item)) {
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
        if feed.items.is_empty() {
            return Ok(HashMap::new());
        }
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

    /// Reads the state of each followed pull request into the status
    /// cache, once per pull request however many tasks follow it. A failed
    /// read keeps what was cached. With `fill_only`, an entry cached while
    /// the read was in flight, such as a poll's newer status, is kept.
    /// Answers whether every read succeeded.
    async fn refresh_pull_status(
        &self,
        source: &dyn GithubSource,
        followed: &[(Work, bool)],
        fill_only: bool,
    ) -> bool {
        let mut complete = true;
        let mut read: HashMap<(&str, u64), Option<TaskPullRequest>> = HashMap::new();
        for (work, _) in followed {
            let Some(number) = work.target.number else {
                continue;
            };
            let repository = work.target.repository.as_str();
            let pull = match read.get(&(repository, number)) {
                Some(pull) => pull.clone(),
                None => {
                    let pull = match source.pull_request_state(repository, number).await {
                        Ok(state) => Some(task_pull_request(
                            repository,
                            number,
                            &state,
                            self.inner.clock.now(),
                        )),
                        Err(error) => {
                            tracing::warn!(%error, repository, number, "could not read a task's pull request state");
                            complete = false;
                            None
                        }
                    };
                    read.insert((repository, number), pull.clone());
                    pull
                }
            };
            let Some(pull) = pull else {
                continue;
            };
            let mut cache = self.inner.pull_status.lock().await;
            let pulls = cache.entry(work.task).or_default();
            match pulls
                .iter_mut()
                .find(|known| known.repository == pull.repository && known.number == pull.number)
            {
                Some(_) if fill_only => {}
                Some(known) => *known = pull,
                None => pulls.push(pull),
            }
        }
        complete
    }

    /// The task's pull requests from the status cache. A task not cached
    /// yet, such as one that ended before this hub started, is read from
    /// GitHub and cached, even when it has none. A read that failed for any
    /// of its pull requests is not cached, so it is tried again next time.
    /// A cached task also reads each linked pull request the cache lacks,
    /// such as one a recorded outcome just linked, keeping the known ones.
    pub(crate) async fn task_pull_requests(
        &self,
        task: TaskId,
    ) -> Result<Vec<TaskPullRequest>, HubError> {
        let Some(source) = self.inner.github.as_ref() else {
            let cache = self.inner.pull_status.lock().await;
            return Ok(cache.get(&task).cloned().unwrap_or_default());
        };
        let routes = Routes::of_task(&self.inner.db, task).await?;
        // Read the cache only after the routes, so a reply never carries a status a poll replaced meanwhile.
        let (missing, cold) = {
            let cache = self.inner.pull_status.lock().await;
            let cached = cache.get(&task);
            let missing: Vec<_> = routes
                .into_iter()
                .filter(|work| {
                    cached.is_none_or(|pulls| {
                        work.target.number.is_some_and(|number| {
                            !pulls.iter().any(|pull| {
                                pull.repository == work.target.repository && pull.number == number
                            })
                        })
                    })
                })
                .map(|work| (work, false))
                .collect();
            if let Some(pulls) = cached
                && missing.is_empty()
            {
                return Ok(pulls.clone());
            }
            (missing, cached.is_none())
        };
        // Reads that succeeded stay cached; the ones that failed are retried next time.
        if !self
            .refresh_pull_status(source.as_ref(), &missing, true)
            .await
            && cold
        {
            let partial = self.inner.pull_status.lock().await.get(&task).cloned();
            return Ok(partial.unwrap_or_default());
        }
        Ok(self
            .inner
            .pull_status
            .lock()
            .await
            .entry(task)
            .or_default()
            .clone())
    }

    /// The files each of the task's pull requests changes, read from GitHub
    /// only when its head moved since the last read. Files read while the
    /// head moved are neither cached nor returned.
    pub(crate) async fn task_pull_files(
        &self,
        task: TaskId,
    ) -> Result<Vec<PullRequestFiles>, ApiError> {
        let pulls = self
            .task_pull_requests(task)
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        let mut answer = Vec::with_capacity(pulls.len());
        for pull in pulls {
            let key = (pull.repository.clone(), pull.number);
            let cached = self
                .inner
                .pull_files
                .lock()
                .await
                .get(&key)
                .filter(|(head, _)| *head == pull.head_sha)
                .map(|(_, files)| files.clone());
            let files =
                match cached {
                    Some(files) => files,
                    None => {
                        let source = self.inner.github.as_ref().ok_or_else(|| {
                            ApiError::Unreachable("this hub does not read GitHub".to_owned())
                        })?;
                        let files = source
                            .pull_request_files(&pull.repository, pull.number)
                            .await
                            .map_err(|error| ApiError::Unreachable(error.to_string()))?;
                        // The files carry no head, so check it did not move while they were read.
                        let state = source
                            .pull_request_state(&pull.repository, pull.number)
                            .await
                            .map_err(|error| ApiError::Unreachable(error.to_string()))?;
                        if state.head_sha != pull.head_sha {
                            let moved = task_pull_request(
                                &pull.repository,
                                pull.number,
                                &state,
                                self.inner.clock.now(),
                            );
                            if let Some(known) =
                                self.inner.pull_status.lock().await.get_mut(&task).and_then(
                                    |pulls| {
                                        pulls.iter_mut().find(|known| {
                                            known.repository == moved.repository
                                                && known.number == moved.number
                                        })
                                    },
                                )
                            {
                                *known = moved;
                            }
                            return Err(ApiError::Unreachable(format!(
                                "{}#{} moved to a new commit while its files were read; try again",
                                pull.repository, pull.number
                            )));
                        }
                        self.inner
                            .pull_files
                            .lock()
                            .await
                            .insert(key, (pull.head_sha.clone(), files.clone()));
                        files
                    }
                };
            answer.push(PullRequestFiles {
                repository: pull.repository,
                number: pull.number,
                head_sha: pull.head_sha,
                files,
            });
        }
        Ok(answer)
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
