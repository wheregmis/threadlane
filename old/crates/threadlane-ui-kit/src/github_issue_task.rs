//! Controlled issue task configuration and destructive confirmation.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_component::dialog::{AlertDialog, Dialog};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, Disableable, WindowExt};
use std::rc::Rc;
use threadlane_protocol::{OrchestratorMode, ReasoningEffort};

pub const ISSUE_TASK_DESCRIPTION: &str = "The agent works in an isolated worktree, pushes a PR, marks it ready after local checks, then monitors CI and replies to reviews until mergeable or blocked. It does not merge.";
pub const ISSUE_TASK_BRANCH_DISCLOSURE: &str =
    "A unique six-character suffix is assigned when the task starts.";
const MODE_HELP: &str = "Agent runs directly on the selected model; Fusion delegates work to the configured Fusion model.";

#[derive(Clone)]
pub struct IssueTaskModel {
    pub id: String,
    pub label: String,
    pub provider: String,
}

#[derive(Clone)]
pub struct GitHubIssueStartForm {
    pub identity: String,
    pub title: String,
    pub branch: String,
    pub model: String,
    pub model_label: String,
    pub models: Vec<IssueTaskModel>,
    pub effort: ReasoningEffort,
    /// None hides reasoning for models without that capability.
    pub efforts: Option<Vec<ReasoningEffort>>,
    pub mode: OrchestratorMode,
    pub starting: bool,
    pub disabled_reason: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubIssueStartAction {
    Model(String),
    Effort(ReasoningEffort),
    Mode(OrchestratorMode),
}

fn picker(id: &'static str, label: &str, value: &str, busy: bool) -> Button {
    Button::new(id)
        .debug_selector(move || id.into())
        .label(value.to_owned())
        .accessibility_label(format!("{label}: {value}"))
        .tooltip(format!("{label}: {value}"))
        .dropdown_caret(true)
        .w_full()
        .disabled(busy)
}

pub fn github_issue_start_form(
    form: GitHubIssueStartForm,
    on_action: impl Fn(GitHubIssueStartAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let callback = Rc::new(on_action);
    let model_callback = callback.clone();
    let no_models = form.models.is_empty();
    let models = form.models;
    let selected = form.model;
    let model = picker(
        "issue-task-model",
        "Model",
        &form.model_label,
        form.starting || no_models,
    )
    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, _| {
        let mut provider = None;
        models.iter().fold(
            menu.max_h(window.rem_size() * 20.0).scrollable(true),
            |menu, option| {
                let menu = if provider == Some(&option.provider) {
                    menu
                } else {
                    provider = Some(&option.provider);
                    menu.item(PopupMenuItem::label(option.provider.clone()))
                };
                let id = option.id.clone();
                let callback = model_callback.clone();
                menu.item(
                    PopupMenuItem::new(option.label.clone())
                        .checked(id == selected)
                        .on_click(move |_, window, cx| {
                            callback(GitHubIssueStartAction::Model(id.clone()), window, cx)
                        }),
                )
            },
        )
    });
    let mode_callback = callback.clone();
    let mode = crate::choice_menu(
        picker(
            "issue-task-mode",
            "Session mode",
            form.mode.label(),
            form.starting,
        )
        .tooltip(MODE_HELP)
        .accessibility_label(format!("Session mode: {}. {MODE_HELP}", form.mode.label())),
        [OrchestratorMode::Normal, OrchestratorMode::Fusion]
            .into_iter()
            .map(|mode| (mode, mode.label().to_owned()))
            .collect(),
        form.mode,
        move |mode, window, cx| mode_callback(GitHubIssueStartAction::Mode(mode), window, cx),
    );
    let theme = cx.theme().colors;
    let field = |label: &'static str, control: AnyElement| {
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(label)
            .child(control)
    };
    div()
        .id("github-issue-start-form")
        .debug_selector(|| "github-issue-start-form".into())
        .role(Role::Group)
        .aria_label(format!(
            "Start task from GitHub issue: {}. {}",
            form.identity, form.title
        ))
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_4()
        .text_sm()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(form.identity),
                )
                .child(div().font_weight(FontWeight::SEMIBOLD).child(form.title)),
        )
        .child(field("Model", model.into_any_element()))
        .children(form.efforts.map(|efforts| {
            field(
                "Reasoning effort",
                crate::choice_menu(
                    picker(
                        "issue-task-effort",
                        "Reasoning effort",
                        form.effort.label(),
                        form.starting,
                    ),
                    efforts
                        .into_iter()
                        .map(|effort| (effort, effort.label().to_owned()))
                        .collect(),
                    form.effort,
                    move |effort, window, cx| {
                        callback(GitHubIssueStartAction::Effort(effort), window, cx)
                    },
                )
                .into_any_element(),
            )
        }))
        .child(field("Mode", mode.into_any_element()))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child("Branch")
                .child(
                    div()
                        .id("github-issue-start-branch")
                        .debug_selector(|| "github-issue-start-branch".into())
                        .role(Role::Group)
                        .aria_label(format!("Branch: {}", form.branch))
                        .w_full()
                        .min_w_0()
                        .truncate()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_xs()
                        .tooltip({
                            let branch = form.branch.clone();
                            move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(branch.clone())
                                    .build(window, cx)
                            }
                        })
                        .child(form.branch),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(ISSUE_TASK_BRANCH_DISCLOSURE),
                ),
        )
        .child(
            div()
                .text_color(theme.muted_foreground)
                .child(ISSUE_TASK_DESCRIPTION),
        )
        .children(
            form.disabled_reason
                .map(|reason| div().text_xs().text_color(theme.warning).child(reason)),
        )
        .children(form.error.map(|error| {
            div()
                .id("github-issue-start-error")
                .debug_selector(|| "github-issue-start-error".into())
                .role(Role::Alert)
                .aria_label(error.clone())
                .text_color(theme.danger)
                .child(error)
        }))
}

pub fn github_issue_start_dialog(
    dialog: Dialog,
    starting: bool,
    label: &'static str,
    disabled_reason: Option<String>,
    on_start: impl Fn(&mut Window, &mut App) + 'static,
    on_dismiss: impl Fn(&mut App) + 'static,
) -> Dialog {
    let start = Rc::new(on_start);
    let confirm = start.clone();
    let dismiss = Rc::new(on_dismiss);
    let cancel = dismiss.clone();
    let enabled = !starting && disabled_reason.is_none();
    let help = disabled_reason.unwrap_or_else(|| {
        if starting {
            "Starting task…".into()
        } else {
            label.into()
        }
    });
    dialog
        .title("Start task from issue")
        .overlay_closable(false)
        .keyboard(!starting)
        .close_button(!starting)
        .footer(
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("cancel-issue-task")
                        .debug_selector(|| "cancel-issue-task".into())
                        .label("Cancel")
                        .disabled(starting)
                        .on_click(move |_, window, cx| {
                            cancel(cx);
                            window.close_dialog(cx);
                        }),
                )
                .child(
                    Button::new("confirm-issue-task")
                        .debug_selector(|| "confirm-issue-task".into())
                        .primary()
                        .label(if starting { "Starting task…" } else { label })
                        .loading(starting)
                        .disabled(!enabled)
                        .tooltip(help.clone())
                        .accessibility_label(help)
                        .on_click(move |_, window, cx| {
                            if enabled {
                                confirm(window, cx);
                            }
                        }),
                ),
        )
        .on_ok(move |_, window, cx| {
            if enabled {
                start(window, cx);
            }
            false
        })
        .on_close(move |_, _, cx| dismiss(cx))
}

/// A bare Enter never confirms deletion. Activate the labeled Delete button.
pub fn github_issue_delete_dialog(
    alert: AlertDialog,
    identity: impl Into<SharedString>,
    title: impl Into<SharedString>,
    on_confirm: impl Fn(&mut Window, &mut App) -> bool + 'static,
) -> AlertDialog {
    let identity = identity.into();
    let title = title.into();
    alert
        .title("Delete issue?")
        .description("Permanently delete this issue on GitHub? This cannot be undone.")
        .child(
            div()
                .id("github-issue-delete-identity")
                .debug_selector(|| "github-issue-delete-identity".into())
                .role(Role::Group)
                .aria_label(format!(
                    "Delete issue? {identity}. {title}. Permanently deletes this issue on GitHub. This cannot be undone."
                ))
                .text_sm()
                .child(identity),
        )
        .child(
            div()
                .debug_selector(|| "github-issue-delete-title".into())
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .footer(
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("cancel-delete-issue")
                        .debug_selector(|| "cancel-delete-issue".into())
                        .label("Cancel")
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .child(
                    Button::new("confirm-delete-issue")
                        .debug_selector(|| "confirm-delete-issue".into())
                        .label("Delete issue")
                        .with_variant(ButtonVariant::Danger)
                        .on_click(move |_, window, cx| {
                            if on_confirm(window, cx) {
                                window.close_dialog(cx);
                            }
                        }),
                ),
        )
        .on_ok(|_, _, _| false)
}
