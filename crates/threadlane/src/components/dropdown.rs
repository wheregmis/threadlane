//! Base reusable DropDown component for options selection (e.g. model & thinking effort selection).

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ComposerDropDown = DropDown {
        width: Fill
        height: Fill
        margin: 0
        padding: Inset{left: 10 right: 24}
        draw_bg +: {
            color: #x232830
            color_hover: #x2a313c
            color_focus: #x2f3a4d
            color_down: #x354153
            border_color: #x3a424e
            border_color_hover: #x4a5564
            border_color_focus: #x6fa8ff
            border_color_down: #x6fa8ff
            border_size: 1.0
            border_radius: 6.0
            arrow_color: #x7f8b9a
            arrow_color_hover: #xc7cdd6
            arrow_color_focus: #xc7cdd6
            arrow_color_down: #xffffff
        }
        draw_text +: {
            color: #xc7cdd6
            color_hover: #xdde3ea
            color_focus: #xdde3ea
            color_down: #xffffff
            text_style +: { font_size: 9.5 }
        }
        popup_menu: PopupMenuFlat {
            width: 220
            draw_bg +: {
                color: #x242932
                border_color: #x454e5b
                border_size: 1.0
                border_radius: 7.0
            }
            menu_item: PopupMenuItem {
                draw_text +: {
                    color: #xc9d0da
                    color_hover: #xffffff
                    color_active: #xffffff
                    text_style +: { font_size: 10.0 }
                }
                draw_bg +: {
                    color: #x00000000
                    color_hover: #x303844
                    color_active: #x354153
                    mark_color: #x00000000
                    mark_color_active: #x6fa8ff
                }
            }
        }
    }
}
