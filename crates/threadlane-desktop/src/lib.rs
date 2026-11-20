//! The desktop app's state (Plan.md §11.1): what the shell, Home, a project,
//! a task, and the machines screen show, and how hub replies and feed items
//! change it. Plain Rust with no user interface in it; the GPUI views in
//! `main.rs` and `screens/` only draw it. Every hub call goes through
//! [`ClientCore`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::{StreamExt as _, stream};
use serde_json::Value as Json;
use threadlane_client::{
    ClientCore, ClientError, FeedStream, HubStatus, IssuedCode, SpaceAnswer, SpaceDecisions,
    SpaceFeedItem, SpaceListing, SpaceMachines,
};
use threadlane_core::admission::AdmittedKey;
use threadlane_core::client::{
    BindingSummary, ConversationEntry, HeldResults, HubInfo, LaneTarget, LeadRun, ProjectSummary,
    RecordedEvent, StandingRuleSummary, TaskDetail, TaskSummary,
};
use threadlane_core::ids::{DecisionId, EventId, ProjectId, RunId, TaskId};
use threadlane_core::model::{
    Account, Agent, Decision, DecisionKind, Machine, MachineStatus, Mode, OutboxKind, Pacing,
    TaskStatus,
};
use threadlane_core::time::{Clock as _, SystemClock, Timestamp};
use tokio::sync::{RwLock, mpsc};

mod agent_edit;
use threadlane_core::attachment::ImageAttachment;

mod attention;
mod clock;
mod composer;
mod control;
mod conversation;
mod conversation_cache;
mod decision_keys;
mod decision_record;
mod dreaming;
mod edges;
mod graph;
mod machines;
mod memory;
mod permissions;
mod prose;
mod routines;
mod settings;
mod tasks;
mod timeline;

pub use agent_edit::{AgentBinding, AgentEdit, AgentFields, backend_summary};
pub use clock::{When, duration, format_count, parse_iso_utc};
pub use composer::{Destination, Draft, Outgoing, SEND_HOLD_SECONDS, SendRequest, sends_on_enter};
pub use control::{
    AgentDraft, AgentForm, BACKENDS, Publication, Recorded, RecordedRow, TaskControl, TaskForm,
    add_repository, can_confirm_deletion, copy_agent, same_remote,
};
pub use conversation::{
    Digest, Line, belongs_in_conversation, follow_latest, message_limit, newest_message,
};
pub use conversation_cache::ConversationState;
pub use decision_keys::{
    DecisionChoice, DecisionCommand, DecisionEffect, DecisionKey, DecisionScope,
};
pub use decision_record::{
    DecisionDisplay, DecisionLink, DecisionRecord, load_decision_link, question_parts,
};
pub use dreaming::{DreamingAction, DreamingCard, DreamingLoad, change_dreaming, load_dreaming};
pub use edges::{
    Confirmation, Connection, ConnectionStatus, Consequence, EmptyRegion, StateSentence,
};
pub use graph::{
    DAG_NODE_H, DAG_NODE_W, DagNode, EdgeState, GraphEdge, ProjectTasksViewMode,
    SpaceTasksViewMode, TOPO_NODE_H, TOPO_NODE_W, TaskDag, TeamViewMode, TopologyGraph,
    TopologyNode, TopologyNodeKind, build_task_dag,
};
pub use machines::{
    MachineDetail, MachineHistory, MachineRow, MachineRun, load_machine_history, machine_row,
};
pub use memory::{
    LeadKnows, MemoryChange, MemoryGroup, MemoryLoad, MemoryRow, MemoryVersionRow, change_memory,
    editable, load_memory,
};
pub use permissions::{
    AgentRow, CeilingCard, CellChoice, MODES, MayDo, ModeChoice, RuleRow, Statement, allows,
    applied, cell_label, matrix, mode_label,
};
pub use prose::{
    ProseLimit, Reference, Segment, github_pull_url, github_repository, markdown_source,
    markdown_source_for, markdown_source_with_time, segments,
};
pub use routines::{
    Cadence, RoutineForm, RoutineGroup, RoutineRow, RoutineText, RoutinesLoad, load_routines,
};
pub use settings::{
    Appearance, Notifications, PACING_FIELDS, PacingField, SETTINGS_FILE, SpaceRow, load_metrics,
    parse_pacing,
};
pub use tasks::{
    BoardCard, Changes, Column, InProgress, LatestStep, Moment, SpaceTaskFilter, SpaceTasksView,
    TaskGroup, TaskHeader, TaskTriageRow, TaskTriageSection, WorkRow, activity_runs, changes,
    chips, merge_state, running_elapsed, status_label, task_header, timeline, usage, work_elapsed,
    work_started,
};
pub use timeline::{
    Spoken, TIMELINE_PAGE, TimelineEntry, TimelineLoad, is_quiet_update, load_timeline,
};

/// How many of the newest delivered items Home asks each hub for.
const RECENT: u32 = 50;

/// How many conversation entries one page holds.
pub const PAGE: u32 = threadlane_core::client::CONVERSATION_PAGE;

/// How many results Home lists.
const RECENT_SHOWN: usize = 8;

/// The key prefix of the hub's account notices, which end in `:blocked`
/// or `:resumed` (`threadlane-store`, Plan.md §5.3).
const ACCOUNT_NOTICE: &str = "system:account:";

/// Selects the singular wording for one item and the plural wording otherwise.
#[must_use]
pub fn count_form<'a, T: From<u8> + PartialEq>(count: T, one: &'a str, many: &'a str) -> &'a str {
    if count == T::from(1) { one } else { many }
}

/// An error with its causes, as the app shows it: `hub work refused the
/// request: not found: task T-0001`.
#[must_use]
pub fn describe(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(next) = cause {
        text = format!("{text}: {next}");
        cause = next.source();
    }
    text
}

/// One part of what the app asks a hub, which can fail on its own while
/// the other parts load (Plan.md §11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Part {
    /// A space's projects.
    Projects,
    /// A space's agents.
    Agents,
    /// The client keys a space admitted.
    Clients,
    /// The decisions waiting on you in a space.
    Decisions,
    /// A space's running tasks.
    Running,
    /// A space's machines and model accounts.
    Machines,
    /// A space's newest delivered items.
    Recent,
    /// A project's tasks.
    Tasks,
    /// A project's repositories.
    Repositories,
    /// The agents bound in a project.
    Bindings,
    /// A project's conversation history.
    Conversation,
    /// Whether a project's lead is working.
    Lead,
    /// The memory a project's lead can see.
    Memory,
    /// The selected memory file’s versions.
    MemoryVersions,
    /// A project's routines and follow-ups.
    Routines,
    /// A project's standing rules.
    Rules,
}

/// How a part last answered; a part not in the map has not answered yet.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    Answered,
    Failed(String),
}

/// What a screen shows in place of a part it has nothing current for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gap {
    /// The hub has not answered yet.
    Loading,
    /// Why it could not be listed: the hub's message.
    Failed(String),
}

/// The gap of `part` in `answers`, `Loading` if it never answered.
fn gap_in(answers: &BTreeMap<Part, Answer>, part: Part) -> Option<Gap> {
    match answers.get(&part) {
        None => Some(Gap::Loading),
        Some(Answer::Answered) => None,
        Some(Answer::Failed(why)) => Some(Gap::Failed(why.clone())),
    }
}

/// Records how `part` answered and returns what to show for it: the new
/// listing, or nothing current (`T::default()`) when the hub refused, so a
/// stale listing is never shown as current.
fn take<T: Default>(
    answers: &mut BTreeMap<Part, Answer>,
    part: Part,
    listed: Result<T, ClientError>,
) -> T {
    match listed {
        Ok(listed) => {
            answers.insert(part, Answer::Answered);
            listed
        }
        Err(error) => {
            answers.insert(part, Answer::Failed(describe(&error)));
            T::default()
        }
    }
}

/// Whether a paired hub answered the last refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Not asked yet: the app just started.
    Unknown,
    /// It answered.
    Reachable,
    /// Why it could not be reached.
    Unreachable(String),
}

/// One paired hub as the app shows it. While the hub is away it keeps its
/// last report (Plan.md §11.3); a part the hub refused shows its error.
#[derive(Debug, Clone, PartialEq)]
pub struct Space {
    /// The name the hub was paired under.
    pub name: String,
    /// Whether the hub answered the last refresh.
    pub reach: Reach,
    unreachable_since: Option<Timestamp>,
    answers: BTreeMap<Part, Answer>,
    /// Its projects.
    pub projects: Vec<ProjectSummary>,
    /// Its running tasks, oldest first.
    pub running: Vec<TaskSummary>,
    leads: Vec<(String, Timestamp)>,
    /// The machines that joined it.
    pub machines: Vec<Machine>,
    /// Its model accounts.
    pub accounts: Vec<Account>,
    /// The client keys it admitted.
    pub clients: Vec<AdmittedKey>,
    /// Its agents.
    pub agents: Vec<Agent>,
    /// What its hub last said about itself.
    pub info: Option<HubInfo>,
    /// How this app reaches it: `Unix socket …` or `Network, hub key …`.
    pub reached_by: String,
    local_socket: Option<PathBuf>,
    /// How many events it recorded and did not act on; `None` while its
    /// hub has not said, or cannot (one from before it could).
    pub recorded: Option<u64>,
    held: Option<HeldResults>,
    /// The tasks of its active projects.
    pub tasks: Vec<TaskSummary>,
    archived_open: bool,
}

impl Space {
    /// The project that stores this space's Main Chat.
    #[must_use]
    pub fn main_chat_project(&self) -> Option<&str> {
        self.active_projects()
            .find(|project| project.is_general)
            .map(|project| project.name.as_str())
    }

    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            reach: Reach::Unknown,
            unreachable_since: None,
            answers: BTreeMap::new(),
            projects: Vec::new(),
            running: Vec::new(),
            leads: Vec::new(),
            machines: Vec::new(),
            accounts: Vec::new(),
            clients: Vec::new(),
            agents: Vec::new(),
            info: None,
            reached_by: String::new(),
            local_socket: None,
            recorded: None,
            held: None,
            tasks: Vec::new(),
            archived_open: false,
        }
    }

    /// Projects shown in ordinary navigation and work pickers.
    pub fn active_projects(&self) -> impl Iterator<Item = &ProjectSummary> {
        self.projects.iter().filter(|project| !project.archived)
    }

    /// Projects available under the Archived group.
    pub fn archived_projects(&self) -> impl Iterator<Item = &ProjectSummary> {
        self.projects.iter().filter(|project| project.archived)
    }

    /// Whether this space's archived projects are revealed in the sidebar.
    #[must_use]
    pub fn archived_open(&self) -> bool {
        self.archived_open
    }

    /// Its machines that are online.
    #[must_use]
    pub fn online(&self) -> usize {
        self.machines
            .iter()
            .filter(|machine| machine.status == MachineStatus::Online)
            .count()
    }

    /// What to show in place of `part`, if anything: `Loading` before the
    /// hub first answered for it, or why it could not. A hub out of reach
    /// keeps what it last reported, so only a part it never listed is a gap.
    #[must_use]
    pub fn gap(&self, part: Part) -> Option<Gap> {
        match (gap_in(&self.answers, part), &self.reach) {
            (Some(Gap::Loading), Reach::Unreachable(why)) => Some(Gap::Failed(why.clone())),
            (gap, _) => gap,
        }
    }

    /// Whether anything shown of it may be out of date: it is not known to
    /// be reachable, or a part failed. Such a space is asked again soon.
    #[must_use]
    pub fn stale(&self) -> bool {
        self.reach != Reach::Reachable
            || self
                .answers
                .values()
                .any(|answer| matches!(answer, Answer::Failed(_)))
    }

    /// Takes one part of a refresh. Out of reach, the last report stays.
    fn take<T: Default>(&mut self, part: Part, listed: Result<T, ClientError>) -> Option<T> {
        if listed.is_err() && matches!(self.reach, Reach::Unreachable(_)) {
            return None;
        }
        Some(take(&mut self.answers, part, listed))
    }
}

/// `(online, all)` machines of `spaces`, or `None` when one of them has no
/// current count: out of reach, not answered yet, or refused.
fn machine_count<'a>(spaces: impl Iterator<Item = &'a Space>) -> Option<(usize, usize)> {
    let mut count = (0, 0);
    for space in spaces {
        if space.reach != Reach::Reachable || space.gap(Part::Machines).is_some() {
            return None;
        }
        count = (count.0 + space.online(), count.1 + space.machines.len());
    }
    Some(count)
}

/// How many characters of an argument a decision card shows.
const ARGUMENT_CHARS: usize = 160;

/// The arguments a decision card names first, and how it labels them; the
/// rest follow by name.
const ARGUMENTS: [(&str, &str); 9] = [
    ("repository", "Repository"),
    ("title", "Title"),
    ("path", "Path"),
    ("from", "From"),
    ("to", "To"),
    ("content", "Content"),
    ("old_text", "Replace"),
    ("new_text", "With"),
    ("body", "Body"),
];

/// The first line of a decision's context, which says why it is asked.
#[must_use]
pub fn context_line(decision: &Decision) -> Option<&str> {
    decision
        .context
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
}

/// The tool's arguments as a decision card shows them, one `Label: value`
/// per line, each value on one line and cut short: for `open_pull_request`
/// the repository and title, for a memory change the path and an excerpt.
#[must_use]
pub fn arguments(decision: &Decision) -> Vec<String> {
    let Some(object) = decision.arguments.as_object() else {
        return Vec::new();
    };
    let shown = |value: &Json| -> Option<String> {
        let text = match value {
            Json::String(text) => text.clone(),
            Json::Number(_) | Json::Bool(_) => value.to_string(),
            _ => return None,
        };
        let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        Some(match line.char_indices().nth(ARGUMENT_CHARS) {
            Some((at, _)) => format!("{}…", line[..at].trim_end()),
            None => line,
        })
    };
    let known = ARGUMENTS
        .iter()
        .filter_map(|(name, label)| Some(format!("{label}: {}", shown(object.get(*name)?)?)));
    let mut rest: Vec<(&String, &Json)> = object
        .iter()
        .filter(|(name, _)| !ARGUMENTS.iter().any(|(known, _)| known == name))
        .collect();
    rest.sort_by(|a, b| a.0.cmp(b.0));
    let rest = rest.into_iter().filter_map(|(name, value)| {
        let mut label = name.replace('_', " ");
        if let Some(first) = label.get(..1) {
            label = format!("{}{}", first.to_uppercase(), &label[1..]);
        }
        Some(format!("{label}: {}", shown(value)?))
    });
    known.chain(rest).collect()
}

/// The last action you took and where it stands, which the status area
/// shows on every screen until the next one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    /// Under way: `Sending to work / api…`.
    Working(String),
    /// Done: `Sent to the lead of work / api.`.
    Done(String),
    /// Failed, with the hub's reason.
    Failed(String),
}

impl Activity {
    /// What the status area says.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Working(text) | Self::Done(text) | Self::Failed(text) => text,
        }
    }
}

/// The tabs of the project screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// The board.
    Tasks,
    /// The conversation with the lead.
    Conversation,
    /// The memory its runs can see.
    Memory,
    /// Its agents and their permissions.
    Team,
    /// Its routines and follow-ups.
    Routines,
    /// Its repositories and machines.
    Setup,
}

impl Tab {
    /// Every tab, in the order the tab bar shows them: Chat first.
    pub const ALL: [Self; 6] = [
        Self::Conversation,
        Self::Tasks,
        Self::Team,
        Self::Routines,
        Self::Memory,
        Self::Setup,
    ];

    /// Its label on the tab bar.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Tasks => "Tasks",
            Self::Conversation => "Chat",
            Self::Memory => "Memory",
            Self::Team => "Team",
            Self::Routines => "Routines",
            Self::Setup => "Setup",
        }
    }

    /// What it shows, as the tooltip on its icon-only tab.
    #[must_use]
    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Tasks => "Tasks: the board",
            Self::Conversation => "Chat with the lead",
            Self::Memory => "Memory: notes and progress files",
            Self::Team => "Team: agents and permissions",
            Self::Routines => "Routines and follow-ups",
            Self::Setup => "Setup: repositories and machines",
        }
    }

    /// Its place on the tab bar, which selects it there: always found
    /// from [`Tab::ALL`], so a tab added anywhere keeps the two in step.
    #[must_use]
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    /// The tab at `index` on the tab bar.
    #[must_use]
    pub fn at(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    /// The tab named `name` in lower case, as `THREADLANE_DESKTOP_OPEN` says.
    /// `agents` remains an alias for Team.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("agents") {
            return Some(Self::Team);
        }
        Self::ALL
            .into_iter()
            .find(|tab| tab.label().eq_ignore_ascii_case(name))
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

/// What an open decision card shows on Home, in conversation, and in a task.
#[derive(Debug, PartialEq, Eq)]
pub struct DecisionCard {
    /// Proposal or question chip, when the decision has a distinct kind.
    pub chip: Option<&'static str>,
    /// Whether the card has the proposal border.
    pub dashed: bool,
    /// Why the decision appeared, when known.
    pub context: Option<String>,
    /// The details of the stored action.
    pub details: Vec<String>,
    /// The proposed assignee and repositories, shown below the options.
    pub would_run_as: Option<String>,
    /// Label for the note field shown above the options.
    pub note_label: &'static str,
    /// The recommended option's index, if the hub picked one.
    pub recommended: Option<usize>,
    /// Optional bounded diff preview attached to a publish approval decision.
    pub diff: Option<threadlane_core::workspace::DiffPreview>,
}

fn decision_label(kind: DecisionKind) -> &'static str {
    match kind {
        DecisionKind::Approval => "Decision",
        DecisionKind::Input => "Question",
        DecisionKind::Proposal => "Proposal",
    }
}

/// Who would run the proposed task if you start it.
fn proposed_assignee(decision: &Decision) -> &str {
    decision.arguments["agent"]
        .as_str()
        .or(decision.proposed_by.as_deref())
        .unwrap_or(if decision.task_id.is_some() {
            "the task’s agent"
        } else {
            "Lead"
        })
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
    /// The decision's kind, on hubs that include it in the feed.
    pub kind: Option<DecisionKind>,
    /// First line from the feed's context, when present.
    pub context: Option<String>,
    /// The recommended option in order, when present.
    pub recommended: Option<usize>,
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
        let options: Vec<String> = body
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
            recommended: body["recommended"]
                .as_str()
                .and_then(|label| options.iter().position(|option| option == label)),
            options,
            context: body["context"]
                .as_str()
                .and_then(|text| text.lines().next())
                .filter(|line| !line.trim().is_empty())
                .map(str::to_owned),
            kind: body
                .get("kind")
                .and_then(Json::as_str)
                .and_then(|kind| kind.parse().ok()),
        })
    }

    /// How to name the card in the conversation or a notification.
    #[must_use]
    pub fn label(&self) -> &'static str {
        decision_label(self.kind.unwrap_or(DecisionKind::Approval))
    }
}

/// How long a decision waits before its one reminder notification.
// shortcut: fixed 10 minutes, make it a notification setting if people ask.
pub const REMIND_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

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
        /// Whether this is the one reminder for a decision still waiting.
        reminder: bool,
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
            Self::Decision {
                label,
                card,
                reminder,
                ..
            } => {
                let still = if *reminder { "Still waiting: " } else { "" };
                format!(
                    "{still}{label}: {} {}",
                    card.label().to_ascii_lowercase(),
                    card.id
                )
            }
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

/// What one refresh pass learned from one or more paired hubs.
#[derive(Debug, Default)]
pub struct Refresh {
    statuses: Vec<HubStatus>,
    projects: Vec<(String, Result<Vec<ProjectSummary>, ClientError>)>,
    agents: Vec<(String, Result<Vec<Agent>, ClientError>)>,
    clients: Vec<(String, Result<Vec<AdmittedKey>, ClientError>)>,
    decisions: Vec<SpaceDecisions>,
    running: Vec<SpaceListing<Vec<TaskSummary>>>,
    leads: Vec<(String, threadlane_core::client::ProjectLeadStatus)>,
    machines: Vec<SpaceListing<SpaceMachines>>,
    recent: Vec<SpaceListing<Vec<SpaceFeedItem>>>,
    recorded: Vec<SpaceListing<u64>>,
    held: Vec<SpaceListing<Option<HeldResults>>>,
    tasks: Vec<(String, Result<Vec<TaskSummary>, ClientError>)>,
}

impl Refresh {
    /// The paired name of a single-hub result from [`refresh_each`].
    #[must_use]
    pub fn refreshed_space(&self) -> Option<&str> {
        match self.statuses.as_slice() {
            [status] => Some(&status.name),
            _ => None,
        }
    }

    fn append(&mut self, other: Self) {
        self.statuses.extend(other.statuses);
        self.projects.extend(other.projects);
        self.agents.extend(other.agents);
        self.clients.extend(other.clients);
        self.decisions.extend(other.decisions);
        self.running.extend(other.running);
        self.leads.extend(other.leads);
        self.machines.extend(other.machines);
        self.recent.extend(other.recent);
        self.recorded.extend(other.recorded);
        self.held.extend(other.held);
        self.tasks.extend(other.tasks);
    }

    /// Whether this refresh supplied tasks and lead status for the named project.
    pub fn project_parts(&self, space: &str, project: &str) -> (bool, bool) {
        let active = self.projects.iter().any(|(name, listed)| {
            name == space
                && listed.as_ref().is_ok_and(|projects| {
                    projects
                        .iter()
                        .any(|row| row.name == project && !row.archived)
                })
        });
        let tasks = active
            && self
                .tasks
                .iter()
                .any(|(name, listed)| name == space && listed.is_ok());
        let lead = active
            && self
                .leads
                .iter()
                .any(|(name, lead)| name == space && lead.project == project);
        (tasks, lead)
    }
}

/// Asks every paired hub about itself, its projects, agents, admitted
/// clients, pending decisions, running tasks, machines, and newest items. A
/// hub that does not answer is reported as such; the others are unaffected.
pub async fn refresh(core: &ClientCore) -> Refresh {
    let mut parts = Vec::with_capacity(core.hubs().len());
    refresh_each(core, |index, part| {
        parts.push((index, part));
        true
    })
    .await;
    // The full snapshot API preserves the stable pairing order.
    parts.sort_by_key(|(index, _)| *index);
    let mut refreshed = Refresh::default();
    for (_, part) in parts {
        refreshed.append(part);
    }
    refreshed
}

const HUB_REFRESH_LIMIT: usize = 4;

/// Refreshes at most four paired hubs at a time, yielding each complete hub
/// result immediately rather than waiting for slow peers. Returning false from
/// the callback stops the pass (such as when the desktop has closed).
pub async fn refresh_each(core: &ClientCore, mut ready: impl FnMut(usize, Refresh) -> bool) {
    // Own the pairing values so the buffered futures are Send when the
    // desktop runs the refresh on Tokio.
    let hubs = core.hubs().to_vec();
    let mut parts = stream::iter(hubs.into_iter().enumerate())
        .map(|(index, hub)| async move { (index, refresh_hub(core, &hub).await) })
        .buffer_unordered(HUB_REFRESH_LIMIT);
    while let Some((index, part)) = parts.next().await {
        if !ready(index, part) {
            break;
        }
    }
}

async fn refresh_hub(core: &ClientCore, hub: &threadlane_client::PairedHub) -> Refresh {
    let name = hub.name.as_str();
    let (mut part, decisions, running, machines, recent, recorded, held) = tokio::join!(
        refresh_work(core, hub),
        core.decisions(Some(name)),
        core.running(Some(name)),
        core.machines(Some(name)),
        core.recent(Some(name), RECENT),
        core.recorded_count(Some(name)),
        core.held_results(Some(name)),
    );
    part.decisions = decisions.unwrap_or_default();
    part.running = running.unwrap_or_default();
    part.machines = machines.unwrap_or_default();
    part.recent = recent.unwrap_or_default();
    part.recorded = recorded.unwrap_or_default();
    part.held = held.unwrap_or_default();
    part
}

async fn refresh_work(core: &ClientCore, hub: &threadlane_client::PairedHub) -> Refresh {
    let name = hub.name.as_str();
    let status = core.status_for(hub).await;
    let reachable = status.is_reachable();
    let mut part = Refresh {
        statuses: vec![status],
        ..Refresh::default()
    };
    if reachable {
        let (work, space_agents, space_clients) = tokio::join!(
            core.work(name),
            core.agents(Some(name)),
            core.clients(Some(name))
        );
        match work {
            Ok(work) => {
                part.projects.push((name.to_owned(), Ok(work.projects)));
                part.tasks.push((name.to_owned(), Ok(work.tasks)));
                part.leads
                    .extend(work.leads.into_iter().map(|lead| (name.to_owned(), lead)));
            }
            Err(error) => {
                let listed = core
                    .project_list(Some(name), true)
                    .await
                    .map(|listed| listed.projects);
                if threadlane_client::unknown_overview(&error, "work") {
                    if let Ok(listed) = &listed {
                        part.tasks
                            .push((name.to_owned(), all_tasks(core, name, listed).await));
                        for project in listed.iter().filter(|project| !project.archived) {
                            if let Ok((running_since, stopped)) =
                                core.lead_status(Some(name), Some(&project.name)).await
                            {
                                part.leads.push((
                                    name.to_owned(),
                                    threadlane_core::client::ProjectLeadStatus {
                                        project: project.name.clone(),
                                        running_since,
                                        stopped,
                                    },
                                ));
                            }
                        }
                    }
                } else {
                    part.tasks.push((name.to_owned(), Err(error)));
                }
                part.projects.push((name.to_owned(), listed));
            }
        }
        part.agents.push((name.to_owned(), space_agents));
        part.clients.push((name.to_owned(), space_clients));
    }
    part
}

/// The tasks of every project in `projects` of `space`, each once; the
/// first refusal if one project's could not be listed.
async fn all_tasks(
    core: &ClientCore,
    space: &str,
    projects: &[ProjectSummary],
) -> Result<Vec<TaskSummary>, ClientError> {
    let mut all: Vec<TaskSummary> = Vec::new();
    let mut seen = HashSet::new();
    for project in projects.iter().filter(|project| !project.archived) {
        for summary in core.tasks(Some(space), Some(&project.name)).await? {
            if seen.insert(summary.task.id) {
                all.push(summary);
            }
        }
    }
    Ok(all)
}

/// What the project screen loaded for one project. Made by
/// [`load_project`], applied with [`AppState::apply_project`].
#[derive(Debug)]
pub struct ProjectLoad {
    space: String,
    project: String,
    tasks: Option<Result<Vec<TaskSummary>, ClientError>>,
    repositories: Option<Result<Vec<threadlane_core::model::Repository>, ClientError>>,
    bindings: Option<Result<Vec<BindingSummary>, ClientError>>,
    history: Option<Result<Vec<ConversationEntry>, ClientError>>,
    decision_records: Vec<Decision>,
    lead: Option<Result<(Option<Timestamp>, bool), ClientError>>,
    recorded: Option<Result<(Vec<RecordedEvent>, u64), ClientError>>,
    rules: Option<Result<Vec<StandingRuleSummary>, ClientError>>,
    lead_runs: Option<Result<Vec<LeadRun>, ClientError>>,
}

impl ProjectLoad {
    /// Keeps work already applied by a refresh that arrived during this load.
    pub fn keep_refreshed_parts(&mut self, (tasks, lead): (bool, bool)) {
        if tasks {
            self.tasks = None;
        }
        if lead {
            self.lead = None;
        }
    }
}

/// Asks a project's hub for its tasks, repositories, bindings, the newest
/// page of its conversation, whether its lead is working, and the events
/// it recorded and did not act on.
pub async fn load_project(core: &ClientCore, space: &str, project: &str) -> ProjectLoad {
    load_project_tab(core, space, project, None, false, false).await
}

/// Fetches the parts used by the current tab. A successful space refresh already supplied work.
pub async fn load_project_tab(
    core: &ClientCore,
    space: &str,
    project: &str,
    tab: Option<Tab>,
    fresh_tasks: bool,
    fresh_lead: bool,
) -> ProjectLoad {
    let (in_space, named) = (Some(space), Some(project));
    let chat = tab.is_none_or(|tab| tab == Tab::Conversation);
    let team = tab.is_none_or(|tab| tab == Tab::Team);
    let (tasks, repositories, bindings, history, lead, recorded, rules, lead_runs) = tokio::join!(
        async {
            if !fresh_tasks {
                Some(core.tasks(in_space, named).await)
            } else {
                None
            }
        },
        async { Some(core.repositories(in_space, named).await) },
        async { Some(core.bindings(in_space, named).await) },
        async {
            if chat {
                Some(core.conversation(in_space, named, None, PAGE).await)
            } else {
                None
            }
        },
        async {
            if fresh_lead {
                None
            } else {
                Some(core.lead_status(in_space, named).await)
            }
        },
        async {
            if chat {
                Some(
                    core.recorded(in_space, named, control::RECORDED_SHOWN)
                        .await,
                )
            } else {
                None
            }
        },
        async {
            if team {
                Some(core.standing_rules(in_space, named).await)
            } else {
                None
            }
        },
        async {
            if chat {
                Some(core.lead_runs(in_space, named).await)
            } else {
                None
            }
        },
    );
    let decision_records = match &history {
        Some(Ok(history)) => decision_record::load_records(core, space, history).await,
        _ => Vec::new(),
    };
    ProjectLoad {
        space: space.to_owned(),
        project: project.to_owned(),
        tasks,
        repositories,
        bindings,
        history,
        decision_records,
        lead,
        recorded,
        rules,
        lead_runs,
    }
}

/// The bindings of one project, for the modes grid. Made by
/// [`load_modes`], applied with [`AppState::apply_modes`].
#[derive(Debug)]
pub struct BindingsLoad {
    space: String,
    project: String,
    bindings: Result<Vec<BindingSummary>, ClientError>,
}

/// Asks for the bindings of each `(space, project)`.
pub async fn load_modes(core: &ClientCore, projects: Vec<(String, String)>) -> Vec<BindingsLoad> {
    let mut loads = Vec::with_capacity(projects.len());
    for (space, project) in projects {
        let bindings = core.bindings(Some(&space), Some(&project)).await;
        loads.push(BindingsLoad {
            space,
            project,
            bindings,
        });
    }
    loads
}

/// One task in full, or why its hub could not show it. Made by
/// [`load_task`], applied with [`AppState::apply_task`].
#[derive(Debug)]
pub struct TaskLoad {
    space: String,
    task: TaskId,
    detail: Result<TaskDetail, ClientError>,
}

/// Asks the hub of `space` for task `task` in full.
pub async fn load_task(core: &ClientCore, space: &str, task: TaskId) -> TaskLoad {
    TaskLoad {
        space: space.to_owned(),
        task,
        detail: core.task(Some(space), &task.to_string()).await,
    }
}

/// Asks the hub for the task screen's initial details.
pub async fn load_task_overview(core: &ClientCore, space: &str, task: TaskId) -> TaskLoad {
    TaskLoad {
        space: space.to_owned(),
        task,
        detail: core.task_overview(space, task).await,
    }
}

/// The modes grid of one space (wireframe 6 "Modes by project").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModesGrid {
    /// The space's agents: the grid's columns.
    pub agents: Vec<String>,
    /// One row per project.
    pub rows: Vec<ModesRow>,
}

/// One project's row of the modes grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModesRow {
    /// The project's name.
    pub project: String,
    /// Each agent's mode here, in the grid's column order, `None` where it
    /// is not bound; or what to show instead while they are not known.
    pub modes: Result<Vec<Option<Mode>>, Gap>,
}

/// Status of a request to load an earlier conversation page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EarlierStatus {
    Loading,
    Failed(String),
}

/// An earlier page of a conversation. Made by [`load_earlier`], applied
/// with [`AppState::apply_earlier`].
#[derive(Debug)]
pub struct EarlierLoad {
    space: String,
    project: String,
    before: Timestamp,
    history: Result<Vec<ConversationEntry>, ClientError>,
    decision_records: Vec<Decision>,
}

/// Asks for the page of a conversation before what is shown.
pub async fn load_earlier(
    core: &ClientCore,
    space: &str,
    project: &str,
    before: Timestamp,
) -> EarlierLoad {
    let history = core
        .conversation(Some(space), Some(project), Some(before), PAGE)
        .await;
    let decision_records =
        decision_record::load_records(core, space, history.as_deref().unwrap_or_default()).await;
    EarlierLoad {
        space: space.to_owned(),
        project: project.to_owned(),
        before,
        history,
        decision_records,
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
    /// When the run its lead is working on started; `None` while idle.
    pub lead: Option<Timestamp>,
    stopped: bool,
    answers: BTreeMap<Part, Answer>,
    /// The events it recorded and did not act on, and how many in all, or
    /// why its hub could not say. Kept out of `answers`: a hub from before
    /// it could say must not make the app ask again every few seconds.
    recorded: Option<Result<(Vec<RecordedEvent>, u64), String>>,
    /// Its standing rules, revoked ones included.
    pub rules: Vec<StandingRuleSummary>,
    lead_runs: Vec<LeadRun>,
}

/// Who wrote a message on the asking side of a lead's conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Author {
    /// You.
    You,
    /// The space lead, General's lead, handing this project a goal.
    SpaceLead,
    /// In General: the lead of this project, replying to the space lead.
    ProjectLead(String),
}

impl Author {
    /// How the conversation and Home name it.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::You => "You".to_owned(),
            Self::SpaceLead => "General lead".to_owned(),
            Self::ProjectLead(project) => format!("{project} lead"),
        }
    }
}

/// A message you sent, as the conversation shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Said {
    project: String,
    images: Vec<ImageAttachment>,
    text: String,
    at: Timestamp,
    by: Author,
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
    checking_connections: bool,
    machine_history: HashMap<String, Result<Vec<MachineRun>, String>>,
    queue: Vec<Queued>,
    decision_keys: decision_keys::DecisionKeys,
    decision_records: HashMap<DecisionKey, Decision>,
    decision_notes: HashSet<DecisionKey>,
    answering: HashSet<DecisionKey>,
    /// Every delivered item seen, from the feed or from history, by space
    /// and feed position, so one delivered both ways is shown once.
    delivered: BTreeMap<InSpace<i64>, SpaceFeedItem>,
    /// `(space, seq)` of every item taken from the feed, so a replay
    /// notifies once.
    notified: HashSet<InSpace<i64>>,
    /// Decisions already reminded about, so each gets one reminder.
    reminded: HashSet<DecisionKey>,
    new_home_replies: HashSet<String>,
    /// Your messages, by space and event id.
    said: BTreeMap<InSpace<EventId>, Said>,
    failures: BTreeMap<InSpace<EventId>, Said>,
    loaded: HashMap<InSpace<String>, Loaded>,
    earlier_requests: HashMap<InSpace<String>, (Timestamp, EarlierStatus)>,
    projects: HashMap<InSpace<String>, ProjectView>,
    tasks: HashMap<InSpace<TaskId>, TaskDetail>,
    /// Why a task could not be shown, by space and id.
    task_failures: HashMap<InSpace<TaskId>, String>,
    task_activity: HashSet<InSpace<TaskId>>,
    /// The Home results shown in full, by space and feed position.
    expanded: HashSet<InSpace<i64>>,
    /// The space Home shows; `None` for all.
    filter: Option<String>,
    /// Per-space filters on the space-wide Tasks triage screen.
    space_task_filters: HashMap<String, SpaceTaskFilter>,
    /// Spaces where the collapsed `Done` section on Tasks has been expanded.
    done_tasks_expanded: HashSet<String>,
    /// Diff preview blocks expanded by the user (keyed by UI element id).
    expanded_diffs: HashSet<String>,
    /// Active view mode on a project's `Tasks` tab (Board | List | Graph).
    project_tasks_view: ProjectTasksViewMode,
    /// Active view mode on the space-wide `Tasks` screen (List | Graph).
    space_tasks_view_mode: SpaceTasksViewMode,
    /// Active view mode on the `Team` screen (Overview | Topology).
    team_view_mode: TeamViewMode,
    /// Optional project scope on the Team Topology canvas, keyed by space name.
    topology_project: HashMap<String, String>,
    /// Currently selected `(space, agent)` on the Team Topology canvas for inline mode dialing.
    topology_selected_agent: Option<(String, String)>,
    /// Inbox disclosure toggles to keep Inbox calm and scannable.
    inbox_show_all_failures: bool,
    inbox_show_all_recent: bool,
    inbox_show_quiet_updates: bool,
    /// Where the Home composer sends: space and project.
    target_space: Option<String>,
    last_targets: BTreeMap<String, String>,
    /// A lead picked on a project Chat tab: the page, then who it sends to.
    chat_target: Option<((String, String), (String, String))>,
    pacing: HashMap<String, Result<Pacing, String>>,
    pacing_edit: Option<String>,
    pacing_saving: bool,
    stopping: Vec<InSpace<LaneTarget>>,
    codes: HashMap<String, IssuedCode>,
    /// Each project's memory, for the Memory tab.
    memory: HashMap<InSpace<String>, memory::MemoryView>,
    dreaming: HashMap<InSpace<String>, dreaming::DreamingView>,
    metrics: HashMap<String, Result<threadlane_core::client::OperationsMetrics, String>>,
    /// The last action and where it stands.
    activity: Option<Activity>,
    /// The space you asked to unpair, until you confirm or cancel.
    unpairing: Option<String>,
    /// Spaces unpaired since the app started: their feed is no longer shown.
    forgotten: HashSet<String>,
    /// Each project's routines and follow-ups.
    routines: HashMap<InSpace<String>, routines::RoutinesView>,
    /// The timer you asked to cancel, until you confirm or keep it.
    cancelling: Option<InSpace<threadlane_core::ids::TimerId>>,
    /// The picks of the "+ New routine" form.
    routine_form: RoutineForm,
    /// The picks of the "+ New agent" form.
    agent_form: AgentForm,
    /// The agent whose edit view is open.
    agent_edit: Option<AgentEdit>,
    agent_saves: HashSet<InSpace<String>>,
    /// The picks of the "+ New task" form.
    task_form: TaskForm,
    /// The project whose "+ Add repository" form is open.
    repository_form: Option<InSpace<String>>,
    /// The task you asked to cancel, until you confirm or keep it.
    cancelling_task: Option<InSpace<TaskId>>,
    /// The projects whose recorded section is expanded.
    recorded_open: HashSet<InSpace<String>>,
    /// Which system notifications the app raises.
    notifications: Notifications,
    appearance: Appearance,
    skip_send_hold: bool,
    /// The space the sidebar shows, as last chosen; saved in `desktop.json`.
    selected_space: Option<String>,
    composer: composer::Composer,
    conversation_read: HashMap<InSpace<String>, conversation::ConversationRead>,
    expanded_messages: HashSet<String>,
    /// Where this app keeps its files.
    client_dir: Option<PathBuf>,
    /// The agent row expanded on a project's Team tab: space, project,
    /// agent.
    open_agent: Option<(String, String, String)>,
    /// The standing rule you asked to revoke, until you confirm or keep it.
    revoking: Option<InSpace<threadlane_core::ids::StandingRuleId>>,
    /// The decisions whose "Always allow" is ticked.
    always: HashSet<InSpace<DecisionId>>,
    expanded_quiet: HashSet<InSpace<RunId>>,
    /// The ceiling you asked to set a space to, until you confirm or keep it.
    space_ceiling_asked: Option<InSpace<Mode>>,
    /// The pages of Home's timeline shown beyond the first.
    timeline_pages: usize,
}

impl AppState {
    /// The state before the first refresh: the hubs paired under `names`,
    /// known from the client's own files, their reachability not yet known.
    #[must_use]
    pub fn paired<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            spaces: names.into_iter().map(Space::new).collect(),
            ..Self::default()
        }
    }

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
        if let Some(space) = space {
            self.target_space = Some(space.to_owned());
        }
    }

    /// Active filters on the space-wide Tasks triage screen.
    #[must_use]
    pub fn space_task_filter(&self, space: &str) -> SpaceTaskFilter {
        self.space_task_filters
            .get(space)
            .cloned()
            .unwrap_or_default()
    }

    /// Filters the space-wide Tasks screen by status group (`None` = all).
    pub fn set_space_task_status_filter(&mut self, space: &str, status: Option<TaskGroup>) {
        self.space_task_filters
            .entry(space.to_owned())
            .or_default()
            .status = status;
    }

    /// Filters the space-wide Tasks screen by project (`None` = all).
    pub fn set_space_task_project_filter(&mut self, space: &str, project: Option<String>) {
        self.space_task_filters
            .entry(space.to_owned())
            .or_default()
            .project = project;
    }

    /// Filters the space-wide Tasks screen by agent (`None` = all).
    pub fn set_space_task_agent_filter(&mut self, space: &str, agent: Option<String>) {
        self.space_task_filters
            .entry(space.to_owned())
            .or_default()
            .agent = agent;
    }

    /// Clears all filters on the space-wide Tasks screen.
    pub fn clear_space_task_filter(&mut self, space: &str) {
        self.space_task_filters.remove(space);
    }

    /// Whether the `Done` section on the space-wide Tasks screen is expanded.
    #[must_use]
    pub fn is_done_tasks_expanded(&self, space: &str) -> bool {
        self.done_tasks_expanded.contains(space)
    }

    /// Toggles the `Done` section on the space-wide Tasks screen.
    pub fn toggle_done_tasks_expanded(&mut self, space: &str) {
        if !self.done_tasks_expanded.remove(space) {
            self.done_tasks_expanded.insert(space.to_owned());
        }
    }

    /// Whether a diff preview block is expanded.
    #[must_use]
    pub fn diff_expanded(&self, id: &str) -> bool {
        self.expanded_diffs.contains(id)
    }

    /// Toggles a diff preview block's expanded state.
    pub fn toggle_diff_expanded(&mut self, id: impl Into<String>) {
        let id = id.into();
        if !self.expanded_diffs.remove(&id) {
            self.expanded_diffs.insert(id);
        }
    }

    /// Current view mode for a project's `Tasks` tab.
    #[must_use]
    pub fn project_tasks_view(&self) -> ProjectTasksViewMode {
        self.project_tasks_view
    }

    /// Sets the view mode for a project's `Tasks` tab.
    pub fn set_project_tasks_view(&mut self, mode: ProjectTasksViewMode) {
        self.project_tasks_view = mode;
    }

    /// Current view mode for the space-wide `Tasks` screen.
    #[must_use]
    pub fn space_tasks_view_mode(&self) -> SpaceTasksViewMode {
        self.space_tasks_view_mode
    }

    /// Sets the view mode for the space-wide `Tasks` screen.
    pub fn set_space_tasks_view_mode(&mut self, mode: SpaceTasksViewMode) {
        self.space_tasks_view_mode = mode;
    }

    /// Current view mode for the `Team` screen.
    #[must_use]
    pub fn team_view_mode(&self) -> TeamViewMode {
        self.team_view_mode
    }

    /// Sets the view mode for the `Team` screen.
    pub fn set_team_view_mode(&mut self, mode: TeamViewMode) {
        self.team_view_mode = mode;
    }

    /// Selected project filter for `space`'s Team Topology canvas.
    #[must_use]
    pub fn topology_project(&self, space: &str) -> Option<&str> {
        self.topology_project.get(space).map(String::as_str)
    }

    /// Sets or clears the project filter for `space`'s Team Topology canvas.
    pub fn set_topology_project(&mut self, space: &str, project: Option<String>) {
        match project {
            Some(project) => {
                self.topology_project.insert(space.to_owned(), project);
            }
            None => {
                self.topology_project.remove(space);
            }
        }
    }

    /// Currently selected `(space, agent)` on the Team Topology canvas.
    #[must_use]
    pub fn topology_selected_agent(&self) -> Option<(&str, &str)> {
        self.topology_selected_agent
            .as_ref()
            .map(|(space, agent)| (space.as_str(), agent.as_str()))
    }

    /// Selects or toggles an agent on the Team Topology canvas.
    pub fn toggle_topology_agent(&mut self, space: &str, agent: &str) {
        let key = (space.to_owned(), agent.to_owned());
        if self.topology_selected_agent.as_ref() == Some(&key) {
            self.topology_selected_agent = None;
        } else {
            self.topology_selected_agent = Some(key);
        }
    }

    #[must_use]
    pub fn inbox_show_all_failures(&self) -> bool {
        self.inbox_show_all_failures
    }

    pub fn toggle_inbox_all_failures(&mut self) {
        self.inbox_show_all_failures = !self.inbox_show_all_failures;
    }

    #[must_use]
    pub fn inbox_show_all_recent(&self) -> bool {
        self.inbox_show_all_recent
    }

    pub fn toggle_inbox_all_recent(&mut self) {
        self.inbox_show_all_recent = !self.inbox_show_all_recent;
    }

    #[must_use]
    pub fn inbox_show_quiet_updates(&self) -> bool {
        self.inbox_show_quiet_updates
    }

    pub fn toggle_inbox_quiet_updates(&mut self) {
        self.inbox_show_quiet_updates = !self.inbox_show_quiet_updates;
    }

    fn shown(&self, space: &str) -> bool {
        self.filter.as_deref().is_none_or(|only| only == space)
    }

    /// The decisions Home lists: those of the spaces it shows, oldest first.
    pub fn needs_you(&self) -> impl Iterator<Item = &Queued> {
        self.queue.iter().filter(|queued| self.shown(&queued.space))
    }

    /// Whether the Inbox's Needs you tab has nothing to show: no decisions,
    /// blockers or running tasks in the spaces shown, and no gaps in either.
    #[must_use]
    pub fn caught_up(&self) -> bool {
        self.needs_you().next().is_none()
            && self.running().is_empty()
            && self.gaps(Part::Decisions).is_empty()
            && self.gaps(Part::Running).is_empty()
            && self
                .spaces
                .iter()
                .filter(|space| self.shown(&space.name))
                .all(|space| self.blocked(&space.name).is_none())
    }

    /// The contents and treatment of an open decision card, by decision kind.
    #[must_use]
    pub fn decision_card(queued: &Queued) -> DecisionCard {
        let decision = &queued.decision;
        let dashed = decision.kind == DecisionKind::Proposal;
        let (context, details, would_run_as, note_label) = match decision.kind {
            DecisionKind::Proposal if decision.is_instruction_proposal() => (
                Some("You did not ask for this. Dreaming suggested a change to project instructions.".to_owned()),
                context_line(decision)
                    .map(str::to_owned)
                    .into_iter()
                    .collect(),
                None,
                "Note (optional)",
            ),
            DecisionKind::Proposal => {
                let proposer =
                    decision
                        .proposed_by
                        .as_deref()
                        .unwrap_or(if decision.task_id.is_some() {
                            "The task’s agent"
                        } else {
                            "Lead"
                        });
                let repositories = decision.arguments["repositories"].as_array().map(|names| {
                    names
                        .iter()
                        .filter_map(Json::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                });
                let repositories = match repositories.as_deref() {
                    Some("") => "no repositories",
                    Some(names) => names,
                    None if decision.task_id.is_some() => "inherits task repositories",
                    None => "no repositories",
                };
                (
                    Some(format!(
                        "You did not ask for this. {proposer} is Assisted here, so it proposes first."
                    )),
                    context_line(decision)
                        .into_iter()
                        .filter(|line| !line.starts_with("From work you did not ask for."))
                        .map(str::to_owned)
                        .collect(),
                    Some(format!(
                        "Would run as: {} · {repositories}",
                        proposed_assignee(decision)
                    )),
                    "Note (optional)",
                )
            }
            DecisionKind::Input => (
                context_line(decision).map(str::to_owned),
                Vec::new(),
                None,
                "Your answer (optional)",
            ),
            DecisionKind::Approval => (
                context_line(decision).map(str::to_owned),
                arguments(decision),
                None,
                "Note (optional)",
            ),
        };
        let diff = decision
            .arguments
            .get(threadlane_core::link::DIFF_PREVIEW)
            .and_then(|value| serde_json::from_value(value.clone()).ok());
        DecisionCard {
            chip: (decision.kind != DecisionKind::Approval).then(|| decision_label(decision.kind)),
            dashed,
            context,
            details,
            would_run_as,
            note_label,
            recommended: decision
                .recommended
                .and_then(|index| usize::try_from(index).ok()),
            diff,
        }
    }

    /// How many decisions wait on you in project `id` of `space`.
    #[must_use]
    pub fn open_in(&self, space: &str, id: ProjectId) -> usize {
        self.queue
            .iter()
            .filter(|queued| queued.space == space && queued.decision.project_id == id)
            .count()
    }

    /// One reminder notification for each decision that has waited
    /// [`REMIND_AFTER`] or longer and was not reminded about yet.
    pub fn reminders(&mut self, now: Timestamp) -> Vec<Alert> {
        let due: Vec<&Queued> = self
            .queue
            .iter()
            .filter(|queued| {
                let key = DecisionKey::from(*queued);
                now.saturating_since(queued.decision.created_at) >= REMIND_AFTER
                    && !self.answering.contains(&key)
                    && !self.reminded.contains(&key)
            })
            .collect();
        let alerts: Vec<Alert> = due
            .iter()
            .map(|queued| {
                let decision = &queued.decision;
                let label = self
                    .project_name(&queued.space, decision.project_id)
                    .map_or_else(
                        || queued.space.clone(),
                        |project| format!("{}/{project}", queued.space),
                    );
                Alert::Decision {
                    space: queued.space.clone(),
                    label,
                    card: Card {
                        id: decision.id.to_string(),
                        question: decision.question.clone(),
                        options: decision
                            .options
                            .iter()
                            .map(|option| option.label.clone())
                            .collect(),
                        kind: Some(decision.kind),
                        context: None,
                        recommended: None,
                    },
                    reminder: true,
                }
            })
            .collect();
        let keys: Vec<DecisionKey> = due.into_iter().map(DecisionKey::from).collect();
        self.reminded.extend(keys);
        alerts
    }

    /// The pending decision that goes with a delivered card, if it still waits.
    #[must_use]
    pub fn queued_card(&self, space: &str, card: &Card) -> Option<&Queued> {
        let id = DecisionId::parse(&card.id)?;
        self.queue
            .iter()
            .find(|queued| queued.space == space && queued.decision.id == id)
    }

    /// The decision waiting on you in task `task` of `space`, if any.
    #[must_use]
    pub fn open_for(&self, space: &str, task: TaskId) -> Option<&Queued> {
        self.queue
            .iter()
            .find(|queued| queued.space == space && queued.decision.task_id == Some(task))
    }

    /// What the task screen's composer says about where a message goes: to
    /// the task, and while a decision is open there, that it declines it.
    #[must_use]
    pub fn task_hint(&self, space: &str, task: TaskId) -> String {
        let stopped = self.task(space, task).is_some_and(|detail| detail.stopped);
        let destination = if stopped {
            "Send a message to resume this task.".to_owned()
        } else {
            format!("Goes to {task}, not the lead.")
        };
        match self.open_for(space, task) {
            Some(queued) if queued.decision.kind == DecisionKind::Input => {
                let prefix = if stopped {
                    format!("{destination} ")
                } else {
                    String::new()
                };
                format!(
                    "{prefix}Sending answers {} with your message.",
                    queued.decision.id
                )
            }
            Some(queued) => format!(
                "{destination} Sending declines {} with your message as the note.",
                queued.decision.id
            ),
            None => destination,
        }
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

    /// Active leads in the spaces Home is showing.
    #[must_use]
    pub fn running_leads(&self) -> Vec<(&str, &str, Timestamp)> {
        self.spaces
            .iter()
            .filter(|space| self.shown(&space.name))
            .flat_map(|space| {
                space
                    .leads
                    .iter()
                    .map(|(project, at)| (space.name.as_str(), project.as_str(), *at))
            })
            .collect()
    }

    /// Home's recent results: the latest message or notice of each project
    /// (and of each space as a whole) in the spaces it shows, newest first.
    #[must_use]
    pub fn recent(&self) -> Vec<&SpaceFeedItem> {
        let mut latest = HashMap::<_, &SpaceFeedItem>::new();
        for item in self.delivered.values() {
            if item.kind() == OutboxKind::Decision || !self.shown(&item.space) {
                continue;
            }
            let key = (
                item.space.as_str(),
                item.item.project_name.as_deref(),
                (!item.item.digest.is_empty()).then_some(item.item.seq),
            );
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
    /// of 2 machines online · 4 recorded, not acted on`, for the spaces it
    /// shows. Machines are left out while a space has no current count, and
    /// the recorded events while there are none.
    #[must_use]
    pub fn status_line(&self) -> String {
        let shown: Vec<&Space> = self
            .spaces
            .iter()
            .filter(|space| self.shown(&space.name))
            .collect();
        let reachable = shown
            .iter()
            .filter(|space| space.reach == Reach::Reachable)
            .count();
        let spaces = count_form(reachable, "space", "spaces");
        let recorded: u64 = shown.iter().filter_map(|space| space.recorded).sum();
        let recorded = if recorded == 0 {
            String::new()
        } else {
            format!(" · {recorded} recorded, not acted on")
        };
        let machines = machine_count(shown.into_iter()).map_or_else(String::new, |(on, all)| {
            format!(
                " · {on} of {all} {} online",
                count_form(all, "machine", "machines")
            )
        });
        let decisions = self.needs_you().count();
        let need = count_form(decisions, "needs", "need");
        format!(
            "{reachable} {spaces} reachable · {} running · {decisions} {need} you{machines}{recorded}",
            self.running().len() + self.running_leads().len(),
        )
    }

    /// Every project the composer can send to, as `(space, project)`.
    #[must_use]
    pub fn targets(&self) -> Vec<(&str, &str)> {
        self.spaces
            .iter()
            .flat_map(|space| {
                space
                    .active_projects()
                    .map(move |project| (space.name.as_str(), project.name.as_str()))
            })
            .collect()
    }

    /// Where the Home composer sends: the last project chosen or sent to,
    /// else the General project of the first space shown (Plan.md §2.1).
    #[must_use]
    pub fn target(&self) -> Option<(&str, &str)> {
        self.target_space
            .as_deref()
            .and_then(|name| self.space(name))
            .and_then(|space| self.target_in(space))
            .or_else(|| {
                self.spaces
                    .iter()
                    .filter(|space| self.shown(&space.name))
                    .find_map(|space| self.target_in(space))
            })
    }

    fn target_in<'a>(&self, space: &'a Space) -> Option<(&'a str, &'a str)> {
        let last = self.last_targets.get(&space.name);
        let project = space
            .active_projects()
            .find(|project| Some(&project.name) == last)
            .or_else(|| space.active_projects().find(|project| project.is_general))
            .or_else(|| space.active_projects().next())?;
        Some((&space.name, &project.name))
    }

    /// Makes the Home composer send to `project` of `space` and remembers the choice.
    pub fn set_target(&mut self, space: &str, project: &str) {
        self.target_space = Some(space.to_owned());
        self.remember_target(space, project);
    }

    /// Every lead a composer can send to, by space: the space lead (its
    /// General project) first, then each other active project's lead.
    #[must_use]
    pub fn lead_targets(&self) -> Vec<(&str, &str)> {
        self.spaces
            .iter()
            .flat_map(|space| {
                let general = space.active_projects().filter(|project| project.is_general);
                let rest = space
                    .active_projects()
                    .filter(|project| !project.is_general);
                general
                    .chain(rest)
                    .map(move |project| (space.name.as_str(), project.name.as_str()))
            })
            .collect()
    }

    /// `Space lead` for a space's General project, else `<project> lead`.
    #[must_use]
    pub fn lead_name(&self, space: &str, project: &str) -> String {
        if self.space(space).and_then(Space::main_chat_project) == Some(project) {
            "Space lead".to_owned()
        } else {
            format!("{project} lead")
        }
    }

    /// The composer's "To:" button, naming the space only when more than one is paired.
    #[must_use]
    pub fn to_label(&self, space: &str, project: &str) -> String {
        let lead = self.lead_name(space, project);
        if self.spaces.len() > 1 {
            format!("To: {space} · {lead}")
        } else {
            format!("To: {lead}")
        }
    }

    /// Where the Chat tab of `project` sends: its own lead unless another
    /// lead was picked on that page and can still be reached.
    #[must_use]
    pub fn chat_target<'a>(&'a self, space: &'a str, project: &'a str) -> (&'a str, &'a str) {
        self.chat_target
            .as_ref()
            .filter(|(page, _)| page.0 == space && page.1 == project)
            .map(|(_, (space, project))| (space.as_str(), project.as_str()))
            .filter(|target| self.lead_targets().contains(target))
            .unwrap_or((space, project))
    }

    /// Makes the Chat tab of `page` send to `target` until you leave it.
    pub fn set_chat_target(&mut self, page: (&str, &str), target: (&str, &str)) {
        let owned = |(space, project): (&str, &str)| (space.to_owned(), project.to_owned());
        self.chat_target = Some((owned(page), owned(target)));
    }

    /// Returns every Chat tab to its own lead, as when you navigate away.
    pub fn reset_chat_target(&mut self) {
        self.chat_target = None;
    }

    /// Pacing notices for the spaces Home shows, using the display clock.
    #[must_use]
    pub fn held_lines(&self, when: &When) -> Vec<String> {
        self.spaces
            .iter()
            .filter(|space| self.shown(&space.name) && space.reach == Reach::Reachable)
            .filter_map(|space| {
                let held = space.held?;
                let count = held.count;
                let noun = count_form(count, "result", "results");
                let release = if held.next == held.latest {
                    "until"
                } else {
                    "· next at"
                };
                let time = when.moment(held.next);
                Some(format!(
                    "{} · {count} {noun} held {release} {time}",
                    space.name
                ))
            })
            .collect()
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

    /// The last action and where it stands, if any.
    #[must_use]
    pub fn activity(&self) -> Option<&Activity> {
        self.activity.as_ref()
    }

    /// Records the last action: it stays shown, on every screen, until the
    /// next one.
    pub fn report(&mut self, activity: Activity) {
        self.activity = Some(activity);
    }

    /// Records that `text` (an id, a branch, a link) was put on the
    /// clipboard: the status area says so.
    pub fn copied(&mut self, text: &str) {
        self.activity = Some(Activity::Done(format!("Copied {text}")));
    }

    /// Whether no hub is paired, so the app shows how to pair one instead
    /// of an empty Home.
    #[must_use]
    pub fn first_run(&self) -> bool {
        self.spaces.is_empty()
    }

    /// Takes a space this app was just paired with: the sidebar lists it at
    /// once, and the next refresh fills it in.
    pub fn space_added(&mut self, name: &str) {
        self.forgotten.remove(name);
        if self.space(name).is_none() {
            self.spaces.push(Space::new(name));
        }
    }

    /// Opens the newly paired space in Home.
    pub fn open_paired_space(&mut self, name: &str) {
        self.space_added(name);
        self.set_filter(Some(name));
    }

    /// Shows or hides this space's archived projects in the sidebar.
    pub fn toggle_archived(&mut self, space: &str) {
        if let Some(space) = self.spaces.iter_mut().find(|known| known.name == space) {
            space.archived_open = !space.archived_open;
        }
    }

    /// Asks to unpair `space`, which waits for [`AppState::space_removed`]
    /// or [`AppState::cancel_unpair`].
    pub fn ask_unpair(&mut self, space: &str) {
        self.unpairing = Some(space.to_owned());
    }

    /// The space waiting for you to confirm that it should be unpaired.
    #[must_use]
    pub fn unpairing(&self) -> Option<&str> {
        self.unpairing.as_deref()
    }

    /// Keeps the space you asked to unpair.
    pub fn cancel_unpair(&mut self) {
        self.unpairing = None;
    }

    /// Forgets a space this app was unpaired from: it leaves the sidebar,
    /// Home and the composer at once, and its feed is no longer shown.
    pub fn space_removed(&mut self, name: &str) {
        self.unpairing = None;
        self.forgotten.insert(name.to_owned());
        self.new_home_replies.remove(name);
        self.spaces.retain(|space| space.name != name);
        self.queue.retain(|queued| queued.space != name);
        self.decision_records.retain(|key, _| key.space != name);
        self.decision_notes.retain(|key| key.space != name);
        self.delivered.retain(|(space, _), _| space != name);
        self.conversation_read.retain(|(space, _), _| space != name);
        self.save_conversation_seen();
        self.said.retain(|(space, _), _| space != name);
        self.loaded.retain(|(space, _), _| space != name);
        self.earlier_requests.retain(|(space, _), _| space != name);
        self.projects.retain(|(space, _), _| space != name);
        self.tasks.retain(|(space, _), _| space != name);
        self.memory.retain(|(space, _), _| space != name);
        self.dreaming.retain(|(space, _), _| space != name);
        self.metrics.remove(name);
        self.machine_history.remove(name);
        self.task_activity.retain(|(space, _)| space != name);
        self.routines.retain(|(space, _), _| space != name);
        if self
            .cancelling
            .as_ref()
            .is_some_and(|(space, _)| space == name)
        {
            self.cancelling = None;
        }
        self.codes.remove(name);
        self.recorded_open.retain(|(space, _)| space != name);
        self.always.retain(|(space, _)| space != name);
        self.expanded_quiet.retain(|(space, _)| space != name);
        self.topology_project.remove(name);
        if self
            .topology_selected_agent
            .as_ref()
            .is_some_and(|(space, _)| space == name)
        {
            self.topology_selected_agent = None;
        }
        if self
            .open_agent
            .as_ref()
            .is_some_and(|(space, ..)| space == name)
        {
            self.open_agent = None;
        }
        let here = |at: Option<&String>| at.is_some_and(|space| space == name);
        if here(self.revoking.as_ref().map(|(space, _)| space)) {
            self.revoking = None;
        }
        if here(self.space_ceiling_asked.as_ref().map(|(space, _)| space)) {
            self.space_ceiling_asked = None;
        }
        if here(self.agent_form.space.as_ref()) {
            self.agent_form = AgentForm::default();
        }
        if here(self.agent_edit.as_ref().map(|edit| &edit.space)) {
            self.agent_edit = None;
        }
        if here(self.repository_form.as_ref().map(|(space, _)| space)) {
            self.repository_form = None;
        }
        if here(self.cancelling_task.as_ref().map(|(space, _)| space)) {
            self.cancelling_task = None;
        }
        self.task_form = TaskForm::default();
        if self.filter.as_deref() == Some(name) {
            self.filter = None;
        }
        self.failures.retain(|(space, _), _| space != name);
        self.pacing.remove(name);
        self.stopping.retain(|(space, _)| space != name);
        if self.pacing_edit.as_deref() == Some(name) {
            self.pacing_edit = None;
        }
        if self.target_space.as_deref() == Some(name) {
            self.target_space = None;
        }
        if self.last_targets.remove(name).is_some() {
            self.save_targets();
        }
    }

    /// Takes what a refresh learned. A hub that could not be reached, or
    /// could not list something, keeps what it last reported.
    pub fn apply(&mut self, refresh: Refresh) {
        self.apply_at(refresh, SystemClock.now());
    }

    /// Applies a refresh with the timestamp of its connection check.
    pub fn apply_at(&mut self, refresh: Refresh, now: Timestamp) {
        self.checking_connections = false;
        self.apply_snapshot(refresh, now, false);
    }

    /// Applies just the hub that answered, preserving other hubs and the
    /// connection check in progress until the caller has all results.
    pub fn apply_partial(&mut self, refresh: Refresh) {
        self.apply_snapshot(refresh, SystemClock.now(), true);
    }

    /// Reconciles partial refresh state with its complete pairing roster.
    /// In particular, a pairing operation can change the roster before its
    /// follow-up cleanup reports an error.
    pub fn retain_paired_spaces(&mut self, paired: &[String]) {
        let known: HashSet<_> = paired.iter().map(String::as_str).collect();
        self.spaces
            .retain(|space| known.contains(space.name.as_str()));
        self.queue
            .retain(|queued| known.contains(queued.space.as_str()));
    }

    fn apply_snapshot(&mut self, refresh: Refresh, now: Timestamp, partial: bool) {
        let Refresh {
            statuses,
            leads,
            projects,
            agents,
            clients,
            decisions,
            running,
            machines,
            recent,
            recorded,
            held,
            tasks,
        } = refresh;
        let mut spaces = if partial {
            std::mem::take(&mut self.spaces)
        } else {
            Vec::with_capacity(statuses.len())
        };
        for status in statuses {
            let existing = spaces.iter().position(|space| space.name == status.name);
            let mut space = if let Some(at) = existing {
                std::mem::replace(&mut spaces[at], Space::new(&status.name))
            } else {
                self.space(&status.name)
                    .cloned()
                    .unwrap_or_else(|| Space::new(&status.name))
            };
            (space.reached_by, space.local_socket) = match status.reach {
                threadlane_client::Reach::Socket { socket } => {
                    (format!("Unix socket {}", socket.display()), Some(socket))
                }
                threadlane_client::Reach::Iroh { hub, .. } => {
                    (format!("Network, hub key {}", hub.fmt_short()), None)
                }
            };
            space.reach = match status.info {
                Ok(info) => {
                    space.info = Some(info);
                    space.unreachable_since = None;
                    Reach::Reachable
                }
                Err(error) => {
                    space.unreachable_since.get_or_insert(now);
                    Reach::Unreachable(describe(&error))
                }
            };
            if let Some(at) = existing {
                spaces[at] = space;
            } else {
                spaces.push(space);
            }
        }
        let find = |spaces: &mut Vec<Space>, name: &str| -> Option<usize> {
            spaces.iter().position(|space| space.name == name)
        };
        let mut removed = Vec::new();
        for (name, listed) in projects {
            let current = listed.is_ok();
            if let Some(at) = find(&mut spaces, &name)
                && let Some(listed) = spaces[at].take(Part::Projects, listed)
                && current
            {
                spaces[at].leads.retain(|(name, _)| {
                    listed
                        .iter()
                        .any(|project| !project.archived && project.name == *name)
                });
                removed.extend(
                    spaces[at]
                        .projects
                        .iter()
                        .filter(|old| !listed.iter().any(|current| current.id == old.id))
                        .map(|old| (name.clone(), old.name.clone(), old.id)),
                );
                spaces[at].projects = listed;
            }
        }
        for (name, lead) in leads {
            if let Some(at) = find(&mut spaces, &name)
                && spaces[at]
                    .projects
                    .iter()
                    .any(|project| project.name == lead.project && !project.archived)
            {
                spaces[at].leads.retain(|(known, _)| *known != lead.project);
                if let Some(start) = lead.running_since {
                    spaces[at].leads.push((lead.project.clone(), start));
                }
                let view = self.projects.entry((name, lead.project)).or_default();
                view.lead = lead.running_since;
                view.stopped = lead.stopped;
                view.answers.insert(Part::Lead, Answer::Answered);
            }
        }
        for (name, listed) in agents {
            if let Some(at) = find(&mut spaces, &name)
                && let Some(listed) = spaces[at].take(Part::Agents, listed)
            {
                spaces[at].agents = listed;
            }
        }
        for (name, listed) in clients {
            if let Some(at) = find(&mut spaces, &name)
                && let Some(listed) = spaces[at].take(Part::Clients, listed)
            {
                spaces[at].clients = listed;
            }
        }
        for SpaceListing { space, listed } in running {
            if let Some(at) = find(&mut spaces, &space)
                && let Some(listed) = spaces[at].take(Part::Running, listed)
            {
                spaces[at].running = listed;
            }
        }
        for SpaceListing { space, listed } in machines {
            if let Some(at) = find(&mut spaces, &space) {
                let listed = listed.map(|listed| (listed.machines, listed.accounts));
                if let Some((machines, accounts)) = spaces[at].take(Part::Machines, listed) {
                    spaces[at].machines = machines;
                    spaces[at].accounts = accounts;
                }
            }
        }
        for (name, listed) in tasks {
            if let Some(at) = find(&mut spaces, &name)
                && let Some(listed) = spaces[at].take(Part::Tasks, listed)
            {
                if matches!(spaces[at].answers.get(&Part::Tasks), Some(Answer::Answered))
                    && matches!(
                        spaces[at].answers.get(&Part::Projects),
                        Some(Answer::Answered)
                    )
                {
                    let mut by_project = HashMap::<_, Vec<_>>::new();
                    for summary in &listed {
                        by_project
                            .entry(summary.task.project_id)
                            .or_default()
                            .push(summary.clone());
                    }
                    for project in spaces[at]
                        .projects
                        .iter()
                        .filter(|project| !project.archived)
                    {
                        let view = self
                            .projects
                            .entry((name.clone(), project.name.clone()))
                            .or_default();
                        view.tasks = by_project.remove(&project.id).unwrap_or_default();
                        view.answers.insert(Part::Tasks, Answer::Answered);
                    }
                }
                spaces[at].tasks = listed;
            }
        }
        for SpaceListing { space, listed } in recorded {
            if let Some(at) = find(&mut spaces, &space)
                && spaces[at].reach == Reach::Reachable
            {
                spaces[at].recorded = listed.ok();
            }
        }
        for SpaceListing { space, listed } in held {
            if let Some(at) = find(&mut spaces, &space) {
                spaces[at].held = listed.ok().flatten();
            }
        }
        let mut items = Vec::new();
        for SpaceListing { space, listed } in recent {
            if let Some(at) = find(&mut spaces, &space) {
                items.extend(spaces[at].take(Part::Recent, listed).unwrap_or_default());
            }
        }
        let mut pending = Vec::new();
        for SpaceDecisions { space, decisions } in decisions {
            if let Some(at) = find(&mut spaces, &space)
                && let Some(listed) = spaces[at].take(Part::Decisions, decisions)
            {
                pending.push((space, listed));
            }
        }
        self.spaces = spaces;
        for (space, project, id) in &removed {
            self.forget_project(space, project, *id);
        }
        for item in items {
            if removed.iter().any(|(space, _, id)| {
                *space == item.space && Some(*id) == item.item.message.project_id
            }) {
                continue;
            }
            self.deliver(item);
        }

        let paired: HashSet<&str> = self
            .spaces
            .iter()
            .map(|space| space.name.as_str())
            .collect();
        self.queue
            .retain(|queued| paired.contains(queued.space.as_str()));
        for (space, pending) in pending {
            for queued in self.queue.iter().filter(|queued| queued.space == space) {
                self.decision_records
                    .entry(DecisionKey::from(queued))
                    .or_insert_with(|| queued.decision.clone());
            }
            self.queue.retain(|queued| queued.space != space);
            self.queue
                .extend(pending.into_iter().filter_map(|decision| {
                    let key = DecisionKey {
                        space: space.clone(),
                        id: decision.id,
                    };
                    self.decision_records
                        .get(&key)
                        .is_none_or(|known| {
                            known.status == threadlane_core::model::DecisionStatus::Pending
                        })
                        .then(|| Queued {
                            space: space.clone(),
                            decision,
                        })
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

    fn forget_project(&mut self, space: &str, project: &str, id: ProjectId) {
        let key = (space.to_owned(), project.to_owned());
        let task_ids: HashSet<_> = self
            .spaces
            .iter()
            .filter(|known| known.name == space)
            .flat_map(|known| &known.tasks)
            .filter(|task| task.task.project_id == id)
            .map(|task| task.task.id)
            .chain(self.tasks.iter().filter_map(|((at, task), detail)| {
                (at == space && detail.summary.task.project_id == id).then_some(*task)
            }))
            .collect();
        if let Some(known) = self.spaces.iter_mut().find(|known| known.name == space) {
            known.projects.retain(|known| known.id != id);
            known.tasks.retain(|task| task.task.project_id != id);
            known.running.retain(|task| task.task.project_id != id);
            known.leads.retain(|(known, _)| known != project);
        }
        self.projects.remove(&key);
        self.memory.remove(&key);
        self.routines.remove(&key);
        self.dreaming.remove(&key);
        self.loaded.remove(&key);
        self.recorded_open.remove(&key);
        self.conversation_read.remove(&key);
        self.save_conversation_seen();
        self.tasks
            .retain(|(at, task), _| at != space || !task_ids.contains(task));
        self.task_failures
            .retain(|(at, task), _| at != space || !task_ids.contains(task));
        self.task_activity
            .retain(|(at, task)| at != space || !task_ids.contains(task));
        self.machine_history.remove(space);
        self.said
            .retain(|(at, _), said| at != space || said.project != project);
        self.failures
            .retain(|(at, _), said| at != space || said.project != project);
        self.delivered.retain(|(at, _), item| {
            at != space
                || (item.item.message.project_id != Some(id)
                    && item
                        .item
                        .digest
                        .iter()
                        .all(|entry| entry.project.as_deref() != Some(project)))
        });
        self.queue
            .retain(|queued| queued.space != space || queued.decision.project_id != id);
        self.decision_records
            .retain(|key, record| key.space != space || record.project_id != id);
        if self
            .last_targets
            .get(space)
            .is_some_and(|known| known == project)
        {
            self.last_targets.remove(space);
            self.save_targets();
        }
    }

    /// Takes what the project screen loaded. A part that could not be
    /// listed shows the hub's error instead; the conversation keeps what
    /// was delivered.
    pub fn apply_project(&mut self, load: ProjectLoad) {
        let ProjectLoad {
            space,
            project,
            tasks,
            repositories,
            bindings,
            history,
            decision_records,
            lead,
            recorded,
            rules,
            lead_runs,
        } = load;
        if self.project_absent(&space, &project) {
            return;
        }
        let view = self
            .projects
            .entry((space.clone(), project.clone()))
            .or_default();
        if let Some(tasks) = tasks {
            view.tasks = take(&mut view.answers, Part::Tasks, tasks);
        }
        if let Some(repositories) = repositories {
            view.repositories = take(&mut view.answers, Part::Repositories, repositories);
        }
        if let Some(bindings) = bindings {
            view.bindings = take(&mut view.answers, Part::Bindings, bindings);
        }
        if let Some(lead) = lead {
            (view.lead, view.stopped) = take(&mut view.answers, Part::Lead, lead);
        }
        if let Some(recorded) = recorded {
            view.recorded = Some(recorded.map_err(|error| describe(&error)));
        }
        if let Some(rules) = rules {
            view.rules = take(&mut view.answers, Part::Rules, rules);
        }
        if let Some(Ok(runs)) = lead_runs {
            view.lead_runs = runs;
        }
        if let Some(history) = history {
            let listed = history.is_ok();
            let entries = take(&mut view.answers, Part::Conversation, history);
            self.remember_decisions(&space, decision_records);
            if listed {
                self.take_history(&space, &project, entries, true, PAGE);
            }
        }
    }

    /// Records the repositories of `project` in `space`.
    pub fn apply_repositories(
        &mut self,
        space: &str,
        project: &str,
        repositories: Result<Vec<threadlane_core::model::Repository>, ClientError>,
    ) {
        if self.project_absent(space, project) {
            return;
        }
        let view = self
            .projects
            .entry((space.to_owned(), project.to_owned()))
            .or_default();
        view.repositories = take(&mut view.answers, Part::Repositories, repositories);
    }

    /// The `owner/repo` a bare `#N` in `project` refers to: its only GitHub
    /// repository, or `None` when it has none or several.
    #[must_use]
    pub fn github_repository(&self, space: &str, project: &str) -> Option<String> {
        self.github_repository_among(space, project, None)
    }

    /// Like [`Self::github_repository`], among only `repositories` of
    /// `project` (such as a task's attached ones); `None` means all of them,
    /// while `Some(&[])` (a task with no repositories) means none.
    #[must_use]
    pub fn github_repository_among(
        &self,
        space: &str,
        project: &str,
        repositories: Option<&[threadlane_core::ids::RepositoryId]>,
    ) -> Option<String> {
        let mut slugs = self
            .project(space, project)?
            .repositories
            .iter()
            .filter(|repository| repositories.is_none_or(|among| among.contains(&repository.id)))
            .filter_map(|repository| github_repository(&repository.remote_url));
        let first = slugs.next()?;
        // Several repositories make a bare `#N` ambiguous: don't guess.
        slugs
            .all(|slug| slug.eq_ignore_ascii_case(&first))
            .then_some(first)
    }

    /// Takes the bindings of each project, for the modes grid.
    pub fn apply_modes(&mut self, loads: Vec<BindingsLoad>) {
        for BindingsLoad {
            space,
            project,
            bindings,
        } in loads
        {
            let view = self.projects.entry((space, project)).or_default();
            view.bindings = take(&mut view.answers, Part::Bindings, bindings);
        }
    }

    /// What to show in place of `part` of `project` in `space`, if anything.
    #[must_use]
    pub fn project_gap(&self, space: &str, project: &str, part: Part) -> Option<Gap> {
        self.project(space, project)
            .map_or(Some(Gap::Loading), |view| gap_in(&view.answers, part))
    }

    /// The conversation header's line about the lead: `Lead is working ·
    /// started 09:20` while it runs, why that is not known if the hub could
    /// not say, `None` while it is idle.
    #[must_use]
    pub fn lead_line(&self, space: &str, project: &str, when: &When) -> Option<String> {
        match self.project_gap(space, project, Part::Lead) {
            Some(Gap::Failed(why)) => Some(format!("Whether the lead is working: {why}")),
            Some(Gap::Loading) => None,
            None => {
                let view = self.project(space, project)?;
                match (view.stopped, view.lead) {
                    (true, Some(_)) => {
                        Some("Stopping the lead. Send a message to resume the lane.".to_owned())
                    }
                    (true, None) => Some("Lead stopped. Send a message to resume.".to_owned()),
                    (false, Some(since)) => {
                        Some(format!("Lead is working · started {}", when.time(since)))
                    }
                    (false, None) => None,
                }
            }
        }
    }

    /// What happened to the current earlier page request, if any.
    #[must_use]
    pub fn earlier_status(&self, space: &str, project: &str) -> Option<&EarlierStatus> {
        self.earlier_requests
            .get(&(space.to_owned(), project.to_owned()))
            .filter(|(before, status)| {
                *status == EarlierStatus::Loading || self.earlier(space, project) == Some(*before)
            })
            .map(|(_, status)| status)
    }

    /// Acquires the earlier-page request for this conversation.
    pub fn begin_earlier(&mut self, space: &str, project: &str, before: Timestamp) -> bool {
        if self.earlier(space, project) != Some(before)
            || self.earlier_status(space, project) == Some(&EarlierStatus::Loading)
        {
            return false;
        }
        self.earlier_requests.insert(
            (space.to_owned(), project.to_owned()),
            (before, EarlierStatus::Loading),
        );
        true
    }

    /// Takes an earlier page of a conversation. Returns true when history was applied.
    pub fn apply_earlier(&mut self, load: EarlierLoad) -> bool {
        let key = (load.space, load.project);
        if self.earlier_requests.get(&key) != Some(&(load.before, EarlierStatus::Loading)) {
            return false;
        }
        self.earlier_requests.remove(&key);
        if self.earlier(&key.0, &key.1) != Some(load.before) {
            return false;
        }
        match load.history {
            Ok(entries) => {
                self.remember_decisions(&key.0, load.decision_records);
                self.take_history(&key.0, &key.1, entries, false, PAGE);
                true
            }
            Err(error) => {
                self.earlier_requests
                    .insert(key, (load.before, EarlierStatus::Failed(describe(&error))));
                false
            }
        }
    }

    /// Takes a page of history. The newest page leaves how far back is
    /// loaded alone once an earlier page was taken.
    pub(crate) fn take_history(
        &mut self,
        space: &str,
        project: &str,
        entries: Vec<ConversationEntry>,
        newest: bool,
        limit: u32,
    ) {
        let full = entries.len() >= usize::try_from(limit).unwrap_or(usize::MAX);
        let mut oldest = None;
        for entry in entries {
            let at = match entry {
                ConversationEntry::You {
                    event_id,
                    text,
                    images,
                    at,
                } => {
                    self.said.insert(
                        (space.to_owned(), event_id),
                        Said {
                            project: project.to_owned(),
                            text,
                            images,
                            at,
                            by: Author::You,
                        },
                    );
                    at
                }
                ConversationEntry::SpaceLead { event_id, text, at } => {
                    self.take_said(space, project, event_id, text, at, Author::SpaceLead)
                }
                ConversationEntry::ProjectReply {
                    event_id,
                    project: from,
                    text,
                    at,
                    ..
                } => self.take_said(
                    space,
                    project,
                    event_id,
                    text,
                    at,
                    Author::ProjectLead(from),
                ),
                ConversationEntry::DeliveryFailure { event_id, text, at } => {
                    self.failures.insert(
                        (space.to_owned(), event_id),
                        Said {
                            project: project.to_owned(),
                            text,
                            at,
                            by: Author::You,
                            images: Vec::new(),
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

    fn take_said(
        &mut self,
        space: &str,
        project: &str,
        event: EventId,
        text: String,
        at: Timestamp,
        by: Author,
    ) -> Timestamp {
        let said = Said {
            project: project.to_owned(),
            text,
            at,
            by,
            images: Vec::new(),
        };
        self.said.insert((space.to_owned(), event), said);
        at
    }

    /// Where an earlier page of the conversation starts, if there may be one.
    #[must_use]
    pub fn earlier(&self, space: &str, project: &str) -> Option<Timestamp> {
        let loaded = self.loaded.get(&(space.to_owned(), project.to_owned()))?;
        loaded.more.then_some(loaded.oldest).flatten()
    }

    /// Takes a task in full, or why its hub could not show it.
    pub fn apply_task(&mut self, load: TaskLoad) {
        let key = (load.space, load.task);
        match load.detail {
            Ok(detail) => {
                let records: Vec<_> = detail
                    .decisions
                    .iter()
                    .filter(|decision| self.is_pending(&key.0, &decision.id.to_string()))
                    .cloned()
                    .collect();
                self.remember_decisions(&key.0, records);
                self.task_failures.remove(&key);
                self.tasks.insert(key, detail);
            }
            Err(error) => {
                self.tasks.remove(&key);
                self.task_failures.insert(key, describe(&error));
            }
        }
    }

    /// What to show in place of task `task` of `space`, if anything.
    #[must_use]
    pub fn task_gap(&self, space: &str, task: TaskId) -> Option<Gap> {
        let key = (space.to_owned(), task);
        if let Some(why) = self.task_failures.get(&key) {
            return Some(Gap::Failed(why.clone()));
        }
        (!self.tasks.contains_key(&key)).then_some(Gap::Loading)
    }

    /// Takes a project its hub created: the sidebar and the composer list
    /// it at once.
    pub fn project_created(&mut self, space: &str, project: ProjectSummary) {
        self.report(Activity::Done(format!(
            "Created {} in {space}.",
            project.name
        )));
        if let Some(known) = self.spaces.iter_mut().find(|known| known.name == space)
            && !known.projects.iter().any(|other| other.id == project.id)
        {
            known.projects.push(project);
        }
    }

    /// Whether a Home result is explicitly expanded.
    #[must_use]
    pub fn result_expanded(&self, space: &str, seq: i64) -> bool {
        self.expanded.contains(&(space.to_owned(), seq))
    }

    /// Whether an older message is explicitly expanded.
    #[must_use]
    pub fn message_expanded(&self, key: &str) -> bool {
        self.expanded_messages.contains(key)
    }

    /// Expands or collapses an older message.
    pub fn toggle_message(&mut self, key: &str) {
        if !self.expanded_messages.remove(key) {
            self.expanded_messages.insert(key.to_owned());
        }
    }

    /// Shows all of a Home result, or only its start again.
    pub fn toggle_result(&mut self, space: &str, seq: i64) {
        let key = (space.to_owned(), seq);
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
    }

    /// Whether anything shown may be out of date, so the next refresh
    /// should come soon: a hub not known to be reachable, or a part that
    /// failed.
    #[must_use]
    pub fn retry_soon(&self) -> bool {
        let failed = |answers: &BTreeMap<Part, Answer>| {
            answers
                .values()
                .any(|answer| matches!(answer, Answer::Failed(_)))
        };
        self.spaces.iter().any(Space::stale)
            || self.projects.values().any(|view| failed(&view.answers))
            || self.memory.values().any(|view| failed(view.answers()))
            || self.routines.values().any(|view| failed(view.answers()))
    }

    /// `(online, all)` machines of every space, or `None` while one of them
    /// has no current count.
    #[must_use]
    pub fn machine_count(&self) -> Option<(usize, usize)> {
        machine_count(self.spaces.iter())
    }

    /// For each space Home shows that has nothing current for `part`: its
    /// name and what to show instead.
    #[must_use]
    pub fn gaps(&self, part: Part) -> Vec<(&str, Gap)> {
        self.spaces
            .iter()
            .filter(|space| self.shown(&space.name))
            .filter_map(|space| Some((space.name.as_str(), space.gap(part)?)))
            .collect()
    }

    /// The modes grid of `space` (wireframe 6): its agents, and one row per
    /// project with each agent's mode there.
    #[must_use]
    pub fn modes(&self, space: &str) -> ModesGrid {
        let Some(known) = self.space(space) else {
            return ModesGrid::default();
        };
        let agents: Vec<String> = known
            .agents
            .iter()
            .map(|agent| agent.name.clone())
            .collect();
        let rows = known
            .active_projects()
            .map(|project| {
                let modes = match self.project_gap(space, &project.name, Part::Bindings) {
                    Some(gap) => Err(gap),
                    None => {
                        let bindings = self
                            .project(space, &project.name)
                            .map(|view| view.bindings.as_slice())
                            .unwrap_or_default();
                        Ok(agents
                            .iter()
                            .map(|agent| {
                                bindings
                                    .iter()
                                    .find(|binding| binding.agent == *agent)
                                    .map(|binding| binding.mode)
                            })
                            .collect())
                    }
                };
                ModesRow {
                    project: project.name.clone(),
                    modes,
                }
            })
            .collect();
        ModesGrid { agents, rows }
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

    /// The Stop target in the conversation header, while the lead is running.
    #[must_use]
    pub fn lead_stop(&self, space: &str, project: &str) -> Option<LaneTarget> {
        let view = self.project(space, project)?;
        (view.lead.is_some() && !view.stopped).then(|| LaneTarget::Lead {
            project: Some(project.to_owned()),
        })
    }

    /// The Stop target on a running task.
    #[must_use]
    pub fn task_stop(&self, space: &str, task: TaskId) -> Option<LaneTarget> {
        let detail = self.task(space, task)?;
        ((detail.summary.task.status == TaskStatus::Running
            || (detail.summary.task.status == TaskStatus::InReview
                && detail.summary.running_since.is_some()))
            && !detail.stopped)
            .then_some(LaneTarget::Task { task })
    }

    /// A stop request is still waiting for the hub.
    #[must_use]
    pub fn stopping(&self, space: &str, target: &LaneTarget) -> bool {
        self.stopping
            .iter()
            .any(|(name, pending)| name == space && pending == target)
    }

    /// Marks a stop request as in flight. Returns false if it is already in flight.
    pub fn begin_stop(&mut self, space: &str, target: &LaneTarget) -> bool {
        if self.stopping(space, target) {
            return false;
        }
        self.stopping.push((space.to_owned(), target.clone()));
        true
    }

    /// Takes the outcome of the hub's stop request.
    pub fn finish_stop(
        &mut self,
        space: &str,
        target: &LaneTarget,
        result: Result<(), ClientError>,
    ) {
        self.stopping
            .retain(|(name, pending)| name != space || pending != target);
        match result {
            Ok(()) => {
                match target {
                    LaneTarget::Lead {
                        project: Some(project),
                    } => {
                        if let Some(view) =
                            self.projects.get_mut(&(space.to_owned(), project.clone()))
                        {
                            view.stopped = true;
                        }
                    }
                    LaneTarget::Task { task } => {
                        if let Some(detail) = self.tasks.get_mut(&(space.to_owned(), *task)) {
                            detail.stopped = true;
                        }
                    }
                    _ => {}
                }
                self.report(Activity::Done("Stop requested.".to_owned()));
            }
            Err(error) => self.report(Activity::Failed(describe(&error))),
        }
    }

    /// Takes a message its hub accepted, so the conversation shows it before
    /// the history does.
    pub fn sent(&mut self, space: &str, project: &str, text: &str, event: EventId, at: Timestamp) {
        self.sent_with_images(space, project, text, event, at, Vec::new());
    }

    pub(crate) fn sent_with_images(
        &mut self,
        space: &str,
        project: &str,
        text: &str,
        event: EventId,
        at: Timestamp,
        images: Vec<ImageAttachment>,
    ) {
        self.said
            .entry((space.to_owned(), event))
            .or_insert_with(|| Said {
                project: project.to_owned(),
                text: text.to_owned(),
                at,
                by: Author::You,
                images,
            });
        if let Some(view) = self
            .projects
            .get_mut(&(space.to_owned(), project.to_owned()))
        {
            view.stopped = false;
        }
    }

    /// Takes a message to a task its hub accepted, so the task's timeline
    /// shows it before the next load does.
    pub fn sent_to_task(
        &mut self,
        space: &str,
        task: TaskId,
        text: &str,
        event: EventId,
        at: Timestamp,
    ) {
        if let Some(detail) = self.tasks.get_mut(&(space.to_owned(), task)) {
            detail.stopped = false;
            let known = detail.messages.iter().any(|entry| {
                matches!(entry, ConversationEntry::You { event_id, .. } if *event_id == event)
            });
            if !known {
                detail.messages.push(ConversationEntry::You {
                    event_id: event,
                    text: text.to_owned(),
                    images: Vec::new(),
                    at,
                });
            }
        }
    }

    /// Takes an answer its hub accepted: the decision no longer waits, even
    /// when an earlier answer was the one that counted.
    pub fn answered(&mut self, answer: &SpaceAnswer) {
        self.remember_decisions(&answer.space, [answer.decision.clone()]);
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

    /// Whether the selected spaces received a Lead message since Home history was viewed.
    pub fn home_has_new_replies(&self) -> bool {
        self.new_home_replies.iter().any(|space| self.shown(space))
    }

    /// Acknowledge only replies in the current Home filter.
    pub fn saw_home_replies(&mut self) {
        let filter = self.filter.as_deref();
        self.new_home_replies
            .retain(|space| filter.is_some_and(|only| only != space));
    }

    /// Takes one item from the feed. Returns what it should raise a system
    /// notification for: a decision or a system notice seen for the first
    /// time. An ordinary message, or an item already taken (a feed replayed
    /// after a reconnect), raises none, and a repeat is not shown again.
    pub fn receive(&mut self, item: SpaceFeedItem) -> Option<Alert> {
        if self.forgotten.contains(&item.space)
            || !self.notified.insert((item.space.clone(), item.item.seq))
        {
            return None;
        }
        let alert = match item.kind() {
            OutboxKind::Message => {
                self.new_home_replies.insert(item.space.clone());
                None
            }
            OutboxKind::Decision => Card::of(&item).map(|mut card| {
                if card.kind.is_none() {
                    card.kind = self
                        .queued_card(&item.space, &card)
                        .map(|queued| queued.decision.kind);
                }
                Alert::Decision {
                    space: item.space.clone(),
                    label: item.label(),
                    card,
                    reminder: false,
                }
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
