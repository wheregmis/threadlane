//! [`GitWorkspaces`] against local bare repositories (Plan.md §5.5,
//! docs/milestone-3.md §3): real `git`, no network.

use std::path::Path;

use tempfile::TempDir;
use threadlane_core::ids::{ProjectId, RepositoryId, TaskId};
use threadlane_core::model::Repository;
use threadlane_core::workspace::Workspaces;
use threadlane_runner::GitWorkspaces;
use threadlane_testkit::git::{bare_repository, git};

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
async fn work_on_another_branch_is_not_committed() {
    let s = setup();
    let path = s.prepare(T1).await;
    git(&path, &["switch", "--quiet", "-c", "elsewhere"]);
    std::fs::write(path.join("work.txt"), "done").unwrap();
    let refused = s
        .workspaces
        .checkpoint(T1, std::slice::from_ref(&s.repository), "x")
        .await;
    assert!(refused.is_err(), "{refused:?}");
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
