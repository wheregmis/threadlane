//! AuthRow login and API key entry surface component.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.AuthRow = RoundedView {
        width: Fill
        height: Fit
        visible: false
        flow: Right
        spacing: 8
        align: Align{y: 0.5}
        padding: 10
        draw_bg +: {
            color: #x262133
            border_radius: 8.0
            border_size: 1.0
            border_color: #x4a3c55
        }
        Label {
            text: "Not signed in"
            draw_text +: {
                color: #xc7cdd6
                text_style +: { font_size: 10.5 }
            }
        }
        api_key_input := TextInput {
            width: Fill
            height: 32
            empty_text: "OpenAI API key (or sign in with ChatGPT)"
        }
        login_btn := Button {
            width: 130
            height: 32
            text: "Login ChatGPT"
        }
    }
}
