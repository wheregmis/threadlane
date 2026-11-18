use super::{AgentsPreview, AgentsPreviewEvent};
use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext};
use std::{cell::RefCell, rc::Rc};

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
fn shared_agents_preserve_tool_expansion_by_lane_and_request_followups(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| AgentsPreview::new(Vec::new(), Vec::new(), None, window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    let prompts = Rc::new(RefCell::new(Vec::new()));
    let capture = prompts.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &AgentsPreviewEvent, _| {
            if let AgentsPreviewEvent::Prompt(prompt) = event {
                capture.borrow_mut().push(prompt.clone());
            }
        })
    });
    cx.simulate_resize(gpui::size(gpui::px(420.0), gpui::px(700.0)));
    draw(cx);
    let key = "preview:queued-1-0:sample-explorer-message:sample-read";
    assert!(
        cx.debug_bounds("tool-preview-card").is_none(),
        "closed output stays lazy"
    );
    click(cx, "tool-activity-disclosure");
    assert!(view.read_with(cx, |host, _| host.expanded.contains(key)));
    assert!(
        cx.debug_bounds("tool-preview-card").is_some(),
        "same read-file card as chat"
    );
    click(cx, "preview-agent-message");
    assert_eq!(
        prompts.borrow().last().unwrap(),
        "Send this message to subagent sample-0: "
    );
    click(cx, "preview-agent-queued-1-1");
    assert!(
        cx.debug_bounds("tool-preview-card").is_none(),
        "another lane does not inherit expansion"
    );
    click(cx, "preview-agent-message");
    assert_eq!(
        prompts.borrow().last().unwrap(),
        "Continue subagent sample-1 with this follow-up: "
    );
    click(cx, "preview-agent-queued-1-0");
    assert!(view.read_with(cx, |host, _| host.expanded.contains(key)));
    assert!(
        cx.debug_bounds("tool-preview-card").is_some(),
        "returning to the lane retains its output"
    );
    click(cx, "right-panel-tab-Trajectory");
    assert!(cx.debug_bounds("tool-preview-card").is_none());
    click(cx, "right-panel-tab-Agents");
    assert!(
        cx.debug_bounds("tool-preview-card").is_some(),
        "surface navigation retains the selected lane and disclosure"
    );
    assert_eq!(
        prompts.borrow().len(),
        2,
        "panel navigation must not dispatch follow-ups"
    );
    click(cx, "preview-agent-main");
    assert!(
        cx.debug_bounds("agent-activity-message").is_none(),
        "empty main has an empty state"
    );
    assert!(view.read_with(cx, |host, _| host.selected == "main"));
}

#[gpui::test]
fn shared_agent_headers_and_activity_fit_narrow_panels_at_zoom(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| AgentsPreview::new(Vec::new(), Vec::new(), None, window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    for font in [13.0, 16.0, 20.0] {
        cx.update(|window, cx| {
            gpui_component::Theme::global_mut(cx).font_size = gpui::px(font);
            gpui_component::Theme::sync_base(cx);
            window.refresh();
        });
        for width in [280.0, 640.0] {
            cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(700.0)));
            cx.update(|_, cx| {
                view.update(cx, |host, cx| {
                    host.list.scroll_to(gpui::ListOffset::default());
                    cx.notify();
                })
            });
            draw(cx);
            let tabs = cx.debug_bounds("agent-profile-tabs").unwrap();
            let header = cx.debug_bounds("agent-detail-header").unwrap();
            let message = cx.debug_bounds("agent-activity-message").unwrap();
            let title = cx.debug_bounds("right-panel-title").unwrap();
            assert!(title.right() <= gpui::px(width) && title.bottom() <= tabs.top());
            for selector in [
                "right-panel-tab-Trajectory",
                "right-panel-tab-Agents",
                "right-panel-tab-Review",
                "right-panel-tab-Files",
                "right-panel-tab-Browser",
            ] {
                let bounds = cx.debug_bounds(selector).unwrap();
                assert!(bounds.left() >= gpui::px(0.0) && bounds.right() <= gpui::px(width));
                assert!(
                    bounds.bottom() <= tabs.top(),
                    "panel navigation overlaps agent profiles"
                );
            }
            for bounds in [header, message] {
                assert!(
                    bounds.left() >= gpui::px(0.0) && bounds.right() <= gpui::px(width),
                    "horizontal overflow at font {font}: {bounds:?}"
                );
                assert!(
                    bounds.top() >= tabs.bottom(),
                    "activity overlaps profiles at font {font}: {bounds:?}"
                );
            }
            cx.update(|_, cx| {
                view.update(cx, |host, cx| {
                    host.list.scroll_to_end();
                    cx.notify();
                })
            });
            draw(cx);
            let message = cx.debug_bounds("agent-activity-message").unwrap();
            let viewport = cx.debug_bounds("preview-agent-activity-viewport").unwrap();
            assert!(
                message.bottom() <= viewport.bottom(),
                "activity end must remain reachable at font {font}/{width}"
            );
        }
    }
}

#[gpui::test]
fn shared_file_tree_expands_and_requests_the_existing_sample_editor(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| AgentsPreview::new(Vec::new(), Vec::new(), None, window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    let opens = Rc::new(RefCell::new(0));
    let capture = opens.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &AgentsPreviewEvent, _| {
            if matches!(event, AgentsPreviewEvent::OpenSampleFile) {
                *capture.borrow_mut() += 1;
            }
        })
    });
    cx.simulate_resize(gpui::size(gpui::px(320.0), gpui::px(700.0)));
    draw(cx);
    click(cx, "right-panel-tab-Files");
    assert!(cx.debug_bounds("project-file-example.rs").is_some());
    click(cx, "project-file-.");
    assert!(cx.debug_bounds("project-file-example.rs").is_none());
    assert_eq!(*opens.borrow(), 0, "expanding folders does not open a file");
    click(cx, "project-file-.");
    click(cx, "project-file-example.rs");
    assert_eq!(*opens.borrow(), 1);
    click(cx, "right-panel-tab-Browser");
    click(cx, "right-panel-tab-Files");
    assert!(cx.debug_bounds("project-file-example.rs").is_some());
    assert_eq!(
        *opens.borrow(),
        1,
        "surface navigation does not reopen files"
    );
}

#[gpui::test]
fn agent_worktree_preview_uses_captured_diff_and_preserves_targets_on_discard(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| AgentsPreview::new(Vec::new(), Vec::new(), None, window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    let capture = events.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &AgentsPreviewEvent, _| match event {
            AgentsPreviewEvent::OpenDiff { content, .. } => {
                capture.borrow_mut().push(content.clone())
            }
            AgentsPreviewEvent::OpenTerminal => capture.borrow_mut().push("terminal".into()),
            _ => (),
        })
    });
    cx.simulate_resize(gpui::size(gpui::px(420.0), gpui::px(700.0)));
    draw(cx);
    click(cx, "agent-inspect-queued-1-0");
    assert!(events.borrow()[0].contains("shared agent worktree controls (preview sample)"));
    click(cx, "agent-terminal-queued-1-0");
    assert_eq!(events.borrow()[1], "terminal");
    click(cx, "preview-agent-queued-1-1");
    click(cx, "agent-terminal-queued-1-1");
    assert_eq!(
        events.borrow().len(),
        2,
        "uncaptured worktree cannot open a terminal"
    );
    click(cx, "preview-agent-queued-1-2");
    let original = view.read_with(cx, |host, _| host.agents[2].isolation.clone());
    click(cx, "agent-discard-queued-1-2");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |host, _| host.agents[2].isolation.clone()),
        original
    );
    click(cx, "agent-discard-queued-1-2");
    cx.update(|window, cx| {
        window.dispatch_action(
            Box::new(gpui_component::dialog::Confirm { secondary: false }),
            cx,
        )
    });
    draw(cx);
    assert_eq!(
        view.read_with(cx, |host, _| host.agents[2].isolation.clone()),
        original,
        "preview confirmation must leave captured metadata intact"
    );
    assert_eq!(
        events.borrow().len(),
        2,
        "discard never opens a file or dispatches a run"
    );
}
