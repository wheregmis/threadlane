//! UserMsgBase component primitive for user chat message bubbles.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.UserMsgBase = View {
        width: Fill
        height: Fit
        flow: Down
        align: Align{x: 1.0}
        margin: Inset{top: 5 bottom: 5 left: 28 right: 20}

        user_bubble := RoundedView {
            width: Fit{max: FitBound.Abs(680)}
            height: Fit
            padding: Inset{left: 13 top: 8 right: 13 bottom: 8}
            draw_bg +: {
                color: theme.color_card
                border_radius: 9.0
            }

            md := mod.components.ChatMarkdown {
                width: Fit{max: FitBound.Abs(654)}
                selectable: true
            }
        }
    }
}
