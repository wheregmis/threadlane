use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext};
use std::{cell::RefCell, rc::Rc};

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

#[gpui::test]
fn external_agent_controls_preserve_scope_validate_and_fit(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let captured = Rc::new(RefCell::new(None));
    let capture = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let preview =
            cx.new(|cx| super::SettingsPreview::new("Sample workspace".into(), window, cx));
        *capture.borrow_mut() = Some(preview.clone());
        gpui_component::Root::new(preview, window, cx)
    });
    let preview = captured.borrow_mut().take().unwrap();
    cx.simulate_resize(gpui::size(gpui::px(1100.0), gpui::px(1800.0)));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    click(cx, "settings-acp");
    click(cx, "acp-toggle-project-codex");
    assert!(preview.read_with(cx, |view, _| view
        .external_agents
        .rows
        .iter()
        .any(|row| row.id == "codex" && !row.global && row.enabled)));
    super::tests::activate_key(cx, "space");
    assert!(!preview.read_with(cx, |view, _| view
        .external_agents
        .rows
        .iter()
        .any(|row| row.id == "codex" && !row.global && row.enabled)));
    click(cx, "acp-scope-global");
    click(cx, "acp-toggle-global-codex");
    assert!(preview.read_with(cx, |view, _| view
        .external_agents
        .rows
        .iter()
        .any(|row| row.id == "codex" && row.global && row.enabled)));
    assert!(!preview.read_with(cx, |view, _| view
        .external_agents
        .rows
        .iter()
        .any(|row| row.id == "codex" && !row.global && row.enabled)));
    let project = cx.debug_bounds("acp-row-project-sample-review").unwrap();
    let global = cx.debug_bounds("acp-row-global-sample-review").unwrap();
    assert_ne!(project.top(), global.top());
    click(cx, "acp-remove-project-sample-review");
    assert!(preview.read_with(cx, |view, _| view
        .external_agents
        .rows
        .iter()
        .any(|row| row.id == "sample-review" && row.global)));
    assert!(!preview.read_with(cx, |view, _| view
        .external_agents
        .rows
        .iter()
        .any(|row| row.id == "sample-review" && !row.global)));
    click(cx, "acp-add");
    assert!(preview.read_with(cx, |view, _| view
        .external_agents
        .status
        .as_deref()
        .unwrap()
        .contains("both")));
    cx.update(|window, cx| preview.update(cx, |view, cx| {
        view.acp_name
            .update(cx, |input, cx| input.set_value("Local custom agent", window, cx));
        view.acp_command
            .update(cx, |input, cx| input.set_value("agent --acp", window, cx));
    }));
    click(cx, "acp-add");
    assert!(preview.read_with(cx, |view, _| view
        .external_agents
        .rows
        .iter()
        .any(|row| row.name == "Local custom agent" && row.global)));
    click(cx, "acp-refresh");

    for (width, font) in [(480.0, 14.0), (800.0, 16.0), (1100.0, 20.0)] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(2400.0)));
        cx.update(|_, cx| gpui_component::Theme::global_mut(cx).font_size = gpui::px(font));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let heading = cx.debug_bounds("settings-page-heading").unwrap();
        for selector in [
            "acp-row-global-codex",
            "acp-row-global-sample-review",
            "acp-custom-form",
        ] {
            let row = cx.debug_bounds(selector).unwrap();
            assert_eq!(row.left(), heading.left());
            assert!(row.right() <= gpui::px(width));
        }
        let row = cx.debug_bounds("acp-row-global-sample-review").unwrap();
        let toggle = cx.debug_bounds("acp-toggle-global-sample-review").unwrap();
        let remove = cx.debug_bounds("acp-remove-global-sample-review").unwrap();
        assert!(toggle.left() > row.left() && remove.right() < row.right());
        assert_eq!(toggle.center().y, remove.center().y);
        assert!(cx.debug_bounds("acp-add").unwrap().right() <= gpui::px(width));
    }
    preview.update(cx, |view, cx| {
        view.external_agents.has_project = false;
        cx.notify();
    });
    click(cx, "acp-scope-project");
    click(cx, "acp-toggle-project-codex");
    assert!(!preview.read_with(cx, |view, _| view
        .external_agents
        .rows
        .iter()
        .any(|row| row.id == "codex" && !row.global && row.enabled)));
    let count = preview.read_with(cx, |view, _| view.external_agents.rows.len());
    click(cx, "acp-add");
    assert_eq!(
        count,
        preview.read_with(cx, |view, _| view.external_agents.rows.len())
    );
    click(cx, "acp-scope-global");
    click(cx, "acp-add");
    assert_eq!(
        count + 1,
        preview.read_with(cx, |view, _| view.external_agents.rows.len())
    );
}
