use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext};
use std::{cell::RefCell, rc::Rc};
use threadlane_ui_kit::settings::{SettingsPage, SettingsProviderStatusKind};

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

#[gpui::test]
fn provider_controls_select_accounts_mask_keys_and_fit(cx: &mut TestAppContext) {
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
    cx.simulate_resize(gpui::size(gpui::px(1100.0), gpui::px(2600.0)));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    click(cx, "settings-providers");
    click(cx, "provider-account-active-sample-work");
    assert!(preview.read_with(cx, |view, _| view
        .providers
        .accounts
        .iter()
        .any(|row| row.id == "sample-work" && row.active)));
    cx.update(|window, cx| {
        window.blur(cx);
        // Back, all navigation destinations, Test, Add account, then Set active.
        for _ in 0..SettingsPage::ALL.len() + 4 {
            window.focus_next(cx);
        }
        window.draw(cx).clear(cx);
    });
    super::tests::activate_key(cx, "enter");
    assert!(preview.read_with(cx, |view, _| view
        .providers
        .accounts
        .iter()
        .any(|row| row.id == "sample-personal" && row.active)));
    click(cx, "provider-account-remove-sample-personal");
    assert!(
        preview.read_with(cx, |view, _| view.providers.accounts.len() == 1
            && view.providers.accounts[0].active)
    );
    click(cx, "provider-add-chatgpt");
    assert_eq!(
        preview.read_with(cx, |view, _| view.providers.accounts.len()),
        2
    );
    click(cx, "provider-test-chatgpt");
    assert!(preview.read_with(cx, |view, _| view
        .providers
        .status
        .as_ref()
        .unwrap()
        .text
        .contains("passed")));
    click(cx, "provider-auth-antigravity");
    assert!(preview.read_with(cx, |view, _| view.providers.antigravity_connected));
    click(cx, "provider-auth-antigravity");
    assert!(!preview.read_with(cx, |view, _| view.providers.antigravity_connected));
    click(cx, "provider-key-save-github");
    assert!(preview.read_with(cx, |view, _| view.providers.github_status.is_none()));
    cx.update(|window, cx| {
        preview.update(cx, |view, cx| {
            view.github_key.update(cx, |input, cx| {
                input.set_value("sample-github-token", window, cx)
            });
        })
    });
    click(cx, "provider-key-save-github");
    assert!(preview.read_with(cx, |view, _| view.providers.github_status.is_some()));
    click(cx, "provider-key-test-opencode");
    assert!(preview.read_with(cx, |view, _| matches!(
        view.providers.status.as_ref().unwrap().kind,
        SettingsProviderStatusKind::Error
    )));
    click(cx, "provider-key-test-openai");
    assert!(preview.read_with(cx, |view, _| {
        let status = view.providers.status.as_ref().unwrap();
        matches!(status.kind, SettingsProviderStatusKind::Success)
            && !status.text.contains("sample-openai-key")
    }));
    click(cx, "provider-auth-gitlab");
    assert!(preview.read_with(cx, |view, _| view.providers.gitlab_status.is_none()));
    // A disconnected GitLab has no sign-in flow here, so it offers no action.
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("provider-auth-gitlab").is_none());

    for (width, font) in [(480.0, 14.0), (800.0, 16.0), (1100.0, 20.0)] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(3000.0)));
        cx.update(|_, cx| gpui_component::Theme::global_mut(cx).font_size = gpui::px(font));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let heading = cx.debug_bounds("settings-page-heading").unwrap();
        let panel = cx.debug_bounds("settings-provider-panel").unwrap();
        assert_eq!(panel.left(), heading.left());
        for selector in [
            "provider-connection-chatgpt",
            "provider-connection-antigravity",
            "provider-key-github",
            "provider-key-openai",
            "provider-key-opencode",
        ] {
            let row = cx.debug_bounds(selector).unwrap();
            assert!(row.left() > panel.left() && row.right() < panel.right());
            assert!(row.right() <= gpui::px(width));
        }
        for selector in [
            "provider-key-save-github",
            "provider-key-test-openai",
            "provider-key-save-openai",
            "provider-key-test-opencode",
            "provider-key-save-opencode",
        ] {
            let button = cx.debug_bounds(selector).unwrap();
            assert!(button.left() > panel.left() && button.right() < panel.right());
        }
        let account = cx.debug_bounds("provider-account-sample-work").unwrap();
        let remove = cx
            .debug_bounds("provider-account-remove-sample-work")
            .unwrap();
        assert!(account.left() > panel.left() && account.right() < panel.right());
        assert!(remove.left() > account.left() && remove.right() < account.right());
    }
}
