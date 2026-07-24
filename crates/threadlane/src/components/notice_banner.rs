//! NoticeBanner component primitive for status bars and notification surfaces.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.NoticeBanner = RoundedView {
        width: Fill
        height: 38
        visible: false
        flow: Right
        spacing: 8
        align: Align{y: 0.5}
        padding: Inset{left: 10 right: 11}
        margin: Inset{left: 2 right: 2}
        show_bg: true
        draw_bg +: {
            color: #x1b232dc8
            border_color: #x354353
            border_size: 1.0
            border_radius: 8.0
        }
    }
}
