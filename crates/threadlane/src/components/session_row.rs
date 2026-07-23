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
        padding: Inset{left: 11 top: 4 right: 9 bottom: 4}
        draw_bg +: {
            hover: instance(0.0)
            color: #x00000000
            color_hover: uniform(#x252b34)
            border_radius: 7.0

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
            width: 14
            height: 14
            icon_walk: Walk{width: 14 height: 14}
            draw_icon +: {
                svg: crate_resource("self:resources/icons/conversation.svg")
                color: #x667386
            }
        }
        title_lbl := Label {
            width: Fill
            height: 18
            text: ""
            draw_text +: { color: #xaab3c0 text_style +: { font_size: 9.5 } }
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
