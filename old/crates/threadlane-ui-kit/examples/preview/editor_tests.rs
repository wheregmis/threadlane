use super::{EditorPreview, FILE};
use gpui::{AppContext, Modifiers, TestAppContext};
use std::{cell::RefCell, rc::Rc};

#[gpui::test]
fn closed_sample_reopens_saved_bytes_through_shared_control_and_shortcut(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_kit::init_editor);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| EditorPreview::new(window, cx));
        gpui_component::Root::new(view, window, cx)
    });
    let view = root.read_with(cx, |root, _| root.view().clone().downcast::<EditorPreview>().unwrap());
    cx.update(|window, cx| view.update(cx, |host, cx| {
        host.buffer.update(cx, |buffer, cx| buffer.set_value("discarded sample edits", window, cx));
        host.remove(&host.tabs.clone(), window, cx);
    }));
    cx.update(|window, cx| { window.refresh(); window.draw(cx).clear(cx); });
    let button = cx.debug_bounds("editor-reopen-closed-file").unwrap();
    cx.simulate_click(button.center(), Modifiers::default());
    cx.run_until_parked();
    view.read_with(cx, |host, cx| {
        assert_eq!(host.tabs, vec![FILE.to_string()]);
        assert!(!host.closed_sample);
        assert_eq!(host.buffer.read(cx).value().as_str(), host.saved);
    });
    cx.update(|window, cx| view.update(cx, |host, cx| host.remove(&[FILE.into()], window, cx)));
    cx.update(|window, cx| { window.refresh(); window.draw(cx).clear(cx); });
    #[cfg(target_os = "macos")]
    cx.simulate_keystrokes("cmd-shift-t");
    #[cfg(not(target_os = "macos"))]
    cx.simulate_keystrokes("ctrl-shift-t");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |host, _| host.selected.clone()), Some(FILE.into()));
}

#[gpui::test]
fn saved_review_diffs_reuse_editor_tabs_and_preserve_unsaved_sample(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_kit::init_editor);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| EditorPreview::new(window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    cx.update(|window, cx| {
        view.update(cx, |host, cx| {
            host.buffer.update(cx, |buffer, cx| {
                buffer.set_value("Keep this unsaved draft", window, cx)
            });
            host.open_diff("src/one.rs @ abc1234".into(), "+first".into(), cx);
            host.open_diff("src/two.rs @ def5678".into(), "+second".into(), cx);
            host.open_diff("src/one.rs @ abc1234".into(), "+updated".into(), cx);
        });
        window.refresh();
        window.draw(cx).clear(cx);
    });
    assert_eq!(
        view.read_with(cx, |host, _| host.tabs.len()),
        4,
        "opening an existing recorded diff reuses its tab"
    );
    assert!(view.read_with(cx, |host, cx| host.dirty(cx)));
    assert_eq!(
        view.read_with(cx, |host, cx| host.buffer.read(cx).value().to_string()),
        "Keep this unsaved draft"
    );
    let close = cx
        .debug_bounds("sample-editor-diff:src/one.rs @ abc1234-close")
        .unwrap();
    cx.simulate_click(close.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |host, _| host.selected.as_deref().map(str::to_string)),
        Some("diff:src/two.rs @ def5678".into())
    );
    view.update(cx, |host, cx| host.open_sample(cx));
    assert_eq!(
        view.read_with(cx, |host, _| host.selected.as_deref().map(str::to_string)),
        Some(FILE.into())
    );
    assert_eq!(
        view.read_with(cx, |host, cx| host.buffer.read(cx).value().to_string()),
        "Keep this unsaved draft"
    );
    cx.update(|window, cx| view.update(cx, |host, cx| host.reset(window, cx)));
    assert_eq!(view.read_with(cx, |host, _| host.tabs.len()), 2);
    assert_eq!(view.read_with(cx, |host, _| host.diffs.len()), 1);
    assert!(!view.read_with(cx, |host, cx| host.dirty(cx)));
}
