use super::{SampleSearchMode, TerminalPreview, TerminalPreviewEvent};
use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext};
use std::{cell::RefCell, rc::Rc};
use threadlane_ui_kit::TerminalTabAction;

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
fn shared_terminal_output_menu_supports_keyboard_appearance_choices(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| TerminalPreview::new("Sample project".into(), cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    draw(cx);
    for (keys, large) in [
        ("down down down enter", true),
        ("down down down down enter", false),
    ] {
        let point = cx.debug_bounds("sample-terminal-grid").unwrap().center();
        cx.simulate_click(point, Modifiers::default());
        cx.simulate_mouse_down(point, gpui::MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(point, gpui::MouseButton::Right, Modifiers::default());
        draw(cx);
        assert!(!cx.update(|window, cx| view.read(cx).focus.is_focused(window)));
        cx.simulate_keystrokes(keys);
        draw(cx);
        view.read_with(cx, |host, _| {
            assert_eq!(host.font_size, 16.);
            assert_eq!(host.compact, !large);
        });
        assert!(
            cx.update(|window, cx| view.read(cx).focus.is_focused(window)),
            "menu returns focus to terminal"
        );
    }
    view.update(cx, |host, cx| {
        host.status = Some(("Terminal read failed: sample error".into(), true));
        cx.notify();
    });
    draw(cx);
    let grid = cx.debug_bounds("sample-terminal-grid").unwrap();
    let status = cx.debug_bounds("terminal-status").unwrap();
    let output = cx.debug_bounds("sample-terminal-output").unwrap();
    assert!(
        grid.bottom() <= status.top(),
        "status has its own row after the grid"
    );
    assert!(
        status.bottom() <= output.bottom(),
        "status stays inside the output surface"
    );
    click(cx, "terminal-restart-banner-btn");
    assert!(view.read_with(cx, |host, _| host.status.is_none()));
}

#[gpui::test]
fn shared_terminal_chrome_preserves_shell_identity_and_close_confirmation(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| TerminalPreview::new("Sample project".into(), cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    let capture = events.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event, _| {
            capture.borrow_mut().push(match event {
                TerminalPreviewEvent::Hide => "hide".into(),
                TerminalPreviewEvent::AddSelection(text) => text.clone(),
            });
        })
    });
    draw(cx);
    let shell = cx.debug_bounds("sample-shell-1-select").unwrap().center();
    cx.simulate_mouse_move(shell, None, Modifiers::default());
    cx.simulate_mouse_down(shell, gpui::MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(shell, gpui::MouseButton::Right, Modifiers::default());
    draw(cx);
    assert!(cx.update(|window, cx| window.focused(cx).is_some()));
    cx.simulate_keystrokes("escape");
    draw(cx);
    view.read_with(cx, |host, _| {
        assert_eq!(host.selected, 1);
        assert_eq!(host.shells.len(), 2);
        assert_eq!(host.close_armed, None);
    });
    click(cx, "sample-shell-2-select");
    assert_eq!(view.read_with(cx, |host, _| host.selected), 2);
    click(cx, "sample-shell-1-close");
    view.read_with(cx, |host, _| {
        assert_eq!(host.close_armed, Some(1));
        assert_eq!(host.selected, 2);
        assert_eq!(host.shells.len(), 2);
    });
    click(cx, "sample-shell-1-close");
    assert_eq!(view.read_with(cx, |host, _| host.selected), 2);
    assert!(
        cx.debug_bounds("sample-shell-2-select").is_some(),
        "remaining shell retains control identity after reordering"
    );
    // A callback targeting the removed shell cannot act on its replacement index.
    view.update(cx, |host, cx| {
        host.request_tab(1, TerminalTabAction::CloseOthers, cx)
    });
    assert_eq!(view.read_with(cx, |host, _| host.shells.len()), 1);
    click(cx, "terminal-add-selection-to-chat");
    assert!(events.borrow().is_empty());
    click(cx, "terminal-preview-selection");
    click(cx, "terminal-add-selection-to-chat");
    assert!(events.borrow()[0].contains("Finished dev profile"));
    click(cx, "terminal-new-tab");
    assert_eq!(view.read_with(cx, |host, _| host.selected), 3);
    click(cx, "terminal-preview-selection");
    click(cx, "terminal-add-selection-to-chat");
    assert!(!view.read_with(cx, |host, _| host.selection));
    assert_eq!(
        events.borrow().len(),
        1,
        "empty output cannot add a selection"
    );
    click(cx, "sample-shell-3-close");
    assert_eq!(
        view.read_with(cx, |host, _| host.selected),
        2,
        "empty sample shell closes immediately"
    );
    click(cx, "sample-shell-2-close");
    assert_eq!(
        view.read_with(cx, |host, _| host.shells.len()),
        1,
        "output requires confirmation"
    );
    click(cx, "sample-shell-2-close");
    assert_eq!(events.borrow().last().map(String::as_str), Some("hide"));
    assert_eq!(view.read_with(cx, |host, _| host.selected), 4);
    assert!(view.read_with(cx, |host, _| host.shells[0].output.is_empty()));
}

#[gpui::test]
fn shared_terminal_actions_wrap_and_recover_unavailable_checkout(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            TerminalPreview::new("A long project name for the terminal toolbar".into(), cx)
        });
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    for width in [360., 800., 1200.] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(500.)));
        draw(cx);
        for selector in [
            "terminal-new-tab",
            "terminal-close-panel-btn",
            "terminal-clear-btn",
            "terminal-restart-btn",
            "terminal-find-btn",
            "terminal-open-link",
            "terminal-add-selection-to-chat",
        ] {
            let bounds = cx.debug_bounds(selector).unwrap();
            assert!(
                bounds.left() >= gpui::px(0.) && bounds.right() <= gpui::px(width),
                "{selector} stays reachable at {width}"
            );
            assert!(bounds.bottom() <= gpui::px(500.));
        }
    }
    click(cx, "terminal-clear-btn");
    assert!(view.read_with(cx, |host, _| host.shells[0].output.is_empty()));
    click(cx, "terminal-restart-btn");
    assert!(!view.read_with(cx, |host, _| host.shells[0].output.is_empty()));
    click(cx, "terminal-preview-unavailable");
    assert!(cx.debug_bounds("terminal-new-tab").is_none());
    click(cx, "terminal-recreate-worktree");
    assert!(!view.read_with(cx, |host, _| host.unavailable));
    click(cx, "terminal-preview-unavailable");
    click(cx, "terminal-use-project-folder");
    assert!(cx.debug_bounds("terminal-new-tab").is_some());
}

fn terminal_with_find(
    cx: &mut TestAppContext,
) -> (gpui::Entity<TerminalPreview>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    cx.update(threadlane_ui_kit::init_terminal_find);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let terminal = cx.new(|cx| TerminalPreview::new("Sample project".into(), cx));
        gpui_component::Root::new(terminal, window, cx)
    });
    let terminal = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<TerminalPreview>().unwrap()
    });
    draw(cx);
    (terminal, cx)
}

fn replace_query(cx: &mut VisualTestContext, value: &str) {
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    cx.simulate_input(value);
    draw(cx);
}

fn choose_external_link(cx: &mut VisualTestContext) {
    // Remove hover selection seeded by the opening click before testing arrows.
    let corner = cx.update(|window, _| {
        gpui::point(
            window.viewport_size().width - gpui::px(1.),
            window.viewport_size().height - gpui::px(1.),
        )
    });
    cx.simulate_mouse_move(corner, None, Modifiers::default());
    draw(cx);
    cx.simulate_keystrokes("down down down");
    draw(cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
}

#[gpui::test]
fn shared_terminal_find_keeps_shell_scope_case_and_keyboard(cx: &mut TestAppContext) {
    let (view, cx) = terminal_with_find(cx);
    click(cx, "terminal-find-btn");
    cx.simulate_input("threadlane");
    draw(cx);
    view.read_with(cx, |host, _| {
        let find = host.shells[0].find.as_ref().unwrap();
        assert_eq!(find.hits.len(), 1);
        assert!(find.hits[0].1.contains("threadlane-ui-kit"));
    });
    replace_query(cx, "Threadlane");
    assert!(view.read_with(cx, |host, _| {
        host.shells[0].find.as_ref().unwrap().hits.is_empty()
    }));
    click(cx, "terminal-find-next");
    assert_eq!(
        view.read_with(cx, |host, _| host.shells[0].find.as_ref().unwrap().selected),
        None
    );
    replace_query(cx, "Finished");
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |host, _| host.shells[0].find.as_ref().unwrap().selected),
        Some(0)
    );
    click(cx, "sample-shell-2-select");
    assert!(cx.debug_bounds("terminal-find-next").is_none());
    click(cx, "terminal-find-btn");
    assert!(view.read_with(cx, |host, _| {
        host.shells[1].find.as_ref().unwrap().query.is_empty()
    }));
    click(cx, "sample-shell-1-select");
    view.read_with(cx, |host, _| {
        let find = host.shells[0].find.as_ref().unwrap();
        assert_eq!(find.query, "Finished");
        assert_eq!(find.selected, Some(0));
    });
    click(cx, "terminal-find-btn");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(view.read_with(cx, |host, _| host.shells[0].find.is_none()));
    assert!(view.read_with(cx, |host, _| host.shells[1].find.is_some()));
}

#[gpui::test]
fn shared_terminal_find_states_wrap_retry_and_link_menu_restores_focus(cx: &mut TestAppContext) {
    let (view, cx) = terminal_with_find(cx);
    click(cx, "terminal-find-btn");
    cx.simulate_input("Finished");
    draw(cx);
    click(cx, "terminal-preview-search-state");
    cx.simulate_keystrokes("down down down enter");
    draw(cx);
    assert!(
        view.read_with(cx, |host, _| host.shells[0].find.as_ref().unwrap().mode
            == SampleSearchMode::Failed)
    );
    for width in [360., 800., 1200.] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(360.)));
        draw(cx);
        for selector in [
            "terminal-find-previous",
            "terminal-find-next",
            "terminal-find-retry",
            "terminal-find-close",
        ] {
            let bounds = cx.debug_bounds(selector).unwrap();
            assert!(
                bounds.left() >= gpui::px(0.) && bounds.right() <= gpui::px(width),
                "{selector} at {width}"
            );
            assert!(bounds.bottom() <= gpui::px(360.));
        }
    }
    click(cx, "terminal-find-next");
    assert_eq!(
        view.read_with(cx, |host, _| host.shells[0].find.as_ref().unwrap().selected),
        None
    );
    click(cx, "terminal-find-retry");
    assert!(
        view.read_with(cx, |host, _| host.shells[0].find.as_ref().unwrap().mode
            == SampleSearchMode::Live)
    );
    cx.simulate_keystrokes("enter");
    draw(cx);
    // Retry restores results, and selecting via the visible control stays in the current shell.
    click(cx, "terminal-find-next");
    assert_eq!(
        view.read_with(cx, |host, _| host.shells[0].find.as_ref().unwrap().selected),
        Some(0)
    );
    click(cx, "terminal-open-link");
    assert!(view.read_with(cx, |host, _| host.link_menu.is_some()));
    cx.simulate_keystrokes("escape");
    draw(cx);
    cx.update(|window, cx| {
        assert!(view.read(cx).link_menu.is_none());
        assert!(view.read(cx).focus.is_focused(window));
    });
    click(cx, "terminal-open-link");
    assert!(view.read_with(cx, |host, _| {
        host.shells[0].output.contains("https://gpui-kit.com")
    }));
    choose_external_link(cx);
    view.read_with(cx, |host, _| {
        assert!(
            host.feedback
                .as_ref()
                .is_some_and(|feedback| feedback.contains("default browser")
                    && feedback.contains("https://gpui-kit.com")),
            "link feedback: {:?}",
            host.feedback
        )
    });
    // A frozen menu cannot act on output invalidated after it opened.
    view.update(cx, |host, _| host.feedback = None);
    click(cx, "terminal-open-link");
    cx.update(|window, cx| {
        view.update(cx, |host, cx| {
            host.request(threadlane_ui_kit::TerminalAction::Clear, window, cx)
        })
    });
    draw(cx);
    choose_external_link(cx);
    assert!(view.read_with(cx, |host, _| host.feedback.is_none()));
}

#[gpui::test]
fn shared_terminal_grid_drag_uses_painted_metrics_at_every_zoom(cx: &mut TestAppContext) {
    let (view, cx) = terminal_with_find(cx);
    for font in [12., 16., 20.] {
        cx.update(|_, cx| {
            gpui_component::Theme::global_mut(cx).font_size = gpui::px(font);
            gpui_component::Theme::sync_base(cx);
        });
        draw(cx);
        let bounds = cx.debug_bounds("sample-terminal-grid").unwrap();
        let metrics = cx.update(|window, cx| {
            threadlane_ui_kit::TerminalTextMetrics::measure(
                threadlane_ui_kit::TERMINAL_FONT_SIZE,
                false,
                window,
                cx,
            )
        });
        let inset = cx.update(|window, _| metrics.inset(window));
        let start = gpui::point(
            bounds.left() + gpui::px(inset + 0.2 * metrics.cell_width()),
            bounds.top() + gpui::px(inset + 0.5 * metrics.row_height()),
        );
        let end = gpui::point(start.x + gpui::px(7.0 * metrics.cell_width()), start.y);
        assert_eq!(view.read_with(cx, |host, _| host.geometry.as_ref().unwrap().cell_at(end)), Some((0, 7)), "font={font}, inset={inset}, width={}, bounds={bounds:?}, start={start:?}, end={end:?}, geometry={}", metrics.cell_width(), view.read_with(cx, |host, _| format!("{:?}", host.geometry)));
        assert_eq!(
            view.read_with(cx, |host, _| host
                .geometry
                .as_ref()
                .unwrap()
                .link_cell_at(start)),
            Some((0, 0)),
            "paint and link bounds must agree at zoom {font}"
        );
        cx.simulate_mouse_move(start, None, Modifiers::default());
        cx.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, Some(gpui::MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(end, gpui::MouseButton::Left, Modifiers::default());
        draw(cx);
        assert_eq!(
            view.read_with(cx, |host, _| host.selected_text())
                .as_deref(),
            Some("$ cargo")
        );
        assert_eq!(
            view.read_with(cx, |host, _| host.selection_anchor),
            Some((0, 0))
        );
        assert_eq!(
            view.read_with(cx, |host, _| host.selection_head),
            Some((0, 7))
        );
    }
}
