//! The desktop app's state (Plan.md §11.1, milestone 4): what Home, a
//! conversation, and the decision queue show, and how hub replies and feed
//! items change it. Plain Rust with no user interface in it; the GPUI views
//! in `main.rs` only draw it. Every hub call goes through [`ClientCore`].

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value as Json;
use threadlane_client::{
    ClientCore, ClientError, FeedStream, HubStatus, SpaceAnswer, SpaceDecisions, SpaceFeedItem,
};
use threadlane_core::client::ProjectSummary;
use threadlane_core::ids::{DecisionId, ProjectId};
use threadlane_core::model::{Decision, OutboxKind};
use tokio::sync::{RwLock, mpsc};

/// One paired hub as Home shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Space {
    /// The name the hub was paired under.
    pub name: String,
    /// Why the hub could not be reached at the last refresh; `None` when it
    /// answered.
    pub unreachable: Option<String>,
    /// Its projects as last reported; kept while the hub is away (Plan.md
    /// §11.3 "unreachable hubs").
    pub projects: Vec<ProjectSummary>,
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
    decisions: Vec<SpaceDecisions>,
}

/// Asks every paired hub about itself, its projects, and its pending
/// decisions. A hub that does not answer is reported as such; the others are
/// unaffected.
pub async fn refresh(core: &ClientCore) -> Refresh {
    let statuses = core.status().await;
    let mut projects = Vec::new();
    for status in statuses.iter().filter(|status| status.is_reachable()) {
        let listed = core.projects(Some(&status.name)).await;
        projects.push((status.name.clone(), listed.map(|listed| listed.projects)));
    }
    // The only error with no space named is "no hub paired": nothing waits.
    let decisions = core.decisions(None).await.unwrap_or_default();
    Refresh {
        statuses,
        projects,
        decisions,
    }
}

/// Everything the app shows.
#[derive(Debug, Default)]
pub struct AppState {
    spaces: Vec<Space>,
    queue: Vec<Queued>,
    feed: Vec<SpaceFeedItem>,
    /// `(space, seq)` of every feed item taken, so a replay is shown once.
    seen: HashSet<(String, i64)>,
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
        self.spaces
            .iter()
            .find(|known| known.name == space)?
            .projects
            .iter()
            .find(|project| project.id == id)
            .map(|project| project.name.as_str())
    }

    /// The feed of one project in one space, oldest first.
    pub fn conversation<'a>(
        &'a self,
        space: &'a str,
        project: &'a str,
    ) -> impl Iterator<Item = &'a SpaceFeedItem> {
        self.feed.iter().filter(move |item| {
            item.space == space && item.item.project_name.as_deref() == Some(project)
        })
    }

    /// Takes what a refresh learned. A hub that could not be reached, or
    /// could not list something, keeps what it last reported.
    pub fn apply(&mut self, refresh: Refresh) {
        let Refresh {
            statuses,
            projects,
            decisions,
        } = refresh;
        let mut spaces = Vec::with_capacity(statuses.len());
        for status in statuses {
            let mut space = self
                .spaces
                .iter()
                .find(|known| known.name == status.name)
                .cloned()
                .unwrap_or_else(|| Space {
                    name: status.name.clone(),
                    unreachable: None,
                    projects: Vec::new(),
                });
            space.unreachable = status.info.err().map(|error| error.to_string());
            if let Some((_, Ok(listed))) = projects.iter().find(|(name, _)| *name == space.name) {
                space.projects.clone_from(listed);
            }
            spaces.push(space);
        }
        self.spaces = spaces;

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

    /// Takes an answer its hub accepted: the decision no longer waits, even
    /// when an earlier answer was the one that counted.
    pub fn answered(&mut self, answer: &SpaceAnswer) {
        self.queue.retain(|queued| {
            queued.space != answer.space || queued.decision.id != answer.decision.id
        });
    }

    /// Takes one item from the feed. Returns what it should raise a system
    /// notification for: a decision or a system notice seen for the first
    /// time. An ordinary message, or an item already taken (a feed replayed
    /// after a reconnect), raises none, and a repeat is not shown again.
    pub fn receive(&mut self, item: SpaceFeedItem) -> Option<Alert> {
        if !self.seen.insert((item.space.clone(), item.item.seq)) {
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
        self.feed.push(item);
        alert
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
