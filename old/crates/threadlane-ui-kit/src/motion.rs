//! Product motion policy over GPUI Kit's retained transition and reveal behavior.
use gpui::{
    div, AnyElement, App, ElementId, IntoElement, ParentElement, RenderOnce, Styled, Window,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Icon, Selectable, Sizable};
use gpui_kit::base::{spring, MotionReveal, Presence, PresenceSample, Transition};

/// Sample before constructing a disclosure's body. Closed content stays lazy;
/// exiting content survives only until the reversible transition finishes.
pub struct DisclosureMotion {
    id: ElementId,
    sample: PresenceSample,
    reduce_motion: bool,
}

impl DisclosureMotion {
    pub fn new(id: impl Into<ElementId>, open: bool, window: &mut Window, cx: &mut App) -> Self {
        let id = id.into();
        let tokens = cx.theme().motion_tokens();
        let transition = Transition::new(tokens.duration_fast).easing(tokens.easing_move.clone());
        let sample = Presence::new(id.clone(), open)
            .transition(transition)
            .sample(window, cx);
        Self {
            id,
            sample,
            reduce_motion: cx.reduce_motion(),
        }
    }

    pub fn is_visible(&self) -> bool {
        self.sample.should_render()
            && !(self.sample.phase == gpui_kit::base::PresencePhase::Exiting
                && self.sample.progress == 0.0)
    }
    pub fn is_animating(&self) -> bool {
        self.is_visible()
            && matches!(
                self.sample.phase,
                gpui_kit::base::PresencePhase::Entering | gpui_kit::base::PresencePhase::Exiting
            )
    }

    /// Virtual lists cache row heights. Invalidate after layout releases its
    /// borrow, so each reveal frame measures the changing row without jumping.
    pub fn remeasure_list_row(&self, list: &gpui::ListState, index: usize, window: &Window) {
        if self.is_animating() {
            let list = list.clone();
            window.on_next_frame(move |window, _| {
                if index < list.item_count() {
                    list.remeasure_items(index..index + 1);
                    window.refresh();
                }
            });
        }
    }

    pub fn content(&self, body: impl IntoElement) -> AnyElement {
        let content = div()
            .w_full()
            .min_w_0()
            .opacity(self.sample.progress)
            .child(body);
        // Keep the measured reveal mounted while open so closing starts at
        // the current height rather than remeasuring a newly mounted element.
        if !self.reduce_motion {
            MotionReveal::new(
                ElementId::NamedChild(self.id.clone().into(), "body".into()),
                self.sample.progress,
                content.into_any_element(),
            )
            .into_any_element()
        } else {
            content.into_any_element()
        }
    }
}

/// A controlled disclosure header. The description includes the requested
/// expand/collapse action; hosts supply content and attach their own callback.
pub fn disclosure_button(
    id: impl Into<ElementId>,
    open: bool,
    description: impl Into<gpui::SharedString>,
    content: impl IntoElement,
) -> Button {
    let id = id.into();
    let description = description.into();
    Button::new(id.clone())
        .ghost()
        .open(open)
        .small()
        .w_full()
        .justify_between()
        .gap_2()
        .accessibility_label(description.clone())
        .tooltip(description)
        .child(content)
        .child(chevron(id, open))
}

pub(crate) fn chevron(id: impl Into<ElementId>, open: bool) -> AnyElement {
    Chevron {
        id: id.into(),
        open,
    }
    .into_any_element()
}

#[derive(IntoElement)]
struct Chevron {
    id: ElementId,
    open: bool,
}

impl RenderOnce for Chevron {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let progress = spring(
            ElementId::NamedChild(self.id.into(), "chevron".into()),
            if self.open { 1.0 } else { 0.0 },
            cx.theme().motion_tokens().spring_control,
            window,
            cx,
        );
        Icon::default()
            .data(gpui_kit_assets::__private::ChevronRight.1)
            .xsmall()
            .rotate(gpui::Radians(progress * std::f32::consts::FRAC_PI_2))
            .text_color(cx.theme().muted_foreground)
    }
}
