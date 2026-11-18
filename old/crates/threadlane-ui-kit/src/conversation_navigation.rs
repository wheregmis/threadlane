//! Controlled prompt rail and conversation outline; hosts own identity and navigation.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};
use threadlane_protocol::transcript::PromptLandmark;

pub const PROMPT_RECALL_KEY_CONTEXT: &str = "ComposerPromptRecall";
pub const PROMPT_RECALL_BINDING_CONTEXT: &str = "ComposerPromptRecall > Input";
actions!(threadlane_composer, [RecallOlderPrompt, RecallNewerPrompt]);
pub fn init_prompt_recall(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", RecallOlderPrompt, Some(PROMPT_RECALL_BINDING_CONTEXT)),
        KeyBinding::new(
            "down",
            RecallNewerPrompt,
            Some(PROMPT_RECALL_BINDING_CONTEXT),
        ),
    ]);
}

pub fn active_prompt_landmark(entries: &[PromptLandmark], transcript: &ListState) -> Option<usize> {
    if entries.is_empty() {
        return None;
    }
    if transcript.is_following_tail() {
        entries.len().checked_sub(1)
    } else {
        Some(
            entries
                .partition_point(|entry| entry.row_index <= transcript.logical_scroll_top().item_ix)
                .saturating_sub(1),
        )
    }
}
pub fn prompt_navigation_rail(
    ticks: List,
    outline: impl IntoElement,
    count: usize,
) -> Stateful<Div> {
    div()
        .id("prompt-navigation-rail")
        .debug_selector(|| "prompt-navigation-rail".into())
        .w_8()
        .h_full()
        .flex_shrink_0()
        .py_3()
        .flex()
        .flex_col()
        .justify_center()
        .items_center()
        .child(
            ticks
                .w_8()
                .h(rems((count as f32 * 1.5).min(12.0)))
                .max_h_full()
                .min_h_0(),
        )
        .child(outline)
}
fn prompt_label(landmark: &PromptLandmark) -> String {
    format!(
        "Prompt {} · {}",
        landmark.ordinal,
        if landmark.excerpt.is_empty() {
            "No text"
        } else {
            &landmark.excerpt
        }
    )
}
pub fn prompt_rail_tick(landmark: &PromptLandmark, selected: bool, cx: &App) -> Button {
    let id = landmark.message_id.clone();
    let label = prompt_label(landmark);
    Button::new(SharedString::from(format!("prompt-rail-{id}")))
        .debug_selector(move || format!("prompt-rail-{id}"))
        .ghost()
        .small()
        .w_8()
        .h_6()
        .accessibility_label(if selected {
            format!("{label} · Current prompt")
        } else {
            label.clone()
        })
        .tooltip(label)
        .tooltip_placement(gpui_component::Placement::Right)
        .child(
            div()
                .h_0p5()
                .rounded_full()
                .when(selected, |el| el.w_4().bg(cx.theme().foreground))
                .when(!selected, |el| el.w_2().bg(cx.theme().muted_foreground)),
        )
}
pub fn conversation_outline_popover(open: bool, focus: &FocusHandle) -> Popover {
    Popover::new("conversation-outline")
        .anchor(Anchor::TopLeft)
        .appearance(false)
        .open(open)
        .track_focus(focus)
        .trigger(
            Button::new("conversation-outline-open")
                .debug_selector(|| "conversation-outline-open".into())
                .label("…")
                .ghost()
                .small()
                .accessibility_label("Conversation outline")
                .tooltip("Conversation outline — jump to an earlier prompt"),
        )
}
pub fn conversation_outline_row(
    landmark: &PromptLandmark,
    focused: bool,
    selected: bool,
    cx: &App,
) -> Stateful<Div> {
    let id = landmark.message_id.clone();
    let theme = cx.theme();
    div()
        .id(SharedString::from(format!("conversation-outline-row-{id}")))
        .debug_selector(move || format!("conversation-outline-row-{id}"))
        .flex()
        .flex_row()
        .w_full()
        .items_center()
        .gap_2()
        .px_3()
        .py_1p5()
        .role(Role::ListBoxOption)
        .aria_label(prompt_label(landmark))
        .aria_selected(selected)
        .when(focused, |el| el.bg(theme.secondary))
        .when(!focused, |el| el.hover(|style| style.bg(theme.list_hover)))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .flex_shrink_0()
                .child(format!("Prompt {}", landmark.ordinal)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(if landmark.excerpt.is_empty() {
                    theme.muted_foreground
                } else {
                    theme.foreground
                })
                .child(if landmark.excerpt.is_empty() {
                    SharedString::from("No text")
                } else {
                    landmark.excerpt.clone().into()
                }),
        )
        .when(selected, |el| {
            el.child(
                Icon::new(IconName::Check)
                    .xsmall()
                    .text_color(theme.primary),
            )
        })
}
pub fn conversation_outline_content(
    focus: &FocusHandle,
    count: usize,
    status: Option<SharedString>,
    rows: impl IntoElement,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id("conversation-outline-content")
        .debug_selector(|| "conversation-outline-content".into())
        .flex()
        .flex_col()
        .role(Role::ListBox)
        .aria_label("Conversation outline prompts")
        .track_focus(focus)
        .key_context("ConversationOutline")
        .w_80()
        .h(rems(18.75))
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .shadow_lg()
        .occlude()
        .child(
            div()
                .flex()
                .flex_row()
                .w_full()
                .items_center()
                .justify_between()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .child("Prompts in this conversation"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{count}")),
                ),
        )
        .child(
            div().flex_1().min_h_0().child(match status {
                Some(message) => div()
                    .w_full()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_3()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(message)
                    .into_any_element(),
                None => rows.into_any_element(),
            }),
        )
        .child(
            div()
                .w_full()
                .px_3()
                .py_1p5()
                .border_t_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("Enter to jump · Esc to close"),
        )
}
