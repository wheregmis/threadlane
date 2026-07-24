//! SessionContextMenu widget and menu popup component.

use crate::panels::sessions::state::set_session_context_target;
use makepad_widgets::*;

#[derive(Clone, Copy, Debug, Default)]
pub enum SessionContextMenuAction {
    Archive,
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
                cx.widget_action(self.widget_uid(), SessionContextMenuAction::Archive);
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

        match event {
            Event::MouseDown(event)
                if event.button.is_primary() && !self.menu_rect.contains(event.abs) =>
            {
                self.close(cx)
            }
            Event::KeyDown(event) if event.key_code == KeyCode::Escape => self.close(cx),
            Event::BackPressed { .. } => self.close(cx),
            _ => {}
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
                color: #x20252d
                border_color: #x3c4654
                border_size: 1.0
                border_radius: 9.0
            }

            archive_session_btn := Button {
                width: Fill
                height: 28
                margin: 0
                text: "Archive Session"
                align: Align{x: 0.0 y: 0.5}
                padding: Inset{left: 11 right: 10}
                draw_bg +: {
                    color: #x00000000
                    color_hover: #x2c3541
                    color_focus: #x2c3541
                    color_down: #x344150
                    border_color: #x00000000
                    border_color_hover: #x00000000
                    border_color_focus: #x00000000
                    border_color_down: #x00000000
                    border_size: 0.0
                    border_radius: 5.0
                }
                draw_text +: {
                    color: #xd6dce5
                    color_hover: #xf4f7fb
                    color_focus: #xf4f7fb
                    color_down: #xffffff
                    text_style +: { font_size: 9.5 }
                }
            }

            delete_session_btn := Button {
                width: Fill
                height: 28
                margin: 0
                text: "Delete Session"
                align: Align{x: 0.0 y: 0.5}
                padding: Inset{left: 11 right: 10}
                draw_bg +: {
                    color: #x00000000
                    color_hover: #x3b2b31
                    color_focus: #x3b2b31
                    color_down: #x4a2e36
                    border_color: #x00000000
                    border_color_hover: #x00000000
                    border_color_focus: #x00000000
                    border_color_down: #x00000000
                    border_size: 0.0
                    border_radius: 5.0
                }
                draw_text +: {
                    color: #xe67f87
                    color_hover: #xffa0a7
                    color_focus: #xffa0a7
                    color_down: #xffffff
                    text_style +: { font_size: 9.5 }
                }
            }
        }
    }
}
