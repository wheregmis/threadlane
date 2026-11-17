//! [`GitForge`] in a task's worktree (docs/milestone-3.md §1 item 4): its
//! push with real `git` against a local bare repository, its pull requests
//! against a fake `gh` that records how it was started. No network.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use threadlane_core::forge::Forge;
use threadlane_core::ids::{ProjectId, RepositoryId, TaskId};
use threadlane_core::model::Repository;
use threadlane_core::workspace::{Workspaces, task_branch};
use threadlane_runner::{GitForge, GitWorkspaces};
use threadlane_testkit::git::{bare_repository, git};

const TASK: TaskId = TaskId(1);

/// A task's worktree of `app`, with one commit of work on its branch, and
/// the bare repository it was cloned from.
async fn worktree_with_work(dir: &Path) -> (PathBuf, PathBuf) {
    let remote = bare_repository(&dir.join("remotes"), "app");
    let repository = Repository {
        id: RepositoryId(1),
        project_id: ProjectId(1),
        name: "app".into(),
        remote_url: remote.display().to_string(),
        default_branch: "main".into(),
        protected_branches: Vec::new(),
    };
    let workspaces = GitWorkspaces::new(dir.join("workspaces"));
    let worktree = workspaces
        .prepare(TASK, std::slice::from_ref(&repository))
        .await
        .unwrap()
        .remove(0);
    std::fs::write(worktree.join("page.html"), "<h1>Page</h1>\n").unwrap();
    workspaces
        .checkpoint(TASK, &[repository], "Add the page")
        .await
        .unwrap();
    (worktree, remote)
}

#[tokio::test]
async fn push_publishes_the_task_branch_to_the_remote_and_is_safe_to_repeat() {
    let dir = tempfile::tempdir().unwrap();
    let (worktree, remote) = worktree_with_work(dir.path()).await;
    let branch = task_branch(TASK);
    let forge = GitForge::default();
    forge.push(&worktree, &branch).await.unwrap();
    forge.push(&worktree, &branch).await.unwrap();
    assert_eq!(
        git(&remote, &["rev-parse", &branch]),
        git(&worktree, &["rev-parse", "HEAD"]),
        "the remote has the task's work"
    );
    assert_eq!(
        git(&remote, &["rev-parse", "main"]),
        git(&worktree, &["rev-parse", "HEAD~1"]),
        "and nothing else moved"
    );
}

/// A fake `gh` that appends its arguments, NUL-separated and ended by a
/// line `<<end>>`, and its working directory to files in its own directory,
/// and prints a pull request URL for `pr create` and for `pr list` once one
/// was created.
struct FakeGh {
    dir: TempDir,
}

impl FakeGh {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().display().to_string();
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\0' \"$@\" >> {record}/args\n\
             printf '<<end>>\\n' >> {record}/args\n\
             pwd -P >> {record}/cwd\n\
             if [ \"$1 $2\" = \"pr create\" ]; then\n\
               touch {record}/created\n\
               echo 'Creating draft pull request for threadlane/T-0001 into main'\n\
               echo 'https://github.test/me/app/pull/7'\n\
             elif [ \"$1 $2\" = \"pr list\" ] && [ -f {record}/created ]; then\n\
               echo 'https://github.test/me/app/pull/7'\n\
             fi\n"
        );
        let program = dir.path().join("gh");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    fn program(&self) -> PathBuf {
        self.dir.path().join("gh")
    }

    /// The arguments of each invocation, in order.
    fn invocations(&self) -> Vec<Vec<String>> {
        let recorded = std::fs::read_to_string(self.dir.path().join("args")).unwrap_or_default();
        recorded
            .split("<<end>>\n")
            .filter(|call| !call.is_empty())
            .map(|call| {
                call.trim_end_matches('\0')
                    .split('\0')
                    .map(str::to_owned)
                    .collect()
            })
            .collect()
    }

    fn working_dirs(&self) -> Vec<PathBuf> {
        std::fs::read_to_string(self.dir.path().join("cwd"))
            .unwrap_or_default()
            .lines()
            .map(PathBuf::from)
            .collect()
    }
}

#[tokio::test]
async fn a_draft_pull_request_is_opened_with_exact_arguments_after_the_push() {
    let dir = tempfile::tempdir().unwrap();
    let (worktree, remote) = worktree_with_work(dir.path()).await;
    let gh = FakeGh::new();
    let forge = GitForge::with_gh(gh.program());
    let branch = task_branch(TASK);

    assert_eq!(forge.find_pull_request(&worktree, &branch).await, Ok(None));
    let url = forge
        .open_draft_pull_request(
            &worktree,
            &branch,
            "main",
            "--title=hijack",
            "Adds the page.\n\n- one\n- two",
        )
        .await
        .unwrap();
    assert_eq!(url, "https://github.test/me/app/pull/7");
    assert_eq!(
        forge.find_pull_request(&worktree, &branch).await,
        Ok(Some(url))
    );

    let list = [
        "pr",
        "list",
        "--head",
        "threadlane/T-0001",
        "--state",
        "all",
        "--limit",
        "1",
        "--json",
        "url",
        "--jq",
        ".[].url",
    ];
    assert_eq!(
        gh.invocations(),
        vec![
            list.map(str::to_owned).to_vec(),
            [
                "pr",
                "create",
                "--draft",
                "--head",
                "threadlane/T-0001",
                "--base",
                "main",
                // Bound to their option, so a title cannot pass as one.
                "--title=--title=hijack",
                "--body=Adds the page.\n\n- one\n- two",
            ]
            .map(str::to_owned)
            .to_vec(),
            list.map(str::to_owned).to_vec(),
        ]
    );
    let worktree = worktree.canonicalize().unwrap();
    assert!(
        gh.working_dirs().iter().all(|cwd| *cwd == worktree),
        "gh runs in the task's worktree: {:?}",
        gh.working_dirs()
    );
    assert_eq!(
        git(&remote, &["rev-parse", &branch]),
        git(&worktree, &["rev-parse", "HEAD"]),
        "opening a pull request publishes its branch first"
    );
}

/// A fake `gh` whose `auth status` prints token-like lines and exits with
/// `code`, recording its arguments.
fn gh_auth(code: u8) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("gh");
    let script = format!(
        "#!/bin/sh\nprintf '%s ' \"$@\" >> {}/args\n\
         echo '  - Token: gho_************************************'\n\
         exit {code}\n",
        dir.path().display()
    );
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, program)
}

#[tokio::test]
async fn the_gh_login_is_read_from_the_exit_status_of_gh_auth_status_alone() {
    let (dir, program) = gh_auth(0);
    assert!(GitForge::with_gh(&program).logged_in().await);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("args")).unwrap(),
        "auth status "
    );
    let (_dir, program) = gh_auth(1);
    assert!(!GitForge::with_gh(program).logged_in().await);
    assert!(
        !GitForge::with_gh("/nonexistent/gh").logged_in().await,
        "no gh is no login"
    );
}
