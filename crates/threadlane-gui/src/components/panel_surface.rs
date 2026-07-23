//! Minimal PanelSurface container primitive component.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.PanelSurface = RoundedView {
        width: Fill
        height: Fill
        draw_bg +: { color: #x1f232b border_radius: 10.0 }
    }
}
