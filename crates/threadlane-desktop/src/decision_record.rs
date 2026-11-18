//! Shared decision presentation and recovery of answers from conversation traces.

use std::collections::{BTreeMap, HashSet};

use threadlane_client::{ClientCore, SpaceFeedItem};
use threadlane_core::client::{ConversationEntry, OperationsReply, OperationsRequest};
use threadlane_core::ids::DecisionId;
use threadlane_core::model::{Decision, DecisionKind, DecisionStatus, OutboxKind};
use threadlane_core::time::Timestamp;

use crate::{AppState, Card, DecisionCard, DecisionKey, Queued, When, decision_label};

/// What is shown on a closed decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRecord {
    /// Status, with the local time of the answer when supplied by the hub.
    pub status: String,
    /// Index of the option that was recorded.
    pub chosen: Option<usize>,
    /// Instructions included in the answer.
    pub note: Option<String>,
}

impl DecisionRecord {
    fn of(decision: &Decision, when: &When) -> Self {
        let answer = decision.answer.as_ref();
        let status = match decision.status {
            DecisionStatus::Answered => "Answered",
            DecisionStatus::Expired => "Expired",
            DecisionStatus::Cancelled => "Cancelled",
            DecisionStatus::Pending => "Decision closed",
        };
        let time = answer
            .and_then(|answer| answer["answered_at"].as_i64())
            .map(Timestamp::from_millis);
        Self {
            status: time.map_or_else(
                || status.to_owned(),
                |at| format!("{status} · {}", when.time(at)),
            ),
            chosen: answer
                .and_then(|answer| usize::try_from(answer["option"].as_u64()?).ok())
                .filter(|&index| index < decision.options.len()),
            note: answer
                .and_then(|answer| answer["note"].as_str())
                .filter(|note| !note.trim().is_empty())
                .map(str::to_owned),
        }
    }
}

/// Longer than this, or on more than one line, a question is split.
const SHORT_QUESTION: usize = 140;

/// A decision's question as the card's title and, when it is long, the rest
/// as a Markdown body: the title is its first line, or that line's first
/// sentence when the line itself is long.
#[must_use]
pub fn question_parts(question: &str) -> (String, Option<String>) {
    let question = question.trim();
    if !question.contains('\n') && question.chars().count() <= SHORT_QUESTION {
        return (question.to_owned(), None);
    }
    let (first, rest) = question.split_once('\n').unwrap_or((question, ""));
    let sentence = (first.chars().count() > SHORT_QUESTION)
        .then(|| {
            first
                .match_indices(['.', '?', '!'])
                .map(|(at, _)| at + 1)
                .find(|&end| first[end..].starts_with(' '))
        })
        .flatten();
    let (title, head) = first.split_at(sentence.unwrap_or(first.len()));
    let body = format!("{}\n{rest}", head.trim()).trim().to_owned();
    (title.trim().to_owned(), (!body.is_empty()).then_some(body))
}

/// Everything the shared card needs, for any origin or lifecycle state.
#[derive(Debug, PartialEq, Eq)]
pub struct DecisionDisplay {
    /// The card's space and decision ID.
    pub key: DecisionKey,
    /// The title: the whole question, or its first line or sentence when it is long.
    pub question: String,
    /// The rest of a long question, as Markdown.
    pub body: Option<String>,
    /// Full option labels, in hub order.
    pub options: Vec<String>,
    /// Space, project, agent, and task metadata.
    pub origin: String,
    /// When the decision was asked.
    pub asked: String,
    /// Context, call details, and the treatment for this decision kind.
    pub contents: DecisionCard,
    /// The closed decision's record, when it is no longer pending.
    pub record: Option<DecisionRecord>,
    /// Whether single-key commands target this decision.
    pub current: bool,
    /// Whether the note editor is visible.
    pub show_note: bool,
    /// The standing-rule checkbox's label and tick, when offered.
    pub always: Option<(String, bool)>,
    /// Whether an answer is in flight.
    pub answering: bool,
}

impl AppState {
    /// Presents a pending or historical decision with the same card content.
    #[must_use]
    pub fn present_decision(&self, queued: &Queued, project: &str, when: &When) -> DecisionDisplay {
        let decision = &queued.decision;
        let key = DecisionKey::from(queued);
        let pending = self.queue.iter().any(|known| key.matches(known));
        let record = (!pending)
            .then(|| DecisionRecord::of(self.decision_records.get(&key).unwrap_or(decision), when));
        let task = decision
            .task_id
            .map_or_else(String::new, |task| format!(" · {task}"));
        let agent = self.asker(queued);
        let (question, body) = question_parts(&decision.question);
        DecisionDisplay {
            key,
            question,
            body,
            options: decision
                .options
                .iter()
                .map(|option| option.label.clone())
                .collect(),
            origin: format!("{} / {project} · {agent}{task}", queued.space),
            asked: format!("Asked {} ago", when.since(decision.created_at)),
            contents: Self::decision_card(queued),
            record,
            current: self.current_decision().is_some_and(|current| {
                current.space == queued.space && current.decision.id == decision.id
            }),
            show_note: pending && self.shows_decision_note(queued),
            always: pending.then(|| self.always_offer(queued)).flatten(),
            answering: self.is_answering(&DecisionKey::from(queued)),
        }
    }

    /// Resolves a conversation card using the queue or recorded hub details.
    /// Older feed items can still show the question and its options.
    #[must_use]
    pub fn present_feed_decision(
        &self,
        space: &str,
        project: &str,
        card: &Card,
        time: &str,
        when: &When,
    ) -> Option<DecisionDisplay> {
        if let Some(queued) = self.queued_card(space, card) {
            return Some(self.present_decision(queued, project, when));
        }
        let key = DecisionKey {
            space: space.to_owned(),
            id: DecisionId::parse(&card.id)?,
        };
        if let Some(decision) = self.decision_records.get(&key) {
            return Some(self.present_decision(
                &Queued {
                    space: space.to_owned(),
                    decision: decision.clone(),
                },
                project,
                when,
            ));
        }
        let dashed = card.kind == Some(DecisionKind::Proposal);
        let (question, body) = question_parts(&card.question);
        Some(DecisionDisplay {
            key,
            question,
            body,
            options: card.options.clone(),
            origin: format!("{space} / {project}"),
            asked: format!("Asked {time}"),
            contents: DecisionCard {
                dashed,
                chip: card
                    .kind
                    .filter(|kind| *kind != DecisionKind::Approval)
                    .map(decision_label),
                context: card.context.clone(),
                details: dashed
                    .then(|| "You did not ask for this.".to_owned())
                    .into_iter()
                    .collect(),
                would_run_as: None,
                note_label: "Note (optional)",
                recommended: card.recommended,
            },
            record: Some(DecisionRecord {
                status: "Answer details unavailable".to_owned(),
                chosen: None,
                note: None,
            }),
            current: false,
            show_note: false,
            always: None,
            answering: false,
        })
    }

    pub(crate) fn remember_decisions(
        &mut self,
        space: &str,
        decisions: impl IntoIterator<Item = Decision>,
    ) {
        if self.forgotten.contains(space) {
            return;
        }
        for decision in decisions {
            if decision.status == DecisionStatus::Pending {
                continue;
            }
            let key = DecisionKey {
                space: space.to_owned(),
                id: decision.id,
            };
            self.advance_answered_decision(&key);
            self.queue
                .retain(|queued| queued.space != space || queued.decision.id != key.id);
            self.always.remove(&(space.to_owned(), key.id));
            self.decision_notes.remove(&key);
            self.decision_records.insert(key, decision);
        }
    }
}

pub(crate) async fn load_records(
    core: &ClientCore,
    space: &str,
    history: &[ConversationEntry],
) -> Vec<Decision> {
    let mut by_trace = BTreeMap::<&str, HashSet<DecisionId>>::new();
    for entry in history {
        if let ConversationEntry::Delivered { item } = entry
            && item.message.kind == OutboxKind::Decision
            && let (Some(trace), Some(id)) =
                (item.message.trace_id.as_deref(), item.message.decision_id)
        {
            by_trace.entry(trace).or_default().insert(id);
        }
    }
    let mut records = Vec::new();
    for (trace, ids) in by_trace {
        if let Ok(OperationsReply::Trace { trace }) = core
            .operations(
                Some(space),
                OperationsRequest::Trace {
                    id: trace.to_owned(),
                },
            )
            .await
        {
            records.extend(
                trace
                    .decisions
                    .into_iter()
                    .filter(|decision| ids.contains(&decision.id)),
            );
        }
    }
    records
}

/// A referenced decision located in conversation history.
#[derive(Debug)]
pub struct DecisionLink {
    project: String,
    item: SpaceFeedItem,
    records: Vec<Decision>,
}

/// Looks up a decision whose card is outside the desktop's loaded history.
///
/// # Errors
///
/// Returns the first project history error if no available project contains the decision.
pub async fn load_decision_link(
    core: &ClientCore,
    space: &str,
    projects: Vec<String>,
    id: DecisionId,
) -> Result<Option<DecisionLink>, threadlane_client::ClientError> {
    let label = id.to_string();
    let mut failure = None;
    for project in projects {
        let mut before = None;
        loop {
            let page = match core
                .conversation(Some(space), Some(&project), before, crate::PAGE)
                .await
            {
                Ok(page) => page,
                Err(error) => {
                    failure.get_or_insert(error);
                    break;
                }
            };
            for entry in &page {
                if let ConversationEntry::Delivered { item } = entry
                    && item.message.kind == OutboxKind::Decision
                {
                    let mut item = SpaceFeedItem {
                        space: space.to_owned(),
                        item: item.clone(),
                    };
                    if Card::of(&item).is_some_and(|card| card.id == label) {
                        item.item
                            .project_name
                            .get_or_insert_with(|| project.clone());
                        let records = load_records(core, space, std::slice::from_ref(entry)).await;
                        return Ok(Some(DecisionLink {
                            project,
                            item,
                            records,
                        }));
                    }
                }
            }
            if page.len() < crate::PAGE as usize {
                break;
            }
            let oldest = page.iter().filter_map(conversation_time).min();
            if oldest.is_none() || oldest == before {
                break;
            }
            before = oldest;
        }
    }
    failure.map_or(Ok(None), Err)
}

impl AppState {
    /// Retains a linked decision and returns the project to open.
    pub fn apply_decision_link(&mut self, link: DecisionLink) -> String {
        self.remember_decisions(&link.item.space, link.records);
        self.deliver(link.item);
        link.project
    }
}

pub(crate) fn conversation_time(entry: &ConversationEntry) -> Option<Timestamp> {
    match entry {
        ConversationEntry::You { at, .. }
        | ConversationEntry::SpaceLead { at, .. }
        | ConversationEntry::ProjectReply { at, .. }
        | ConversationEntry::DeliveryFailure { at, .. } => Some(*at),
        ConversationEntry::Delivered { item } => Some(item.delivered_at),
        _ => None,
    }
}
