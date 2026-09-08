use std::path::PathBuf;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::spinner::Spinner;
use gpui_component::{h_flex, v_flex, ActiveTheme, Disableable, Sizable, WindowExt};
use threadlane_git::GitStatus;

use super::types::{nonempty, Surface};
use super::RightPanelView;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftPrFields {
    pub(crate) base: String,
    pub(crate) title: String,
    pub(crate) body: String,
}

impl DraftPrFields {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        [
            (&self.base, "Enter the base branch."),
            (&self.title, "Enter a pull request title."),
            (&self.body, "Enter a pull request description."),
        ]
        .into_iter()
        .find(|(value, _)| value.trim().is_empty())
        .map_or(Ok(()), |(_, error)| Err(error))
    }
}

pub(crate) fn draft_pr_prefill(status: &GitStatus) -> DraftPrFields {
    let branch = status
        .branch
        .as_deref()
        .and_then(nonempty)
        .unwrap_or("main");
    let base = status
        .default_branch
        .as_deref()
        .and_then(nonempty)
        .or_else(|| {
            status
                .branch_details
                .iter()
                .find(|branch| branch.is_default)
                .and_then(|branch| nonempty(&branch.name))
        })
        .unwrap_or("main")
        .to_string();
    let commit = status.recent_commits.first();
    let summary = commit
        .and_then(|commit| nonempty(&commit.summary))
        .unwrap_or(branch);
    let body = commit
        .and_then(|commit| nonempty(&commit.body))
        .unwrap_or(summary);
    DraftPrFields {
        base,
        title: summary.into(),
        body: body.into(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftPrContextKey {
    pub(crate) project: PathBuf,
    pub(crate) branch: String,
    pub(crate) revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftPrAttempt {
    pub(crate) id: u64,
    pub(crate) key: DraftPrContextKey,
    pub(crate) fields: DraftPrFields,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum DraftPrPhase {
    #[default]
    Idle,
    Posting(DraftPrAttempt),
    Unknown(DraftPrAttempt),
    Checking(DraftPrAttempt),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DraftPrAttemptState {
    pub(crate) next_id: u64,
    pub(crate) phase: DraftPrPhase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DraftPrCompletion {
    Stale,
    Failure(String),
    Unknown(String),
    SuccessExact(String),
    SuccessWithNewerEdits(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DraftPrRemoteResult {
    Exists(String),
    Absent(String),
    Unknown(String),
}

impl DraftPrAttemptState {
    pub(crate) fn begin(
        &mut self,
        key: DraftPrContextKey,
        fields: DraftPrFields,
    ) -> Result<DraftPrAttempt, &'static str> {
        fields.validate()?;
        if !matches!(self.phase, DraftPrPhase::Idle) {
            return Err("A draft pull request is already being created.");
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let attempt = DraftPrAttempt {
            id: self.next_id,
            key,
            fields,
        };
        self.phase = DraftPrPhase::Posting(attempt.clone());
        Ok(attempt)
    }

    pub(crate) fn is_busy(&self) -> bool {
        matches!(
            self.phase,
            DraftPrPhase::Posting(_) | DraftPrPhase::Checking(_)
        )
    }

    pub(crate) fn is_uncertain(&self) -> bool {
        matches!(self.phase, DraftPrPhase::Unknown(_))
    }

    pub(crate) fn begin_check(&mut self) -> Result<DraftPrAttempt, &'static str> {
        let DraftPrPhase::Unknown(attempt) = &self.phase else {
            return Err("There is no uncertain draft pull request to check.");
        };
        let attempt = attempt.clone();
        self.phase = DraftPrPhase::Checking(attempt.clone());
        Ok(attempt)
    }

    pub(crate) fn complete(
        &mut self,
        completed: &DraftPrAttempt,
        current_key: &DraftPrContextKey,
        current_fields: &DraftPrFields,
        result: DraftPrRemoteResult,
    ) -> DraftPrCompletion {
        let attempt = match &self.phase {
            DraftPrPhase::Posting(attempt) | DraftPrPhase::Checking(attempt) => attempt,
            _ => return DraftPrCompletion::Stale,
        };
        if attempt.id != completed.id || &attempt.key != current_key {
            return DraftPrCompletion::Stale;
        }
        let exact = attempt.fields == *current_fields;
        let attempt = attempt.clone();
        match result {
            DraftPrRemoteResult::Absent(error) => {
                self.phase = DraftPrPhase::Idle;
                DraftPrCompletion::Failure(error)
            }
            DraftPrRemoteResult::Unknown(error) => {
                self.phase = DraftPrPhase::Unknown(attempt);
                DraftPrCompletion::Unknown(error)
            }
            DraftPrRemoteResult::Exists(url) if exact => {
                self.phase = DraftPrPhase::Idle;
                DraftPrCompletion::SuccessExact(url)
            }
            DraftPrRemoteResult::Exists(url) => {
                self.phase = DraftPrPhase::Idle;
                DraftPrCompletion::SuccessWithNewerEdits(url)
            }
        }
    }
}

pub(crate) struct DraftPrDialogView {
    pub(crate) panel: WeakEntity<RightPanelView>,
    pub(crate) key: DraftPrContextKey,
    pub(crate) base_input: Entity<InputState>,
    pub(crate) title_input: Entity<InputState>,
    pub(crate) body_input: Entity<TextareaState>,
    pub(crate) attempts: DraftPrAttemptState,
    pub(crate) error: Option<String>,
    pub(crate) created: bool,
    pub(crate) _subscriptions: Vec<Subscription>,
}

impl DraftPrDialogView {
    pub(crate) fn new(
        panel: WeakEntity<RightPanelView>,
        key: DraftPrContextKey,
        fields: DraftPrFields,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let base_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Base branch")
                .default_value(fields.base)
        });
        let title_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Pull request title")
                .default_value(fields.title)
        });
        let body_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Describe the change and how it was verified")
                .default_value(fields.body)
                .auto_grow(4, 10)
                .soft_wrap(true)
        });
        let subscriptions = vec![
            cx.observe(&base_input, |_, _, cx| cx.notify()),
            cx.observe(&title_input, |_, _, cx| cx.notify()),
            cx.observe(&body_input, |_, _, cx| cx.notify()),
        ];
        Self {
            panel,
            key,
            base_input,
            title_input,
            body_input,
            attempts: DraftPrAttemptState::default(),
            error: None,
            created: false,
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn fields(&self, cx: &App) -> DraftPrFields {
        DraftPrFields {
            base: self.base_input.read(cx).value().to_string(),
            title: self.title_input.read(cx).value().to_string(),
            body: self.body_input.read(cx).value().to_string(),
        }
    }

    pub(crate) fn current_key(&self, creation: bool, cx: &App) -> Option<DraftPrContextKey> {
        self.panel.upgrade().and_then(|panel| {
            let panel = panel.read(cx);
            if creation {
                panel.draft_pr_creation_key()
            } else {
                panel.draft_pr_checkout_key()
            }
        })
    }

    pub(crate) fn start_request(&mut self, check: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.created || self.attempts.is_busy() || (!check && self.attempts.is_uncertain()) {
            return;
        }
        let Some(key) = self.current_key(!check, cx).filter(|key| key == &self.key) else {
            self.error = Some(
                "The active checkout or branch changed. Close this dialog and open it again."
                    .into(),
            );
            cx.notify();
            return;
        };
        let fields = self.fields(cx);
        let attempt = if check {
            self.attempts.begin_check()
        } else {
            self.attempts.begin(key, fields.clone())
        };
        let attempt = match attempt {
            Ok(attempt) => attempt,
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return;
            }
        };
        self.error = None;
        let (work_dir, branch) = (attempt.key.project.clone(), attempt.key.branch.clone());
        let (base, title, body) = (
            fields.base.trim().to_string(),
            fields.title.trim().to_string(),
            fields.body.trim().to_string(),
        );
        let task = cx.background_executor().spawn(async move {
            let inspect = |absent| match threadlane_git::inspect_pr_for_branch(&work_dir, &branch) {
                Ok(Some(pr)) => DraftPrRemoteResult::Exists(pr.url),
                Ok(None) => DraftPrRemoteResult::Absent(absent),
                Err(error) => DraftPrRemoteResult::Unknown(error.to_string()),
            };
            if check {
                return inspect(
                    "GitHub did not create the draft pull request. You can try again.".into(),
                );
            }
            match threadlane_git::create_draft_pull_request(&work_dir, &base, &title, &body) {
                Ok(url) => DraftPrRemoteResult::Exists(url),
                Err(error) => inspect(error.to_string()),
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(key) = this.current_key(false, cx) else {
                    return;
                };
                let fields = this.fields(cx);
                let completion = this.attempts.complete(&attempt, &key, &fields, result);
                this.apply_completion(completion, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_completion(
        &mut self,
        completion: DraftPrCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match completion {
            DraftPrCompletion::Stale => return,
            DraftPrCompletion::Failure(error) => {
                self.error = Some(format!(
                    "Couldn’t create the draft pull request. {error} Review the fields and try again."
                ));
            }
            DraftPrCompletion::Unknown(error) => {
                self.error = Some(format!(
                    "Couldn’t confirm whether GitHub created the draft pull request. {error} Use Check again before trying to create another."
                ));
            }
            DraftPrCompletion::SuccessExact(url) => {
                self.finish_success(url, true, window, cx);
            }
            DraftPrCompletion::SuccessWithNewerEdits(url) => {
                self.finish_success(url, false, window, cx);
            }
        }
        cx.notify();
    }

    fn finish_success(
        &mut self,
        url: String,
        exact: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.panel.upgrade() {
            let message = if url.is_empty() {
                "Draft pull request created.".into()
            } else {
                format!("Draft pull request created: {url}")
            };
            panel.update(cx, |panel, cx| {
                panel.git_feedback = Some(message);
                panel.refresh_surface(Surface::Review);
                cx.notify();
            });
        }
        if !url.is_empty() {
            cx.open_url(&url);
        }
        if exact {
            self.base_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.title_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.body_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            window.close_dialog(cx);
        } else {
            self.created = true;
            self.error = Some(
                "The draft pull request was created. Your newer edits remain in this dialog."
                    .into(),
            );
        }
    }
}

fn draft_pr_field(label: &'static str, field: impl IntoElement) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
        .child(field)
}

impl Render for DraftPrDialogView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let fields = self.fields(cx);
        let busy = self.attempts.is_busy();
        let uncertain = self.attempts.is_uncertain();
        let checking = matches!(&self.attempts.phase, DraftPrPhase::Checking(_));
        let created = self.created;
        let context_matches = self.current_key(false, cx).as_ref() == Some(&self.key);
        let creation_available = self.current_key(true, cx).as_ref() == Some(&self.key);
        let can_submit = creation_available && fields.validate().is_ok() && !busy;
        let base = if fields.base.trim().is_empty() {
            "base branch".to_string()
        } else {
            fields.base.trim().to_string()
        };

        v_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "Creates a DRAFT pull request on GitHub from {} into {base}. No commits are pushed.",
                        self.key.branch
                    )),
            )
            .child(draft_pr_field(
                "Base branch",
                Input::new(&self.base_input).disabled(created),
            ))
            .child(draft_pr_field(
                "Title",
                Input::new(&self.title_input).disabled(created),
            ))
            .child(draft_pr_field(
                "Description",
                Textarea::new(&self.body_input).disabled(created),
            ))
            .children((!context_matches).then(|| {
                div()
                    .text_sm()
                    .text_color(theme.danger)
                    .child("The active checkout or branch changed. Close this dialog and open it again.")
            }))
            .children(self.error.as_ref().map(|error| {
                div()
                    .text_sm()
                    .text_color(if created {
                        theme.success
                    } else {
                        theme.danger
                    })
                    .child(error.clone())
            }))
            .children(busy.then(|| {
                h_flex()
                    .gap_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(Spinner::new().small())
                    .child(if checking {
                        "Checking GitHub…"
                    } else {
                        "Creating draft on GitHub…"
                    })
            }))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        Button::new("cancel-draft-pr")
                            .label(if created { "Done" } else { "Cancel" })
                            .disabled(busy && context_matches)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .children((!uncertain && !created).then(|| {
                        Button::new("submit-draft-pr")
                            .label(if busy { "Creating…" } else { "Create draft" })
                            .primary()
                            .disabled(!can_submit)
                            .tooltip(if context_matches {
                                "Create a draft pull request on GitHub"
                            } else {
                                "The active checkout or branch changed"
                            })
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.start_request(false, window, cx);
                            }))
                    }))
                    .children((uncertain && !busy).then(|| {
                        Button::new("check-draft-pr")
                            .label("Check again")
                            .outline()
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.start_request(true, window, cx);
                            }))
                    })),
            )
    }
}
