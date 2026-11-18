//! Local draft-PR host. Uses production presentation without GitHub or model requests.
use gpui::{prelude::*, *};
use gpui_component::input::{InputState, TextareaState};
use gpui_component::WindowExt;
use std::time::Duration;
use threadlane_ui_kit as kit;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum DraftPrSample {
    #[default]
    Ready,
    Failure,
    Uncertain,
    ChangedCheckout,
    Created,
}

pub(super) struct DraftPrPreview {
    branch: String,
    pub(super) base: Entity<InputState>,
    pub(super) title: Entity<InputState>,
    pub(super) body: Entity<TextareaState>,
    pub(super) phase: kit::ReviewDraftPrPhase,
    pub(super) generating: bool,
    pub(super) error: Option<String>,
    sample: DraftPrSample,
    revision: u64,
    _subscriptions: Vec<Subscription>,
}
impl DraftPrPreview {
    pub(super) fn new(
        status: Option<&threadlane_protocol::repo::GitStatus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let fields = kit::review_draft_pr_prefill(status.unwrap_or(&Default::default()));
        let base = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(kit::REVIEW_PR_BASE_PLACEHOLDER)
                .default_value(fields.base)
        });
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(kit::REVIEW_PR_TITLE_PLACEHOLDER)
                .default_value(fields.title)
        });
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(kit::REVIEW_PR_BODY_PLACEHOLDER)
                .default_value(fields.body)
                .auto_grow(4, 10)
                .soft_wrap(true)
        });
        let subscriptions = vec![
            cx.observe(&base, |_, _, cx| cx.notify()),
            cx.observe(&title, |_, _, cx| cx.notify()),
            cx.observe(&body, |_, _, cx| cx.notify()),
        ];
        Self {
            branch: status
                .and_then(|status| status.branch.clone())
                .unwrap_or_else(|| "feature/shared-ui".into()),
            base,
            title,
            body,
            phase: kit::ReviewDraftPrPhase::Idle,
            generating: false,
            error: None,
            sample: DraftPrSample::Ready,
            revision: 0,
            _subscriptions: subscriptions,
        }
    }
    pub(super) fn fields(&self, cx: &App) -> kit::ReviewDraftPrFields {
        kit::ReviewDraftPrFields {
            base: self.base.read(cx).value().to_string(),
            title: self.title.read(cx).value().to_string(),
            body: self.body.read(cx).value().to_string(),
        }
    }
    pub(super) fn set_sample(&mut self, sample: DraftPrSample, cx: &mut Context<Self>) {
        self.revision = self.revision.wrapping_add(1);
        self.sample = sample;
        self.generating = false;
        self.phase = match sample {
            DraftPrSample::Uncertain => kit::ReviewDraftPrPhase::Uncertain,
            DraftPrSample::Created => kit::ReviewDraftPrPhase::Created,
            _ => kit::ReviewDraftPrPhase::Idle,
        };
        self.error = match sample {
            DraftPrSample::Failure => Some("Couldn’t create the draft pull request. Sample request failed. Review the fields and try again.".into()),
            DraftPrSample::Uncertain => Some("Couldn’t confirm whether GitHub created the draft pull request. Sample connection interrupted. Use Check again before trying to create another.".into()),
            DraftPrSample::Created => Some("The draft pull request was created. Your newer edits remain in this dialog.".into()),
            _ => None,
        };
        cx.notify();
    }
    fn dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        if self.phase.is_busy() && self.sample != DraftPrSample::ChangedCheckout {
            return false;
        }
        self.revision = self.revision.wrapping_add(1);
        self.generating = false;
        cx.notify();
        true
    }

    pub(super) fn action(
        &mut self,
        action: &kit::ReviewDraftPrAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use kit::{ReviewDraftPrAction as Action, ReviewDraftPrPhase as Phase};
        if *action == Action::Close {
            if self.dismiss(cx) {
                window.close_dialog(cx);
            }
            return;
        }
        if self.phase.is_busy()
            || self.generating
            || self.phase == Phase::Created
            || self.sample == DraftPrSample::ChangedCheckout
        {
            return;
        }
        let fields = self.fields(cx);
        if *action == Action::Create {
            if self.phase == Phase::Uncertain {
                return;
            }
            if let Err(error) = fields.validate() {
                self.error = Some(error.into());
                cx.notify();
                return;
            }
        }
        if *action == Action::Check && self.phase != Phase::Uncertain {
            return;
        }
        let generate = matches!(action, Action::GenerateTitle | Action::GenerateDescription);
        if generate && self.phase == Phase::Uncertain {
            return;
        }
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        let action = *action;
        self.error = None;
        self.generating = generate;
        if !generate {
            self.phase = if action == Action::Check {
                Phase::Checking
            } else {
                Phase::Creating
            };
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.revision != revision { return; }
                this.generating = false;
                let current = this.fields(cx);
                if generate {
                    let unchanged = current.base == fields.base && match action { Action::GenerateTitle => current.title == fields.title, _ => current.body == fields.body };
                    if !unchanged { this.error = Some("The checkout or field changed; generated text was not applied.".into()); }
                    else if action == Action::GenerateTitle { this.title.update(cx, |input, cx| input.set_value("Refine the shared Review components", window, cx)); }
                    else { this.body.update(cx, |input, cx| input.set_value("## Change\nUse the same Review components on desktop and web.\n\n## Validation\nCheck layout, keyboard controls, and preserved drafts.", window, cx)); }
                } else if action == Action::Check {
                    this.phase = Phase::Idle;
                    this.error = Some("GitHub did not create the draft pull request. You can try again.".into());
                } else if this.sample == DraftPrSample::Failure {
                    this.phase = Phase::Idle;
                    this.error = Some("Couldn’t create the draft pull request. Sample request failed. Review the fields and try again.".into());
                } else if current != fields {
                    this.phase = Phase::Created;
                    this.error = Some("The draft pull request was created. Your newer edits remain in this dialog.".into());
                } else {
                    this.phase = Phase::Idle;
                    window.close_dialog(cx);
                    window.push_notification(gpui_component::notification::Notification::info("Sample draft completed · no GitHub request was sent"), cx);
                }
                cx.notify();
                window.refresh();
            });
        }).detach();
    }
}
impl Render for DraftPrPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        kit::review_draft_pr_form(
            &kit::ReviewDraftPrForm {
                branch: &self.branch,
                base: &self.base,
                title: &self.title,
                body: &self.body,
                phase: self.phase,
                generating: self.generating,
                context_matches: self.sample != DraftPrSample::ChangedCheckout,
                creation_available: true,
                error: self.error.as_deref(),
            },
            cx.listener(|this, action, window, cx| this.action(action, window, cx)),
            cx,
        )
    }
}
pub(super) fn open(state: Entity<DraftPrPreview>, window: &mut Window, cx: &mut App) {
    let content = state.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let submit = content.clone();
        let cancel = content.clone();
        kit::review_draft_pr_dialog(dialog)
            .child(content.clone())
            .on_ok(move |_, window, cx| {
                submit.update(cx, |this, cx| {
                    this.action(&kit::ReviewDraftPrAction::Create, window, cx)
                });
                false
            })
            .on_cancel(move |_, _, cx| cancel.update(cx, |host, cx| host.dismiss(cx)))
    });
    state
        .read(cx)
        .base
        .read(cx)
        .focus_handle(cx)
        .focus(window, cx);
}

#[cfg(test)]
#[path = "draft_pr_tests.rs"]
mod tests;
