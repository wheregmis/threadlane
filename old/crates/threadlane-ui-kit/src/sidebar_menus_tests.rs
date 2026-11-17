use crate::{
    sidebar_session_menu, SidebarSessionAction, SidebarSessionMenuScope, SidebarSessionMenuState,
    SidebarSnoozeMenu,
};
use gpui::{
    div, App, AppContext, Context, IntoElement, Modifiers, ParentElement, Render, Styled,
    TestAppContext, Window,
};
use gpui_component::menu::DropdownMenu;
use std::{cell::RefCell, rc::Rc};

struct MenuHost {
    scope: SidebarSessionMenuScope,
    snooze: SidebarSnoozeMenu,
    actions: Rc<RefCell<Vec<SidebarSessionAction>>>,
}
impl Render for MenuHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let scope = self.scope;
        let snooze = self.snooze.clone();
        let actions = self.actions.clone();
        div().w_64().child(
            crate::session_actions_button("fixture", true).dropdown_menu(
                move |menu, window, cx| {
                    let actions = actions.clone();
                    sidebar_session_menu(
                        menu,
                        SidebarSessionMenuState::new(snooze.clone())
                            .title_loading(true)
                            .terminal_available(false),
                        scope,
                        move |action, _, _: &mut App| actions.borrow_mut().push(action),
                        window,
                        cx,
                    )
                },
            ),
        )
    }
}

fn activate(
    cx: &mut TestAppContext,
    scope: SidebarSessionMenuScope,
    snooze: SidebarSnoozeMenu,
    keys: &str,
) -> Vec<SidebarSessionAction> {
    cx.update(gpui_component::init);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let output = actions.clone();
    let (_, cx) = cx.add_window_view(|window, cx| {
        gpui_component::Root::new(
            cx.new(|_| MenuHost {
                scope,
                snooze,
                actions,
            }),
            window,
            cx,
        )
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let trigger = cx.debug_bounds("session-actions-fixture").unwrap();
    cx.simulate_click(trigger.center(), Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    for key in keys.split_whitespace() {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
    let result = output.borrow().clone();
    result
}

#[gpui::test]
fn sidebar_menu_failed_snooze_can_retry_from_keyboard(cx: &mut TestAppContext) {
    assert_eq!(
        activate(
            cx,
            SidebarSessionMenuScope::Quick,
            SidebarSnoozeMenu::Status(crate::SidebarSnoozeStatus::SaveFailed),
            "down down down enter"
        ),
        vec![SidebarSessionAction::RetrySnooze]
    );
}

#[gpui::test]
fn sidebar_menu_pending_snooze_can_be_cancelled(cx: &mut TestAppContext) {
    assert_eq!(
        activate(
            cx,
            SidebarSessionMenuScope::Quick,
            SidebarSnoozeMenu::Status(crate::SidebarSnoozeStatus::Saving),
            "down down down enter"
        ),
        vec![SidebarSessionAction::Unsnooze]
    );
}

#[gpui::test]
fn sidebar_menu_full_skips_disabled_actions_and_opens_copy_submenu(cx: &mut TestAppContext) {
    assert_eq!(
        activate(
            cx,
            SidebarSessionMenuScope::Full,
            SidebarSnoozeMenu::Unavailable("Session running".into()),
            "down down down down down right enter"
        ),
        vec![SidebarSessionAction::CopyId]
    );
}

#[gpui::test]
fn sidebar_menu_snooze_dispatches_duration_from_host_choice(cx: &mut TestAppContext) {
    assert_eq!(
        activate(
            cx,
            SidebarSessionMenuScope::Quick,
            SidebarSnoozeMenu::Available(vec![crate::SidebarSnoozeChoice::new("For 1 hour", 3600)
                .with_return_label("Tomorrow, 9:00 AM")]),
            "down down down right down enter",
        ),
        vec![SidebarSessionAction::Snooze(3600)]
    );
}
