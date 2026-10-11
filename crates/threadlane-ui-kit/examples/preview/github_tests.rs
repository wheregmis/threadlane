use gpui::{AppContext, Focusable, Modifiers, TestAppContext, VisualTestContext};
use std::{cell::RefCell, rc::Rc};

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        // Retained frames copy prior debug bounds; refresh before checking absence.
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

#[gpui::test]
fn shared_issue_start_preserves_choices_busy_guards_and_error_recovery(cx: &mut TestAppContext) {
    use gpui_component::WindowExt;
    use threadlane_protocol::{OrchestratorMode, ReasoningEffort};
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| super::GitHubPreview::new("Sample workspace".into(), window, cx));
        gpui_component::Root::new(view, window, cx)
    });
    cx.simulate_resize(gpui::size(gpui::px(800.), gpui::px(850.)));
    let completed = Rc::new(RefCell::new(Vec::new()));
    let capture = completed.clone();
    let state = cx.update(|window, cx| {
        super::issue_dialog::open_start(
            "threadlane/sample · #42".into(),
            "Keep chat activities aligned with the shared content gutter".into(),
            42,
            "issue-start-error",
            move |form, _| {
                capture
                    .borrow_mut()
                    .push((form.model.clone(), form.effort, form.mode))
            },
            window,
            cx,
        )
    });
    draw(cx);
    click(cx, "issue-task-model");
    cx.simulate_keystrokes("down down enter");
    draw(cx);
    assert_eq!(
        state.read_with(cx, |state, _| state.form.model.clone()),
        "sample/fast"
    );
    click(cx, "issue-task-effort");
    cx.simulate_keystrokes("down down down enter");
    draw(cx);
    click(cx, "issue-task-mode");
    cx.simulate_keystrokes("down down enter");
    draw(cx);
    click(cx, "confirm-issue-task");
    assert!(state.read_with(cx, |state, _| state.form.starting));
    cx.update(|window, cx| {
        state.update(cx, |state, cx| {
            state.select(
                threadlane_ui_kit::github::GitHubIssueStartAction::Mode(OrchestratorMode::Normal),
                window,
                cx,
            )
        })
    });
    cx.simulate_keystrokes("escape enter");
    assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    draw(cx);
    assert!(cx.debug_bounds("github-issue-start-error").is_some());
    state.read_with(cx, |state, _| {
        assert_eq!(state.form.model, "sample/fast");
        assert_eq!(state.form.effort, ReasoningEffort::High);
        assert_eq!(state.form.mode, OrchestratorMode::Fusion);
        assert!(!state.form.starting);
    });
    assert!(completed.borrow().is_empty());
    let mut branch_height = None;
    for (width, height) in [(360., 500.), (800., 850.)] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(height)));
        draw(cx);
        let branch = cx.debug_bounds("github-issue-start-branch").unwrap();
        let confirm = cx.debug_bounds("confirm-issue-task").unwrap();
        assert!(branch.left() >= gpui::px(0.) && branch.right() <= gpui::px(width));
        if let Some(height) = branch_height {
            assert_eq!(branch.size.height, height, "branch remains a single line");
        }
        branch_height = Some(branch.size.height);
        assert!(confirm.right() <= gpui::px(width));
        assert!(
            confirm.bottom() <= gpui::px(height),
            "confirmation remains reachable in short windows"
        );
    }
    click(cx, "confirm-issue-task");
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    draw(cx);
    assert_eq!(
        &*completed.borrow(),
        &[(
            "sample/fast".into(),
            ReasoningEffort::High,
            OrchestratorMode::Fusion
        )]
    );
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    // Missing repository/provider states never submit even through Enter.
    for scenario in ["issue-start-no-repo", "issue-start-no-provider"] {
        let disabled = cx.update(|window, cx| {
            super::issue_dialog::open_start(
                "threadlane/sample · #42".into(),
                "A disabled task".into(),
                42,
                scenario,
                |_, _| panic!("unavailable task submitted"),
                window,
                cx,
            )
        });
        draw(cx);
        click(cx, "confirm-issue-task");
        cx.simulate_keystrokes("enter");
        assert!(!disabled.read_with(cx, |state, _| state.form.starting));
        click(cx, "cancel-issue-task");
    }
    let external = cx.update(|window, cx| {
        super::issue_dialog::open_start(
            "threadlane/sample · #42".into(),
            "External agent task".into(),
            42,
            "loaded",
            |_, _| {},
            window,
            cx,
        )
    });
    draw(cx);
    click(cx, "issue-task-model");
    cx.simulate_keystrokes("up enter");
    draw(cx);
    assert_eq!(
        external.read_with(cx, |state, _| state.form.model.clone()),
        "acp/sample-agent"
    );
    assert!(cx.debug_bounds("issue-task-effort").is_none());
}

#[gpui::test]
fn shared_issue_delete_requires_explicit_action_and_keeps_project_scope(cx: &mut TestAppContext) {
    use gpui_component::WindowExt;
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let captured = Rc::new(RefCell::new(None));
    let capture = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| super::GitHubPreview::new("Sample workspace".into(), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = captured.borrow_mut().take().unwrap();
    draw(cx);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.request_detail(
                threadlane_ui_kit::github::GitHubDetailAction::DeleteIssue,
                window,
                cx,
            )
        })
    });
    draw(cx);
    cx.simulate_keystrokes("enter");
    assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
    assert!(view.read_with(cx, |view, _| view.notice.is_none()));
    click(cx, "cancel-delete-issue");
    assert!(view.read_with(cx, |view, _| view.notice.is_none()));
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.request_detail(
                threadlane_ui_kit::github::GitHubDetailAction::DeleteIssue,
                window,
                cx,
            )
        })
    });
    draw(cx);
    view.update(cx, |view, cx| {
        view.select_item("sample-issue-sample-other-42".into(), cx)
    });
    click(cx, "confirm-delete-issue");
    assert_eq!(
        view.read_with(cx, |view, _| view.notice.clone()).as_deref(),
        Some("Issue selection changed. Open the delete confirmation again.")
    );
    let original_count = view.read_with(cx, |view, _| view.items.len());
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.request_detail(
                threadlane_ui_kit::github::GitHubDetailAction::DeleteIssue,
                window,
                cx,
            )
        })
    });
    draw(cx);
    click(cx, "confirm-delete-issue");
    assert_eq!(
        view.read_with(cx, |view, _| view.notice.clone()).as_deref(),
        Some("Delete confirmed locally. No GitHub issue was deleted.")
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.items.len()),
        original_count
    );
}

#[gpui::test]
fn shared_issue_creation_preserves_draft_through_failure_and_retry(cx: &mut TestAppContext) {
    use gpui_component::WindowExt;
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let completed = Rc::new(RefCell::new(Vec::new()));
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| super::GitHubPreview::new("Sample workspace".into(), window, cx));
        gpui_component::Root::new(view, window, cx)
    });
    cx.simulate_resize(gpui::size(gpui::px(800.), gpui::px(700.)));
    let capture = completed.clone();
    let dialog = cx.update(|window, cx| {
        super::issue_dialog::open(
            "threadlane/sample".into(),
            true,
            move |title, _| capture.borrow_mut().push(title),
            window,
            cx,
        )
    });
    draw(cx);
    cx.update(|window, cx| {
        assert!(dialog
            .read(cx)
            .title
            .read(cx)
            .focus_handle(cx)
            .is_focused(window))
    });
    click(cx, "confirm-create-issue");
    assert!(cx.debug_bounds("github-issue-create-error").is_some());
    cx.update(|window, cx| {
        assert!(dialog
            .read(cx)
            .title
            .read(cx)
            .focus_handle(cx)
            .is_focused(window))
    });
    assert!(completed.borrow().is_empty());
    cx.update(|window, cx| {
        dialog.update(cx, |state, cx| {
            state.title.update(cx, |input, cx| {
                input.set_value("Keep this draft", window, cx)
            });
            state.body.update(cx, |input, cx| {
                input.set_value("Description with **Markdown**", window, cx)
            });
        })
    });
    click(cx, "confirm-create-issue");
    assert!(dialog.read_with(cx, |state, _| state.creating));
    cx.simulate_keystrokes("escape enter");
    draw(cx);
    assert!(
        cx.update(|window, cx| window.has_active_dialog(cx)),
        "pending creation remains visible"
    );
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    draw(cx);
    dialog.read_with(cx, |state, cx| {
        assert!(!state.creating);
        assert!(state
            .error
            .as_deref()
            .unwrap()
            .contains("draft is preserved"));
        assert_eq!(state.title.read(cx).value(), "Keep this draft");
        assert_eq!(state.body.read(cx).value(), "Description with **Markdown**");
    });
    assert!(cx.debug_bounds("github-issue-create-error").is_some());
    assert!(completed.borrow().is_empty());
    for width in [360., 800.] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(700.)));
        draw(cx);
        let form = cx.debug_bounds("github-issue-create-form").unwrap();
        let confirm = cx.debug_bounds("confirm-create-issue").unwrap();
        assert!(form.left() >= gpui::px(0.) && form.right() <= gpui::px(width));
        assert!(confirm.left() >= gpui::px(0.) && confirm.right() <= gpui::px(width));
    }
    click(cx, "confirm-create-issue");
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    draw(cx);
    assert_eq!(&*completed.borrow(), &["Keep this draft"]);
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    // A retained entity from a cancelled form must never submit a later draft.
    let cancelled = cx.update(|window, cx| {
        super::issue_dialog::open(
            "threadlane/sample".into(),
            false,
            |_, _| panic!("cancelled dialog submitted"),
            window,
            cx,
        )
    });
    draw(cx);
    click(cx, "cancel-create-issue");
    cx.update(|window, cx| cancelled.update(cx, |state, cx| state.create(window, cx)));
    assert!(!cancelled.read_with(cx, |state, _| state.creating));
}

#[gpui::test]
fn shared_file_review_keeps_scope_keyboard_guards_and_layout(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    cx.update(super::init);
    let captured = Rc::new(RefCell::new(None));
    let capture = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| super::GitHubPreview::new("Sample workspace".into(), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = captured.borrow_mut().take().unwrap();
    cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(800.)));
    view.update(cx, |view, cx| view.select_kind(true, cx));
    draw(cx);
    click(cx, "sample-pr-sample-local-42");
    cx.simulate_keystrokes("enter right right right");
    draw(cx);
    click(cx, "github-pr-file-src/conversation.rs");
    cx.update(|window, cx| assert!(view.read(cx).files_focus.is_focused(window)));
    cx.simulate_keystrokes("down enter");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_file().map(str::to_owned))
            .as_deref(),
        Some(
            "crates/threadlane-ui-kit/src/components/long-review-path/shared_conversation_details.rs"
        )
    );
    let retained_diff = view.read_with(cx, |view, _| view.file_diff.entity_id());
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(
        retained_diff,
        view.read_with(cx, |view, _| view.file_diff.entity_id())
    );
    click(cx, "github-pr-viewed");
    let local_viewed = view.read_with(cx, |view, _| {
        view.viewed_files["sample-pr-sample-local-42"].clone()
    });
    assert_eq!(local_viewed.len(), 1);
    click(cx, "github-pr-next-unviewed");
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_file().map(str::to_owned))
            .as_deref(),
        Some("assets/preview.png")
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.viewed_files["sample-pr-sample-local-42"]
            .clone()),
        local_viewed,
        "navigation never marks a file"
    );
    for scenario in [
        "diff-loading",
        "diff-error",
        "viewed-loading",
        "viewed-error",
        "viewed-pending",
        "viewed-unknown",
    ] {
        view.update(cx, |view, cx| {
            view.scenario = scenario.into();
            cx.notify();
        });
        draw(cx);
        click(cx, "github-pr-viewed");
        assert_eq!(
            view.read_with(cx, |view, _| view.viewed_files["sample-pr-sample-local-42"]
                .clone()),
            local_viewed,
            "{scenario} disables viewed writes"
        );
        if scenario == "diff-error" {
            assert!(
                cx.debug_bounds("github-pr-diff-retry").unwrap().size.width
                    < cx.debug_bounds("github-pr-diff").unwrap().size.width * 0.5
            );
            click(cx, "github-pr-diff-retry");
            assert_eq!(
                view.read_with(cx, |view, _| view.scenario.clone()),
                "loaded"
            );
            assert_eq!(
                view.read_with(cx, |view, _| view.selected_file().map(str::to_owned))
                    .as_deref(),
                Some("assets/preview.png")
            );
        }
    }
    view.update(cx, |view, cx| {
        view.scenario = "loaded".into();
        cx.notify();
    });
    draw(cx);
    click(cx, "github-back-to-pr-list");
    click(cx, "sample-pr-sample-other-42");
    cx.simulate_keystrokes("enter right right right");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_file().map(str::to_owned))
            .as_deref(),
        Some("src/conversation.rs")
    );
    click(cx, "github-pr-viewed");
    assert!(view.read_with(cx, |view, _| {
        view.viewed_files["sample-pr-sample-other-42"].contains("src/conversation.rs")
    }));
    click(cx, "github-back-to-pr-list");
    click(cx, "sample-pr-sample-local-42");
    cx.simulate_keystrokes("enter right right right");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_file().map(str::to_owned))
            .as_deref(),
        Some("assets/preview.png")
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.viewed_files["sample-pr-sample-local-42"]
            .clone()),
        local_viewed
    );
    for (width, font) in [(480., 14.), (800., 16.), (1600., 20.)] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(900.)));
        cx.update(|_, cx| gpui_component::Theme::global_mut(cx).font_size = gpui::px(font));
        draw(cx);
        let title = cx.debug_bounds("github-detail-title").unwrap();
        let toolbar = cx.debug_bounds("github-pr-files-progress").unwrap();
        assert!((title.left() - toolbar.left()).abs() <= gpui::px(1.));
        let files = cx.debug_bounds("github-pr-file-list").unwrap();
        let diff = cx.debug_bounds("github-pr-diff").unwrap();
        assert!((files.right() - diff.left()).abs() <= gpui::px(1.));
        assert!(diff.size.width >= gpui::px(width * 0.5));
        assert!(diff.right() <= gpui::px(width) + gpui::px(1.));
        assert!(cx.debug_bounds("github-pr-next-unviewed").unwrap().right() <= gpui::px(width));
    }
    cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(800.)));
    view.update(cx, |view, cx| {
        view.files.scroll_to_end();
        cx.notify();
    });
    draw(cx);
    click(cx, "github-pr-file-src/components/review_23.rs");
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_file().map(str::to_owned))
            .as_deref(),
        Some("src/components/review_23.rs")
    );
    view.update(cx, |view, cx| {
        view.select_item("sample-pr-sample-local-44".into(), cx);
        view.select_tab(super::PrDetailTab::Code, cx);
    });
    draw(cx);
    assert!(cx.debug_bounds("github-pr-file-list").is_none());
    assert!(cx.debug_bounds("github-pr-viewed").is_none());
}

#[gpui::test]
fn shared_discussion_retains_scoped_drafts_and_commits_align(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    cx.update(super::init);
    let captured = Rc::new(RefCell::new(None));
    let capture = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| super::GitHubPreview::new("Sample workspace".into(), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = captured.borrow_mut().take().unwrap();
    cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(3000.)));
    view.update(cx, |view, cx| view.select_kind(true, cx));
    draw(cx);
    click(cx, "sample-pr-sample-local-42");
    cx.simulate_keystrokes("enter right");
    draw(cx);
    cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(800.)));
    draw(cx);
    click(
        cx,
        "github-pr-reply-sample-pr-sample-local-42-sample-inline-0",
    );
    cx.update(|window, cx| {
        assert!(view
            .read(cx)
            .reply_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window));
    });
    let viewport = cx.debug_bounds("github-pr-conversation").unwrap();
    let reply = cx.debug_bounds("github-pr-reply-editor").unwrap();
    assert!(reply.top() >= viewport.top() && reply.bottom() <= viewport.bottom());
    cx.simulate_input("Visible reply draft");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.reply_input.read(cx).value().to_string()),
        "Visible reply draft"
    );
    click(cx, "github-pr-reply-clear");
    cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(3000.)));
    draw(cx);
    click(cx, "github-pr-comment-publish");
    assert!(
        view.read_with(cx, |view, _| view.notice.is_none()),
        "empty draft cannot publish"
    );
    view.update_in(cx, |view, window, cx| {
        view.comment_input
            .update(cx, |input, cx| input.focus(window, cx))
    });
    cx.simulate_input("Local project comment");
    draw(cx);
    click(
        cx,
        "github-pr-reply-sample-pr-sample-local-42-sample-inline-0",
    );
    cx.simulate_input("Keep this reply");
    draw(cx);
    click(
        cx,
        "github-pr-reply-sample-pr-sample-local-42-sample-inline-1",
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.drafts["sample-pr-sample-local-42"]
            .reply
            .clone()),
        Some(("sample-inline-0".into(), "Keep this reply".into()))
    );
    click(cx, "sample-pr-sample-other-42");
    cx.simulate_keystrokes("enter right");
    draw(cx);
    view.update_in(cx, |view, window, cx| {
        view.comment_input
            .update(cx, |input, cx| input.focus(window, cx))
    });
    cx.simulate_input("Other project comment");
    draw(cx);
    click(cx, "sample-pr-sample-local-42");
    assert_eq!(
        view.read_with(cx, |view, cx| view
            .comment_input
            .read(cx)
            .value()
            .to_string()),
        "Local project comment"
    );
    assert_eq!(
        view.read_with(cx, |view, cx| view.reply_input.read(cx).value().to_string()),
        "Keep this reply"
    );
    view.update(cx, |view, cx| {
        view.notice = None;
        view.scenario = "draft-publishing".into();
        cx.notify();
    });
    draw(cx);
    click(cx, "github-pr-comment-publish");
    click(cx, "github-pr-reply-clear");
    assert!(view.read_with(cx, |view, _| view.notice.is_none()));
    assert!(view.read_with(cx, |view, _| {
        view.drafts["sample-pr-sample-local-42"].reply.is_some()
    }));
    view.update(cx, |view, cx| {
        view.scenario = "draft-error".into();
        cx.notify();
    });
    draw(cx);
    click(cx, "github-pr-comment-publish");
    assert!(view.read_with(cx, |view, _| {
        view.notice
            .as_deref()
            .unwrap()
            .contains("No comment or reply was sent")
    }));
    assert_eq!(
        view.read_with(cx, |view, cx| view
            .comment_input
            .read(cx)
            .value()
            .to_string()),
        "Local project comment"
    );
    view.update(cx, |view, cx| {
        view.scenario = "draft-unknown".into();
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("github-pr-comment-check-again").is_some());
    assert!(cx.debug_bounds("github-pr-comment-publish").is_none());
    click(cx, "github-pr-comment-check-again");
    view.update(cx, |view, cx| {
        view.scenario = "loaded".into();
        cx.notify();
    });
    draw(cx);
    for (width, font) in [(480., 14.), (800., 16.), (1600., 20.)] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(3000.)));
        cx.update(|_, cx| gpui_component::Theme::global_mut(cx).font_size = gpui::px(font));
        draw(cx);
        let title = cx.debug_bounds("github-detail-title").unwrap();
        for selector in [
            "github-pr-conversation-sample-pr-sample-local-42-sample-inline-0",
            "github-pr-comment-editor",
            "github-pr-reply-editor",
        ] {
            let bounds = cx.debug_bounds(selector).unwrap();
            assert!(
                (bounds.left() - title.left()).abs() <= gpui::px(1.),
                "{selector} shares the content inset at {width}/{font}"
            );
            assert!(bounds.right() <= title.right() + gpui::px(1.));
        }
        view.update(cx, |view, cx| {
            view.select_tab(super::PrDetailTab::Timeline, cx)
        });
        draw(cx);
        let title = cx.debug_bounds("github-detail-title").unwrap();
        let count = cx.debug_bounds("github-pr-commit-count").unwrap();
        assert!((title.left() - count.left()).abs() <= gpui::px(1.));
        let row = cx
            .debug_bounds(concat!(
                "github-pr-commit-",
                "abcdef0",
                "000000000000000000000000000000000"
            ))
            .unwrap();
        assert!(row.left() <= title.left() && row.right() >= title.right());
        view.update(cx, |view, cx| {
            view.select_tab(super::PrDetailTab::Conversation, cx)
        });
    }
    cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(800.)));
    view.update(cx, |view, cx| {
        view.select_tab(super::PrDetailTab::Timeline, cx);
    });
    draw(cx);
    view.update(cx, |view, cx| {
        // The final row is unmeasured; end anchoring measures backwards from it.
        view.commits.scroll_to_end();
        cx.notify();
    });
    draw(cx);
    assert!(
        cx.debug_bounds(concat!(
            "github-pr-commit-",
            "abcdf01",
            "000000000000000000000000000000000"
        ))
        .is_some(),
        "virtual commit list reveals the final row"
    );
    view.update(cx, |view, cx| {
        view.select_item("sample-pr-sample-local-44".into(), cx);
        view.select_tab(super::PrDetailTab::Timeline, cx);
    });
    draw(cx);
    assert!(cx.debug_bounds("github-pr-commit-count").is_some());
    view.update(cx, |view, cx| {
        view.select_tab(super::PrDetailTab::Conversation, cx)
    });
    draw(cx);
    assert!(
        cx.debug_bounds("github-pr-comment-editor").is_some(),
        "empty discussion still offers a draft editor"
    );
}
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    draw(cx);
}

#[gpui::test]
fn collection_preserves_scoped_identity_keyboard_search_and_recovery(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    cx.update(super::init);
    let captured = Rc::new(RefCell::new(None));
    let capture = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| super::GitHubPreview::new("Sample workspace".into(), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = captured.borrow_mut().take().unwrap();
    cx.simulate_resize(gpui::size(gpui::px(1100.0), gpui::px(2600.0)));
    draw(cx);
    click(cx, "sample-issue-sample-local-42");
    cx.simulate_keystrokes("down");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.selected.clone())
            .as_deref(),
        Some("sample-issue-sample-other-42")
    );
    cx.simulate_keystrokes("up");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.selected.clone())
            .as_deref(),
        Some("sample-issue-sample-local-42")
    );
    click(cx, "sample-issue-sample-other-42");
    click(cx, "github-load-more");
    assert_eq!(view.read_with(cx, |view, _| view.visible.len()), 20);
    assert_eq!(
        view.read_with(cx, |view, _| view.selected.clone())
            .as_deref(),
        Some("sample-issue-sample-other-42")
    );
    assert!(cx.debug_bounds("github-load-more").is_none());

    click(cx, "github-search-field");
    cx.simulate_input("no matching sample");
    draw(cx);
    assert!(view.read_with(cx, |view, _| view.visible.is_empty()));
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("aligned");
    draw(cx);
    assert_eq!(view.read_with(cx, |view, _| view.visible.len()), 2);
    click(cx, "github-new-issue");
    assert!(
        view.read_with(cx, |view, _| view.notice.is_none()),
        "all-project scope disables issue creation"
    );

    view.update(cx, |view, cx| {
        view.select_kind(true, cx);
    });
    draw(cx);
    click(cx, "github-state-merged");
    assert_eq!(
        view.read_with(cx, |view, _| view.state),
        super::State::Merged
    );
    view.update(cx, |view, cx| {
        view.select_kind(false, cx);
    });
    draw(cx);
    assert_eq!(view.read_with(cx, |view, _| view.state), super::State::Open);
    assert!(cx.debug_bounds("github-state-merged").is_none());

    view.update(cx, |view, cx| {
        view.scenario = "partial".into();
        cx.notify();
    });
    draw(cx);
    let warning = cx.debug_bounds("github-list-warning").unwrap();
    let results = cx.debug_bounds("github-result-list").unwrap();
    assert!(warning.bottom() <= results.top());
    click(cx, "github-list-warning-copy");
    cx.update(|_, cx| {
        assert!(cx
            .read_from_clipboard()
            .unwrap()
            .text()
            .unwrap()
            .contains("Sample GitHub network error"))
    });
    click(cx, "github-list-warning-retry");
    assert_eq!(
        view.read_with(cx, |view, _| view.scenario.clone()),
        "loaded"
    );
    view.update(cx, |view, cx| {
        view.scenario = "error".into();
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("github-list-error-copy").is_some());
    assert!(cx.debug_bounds("github-result-list").is_none());
    click(cx, "github-list-error-retry");

    for (width, font) in [(480.0, 14.0), (800.0, 16.0), (1100.0, 20.0)] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(2000.0)));
        cx.update(|_, cx| gpui_component::Theme::global_mut(cx).font_size = gpui::px(font));
        draw(cx);
        let heading = cx.debug_bounds("github-page-heading").unwrap();
        // Shared header row: the heading sits inside the window-controls band.
        assert!(heading.bottom() <= threadlane_ui_theme::WINDOW_CONTROLS_CLEARANCE, "{heading:?}");
        let search = cx.debug_bounds("github-search-field").unwrap();
        let results = cx.debug_bounds("github-result-list").unwrap();
        assert!(search.left() > results.left() && search.right() < results.right());
        let row = cx.debug_bounds("sample-issue-sample-local-42").unwrap();
        assert!(row.left() >= results.left() && row.right() <= results.right());
        for selector in [
            "github-state-open",
            "github-state-closed",
            "github-refresh",
            "github-new-issue",
        ] {
            let control = cx.debug_bounds(selector).unwrap();
            assert!(control.left() >= results.left() && control.right() <= results.right());
        }
    }
}

#[gpui::test(iterations = 10)]
fn shared_details_keep_action_state_scoped_tabs_and_content_alignment(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    cx.update(super::init);
    cx.update(|cx| cx.set_reduce_motion(true));
    let captured = Rc::new(RefCell::new(None));
    let capture = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| super::GitHubPreview::new("Sample workspace".into(), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = captured.borrow_mut().take().unwrap();
    cx.simulate_resize(gpui::size(gpui::px(1100.), gpui::px(2600.)));
    draw(cx);
    click(cx, "github-start-agent-task");
    assert!(cx.debug_bounds("github-issue-start-form").is_some());
    assert!(
        view.read_with(cx, |view, _| view.notice.is_none()),
        "opening a confirmation does not start a task"
    );
    click(cx, "cancel-issue-task");
    view.update(cx, |view, cx| {
        view.scenario = "issue-pending".into();
        view.notice = None;
        cx.notify();
    });
    draw(cx);
    for control in [
        "github-issue-close-reopen",
        "github-issue-suggest-labels",
        "github-issue-delete",
    ] {
        click(cx, control);
        assert!(view.read_with(cx, |view, _| view.notice.is_none()));
        assert_eq!(
            view.read_with(cx, |view, _| view.selected.clone())
                .as_deref(),
            Some("sample-issue-sample-local-42")
        );
    }
    view.update(cx, |view, cx| {
        view.scenario = "loaded".into();
        cx.notify();
    });
    draw(cx);
    click(cx, "github-issue-close-reopen");
    assert_eq!(
        view.read_with(cx, |view, _| view
            .items
            .iter()
            .find(|item| item.id == "sample-issue-sample-local-42")
            .unwrap()
            .state),
        super::State::Closed
    );
    click(cx, "github-state-closed");
    click(cx, "sample-issue-sample-local-42");
    click(cx, "github-issue-close-reopen");
    assert_eq!(
        view.read_with(cx, |view, _| view
            .items
            .iter()
            .find(|item| item.id == "sample-issue-sample-local-42")
            .unwrap()
            .state),
        super::State::Open
    );
    click(cx, "github-state-open");
    click(cx, "sample-issue-sample-local-42");
    view.update(cx, |view, cx| {
        view.scenario = "detail-error".into();
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("github-detail-title").is_none());
    assert!(cx.debug_bounds("github-result-list").is_some());
    click(cx, "github-detail-error-retry");
    assert_eq!(
        view.read_with(cx, |view, _| view.selected.clone())
            .as_deref(),
        Some("sample-issue-sample-local-42")
    );
    assert!(cx.debug_bounds("github-detail-title").is_some());

    view.update(cx, |view, cx| view.select_kind(true, cx));
    draw(cx);
    click(cx, "sample-pr-sample-local-42");
    cx.simulate_keystrokes("enter");
    draw(cx);
    cx.update(|window, cx| assert!(view.read(cx).tabs_focus.is_focused(window)));
    cx.simulate_keystrokes("right right right");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.current_tab()),
        super::PrDetailTab::Code
    );
    assert!(cx.debug_bounds("github-result-list").is_none());
    click(cx, "github-back-to-pr-list");
    assert_eq!(
        view.read_with(cx, |view, _| view.current_tab()),
        super::PrDetailTab::Summary
    );
    cx.simulate_keystrokes("enter right");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.current_tab()),
        super::PrDetailTab::Conversation
    );
    click(cx, "sample-pr-sample-other-42");
    cx.simulate_keystrokes("enter right right");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.current_tab()),
        super::PrDetailTab::Timeline
    );
    click(cx, "sample-pr-sample-local-42");
    assert_eq!(
        view.read_with(cx, |view, _| view.current_tab()),
        super::PrDetailTab::Conversation
    );
    view.update(cx, |view, cx| {
        view.select_tab(super::PrDetailTab::Summary, cx)
    });
    draw(cx);
    assert!(
        cx.debug_bounds("github-pr-check-log-Documentation")
            .is_none(),
        "local file URLs are not exposed as check-log links"
    );
    click(cx, "github-pr-check-log-Build");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/threadlane/sample/actions/runs/42")
    );

    for (width, font) in [(480., 14.), (800., 16.), (1600., 20.)] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(3000.)));
        cx.update(|_, cx| gpui_component::Theme::global_mut(cx).font_size = gpui::px(font));
        for pull_requests in [false, true] {
            view.update(cx, |view, cx| view.select_kind(pull_requests, cx));
            draw(cx);
            let title = cx.debug_bounds("github-detail-title").unwrap();
            let body = cx.debug_bounds("github-detail-description").unwrap();
            assert!(
                (title.left() - body.left()).abs() <= gpui::px(1.),
                "title and description share the content spine at {width}px/{font}px"
            );
            assert!(title.left() >= gpui::px(0.) && title.right() <= gpui::px(width));
            if !pull_requests {
                for control in [
                    "github-start-agent-task",
                    "github-issue-close-reopen",
                    "github-issue-suggest-labels",
                    "github-issue-delete",
                ] {
                    let bounds = cx.debug_bounds(control).unwrap();
                    assert!(
                        bounds.left() >= title.left() && bounds.right() <= title.right(),
                        "{control} wraps inside the detail content"
                    );
                }
            }
        }
    }
}
