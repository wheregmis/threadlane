//! Workspace presentation shared by the desktop service host and WASM preview.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_component::resizable::{
    h_resizable, v_resizable, resizable_panel, ResizablePanelGroup, ResizableState,
};
use gpui_component::Icon;
use gpui_component::{ActiveTheme, Disableable, Selectable, Sizable};
use gpui_component::menu::PopupMenuItem;
use threadlane_protocol::ReasoningEffort;
use threadlane_ui_theme::CHAT_CONTENT_MAX_WIDTH;

/// Shared responsive geometry. Hosts retain panel visibility and resize preferences.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorkspaceLayout {
    pub sidebar_visible: bool,
    pub sidebar_available: bool,
    pub right_panel_focus: bool,
    pub environment_width: Pixels,
    pub header_inset: Pixels,
}

impl WorkspaceLayout {
    pub fn new(
        width: Pixels,
        rem: Pixels,
        sidebar_width: Pixels,
        sidebar_collapsed: bool,
        right_panel_visible: bool,
    ) -> Self {
        let sidebar_width = sidebar_width.clamp(rem * 12.0, rem * 18.0);
        let required_content = rem * if right_panel_visible { 48.0 } else { 28.0 };
        let sidebar_available = width >= sidebar_width + required_content;
        let sidebar_visible = !sidebar_collapsed && sidebar_available;
        Self {
            sidebar_visible,
            sidebar_available,
            right_panel_focus: right_panel_visible && width < rem * 48.0,
            environment_width: if right_panel_visible {
                Pixels::ZERO
            } else {
                width
                    - if sidebar_visible {
                        sidebar_width
                    } else {
                        Pixels::ZERO
                    }
            },
            header_inset: rem
                * if sidebar_visible {
                    0.875
                } else {
                    threadlane_ui_theme::WINDOW_CONTROLS_CONTENT_INSET
                },
        }
    }
}

/// Restore each visible split after its current container has been measured.
/// Call from the host's guarded next-frame callback, never while building elements.
pub fn restore_workspace_panel_sizes(
    panels: impl IntoIterator<Item = (bool, Entity<ResizableState>, usize, f32)>,
    rem: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    for (visible, state, index, preferred) in panels {
        if visible {
            state.update(cx, |state, cx| {
                state.resize_panel(index, rem * preferred, window, cx)
            });
        }
    }
}

/// Floating toggle pinned to the top-left of the content area (beside the
/// macOS traffic lights), over the sidebar header or, when collapsed, the
/// chat header's leading inset.
pub fn workspace_sidebar_toggle(collapsed: bool, available: bool) -> Button {
    sidebar_toggle_button(collapsed, available)
        .absolute()
        .top(rems(if threadlane_ui_theme::APP_CAPTION_STRIP { 0.5 } else { 0.5625 }))
        .left(rems(threadlane_ui_theme::SIDEBAR_TOGGLE_LEFT))
}

fn sidebar_toggle_button(collapsed: bool, available: bool) -> Button {
    let hint = if !available {
        "Sidebar needs a wider window. Use the command palette to switch sessions."
    } else if collapsed {
        "Expand sidebar"
    } else {
        "Collapse sidebar"
    };
    Button::new("sidebar-collapse-toggle")
        .debug_selector(|| "sidebar-collapse-toggle".into())
        .accessibility_label(hint)
        .icon(gpui_component::IconName::PanelLeft)
        .tooltip(hint)
        .disabled(!available)
        .ghost()
        .xsmall()
}

/// Keep the selected inspector usable when chat and panel cannot fit side by side.
/// Compact focus: the right panel replaces the conversation. Its own header carries
/// Back ([`crate::right_panel_back_button`]), so no second header band is stacked here.
pub fn workspace_right_panel_focus(panel: impl IntoElement) -> Div {
    div()
        .debug_selector(|| "workspace-right-panel-focus".into())
        .flex()
        .flex_col()
        .size_full()
        .child(div().flex_1().min_h_0().child(panel))
}

pub fn workspace_sidebar_split(
    state: &Entity<ResizableState>,
    sidebar: impl IntoElement,
    content: impl IntoElement,
    rem: Pixels,
) -> ResizablePanelGroup {
    h_resizable("workspace-sidebar-main-split")
        .with_state(state)
        .child(
            resizable_panel()
                .size(rem * 16.5)
                .size_range(rem * 12.0..rem * 18.0)
                .child(sidebar),
        )
        .child(resizable_panel().child(content))
}

/// The shared chat/right-panel split; hosts retain resize preferences and visibility.
pub fn workspace_right_panel_split(
    state: &Entity<ResizableState>, content: impl IntoElement, panel: impl IntoElement,
    rem: Pixels, width: Pixels,
) -> ResizablePanelGroup {
    h_resizable("workspace-chat-right-split").with_state(state)
        .child(resizable_panel().size_range(rem * 24.0..Pixels::MAX).child(content))
        .child(resizable_panel().size(rem * 22.0)
            .size_range(rem * 18.0..(width * 0.42).max(rem * 18.0)).child(panel))
}

pub fn workspace_with_status(content: impl IntoElement, status: impl IntoElement) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(div().flex_1().min_h_0().child(content))
        .child(status)
}

pub fn workspace_terminal_split(
    state: &Entity<ResizableState>,
    content: impl IntoElement,
    terminal: impl IntoElement,
    rem: Pixels,
    height: Pixels,
) -> ResizablePanelGroup {
    v_resizable("workspace-main-bottom-split")
        .with_state(state)
        .child(resizable_panel().child(content))
        .child(
            resizable_panel()
                .size(rem * 14.0)
                .size_range(rem * 8.0..(height - rem * 24.0).max(rem * 8.0))
                .child(terminal),
        )
}

pub fn conversation_surface(cx: &App) -> Stateful<Div> {
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .h_full()
        .min_w_0()
        .min_h_0()
        .bg(cx.theme().background)
        .id("conversation-surface")
}

pub fn conversation_column(environment: bool) -> Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .max_w(rems(
            CHAT_CONTENT_MAX_WIDTH + if environment { 4.0 } else { 0.0 },
        ))
        .min_h_0()
        .min_w_0()
}

pub fn chat_header_surface(left_padding: Pixels, cx: &App) -> Div {
    let theme = cx.theme().colors;
    div()
        .min_h(rems(3.0))
        .flex_none()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .px_4()
        .pl(left_padding)
        .pr_32()
        .border_b_1()
        .border_color(theme.border.opacity(0.5))
        .bg(theme.title_bar)
}

pub fn chat_header_identity(
    title: impl Into<SharedString>,
    full_title: impl Into<SharedString>,
    status: Option<AnyElement>,
    cx: &App,
) -> Div {
    let full_title = full_title.into();
    let theme = cx.theme().colors;
    div()
        .flex_1()
        .flex()
        .items_center()
        .gap_2()
        .justify_start()
        .min_w_0()
        .child(
            Icon::default()
                .path("icons/tabs/chat.svg")
                .size_4()
                .text_color(theme.primary),
        )
        .child(
            div()
                .id("chat-header-title")
                .truncate()
                .text_sm()
                .line_height(rems(1.125))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(full_title.clone()).build(window, cx)
                })
                .child(title.into()),
        )
        .children(status)
}

pub fn assistant_message_content() -> Div {
    div().w_full().min_w_0().flex().flex_col().gap_2p5()
}

pub fn message_content_column() -> Div {
    div().w_full().flex().flex_col().gap_1()
}

pub fn chat_error_card(summary: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme().colors;
    div()
        .w_full()
        .p_3p5()
        .rounded_xl()
        .bg(theme.danger.opacity(0.08))
        .border_1()
        .border_color(theme.danger.opacity(0.4))
        .shadow_sm()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.danger)
                .child("Turn stopped"),
        )
        .child(
            div()
                .mt_1()
                .text_sm()
                .text_color(theme.foreground)
                .child(summary.into()),
        )
}

pub fn chat_tab_button(
    id: &'static str,
    label: impl Into<SharedString>,
    selected: bool,
    cx: &App,
) -> Button {
    let theme = cx.theme().colors;
    Button::new(id)
        .debug_selector(move || id.into())
        .label(label)
        .custom(
            ButtonCustomVariant::new(cx)
                .color(theme.muted.opacity(0.45))
                .hover(theme.secondary)
                .active(theme.secondary_active),
        )
        .small()
        .rounded_full()
        .selected(selected)
}

pub fn composer_container(cx: &App) -> Div {
    div()
        .w_full()
        .max_w(rems(CHAT_CONTENT_MAX_WIDTH))
        .mx_auto()
        .flex_none()
        .flex()
        .flex_col()
        .px_5()
        .pt_2()
        .pb_4()
        .bg(cx.theme().background)
}

pub fn composer_toolbar(cx: &App) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .flex_wrap()
        .mt_2()
        .pt_2p5()
        .border_t_1()
        .border_color(cx.theme().border.opacity(0.22))
}

pub fn composer_picker_group() -> Div {
    div().flex().items_center().gap_1().min_w_0().flex_wrap()
}

pub fn composer_actions_group() -> Div {
    div().flex().items_center().gap_1().flex_wrap()
}

pub fn composer_shortcuts(label: impl Into<SharedString>, cx: &App) -> Stateful<Div> {
    let label = label.into();
    div()
        .id("composer-shortcuts")
        .debug_selector(|| "composer-shortcuts".into())
        .role(Role::Note)
        .aria_label(label.clone())
        .mt_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(label)
}

pub fn composer_model_button(
    label: impl Into<SharedString>,
    available: bool,
    selected: bool,
    disabled: bool,
) -> Button {
    let label = label.into();
    Button::new("composer-model-picker")
        .debug_selector(|| "composer-model-picker".into())
        .small()
        .label(label.clone())
        .accessibility_label(format!("Model: {label}"))
        .dropdown_caret(true)
        .ghost()
        .rounded_full()
        .selected(selected)
        .disabled(disabled)
        .max_w(rems(12.5))
        .tooltip(if available {
            format!("Model: {label}")
        } else {
            "No models available — connect a provider in Settings".into()
        })
}

pub fn composer_effort_button(effort: ReasoningEffort) -> Button {
    // A bare level ("Medium") does not say what it measures.
    let text = effort_text(effort);
    Button::new("composer-reasoning-effort-picker")
        .debug_selector(|| "composer-reasoning-effort-picker".into())
        .small()
        .icon(Icon::default().path("icons/effort.svg"))
        .label(text.clone())
        // The accessible name starts with the visible text (label in name).
        .accessibility_label(format!("{text}, reasoning effort"))
        .tooltip("Reasoning effort")
        .dropdown_caret(true)
        .ghost()
        .rounded_full()
        .max_w(rems(12.5))
}

/// One row of the reasoning-effort menu, worded like the button.
pub fn composer_effort_menu_item(effort: ReasoningEffort, selected: bool) -> PopupMenuItem {
    PopupMenuItem::new(effort_text(effort)).checked(selected)
}

/// Visible text for a reasoning-effort level: "Medium effort", "Thinking off".
/// Runtime levels (`Other`) are stored lowercase, so they are capitalized to
/// sit beside the built-in ones.
fn effort_text(effort: ReasoningEffort) -> String {
    match effort {
        ReasoningEffort::Off => "Thinking off".into(),
        other => {
            let label = other.label();
            let mut chars = label.chars();
            let first = chars.next().map(|c| c.to_uppercase().collect::<String>());
            format!("{}{} effort", first.unwrap_or_default(), chars.as_str())
        }
    }
}

pub fn composer_mode_button(label: impl Into<SharedString>, enabled: bool) -> Button {
    let label = label.into();
    Button::new("composer-mode-picker")
        .debug_selector(|| "composer-mode-picker".into())
        .small()
        .label(label.clone())
        .accessibility_label(format!("Mode: {label}"))
        .tooltip(if enabled {
            "Session mode: Agent or Fusion"
        } else {
            "Attach a project to switch session modes"
        })
        .dropdown_caret(true)
        .ghost()
        .rounded_full()
        .disabled(!enabled)
}

pub fn composer_send_button(enabled: bool, hint: impl Into<SharedString>) -> Button {
    let hint = hint.into();
    Button::new("send-btn")
        .debug_selector(|| "send-btn".into())
        .size_8()
        .rounded_full()
        .icon(gpui_component::IconName::ArrowUp)
        .accessibility_label(hint.clone())
        .tooltip(hint)
        .when(enabled, |button| button.primary())
        .when(!enabled, |button| button.ghost().disabled(true))
}

pub fn composer_context_bar() -> Div {
    div()
        .w_full()
        .max_w(rems(CHAT_CONTENT_MAX_WIDTH))
        .mx_auto()
        .mb_2p5()
        .px_1()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1p5()
}

pub fn composer_project_button(
    label: impl Into<SharedString>,
    path: impl Into<SharedString>,
) -> Button {
    let label = label.into();
    let path = path.into();
    Button::new("composer-project-chip")
        .icon(gpui_component::IconName::Folder)
        .label(label.clone())
        .accessibility_label(format!("Project: {label} · {path}"))
        .dropdown_caret(true)
        .ghost()
        .xsmall()
        .rounded_full()
        .max_w(rems(10.0))
        .tooltip(format!("Project: {path}"))
}

pub fn composer_work_mode_button(label: &'static str, worktree: bool) -> Button {
    Button::new("composer-workmode-chip")
        .icon(if worktree { Icon::default().path("icons/git/branch.svg") }
            else { Icon::new(gpui_component::IconName::SquareTerminal) })
        .label(label).accessibility_label(format!("Execution location: {label}"))
        .tooltip(format!("Execution location: {label} · Where new tasks run: local checkout or an isolated worktree"))
        .dropdown_caret(true).ghost().xsmall().rounded_full()
}

pub fn composer_worktree_base_button(label: impl Into<SharedString>, available: bool) -> Button {
    let label = label.into();
    Button::new("composer-worktree-base")
        .debug_selector(|| "composer-worktree-base".into())
        .max_w(rems(14.0))
        .label(format!("Base: {label}"))
        .accessibility_label(format!("Worktree base branch: {label}"))
        .tooltip("Create the worktree from this branch’s committed changes")
        .dropdown_caret(true)
        .ghost()
        .xsmall()
        .rounded_full()
        .disabled(!available)
}

pub fn skills_chip_label(active_count: usize) -> String {
    if active_count == 0 {
        "Skills".into()
    } else {
        format!(
            "{active_count} {}",
            if active_count == 1 { "Skill" } else { "Skills" }
        )
    }
}

pub fn composer_skills_button(active_count: usize) -> Button {
    let description = format!("Manage workspace skills · {active_count} active");
    Button::new("composer-skills-chip")
        .icon(gpui_component::IconName::BookOpen)
        .label(skills_chip_label(active_count))
        .accessibility_label(description.clone())
        .tooltip(description)
        .xsmall()
        .ghost()
        .rounded_full()
}

pub fn composer_branch_label(branch: impl Into<SharedString>, cx: &App) -> Stateful<Div> {
    let branch = branch.into();
    let tooltip = branch.clone();
    div()
        .id("composer-branch")
        .role(Role::Note)
        .aria_label(format!("Branch: {branch}"))
        .min_w_0()
        .flex_1()
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .truncate()
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
        })
        .child(
            Icon::default()
                .path("icons/git/branch.svg")
                .xsmall()
                .text_color(cx.theme().muted_foreground),
        )
        .child(branch)
}

#[cfg(test)]
mod effort_text_tests {
    use super::effort_text;
    use threadlane_protocol::ReasoningEffort;

    #[test]
    fn effort_text_names_the_setting() {
        assert_eq!(effort_text(ReasoningEffort::Off), "Thinking off");
        assert_eq!(effort_text(ReasoningEffort::Medium), "Medium effort");
        assert_eq!(effort_text(ReasoningEffort::XHigh), "XHigh effort");
        assert_eq!(effort_text(ReasoningEffort::Other("auto")), "Auto effort");
    }
}
