use super::{visible_terminal_frame, LinkDestination, OpenTerminalLink, PtyEvent, TerminalView};
use gpui::{
    point, px, AppContext, Entity, Focusable, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, TestAppContext,
};
use std::sync::{Arc, Mutex};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

struct RecordingIo(Arc<Mutex<Vec<u8>>>);

impl super::TerminalIo for RecordingIo {
    fn write(&self, bytes: &[u8]) {
        self.0.lock().unwrap().extend_from_slice(bytes);
    }

    fn resize(&self, _: u16, _: u16) {}
}

fn output(terminal: &mut TerminalView, text: &str) {
    let mut parser = vt100::Parser::new(terminal.rows, terminal.cols, 20);
    parser.process(text.as_bytes());
    let frame = visible_terminal_frame(&mut parser, terminal.rows, terminal.cols)
        .with_link_epoch(terminal.frame_epoch);
    terminal.apply_event(PtyEvent::Frame(frame));
}

fn terminal(cx: &mut TestAppContext) -> (Entity<TerminalView>, &mut gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let terminal = cx.new(|cx| TerminalView::new_for_test(PathBuf::from("/tmp"), cx));
        gpui_component::Root::new(terminal, window, cx)
    });
    let terminal = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<TerminalView>().unwrap()
    });
    cx.run_until_parked();
    (terminal, cx)
}

#[gpui::test]
fn links_menu_escape_restores_terminal_and_freezes_destination(cx: &mut TestAppContext) {
    let (terminal, cx) = terminal(cx);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let events = opened.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&terminal, move |_, event: &OpenTerminalLink, _| {
            events.borrow_mut().push(event.clone())
        })
    });
    cx.update(|window, cx| {
        terminal.update(cx, |terminal, cx| {
            output(terminal, "http://localhost:3000/ http://localhost:3000/");
            terminal.open_links(window, cx);
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|window, cx| {
        assert!(terminal.read(cx).link_menu.is_none());
        assert!(terminal.read(cx).focus_handle(cx).is_focused(window));
        terminal.update(cx, |terminal, cx| terminal.open_links(window, cx));
    });
    cx.run_until_parked();
    let epoch = terminal.read_with(cx, |terminal, _| {
        assert!(!terminal.links.is_empty(), "links lost before reopening");
        terminal.link_epoch
    });
    terminal.update(cx, |terminal, cx| {
        output(terminal, "http://different/");
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        let terminal = terminal.read(cx);
        assert_eq!(terminal.link_epoch, epoch);
        assert!(terminal.link_menu.is_some(), "menu dismissed by output");
        assert!(
            terminal
                .link_menu
                .as_ref()
                .unwrap()
                .read(cx)
                .focus_handle(cx)
                .is_focused(window),
            "menu lost focus"
        );
    });
    cx.simulate_keystrokes("down down");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(opened.borrow().len(), 1);
    assert_eq!(opened.borrow()[0].url, "http://localhost:3000/");
}

#[gpui::test]
fn links_pointer_requires_modifier_and_stable_press_release(cx: &mut TestAppContext) {
    let (terminal, cx) = terminal(cx);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let events = opened.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&terminal, move |_, event: &OpenTerminalLink, _| {
            events.borrow_mut().push(event.clone())
        })
    });
    cx.update(|window, cx| {
        terminal.update(cx, |terminal, cx| {
            output(terminal, "http://localhost:3000/");
            let bounds = terminal.screen_bounds.unwrap();
            let position = bounds.origin + point(px(16.), px(16.));
            let modifiers = if cfg!(target_os = "macos") {
                Modifiers {
                    platform: true,
                    ..Default::default()
                }
            } else {
                Modifiers {
                    control: true,
                    ..Default::default()
                }
            };
            let down = MouseDownEvent {
                position,
                button: MouseButton::Left,
                modifiers,
                click_count: 1,
                ..Default::default()
            };
            let up = MouseUpEvent {
                position,
                button: MouseButton::Left,
                modifiers,
                click_count: 1,
            };
            terminal.begin_selection(
                &MouseDownEvent {
                    modifiers: Modifiers::default(),
                    ..down.clone()
                },
                window,
                cx,
            );
            terminal.end_selection(&up, window, cx);
            terminal.begin_selection(&down, window, cx);
            terminal.extend_selection(
                &MouseMoveEvent {
                    position: position + point(px(10.), px(0.)),
                    pressed_button: Some(MouseButton::Left),
                    modifiers,
                },
                window,
                cx,
            );
            terminal.end_selection(&up, window, cx);
            terminal.begin_selection(&down, window, cx);
            output(terminal, "http://different/");
            terminal.end_selection(&up, window, cx);
            terminal.begin_selection(&down, window, cx);
            terminal.end_selection(&up, window, cx);
        })
    });
    cx.run_until_parked();
    assert_eq!(opened.borrow().len(), 1);
    assert_eq!(opened.borrow()[0].url, "http://different/");
}

#[gpui::test]
fn links_clear_and_resize_reject_queued_frames_and_old_menu_actions(cx: &mut TestAppContext) {
    let terminal = cx.new(|cx| TerminalView::new_for_test(PathBuf::from("/tmp"), cx));
    let opened = Rc::new(RefCell::new(Vec::new()));
    let events = opened.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&terminal, move |_, event: &OpenTerminalLink, _| {
            events.borrow_mut().push(event.clone())
        })
    });
    terminal.update(cx, |terminal, cx| {
        output(terminal, "http://host/");
        let epoch = terminal.link_epoch;
        let mut parser = vt100::Parser::new(terminal.rows, terminal.cols, 0);
        parser.process(b"http://stale/");
        let stale = visible_terminal_frame(&mut parser, terminal.rows, terminal.cols);
        terminal.clear(cx);
        terminal.apply_event(PtyEvent::Frame(stale));
        assert!(terminal.links.is_empty());
        terminal.activate_link(
            "http://host/".into(),
            LinkDestination::DefaultBrowser,
            epoch,
            cx,
        );
        output(terminal, "http://host/");
        let epoch = terminal.link_epoch;
        terminal.resize(10, 40, cx);
        terminal.activate_link(
            "http://host/".into(),
            LinkDestination::DefaultBrowser,
            epoch,
            cx,
        );
        assert!(terminal.links.is_empty());
    });
    cx.run_until_parked();
    assert!(opened.borrow().is_empty());
}

#[gpui::test]
fn terminal_dispatches_modified_keys_to_the_io_and_keeps_shift_navigation_local(
    cx: &mut TestAppContext,
) {
    use gpui::Focusable as _;

    let (terminal, cx) = terminal(cx);
    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = written.clone();
    terminal.update(cx, |terminal, _| {
        terminal.session = Some(Box::new(RecordingIo(sink)));
    });
    cx.update(|window, app| {
        terminal.read(app).focus_handle(app).focus(window, app);
        window.draw(app).clear(app);
    });
    cx.simulate_keystrokes("ctrl-shift-up");
    assert_eq!(&*written.lock().unwrap(), b"\x1b[1;6A");
    cx.simulate_keystrokes("shift-up");
    assert_eq!(&*written.lock().unwrap(), b"\x1b[1;6A");
}

#[gpui::test]
fn terminal_dispatches_mouse_reports_and_keeps_local_interactions(cx: &mut TestAppContext) {
    let (terminal, cx) = terminal(cx);
    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = written.clone();
    terminal.update(cx, |terminal, _| {
        terminal.session = Some(Box::new(RecordingIo(sink)));
        output(terminal, "\x1b[?1002h\x1b[?1006hword other");
    });
    cx.run_until_parked();
    let position = terminal.read_with(cx, |terminal, _| {
        terminal.screen_bounds.unwrap().origin + gpui::point(gpui::px(16.), gpui::px(16.))
    });
    let cell = terminal.read_with(cx, |terminal, _| terminal.report_cell_at(position).unwrap());
    cx.update(|window, app| window.draw(app).clear(app));
    cx.simulate_event(MouseMoveEvent {
        position,
        ..Default::default()
    });
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear(app));

    cx.simulate_event(MouseDownEvent {
        position,
        button: MouseButton::Left,
        ..Default::default()
    });
    cx.update(|window, app| {
        assert!(
            terminal.read(app).focus_handle(app).is_focused(window),
            "reported mouse input must focus the terminal"
        );
    });
    cx.simulate_event(MouseUpEvent {
        position,
        button: MouseButton::Left,
        ..Default::default()
    });
    let expected = format!(
        "\x1b[<0;{};{}M\x1b[<0;{};{}m",
        cell.1 + 1,
        cell.0 + 1,
        cell.1 + 1,
        cell.0 + 1
    );
    assert_eq!(
        &*written.lock().unwrap(),
        expected.as_bytes(),
        "mouse down/up must use SGR press and release reports"
    );

    written.lock().unwrap().clear();
    let (padding, padding_cell) = terminal.read_with(cx, |terminal, _| {
        let bounds = terminal.screen_bounds.unwrap();
        let padding = bounds.origin + gpui::point(bounds.size.width - gpui::px(1.), gpui::px(16.));
        assert!(
            terminal.report_cell_at(padding).is_none(),
            "drag endpoint should be in inset padding"
        );
        (padding, terminal.cell_at(padding).unwrap())
    });
    cx.update(|window, app| window.draw(app).clear(app));
    cx.simulate_event(MouseDownEvent {
        position,
        button: MouseButton::Left,
        ..Default::default()
    });
    cx.simulate_event(MouseMoveEvent {
        position: padding,
        pressed_button: Some(MouseButton::Left),
        modifiers: Default::default(),
    });
    cx.simulate_event(MouseUpEvent {
        position: padding,
        button: MouseButton::Left,
        ..Default::default()
    });
    let drag_expected = format!(
        "\x1b[<0;{};{}M\x1b[<32;{};{}M\x1b[<0;{};{}m",
        cell.1 + 1,
        cell.0 + 1,
        padding_cell.1 + 1,
        padding_cell.0 + 1,
        padding_cell.1 + 1,
        padding_cell.0 + 1,
    );
    assert_eq!(
        &*written.lock().unwrap(),
        drag_expected.as_bytes(),
        "drag and release in grid padding must report bounded SGR cells"
    );

    written.lock().unwrap().clear();
    cx.update(|window, app| window.draw(app).clear(app));
    let other = terminal.read_with(cx, |terminal, _| {
        terminal.screen_bounds.unwrap().origin
            + gpui::point(
                gpui::px(terminal.content_inset + terminal.cell_width * 10.0 + 0.1),
                gpui::px(16.),
            )
    });
    let shift = Modifiers {
        shift: true,
        ..Default::default()
    };
    cx.simulate_event(MouseDownEvent {
        position,
        button: MouseButton::Left,
        modifiers: shift,
        ..Default::default()
    });
    cx.simulate_event(MouseMoveEvent {
        position: other,
        pressed_button: Some(MouseButton::Left),
        modifiers: shift,
    });
    cx.simulate_event(MouseUpEvent {
        position: other,
        button: MouseButton::Left,
        modifiers: shift,
        ..Default::default()
    });
    assert!(
        written.lock().unwrap().is_empty(),
        "Shift selection must bypass mouse reporting"
    );
    assert_eq!(
        terminal.read_with(cx, |terminal, _| terminal.selected_text()),
        Some("word other".to_string())
    );

    terminal.update(cx, |terminal, _| {
        let mut parser = vt100::Parser::new(terminal.rows, terminal.cols, 20);
        parser.process(b"\x1b[?1002h\x1b[?1006h");
        for index in 0..=terminal.rows {
            parser.process(format!("line{index}\r\n").as_bytes());
        }
        parser.screen_mut().set_scrollback(1);
        let frame = visible_terminal_frame(&mut parser, terminal.rows, terminal.cols)
            .with_link_epoch(terminal.frame_epoch);
        terminal.apply_event(PtyEvent::Frame(frame));
    });
    cx.run_until_parked();
    written.lock().unwrap().clear();
    cx.update(|window, app| window.draw(app).clear(app));
    let before_offset = terminal.read_with(cx, |terminal, _| terminal.scrollback_offset);
    cx.simulate_event(gpui::ScrollWheelEvent {
        position,
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(40.))),
        ..Default::default()
    });
    assert!(
        written.lock().unwrap().is_empty(),
        "scrollback wheel input must not report to the TUI"
    );
    assert!(
        terminal.read_with(cx, |terminal, _| terminal.scrollback_offset) > before_offset,
        "scrollback wheel input must remain local"
    );
}

#[gpui::test]
fn terminal_context_menu_respects_shift_while_mouse_reporting(cx: &mut TestAppContext) {
    let (terminal, cx) = terminal(cx);
    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = written.clone();
    terminal.update(cx, |terminal, _| {
        terminal.session = Some(Box::new(RecordingIo(sink)));
        output(terminal, "\x1b[?1000h\x1b[?1006hmenu output");
    });
    cx.run_until_parked();
    let position = terminal.read_with(cx, |terminal, _| {
        terminal.screen_bounds.unwrap().origin + gpui::point(gpui::px(16.), gpui::px(16.))
    });
    let cell = terminal.read_with(cx, |terminal, _| terminal.report_cell_at(position).unwrap());
    let terminal_focus = terminal.read_with(cx, |terminal, cx| terminal.focus_handle(cx));
    cx.update(|window, app| window.draw(app).clear(app));

    cx.simulate_event(MouseDownEvent {
        position,
        button: MouseButton::Right,
        ..Default::default()
    });
    cx.run_until_parked();
    cx.update(|window, app| {
        window.draw(app).clear(app);
        assert!(
            terminal_focus.is_focused(window),
            "unmodified right-click must keep focus in the terminal"
        );
    });
    cx.simulate_event(MouseUpEvent {
        position,
        button: MouseButton::Right,
        ..Default::default()
    });
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear(app));
    let expected = format!(
        "\x1b[<2;{};{}M\x1b[<2;{};{}m",
        cell.1 + 1,
        cell.0 + 1,
        cell.1 + 1,
        cell.0 + 1
    );
    assert_eq!(&*written.lock().unwrap(), expected.as_bytes());

    written.lock().unwrap().clear();
    let shift = Modifiers {
        shift: true,
        ..Default::default()
    };
    cx.simulate_mouse_move(position, None, shift);
    cx.simulate_event(MouseDownEvent {
        position,
        button: MouseButton::Right,
        modifiers: shift,
        ..Default::default()
    });
    cx.run_until_parked();
    cx.update(|window, app| {
        window.draw(app).clear(app);
        assert!(
            !terminal_focus.is_focused(window),
            "Shift-right-click must transfer focus to the local menu"
        );
    });
    cx.simulate_event(MouseUpEvent {
        position,
        button: MouseButton::Right,
        modifiers: shift,
        ..Default::default()
    });
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear(app));
    assert!(
        written.lock().unwrap().is_empty(),
        "Shift-right-click must not report mouse input to the TUI"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|window, _| {
        assert!(
            terminal_focus.is_focused(window),
            "Escape must restore focus from the local menu to the terminal"
        );
    });

    cx.simulate_event(MouseDownEvent {
        position,
        button: MouseButton::Right,
        ..Default::default()
    });
    cx.simulate_event(MouseUpEvent {
        position,
        button: MouseButton::Right,
        ..Default::default()
    });
    cx.run_until_parked();
    assert_eq!(&*written.lock().unwrap(), expected.as_bytes());
}

#[gpui::test]
fn terminal_multiclick_dispatch_selects_words_and_visible_lines(cx: &mut TestAppContext) {
    let (terminal, cx) = terminal(cx);
    terminal.update(cx, |terminal, _cx| output(terminal, "word other"));
    cx.run_until_parked();
    let position = terminal.read_with(cx, |terminal, _| {
        terminal.screen_bounds.unwrap().origin + gpui::point(gpui::px(30.), gpui::px(16.))
    });
    cx.update(|window, app| window.draw(app).clear(app));
    cx.simulate_event(MouseDownEvent {
        position,
        button: MouseButton::Left,
        click_count: 2,
        first_mouse: false,
        ..Default::default()
    });
    assert_eq!(
        terminal.read_with(cx, |terminal, _| terminal.selected_text()),
        Some("word".to_string())
    );

    cx.update(|window, app| window.draw(app).clear(app));
    let other_word = position + gpui::point(gpui::px(40.), gpui::px(0.));
    cx.simulate_event(MouseMoveEvent {
        position: other_word,
        pressed_button: Some(MouseButton::Left),
        modifiers: Default::default(),
    });
    assert_eq!(
        terminal.read_with(cx, |terminal, _| terminal.selected_text()),
        Some("word other".to_string())
    );
    cx.update(|window, app| window.draw(app).clear(app));
    cx.simulate_event(MouseUpEvent {
        position: other_word,
        button: MouseButton::Left,
        click_count: 2,
        ..Default::default()
    });

    cx.update(|window, app| window.draw(app).clear(app));
    cx.simulate_event(MouseDownEvent {
        position,
        button: MouseButton::Left,
        click_count: 3,
        first_mouse: false,
        ..Default::default()
    });
    assert_eq!(
        terminal.read_with(cx, |terminal, _| terminal.selected_text()),
        Some("word other".to_string())
    );
}

#[test]
fn links_worker_omits_clipped_wrap_and_only_includes_current_viewport() {
    let mut parser = vt100::Parser::new(3, 20, 20);
    parser.process(b" http://host/a-long-path-that-wraps-over-rows\r\nother\r\nlast");
    let frame = visible_terminal_frame(&mut parser, 3, 20);
    assert!(frame.links.is_empty());
    parser.screen_mut().set_scrollback(2);
    let frame = visible_terminal_frame(&mut parser, 3, 20);
    assert_eq!(
        frame.links[0].url,
        "http://host/a-long-path-that-wraps-over-rows"
    );
    assert_eq!(parser.screen().scrollback(), 2);
}

#[gpui::test]
fn links_menu_clear_dismisses_and_restores_focus(cx: &mut TestAppContext) {
    let (terminal, cx) = terminal(cx);
    cx.update(|window, cx| {
        terminal.update(cx, |terminal, cx| {
            output(terminal, "http://host/");
            terminal.open_links(window, cx);
        })
    });
    cx.run_until_parked();
    terminal.update(cx, |terminal, cx| terminal.clear(cx));
    cx.run_until_parked();
    cx.update(|window, cx| {
        assert!(terminal.read(cx).link_menu.is_none());
        assert!(terminal.read(cx).focus_handle(cx).is_focused(window));
    });
}

#[test]
fn links_validation_keeps_destination_and_rejects_parser_fixups() {
    for url in [
        "https://host/%",
        "https://host/%zz",
        "https://host/\n",
        "http://@host/",
        "https://host\\other",
        "http:host",
        "http:///host",
    ] {
        assert!(!super::is_web_url(url), "{url:?}");
    }
    let mut parser = vt100::Parser::new(4, 80, 0);
    parser.process("http://0.0.0.0:3000/界e\u{301}?q=x%20y#fragment ".as_bytes());
    let frame = visible_terminal_frame(&mut parser, 4, 80);
    assert_eq!(
        frame.links[0].url,
        "http://0.0.0.0:3000/界e\u{301}?q=x%20y#fragment"
    );
    // Both columns of the wide path character belong to the same destination.
    assert!(frame.links[0].cells.contains(&(0, 18)));
    assert!(frame.links[0].cells.contains(&(0, 19)));
}
