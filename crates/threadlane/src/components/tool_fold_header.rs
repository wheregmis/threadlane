//! ToolFoldHeader collapsible container widget.

use makepad_widgets::fold_button::{FoldButton, FoldButtonAction};
use makepad_widgets::*;

fn activity_header_hover_color() -> Vec4f {
    vec4(0.145, 0.173, 0.208, 0.28)
}

fn activity_header_pressed_color() -> Vec4f {
    vec4(0.176, 0.216, 0.267, 0.42)
}

#[derive(Script, ScriptHook, Widget, Animator)]
pub struct ToolFoldHeader {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[rust]
    draw_state: DrawStateWrap<ToolFoldDrawState>,
    #[rust]
    rect_size: f64,
    #[rust]
    area: Area,
    #[find]
    #[redraw]
    #[live]
    header: WidgetRef,
    #[find]
    #[redraw]
    #[live]
    body: WidgetRef,
    #[apply_default]
    animator: Animator,
    #[live]
    opened: f64,
    #[layout]
    layout: Layout,
    #[walk]
    walk: Walk,
    #[live]
    body_walk: Walk,
}

#[derive(Clone)]
enum ToolFoldDrawState {
    DrawHeader,
    DrawBody,
}

impl Widget for ToolFoldHeader {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.animator_handle_event(cx, event).must_redraw() {
            self.area.redraw(cx);
        }
        self.header.handle_event(cx, event, scope);
        self.body.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            let header_view = self.header.as_view();
            if header_view.finger_hover_in(actions).is_some() {
                self.set_header_background(cx, activity_header_hover_color());
                self.set_disclosure_fade(cx, 1.0);
            }
            if header_view.finger_hover_out(actions).is_some() {
                self.set_header_background(cx, Vec4f::all(0.0));
                self.set_disclosure_fade(cx, 0.0);
            }
            if header_view.finger_down(actions).is_some() {
                self.set_header_background(cx, activity_header_pressed_color());
            }
            if let Some(finger_up) = header_view.finger_up(actions) {
                self.set_header_background(
                    cx,
                    if finger_up.is_over {
                        activity_header_hover_color()
                    } else {
                        Vec4f::all(0.0)
                    },
                );
            }

            let fold_button_uid = self.header.widget(cx, ids!(fold_button)).widget_uid();
            let mut button_handled = false;
            for action in actions {
                if let Some(widget_action) = action.downcast_ref::<WidgetAction>() {
                    if widget_action.widget_uid != fold_button_uid {
                        continue;
                    }
                    button_handled = true;
                    match widget_action.cast::<FoldButtonAction>() {
                        FoldButtonAction::Opening => self.set_open(cx, true),
                        FoldButtonAction::Closing => self.set_open(cx, false),
                        _ => (),
                    }
                }
            }

            if !button_handled && self.header.as_view().finger_down(actions).is_some() {
                self.set_open(cx, self.opened <= 0.0);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if self.draw_state.begin(cx, ToolFoldDrawState::DrawHeader) {
            cx.begin_turtle(walk, self.layout);
        }
        if let Some(ToolFoldDrawState::DrawHeader) = self.draw_state.get() {
            let header_walk = self.header.walk(cx);
            self.header.draw_walk(cx, scope, header_walk)?;
            if self.opened <= 0.0 {
                cx.end_turtle_with_area(&mut self.area);
                self.draw_state.end();
                return DrawStep::done();
            }
            let (body_walk, scroll_y) = if self.opened >= 1.0 || self.rect_size == 0.0 {
                (self.body_walk, 0.0)
            } else {
                (
                    Walk {
                        height: Size::Fixed(self.rect_size * self.opened),
                        ..self.body_walk
                    },
                    self.rect_size * (1.0 - self.opened),
                )
            };
            cx.begin_turtle(
                body_walk,
                Layout::flow_down().with_scroll(dvec2(0.0, scroll_y)),
            );
            self.draw_state.set(ToolFoldDrawState::DrawBody);
        }
        if let Some(ToolFoldDrawState::DrawBody) = self.draw_state.get() {
            let body_walk = self.body.walk(cx);
            self.body.draw_walk(cx, scope, body_walk)?;
            let used_y = cx.turtle().used().y;
            if used_y > 0.0 {
                self.rect_size = used_y;
            }
            cx.end_turtle();
            cx.end_turtle_with_area(&mut self.area);
            self.draw_state.end();
        }
        DrawStep::done()
    }
}

impl ToolFoldHeader {
    fn set_header_background(&mut self, cx: &mut Cx, color: Vec4f) {
        let mut header = self.header.clone();
        script_apply_eval!(cx, header, {
            draw_bg +: { color: #(color) }
        });
        self.area.redraw(cx);
    }

    fn set_disclosure_fade(&mut self, cx: &mut Cx, fade: f64) {
        let mut fold_button = self.header.widget(cx, ids!(fold_button));
        script_apply_eval!(cx, fold_button, {
            draw_bg +: { fade: #(fade) }
        });
        self.area.redraw(cx);
    }

    fn set_open(&mut self, cx: &mut Cx, open: bool) {
        self.opened = if open { 1.0 } else { 0.0 };
        self.header
            .widget(cx, ids!(preview_lbl))
            .set_visible(cx, !open);
        self.animator_play(
            cx,
            if open {
                ids!(active.on)
            } else {
                ids!(active.off)
            },
        );
        if let Some(mut fold_button) = self
            .header
            .widget(cx, ids!(fold_button))
            .borrow_mut::<FoldButton>()
        {
            fold_button.set_is_open(cx, open, Animate::Yes);
        }
        self.area.redraw(cx);
    }
}
