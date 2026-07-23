//! Session-specific widgets and DSL components.

use super::state::set_session_context_target;
use makepad_widgets::*;

#[derive(Clone, Copy, Debug, Default)]
pub enum SessionContextMenuAction {
    Archive,
    Delete,
    #[default]
    None,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ContextMenuItem {
    Archive,
    Delete,
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
    #[rust]
    hovered_item: Option<ContextMenuItem>,
    #[rust]
    pressed_item: Option<ContextMenuItem>,
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

        match event {
            Event::MouseMove(event) => {
                let item = self.item_at(event.abs);
                self.set_hovered_item(cx, item);
                if item.is_some() {
                    cx.set_cursor(MouseCursor::Hand);
                }
            }
            Event::MouseDown(event) if event.button.is_primary() => {
                self.pressed_item = self.item_at(event.abs);
                if let Some(item) = self.pressed_item {
                    self.set_pressed_item(cx, item);
                } else {
                    self.close(cx);
                }
            }
            Event::MouseUp(event) if event.button.is_primary() => {
                let released_item = self.item_at(event.abs);
                let selected_item = self
                    .pressed_item
                    .take()
                    .filter(|pressed| Some(*pressed) == released_item);
                if let Some(item) = selected_item {
                    cx.widget_action(
                        self.widget_uid(),
                        match item {
                            ContextMenuItem::Archive => SessionContextMenuAction::Archive,
                            ContextMenuItem::Delete => SessionContextMenuAction::Delete,
                        },
                    );
                    self.close(cx);
                } else {
                    self.set_hovered_item(cx, released_item);
                }
            }
            Event::KeyDown(event) if event.key_code == KeyCode::Escape => self.close(cx),
            Event::BackPressed { .. } => self.close(cx),
            Event::MouseLeave(_) => self.set_hovered_item(cx, None),
            _ => self.view.handle_event(cx, event, scope),
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, _walk: Walk) -> DrawStep {
        let draw_list = self.draw_list.as_mut().unwrap();
        draw_list.begin_overlay_reuse(cx);

        let pass_size = cx.current_pass_size();
        cx.begin_root_turtle(pass_size, Layout::flow_down());

        if self.opened {
            const MENU_WIDTH: f64 = 156.0;
            const MENU_HEIGHT: f64 = 57.0;
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
    fn item_at(&self, position: Vec2d) -> Option<ContextMenuItem> {
        if !self.menu_rect.contains(position) {
            return None;
        }
        let local_y = position.y - self.menu_rect.pos.y;
        if local_y < 28.5 {
            Some(ContextMenuItem::Archive)
        } else if local_y >= 28.5 {
            Some(ContextMenuItem::Delete)
        } else {
            None
        }
    }

    fn set_hovered_item(&mut self, cx: &mut Cx, item: Option<ContextMenuItem>) {
        if self.hovered_item == item {
            return;
        }
        self.hovered_item = item;
        self.set_menu_item_state(
            cx,
            match item {
                Some(ContextMenuItem::Archive) => 1.0,
                Some(ContextMenuItem::Delete) => 2.0,
                None => 0.0,
            },
        );
    }

    fn set_pressed_item(&mut self, cx: &mut Cx, item: ContextMenuItem) {
        self.hovered_item = Some(item);
        self.set_menu_item_state(
            cx,
            match item {
                ContextMenuItem::Archive => 3.0,
                ContextMenuItem::Delete => 4.0,
            },
        );
    }

    fn set_menu_item_state(&mut self, cx: &mut Cx, item_state: f64) {
        let mut surface = self.view.widget(cx, ids!(menu_surface));
        script_apply_eval!(cx, surface, {
            draw_bg +: { item_state: #(item_state) }
        });
        surface.redraw(cx);
        if let Some(draw_list) = &self.draw_list {
            draw_list.redraw(cx);
        }
    }

    pub fn open(&mut self, cx: &mut Cx, position: Vec2d) {
        self.menu_pos = position;
        self.opened = true;
        self.hovered_item = None;
        self.pressed_item = None;
        self.set_menu_item_state(cx, 0.0);
        if let Some(draw_list) = &self.draw_list {
            draw_list.redraw(cx);
        }
    }

    pub fn close(&mut self, cx: &mut Cx) {
        self.opened = false;
        self.pressed_item = None;
        self.set_hovered_item(cx, None);
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
        width: 156
        height: Fit
        flow: Down

        menu_surface := RoundedView {
            width: Fill
            height: Fit
            flow: Down
            new_batch: true
            padding: Inset{left: 3 top: 3 right: 3 bottom: 3}
            draw_bg +: {
                item_state: instance(0.0)
                color: #x252a32
                border_color: #x434b58
                border_size: 1.0
                border_radius: 7.0
                archive_hover_color: uniform(#x343b46)
                archive_down_color: uniform(#x3b4451)
                delete_hover_color: uniform(#x402d33)
                delete_down_color: uniform(#x503138)

                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(
                        self.border_size
                        self.border_size
                        self.rect_size.x - self.border_size * 2.0
                        self.rect_size.y - self.border_size * 2.0
                        self.border_radius
                    )
                    sdf.fill_keep(self.color)
                    sdf.stroke(self.border_color, self.border_size)

                    if self.item_state > 0.5 {
                        let pressed = self.item_state > 2.5
                        let pressed_mix = if pressed {1.0} else {0.0}
                        let item = if pressed {
                            self.item_state - 2.0
                        } else {
                            self.item_state
                        }
                        let is_delete = item > 1.5
                        let item_y = if is_delete {30.0} else {3.0}
                        let hover_color = if is_delete {
                            self.delete_hover_color
                        } else {
                            self.archive_hover_color
                        }
                        let down_color = if is_delete {
                            self.delete_down_color
                        } else {
                            self.archive_down_color
                        }
                        sdf.box(3.0, item_y, self.rect_size.x - 6.0, 24.0, 4.0)
                        sdf.fill(mix(hover_color, down_color, pressed_mix))
                    }
                    return sdf.result
                }
            }

            archive_session_btn := Button {
                width: Fill
                height: 24
                text: "Archive Session"
                align: Align{x: 0.0 y: 0.5}
                padding: Inset{left: 9 right: 8}
                draw_bg +: {
                    color: #x00000000
                    color_hover: #x00000000
                    color_down: #x00000000
                    border_size: 0.0
                    border_radius: 4.0
                }
                draw_text +: {
                    color: #xd3d9e2
                    color_hover: #xffffff
                    color_down: #xffffff
                    text_style +: { font_size: 9.25 }
                }
            }

            View {
                width: Fill
                height: 1
                margin: Inset{left: 7 right: 7 top: 1 bottom: 1}
                show_bg: true
                draw_bg +: { color: #x353c47 }
            }

            delete_session_btn := Button {
                width: Fill
                height: 24
                text: "Delete Session"
                align: Align{x: 0.0 y: 0.5}
                padding: Inset{left: 9 right: 8}
                draw_bg +: {
                    color: #x00000000
                    color_hover: #x00000000
                    color_down: #x00000000
                    border_size: 0.0
                    border_radius: 4.0
                }
                draw_text +: {
                    color: #xe87982
                    color_hover: #xff9aa2
                    color_down: #xffffff
                    text_style +: { font_size: 9.25 }
                }
            }
        }
    }

    mod.components.SessionRowBase = RoundedView {
        width: Fill
        height: Fit
        cursor: MouseCursor.Hand
        flow: Right
        spacing: 7
        align: Align{y: 0.5}
        margin: Inset{left: 6 right: 6 top: 1 bottom: 1}
        padding: Inset{left: 12 top: 5 right: 9 bottom: 5}
        draw_bg +: {
            hover: instance(0.0)
            color: #x00000000
            color_hover: uniform(#x262c35)
            border_radius: 6.0

            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(
                    self.border_size
                    self.border_size
                    self.rect_size.x - self.border_size * 2.0
                    self.rect_size.y - self.border_size * 2.0
                    max(1.0 self.border_radius)
                )
                sdf.fill_keep(mix(self.color, self.color_hover, self.hover))
                if self.border_size > 0.0 {
                    sdf.stroke(self.border_color, self.border_size)
                }
                return sdf.result
            }
        }
        animator +: {
            hover: {
                default: @off
                off: AnimatorState {
                    from: {all: Forward {duration: 0.10}}
                    apply: {draw_bg: {hover: 0.0}}
                }
                on: AnimatorState {
                    from: {all: Forward {duration: 0.08}}
                    apply: {draw_bg: {hover: snap(1.0)}}
                }
            }
        }
        session_icon := Icon {
            width: 13
            height: 13
            icon_walk: Walk{width: 13 height: 13}
            draw_icon +: {
                svg: crate_resource("self:resources/icons/conversation.svg")
                color: #x657181
            }
        }
        title_lbl := Label {
            width: Fill
            height: Fit
            text: ""
            draw_text +: { color: #xa6afbc text_style +: { font_size: 9.75 } }
        }
        session_row_spinner := mod.components.ActivityLoader {
            width: 18
            height: 10
            visible: false
        }
        time_lbl := Label {
            width: Fit
            height: Fit
            text: ""
            draw_text +: { color: #x697483 text_style +: { font_size: 8.75 } }
        }
    }
}
