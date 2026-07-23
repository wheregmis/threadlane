//! Minimal ComposerSurface card container component.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ComposerSurface = RoundedView {
        width: Fill
        height: Fit
        draw_bg +: {
            color: #x1f232b
            border_color: #x3a424e
            border_size: 1.0
            border_radius: 11.0
        }
    }
}
