//! [`GitForge`] in a task's worktree (docs/milestone-3.md §1 item 4): its
//! push with real `git` against a local bare repository, its pull requests
//! against a fake `gh` that records how it was started. No network.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use threadlane_core::forge::{ApprovedBranch, Forge};
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
    let revision = git(&worktree, &["rev-parse", "HEAD"]);
    let approved = ApprovedBranch {
        name: &branch,
        revision: revision.trim(),
    };
    forge.push(&worktree, approved).await.unwrap();
    forge.push(&worktree, approved).await.unwrap();
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

#[tokio::test]
async fn push_uses_the_approved_commit_even_if_the_local_ref_moves() {
    let dir = tempfile::tempdir().unwrap();
    let (worktree, remote) = worktree_with_work(dir.path()).await;
    let branch = task_branch(TASK);
    let revision = git(&worktree, &["rev-parse", "HEAD"]);
    git(
        &worktree,
        &["commit", "--allow-empty", "-m", "Later checkpoint"],
    );
    GitForge::default()
        .push(
            &worktree,
            ApprovedBranch {
                name: &branch,
                revision: revision.trim(),
            },
        )
        .await
        .unwrap();
    assert_eq!(git(&remote, &["rev-parse", &branch]), revision);
    assert_ne!(git(&worktree, &["rev-parse", &branch]), revision);
}

/// A fake `gh` that appends its arguments, NUL-separated and ended by a
/// line `<<end>>`, and its working directory to files in its own directory,
/// and prints a pull request URL for `pr create` and for `pr list` once one
/// was created. `api user` prints the login `me`, or fails if `login` is
/// false.
struct FakeGh {
    dir: TempDir,
}

impl FakeGh {
    fn new() -> Self {
        Self::with_login(true)
    }

    fn with_login(login: bool) -> Self {
        let api_user = if login {
            "echo me"
        } else {
            "echo 'HTTP 401' >&2; exit 1"
        };
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
             elif [ \"$3\" = \"--paginate\" ] && [ -f {record}/reviews ]; then\n\
               cat {record}/reviews\n\
             elif [ \"$1 $2\" = \"api user\" ]; then\n\
               {api_user}\n\
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
    let revision = git(&worktree, &["rev-parse", "HEAD"]);

    assert_eq!(forge.find_pull_request(&worktree, &branch).await, Ok(None));
    let url = forge
        .open_draft_pull_request(
            &worktree,
            ApprovedBranch {
                name: &branch,
                revision: revision.trim(),
            },
            "main",
            "--title=hijack",
            "Adds the page.\n\n- one\n- two\n",
            "Engineer, T-0001",
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
            ["api", "user", "--jq", ".login"]
                .map(str::to_owned)
                .to_vec(),
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
                "--body=Adds the page.\n\n- one\n- two\n\n\
                 🤖 Opened by Threadlane (Engineer, T-0001) acting for @me",
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

#[tokio::test]
async fn a_failed_login_lookup_keeps_the_agent_attribution_and_is_not_repeated() {
    let dir = tempfile::tempdir().unwrap();
    let (worktree, _remote) = worktree_with_work(dir.path()).await;
    let gh = FakeGh::with_login(false);
    let forge = GitForge::with_gh(gh.program());
    let branch = task_branch(TASK);
    let revision = git(&worktree, &["rev-parse", "HEAD"]);
    let approved = ApprovedBranch {
        name: &branch,
        revision: revision.trim(),
    };
    for _ in 0..2 {
        let url = forge
            .open_draft_pull_request(&worktree, approved, "main", "t", "Body", "Engineer, T-0001")
            .await
            .unwrap();
        assert_eq!(url, "https://github.test/me/app/pull/7");
    }
    let calls = gh.invocations();
    let lookups = calls.iter().filter(|call| call[0] == "api").count();
    assert_eq!(lookups, 1, "the login is looked up once: {calls:?}");
    let bodies: Vec<_> = calls
        .iter()
        .filter_map(|call| call.last())
        .filter(|arg| arg.starts_with("--body="))
        .collect();
    assert_eq!(
        bodies,
        ["--body=Body\n\n🤖 Opened by Threadlane (Engineer, T-0001)"; 2]
    );
}

#[tokio::test]
async fn a_retried_review_is_published_once_on_its_commit() {
    let gh = FakeGh::new();
    let forge = GitForge::with_gh(gh.program());
    let marker = "<!-- threadlane-review-test -->";
    let body = "Feedback\n\n🤖 Reviewed by Threadlane (Reviewer, T-0001)";
    for _ in 0..2 {
        forge
            .publish_review("https://github.test/me/app/pull/7", "abcd", body, "test")
            .await
            .unwrap();
        std::fs::write(gh.dir.path().join("reviews"), marker).unwrap();
    }
    let calls = gh.invocations();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0], calls[2]);
    assert_eq!(
        calls[1],
        [
            "api",
            "--hostname=github.test",
            "--method",
            "POST",
            "repos/me/app/pulls/7/reviews",
            "-f",
            "event=COMMENT",
            "-f",
            "commit_id=abcd",
            "-f",
            &format!("body={body}\n\n{marker}")
        ]
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

#[tokio::test]
async fn repository_actions_use_the_registered_remote_and_bind_text_to_flags() {
    use threadlane_core::forge::{CommentTarget, RepositoryAction};
    let gh = FakeGh::new();
    let forge = GitForge::with_gh(gh.program());
    let number = std::num::NonZeroU64::new(12).unwrap();
    let remote = "git@github.test:ACME/APP.git";
    for target in [CommentTarget::Issue, CommentTarget::PullRequest] {
        forge
            .apply(
                remote,
                &RepositoryAction::Comment {
                    target,
                    number,
                    body: "--help".into(),
                },
            )
            .await
            .unwrap();
    }
    forge
        .apply(
            remote,
            &RepositoryAction::CreateIssue {
                title: "Title".into(),
                body: "First line\nNext".into(),
            },
        )
        .await
        .unwrap();
    forge
        .apply(remote, &RepositoryAction::Merge { number })
        .await
        .unwrap();
    forge
        .apply(remote, &RepositoryAction::MarkReady { number })
        .await
        .unwrap();
    let repo = "--repo=github.test/acme/app";
    assert_eq!(
        gh.invocations(),
        [
            vec!["issue", "comment", "12", "--body=--help", repo],
            vec!["pr", "comment", "12", "--body=--help", repo],
            vec![
                "issue",
                "create",
                "--title=Title",
                "--body=First line\nNext",
                repo
            ],
            vec!["pr", "merge", "12", "--merge", repo],
            vec!["pr", "ready", "12", repo],
        ]
    );
}
