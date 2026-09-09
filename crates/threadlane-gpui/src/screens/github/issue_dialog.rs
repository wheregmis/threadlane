use std::path::PathBuf;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Disableable, Sizable, WindowExt};
use threadlane_git::GitHubIssueRef;

use crate::state::AppState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueStartConfirmation {
    pub copy: String,
    pub model: String,
    pub reasoning_effort: String,
    pub branch_preview: String,
    pub branch_disclosure: String,
    pub start_enabled: bool,
    pub start_disabled_reason: Option<String>,
    pub show_open_task: bool,
    pub start_label: &'static str,
}

pub fn issue_start_confirmation(
    issue: &GitHubIssueRef,
    title: &str,
    model: &str,
    reasoning_effort: &str,
    is_git_repository: bool,
    has_linked_task: bool,
) -> IssueStartConfirmation {
    IssueStartConfirmation {
        copy: "Threadlane will create an isolated worktree, then the agent will push its issue branch to origin and open a draft pull request on GitHub after completing the task.".into(),
        model: model.into(),
        reasoning_effort: reasoning_effort.into(),
        branch_preview: AppState::issue_branch_name(issue.number, title, "xxxxxx"),
        branch_disclosure: "A unique six-character suffix is assigned when the task starts.".into(),
        start_enabled: is_git_repository,
        start_disabled_reason: (!is_git_repository)
            .then_some("This project is not a Git repository.".into()),
        show_open_task: has_linked_task,
        start_label: if has_linked_task {
            "Start another, push & create draft PR"
        } else {
            "Start, push & create draft PR"
        },
    }
}

pub fn issue_start_activation(
    start_enabled: bool,
    start: impl FnOnce() -> Result<(), String>,
) -> Result<bool, String> {
    if !start_enabled {
        return Ok(false);
    }
    start().map(|()| true)
}

pub fn issue_start_dialog_result(result: Result<bool, String>, on_error: impl FnOnce(String)) -> bool {
    match result {
        Ok(started) => started,
        Err(error) => {
            on_error(error);
            false
        }
    }
}

pub struct IssueStartDialog {
    pub model: Entity<AppState>,
    pub work_dir: PathBuf,
    pub issue: GitHubIssueRef,
    pub title: String,
    pub confirmation: IssueStartConfirmation,
    pub error: Option<String>,
}

impl IssueStartDialog {
    pub fn start(&mut self, cx: &mut Context<Self>) -> bool {
        let result = issue_start_activation(self.confirmation.start_enabled, || {
            self.model.update(cx, |state, cx| {
                let result = state.start_issue_work(
                    self.work_dir.clone(),
                    self.issue.clone(),
                    self.title.clone(),
                );
                if let Err(error) = &result {
                    state.session_status = Some(error.clone());
                }
                cx.notify();
                result.map(|_| ())
            })
        });
        issue_start_dialog_result(result, |error| {
            self.error = Some(error);
            cx.notify();
        })
    }
}

pub fn activate_issue_start_dialog(dialog: &Entity<IssueStartDialog>, cx: &mut App) -> bool {
    dialog.update(cx, |dialog, cx| dialog.start(cx))
}

impl Render for IssueStartDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let confirmation = &self.confirmation;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .text_sm()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(confirmation.copy.clone()),
            )
            .child(format!("Issue: #{} {}", self.issue.number, self.title))
            .child(format!("Model: {}", confirmation.model))
            .child(format!(
                "Reasoning effort: {}",
                confirmation.reasoning_effort
            ))
            .child(format!("Branch preview: {}", confirmation.branch_preview))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(confirmation.branch_disclosure.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .p_2()
                    .bg(theme.secondary)
                    .rounded_md()
                    .child("Isolated worktree")
                    .child(Tag::new().small().child("Locked")),
            )
            .children(confirmation.start_disabled_reason.as_ref().map(|reason| {
                div()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(reason.clone())
            }))
            .children(self.error.as_ref().map(|error| {
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_color(theme.danger)
                    .child(error.clone())
                    .child(
                        Button::new("retry-issue-task")
                            .label("Retry")
                            .small()
                            .on_click(cx.listener(|this, _event, window, cx| {
                                if this.start(cx) {
                                    window.close_dialog(cx);
                                }
                            })),
                    )
            }))
    }
}

pub fn open_issue_start_dialog(
    model: Entity<AppState>,
    work_dir: PathBuf,
    issue: GitHubIssueRef,
    title: String,
    has_linked_task: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let (selected_model, reasoning_effort) = {
        let state = model.read(cx);
        (state.selected_model.clone(), state.reasoning_effort.label())
    };
    let confirmation = issue_start_confirmation(
        &issue,
        &title,
        &selected_model,
        reasoning_effort,
        threadlane_git::is_git_repo(&work_dir),
        has_linked_task,
    );
    let start_enabled = confirmation.start_enabled;
    let start_label = confirmation.start_label;
    let disabled_reason = confirmation.start_disabled_reason.clone();
    let dialog_state = cx.new(|_| IssueStartDialog {
        model,
        work_dir,
        issue,
        title,
        confirmation,
        error: None,
    });
    window.open_dialog(cx, move |dialog, _window, _cx| {
        let confirm_state = dialog_state.clone();
        let on_ok_state = dialog_state.clone();
        dialog
            .title("Start task and publish draft PR?")
            .child(dialog_state.clone())
            .footer(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-issue-task")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("confirm-issue-task")
                            .primary()
                            .label(start_label)
                            .disabled(!start_enabled)
                            .tooltip(disabled_reason.clone().unwrap_or_default())
                            .on_click(move |_, window, cx| {
                                if activate_issue_start_dialog(&confirm_state, cx) {
                                    window.close_dialog(cx);
                                }
                            }),
                    ),
            )
            .on_ok(move |_, _, cx| activate_issue_start_dialog(&on_ok_state, cx))
    });
}
