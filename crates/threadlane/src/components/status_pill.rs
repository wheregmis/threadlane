//! StatusPill workspace header status indicator component.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.StatusPill = View {
        width: 16
        height: 20
        visible: false
        flow: Down
        spacing: 2
        align: Align{x: 0.5 y: 0.5}
        progress_dot_1 := mod.components.ProgressDot {
            visible: false
        }
        progress_dot_2 := mod.components.ProgressDot {
            visible: false
            draw_bg +: { color: #xaeb6c2 }
        }
        progress_dot_3 := mod.components.ProgressDot {
            visible: false
            draw_bg +: { color: #x6f7a88 }
        }
        error_dot := mod.components.StatusDot {
            width: 5
            height: 5
            draw_bg +: { color: #xe5534b, border_radius: 2.5 }
        }
    }
}
