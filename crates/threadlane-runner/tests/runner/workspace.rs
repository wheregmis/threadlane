//! [`GitWorkspaces`] against local bare repositories (Plan.md §5.5,
//! docs/milestone-3.md §3): real `git`, no network.

use std::path::Path;

use serde_json::json;
use tempfile::TempDir;
use threadlane_core::ids::{ProjectId, RepositoryId, RunId, TaskId};
use threadlane_core::link::{PROGRESS_WRITTEN, PullRequestReview, RunReview};
use threadlane_core::model::{LaneKind, Repository};
use threadlane_core::workspace::{ReviewCheckout, ReviewRevision, Workspaces};
use threadlane_runner::GitWorkspaces;
use threadlane_testkit::Script;
use threadlane_testkit::git::{bare_repository, git};

use crate::support::{Call, Harness, LANE, assignment, lease, run};

const T1: TaskId = TaskId(1);
const T2: TaskId = TaskId(2);

struct Setup {
    dir: TempDir,
    workspaces: GitWorkspaces,
    repository: Repository,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().expect("temporary directory");
    let remote = bare_repository(&dir.path().join("remotes"), "app");
    let workspaces = GitWorkspaces::new(dir.path().join("workspaces"));
    let repository = Repository {
        id: RepositoryId(4),
        project_id: ProjectId(1),
        name: "app".into(),
        remote_url: remote.display().to_string(),
        default_branch: "main".into(),
        protected_branches: Vec::new(),
    };
    Setup {
        dir,
        workspaces,
        repository,
    }
}

impl Setup {
    async fn prepare(&self, task: TaskId) -> std::path::PathBuf {
        let mut paths = self
            .workspaces
            .prepare(task, std::slice::from_ref(&self.repository))
            .await
            .expect("prepare");
        assert_eq!(paths.len(), 1);
        paths.remove(0)
    }

    async fn checkpoint(&self, task: TaskId) {
        self.workspaces
            .checkpoint(
                task,
                std::slice::from_ref(&self.repository),
                "Save the work",
            )
            .await
            .expect("checkpoint");
    }

    fn clones(&self) -> std::path::PathBuf {
        self.dir.path().join("workspaces").join("clones")
    }
}

fn branch(path: &Path) -> String {
    git(path, &["symbolic-ref", "--short", "HEAD"])
        .trim()
        .to_owned()
}

fn commits(path: &Path) -> usize {
    git(path, &["rev-list", "--count", "HEAD"])
        .trim()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn a_task_gets_its_own_worktree_on_its_own_branch_and_setting_up_again_reuses_it() {
    let s = setup();
    let path = s.prepare(T1).await;
    assert_eq!(
        path,
        s.dir.path().join("workspaces/worktrees/T-0001/app"),
        "one worktree per task per repository, under the data directory"
    );
    assert_eq!(branch(&path), "threadlane/T-0001");
    assert_eq!(
        std::fs::read_to_string(path.join("README.md")).unwrap(),
        "# app\n"
    );

    std::fs::write(path.join("work.txt"), "half done").unwrap();
    assert_eq!(s.prepare(T1).await, path, "the same worktree");
    assert_eq!(
        std::fs::read_to_string(path.join("work.txt")).unwrap(),
        "half done",
        "reused as it is"
    );

    let other = s.prepare(T2).await;
    assert_ne!(other, path, "two tasks never share a worktree");
    assert_eq!(branch(&other), "threadlane/T-0002");
    let clones: Vec<_> = std::fs::read_dir(s.clones()).unwrap().collect();
    assert_eq!(clones.len(), 1, "one clone per repository");
}

#[tokio::test]
async fn ending_a_run_commits_uncommitted_work_on_the_task_branch_and_a_clean_tree_nothing() {
    let s = setup();
    let path = s.prepare(T1).await;
    s.checkpoint(T1).await;
    assert_eq!(commits(&path), 1, "nothing to commit, no commit");

    std::fs::write(path.join("work.txt"), "done").unwrap();
    std::fs::write(path.join("README.md"), "# app, changed\n").unwrap();
    s.checkpoint(T1).await;
    assert_eq!(commits(&path), 2);
    assert_eq!(git(&path, &["status", "--porcelain"]), "");
    let last = git(
        &path,
        &["log", "-1", "--format=%an <%ae>|%s", "threadlane/T-0001"],
    );
    assert_eq!(
        last.trim(),
        "Threadlane <threadlane@localhost>|Save the work"
    );

    s.checkpoint(T1).await;
    assert_eq!(commits(&path), 2, "still nothing new");
    s.workspaces
        .checkpoint(T2, std::slice::from_ref(&s.repository), "x")
        .await
        .expect("a worktree never set up is skipped");
}

#[tokio::test]
async fn an_unfinished_run_commits_tracked_changes_and_leaves_scratch_files_out() {
    let s = setup();
    let path = s.prepare(T1).await;
    std::fs::write(path.join("README.md"), "# app, changed\n").unwrap();
    std::fs::write(path.join("scratch.sh"), "sleep 600").unwrap();
    s.workspaces
        .checkpoint_tracked(T1, std::slice::from_ref(&s.repository), "Timed out")
        .await
        .expect("checkpoint");
    assert_eq!(commits(&path), 2, "the tracked change is kept");
    assert_eq!(
        git(&path, &["status", "--porcelain"]),
        "?? scratch.sh\n",
        "the scratch file stays in the worktree, uncommitted"
    );
}

#[tokio::test]
async fn work_on_another_branch_comes_back_to_the_task_branch() {
    let s = setup();
    let path = s.prepare(T1).await;
    let before = commits(&path);
    git(&path, &["switch", "--quiet", "-c", "elsewhere"]);
    std::fs::write(path.join("made.txt"), "committed").unwrap();
    git(&path, &["add", "made.txt"]);
    git(&path, &["commit", "--quiet", "-m", "On the other branch"]);
    std::fs::write(path.join("work.txt"), "done").unwrap();
    s.checkpoint(T1).await;
    assert_eq!(branch(&path), "threadlane/T-0001");
    assert_eq!(
        commits(&path),
        before + 2,
        "both commits are on the task branch"
    );
    assert_eq!(git(&path, &["status", "--porcelain"]), "");
    assert_eq!(git(&path, &["show", "HEAD:work.txt"]), "done");
}

#[tokio::test]
async fn work_that_does_not_apply_on_the_task_branch_is_kept_aside() {
    let s = setup();
    let path = s.prepare(T1).await;
    std::fs::write(path.join("task.txt"), "task").unwrap();
    s.checkpoint(T1).await;
    git(&path, &["switch", "--quiet", "--detach", "HEAD~1"]);
    // Untracked here, tracked on the task branch: switching back would overwrite it.
    std::fs::write(path.join("task.txt"), "mine").unwrap();
    let refused = s
        .workspaces
        .checkpoint(T1, std::slice::from_ref(&s.repository), "x")
        .await;
    let error = refused.expect_err("the work could not go on the task branch");
    assert!(
        error.to_string().contains("threadlane/T-0001-switched"),
        "{error}"
    );
    assert_eq!(branch(&path), "threadlane/T-0001");
    assert_eq!(
        std::fs::read_to_string(path.join("task.txt")).unwrap(),
        "task"
    );
    assert_eq!(
        git(&path, &["show", "threadlane/T-0001-switched:task.txt"]),
        "mine"
    );
}

#[tokio::test]
async fn commits_on_a_branch_off_another_base_are_kept_and_the_next_run_is_told() {
    let s = setup();
    let path = s.prepare(T1).await;
    std::fs::write(path.join("task.txt"), "task").unwrap();
    s.checkpoint(T1).await;
    for (round, name) in [
        (1, "threadlane/T-0001-switched"),
        (2, "threadlane/T-0001-switched-2"),
    ] {
        git(
            &path,
            &["switch", "--quiet", "-c", &format!("off-{round}"), "HEAD~1"],
        );
        std::fs::write(path.join("other.txt"), format!("round {round}")).unwrap();
        git(&path, &["add", "other.txt"]);
        git(&path, &["commit", "--quiet", "-m", "Off the task branch"]);
        std::fs::write(path.join("scratch.txt"), "scratch").unwrap();
        let refused = s
            .workspaces
            .checkpoint_tracked(T1, std::slice::from_ref(&s.repository), "x")
            .await;
        let error = refused.expect_err("the commit is not on the task branch");
        assert!(error.to_string().contains(name), "{error}");
        assert_eq!(branch(&path), "threadlane/T-0001");
        assert_eq!(
            git(&path, &["show", &format!("{name}:other.txt")]),
            format!("round {round}"),
            "an earlier kept branch is not overwritten"
        );
        assert!(
            git(&path, &["ls-tree", "--name-only", name])
                .lines()
                .all(|f| f != "scratch.txt"),
            "untracked scratch stays out of an unfinished run's commit"
        );
        let notes = s
            .workspaces
            .checkpoint_notes(T1, std::slice::from_ref(&s.repository))
            .await;
        assert!(notes.contains(name), "{notes}");
        std::fs::remove_file(path.join("scratch.txt")).unwrap();
    }
}

#[tokio::test]
async fn a_half_made_clone_or_worktree_is_made_again() {
    let s = setup();
    // A clone cut short: only its partial directory exists.
    std::fs::create_dir_all(s.clones().join("4.partial/.git")).unwrap();
    let path = s.prepare(T1).await;
    assert_eq!(branch(&path), "threadlane/T-0001");
    assert!(!s.clones().join("4.partial").exists());

    // A worktree cut short after its branch was made: no ready marker.
    std::fs::write(path.join("work.txt"), "unsaved").unwrap();
    std::fs::remove_file(path.parent().unwrap().join(".app.ready")).unwrap();
    std::fs::remove_file(path.join("README.md")).unwrap();
    let again = s.prepare(T1).await;
    assert_eq!(again, path);
    assert_eq!(branch(&again), "threadlane/T-0001", "the existing branch");
    assert!(again.join("README.md").is_file(), "checked out whole");
    assert!(!again.join("work.txt").exists());

    // A worktree directory deleted outright.
    std::fs::remove_dir_all(&path).unwrap();
    assert_eq!(s.prepare(T1).await, path);
    assert!(path.join("README.md").is_file());
}

#[tokio::test]
async fn names_git_could_read_as_options_are_refused() {
    let s = setup();
    for (name, branch) in [
        ("--upload-pack=x", "main"),
        ("app", "--orphan"),
        ("../x", "main"),
    ] {
        let repository = Repository {
            name: name.into(),
            default_branch: branch.into(),
            ..s.repository.clone()
        };
        let refused = s.workspaces.prepare(T1, &[repository]).await;
        assert!(refused.is_err(), "{name} {branch}: {refused:?}");
    }
    assert!(!s.clones().exists(), "git never ran");
}

#[tokio::test]
async fn a_remote_that_cannot_be_cloned_fails_without_prompting() {
    let s = setup();
    let repository = Repository {
        remote_url: s.dir.path().join("missing.git").display().to_string(),
        ..s.repository.clone()
    };
    let failed = s.workspaces.prepare(T1, &[repository]).await;
    assert!(failed.is_err(), "{failed:?}");
}

#[tokio::test]
async fn tasks_set_up_at_the_same_time_share_the_clone_safely() {
    let s = setup();
    let (first, second) = tokio::join!(s.prepare(T1), s.prepare(T2));
    assert_ne!(first, second);
    assert_eq!(branch(&first), "threadlane/T-0001");
    assert_eq!(branch(&second), "threadlane/T-0002");
}

#[tokio::test]
async fn workspace_deletion_removes_git_worktrees_and_only_requested_clones() {
    use threadlane_core::workspace::{WorkspaceCounts, WorkspaceRemoval};

    let s = setup();
    let first = s.prepare(T1).await;
    let second = s.prepare(T2).await;
    let clone = s.clones().join(s.repository.id.to_string());
    let mut removal = WorkspaceRemoval {
        tasks: vec![T1],
        repositories: vec![(s.repository.id, s.repository.name.clone())],
        clones: Vec::new(),
    };
    assert_eq!(
        s.workspaces.removal_counts(&removal).await,
        WorkspaceCounts {
            worktrees: 1,
            clones: 0
        }
    );
    assert!(s.workspaces.remove(&removal).await.is_empty());
    assert!(!first.exists() && second.is_dir() && clone.is_dir());
    let registrations = git(&clone, &["worktree", "list", "--porcelain"]);
    assert!(!registrations.contains(&T1.to_string()));
    assert!(registrations.contains(&T2.to_string()));

    removal.tasks = vec![T2];
    removal.clones = vec![s.repository.id];
    assert_eq!(
        s.workspaces.removal_counts(&removal).await,
        WorkspaceCounts {
            worktrees: 1,
            clones: 1
        }
    );
    assert!(s.workspaces.remove(&removal).await.is_empty());
    assert!(!second.exists() && !clone.exists());
    assert!(s.workspaces.remove(&removal).await.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_deletion_refuses_symlinks_outside_the_root() {
    use std::os::unix::fs::symlink;
    use threadlane_core::workspace::WorkspaceRemoval;
    let s = setup();
    let root = s.dir.path().join("workspaces");
    let outside = s.dir.path().join("keep");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), "keep").unwrap();
    std::fs::create_dir_all(root.join("worktrees")).unwrap();
    symlink(&outside, root.join("worktrees").join(T1.to_string())).unwrap();
    symlink(&outside, root.join("clones")).unwrap();
    std::fs::create_dir(outside.join(s.repository.id.to_string())).unwrap();
    let removal = WorkspaceRemoval {
        tasks: vec![T1],
        clones: vec![s.repository.id],
        ..WorkspaceRemoval::default()
    };
    let leftovers = s.workspaces.remove(&removal).await;
    assert_eq!(leftovers.len(), 2);
    assert!(leftovers[0].ends_with(&T1.to_string()));
    assert!(leftovers[1].ends_with(&s.repository.id.to_string()));
    assert_eq!(
        std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "keep"
    );
}

impl Setup {
    /// Lands a commit writing `file` on the remote's `main`, as another
    /// task's merged branch would; returns that commit.
    fn land_on_main(&self, file: &str, content: &str) -> String {
        let other = self.dir.path().join("other");
        if !other.exists() {
            git(
                self.dir.path(),
                &["clone", "--quiet", &self.repository.remote_url, "other"],
            );
        }
        git(&other, &["pull", "--quiet", "--ff-only"]);
        std::fs::write(other.join(file), content).unwrap();
        git(&other, &["add", "--all"]);
        git(&other, &["commit", "--quiet", "--no-verify", "-m", "Land"]);
        git(&other, &["push", "--quiet", "origin", "main"]);
        git(&other, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    async fn check_merge(&self, task: TaskId) -> threadlane_core::workspace::MergeCheck {
        let mut checks = self
            .workspaces
            .check_merge(task, std::slice::from_ref(&self.repository))
            .await
            .expect("check");
        assert_eq!(checks.len(), 1);
        checks.remove(0)
    }

    async fn bring_in_base(&self, task: TaskId) -> threadlane_core::workspace::MergeCheck {
        let mut checks = self
            .workspaces
            .bring_in_base(task, std::slice::from_ref(&self.repository))
            .await
            .expect("bring in base");
        assert_eq!(checks.len(), 1);
        checks.remove(0)
    }
}

#[tokio::test]
async fn the_merge_check_names_the_base_and_the_conflicting_files_and_touches_no_worktree() {
    let s = setup();
    let path = s.prepare(T1).await;
    std::fs::write(path.join("README.md"), "# app, by the task\n").unwrap();
    std::fs::write(path.join("new.txt"), "new").unwrap();
    s.checkpoint(T1).await;

    let other = s.land_on_main("other.txt", "elsewhere");
    let clean = s.check_merge(T1).await;
    assert!(clean.clean(), "{clean:?}");
    assert_eq!(
        (clean.branch.as_str(), clean.base.as_str()),
        ("main", other.as_str())
    );
    assert_eq!(clean.repository, "app");

    let conflicting = s.land_on_main("README.md", "# app, by another task\n");
    let head = git(&path, &["rev-parse", "HEAD"]);
    let checked = s.check_merge(T1).await;
    assert_eq!(checked.conflicts, ["README.md"]);
    assert_eq!(checked.base, conflicting);
    assert_eq!(checked.revision.as_deref(), Some(head.trim()));
    assert_eq!(
        git(&path, &["rev-parse", "HEAD"]),
        head,
        "the branch is untouched"
    );
    assert_eq!(git(&path, &["status", "--porcelain"]), "");

    let unset = s
        .workspaces
        .check_merge(T2, std::slice::from_ref(&s.repository))
        .await
        .unwrap();
    assert!(unset.is_empty(), "a worktree never set up is skipped");
}

#[tokio::test]
async fn a_pr_review_whose_base_fetch_fails_is_not_pointed_at_origin_base() {
    let h = Harness::new();
    let mut s = setup();
    s.workspaces = GitWorkspaces::new(h.config.lanes_dir.parent().unwrap().join("workspaces"));
    s.prepare(T1).await;
    let seed = s.dir.path().join("remotes/app-seed");
    git(&seed, &["commit", "--allow-empty", "-m", "PR-only commit"]);
    git(
        &seed,
        &["push", &s.repository.remote_url, "HEAD:refs/pull/42/head"],
    );
    let head = git(&seed, &["rev-parse", "HEAD"]).trim().to_owned();
    // The remote has no `gone` branch, so fetching the base fails.
    h.forge.set_pull_base("gone");

    let mut assignment = assignment(11);
    assignment.lane_kind = LaneKind::Task;
    assignment.task_id = Some(T1);
    assignment.pr_review = Some(PullRequestReview {
        number: 42,
        revision: ReviewRevision {
            repository: s.repository.id,
            commit: head.clone(),
        },
    });
    assignment.repositories.push(s.repository.clone());
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment)));
    h.link.push_tool(Ok(json!({PROGRESS_WRITTEN: true})));
    h.link.push_tool(Ok(json!({"finished": true})));
    h.backend.scripted.push(
        Script::new()
            .call("progress", "memory_create", json!({}))
            .call(
                "verdict",
                "finish_task",
                json!({"reviewed_head":head,"pr_verdict":"approved","summary":"Reviewed"}),
            )
            .finish("Reviewed"),
    );
    h.runner.work_once().await.unwrap();
    let prompt = &h.backend.scripted.received()[0].prompt;
    assert!(prompt.contains("fetching that branch failed"), "{prompt}");
    assert!(prompt.contains("gh pr view 42"), "{prompt}");
    assert!(!prompt.contains("git diff origin/gone"), "{prompt}");
}

#[tokio::test]
async fn a_standalone_pr_review_fetches_its_head_and_completes_from_a_detached_checkout() {
    let h = Harness::new();
    let mut s = setup();
    s.workspaces = GitWorkspaces::new(h.config.lanes_dir.parent().unwrap().join("workspaces"));
    let task_tree = s.prepare(T1).await;
    let task_head = git(&task_tree, &["rev-parse", "HEAD"]);
    let seed = s.dir.path().join("remotes/app-seed");
    git(&seed, &["commit", "--allow-empty", "-m", "PR-only commit"]);
    git(
        &seed,
        &["push", &s.repository.remote_url, "HEAD:refs/pull/42/head"],
    );
    let head = git(&seed, &["rev-parse", "HEAD"]).trim().to_owned();
    // The PR merges into `release`, not the registration's tracked `main` (#715).
    git(
        &seed,
        &[
            "push",
            &s.repository.remote_url,
            "HEAD~1:refs/heads/release",
        ],
    );
    h.forge.set_pull_base("release");

    let mut assignment = assignment(11);
    assignment.lane_kind = LaneKind::Task;
    assignment.task_id = Some(T1);
    assignment.pr_review = Some(PullRequestReview {
        number: 42,
        revision: ReviewRevision {
            repository: s.repository.id,
            commit: head.clone(),
        },
    });
    assignment.repositories.push(s.repository.clone());
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment)));
    h.link.push_tool(Ok(json!({PROGRESS_WRITTEN: true})));
    h.link.push_tool(Ok(json!({"finished": true})));
    h.backend.scripted.push(
        Script::new()
            .call("progress", "memory_create", json!({}))
            .call(
                "verdict",
                "finish_task",
                json!({"reviewed_head":head,"pr_verdict":"approved","summary":"Reviewed"}),
            )
            .finish("Reviewed"),
    );
    h.runner.work_once().await.unwrap();
    let runs = h.backend.scripted.received();
    let checkout = &runs[0].working_dir;
    assert_ne!(checkout, &task_tree);
    let prompt = &runs[0].prompt;
    assert!(prompt.contains("merges into `release`"), "{prompt}");
    assert!(prompt.contains("tracks `main`"), "{prompt}");
    assert_eq!(
        git(checkout, &["rev-parse", "origin/release"]).trim(),
        git(&seed, &["rev-parse", "HEAD~1"]).trim()
    );
    assert_eq!(git(checkout, &["rev-parse", "HEAD"]).trim(), head);
    assert_eq!(
        git(checkout, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD"
    );
    assert_eq!(branch(&task_tree), "threadlane/T-0001");
    assert_eq!(git(&task_tree, &["rev-parse", "HEAD"]), task_head);
    // Approval or progress follow-ups resume in the same checkout, even after inspecting a PR branch.
    git(checkout, &["switch", "-c", "reviewed-pr"]);
    let resumed = s
        .workspaces
        .prepare_review(
            T1,
            ReviewCheckout::PullRequest(42),
            std::slice::from_ref(&s.repository),
            &[ReviewRevision {
                repository: s.repository.id,
                commit: head.clone(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(resumed, std::slice::from_ref(checkout));
    assert_eq!(
        git(checkout, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD"
    );
    let calls = h.calls();
    assert!(calls.iter().any(|call| matches!(call, Call::Tool {tool, arguments, ..} if tool == "finish_task" && arguments["reviewed_head"] == head)));
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::Complete { .. }))
    );
}

#[tokio::test]
async fn a_review_uses_the_submitted_commit_after_the_implementation_branch_moves() {
    let h = Harness::new();
    let mut s = setup();
    s.workspaces = GitWorkspaces::new(h.config.lanes_dir.parent().unwrap().join("workspaces"));
    let implementation = s.prepare(T1).await;
    std::fs::write(implementation.join("work.txt"), "submitted").unwrap();
    std::fs::write(implementation.join(".gitignore"), "node_modules/\n").unwrap();
    std::fs::create_dir(implementation.join("node_modules")).unwrap();
    std::fs::write(implementation.join("node_modules/dep.js"), "installed").unwrap();
    s.checkpoint(T1).await;
    let checked = s.check_merge(T1).await;
    let submitted = checked.revision.clone().unwrap();
    std::fs::write(implementation.join("work.txt"), "later edit").unwrap();
    s.checkpoint(T1).await;

    let repositories = std::slice::from_ref(&s.repository);
    assert!(
        s.workspaces
            .verify_submission(T1, repositories, &[checked])
            .await
            .is_err()
    );
    let revisions = [ReviewRevision {
        repository: s.repository.id,
        commit: submitted.clone(),
    }];
    let mut assignment = assignment(11);
    assignment.lane_kind = LaneKind::Task;
    assignment.task_id = Some(T1);
    assignment.repositories = repositories.to_vec();
    assignment.review = Some(RunReview {
        submission: RunId(10),
        revisions: revisions.to_vec(),
    });
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment)));
    h.link.push_tool(Ok(json!({PROGRESS_WRITTEN: true})));
    h.backend.scripted.push(
        Script::new()
            .call("progress", "memory_create", json!({}))
            .finish("Reviewed"),
    );
    h.runner.work_once().await.unwrap();
    let runs = h.backend.scripted.received();
    assert_eq!(runs.len(), 1);
    let review = &runs[0].working_dir;
    assert!(
        h.calls()
            .iter()
            .any(|call| matches!(call, Call::Complete { .. }))
    );
    assert_ne!(review, &implementation);
    assert_eq!(git(review, &["rev-parse", "HEAD"]).trim(), submitted);
    assert_eq!(
        git(review, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD"
    );
    assert_eq!(
        std::fs::read_to_string(review.join("work.txt")).unwrap(),
        "submitted"
    );
    assert_eq!(
        std::fs::read_to_string(implementation.join("work.txt")).unwrap(),
        "later edit"
    );
    // The submitter's installed dependencies are reused, not left out of the checkout.
    #[cfg(unix)]
    {
        assert!(
            std::fs::symlink_metadata(review.join("node_modules"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(review.join("node_modules/dep.js")).unwrap(),
            "installed"
        );
    }
    assert_eq!(
        s.workspaces
            .prepare_review(
                T1,
                ReviewCheckout::Submission(RunId(10)),
                repositories,
                &revisions
            )
            .await
            .unwrap(),
        std::slice::from_ref(review)
    );
    assert!(
        s.workspaces
            .prepare_review(T1, ReviewCheckout::Submission(RunId(10)), repositories, &[])
            .await
            .is_err()
    );
    let removal = threadlane_core::workspace::WorkspaceRemoval {
        tasks: vec![T1],
        repositories: vec![(s.repository.id, s.repository.name.clone())],
        clones: vec![],
    };
    assert!(s.workspaces.remove(&removal).await.is_empty());
    assert!(!review.exists());
    assert!(!implementation.exists());
}

#[tokio::test]
async fn a_run_start_rebases_a_clean_branch_and_leaves_a_conflicting_one() {
    let s = setup();
    let path = s.prepare(T1).await;
    std::fs::write(path.join("work.txt"), "work").unwrap();
    s.checkpoint(T1).await;

    let landed = s.land_on_main("other.txt", "elsewhere");
    let rebased = s.bring_in_base(T1).await;
    assert!(rebased.clean(), "{rebased:?}");
    assert_eq!(rebased.base, landed);
    git(&path, &["merge-base", "--is-ancestor", &landed, "HEAD"]);
    assert!(path.join("other.txt").is_file() && path.join("work.txt").is_file());
    assert_eq!(branch(&path), "threadlane/T-0001");

    std::fs::write(path.join("README.md"), "# app, by the task\n").unwrap();
    s.checkpoint(T1).await;
    let head = git(&path, &["rev-parse", "HEAD"]);
    s.land_on_main("README.md", "# app, by another task\n");
    let left = s.bring_in_base(T1).await;
    assert_eq!(left.conflicts, ["README.md"]);
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), head, "left as it is");
    assert_eq!(
        git(&path, &["status", "--porcelain"]),
        "",
        "no rebase left half done"
    );
    assert_eq!(branch(&path), "threadlane/T-0001");
}

#[tokio::test]
async fn a_published_task_can_push_again_after_the_base_moves() {
    let s = setup();
    let path = s.prepare(T1).await;
    std::fs::write(path.join("work.txt"), "work").unwrap();
    s.checkpoint(T1).await;
    git(&path, &["push", "origin", "HEAD"]);
    let published = git(&path, &["rev-parse", "HEAD"]);

    let landed = s.land_on_main("other.txt", "elsewhere");
    let check = s.bring_in_base(T1).await;
    assert!(check.clean(), "{check:?}");
    assert_eq!(check.base, landed);
    // Merged, not rebased: the pushed commit stays in the history (#730).
    git(
        &path,
        &["merge-base", "--is-ancestor", published.trim(), "HEAD"],
    );
    git(&path, &["merge-base", "--is-ancestor", &landed, "HEAD"]);

    std::fs::write(path.join("work.txt"), "updated").unwrap();
    s.checkpoint(T1).await;
    git(&path, &["push", "origin", "HEAD"]);
}

#[tokio::test]
async fn a_branch_pushed_onto_another_tasks_pr_is_merged_not_rebased() {
    let s = setup();
    let first = s.prepare(T1).await;
    std::fs::write(first.join("work.txt"), "work").unwrap();
    s.checkpoint(T1).await;
    git(&first, &["push", "origin", "HEAD"]);

    // T2 updates T1's PR in place: it merges T1's branch and pushes onto it.
    let path = s.prepare(T2).await;
    git(&path, &["merge", "--quiet", "threadlane/T-0001"]);
    std::fs::write(path.join("fix.txt"), "fix").unwrap();
    s.checkpoint(T2).await;
    git(&path, &["push", "origin", "HEAD:threadlane/T-0001"]);
    let pushed = git(&path, &["rev-parse", "HEAD"]);

    let landed = s.land_on_main("other.txt", "elsewhere");
    assert!(s.bring_in_base(T2).await.clean());
    git(
        &path,
        &["merge-base", "--is-ancestor", pushed.trim(), "HEAD"],
    );
    git(&path, &["merge-base", "--is-ancestor", &landed, "HEAD"]);
    let preview = s
        .workspaces
        .preview_publish(T2, &s.repository, None, "threadlane/T-0001")
        .await
        .expect("the push extends the PR head");
    assert_eq!(preview.revision, git(&path, &["rev-parse", "HEAD"]).trim());
}

#[tokio::test]
async fn a_push_that_would_not_extend_the_remote_branch_is_refused_before_approval() {
    let s = setup();
    let path = s.prepare(T1).await;
    std::fs::write(path.join("work.txt"), "work").unwrap();
    s.checkpoint(T1).await;
    git(&path, &["push", "origin", "HEAD"]);
    let pushed = git(&path, &["rev-parse", "HEAD"]);

    // Rewritten after the push, as a rebase would.
    git(&path, &["commit", "--quiet", "--amend", "-m", "Rewritten"]);
    let error = s
        .workspaces
        .preview_publish(T1, &s.repository, None, "threadlane/T-0001")
        .await
        .expect_err("the push cannot land");
    assert!(error.0.contains("would be rejected"), "{error}");
    assert!(error.0.contains(pushed.trim()), "{error}");
    assert!(
        error.0.contains("git merge origin/threadlane/T-0001"),
        "{error}"
    );

    git(
        &path,
        &["merge", "--quiet", "--no-edit", "origin/threadlane/T-0001"],
    );
    s.workspaces
        .preview_publish(T1, &s.repository, None, "threadlane/T-0001")
        .await
        .expect("merged back in, the push extends the remote");
}

#[tokio::test]
async fn a_task_given_a_repository_is_told_where_its_earlier_work_is() {
    let h = Harness::new();
    let earlier = h.config.lanes_dir.join(LANE.to_string());
    std::fs::create_dir_all(&earlier).unwrap();
    std::fs::write(earlier.join("work.txt"), "done before the attach").unwrap();
    let mut assignment = assignment(11);
    assignment.lane_kind = LaneKind::Task;
    assignment.task_id = Some(T1);
    let s = setup();
    assignment.repositories = vec![s.repository.clone()];
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment)));
    h.link.push_tool(Ok(json!({PROGRESS_WRITTEN: true})));
    h.backend.scripted.push(
        Script::new()
            .call("progress", "memory_create", json!({}))
            .finish("Moved"),
    );
    h.runner.work_once().await.unwrap();
    let runs = h.backend.scripted.received();
    assert_ne!(runs[0].working_dir, earlier);
    assert!(
        runs[0].prompt.contains(&earlier.display().to_string()),
        "{}",
        runs[0].prompt
    );
    assert!(
        earlier.join("work.txt").is_file(),
        "the earlier work is kept"
    );
}

#[tokio::test]
async fn the_run_after_an_unfinished_one_is_told_what_its_checkpoint_left_out() {
    let h = Harness::new();
    let s = setup();
    let root = h.config.lanes_dir.parent().unwrap().join("workspaces");
    let workspaces = GitWorkspaces::new(root);
    let repositories = std::slice::from_ref(&s.repository);
    let path = workspaces.prepare(T1, repositories).await.expect("prepare")[0].clone();
    std::fs::write(path.join("README.md"), "# app, changed\n").unwrap();
    for n in 0..12 {
        std::fs::write(path.join(format!("scratch{n:02}.sh")), "sleep 600").unwrap();
    }
    workspaces
        .checkpoint_tracked(T1, repositories, "Timed out")
        .await
        .expect("checkpoint");

    let mut assignment = assignment(11);
    assignment.lane_kind = LaneKind::Task;
    assignment.task_id = Some(T1);
    assignment.repositories = vec![s.repository.clone()];
    // A run whose backend cannot start (no script) is not told: the next one is.
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment.clone())));
    h.runner.work_once().await.unwrap();
    assert_ne!(
        workspaces.checkpoint_notes(T1, repositories).await,
        "",
        "kept after a failed start"
    );
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment)));
    h.link.push_tool(Ok(json!({PROGRESS_WRITTEN: true})));
    h.backend.scripted.push(
        Script::new()
            .call("progress", "memory_create", json!({}))
            .finish("Resumed"),
    );
    h.runner.work_once().await.unwrap();
    let prompt = &h.backend.scripted.received()[0].prompt;
    assert!(
        prompt.contains("committed changes to tracked files: README.md.")
            && prompt.contains("scratch00.sh")
            && prompt.contains("and 2 more")
            && !prompt.contains("scratch11.sh"),
        "{prompt}"
    );
    assert_eq!(
        workspaces.checkpoint_notes(T1, repositories).await,
        "",
        "told once"
    );
}

#[tokio::test]
async fn an_ended_tasks_worktrees_go_only_once_nothing_in_them_would_be_lost() {
    let s = setup();
    let repositories = std::slice::from_ref(&s.repository);
    let path = s.prepare(T1).await;
    let task_dir = path.parent().unwrap().to_path_buf();
    let kept = |reason: Option<String>, what: &str| {
        let reason = reason.expect("kept");
        assert!(reason.contains(what), "{reason}");
    };

    std::fs::write(path.join("work.txt"), "half done").unwrap();
    kept(
        s.workspaces
            .remove_finished(T1, repositories)
            .await
            .unwrap(),
        "uncommitted",
    );
    s.checkpoint(T1).await;
    let commit = git(&path, &["rev-parse", "HEAD"]).trim().to_owned();
    let review = s
        .workspaces
        .prepare_review(
            T1,
            ReviewCheckout::Submission(RunId(10)),
            repositories,
            &[ReviewRevision {
                repository: s.repository.id,
                commit: commit.clone(),
            }],
        )
        .await
        .unwrap()
        .remove(0);
    kept(
        s.workspaces
            .remove_finished(T1, repositories)
            .await
            .unwrap(),
        "no remote branch",
    );
    assert_eq!(
        std::fs::read_to_string(path.join("work.txt")).unwrap(),
        "half done"
    );

    git(&path, &["push", "--quiet", "origin", "threadlane/T-0001"]);
    std::fs::write(review.join("notes.txt"), "a reviewer's note").unwrap();
    kept(
        s.workspaces
            .remove_finished(T1, repositories)
            .await
            .unwrap(),
        "uncommitted",
    );
    std::fs::remove_file(review.join("notes.txt")).unwrap();

    git(&path, &["config", "status.showUntrackedFiles", "no"]);
    std::fs::write(path.join("hidden.txt"), "untracked").unwrap();
    kept(
        s.workspaces
            .remove_finished(T1, repositories)
            .await
            .unwrap(),
        "uncommitted",
    );
    std::fs::remove_file(path.join("hidden.txt")).unwrap();

    let lib = bare_repository(&s.dir.path().join("remotes"), "lib");
    git(
        &path,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            lib.to_str().unwrap(),
            "lib",
        ],
    );
    git(
        &path,
        &["commit", "--quiet", "--no-verify", "-m", "Add lib"],
    );
    git(&path, &["push", "--quiet", "origin", "threadlane/T-0001"]);
    git(&path, &["config", "submodule.lib.ignore", "all"]);
    std::fs::write(path.join("lib/README.md"), "changed in the submodule").unwrap();
    kept(
        s.workspaces
            .remove_finished(T1, repositories)
            .await
            .unwrap(),
        "uncommitted",
    );
    git(&path.join("lib"), &["checkout", "--quiet", "README.md"]);
    let commit = git(&path, &["rev-parse", "HEAD"]).trim().to_owned();

    std::fs::write(task_dir.join(".handoff-notes"), "not a marker").unwrap();
    kept(
        s.workspaces
            .remove_finished(T1, repositories)
            .await
            .unwrap(),
        "not a known worktree",
    );
    std::fs::remove_file(task_dir.join(".handoff-notes")).unwrap();

    assert_eq!(
        s.workspaces
            .remove_finished(T1, repositories)
            .await
            .unwrap(),
        None
    );
    assert!(!task_dir.exists());
    let clone = s.clones().join(s.repository.id.to_string());
    assert_eq!(
        git(&clone, &["rev-parse", "refs/heads/threadlane/T-0001"]).trim(),
        commit,
        "the task branch stays"
    );
    assert!(
        !git(&clone, &["worktree", "list", "--porcelain"]).contains("T-0001"),
        "the worktrees are unregistered"
    );
    // Safe to run again, and for a task whose worktree was never set up.
    for task in [T1, T2] {
        assert_eq!(
            s.workspaces
                .remove_finished(task, repositories)
                .await
                .unwrap(),
            None
        );
    }
}

/// #790: a task commits a separate finding on a side branch and hands it to
/// a subtask, whose branch starts from the default branch with just those
/// commits, authors and messages kept, applied once.
#[tokio::test]
async fn a_side_branch_handed_to_a_subtask_starts_its_branch() {
    let s = setup();
    let path = s.prepare(T1).await;
    git(&path, &["switch", "--quiet", "-c", "finding-2"]);
    std::fs::write(path.join("finding.txt"), "second finding\n").unwrap();
    git(&path, &["add", "finding.txt"]);
    git(
        &path,
        &[
            "-c",
            "user.name=Finder",
            "-c",
            "user.email=finder@localhost",
            "commit",
            "--quiet",
            "-m",
            "Second finding",
        ],
    );
    git(&path, &["switch", "--quiet", "threadlane/T-0001"]);
    std::fs::write(path.join("first.txt"), "first finding\n").unwrap();
    s.checkpoint(T1).await;

    let patch = s
        .workspaces
        .start_patch(T1, &s.repository, "finding-2")
        .await
        .expect("patch");
    assert_eq!(patch.repository, s.repository.id);
    let main = git(&path, &["rev-parse", "origin/main"]);
    assert_eq!(patch.base, main.trim(), "where finding-2 leaves main");

    // A later change on main is brought in at run start, not here.
    s.land_on_main("other.txt", "elsewhere");
    let repositories = std::slice::from_ref(&s.repository);
    let start = std::slice::from_ref(&patch);
    let child = s
        .workspaces
        .prepare_with_start(T2, repositories, start)
        .await
        .expect("prepare the subtask")
        .remove(0);
    assert_eq!(branch(&child), "threadlane/T-0002");
    assert_eq!(
        std::fs::read_to_string(child.join("finding.txt")).unwrap(),
        "second finding\n"
    );
    assert!(!child.join("first.txt").exists(), "only the handed commits");
    assert_eq!(commits(&child), 2);
    let last = git(&child, &["log", "-1", "--format=%an <%ae>|%s"]);
    assert_eq!(last.trim(), "Finder <finder@localhost>|Second finding");
    assert_eq!(git(&child, &["status", "--porcelain"]), "");

    let again = s
        .workspaces
        .prepare_with_start(T2, repositories, start)
        .await
        .expect("prepare again")
        .remove(0);
    assert_eq!(again, child);
    assert_eq!(commits(&child), 2, "applied once");
}

#[tokio::test]
async fn a_hand_off_with_nothing_to_hand_over_or_an_odd_name_is_refused() {
    let s = setup();
    let path = s.prepare(T1).await;
    for (revision, says) in [
        ("HEAD", "no commits beyond origin/main"),
        ("no-such-branch", "is not a commit or branch"),
        ("--output=x", "is not a commit or branch"),
        ("", "is not a commit or branch"),
    ] {
        let error = s
            .workspaces
            .start_patch(T1, &s.repository, revision)
            .await
            .expect_err(revision);
        assert!(error.0.contains(says), "{revision}: {error}");
    }
    let error = s
        .workspaces
        .start_patch(T2, &s.repository, "HEAD")
        .await
        .expect_err("no worktree");
    assert!(error.0.contains("has no worktree"), "{error}");

    // A merge since leaving main would be lost by format-patch.
    git(&path, &["switch", "--quiet", "-c", "side"]);
    std::fs::write(path.join("a.txt"), "a").unwrap();
    git(&path, &["add", "a.txt"]);
    git(&path, &["commit", "--quiet", "-m", "A"]);
    git(&path, &["switch", "--quiet", "threadlane/T-0001"]);
    std::fs::write(path.join("b.txt"), "b").unwrap();
    git(&path, &["add", "b.txt"]);
    git(&path, &["commit", "--quiet", "-m", "B"]);
    git(&path, &["merge", "--quiet", "--no-edit", "side"]);
    let error = s
        .workspaces
        .start_patch(T1, &s.repository, "HEAD")
        .await
        .expect_err("merges");
    assert!(error.0.contains("merge commits"), "{error}");
}

/// A patch made on another machine has a base this clone lacks: it goes on
/// the latest main, and when it does not apply the subtask gets no branch,
/// so the next attempt tries again instead of starting without it.
#[tokio::test]
async fn a_hand_off_that_does_not_apply_leaves_no_branch() {
    let s = setup();
    let path = s.prepare(T1).await;
    std::fs::write(path.join("README.md"), "# app, by the finding\n").unwrap();
    s.checkpoint(T1).await;
    let mut patch = s
        .workspaces
        .start_patch(T1, &s.repository, "HEAD")
        .await
        .expect("patch");
    patch.base = "0".repeat(40);
    s.land_on_main("README.md", "# app, by another task\n");

    let error = s
        .workspaces
        .prepare_with_start(
            T2,
            std::slice::from_ref(&s.repository),
            std::slice::from_ref(&patch),
        )
        .await
        .expect_err("conflicts");
    assert!(error.0.contains("do not apply"), "{error}");
    let clone = s.clones().join(s.repository.id.to_string());
    let branches = git(&clone, &["branch", "--list", "threadlane/T-0002"]);
    assert_eq!(branches.trim(), "", "no branch without the patch");

    // Without the conflict, the same patch goes on the latest main.
    s.land_on_main("README.md", "# app\n");
    let child = s
        .workspaces
        .prepare_with_start(
            T2,
            std::slice::from_ref(&s.repository),
            std::slice::from_ref(&patch),
        )
        .await
        .expect("applies")
        .remove(0);
    assert_eq!(
        std::fs::read_to_string(child.join("README.md")).unwrap(),
        "# app, by the finding\n"
    );
}

/// #790: the runner turns a task's `start_from` into start patches from its
/// worktree before the hub sees the call, replacing any the agent wrote.
#[tokio::test]
async fn a_tasks_create_task_carries_the_commits_it_hands_over() {
    let h = Harness::new();
    let s = setup();
    let root = h.config.lanes_dir.parent().unwrap().join("workspaces");
    let workspaces = GitWorkspaces::new(root);
    let repositories = std::slice::from_ref(&s.repository);
    let path = workspaces.prepare(T1, repositories).await.expect("prepare")[0].clone();
    git(&path, &["switch", "--quiet", "-c", "finding-2"]);
    std::fs::write(path.join("finding.txt"), "second finding\n").unwrap();
    git(&path, &["add", "finding.txt"]);
    git(&path, &["commit", "--quiet", "-m", "Second finding"]);
    git(&path, &["switch", "--quiet", "threadlane/T-0001"]);

    let mut assignment = assignment(11);
    assignment.lane_kind = LaneKind::Task;
    assignment.task_id = Some(T1);
    assignment.repositories = vec![s.repository.clone()];
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment)));
    h.link.push_tool(Ok(json!({ "task_id": "T-0002" })));
    h.link.push_tool(Ok(json!({PROGRESS_WRITTEN: true})));
    h.backend.scripted.push(
        Script::new()
            .call(
                "sub",
                "create_task",
                json!({
                    "title": "second finding",
                    "goal": "its own PR",
                    "start_from": { "app": "finding-2" },
                    "start_patches": "from the agent",
                }),
            )
            .call("progress", "memory_create", json!({}))
            .finish("Handed over"),
    );
    h.runner.work_once().await.unwrap();
    let forwarded = h
        .calls()
        .into_iter()
        .find_map(|call| match call {
            Call::Tool {
                tool, arguments, ..
            } if tool == "create_task" => Some(arguments),
            _ => None,
        })
        .expect("create_task reached the hub");
    let patches = forwarded["start_patches"].as_array().expect("patches");
    assert_eq!(patches.len(), 1, "{forwarded}");
    assert_eq!(patches[0]["repository"], json!(s.repository.id));
    let patch = patches[0]["patch"].as_str().unwrap();
    assert!(
        patch.contains("Subject: [PATCH] Second finding") && patch.contains("+second finding"),
        "{patch}"
    );
    assert_eq!(forwarded["start_from"], json!({ "app": "finding-2" }));
}
