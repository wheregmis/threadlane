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
             elif [ \"$3\" = \"--method=GET\" ] && echo \"$4\" | grep -q /git/ref/; then\n\
               cat {record}/ref.json 2>/dev/null || {{ echo 'gh: Not Found (HTTP 404)' >&2; exit 1; }}\n\
             elif [ \"$3\" = \"--method=GET\" ] && echo \"$4\" | grep -q /compare/; then\n\
               cat {record}/compare.json\n\
             elif [ \"$3\" = graphql ] && [ -f {record}/thread.json ]; then\n\
               cat {record}/thread.json\n\
             elif [ \"$3\" = \"--method=GET\" ] && echo \"$4\" | grep -q /pulls/; then\n\
               cat {record}/pull.json\n\
             elif [ \"$1 $2\" = \"api user\" ]; then\n\
               {api_user}\n\
             elif [ \"$1\" = api ]; then\n\
               for last; do :; done\n\
               case \"$3 $last\" in\n\
                 '--method=GET repos/acme/app/issues/12') echo '{{\"number\":12}}' ;;\n\
                 '--method=GET repos/acme/app/issues/13') echo '{{\"pull_request\":{{}}}}' ;;\n\
                 '--method=GET repos/acme/app/labels') printf 'bug\\nNeeds Review\\n' ;;\n\
                 '--method=DELETE repos/acme/app/issues/12/labels/gone') echo 'Not Found (HTTP 404)' >&2; exit 1 ;;\n\
               esac\n\
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
    for (title, body) in [
        (Some("New title"), Some("Head is now abc")),
        (None, Some("-x")),
    ] {
        forge
            .apply(
                remote,
                &RepositoryAction::EditPullRequest {
                    number,
                    title: title.map(str::to_owned),
                    body: body.map(str::to_owned),
                },
            )
            .await
            .unwrap();
    }
    forge
        .apply(
            remote,
            &RepositoryAction::GhCli {
                args: ["issue", "edit", "12", "--add-label=bug"]
                    .map(str::to_owned)
                    .to_vec(),
            },
        )
        .await
        .unwrap();
    forge
        .apply(remote, &RepositoryAction::AssignIssue { number })
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
            vec![
                "pr",
                "edit",
                "12",
                "--title=New title",
                "--body=Head is now abc",
                repo
            ],
            vec!["pr", "edit", "12", "--body=-x", repo],
            vec!["issue", "edit", "12", "--add-label=bug", repo],
            vec!["issue", "edit", "12", "--add-assignee=@me", repo],
        ]
    );
}

#[tokio::test]
async fn issue_labels_use_gh_api_and_report_labels_the_repository_lacks() {
    use threadlane_core::forge::RepositoryAction;
    let gh = FakeGh::new();
    let forge = GitForge::with_gh(gh.program());
    let remote = "git@github.test:ACME/APP.git";
    let label = |number: u64| RepositoryAction::LabelIssue {
        number: std::num::NonZeroU64::new(number).unwrap(),
        add: vec!["needs review".into(), "bug".into(), "nope".into()],
        remove: vec!["gone".into(), "area: hub".into()],
    };
    let report = forge.apply(remote, &label(12)).await.unwrap();
    assert_eq!(
        report,
        "Issue #12 in github.test/acme/app: Added needs review, bug. Not added, because \
         github.test/acme/app has no such label: nope. Removed gone, area: hub."
    );
    let host = "--hostname=github.test";
    assert_eq!(
        gh.invocations(),
        [
            vec!["api", host, "--method=GET", "repos/acme/app/issues/12"],
            vec![
                "api",
                host,
                "--method=GET",
                "--paginate",
                "--jq=.[].name",
                "repos/acme/app/labels"
            ],
            vec![
                "api",
                host,
                "--method=POST",
                "--raw-field=labels[]=needs review",
                "--raw-field=labels[]=bug",
                "repos/acme/app/issues/12/labels"
            ],
            vec![
                "api",
                host,
                "--method=DELETE",
                "repos/acme/app/issues/12/labels/gone"
            ],
            vec![
                "api",
                host,
                "--method=DELETE",
                "repos/acme/app/issues/12/labels/area%3A%20hub"
            ],
        ],
        "labels the repository lacks are never sent, so none is created"
    );

    let gh = FakeGh::new();
    let forge = GitForge::with_gh(gh.program());
    let error = forge.apply(remote, &label(13)).await.unwrap_err();
    assert!(error.0.contains("is a pull request"), "{error}");
    assert_eq!(
        gh.invocations().len(),
        1,
        "nothing is changed on a pull request"
    );
}

#[tokio::test]
async fn a_branch_is_deleted_only_while_it_still_points_at_the_expected_head() {
    use threadlane_core::forge::RepositoryAction;
    let dir = tempfile::tempdir().unwrap();
    let bare = bare_repository(dir.path(), "app");
    let remote = "https://github.test/acme/app.git";
    // Real `git`, with the forge's remote rewritten to the local bare repository.
    let fake_git = dir.path().join("git");
    std::fs::write(
        &fake_git,
        format!(
            "#!/bin/sh\nexec git -c url.{}.insteadOf={remote} \"$@\"\n",
            bare.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();
    let head = git(&bare, &["rev-parse", "main"]).trim().to_owned();
    let moved = git(
        &bare,
        &["commit-tree", "main^{tree}", "-p", "main", "-m", "moved"],
    )
    .trim()
    .to_owned();
    let close = RepositoryAction::ClosePullRequest {
        number: 12.try_into().unwrap(),
        expected_head: head.clone(),
        delete_branch: true,
    };
    let gh = || {
        let gh = FakeGh::new();
        let pull = serde_json::json!({
            "state": "open",
            "base": {"ref": "main"},
            "head": {"sha": head, "ref": "dup/pr", "repo": {"full_name": "acme/app", "default_branch": "main"}}
        });
        std::fs::write(gh.dir.path().join("pull.json"), pull.to_string()).unwrap();
        // GitHub's view of the branch, read before the push.
        let live = serde_json::json!({"object": {"sha": head}});
        std::fs::write(gh.dir.path().join("ref.json"), live.to_string()).unwrap();
        gh
    };

    // Still at the expected head: closed, then deleted.
    git(&bare, &["update-ref", "refs/heads/dup/pr", &head]);
    let fake_gh = gh();
    let forge = GitForge::with_gh(fake_gh.program()).with_git(&fake_git);
    let done = forge.apply(remote, &close).await.unwrap();
    assert!(done.contains("and deleted its branch dup/pr"), "{done}");
    assert_eq!(git(&bare, &["branch", "--list", "dup/pr"]), "");
    assert!(
        !fake_gh
            .invocations()
            .iter()
            .any(|argv| argv.contains(&"--method=DELETE".into())),
        "the branch is deleted by the leased push, not the API"
    );

    // Pushed to after the check: the lease keeps the push, and the close is still reported.
    git(&bare, &["update-ref", "refs/heads/dup/pr", &moved]);
    let fake_gh = gh();
    let forge = GitForge::with_gh(fake_gh.program()).with_git(&fake_git);
    let done = forge.apply(remote, &close).await.unwrap();
    assert!(done.starts_with("Closed pull request #12"), "{done}");
    assert!(done.contains("Warning: kept branch dup/pr"), "{done}");
    assert_eq!(git(&bare, &["rev-parse", "dup/pr"]).trim(), moved);
}

#[tokio::test]
async fn a_close_is_refused_once_the_head_moved_and_deletes_only_a_same_repository_branch() {
    use threadlane_core::forge::RepositoryAction;
    let head = "0123456789abcdef0123456789abcdef01234567";
    let close = |delete_branch| RepositoryAction::ClosePullRequest {
        number: 12.try_into().unwrap(),
        expected_head: head.into(),
        delete_branch,
    };
    let pull = |sha: &str, state: &str, owner: &str, branch: &str| {
        serde_json::json!({
            "state": state,
            "base": {"ref": "main"},
            "head": {"sha": sha, "ref": branch, "repo": {"full_name": format!("{owner}/app"), "default_branch": "main"}}
        })
        .to_string()
    };
    let remote = "https://github.test/acme/app.git";
    let get = vec![
        "api",
        "--hostname=github.test",
        "--method=GET",
        "repos/acme/app/pulls/12",
    ];
    let patch = vec![
        "api",
        "--hostname=github.test",
        "--method=PATCH",
        "repos/acme/app/pulls/12",
        "-f",
        "state=closed",
    ];
    let get_ref = vec![
        "api",
        "--hostname=github.test",
        "--method=GET",
        "repos/acme/app/git/ref/heads/dup/pr",
    ];

    // The head moved since it was shown: nothing is changed.
    let gh = FakeGh::new();
    let moved = "1123456789abcdef0123456789abcdef01234567";
    std::fs::write(
        gh.dir.path().join("pull.json"),
        pull(moved, "open", "Acme", "dup/pr"),
    )
    .unwrap();
    let forge = GitForge::with_gh(gh.program());
    let error = forge.apply(remote, &close(true)).await.unwrap_err();
    assert!(error.0.contains("no longer has expected head"), "{error}");
    assert_eq!(gh.invocations(), std::slice::from_ref(&get));

    let ref_json = |sha: &str| serde_json::json!({"object": {"sha": sha}}).to_string();

    // A reused branch whose ref moved past the closed PR's frozen head is kept.
    let gh = FakeGh::new();
    std::fs::write(
        gh.dir.path().join("pull.json"),
        pull(head, "closed", "acme", "dup/pr"),
    )
    .unwrap();
    std::fs::write(gh.dir.path().join("ref.json"), ref_json(moved)).unwrap();
    let forge = GitForge::with_gh(gh.program());
    let done = forge.apply(remote, &close(true)).await.unwrap();
    assert!(done.contains("Warning: kept branch dup/pr"), "{done}");
    assert_eq!(gh.invocations(), [get.clone(), get_ref.clone()]);

    // A branch already gone (404) counts as deleted.
    let gh = FakeGh::new();
    std::fs::write(
        gh.dir.path().join("pull.json"),
        pull(head, "open", "acme", "dup/pr"),
    )
    .unwrap();
    let forge = GitForge::with_gh(gh.program());
    let done = forge.apply(remote, &close(true)).await.unwrap();
    assert!(done.contains("already deleted"), "{done}");
    assert_eq!(gh.invocations(), [get.clone(), patch, get_ref]);

    // A merged PR is reported as merged; a null head repository means its branch is gone.
    let gh = FakeGh::new();
    std::fs::write(
        gh.dir.path().join("pull.json"),
        serde_json::json!({
            "state": "closed",
            "merged": true,
            "base": {"ref": "main"},
            "head": {"sha": head, "ref": "dup/pr", "repo": null}
        })
        .to_string(),
    )
    .unwrap();
    let forge = GitForge::with_gh(gh.program());
    let done = forge.apply(remote, &close(true)).await.unwrap();
    assert!(done.contains("already merged"), "{done}");
    assert_eq!(gh.invocations(), std::slice::from_ref(&get));

    // Already closed, as on a retry, and the branch kept.
    let gh = FakeGh::new();
    std::fs::write(
        gh.dir.path().join("pull.json"),
        pull(head, "closed", "acme", "dup/pr"),
    )
    .unwrap();
    let forge = GitForge::with_gh(gh.program());
    forge.apply(remote, &close(false)).await.unwrap();
    assert_eq!(gh.invocations(), std::slice::from_ref(&get));

    // A fork's branch, or the base branch, is never deleted.
    for (owner, branch) in [("fork", "dup/pr"), ("acme", "main")] {
        let gh = FakeGh::new();
        std::fs::write(
            gh.dir.path().join("pull.json"),
            pull(head, "open", owner, branch),
        )
        .unwrap();
        let forge = GitForge::with_gh(gh.program());
        assert!(
            forge.apply(remote, &close(true)).await.is_err(),
            "{owner} {branch}"
        );
        assert_eq!(gh.invocations(), std::slice::from_ref(&get));
    }
}

const PR_HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const OLD_HEAD: &str = "1123456789abcdef0123456789abcdef01234567";

/// A fake `gh` whose PR #12 has head `head`.
fn gh_at(head: &str) -> FakeGh {
    let gh = FakeGh::new();
    let pull = serde_json::json!({"state": "open", "head": {"sha": head}});
    std::fs::write(gh.dir.path().join("pull.json"), pull.to_string()).unwrap();
    gh
}

fn get_pull() -> Vec<String> {
    [
        "api",
        "--hostname=github.test",
        "--method=GET",
        "repos/acme/app/pulls/12",
    ]
    .map(str::to_owned)
    .to_vec()
}

#[tokio::test]
async fn a_review_thread_is_resolved_only_at_the_approved_head_on_its_own_pull_request() {
    use threadlane_core::forge::RepositoryAction;
    let remote = "https://github.test/acme/app.git";
    let fixed_by = "2223456789abcdef0123456789abcdef01234567";
    let resolve = RepositoryAction::ResolveReviewThread {
        number: 12.try_into().unwrap(),
        expected_head: PR_HEAD.into(),
        thread_id: "PRRT_kw1".into(),
        fixed_by: fixed_by.into(),
        body: "Guarded the null case.".into(),
    };
    let thread = |number: u64, owner: &str, resolved: bool| {
        serde_json::json!({"data": {"node": {"isResolved": resolved,
            "pullRequest": {"number": number, "repository": {"nameWithOwner": format!("{owner}/app")}}}}})
        .to_string()
    };
    let setup = |head: &str, status: &str, thread: String| {
        let gh = gh_at(head);
        let compare = serde_json::json!({"status": status}).to_string();
        std::fs::write(gh.dir.path().join("compare.json"), compare).unwrap();
        std::fs::write(gh.dir.path().join("thread.json"), thread).unwrap();
        gh
    };

    // The head moved past the approval: nothing else is asked or changed.
    let gh = setup(OLD_HEAD, "ahead", thread(12, "acme", false));
    let error = GitForge::with_gh(gh.program())
        .apply(remote, &resolve)
        .await
        .unwrap_err();
    assert!(error.0.contains("no longer has expected head"), "{error}");
    assert_eq!(gh.invocations(), [get_pull()]);

    // The head does not contain the fixing commit.
    let gh = setup(PR_HEAD, "diverged", thread(12, "acme", false));
    let error = GitForge::with_gh(gh.program())
        .apply(remote, &resolve)
        .await
        .unwrap_err();
    assert!(error.0.contains("does not contain"), "{error}");
    assert_eq!(gh.invocations().len(), 2);

    // A thread of another pull request or repository is refused.
    for other in [thread(13, "acme", false), thread(12, "evil", false)] {
        let gh = setup(PR_HEAD, "ahead", other);
        let error = GitForge::with_gh(gh.program())
            .apply(remote, &resolve)
            .await
            .unwrap_err();
        assert!(error.0.contains("not a review thread of PR #12"), "{error}");
        assert_eq!(gh.invocations().len(), 3);
    }

    // An already resolved thread, as on a retry, gets no second reply.
    let gh = setup(PR_HEAD, "identical", thread(12, "Acme", true));
    let done = GitForge::with_gh(gh.program())
        .apply(remote, &resolve)
        .await
        .unwrap();
    assert!(done.contains("already resolved"), "{done}");
    assert_eq!(gh.invocations().len(), 3);

    // At the head and containing the fix: one reply naming the commit, then the resolve.
    let gh = setup(PR_HEAD, "ahead", thread(12, "acme", false));
    let done = GitForge::with_gh(gh.program())
        .apply(remote, &resolve)
        .await
        .unwrap();
    assert!(
        done.starts_with("Resolved review thread PRRT_kw1"),
        "{done}"
    );
    let calls = gh.invocations();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0], get_pull());
    assert_eq!(
        calls[1][3],
        format!("repos/acme/app/compare/{fixed_by}...{PR_HEAD}")
    );
    let mutation = &calls[3];
    assert!(mutation[4].contains("resolveReviewThread"), "{mutation:?}");
    assert_eq!(mutation[6], "id=PRRT_kw1");
    assert_eq!(
        mutation[8],
        format!("body=Guarded the null case.\n\nFixed in {fixed_by}.")
    );
}

#[tokio::test]
async fn a_fresh_review_is_requested_only_at_the_approved_head() {
    use threadlane_core::forge::RepositoryAction;
    let remote = "https://github.test/acme/app.git";
    let request = |reviewers: &[&str], comment: Option<&str>| RepositoryAction::RequestReview {
        number: 12.try_into().unwrap(),
        expected_head: PR_HEAD.into(),
        reviewers: reviewers.iter().map(|r| (*r).to_owned()).collect(),
        comment: comment.map(str::to_owned),
    };

    let gh = gh_at(OLD_HEAD);
    let error = GitForge::with_gh(gh.program())
        .apply(remote, &request(&["octocat"], Some("@coderabbitai review")))
        .await
        .unwrap_err();
    assert!(error.0.contains("no longer has expected head"), "{error}");
    assert_eq!(gh.invocations(), [get_pull()]);

    let gh = gh_at(PR_HEAD);
    let done = GitForge::with_gh(gh.program())
        .apply(
            remote,
            &request(&["octocat", "hubot"], Some("@coderabbitai review")),
        )
        .await
        .unwrap();
    assert!(done.contains("at 0123456789abcdef"), "{done}");
    assert_eq!(
        gh.invocations(),
        [
            get_pull(),
            [
                "api",
                "--hostname=github.test",
                "--method=POST",
                "repos/acme/app/pulls/12/requested_reviewers",
                "-f",
                "reviewers[]=octocat",
                "-f",
                "reviewers[]=hubot",
            ]
            .map(str::to_owned)
            .to_vec(),
            [
                "api",
                "--hostname=github.test",
                "--method=POST",
                "repos/acme/app/issues/12/comments",
                "-f",
                "body=@coderabbitai review",
            ]
            .map(str::to_owned)
            .to_vec(),
        ]
    );

    // A comment alone posts only the comment.
    let gh = gh_at(PR_HEAD);
    GitForge::with_gh(gh.program())
        .apply(remote, &request(&[], Some("@coderabbitai review")))
        .await
        .unwrap();
    assert_eq!(gh.invocations().len(), 2);
}

#[tokio::test]
async fn a_base_missing_from_the_remote_is_refused_before_anything_is_pushed() {
    let dir = tempfile::tempdir().unwrap();
    let (worktree, remote) = worktree_with_work(dir.path()).await;
    let gh = FakeGh::new();
    let forge = GitForge::with_gh(gh.program());
    let branch = task_branch(TASK);
    let revision = git(&worktree, &["rev-parse", "HEAD"]);
    let approved = ApprovedBranch {
        name: &branch,
        revision: revision.trim(),
    };
    let error = forge
        .open_draft_pull_request(
            &worktree,
            approved,
            "release",
            "t",
            "Body",
            "Engineer, T-0001",
        )
        .await
        .unwrap_err();
    assert!(error.0.contains("\"release\" does not exist"), "{error:?}");
    assert!(gh.invocations().is_empty(), "no pull request asked for");
    assert!(
        std::process::Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", &branch])
            .current_dir(&remote)
            .output()
            .unwrap()
            .stdout
            .is_empty(),
        "nothing pushed"
    );

    git(&remote, &["branch", "release", "main"]);
    forge
        .open_draft_pull_request(
            &worktree,
            approved,
            "release",
            "t",
            "Body",
            "Engineer, T-0001",
        )
        .await
        .unwrap();
    let create = gh
        .invocations()
        .into_iter()
        .find(|call| call[1] == "create")
        .unwrap();
    assert_eq!(create[5..7], ["--base", "release"]);
}
