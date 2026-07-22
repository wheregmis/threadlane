//! Session-specific DSL components.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.SessionRowBase = RoundedView {
        width: Fill
        height: Fit
        cursor: MouseCursor.Hand
        flow: Right
        spacing: 8
        align: Align{y: 0.5}
        margin: Inset{left: 6 right: 6 top: 1 bottom: 1}
        padding: Inset{left: 12 top: 7 right: 10 bottom: 7}
        draw_bg +: { color: #x00000000 border_radius: 8.0 }
        title_lbl := Label {
            width: Fill
            height: Fit
            text: ""
            draw_text +: { color: #x9aa3b0 text_style +: { font_size: 11.0 } }
        }
        time_lbl := Label {
            width: Fit
            height: Fit
            text: ""
            draw_text +: { color: #x6f7a88 text_style +: { font_size: 10.0 } }
        }
    }
}
