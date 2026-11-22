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
        let agents = self.mention_agents(destination);
        let picked = self.picked_agents(destination);
        let mut named = Vec::new();
        for word in text.split_whitespace() {
            if let Some(name) = chip_name(word)
                && picked.iter().any(|agent| agent == name)
                && let Some(agent) = agents.iter().find(|agent| agent.name == name)
                && !named.contains(&agent.name)
            {
                named.push(agent.name.clone());
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
        let agents = self.mentioned_agents(destination, text);
        let Some((space, project)) = self.mention_project(destination) else {
            return Vec::new();
        };
        if agents.is_empty() {
            return Vec::new();
        }
        let goal = format!(
            "{}\n\n{}{}",
            text.trim(),
            self.thread_context(destination, &project, when),
            self.referenced_context(&space, text)
        );
        let title = dispatch_title(text);
        agents
            .into_iter()
            .map(|agent| NewTaskSpec {
                project: Some(project.clone()),
                title: title.clone(),
                goal: goal.clone(),
                agent,
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

/// The agent a `@Name` word names, trailing punctuation dropped.
fn chip_name(word: &str) -> Option<&str> {
    word.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
        .strip_prefix('@')
}

/// Whether `text` still holds the chip `@agent` as a whole word.
pub(crate) fn has_chip(text: &str, agent: &str) -> bool {
    text.split_whitespace()
        .any(|word| chip_name(word) == Some(agent))
}

/// `text` without the chips of `agents`.
pub(crate) fn drop_chips(text: &str, agents: &[&str]) -> String {
    text.split_inclusive(char::is_whitespace)
        .filter(|piece| chip_name(piece.trim_end()).is_none_or(|name| !agents.contains(&name)))
        .collect()
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

    #[test]
    fn chips_are_whole_words() {
        assert!(has_chip("ask @Reviewer, then", "Reviewer"));
        assert!(!has_chip("ask @Reviewe then", "Reviewer"));
        assert!(!has_chip("ask @Reviewerx", "Reviewer"));
        assert_eq!(
            drop_chips("@A @B look\nat @A, now", &["A"]),
            "@B look\nat now"
        );
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
