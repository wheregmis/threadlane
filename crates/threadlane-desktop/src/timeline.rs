//! Home's timeline (docs/one-place-to-talk.md, stage 2): every conversation
//! in every paired space, newest first, merged from the conversation
//! requests the project screen already makes. App-only: each hub still
//! knows nothing of another.

use threadlane_client::{ClientCore, ClientError, SpaceFeedItem};
use threadlane_core::attachment::ImageAttachment;
use threadlane_core::client::ConversationEntry;
use threadlane_core::ids::EventId;
use threadlane_core::model::OutboxKind;
use threadlane_core::time::Timestamp;

use crate::{AppState, Author};

/// How many entries the timeline shows at first, and adds per "Show earlier".
pub const TIMELINE_PAGE: usize = 20;

/// How many of each conversation's newest entries a Home load asks for.
const NEWEST: u32 = crate::PAGE;

/// Whether a delivered lead message is an internal no-op reflection that
/// shouldn't crowd the Inbox by default (still reachable via "Show quiet updates"
/// and in the project's Chat tab).
#[must_use]
pub fn is_quiet_update(text: &str) -> bool {
    let lower: String = text
        .trim_start()
        .chars()
        .take(48)
        .collect::<String>()
        .to_ascii_lowercase();
    lower.starts_with("i didn't send a message")
        || lower.starts_with("i didn’t send a message")
        || lower.starts_with("i didn't message the user")
        || lower.starts_with("i didn’t message the user")
        || lower.starts_with("no message sent")
}

/// What an entry of the timeline says.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Spoken<'a> {
    /// A message on the asking side: yours, the space lead's ask, or in
    /// General a project lead's reply to it.
    Said(EventId, &'a str, &'a Author),
    /// A message or notice the hub delivered.
    Delivered(&'a SpaceFeedItem),
}

/// One entry of Home's timeline: which conversation, when, and what.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineEntry<'a> {
    /// The space it belongs to.
    pub space: &'a str,
    /// The project whose conversation it opens; `None` for a notice to the
    /// space as a whole.
    pub project: Option<&'a str>,
    /// When it was said or delivered.
    pub at: Timestamp,
    /// What it says.
    pub spoken: Spoken<'a>,
    /// Inline images of an owner message.
    pub images: &'a [ImageAttachment],
}

impl TimelineEntry<'_> {
    /// Stable identity for this entry across renders, prose caching, and disclosure.
    #[must_use]
    pub fn prose_id(&self) -> String {
        match self.spoken {
            Spoken::Said(id, _, _) => format!("timeline-said-{}-{id}", self.space),
            Spoken::Delivered(item) => format!("result-{}-{}", self.space, item.item.seq),
        }
    }

    /// The text, for a delivered item its readable body.
    #[must_use]
    pub fn text(&self) -> String {
        match self.spoken {
            Spoken::Said(_, text, _) => text.to_owned(),
            Spoken::Delivered(item) => item.text(),
        }
    }
}

/// The newest page of each conversation Home merges. Made by
/// [`load_timeline`], applied with [`AppState::apply_timeline`].
#[derive(Debug)]
pub struct TimelineLoad {
    pages: Vec<(String, String, Result<Vec<ConversationEntry>, ClientError>)>,
}

/// Asks for the newest entries of each `(space, project)` conversation.
pub async fn load_timeline(core: &ClientCore, projects: Vec<(String, String)>) -> TimelineLoad {
    let mut pages = Vec::with_capacity(projects.len());
    for (space, project) in projects {
        let page = core
            .conversation(Some(&space), Some(&project), None, NEWEST)
            .await;
        pages.push((space, project, page));
    }
    TimelineLoad { pages }
}

impl AppState {
    /// Takes the newest page of each conversation into the same history the
    /// project screen shows, so an entry is never shown twice.
    // shortcut: a conversation that cannot be listed keeps what was shown,
    // with no line saying so; add a gap line if hubs refuse it in practice.
    pub fn apply_timeline(&mut self, load: TimelineLoad) {
        for (space, project, page) in load.pages {
            if let Ok(entries) = page
                && !self.project_absent(&space, &project)
            {
                self.take_history(&space, &project, entries, true, NEWEST);
            }
        }
    }

    /// The conversations Home merges: every active project of the spaces it
    /// shows, as `(space, project)`.
    #[must_use]
    pub fn timeline_projects(&self) -> Vec<(String, String)> {
        self.targets()
            .into_iter()
            .filter(|(space, _)| self.shown(space))
            .map(|(space, project)| (space.to_owned(), project.to_owned()))
            .collect()
    }

    /// The moment before which some merged conversation may still hold
    /// entries not loaded, so the timeline stops there.
    fn timeline_cut(&self) -> Option<Timestamp> {
        self.timeline_projects()
            .into_iter()
            .filter_map(|key| self.loaded.get(&key))
            .filter(|loaded| loaded.more)
            .filter_map(|loaded| loaded.oldest)
            .max()
    }

    /// Every entry known down to the cut, newest first. Decisions are
    /// left to "Needs you".
    fn timeline_all(&self) -> Vec<TimelineEntry<'_>> {
        let cut = self.timeline_cut();
        let said = self.said.iter().map(|((space, id), said)| TimelineEntry {
            space,
            project: Some(&said.project),
            at: said.at,
            spoken: Spoken::Said(*id, &said.text, &said.by),
            images: &said.images,
        });
        let delivered = self
            .delivered
            .values()
            .filter(|item| item.kind() != OutboxKind::Decision)
            .map(|item| TimelineEntry {
                space: &item.space,
                project: item.item.project_name.as_deref(),
                at: item.item.delivered_at,
                spoken: Spoken::Delivered(item),
                images: &[],
            });
        let mut all: Vec<_> = said
            .chain(delivered)
            .filter(|entry| self.shown(entry.space) && cut.is_none_or(|cut| entry.at >= cut))
            .collect();
        // Stable on ties: your message before what answered it.
        all.sort_by_key(|entry| std::cmp::Reverse(entry.at));
        all
    }

    fn timeline_shown(&self) -> usize {
        TIMELINE_PAGE * (1 + self.timeline_pages)
    }

    /// Home's timeline: the newest entries of every conversation in the
    /// spaces it shows, newest first, one page more per "Show earlier".
    #[must_use]
    pub fn timeline(&self) -> Vec<TimelineEntry<'_>> {
        let mut all = self.timeline_all();
        all.truncate(self.timeline_shown());
        all
    }

    /// Whether "Show earlier" can show more: entries loaded but not shown,
    /// or a merged conversation with more on its hub.
    #[must_use]
    pub fn timeline_has_earlier(&self) -> bool {
        self.timeline_all().len() > self.timeline_shown()
            || self
                .timeline_projects()
                .iter()
                .any(|key| self.loaded.get(key).is_some_and(|loaded| loaded.more))
    }

    /// Shows one page more, and returns the conversations to load an
    /// earlier page of when what is loaded does not fill it: `(space,
    /// project, before)`, for [`crate::load_earlier`].
    pub fn show_earlier_timeline(&mut self) -> Vec<(String, String, Timestamp)> {
        self.timeline_pages += 1;
        if self.timeline_all().len() >= self.timeline_shown() {
            return Vec::new();
        }
        self.timeline_projects()
            .into_iter()
            .filter_map(|(space, project)| {
                let before = self.earlier(&space, &project)?;
                Some((space, project, before))
            })
            .collect()
    }
}
