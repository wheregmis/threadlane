//! SessionContextMenu widget and menu popup component.

use super::overlay_popup_base::is_overlay_dismissal_event;
use crate::panels::sessions::state::set_session_context_target;
use makepad_widgets::*;

#[derive(Clone, Copy, Debug, Default)]
pub enum SessionContextMenuAction {
    /// Completes a session and moves its persisted history out of the active sidebar.
    Settle,
    Delete,
    #[default]
    None,
}

#[derive(Script, Widget)]
pub struct SessionContextMenu {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    draw_list: Option<DrawList2d>,
    #[rust]
    opened: bool,
    #[rust]
    menu_pos: Vec2d,
    #[rust]
    menu_rect: Rect,
}

impl ScriptHook for SessionContextMenu {
    fn on_after_new(&mut self, vm: &mut ScriptVm) {
        self.draw_list = Some(DrawList2d::script_new(vm));
    }

    fn on_after_apply(
        &mut self,
        vm: &mut ScriptVm,
        _apply: &Apply,
        _scope: &mut Scope,
        _value: ScriptValue,
    ) {
        vm.with_cx_mut(|cx| {
            if let Some(draw_list) = &self.draw_list {
                draw_list.redraw(cx);
            }
        });
    }
}

impl Widget for SessionContextMenu {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.opened {
            return;
        }

        self.view.handle_event(cx, event, scope);

        if let Event::Actions(actions) = event {
            if self
                .view
                .button(cx, ids!(archive_session_btn))
                .clicked(actions)
            {
                cx.widget_action(self.widget_uid(), SessionContextMenuAction::Settle);
                self.close(cx);
                return;
            }
            if self
                .view
                .button(cx, ids!(delete_session_btn))
                .clicked(actions)
            {
                cx.widget_action(self.widget_uid(), SessionContextMenuAction::Delete);
                self.close(cx);
                return;
            }
        }

        if is_overlay_dismissal_event(event, self.menu_rect)
            || matches!(event, Event::BackPressed { .. })
        {
            self.close(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, _walk: Walk) -> DrawStep {
        let draw_list = self.draw_list.as_mut().unwrap();
        draw_list.begin_overlay_reuse(cx);

        let pass_size = cx.current_pass_size();
        cx.begin_root_turtle(pass_size, Layout::flow_down());

        if self.opened {
            const MENU_WIDTH: f64 = 168.0;
            const MENU_HEIGHT: f64 = 64.0;
            const EDGE_GAP: f64 = 6.0;
            const POINTER_GAP: f64 = 2.0;

            let max_x = (pass_size.x - MENU_WIDTH - EDGE_GAP).max(EDGE_GAP);
            let x = (self.menu_pos.x + POINTER_GAP).clamp(EDGE_GAP, max_x);
            let below_y = self.menu_pos.y + POINTER_GAP;
            let y = if below_y + MENU_HEIGHT > pass_size.y - EDGE_GAP {
                self.menu_pos.y - MENU_HEIGHT - POINTER_GAP
            } else {
                below_y
            }
            .clamp(
                EDGE_GAP,
                (pass_size.y - MENU_HEIGHT - EDGE_GAP).max(EDGE_GAP),
            );

            self.menu_rect = Rect {
                pos: dvec2(x, y),
                size: dvec2(MENU_WIDTH, MENU_HEIGHT),
            };
            let walk = self.view.walk(cx).with_abs_pos(self.menu_rect.pos);
            self.view.draw_walk_all(cx, scope, walk);
        }

        cx.end_pass_sized_turtle();
        draw_list.end(cx);
        DrawStep::done()
    }
}

impl SessionContextMenu {
    pub fn open(&mut self, cx: &mut Cx, position: Vec2d) {
        self.menu_pos = position;
        self.opened = true;
        if let Some(draw_list) = &self.draw_list {
            draw_list.redraw(cx);
        }
    }

    pub fn close(&mut self, cx: &mut Cx) {
        self.opened = false;
        set_session_context_target(None);
        if let Some(draw_list) = &self.draw_list {
            draw_list.redraw(cx);
        }
        cx.redraw_all();
    }
}

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ContextMenuItem = Button {
        width: Fill
        height: 28
        margin: 0
        align: Align{x: 0.0 y: 0.5}
        padding: Inset{left: 11 right: 10}
        draw_bg +: {
            color: theme.color_transparent
            color_hover: theme.color_accent
            color_focus: theme.color_accent
            color_down: theme.color_secondary
            border_color: theme.color_transparent
            border_color_hover: theme.color_transparent
            border_color_focus: theme.color_transparent
            border_color_down: theme.color_transparent
            border_size: 0.0
            border_radius: theme.radius_xs
        }
        draw_text +: {
            color: theme.color_foreground
            color_hover: theme.color_accent_foreground
            color_focus: theme.color_accent_foreground
            color_down: theme.color_accent_foreground
            text_style +: { font_size: 9.5 }
        }
    }

    mod.components.SessionContextMenu = #(SessionContextMenu::register_widget(vm)) {
        width: 168
        height: Fit
        flow: Down

        menu_surface := RoundedView {
            width: Fill
            height: Fit
            flow: Down
            new_batch: true
            padding: Inset{left: 4 top: 4 right: 4 bottom: 4}
            draw_bg +: {
                color: theme.color_popover
                border_color: theme.color_border
                border_size: 1.0
                border_radius: theme.radius_md
            }

            archive_session_btn := mod.components.ContextMenuItem {
                text: "Settle Session"
            }

            delete_session_btn := mod.components.ContextMenuItem {
                text: "Delete Session"
                draw_bg +: {
                    color_hover: theme.color_accent
                    color_focus: theme.color_accent
                    color_down: theme.color_accent
                }
                draw_text +: {
                    color: theme.color_destructive
                    color_hover: theme.color_destructive
                    color_focus: theme.color_destructive
                }
            }
        }
    }
}
