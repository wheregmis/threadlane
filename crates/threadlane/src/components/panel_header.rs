//! Minimal PanelHeader container primitive component.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.PanelHeader = View {
        width: Fill
        height: Fit
        flow: Right
        align: Align{y: 0.5}
        padding: Inset{left: 8 right: 8 top: 2 bottom: 2}
    }
}
