//! Local workflow adapter over the same issue form and dialog used by desktop.
use gpui::{prelude::*, *};
use gpui_component::input::{InputState, TextareaState};
use gpui_component::WindowExt;
use std::time::Duration;
use threadlane_ui_kit::github as kit;

pub(super) struct IssueCreatePreview {
    repository: String,
    pub(super) title: Entity<InputState>,
    pub(super) body: Entity<TextareaState>,
    pub(super) creating: bool,
    pub(super) error: Option<String>,
    fail_next: bool,
    dismissed: bool,
    completed: Box<dyn Fn(String, &mut App)>,
}

impl IssueCreatePreview {
    pub(super) fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating || self.dismissed {
            return;
        }
        let title = self.title.read(cx).value().to_string();
        if let Err(error) = kit::validate_issue_title(&title) {
            self.error = Some(error.into());
            cx.notify();
            window.refresh();
            self.title.read(cx).focus_handle(cx).focus(window, cx);
            return;
        }
        self.error = None;
        self.creating = true;
        cx.notify();
        window.refresh();
        let fail = std::mem::take(&mut self.fail_next);
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let close = this.update(cx, |this, cx| {
                this.creating = false;
                if fail {
                    this.error = Some("Could not create the issue: sample request failed. Your draft is preserved; try again.".into());
                    cx.notify();
                    false
                } else {
                    (this.completed)(title, cx);
                    !this.dismissed
                }
            }).unwrap_or(false);
            let _ = cx.update_window(handle, |_, window, cx| {
                if close { window.close_dialog(cx); } else { window.refresh(); }
            });
        }).detach();
    }
}

impl Render for IssueCreatePreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        kit::github_issue_create_form(
            &self.repository,
            &self.title,
            &self.body,
            self.creating,
            self.error.as_deref(),
            cx,
        )
    }
}

pub(super) fn open(
    repository: String,
    fail_next: bool,
    completed: impl Fn(String, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> Entity<IssueCreatePreview> {
    let state = cx.new(|cx| IssueCreatePreview {
        repository,
        title: cx.new(|cx| InputState::new(window, cx).placeholder("Issue title")),
        body: cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Describe the issue…")
                .auto_grow(3, 8)
                .soft_wrap(true)
        }),
        creating: false,
        error: None,
        fail_next,
        dismissed: false,
        completed: Box::new(completed),
    });
    let dialog_state = state.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let create = dialog_state.clone();
        let dismissed = dialog_state.clone();
        kit::github_issue_create_dialog(
            dialog,
            dialog_state.read(cx).creating,
            move |window, cx| create.update(cx, |state, cx| state.create(window, cx)),
            move |cx| dismissed.update(cx, |state, _| state.dismissed = true),
        )
        .child(dialog_state.clone())
    });
    state
        .read(cx)
        .title
        .read(cx)
        .focus_handle(cx)
        .focus(window, cx);
    state
}

pub(super) struct IssueStartPreview {
    pub(super) form: kit::GitHubIssueStartForm,
    fail_next: bool,
    dismissed: bool,
    completed: Box<dyn Fn(&kit::GitHubIssueStartForm, &mut App)>,
}

impl IssueStartPreview {
    pub(super) fn select(
        &mut self,
        action: kit::GitHubIssueStartAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.form.starting || self.dismissed {
            return;
        }
        match action {
            kit::GitHubIssueStartAction::Model(id) => {
                let Some(model) = self.form.models.iter().find(|model| model.id == id) else {
                    return;
                };
                self.form.model_label = model.label.clone();
                self.form.efforts = (!id.starts_with("acp/")).then(|| {
                    vec![
                        threadlane_protocol::ReasoningEffort::Low,
                        threadlane_protocol::ReasoningEffort::Medium,
                        threadlane_protocol::ReasoningEffort::High,
                    ]
                });
                self.form.model = id;
            }
            kit::GitHubIssueStartAction::Effort(effort) => self.form.effort = effort,
            kit::GitHubIssueStartAction::Mode(mode) => self.form.mode = mode,
        }
        self.form.error = None;
        cx.notify();
        window.refresh();
    }

    pub(super) fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form.starting || self.dismissed || self.form.disabled_reason.is_some() {
            return;
        }
        self.form.error = None;
        self.form.starting = true;
        cx.notify();
        window.refresh();
        let fail = std::mem::take(&mut self.fail_next);
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let close = this
                .update(cx, |this, cx| {
                    this.form.starting = false;
                    if fail {
                        this.form.error = Some(
                            "Could not check the repository: sample request failed. Try again."
                                .into(),
                        );
                        cx.notify();
                        false
                    } else {
                        (this.completed)(&this.form, cx);
                        !this.dismissed
                    }
                })
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

impl Render for IssueStartPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        kit::github_issue_start_form(
            self.form.clone(),
            move |action, window, cx| {
                let _ = owner.update(cx, |this, cx| this.select(action, window, cx));
            },
            cx,
        )
    }
}

pub(super) fn open_start(
    identity: String,
    title: String,
    number: u64,
    scenario: &str,
    completed: impl Fn(&kit::GitHubIssueStartForm, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> Entity<IssueStartPreview> {
    use threadlane_protocol::{OrchestratorMode, ReasoningEffort};
    let no_provider = scenario == "issue-start-no-provider";
    let state = cx.new(|_| IssueStartPreview {
        form: kit::GitHubIssueStartForm {
            identity,
            branch: threadlane_protocol::repo::issue_branch_name(number, &title, "xxxxxx"),
            title,
            model: "sample/quality".into(),
            model_label: "Quality sample model".into(),
            models: if no_provider {
                Vec::new()
            } else {
                vec![
                    kit::IssueTaskModel {
                        id: "sample/fast".into(),
                        label: "Fast sample model".into(),
                        provider: "Sample provider".into(),
                    },
                    kit::IssueTaskModel {
                        id: "sample/quality".into(),
                        label: "Quality sample model".into(),
                        provider: "Sample provider".into(),
                    },
                    kit::IssueTaskModel {
                        id: "acp/sample-agent".into(),
                        label: "Sample external agent".into(),
                        provider: "External agents".into(),
                    },
                ]
            },
            effort: ReasoningEffort::Medium,
            efforts: Some(vec![
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
            ]),
            mode: OrchestratorMode::Normal,
            starting: false,
            disabled_reason: if no_provider {
                Some("Connect a provider in Settings to choose a model.".into())
            } else {
                (scenario == "issue-start-no-repo")
                    .then(|| "This project is not a Git repository.".into())
            },
            error: None,
        },
        fail_next: scenario == "issue-start-error",
        dismissed: false,
        completed: Box::new(completed),
    });
    let dialog_state = state.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let start = dialog_state.clone();
        let dismiss = dialog_state.clone();
        let form = &dialog_state.read(cx).form;
        kit::github_issue_start_dialog(
            dialog,
            form.starting,
            "Start another task",
            form.disabled_reason.clone(),
            move |window, cx| start.update(cx, |this, cx| this.start(window, cx)),
            move |cx| dismiss.update(cx, |this, _| this.dismissed = true),
        )
        .child(dialog_state.clone())
    });
    state
}
