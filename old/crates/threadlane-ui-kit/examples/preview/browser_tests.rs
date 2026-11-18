use super::BrowserPreview;
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
fn shared_browser_tabs_keep_identity_and_cancel_annotation(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| BrowserPreview::new(window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    cx.simulate_resize(gpui::size(gpui::px(420.0), gpui::px(700.0)));
    draw(cx);
    click(cx, "browser-new-tab");
    click(cx, "browser-new-tab");
    view.read_with(cx, |host, _| {
        assert_eq!(
            host.tabs.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(host.tabs[host.active].0, 2);
    });
    click(cx, "browser-tab-close-2");
    view.read_with(cx, |host, _| assert_eq!(host.tabs[host.active].0, 1));
    click(cx, "browser-tab-0");
    view.read_with(cx, |host, _| assert_eq!(host.tabs[host.active].0, 0));
    click(cx, "browser-annotate");
    assert!(view.read_with(cx, |host, _| host.annotating));
    cx.simulate_keystrokes("shift-escape");
    assert!(view.read_with(cx, |host, _| host.annotating));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!view.read_with(cx, |host, _| host.annotating));
    click(cx, "browser-tab-close-0");
    view.read_with(cx, |host, _| {
        assert_eq!(host.tabs.len(), 1);
        assert_eq!(
            host.tabs[host.active].0, 1,
            "closing does not recycle an identity"
        );
    });
    click(cx, "browser-tab-close-1");
    view.read_with(cx, |host, _| {
        assert_eq!(host.tabs.len(), 1, "desktop keeps its last tab")
    });
}

#[gpui::test]
fn shared_browser_address_tabs_and_viewport_fit_at_zoom(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| BrowserPreview::new(window, cx));
        view.update(cx, |host, _| {
            host.tabs = (0..12)
                .map(|id| (id, format!("https://long-subdomain-{id}.example.com/path")))
                .collect();
            host.active = 11;
            host.next_id = 12;
            host.annotating = true;
        });
        gpui_component::Root::new(view, window, cx)
    });
    for font in [13.0, 16.0, 20.0] {
        cx.update(|window, cx| {
            gpui_component::Theme::global_mut(cx).font_size = gpui::px(font);
            gpui_component::Theme::sync_base(cx);
            window.refresh();
        });
        for width in [320.0, 640.0] {
            cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(700.0)));
            draw(cx);
            let address = cx.debug_bounds("browser-address-field").unwrap();
            let strip = cx.debug_bounds("browser-tab-strip").unwrap();
            let add = cx.debug_bounds("browser-new-tab-control").unwrap();
            let viewport = cx.debug_bounds("browser-viewport").unwrap();
            assert!(
                address.size.width > gpui::px(80.0),
                "address collapsed at font {font}"
            );
            assert!(strip.right() <= add.left(), "tabs overlap New tab");
            for bounds in [address, strip, add, viewport] {
                assert!(
                    bounds.left() >= gpui::px(0.0) && bounds.right() <= gpui::px(width),
                    "overflow at font {font}: {bounds:?}"
                );
            }
            assert!(viewport.top() >= add.bottom() && viewport.bottom() <= gpui::px(700.0));
        }
    }
}
