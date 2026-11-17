//! Threadlane's native agent UI toolkit. Hosts own state, navigation, services and actions.
//! Components share theme tokens and controlled state across desktop and iOS.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Disableable, Selectable, Sizable, StyledExt};
use threadlane_protocol::daemon::{MessageRole, SessionAttention, SessionInfo};
use threadlane_protocol::QuestionItem;
use threadlane_ui_theme::{CHAT_CONTENT_MAX_WIDTH, QUESTION_CARD_MAX_WIDTH, USER_BUBBLE_MAX_WIDTH};

pub mod markdown;
pub mod transcript;
pub mod tool_detail;
pub mod tool_preview;
pub mod file_completion;
mod diff;
pub use diff::diff_text_view;
mod navigation;
pub use navigation::*;
mod sidebar_menus;
pub use sidebar_menus::*;
mod sidebar_dialogs;
pub use sidebar_dialogs::*;
mod sidebar_card;
pub use sidebar_card::*;
#[cfg(test)]
mod sidebar_card_tests;
#[cfg(test)]
mod sidebar_dialogs_tests;
#[cfg(test)]
mod sidebar_menus_tests;
mod settings_search;
pub use settings_search::*;
mod palette;
#[cfg(test)]
mod palette_tests;
pub use palette::*;
mod workspace;
pub use workspace::*;
mod panel;
pub use panel::*;
mod browser;
pub use browser::*;
mod files;
pub use files::*;
mod review;
pub use review::*;
mod review_find;
pub use review_find::{AddReviewSelectionToChat, FindInDiff, ReviewDiffDocument};
mod review_excerpt;
pub use review_excerpt::*;
mod review_actions;
pub use review_actions::*;
mod review_branches;
pub use review_branches::*;
mod review_draft_pr;
pub use review_draft_pr::*;
mod review_records;
pub use review_records::*;
mod queue;
pub use queue::*;
mod attachment;
pub use attachment::*;
mod image_preview;
pub use image_preview::*;
mod draft;
pub use draft::*;
mod picker;
pub use picker::*;
mod editor;
mod editor_completion;
mod editor_workbench;
pub use editor_workbench::{EditorLanguageRefresh, EditorWorkbench};
mod editor_markdown;
pub use editor_markdown::*;
#[cfg(test)]
mod editor_tests;
pub use editor::*;
mod terminal;
pub use terminal::*;
mod terminal_grid;
pub use terminal_grid::*;
mod terminal_output;
pub use terminal_output::*;
mod code_block;
pub use code_block::*;
mod conversation;
pub use conversation::*;
mod conversation_navigation;
pub use conversation_navigation::*;
mod conversation_find;
pub use conversation_find::*;
mod terminal_find;
pub use terminal_find::*;
mod terminal_links;
pub use terminal_links::*;
mod surfaces;
pub use surfaces::{result_header, result_scroll_body, result_surface, result_viewport};
mod agents;
#[cfg(test)]
mod agents_worktree_tests;
pub use agents::*;
mod trajectory_cache;
pub use trajectory_cache::{TrajectoryMode, TrajectoryInspectorTab};
mod trajectory_view;
pub use trajectory_view::{TrajectoryView, TrajectorySource};
#[cfg(test)]
mod trajectory_view_tests;
mod trajectory;
pub use trajectory::*;
mod activity;
pub use activity::{completed_activity_group, tool_activity, tool_group_summary};
#[cfg(test)]
mod refinement_tests;
mod motion;
pub use motion::{disclosure_button, DisclosureMotion};

/// WASM uses GPUI Kit's non-macOS input defaults. Add Command-key aliases
/// for Mac browsers while keeping the existing Control-key bindings.
#[cfg(target_family = "wasm")]
pub fn init_web_input_shortcuts(cx: &mut App) {
    use gpui_component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
    cx.bind_keys([
        KeyBinding::new("cmd-a", SelectAll, Some("Input")),
        KeyBinding::new("cmd-c", Copy, Some("Input")),
        KeyBinding::new("cmd-x", Cut, Some("Input")),
        KeyBinding::new("cmd-v", Paste, Some("Input")),
        KeyBinding::new("cmd-z", Undo, Some("Input")),
        KeyBinding::new("cmd-shift-z", Redo, Some("Input")),
    ]);
}

/// Label and control share the same spacing in every host.
pub fn form_field(label: impl IntoElement, control: impl IntoElement) -> Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_sm().child(label))
        .child(control)
}

pub fn user_message_bubble(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .min_w_0()
        .max_w(rems(USER_BUBBLE_MAX_WIDTH))
        .px_4()
        .py_2p5()
        .rounded_xl()
        .border_1()
        .border_color(theme.border.opacity(0.22))
        .bg(theme.secondary.opacity(0.85))
        .text_sm()
        .text_color(theme.secondary_foreground)
}

/// One session card; callers supply identity, metadata, signals and actions.
pub fn session_card(id: &str, selected: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme().colors;
    div()
        .id(SharedString::from(format!("session-card-{id}")))
        .group("session-card")
        .role(Role::ListItem)
        .relative()
        .flex()
        .items_stretch()
        .w_full()
        .my(rems(0.1875))
        .rounded_lg()
        .bg(if selected {
            theme.sidebar_accent
        } else {
            transparent_black()
        })
        .border_1()
        .border_color(if selected {
            theme.border.opacity(0.5)
        } else {
            transparent_black()
        })
        .when(selected, |el| {
            el.child(
                div()
                    .absolute()
                    .left_1()
                    .top_2()
                    .bottom_2()
                    .w(rems(0.125))
                    .rounded_full()
                    .bg(theme.primary.opacity(0.9)),
            )
        })
        .hover(move |style| {
            style.bg(if selected {
                theme.sidebar_accent
            } else {
                theme.list_hover.opacity(0.7)
            })
        })
}

pub fn session_attention(id: &str, attention: SessionAttention, cx: &App) -> Option<AnyElement> {
    let theme = cx.theme().colors;
    let session_id = id;
    match attention {
        SessionAttention::NeedsYou => Some(
            div()
                .debug_selector({
                    let id = session_id.to_owned();
                    move || format!("session-attention-{id}")
                })
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .px_1p5()
                .py(rems(0.125))
                .rounded_full()
                .bg(theme.warning.opacity(0.12))
                .text_color(theme.warning)
                .child(div().size(rems(0.3125)).rounded_full().bg(theme.warning))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(attention.label()),
                )
                .into_any_element(),
        ),
        SessionAttention::Working => Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .px_1p5()
                .py(rems(0.125))
                .rounded_full()
                .bg(theme.info.opacity(0.1))
                .text_color(theme.foreground)
                .child(Spinner::new().xsmall().color(theme.info))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(attention.label()),
                )
                .into_any_element(),
        ),
        SessionAttention::Ready => Some(
            div()
                .flex_none()
                .px_1p5()
                .py(rems(0.125))
                .rounded_full()
                .bg(theme.muted.opacity(0.35))
                .text_xs()
                .font_medium()
                .text_color(theme.muted_foreground.opacity(0.9))
                .child(attention.label())
                .into_any_element(),
        ),
        SessionAttention::Idle => None,
    }
}

/// The same multiline editing control on desktop and iOS.
pub fn composer_input(input: &Entity<TextareaState>) -> impl IntoElement {
    Textarea::new(input)
        .appearance(false)
        .bordered(false)
        .aria_label("Message the agent")
}

/// Frame for the borderless composer. Hosts pass the text control's current
/// focus state and retain its subscriptions; the input owns keyboard behavior.
pub fn composer_surface(focused: bool, cx: &App) -> Div {
    let theme = cx.theme().colors;
    div()
        .debug_selector(|| "composer-surface".into())
        .w_full()
        .max_w(rems(CHAT_CONTENT_MAX_WIDTH))
        .mx_auto()
        .relative()
        .min_h(rems(4.5))
        .flex()
        .flex_col()
        .justify_between()
        .px_4()
        .pt_3()
        .pb_3()
        .rounded_2xl()
        .border_1()
        .border_color(if focused {
            theme.ring
        } else {
            theme.border.opacity(0.5)
        })
        .bg(theme.popover)
        .when(!focused, |surface| {
            surface.hover(|style| style.border_color(theme.border))
        })
}

/// Message row geometry is shared; hosts can supply richer body/action slots.
pub fn message_row(role: MessageRole) -> Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .when(role == MessageRole::User, |el| {
            el.items_end().my_2p5().px_5()
        })
        .when(role == MessageRole::Assistant, |el| el.my_3().px_5())
}

/// A controlled question item. Its input entity and selections belong to the card owner.
pub fn question_item(
    request_id: &str,
    item: &QuestionItem,
    selected: &[String],
    input: Option<&Entity<InputState>>,
    touch: bool,
    on_toggle: impl Fn(&str, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    let on_toggle = std::rc::Rc::new(on_toggle);
    let options = item
        .options
        .iter()
        .enumerate()
        .map(|(ix, option)| {
            let picked = selected.contains(option);
            let value = option.clone();
            let callback = on_toggle.clone();
            let label = if picked {
                "Selected — activate to remove"
            } else {
                "Toggle this answer"
            };
            Button::new(SharedString::from(format!(
                "question-{request_id}-{}-{option}",
                item.id
            )))
            .debug_selector({
                let id = item.id.clone();
                move || format!("question-option-{id}-{ix}")
            })
            .label(option.clone())
            .small()
            .outline()
            .rounded_full()
            .selected(picked)
            .tooltip(label)
            .accessibility_label(format!("{option}, {label}"))
            .when(touch, |button| button.h_11())
            .on_click(move |_, window, cx| callback(&value, window, cx))
        })
        .collect::<Vec<_>>();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(item.header.clone()),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(item.question.clone()),
        )
        .child(div().flex().flex_wrap().gap_1().children(options))
        .children(input.filter(|_| item.allow_custom).map(|input| {
            div().w_full().child(
                Input::new(input)
                    .small()
                    .when(touch, |input| input.large())
                    .aria_label(format!("Custom answer for {}", item.header)),
            )
        }))
}

/// The shared virtualized transcript viewport; screen-specific row actions are slots.
pub fn transcript_list(
    state: &transcript::TranscriptState,
    render: impl FnMut(usize, &mut Window, &mut App) -> AnyElement + 'static,
) -> gpui::List {
    gpui::list(state.list.clone(), render)
        .w_full()
        .h_full()
        .mx_auto()
        .pt_3()
        .pb_6()
        .with_sizing_behavior(ListSizingBehavior::Auto)
}

/// Permission decisions carry the retained request ID so stale cards cannot answer a replacement.
pub fn permission_card(
    request: &threadlane_protocol::PermissionRequest,
    touch: bool,
    enabled: bool,
    details: Option<AnyElement>,
    on_decision: impl Fn(&str, threadlane_protocol::daemon::PermissionDecision, &mut Window, &mut App)
        + 'static,
    cx: &App,
) -> Stateful<Div> {
    use threadlane_protocol::{daemon::PermissionDecision, PermissionScope};
    let theme = cx.theme().colors;
    let callback = std::rc::Rc::new(on_decision);
    let button = |id: &'static str, label: &'static str, decision, primary| {
        let callback = callback.clone();
        let request_id = request.id.clone();
        let scope = match decision {
            PermissionDecision::Deny => "Deny this request",
            PermissionDecision::AllowOnce => "Allow this request once",
            PermissionDecision::AllowSession => "Allow this capability for this session",
            PermissionDecision::AllowAlways => "Always allow this capability for this project",
        };
        Button::new(SharedString::from(format!("{id}-{request_id}")))
            .label(label)
            .tooltip(scope)
            .accessibility_label(format!("{scope}: {}", request.title))
            .small()
            .rounded_md()
            .disabled(!enabled)
            .when(primary, |b| b.primary())
            .when(touch, |b| b.h_11())
            .on_click(move |_, window, cx| callback(&request_id, decision, window, cx))
    };
    div()
        .id(SharedString::from(format!(
            "permission-prompt-card-{}",
            request.id
        )))
        .role(Role::Alert)
        .aria_label("Permission request")
        .debug_selector(|| "permission-card".into())
        .w_full()
        .max_w(rems(CHAT_CONTENT_MAX_WIDTH))
        .mx_auto()
        .px_3p5()
        .py_2p5()
        .rounded_xl()
        .border_1()
        .border_color(theme.warning.opacity(0.4))
        .bg(theme.secondary.opacity(0.35))
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .debug_selector(|| "permission-title".into())
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(request.title.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(request.detail.clone()),
                ),
        )
        .children(details)
        .child(
            div()
                .debug_selector(|| "permission-actions".into())
                .w_full()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(button(
                    "permission-deny",
                    "Deny",
                    PermissionDecision::Deny,
                    false,
                ))
                .when(request.scopes.contains(&PermissionScope::Once), |el| {
                    el.child(button(
                        "permission-allow-once",
                        "Allow once",
                        PermissionDecision::AllowOnce,
                        true,
                    ))
                })
                .when(request.scopes.contains(&PermissionScope::Session), |el| {
                    el.child(
                        button(
                            "permission-allow-session",
                            "Allow session",
                            PermissionDecision::AllowSession,
                            false,
                        )
                        .debug_selector(|| "permission-inline-session".into()),
                    )
                })
                .when(request.scopes.contains(&PermissionScope::Always), |el| {
                    el.child(
                        button(
                            "permission-allow-always",
                            "Always allow",
                            PermissionDecision::AllowAlways,
                            false,
                        )
                        .debug_selector(|| "permission-inline-always".into()),
                    )
                }),
        )
}

pub fn question_surface(cx: &App) -> Div {
    let theme = cx.theme().colors;
    div()
        .w_full()
        .max_w(rems(QUESTION_CARD_MAX_WIDTH))
        .mx_auto()
        .px_3p5()
        .py_3()
        .rounded_xl()
        .border_1()
        .border_color(theme.border.opacity(0.5))
        .bg(theme.popover)
        .flex()
        .flex_col()
        .gap_2()
}

pub struct SessionIdentity {
    pub title: String,
    pub tooltip: String,
}

/// Title shown for a session that has not been named yet. Discovery seeds
/// `SessionInfo::title` with the raw session id until a title is generated;
/// that id is an implementation detail, not something to show a user.
pub const UNTITLED_SESSION_TITLE: &str = "New task";

pub fn session_display_title(session: &SessionInfo) -> String {
    if session.title.trim().is_empty() || session.title == session.id {
        UNTITLED_SESSION_TITLE.to_string()
    } else {
        session.title.clone()
    }
}

pub fn session_identity(session: &SessionInfo) -> SessionIdentity {
    let Some(issue) = session.github_issue.as_ref() else {
        let title = session_display_title(session);
        return SessionIdentity {
            tooltip: title.clone(),
            title,
        };
    };
    let prefix = format!("#{}", issue.number);
    let title = session.title.trim();
    let issue_title = title
        .strip_prefix(&prefix)
        .filter(|rest| rest.is_empty() || rest.chars().next().is_some_and(char::is_whitespace))
        .map(str::trim_start)
        .unwrap_or(title);
    let title = if issue_title.is_empty() {
        prefix.clone()
    } else {
        format!("{prefix} {issue_title}")
    };
    SessionIdentity {
        tooltip: format!("{}/{}\n{}", issue.owner, issue.repo, title),
        title,
    }
}

/// Shared virtualized navigation list and its scrollbar. Grouping belongs to the owner.
pub fn session_list(
    state: ListState,
    render: impl FnMut(usize, &mut Window, &mut App) -> AnyElement + 'static,
) -> Div {
    div()
        .relative()
        .size_full()
        .pt_2()
        .child(
            list(state.clone(), render)
                .size_full()
                .pb_3()
                .with_sizing_behavior(ListSizingBehavior::Auto),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .child(gpui_component::scroll::Scrollbar::vertical(&state)),
        )
}

pub fn reasoning_token_badge(is_streaming: bool, reasoning_len: usize) -> String {
    if is_streaming {
        return "thinking…".to_string();
    }
    let approx_tokens = (reasoning_len + 3) / 4;
    if approx_tokens == 1 {
        "~1 token".to_string()
    } else {
        format!("~{approx_tokens} tokens")
    }
}

pub fn tool_activity_glyph(category: &str) -> &'static str {
    match category {
        "Error" => "!",
        "Working" | "Thinking" => "◌",
        "Completed" | "Result" | "Edited" | "Created" | "Ran" | "Loaded" | "Explored" => "✓",
        _ => "•",
    }
}

pub fn reasoning_card(
    msg: &threadlane_protocol::daemon::ChatMessageInfo,
    detail: Option<AnyElement>,
    touch: bool,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme().colors;
    let is_streaming = msg.streaming;
    let is_expanded = msg.reasoning_expanded;
    let approx_badge = reasoning_token_badge(
        is_streaming,
        msg.reasoning_content.as_ref().map_or(0, String::len),
    );
    let disclosure_label = format!(
        "{} thought process, {approx_badge}",
        if is_expanded { "Collapse" } else { "Expand" },
    );
    let token_badge = div()
        .debug_selector(|| "reasoning-token-label".into())
        .min_w_0()
        .truncate()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(approx_badge);

    let header = Button::new(SharedString::from(format!("reasoning-toggle-{}", msg.id)))
        .debug_selector(|| "reasoning-disclosure".into())
        .accessibility_label(disclosure_label.clone())
        .tooltip(disclosure_label)
        .ghost()
        .small()
        .w_full()
        .open(is_expanded)
        .when(is_expanded, |button| {
            button.px_3().rounded_none().bg(theme.muted.opacity(0.25))
        })
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .flex_1()
                .child(if is_streaming {
                    gpui_component::spinner::Spinner::new()
                        .xsmall()
                        .color(theme.primary)
                        .into_any_element()
                } else {
                    gpui_component::Icon::default()
                        .data(gpui_kit_assets::__private::Asterisk.1)
                        .xsmall()
                        .text_color(theme.muted_foreground)
                        .into_any_element()
                })
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child(if is_streaming {
                            "Thinking…"
                        } else {
                            "Thought process"
                        }),
                )
                .when(!is_streaming, |row| row.child(token_badge)),
        )
        .child(
            crate::motion::chevron(SharedString::from(format!("reasoning-{}", msg.id)), is_expanded),
        )
        .when(touch, |button| button.h_11())
        .on_click(move |_, window, cx| on_toggle(window, cx));
    // Collapsed reasoning reads as a quiet transcript row, like tool
    // activity; the bordered card only appears around expanded content.
    let container = div()
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .when(is_expanded || detail.is_some(), |el| {
            el.rounded_xl()
                .border_1()
                .border_color(theme.border.opacity(0.3))
                .bg(theme.muted.opacity(0.14))
        });

    container.child(header).children(detail).into_any_element()
}

pub fn reasoning_detail(cx: &App) -> gpui_component::scroll::Scrollable<Div> {
    let theme = cx.theme().colors;
    div()
        .p_3()
        .max_h(rems(21.25))
        .border_t_1()
        .border_color(theme.border.opacity(0.35))
        .bg(theme.background.opacity(0.4))
        .text_xs()
        .text_color(theme.muted_foreground)
        .overflow_y_scrollbar()
}

pub fn tool_detail(cx: &App) -> gpui_component::scroll::Scrollable<Div> {
    let theme = cx.theme().colors;
    div()
        .mt_1()
        .p_2p5()
        .max_h(rems(15.0))
        .rounded_lg()
        .border_1()
        .border_color(theme.border.opacity(0.5))
        .bg(theme.title_bar)
        .text_xs()
        .text_color(theme.muted_foreground)
        .overflow_y_scrollbar()
}

pub mod context_meter;
mod environment;
pub use environment::*;
mod efficiency;
pub use efficiency::token_efficiency;

mod plan;
pub use plan::{plan_tracker, plan_tracker_texts};

pub fn truncate_preview_text(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let clamped = max_chars.max(2) - 1;
    let truncated: String = text.chars().take(clamped).collect();
    format!("{}…", truncated.trim_end())
}

pub mod automation;

pub mod automation_form;

pub mod settings;

pub mod github;
