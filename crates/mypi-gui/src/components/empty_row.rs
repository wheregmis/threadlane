//! Minimal EmptyRowBase container primitive component.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.EmptyRowBase = View {
        width: Fill
        height: Fit
        lbl := Label {
            width: Fill
            height: Fit
            text: ""
            draw_text +: { color: #x6f7a88 text_style +: { font_size: 10.0 } }
        }
    }
}
