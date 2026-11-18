//! Deterministic checks over the live notes of a single project.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use threadlane_core::client::MemoryFinding;
use threadlane_core::ids::{ProjectId, TaskId};
use threadlane_core::model::{MemoryFile, Repository};
use threadlane_core::store::{MemoryStore, OrgStore, StoreResult, TaskStore};
use threadlane_store::HubDb;

#[derive(Debug)]
enum RepoFiles {
    Local(PathBuf),
    Tracked(HashSet<String>),
    Unknown,
}

pub(crate) async fn check(
    db: &HubDb,
    project: ProjectId,
    workspaces: Option<&Path>,
) -> StoreResult<Vec<MemoryFinding>> {
    let prefix = format!("projects/{}/", project.get());
    let mut files = db.list(&prefix).await?;
    for file in &mut files {
        if let Some(path) = file.path.strip_prefix(&prefix) {
            file.path = format!("project/{path}");
        }
    }
    let tasks = db
        .tasks(project)
        .await?
        .into_iter()
        .map(|task| task.id)
        .collect();
    let referenced: HashSet<_> = files
        .iter()
        .filter(|file| is_note(file))
        .flat_map(|file| file_refs(&file.content))
        .filter_map(|target| target.strip_prefix("repo:")?.split_once('/'))
        .map(|(name, _)| name)
        .collect();
    let mut repos = HashMap::new();
    if !referenced.is_empty() {
        for repo in db.repositories(project).await? {
            if referenced.contains(repo.name.as_str()) {
                let source = repository_files(&repo, workspaces).await;
                repos.insert(repo.name, source);
            }
        }
    }
    Ok(findings(&files, &tasks, &repos))
}

async fn repository_files(repo: &Repository, workspaces: Option<&Path>) -> RepoFiles {
    let path = Path::new(
        repo.remote_url
            .strip_prefix("file://")
            .unwrap_or(&repo.remote_url),
    );
    if path.is_absolute()
        && let Ok(root) = path.canonicalize()
    {
        if root.join(".git").exists() || !root.join("objects").is_dir() {
            return RepoFiles::Local(root);
        }
        return git_files(&root, &[format!("refs/heads/{}", repo.default_branch)]).await;
    }
    if let Some(root) = workspaces {
        let clone = root.join("clones").join(repo.id.to_string());
        if clone.join(".git").is_dir() {
            return git_files(
                &clone,
                &[
                    format!("refs/remotes/origin/{}", repo.default_branch),
                    format!("refs/heads/{}", repo.default_branch),
                ],
            )
            .await;
        }
    }
    // shortcut: remote-only repositories are unresolved until the hub has a local clone.
    RepoFiles::Unknown
}

async fn git_files(root: &Path, refs: &[String]) -> RepoFiles {
    for reference in refs {
        let result = tokio::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "ls-tree",
                "--full-tree",
                "-r",
                "--name-only",
                "-z",
                reference,
                "--",
            ])
            .env("GIT_ALLOW_PROTOCOL", "")
            .env("GIT_NO_LAZY_FETCH", "1")
            .output()
            .await;
        if let Ok(output) = result
            && output.status.success()
        {
            return RepoFiles::Tracked(
                output
                    .stdout
                    .split(|byte| *byte == 0)
                    .filter(|path| !path.is_empty())
                    .filter_map(|path| std::str::from_utf8(path).ok().map(str::to_owned))
                    .collect(),
            );
        }
    }
    RepoFiles::Unknown
}

fn is_note(file: &MemoryFile) -> bool {
    !file.deleted && file.path.starts_with("project/notes/") && !file.path.ends_with("/INDEX.md")
}

/// Lowercase words, ignoring punctuation and the note's display title.
fn words(content: &str) -> Vec<String> {
    let content = content.trim_start();
    let body = if content.starts_with("# ") {
        content.split_once('\n').map_or("", |(_, body)| body)
    } else {
        content
    };
    body.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn near(first: &[String], second: &[String]) -> bool {
    if first.len().min(second.len()) < 5 {
        return false;
    }
    let mut counts = HashMap::new();
    for word in first {
        *counts.entry(word).or_insert(0_usize) += 1;
    }
    let common = second
        .iter()
        .filter(|word| {
            let Some(count) = counts.get_mut(*word) else {
                return false;
            };
            let available = *count > 0;
            *count = count.saturating_sub(1);
            available
        })
        .count();
    // Sørensen–Dice similarity on the token multiset, at least 0.8.
    5 * common >= 2 * (first.len() + second.len())
}

fn task_refs(content: &str) -> impl Iterator<Item = TaskId> + '_ {
    content
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .filter_map(|word| {
            let digits = word
                .strip_prefix("T-")
                .or_else(|| word.strip_prefix("t-"))?;
            digits.parse::<i64>().ok().filter(|id| *id > 0).map(TaskId)
        })
}

fn file_refs(content: &str) -> impl Iterator<Item = &str> {
    let links = content
        .split("](")
        .skip(1)
        .filter_map(|tail| tail.split_once(')').map(|(target, _)| target));
    let code = content
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|target| target.starts_with("project/") || target.starts_with("repo:"));
    links
        .chain(code)
        .map(|target| target.trim().trim_start_matches('<').trim_end_matches('>'))
        .map(|target| target.split_once('#').map_or(target, |(path, _)| path))
        .filter(|target| !target.is_empty() && !target.chars().any(char::is_whitespace))
}

fn missing_file(
    note: &str,
    target: &str,
    files: &HashSet<&str>,
    repos: &HashMap<String, RepoFiles>,
) -> bool {
    if let Some(reference) = target.strip_prefix("repo:") {
        let Some((name, path)) = reference.split_once('/') else {
            return false;
        };
        let Some(root) = repos.get(name) else {
            return true;
        };
        let path = Path::new(path);
        if !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        {
            return false;
        }
        return match root {
            RepoFiles::Local(root) => match root.join(path).canonicalize() {
                Ok(file) => !file.starts_with(root) || !file.is_file(),
                Err(error) => error.kind() == std::io::ErrorKind::NotFound,
            },
            RepoFiles::Tracked(files) => !path.to_str().is_some_and(|path| files.contains(path)),
            RepoFiles::Unknown => false,
        };
    }
    if target.contains(':') || target.starts_with('/') || target.starts_with("space/") {
        return false;
    }
    let mut parts: Vec<_> = if target.starts_with("project/") {
        Vec::new()
    } else {
        note.rsplit_once('/')
            .map_or("", |(parent, _)| parent)
            .split('/')
            .collect()
    };
    for part in target.split('/') {
        match part {
            "." | "" => {}
            ".." if parts.len() > 1 => {
                parts.pop();
            }
            ".." => return false,
            _ => parts.push(part),
        }
    }
    !files.contains(parts.join("/").as_str())
}

fn findings(
    files: &[MemoryFile],
    tasks: &HashSet<TaskId>,
    repos: &HashMap<String, RepoFiles>,
) -> Vec<MemoryFinding> {
    let live: HashSet<_> = files
        .iter()
        .filter(|file| !file.deleted)
        .map(|file| file.path.as_str())
        .collect();
    let mut notes: Vec<_> = files
        .iter()
        .filter(|file| is_note(file))
        .map(|file| (file, words(&file.content)))
        .collect();
    notes.sort_by_key(|(file, _)| &file.path);
    let mut findings = BTreeSet::new();
    for (index, (note, body)) in notes.iter().enumerate() {
        for (other, other_body) in &notes[index + 1..] {
            let first = || note.path.clone();
            let second = || other.path.clone();
            if !body.is_empty() && body == other_body {
                findings.insert(MemoryFinding::ExactDuplicate {
                    first: first(),
                    second: second(),
                });
            } else if near(body, other_body) {
                findings.insert(MemoryFinding::NearDuplicate {
                    first: first(),
                    second: second(),
                });
            }
        }
        for task in task_refs(&note.content).filter(|task| !tasks.contains(task)) {
            findings.insert(MemoryFinding::MissingTask {
                note: note.path.clone(),
                task,
            });
        }
        for target in
            file_refs(&note.content).filter(|target| missing_file(&note.path, target, &live, repos))
        {
            findings.insert(MemoryFinding::MissingFile {
                note: note.path.clone(),
                target: target.to_owned(),
            });
        }
    }
    findings.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_core::ids::MemoryFileId;
    use threadlane_core::time::Timestamp;

    #[test]
    fn detects_exact_near_and_broken_references_in_project_notes() {
        let note = |path: &str, content: &str, deleted| MemoryFile {
            id: MemoryFileId(1),
            path: format!("project/notes/{path}.md"),
            content: content.to_owned(),
            version: 1,
            written_by_run: None,
            deleted,
            written_at: Timestamp::from_millis(0),
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("present.rs"), "").unwrap();
        let notes = [
            note("a", "# Title\nUse the cached build artifact only", false),
            note("b", "# Other\nUSE the cached build artifact only!", false),
            note("c", "# Third\nUse the cached build artifact today", false),
            note("gone", "# Old\nUse the cached build artifact only", true),
            note(
                "INDEX",
                "# Index\nUse the cached build artifact only",
                false,
            ),
            note(
                "links",
                "# Links\nT-0001 T-0023 T-0023 [ok](a.md#top) [gone](./gone.md) [web](https://example.invalid) `repo:local/present.rs` [missing](repo:local/gone.rs) `repo:remote/unknown.rs`",
                false,
            ),
        ];
        let repo = HashMap::from([
            (
                "local".into(),
                RepoFiles::Local(dir.path().canonicalize().unwrap()),
            ),
            ("remote".into(), RepoFiles::Unknown),
            (
                "tracked".into(),
                RepoFiles::Tracked(HashSet::from(["src/a.rs".to_owned()])),
            ),
        ]);
        let actual = findings(&notes, &HashSet::from([TaskId(1)]), &repo);
        for (path, missing) in [
            ("src/a.rs", false),
            ("src/gone.rs", true),
            ("../outside", false),
        ] {
            assert_eq!(
                missing_file(
                    "project/notes/links.md",
                    &format!("repo:tracked/{path}"),
                    &HashSet::new(),
                    &repo
                ),
                missing
            );
        }
        assert_eq!(
            actual,
            [
                MemoryFinding::ExactDuplicate {
                    first: "project/notes/a.md".into(),
                    second: "project/notes/b.md".into()
                },
                MemoryFinding::NearDuplicate {
                    first: "project/notes/a.md".into(),
                    second: "project/notes/c.md".into()
                },
                MemoryFinding::NearDuplicate {
                    first: "project/notes/b.md".into(),
                    second: "project/notes/c.md".into()
                },
                MemoryFinding::MissingTask {
                    note: "project/notes/links.md".into(),
                    task: TaskId(23)
                },
                MemoryFinding::MissingFile {
                    note: "project/notes/links.md".into(),
                    target: "./gone.md".into()
                },
                MemoryFinding::MissingFile {
                    note: "project/notes/links.md".into(),
                    target: "repo:local/gone.rs".into()
                },
            ]
        );
    }
}
