//! The desktop app's state (Plan.md §11.1): what the shell, Home, a project,
//! a task, and the machines screen show, and how hub replies and feed items
//! change it. Plain Rust with no user interface in it; the GPUI views in
//! `main.rs` and `screens/` only draw it. Every hub call goes through
//! [`ClientCore`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value as Json;
use threadlane_client::{
    ClientCore, ClientError, FeedStream, HubStatus, IssuedCode, SpaceAnswer, SpaceDecisions,
    SpaceFeedItem, SpaceListing, SpaceMachines,
};
use threadlane_core::admission::AdmittedKey;
use threadlane_core::client::{
    BindingSummary, ConversationEntry, ProjectSummary, TaskDetail, TaskSummary,
};
use threadlane_core::ids::{DecisionId, EventId, ProjectId, TaskId};
use threadlane_core::model::{Account, Agent, Decision, Machine, MachineStatus, OutboxKind};
use threadlane_core::time::Timestamp;
use tokio::sync::{RwLock, mpsc};

mod clock;
mod conversation;
mod machines;
mod tasks;

pub use clock::{When, duration};
pub use conversation::Line;
pub use machines::{MachineRow, machine_row};
pub use tasks::{BoardCard, Column, Moment, chips, status_label, timeline};

/// How many of the newest delivered items Home asks each hub for.
const RECENT: u32 = 50;

/// How many conversation entries one page holds.
pub const PAGE: u32 = 100;

/// How many results Home lists.
const RECENT_SHOWN: usize = 8;

/// The key prefix of the hub's account notices, which end in `:blocked`
/// or `:resumed` (`threadlane-store`, Plan.md §5.3).
const ACCOUNT_NOTICE: &str = "system:account:";

/// One paired hub as the app shows it. Everything but `unreachable` is its
/// last report, kept while the hub is away (Plan.md §11.3).
#[derive(Debug, Clone, PartialEq)]
pub struct Space {
    /// The name the hub was paired under.
    pub name: String,
    /// Why the hub could not be reached at the last refresh; `None` when it
    /// answered.
    pub unreachable: Option<String>,
    /// Its projects.
    pub projects: Vec<ProjectSummary>,
    /// Its running tasks, oldest first.
    pub running: Vec<TaskSummary>,
    /// The machines that joined it.
    pub machines: Vec<Machine>,
    /// Its model accounts.
    pub accounts: Vec<Account>,
    /// The client keys it admitted.
    pub clients: Vec<AdmittedKey>,
    /// Its agents.
    pub agents: Vec<Agent>,
}

impl Space {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            unreachable: None,
            projects: Vec::new(),
            running: Vec::new(),
            machines: Vec::new(),
            accounts: Vec::new(),
            clients: Vec::new(),
            agents: Vec::new(),
        }
    }

    /// Its machines that are online.
    #[must_use]
    pub fn online(&self) -> usize {
        self.machines
            .iter()
            .filter(|machine| machine.status == MachineStatus::Online)
            .count()
    }
}

/// A decision waiting on you, labeled with its space.
#[derive(Debug, Clone, PartialEq)]
pub struct Queued {
    /// The space whose hub asked.
    pub space: String,
    /// The decision as that hub listed it.
    pub decision: Decision,
}

/// A decision card on the feed: what the hub put in its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    /// The decision's id, for example `D-0012`.
    pub id: String,
    /// The question asked.
    pub question: String,
    /// The labels of its options, in order.
    pub options: Vec<String>,
}

impl Card {
    /// The card of a decision item; `None` for any other item, or a body
    /// without a decision id.
    #[must_use]
    pub fn of(item: &SpaceFeedItem) -> Option<Self> {
        if item.kind() != OutboxKind::Decision {
            return None;
        }
        let body = &item.item.message.body;
        let id = body
            .get("decision")
            .and_then(Json::as_str)
            .map(str::to_owned)
            .or_else(|| item.item.message.decision_id.map(|id| id.to_string()))?;
        let options = body
            .get("options")
            .and_then(Json::as_array)
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(Json::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let question = body
            .get("question")
            .and_then(Json::as_str)
            .map_or_else(|| item.text(), str::to_owned);
        Some(Self {
            id,
            question,
            options,
        })
    }
}

/// Something on the feed that raises a system notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alert {
    /// A new decision; its options become the notification's buttons.
    Decision {
        /// The space whose hub asked.
        space: String,
        /// `<space>/<project>` or `<space>`.
        label: String,
        /// The decision card.
        card: Card,
    },
    /// A system notice.
    Notice {
        /// `<space>/<project>` or `<space>`.
        label: String,
        /// The notice's text.
        text: String,
        /// Its feed position, which makes its tag unique.
        seq: i64,
    },
}

impl Alert {
    /// The notification's tag: `<space>/<decision id>` for a decision, so a
    /// response can be routed back with [`decision_of_tag`].
    #[must_use]
    pub fn tag(&self) -> String {
        match self {
            Self::Decision { space, card, .. } => format!("{space}/{}", card.id),
            Self::Notice { label, seq, .. } => format!("notice:{label}:{seq}"),
        }
    }

    /// The notification's headline.
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::Decision { label, card, .. } => format!("{label}: decision {}", card.id),
            Self::Notice { label, .. } => format!("{label}: notice"),
        }
    }

    /// The notification's text.
    #[must_use]
    pub fn body(&self) -> String {
        match self {
            Self::Decision { card, .. } => card.question.clone(),
            Self::Notice { text, .. } => text.clone(),
        }
    }

    /// The notification's buttons: a decision's option labels.
    #[must_use]
    pub fn options(&self) -> &[String] {
        match self {
            Self::Decision { card, .. } => &card.options,
            Self::Notice { .. } => &[],
        }
    }
}

/// The space and decision id a decision notification's tag names; `None`
/// for any other tag.
#[must_use]
pub fn decision_of_tag(tag: &str) -> Option<(&str, &str)> {
    tag.split_once('/')
        .filter(|(_, id)| DecisionId::parse(id).is_some())
}

/// What one refresh learned from every paired hub. Made by [`refresh`],
/// applied with [`AppState::apply`].
#[derive(Debug)]
pub struct Refresh {
    statuses: Vec<HubStatus>,
    projects: Vec<(String, Result<Vec<ProjectSummary>, ClientError>)>,
    agents: Vec<(String, Result<Vec<Agent>, ClientError>)>,
    clients: Vec<(String, Result<Vec<AdmittedKey>, ClientError>)>,
    decisions: Vec<SpaceDecisions>,
    running: Vec<SpaceListing<Vec<TaskSummary>>>,
    machines: Vec<SpaceListing<SpaceMachines>>,
    recent: Vec<SpaceListing<Vec<SpaceFeedItem>>>,
}

/// Asks every paired hub about itself, its projects, agents, admitted
/// clients, pending decisions, running tasks, machines, and newest items. A
/// hub that does not answer is reported as such; the others are unaffected.
pub async fn refresh(core: &ClientCore) -> Refresh {
    let statuses = core.status().await;
    let mut projects = Vec::new();
    let mut agents = Vec::new();
    let mut clients = Vec::new();
    for status in statuses.iter().filter(|status| status.is_reachable()) {
        let name = status.name.as_str();
        let listed = core.projects(Some(name)).await;
        projects.push((name.to_owned(), listed.map(|listed| listed.projects)));
        agents.push((name.to_owned(), core.agents(Some(name)).await));
        clients.push((name.to_owned(), core.clients(Some(name)).await));
    }
    // The only error with no space named is "no hub paired": nothing to show.
    Refresh {
        statuses,
        projects,
        agents,
        clients,
        decisions: core.decisions(None).await.unwrap_or_default(),
        running: core.running(None).await.unwrap_or_default(),
        machines: core.machines(None).await.unwrap_or_default(),
        recent: core.recent(None, RECENT).await.unwrap_or_default(),
    }
}

/// What the project screen loaded for one project. Made by
/// [`load_project`], applied with [`AppState::apply_project`].
#[derive(Debug)]
pub struct ProjectLoad {
    space: String,
    project: String,
    tasks: Result<Vec<TaskSummary>, ClientError>,
    repositories: Result<Vec<threadlane_core::model::Repository>, ClientError>,
    bindings: Result<Vec<BindingSummary>, ClientError>,
    history: Result<Vec<ConversationEntry>, ClientError>,
}

/// Asks a project's hub for its tasks, repositories, bindings, and the
/// newest page of its conversation.
pub async fn load_project(core: &ClientCore, space: &str, project: &str) -> ProjectLoad {
    let (in_space, named) = (Some(space), Some(project));
    ProjectLoad {
        space: space.to_owned(),
        project: project.to_owned(),
        tasks: core.tasks(in_space, named).await,
        repositories: core.repositories(in_space, named).await,
        bindings: core.bindings(in_space, named).await,
        history: core.conversation(in_space, named, None, PAGE).await,
    }
}

/// An earlier page of a conversation. Made by [`load_earlier`], applied
/// with [`AppState::apply_earlier`].
#[derive(Debug)]
pub struct EarlierLoad {
    space: String,
    project: String,
    history: Result<Vec<ConversationEntry>, ClientError>,
}

/// Asks for the page of a conversation before what is shown.
pub async fn load_earlier(
    core: &ClientCore,
    space: &str,
    project: &str,
    before: Timestamp,
) -> EarlierLoad {
    EarlierLoad {
        space: space.to_owned(),
        project: project.to_owned(),
        history: core
            .conversation(Some(space), Some(project), Some(before), PAGE)
            .await,
    }
}

/// The project boards, conversations and tasks are keyed by space first:
/// names and ids are per space (Plan.md §11.3).
type InSpace<T> = (String, T);

/// What the project screen shows of one project besides its conversation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectView {
    /// Its tasks, oldest first.
    pub tasks: Vec<TaskSummary>,
    /// Its repositories.
    pub repositories: Vec<threadlane_core::model::Repository>,
    /// The agents bound in it, with their modes.
    pub bindings: Vec<BindingSummary>,
}

/// A message you sent, as the conversation shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Said {
    project: String,
    text: String,
    at: Timestamp,
}

/// How far back a conversation is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Loaded {
    /// The oldest entry loaded.
    oldest: Option<Timestamp>,
    /// Whether the hub may have entries before it.
    more: bool,
}

/// Everything the app shows.
#[derive(Debug, Default)]
pub struct AppState {
    spaces: Vec<Space>,
    queue: Vec<Queued>,
    /// Every delivered item seen, from the feed or from history, by space
    /// and feed position, so one delivered both ways is shown once.
    delivered: BTreeMap<InSpace<i64>, SpaceFeedItem>,
    /// `(space, seq)` of every item taken from the feed, so a replay
    /// notifies once.
    notified: HashSet<InSpace<i64>>,
    /// Your messages, by space and event id.
    said: BTreeMap<InSpace<EventId>, Said>,
    loaded: HashMap<InSpace<String>, Loaded>,
    projects: HashMap<InSpace<String>, ProjectView>,
    tasks: HashMap<InSpace<TaskId>, TaskDetail>,
    /// The space Home shows; `None` for all.
    filter: Option<String>,
    /// Where the Home composer sends: space and project.
    target: Option<InSpace<String>>,
    codes: HashMap<String, IssuedCode>,
}

impl AppState {
    /// The paired hubs, in the order they were paired.
    #[must_use]
    pub fn spaces(&self) -> &[Space] {
        &self.spaces
    }

    /// The decisions waiting on you across every space, oldest first.
    #[must_use]
    pub fn queue(&self) -> &[Queued] {
        &self.queue
    }

    /// Whether decision `id` of `space` is still waiting on you.
    #[must_use]
    pub fn is_pending(&self, space: &str, id: &str) -> bool {
        self.queue
            .iter()
            .any(|queued| queued.space == space && queued.decision.id.to_string() == id)
    }

    /// The name of project `id` in `space`, if its hub has listed it.
    #[must_use]
    pub fn project_name(&self, space: &str, id: ProjectId) -> Option<&str> {
        self.space(space)?
            .projects
            .iter()
            .find(|project| project.id == id)
            .map(|project| project.name.as_str())
    }

    /// The paired hub named `name`.
    #[must_use]
    pub fn space(&self, name: &str) -> Option<&Space> {
        self.spaces.iter().find(|space| space.name == name)
    }

    /// The space Home shows, or `None` for all of them.
    #[must_use]
    pub fn filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    /// Shows one space on Home, or every space with `None`.
    pub fn set_filter(&mut self, space: Option<&str>) {
        self.filter = space.map(str::to_owned);
    }

    fn shown(&self, space: &str) -> bool {
        self.filter.as_deref().is_none_or(|only| only == space)
    }

    /// The decisions Home lists: those of the spaces it shows, oldest first.
    pub fn needs_you(&self) -> impl Iterator<Item = &Queued> {
        self.queue.iter().filter(|queued| self.shown(&queued.space))
    }

    /// How many decisions wait on you in project `id` of `space`.
    #[must_use]
    pub fn open_in(&self, space: &str, id: ProjectId) -> usize {
        self.queue
            .iter()
            .filter(|queued| queued.space == space && queued.decision.project_id == id)
            .count()
    }

    /// The decision waiting on you in task `task` of `space`, if any.
    #[must_use]
    pub fn open_for(&self, space: &str, task: TaskId) -> Option<&Queued> {
        self.queue
            .iter()
            .find(|queued| queued.space == space && queued.decision.task_id == Some(task))
    }

    /// The running tasks Home lists, with their space, oldest first.
    #[must_use]
    pub fn running(&self) -> Vec<(&str, &TaskSummary)> {
        self.spaces
            .iter()
            .filter(|space| self.shown(&space.name))
            .flat_map(|space| {
                space
                    .running
                    .iter()
                    .map(move |task| (space.name.as_str(), task))
            })
            .collect()
    }

    /// Home's recent results: the latest message or notice of each project
    /// (and of each space as a whole) in the spaces it shows, newest first.
    #[must_use]
    pub fn recent(&self) -> Vec<&SpaceFeedItem> {
        let mut latest: HashMap<(&str, Option<&str>), &SpaceFeedItem> = HashMap::new();
        for item in self.delivered.values() {
            if item.kind() == OutboxKind::Decision || !self.shown(&item.space) {
                continue;
            }
            let key = (item.space.as_str(), item.item.project_name.as_deref());
            let newer = latest
                .get(&key)
                .is_none_or(|known| known.item.delivered_at <= item.item.delivered_at);
            if newer {
                latest.insert(key, item);
            }
        }
        let mut recent: Vec<_> = latest.into_values().collect();
        recent.sort_by_key(|item| std::cmp::Reverse((item.item.delivered_at, item.item.seq)));
        recent.truncate(RECENT_SHOWN);
        recent
    }

    /// The text of the notice that says `space` is blocked for lack of a
    /// model account, while no later notice says it resumed.
    #[must_use]
    pub fn blocked(&self, space: &str) -> Option<String> {
        let latest = self
            .delivered
            .range((space.to_owned(), i64::MIN)..=(space.to_owned(), i64::MAX))
            .map(|(_, item)| item)
            .filter(|item| item.kind() == OutboxKind::SystemNotice)
            .filter(|item| {
                item.item
                    .message
                    .tool_call_id
                    .as_deref()
                    .is_some_and(|key| key.starts_with(ACCOUNT_NOTICE))
            })
            .max_by_key(|item| item.item.seq)?;
        let key = latest.item.message.tool_call_id.as_deref()?;
        key.ends_with(":blocked").then(|| latest.text())
    }

    /// Home's status line: `2 spaces reachable · 3 running · 2 need you · 2
    /// of 2 machines online`, for the spaces it shows.
    #[must_use]
    pub fn status_line(&self) -> String {
        let shown: Vec<&Space> = self
            .spaces
            .iter()
            .filter(|space| self.shown(&space.name))
            .collect();
        let reachable = shown.iter().filter(|s| s.unreachable.is_none()).count();
        let machines: usize = shown.iter().map(|space| space.machines.len()).sum();
        let online: usize = shown.iter().map(|space| space.online()).sum();
        let spaces = if reachable == 1 { "space" } else { "spaces" };
        format!(
            "{reachable} {spaces} reachable · {} running · {} need you · {online} of {machines} \
             machines online",
            self.running().len(),
            self.needs_you().count(),
        )
    }

    /// Every project the composer can send to, as `(space, project)`.
    #[must_use]
    pub fn targets(&self) -> Vec<(&str, &str)> {
        self.spaces
            .iter()
            .flat_map(|space| {
                space
                    .projects
                    .iter()
                    .map(move |project| (space.name.as_str(), project.name.as_str()))
            })
            .collect()
    }

    /// Where the Home composer sends: the last project chosen or sent to,
    /// else the General project of the first space shown (Plan.md §2.1).
    #[must_use]
    pub fn target(&self) -> Option<(&str, &str)> {
        if let Some((space, project)) = &self.target {
            return Some((space, project));
        }
        self.spaces
            .iter()
            .filter(|space| self.shown(&space.name))
            .find_map(|space| {
                let general = space.projects.iter().find(|project| project.is_general)?;
                Some((space.name.as_str(), general.name.as_str()))
            })
    }

    /// Makes the Home composer send to `project` of `space`.
    pub fn set_target(&mut self, space: &str, project: &str) {
        self.target = Some((space.to_owned(), project.to_owned()));
    }

    /// What the project screen shows of `project` in `space`, once loaded.
    #[must_use]
    pub fn project(&self, space: &str, project: &str) -> Option<&ProjectView> {
        self.projects.get(&(space.to_owned(), project.to_owned()))
    }

    /// Task `task` of `space` in full, once loaded.
    #[must_use]
    pub fn task(&self, space: &str, task: TaskId) -> Option<&TaskDetail> {
        self.tasks.get(&(space.to_owned(), task))
    }

    /// The join code last issued by `space`, if any.
    #[must_use]
    pub fn code(&self, space: &str) -> Option<&IssuedCode> {
        self.codes.get(space)
    }

    /// Takes a join code its hub issued.
    pub fn issued(&mut self, code: IssuedCode) {
        self.codes.insert(code.space.clone(), code);
    }

    /// Takes what a refresh learned. A hub that could not be reached, or
    /// could not list something, keeps what it last reported.
    pub fn apply(&mut self, refresh: Refresh) {
        let Refresh {
            statuses,
            projects,
            agents,
            clients,
            decisions,
            running,
            machines,
            recent,
        } = refresh;
        let mut spaces = Vec::with_capacity(statuses.len());
        for status in statuses {
            let mut space = self
                .space(&status.name)
                .cloned()
                .unwrap_or_else(|| Space::new(&status.name));
            space.unreachable = status.info.err().map(|error| error.to_string());
            spaces.push(space);
        }
        let find = |spaces: &mut Vec<Space>, name: &str| -> Option<usize> {
            spaces.iter().position(|space| space.name == name)
        };
        for (name, listed) in projects {
            if let (Some(at), Ok(listed)) = (find(&mut spaces, &name), listed) {
                spaces[at].projects = listed;
            }
        }
        for (name, listed) in agents {
            if let (Some(at), Ok(listed)) = (find(&mut spaces, &name), listed) {
                spaces[at].agents = listed;
            }
        }
        for (name, listed) in clients {
            if let (Some(at), Ok(listed)) = (find(&mut spaces, &name), listed) {
                spaces[at].clients = listed;
            }
        }
        for SpaceListing { space, listed } in running {
            if let (Some(at), Ok(listed)) = (find(&mut spaces, &space), listed) {
                spaces[at].running = listed;
            }
        }
        for SpaceListing { space, listed } in machines {
            if let (Some(at), Ok(listed)) = (find(&mut spaces, &space), listed) {
                spaces[at].machines = listed.machines;
                spaces[at].accounts = listed.accounts;
            }
        }
        self.spaces = spaces;
        for listing in recent {
            for item in listing.listed.into_iter().flatten() {
                self.deliver(item);
            }
        }

        let paired: HashSet<&str> = self
            .spaces
            .iter()
            .map(|space| space.name.as_str())
            .collect();
        self.queue
            .retain(|queued| paired.contains(queued.space.as_str()));
        for listed in decisions {
            let Ok(pending) = listed.decisions else {
                continue;
            };
            self.queue.retain(|queued| queued.space != listed.space);
            self.queue
                .extend(pending.into_iter().map(|decision| Queued {
                    space: listed.space.clone(),
                    decision,
                }));
        }
        self.queue.sort_by(|a, b| {
            (a.decision.created_at, &a.space, a.decision.id).cmp(&(
                b.decision.created_at,
                &b.space,
                b.decision.id,
            ))
        });
    }

    /// Takes what the project screen loaded. What could not be listed keeps
    /// what was last loaded.
    pub fn apply_project(&mut self, load: ProjectLoad) {
        let ProjectLoad {
            space,
            project,
            tasks,
            repositories,
            bindings,
            history,
        } = load;
        let view = self
            .projects
            .entry((space.clone(), project.clone()))
            .or_default();
        if let Ok(tasks) = tasks {
            view.tasks = tasks;
        }
        if let Ok(repositories) = repositories {
            view.repositories = repositories;
        }
        if let Ok(bindings) = bindings {
            view.bindings = bindings;
        }
        if let Ok(entries) = history {
            self.take_history(&space, &project, entries, true);
        }
    }

    /// Takes an earlier page of a conversation.
    pub fn apply_earlier(&mut self, load: EarlierLoad) {
        if let Ok(entries) = load.history {
            self.take_history(&load.space, &load.project, entries, false);
        }
    }

    /// Takes a page of history. The newest page leaves how far back is
    /// loaded alone once an earlier page was taken.
    fn take_history(
        &mut self,
        space: &str,
        project: &str,
        entries: Vec<ConversationEntry>,
        newest: bool,
    ) {
        let full = entries.len() >= usize::try_from(PAGE).unwrap_or(usize::MAX);
        let mut oldest = None;
        for entry in entries {
            let at = match entry {
                ConversationEntry::You { event_id, text, at } => {
                    self.said.insert(
                        (space.to_owned(), event_id),
                        Said {
                            project: project.to_owned(),
                            text,
                            at,
                        },
                    );
                    at
                }
                ConversationEntry::Delivered { item } => {
                    let at = item.delivered_at;
                    self.deliver(SpaceFeedItem {
                        space: space.to_owned(),
                        item,
                    });
                    at
                }
                _ => continue,
            };
            oldest = Some(oldest.map_or(at, |known: Timestamp| known.min(at)));
        }
        let key = (space.to_owned(), project.to_owned());
        let page = Loaded { oldest, more: full };
        match self.loaded.get_mut(&key) {
            Some(known) if newest && known.oldest.is_some() && known.oldest <= oldest => {}
            Some(known) => *known = page,
            None => {
                self.loaded.insert(key, page);
            }
        }
    }

    /// Where an earlier page of the conversation starts, if there may be one.
    #[must_use]
    pub fn earlier(&self, space: &str, project: &str) -> Option<Timestamp> {
        let loaded = self.loaded.get(&(space.to_owned(), project.to_owned()))?;
        loaded.more.then_some(loaded.oldest).flatten()
    }

    /// Takes task `task` of `space` in full.
    pub fn apply_task(&mut self, space: &str, task: TaskDetail) {
        self.tasks
            .insert((space.to_owned(), task.summary.task.id), task);
    }

    /// Takes a mode its hub set: the project's binding shows it at once.
    pub fn mode_set(&mut self, space: &str, project: &str, binding: BindingSummary) {
        let view = self
            .projects
            .entry((space.to_owned(), project.to_owned()))
            .or_default();
        match view
            .bindings
            .iter_mut()
            .find(|known| known.agent == binding.agent)
        {
            Some(known) => *known = binding,
            None => view.bindings.push(binding),
        }
    }

    /// Takes a message its hub accepted, so the conversation shows it before
    /// the history does.
    pub fn sent(&mut self, space: &str, project: &str, text: &str, event: EventId, at: Timestamp) {
        self.said
            .entry((space.to_owned(), event))
            .or_insert_with(|| Said {
                project: project.to_owned(),
                text: text.to_owned(),
                at,
            });
        self.target = Some((space.to_owned(), project.to_owned()));
    }

    /// Takes an answer its hub accepted: the decision no longer waits, even
    /// when an earlier answer was the one that counted.
    pub fn answered(&mut self, answer: &SpaceAnswer) {
        self.queue.retain(|queued| {
            queued.space != answer.space || queued.decision.id != answer.decision.id
        });
        for ((space, _), detail) in &mut self.tasks {
            if *space != answer.space {
                continue;
            }
            for decision in &mut detail.decisions {
                if decision.id == answer.decision.id {
                    *decision = answer.decision.clone();
                }
            }
        }
    }

    /// Takes one item from the feed. Returns what it should raise a system
    /// notification for: a decision or a system notice seen for the first
    /// time. An ordinary message, or an item already taken (a feed replayed
    /// after a reconnect), raises none, and a repeat is not shown again.
    pub fn receive(&mut self, item: SpaceFeedItem) -> Option<Alert> {
        if !self.notified.insert((item.space.clone(), item.item.seq)) {
            return None;
        }
        let alert = match item.kind() {
            OutboxKind::Message => None,
            OutboxKind::Decision => Card::of(&item).map(|card| Alert::Decision {
                space: item.space.clone(),
                label: item.label(),
                card,
            }),
            OutboxKind::SystemNotice => Some(Alert::Notice {
                label: item.label(),
                text: item.text(),
                seq: item.item.seq,
            }),
        };
        self.deliver(item);
        alert
    }

    fn deliver(&mut self, item: SpaceFeedItem) {
        self.delivered
            .insert((item.space.clone(), item.item.seq), item);
    }
}

/// Moves feed items from `stream` to `shown` until either side closes, and
/// saves the feed position of what was handed over (Plan.md §11.2 "feed
/// stream"). Errors are dropped: a refresh reports which hubs are away.
pub async fn pump(
    core: Arc<RwLock<ClientCore>>,
    mut stream: FeedStream,
    shown: mpsc::UnboundedSender<SpaceFeedItem>,
) {
    // The newest item per space not yet saved, while a request holds the core.
    let mut unsaved: Vec<SpaceFeedItem> = Vec::new();
    while let Some(result) = stream.recv().await {
        let Ok(item) = result else { continue };
        unsaved.retain(|other| other.space != item.space);
        unsaved.push(item.clone());
        if shown.send(item).is_err() {
            return;
        }
        // Never wait for the core here: a slow hub call would hold up the feed.
        if let Ok(mut core) = core.try_write() {
            unsaved.retain(|item| core.acknowledge(item).is_err());
        }
    }
}

#[cfg(test)]
mod tests;
