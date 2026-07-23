//! SessionRowBase component for list row items.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

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
