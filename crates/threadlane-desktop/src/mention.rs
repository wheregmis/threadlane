//! `@agent` and `#task` mentions in a composer draft: what the popover
//! offers, what a pick inserts, and the tasks a mention dispatches.

use std::ops::Range;

use threadlane_client::NewTaskSpec;
use threadlane_core::ids::TaskId;

use crate::{AppState, Destination, Line, When};

/// How many rows the popover lists at most.
pub const MENTION_LIMIT: usize = 8;

/// What the token under the cursor asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MentionKind {
    /// `@` names an agent.
    Agent,
    /// `#` names a task.
    Task,
}

/// The `@…` or `#…` token being typed, ending at the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionQuery {
    /// Which picker it opens.
    pub kind: MentionKind,
    /// The bytes of the token, trigger included.
    pub range: Range<usize>,
    /// What follows the trigger.
    pub query: String,
}

/// The token being typed at byte `cursor` of `text`, if it is a mention:
/// `@` or `#` at the start or after whitespace, then no whitespace.
#[must_use]
pub fn mention_at(text: &str, cursor: usize) -> Option<MentionQuery> {
    let before = text.get(..cursor)?;
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(at, c)| at + c.len_utf8());
    let token = &before[start..];
    let kind = match token.chars().next()? {
        '@' => MentionKind::Agent,
        '#' => MentionKind::Task,
        _ => return None,
    };
    let query = &token[1..];
    if query.contains(['@', '#']) {
        return None;
    }
    Some(MentionQuery {
        kind,
        range: start..cursor,
        query: query.to_owned(),
    })
}

/// `text` with the mention replaced by `insert` and a space, and the cursor
/// after it.
#[must_use]
pub fn complete_mention(text: &str, mention: &MentionQuery, insert: &str) -> (String, usize) {
    let rest = &text[mention.range.end..];
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    let completed = format!("{}{insert} {rest}", &text[..mention.range.start]);
    (completed, mention.range.start + insert.len() + 1)
}

/// The moved popover selection: wraps at both ends, 0 with no rows.
#[must_use]
pub fn step_selection(selected: usize, len: usize, down: bool) -> usize {
    match (len, down) {
        (0, _) => 0,
        (len, true) => (selected + 1) % len,
        (len, false) => (selected + len - 1) % len,
    }
}

/// How well `query` matches `candidate`, lower is better: a prefix, then a
/// substring, then the letters in order. `None` when it does not match.
#[must_use]
pub fn fuzzy_rank(query: &str, candidate: &str) -> Option<usize> {
    let (query, candidate) = (query.to_lowercase(), candidate.to_lowercase());
    if candidate.starts_with(&query) {
        return Some(0);
    }
    if let Some(at) = candidate.find(&query) {
        return Some(1 + at);
    }
    let mut letters = candidate.chars();
    query
        .chars()
        .all(|wanted| letters.any(|c| c == wanted))
        .then_some(1000 + candidate.len())
}

/// The `#T-0196` references in `text`, in order and without repeats.
#[must_use]
pub fn task_refs(text: &str) -> Vec<TaskId> {
    let mut refs = Vec::new();
    for word in text.split_whitespace() {
        let word = word.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
        if let Some(id) = word.strip_prefix('#').and_then(TaskId::parse)
            && !refs.contains(&id)
        {
            refs.push(id);
        }
    }
    refs
}

/// One agent the `@` popover offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionAgent {
    /// Its name, as `@Name` inserts it.
    pub name: String,
    /// Its role in the project.
    pub role: String,
    /// Whether one of its tasks is running.
    pub busy: bool,
}

/// One row of the popover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionItem {
    /// An agent to dispatch.
    Agent(MentionAgent),
    /// A task to reference, with its title.
    Task(TaskId, String),
}

impl MentionItem {
    /// What picking it inserts.
    #[must_use]
    pub fn insert(&self) -> String {
        match self {
            Self::Agent(agent) => format!("@{}", agent.name),
            Self::Task(id, _) => format!("#{id}"),
        }
    }
}

impl AppState {
    /// The space and project whose agents a draft for `destination` can
    /// mention; `None` while a task's project is unknown.
    #[must_use]
    pub fn mention_project(&self, destination: &Destination) -> Option<(String, String)> {
        match destination {
            Destination::Lead(space, project) => Some((space.clone(), project.clone())),
            Destination::Task(space, task) => {
                let project = self
                    .task(space, *task)
                    .map(|detail| detail.summary.project.clone())
                    .or_else(|| {
                        self.space(space)?
                            .tasks
                            .iter()
                            .find(|summary| summary.task.id == *task)
                            .map(|summary| summary.project.clone())
                    })?;
                Some((space.clone(), project))
            }
        }
    }

    /// The agents a draft for `destination` can mention: those bound in its
    /// project and not Off, its lead excluded, each with whether it is busy.
    #[must_use]
    pub fn mention_agents(&self, destination: &Destination) -> Vec<MentionAgent> {
        let Some((space, project)) = self.mention_project(destination) else {
            return Vec::new();
        };
        let enabled = self.task_agents(&space, &project);
        let running = self.space(&space).map(|space| space.running.as_slice());
        self.project(&space, &project)
            .map(|view| {
                view.bindings
                    .iter()
                    .filter(|binding| !binding.is_lead && enabled.contains(&binding.agent))
                    .map(|binding| MentionAgent {
                        name: binding.agent.clone(),
                        role: binding.role.clone(),
                        busy: running
                            .unwrap_or_default()
                            .iter()
                            .any(|task| task.agent == binding.agent && task.project == project),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The popover rows for `mention`, best match first.
    #[must_use]
    pub fn mention_items(
        &self,
        destination: &Destination,
        mention: &MentionQuery,
    ) -> Vec<MentionItem> {
        let mut ranked: Vec<(usize, MentionItem)> = match mention.kind {
            MentionKind::Agent => self
                .mention_agents(destination)
                .into_iter()
                .filter_map(|agent| {
                    let rank = fuzzy_rank(&mention.query, &agent.name)
                        .or_else(|| fuzzy_rank(&mention.query, &agent.role).map(|r| r + 2000))?;
                    Some((rank, MentionItem::Agent(agent)))
                })
                .collect(),
            MentionKind::Task => {
                let space = match destination {
                    Destination::Lead(space, _) | Destination::Task(space, _) => space,
                };
                let mut tasks: Vec<_> = self
                    .space(space)
                    .map(|space| space.tasks.iter().rev().map(|summary| &summary.task))
                    .into_iter()
                    .flatten()
                    .filter_map(|task| {
                        let rank =
                            fuzzy_rank(&mention.query, &format!("{} {}", task.id, task.title))?;
                        Some((rank, MentionItem::Task(task.id, task.title.clone())))
                    })
                    .collect();
                let mut seen = Vec::new();
                tasks.retain(|(_, item)| {
                    let new = !seen.contains(item);
                    seen.push(item.clone());
                    new
                });
                tasks
            }
        };
        ranked.sort_by_key(|(rank, _)| *rank);
        ranked
            .into_iter()
            .map(|(_, item)| item)
            .take(MENTION_LIMIT)
            .collect()
    }

    /// The picked `@Name` chips in `text` that a draft for `destination`
    /// can dispatch, in order. Typed, unpicked `@Name` text is left out.
    #[must_use]
    pub fn mentioned_agents(&self, destination: &Destination, text: &str) -> Vec<String> {
        self.picked_chips(destination, text)
            .into_iter()
            .map(|pick| pick.agent.clone())
            .collect()
    }

    /// The picks whose chip is in `text` and names an agent the draft can
    /// dispatch: the first chip of each agent, in order.
    fn picked_chips(&self, destination: &Destination, text: &str) -> Vec<&Pick> {
        let agents = self.mention_agents(destination);
        let mut picks: Vec<&Pick> = self
            .draft_picks(destination)
            .iter()
            .filter(|pick| {
                is_chip(text, pick) && agents.iter().any(|agent| agent.name == pick.agent)
            })
            .collect();
        picks.sort_by_key(|pick| pick.at);
        let mut named: Vec<&Pick> = Vec::new();
        for pick in picks {
            if !named.iter().any(|other| other.agent == pick.agent) {
                named.push(pick);
            }
        }
        named
    }

    /// Where the draft goes once it mentions agents: `Goes to @A and @B as
    /// new tasks, not …`. `None` while it mentions none.
    #[must_use]
    pub fn mention_hint(&self, destination: &Destination) -> Option<String> {
        let named = self.mentioned_agents(destination, self.draft(destination));
        if named.is_empty() {
            return None;
        }
        let who = named
            .iter()
            .map(|name| format!("@{name}"))
            .collect::<Vec<_>>()
            .join(" and ");
        let what = if named.len() == 1 {
            "a new task"
        } else {
            "new tasks"
        };
        let instead = match destination {
            Destination::Lead(..) => "not the lead".to_owned(),
            Destination::Task(_, task) => format!("not {task}"),
        };
        Some(format!("Goes to {who} as {what}, {instead}."))
    }

    /// The tasks `text` dispatches: one per mentioned agent, in the
    /// thread's project, with the thread and every `#task` it references as
    /// context. Empty when it mentions no agent.
    #[must_use]
    pub fn dispatch_specs(
        &self,
        destination: &Destination,
        text: &str,
        when: &When,
    ) -> Vec<NewTaskSpec> {
        let picks = self.picked_chips(destination, text);
        let Some((space, project)) = self.mention_project(destination) else {
            return Vec::new();
        };
        if picks.is_empty() {
            return Vec::new();
        }
        let goal = format!(
            "{}\n\n{}{}",
            text.trim(),
            self.thread_context(destination, &project, when),
            self.referenced_context(&space, text)
        );
        let title = dispatch_title(&drop_picks(text, self.draft_picks(destination), |_| true).0);
        picks
            .into_iter()
            .map(|pick| NewTaskSpec {
                project: Some(project.clone()),
                title: title.clone(),
                goal: goal.clone(),
                agent: pick.agent.clone(),
                request_id: Some(pick.key.clone()),
                ..NewTaskSpec::default()
            })
            .collect()
    }

    fn thread_context(&self, destination: &Destination, project: &str, when: &When) -> String {
        match destination {
            Destination::Task(space, id) => {
                let task = self
                    .task(space, *id)
                    .map(|detail| &detail.summary.task)
                    .or_else(|| {
                        self.space(space)?
                            .tasks
                            .iter()
                            .map(|summary| &summary.task)
                            .find(|task| task.id == *id)
                    });
                let about = task.map_or_else(String::new, |task| {
                    format!(" \"{}\". Its goal:\n{}", task.title, task.goal)
                });
                format!(
                    "Asked from the timeline of {id}{about}\nIts progress notes are in project/tasks/{id}.md."
                )
            }
            Destination::Lead(space, _) => {
                // shortcut: the last few messages only, not the whole conversation; add a
                // hub request for a thread excerpt if agents need more.
                let recent: Vec<String> = self
                    .conversation(space, project, when)
                    .into_iter()
                    .filter_map(|line| match line {
                        Line::You { text, .. } => Some(format!("You: {text}")),
                        Line::Lead { text, .. }
                        | Line::SpaceLead { text, .. }
                        | Line::ProjectReply { text, .. } => Some(format!("Lead: {text}")),
                        _ => None,
                    })
                    .collect();
                let recent = &recent[recent.len().saturating_sub(4)..];
                if recent.is_empty() {
                    format!("Asked from the {project} chat.")
                } else {
                    format!(
                        "Asked from the {project} chat. Its latest messages:\n{}",
                        recent.join("\n")
                    )
                }
            }
        }
    }

    fn referenced_context(&self, space: &str, text: &str) -> String {
        let Some(known) = self.space(space) else {
            return String::new();
        };
        task_refs(text)
            .into_iter()
            .map(|id| {
                let about = known
                    .tasks
                    .iter()
                    .find(|summary| summary.task.id == id)
                    .map_or_else(String::new, |summary| {
                        let task = &summary.task;
                        format!(
                            " \"{}\" ({}, {:?}). Its goal:\n{}",
                            task.title, summary.project, task.status, task.goal
                        )
                    });
                format!(
                    "\n\nReferenced {id}{about}\nIts progress notes are in project/tasks/{id}.md."
                )
            })
            .collect()
    }
}

/// An agent picked from the `@` popover: its chip `@agent` at byte `at` of
/// the draft, and the `create_task` request key every attempt to start it
/// sends, so a retry after a lost reply can't start it twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pick {
    pub(crate) agent: String,
    pub(crate) at: usize,
    pub(crate) key: String,
}

impl Pick {
    fn end(&self) -> usize {
        self.at + 1 + self.agent.len()
    }
}

/// Whether `text` holds the chip of `pick` where it says, as a whole word.
pub(crate) fn is_chip(text: &str, pick: &Pick) -> bool {
    let starts_word = text
        .get(..pick.at)
        .is_some_and(|before| before.is_empty() || before.ends_with(char::is_whitespace));
    starts_word
        && text
            .get(pick.at..)
            .and_then(|rest| rest.strip_prefix('@'))
            .and_then(|rest| rest.strip_prefix(pick.agent.as_str()))
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
}

/// Moves `picks` from `old` to `new` across one edit: a chip the edit
/// touched is dropped, one after it shifts, and only whole chips stay.
pub(crate) fn remap_picks(old: &str, new: &str, picks: &mut Vec<Pick>) {
    let prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take(old.len().min(new.len()) - prefix)
        .take_while(|(a, b)| a == b)
        .count();
    let edited_end = old.len() - suffix;
    picks.retain_mut(|pick| {
        if pick.end() <= prefix {
            true
        } else if pick.at >= edited_end {
            pick.at = pick.at + new.len() - old.len();
            true
        } else {
            false
        }
    });
    picks.retain(|pick| is_chip(new, pick));
}

/// `text` without the chips of the `picks` that `drop` selects, each with
/// its trailing punctuation and one space, and the other picks moved to match.
pub(crate) fn drop_picks(
    text: &str,
    picks: &[Pick],
    drop: impl Fn(&Pick) -> bool,
) -> (String, Vec<Pick>) {
    let mut sorted: Vec<&Pick> = picks.iter().filter(|pick| is_chip(text, pick)).collect();
    sorted.sort_by_key(|pick| pick.at);
    let (mut out, mut kept, mut from) = (String::new(), Vec::new(), 0);
    for pick in sorted {
        if pick.at < from {
            continue;
        }
        out.push_str(&text[from..pick.at]);
        if drop(pick) {
            let rest = &text[pick.end()..];
            let rest =
                rest.trim_start_matches(|c: char| !c.is_whitespace() && !c.is_alphanumeric());
            let mut chars = rest.chars();
            let rest = match chars.next() {
                Some(c) if c.is_whitespace() => chars.as_str(),
                _ => rest,
            };
            from = text.len() - rest.len();
        } else {
            kept.push(Pick {
                at: out.len(),
                ..pick.clone()
            });
            out.push_str(&text[pick.at..pick.end()]);
            from = pick.end();
        }
    }
    out.push_str(&text[from..]);
    (out, kept)
}

/// The first line of `text` without mentions, cut to a title's length.
fn dispatch_title(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    let words: Vec<&str> = line
        .split_whitespace()
        .filter(|word| !word.starts_with('@'))
        .collect();
    let title = words.join(" ");
    if title.is_empty() {
        return "Asked by you".to_owned();
    }
    let mut cut: String = title.chars().take(72).collect();
    if cut.len() < title.len() {
        cut.push('…');
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_mention_under_the_cursor() {
        let text = "ask @Rev about #T-01";
        let agent = mention_at(text, 8).expect("agent");
        assert_eq!(agent.kind, MentionKind::Agent);
        assert_eq!(agent.query, "Rev");
        assert_eq!(&text[agent.range.clone()], "@Rev");
        let task = mention_at(text, text.len()).expect("task");
        assert_eq!(
            (task.kind, task.query.as_str()),
            (MentionKind::Task, "T-01")
        );
        assert_eq!(mention_at("@", 1).map(|m| m.query), Some(String::new()));
        assert_eq!(mention_at("mail a@b", 8), None, "mid-word @ is no mention");
        assert_eq!(mention_at("@Rev done", 9), None, "cursor past the token");
        assert_eq!(
            mention_at("é @x", "é @x".len()).map(|m| m.query),
            Some("x".into())
        );
    }

    #[test]
    fn completing_replaces_the_token_and_moves_the_cursor() {
        let text = "hi @re please";
        let mention = mention_at(text, 6).expect("mention");
        let (done, cursor) = complete_mention(text, &mention, "@Reviewer");
        assert_eq!(done, "hi @Reviewer please");
        assert_eq!(&done[..cursor], "hi @Reviewer ");
    }

    #[test]
    fn selection_wraps() {
        assert_eq!(step_selection(0, 3, false), 2);
        assert_eq!(step_selection(2, 3, true), 0);
        assert_eq!(step_selection(1, 3, true), 2);
        assert_eq!(step_selection(4, 0, true), 0);
    }

    #[test]
    fn fuzzy_ranks_prefix_then_substring_then_letters() {
        assert_eq!(fuzzy_rank("", "Reviewer"), Some(0));
        assert!(fuzzy_rank("rev", "Reviewer") < fuzzy_rank("view", "Reviewer"));
        assert!(fuzzy_rank("view", "Reviewer") < fuzzy_rank("rvw", "Reviewer"));
        assert_eq!(fuzzy_rank("xyz", "Reviewer"), None);
        assert_eq!(fuzzy_rank("0196", "T-0196 Fix login"), Some(3));
    }

    #[test]
    fn reads_task_references() {
        let id = |n| TaskId::parse(n).expect("id");
        assert_eq!(
            task_refs("see #T-0196, and #T-0002. Also #T-0196 #nope T-0003"),
            vec![id("T-0196"), id("T-0002")]
        );
    }

    fn pick(agent: &str, at: usize) -> Pick {
        Pick {
            agent: agent.to_owned(),
            at,
            key: format!("{agent}@{at}"),
        }
    }

    #[test]
    fn chips_are_whole_words_where_picked() {
        assert!(is_chip("ask @Reviewer, then", &pick("Reviewer", 4)));
        assert!(!is_chip("ask @Reviewe then", &pick("Reviewer", 4)));
        assert!(!is_chip("ask @Reviewerx", &pick("Reviewer", 4)));
        assert!(!is_chip("ask x@Reviewer", &pick("Reviewer", 5)));
        assert!(is_chip("@Chief of Staff look", &pick("Chief of Staff", 0)));
        let (rest, kept) = drop_picks(
            "@A @B look\nat @A, now",
            &[pick("A", 0), pick("B", 3), pick("A", 14)],
            |pick| pick.agent == "A",
        );
        assert_eq!(rest, "@B look\nat now");
        assert_eq!(
            kept,
            [Pick {
                at: 0,
                ..pick("B", 3)
            }]
        );
    }

    #[test]
    fn an_edit_keeps_only_the_picks_it_left_whole() {
        let remap = |old: &str, new: &str, picks: &[Pick]| {
            let mut picks = picks.to_vec();
            remap_picks(old, new, &mut picks);
            picks.iter().map(|pick| pick.at).collect::<Vec<_>>()
        };
        let both = [pick("R", 0), pick("R", 7)];
        assert_eq!(remap("@R and @R", "x @R and @R", &both), [2, 9]);
        assert_eq!(remap("@R and @R", "@R or @R", &both), [0, 6]);
        assert_eq!(
            remap("@R and @R", "and @R", &both[..1]),
            [0; 0],
            "the picked one went"
        );
        assert!(remap("@R and @R", "@Rx and @R", &both[..1]).is_empty());
        assert!(remap("@R and @R", "", &both).is_empty());
    }

    #[test]
    fn titles_drop_mentions() {
        assert_eq!(
            dispatch_title("@Reviewer check #T-0196\nmore"),
            "check #T-0196"
        );
        assert_eq!(dispatch_title("@Reviewer"), "Asked by you");
        assert_eq!(dispatch_title(&"x".repeat(80)).chars().count(), 73);
    }
}
