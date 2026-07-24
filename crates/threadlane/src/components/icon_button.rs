//! Base reusable IconButton primitive for compact icon-only buttons.
//!
//! Enforces centered SVG viewbox alignment and standard hit-test padding
//! according to repository Makepad component conventions.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.IconButton = Button {
        width: 24
        height: 24
        margin: 0
        padding: 0
        spacing: 0
        text: ""
        align: Align{x: 0.5 y: 0.5}
        icon_walk: Walk{width: 12 height: 12 margin: 0}
        draw_icon +: {
            color: #x758294
            color_hover: #xb8d5f5
            color_down: #xffffff
        }
        draw_bg +: {
            color: #x00000000
            color_hover: #x283544
            color_focus: #x283544
            color_down: #x30445b
            border_color: #x00000000
            border_color_hover: #x00000000
            border_color_focus: #x00000000
            border_color_down: #x00000000
            border_size: 0.0
            border_radius: 6.0
        }
    }
}
