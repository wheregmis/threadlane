use super::{CapturedReviewRecord, ReviewPreview, ReviewPreviewEvent};
use crate::session::Snapshot;
use gpui::{
    prelude::*, AppContext, Context, Entity, IntoElement, Modifiers, Render, TestAppContext,
    VisualTestContext, Window,
};
use gpui_component::WindowExt;
use std::{cell::RefCell, rc::Rc};
use threadlane_protocol::daemon::SessionInfo;
use threadlane_protocol::repo::{GitCommitInfo, GitFile, GitStashInfo, GitStatus};

struct PanelHarness(Entity<ReviewPreview>);

impl Render for PanelHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.0.clone())
    }
}

fn fixture() -> Snapshot {
    let files = ["src/main.rs", "src/sidebar.rs", "src/chat.rs", "README.md"]
        .into_iter()
        .map(|path| GitFile {
            path: path.into(),
            status: "MM".into(),
            index_status: 'M',
            worktree_status: 'M',
            staged: true,
            unstaged: true,
            additions: 35000,
            deletions: 16000,
            ..Default::default()
        })
        .collect();
    let status = GitStatus {
        branch: Some("codex/a-long-branch-name-for-review".into()),
        remote: Some("origin".into()),
        has_changes: true,
        staged_changes: true,
        unstaged_changes: true,
        ahead: 2,
        files,
        ..Default::default()
    };
    serde_json::from_value(serde_json::json!({
        "session": SessionInfo {
            work_dir: "/sample/threadlane".into(),
            runtime_work_dir: "/sample/threadlane".into(),
            worktree_available: true,
            ..Default::default()
        },
        "messages": [],
        "git_status": status,
        "review_can_create_pr": true,
        "review_diffs": {"src/main.rs": {
            "text": "diff --git a/src/main.rs b/src/main.rs\n@@ -1 +1 @@\n-old\n+new",
            "ignore_whitespace": ""
        }}
    }))
    .unwrap()
}

#[gpui::test]
fn shared_review_sample_combined_diff_deserializes_without_filesystem(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let snapshot: Snapshot =
        serde_json::from_str(include_str!("session.sample.json")).unwrap();
    let combined = snapshot
        .review_combined_diff
        .as_ref()
        .expect("sample snapshot includes its combined diff");
    assert!(combined.text.contains("removed_offscreen"));
    assert!(combined.text.lines().count() > 150);

    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| ReviewPreview::new(Some(&snapshot), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        let panel = cx.new(|_| PanelHarness(view));
        gpui_component::Root::new(panel, window, cx)
    });
    let view = saved.borrow_mut().take().expect("preview mounted");
    view.update(cx, |host, cx| host.open_diff(String::new(), cx));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |host, _| host.document.clone()),
        Some(String::new())
    );
}

#[gpui::test]
fn shared_review_find_forwards_from_document_and_navigation_focus(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let snapshot = fixture();
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| ReviewPreview::new(Some(&snapshot), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(cx.new(|_| PanelHarness(view)), window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    cx.simulate_resize(gpui::size(gpui::px(480.0), gpui::px(900.0)));
    draw(cx);

    cx.update(|window, cx| {
        view.update(cx, |host, cx| {
            host.filter.update(cx, |input, cx| input.focus(window, cx));
            host.open_diff("src/main.rs".into(), cx);
        });
    });
    draw(cx);
    assert!(cx.debug_bounds("review-find-strip").is_none());
    #[cfg(target_os = "macos")]
    cx.simulate_keystrokes("cmd-f");
    #[cfg(not(target_os = "macos"))]
    cx.simulate_keystrokes("ctrl-f");
    draw(cx);
    assert!(cx.debug_bounds("review-find-strip").is_some());
    cx.simulate_keystrokes("escape");
    draw(cx);

    click(cx, "review-diff-next");
    click(cx, "review-diff-next");
    click(cx, "review-diff-next");
    cx.update(|window, cx| {
        assert!(view.read(cx).review_nav_focus.is_focused(window));
    });
    #[cfg(target_os = "macos")]
    cx.simulate_keystrokes("cmd-f");
    #[cfg(not(target_os = "macos"))]
    cx.simulate_keystrokes("ctrl-f");
    draw(cx);
    assert!(cx.debug_bounds("review-find-strip").is_some());
}

fn record_fixture() -> Snapshot {
    let mut snapshot = fixture();
    let status = snapshot.git_status.as_mut().unwrap();
    status.recent_commits = [
        (
            "abc1234",
            "First change with a very long summary to keep bounded in compact panels",
        ),
        ("def5678", "Second change"),
    ]
    .into_iter()
    .map(|(sha, summary)| GitCommitInfo {
        sha: sha.into(),
        short_sha: sha.into(),
        summary: summary.into(),
        author_name: "An author with a long name for compact history cards".into(),
        relative_time: "2 hours ago".into(),
        ..Default::default()
    })
    .collect();
    let file = GitFile {
        path: "src/a/very/long/recorded/path/main.rs".into(),
        additions: 35000,
        deletions: 12000,
        status: "M".into(),
        index_status: 'M',
        ..Default::default()
    };
    let record = CapturedReviewRecord {
        files: vec![file.clone()],
        diffs: std::collections::HashMap::from([(
            file.path.clone(),
            "@@ -1 +1 @@\n-before\n+after".into(),
        )]),
    };
    snapshot
        .review_commits
        .insert("abc1234".into(), record.clone());
    status.current_stash = Some(GitStashInfo {
        index: 2,
        message: "Work in progress with a long description".into(),
        relative_time: "1 hour ago".into(),
        ..Default::default()
    });
    snapshot.review_stashes.insert(2, record);
    snapshot
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    draw(cx);
}

#[gpui::test]
fn shared_review_preserves_selection_and_draft_across_diff_and_filter(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| ReviewPreview::new(Some(&fixture()), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        let panel = cx.new(|_| PanelHarness(view));
        gpui_component::Root::new(panel, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    cx.simulate_resize(gpui::size(gpui::px(480.0), gpui::px(900.0)));
    draw(cx);
    assert!(
        cx.debug_bounds("review-select-src/main.rs").is_some(),
        "selection exists before input"
    );
    cx.update(|window, cx| {
        let input = view.read(cx).commit.clone();
        input.update(cx, |input, cx| input.focus(window, cx));
    });
    cx.simulate_input("Keep my draft");
    draw(cx);
    click(cx, "review-select-src/main.rs");
    assert!(view.read_with(cx, |host, _| host.document.is_none()));
    assert!(!view.read_with(cx, |host, _| host.selected.contains("src/main.rs")));
    click(cx, "review-open-src/main.rs");
    click(cx, "review-ignore-whitespace");
    assert!(cx.debug_bounds("show-whitespace-changes").is_some());
    click(cx, "show-whitespace-changes");
    assert!(!view.read_with(cx, |host, _| host.ignore_whitespace));
    click(cx, "right-panel-document-back");
    assert_eq!(
        view.read_with(cx, |host, cx| host.commit.read(cx).value().to_string()),
        "Keep my draft"
    );
    assert_eq!(view.read_with(cx, |host, _| host.selected.len()), 3);
    cx.update(|window, cx| {
        let input = view.read(cx).filter.clone();
        input.update(cx, |input, cx| input.focus(window, cx));
    });
    cx.simulate_input("no-such-file");
    draw(cx);
    assert!(cx.debug_bounds("review-no-results").is_some());
    click(cx, "review-clear-no-results");
    assert!(cx.debug_bounds("review-open-src/main.rs").is_some());
    click(cx, "review-view-tree");
    click(cx, "review-folder-src");
    assert!(cx.debug_bounds("review-open-src/main.rs").is_none());
    assert_eq!(view.read_with(cx, |host, _| host.selected.len()), 3);
    click(cx, "review-folder-src");
    assert!(cx.debug_bounds("review-open-src/main.rs").is_some());
    click(cx, "git-stage-all-btn");
    click(cx, "git-commit-only");
    assert_eq!(view.read_with(cx, |host, _| host.files().len()), 4);
    assert_eq!(view.read_with(cx, |host, _| host.selected.len()), 3);
    assert_eq!(
        view.read_with(cx, |host, cx| host.commit.read(cx).value().to_string()),
        "Keep my draft"
    );
}

#[gpui::test]
fn shared_review_controls_fit_narrow_panels_at_zoom(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            let mut host = ReviewPreview::new(Some(&fixture()), window, cx);
            host.selected.remove("README.md");
            host.commit.update(cx, |input, cx| {
                input.set_value("A commit draft", window, cx)
            });
            host
        });
        let panel = cx.new(|_| PanelHarness(view));
        gpui_component::Root::new(panel, window, cx)
    });
    for font in [13.0, 16.0, 20.0] {
        cx.update(|window, cx| {
            gpui_component::Theme::global_mut(cx).font_size = gpui::px(font);
            gpui_component::Theme::sync_base(cx);
            window.refresh();
        });
        for width in [280.0, 320.0, 480.0, 640.0] {
            cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(1000.0)));
            draw(cx);
            for selector in [
                "git-branch-selector-btn",
                "git-sync-action-btn",
                "review-view-list",
                "review-view-tree",
                "select-all-files",
                "view-combined-diff-btn",
                "git-stage-all-btn",
                "git-unstage-all-btn",
                "review-open-src/main.rs",
                "review-file-status",
                "review-file-stats",
                "clear-commit-input",
                "git-generate-commit-msg",
                "git-commit-and-push",
                "git-commit-only",
                "git-push-only",
            ] {
                let bounds = cx
                    .debug_bounds(selector)
                    .unwrap_or_else(|| panic!("missing {selector}"));
                assert!(
                    bounds.left() >= gpui::px(0.0) && bounds.right() <= gpui::px(width),
                    "{selector} overflows at font {font}, width {width}: {bounds:?}"
                );
                assert!(
                    bounds.top() >= gpui::px(0.0) && bounds.bottom() <= gpui::px(1000.0),
                    "{selector} clipped at font {font}, width {width}: {bounds:?}"
                );
            }
        }
    }
}

#[gpui::test]
fn shared_review_records_retain_filters_and_request_existing_editor(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| ReviewPreview::new(Some(&record_fixture()), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        let panel = cx.new(|_| PanelHarness(view));
        gpui_component::Root::new(panel, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    let opens = Rc::new(RefCell::new(Vec::new()));
    let capture = opens.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &ReviewPreviewEvent, _| {
            let ReviewPreviewEvent::OpenDiff { title, content } = event;
            capture.borrow_mut().push((title.clone(), content.clone()));
        })
    });
    cx.simulate_resize(gpui::size(gpui::px(420.0), gpui::px(1000.0)));
    draw(cx);
    click(cx, "review-tab-history");
    click(cx, "commit-header-abc1234");
    click(
        cx,
        "commit-file-abc1234-src/a/very/long/recorded/path/main.rs",
    );
    assert_eq!(
        opens.borrow()[0].0,
        "src/a/very/long/recorded/path/main.rs @ abc1234"
    );
    assert!(opens.borrow()[0].1.contains("+after"));
    assert!(
        view.read_with(cx, |host, _| host.document.is_none()),
        "recorded diffs belong in the editor"
    );
    cx.update(|window, cx| {
        let input = view.read(cx).history_filter.clone();
        input.update(cx, |input, cx| input.focus(window, cx));
    });
    cx.simulate_input("unmatched");
    draw(cx);
    assert!(cx.debug_bounds("review-history-empty").is_some());
    click(cx, "history-clear-filter");
    assert!(
        cx.debug_bounds("commit-file-abc1234-src/a/very/long/recorded/path/main.rs")
            .is_some(),
        "filter changes retain commit expansion"
    );
    click(cx, "review-tab-changes");
    click(cx, "stash-header-toggle");
    click(cx, "stash-file-2-src/a/very/long/recorded/path/main.rs");
    assert_eq!(opens.borrow()[1].0, "src/a/very/long/recorded/path/main.rs");
    click(cx, "restore-stash-btn");
    click(cx, "discard-stash-btn");
    assert!(
        view.read_with(cx, |host, _| host
            .status
            .as_ref()
            .unwrap()
            .current_stash
            .is_some()),
        "preview actions retain the saved stash"
    );
    assert_eq!(view.read_with(cx, |host, _| host.selected.len()), 4);
    click(cx, "review-tab-history");
    assert!(cx
        .debug_bounds("commit-file-abc1234-src/a/very/long/recorded/path/main.rs")
        .is_some());
    click(cx, "commit-header-def5678");
    assert!(
        cx.debug_bounds("review-commit-files").is_none(),
        "missing captured details must not be presented as an empty commit"
    );
    assert_eq!(opens.borrow().len(), 2);
}

#[gpui::test]
fn shared_review_records_fit_narrow_panels_at_zoom(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            let mut snapshot = record_fixture();
            let stash = snapshot.review_stashes.get_mut(&2).unwrap();
            let file = stash.files[0].clone();
            stash.files = (0..20)
                .map(|index| GitFile {
                    path: format!("src/recorded/file-{index}.rs"),
                    ..file.clone()
                })
                .collect();
            ReviewPreview::new(Some(&snapshot), window, cx)
        });
        *capture.borrow_mut() = Some(view.clone());
        let panel = cx.new(|_| PanelHarness(view));
        gpui_component::Root::new(panel, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    for font in [13.0, 16.0, 20.0] {
        cx.update(|window, cx| {
            gpui_component::Theme::global_mut(cx).font_size = gpui::px(font);
            gpui_component::Theme::sync_base(cx);
            window.refresh();
        });
        for width in [280.0, 320.0, 480.0] {
            cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(700.0)));
            view.update(cx, |host, cx| {
                host.tab = threadlane_ui_kit::ReviewTab::History;
                host.selected_commit = Some("abc1234".into());
                cx.notify();
            });
            draw(cx);
            for selector in [
                "commit-header-abc1234",
                "review-commit-summary",
                "review-commit-sha",
                "review-recorded-file-path",
                "review-recorded-file-stats",
            ] {
                let bounds = cx.debug_bounds(selector).unwrap();
                assert!(
                    bounds.left() >= gpui::px(0.0) && bounds.right() <= gpui::px(width),
                    "{selector} overflows at font {font}, width {width}: {bounds:?}"
                );
            }
            view.update(cx, |host, cx| {
                host.tab = threadlane_ui_kit::ReviewTab::Changes;
                host.stash_expanded = true;
                cx.notify();
            });
            draw(cx);
            for selector in [
                "stash-header-toggle",
                "review-stash-file-path",
                "review-stash-file-stats",
                "restore-stash-btn",
                "discard-stash-btn",
            ] {
                let bounds = cx.debug_bounds(selector).unwrap();
                assert!(
                    bounds.left() >= gpui::px(0.0) && bounds.right() <= gpui::px(width),
                    "{selector} overflows at font {font}, width {width}: {bounds:?}"
                );
            }
            let footer = cx.debug_bounds("review-commit-footer").unwrap();
            assert!(
                footer.bottom() <= gpui::px(700.0),
                "expanded stash clips the commit footer at font {font}: {footer:?}"
            );
        }
    }
}

fn right_click(cx: &mut VisualTestContext, selector: &'static str) {
    let point = cx.debug_bounds(selector).unwrap().center();
    cx.simulate_mouse_move(point, None, Modifiers::default());
    cx.simulate_mouse_down(point, gpui::MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(point, gpui::MouseButton::Right, Modifiers::default());
    draw(cx);
}

#[gpui::test]
fn shared_review_file_menus_keep_selection_and_open_captured_editor_diff(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| ReviewPreview::new(Some(&fixture()), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        let panel = cx.new(|_| PanelHarness(view));
        gpui_component::Root::new(panel, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    let opens = Rc::new(RefCell::new(Vec::new()));
    let capture = opens.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &ReviewPreviewEvent, _| {
            let ReviewPreviewEvent::OpenDiff { title, content } = event;
            capture.borrow_mut().push((title.clone(), content.clone()));
        })
    });
    cx.simulate_resize(gpui::size(gpui::px(420.0), gpui::px(1000.0)));
    draw(cx);
    view.update(cx, |host, cx| {
        host.selected = ["src/main.rs".into(), "README.md".into()]
            .into_iter()
            .collect();
        cx.notify();
    });
    draw(cx);
    right_click(cx, "review-open-src/main.rs");
    cx.simulate_keystrokes("down down down enter"); // Selection-scoped discard.
    draw(cx);
    assert_eq!(view.read_with(cx, |host, _| host.selected.len()), 2);
    assert_eq!(view.read_with(cx, |host, _| host.files().len()), 4);
    assert!(opens.borrow().is_empty());
    view.update(cx, |host, cx| {
        host.selected = host.files().iter().map(|file| file.path.clone()).collect();
        cx.notify();
    });
    draw(cx);
    right_click(cx, "review-open-src/main.rs");
    cx.simulate_keystrokes("down down down down down down enter");
    draw(cx);
    assert_eq!(opens.borrow().len(), 1);
    assert_eq!(opens.borrow()[0].0, "src/main.rs");
    assert!(opens.borrow()[0].1.contains("+new"));
    assert!(view.read_with(cx, |host, _| host.document.is_none()));
    right_click(cx, "review-open-src/main.rs");
    cx.simulate_keystrokes("down down down down down down down enter");
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        "src/main.rs"
    );
    right_click(cx, "review-open-src/main.rs");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(view.read_with(cx, |host, _| host.selected.len()), 4);
}

struct PrCardHarness {
    pr: threadlane_protocol::repo::GitHubPrInfo,
    expanded: bool,
    busy: bool,
    can_address: bool,
    actions: Vec<threadlane_ui_kit::ReviewPrAction>,
}

impl Render for PrCardHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui::div()
            .size_full()
            .child(threadlane_ui_kit::review_pr_card(
                &self.pr,
                &threadlane_ui_kit::ReviewPrState {
                    expanded: self.expanded,
                    feedback_count: Some(2),
                    can_address: self.can_address,
                    busy: self.busy,
                },
                cx.listener(|host, action: &threadlane_ui_kit::ReviewPrAction, _, cx| {
                    host.actions.push(*action);
                    if *action == threadlane_ui_kit::ReviewPrAction::Toggle {
                        host.expanded = !host.expanded;
                    }
                    cx.notify();
                }),
                cx,
            ))
    }
}

#[gpui::test]
fn shared_review_pr_card_separates_link_disclosure_and_busy_actions(cx: &mut TestAppContext) {
    use threadlane_ui_kit::ReviewPrAction;
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|_| PrCardHarness {
            pr: super::sample_review_pr(0),
            expanded: false,
            busy: false,
            can_address: true,
            actions: Vec::new(),
        });
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    cx.simulate_resize(gpui::size(gpui::px(320.0), gpui::px(700.0)));
    draw(cx);
    click(cx, "pr-link-btn");
    assert_eq!(
        view.read_with(cx, |host, _| host.actions.clone()),
        vec![ReviewPrAction::Open]
    );
    assert!(!view.read_with(cx, |host, _| host.expanded));
    click(cx, "pr-card-toggle");
    click(cx, "fix-ci-btn");
    click(cx, "address-comments-btn");
    assert_eq!(
        view.read_with(cx, |host, _| host.actions.clone()),
        vec![
            ReviewPrAction::Open,
            ReviewPrAction::Toggle,
            ReviewPrAction::FixCi,
            ReviewPrAction::AddressComments
        ]
    );
    view.update(cx, |host, cx| {
        host.busy = true;
        cx.notify();
    });
    draw(cx);
    click(cx, "fix-ci-btn");
    click(cx, "address-comments-btn");
    assert_eq!(view.read_with(cx, |host, _| host.actions.len()), 4);
    view.update(cx, |host, cx| {
        host.busy = false;
        host.can_address = false;
        cx.notify();
    });
    draw(cx);
    click(cx, "address-comments-btn");
    assert_eq!(view.read_with(cx, |host, _| host.actions.len()), 4);
    for font in [13.0, 16.0, 20.0] {
        cx.update(|window, cx| {
            gpui_component::Theme::global_mut(cx).font_size = gpui::px(font);
            gpui_component::Theme::sync_base(cx);
            window.refresh();
        });
        for width in [280.0, 320.0, 480.0] {
            cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(700.0)));
            for state in 0..4 {
                view.update(cx, |host, cx| {
                    host.pr = super::sample_review_pr(state);
                    cx.notify();
                });
                draw(cx);
                for selector in [
                    "review-pr-title",
                    "pr-link-btn",
                    "review-pr-status",
                    "address-comments-btn",
                ] {
                    let bounds = cx.debug_bounds(selector).unwrap();
                    assert!(
                        bounds.left() >= gpui::px(0.0) && bounds.right() <= gpui::px(width),
                        "{selector} overflows at {font}, {width}: {bounds:?}"
                    );
                }
                assert_eq!(cx.debug_bounds("fix-ci-btn").is_some(), state == 0);
                assert_eq!(
                    cx.debug_bounds("review-pr-check-count").is_some(),
                    state == 1 || state == 2
                );
            }
        }
    }
}

#[gpui::test]
fn shared_review_menus_transfer_focus_during_construction(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let panel = cx.new(|cx| ReviewPreview::new(Some(&fixture()), window, cx));
        gpui_component::Root::new(panel, window, cx)
    });
    cx.update(|window, cx| {
        let previous = cx.focus_handle();
        for file_menu in [false, true] {
            previous.focus(window, cx);
            assert!(previous.is_focused(window));
            let file = fixture().git_status.unwrap().files.remove(0);
            let menu = gpui_component::menu::PopupMenu::build(window, cx, |menu, window, cx| {
                let menu = if file_menu {
                    threadlane_ui_kit::review_file_menu(
                        menu,
                        &threadlane_ui_kit::ReviewFileMenu {
                            file: &file,
                            selected_paths: &[],
                            total_files: 4,
                            absolute_path: None,
                            reveal_label: "Reveal in Finder",
                            busy: false,
                        },
                        |_, _, _| {},
                        window,
                        cx,
                    )
                } else {
                    threadlane_ui_kit::review_discard_menu(
                        menu,
                        vec![threadlane_ui_kit::ReviewDiscardTarget::All(4)],
                        false,
                        |_, _, _| {},
                        window,
                        cx,
                    )
                };
                // Must already have focus before any menu prepaint. Late focus
                // leaves the former input and the menu focused in one a11y frame.
                assert!(gpui::Focusable::focus_handle(&menu, cx).is_focused(window));
                assert!(!previous.is_focused(window));
                menu
            });
            assert!(gpui::Focusable::focus_handle(menu.read(cx), cx).is_focused(window));
        }
    });
}

#[gpui::test(iterations = 10)]
fn shared_review_branch_forms_preserve_recorded_checkout_and_drafts(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    cx.update(|cx| cx.set_reduce_motion(true));
    let mut snapshot = fixture();
    let status = snapshot.git_status.as_mut().unwrap();
    status.branch_details = [
        (status.branch.clone().unwrap(), true, false, false),
        ("main".into(), false, true, false),
        ("feature".into(), false, false, false),
        ("origin/feature".into(), false, false, true),
    ]
    .into_iter()
    .map(
        |(name, is_current, is_default, is_remote)| threadlane_protocol::repo::GitBranchInfo {
            name,
            is_current,
            is_default,
            is_remote,
            ..Default::default()
        },
    )
    .collect();
    let recorded = serde_json::to_value(&snapshot.git_status).unwrap();
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| ReviewPreview::new(Some(&snapshot), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(cx.new(|_| PanelHarness(view)), window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    cx.simulate_resize(gpui::size(gpui::px(420.0), gpui::px(900.0)));
    cx.update(|window, cx| {
        view.update(cx, |host, cx| {
            host.commit
                .update(cx, |input, cx| input.set_value("Keep my draft", window, cx));
            host.branch_open = true;
            cx.notify();
        })
    });
    draw(cx);
    click(cx, "review-branch-new");
    cx.update(|window, cx| assert!(window.has_active_dialog(cx)));
    cx.simulate_input("codex/local-preview");
    draw(cx);
    click(cx, "submit-new-branch-btn");
    cx.update(|window, cx| assert!(!window.has_active_dialog(cx)));
    cx.update(|window, cx| {
        view.update(cx, |host, cx| {
            host.open_git_dialog(threadlane_ui_kit::ReviewGitDialog::Merge, window, cx)
        })
    });
    draw(cx);
    click(cx, "merge-select-feature");
    assert_eq!(
        view.read_with(cx, |host, _| host.merge_selected.clone())
            .as_deref(),
        Some("feature")
    );
    click(cx, "submit-merge-btn");
    cx.update(|window, cx| assert!(!window.has_active_dialog(cx)));
    view.update(cx, |host, cx| {
        host.branch_open = true;
        cx.notify();
    });
    draw(cx);
    right_click(cx, "branch-row-feature");
    cx.simulate_keystrokes("down enter");
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        "feature"
    );
    right_click(cx, "branch-row-feature");
    cx.simulate_keystrokes("down down enter");
    draw(cx);
    cx.update(|window, cx| assert!(window.has_active_dialog(cx)));
    cx.simulate_keystrokes("escape");
    draw(cx);
    click(cx, "branch-row-feature");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |host, _| host.switch_target.clone())
            .as_deref(),
        Some("feature")
    );
    click(cx, "switch-opt-carry");
    assert!(!view.read_with(cx, |host, _| host.switch_stash));
    click(cx, "submit-switch-dialog-btn");
    cx.update(|window, cx| assert!(!window.has_active_dialog(cx)));
    cx.update(|window, cx| {
        view.update(cx, |host, cx| {
            host.open_git_dialog(threadlane_ui_kit::ReviewGitDialog::Stash, window, cx)
        })
    });
    draw(cx);
    cx.simulate_input("Keep this stash draft");
    click(cx, "stash-include-untracked-chk");
    assert!(!view.read_with(cx, |host, _| host.include_untracked));
    click(cx, "confirm-stash-btn");
    cx.update(|window, cx| assert!(!window.has_active_dialog(cx)));
    assert_eq!(
        view.read_with(cx, |host, cx| host.commit.read(cx).value().to_string()),
        "Keep my draft"
    );
    assert_eq!(
        view.read_with(cx, |host, cx| host
            .stash_message
            .read(cx)
            .value()
            .to_string()),
        "Keep this stash draft"
    );
    assert_eq!(
        view.read_with(cx, |host, _| serde_json::to_value(&host.status).unwrap()),
        recorded
    );
}

struct GitFormHarness {
    input: Entity<gpui_component::input::InputState>,
    kind: threadlane_ui_kit::ReviewGitDialog,
    busy: bool,
    status: GitStatus,
    requests: Rc<RefCell<Vec<threadlane_ui_kit::ReviewGitFormAction>>>,
}

impl Render for GitFormHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let branch = self.status.branch.as_deref().unwrap();
        let state = threadlane_ui_kit::ReviewGitFormState {
            current_branch: branch,
            target_branch: Some(branch),
            selected_merge: Some(branch),
            busy: self.busy,
            stash_changes: true,
            include_untracked: true,
        };
        let requests = self.requests.clone();
        let action =
            move |action: &threadlane_ui_kit::ReviewGitFormAction,
                  _: &mut Window,
                  _: &mut gpui::App| requests.borrow_mut().push(action.clone());
        let form = match self.kind {
            threadlane_ui_kit::ReviewGitDialog::NewBranch => {
                threadlane_ui_kit::review_new_branch_form(&self.input, &state, action, cx)
            }
            threadlane_ui_kit::ReviewGitDialog::Merge => threadlane_ui_kit::review_merge_form(
                &self.input,
                Some(&self.status),
                &state,
                action,
                cx,
            ),
            threadlane_ui_kit::ReviewGitDialog::Switch => {
                threadlane_ui_kit::review_switch_form(&state, action, cx)
            }
            threadlane_ui_kit::ReviewGitDialog::Stash => {
                threadlane_ui_kit::review_stash_form(&self.input, &state, action, cx)
            }
        };
        gpui::div().size_full().p_3().child(form)
    }
}

#[gpui::test]
fn shared_review_git_forms_fit_long_names_and_block_busy_submits(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let requests = Rc::new(RefCell::new(Vec::new()));
    let capture_requests = requests.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let input = cx.new(|cx| gpui_component::input::InputState::new(window, cx));
        let branch =
            "codex/a-very-long-branch-name-with-many-segments-for-component-layout".repeat(2);
        let status = GitStatus {
            branch: Some(branch),
            branches: vec![
                "feature/another-long-target-branch-name-for-the-merge-picker".repeat(2),
            ],
            ..Default::default()
        };
        let view = cx.new(|_| GitFormHarness {
            input,
            kind: threadlane_ui_kit::ReviewGitDialog::NewBranch,
            busy: false,
            status,
            requests: capture_requests,
        });
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    for font in [13.0, 20.0] {
        cx.update(|window, _| window.set_rem_size(gpui::px(font)));
        for width in [280.0, 420.0] {
            cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(900.0)));
            for (kind, form, submit) in [
                (
                    threadlane_ui_kit::ReviewGitDialog::NewBranch,
                    "new-branch-dialog",
                    "submit-new-branch-btn",
                ),
                (
                    threadlane_ui_kit::ReviewGitDialog::Merge,
                    "merge-branch-dialog",
                    "submit-merge-btn",
                ),
                (
                    threadlane_ui_kit::ReviewGitDialog::Switch,
                    "switch-branch-dialog",
                    "submit-switch-dialog-btn",
                ),
                (
                    threadlane_ui_kit::ReviewGitDialog::Stash,
                    "stash-dialog",
                    "confirm-stash-btn",
                ),
            ] {
                cx.update(|window, cx| {
                    view.update(cx, |host, cx| {
                        host.kind = kind;
                        host.busy = true;
                        let name = if kind == threadlane_ui_kit::ReviewGitDialog::Merge {
                            ""
                        } else {
                            host.status.branch.as_deref().unwrap()
                        };
                        host.input
                            .update(cx, |input, cx| input.set_value(name, window, cx));
                        cx.notify();
                    })
                });
                draw(cx);
                let form_bounds = cx.debug_bounds(form).unwrap();
                let submit_bounds = cx.debug_bounds(submit).unwrap();
                assert!(
                    form_bounds.right() <= gpui::px(width),
                    "{kind:?}: {form_bounds:?}"
                );
                assert!(submit_bounds.left() >= form_bounds.left() && submit_bounds.right() <= form_bounds.right(),
                    "{kind:?} button outside form at font {font}, width {width}: {submit_bounds:?}, {form_bounds:?}");
                if kind == threadlane_ui_kit::ReviewGitDialog::NewBranch {
                    let base = cx.debug_bounds("review-new-base").unwrap();
                    assert!(
                        base.right() <= form_bounds.right(),
                        "base branch exceeds form: {base:?}"
                    );
                }
                if kind == threadlane_ui_kit::ReviewGitDialog::Switch {
                    for selector in [
                        "switch-opt-carry",
                        "switch-opt-stash-label",
                        "switch-opt-carry-label",
                        "switch-stash-description",
                        "switch-carry-description",
                    ] {
                        let choice = cx.debug_bounds(selector).unwrap();
                        assert!(choice.right() <= form_bounds.right(),
                            "{selector} exceeds form at font {font}, width {width}: {choice:?}, {form_bounds:?}");
                    }
                }
                if kind == threadlane_ui_kit::ReviewGitDialog::Switch {
                    click(cx, "switch-opt-carry");
                }
                click(cx, submit);
                assert!(
                    requests.borrow().is_empty(),
                    "busy {kind:?} emits an action"
                );
            }
        }
    }
}

#[gpui::test]
fn shared_review_draft_pr_button_opens_retained_local_form(cx: &mut TestAppContext) {
    cx.update(gpui_component::init); cx.update(threadlane_ui_theme::init_bundled);
    let mut snapshot = fixture(); snapshot.review_can_create_pr = true;
    let recorded = serde_json::to_value(&snapshot.git_status).unwrap();
    let saved = Rc::new(RefCell::new(None)); let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| ReviewPreview::new(Some(&snapshot), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(cx.new(|_| PanelHarness(view)), window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    cx.simulate_resize(gpui::size(gpui::px(420.0), gpui::px(900.0))); draw(cx);
    click(cx, "git-create-pull-request");
    assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("changed-base"); draw(cx);
    cx.simulate_keystrokes("escape"); draw(cx);
    click(cx, "git-create-pull-request");
    let draft = view.read_with(cx, |view, _| view.draft_pr.clone());
    assert_eq!(draft.read_with(cx, |draft, cx| draft.fields(cx).base), "changed-base");
    assert_eq!(view.read_with(cx, |view, _| serde_json::to_value(&view.status).unwrap()), recorded);
}

#[gpui::test]
fn shared_review_diff_navigates_adjacent_files_in_filtered_order(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let mut snapshot = fixture();
    // Capture every file but README.md: it exercises the unavailable-snapshot
    // state while navigation keeps working around it.
    for path in ["src/sidebar.rs", "src/chat.rs"] {
        snapshot.review_diffs.insert(
            path.into(),
            super::CapturedReviewDiff {
                text: format!("diff --git a/{path} b/{path}\n@@ -1 +1 @@\n-old\n+new"),
                ignore_whitespace: String::new(),
            },
        );
    }
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| ReviewPreview::new(Some(&snapshot), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(cx.new(|_| PanelHarness(view)), window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    cx.simulate_resize(gpui::size(gpui::px(480.0), gpui::px(900.0)));
    draw(cx);

    // Three consecutive files without returning to the list.
    click(cx, "review-open-src/main.rs");
    assert!(cx.debug_bounds("review-diff-nav").is_some());
    assert!(cx.debug_bounds("review-diff-nav-position").is_some());
    assert_eq!(
        view.read_with(cx, |host, _| host.document.clone()),
        Some("src/main.rs".into())
    );
    // First file: Previous is a bounded no-op.
    click(cx, "review-diff-prev");
    assert_eq!(
        view.read_with(cx, |host, _| host.document.clone()),
        Some("src/main.rs".into())
    );
    click(cx, "review-diff-next");
    assert_eq!(
        view.read_with(cx, |host, _| host.document.clone()),
        Some("src/sidebar.rs".into())
    );
    click(cx, "review-diff-next");
    assert_eq!(
        view.read_with(cx, |host, _| host.document.clone()),
        Some("src/chat.rs".into())
    );
    // README.md has no captured patch; the row stays usable around the
    // explicit unavailable-snapshot body.
    click(cx, "review-diff-next");
    assert_eq!(
        view.read_with(cx, |host, _| host.document.clone()),
        Some("README.md".into())
    );
    assert!(cx.debug_bounds("review-diff-nav").is_some());
    // Last file: Next is a bounded no-op.
    click(cx, "review-diff-next");
    assert_eq!(
        view.read_with(cx, |host, _| host.document.clone()),
        Some("README.md".into())
    );
    click(cx, "review-diff-prev");
    assert_eq!(
        view.read_with(cx, |host, _| host.document.clone()),
        Some("src/chat.rs".into())
    );

    // Filtering the current file out shows the removed-file state and
    // disables navigation rather than retargeting a row index. The filter
    // input is not mounted in document view, so set it directly.
    cx.update(|window, cx| {
        view.update(cx, |host, cx| {
            host.filter
                .update(cx, |input, cx| input.set_value("sidebar", window, cx));
            cx.notify();
        });
    });
    draw(cx);
    assert!(cx.debug_bounds("review-diff-nav").is_some());
    click(cx, "review-diff-prev");
    click(cx, "review-diff-next");
    assert_eq!(
        view.read_with(cx, |host, _| host.document.clone()),
        Some("src/chat.rs".into())
    );
    cx.update(|window, cx| {
        view.update(cx, |host, cx| {
            host.filter
                .update(cx, |input, cx| input.set_value("", window, cx));
            cx.notify();
        });
    });
    draw(cx);

    // Filtered order: README.md is out, so src/chat.rs is the boundary.
    cx.update(|window, cx| {
        view.update(cx, |host, cx| {
            host.filter
                .update(cx, |input, cx| input.set_value("src/", window, cx));
            cx.notify();
        });
    });
    draw(cx);
    click(cx, "review-diff-prev");
    click(cx, "review-diff-prev");
    assert_eq!(
        view.read_with(cx, |host, _| host.document.clone()),
        Some("src/main.rs".into())
    );
    click(cx, "review-diff-next");
    click(cx, "review-diff-next");
    click(cx, "review-diff-next");
    assert_eq!(
        view.read_with(cx, |host, _| host.document.clone()),
        Some("src/chat.rs".into())
    );

    // A refresh that removes the current file disables navigation.
    view.update(cx, |host, cx| {
        host.status
            .as_mut()
            .unwrap()
            .files
            .retain(|file| file.path != "src/chat.rs");
        cx.notify();
    });
    draw(cx);
    click(cx, "review-diff-prev");
    assert_eq!(
        view.read_with(cx, |host, _| host.document.clone()),
        Some("src/chat.rs".into())
    );
    // Back still returns to the list.
    click(cx, "right-panel-document-back");
    assert!(view.read_with(cx, |host, _| host.document.is_none()));
}
