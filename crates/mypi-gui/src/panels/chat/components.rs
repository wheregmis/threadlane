//! Chat-reusable components and custom tool fold header widget.

use makepad_widgets::fold_button::{FoldButton, FoldButtonAction};
use makepad_widgets::*;

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

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ActivityHeader = RoundedView {
        width: Fill
        height: 28
        cursor: MouseCursor.Hand
        padding: Inset{left: 8 top: 3 right: 8 bottom: 3}
        flow: Right
        spacing: 7
        align: Align{y: 0.5}
        draw_bg +: { color: #x20242c border_radius: 6.0 border_size: 0.0 }
        icon_lbl := Label {
            width: 18
            height: Fit
            text: "•"
            draw_text +: { color: #x8fa7c4 text_style: theme.font_bold { font_size: 10.0 } }
        }
        title_lbl := Label {
            width: 112
            height: Fit
            text: "Tool"
            draw_text +: { color: #xcbd2dc text_style: theme.font_bold { font_size: 10.0 } }
        }
        summary := View { width: Fill height: Fit flow: Right spacing: 7 align: Align{y: 0.5} }
        fold_button := FoldButton {
            draw_bg +: { active: 0.0 }
            animator +: { active: { default: @off } }
        }
    }

    mod.components.ChatBubble = RoundedView {
        width: Fill
        height: Fit
        padding: Inset{left: 14 top: 10 right: 14 bottom: 10}
        md := Markdown { width: Fill height: Fit selectable: true use_code_block_widget: false body: "" }
    }

    mod.components.ToolSection = View {
        width: Fill
        height: Fit
        flow: Down
        spacing: 4
        section_label := Label {
            width: Fill
            height: Fit
            text: "SECTION"
            draw_text +: { color: #x6fa8ff text_style: theme.font_bold { font_size: 8.0 } }
        }
        content_lbl := Label {
            width: Fill
            height: Fit
            text: ""
            draw_text +: { color: #xaab4c1 text_style: theme.font_code { font_size: 9.0 } }
        }
    }
}
