use super::{SessionPreview, Snapshot};
use gpui::{px, size, AppContext, Modifiers, TestAppContext};
use std::{cell::RefCell, rc::Rc, sync::Arc};

#[gpui::test]
fn captured_files_share_keyboard_insertion_dismissal_and_narrow_geometry(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    cx.update(threadlane_ui_kit::file_completion::init_file_completion);
    let snapshot: Snapshot = serde_json::from_value(serde_json::json!({
        "session": null, "messages": [],
        "file_inventory": {"Ok": {"paths": ["src/lib.rs", "src/main.rs", "日本 語/tick`file.rs"], "non_utf8_skipped": 2}}
    })).unwrap();
    let retained = Rc::new(RefCell::new(None));
    let capture = retained.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| SessionPreview::with_snapshot(Arc::new(snapshot), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = retained.borrow_mut().take().unwrap();
    cx.run_until_parked();
    cx.simulate_resize(size(px(480.0), px(900.0)));
    cx.simulate_input("open @src/");
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    let menu = cx.debug_bounds("file-completion-menu").unwrap();
    assert!(menu.left() >= px(0.0) && menu.right() <= px(480.0));
    let list = cx.debug_bounds("file-completion-list").unwrap();
    assert!(list.left() >= menu.left() && list.right() <= menu.right());
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |host, cx| host.input.read(cx).value().to_string()),
        "open `src/main.rs` "
    );
    cx.simulate_input("@src/");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(view.read_with(cx, |host, _| host.dismiss_file_menu));
    assert!(view.read_with(cx, |host, cx| host
        .input
        .read(cx)
        .value()
        .ends_with("@src/")));
    cx.simulate_input("main");
    cx.run_until_parked();
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |host, cx| host.input.read(cx).value().to_string()),
        "open `src/main.rs` `src/main.rs` "
    );
    // A click invokes the same controlled Insert request as the desktop.
    cx.simulate_input("@");
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    let row = cx.debug_bounds("composer-file-2").unwrap();
    cx.simulate_click(row.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(view.read_with(cx, |host, cx| host
        .input
        .read(cx)
        .value()
        .ends_with("``日本 語/tick`file.rs`` ")));
}

#[gpui::test]
fn missing_captured_files_do_not_turn_enter_into_a_newline(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    cx.update(threadlane_ui_kit::file_completion::init_file_completion);
    let snapshot: Snapshot =
        serde_json::from_value(serde_json::json!({"session": null, "messages": []})).unwrap();
    let retained = Rc::new(RefCell::new(None));
    let capture = retained.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| SessionPreview::with_snapshot(Arc::new(snapshot), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = retained.borrow_mut().take().unwrap();
    cx.run_until_parked();
    cx.simulate_input("@missing");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter tab");
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |host, cx| host.input.read(cx).value().to_string()),
        "@missing"
    );
}
