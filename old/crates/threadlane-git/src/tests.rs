use super::*;
use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

fn run_git(work_dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(work_dir)
        .env("GIT_CONFIG_GLOBAL", work_dir.join("git-test-global-config"))
        .env("GIT_CONFIG_SYSTEM", work_dir.join("git-test-system-config"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    if args.first() == Some(&"init") {
        // Library commands also use this repository; keep host hooks out of fixtures.
        let hooks = work_dir.canonicalize().unwrap().join(".git/empty-hooks");
        fs::create_dir_all(&hooks).unwrap();
        run_git(work_dir, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    }
}

#[test]
fn inspect_counts_untracked_files_inside_new_directories() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Threadlane"]);
    fs::write(dir.path().join(".gitignore"), "ignored/\n").unwrap();
    run_git(dir.path(), &["add", ".gitignore"]);
    run_git(dir.path(), &["commit", "-qm", "initial"]);
    // Explicitly override Git's directory-collapsing user preference.
    run_git(
        dir.path(),
        &["config", "status.showUntrackedFiles", "normal"],
    );
    fs::create_dir_all(dir.path().join("new/nested")).unwrap();
    fs::create_dir_all(dir.path().join("ignored")).unwrap();
    fs::write(dir.path().join("new/one.txt"), "one\ntwo\n").unwrap();
    fs::write(dir.path().join("new/nested/two.txt"), "three\nfour\nfive\n").unwrap();
    fs::write(dir.path().join("ignored/hidden.txt"), "ignored\n").unwrap();

    let status = inspect(dir.path()).unwrap();
    assert_eq!(status.files.len(), 2);
    assert!(status.files.iter().all(|file| file.is_untracked()));
    assert!(status
        .files
        .iter()
        .any(|file| file.path == "new/one.txt" && file.additions == 2));
    assert!(status
        .files
        .iter()
        .any(|file| file.path == "new/nested/two.txt" && file.additions == 3));
    assert_eq!(
        status.files.iter().map(|file| file.additions).sum::<u32>(),
        5
    );
    assert_eq!(
        status.files.iter().map(|file| file.deletions).sum::<u32>(),
        0
    );
}

#[test]
fn clean_file_inspection_only_runs_status() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-q"]);
    COMMAND_SPAWNS.set(0);
    assert!(inspect_files(dir.path()).unwrap().is_empty());
    assert_eq!(COMMAND_SPAWNS.get(), 1);
}

#[test]
fn inspection_reuses_branch_listing_without_losing_local_refs() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(
        dir.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
    );
    for branch in [
        "z-last",
        "origin/local",
        "origin/main",
        "nested/HEAD",
        "a-first",
    ] {
        run_git(dir.path(), &["branch", branch]);
    }
    run_git(
        dir.path(),
        &["update-ref", "refs/remotes/origin/remote-only", "HEAD"],
    );
    run_git(
        dir.path(),
        &["update-ref", "refs/remotes/origin/local", "HEAD"],
    );
    run_git(
        dir.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    run_git(dir.path(), &["tag", "a-first"]);
    BRANCH_LIST_RUNS.set(0);
    let status = inspect(dir.path()).unwrap();
    assert_eq!(BRANCH_LIST_RUNS.get(), 1);
    assert_eq!(
        status.branches,
        [
            "a-first",
            "main",
            "nested/HEAD",
            "origin/local",
            "origin/main",
            "z-last"
        ]
    );
    assert!(
        status
            .branch_details
            .iter()
            .any(|branch| branch.name == "origin/local" && !branch.is_remote)
    );
    assert!(
        status
            .branch_details
            .iter()
            .any(|branch| branch.name == "remotes/origin/local" && branch.is_remote)
    );
    assert!(
        status
            .branch_details
            .iter()
            .any(|branch| branch.name == "origin/remote-only" && branch.is_remote)
    );
    assert!(
        status
            .branch_details
            .iter()
            .any(|branch| branch.name == "main" && !branch.is_remote && branch.is_default)
    );
    assert!(
        status
            .branch_details
            .iter()
            .any(|branch| {
                branch.name == "remotes/origin/main" && branch.is_remote && branch.is_default
            })
    );
    assert!(
        status
            .branch_details
            .iter()
            .any(|branch| {
                branch.name == "origin/main" && !branch.is_remote && !branch.is_default
            })
    );

    let local_a_first = status
        .branches
        .iter()
        .find(|branch| branch.as_str() == "a-first")
        .unwrap();
    run_git(dir.path(), &["switch", local_a_first.as_str()]);
    let head = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(head.status.success());
    assert_eq!(
        String::from_utf8_lossy(&head.stdout).trim(),
        "refs/heads/a-first"
    );
}

#[test]
fn inspection_preserves_unborn_current_branch() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-q", "-b", "new-branch"]);
    let status = inspect(dir.path()).unwrap();
    assert_eq!(status.branches, vec![status.branch.unwrap()]);
}

#[test]
fn file_inspection_preserves_review_changes_with_two_git_commands() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Threadlane"]);
    run_git(dir.path(), &["config", "status.renames", "true"]);
    for (path, content) in [
        (".gitignore", "ignored/\n"),
        ("tracked.txt", "initial\n"),
        ("original.txt", "rename me\n"),
        ("deleted.txt", "delete me\n"),
    ] {
        fs::write(dir.path().join(path), content).unwrap();
    }
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial"]);
    fs::write(dir.path().join("tracked.txt"), "staged\n").unwrap();
    run_git(dir.path(), &["add", "tracked.txt"]);
    fs::write(dir.path().join("tracked.txt"), "staged\nunstaged\n").unwrap();
    run_git(dir.path(), &["mv", "original.txt", "renamed.txt"]);
    fs::remove_file(dir.path().join("deleted.txt")).unwrap();
    for folder in ["new/nested", "ignored"] {
        fs::create_dir_all(dir.path().join(folder)).unwrap();
        fs::write(dir.path().join(folder).join("file.txt"), "one\ntwo\n").unwrap();
    }

    COMMAND_SPAWNS.set(0);
    let review = inspect(dir.path()).unwrap();
    let review_commands = COMMAND_SPAWNS.get();
    COMMAND_SPAWNS.set(0);
    let files = inspect_files(dir.path()).unwrap();
    let file_commands = COMMAND_SPAWNS.get();

    assert_eq!(files, review.files);
    assert_eq!(files.len(), 4);
    assert!(files.iter().any(|file| file.path == "tracked.txt"
        && file.index_status == 'M'
        && file.worktree_status == 'M'));
    assert!(files.iter().any(
        |file| file.path == "renamed.txt" && file.orig_path.as_deref() == Some("original.txt")
    ));
    assert!(
        files
            .iter()
            .any(|file| file.path == "deleted.txt" && file.worktree_status == 'D')
    );
    assert!(files.iter().any(|file| file.path == "new/nested/file.txt"
        && file.is_untracked()
        && file.additions == 2));
    assert_eq!(file_commands, 2);
    assert!(review_commands > file_commands);
    println!("Git commands: full review {review_commands}, files {file_commands}");
}

#[test]
fn diff_file_uses_builtin_text_diff_when_external_diff_is_configured() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Threadlane"]);
    fs::write(dir.path().join("tracked.txt"), "original\n").unwrap();
    run_git(dir.path(), &["add", "tracked.txt"]);
    run_git(dir.path(), &["commit", "-qm", "initial"]);
    fs::write(dir.path().join("tracked.txt"), "changed\n").unwrap();

    let helper_path = if cfg!(windows) {
        let path = dir.path().join("external-diff.cmd");
        fs::write(&path, "@echo external-diff-sentinel\r\n").unwrap();
        path
    } else {
        let path = dir.path().join("external-diff.sh");
        fs::write(&path, "#!/bin/sh\necho \"external-diff-sentinel\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    };
    let helper_arg = helper_path.to_str().unwrap();
    run_git(dir.path(), &["config", "diff.external", helper_arg]);

    let diff = diff_file(dir.path(), "tracked.txt").unwrap();

    assert!(!diff.contains("external-diff-sentinel"));
    assert!(diff.contains("-original"));
    assert!(diff.contains("+changed"));
}

#[test]
fn diff_file_does_not_turn_clean_tracked_files_into_new_files() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Threadlane"]);
    fs::write(dir.path().join("tracked.txt"), "unchanged\n").unwrap();
    run_git(dir.path(), &["add", "tracked.txt"]);
    run_git(dir.path(), &["commit", "-qm", "initial"]);

    let diff = diff_file(dir.path(), "tracked.txt").unwrap();

    assert_eq!(diff, "No textual diff available for this file.\n");
}

#[test]
fn diff_file_rejects_paths_outside_workspace() {
    let dir = tempdir().unwrap();

    let error = diff_file(dir.path(), "../outside.txt").unwrap_err();

    assert!(error.message.contains("outside the workspace"));
}

#[test]
fn diff_file_preserves_git_errors() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file.txt"), "content\n").unwrap();

    let error = diff_file(dir.path(), "file.txt").unwrap_err();

    assert!(!error.message.is_empty());
}

fn init_diff_repo(root: &Path) {
    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    run_git(root, &["config", "user.name", "Threadlane"]);
    fs::write(root.join("tracked.txt"), "one two\nold value\n").unwrap();
    run_git(root, &["add", "tracked.txt"]);
    run_git(root, &["commit", "-qm", "initial"]);
}

#[test]
fn local_diff_options_filter_spacing_and_preserve_mixed_edits_without_mutations() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    init_diff_repo(root);
    fs::write(root.join("tracked.txt"), " one   two\nnew value\n").unwrap();
    let bytes = fs::read(root.join("tracked.txt")).unwrap();
    let index = fs::read(root.join(".git/index")).unwrap();
    let filtered = DiffOptions {
        ignore_whitespace: true,
    };
    for diff in [
        diff_file_with_options(root, "tracked.txt", filtered).unwrap(),
        worktree_diff_with_options(root, filtered).unwrap(),
    ] {
        assert!(diff.contains("-old value\n+new value"));
        assert!(!diff.contains("+ one   two"));
    }
    assert!(diff_file(root, "tracked.txt")
        .unwrap()
        .contains("+ one   two"));
    assert!(worktree_diff(root).unwrap().contains("+ one   two"));
    assert_eq!(fs::read(root.join("tracked.txt")).unwrap(), bytes);
    assert_eq!(fs::read(root.join(".git/index")).unwrap(), index);
}

#[test]
fn local_diff_successful_empty_head_does_not_fall_back_to_staged_changes() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    init_diff_repo(root);
    fs::write(root.join("tracked.txt"), "staged change\n").unwrap();
    run_git(root, &["add", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), " one   two\nold value\n").unwrap();
    let filtered = DiffOptions {
        ignore_whitespace: true,
    };
    assert!(diff_file_with_options(root, "tracked.txt", filtered)
        .unwrap()
        .is_empty());
    assert!(worktree_diff_with_options(root, filtered)
        .unwrap()
        .is_empty());
    assert!(worktree_diff(root).unwrap().contains("+ one   two"));
    fs::write(root.join("tracked.txt"), "one two\nold value\n").unwrap();
    assert!(worktree_diff_with_options(root, DiffOptions::default())
        .unwrap()
        .is_empty());
}

#[test]
fn local_diff_includes_untracked_files_alongside_tracked_changes_in_both_modes() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    init_diff_repo(root);
    run_git(root, &["config", "core.quotePath", "false"]);
    fs::write(root.join("tracked.txt"), " one two\nold value\n").unwrap();
    for name in [" spaced name.txt ", "日本語.txt", "line\nbreak.txt"] {
        fs::write(root.join(name), "new file\n").unwrap();
    }
    fs::write(root.join("binary.bin"), [0, 1, 0, 2]).unwrap();
    fs::write(root.join("empty.txt"), "").unwrap();
    for ignore_whitespace in [false, true] {
        let options = DiffOptions { ignore_whitespace };
        let combined = worktree_diff_with_options(root, options).unwrap();
        assert!(combined.contains(" spaced name.txt "));
        assert!(combined.contains("日本語.txt"));
        assert!(combined.contains("line\\nbreak.txt"));
        assert!(combined.contains("Binary files"));
        assert!(combined.contains("empty.txt"));
        assert_eq!(combined.contains("tracked.txt"), !ignore_whitespace);
        let single = diff_file_with_options(root, " spaced name.txt ", options).unwrap();
        assert!(single.contains("+new file"));
        assert!(diff_file_with_options(root, "binary.bin", options)
            .unwrap()
            .contains("Binary files"));
    }
}

#[test]
fn local_diff_supports_unborn_and_detached_head() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    run_git(root, &["config", "user.name", "Threadlane"]);
    fs::write(root.join("staged.txt"), "initial\n").unwrap();
    run_git(root, &["add", "staged.txt"]);
    fs::write(root.join("staged.txt"), "changed\n").unwrap();
    fs::write(root.join("new.txt"), "untracked\n").unwrap();
    for ignore_whitespace in [false, true] {
        let options = DiffOptions { ignore_whitespace };
        let diff = worktree_diff_with_options(root, options).unwrap();
        assert!(diff.contains("+initial"));
        assert!(diff.contains("+changed"));
        assert!(diff.contains("+untracked"));
        let single = diff_file_with_options(root, "staged.txt", options).unwrap();
        assert!(single.contains("+initial"));
        assert!(single.contains("+changed"));
    }
    run_git(root, &["commit", "-qm", "initial"]);
    run_git(root, &["checkout", "--detach", "-q", "HEAD"]);
    assert!(diff_file_with_options(
        root,
        "staged.txt",
        DiffOptions {
            ignore_whitespace: true
        }
    )
    .unwrap()
    .contains("+changed"));
}

#[test]
fn local_diff_preserves_binary_rename_mode_and_deletion_metadata() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    init_diff_repo(root);
    fs::write(root.join("binary.bin"), [0, 1, 0, 2]).unwrap();
    fs::write(root.join("delete.txt"), "delete me\n").unwrap();
    run_git(root, &["add", "binary.bin", "delete.txt"]);
    run_git(root, &["commit", "-qm", "metadata fixtures"]);
    run_git(root, &["mv", "tracked.txt", "renamed.txt"]);
    #[cfg(unix)]
    fs::set_permissions(root.join("renamed.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(root.join("binary.bin"), [0, 3, 0, 4]).unwrap();
    fs::remove_file(root.join("delete.txt")).unwrap();
    let diff = worktree_diff_with_options(
        root,
        DiffOptions {
            ignore_whitespace: true,
        },
    )
    .unwrap();
    assert!(diff.contains("rename to renamed.txt"));
    #[cfg(unix)]
    assert!(diff.contains("new mode 100755"));
    assert!(diff.contains("Binary files"));
    assert!(diff.contains("deleted file mode"));
}

#[test]
fn local_diff_options_preserve_errors_and_path_validation() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file.txt"), "file\n").unwrap();
    for ignore_whitespace in [false, true] {
        let options = DiffOptions { ignore_whitespace };
        assert!(diff_file_with_options(dir.path(), "file.txt", options).is_err());
        assert!(worktree_diff_with_options(dir.path(), options).is_err());
        assert!(diff_file_with_options(dir.path(), "../outside.txt", options).is_err());
    }
}

#[test]
fn local_diff_batches_untracked_files_without_repository_writes() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    init_diff_repo(root);
    run_git(root, &["config", "core.splitIndex", "true"]);
    fs::create_dir(root.join("new")).unwrap();
    for i in 0..500 {
        fs::write(root.join(format!("new/{i}.txt")), format!("file {i}\n")).unwrap();
    }
    fs::write(root.join("literal[1].txt"), "").unwrap();
    let index = fs::read(root.join(".git/index")).unwrap();
    let empty_oid = command(root, &["hash-object", "--stdin"]).unwrap();
    let empty_oid = empty_oid.trim();
    let empty_object = root
        .join(".git/objects")
        .join(&empty_oid[..2])
        .join(&empty_oid[2..]);
    assert!(!empty_object.exists());
    for ignore_whitespace in [false, true] {
        COMMAND_SPAWNS.set(0);
        let diff = worktree_diff_with_options(root, DiffOptions { ignore_whitespace }).unwrap();
        assert_eq!(COMMAND_SPAWNS.get(), 4);
        assert_eq!(diff.matches("diff --git ").count(), 501);
        assert!(diff.contains("a/new/499.txt b/new/499.txt"));
        assert!(diff.contains("literal[1].txt"));
        assert_eq!(fs::read(root.join(".git/index")).unwrap(), index);
        assert!(!empty_object.exists());
        assert!(!fs::read_dir(root.join(".git")).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("sharedindex.")
        }));
        assert_eq!(
            fs::read_to_string(root.join("new/499.txt")).unwrap(),
            "file 499\n"
        );
    }
}

#[test]
fn local_diff_disables_configured_text_conversion() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    init_diff_repo(root);
    fs::write(root.join(".gitattributes"), "*.txt diff=review-test\n").unwrap();
    run_git(root, &["config", "diff.review-test.textconv", "git --version"]);
    fs::write(root.join("tracked.txt"), "new value\n").unwrap();
    fs::write(root.join("new.txt"), "untracked value\n").unwrap();
    for ignore_whitespace in [false, true] {
        let options = DiffOptions { ignore_whitespace };
        assert!(diff_file_with_options(root, "tracked.txt", options)
            .unwrap()
            .contains("+new value"));
        assert!(diff_file_with_options(root, "new.txt", options)
            .unwrap()
            .contains("+untracked value"));
        let diff = worktree_diff_with_options(root, options).unwrap();
        assert!(diff.contains("+new value"));
        assert!(diff.contains("+untracked value"));
        assert!(!diff.contains("git version"));
    }
}

#[test]
fn parses_branch_and_change_state() {
    let status = parse_status(
            Path::new("/tmp/project"),
            "## feature/demo...origin/feature/demo [ahead 2, behind 1]\nM  staged.rs\n M working.rs\nMM mixed.rs\n?? new.rs\n",
        );
    assert_eq!(status.branch.as_deref(), Some("feature/demo"));
    assert_eq!(status.ahead, 2);
    assert_eq!(status.behind, 1);
    assert!(status.has_upstream);
    assert!(status.staged_changes);
    assert!(status.unstaged_changes);
    assert!(status.has_changes);
    let mixed = status
        .files
        .iter()
        .find(|file| file.path == "mixed.rs")
        .unwrap();
    assert_eq!(mixed.status, "MM");
    assert_eq!(mixed.status_for_section(true), 'M');
    assert_eq!(mixed.status_for_section(false), 'M');
    assert!(mixed.staged);
    assert!(mixed.unstaged);
}

#[test]
fn cache_entry_expires_after_ttl() {
    let started = std::time::Instant::now();
    let fresh = (started, "cached".to_string());

    assert_eq!(
        fresh_cache_value(&fresh, started, std::time::Duration::from_secs(30)),
        Some("cached".to_string())
    );
    assert_eq!(
        fresh_cache_value(
            &fresh,
            started + std::time::Duration::from_secs(31),
            std::time::Duration::from_secs(30),
        ),
        None
    );
}

#[test]
fn pr_cache_separates_branches_in_the_same_repository() {
    let repository = Path::new("/tmp/project");

    assert_ne!(
        pr_cache_key(repository, "feature/one"),
        pr_cache_key(repository, "feature/two")
    );
}

#[test]
fn parses_detached_head() {
    let status = parse_status(Path::new("/tmp/project"), "## HEAD\n");
    assert!(status.detached);
    assert!(status.branch.is_none());
}

#[test]
fn normalizes_renamed_paths() {
    let status = parse_status(
        Path::new("/tmp/project"),
        "## main\nR  old_name.rs -> new_name.rs\n",
    );
    assert_eq!(status.files[0].path, "new_name.rs");
    assert_eq!(status.files[0].status, "R");
}

#[test]
fn parses_nul_delimited_paths_and_renames() {
    let status = parse_status(
        Path::new("/tmp/project"),
        "## feature/demo\0?? line\nbreak.txt\0R  new name.txt\0old name.txt\0",
    );
    assert_eq!(status.files.len(), 2);
    assert_eq!(status.files[0].path, "line\nbreak.txt");
    assert_eq!(status.files[1].path, "new name.txt");
    assert_eq!(status.files[1].status, "R");
}

#[test]
fn preserves_leading_and_trailing_whitespace_in_nul_paths() {
    let status = parse_status(
        Path::new("/tmp/project"),
        "## feature/demo\0??  leading.txt \0",
    );
    assert_eq!(status.files[0].path, " leading.txt ");
}

#[test]
fn atomic_commit_groups_exclude_locks_and_order_sources_first() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial"]);
    std::fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.path().join("Cargo.lock"), "lock\n").unwrap();
    let groups = atomic_commit_groups(dir.path()).unwrap();
    assert_eq!(groups, vec![vec!["src.rs".to_string()]]);
}

#[test]
fn atomic_commit_execution_creates_one_commit_per_group() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    std::fs::write(dir.path().join("initial.txt"), "initial\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial"]);
    std::fs::write(dir.path().join("first.rs"), "fn first() {}\n").unwrap();
    std::fs::write(dir.path().join("second.rs"), "fn second() {}\n").unwrap();
    let groups = commit_atomic_groups(dir.path(), "atomic changes").unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(
        command(dir.path(), &["rev-list", "--count", "HEAD"])
            .unwrap()
            .trim(),
        "3"
    );
    assert!(!inspect(dir.path()).unwrap().has_changes);
}

#[test]
fn commit_message_diff_prefers_staged_changes_and_includes_untracked_files() {
    let root = std::env::temp_dir().join(format!(
        "threadlane-git-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    run_git(&root, &["init", "-q"]);
    run_git(&root, &["config", "user.email", "threadlane@example.com"]);
    run_git(&root, &["config", "user.name", "Threadlane"]);
    fs::write(root.join("tracked.txt"), "original\n").unwrap();
    run_git(&root, &["add", "tracked.txt"]);
    run_git(&root, &["commit", "-qm", "initial"]);

    fs::write(root.join("tracked.txt"), "staged\n").unwrap();
    run_git(&root, &["add", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), "staged\nunstaged\n").unwrap();
    fs::write(root.join("new.txt"), "new file\n").unwrap();

    let staged = commit_message_diff(&root).unwrap();
    assert!(staged.contains("+staged"));
    assert!(!staged.contains("+unstaged"));
    assert!(!staged.contains("new.txt"));

    run_git(&root, &["restore", "--staged", "tracked.txt"]);
    let working_tree = commit_message_diff(&root).unwrap();
    assert!(working_tree.contains("+unstaged"));
    assert!(working_tree.contains("new.txt"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn commit_message_diff_process_count_is_constant_for_untracked_files() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Threadlane"]);
    fs::write(dir.path().join("tracked.txt"), "initial\n").unwrap();
    run_git(dir.path(), &["add", "tracked.txt"]);
    run_git(dir.path(), &["commit", "-qm", "initial"]);
    for index in 0..8 {
        fs::write(dir.path().join(format!("new-{index}.txt")), "new\n").unwrap();
    }

    COMMAND_SPAWNS.set(0);
    let diff = commit_message_diff(dir.path()).unwrap();
    let spawn_count = COMMAND_SPAWNS.get();

    assert!(diff.contains("new-0.txt"));
    assert!(diff.contains("new-7.txt"));
    assert_eq!(spawn_count, 4);
}

#[test]
fn commit_message_diff_includes_untracked_files_before_first_commit() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-q"]);
    fs::write(dir.path().join("new.txt"), "new\n").unwrap();

    let diff = commit_message_diff(dir.path()).unwrap();

    assert!(diff.contains("new.txt"));
    assert!(diff.contains("+new"));
}

#[test]
fn parses_github_pr_json_with_checks_and_comments() {
    let sample = r#"{
            "number": 42,
            "title": "Center editor panel",
            "url": "https://github.com/threadlane/threadlane/pull/42",
            "state": "OPEN",
            "headRefName": "center_editor_panel",
            "baseRefName": "main",
            "comments": [
                {
                    "author": { "login": "reviewer1" },
                    "body": "Please double check the layout.",
                    "createdAt": "2026-08-19T00:00:00Z",
                    "path": "src/screens/editor/view.rs",
                    "line": 45
                },
                {
                    "author": { "login": "reviewer2" },
                    "body": "Looks great overall!",
                    "createdAt": "2026-08-19T00:05:00Z"
                },
                {
                    "author": { "login": "bot" },
                    "body": "Benchmark passed.",
                    "createdAt": "2026-08-19T00:10:00Z"
                }
            ],
            "statusCheckRollup": [
                {
                    "name": "cargo-test",
                    "status": "COMPLETED",
                    "conclusion": "FAILURE",
                    "detailsUrl": "https://github.com/threadlane/threadlane/actions/runs/1"
                },
                {
                    "name": "cargo-check",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "detailsUrl": "https://github.com/threadlane/threadlane/actions/runs/2"
                },
                {
                    "name": "e2e-tests",
                    "status": "IN_PROGRESS",
                    "conclusion": null,
                    "detailsUrl": "https://github.com/threadlane/threadlane/actions/runs/3"
                }
            ]
        }"#;

    let pr = parse_gh_pr_json(sample).unwrap();
    assert_eq!(pr.number, 42);
    assert_eq!(pr.title, "Center editor panel");
    assert!(!pr.issue_comments[0].viewer_did_author);
    assert_eq!(pr.head_ref, "center_editor_panel");
    assert_eq!(pr.base_ref, "main");
    assert!(!pr.is_draft);
    assert_eq!(pr.comments_count, 3);
    assert_eq!(pr.issue_comments.len(), 3);
    assert_eq!(pr.issue_comments[0].author, "reviewer1");
    assert!(pr.review_comments.is_empty());
    assert_eq!(pr.total_checks, 3);
    assert_eq!(pr.failing_checks, 1);
    assert_eq!(pr.passing_checks, 1);
    assert_eq!(pr.pending_checks, 1);

    let draft_sample = r#"{
            "number": 43,
            "title": "WIP Feature",
            "url": "https://github.com/threadlane/threadlane/pull/43",
            "state": "OPEN",
            "isDraft": true,
            "headRefName": "wip-feature",
            "baseRefName": "main",
            "comments": [],
            "statusCheckRollup": []
        }"#;
    let draft_pr = parse_gh_pr_json(draft_sample).unwrap();
    assert!(draft_pr.is_draft);
    assert_eq!(draft_pr.state, "OPEN");

    let merged_sample = r#"{
            "number": 44,
            "title": "Merged Feature",
            "url": "https://github.com/threadlane/threadlane/pull/44",
            "state": "MERGED",
            "isDraft": false,
            "headRefName": "merged-feature",
            "baseRefName": "main",
            "comments": [],
            "statusCheckRollup": []
        }"#;
    let merged_pr = parse_gh_pr_json(merged_sample).unwrap();
    assert!(!merged_pr.is_draft);
    assert_eq!(merged_pr.state, "MERGED");
}

#[test]
fn github_issue_parses_declared_fields_and_optional_values() {
    let issue = parse_github_issue_json(
            r#"{
                "number": 123,
                "title": "Ship agent workspace",
                "state": "OPEN",
                "url": "https://github.com/threadlane/threadlane/issues/123",
                "body": "Track the work.",
                "updatedAt": "2026-08-30T12:00:00Z",
                "author": { "login": "octocat" },
                "assignees": [{ "login": "maintainer" }],
                "labels": [{ "name": "feature", "color": "a2eeef", "description": "New feature" }],
                "comments": [{ "id": "IC_1", "author": { "login": "reviewer" }, "body": "Looks good", "createdAt": "2026-08-30T13:00:00Z", "url": "https://github.com/threadlane/threadlane/issues/123#issuecomment-1" }]
            }"#,
        )
        .unwrap();

    assert_eq!(issue.summary.issue.host, "github.com");
    assert_eq!(issue.summary.issue.owner, "threadlane");
    assert_eq!(issue.summary.issue.repo, "threadlane");
    assert_eq!(issue.summary.issue.number, 123);
    assert_eq!(issue.summary.updated_at, "2026-08-30T12:00:00Z");
    assert_eq!(issue.summary.assignees, ["maintainer"]);
    assert_eq!(issue.summary.labels[0].name, "feature");
    assert_eq!(issue.body, "Track the work.");
    assert_eq!(issue.comments[0].remote_id, "IC_1");

    let optional = parse_github_issue_json(
        r#"{"number":1,"title":"Minimal","state":"OPEN","url":"https://github.com/o/r/issues/1"}"#,
    )
    .unwrap();
    assert!(optional.body.is_empty());
    assert!(optional.comments.is_empty());
    assert!(optional.summary.assignees.is_empty());
    assert!(parse_github_issue_json("not json").is_err());
}

#[test]
fn github_issue_parses_pull_request_detail_fields() {
    let pr = parse_gh_pr_json(
            r#"{
                "number": 42,
                "title": "Center editor panel",
                "url": "https://github.com/threadlane/threadlane/pull/42",
                "state": "OPEN",
                "isDraft": false,
                "headRefName": "feature/panel",
                "baseRefName": "main",
                "headRefOid": "abc123",
                "body": "PR body",
                "updatedAt": "2026-08-30T12:00:00Z",
                "author": { "login": "author" },
                "reviewDecision": "APPROVED",
                "comments": [{ "id": "IC_2", "author": { "login": "commenter" }, "body": "Issue comment", "createdAt": "2026-08-30T12:01:00Z", "url": "https://github.com/threadlane/threadlane/pull/42#issuecomment-2" }],
                "reviews": [{ "id": "PRR_1", "author": { "login": "reviewer" }, "body": "LGTM", "state": "APPROVED", "submittedAt": "2026-08-30T12:02:00Z" }],
                "files": [{ "path": "src/lib.rs", "additions": 5, "deletions": 2, "changeType": "MODIFIED" }],
                "statusCheckRollup": [{ "name": "check", "status": "COMPLETED", "conclusion": "SUCCESS" }]
            }"#,
        )
        .unwrap();

    assert_eq!(pr.author, "author");
    assert_eq!(pr.updated_at, "2026-08-30T12:00:00Z");
    assert_eq!(pr.head_oid, "abc123");
    assert_eq!(pr.review_decision.as_deref(), Some("APPROVED"));
    assert_eq!(pr.issue_comments[0].remote_id, "IC_2");
    assert_eq!(pr.reviews[0].remote_id, "PRR_1");
    assert_eq!(pr.files[0].path, "src/lib.rs");
    assert_eq!(pr.checks[0].name, "check");
}

#[test]
fn github_issue_keeps_conversation_and_review_comments_separate() {
    let mut pr = parse_gh_pr_json(
            r#"{
                "number": 42,
                "url": "https://github.com/threadlane/threadlane/pull/42",
                "comments": [
                    { "id": "IC_2", "author": { "login": "agent-bot" }, "body": "Agent reply", "createdAt": "2026-08-30T12:01:00Z", "url": "https://github.com/threadlane/threadlane/pull/42#issuecomment-2", "viewerDidAuthor": true },
                    { "id": "IC_3", "author": { "login": "reviewer" }, "body": "Reviewer feedback", "createdAt": "2026-08-30T12:02:00Z", "url": "https://github.com/threadlane/threadlane/pull/42#issuecomment-3", "viewerDidAuthor": false }
                ]
            }"#,
        )
        .unwrap();

    assert_eq!(pr.issue_comments.len(), 2);
    assert_eq!(pr.comments_count, 2);
    assert!(pr.issue_comments[0].viewer_did_author);
    assert!(!pr.issue_comments[1].viewer_did_author);
    assert!(pr.review_comments.is_empty());
    assert!(!pr.review_comments_complete);

    enrich_pr_review_comments(
            &mut pr,
            r#"[{"id": 99, "in_reply_to_id": 41, "user": { "login": "reviewer" }, "body": "Inline note", "path": "src/lib.rs", "line": 12, "created_at": "2026-08-30T12:02:00Z"}]"#,
        )
        .unwrap();

    assert_eq!(pr.issue_comments.len(), 2);
    assert_eq!(pr.comments_count, 2);
    assert_eq!(pr.review_comments.len(), 1);
    assert_eq!(pr.review_comments[0].remote_id, "99");
    assert_eq!(pr.review_comments[0].in_reply_to_id.as_deref(), Some("41"));
    assert!(pr.review_comments_complete);
}

#[test]
fn github_review_comment_hydration_is_atomic_and_malformed_data_stays_incomplete() {
    let existing = PrReviewComment {
        remote_id: "existing".into(),
        body: "retained".into(),
        ..Default::default()
    };
    let mut pr = GitHubPrInfo {
        review_comments: vec![existing.clone()],
        review_comments_complete: true,
        ..Default::default()
    };
    assert!(enrich_pr_review_comments(
        &mut pr,
        r#"[{"id": 99, "body": "valid"}, {"id": 100, "body": 17}]"#,
    )
    .is_err());
    assert_eq!(pr.review_comments, [existing]);
    assert!(!pr.review_comments_complete);

    let mut legacy = serde_json::to_value(GitHubPrInfo::default()).unwrap();
    let legacy = legacy.as_object_mut().unwrap();
    legacy.remove("review_comments_complete");
    legacy.remove("commits");
    let deserialized =
        serde_json::from_value::<GitHubPrInfo>(serde_json::Value::Object(legacy.clone())).unwrap();
    assert!(!deserialized.review_comments_complete);
    assert!(deserialized.commits.is_empty());
}

#[test]
fn github_mutation_args_match_gh_contract_and_reject_invalid_input() {
    let plain_issue_args = github_issue_list_args("open", None, 50).unwrap();
    assert!(!plain_issue_args
        .iter()
        .any(|argument| argument == "--search"));
    for args in [
        github_issue_list_args("open", Some("older task"), 50).unwrap(),
        github_pr_list_args("open", Some("older task"), 100).unwrap(),
    ] {
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--search", "older task"]));
    }
    assert_eq!(
        github_issue_list_args("closed", Some("bug label:desktop"), 100).unwrap(),
        vec![
            "issue",
            "list",
            "--state",
            "closed",
            "--search",
            "bug label:desktop",
            "--limit",
            "100",
            "--json",
            "number,title,state,url,updatedAt,author,assignees,labels,comments"
        ]
    );
    assert_eq!(
        github_issue_view_args(123).unwrap(),
        vec![
            "issue",
            "view",
            "123",
            "--json",
            "number,title,state,url,body,updatedAt,author,assignees,labels,comments"
        ]
    );
    assert_eq!(
            github_pr_list_args("open", None, 50).unwrap(),
            vec![
                "pr",
                "list",
                "--state",
                "open",
                "--limit",
                "50",
                "--json",
                "number,title,state,url,isDraft,headRefName,baseRefName,updatedAt,author,reviewDecision,statusCheckRollup"
            ]
        );
    assert_eq!(
        github_api_args(
            "github.example.com",
            &["GET", "repos/acme/app/pulls/42/comments"]
        ),
        vec![
            "api",
            "--hostname",
            "github.example.com",
            "GET",
            "repos/acme/app/pulls/42/comments",
        ]
    );
    assert_eq!(
        github_pr_list_args("merged", None, 50).unwrap()[3],
        "merged"
    );
    assert!(github_issue_list_args("all", None, 50).is_err());
    assert!(github_issue_list_args("open", None, 0).is_err());
    assert!(github_issue_list_args("merged", None, 50).is_err());
    assert!(github_pr_list_args("closed", Some("\n"), 50).is_err());
    assert_eq!(
        create_draft_pr_args("main", "Title", "Body").unwrap(),
        vec!["pr", "create", "--draft", "--base", "main", "--title", "Title", "--body", "Body"]
    );
    assert_eq!(
        github_pr_comment_args(42, "Body").unwrap(),
        vec!["pr", "comment", "42", "--body", "Body"]
    );
    assert_eq!(
        github_pr_review_args(42, PullRequestReviewVerdict::RequestChanges, "Body").unwrap(),
        vec!["pr", "review", "42", "--request-changes", "--body", "Body"]
    );
    assert!(github_issue_view_args(0).is_err());
    assert!(create_draft_pr_args("main", " ", "Body").is_err());
    assert!(create_draft_pr_args("main", "Title", " ").is_err());
    assert!(github_pr_comment_args(0, "Body").is_err());
    assert!(github_pr_review_args(42, PullRequestReviewVerdict::Comment, " ").is_err());
    assert!(review_comment_payloads(
        "commit",
        &[PullRequestReviewCommentDraft {
            path: "../outside.rs".into(),
            body: "Note".into()
        }]
    )
    .is_err());
    assert!(review_comment_payloads(
        "commit",
        &[PullRequestReviewCommentDraft {
            path: "/absolute.rs".into(),
            body: "Note".into()
        }]
    )
    .is_err());
    assert!(review_comment_payloads(
        "commit",
        &[PullRequestReviewCommentDraft {
            path: "src/lib.rs".into(),
            body: " ".into()
        }]
    )
    .is_err());

    let payloads = review_comment_payloads(
        "abc123",
        &[PullRequestReviewCommentDraft {
            path: "src/lib.rs".into(),
            body: "Minor note".into(),
        }],
    )
    .unwrap();
    assert_eq!(
        payloads,
        vec![serde_json::json!({
            "commit_id": "abc123",
            "path": "src/lib.rs",
            "body": "Minor note",
            "subject_type": "file"
        })]
    );
}

#[test]
fn github_issue_create_args_match_gh_contract() {
    assert_eq!(
        github_issue_create_args("Title", "Body").unwrap(),
        vec!["issue", "create", "--title", "Title", "--body", "Body"]
    );
    assert!(github_issue_create_args(" ", "Body").is_err());
}

#[test]
fn parses_gh_issue_create_output() {
    assert_eq!(
        parse_gh_issue_create_output("https://github.com/acme/project/issues/42\n").unwrap(),
        42
    );
    assert_eq!(
        parse_gh_issue_create_output("https://github.example.com/acme/project/issues/7\n").unwrap(),
        7
    );
    assert!(parse_gh_issue_create_output("not an issue URL").is_err());
    assert!(parse_gh_issue_create_output("https://github.com/acme/project/issues/0").is_err());
}

#[test]
fn github_mutation_args_put_stored_token_only_in_environment() {
    let command = gh_command_with_token(
        Path::new("/tmp/project"),
        &["issue", "list"],
        Some("stored-token"),
    );
    assert_eq!(command.get_args().collect::<Vec<_>>(), ["issue", "list"]);
    assert!(command.get_args().all(|arg| arg != "stored-token"));
    assert!(command
        .get_envs()
        .any(|(key, value)| key == "GH_TOKEN" && value == Some("stored-token".as_ref())));
}

#[test]
fn github_mutation_args_redacts_tokens_from_gh_failures() {
    let redacted = redact_gh_failure(
        "gh failed: GH_TOKEN=stored-token and bearer stored-token",
        Some("stored-token"),
    );

    assert!(!redacted.contains("stored-token"));
    assert!(!redacted.contains("GH_TOKEN="));
    assert!(redacted.contains("<redacted>"));
}

#[test]
fn github_mutation_args_failure_uses_the_token_captured_for_its_command() {
    let (command, captured_token) = gh_command_with_token_capture(
        Path::new("/tmp/project"),
        &["issue", "list"],
        Some("injected-token".to_owned()),
    );

    assert!(command
        .get_envs()
        .any(|(key, value)| key == "GH_TOKEN" && value == Some("injected-token".as_ref())));
    assert_eq!(
        gh_failure_message(
            "remote said bearer injected-token",
            captured_token.as_deref()
        ),
        "remote said bearer <redacted>"
    );
}

#[test]
fn github_mutation_args_rejects_zero_or_mismatched_review_url_numbers() {
    let mismatched = GitHubPrInfo {
        number: 42,
        url: "https://github.com/threadlane/threadlane/pull/41".into(),
        ..Default::default()
    };
    let zero = GitHubPrInfo {
        number: 42,
        url: "https://github.com/threadlane/threadlane/pull/0".into(),
        ..Default::default()
    };

    assert!(validated_review_endpoint(&mismatched).is_err());
    assert!(validated_review_endpoint(&zero).is_err());
}

#[test]
fn safe_branch_deletion_preserves_unmerged_and_checked_out_branches() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    fs::write(dir.path().join("base.txt"), "base\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial"]);
    assert!(delete_branch(dir.path(), "main", false).is_err());

    create_branch(dir.path(), "unmerged").unwrap();
    fs::write(dir.path().join("feature.txt"), "keep this work\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "feature"]);
    checkout(dir.path(), "main").unwrap();
    assert!(delete_branch(dir.path(), "unmerged", false).is_err());
    assert!(diff_branch(dir.path(), "unmerged")
        .unwrap()
        .contains("keep this work"));

    let worktree = dir.path().join("linked");
    run_git(
        dir.path(),
        &["worktree", "add", "-b", "linked", worktree.to_str().unwrap()],
    );
    assert!(delete_branch(dir.path(), "linked", false).is_err());
    assert!(worktree.join("base.txt").exists());
    let branches = list_branches_detailed(dir.path(), None).unwrap();
    for name in ["main", "unmerged", "linked"] {
        assert!(branches.iter().any(|branch| branch.name == name));
    }
}
#[test]
fn branch_lifecycle_and_merge() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    fs::write(dir.path().join("base.txt"), "base\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial commit"]);

    // Create feature branch
    create_branch(dir.path(), "feature-1").unwrap();
    let status = inspect(dir.path()).unwrap();
    assert_eq!(status.branch.as_deref(), Some("feature-1"));

    // Commit on feature branch
    fs::write(dir.path().join("feature.txt"), "feature content\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "add feature"]);

    // Detailed branches
    let branches = list_branches_detailed(dir.path(), None).unwrap();
    assert!(branches
        .iter()
        .any(|b| b.name == "feature-1" && b.is_current));
    assert!(branches.iter().any(|b| b.name == "main"));

    // Switch back to main
    checkout(dir.path(), "main").unwrap();
    let status = inspect(dir.path()).unwrap();
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert!(diff_branch(dir.path(), "feature-1")
        .unwrap()
        .contains("feature.txt"));

    // Merge feature-1 into main
    merge(dir.path(), "feature-1").unwrap();
    assert!(dir.path().join("feature.txt").exists());
    delete_branch(dir.path(), "feature-1", false).unwrap();
    assert!(!list_branches_detailed(dir.path(), None)
        .unwrap()
        .iter()
        .any(|branch| branch.name == "feature-1"));
}

#[test]
fn switch_branch_with_stash_and_carry() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    fs::write(dir.path().join("base.txt"), "base\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial commit"]);

    create_branch(dir.path(), "branch-a").unwrap();
    create_branch(dir.path(), "branch-b").unwrap();

    // Switch to branch-a and create uncommitted change
    checkout(dir.path(), "branch-a").unwrap();
    fs::write(dir.path().join("dirty.txt"), "dirty work\n").unwrap();
    assert!(!inspect(dir.path()).unwrap().files.is_empty());

    // Stash and switch to branch-b
    checkout_with_stash(dir.path(), "branch-b").unwrap();
    let status_b = inspect(dir.path()).unwrap();
    assert_eq!(status_b.branch.as_deref(), Some("branch-b"));
    // Dirty file should have been stashed
    assert!(status_b.files.is_empty());

    // Switch carrying changes test
    fs::write(dir.path().join("carry.txt"), "carry me\n").unwrap();
    assert!(!inspect(dir.path()).unwrap().files.is_empty());
    checkout_carrying_changes(dir.path(), "main").unwrap();
    let status_main = inspect(dir.path()).unwrap();
    assert_eq!(status_main.branch.as_deref(), Some("main"));
    assert!(dir.path().join("carry.txt").exists());
}

#[test]
fn pull_request_creation_rejects_detached_head() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    fs::write(dir.path().join("base.txt"), "base\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial commit"]);
    run_git(dir.path(), &["checkout", "--detach", "HEAD"]);

    let error = create_pull_request(dir.path()).unwrap_err();
    assert!(error.message.contains("detached HEAD"));
}

#[test]
fn stash_pop_and_drop_lifecycle() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    fs::write(dir.path().join("base.txt"), "base\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial commit"]);

    create_branch(dir.path(), "feature").unwrap();
    checkout(dir.path(), "feature").unwrap();

    fs::write(dir.path().join("work.txt"), "in-progress work\n").unwrap();
    checkout_with_stash(dir.path(), "main").unwrap();

    // Switch back to feature branch
    checkout(dir.path(), "feature").unwrap();
    let status = inspect(dir.path()).unwrap();
    assert!(status.current_stash.is_some());
    let current = status.current_stash.as_ref().unwrap();
    assert!(current.files.is_empty());
    let files = inspect_stash_files(dir.path(), current.index);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "work.txt");

    let diff = diff_stash_file(dir.path(), 0, "work.txt").unwrap();
    assert!(diff.contains("in-progress work"));

    fs::write(dir.path().join("second.txt"), "second stash\n").unwrap();
    run_git(dir.path(), &["stash", "push", "-u", "-m", "second stash"]);
    assert_eq!(list_stashes(dir.path()).unwrap().len(), 2);
    drop_stash(dir.path(), Some(0)).unwrap();
    assert_eq!(list_stashes(dir.path()).unwrap().len(), 1);

    pop_stash(dir.path(), Some(0)).unwrap();
    assert!(dir.path().join("work.txt").exists());
    let status_after_pop = inspect(dir.path()).unwrap();
    assert_eq!(status_after_pop.stashes.len(), 0);

    fs::write(dir.path().join("third.txt"), "third stash\n").unwrap();
    run_git(dir.path(), &["stash", "push", "-u", "-m", "third stash"]);
    pop_stash(dir.path(), None).unwrap();
    assert!(dir.path().join("third.txt").exists());
    assert!(inspect(dir.path()).unwrap().stashes.is_empty());
}

#[test]
fn inspect_does_not_expose_a_stash_from_another_branch() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    fs::write(dir.path().join("base.txt"), "base\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial"]);
    create_branch(dir.path(), "feature").unwrap();
    fs::write(dir.path().join("feature.txt"), "work\n").unwrap();
    checkout_with_stash(dir.path(), "main").unwrap();

    let status = inspect(dir.path()).unwrap();

    assert!(status.current_stash.is_none());
    assert_eq!(status.stashes.len(), 1);
}

#[test]
fn git_metadata_parsing_preserves_pipe_characters() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test | Author"]);
    fs::write(dir.path().join("base.txt"), "base\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "subject | detail"]);
    run_git(dir.path(), &["branch", "topic|branch"]);

    let commits = list_commits(dir.path(), 1).unwrap();
    let branches = list_branches_detailed(dir.path(), None).unwrap();

    assert_eq!(commits[0].author_name, "Test | Author");
    assert_eq!(commits[0].summary, "subject | detail");
    assert!(branches.iter().any(|branch| branch.name == "topic|branch"));
}

#[test]
fn stash_metadata_parsing_preserves_pipe_characters() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    fs::write(dir.path().join("base.txt"), "base\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial"]);
    fs::write(dir.path().join("work.txt"), "work\n").unwrap();
    run_git(
        dir.path(),
        &["stash", "push", "-u", "-m", "message | detail"],
    );

    let stashes = list_stashes(dir.path()).unwrap();

    assert_eq!(stashes.len(), 1);
    assert!(stashes[0].message.ends_with("message | detail"));
}

#[test]
fn commit_file_inspection_uses_the_destination_of_a_rename() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    fs::write(dir.path().join("old.txt"), "contents\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial"]);
    run_git(dir.path(), &["mv", "old.txt", "new.txt"]);
    run_git(dir.path(), &["commit", "-qm", "rename file"]);
    let sha = command(dir.path(), &["rev-parse", "HEAD"]).unwrap();

    let files = inspect_commit_files(dir.path(), sha.trim());

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "new.txt");
    assert_eq!(files[0].status_char(), 'R');
}

#[test]
fn file_mutations_reject_paths_outside_the_workspace() {
    let dir = tempdir().unwrap();

    assert!(discard_file_changes(dir.path(), "../outside.txt").is_err());
    assert!(ignore_file(dir.path(), "../outside.txt").is_err());
}

#[test]
fn file_discard_and_ignore_lifecycle() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    fs::write(dir.path().join("file1.txt"), "original\n").unwrap();
    fs::write(dir.path().join("file2.md"), "markdown\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial commit"]);

    // 1. Modify tracked file and discard
    fs::write(dir.path().join("file1.txt"), "modified\n").unwrap();
    assert_eq!(inspect(dir.path()).unwrap().files.len(), 1);
    discard_file_changes(dir.path(), "file1.txt").unwrap();
    assert_eq!(inspect(dir.path()).unwrap().files.len(), 0);
    assert_eq!(
        fs::read_to_string(dir.path().join("file1.txt")).unwrap(),
        "original\n"
    );

    // 2. Create untracked file and discard
    fs::write(dir.path().join("untracked.rs"), "fn main() {}\n").unwrap();
    assert_eq!(inspect(dir.path()).unwrap().files.len(), 1);
    discard_file_changes(dir.path(), "untracked.rs").unwrap();
    assert!(!dir.path().join("untracked.rs").exists());
    assert_eq!(inspect(dir.path()).unwrap().files.len(), 0);

    // 3. Ignore file
    fs::write(dir.path().join("secret.env"), "KEY=123\n").unwrap();
    assert_eq!(inspect(dir.path()).unwrap().files.len(), 1);
    ignore_file(dir.path(), "secret.env").unwrap();
    assert!(dir.path().join(".gitignore").exists());
    let gitignore = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(gitignore.contains("/secret.env"));

    // 4. Ignore extension
    ignore_extension(dir.path(), "log").unwrap();
    let gitignore2 = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(gitignore2.contains("*.log"));
}

#[test]
fn discard_multiple_files_and_all_changes() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    fs::write(dir.path().join("tracked1.txt"), "t1 original\n").unwrap();
    fs::write(dir.path().join("tracked2.txt"), "t2 original\n").unwrap();
    fs::write(dir.path().join("tracked3.txt"), "t3 original\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial commit"]);

    // Test rejection of path outside workspace in discard_files
    assert!(discard_files(dir.path(), &["tracked1.txt", "../outside.txt"]).is_err());

    // 1. Modify multiple tracked files and create untracked, discard specific subset
    fs::write(dir.path().join("tracked1.txt"), "t1 modified\n").unwrap();
    fs::write(dir.path().join("tracked2.txt"), "t2 modified\n").unwrap();
    fs::write(dir.path().join("untracked1.txt"), "u1 content\n").unwrap();
    fs::write(dir.path().join("untracked2.txt"), "u2 content\n").unwrap();
    assert_eq!(inspect(dir.path()).unwrap().files.len(), 4);

    // Discard tracked1 and untracked1
    discard_files(dir.path(), &["tracked1.txt", "untracked1.txt"]).unwrap();
    let status = inspect(dir.path()).unwrap();
    assert_eq!(status.files.len(), 2);
    assert_eq!(
        fs::read_to_string(dir.path().join("tracked1.txt")).unwrap(),
        "t1 original\n"
    );
    assert!(!dir.path().join("untracked1.txt").exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("tracked2.txt")).unwrap(),
        "t2 modified\n"
    );
    assert!(dir.path().join("untracked2.txt").exists());

    // 2. Discard all remaining changes
    discard_all_changes(dir.path()).unwrap();
    assert_eq!(inspect(dir.path()).unwrap().files.len(), 0);
    assert_eq!(
        fs::read_to_string(dir.path().join("tracked2.txt")).unwrap(),
        "t2 original\n"
    );
    assert!(!dir.path().join("untracked2.txt").exists());

    // 3. Test with deletions and staged additions
    fs::remove_file(dir.path().join("tracked3.txt")).unwrap();
    fs::write(dir.path().join("new_staged.txt"), "staged\n").unwrap();
    run_git(dir.path(), &["add", "new_staged.txt"]);
    assert_eq!(inspect(dir.path()).unwrap().files.len(), 2);
    discard_all_changes(dir.path()).unwrap();
    assert_eq!(inspect(dir.path()).unwrap().files.len(), 0);
    assert!(dir.path().join("tracked3.txt").exists());
    assert!(!dir.path().join("new_staged.txt").exists());

    // 4. Test staged rename discard preserves both paths
    run_git(dir.path(), &["mv", "tracked1.txt", "renamed1.txt"]);
    let status = inspect(dir.path()).unwrap();
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.files[0].path, "renamed1.txt");
    assert_eq!(status.files[0].orig_path.as_deref(), Some("tracked1.txt"));

    discard_file_changes(dir.path(), "renamed1.txt").unwrap();
    assert_eq!(inspect(dir.path()).unwrap().files.len(), 0);
    assert!(dir.path().join("tracked1.txt").exists());
    assert!(!dir.path().join("renamed1.txt").exists());

    // 5. Test staged rename discard via discard_all_changes
    run_git(dir.path(), &["mv", "tracked2.txt", "renamed2.txt"]);
    assert_eq!(inspect(dir.path()).unwrap().files.len(), 1);
    discard_all_changes(dir.path()).unwrap();
    assert_eq!(inspect(dir.path()).unwrap().files.len(), 0);
    assert!(dir.path().join("tracked2.txt").exists());
    assert!(!dir.path().join("renamed2.txt").exists());
}

#[test]
fn commit_history_inspection() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "dev@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Dev"]);
    fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "feat: initial commit"]);

    fs::write(dir.path().join("b.txt"), "second file\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "feat: second commit"]);

    let commits = list_commits(dir.path(), 10).unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].summary, "feat: second commit");
    assert_eq!(commits[0].author_name, "Dev");
    assert_eq!(commits[1].summary, "feat: initial commit");

    let files = inspect_commit_files(dir.path(), &commits[0].sha);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "b.txt");

    let diff = diff_commit_file(dir.path(), &commits[0].sha, "b.txt").unwrap();
    assert!(diff.contains("second file"));
}

#[test]
fn worktree_lifecycle_and_listing() {
    let dir = tempdir().unwrap();
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "dev@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Dev"]);
    fs::write(dir.path().join("base.txt"), "hello worktree\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial commit"]);

    assert!(is_git_repo(dir.path()));

    let worktree_dir = dir.path().join(".threadlane/worktrees/task_1");
    create_worktree(dir.path(), &worktree_dir, "worktree/task_1").unwrap();
    assert_eq!(
        primary_worktree_root(&worktree_dir).unwrap(),
        dir.path().canonicalize().unwrap()
    );
    assert!(worktree_dir.join("base.txt").exists());

    let worktrees = list_worktrees(dir.path()).unwrap();
    assert!(worktrees.len() >= 2);
    assert!(worktrees
        .iter()
        .any(|wt| wt.branch.as_deref() == Some("worktree/task_1")));

    // Write changes in worktree
    fs::write(worktree_dir.join("new_feature.txt"), "isolated feature\n").unwrap();
    assert!(!dir.path().join("new_feature.txt").exists());

    remove_worktree(dir.path(), &worktree_dir, true).unwrap();
    assert!(!worktree_dir.exists());
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn init_repo(dir: &Path) {
    run_git(dir, &["init", "-b", "main"]);
    run_git(dir, &["config", "user.email", "test@example.com"]);
    run_git(dir, &["config", "user.name", "Test"]);
    run_git(dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("README.md"), "hi").unwrap();
    run_git(dir, &["add", "."]);
    run_git(dir, &["commit", "-m", "init"]);
}

#[test]
fn orphaned_subagent_worktree_is_reused_when_clean() {
    use crate::git::{create_worktree, list_worktrees, reclaim_subagent_worktree};
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    let slot = dir
        .path()
        .join(".threadlane/worktrees/subagents/lane-1");
    let branch = "threadlane/subagent-lane-1";
    create_worktree(dir.path(), &slot, branch).unwrap();
    // Simulate the crash: path+branch left behind, work clean.
    assert!(reclaim_subagent_worktree(dir.path(), &slot, branch).unwrap());
    assert!(list_worktrees(dir.path())
        .unwrap()
        .iter()
        .any(|worktree| canonical(&worktree.path) == canonical(&slot)));
}

#[test]
fn dirty_orphan_is_cleared_for_recreation() {
    use crate::git::{create_worktree, list_worktrees, reclaim_subagent_worktree};
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    let slot = dir
        .path()
        .join(".threadlane/worktrees/subagents/lane-2");
    let branch = "threadlane/subagent-lane-2";
    create_worktree(dir.path(), &slot, branch).unwrap();
    std::fs::write(slot.join("README.md"), "modified in dead run").unwrap();
    assert!(!reclaim_subagent_worktree(dir.path(), &slot, branch).unwrap());
    // Slot cleared: provisioning recreates without "already used".
    create_worktree(dir.path(), &slot, branch).unwrap();
    assert!(list_worktrees(dir.path())
        .unwrap()
        .iter()
        .any(|worktree| canonical(&worktree.path) == canonical(&slot)));
}

#[test]
fn branch_checked_out_elsewhere_is_refused() {
    use crate::git::{create_worktree, reclaim_subagent_worktree};
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    let elsewhere = dir.path().join("elsewhere");
    let branch = "threadlane/subagent-lane-3";
    create_worktree(dir.path(), &elsewhere, branch).unwrap();
    let slot = dir
        .path()
        .join(".threadlane/worktrees/subagents/lane-3");
    let error = reclaim_subagent_worktree(dir.path(), &slot, branch).unwrap_err();
    assert!(
        error.to_string().contains("already checked out"),
        "unexpected error: {error}"
    );
}

#[test]
fn reclaim_refuses_paths_outside_threadlane() {
    use crate::git::reclaim_subagent_worktree;
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    let outside = dir.path().join("outside");
    let error =
        reclaim_subagent_worktree(dir.path(), &outside, "threadlane/subagent-x").unwrap_err();
    assert!(error.to_string().contains("outside .threadlane"));
}

#[test]
fn pr_list_skips_bad_entries_and_keeps_the_rest() {
    use crate::github::{rate_limit_message, summarize_pr_list};
    let good = serde_json::json!({
        "number": 7,
        "title": "Good",
        "url": "https://github.com/o/r/pull/7",
        "state": "OPEN",
        "isDraft": false,
        "headRefName": "feat",
        "baseRefName": "main",
        "updatedAt": "2026-09-01T00:00:00Z",
    });
    let no_number = serde_json::json!({"title": "No number", "url": ""});
    let bad_url = serde_json::json!({
        "number": 8,
        "title": "Bad url",
        "url": "not-a-url",
        "state": "OPEN",
    });
    let rows = summarize_pr_list(vec![good, no_number, bad_url]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].number, 7);
    assert_eq!(rows[0].title, "Good");
}

#[test]
fn rate_limit_messages_are_detected() {
    assert!(rate_limit_message("API rate limit exceeded for user ID 1.").is_some());
    assert!(rate_limit_message("You have exceeded a secondary rate limit.").is_some());
    assert!(rate_limit_message("HTTP 403: rate limit exceeded").is_some());
    assert!(rate_limit_message("gh: Not Found (HTTP 404)").is_none());
    assert!(rate_limit_message("").is_none());
    let guidance = rate_limit_message("API rate limit exceeded for user ID 1.").unwrap();
    assert!(guidance.contains("Retry") || guidance.contains("retry") || guidance.contains("Wait"));
}

#[test]
fn issue_mutations_validate_before_spawning_gh() {
    let dir = tempdir().unwrap();
    assert!(create_github_issue(dir.path(), "", "body").is_err());
    assert!(create_github_issue(dir.path(), "   ", "body").is_err());
    assert!(set_github_issue_state(dir.path(), 0, true).is_err());
    assert!(delete_github_issue(dir.path(), 0).is_err());
    // Empty label edits are no-ops without spawning gh.
    assert!(edit_github_issue_labels(dir.path(), 1, &[], &[]).is_ok());
}

#[test]
fn draft_pr_diff_uses_selected_base_and_excludes_uncommitted_changes() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    run_git(root, &["init", "-q", "-b", "base"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    run_git(root, &["config", "user.name", "Threadlane"]);
    fs::write(root.join("file.txt"), "base\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-qm", "base"]);
    run_git(root, &["checkout", "-qb", "feature"]);
    fs::write(root.join("file.txt"), "committed feature\n").unwrap();
    run_git(root, &["commit", "-qam", "feature"]);
    let diff = draft_pr_diff(root, "base").unwrap();
    assert!(diff.contains("+committed feature"));
    assert!(diff.contains("-base"));
    assert!(draft_pr_diff(root, "feature").unwrap().is_empty());
    fs::write(root.join("file.txt"), "staged edit\n").unwrap();
    run_git(root, &["add", "."]);
    fs::write(root.join("file.txt"), "unstaged edit\n").unwrap();
    fs::write(root.join("untracked.txt"), "untracked\n").unwrap();
    assert_eq!(draft_pr_diff(root, "base").unwrap(), diff);
    // A remote-only base must work, too.
    run_git(root, &["update-ref", "refs/remotes/origin/base", "base"]);
    run_git(root, &["branch", "-D", "base"]);
    assert_eq!(draft_pr_diff(root, "base").unwrap(), diff);
    assert!(draft_pr_diff(root, "missing").is_err());
    assert!(draft_pr_diff(root, "--output=oops").is_err());
}

#[test]
fn worktree_from_selected_base_preserves_checkout_and_rejects_bad_refs() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    run_git(root, &["init", "-q", "-b", "main"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    run_git(root, &["config", "user.name", "Threadlane"]);
    fs::write(root.join("file.txt"), "main").unwrap();
    run_git(root, &["add", "file.txt"]);
    run_git(root, &["commit", "-qm", "base"]);
    run_git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    run_git(
        root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    run_git(root, &["checkout", "-qb", "unrelated"]);
    fs::write(root.join("file.txt"), "unrelated").unwrap();
    run_git(root, &["commit", "-qam", "unrelated"]);
    fs::write(root.join("file.txt"), "local edits").unwrap();
    let (default, bases) = worktree_bases(root).unwrap();
    assert_eq!(default, "origin/main");
    assert!(bases.contains(&"main".into()) && bases.contains(&"origin/main".into()));
    let checkout = root.join("checkout");
    create_worktree_from(root, &checkout, "worktree/fix-login", &default).unwrap();
    assert_eq!(
        fs::read_to_string(checkout.join("file.txt")).unwrap(),
        "main"
    );
    assert_eq!(
        fs::read_to_string(root.join("file.txt")).unwrap(),
        "local edits"
    );
    assert_eq!(current_branch(root).unwrap().as_deref(), Some("unrelated"));
    assert!(create_worktree_from(root, &root.join("bad"), "worktree/new", "--help").is_err());
    assert!(!root.join("bad").exists());
    assert!(create_worktree_from(root, &root.join("other"), "worktree/fix-login", "main").is_err());
}

#[test]
fn worktree_bases_refreshes_refs_and_selects_only_existing_defaults() {
    let remote = tempdir().unwrap();
    let local = tempdir().unwrap();
    run_git(remote.path(), &["init", "-q", "-b", "main"]);
    run_git(remote.path(), &["config", "user.email", "test@example.com"]);
    run_git(remote.path(), &["config", "user.name", "Test"]);
    run_git(
        remote.path(),
        &["commit", "--allow-empty", "-qm", "initial"],
    );
    run_git(remote.path(), &["branch", "obsolete"]);
    run_git(local.path(), &["init", "-q", "-b", "trunk"]);
    assert!(worktree_bases(local.path()).is_err());
    run_git(local.path(), &["config", "user.email", "test@example.com"]);
    run_git(local.path(), &["config", "user.name", "Test"]);
    run_git(local.path(), &["commit", "--allow-empty", "-qm", "local"]);
    assert_eq!(worktree_bases(local.path()).unwrap().0, "trunk");
    run_git(
        local.path(),
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    assert_eq!(worktree_bases(local.path()).unwrap().0, "origin/main");
    run_git(remote.path(), &["branch", "-D", "obsolete"]);
    run_git(remote.path(), &["branch", "new-base"]);
    run_git(remote.path(), &["commit", "--allow-empty", "-qm", "moved"]);
    let (default, branches) = worktree_bases(local.path()).unwrap();
    assert_eq!(default, "origin/main");
    assert!(branches.contains(&"origin/new-base".to_string()));
    assert!(!branches.contains(&"origin/obsolete".to_string()));
    assert_eq!(
        command(local.path(), &["rev-parse", "origin/main"]).unwrap(),
        command(remote.path(), &["rev-parse", "HEAD"]).unwrap()
    );
    run_git(
        local.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "/nonexistent/threadlane-test-remote",
        ],
    );
    assert_eq!(worktree_bases(local.path()).unwrap(), (default, branches));
}

fn init_fetch_repo_pair() -> (tempfile::TempDir, tempfile::TempDir) {
    let remote = tempdir().unwrap();
    let local = tempdir().unwrap();
    for dir in [remote.path(), local.path()] {
        run_git(dir, &["init", "-q", "-b", "main"]);
        run_git(dir, &["config", "user.email", "test@example.com"]);
        run_git(dir, &["config", "user.name", "Test"]);
    }
    run_git(
        remote.path(),
        &["commit", "--allow-empty", "-qm", "initial"],
    );
    run_git(
        local.path(),
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    (remote, local)
}

#[test]
fn sync_remote_coalesces_fetches_within_a_window() {
    let (_remote, local) = init_fetch_repo_pair();
    REMOTE_FETCH_RUNS.set(0);
    sync_remote(local.path()).unwrap();
    assert_eq!(REMOTE_FETCH_RUNS.get(), 1);
    // Within the window the remote state is shared, not refetched.
    sync_remote(local.path()).unwrap();
    assert_eq!(REMOTE_FETCH_RUNS.get(), 1);
    // An explicit Fetch always runs and re-arms the window.
    fetch(local.path()).unwrap();
    assert_eq!(REMOTE_FETCH_RUNS.get(), 2);
    sync_remote(local.path()).unwrap();
    assert_eq!(REMOTE_FETCH_RUNS.get(), 2);
}

#[test]
fn sync_remote_shares_one_fetch_across_linked_worktrees() {
    let (_remote, local) = init_fetch_repo_pair();
    run_git(local.path(), &["commit", "--allow-empty", "-qm", "local"]);
    let sibling = local.path().join("sibling");
    run_git(
        local.path(),
        &[
            "worktree",
            "add",
            "-q",
            sibling.to_str().unwrap(),
            "-b",
            "sibling",
        ],
    );
    REMOTE_FETCH_RUNS.set(0);
    sync_remote(local.path()).unwrap();
    assert_eq!(REMOTE_FETCH_RUNS.get(), 1);
    // A linked worktree shares refs/remotes with the root, so its sync
    // is already fresh — no second remote call.
    sync_remote(&sibling).unwrap();
    assert_eq!(REMOTE_FETCH_RUNS.get(), 1);
}

#[test]
fn sync_remote_retries_after_a_failed_fetch() {
    let (_remote, local) = init_fetch_repo_pair();
    run_git(
        local.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "/nonexistent/threadlane-test-remote",
        ],
    );
    REMOTE_FETCH_RUNS.set(0);
    assert!(sync_remote(local.path()).is_err());
    // A failure is not freshness-stamped: the next sync retries.
    assert!(sync_remote(local.path()).is_err());
    assert_eq!(REMOTE_FETCH_RUNS.get(), 2);
}

#[test]
fn pull_marks_remote_refs_fresh_for_background_syncs() {
    let (remote, _keep) = init_fetch_repo_pair();
    let parent = tempdir().unwrap();
    run_git(
        parent.path(),
        &[
            "clone",
            "-q",
            remote.path().to_str().unwrap(),
            "local",
        ],
    );
    let local = parent.path().join("local");
    pull(&local).unwrap();
    REMOTE_FETCH_RUNS.set(0);
    // The pull already refreshed remote-tracking refs.
    sync_remote(&local).unwrap();
    assert_eq!(REMOTE_FETCH_RUNS.get(), 0);
}

#[test]
fn list_project_files_reports_tracked_and_nonignored_untracked() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    fs::write(dir.path().join("tracked.rs"), "fn main() {}\n").unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    fs::write(dir.path().join(".gitignore"), "ignored/\nsecret.env\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial"]);

    fs::write(dir.path().join("untracked.rs"), "x\n").unwrap();
    fs::create_dir_all(dir.path().join("ignored")).unwrap();
    fs::write(dir.path().join("ignored/hidden.rs"), "x\n").unwrap();
    fs::write(dir.path().join("secret.env"), "TOKEN=1\n").unwrap();
    fs::create_dir_all(dir.path().join(".threadlane")).unwrap();
    fs::write(dir.path().join(".threadlane/state.json"), "{}\n").unwrap();

    let inventory = list_project_files(dir.path()).unwrap();
    assert_eq!(inventory.non_utf8_skipped, 0);
    assert!(inventory.paths.contains(&"tracked.rs".to_string()));
    assert!(inventory.paths.contains(&"src/lib.rs".to_string()));
    assert!(inventory.paths.contains(&"untracked.rs".to_string()));
    assert!(inventory.paths.contains(&".gitignore".to_string()));
    // Ignored names and Threadlane state are never enumerated.
    assert!(!inventory
        .paths
        .iter()
        .any(|path| path.contains("ignored") || path.contains("secret.env")));
    assert!(!inventory
        .paths
        .iter()
        .any(|path| path == ".threadlane" || path.starts_with(".threadlane/")
            || path == ".git" || path.starts_with(".git/")));
}

#[test]
fn list_project_files_rejects_non_repositories() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file.rs"), "x\n").unwrap();
    assert_eq!(
        list_project_files(dir.path()),
        Err(FileInventoryError::NotARepository)
    );
}

#[test]
fn list_project_files_works_inside_linked_worktrees() {
    let main = tempdir().unwrap();
    init_repo(main.path());
    fs::write(main.path().join("base.rs"), "x\n").unwrap();
    run_git(main.path(), &["add", "base.rs"]);
    run_git(main.path(), &["commit", "-qm", "initial"]);

    let linked = main.path().join("linked-worktree");
    run_git(
        main.path(),
        &[
            "worktree",
            "add",
            linked.to_str().unwrap(),
            "-b",
            "linked-branch",
        ],
    );
    fs::write(linked.join("new.rs"), "x\n").unwrap();

    // A linked worktree has a `.git` FILE, not a directory — it must not leak.
    assert!(linked.join(".git").is_file());
    let inventory = list_project_files(&linked).unwrap();
    assert_eq!(
        inventory.paths,
        vec!["README.md".to_string(), "base.rs".to_string(), "new.rs".to_string()]
    );
}

#[test]
fn list_project_files_round_trips_unusual_names() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    for name in [
        "a file.rs",
        "tick`name.rs",
        "日本語のファイル.rs",
        " leading space.rs",
        "trailing space .rs",
    ] {
        fs::write(dir.path().join(name), "x\n").unwrap();
    }
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-qm", "initial"]);

    let inventory = list_project_files(dir.path()).unwrap();
    for name in [
        "a file.rs",
        "tick`name.rs",
        "日本語のファイル.rs",
        " leading space.rs",
        "trailing space .rs",
    ] {
        assert!(
            inventory.paths.contains(&name.to_string()),
            "missing {name:?} in {:?}",
            inventory.paths
        );
    }
}

#[cfg(unix)]
#[test]
fn list_project_files_skips_non_utf8_names() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let dir = tempdir().unwrap();
    init_repo(dir.path());
    fs::write(dir.path().join("normal.rs"), "x\n").unwrap();
    run_git(dir.path(), &["add", "normal.rs"]);

    // A filename that is not valid UTF-8 must be skipped, not lossy-decoded.
    // APFS refuses to create such a name at all, so stage it straight into
    // the index by reusing normal.rs's blob.
    let sha = Command::new("git")
        .args(["rev-parse", ":normal.rs"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(sha.status.success());
    let mut spec = b"100644,".to_vec();
    spec.extend_from_slice(&sha.stdout);
    spec.extend_from_slice(b",bad-\xFFname.rs");
    spec.retain(|byte| *byte != b'\n');
    let output = Command::new("git")
        .arg("update-index")
        .arg("--add")
        .arg("--cacheinfo")
        .arg(OsString::from_vec(spec))
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "update-index failed: {output:?}");

    let inventory = list_project_files(dir.path()).unwrap();
    assert_eq!(inventory.non_utf8_skipped, 1);
    assert_eq!(
        inventory.paths,
        vec!["README.md".to_string(), "normal.rs".to_string()]
    );
}

#[cfg(unix)]
#[test]
fn list_project_files_does_not_traverse_external_symlinks() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    init_repo(dir.path());
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("outside.rs"), "x\n").unwrap();
    fs::write(dir.path().join("inside.rs"), "x\n").unwrap();
    symlink(outside.path(), dir.path().join("link-out")).unwrap();

    let inventory = list_project_files(dir.path()).unwrap();
    // The symlink itself is a valid name; nothing under it is enumerated.
    assert!(inventory.paths.contains(&"link-out".to_string()));
    assert!(inventory.paths.contains(&"inside.rs".to_string()));
    assert!(!inventory
        .paths
        .iter()
        .any(|path| path.contains("outside") || path.starts_with("link-out/")));
}

#[test]
fn list_project_files_drops_index_entries_deleted_on_disk() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    fs::write(dir.path().join("gone.rs"), "x\n").unwrap();
    run_git(dir.path(), &["add", "gone.rs"]);
    run_git(dir.path(), &["commit", "-qm", "add gone"]);

    fs::remove_file(dir.path().join("gone.rs")).unwrap();
    // The index still names `gone.rs` until the deletion is staged, but the
    // file does not exist, so it must not be offered for insertion.
    let inventory = list_project_files(dir.path()).unwrap();
    assert_eq!(inventory.paths, vec!["README.md".to_string()]);
}

fn viewed_page_fixture(
    nodes: &str,
    has_next_page: bool,
    end_cursor: Option<&str>,
) -> String {
    let cursor = end_cursor
        .map(|cursor| format!("\"{cursor}\""))
        .unwrap_or_else(|| "null".to_owned());
    format!(
        r#"{{"data": {{"repository": {{"pullRequest": {{"id": "PR_node_1", "headRefOid": "head123", "files": {{"pageInfo": {{"hasNextPage": {has_next_page}, "endCursor": {cursor}}}, "nodes": [{nodes}]}}}}}}, "viewer": {{"login": "octocat"}}}}}}"#
    )
}

#[test]
fn viewed_state_page_parses_statuses_and_pagination() {
    let page = parse_pr_viewed_state_page(&viewed_page_fixture(
        r#"{"path": "src/lib.rs", "viewerViewedState": "VIEWED"},
        {"path": "src/view.rs", "viewerViewedState": "UNVIEWED"},
        {"path": "docs/guide.md", "viewerViewedState": "DISMISSED"},
        {"path": "unicode/文件.rs", "viewerViewedState": "VIEWED"},
        {"path": "other.rs"}"#,
        true,
        Some("cursor-2"),
    ))
    .unwrap();

    assert_eq!(page.pull_request_id, "PR_node_1");
    assert_eq!(page.head_oid, "head123");
    assert_eq!(page.viewer, "octocat");
    assert!(page.has_next_page);
    assert_eq!(page.end_cursor.as_deref(), Some("cursor-2"));
    assert_eq!(page.files.len(), 5);
    assert_eq!(page.files[0].status, PrFileViewedStatus::Viewed);
    assert_eq!(page.files[1].status, PrFileViewedStatus::Unviewed);
    assert_eq!(page.files[2].status, PrFileViewedStatus::ChangedSinceViewed);
    assert_eq!(page.files[3].path, "unicode/文件.rs");
    assert_eq!(page.files[4].status, PrFileViewedStatus::Unknown);
}

#[test]
fn viewed_state_page_rejects_graphql_errors_and_missing_pull_request() {
    let errors = r#"{"data": {"repository": {"pullRequest": null}, "viewer": {"login": "octocat"}}, "errors": [{"message": "Could not resolve to a PullRequest"}]}"#;
    assert!(parse_pr_viewed_state_page(errors)
        .unwrap_err()
        .contains("Could not resolve"));

    let missing_pr = r#"{"data": {"repository": {"pullRequest": null}, "viewer": {"login": "octocat"}}}"#;
    assert!(parse_pr_viewed_state_page(missing_pr).is_err());

    let missing_id = r#"{"data": {"repository": {"pullRequest": {"headRefOid": "h", "files": {"pageInfo": {"hasNextPage": false}, "nodes": []}}}, "viewer": {"login": "octocat"}}}"#;
    assert!(parse_pr_viewed_state_page(missing_id).is_err());
    assert!(parse_pr_viewed_state_page("not json").is_err());
}

#[test]
fn viewed_state_collection_paginates_within_budget() {
    let repository = GitHubRepository {
        host: "github.com".into(),
        owner: "threadlane".into(),
        repo: "app".into(),
    };
    let first = viewed_page_fixture(
        r#"{"path": "a.rs", "viewerViewedState": "VIEWED"}"#,
        true,
        Some("cursor-2"),
    );
    let second = viewed_page_fixture(
        r#"{"path": "b.rs", "viewerViewedState": "UNVIEWED"}"#,
        false,
        None,
    );
    let mut pages = vec![Ok(first), Ok(second)].into_iter();
    let mut cursors = Vec::new();
    let state = collect_pr_viewed_state(Path::new("/tmp/project"), &repository, 42, |payload| {
        cursors.push(payload["variables"]["cursor"].as_str().map(str::to_owned));
        assert_eq!(payload["variables"]["owner"], "threadlane");
        assert_eq!(payload["variables"]["repo"], "app");
        assert_eq!(payload["variables"]["number"], 42);
        pages.next().unwrap()
    })
    .unwrap();

    assert_eq!(
        cursors,
        vec![None, Some("cursor-2".to_owned())]
    );
    assert!(state.complete);
    assert_eq!(state.pull_request_id, "PR_node_1");
    assert_eq!(state.head_oid, "head123");
    assert_eq!(state.viewer, "octocat");
    assert_eq!(state.files.len(), 2);
    assert_eq!(state.file_status("a.rs"), PrFileViewedStatus::Viewed);
    assert_eq!(state.file_status("b.rs"), PrFileViewedStatus::Unviewed);
    assert_eq!(state.file_status("absent.rs"), PrFileViewedStatus::Unknown);
}

#[test]
fn viewed_state_collection_flags_incomplete_and_rejects_inconsistent_pages() {
    let repository = GitHubRepository {
        host: "github.com".into(),
        owner: "threadlane".into(),
        repo: "app".into(),
    };
    // hasNextPage without a cursor must not loop or claim completeness.
    let dangling = viewed_page_fixture(
        r#"{"path": "a.rs", "viewerViewedState": "VIEWED"}"#,
        true,
        None,
    );
    let state = collect_pr_viewed_state(Path::new("/tmp/project"), &repository, 42, |_| {
        Ok(dangling.clone())
    })
    .unwrap();
    assert!(!state.complete);
    assert_eq!(state.files.len(), 1);

    // A page from a different PR or account is rejected instead of merged.
    let other = viewed_page_fixture(
        r#"{"path": "b.rs", "viewerViewedState": "UNVIEWED"}"#,
        false,
        None,
    )
    .replace("PR_node_1", "PR_node_2");
    let mut pages = vec![
        Ok(viewed_page_fixture(
            r#"{"path": "a.rs", "viewerViewedState": "VIEWED"}"#,
            true,
            Some("cursor-2"),
        )),
        Ok(other),
    ]
    .into_iter();
    assert!(collect_pr_viewed_state(Path::new("/tmp/project"), &repository, 42, |_| {
        pages.next().unwrap()
    })
    .is_err());

    // A head change between pages means the merged markers would mix
    // revisions; the read is rejected so a refresh can settle it.
    let moved_head = viewed_page_fixture(
        r#"{"path": "b.rs", "viewerViewedState": "UNVIEWED"}"#,
        false,
        None,
    )
    .replace("head123", "head456");
    let mut pages = vec![
        Ok(viewed_page_fixture(
            r#"{"path": "a.rs", "viewerViewedState": "VIEWED"}"#,
            true,
            Some("cursor-2"),
        )),
        Ok(moved_head),
    ]
    .into_iter();
    assert!(collect_pr_viewed_state(Path::new("/tmp/project"), &repository, 42, |_| {
        pages.next().unwrap()
    })
    .is_err());
}

#[test]
fn viewed_state_payloads_use_variables_for_pr_and_path_data() {
    let payload = pr_viewed_state_payload("acme", "app", 42, Some("cursor-9"));
    assert!(payload["query"].as_str().unwrap().contains("viewerViewedState"));
    assert_eq!(
        payload["variables"],
        serde_json::json!({
            "owner": "acme",
            "repo": "app",
            "number": 42,
            "cursor": "cursor-9",
        })
    );

    let mark = pr_file_viewed_mutation_payload("PR_node_1", "src/lib.rs", true).unwrap();
    assert!(mark["query"]
        .as_str()
        .unwrap()
        .contains("markFileAsViewed"));
    assert_eq!(
        mark["variables"],
        serde_json::json!({"pullRequestId": "PR_node_1", "path": "src/lib.rs"})
    );
    let unmark = pr_file_viewed_mutation_payload("PR_node_1", "src/lib.rs", false).unwrap();
    assert!(unmark["query"]
        .as_str()
        .unwrap()
        .contains("unmarkFileAsViewed"));

    assert!(pr_file_viewed_mutation_payload(" ", "src/lib.rs", true).is_err());
    assert!(pr_file_viewed_mutation_payload("PR_node_1", "../outside.rs", true).is_err());
    assert!(pr_file_viewed_mutation_payload("PR_node_1", "/absolute.rs", true).is_err());
}
