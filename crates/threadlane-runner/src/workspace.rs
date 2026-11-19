//! [`GitWorkspaces`]: a task's worktrees, made and saved with `git`
//! (Plan.md §5.5).
//!
//! Under the root, one clone per repository and one worktree per task per
//! repository:
//!
//! ```text
//! <root>/clones/<repository id>            git clone --no-checkout
//! <root>/worktrees/<task id>/<name>        git worktree add, branch threadlane/<task id>
//! <root>/worktrees/<task id>/.<name>.ready written once the worktree is whole
//! ```
//!
//! `git` runs as a subprocess with explicit arguments, never through a
//! shell, each command bounded by a timeout. Every name that reaches it is
//! validated, and options end with `--` where a value follows. The system
//! sets no credentials: `git` uses whatever logins this user has
//! (invariant 8); it is only kept from prompting for one.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use threadlane_core::ids::{RepositoryId, RunId, TaskId};
use threadlane_core::model::Repository;
use threadlane_core::workspace::{
    DiffFileStat, DiffPreview, MergeCheck, PublishPreview, ReviewRevision, WorkspaceCounts,
    WorkspaceError, WorkspaceRemoval, Workspaces, task_branch, valid_branch_name, valid_commit_id,
    valid_repository_name,
};
use tokio::process::Command;
use tokio::sync::{Mutex, OwnedMutexGuard};

/// The longest any one `git` command may take; a clone of a large
/// repository is the slow case.
const GIT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Who the system's own checkpoint commits are by, set per command so they
/// do not depend on this user's `git` configuration. Signing is off because
/// it may need a key or a prompt.
const COMMITTER: [&str; 6] = [
    "-c",
    "user.name=Threadlane",
    "-c",
    "user.email=threadlane@localhost",
    "-c",
    "commit.gpgsign=false",
];

/// [`Workspaces`] on the local disk, with `git`.
#[derive(Debug, Clone)]
pub struct GitWorkspaces {
    root: PathBuf,
    repositories: Arc<Mutex<HashMap<RepositoryId, Arc<Mutex<()>>>>>,
}

impl GitWorkspaces {
    /// Workspaces under `root`, created as needed.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            // Git runs inside a clone (`-C`), where a relative worktree path
            // would land under the clone instead of under `root`.
            root: std::path::absolute(&root).unwrap_or(root),
            repositories: Arc::default(),
        }
    }

    async fn lock_repository(&self, id: RepositoryId) -> OwnedMutexGuard<()> {
        let lock = Arc::clone(self.repositories.lock().await.entry(id).or_default());
        lock.lock_owned().await
    }

    async fn lock_removal(&self, removal: &WorkspaceRemoval) -> Vec<OwnedMutexGuard<()>> {
        // Cleanup may span multiple clones. Take their locks in the same order.
        let ids: BTreeSet<_> = removal
            .repositories
            .iter()
            .map(|(id, _)| *id)
            .chain(removal.clones.iter().copied())
            .collect();
        let mut locks = Vec::with_capacity(ids.len());
        for id in ids {
            locks.push(self.lock_repository(id).await);
        }
        locks
    }

    /// Where the task's worktree of `repository` is, set up or not.
    fn worktree(&self, task: TaskId, repository: &Repository) -> PathBuf {
        self.task_dir(task).join(&repository.name)
    }

    fn task_dir(&self, task: TaskId) -> PathBuf {
        self.root.join("worktrees").join(task.to_string())
    }

    /// Written once the worktree is whole; without it the worktree is made
    /// again.
    fn ready_marker(&self, task: TaskId, repository: &Repository) -> PathBuf {
        self.task_dir(task)
            .join(format!(".{}.ready", repository.name))
    }

    fn is_ready(&self, task: TaskId, repository: &Repository) -> bool {
        self.ready_marker(task, repository).is_file()
            && self.worktree(task, repository).join(".git").exists()
    }

    /// The repository's clone, made if missing. It is cloned beside its
    /// place and renamed into it, so a clone cut short is never mistaken for
    /// a whole one.
    async fn clone(&self, repository: &Repository) -> Result<PathBuf, WorkspaceError> {
        let clones = self.root.join("clones");
        let clone = clones.join(repository.id.to_string());
        if clone.join(".git").is_dir() {
            return Ok(clone);
        }
        let partial = clones.join(format!("{}.partial", repository.id));
        remove_dir(&partial).await?;
        create_dir(&clones).await?;
        git(
            None,
            [
                OsStr::new("clone"),
                OsStr::new("--no-checkout"),
                OsStr::new("--quiet"),
                OsStr::new("--"),
                OsStr::new(&repository.remote_url),
                partial.as_os_str(),
            ],
        )
        .await?;
        remove_dir(&clone).await?;
        tokio::fs::rename(&partial, &clone)
            .await
            .map_err(|error| io_error("moving the clone into place", &clone, &error))?;
        Ok(clone)
    }

    /// The task's worktree of `repository`, made on the task branch if it is
    /// not whole: from the branch if it exists, else from the latest
    /// default branch.
    async fn worktree_of(
        &self,
        task: TaskId,
        repository: &Repository,
    ) -> Result<PathBuf, WorkspaceError> {
        let path = self.worktree(task, repository);
        if self.is_ready(task, repository) {
            return Ok(path);
        }
        let clone = self.clone(repository).await?;
        let marker = self.ready_marker(task, repository);
        remove_file(&marker).await?;
        remove_dir(&path).await?;
        create_dir(&self.task_dir(task)).await?;
        // Forget a registration left by a worktree that was cut short.
        git(Some(&clone), ["worktree", "prune"]).await?;
        git(Some(&clone), ["fetch", "--quiet", "origin"]).await?;
        let branch = task_branch(task);
        let reference = format!("refs/heads/{branch}");
        let exists = git(
            Some(&clone),
            ["rev-parse", "--verify", "--quiet", reference.as_str()],
        )
        .await
        .is_ok();
        let base = format!("origin/{}", repository.default_branch);
        let mut args = vec![
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--quiet"),
        ];
        if !exists {
            args.extend([OsStr::new("-b"), OsStr::new(&branch)]);
        }
        args.extend([OsStr::new("--"), path.as_os_str()]);
        args.push(OsStr::new(if exists { &branch } else { &base }));
        git(Some(&clone), args).await?;
        tokio::fs::write(&marker, b"")
            .await
            .map_err(|error| io_error("marking the worktree ready", &marker, &error))?;
        Ok(path)
    }

    /// Fetches the default branch into the repository's clone and returns
    /// the commit it is at.
    async fn fetch_base(&self, repository: &Repository) -> Result<String, WorkspaceError> {
        let clone = self.clone(repository).await?;
        git(Some(&clone), ["fetch", "--quiet", "origin"]).await?;
        let base = format!("origin/{}", repository.default_branch);
        let commit = git(Some(&clone), ["rev-parse", "--verify", base.as_str()]).await?;
        Ok(commit.trim().to_owned())
    }

    /// Whether `branch` merges into `base` cleanly, by `git merge-tree
    /// --write-tree`, which touches no worktree: the conflicting files.
    async fn conflicts(
        &self,
        repository: &Repository,
        base: &str,
        branch: &str,
    ) -> Result<Vec<String>, WorkspaceError> {
        let clone = self.clone(repository).await?;
        let args: Vec<OsString> = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            base,
            branch,
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let mut command = git_command(Some(&clone), &args);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let output = tokio::time::timeout(GIT_TIMEOUT, command.output())
            .await
            .map_err(|_| WorkspaceError("git merge-tree timed out".to_owned()))?
            .map_err(|error| WorkspaceError(format!("could not run git merge-tree: {error}")))?;
        // 0: merges cleanly; 1: conflicts; anything else: it could not tell.
        match output.status.code() {
            Some(0) => Ok(Vec::new()),
            Some(1) => {
                let mut files: Vec<String> = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .skip(1) // the tree it wrote
                    .take_while(|line| !line.is_empty())
                    .map(str::to_owned)
                    .collect();
                files.dedup();
                Ok(files)
            }
            _ => Err(WorkspaceError(format!(
                "git merge-tree failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
        }
    }

    /// Rebases an unpublished, clean worktree onto `base` when it can.
    /// Returns the files that conflict.
    async fn rebase(
        &self,
        repository: &Repository,
        path: &Path,
        base: &str,
        branch: &str,
    ) -> Result<Vec<String>, WorkspaceError> {
        if git(Some(path), ["merge-base", "--is-ancestor", base, "HEAD"])
            .await
            .is_ok()
        {
            return Ok(Vec::new());
        }
        let remote = format!("refs/remotes/origin/{branch}");
        let published = git(Some(path), ["show-ref", "--verify", "--quiet", &remote])
            .await
            .is_ok();
        if published || !git(Some(path), ["status", "--porcelain"]).await?.is_empty() {
            return self.conflicts(repository, base, branch).await;
        }
        let mut args = COMMITTER.to_vec();
        args.extend(["rebase", "--quiet", base]);
        if git(Some(path), args).await.is_ok() {
            return Ok(Vec::new());
        }
        // A rebase replays commit by commit, so it can stop where the
        // merge-tree of the tips would not: the files it stopped on.
        let mut files: Vec<String> = git(Some(path), ["diff", "--name-only", "--diff-filter=U"])
            .await
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        git(Some(path), ["rebase", "--abort"]).await?;
        if files.is_empty() {
            files = self.conflicts(repository, base, branch).await?;
        }
        Ok(files)
    }

    /// Commits what is uncommitted in one worktree on the task branch.
    async fn commit(&self, path: &Path, task: TaskId, message: &str) -> Result<(), WorkspaceError> {
        let branch = task_branch(task);
        let head = git(Some(path), ["symbolic-ref", "--quiet", "--short", "HEAD"]).await?;
        if head.trim() != branch {
            return Err(WorkspaceError(format!(
                "{} is on {:?}, not on its branch {branch}",
                path.display(),
                head.trim()
            )));
        }
        git(Some(path), ["add", "--all"]).await?;
        if git(Some(path), ["diff", "--cached", "--quiet"])
            .await
            .is_ok()
        {
            return Ok(());
        }
        let mut args = COMMITTER.to_vec();
        args.extend(["commit", "--quiet", "--no-verify", "--message", message]);
        git(Some(path), args).await?;
        Ok(())
    }

    /// Computes a bounded diffstat + unified diff preview between `base` and
    /// `revision` inside `worktree`. Returns `Ok(None)` when the two commits
    /// have no changes between them.
    async fn compute_diff_preview(
        &self,
        repository: &Repository,
        worktree: &Path,
        base: &str,
        revision: &str,
    ) -> Result<Option<DiffPreview>, WorkspaceError> {
        let range = format!("{base}...{revision}");
        let numstat = git(
            Some(worktree),
            ["diff", "--numstat", "--no-renames", range.as_str()],
        )
        .await?;
        let files = parse_numstat(&numstat);
        if files.is_empty() {
            return Ok(None);
        }
        let insertions = files.iter().map(|f| f.insertions).sum();
        let deletions = files.iter().map(|f| f.deletions).sum();
        let raw_patch = git(
            Some(worktree),
            [
                "diff",
                "--unified=3",
                "--no-color",
                "--no-renames",
                range.as_str(),
            ],
        )
        .await?;
        let (patch, truncated) = truncate_patch(&raw_patch, DIFF_MAX_LINES, DIFF_MAX_BYTES);
        Ok(Some(DiffPreview {
            repository: repository.name.clone(),
            base_branch: repository.default_branch.clone(),
            files,
            insertions,
            deletions,
            patch,
            truncated,
        }))
    }
}

/// Maximum unified diff lines stored in a [`DiffPreview`].
const DIFF_MAX_LINES: usize = 160;
/// Maximum UTF-8 bytes stored in a [`DiffPreview`] patch.
const DIFF_MAX_BYTES: usize = 12 * 1024;

fn parse_numstat(output: &str) -> Vec<DiffFileStat> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let added = parts.next()?;
            let removed = parts.next()?;
            let path = parts.next()?.trim();
            if path.is_empty() {
                return None;
            }
            if added == "-" && removed == "-" {
                return Some(DiffFileStat {
                    path: path.to_owned(),
                    insertions: 0,
                    deletions: 0,
                    binary: true,
                });
            }
            Some(DiffFileStat {
                path: path.to_owned(),
                insertions: added.parse().ok()?,
                deletions: removed.parse().ok()?,
                binary: false,
            })
        })
        .collect()
}

fn truncate_patch(raw: &str, max_lines: usize, max_bytes: usize) -> (String, bool) {
    let mut out = String::new();
    let mut truncated = false;
    for (index, line) in raw.split_inclusive('\n').enumerate() {
        if index >= max_lines || out.len() + line.len() > max_bytes {
            truncated = true;
            break;
        }
        out.push_str(line);
    }
    (out.trim_end_matches('\n').to_owned(), truncated)
}

#[async_trait]
impl Workspaces for GitWorkspaces {
    async fn prepare(
        &self,
        task: TaskId,
        repositories: &[Repository],
    ) -> Result<Vec<PathBuf>, WorkspaceError> {
        let mut paths = Vec::with_capacity(repositories.len());
        for repository in repositories {
            check(repository)?;
            let _repo = self.lock_repository(repository.id).await;
            paths.push(self.worktree_of(task, repository).await?);
        }
        Ok(paths)
    }

    async fn verify_submission(
        &self,
        task: TaskId,
        repositories: &[Repository],
        revisions: &[MergeCheck],
    ) -> Result<(), WorkspaceError> {
        for repository in repositories {
            let _repo = self.lock_repository(repository.id).await;
            let head = git(
                Some(&self.worktree(task, repository)),
                ["rev-parse", "--verify", "HEAD"],
            )
            .await?;
            if !revisions.iter().any(|check| {
                check.repository == repository.name
                    && check.revision.as_deref() == Some(head.trim())
            }) {
                return Err(WorkspaceError(format!(
                    "{} changed after submission; submit the final revision again",
                    repository.name
                )));
            }
        }
        Ok(())
    }

    async fn prepare_review(
        &self,
        task: TaskId,
        submission: RunId,
        repositories: &[Repository],
        revisions: &[ReviewRevision],
    ) -> Result<Vec<PathBuf>, WorkspaceError> {
        if repositories.len() != revisions.len() {
            return Err(WorkspaceError(
                "review must pin one commit per repository".into(),
            ));
        }
        let mut paths = Vec::with_capacity(repositories.len());
        if repositories.is_empty() {
            return Ok(paths);
        }
        let root = tokio::fs::canonicalize(&self.root)
            .await
            .map_err(|error| io_error("resolving", &self.root, &error))?;
        let task_dir = root.join("worktrees").join(task.to_string());
        let reviews = task_dir.join(".reviews");
        let dir = reviews.join(submission.to_string());
        checked_dir(&root, &task_dir).await?;
        checked_dir(&root, &reviews).await?;
        checked_dir(&root, &dir).await?;
        for (repository, revision) in repositories.iter().zip(revisions) {
            check(repository)?;
            let commit = revision.commit.as_str();
            if repository.id != revision.repository || !valid_commit_id(commit) {
                return Err(WorkspaceError(format!(
                    "invalid review revision for {}",
                    repository.name
                )));
            }
            let _repo = self.lock_repository(repository.id).await;
            let path = dir.join(&repository.name);
            let marker = dir.join(format!(".{}.ready", repository.name));
            checked_dir(&root, &path).await?;
            if marker.is_file() && path.join(".git").is_file() {
                if git(Some(&path), ["rev-parse", "--abbrev-ref", "HEAD"])
                    .await?
                    .trim()
                    != "HEAD"
                    || git(Some(&path), ["rev-parse", "HEAD"]).await?.trim() != commit
                    || !git(
                        Some(&path),
                        ["status", "--porcelain", "--untracked-files=no"],
                    )
                    .await?
                    .is_empty()
                {
                    return Err(WorkspaceError(format!(
                        "review worktree for {} has changed",
                        repository.name
                    )));
                }
            } else {
                let clone = self.clone(repository).await?;
                remove_file(&marker).await?;
                remove_dir(&path).await?;
                create_dir(&dir).await?;
                git(Some(&clone), ["worktree", "prune", "--expire", "now"]).await?;
                git(
                    Some(&clone),
                    [
                        OsStr::new("worktree"),
                        OsStr::new("add"),
                        OsStr::new("--detach"),
                        OsStr::new("--"),
                        path.as_os_str(),
                        OsStr::new(commit),
                    ],
                )
                .await?;
                tokio::fs::write(&marker, b"").await.map_err(|error| {
                    io_error("marking the review worktree ready", &marker, &error)
                })?;
            }
            paths.push(path);
        }
        Ok(paths)
    }

    async fn checkpoint(
        &self,
        task: TaskId,
        repositories: &[Repository],
        message: &str,
    ) -> Result<(), WorkspaceError> {
        for repository in repositories {
            check(repository)?;
            let _repo = self.lock_repository(repository.id).await;
            if self.is_ready(task, repository) {
                self.commit(&self.worktree(task, repository), task, message)
                    .await?;
            }
        }
        Ok(())
    }
    async fn check_merge(
        &self,
        task: TaskId,
        repositories: &[Repository],
    ) -> Result<Vec<MergeCheck>, WorkspaceError> {
        let branch = format!("refs/heads/{}", task_branch(task));
        let mut checks = Vec::new();
        for repository in repositories {
            check(repository)?;
            let _repo = self.lock_repository(repository.id).await;
            if !self.is_ready(task, repository) {
                continue;
            }
            let commit = self.fetch_base(repository).await?;
            let worktree = self.worktree(task, repository);
            let revision = git(Some(&worktree), ["rev-parse", "--verify", branch.as_str()]).await?;
            let revision = revision.trim().to_owned();
            let diff = self
                .compute_diff_preview(repository, &worktree, &commit, &revision)
                .await
                .ok()
                .flatten();
            checks.push(MergeCheck {
                repository: repository.name.clone(),
                branch: repository.default_branch.clone(),
                conflicts: self.conflicts(repository, &commit, &revision).await?,
                base: commit,
                revision: Some(revision),
                diff,
            });
        }
        Ok(checks)
    }

    async fn bring_in_base(
        &self,
        task: TaskId,
        repositories: &[Repository],
    ) -> Result<Vec<MergeCheck>, WorkspaceError> {
        let branch = task_branch(task);
        let mut checks = Vec::new();
        for repository in repositories {
            check(repository)?;
            let _repo = self.lock_repository(repository.id).await;
            if !self.is_ready(task, repository) {
                continue;
            }
            let commit = self.fetch_base(repository).await?;
            let path = self.worktree(task, repository);
            checks.push(MergeCheck {
                repository: repository.name.clone(),
                branch: repository.default_branch.clone(),
                conflicts: self.rebase(repository, &path, &commit, &branch).await?,
                base: commit,
                revision: None,
                diff: None,
            });
        }
        Ok(checks)
    }

    async fn preview_publish(
        &self,
        task: TaskId,
        repository: &Repository,
    ) -> Result<PublishPreview, WorkspaceError> {
        check(repository)?;
        let _repo = self.lock_repository(repository.id).await;
        if !self.is_ready(task, repository) {
            return Err(WorkspaceError("no worktree was set up".into()));
        }
        let commit = self.fetch_base(repository).await?;
        let worktree = self.worktree(task, repository);
        let revision = branch_revision(&worktree, &task_branch(task)).await?;
        let diff = self
            .compute_diff_preview(repository, &worktree, &commit, &revision)
            .await?;
        Ok(PublishPreview { revision, diff })
    }

    async fn removal_counts(&self, removal: &WorkspaceRemoval) -> WorkspaceCounts {
        let _repos = self.lock_removal(removal).await;
        let mut counts = WorkspaceCounts::default();
        let Ok(root) = tokio::fs::canonicalize(&self.root).await else {
            return counts;
        };
        for task in &removal.tasks {
            for (_, name) in &removal.repositories {
                if valid_repository_name(name)
                    && matches!(
                        checked_dir(
                            &root,
                            &root.join("worktrees").join(task.to_string()).join(name)
                        )
                        .await,
                        Ok(true)
                    )
                {
                    counts.worktrees += 1;
                }
            }
        }
        for path in clone_paths(&root, removal) {
            if matches!(checked_dir(&root, &path).await, Ok(true)) {
                counts.clones += 1;
            }
        }
        counts
    }

    async fn remove(&self, removal: &WorkspaceRemoval) -> Vec<String> {
        let _repos = self.lock_removal(removal).await;
        let root = match tokio::fs::canonicalize(&self.root).await {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                tracing::warn!(%error, path = %self.root.display(), "workspace cleanup failed");
                return vec![self.root.display().to_string()];
            }
        };
        let mut leftovers = Vec::new();
        for task in &removal.tasks {
            let task_dir = root.join("worktrees").join(task.to_string());
            let result = remove_task_dir(&root, &task_dir, removal).await;
            report_leftover(&mut leftovers, &task_dir, result);
        }
        // Keep clones available so a later sweep can unregister any failed worktree.
        let task_failed = !leftovers.is_empty();
        for path in clone_paths(&root, removal) {
            let result = match checked_dir(&root, &path).await {
                Ok(false) => Ok(()),
                Ok(true) if !task_failed => remove_dir(&path).await,
                Ok(true) => Err(WorkspaceError("waiting for task worktree cleanup".into())),
                Err(error) => Err(error),
            };
            report_leftover(&mut leftovers, &path, result);
        }
        leftovers
    }
}

pub(crate) async fn branch_revision(
    worktree: &Path,
    branch: &str,
) -> Result<String, WorkspaceError> {
    let reference = format!("refs/heads/{branch}");
    Ok(git(Some(worktree), ["rev-parse", "--verify", &reference])
        .await?
        .trim()
        .to_owned())
}

fn clone_paths<'a>(
    root: &'a Path,
    removal: &'a WorkspaceRemoval,
) -> impl Iterator<Item = PathBuf> + 'a {
    removal.clones.iter().flat_map(|id| {
        [
            root.join("clones").join(id.to_string()),
            root.join("clones").join(format!("{id}.partial")),
        ]
    })
}

fn report_leftover(paths: &mut Vec<String>, path: &Path, result: Result<(), WorkspaceError>) {
    if let Err(error) = result {
        tracing::warn!(%error, path = %path.display(), "workspace cleanup failed");
        paths.push(path.display().to_string());
    }
}

async fn checked_dir(root: &Path, path: &Path) -> Result<bool, WorkspaceError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => {
            let resolved = tokio::fs::canonicalize(path)
                .await
                .map_err(|error| io_error("resolving", path, &error))?;
            if resolved == root
                || !resolved.starts_with(root)
                || resolved != path
                || !metadata.is_dir()
            {
                return Err(WorkspaceError(format!(
                    "Refusing to remove an unsafe workspace path: {}",
                    path.display()
                )));
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error("checking", path, &error)),
    }
}

async fn remove_task_dir(
    root: &Path,
    dir: &Path,
    removal: &WorkspaceRemoval,
) -> Result<(), WorkspaceError> {
    if !checked_dir(root, dir).await? {
        return Ok(());
    }
    let reviews = dir.join(".reviews");
    if checked_dir(root, &reviews).await? {
        let mut entries = tokio::fs::read_dir(&reviews)
            .await
            .map_err(|error| io_error("reading reviews in", &reviews, &error))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| io_error("reading reviews in", &reviews, &error))?
        {
            let round = entry.path();
            if checked_dir(root, &round).await? {
                remove_worktree_set(root, &round, removal).await?;
            }
        }
    }
    remove_worktree_set(root, dir, removal).await
}

async fn remove_worktree_set(
    root: &Path,
    dir: &Path,
    removal: &WorkspaceRemoval,
) -> Result<(), WorkspaceError> {
    for (repo, name) in &removal.repositories {
        if !valid_repository_name(name) {
            return Err(WorkspaceError(format!("Invalid worktree name {name:?}")));
        }
        let tree = dir.join(name);
        if !checked_dir(root, &tree).await? {
            continue;
        }
        let git_dir = root.join("clones").join(repo.to_string()).join(".git");
        if checked_dir(root, &git_dir).await? && tree.join(".git").is_file() {
            if tokio::fs::try_exists(git_dir.join("commondir"))
                .await
                .map_err(|error| {
                    io_error("checking the common git directory in", &git_dir, &error)
                })?
            {
                return Err(WorkspaceError(format!(
                    "Refusing a redirected common git directory in {}",
                    git_dir.display()
                )));
            }
            if checked_dir(root, &git_dir.join("worktrees")).await? {
                let mut entries = tokio::fs::read_dir(git_dir.join("worktrees"))
                    .await
                    .map_err(|error| io_error("reading registrations in", &git_dir, &error))?;
                while let Some(entry) = entries
                    .next_entry()
                    .await
                    .map_err(|error| io_error("reading registrations in", &git_dir, &error))?
                {
                    checked_dir(root, &entry.path()).await?;
                }
            }
            let args: Vec<OsString> = [
                OsStr::new("--git-dir"),
                git_dir.as_os_str(),
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                OsStr::new("--"),
                tree.as_os_str(),
            ]
            .into_iter()
            .map(OsStr::to_owned)
            .collect();
            let mut command = git_command(None, &args);
            command
                .env_remove("GIT_COMMON_DIR")
                .env_remove("GIT_WORK_TREE");
            run(command, "git", &args, GIT_TIMEOUT)
                .await
                .map_err(WorkspaceError)?;
        }
    }
    remove_dir(dir).await
}

/// Refuses a repository whose name or default branch `git` could misread.
fn check(repository: &Repository) -> Result<(), WorkspaceError> {
    if !valid_repository_name(&repository.name) {
        return Err(WorkspaceError(format!(
            "{:?} is not a valid repository name",
            repository.name
        )));
    }
    if !valid_branch_name(&repository.default_branch) {
        return Err(WorkspaceError(format!(
            "{:?} is not a valid branch name",
            repository.default_branch
        )));
    }
    Ok(())
}

/// Runs `git [-C dir] <args>` and returns its standard output if it
/// succeeds.
pub(crate) async fn git<I, S>(dir: Option<&Path>, args: I) -> Result<String, WorkspaceError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_owned())
        .collect();
    run(git_command(dir, &args), "git", &args, GIT_TIMEOUT)
        .await
        .map_err(WorkspaceError)
}

/// `git [-C dir] <args>`, kept from prompting. It sets no credentials and
/// reads none: `git` uses this user's logins (invariant 8).
pub(crate) fn git_command(dir: Option<&Path>, args: &[OsString]) -> Command {
    let mut command = Command::new("git");
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    // Fail rather than wait for a password nobody will type.
    command.args(args).env("GIT_TERMINAL_PROMPT", "0");
    command
}

/// Runs a prepared command with no input, bounded by `timeout`, and returns
/// its standard output if it succeeds. Shared by `git` and `gh`.
pub(crate) async fn run(
    mut command: Command,
    program: &str,
    args: &[OsString],
    timeout: Duration,
) -> Result<String, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let shown = args
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| format!("{program} {shown} timed out after {timeout:?}"))?
        .map_err(|error| format!("could not run {program} {shown}: {error}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    Err(format!(
        "{program} {shown} failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

async fn create_dir(dir: &Path) -> Result<(), WorkspaceError> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|error| io_error("creating", dir, &error))
}

async fn remove_dir(dir: &Path) -> Result<(), WorkspaceError> {
    match tokio::fs::remove_dir_all(dir).await {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Err(io_error("removing", dir, &error))
        }
        _ => Ok(()),
    }
}

async fn remove_file(file: &Path) -> Result<(), WorkspaceError> {
    match tokio::fs::remove_file(file).await {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Err(io_error("removing", file, &error))
        }
        _ => Ok(()),
    }
}

fn io_error(what: &str, path: &Path, error: &io::Error) -> WorkspaceError {
    WorkspaceError(format!("{what} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_root_is_made_absolute() {
        let workspaces = GitWorkspaces::new("data/workspaces");
        assert!(workspaces.root.is_absolute(), "{:?}", workspaces.root);
        assert!(workspaces.root.ends_with("data/workspaces"));
    }

    #[tokio::test]
    async fn locking_one_repository_leaves_other_repositories_available() {
        let workspaces = GitWorkspaces::new("data/workspaces");
        let _repo = workspaces.lock_repository(RepositoryId(1)).await;
        let time = Duration::from_millis(100);
        assert!(
            tokio::time::timeout(time, workspaces.lock_repository(RepositoryId(1)))
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(time, workspaces.lock_repository(RepositoryId(2)))
                .await
                .is_ok()
        );
    }

    #[test]
    fn parse_numstat_handles_text_and_binary_files() {
        let parsed = parse_numstat("12\t3\tsrc/main.rs\n-\t-\tassets/logo.png\n0\t5\tREADME.md\n");
        assert_eq!(
            parsed,
            vec![
                DiffFileStat {
                    path: "src/main.rs".into(),
                    insertions: 12,
                    deletions: 3,
                    binary: false,
                },
                DiffFileStat {
                    path: "assets/logo.png".into(),
                    insertions: 0,
                    deletions: 0,
                    binary: true,
                },
                DiffFileStat {
                    path: "README.md".into(),
                    insertions: 0,
                    deletions: 5,
                    binary: false,
                },
            ]
        );
    }

    #[test]
    fn truncate_patch_clips_by_lines_and_bytes() {
        let raw = "line1\nline2\nline3\nline4\n";
        let (clipped, truncated) = truncate_patch(raw, 2, 1024);
        assert!(truncated);
        assert_eq!(clipped, "line1\nline2");

        let (clipped_bytes, truncated_bytes) = truncate_patch(raw, 10, 12);
        assert!(truncated_bytes);
        assert_eq!(clipped_bytes, "line1\nline2");

        let (full, not_truncated) = truncate_patch(raw, 10, 1024);
        assert!(!not_truncated);
        assert_eq!(full, "line1\nline2\nline3\nline4");
    }
}
