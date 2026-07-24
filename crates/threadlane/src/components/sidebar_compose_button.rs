//! Compact icon-only compose button used by sidebar actions.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.SidebarComposeButton = mod.components.IconButton {
        visible: false
        draw_icon +: {
            svg: crate_resource("self:resources/icons/compose.svg")
        }
    }
}
