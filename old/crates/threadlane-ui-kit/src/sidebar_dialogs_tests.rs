use crate::{sidebar_session_removal_dialog, SidebarSessionRemoval, SidebarSessionRemovalTarget};
use gpui::{
    div, AppContext, Context, InteractiveElement, IntoElement, Modifiers, ParentElement, Render,
    TestAppContext, Window,
};
use gpui_component::button::Button;
use gpui_component::WindowExt;
use std::{cell::RefCell, rc::Rc};

struct DialogHost {
    delete_worktree: bool,
    requests: Rc<RefCell<Vec<bool>>>,
}
impl Render for DialogHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().child(
            Button::new("open-removal")
                .debug_selector(|| "open-removal".into())
                .label("Remove")
                .on_click(cx.listener(|_, _, window, cx| {
                    let owner = cx.entity().downgrade();
                    window.open_alert_dialog(cx, move |alert, _, cx| {
                        let value = owner.upgrade().unwrap().read(cx).delete_worktree;
                        let target = SidebarSessionRemovalTarget::new(
                            "fixture",
                            "Captured session",
                            "Project",
                        )
                        .worktree(Some("feature/example".into()));
                        let toggle = owner.clone();
                        let confirm = owner.clone();
                        sidebar_session_removal_dialog(
                            alert,
                            SidebarSessionRemoval::Remove,
                            &target,
                            value,
                            move |checked, _, cx| {
                                let _ = toggle.update(cx, |host, cx| {
                                    host.delete_worktree = checked;
                                    cx.notify();
                                });
                            },
                        )
                        .on_ok(move |_, _, cx| {
                            let _ = confirm.update(cx, |host, _| {
                                host.requests.borrow_mut().push(host.delete_worktree)
                            });
                            true
                        })
                    });
                })),
        )
    }
}

#[gpui::test]
fn sidebar_removal_escape_cancels_and_worktree_choice_reaches_confirmation(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let requests = Rc::new(RefCell::new(Vec::new()));
    let output = requests.clone();
    let (_, cx) = cx.add_window_view(|window, cx| {
        gpui_component::Root::new(
            cx.new(|_| DialogHost {
                delete_worktree: true,
                requests,
            }),
            window,
            cx,
        )
    });
    for confirm in [false, true] {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let trigger = cx.debug_bounds("open-removal").unwrap();
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let checkbox = cx.debug_bounds("remove-delete-worktree-fixture").unwrap();
        if confirm {
            cx.simulate_click(checkbox.center(), Modifiers::default());
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
                window.dispatch_action(
                    Box::new(gpui_component::dialog::Confirm { secondary: false }),
                    cx,
                );
            });
        } else {
            cx.simulate_keystrokes("escape");
            assert!(output.borrow().is_empty());
        }
    }
    assert_eq!(
        *output.borrow(),
        vec![false],
        "keeping the worktree must be passed to the host exactly once"
    );
}

#[test]
fn sidebar_removal_copy_distinguishes_archive_and_permanent_removal() {
    let target = SidebarSessionRemovalTarget::new("id", "Session", "Project");
    assert!(target
        .description(SidebarSessionRemoval::Archive, true)
        .contains("transcript stays in the archive"));
    let removal = target.description(SidebarSessionRemoval::Remove, true);
    assert!(removal.contains("permanently deleted"));
    assert!(!removal.contains("worktree"));
    let target = target.worktree(Some("feature/example".into()));
    assert!(target
        .description(SidebarSessionRemoval::Archive, true)
        .contains("will be deleted too"));
    assert!(target
        .description(SidebarSessionRemoval::Remove, false)
        .contains("feature/example' will be kept"));
}
