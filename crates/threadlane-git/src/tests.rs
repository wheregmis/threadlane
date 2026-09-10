    use super::*;
    use std::fs;
    use std::path::Path;
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
                "comments": [{ "id": "IC_2", "author": { "login": "commenter" }, "body": "Issue comment", "createdAt": "2026-08-30T12:01:00Z", "url": "https://github.com/threadlane/threadlane/pull/42#issuecomment-2" }]
            }"#,
        )
        .unwrap();

        assert_eq!(pr.issue_comments.len(), 1);
        assert_eq!(pr.comments_count, 1);
        assert!(pr.review_comments.is_empty());
        assert!(!pr.review_comments_complete);

        enrich_pr_review_comments(
            &mut pr,
            r#"[{"id": 99, "in_reply_to_id": 41, "user": { "login": "reviewer" }, "body": "Inline note", "path": "src/lib.rs", "line": 12, "created_at": "2026-08-30T12:02:00Z"}]"#,
        )
        .unwrap();

        assert_eq!(pr.issue_comments.len(), 1);
        assert_eq!(pr.comments_count, 1);
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
            serde_json::from_value::<GitHubPrInfo>(serde_json::Value::Object(legacy.clone()))
                .unwrap();
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
            assert!(args.windows(2).any(|pair| pair == ["--search", "older task"]));
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
