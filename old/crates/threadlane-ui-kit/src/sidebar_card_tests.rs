use crate::{
    sidebar_session_card, sidebar_session_menu, SidebarSessionAction, SidebarSessionCardState,
    SidebarSessionMenuScope, SidebarSessionMenuState, SidebarSnoozeMenu, SidebarSnoozeStatus,
};
use gpui::{
    div, px, AppContext, Context, IntoElement, Modifiers, ParentElement, Render, Styled,
    TestAppContext, Window,
};
use std::{cell::RefCell, rc::Rc};
use threadlane_protocol::daemon::{SessionAttention, SessionInfo};

struct CardHost {
    width: f32,
    selected: bool,
    actions: Rc<RefCell<Vec<SidebarSessionAction>>>,
}
impl Render for CardHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let actions = self.actions.clone();
        let menu_actions = self.actions.clone();
        let session = SessionInfo {
            id: "shared-card".into(),
            title: "A long saved-session title that must truncate".into(),
            ..Default::default()
        };
        div().w(px(self.width)).child(sidebar_session_card(
            &session,
            SidebarSessionCardState {
                show_project: true,
                project: "A long project name".into(),
                attention: SessionAttention::Ready,
                selected: self.selected,
                pinned: self.selected,
                unseen_result: true,
                snooze: Some(SidebarSnoozeStatus::Snoozed("Tomorrow, 11:59 PM".into())),
                git_status: None,
                pr: None,
                now: 100,
            },
            move |action, _, _| actions.borrow_mut().push(action),
            move |menu, window, cx| {
                let actions = menu_actions.clone();
                sidebar_session_menu(
                    menu,
                    SidebarSessionMenuState::new(SidebarSnoozeMenu::Unavailable("Fixture".into())),
                    SidebarSessionMenuScope::Quick,
                    move |action, _, _| actions.borrow_mut().push(action),
                    window,
                    cx,
                )
            },
            |menu, _, _| menu,
            cx,
        ))
    }
}

#[gpui::test]
fn sidebar_card_buttons_dispatch_once_and_long_signals_fit(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for width in [223.0, 352.0] {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let output = actions.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            gpui_component::Root::new(
                cx.new(|_| CardHost {
                    width,
                    selected: true,
                    actions,
                }),
                window,
                cx,
            )
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let snooze = cx.debug_bounds("session-snoozed-shared-card").unwrap();
        assert!(
            snooze.right() <= px(width),
            "the complete snooze badge must stay inside a narrow sidebar"
        );
        for (selector, action) in [
            ("session-title-shared-card", SidebarSessionAction::Open),
            ("pin-session-shared-card", SidebarSessionAction::TogglePin),
            ("settle-session-shared-card", SidebarSessionAction::Archive),
        ] {
            output.borrow_mut().clear();
            let bounds = cx.debug_bounds(selector).unwrap();
            cx.simulate_click(bounds.center(), Modifiers::default());
            assert_eq!(
                *output.borrow(),
                vec![action],
                "a control must not also activate the parent row"
            );
        }
    }
}

#[gpui::test]
fn sidebar_card_menu_does_not_open_chat_and_keeps_keyboard_actions(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for selected in [false, true] {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let output = actions.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            gpui_component::Root::new(
                cx.new(|_| CardHost {
                    width: 223.0,
                    selected,
                    actions,
                }),
                window,
                cx,
            )
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let trigger = cx.debug_bounds("session-actions-shared-card").unwrap();
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(
            output.borrow().is_empty(),
            "opening actions must not activate the chat"
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(
            output.borrow().is_empty(),
            "dismissing actions must not activate the chat"
        );
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.run_until_parked();
        for key in ["down", "down", "enter"] {
            cx.simulate_keystrokes(key);
            cx.run_until_parked();
        }
        assert_eq!(*output.borrow(), vec![SidebarSessionAction::TogglePin]);
    }
}
