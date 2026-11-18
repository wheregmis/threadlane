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

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use threadlane_core::ids::TaskId;
use threadlane_core::model::Repository;
use threadlane_core::workspace::{
    MergeCheck, WorkspaceCounts, WorkspaceError, WorkspaceRemoval, Workspaces, task_branch,
    valid_branch_name, valid_repository_name,
};
use tokio::process::Command;
use tokio::sync::Mutex;

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
    /// Lanes run in parallel, and two tasks of one repository share its
    /// clone. shortcut: one lock for every setup, not one per clone; make it
    /// per clone if setups ever queue behind slow clones.
    setup: Arc<Mutex<()>>,
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
            setup: Arc::default(),
        }
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

    /// Rebases the worktree at `path` onto `base` if it rebases cleanly and
    /// has nothing uncommitted; otherwise leaves it as it was. Returns the
    /// files that conflict.
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
        let dirty = !git(Some(path), ["status", "--porcelain"]).await?.is_empty();
        if dirty {
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
}

#[async_trait]
impl Workspaces for GitWorkspaces {
    async fn prepare(
        &self,
        task: TaskId,
        repositories: &[Repository],
    ) -> Result<Vec<PathBuf>, WorkspaceError> {
        let _setup = self.setup.lock().await;
        let mut paths = Vec::with_capacity(repositories.len());
        for repository in repositories {
            check(repository)?;
            paths.push(self.worktree_of(task, repository).await?);
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
        let _setup = self.setup.lock().await;
        let branch = task_branch(task);
        let mut checks = Vec::new();
        for repository in repositories {
            check(repository)?;
            if !self.is_ready(task, repository) {
                continue;
            }
            let commit = self.fetch_base(repository).await?;
            checks.push(MergeCheck {
                repository: repository.name.clone(),
                branch: repository.default_branch.clone(),
                conflicts: self.conflicts(repository, &commit, &branch).await?,
                base: commit,
            });
        }
        Ok(checks)
    }

    async fn bring_in_base(
        &self,
        task: TaskId,
        repositories: &[Repository],
    ) -> Result<Vec<MergeCheck>, WorkspaceError> {
        let _setup = self.setup.lock().await;
        let branch = task_branch(task);
        let mut checks = Vec::new();
        for repository in repositories {
            check(repository)?;
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
            });
        }
        Ok(checks)
    }

    async fn removal_counts(&self, removal: &WorkspaceRemoval) -> WorkspaceCounts {
        let _setup = self.setup.lock().await;
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
        let _setup = self.setup.lock().await;
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
}
