//! SessionRowBase component for list row items.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.SessionTitle = #(SessionTitle::register_widget(vm)) {
        width: Fill
        height: 18
        flow: Right
        clip_x: true
        clip_y: false
        padding: 0
        title_lbl := Label {
            width: Fit
            height: 18
            text: ""
            draw_text +: { color: #xaab3c0 text_style +: { font_size: 9.5 } }
        }
    }

    mod.components.SessionRowBase = RoundedView {
        width: Fill
        height: 32
        cursor: MouseCursor.Hand
        flow: Right
        spacing: 8
        align: Align{y: 0.5}
        margin: Inset{left: 10 right: 4 top: 1 bottom: 1}
        padding: Inset{left: 20 top: 4 right: 9 bottom: 4}
        draw_bg +: {
            hover: instance(0.0)
            tree_last: instance(0.0)
            is_active: instance(0.0)
            color: #x00000000
            color_hover: uniform(#x00000000)
            tree_color: uniform(#x3b4552)
            hover_line_color: uniform(#x61748b)
            active_line_color: uniform(#x8fb9e8)
            border_radius: 7.0

            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let tree_x = 9.0
                let surface_x = 14.0
                let surface_left = surface_x + self.border_size
                let surface_width = max(
                    0.0
                    self.rect_size.x - surface_x - self.border_size * 2.0
                )
                sdf.box(
                    surface_left
                    self.border_size
                    surface_width
                    self.rect_size.y - self.border_size * 2.0
                    max(1.0 self.border_radius)
                )
                sdf.fill_keep(mix(self.color, self.color_hover, self.hover))
                sdf.stroke(self.border_color, self.border_size)

                let tree_mid = self.rect_size.y * 0.5
                let tree_height = mix(self.rect_size.y, tree_mid, self.tree_last)
                sdf.rect(tree_x, 0.0, 1.0, max(0.0, tree_height))
                sdf.fill(self.tree_color)
                sdf.rect(tree_x, tree_mid, surface_x - tree_x + 1.0, 1.0)
                sdf.fill(self.tree_color)
                let line_amount = max(self.hover, self.is_active)
                if line_amount > 0.0 {
                    let line_color = mix(
                        self.hover_line_color
                        self.active_line_color
                        self.is_active
                    )
                    let underline_padding = surface_x - tree_x
                    sdf.rect(
                        surface_left + underline_padding
                        max(0.0, self.rect_size.y - 2.5)
                        max(0.0, surface_width - underline_padding * 2.0)
                        2.0
                    )
                    sdf.fill(mix(#x00000000, line_color, line_amount))
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
            width: 14
            height: 14
            icon_walk: Walk{width: 14 height: 14}
            draw_icon +: {
                svg: crate_resource("self:resources/icons/conversation.svg")
                color: #x667386
            }
        }
        title_surface := mod.components.SessionTitle {}
        session_row_spinner := mod.components.ActivityLoader {
            width: 18
            height: 10
            visible: false
        }
        time_lbl := Label {
            width: Fit
            height: Fit
            text: ""
            draw_text +: { color: #x667180 text_style +: { font_size: 8.5 } }
        }
    }

    mod.components.ProjectHeaderBase = RoundedView {
        width: Fill
        height: 34
        cursor: MouseCursor.Hand
        flow: Right
        spacing: 8
        align: Align{y: 0.5}
        margin: Inset{left: 3 right: 3 top: 7 bottom: 2}
        padding: Inset{left: 8 top: 4 right: 4 bottom: 4}
        draw_bg +: {
            hover: instance(0.0)
            tree_color: uniform(#x3b4552)
            color: #x00000000
            color_hover: #x222831
            border_radius: 8.0

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
                sdf.stroke(self.border_color, self.border_size)

                let tree_x = 16.0
                let tree_start = self.rect_size.y * 0.5 + 8.0
                sdf.rect(tree_x, tree_start, 1.0, max(0.0, self.rect_size.y - tree_start))
                sdf.fill(self.tree_color)
                return sdf.result
            }
        }
        animator +: {
            hover: {
                default: @off
                off: AnimatorState {
                    apply: {draw_bg: {hover: 0.0}}
                }
                on: AnimatorState {
                    apply: {draw_bg: {hover: 1.0}}
                }
            }
        }
        folder_icon := Icon {
            width: 16
            height: 16
            icon_walk: Walk{width: 16 height: 16}
            draw_icon +: {
                svg: crate_resource("self:resources/icons/folder.svg")
                color: #x8291a5
            }
        }
        name_lbl := mod.components.ClippedLabel {
            height: 18
            draw_text +: {
                color: #xc2cad5
                text_style: theme.font_bold { font_size: 9.75 }
            }
        }
        detach_project_btn := mod.components.IconButton {
            width: 22
            height: 22
            visible: false
            text: "×"
            draw_text +: {
                color: #x626d7d
                color_hover: #xd08a92
                color_down: #xf2a0aa
                text_style +: { font_size: 11.0 }
            }
            draw_bg +: {
                color_hover: #x36272d
                color_focus: #x36272d
                color_down: #x482c34
            }
        }
        new_project_session_btn := mod.components.SidebarComposeButton {}
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct SessionTitle {
    #[deref]
    view: View,
    #[rust]
    hovered: bool,
    #[rust]
    offset: f64,
    #[rust]
    max_offset: f64,
    #[rust]
    phase_start: Option<f64>,
    #[rust]
    next_frame: NextFrame,
}

impl SessionTitle {
    const START_PAUSE: f64 = 0.45;
    const END_PAUSE: f64 = 0.65;
    const SPEED: f64 = 28.0;

    fn reset(&mut self, cx: &mut Cx) {
        self.offset = 0.0;
        self.phase_start = None;
        self.view.set_scroll_pos(cx, dvec2(0.0, 0.0));
        self.view.redraw(cx);
    }

    fn set_hovered(&mut self, cx: &mut Cx, hovered: bool) {
        if self.hovered == hovered {
            return;
        }

        self.hovered = hovered;
        self.reset(cx);
        if hovered {
            if self.max_offset > 0.5 {
                self.next_frame = cx.new_next_frame();
            }
        }
    }

    fn advance(&mut self, cx: &mut Cx, time: f64) {
        if !self.hovered || self.max_offset <= 0.5 {
            return;
        }

        let phase_start = *self.phase_start.get_or_insert(time);
        let travel_duration = self.max_offset / Self::SPEED;
        let elapsed = time - phase_start;
        let travel_start = Self::START_PAUSE;
        let travel_end = travel_start + travel_duration;
        let cycle_end = travel_end + Self::END_PAUSE;

        self.offset = if elapsed < travel_start {
            0.0
        } else if elapsed < travel_end {
            ((elapsed - travel_start) * Self::SPEED).min(self.max_offset)
        } else if elapsed < cycle_end {
            self.max_offset
        } else {
            self.phase_start = Some(time);
            0.0
        };

        self.view.set_scroll_pos(cx, dvec2(self.offset, 0.0));
        self.view.redraw(cx);
        self.next_frame = cx.new_next_frame();
    }
}

impl Widget for SessionTitle {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let title = self.view.label(cx, ids!(title_lbl));
        let text = title.text();
        let text_width = title
            .borrow()
            .and_then(|title| title.draw_text.prepare_single_line_run(cx, &text))
            .map(|run| run.width_in_lpxs as f64)
            .unwrap_or(0.0);

        self.view.set_scroll_pos(cx, dvec2(self.offset, 0.0));
        let step = self.view.draw_walk(cx, scope, walk);
        let viewport_width = self.view.area().rect(cx).size.x;
        self.max_offset = (text_width - viewport_width).max(0.0);
        self.offset = self.offset.min(self.max_offset);
        step
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);

        if let Some(frame) = self.next_frame.is_event(event) {
            self.advance(cx, frame.time);
        }

        match event {
            Event::MouseMove(event) => {
                let hovered = self.view.area().rect(cx).contains(event.abs);
                self.set_hovered(cx, hovered);
            }
            Event::MouseLeave(_) => self.set_hovered(cx, false),
            _ => {}
        }
    }
}
