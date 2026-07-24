//! SectionHeader and SectionLabel component primitives.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.SectionLabel = Label {
        width: Fill
        height: Fit
        text: ""
        draw_text +: {
            color: #x7f8b9b
            text_style: theme.font_bold { font_size: 8.5 }
        }
    }

    mod.components.SectionHeader = View {
        width: Fill
        height: 34
        cursor: MouseCursor.Arrow
        flow: Right
        spacing: 8
        align: Align{y: 0.5}
        padding: Inset{left: 7 right: 4 bottom: 4}
        section_label := mod.components.SectionLabel {}
    }
}
