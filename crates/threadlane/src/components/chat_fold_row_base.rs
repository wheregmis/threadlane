//! ChatFoldRowBase component primitive for collapsible chat message fold headers.

use super::tool_fold_header::ToolFoldHeader;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ChatFoldRowBase = #(ToolFoldHeader::register_widget(vm)) {
        width: Fill
        height: Fit
        flow: Down
        body_walk: Walk{width: Fill, height: Fit}
        margin: Inset{top: 4 bottom: 2 left: 20 right: 24}
        opened: 0.0
        animator +: {
            active: { default: @off }
        }
        header: mod.components.ActivityHeader {}
        body: RoundedView {
            width: Fill
            height: Fit
            padding: Inset{left: 30 top: 3 right: 18 bottom: 7}
            draw_bg +: {
                color: theme.color_transparent
                border_size: 0.0
            }
        }
    }
}
