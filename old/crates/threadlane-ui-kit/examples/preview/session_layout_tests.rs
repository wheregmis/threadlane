use super::{SessionPreview, Snapshot};
use gpui::{px, size, AppContext, Focusable, Modifiers, TestAppContext, VisualTestContext};
use std::{cell::RefCell, rc::Rc, sync::Arc};

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
fn shared_workspace_responsiveness_preserves_draft_and_agent_selection(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let snapshot: Snapshot =
        serde_json::from_value(serde_json::json!({"session":null,"messages":[]})).unwrap();
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| SessionPreview::with_snapshot(Arc::new(snapshot), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    for font in [13.0, 20.0] {
        cx.update(|window, cx| {
            gpui_component::Theme::global_mut(cx).font_size = px(font);
            gpui_component::Theme::sync_base(cx);
            view.update(cx, |host, cx| {
                host.agents_open = false;
                host.sidebar_collapsed = false;
                host.input
                    .update(cx, |input, cx| input.set_value("Keep my draft", window, cx));
                cx.notify();
            });
        });
        let rem = cx.update(|window, _| window.rem_size());
        cx.simulate_resize(size(rem * 100.0, rem * 60.0));
        draw(cx);
        assert!(cx.debug_bounds("workspace-sidebar").is_some());
        assert!(cx.debug_bounds("chat-environment").is_some());
        click(cx, "sidebar-collapse-toggle");
        assert!(cx.debug_bounds("workspace-sidebar").is_none());
        click(cx, "sidebar-collapse-toggle");
        assert!(cx.debug_bounds("workspace-sidebar").is_some());
        click(cx, "preview-agents-toggle");
        assert!(
            cx.debug_bounds("chat-environment").is_none(),
            "right panel must replace Environment"
        );
        click(cx, "preview-agent-queued-1-1");
        assert!(
            cx.debug_bounds("agent-branch-queued-1-1").is_some(),
            "selected profile must mount its own controls"
        );
        cx.simulate_resize(size(rem * 55.0, rem * 60.0));
        draw(cx);
        assert!(
            cx.debug_bounds("workspace-sidebar").is_none(),
            "secondary sidebar must yield room to chat and inspector"
        );
        assert!(cx.debug_bounds("workspace-right-panel-focus").is_none());
        assert!(cx.debug_bounds("composer-model-picker").is_some());
        let controls = cx.debug_bounds("agent-worktree-controls").unwrap();
        assert!(controls.left() >= px(0.0) && controls.right() <= rem * 55.0);
        cx.simulate_resize(size(rem * 30.0, rem * 60.0));
        draw(cx);
        assert!(cx.debug_bounds("workspace-right-panel-focus").is_some());
        assert!(
            cx.debug_bounds("composer-model-picker").is_none(),
            "narrow panel focus mounts one primary surface"
        );
        assert!(
            cx.debug_bounds("agent-branch-queued-1-1").is_some(),
            "selection must survive the split replacement"
        );
        for selector in [
            "preview-agents-toggle",
            "preview-terminal-toggle",
            "preview-command-palette",
            "preview-component-mode",
        ] {
            let bounds = cx.debug_bounds(selector).unwrap();
            assert!(
                bounds.left() >= px(0.0) && bounds.right() <= rem * 30.0,
                "preview footer control must remain reachable at font {font}: {selector}"
            );
        }
        let back = cx.debug_bounds("review-back-to-chat").unwrap();
        assert!(back.left() >= px(0.0) && back.right() <= rem * 30.0);
        // Back shares the panel's title row (no second header band) and clears the
        // window controls, since the panel spans the window in compact focus.
        let title = cx.debug_bounds("right-panel-title").unwrap();
        assert!(back.right() <= title.left(), "Back leads the panel title");
        assert!(
            back.top() < title.bottom() && title.top() < back.bottom(),
            "Back and the title share one row"
        );
        assert!(
            back.left() >= rem * threadlane_ui_theme::WINDOW_CONTROLS_CONTENT_INSET,
            "Back clears the window controls"
        );
        let collapsed = view.read_with(cx, |host, _| host.sidebar_collapsed);
        click(cx, "sidebar-collapse-toggle");
        assert_eq!(
            view.read_with(cx, |host, _| host.sidebar_collapsed),
            collapsed,
            "unavailable sidebar button must not change retained preferences"
        );
        click(cx, "review-back-to-chat");
        cx.update(|window, cx| {
            assert!(
                view.read(cx)
                    .input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window),
                "returning to conversation must restore composer focus"
            );
        });
        assert!(cx.debug_bounds("workspace-right-panel-focus").is_none());
        assert!(cx.debug_bounds("composer-model-picker").is_some());
        assert_eq!(
            view.read_with(cx, |host, cx| host.input.read(cx).value().to_string()),
            "Keep my draft"
        );
        cx.simulate_resize(size(rem * 100.0, rem * 60.0));
        draw(cx);
        assert!(cx.debug_bounds("workspace-sidebar").is_some());
        assert!(cx.debug_bounds("chat-environment").is_some());
        click(cx, "preview-agents-toggle");
        assert!(cx.debug_bounds("agent-branch-queued-1-1").is_some());
        assert!(cx.debug_bounds("chat-environment").is_none());
    }
}

#[gpui::test]
fn shared_conversation_find_keyboard_palette_and_narrow_layout(cx: &mut TestAppContext) {
    use threadlane_protocol::daemon::{ChatMessageInfo, MessageRole};
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    cx.update(threadlane_ui_kit::init_conversation_find);
    cx.update(super::init_palette);
    let messages = (0..120)
        .map(|index| ChatMessageInfo {
            id: format!("m{index}"),
            role: MessageRole::User,
            content: if index == 4 || index == 110 {
                "A needle in this conversation".into()
            } else {
                format!("Unrelated message {index}")
            },
            tool_activities: Vec::new(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
            retry_prompt: None,
        })
        .collect::<Vec<_>>();
    let mut snapshot: Snapshot =
        serde_json::from_value(serde_json::json!({"session":null,"messages":[]})).unwrap();
    snapshot.messages = messages;
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| SessionPreview::with_snapshot(Arc::new(snapshot), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    draw(cx);
    cx.simulate_input("Keep my draft");
    let shortcut = if cfg!(target_os = "macos") {
        "cmd-f"
    } else {
        "ctrl-f"
    };
    cx.simulate_keystrokes(shortcut);
    draw(cx);
    assert!(cx.debug_bounds("conversation-find-strip").is_some());
    cx.simulate_input("needle");
    draw(cx);
    view.read_with(cx, |host, _| {
        assert_eq!(host.find_results.len(), 2);
        assert_eq!(host.find_selected.as_deref(), Some("m4"));
    });
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |host, _| host.find_selected.clone())
            .as_deref(),
        Some("m110")
    );
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |host, _| host.find_selected.clone())
            .as_deref(),
        Some("m4")
    );
    cx.simulate_keystrokes("shift-enter");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |host, _| host.find_selected.clone())
            .as_deref(),
        Some("m110")
    );
    for font in [13.0, 20.0] {
        cx.update(|_, cx| {
            gpui_component::Theme::global_mut(cx).font_size = px(font);
            gpui_component::Theme::sync_base(cx);
        });
        let rem = cx.update(|window, _| window.rem_size());
        cx.simulate_resize(size(rem * 30.0, rem * 55.0));
        draw(cx);
        for selector in [
            "conversation-find-previous",
            "conversation-find-next",
            "conversation-find-close",
        ] {
            let bounds = cx.debug_bounds(selector).unwrap();
            assert!(
                bounds.left() >= px(0.) && bounds.right() <= rem * 30.,
                "{selector} exceeds viewport"
            );
        }
    }
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("no matching text");
    draw(cx);
    assert!(view.read_with(cx, |host, _| host.find_results.is_empty()));
    click(cx, "conversation-find-next");
    assert!(view.read_with(cx, |host, _| host.find_selected.is_none()));
    cx.simulate_keystrokes("escape");
    draw(cx);
    cx.update(|window, cx| {
        view.read_with(cx, |host, cx| {
            assert!(!host.find_open);
            assert_eq!(host.input.read(cx).value(), "Keep my draft");
            assert!(host.input.read(cx).focus_handle(cx).is_focused(window));
        })
    });
    // Enter in the composer must never navigate matches.
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(view.read_with(cx, |host, _| host.find_selected.is_none()));
    // A palette handoff from Editor opens Chat and restores focus within Chat.
    click(cx, "central-tab-editor");
    // The command palette opens the same strip and selects the confirmed message.
    cx.simulate_keystrokes("cmd-k");
    assert!(
        view.read_with(cx, |host, _| host.palette_open),
        "palette must open"
    );
    cx.simulate_input("conversations");
    draw(cx);
    view.read_with(cx, |host, cx| {
        assert_eq!(host.command_state.read(cx).query(cx), "conversations")
    });
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(view.read_with(cx, |host, _| host.palette_search));
    cx.simulate_input("needle");
    draw(cx);
    assert_eq!(view.read_with(cx, |host, _| host.palette_matches.len()), 2);
    cx.simulate_keystrokes("down enter");
    draw(cx);
    view.read_with(cx, |host, _| {
        assert!(!host.palette_open);
        assert!(host.find_open);
        assert_eq!(host.find_query, "needle");
        assert_eq!(host.find_selected.as_deref(), Some("m110"));
    });
    cx.simulate_keystrokes("escape");
    draw(cx);
    cx.update(|window, cx| {
        view.read_with(cx, |host, cx| {
            assert!(host.input.read(cx).focus_handle(cx).is_focused(window));
        })
    });
    click(cx, "conversation-find-open");
    click(cx, "central-tab-editor");
    assert!(view.read_with(cx, |host, _| !host.find_open
        && host.find_results.is_empty()));
    click(cx, "central-tab-chat");
    assert!(cx.debug_bounds("conversation-find-strip").is_none());
}
