//! Shared settings action button styling and selected-state synchronization.

use makepad_widgets::*;


script_mod! {
    use mod.prelude.widgets.*

    mod.components.SettingsActionButton = Button {
        width: Fit
        height: 28
        padding: Inset{left: 10 right: 10 top: 4 bottom: 4}

        draw_bg +: {
            border_size: 1.0
            border_radius: 6.0
            color: theme.color_card
            color_hover: theme.color_secondary
            color_focus: theme.color_secondary
            color_down: theme.color_input
            border_color: theme.color_secondary
            border_color_hover: theme.color_primary
            border_color_focus: theme.color_primary
            border_color_down: theme.color_primary
        }

        draw_text +: {
            color: theme.color_card_foreground
            color_hover: theme.color_foreground
            color_focus: theme.color_primary_foreground
            color_down: theme.color_primary_foreground
            text_style +: { font_size: 9.0 }
        }

        animator: Animator{
            selected: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: 0.}}
                    apply: {
                        draw_bg: {
                            color: theme.color_card
                            border_color: theme.color_secondary
                        }
                        draw_text: {
                            color: theme.color_card_foreground
                        }
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: 0.}}
                    apply: {
                        draw_bg: {
                            color: theme.color_primary
                            border_color: theme.color_primary
                        }
                        draw_text: {
                            color: theme.color_primary_foreground
                        }
                    }
                }
            }
        }
    }
}
