//! Base reusable DropDown component for options selection (e.g. model & thinking effort selection).

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    // The selected option is kept last by the app. Its transparent 24px row
    // anchors Makepad's OnSelected popup while leaving the closed picker visible.
    mod.components.ComposerPopupMenu = PopupMenuFlat {
        width: 130
        padding: Inset{left: 4 top: 4 right: 4 bottom: 0}
        draw_bg +: {
            color: #x242932
            border_color: #x454e5b
            connector_color: uniform(#x5f82ad)
            border_size: 1.0
            border_radius: 7.0
            selected_anchor_height: uniform(24.0)

            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let visible_height = max(
                    0.0,
                    self.rect_size.y - self.selected_anchor_height
                )
                sdf.box(
                    self.border_size,
                    self.border_size,
                    self.rect_size.x - self.border_size * 2.0,
                    max(0.0, visible_height - self.border_size * 2.0),
                    self.border_radius
                )
                sdf.fill_keep(self.color)
                sdf.stroke(self.border_color, self.border_size)
                sdf.move_to(10.0, visible_height - 1.0)
                sdf.line_to(self.rect_size.x - 10.0, visible_height - 1.0)
                sdf.stroke(self.connector_color, 1.0)
                return sdf.result
            }
        }
        menu_item: PopupMenuItem {
            width: Fill
            height: 24
            align: Align{y: 0.5}
            padding: Inset{left: 10 right: 10}
            draw_text +: {
                color: #xc9d0da
                color_hover: #xffffff
                color_active: #x00000000
                text_style +: { font_size: 9.5 }

                get_color: fn() {
                    let normal = self.color.mix(self.color_hover, self.hover)
                    return normal.mix(#x00000000, self.active)
                }
            }
            draw_bg +: {
                color: #x00000000
                color_hover: #x303844
                color_active: #x00000000
                mark_color: #x00000000
                mark_color_active: #x00000000

                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, 5.0)
                    let hover_fill = self.color.mix(self.color_hover, self.hover)
                    sdf.fill(hover_fill * (1.0 - self.active))
                    return sdf.result
                }
            }
        }
    }

    mod.components.ComposerDropDown = DropDown {
        width: Fill
        height: Fill
        margin: 0
        align: Align{x: 0.0 y: 0.5}
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
        popup_menu: mod.components.ComposerPopupMenu {}
    }

    mod.components.EffortDropDown = mod.components.ComposerDropDown {
        popup_menu: mod.components.ComposerPopupMenu { width: 116 }
    }

    mod.components.ModelDropDown = mod.components.ComposerDropDown {
        popup_menu: mod.components.ComposerPopupMenu { width: 142 }
    }
}
