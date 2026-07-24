//! Minimal ChatBubble message component.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ChatMarkdown = Markdown {
        width: Fill
        height: Fit
        selectable: true
        use_code_block_widget: false
        body: ""
    }

    mod.components.ChatBubble = RoundedView {
        width: Fill
        height: Fit
        padding: Inset{left: 14 top: 10 right: 14 bottom: 10}
        md := mod.components.ChatMarkdown {}
    }
}
