use std::path::PathBuf;

use gpui::*;
use gpui_component::input::{InputState, TextareaState};
use gpui_component::WindowExt;
use threadlane_git::GitHubIssueRef;
use threadlane_protocol::{OrchestratorMode, ReasoningEffort};
use threadlane_provider::model_registry::effective_effort;
use threadlane_ui_kit::github as kit;

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
        copy: kit::ISSUE_TASK_DESCRIPTION.into(),
        model: model.into(),
        reasoning_effort: reasoning_effort.into(),
        branch_preview: AppState::issue_branch_name(issue.number, title, "xxxxxx"),
        branch_disclosure: kit::ISSUE_TASK_BRANCH_DISCLOSURE.into(),
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
    mode: OrchestratorMode,
    models: Vec<threadlane_daemon::catalog::ModelOption>,
    /// Repo validation / task creation is in flight on the daemon.
    pub starting: bool,
    dismissed: bool,
}

impl IssueStartDialog {
    fn unavailable_reason(&self, cx: &App) -> Option<String> {
        if !self.confirmation.start_enabled {
            return Some(
                self.confirmation
                    .start_disabled_reason
                    .clone()
                    .unwrap_or_else(|| "GitHub issue work requires a Git repository.".into()),
            );
        }
        if self.model.read(cx).daemon_remote {
            return Some("GitHub issue work is not yet supported on remote daemons".into());
        }
        if self.models.is_empty() {
            return Some("Connect a provider in Settings to choose a model.".into());
        }
        self.confirmation
            .model
            .is_empty()
            .then(|| "Choose a model to start the task.".into())
    }

    /// Validate the checkout on the daemon host, then create the worktree
    /// task. Runs async: failures stay on the dialog; a successful start
    /// closes it.
    pub fn start(&mut self, window_handle: AnyWindowHandle, cx: &mut Context<Self>) {
        if self.unavailable_reason(cx).is_some() || self.starting || self.dismissed {
            return;
        }
        self.starting = true;
        self.error = None;
        cx.notify();
        let work_dir = self.work_dir.clone();
        let issue = self.issue.clone();
        let title = self.title.clone();
        let selected = self.confirmation.model.clone();
        let effort = self.effort;
        let mode = self.mode;
        let client = self.model.read(cx).daemon_client.clone();
        cx.spawn(async move |this, cx| {
            // The same checks `start_issue_work_with_prompt` applies
            // locally, run against the daemon host's filesystem. The IO
            // stays on a background thread so an in-process daemon never
            // blocks the UI.
            let preflight_dir = work_dir.clone();
            let preflight = cx.background_executor().spawn(async move {
                match threadlane_ui_state::project_io::is_repo(&client, &preflight_dir).await {
                    Err(error) => {
                        return Err(format!("Could not check the repository: {error}"));
                    }
                    Ok(false) => {
                        return Err("GitHub issue work requires a Git repository".to_string());
                    }
                    Ok(true) => {}
                }
                let status =
                    threadlane_ui_state::project_io::inspect(&client, &preflight_dir, false)
                        .await
                        .map_err(|error| format!("Could not inspect the repository: {error}"))?;
                if status.recent_commits.is_empty() {
                    return Err("GitHub issue work requires an initial commit".to_string());
                }
                Ok(())
            });
            let close = match preflight.await {
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.starting = false;
                        this.error = Some(error);
                        cx.notify();
                    });
                    false
                }
                Ok(()) => this
                    .update(cx, |this, cx| {
                        let result = this.model.update(cx, |state, cx| {
                            let result = state.start_issue_work_with_options(
                                work_dir, issue, title, selected, effort, mode,
                            );
                            if let Err(error) = &result {
                                state.session_status = Some(error.clone());
                            }
                            cx.notify();
                            result.map(|_| ())
                        });
                        this.starting = false;
                        this.error = result.as_ref().err().cloned();
                        cx.notify();
                        result.is_ok() && !this.dismissed
                    })
                    .unwrap_or(false),
            };
            let _ = cx.update_window(window_handle, |_, window, cx| {
                if close {
                    window.close_dialog(cx);
                } else {
                    window.refresh();
                }
            });
        })
        .detach();
    }
}

pub fn activate_issue_start_dialog(
    dialog: &Entity<IssueStartDialog>,
    window: &mut Window,
    cx: &mut App,
) {
    let handle = window.window_handle();
    dialog.update(cx, |dialog, cx| dialog.start(handle, cx));
    window.refresh();
}

impl Render for IssueStartDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = &self.confirmation.model;
        let owner = cx.entity().downgrade();
        kit::github_issue_start_form(
            kit::GitHubIssueStartForm {
                identity: format!(
                    "{}/{} · #{}",
                    self.issue.owner, self.issue.repo, self.issue.number
                ),
                title: self.title.clone(),
                branch: self.confirmation.branch_preview.clone(),
                model: selected.clone(),
                model_label: threadlane_daemon::catalog::selection_label(selected, &self.models),
                models: self
                    .models
                    .iter()
                    .map(|model| kit::IssueTaskModel {
                        id: model.id.clone(),
                        label: model.label.clone(),
                        provider: model.provider.label().into(),
                    })
                    .collect(),
                effort: self.effort,
                efforts: threadlane_daemon::catalog::supports_reasoning(
                    selected,
                    Some(&self.work_dir),
                )
                .then(|| {
                    threadlane_daemon::catalog::efforts_for_model(selected, Some(&self.work_dir))
                }),
                mode: self.mode,
                starting: self.starting,
                disabled_reason: self.unavailable_reason(cx),
                error: self.error.clone(),
            },
            move |action, window, cx| {
                let _ = owner.update(cx, |this, cx| {
                    if this.starting || this.dismissed {
                        return;
                    }
                    match action {
                        kit::GitHubIssueStartAction::Model(id) => {
                            this.effort = effective_effort(&id, this.effort, Some(&this.work_dir));
                            this.confirmation.model = id;
                        }
                        kit::GitHubIssueStartAction::Effort(effort) => this.effort = effort,
                        kit::GitHubIssueStartAction::Mode(mode) => this.mode = mode,
                    }
                    this.error = None;
                    cx.notify();
                });
                window.refresh();
            },
            cx,
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
    let (selected_model, reasoning_effort, client) = {
        let state = model.read(cx);
        (
            state.selected_model.clone(),
            state.reasoning_effort,
            state.daemon_client.clone(),
        )
    };
    let mode = threadlane_project::subagent_settings::load(&work_dir).orchestrator_mode;
    let models = threadlane_daemon::catalog::available_models_for_project(Some(&work_dir));
    let effort = effective_effort(&selected_model, reasoning_effort, Some(&work_dir));
    let window_handle = window.window_handle();
    // `is_git_repo` must answer for the checkout on the daemon host, not
    // the client's disk, so the dialog opens once the daemon replies.
    cx.spawn(async move |cx| {
        let probe_dir = work_dir.clone();
        // A failed probe is not "not a repository" — the dialog opens
        // disabled with the probe error as the reason, so a retryable
        // daemon failure isn't misreported as missing Git.
        let is_git: Result<bool, String> = cx
            .background_executor()
            .spawn(
                async move { threadlane_ui_state::project_io::is_repo(&client, &probe_dir).await },
            )
            .await;
        let (is_git, probe_error) = match is_git {
            Ok(is_git) => (is_git, None),
            Err(error) => (
                false,
                Some(format!("Could not check the repository: {error}")),
            ),
        };
        let _ = cx.update_window(window_handle, |_, window, cx| {
            let mut confirmation = issue_start_confirmation(
                &issue,
                &title,
                &selected_model,
                reasoning_effort.label(),
                is_git,
                has_linked_task,
            );
            if let Some(reason) = probe_error {
                confirmation.start_disabled_reason = Some(reason);
            }
            let dialog_state = cx.new(|_| IssueStartDialog {
                model,
                work_dir,
                issue,
                title,
                confirmation,
                effort,
                mode,
                models,
                error: None,
                starting: false,
                dismissed: false,
            });
            window.open_dialog(cx, move |dialog, _window, cx| {
                let state = dialog_state.read(cx);
                let confirm = dialog_state.clone();
                let dismiss = dialog_state.clone();
                kit::github_issue_start_dialog(
                    dialog,
                    state.starting,
                    state.confirmation.start_label,
                    state.unavailable_reason(cx),
                    move |window, cx| activate_issue_start_dialog(&confirm, window, cx),
                    move |cx| dismiss.update(cx, |state, _| state.dismissed = true),
                )
                .child(dialog_state.clone())
            });
        });
    })
    .detach();
}

/// Dialog state for creating a GitHub issue in one project.
pub struct IssueCreateDialog {
    model: Entity<AppState>,
    work_dir: PathBuf,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    creating: bool,
    dismissed: bool,
    pub error: Option<String>,
}

impl IssueCreateDialog {
    fn begin_create(&mut self, cx: &mut Context<Self>) -> Option<(String, String)> {
        if self.creating || self.dismissed {
            return None;
        }
        let title = self.title.read(cx).value().to_string();
        if let Err(error) = threadlane_ui_kit::github::validate_issue_title(&title) {
            self.error = Some(error.into());
            cx.notify();
            return None;
        }
        self.creating = true;
        self.error = None;
        cx.notify();
        Some((title, self.body.read(cx).value().to_string()))
    }

    fn finish_create(&mut self, result: Result<u64, String>, cx: &mut Context<Self>) -> bool {
        self.creating = false;
        self.error = result.as_ref().err().cloned();
        cx.notify();
        result.is_ok() && !self.dismissed
    }

    /// Retain the draft until GitHub acknowledges creation; errors remain inline.
    pub fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let request = self.begin_create(cx);
        // The footer and close controls belong to the dialog root.
        window.refresh();
        let Some((title, body)) = request else {
            if !self.creating && !self.dismissed {
                self.title.read(cx).focus_handle(cx).focus(window, cx);
            }
            return;
        };
        let model = self.model.clone();
        let work_dir = self.work_dir.clone();
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let created = cx
                .background_executor()
                .spawn(async move {
                    threadlane_git::create_github_issue(&work_dir, &title, &body)
                        .map_err(|error| error.message)
                })
                .await;
            // Refresh the collection even if the originating dialog was dismissed.
            let _ = model.update(cx, |state, cx| {
                match &created {
                    Ok(number) => {
                        state.session_status = Some(format!("Created issue #{number}."));
                        state.github_list_revision += 1;
                    }
                    Err(error) => {
                        state.session_status = Some(format!("Could not create the issue: {error}"));
                    }
                }
                cx.notify();
            });
            let close = this
                .update(cx, |this, cx| this.finish_create(created, cx))
                .unwrap_or(false);
            let _ = cx.update_window(handle, |_, window, cx| {
                if close {
                    window.close_dialog(cx);
                } else {
                    window.refresh();
                }
            });
        })
        .detach();
    }
}

impl Render for IssueCreateDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        threadlane_ui_kit::github::github_issue_create_form(
            self.work_dir.display().to_string(),
            &self.title,
            &self.body,
            self.creating,
            self.error.as_deref(),
            cx,
        )
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
        title: cx.new(|cx| InputState::new(window, cx).placeholder("Issue title")),
        body: cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Describe the issue…")
                .auto_grow(3, 8)
                .soft_wrap(true)
        }),
        creating: false,
        dismissed: false,
        error: None,
    });
    let title_focus = dialog_state.read(cx).title.read(cx).focus_handle(cx);
    window.open_dialog(cx, move |dialog, _window, cx| {
        let create_state = dialog_state.clone();
        let dismissed_state = dialog_state.clone();
        threadlane_ui_kit::github::github_issue_create_dialog(
            dialog,
            dialog_state.read(cx).creating,
            move |window, cx| create_state.update(cx, |state, cx| state.create(window, cx)),
            move |cx| dismissed_state.update(cx, |state, _| state.dismissed = true),
        )
        .child(dialog_state.clone())
    });
    title_focus.focus(window, cx);
}

#[cfg(test)]
mod tests {
    use super::{issue_start_confirmation, IssueCreateDialog, IssueStartDialog};
    use gpui::{AppContext, Modifiers, TestAppContext};
    use threadlane_protocol::{OrchestratorMode, ReasoningEffort};
    use threadlane_daemon::catalog::{ModelOption, ModelProvider};
    use threadlane_ui_state::AppState;

    #[gpui::test]
    fn issue_creation_validates_and_keeps_drafts_until_acknowledgement(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| AppState::default());
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let state = cx.new(|cx| IssueCreateDialog {
                model,
                work_dir: std::env::temp_dir(),
                title: cx.new(|cx| gpui_component::input::InputState::new(window, cx)),
                body: cx.new(|cx| gpui_component::input::TextareaState::new(window, cx)),
                creating: false,
                dismissed: false,
                error: None,
            });
            *capture.borrow_mut() = Some(state.clone());
            gpui_component::Root::new(state, window, cx)
        });
        let state = captured.borrow_mut().take().unwrap();
        state.update(cx, |state, cx| {
            assert!(state.begin_create(cx).is_none());
            assert_eq!(state.error.as_deref(), Some("Enter a title for the issue."));
        });
        cx.update(|window, cx| {
            state.update(cx, |state, cx| {
                state
                    .title
                    .update(cx, |input, cx| input.set_value("Title", window, cx));
                state.body.update(cx, |input, cx| {
                    input.set_value("Retain this description", window, cx)
                });
            })
        });
        state.update(cx, |state, cx| {
            assert_eq!(
                state.begin_create(cx),
                Some(("Title".into(), "Retain this description".into()))
            );
            assert!(
                state.begin_create(cx).is_none(),
                "duplicate requests are blocked"
            );
            assert!(!state.finish_create(Err("GitHub unavailable".into()), cx));
            assert!(!state.creating);
            assert_eq!(state.title.read(cx).value(), "Title");
            assert_eq!(state.body.read(cx).value(), "Retain this description");
            assert!(
                state.begin_create(cx).is_some(),
                "a failed request can be retried"
            );
            assert!(
                state.finish_create(Ok(42), cx),
                "only acknowledgement closes the dialog"
            );
            state.dismissed = true;
            assert!(
                !state.finish_create(Ok(43), cx),
                "late completion cannot close another dialog"
            );
            assert!(state.begin_create(cx).is_none());
        });
    }

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
            mode: OrchestratorMode::Normal,
            models: vec![ModelOption {
                id: "acp/test-agent".into(),
                label: "Test agent".into(),
                provider: ModelProvider::Acp,
            }],
            starting: false,
            dismissed: false,
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

    #[gpui::test]
    fn issue_start_mode_picker_keeps_change_local(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| AppState::default());
        let original_mode =
            model.read_with(cx, |state, _| state.orchestrator_mode);
        let issue = threadlane_git::GitHubIssueRef {
            owner: "example".into(),
            repo: "app".into(),
            number: 7,
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
            mode: OrchestratorMode::Normal,
            models: Vec::new(),
            starting: false,
            dismissed: false,
        });
        let view = state.clone();
        let (_, cx) =
            cx.add_window_view(move |window, cx| gpui_component::Root::new(view, window, cx));
        cx.update(|window, cx| state.update(cx, |state, cx| {
            assert!(state.unavailable_reason(cx).unwrap().contains("Connect a provider"));
            state.start(window.window_handle(), cx);
            assert!(!state.starting, "an unavailable model never reaches daemon preflight");
        }));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let picker = cx.debug_bounds("issue-task-mode").unwrap();
        cx.simulate_click(picker.center(), Modifiers::default());
        cx.simulate_keystrokes("down down enter");
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert_eq!(state.mode, OrchestratorMode::Fusion)
        });
        model.read_with(cx, |state, _| {
            assert_eq!(state.orchestrator_mode, original_mode)
        });
    }
}
