//! Cross-panel primitive widgets and reusable Makepad DSL components.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components = {}

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

    mod.components.ProgressDot = RoundedView {
        width: 3
        height: 3
        draw_bg +: { color: #x8b93a0 border_radius: 1.5 }
    }

    mod.components.FlexSpacer = View { width: Fill height: 1 }
}
