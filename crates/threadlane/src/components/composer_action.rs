//! Minimal ComposerAction and ComposerChip button base components.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ComposerChip = Button {
        width: Fit
        height: 24
        padding: Inset{left: 9 right: 9 top: 2 bottom: 2}
        draw_bg +: {
            color: #x232830
            color_hover: #x2a313c
            color_down: #x354153
            border_color: #x3a424e
            border_color_hover: #x4a5564
            border_size: 1.0
            border_radius: 6.0
        }
        draw_text +: {
            color: #xc7cdd6
            color_hover: #xdde3ea
            color_down: #xffffff
            text_style +: { font_size: 9.0 }
        }
    }

    mod.components.AttachmentChip = mod.components.ComposerChip {
        visible: false
        padding: Inset{left: 8 right: 9 top: 2 bottom: 2}
        icon_walk: Walk{width: 12 height: 12 margin: Inset{right: 5}}
        draw_icon +: {
            svg: crate_resource("self:resources/icons/image.svg")
            color: #x8eb7ef
            color_hover: #xb8d5ff
            color_down: #xffffff
        }
    }

    mod.components.ComposerAction = Button {
        width: Fit
        height: 28
        padding: Inset{left: 11 right: 11 top: 2 bottom: 2}
        draw_bg +: {
            color: #x4f78aa
            color_hover: #x6092cc
            color_down: #x70a7ff
            border_radius: 7.0
        }
        draw_text +: {
            color: #xffffff
            text_style: theme.font_bold { font_size: 9.5 }
        }
    }
}
