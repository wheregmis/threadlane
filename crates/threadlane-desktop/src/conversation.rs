//! The conversation with a project's lead (Plan.md §4.5, §11.1): your
//! messages and what the hub delivered, in time order with day marks.

use threadlane_client::SpaceFeedItem;
use threadlane_core::attachment::ImageAttachment;
use threadlane_core::client::{DigestResult, LeadRun};
use threadlane_core::ids::RunId;
use threadlane_core::model::{Mode, OutboxKind, Run, RunOutcome, RunStatus};
use threadlane_core::time::Timestamp;

use crate::{
    AppState, Author, Card, DecisionScope, Destination, Moment, ProseLimit, When, count_form,
};

/// One line of a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// The first line of a day: `Today`, `Yesterday`, or a date.
    Day(String),
    /// The first delivery since the previous visit.
    New,
    /// A local submission awaiting confirmation.
    Sending {
        /// Image attachments.
        images: Vec<ImageAttachment>,
        /// Local message identity.
        id: u64,
        /// What you wrote.
        text: String,
        /// When you submitted it.
        time: String,
    },
    /// A message you sent.
    You {
        /// Image attachments.
        images: Vec<ImageAttachment>,
        /// Stable owner event identity.
        id: threadlane_core::ids::EventId,
        /// What you wrote.
        text: String,
        /// When, as `14:20`.
        time: String,
    },
    /// An ask from the space lead, General's lead (docs/one-place-to-talk.md).
    SpaceLead {
        /// Stable event identity.
        id: threadlane_core::ids::EventId,
        /// What it asked.
        text: String,
        /// When it asked.
        time: String,
    },
    /// In General: a project lead's reply to the space lead, which links to
    /// that project's conversation.
    ProjectReply {
        /// Stable event identity.
        id: threadlane_core::ids::EventId,
        /// The project whose lead replied.
        project: String,
        /// The reply.
        text: String,
        /// When it arrived.
        time: String,
    },
    /// A message from the lead.
    Lead {
        /// Feed sequence within this space.
        seq: i64,
        /// What it said.
        text: String,
        /// When it was delivered.
        time: String,
    },
    /// A run interrupted by the owner.
    Stopped {
        /// When the run stopped.
        time: String,
    },
    /// A delivery that failed permanently.
    DeliveryFailure {
        /// Stable event identity.
        id: threadlane_core::ids::EventId,
        /// What failed.
        text: String,
        /// When it failed.
        time: String,
    },
    /// A delivery containing several project results.
    Digest {
        /// The disclosure and its contents when expanded.
        digest: Digest,
        /// When the digest was delivered.
        time: String,
    },
    /// A system notice.
    Notice {
        /// Feed sequence within this space.
        seq: i64,
        /// What it said.
        text: String,
        /// When it was delivered.
        time: String,
    },
    /// A decision card, from any agent in the project (Plan.md §9.4).
    Decision {
        /// The card.
        card: Card,
        /// When it was delivered.
        time: String,
        /// Whether it still waits on you.
        pending: bool,
    },
    /// Consecutive lead runs that finished without an ordinary message.
    Quiet {
        /// The first run's ID, for the disclosure button.
        id: RunId,
        /// The collapsed line, with the runs' times.
        text: String,
        /// The disclosure button's label.
        toggle: &'static str,
        /// Each run's reason while expanded.
        reasons: Vec<Moment>,
    },
}

/// A digest as shown in Home and the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest {
    /// Position of the digest in this space's feed.
    pub seq: i64,
    /// The disclosure button's label.
    pub toggle: String,
    /// The original results when expanded.
    pub results: Vec<DigestResult>,
}

enum Entry<'a> {
    Visible(Line),
    Delivered(Line),
    Run(&'a LeadRun),
}

fn reason(run: &Run) -> &str {
    run.rationale
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .or(run.error.as_deref())
        .unwrap_or(match (run.status, run.outcome) {
            (_, Some(RunOutcome::Deferred)) => "Stopped to ask you.",
            (_, Some(RunOutcome::Silent)) => "The run ended silently.",
            (RunStatus::Failed, _) => "The run failed.",
            (RunStatus::Abandoned, _) => "The run was abandoned.",
            _ => "The run ended without a message.",
        })
}

/// Whether a feed item belongs in this project's conversation.
#[must_use]
pub fn belongs_in_conversation(item: &SpaceFeedItem, space: &str, project: &str) -> bool {
    item.space == space
        && (item.item.project_name.as_deref() == Some(project)
            || item
                .item
                .digest
                .iter()
                .any(|result| result.project.as_deref() == Some(project)))
}

impl AppState {
    /// The lead's configured mode under the project's effective ceiling.
    #[must_use]
    pub fn lead_mode(&self, space: &str, project: &str) -> Option<Mode> {
        let binding = self
            .project(space, project)?
            .bindings
            .iter()
            .find(|binding| binding.is_lead)?;
        Some(binding.mode.stricter(self.project_ceiling(space, project)))
    }

    /// The digest disclosure for a delivered item, if it has original results.
    #[must_use]
    pub fn digest(&self, item: &SpaceFeedItem) -> Option<Digest> {
        let results = &item.item.digest;
        if results.is_empty() {
            return None;
        }
        let seq = item.item.seq;
        let open = self.expanded.contains(&(item.space.clone(), seq));
        let verb = if open { "Hide" } else { "Show" };
        let count = results.len();
        let noun = count_form(count, "result", "results");
        Some(Digest {
            seq,
            toggle: format!("{verb} digest · {count} {noun}"),
            results: if open { results.clone() } else { Vec::new() },
        })
    }

    /// The conversation with the lead of `project` in `space`, oldest
    /// first, with a day mark before each day's first line.
    #[must_use]
    pub fn conversation(&self, space: &str, project: &str, when: &When) -> Vec<Line> {
        // `order` keeps what was delivered at the same time in feed order.
        let mut lines: Vec<(Timestamp, i64, Entry<'_>)> = Vec::new();
        let destination = Destination::Lead(space.to_owned(), project.to_owned());
        for ((in_space, id), said) in &self.said {
            if in_space == space
                && said.project == project
                && !(said.by == Author::You
                    && self.has_optimistic_copy(&destination, &said.text, &said.images, said.at))
            {
                lines.push((
                    said.at,
                    i64::MIN,
                    Entry::Visible(match &said.by {
                        Author::You => Line::You {
                            images: said.images.clone(),
                            id: *id,
                            text: said.text.clone(),
                            time: when.time(said.at),
                        },
                        Author::SpaceLead => Line::SpaceLead {
                            id: *id,
                            text: said.text.clone(),
                            time: when.time(said.at),
                        },
                        Author::ProjectLead(from) => Line::ProjectReply {
                            id: *id,
                            project: from.clone(),
                            text: said.text.clone(),
                            time: when.time(said.at),
                        },
                    }),
                ));
            }
        }
        for ((in_space, id), failure) in &self.failures {
            if in_space == space && failure.project == project {
                lines.push((
                    failure.at,
                    i64::MAX,
                    Entry::Visible(Line::DeliveryFailure {
                        id: *id,
                        text: failure.text.clone(),
                        time: when.time(failure.at),
                    }),
                ));
            }
        }
        for send in self.outgoing() {
            if send.destination == destination {
                lines.push((
                    send.at,
                    i64::MAX,
                    Entry::Visible(Line::Sending {
                        images: send.images.clone(),
                        id: send.id,
                        text: send.text.clone(),
                        time: when.time(send.at),
                    }),
                ));
            }
        }
        let mut delivered_decisions = std::collections::HashSet::new();
        for item in self.delivered.values() {
            if !belongs_in_conversation(item, space, project) {
                continue;
            }
            let at = item.item.delivered_at;
            let time = when.time(at);
            let line = match item.kind() {
                OutboxKind::Message => match self.digest(item) {
                    Some(digest) => Line::Digest { digest, time },
                    None => Line::Lead {
                        seq: item.item.seq,
                        text: item.text(),
                        time,
                    },
                },
                OutboxKind::SystemNotice => Line::Notice {
                    seq: item.item.seq,
                    text: item.text(),
                    time,
                },
                OutboxKind::Decision => match Card::of(item) {
                    Some(card) => {
                        if !delivered_decisions.insert(card.id.clone()) {
                            continue;
                        }
                        Line::Decision {
                            pending: self.is_pending(space, &card.id),
                            card,
                            time,
                        }
                    }
                    None => continue,
                },
            };
            lines.push((at, item.item.seq, Entry::Delivered(line)));
        }
        let scope = DecisionScope::Project(space.to_owned(), project.to_owned());
        let pending = self
            .decisions_in(&scope)
            .map(|queued| (&queued.decision, true));
        let remembered = self
            .decision_records
            .iter()
            .filter(|(key, decision)| {
                key.space == space && self.project_name(space, decision.project_id) == Some(project)
            })
            .map(|(_, decision)| (decision, false));
        for (decision, pending) in pending.chain(remembered) {
            let id = decision.id.to_string();
            if delivered_decisions.insert(id.clone()) {
                let at = decision.created_at;
                lines.push((
                    at,
                    decision.id.get(),
                    Entry::Visible(Line::Decision {
                        card: Card {
                            id,
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
                        time: when.time(at),
                        pending,
                    }),
                ));
            }
        }
        if let Some(view) = self.project(space, project) {
            let before = self.earlier(space, project);
            lines.extend(view.lead_runs.iter().filter_map(|run| {
                let at = run.run.ended_at?;
                // Do not group quiet runs across a part of the conversation we have not loaded.
                if before.is_some_and(|before| at < before) {
                    return None;
                }
                Some((at, i64::MAX, Entry::Run(run)))
            }));
        }
        lines.sort_by_key(|(at, order, _)| (*at, *order));

        let mut shown = Vec::with_capacity(lines.len());
        let mut today = None;
        let mut lines = lines.into_iter().peekable();
        let unread = self
            .conversation_read
            .get(&(space.to_owned(), project.to_owned()))
            .and_then(|read| read.new_after);
        let mut marked = false;
        while let Some((at, seq, entry)) = lines.next() {
            let first_new = !marked
                && unread.is_some_and(|after| seq > after)
                && matches!(entry, Entry::Delivered(_));
            let day = when.day(at);
            let line = match entry {
                Entry::Visible(line) | Entry::Delivered(line) => line,
                Entry::Run(run) if run.run.status == RunStatus::Stopped => Line::Stopped {
                    time: when.time(at),
                },
                Entry::Run(first) if !first.sent_message => {
                    let id = first.run.id;
                    let expanded = self.expanded_quiet.contains(&(space.to_owned(), id));
                    let mut times = vec![when.time(at)];
                    let mut reasons = Vec::new();
                    if expanded {
                        reasons.push(Moment {
                            id: id.to_string(),
                            time: times[0].clone(),
                            text: reason(&first.run).to_owned(),
                            message: None,
                        });
                    }
                    while let Some((at, _, Entry::Run(next))) = lines.next_if(|(at, _, entry)| {
                        matches!(entry, Entry::Run(run) if !run.sent_message && run.run.status != RunStatus::Stopped)
                            && when.day(*at) == day
                    }) {
                        let time = when.time(at);
                        if expanded {
                            reasons.push(Moment {
                                id: next.run.id.to_string(),
                                time: time.clone(),
                                text: reason(&next.run).to_owned(),
                                message: None,
                            });
                        }
                        times.push(time);
                    }
                    let runs = match times.len() {
                        1 => "Lead ran and stayed quiet".to_owned(),
                        2 => "Lead ran twice and stayed quiet".to_owned(),
                        count => format!("Lead ran {count} times and stayed quiet"),
                    };
                    Line::Quiet {
                        id,
                        text: format!("{runs} · {}", times.join(", ")),
                        toggle: if expanded { "Hide" } else { "Why" },
                        reasons,
                    }
                }
                Entry::Run(_) => continue,
            };
            if today.as_ref() != Some(&day) {
                shown.push(Line::Day(day.clone()));
                today = Some(day);
            }
            if first_new {
                shown.push(Line::New);
                marked = true;
            }
            shown.push(line);
        }
        shown
    }

    /// Opens or closes the reasons for the quiet runs starting with `id`.
    pub fn toggle_quiet(&mut self, space: &str, id: RunId) {
        let key = (space.to_owned(), id);
        if !self.expanded_quiet.remove(&key) {
            self.expanded_quiet.insert(key);
        }
    }
}

/// Read position for one project.
#[derive(Debug)]
pub(crate) struct ConversationRead {
    pub(crate) seen: i64,
    new_after: Option<i64>,
}

impl ConversationRead {
    pub(crate) fn saved(seen: i64) -> Self {
        Self {
            seen,
            new_after: None,
        }
    }
}

impl AppState {
    /// The newest delivery belonging to a conversation.
    #[must_use]
    pub fn conversation_latest(&self, space: &str, project: &str) -> i64 {
        self.delivered
            .values()
            .rev()
            .find(|item| belongs_in_conversation(item, space, project))
            .map_or(0, |item| item.item.seq)
    }

    /// Sets the New divider baseline when entering a conversation.
    pub fn visit_conversation(&mut self, space: &str, project: &str) {
        use std::collections::hash_map::Entry;
        match self
            .conversation_read
            .entry((space.to_owned(), project.to_owned()))
        {
            Entry::Occupied(mut entry) => {
                let read = entry.get_mut();
                read.new_after = Some(read.seen);
            }
            Entry::Vacant(entry) => {
                entry.insert(ConversationRead::saved(0));
            }
        }
    }

    /// Records a new live arrival before advancing the seen position.
    pub fn conversation_arrived(&mut self, space: &str, project: &str, previous: i64) {
        if let Some(read) = self
            .conversation_read
            .get_mut(&(space.to_owned(), project.to_owned()))
        {
            read.new_after.get_or_insert(previous);
        }
    }

    /// Records that the newest message is in view, leaving the divider for this visit.
    pub fn saw_conversation_latest(&mut self, space: &str, project: &str) {
        let latest = self.conversation_latest(space, project);
        if let Some(read) = self
            .conversation_read
            .get_mut(&(space.to_owned(), project.to_owned()))
            && latest > read.seen
        {
            read.seen = latest;
            self.save_conversation_seen();
        }
    }

    /// Whether arrivals warrant a jump control while the viewport is away from the bottom.
    #[must_use]
    pub fn conversation_has_newer(&self, space: &str, project: &str, at_bottom: bool) -> bool {
        !at_bottom
            && self
                .conversation_read
                .get(&(space.to_owned(), project.to_owned()))
                .is_some_and(|read| self.conversation_latest(space, project) > read.seen)
    }

    /// The known decision in the queue, project history, or a loaded task.
    #[must_use]
    pub fn linked_decision(
        &self,
        space: &str,
        id: threadlane_core::ids::DecisionId,
    ) -> Option<&threadlane_core::model::Decision> {
        let key = crate::DecisionKey {
            space: space.to_owned(),
            id,
        };
        self.queue
            .iter()
            .find(|item| item.space == space && item.decision.id == id)
            .map(|item| &item.decision)
            .or_else(|| self.decision_records.get(&key))
            .or_else(|| {
                self.tasks.iter().find_map(|((name, _), task)| {
                    if name == space {
                        task.decisions.iter().find(|decision| decision.id == id)
                    } else {
                        None
                    }
                })
            })
    }

    /// A decision's project in the given space, from the queue, task, or history.
    #[must_use]
    pub fn reference_project(
        &self,
        space: &str,
        id: threadlane_core::ids::DecisionId,
    ) -> Option<&str> {
        let label = id.to_string();
        self.linked_decision(space, id)
            .and_then(|decision| self.project_name(space, decision.project_id))
            .or_else(|| {
                self.delivered.values().find_map(|item| {
                    (item.space == space && Card::of(item).is_some_and(|card| card.id == label))
                        .then_some(item.item.project_name.as_deref())
                        .flatten()
                })
            })
    }
}

/// Whether the viewport should follow growing content or an explicit jump.
#[must_use]
pub fn follow_latest(requested: bool, at_bottom: bool, grew: bool) -> bool {
    requested || (at_bottom && grew)
}

/// Finds the newest message, which should remain expanded at any length.
#[must_use]
pub fn newest_message(lines: &[Line]) -> Option<usize> {
    lines.iter().rposition(|line| {
        matches!(
            line,
            Line::You { .. }
                | Line::SpaceLead { .. }
                | Line::ProjectReply { .. }
                | Line::Lead { .. }
                | Line::Sending { .. }
                | Line::Digest { .. }
        )
    })
}

/// Older message bodies can be collapsed after layout.
#[must_use]
pub fn message_limit(newest: Option<usize>, index: usize) -> ProseLimit {
    if newest.is_some_and(|latest| index < latest) {
        ProseLimit::Message
    } else {
        ProseLimit::Full
    }
}
