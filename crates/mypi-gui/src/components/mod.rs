//! Truly cross-panel primitives only.

pub mod primitives;

pub fn script_mod(vm: &mut makepad_widgets::ScriptVm) -> makepad_widgets::ScriptValue {
    primitives::script_mod(vm)
}
