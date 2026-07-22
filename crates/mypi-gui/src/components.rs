//! Reusable GUI shells. Defaults live here so call sites only override intent.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components = {}

    mod.components.ActivityHeader = RoundedView {
        width: Fill
        height: 28
        cursor: MouseCursor.Hand
        padding: Inset{left: 8 top: 3 right: 8 bottom: 3}
        flow: Right
        spacing: 7
        align: Align{y: 0.5}
        draw_bg +: { color: #x20242c border_radius: 6.0 border_size: 0.0 }
        icon_lbl := Label {
            width: 18
            height: Fit
            text: "•"
            draw_text +: { color: #x8fa7c4 text_style: theme.font_bold { font_size: 10.0 } }
        }
        title_lbl := Label {
            width: 112
            height: Fit
            text: "Tool"
            draw_text +: { color: #xcbd2dc text_style: theme.font_bold { font_size: 10.0 } }
        }
        summary := View { width: Fill height: Fit flow: Right spacing: 7 align: Align{y: 0.5} }
        fold_button := FoldButton {
            draw_bg +: { active: 0.0 }
            animator +: { active: { default: @off } }
        }
    }

    mod.components.ChatBubble = RoundedView {
        width: Fill
        height: Fit
        padding: Inset{left: 14 top: 10 right: 14 bottom: 10}
        md := Markdown { width: Fill height: Fit selectable: true use_code_block_widget: false body: "" }
    }

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

    mod.components.PanelHeader = View {
        width: Fill
        height: Fit
        flow: Right
        align: Align{y: 0.5}
        padding: Inset{left: 8 right: 8 top: 2 bottom: 2}
    }

    mod.components.PanelSurface = RoundedView {
        width: Fill
        height: Fill
        draw_bg +: { color: #x1f232b border_radius: 10.0 }
    }

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

    mod.components.ToolSection = View {
        width: Fill
        height: Fit
        flow: Down
        spacing: 4
        section_label := Label {
            width: Fill
            height: Fit
            text: "SECTION"
            draw_text +: { color: #x6fa8ff text_style: theme.font_bold { font_size: 8.0 } }
        }
        content_lbl := Label {
            width: Fill
            height: Fit
            text: ""
            draw_text +: { color: #xaab4c1 text_style: theme.font_code { font_size: 9.0 } }
        }
    }

    mod.components.ProgressDot = RoundedView {
        width: 3
        height: 3
        draw_bg +: { color: #x8b93a0 border_radius: 1.5 }
    }

    mod.components.FlexSpacer = View { width: Fill height: 1 }
}
