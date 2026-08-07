//! SessionRowBase component for list row items.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.SessionRowBase = #(SessionRow::register_widget(vm)) {
        width: Fill
        height: 34
        cursor: MouseCursor.Hand
        flow: Right
        spacing: 10
        align: Align{y: 0.5}
        margin: Inset{left: 10 right: 4 top: 1 bottom: 1}
        padding: Inset{left: 20 top: 4 right: 9 bottom: 4}
        draw_bg +: {
            hover: instance(0.0)
            tree_last: instance(0.0)
            is_active: instance(0.0)
            is_context: instance(0.0)
            color: theme.color_transparent
            color_hover: uniform(theme.color_card)
            color_active: uniform(theme.color_secondary)
            color_active_hover: uniform(theme.color_accent)
            color_context: uniform(theme.color_card)
            color_context_hover: uniform(theme.color_accent)
            tree_color: uniform(theme.color_card)
            border_color: theme.color_transparent
            border_color_active: uniform(theme.color_border)
            border_color_context: uniform(theme.color_border)
            border_size: 0.0
            border_size_active: uniform(1.0)
            border_size_context: uniform(1.0)
            border_radius: 7.0
            border_radius_active: uniform(theme.radius_sm)
            border_radius_context: uniform(theme.radius_sm)

            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let tree_x = 9.0
                let surface_x = 14.0

                let bg = mix(self.color, self.color_context, self.is_context)
                let bg = mix(bg, self.color_active, self.is_active)

                let hov_color = mix(self.color_hover, self.color_context_hover, self.is_context)
                let hov_color = mix(hov_color, self.color_active_hover, self.is_active)

                let bd_color = mix(self.border_color, self.border_color_context, self.is_context)
                let bd_color = mix(bd_color, self.border_color_active, self.is_active)

                let bd_size = mix(self.border_size, self.border_size_context, self.is_context)
                let bd_size = mix(bd_size, self.border_size_active, self.is_active)

                let rad = mix(self.border_radius, self.border_radius_context, self.is_context)
                let rad = mix(rad, self.border_radius_active, self.is_active)

                let surface_left = surface_x + bd_size
                let surface_width = max(
                    0.0
                    self.rect_size.x - surface_x - bd_size * 2.0
                )
                sdf.box(
                    surface_left
                    bd_size
                    surface_width
                    self.rect_size.y - bd_size * 2.0
                    max(1.0 rad)
                )
                sdf.fill_keep(mix(bg, hov_color, self.hover))
                sdf.stroke(bd_color, bd_size)

                let tree_mid = self.rect_size.y * 0.5
                let tree_height = mix(self.rect_size.y, tree_mid, self.tree_last)
                sdf.rect(tree_x, 0.0, 1.0, max(0.0, tree_height))
                sdf.fill(self.tree_color)
                sdf.rect(tree_x, tree_mid, surface_x - tree_x + 1.0, 1.0)
                sdf.fill(self.tree_color)
                return sdf.result
            }
        }
        animator +: {
            hover: {
                default: @off
                off: AnimatorState {
                    from: {all: Forward {duration: 0.10}}
                    apply: {draw_bg: {hover: 0.0}}
                }
                on: AnimatorState {
                    from: {all: Forward {duration: 0.08}}
                    apply: {draw_bg: {hover: snap(1.0)}}
                }
            }
        }
        title_surface := mod.components.SessionTitle {}
        session_row_spinner := mod.components.ActivityLoader {
            width: 18
            height: 10
            visible: false
        }
        health_lbl := Label {
            width: Fit
            height: Fit
            visible: false
            text: ""
            draw_text +: { color: theme.color_muted_foreground text_style +: { font_size: 8.0 } }
        }
        time_lbl := Label {
            width: Fit
            height: Fit
            text: ""
            draw_text +: { color: theme.color_muted_foreground text_style +: { font_size: 9.0 } }
        }
        settle_session_btn := mod.components.IconButton {
            width: 20
            height: 20
            visible: false
            icon_walk: Walk{width: 13 height: 13 margin: 0}
            draw_icon +: {
                svg: crate_resource("self:resources/icons/checkbox-checked.svg")
                color: theme.color_muted_foreground
                color_hover: theme.color_foreground
                color_focus: theme.color_foreground
                color_down: theme.color_primary_foreground
            }
            draw_bg +: {
                color_hover: theme.color_secondary
                color_focus: theme.color_secondary
                color_down: theme.color_input
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub enum SessionRowAction {
    Settle,
    #[default]
    None,
}

#[derive(Script, ScriptHook, Widget)]
pub struct SessionRow {
    #[deref]
    view: View,
    #[rust]
    menu_painted: bool,
}

impl SessionRow {
    pub fn set_state(&mut self, cx: &mut Cx, active: bool, context_target: bool) {
        self.view
            .draw_bg
            .draw_vars
            .set_dyn_instance(cx, id!(is_active), &[active as u8 as f32]);
        self.view.draw_bg.draw_vars.set_dyn_instance(
            cx,
            id!(is_context),
            &[context_target as u8 as f32],
        );
        self.view.draw_bg.redraw(cx);
    }

    fn set_settle_painted(&mut self, cx: &mut Cx, painted: bool) {
        if self.menu_painted == painted {
            return;
        }
        self.menu_painted = painted;
        self.view
            .button(cx, ids!(settle_session_btn))
            .set_visible(cx, painted);
        self.view.redraw(cx);
    }
}

impl Widget for SessionRow {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        match event {
            Event::MouseMove(event) => {
                self.set_settle_painted(cx, self.view.area().clipped_rect(cx).contains(event.abs));
            }
            Event::MouseLeave(_) => self.set_settle_painted(cx, false),
            _ => {}
        }

        if let Event::Actions(actions) = event {
            if self
                .view
                .button(cx, ids!(settle_session_btn))
                .clicked(actions)
            {
                cx.widget_action(self.widget_uid(), SessionRowAction::Settle);
            }
        }
    }
}
