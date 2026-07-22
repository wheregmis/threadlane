//! Plan panel main view & plan list widget.

use crate::workspace::AppState;
use makepad_widgets::*;

#[derive(Script, ScriptHook, Widget)]
pub struct PlanList {
    #[deref]
    view: View,
}

impl Widget for PlanList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(data) = scope
            .data
            .get::<AppState>()
            .and_then(AppState::active_workspace)
            .map(|workspace| workspace.plan.clone())
        else {
            return DrawStep::done();
        };

        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, data.items.len());

                while let Some(item_id) = list.next_visible_item(cx) {
                    if let Some(plan_item) = data.items.get(item_id) {
                        let item_widget = list.item(cx, item_id, id!(PlanRow));
                        item_widget
                            .label(cx, ids!(status_lbl))
                            .set_text(cx, if plan_item.completed { "✓" } else { "○" });
                        item_widget.label(cx, ids!(desc_lbl)).set_text(
                            cx,
                            &format!("{}. {}", plan_item.index, plan_item.description),
                        );
                        item_widget.draw_all_unscoped(cx);
                    }
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}
