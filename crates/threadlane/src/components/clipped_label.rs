//! ClippedLabel component primitive for single-line ellipsized text labels.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ClippedLabel = Label {
        width: Fill
        height: Fit
        max_lines: 1
        text_overflow: Ellipsis
        draw_text +: {
            color: #x9ba7b6
            text_style +: { font_size: 9.0 }
        }
    }
}
