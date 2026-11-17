use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext};
use std::{cell::RefCell, rc::Rc};
use threadlane_ui_kit::settings::SettingsCatalogStatus;

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

#[gpui::test]
fn shared_catalog_controls_keep_scope_identity_and_availability(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let captured = Rc::new(RefCell::new(None));
    let capture = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let preview = cx.new(|cx| super::SettingsPreview::new("Sample workspace".into(), window, cx));
        *capture.borrow_mut() = Some(preview.clone());
        gpui_component::Root::new(preview, window, cx)
    });
    let preview = captured.borrow_mut().take().unwrap();
    cx.simulate_resize(gpui::size(gpui::px(960.0), gpui::px(1100.0)));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    click(cx, "settings-skills");
    click(cx, "skill-toggle-sample-review");
    assert!(!preview.read_with(cx, |view, _| view.skills.rows[0].enabled));
    super::tests::activate_key(cx, "space");
    assert!(preview.read_with(cx, |view, _| view.skills.rows[0].enabled));
    click(cx, "skill-toggle-sample-invalid");
    assert!(!preview.read_with(cx, |view, _| view.skills.rows[2].enabled));
    click(cx, "skills-disable-all");
    assert!(preview.read_with(cx, |view, _| view
        .skills
        .rows
        .iter()
        .all(|row| !row.enabled)));
    click(cx, "skills-refresh");
    assert!(cx.debug_bounds("settings-catalog-status").is_some());

    click(cx, "settings-extensions");
    let global = cx
        .debug_bounds("extension-toggle-sample-global-search")
        .unwrap();
    let project = cx
        .debug_bounds("extension-toggle-sample-project-search")
        .unwrap();
    assert_ne!(
        global.top(),
        project.top(),
        "same extension in two scopes has separate controls"
    );
    click(cx, "extension-toggle-sample-project-search");
    assert!(preview.read_with(cx, |view, _| matches!(
        view.extensions.rows[0].status,
        SettingsCatalogStatus::Active
    )));
    assert!(!preview.read_with(cx, |view, _| view.extensions.rows[1].enabled));
    click(cx, "extension-remove-sample-project-search");
    assert!(preview.read_with(cx, |view, _| view
        .extensions
        .rows
        .iter()
        .any(|row| row.id == "sample-global-search")));
    assert!(!preview.read_with(cx, |view, _| view
        .extensions
        .rows
        .iter()
        .any(|row| row.id == "sample-project-search")));
    click(cx, "extension-scope-global");
    click(cx, "extension-install");
    assert!(preview.read_with(cx, |view, _| view
        .extensions
        .rows
        .iter()
        .any(|row| row.id == "sample-installed-1" && row.scope == "Global")));

    for (width, font) in [(480.0, 14.0), (800.0, 16.0), (1100.0, 20.0)] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(1400.0)));
        cx.update(|_, cx| gpui_component::Theme::global_mut(cx).font_size = gpui::px(font));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let heading = cx.debug_bounds("settings-page-heading").unwrap();
        let selectors = [
            (
                "extension-row-sample-global-search",
                "extension-toggle-sample-global-search",
                "extension-remove-sample-global-search",
            ),
            (
                "extension-row-sample-global-format",
                "extension-toggle-sample-global-format",
                "extension-remove-sample-global-format",
            ),
            (
                "extension-row-sample-installed-1",
                "extension-toggle-sample-installed-1",
                "extension-remove-sample-installed-1",
            ),
        ];
        let mut trailing_edge = None;
        for (row_selector, toggle_selector, remove_selector) in selectors {
            let row = cx.debug_bounds(row_selector).unwrap();
            let toggle = cx.debug_bounds(toggle_selector).unwrap();
            let remove = cx.debug_bounds(remove_selector).unwrap();
            assert_eq!(row.left(), heading.left());
            assert!(row.right() <= gpui::px(width));
            assert!(toggle.left() > row.left() && remove.right() < row.right());
            assert_eq!(toggle.center().y, remove.center().y);
            if let Some(right) = trailing_edge {
                assert_eq!(right, remove.right());
            }
            trailing_edge = Some(remove.right());
        }
        assert!(cx.debug_bounds("extension-install").unwrap().right() <= gpui::px(width));
    }
    cx.simulate_resize(gpui::size(gpui::px(960.0), gpui::px(1100.0)));
    cx.update(|_, cx| gpui_component::Theme::global_mut(cx).font_size = gpui::px(16.0));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    for selector in [
        "extension-remove-sample-global-search",
        "extension-remove-sample-global-format",
        "extension-remove-sample-installed-1",
    ] {
        click(cx, selector);
    }
    assert!(cx.debug_bounds("settings-catalog-empty").is_some());
    assert!(cx.debug_bounds("extension-install").is_some());

    // No-project installation is available only in Global scope.
    preview.update(cx, |view, cx| {
        view.extensions.has_project = false;
        view.extensions.install_globally = false;
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    click(cx, "extension-install");
    assert!(preview.read_with(cx, |view, _| view.extensions.rows.is_empty()));
    click(cx, "extension-scope-global");
    click(cx, "extension-install");
    assert!(preview.read_with(cx, |view, _| view
        .extensions
        .rows
        .iter()
        .any(|row| row.id == "sample-installed-2")));

    preview.update(cx, |view, cx| {
        view.skills.has_project = false;
        for row in &mut view.skills.rows {
            row.disabled_reason = Some("Attach a project to manage skills".into());
        }
        cx.notify();
    });
    click(cx, "settings-skills");
    click(cx, "skill-toggle-sample-review");
    assert!(!preview.read_with(cx, |view, _| view.skills.rows[0].enabled));
}
