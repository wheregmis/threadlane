//! Controlled draft-PR presentation; hosts own requests and checkout identity.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::dialog::Dialog;
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Disableable, Sizable};
use std::rc::Rc;
use threadlane_protocol::repo::GitStatus;

pub const REVIEW_PR_BASE_PLACEHOLDER: &str = "Base branch";
pub const REVIEW_PR_TITLE_PLACEHOLDER: &str = "Pull request title";
pub const REVIEW_PR_BODY_PLACEHOLDER: &str = "Describe the change and how it was verified";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewDraftPrFields {
    pub base: String,
    pub title: String,
    pub body: String,
}

impl ReviewDraftPrFields {
    pub fn validate(&self) -> Result<(), &'static str> {
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

pub fn review_draft_pr_prefill(status: &GitStatus) -> ReviewDraftPrFields {
    fn nonempty(value: &str) -> Option<&str> {
        let value = value.trim();
        (!value.is_empty()).then_some(value)
    }
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
    ReviewDraftPrFields {
        base,
        title: summary.into(),
        body: body.into(),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReviewDraftPrPhase {
    #[default]
    Idle,
    Creating,
    Uncertain,
    Checking,
    Created,
}
impl ReviewDraftPrPhase {
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Creating | Self::Checking)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDraftPrAction {
    Close,
    Create,
    Check,
    GenerateTitle,
    GenerateDescription,
}

pub struct ReviewDraftPrForm<'a> {
    pub branch: &'a str,
    pub base: &'a Entity<InputState>,
    pub title: &'a Entity<InputState>,
    pub body: &'a Entity<TextareaState>,
    pub phase: ReviewDraftPrPhase,
    pub generating: bool,
    pub context_matches: bool,
    pub creation_available: bool,
    pub error: Option<&'a str>,
}

pub fn review_draft_pr_dialog(dialog: Dialog) -> Dialog {
    dialog
        .title("Create draft pull request")
        .max_w_full()
        .close_button(false)
}

pub fn review_draft_pr_form(
    form: &ReviewDraftPrForm<'_>,
    on_action: impl Fn(&ReviewDraftPrAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().colors;
    let request = Rc::new(on_action);
    let busy = form.phase.is_busy();
    let created = form.phase == ReviewDraftPrPhase::Created;
    let uncertain = form.phase == ReviewDraftPrPhase::Uncertain;
    let fields = ReviewDraftPrFields {
        base: form.base.read(cx).value().to_string(),
        title: form.title.read(cx).value().to_string(),
        body: form.body.read(cx).value().to_string(),
    };
    let can_submit = form.creation_available
        && form.context_matches
        && fields.validate().is_ok()
        && !busy
        && !form.generating;
    let can_generate = !form.generating && !busy && !uncertain && !created && form.context_matches;
    let base = if fields.base.trim().is_empty() {
        "base branch"
    } else {
        fields.base.trim()
    };
    div().id("review-draft-pr-form").debug_selector(|| "review-draft-pr-form".into())
        .role(Role::Group).aria_label("Draft pull request fields")
        .w_full().min_w_0().flex().flex_col().gap_3()
        .child(div().id("review-draft-pr-context").debug_selector(|| "review-draft-pr-context".into())
            .whitespace_normal().text_sm().text_color(theme.muted_foreground)
            .child(format!("Creates a DRAFT pull request on GitHub from {} into {base}. No commits are pushed.", form.branch)))
        .child(crate::form_field(div().font_weight(FontWeight::MEDIUM).child("Base branch"),
            div().id("draft-pr-base").debug_selector(|| "draft-pr-base".into()).child(Input::new(form.base).aria_label("Base branch").disabled(created))))
        .child(crate::form_field(div().font_weight(FontWeight::MEDIUM).child("Title"),
            div().id("draft-pr-title").debug_selector(|| "draft-pr-title".into()).child(Input::new(form.title).aria_label("Pull request title").disabled(created))))
        .child(crate::form_field(div().font_weight(FontWeight::MEDIUM).child("Description"),
            div().id("draft-pr-body").debug_selector(|| "draft-pr-body".into()).child(Textarea::new(form.body).aria_label("Pull request description").disabled(created))))
        .child(div().flex().flex_wrap().gap_2()
            .child(Button::new("regenerate-pr-title").debug_selector(|| "regenerate-pr-title".into())
                .label("Regenerate title").small().disabled(!can_generate)
                .on_click({let request = request.clone(); move |_, window, cx| request(&ReviewDraftPrAction::GenerateTitle, window, cx)}))
            .child(Button::new("regenerate-pr-description").debug_selector(|| "regenerate-pr-description".into())
                .label("Regenerate description").small().disabled(!can_generate)
                .on_click({let request = request.clone(); move |_, window, cx| request(&ReviewDraftPrAction::GenerateDescription, window, cx)})))
        .children(form.generating.then(|| div().id("draft-pr-generating").role(Role::Status).aria_label("Generating pull request text").text_sm().child("Generating…")))
        .children((!form.context_matches).then(|| div().id("draft-pr-context-error").debug_selector(|| "draft-pr-context-error".into())
            .role(Role::Alert).text_sm().text_color(theme.danger)
            .child("The active checkout or branch changed. Close this dialog and open it again.")))
        .children(form.error.map(|error| div().id("draft-pr-error").debug_selector(|| "draft-pr-error".into())
            .role(if created { Role::Status } else { Role::Alert }).aria_label(error.to_owned())
            .whitespace_normal().text_sm().text_color(if created { theme.success } else { theme.danger }).child(error.to_owned())))
        .children(busy.then(|| {
            let label = if form.phase == ReviewDraftPrPhase::Checking { "Checking GitHub…" } else { "Creating draft on GitHub…" };
            div().id("draft-pr-busy").role(Role::Status).aria_label(label).flex().gap_2().text_sm().text_color(theme.muted_foreground)
                .child(Spinner::new().small()).child(label)
        }))
        .child(div().flex().flex_wrap().justify_end().gap_2().pt_2().border_t_1().border_color(theme.border)
            .child(Button::new("cancel-draft-pr").debug_selector(|| "cancel-draft-pr".into())
                .label(if created { "Done" } else { "Cancel" }).tooltip("Close dialog").accessibility_label("Close draft pull request dialog")
                .disabled(busy && form.context_matches)
                .on_click({let request = request.clone(); move |_, window, cx| request(&ReviewDraftPrAction::Close, window, cx)}))
            .children((!uncertain && !created).then(|| Button::new("submit-draft-pr").debug_selector(|| "submit-draft-pr".into())
                .label(if form.phase == ReviewDraftPrPhase::Checking { "Checking…" } else if busy { "Creating…" } else { "Create draft" }).primary().disabled(!can_submit)
                .tooltip(if form.context_matches { "Create a draft pull request on GitHub" } else { "The active checkout or branch changed" })
                .accessibility_label(if form.context_matches { "Create a draft pull request on GitHub" } else { "Create a draft pull request on GitHub · Active checkout or branch changed" })
                .on_click({let request = request.clone(); move |_, window, cx| request(&ReviewDraftPrAction::Create, window, cx)})))
            .children((uncertain && !busy).then(|| Button::new("check-draft-pr").debug_selector(|| "check-draft-pr".into())
                .label("Check again").outline().disabled(!form.context_matches)
                .tooltip("Re-check pull request status on GitHub").accessibility_label("Re-check draft pull request status on GitHub")
                .on_click(move |_, window, cx| request(&ReviewDraftPrAction::Check, window, cx)))))
}
