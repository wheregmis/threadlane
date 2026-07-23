//! ToolSection container component for payload details (INPUT / OUTPUT).

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ToolSection = RoundedView {
        width: Fill
        height: Fit
        flow: Down
        spacing: 5
        padding: Inset{left: 8 top: 6 right: 8 bottom: 7}
        draw_bg +: {
            color: #x1c2027
            border_radius: 4.0
            border_size: 0.0
        }
        section_label := Label {
            width: Fill
            height: Fit
            text: "SECTION"
            draw_text +: {
                color: #x748397
                text_style: theme.font_bold { font_size: 7.5 }
            }
        }
        content_lbl := Label {
            width: Fill
            height: Fit
            text: ""
            draw_text +: {
                color: #xaeb7c4
                text_style: theme.font_code { font_size: 8.5 }
            }
        }
        content_md_wrap := View {
            width: Fill
            height: Fit
            visible: false
            content_md := Markdown {
                width: Fill
                height: Fit
                selectable: true
                use_code_block_widget: false
                body: ""
            }
        }
    }
}
