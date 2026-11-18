use crate::{agent_worktree_controls, agent_worktree_discard_dialog, AgentWorktreeAction};
use gpui::{
    div, px, size, AppContext, Context, IntoElement, Modifiers, ParentElement, Render, Styled,
    TestAppContext, Window,
};
use gpui_component::WindowExt;
use std::{cell::RefCell, rc::Rc};
use threadlane_protocol::{daemon::SubagentActivityStatus, events::SubagentIsolation};

struct ControlsHost {
    status: SubagentActivityStatus,
    available: bool,
    confirm: bool,
    actions: Rc<RefCell<Vec<AgentWorktreeAction>>>,
}
impl Render for ControlsHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let isolation = SubagentIsolation {
            branch: "agents/a-long-branch-name-that-needs-to-fit-a-narrow-panel".into(),
            workspace: "/sample/project/.threadlane/worktrees/agent".into(),
        };
        let target = isolation.clone();
        let output = self.actions.clone();
        let confirm = self.confirm;
        div().w_full().child(agent_worktree_controls(
            "fixture",
            &isolation,
            self.status,
            self.available,
            move |action, window, cx| {
                if confirm && action == AgentWorktreeAction::Discard {
                    let output = output.clone();
                    let target = target.clone();
                    window.open_alert_dialog(cx, move |alert, _, _| {
                        let output = output.clone();
                        agent_worktree_discard_dialog(
                            alert,
                            &target,
                            std::path::Path::new("/sample/project"),
                        )
                        .on_ok(move |_, _, _| {
                            output.borrow_mut().push(AgentWorktreeAction::Discard);
                            true
                        })
                    });
                } else {
                    output.borrow_mut().push(action);
                }
            },
            cx,
        ))
    }
}

#[gpui::test]
fn agent_worktree_controls_fit_and_only_dispatch_enabled_commands(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for (status, available, expected) in [
        (
            SubagentActivityStatus::Running,
            true,
            vec![AgentWorktreeAction::Inspect, AgentWorktreeAction::Terminal],
        ),
        (
            SubagentActivityStatus::Failed,
            false,
            vec![AgentWorktreeAction::Inspect, AgentWorktreeAction::Discard],
        ),
        (
            SubagentActivityStatus::Completed,
            true,
            vec![
                AgentWorktreeAction::Inspect,
                AgentWorktreeAction::Terminal,
                AgentWorktreeAction::Apply,
                AgentWorktreeAction::Discard,
            ],
        ),
    ] {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let output = actions.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            gpui_component::Root::new(
                cx.new(|_| ControlsHost {
                    status,
                    available,
                    confirm: false,
                    actions,
                }),
                window,
                cx,
            )
        });
        for font in [13.0, 20.0] {
            cx.update(|window, cx| {
                gpui_component::Theme::global_mut(cx).font_size = px(font);
                gpui_component::Theme::sync_base(cx);
                window.refresh();
            });
            for width in [280.0, 440.0] {
                cx.simulate_resize(size(px(width), px(600.0)));
                cx.run_until_parked();
                cx.update(|window, cx| window.draw(cx).clear(cx));
                let branch = cx.debug_bounds("agent-branch-fixture").unwrap();
                let controls = cx.debug_bounds("agent-worktree-controls").unwrap();
                assert!(
                    branch.left() >= controls.left() && branch.right() <= controls.right(),
                    "long branch must stay within controls"
                );
                output.borrow_mut().clear();
                for selector in [
                    "agent-inspect-fixture",
                    "agent-terminal-fixture",
                    "agent-apply-fixture",
                    "agent-discard-fixture",
                ] {
                    let bounds = cx.debug_bounds(selector).unwrap();
                    assert!(
                        bounds.left() >= controls.left()
                            && bounds.right() <= controls.right()
                            && bounds.bottom() <= controls.bottom(),
                        "wrapped control must fit at {font}/{width}"
                    );
                    cx.simulate_click(bounds.center(), Modifiers::default());
                    cx.run_until_parked();
                }
                assert_eq!(
                    *output.borrow(),
                    expected,
                    "disabled commands must never dispatch"
                );
            }
        }
    }
}

#[gpui::test]
fn agent_worktree_discard_escape_cancels_and_confirmation_dispatches_once(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let output = actions.clone();
    let (_, cx) = cx.add_window_view(|window, cx| {
        gpui_component::Root::new(
            cx.new(|_| ControlsHost {
                status: SubagentActivityStatus::Completed,
                available: true,
                confirm: true,
                actions,
            }),
            window,
            cx,
        )
    });
    for confirm in [false, true] {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = cx.debug_bounds("agent-discard-fixture").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        if confirm {
            cx.update(|window, cx| {
                window.dispatch_action(
                    Box::new(gpui_component::dialog::Confirm { secondary: false }),
                    cx,
                )
            });
        } else {
            cx.simulate_keystrokes("escape");
        }
        cx.run_until_parked();
        assert_eq!(output.borrow().len(), usize::from(confirm));
    }
    assert_eq!(*output.borrow(), vec![AgentWorktreeAction::Discard]);
}
