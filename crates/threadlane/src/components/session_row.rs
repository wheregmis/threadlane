//! SessionRowBase component for list row items.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

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
                sdf.box(
                    surface_x + self.border_size
                    self.border_size
                    max(0.0, self.rect_size.x - surface_x - self.border_size * 2.0)
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
                    sdf.rect(
                        42.0
                        max(0.0, self.rect_size.y - 2.5)
                        max(0.0, self.rect_size.x - 90.0)
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
        title_surface := View {
            width: Fill
            height: 18
            flow: Right
            padding: 0
            title_lbl := Label {
                width: Fill
                height: 18
                max_lines: 1
                text_overflow: Ellipsis
                text: ""
                draw_text +: { color: #xaab3c0 text_style +: { font_size: 9.5 } }
            }
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
            draw_text +: { color: #x667180 text_style +: { font_size: 8.5 } }
        }
    }
}
