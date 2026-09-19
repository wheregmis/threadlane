use std::path::PathBuf;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, Disableable, WindowExt};
use threadlane_git::GitHubIssueRef;
use threadlane_protocol::ReasoningEffort;
use threadlane_provider::model_registry::effective_effort;

use threadlane_ui_state::AppState;

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
        copy: "The agent works in an isolated worktree, verifies and commits its changes, then pushes to origin and creates a draft PR on GitHub.".into(),
        model: model.into(),
        reasoning_effort: reasoning_effort.into(),
        branch_preview: AppState::issue_branch_name(issue.number, title, "xxxxxx"),
        branch_disclosure: "A unique six-character suffix is assigned when the task starts.".into(),
        start_enabled: is_git_repository,
        start_disabled_reason: (!is_git_repository)
            .then_some("This project is not a Git repository.".into()),
        show_open_task: has_linked_task,
        start_label: if has_linked_task {
            "Start another task"
        } else {
            "Start task"
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

pub fn issue_start_dialog_result(
    result: Result<bool, String>,
    on_error: impl FnOnce(String),
) -> bool {
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
    effort: ReasoningEffort,
    models: Vec<threadlane_ui_catalog::ModelOption>,
}

impl IssueStartDialog {
    pub fn start(&mut self, cx: &mut Context<Self>) -> bool {
        let result = issue_start_activation(self.confirmation.start_enabled, || {
            self.model.update(cx, |state, cx| {
                let result = state.start_issue_work_with_options(
                    self.work_dir.clone(),
                    self.issue.clone(),
                    self.title.clone(),
                    self.confirmation.model.clone(),
                    self.effort,
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
        let selected = self.confirmation.model.clone();
        let model_label = threadlane_ui_catalog::selection_label(&selected, &self.models);
        let models = self.models.clone();
        let owner = cx.entity().downgrade();
        let model_picker = Button::new("issue-task-model")
            .debug_selector(|| "issue-task-model".into())
            .label(model_label.clone())
            .accessibility_label(format!("Model: {model_label}"))
            .dropdown_caret(true)
            .w_full()
            .disabled(models.is_empty())
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, _cx| {
                let mut previous_provider = None;
                models.iter().fold(
                    menu.max_h(window.rem_size() * 20.0).scrollable(true),
                    |menu, option| {
                        let menu = if previous_provider == Some(option.provider) {
                            menu
                        } else {
                            previous_provider = Some(option.provider);
                            menu.item(PopupMenuItem::label(option.provider.label()))
                        };
                        let owner = owner.clone();
                        let id = option.id.clone();
                        menu.item(
                            PopupMenuItem::new(option.label.clone())
                                .checked(id == selected)
                                .on_click(move |_, _, cx| {
                                    let _ = owner.update(cx, |this, cx| {
                                        this.effort = effective_effort(
                                            &id,
                                            this.effort,
                                            Some(&this.work_dir),
                                        );
                                        this.confirmation.model = id.clone();
                                        this.error = None;
                                        cx.notify();
                                    });
                                }),
                        )
                    },
                )
            });
        let show_effort = threadlane_ui_catalog::supports_reasoning(
            &self.confirmation.model,
            Some(&self.work_dir),
        );
        let efforts = threadlane_ui_catalog::efforts_for_model(
            &self.confirmation.model,
            Some(&self.work_dir),
        );
        let effort = self.effort;
        let owner = cx.entity().downgrade();
        let effort_picker = Button::new("issue-task-effort")
            .debug_selector(|| "issue-task-effort".into())
            .label(effort.label())
            .accessibility_label(format!("Reasoning effort: {}", effort.label()))
            .dropdown_caret(true)
            .w_full()
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                efforts.iter().fold(menu, |menu, option| {
                    let option = *option;
                    let owner = owner.clone();
                    menu.item(
                        PopupMenuItem::new(option.label())
                            .checked(option == effort)
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |this, cx| {
                                    this.effort = option;
                                    cx.notify();
                                });
                            }),
                    )
                })
            });
        let confirmation = &self.confirmation;
        div()
            .flex()
            .flex_col()
            .gap_4()
            .text_sm()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_color(theme.muted_foreground).child(format!(
                        "{}/{} · #{}",
                        self.issue.owner, self.issue.repo, self.issue.number
                    )))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child("Model")
                    .child(model_picker),
            )
            .children(show_effort.then(|| {
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child("Reasoning effort")
                    .child(effort_picker)
            }))
            .children(self.models.is_empty().then(|| {
                div()
                    .text_color(theme.warning)
                    .child("Connect a provider in Settings to choose a model.")
            }))
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(confirmation.copy.clone()),
            )
            .children(confirmation.start_disabled_reason.as_ref().map(|reason| {
                div()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(reason.clone())
            }))
            .children(
                self.error
                    .as_ref()
                    .map(|error| div().text_color(theme.danger).child(error.clone())),
            )
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
        (state.selected_model.clone(), state.reasoning_effort)
    };
    let confirmation = issue_start_confirmation(
        &issue,
        &title,
        &selected_model,
        reasoning_effort.label(),
        threadlane_git::is_git_repo(&work_dir),
        has_linked_task,
    );
    let models = threadlane_ui_catalog::available_models_for_project(Some(&work_dir));
    let effort = effective_effort(&selected_model, reasoning_effort, Some(&work_dir));
    let start_enabled = confirmation.start_enabled;
    let start_label = confirmation.start_label;
    let disabled_reason = confirmation.start_disabled_reason.clone();
    let dialog_state = cx.new(|_| IssueStartDialog {
        model,
        work_dir,
        issue,
        title,
        confirmation,
        effort,
        models,
        error: None,
    });
    window.open_dialog(cx, move |dialog, _window, cx| {
        let start_enabled = start_enabled && !dialog_state.read(cx).confirmation.model.is_empty();
        let confirm_state = dialog_state.clone();
        let on_ok_state = dialog_state.clone();
        dialog
            .title("Start task from issue")
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

/// Dialog state for creating a GitHub issue in one project.
pub struct IssueCreateDialog {
    model: Entity<AppState>,
    work_dir: PathBuf,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    creating: bool,
    pub error: Option<String>,
}

impl IssueCreateDialog {
    /// Creates the issue on a background thread; `true` closes the dialog.
    /// Refreshes the issue list so the new row appears without a restart.
    pub fn create(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        let title = self.title.read(cx).value().to_string();
        if title.trim().is_empty() || self.creating {
            return false;
        }
        self.creating = true;
        self.error = None;
        cx.notify();
        let model = self.model.clone();
        let work_dir = self.work_dir.clone();
        let body = self.body.read(cx).value().to_string();
        cx.spawn(async move |_this, cx| {
                let created =
                    cx.background_executor()
                        .spawn(async move {
                            threadlane_git::create_github_issue(&work_dir, &title, &body)
                        })
                        .await;
                let _ = model.update(cx, |state, cx| {
                    match created {
                        Ok(number) => {
                            state.session_status =
                                Some(format!("Created issue #{number}."));
                            state.github_list_revision += 1;
                        }
                        Err(error) => {
                            state.session_status = Some(format!(
                                "Could not create the issue: {}",
                                error.message
                            ));
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        true
    }
}

impl Render for IssueCreateDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .text_sm()
            .child(Input::new(&self.title).aria_label("Issue title"))
            .child(Textarea::new(&self.body).aria_label("Issue description"))
            .children(self.error.as_ref().map(|error| {
                div()
                    .text_xs()
                    .text_color(theme.danger)
                    .child(error.clone())
            }))
    }
}

pub fn open_issue_create_dialog(
    model: Entity<AppState>,
    work_dir: PathBuf,
    window: &mut Window,
    cx: &mut App,
) {
    let dialog_state = cx.new(|cx| IssueCreateDialog {
        model,
        work_dir,
        title: cx.new(|cx| {
            InputState::new(window, cx).placeholder("Issue title (required)")
        }),
        body: cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Description (optional, Markdown supported)")
                .auto_grow(3, 8)
                .soft_wrap(true)
        }),
        creating: false,
        error: None,
    });
    window.open_dialog(cx, move |dialog, _window, _cx| {
        let create_state = dialog_state.clone();
        let on_ok_state = dialog_state.clone();
        dialog
            .title("New issue")
            .child(dialog_state.clone())
            .footer(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-create-issue")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("confirm-create-issue")
                            .primary()
                            .label("Create issue")
                            .on_click(move |_, window, cx| {
                                if create_state.update(cx, |dialog, cx| {
                                    dialog.create(window, cx)
                                }) {
                                    window.close_dialog(cx);
                                }
                            }),
                    ),
            )
            .on_ok(move |_, window, cx| {
                on_ok_state.update(cx, |dialog, cx| dialog.create(window, cx))
            })
    });
}

#[cfg(test)]
mod tests {
    use super::{issue_start_confirmation, IssueStartDialog};
    use gpui::{AppContext, Modifiers, TestAppContext};
    use threadlane_protocol::ReasoningEffort;
    use threadlane_ui_catalog::{ModelOption, ModelProvider};
    use threadlane_ui_state::AppState;

    #[gpui::test]
    fn issue_start_pickers_keep_changes_local_and_hide_external_agent_effort(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| AppState::default());
        let original = model.read_with(cx, |state, _| {
            (state.selected_model.clone(), state.reasoning_effort)
        });
        let issue = threadlane_git::GitHubIssueRef {
            owner: "example".into(),
            repo: "app".into(),
            number: 42,
            ..Default::default()
        };
        let state = cx.new(|_| IssueStartDialog {
            model: model.clone(),
            work_dir: std::env::temp_dir(),
            confirmation: issue_start_confirmation(
                &issue,
                "Fix an issue",
                "test-model",
                "High",
                true,
                false,
            ),
            issue,
            title: "Fix an issue".into(),
            error: None,
            effort: ReasoningEffort::High,
            models: vec![ModelOption {
                id: "acp/test-agent".into(),
                label: "Test agent".into(),
                provider: ModelProvider::Acp,
            }],
        });
        let view = state.clone();
        let (_, cx) =
            cx.add_window_view(move |window, cx| gpui_component::Root::new(view, window, cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let effort = cx.debug_bounds("issue-task-effort").unwrap();
        cx.simulate_click(effort.center(), Modifiers::default());
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert_ne!(state.effort, ReasoningEffort::High)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let picker = cx.debug_bounds("issue-task-model").unwrap();
        cx.simulate_click(picker.center(), Modifiers::default());
        // The first row is the provider heading.
        cx.simulate_keystrokes("down down enter");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        state.read_with(cx, |state, _| {
            assert_eq!(state.confirmation.model, "acp/test-agent")
        });
        assert!(cx.debug_bounds("issue-task-effort").is_none());
        model.read_with(cx, |state, _| {
            assert_eq!(
                (state.selected_model.clone(), state.reasoning_effort),
                original
            )
        });
    }
}
