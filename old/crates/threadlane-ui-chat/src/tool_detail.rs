//! Desktop adapters for the toolkit's tool details. Filesystem and editor actions stay here.
use gpui::{AnyElement, App, Entity};
pub(crate) use threadlane_ui_kit::tool_detail::{
    args_json, args_path, expandable, is_command_tool,
};

use threadlane_ui_state::{actions::AppAction, controller, AppState, ToolActivityInfo};

pub(crate) fn render_activity_detail_card(
    activity: &ToolActivityInfo,
    model: &Entity<AppState>,
    cx: &mut App,
) -> Option<AnyElement> {
    if let Some(preview) = super::tool_preview::render(activity, model, cx) {
        return Some(preview);
    }
    let model = model.clone();
    threadlane_ui_kit::tool_detail::render_activity_detail_card(
        activity,
        Some(move |path, cx: &mut App| {
            model.update(cx, |state, cx| {
                controller::dispatch(state, AppAction::OpenFileInEditor(path));
                cx.notify();
            });
        }),
        cx,
    )
}
