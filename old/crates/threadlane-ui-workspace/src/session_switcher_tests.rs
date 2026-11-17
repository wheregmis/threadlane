//! Mounted host tests omit service pumps: confirming only queues normal hydration.
use super::{SessionNavigation, WorkspaceView};
use gpui::{div, App, AppContext, Context, Entity, IntoElement, Render, TestAppContext, Window};
use gpui_component::{command::CommandState, resizable::ResizableState, Root};

struct PickerHost(Option<Entity<WorkspaceView>>);
impl Render for PickerHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(view) = &self.0 else {
            return div().into_any_element();
        };
        view.update(cx, |view, cx| {
            if view.command_palette_open {
                view.render_session_picker(cx)
            } else {
                div().into_any_element()
            }
        })
    }
}

pub(crate) fn workspace(
    state: threadlane_ui_state::AppState,
    window: &mut Window,
    cx: &mut App,
) -> Entity<WorkspaceView> {
    let model = cx.new(|_| state);
    let sidebar = cx.new(|cx| threadlane_ui_sidebar::SidebarView::new(model.clone(), window, cx));
    let chat_list = cx.new(|cx| threadlane_ui_chat::ChatListView::new(model.clone(), window, cx));
    let github = cx.new(|cx| threadlane_ui_github::GitHubView::new(model.clone(), window, cx));
    let automations =
        cx.new(|cx| threadlane_ui_automation::AutomationsView::new(model.clone(), window, cx));
    let settings =
        cx.new(|cx| threadlane_ui_settings::SettingsView::new(model.clone(), window, cx));
    let right_panel =
        cx.new(|cx| threadlane_ui_right_panel::RightPanelView::new(model.clone(), window, cx));
    let command_state = cx.new(|cx| CommandState::new(window, cx));
    cx.new(|cx| {
        let sub = cx.observe(&model, |view: &mut WorkspaceView, model, cx| {
            view.session_navigation.observe(model.read(cx));
            cx.notify();
        });
        let command_state_subscription = cx.observe(&command_state, |_, _, cx| cx.notify());
        WorkspaceView {
            pending_completion_activation: None,
            completion_activation_task: None,
            window_handle: window.window_handle(),
            last_link_terminal: None,
            focus_handle: cx.focus_handle(),
            rendered_page: threadlane_ui_state::WorkspacePage::Chat,
            session_navigation: SessionNavigation::new(model.read(cx)),
            model,
            sidebar,
            chat_list,
            github,
            automations,
            settings,
            right_panel,
            fallback_terminal: None,
            terminal_groups: Default::default(),
            sidebar_collapsed: false,
            right_panel_visible: false,
            bottom_panel_visible: false,
            terminal_maximized: false,
            command_palette_open: false,
            command_palette_previous_focus: None,
            command_state,
            command_state_subscription,
            conversation_search: None,
            session_picker: None,
            recent_palette_actions: vec![],
            last_git_work_dir: None,
            last_git_pr_targets: Default::default(),
            sidebar_resizable_state: cx.new(|_| ResizableState::default()),
            right_panel_resizable_state: cx.new(|_| ResizableState::default()),
            bottom_panel_resizable_state: cx.new(|_| ResizableState::default()),
            preferred_panel_sizes: [16.5, 22., 14.],
            panel_layout: None,
            git_event_tx: tokio::sync::mpsc::unbounded_channel().0,
            updater_tx: tokio::sync::mpsc::unbounded_channel().0,
            pending_terminal_close: None,
            terminal_subscriptions: vec![],
            _subscriptions: vec![sub],
        }
    })
}

#[gpui::test]
fn switch_session_enter_cancel_and_stale_target_use_normal_selection(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let mut state = super::tests::state();
    // Remote selection queues hydration without local filesystem/registry access.
    state.daemon_remote = true;
    state.active_session_id = Some("a".into());
    state.pending_permissions.insert("d".into(), threadlane_protocol::PermissionRequest {
        id: "background-permission".into(), capability: "network".into(),
        title: "Allow network".into(), detail: String::new(), scopes: vec![],
    });
    state.pending_questions.insert("d".into(), threadlane_protocol::QuestionRequest {
        id: "background-question".into(), questions: vec![],
    });
    let project = state.projects[0].clone();
    state.drain_chat_stream(vec![
        threadlane_protocol::daemon::SessionEvent::ProjectChanged { project },
    ]);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let host = cx.new(|_| PickerHost(None));
        Root::new(host, window, cx)
    });
    let view = cx.update(|window, cx| workspace(state, window, cx));
    // Install a host after the Root exists, before constructors can notify.
    cx.update(|window, cx| {
        let host = cx.new(|cx| {
            cx.observe(&view, |_, _, cx| cx.notify()).detach();
            PickerHost(Some(view.clone()))
        });
        root.update(cx, |root, cx| *root = Root::new(host, window, cx));
    });
    let model = view.read_with(cx, |view, _| view.model.clone());
    for id in ["b", "c"] {
        model.update(cx, |state, cx| {
            state.active_session_id = Some(id.into());
            cx.notify();
        });
        cx.run_until_parked();
    }
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.switch_session_action(&super::super::SwitchSession, window, cx)
        })
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(
        model
            .read_with(cx, |state, _| state.active_session_id.clone())
            .as_deref(),
        Some("c")
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        model
            .read_with(cx, |state, _| state.active_session_id.clone())
            .as_deref(),
        Some("b")
    );
    assert!(!view.read_with(cx, |view, _| view.command_palette_open));
    assert_eq!(
        model.read_with(cx, |state, _| state.pending_hydrations.len()),
        1
    );
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.switch_session_action(&super::super::SwitchSession, window, cx)
        })
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(
        view.read_with(cx, |view, _| view.session_picker.as_ref().unwrap().recent
            [0]
        .session
        .session_id
        .clone()),
        "c"
    );
    let prior_focus = view.read_with(cx, |view, _| view.command_palette_previous_focus.clone().unwrap());
    cx.simulate_input("a query");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!view.read_with(cx, |view, _| view.command_palette_open));
    cx.update(|window, _| assert!(prior_focus.is_focused(window)));
    assert_eq!(
        model
            .read_with(cx, |state, _| state.active_session_id.clone())
            .as_deref(),
        Some("b")
    );
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.switch_session_action(&super::super::SwitchSession, window, cx)
        })
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    model.update(cx, |state, cx| {
        state.projects[0]
            .sessions
            .retain(|session| session.id != "c");
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.command_palette_open));
    assert_eq!(
        view.read_with(cx, |view, _| view.session_picker.as_ref().unwrap().error),
        Some("This session is no longer available")
    );
    assert_eq!(
        model
            .read_with(cx, |state, _| state.active_session_id.clone())
            .as_deref(),
        Some("b")
    );
}
