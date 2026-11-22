//! In-place review of a task's work: its pull requests' state and checks,
//! and a full-width, per-file diff whose files and hunks quote into the
//! task composer.

use std::collections::HashMap;

use threadlane_client::{ClientCore, ClientError};
use threadlane_core::client::{PullRequestFiles, TaskCheckRun, TaskPullRequest};
use threadlane_core::github::{CheckStatus, PullRequestStatus};
use threadlane_core::ids::TaskId;
use threadlane_core::workspace::DiffPreview;

use crate::{AppState, changes, describe};

/// The most tasks one board asks pull request status for.
const BOARD_PULLS: usize = 40;

/// Pull request status for some tasks of one space. Made by
/// [`load_pulls`], applied with [`AppState::apply_pulls`].
#[derive(Debug)]
pub struct PullsLoad {
    space: String,
    pulls: Vec<(TaskId, Result<Vec<TaskPullRequest>, ClientError>)>,
}

/// Asks the hub of `space` for each task's pull requests: served from the
/// hub's cache, so cheap. Only the first [`BOARD_PULLS`] tasks are asked.
pub async fn load_pulls(
    core: &ClientCore,
    space: &str,
    tasks: impl IntoIterator<Item = TaskId>,
) -> PullsLoad {
    let asks = tasks
        .into_iter()
        .take(BOARD_PULLS)
        .map(|task| async move { (task, core.task_pull_requests(space, task).await) });
    PullsLoad {
        space: space.to_owned(),
        pulls: futures_util::future::join_all(asks).await,
    }
}

/// The files of a task's pull requests. Made by [`load_pull_files`],
/// applied with [`AppState::apply_pull_files`].
#[derive(Debug)]
pub struct PullFilesLoad {
    space: String,
    task: TaskId,
    files: Result<Vec<PullRequestFiles>, ClientError>,
}

/// Asks the hub of `space` for every file task `task`'s pull requests change.
pub async fn load_pull_files(core: &ClientCore, space: &str, task: TaskId) -> PullFilesLoad {
    PullFilesLoad {
        space: space.to_owned(),
        task,
        files: core.task_pull_files(space, task).await,
    }
}

/// `#42 Draft`.
#[must_use]
pub fn pull_label(pull: &TaskPullRequest) -> String {
    let state = match pull.status {
        PullRequestStatus::Open => "Open",
        PullRequestStatus::Draft => "Draft",
        PullRequestStatus::Merged => "Merged",
        PullRequestStatus::Closed => "Closed",
    };
    format!("#{} {state}", pull.number)
}

/// How many checks pass, run and fail, in that order, leaving out none.
#[must_use]
pub fn check_counts(checks: &[TaskCheckRun]) -> Vec<(CheckStatus, usize)> {
    [
        CheckStatus::Passing,
        CheckStatus::Running,
        CheckStatus::Failing,
    ]
    .into_iter()
    .map(|status| {
        let n = checks.iter().filter(|check| check.status == status).count();
        (status, n)
    })
    .filter(|(_, n)| *n > 0)
    .collect()
}

/// `2 passing`, `1 running`, `1 failing`.
#[must_use]
pub fn check_label(status: CheckStatus, count: usize) -> String {
    let word = match status {
        CheckStatus::Passing => "passing",
        CheckStatus::Running => "running",
        CheckStatus::Failing => "failing",
    };
    format!("{count} {word}")
}

/// What one diff line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// One line of a hunk with its numbers on each side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

/// One `@@` hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// `@@ -1,3 +1,4 @@ fn main`.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

/// One changed file as the review view shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFile {
    /// The repository the file is in, so one path in two repositories stays two files.
    pub repository: String,
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    /// Empty for a binary file or one too large to show.
    pub hunks: Vec<Hunk>,
}

/// The hunks of a unified diff; lines before the first `@@` are skipped.
#[must_use]
pub fn parse_hunks(patch: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut old, mut new) = (0, 0);
    for line in patch.lines() {
        if line.starts_with("@@") {
            let mut sides = line.split_whitespace().skip(1);
            let start = |side: Option<&str>| {
                side.and_then(|s| s.get(1..)?.split(',').next()?.parse().ok())
                    .unwrap_or(0)
            };
            old = start(sides.next());
            new = start(sides.next());
            hunks.push(Hunk {
                header: line.to_owned(),
                lines: Vec::new(),
            });
            continue;
        }
        let Some(hunk) = hunks.last_mut() else {
            continue;
        };
        let (kind, text) = match line.chars().next() {
            Some('+') => (LineKind::Added, &line[1..]),
            Some('-') => (LineKind::Removed, &line[1..]),
            Some('\\') => continue,
            Some(' ') => (LineKind::Context, &line[1..]),
            _ => (LineKind::Context, line),
        };
        let numbered = DiffLine {
            kind,
            old: (kind != LineKind::Added).then_some(old),
            new: (kind != LineKind::Removed).then_some(new),
            text: text.to_owned(),
        };
        old += u32::from(kind != LineKind::Added);
        new += u32::from(kind != LineKind::Removed);
        hunk.lines.push(numbered);
    }
    hunks
}

/// The rows of a side-by-side hunk: removed lines beside the added lines
/// that replace them, context on both sides.
#[must_use]
pub fn side_by_side(hunk: &Hunk) -> Vec<(Option<&DiffLine>, Option<&DiffLine>)> {
    let mut rows = Vec::new();
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    let flush = |rows: &mut Vec<_>, removed: &mut Vec<_>, added: &mut Vec<_>| {
        let n = removed.len().max(added.len());
        let (mut r, mut a) = (removed.drain(..), added.drain(..));
        for _ in 0..n {
            rows.push((r.next(), a.next()));
        }
    };
    for line in &hunk.lines {
        match line.kind {
            LineKind::Removed => {
                if !added.is_empty() {
                    flush(&mut rows, &mut removed, &mut added);
                }
                removed.push(line);
            }
            LineKind::Added => added.push(line),
            LineKind::Context => {
                flush(&mut rows, &mut removed, &mut added);
                rows.push((Some(line), Some(line)));
            }
        }
    }
    flush(&mut rows, &mut removed, &mut added);
    rows
}

/// The files of a combined `git diff` patch, keyed by their `b/` path.
fn split_patch(patch: &str) -> HashMap<String, String> {
    let mut files = HashMap::new();
    let mut current: Option<(String, String)> = None;
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.extend(current.take());
            let path = rest
                .rsplit_once(" b/")
                .map_or(rest, |(_, path)| path)
                .to_owned();
            current = Some((path, String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    files.extend(current);
    files
}

fn preview_files(diff: &DiffPreview) -> impl Iterator<Item = ReviewFile> + '_ {
    let mut patches = split_patch(&diff.patch);
    diff.files.iter().map(move |file| ReviewFile {
        repository: diff.repository.clone(),
        path: file.path.clone(),
        additions: file.insertions,
        deletions: file.deletions,
        hunks: patches
            .remove(&file.path)
            .map(|patch| parse_hunks(&patch))
            .unwrap_or_default(),
    })
}

/// A file or hunk quoted into the composer as a reference the agent can act on.
#[must_use]
pub fn quote(file: &ReviewFile, hunk: Option<&Hunk>) -> String {
    match hunk {
        None => format!(
            "In {} `{}` (+{} -{}):\n",
            file.repository, file.path, file.additions, file.deletions
        ),
        Some(hunk) => {
            let body: String = hunk
                .lines
                .iter()
                .map(|line| {
                    let sign = match line.kind {
                        LineKind::Context => ' ',
                        LineKind::Added => '+',
                        LineKind::Removed => '-',
                    };
                    format!("{sign}{}\n", line.text)
                })
                .collect();
            format!(
                "In {} `{}` {}:\n```diff\n{body}```\n",
                file.repository, file.path, hunk.header
            )
        }
    }
}

/// The open full-width review of one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub space: String,
    pub task: TaskId,
    /// The file shown, by index into [`AppState::review_files`].
    pub file: usize,
    /// Side by side rather than unified.
    pub split: bool,
}

impl AppState {
    /// Takes pull request status for some tasks; a failed read keeps what was shown.
    pub fn apply_pulls(&mut self, load: PullsLoad) {
        for (task, pulls) in load.pulls {
            if let Ok(pulls) = pulls {
                self.pulls.insert((load.space.clone(), task), pulls);
            }
        }
    }

    /// The tasks of a project whose pull requests the board shows: open
    /// ones first, then the newest.
    #[must_use]
    pub fn pull_candidates(&self, space: &str, project: &str) -> Vec<TaskId> {
        let mut tasks: Vec<_> = self
            .project(space, project)
            .map(|view| view.tasks.iter().map(|s| &s.task).collect())
            .unwrap_or_default();
        tasks.sort_by_key(|task| (task.status.is_terminal(), std::cmp::Reverse(task.id)));
        tasks.into_iter().map(|task| task.id).collect()
    }

    /// The pull requests of task `task`, as last read.
    #[must_use]
    pub fn task_pulls(&self, space: &str, task: TaskId) -> &[TaskPullRequest] {
        self.pulls
            .get(&(space.to_owned(), task))
            .map_or(&[], Vec::as_slice)
    }

    /// Takes the files of a task's pull requests, or why they could not be read.
    pub fn apply_pull_files(&mut self, load: PullFilesLoad) {
        self.pull_files.insert(
            (load.space, load.task),
            load.files.map_err(|error| describe(&error)),
        );
    }

    /// Why the review shows no pull request files, once they were asked for.
    #[must_use]
    pub fn pull_files_note(&self, space: &str, task: TaskId) -> Option<String> {
        match self.pull_files.get(&(space.to_owned(), task))? {
            Err(why) => Some(format!("Showing the merge-check preview: {why}")),
            Ok(pulls) if pulls.iter().all(|pull| pull.files.is_empty()) => Some(
                if pulls.is_empty() {
                    "This task has no pull request yet."
                } else {
                    "Its pull requests change no files yet."
                }
                .to_owned(),
            ),
            Ok(_) => None,
        }
    }

    /// The pull request files read for the open review of task `task` are
    /// for heads its pull requests have since moved from.
    #[must_use]
    pub fn review_files_stale(&self, space: &str, task: TaskId) -> bool {
        let Some(Ok(read)) = self.pull_files.get(&(space.to_owned(), task)) else {
            return false;
        };
        // Both lists come from the hub in the order the pull requests were opened.
        let current = self
            .task_pulls(space, task)
            .iter()
            .map(|p| (&p.repository, p.number, &p.head_sha));
        let shown = read.iter().map(|p| (&p.repository, p.number, &p.head_sha));
        self.review(space, task).is_some() && !current.eq(shown)
    }

    /// Every file the review of task `task` shows: from its pull requests
    /// once read, else from the diff previews on its merge checks.
    #[must_use]
    pub fn review_files(&self, space: &str, task: TaskId) -> Vec<ReviewFile> {
        let key = (space.to_owned(), task);
        if let Some(Ok(pulls)) = self.pull_files.get(&key)
            && pulls.iter().any(|pull| !pull.files.is_empty())
        {
            return pulls
                .iter()
                .flat_map(|pull| pull.files.iter().map(move |file| (&pull.repository, file)))
                .map(|(repository, file)| ReviewFile {
                    repository: repository.clone(),
                    path: file.path.clone(),
                    additions: file.additions,
                    deletions: file.deletions,
                    hunks: file.patch.as_deref().map(parse_hunks).unwrap_or_default(),
                })
                .collect();
        }
        self.tasks
            .get(&key)
            .map(|detail| {
                changes(detail)
                    .diffs
                    .iter()
                    .flat_map(preview_files)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Opens the full-width review of task `task` at its first file.
    pub fn open_review(&mut self, space: &str, task: TaskId) {
        self.review = Some(Review {
            space: space.to_owned(),
            task,
            file: 0,
            split: false,
        });
    }

    pub fn close_review(&mut self) {
        self.review = None;
    }

    /// The review of task `task`, while it is open.
    #[must_use]
    pub fn review(&self, space: &str, task: TaskId) -> Option<&Review> {
        self.review
            .as_ref()
            .filter(|review| review.space == space && review.task == task)
    }

    /// Shows file `file` of the open review, kept within its files.
    pub fn select_review_file(&mut self, file: usize) {
        let Some((space, task)) = self.review.as_ref().map(|r| (r.space.clone(), r.task)) else {
            return;
        };
        let last = self.review_files(&space, task).len().saturating_sub(1);
        if let Some(review) = &mut self.review {
            review.file = file.min(last);
        }
    }

    pub fn toggle_review_split(&mut self) {
        if let Some(review) = &mut self.review {
            review.split = !review.split;
        }
    }
}

// The fake-hub tests live in `tests.rs`.
#[cfg(test)]
mod unit {
    use super::*;

    const PATCH: &str = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,3 @@ fn main
 keep
-old
+new
 tail
diff --git a/b.txt b/b.txt
@@ -0,0 +1 @@
+hello
";

    #[test]
    fn a_patch_splits_into_numbered_hunks_per_file() {
        let files = split_patch(PATCH);
        let hunks = parse_hunks(&files["src/a.rs"]);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].header, "@@ -1,3 +1,3 @@ fn main");
        let numbers: Vec<_> = hunks[0].lines.iter().map(|l| (l.old, l.new)).collect();
        assert_eq!(
            numbers,
            [
                (Some(1), Some(1)),
                (Some(2), None),
                (None, Some(2)),
                (Some(3), Some(3))
            ]
        );
        assert_eq!(parse_hunks(&files["b.txt"])[0].lines[0].new, Some(1));
    }

    #[test]
    fn side_by_side_pairs_a_removed_line_with_the_line_that_replaces_it() {
        let hunk = &parse_hunks("@@ -1,3 +1,4 @@\n a\n-b\n+c\n+d\n e\n")[0];
        let rows: Vec<_> = side_by_side(hunk)
            .into_iter()
            .map(|(l, r)| (l.map(|l| l.text.as_str()), r.map(|r| r.text.as_str())))
            .collect();
        assert_eq!(
            rows,
            [
                (Some("a"), Some("a")),
                (Some("b"), Some("c")),
                (None, Some("d")),
                (Some("e"), Some("e"))
            ]
        );
    }

    #[test]
    fn a_hunk_quotes_as_a_fenced_diff_under_its_file() {
        let hunks = parse_hunks(&split_patch(PATCH)["src/a.rs"]);
        let file = ReviewFile {
            repository: "api".into(),
            path: "src/a.rs".into(),
            additions: 1,
            deletions: 1,
            hunks,
        };
        assert_eq!(quote(&file, None), "In api `src/a.rs` (+1 -1):\n");
        assert_eq!(
            quote(&file, Some(&file.hunks[0])),
            "In api `src/a.rs` @@ -1,3 +1,3 @@ fn main:\n```diff\n keep\n-old\n+new\n tail\n```\n"
        );
    }
}
