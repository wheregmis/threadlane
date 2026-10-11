//! Discussion, editable drafts and commits. Hosts own projection, input and publish guards.
use super::{detail_content, detail_markdown_style};
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::spinner::Spinner;
use gpui_component::tag::Tag;
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme, Disableable, Sizable};
use gpui_kit::base::Link;
use std::rc::Rc;
use threadlane_protocol::repo::GitHubPrCommit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubConversationAction {
    Reply,
    AskAgent,
}

/// A prepared row, without a second source of conversation or reply state.
pub struct GitHubConversationEntry {
    id: String,
    label: String,
    author: String,
    time: String,
    body: String,
    location: Option<String>,
    review: bool,
    reply_author: Option<String>,
}
impl GitHubConversationEntry {
    pub fn new(id: String, label: String, author: String, time: String, body: String) -> Self {
        Self {
            id,
            label,
            author,
            time,
            body,
            location: None,
            review: false,
            reply_author: None,
        }
    }
    pub fn location(mut self, location: Option<String>) -> Self {
        self.location = location;
        self
    }
    pub fn review(mut self, review: bool) -> Self {
        self.review = review;
        self
    }
    pub fn reply_author(mut self, author: Option<String>) -> Self {
        self.reply_author = author;
        self
    }
}
pub fn github_conversation_row(
    row: GitHubConversationEntry,
    callback: impl Fn(GitHubConversationAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let callback = Rc::new(callback);
    let reply = callback.clone();
    let id = row.id.clone();
    div()
        .id(SharedString::from(format!("github-pr-conversation-{id}")))
        .debug_selector(move || format!("github-pr-conversation-{id}").into())
        .w_full()
        .min_w_0()
        .py_3()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_2()
                .text_xs()
                .child(Tag::new().small().child(row.label))
                .child(div().font_weight(FontWeight::MEDIUM).child(row.author))
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(row.time),
                )
                .children(
                    row.location
                        .map(|location| div().min_w_0().whitespace_normal().child(location)),
                ),
        )
        .child(if row.body.trim().is_empty() {
            div()
                .mt_2()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(if row.review {
                    "No review body"
                } else {
                    "No comment body"
                })
                .into_any_element()
        } else {
            div()
                .mt_2()
                .text_sm()
                .child(
                    TextView::markdown(
                        SharedString::from(format!("github-pr-conversation-body-{}", row.id)),
                        row.body,
                    )
                    .style(detail_markdown_style())
                    .selectable(true),
                )
                .into_any_element()
        })
        .child(
            div()
                .mt_2()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(row.reply_author.map(|author| {
                    let id = format!("github-pr-reply-{}", row.id);
                    Button::new(SharedString::from(id.clone()))
                        .debug_selector(move || id.clone().into())
                        .label("Reply")
                        .ghost()
                        .xsmall()
                        .tooltip(format!("Reply to {author}"))
                        .accessibility_label(format!("Reply to {author}"))
                        .on_click(move |_, window, cx| {
                            reply(GitHubConversationAction::Reply, window, cx)
                        })
                }))
                .child({
                    let id = format!("github-pr-draft-reply-{}", row.id);
                    Button::new(SharedString::from(id.clone()))
                        .debug_selector(move || id.clone().into())
                        .label("Ask agent to draft reply")
                        .ghost()
                        .xsmall()
                        .tooltip("Ask the coding agent to draft a reply to this thread")
                        .accessibility_label("Ask the coding agent to draft a reply to this thread")
                        .on_click(move |_, window, cx| {
                            callback(GitHubConversationAction::AskAgent, window, cx)
                        })
                }),
        )
}

pub fn github_pr_conversation(
    comments: Vec<AnyElement>,
    editor: AnyElement,
    reply_editor: Option<AnyElement>,
    scroll: &ScrollHandle,
    cx: &App,
) -> impl IntoElement {
    div()
        .id("github-pr-conversation")
        .debug_selector(|| "github-pr-conversation".into())
        .size_full()
        .min_w_0()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .vertical_scrollbar(scroll)
        .child(
            detail_content()
                .py_4()
                .child(
                    div()
                        .debug_selector(|| "github-pr-conversation-comments".into())
                        .children(comments.is_empty().then(|| {
                            div()
                                .py_6()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No comments yet.")
                        }))
                        .children(comments),
                )
                .child(editor)
                .children(reply_editor),
        )
}

/// Reveal the retained draft on the next layout and route typing to it immediately.
pub fn focus_github_reply(
    input: &Entity<TextareaState>,
    scroll: &ScrollHandle,
    window: &mut Window,
    cx: &mut App,
) {
    scroll.scroll_to_bottom();
    input.update(cx, |input, cx| input.focus(window, cx));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubDraftAction {
    Post,
    Retry,
    PostNewDraft,
    CheckAgain,
    ClearDraft,
}
pub fn github_draft_recovery(id: SharedString, url: String, cx: &App) -> Link {
    Link::new(id)
        .href(url)
        .open_with(|url, _, _, cx| cx.open_url(url))
        .accessibility_label("Open published draft on GitHub")
        .text_sm()
        .text_color(cx.theme().link)
        .underline()
        .cursor_pointer()
        .border_1()
        .border_color(cx.theme().transparent)
        .rounded(cx.theme().radius)
        .px_1()
        .focus_visible(|style| style.border_color(cx.theme().ring))
        .child("Open on GitHub")
}
pub fn github_draft_action(
    reply: bool,
    action: GitHubDraftAction,
    disabled: bool,
    callback: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    let prefix = if reply { "reply" } else { "comment" };
    let suffix = match action {
        GitHubDraftAction::CheckAgain => "check-again",
        GitHubDraftAction::ClearDraft => "clear",
        _ => "publish",
    };
    let id = format!("github-pr-{prefix}-{suffix}");
    Button::new(SharedString::from(id.clone()))
        .debug_selector(move || id.clone().into())
        .label(match action {
            GitHubDraftAction::Retry => "Retry",
            GitHubDraftAction::CheckAgain => "Check again",
            GitHubDraftAction::ClearDraft if reply => "Clear target and draft",
            GitHubDraftAction::ClearDraft => "Clear draft",
            GitHubDraftAction::PostNewDraft if reply => "Post new reply",
            GitHubDraftAction::PostNewDraft => "Post new draft",
            GitHubDraftAction::Post if reply => "Reply",
            GitHubDraftAction::Post => "Post comment",
        })
        .small()
        .when(action == GitHubDraftAction::ClearDraft, |button| {
            button.ghost()
        })
        .disabled(disabled)
        .on_click(callback)
}

/// Hosts retain the input entity, target, draft and publication reconciliation.
pub struct GitHubCommentEditor {
    input: Entity<TextareaState>,
    reply: bool,
    target: Option<(String, String, Option<String>)>,
    before: Vec<AnyElement>,
    after: Vec<AnyElement>,
    validation: Vec<String>,
    status: Option<(String, bool, bool)>,
    error: Option<String>,
}
impl GitHubCommentEditor {
    pub fn new(input: Entity<TextareaState>, reply: bool) -> Self {
        Self {
            input,
            reply,
            target: None,
            before: Vec::new(),
            after: Vec::new(),
            validation: Vec::new(),
            status: None,
            error: None,
        }
    }
    pub fn target(mut self, author: String, body: String, location: Option<String>) -> Self {
        self.target = Some((author, body, location));
        self
    }
    pub fn controls(mut self, before: Vec<AnyElement>, after: Vec<AnyElement>) -> Self {
        self.before = before;
        self.after = after;
        self
    }
    pub fn validation(mut self, messages: Vec<String>) -> Self {
        self.validation = messages;
        self
    }
    pub fn status(mut self, status: Option<String>, busy: bool, failed: bool) -> Self {
        self.status = status.map(|status| (status, busy, failed));
        self
    }
    pub fn error(mut self, error: Option<String>) -> Self {
        self.error = error;
        self
    }
    pub fn render(self, cx: &App) -> Stateful<Div> {
        let id = if self.reply {
            "github-pr-reply-editor"
        } else {
            "github-pr-comment-editor"
        };
        let controls = |controls: Vec<AnyElement>| {
            (!controls.is_empty()).then(|| {
                div()
                    .mt_2()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .children(controls)
            })
        };
        div()
            .id(id)
            .debug_selector(move || id.into())
            .tab_group()
            .w_full()
            .min_w_0()
            .flex_none()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Draft · Not published"),
            )
            .children(self.target.map(|(author, body, location)| {
                div()
                    .mt_1()
                    .text_xs()
                    .whitespace_normal()
                    .child(format!(
                        "Reply to {author}{}",
                        location
                            .map(|location| format!(" · {location}"))
                            .unwrap_or_default()
                    ))
                    .child(
                        div()
                            .mt_1()
                            .text_color(cx.theme().muted_foreground)
                            .child(body),
                    )
            }))
            .children(controls(self.before))
            .child(
                div()
                    .mt_2()
                    .child(Textarea::new(&self.input).aria_label(if self.reply {
                        "Reply to review comment"
                    } else {
                        "New review comment"
                    })),
            )
            .children(self.validation.into_iter().map(|message| {
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .whitespace_normal()
                    .child(message)
            }))
            .children(self.status.map(|(status, busy, failed)| {
                div()
                    .id(SharedString::from(format!("{id}-status")))
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .mt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(if failed {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .children(busy.then(|| Spinner::new().xsmall()))
                    .child(status)
            }))
            .children(self.error.map(|error| {
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .whitespace_normal()
                    .child(error)
            }))
            .children(controls(self.after))
    }
}

pub fn github_pr_commit_row(commit: &GitHubPrCommit, cx: &App) -> impl IntoElement {
    let id = commit.oid.clone();
    div()
        .id(SharedString::from(format!("github-pr-commit-{id}")))
        .debug_selector(move || format!("github-pr-commit-{id}").into())
        .w_full()
        .min_w_0()
        .py_3()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            detail_content()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .whitespace_normal()
                        .child(commit.message.clone()),
                )
                .child(
                    div()
                        .mt_1()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            div()
                                .font_family(cx.theme().mono_font_family.clone())
                                .child(commit.oid.chars().take(7).collect::<String>()),
                        )
                        .child(commit.author.clone())
                        .child(commit.committed_at.clone()),
                ),
        )
}
/// The host provides its retained virtual list and edge-owned scrollbar.
pub fn github_pr_commits(count: usize, list: Option<AnyElement>, cx: &App) -> Div {
    div()
        .size_full()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            detail_content()
                .py_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(
                    div()
                        .debug_selector(|| "github-pr-commit-count".into())
                        .child(format!(
                            "{count} {}",
                            if count == 1 { "commit" } else { "commits" }
                        )),
                ),
        )
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .children(list)
                .children((count == 0).then(|| {
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No commits reported.")
                })),
        )
}
