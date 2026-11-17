//! The conversation with a project's lead (Plan.md §4.5, §11.1): your
//! messages and what the hub delivered, in time order with day marks.

use threadlane_core::model::OutboxKind;
use threadlane_core::time::Timestamp;

use crate::{AppState, Card, When};

/// One line of a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// The first line of a day: `Today`, `Yesterday`, or a date.
    Day(String),
    /// A message you sent.
    You {
        /// What you wrote.
        text: String,
        /// When, as `14:20`.
        time: String,
    },
    /// A message from the lead.
    Lead {
        /// What it said.
        text: String,
        /// When it was delivered.
        time: String,
    },
    /// A system notice.
    Notice {
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
}

impl AppState {
    /// The conversation with the lead of `project` in `space`, oldest
    /// first, with a day mark before each day's first line.
    #[must_use]
    pub fn conversation(&self, space: &str, project: &str, when: &When) -> Vec<Line> {
        // `order` keeps what was delivered at the same time in feed order.
        let mut lines: Vec<(Timestamp, i64, Line)> = Vec::new();
        for ((in_space, _), said) in &self.said {
            if in_space == space && said.project == project {
                lines.push((
                    said.at,
                    i64::MIN,
                    Line::You {
                        text: said.text.clone(),
                        time: when.time(said.at),
                    },
                ));
            }
        }
        for item in self.delivered.values() {
            if item.space != space || item.item.project_name.as_deref() != Some(project) {
                continue;
            }
            let at = item.item.delivered_at;
            let time = when.time(at);
            let line = match item.kind() {
                OutboxKind::Message => Line::Lead {
                    text: item.text(),
                    time,
                },
                OutboxKind::SystemNotice => Line::Notice {
                    text: item.text(),
                    time,
                },
                OutboxKind::Decision => match Card::of(item) {
                    Some(card) => Line::Decision {
                        pending: self.is_pending(space, &card.id),
                        card,
                        time,
                    },
                    None => continue,
                },
            };
            lines.push((at, item.item.seq, line));
        }
        lines.sort_by_key(|(at, order, _)| (*at, *order));

        let mut shown = Vec::with_capacity(lines.len());
        let mut today = None;
        for (at, _, line) in lines {
            let day = when.day(at);
            if today.as_ref() != Some(&day) {
                shown.push(Line::Day(day.clone()));
                today = Some(day);
            }
            shown.push(line);
        }
        shown
    }
}
