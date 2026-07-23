//! Truly cross-panel primitives and minimal reusable components.

use makepad_widgets::*;

pub mod activity_header;
pub mod activity_loader;
pub mod auth_row;
pub mod chat_bubble;
pub mod command_input;
pub mod composer_action;
pub mod composer_surface;
pub mod context_menu;
pub mod dropdown;
pub mod empty_row;
pub mod flex_spacer;
pub mod init;
pub mod panel_header;
pub mod panel_surface;
pub mod session_row;
pub mod status_pill;
pub mod tool_fold_header;
pub mod tool_section;

pub fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
    init::script_mod(vm);
    activity_header::script_mod(vm);
    activity_loader::script_mod(vm);
    auth_row::script_mod(vm);
    chat_bubble::script_mod(vm);
    command_input::script_mod(vm);
    composer_action::script_mod(vm);
    composer_surface::script_mod(vm);
    context_menu::script_mod(vm);
    dropdown::script_mod(vm);
    empty_row::script_mod(vm);
    flex_spacer::script_mod(vm);
    panel_header::script_mod(vm);
    panel_surface::script_mod(vm);
    session_row::script_mod(vm);
    status_pill::script_mod(vm);
    tool_section::script_mod(vm)
}
