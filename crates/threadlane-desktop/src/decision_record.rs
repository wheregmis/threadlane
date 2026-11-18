//! Shared decision presentation and recovery of answers from conversation traces.

use std::collections::{BTreeMap, HashSet};

use threadlane_client::ClientCore;
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

/// Everything the shared card needs, for any origin or lifecycle state.
#[derive(Debug, PartialEq, Eq)]
pub struct DecisionDisplay {
    /// The card's space and decision ID.
    pub key: DecisionKey,
    /// The title asked by the agent.
    pub question: String,
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
        DecisionDisplay {
            key,
            question: decision.question.clone(),
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
        Some(DecisionDisplay {
            key,
            question: card.question.clone(),
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
