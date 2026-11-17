//! The conversation with a project's lead (Plan.md §4.5, §11.1): your
//! messages and what the hub delivered, in time order with day marks.

use threadlane_client::SpaceFeedItem;
use threadlane_core::client::{DigestResult, LeadRun};
use threadlane_core::ids::RunId;
use threadlane_core::model::{Mode, OutboxKind, Run, RunOutcome, RunStatus};
use threadlane_core::time::Timestamp;

use crate::{AppState, Card, Moment, When};

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
    /// A run interrupted by the owner.
    Stopped {
        /// When the run stopped.
        time: String,
    },
    /// A delivery that failed permanently.
    DeliveryFailure {
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
        Some(Digest {
            seq,
            toggle: format!("{verb} digest · {} results", results.len()),
            results: if open { results.clone() } else { Vec::new() },
        })
    }

    /// The conversation with the lead of `project` in `space`, oldest
    /// first, with a day mark before each day's first line.
    #[must_use]
    pub fn conversation(&self, space: &str, project: &str, when: &When) -> Vec<Line> {
        // `order` keeps what was delivered at the same time in feed order.
        let mut lines: Vec<(Timestamp, i64, Entry<'_>)> = Vec::new();
        for ((in_space, _), said) in &self.said {
            if in_space == space && said.project == project {
                lines.push((
                    said.at,
                    i64::MIN,
                    Entry::Visible(Line::You {
                        text: said.text.clone(),
                        time: when.time(said.at),
                    }),
                ));
            }
        }
        for ((in_space, _), failure) in &self.failures {
            if in_space == space && failure.project == project {
                lines.push((
                    failure.at,
                    i64::MAX,
                    Entry::Visible(Line::DeliveryFailure {
                        text: failure.text.clone(),
                        time: when.time(failure.at),
                    }),
                ));
            }
        }
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
                        text: item.text(),
                        time,
                    },
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
            lines.push((at, item.item.seq, Entry::Visible(line)));
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
        while let Some((at, _, entry)) = lines.next() {
            let day = when.day(at);
            let line = match entry {
                Entry::Visible(line) => line,
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
                            time: times[0].clone(),
                            text: reason(&first.run).to_owned(),
                        });
                    }
                    while let Some((at, _, Entry::Run(next))) = lines.next_if(|(at, _, entry)| {
                        matches!(entry, Entry::Run(run) if !run.sent_message && run.run.status != RunStatus::Stopped)
                            && when.day(*at) == day
                    }) {
                        let time = when.time(at);
                        if expanded {
                            reasons.push(Moment {
                                time: time.clone(),
                                text: reason(&next.run).to_owned(),
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
