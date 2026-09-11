use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use threadlane_git::GitStatus;

use super::draft_pr::{
    draft_pr_prefill, DraftPrAttemptState, DraftPrCompletion, DraftPrContextKey, DraftPrFields,
    DraftPrRemoteResult,
};
use super::types::{
    can_create_pull_request, can_publish_branch, discard_options, message_generated_matches_active_project,
    selection_bar_discard_options, DiscardOption,
};
use super::view::{retain_review_selection, scan_project_tree};

fn paths(values: &[&str]) -> HashSet<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn review_refresh_defaults_once_then_preserves_an_empty_selection() {
    let mut selected = HashSet::new();
    let mut initialized = false;

    retain_review_selection(
        &mut selected,
        paths(&["src/a.rs", "src/b.rs"]),
        &mut initialized,
    );
    assert_eq!(selected, paths(&["src/a.rs", "src/b.rs"]));

    selected.clear();
    retain_review_selection(
        &mut selected,
        paths(&["src/a.rs", "src/b.rs", "src/c.rs"]),
        &mut initialized,
    );
    assert!(selected.is_empty());
}

#[test]
fn review_refresh_does_not_select_everything_when_selected_files_disappear() {
    let mut selected = paths(&["src/removed.rs"]);
    let mut initialized = true;

    retain_review_selection(
        &mut selected,
        paths(&["src/a.rs", "src/b.rs"]),
        &mut initialized,
    );

    assert!(selected.is_empty());
}
#[test]
fn generated_commit_messages_only_apply_to_the_originating_checkout() {
    let origin = std::path::Path::new("/projects/app/.threadlane/worktrees/session-a");
    let other = std::path::Path::new("/projects/app/.threadlane/worktrees/session-b");

    assert!(message_generated_matches_active_project(
        origin,
        Some(origin)
    ));
    assert!(!message_generated_matches_active_project(
        origin,
        Some(other)
    ));
    assert!(!message_generated_matches_active_project(origin, None));
}

#[test]
fn generated_commit_messages_follow_conventional_commits() {
    assert_eq!(
        super::types::normalize_generated_commit_message("```\nCommit: Fix branch picker\n```"),
        "fix: branch picker"
    );
    assert_eq!(
        super::types::normalize_generated_commit_message("feat: add generated commit messages\nDetails"),
        "feat: add generated commit messages"
    );
}

#[test]
fn only_publishable_branches_without_upstreams_use_the_publish_action() {
    let unpublished = GitStatus {
        branch: Some("feature/demo".into()),
        remote: Some("git@github.com:threadlane/threadlane.git".into()),
        ahead: 731,
        ..GitStatus::default()
    };
    assert!(can_publish_branch(true, Some(&unpublished)));
    assert!(!can_publish_branch(false, Some(&unpublished)));

    let published = GitStatus {
        has_upstream: true,
        ..unpublished.clone()
    };
    assert!(!can_publish_branch(true, Some(&published)));

    let detached = GitStatus {
        detached: true,
        branch: None,
        ..unpublished
    };
    assert!(!can_publish_branch(true, Some(&detached)));
    assert!(!can_publish_branch(true, None));
}

#[test]
fn draft_pr_gate_only_allows_ready_published_named_branches() {
    let ready = GitStatus {
        branch: Some("feature/demo".into()),
        remote: Some("git@github.com:threadlane/threadlane.git".into()),
        has_upstream: true,
        pr_ready: true,
        pr_lookup_available: true,
        ..GitStatus::default()
    };
    assert!(can_create_pull_request(true, Some(&ready)));
    assert!(!can_create_pull_request(false, Some(&ready)));
    let blockers: [fn(&mut GitStatus); 7] = [
        |status| status.pr_lookup_available = false,
        |status| status.has_upstream = false,
        |status| status.remote = None,
        |status| status.branch = Some(" ".into()),
        |status| status.pr = Some(Default::default()),
        |status| status.pr_ready = false,
        |status| {
            status.detached = true;
            status.branch = None;
        },
    ];
    for block in blockers {
        let mut blocked = ready.clone();
        block(&mut blocked);
        assert!(!can_create_pull_request(true, Some(&blocked)));
    }
    assert!(!can_create_pull_request(true, None));
}

#[test]
fn draft_pr_fields_validate_every_value_required_by_the_existing_backend() {
    assert!(draft_fields("Improve review flow").validate().is_ok());
    for (base, title, body) in [
        (" ", "Title", "Body"),
        ("main", "\n", "Body"),
        ("main", "Title", ""),
    ] {
        assert!(DraftPrFields {
            base: base.into(),
            title: title.into(),
            body: body.into(),
        }
        .validate()
        .is_err());
    }
}

#[test]
fn draft_prefill_reuses_default_branch_and_latest_commit_metadata() {
    let status = GitStatus {
        branch: Some("feature/draft-pr".into()),
        default_branch: Some("develop".into()),
        recent_commits: vec![threadlane_git::GitCommitInfo {
            summary: "Add draft PR workflow".into(),
            body: "Keep publishing explicit and recoverable.".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    assert_eq!(
        draft_pr_prefill(&status),
        DraftPrFields {
            base: "develop".into(),
            title: "Add draft PR workflow".into(),
            body: "Keep publishing explicit and recoverable.".into(),
        }
    );

    let fallback = draft_pr_prefill(&GitStatus {
        branch: Some("feature/draft-pr".into()),
        ..Default::default()
    });
    assert_eq!(
        fallback,
        DraftPrFields {
            base: "main".into(),
            title: "feature/draft-pr".into(),
            body: "feature/draft-pr".into(),
        }
    );
}

fn draft_key(branch: &str, revision: u64) -> DraftPrContextKey {
    DraftPrContextKey {
        project: PathBuf::from("/project/.threadlane/worktrees/task"),
        branch: branch.into(),
        revision,
    }
}

fn draft_fields(title: &str) -> DraftPrFields {
    DraftPrFields {
        base: "main".into(),
        title: title.into(),
        body: "Body".into(),
    }
}

#[test]
fn draft_pr_attempts_are_single_flight_and_snapshot_safe() {
    let mut state = DraftPrAttemptState::default();
    let key = draft_key("feature/a", 1);
    let fields = draft_fields("First");
    let first = state.begin(key.clone(), fields.clone()).unwrap();
    assert!(state.begin(key.clone(), fields.clone()).is_err());

    assert_eq!(
        state.complete(
            &first,
            &draft_key("feature/b", 2),
            &fields,
            DraftPrRemoteResult::Exists("https://github.com/o/r/pull/1".into()),
        ),
        DraftPrCompletion::Stale
    );
    assert!(state.is_busy());
    assert_eq!(
        state.complete(
            &first,
            &key,
            &fields,
            DraftPrRemoteResult::Exists("https://github.com/o/r/pull/1".into()),
        ),
        DraftPrCompletion::SuccessExact("https://github.com/o/r/pull/1".into())
    );

    let edited = state.begin(key.clone(), fields.clone()).unwrap();
    assert_eq!(
        state.complete(
            &edited,
            &key,
            &draft_fields("Newer edit"),
            DraftPrRemoteResult::Exists("https://github.com/o/r/pull/2".into()),
        ),
        DraftPrCompletion::SuccessWithNewerEdits("https://github.com/o/r/pull/2".into())
    );

    let failed = state.begin(key.clone(), fields.clone()).unwrap();
    assert_eq!(
        state.complete(
            &failed,
            &key,
            &fields,
            DraftPrRemoteResult::Absent("offline".into()),
        ),
        DraftPrCompletion::Failure("offline".into())
    );
    assert!(state.begin(key, fields).is_ok());
}

#[test]
fn ambiguous_draft_pr_write_requires_explicit_readback_before_another_post() {
    let mut state = DraftPrAttemptState::default();
    let key = draft_key("feature/a", 1);
    let fields = draft_fields("First");
    let attempt = state.begin(key.clone(), fields.clone()).unwrap();

    assert_eq!(
        state.complete(
            &attempt,
            &key,
            &fields,
            DraftPrRemoteResult::Unknown("network result unknown".into()),
        ),
        DraftPrCompletion::Unknown("network result unknown".into())
    );
    assert!(state.begin(key.clone(), fields.clone()).is_err());

    let check = state.begin_check().unwrap();
    assert!(state.begin_check().is_err());
    assert!(matches!(
        state.complete(
            &check,
            &key,
            &fields,
            DraftPrRemoteResult::Unknown("still unknown".into()),
        ),
        DraftPrCompletion::Unknown(_)
    ));
    assert!(state.begin(key.clone(), fields.clone()).is_err());
    let check = state.begin_check().unwrap();
    assert_eq!(
        state.complete(
            &check,
            &key,
            &fields,
            DraftPrRemoteResult::Absent("not present".into()),
        ),
        DraftPrCompletion::Failure("not present".into())
    );
    let attempt = state.begin(key.clone(), fields.clone()).unwrap();
    assert!(matches!(
        state.complete(
            &attempt,
            &key,
            &fields,
            DraftPrRemoteResult::Unknown("unknown".into()),
        ),
        DraftPrCompletion::Unknown(_)
    ));
    let check = state.begin_check().unwrap();
    assert_eq!(
        state.complete(
            &check,
            &key,
            &fields,
            DraftPrRemoteResult::Exists("https://github.com/o/r/pull/3".into()),
        ),
        DraftPrCompletion::SuccessExact("https://github.com/o/r/pull/3".into())
    );
    assert!(state.begin_check().is_err());
}

#[test]
fn project_scan_is_bounded_and_skips_generated_roots() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("threadlane-panel-{nonce}"));
    std::fs::create_dir_all(root.join("src/nested")).unwrap();
    std::fs::create_dir_all(root.join("target/debug")).unwrap();
    std::fs::create_dir_all(root.join(".threadlane/sessions")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join("src/nested/lib.rs"), "pub fn value() {}\n").unwrap();
    std::fs::write(root.join("target/debug/generated"), "ignored").unwrap();

    let items = scan_project_tree(&root, 10);
    assert_eq!(
        items
            .iter()
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>(),
        vec!["src"]
    );
    assert!(items[0]
        .children
        .iter()
        .any(|item| item.relative_path == "src/main.rs"));
    assert!(items[0]
        .children
        .iter()
        .any(|item| item.relative_path == "src/nested"));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn discard_options_for_single_file() {
    let selected = vec!["src/a.rs".to_string()];
    let options = discard_options("src/a.rs", &selected, 1);
    assert_eq!(options, vec![DiscardOption::Single("src/a.rs".to_string())]);
    assert_eq!(options[0].label(), "Discard Changes...");
}

#[test]
fn discard_options_when_all_files_selected() {
    let selected = vec![
        "src/a.rs".to_string(),
        "src/b.rs".to_string(),
        "src/c.rs".to_string(),
    ];
    let options = discard_options("src/a.rs", &selected, 3);
    assert_eq!(
        options,
        vec![
            DiscardOption::Single("src/a.rs".to_string()),
            DiscardOption::All(3),
        ]
    );
    assert_eq!(options[0].label(), "Discard Changes...");
    assert_eq!(options[1].label(), "Discard All Changes (3)...");
}

#[test]
fn discard_options_when_subset_of_files_selected() {
    let selected = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
    let options = discard_options("src/c.rs", &selected, 5);
    assert_eq!(
        options,
        vec![
            DiscardOption::Single("src/c.rs".to_string()),
            DiscardOption::Selected(vec!["src/a.rs".to_string(), "src/b.rs".to_string()]),
            DiscardOption::All(5),
        ]
    );
    assert_eq!(options[0].label(), "Discard Changes...");
    assert_eq!(options[1].label(), "Discard Selected Changes (2)...");
    assert_eq!(options[2].label(), "Discard All Changes (5)...");
}

#[test]
fn discard_options_when_no_files_selected() {
    let selected: Vec<String> = Vec::new();
    let options = discard_options("src/a.rs", &selected, 4);
    assert_eq!(
        options,
        vec![
            DiscardOption::Single("src/a.rs".to_string()),
            DiscardOption::All(4),
        ]
    );
    assert_eq!(options[0].label(), "Discard Changes...");
    assert_eq!(options[1].label(), "Discard All Changes (4)...");
}

#[test]
fn selection_bar_discard_options_scenarios() {
    // 0 files: empty
    assert!(selection_bar_discard_options(&[], 0).is_empty());

    // 1 file, 1 selected
    let opts1 = selection_bar_discard_options(&["src/a.rs".to_string()], 1);
    assert_eq!(opts1, vec![DiscardOption::All(1)]);
    assert_eq!(opts1[0].label(), "Discard All Changes (1)...");

    // 3 files, all selected
    let all3 = vec![
        "src/a.rs".to_string(),
        "src/b.rs".to_string(),
        "src/c.rs".to_string(),
    ];
    let opts3 = selection_bar_discard_options(&all3, 3);
    assert_eq!(opts3, vec![DiscardOption::All(3)]);
    assert_eq!(opts3[0].label(), "Discard All Changes (3)...");

    // 5 files, 2 selected
    let sub2 = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
    let opts5 = selection_bar_discard_options(&sub2, 5);
    assert_eq!(
        opts5,
        vec![
            DiscardOption::Selected(vec!["src/a.rs".to_string(), "src/b.rs".to_string()]),
            DiscardOption::All(5),
        ]
    );
    assert_eq!(opts5[0].label(), "Discard Selected Changes (2)...");
    assert_eq!(opts5[1].label(), "Discard All Changes (5)...");

    // 5 files, 0 selected
    let opts5_none = selection_bar_discard_options(&[], 5);
    assert_eq!(opts5_none, vec![DiscardOption::All(5)]);
    assert_eq!(opts5_none[0].label(), "Discard All Changes (5)...");
}

#[test]
fn discard_option_confirmation_and_action() {
    let single = DiscardOption::Single("src/foo.rs".to_string());
    assert!(!single.requires_confirmation());
    assert_eq!(single.confirmation_prompt(), None);
    assert_eq!(
        single.git_action(),
        GitAction::DiscardFile("src/foo.rs".to_string())
    );

    let selected = DiscardOption::Selected(vec!["src/a.rs".into(), "src/b.rs".into()]);
    assert!(selected.requires_confirmation());
    let prompt = selected.confirmation_prompt().unwrap();
    assert_eq!(prompt.0, "Discard selected changes?");
    assert!(prompt.1.contains("2 selected files"));
    assert_eq!(
        selected.git_action(),
        GitAction::DiscardFiles(vec!["src/a.rs".into(), "src/b.rs".into()])
    );

    let all = DiscardOption::All(4);
    assert!(all.requires_confirmation());
    let prompt_all = all.confirmation_prompt().unwrap();
    assert_eq!(prompt_all.0, "Discard all changes?");
    assert!(prompt_all.1.contains("4 files"));
    assert_eq!(all.git_action(), GitAction::DiscardAll);
}
