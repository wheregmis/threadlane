//! StatusDot component primitive for circular status indicators.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.StatusDot = RoundedView {
        width: 7
        height: 7
        visible: false
        draw_bg +: {
            color: #x67c58b
            border_radius: 3.5
        }
    }
}
