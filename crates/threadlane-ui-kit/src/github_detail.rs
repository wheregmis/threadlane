//! Shared GitHub detail presentation. Hosts retain requests, cached text and workflow state.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::link::Link;
use gpui_component::scroll::ScrollableElement;
use gpui_component::spinner::Spinner;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::tag::Tag;
use gpui_component::text::{TextView, TextViewState, TextViewStyle};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Sizable};
use std::rc::Rc;
use threadlane_protocol::repo::{GitHubIssueDetail, GitHubPrInfo, PrCheckStatus};

#[path = "github_conversation.rs"]
mod conversation;
pub use conversation::*;
#[path = "github_files.rs"]
mod files;
pub use files::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrDetailTab {
    #[default]
    Summary,
    Conversation,
    Timeline,
    Code,
}

impl PrDetailTab {
    pub const ALL: [Self; 4] = [
        Self::Summary,
        Self::Conversation,
        Self::Timeline,
        Self::Code,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Summary => "Overview",
            Self::Conversation => "Conversation",
            Self::Timeline => "Commits",
            Self::Code => "Files changed",
        }
    }

    pub fn ix(self) -> usize {
        match self {
            Self::Summary => 0,
            Self::Conversation => 1,
            Self::Timeline => 2,
            Self::Code => 3,
        }
    }

    pub fn adjacent(self, delta: isize) -> Self {
        Self::ALL[self
            .ix()
            .saturating_add_signed(delta)
            .min(Self::ALL.len() - 1)]
    }
}

pub fn pr_review_label(decision: &str) -> Option<&'static str> {
    match decision {
        "APPROVED" => Some("Approved"),
        "CHANGES_REQUESTED" => Some("Changes requested"),
        "REVIEW_REQUIRED" => Some("Review required"),
        _ => None,
    }
}

pub fn pr_check_status_label(check: &PrCheckStatus) -> String {
    let status = check
        .conclusion
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&check.status)
        .replace('_', " ")
        .to_ascii_lowercase();
    let mut chars = status.trim().chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => "Unknown".into(),
    }
}

pub fn github_pr_summary(
    detail: &GitHubPrInfo,
    body: &Entity<TextViewState>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme().colors;
    div().size_full().overflow_y_scrollbar().child(
        detail_content()
            .py_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    // State is already in the header metadata line, as for issues.
                    .children(detail.is_draft.then(|| Tag::new().small().child("Draft")))
                    .children(
                        detail
                            .review_decision
                            .clone()
                            .as_deref()
                            .and_then(pr_review_label)
                            .map(|decision| Tag::new().small().child(decision)),
                    ),
            )
            .child(
                div()
                    .mt_5()
                    .debug_selector(|| "github-detail-description".into())
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Description"),
            )
            .child(
                div()
                    .mt_2()
                    .text_sm()
                    // Prose reads at ~90 characters; tables and diffs keep the
                    // full detail width.
                    .max_w(rems(40.))
                    .child(if detail.body.trim().is_empty() {
                        div()
                            .text_color(theme.muted_foreground)
                            .child("No description provided.")
                            .into_any_element()
                    } else {
                        TextView::new(body).style(detail_markdown_style()).selectable(true).into_any_element()
                    }),
            )
            .child(
                div()
                    .mt_3()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Checks"),
            )
            .child(
                div()
                    .mt_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if detail.total_checks == 0 {
                        "No checks reported".to_owned()
                    } else {
                        format!(
                            "{} passing · {} pending · {} failing",
                            detail.passing_checks, detail.pending_checks, detail.failing_checks
                        )
                    }),
            )
            .children(detail.checks.iter().map(|check| {
                div()
                    .debug_selector({
                        let name = check.name.clone();
                        move || format!("github-pr-check-{name}")
                    })
                    .mt_2()
                    .flex()
                    .items_center()
                    .gap_3()
                    .text_sm()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .whitespace_normal()
                            .child(check.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .debug_selector({
                                let name = check.name.clone();
                                move || format!("github-pr-check-status-{name}")
                            })
                            .text_color(theme.muted_foreground)
                            .child(pr_check_status_label(check)),
                    )
                    .child(
                        div().w_20().flex_none().children(
                            check
                                .details_url
                                .as_deref()
                                .map(str::trim)
                                .filter(|url| {
                                    url.starts_with("https://") || url.starts_with("http://")
                                })
                                .map(|url| {
                                    gpui_kit::base::Link::new(SharedString::from(format!(
                                        "github-pr-check-log-{}-{}-{url}",
                                        detail.number, check.name
                                    )))
                                    .href(url.to_owned())
                                    .open_with(|url, _, _, cx| cx.open_url(url))
                                    .accessibility_label(format!("View logs for {}", check.name))
                                    .flex_none()
                                    .text_color(theme.link)
                                    .underline()
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(cx.theme().transparent)
                                    .rounded(cx.theme().radius)
                                    .px_1()
                                    .py_1()
                                    .hover(|style| style.bg(theme.list_hover))
                                    .focus_visible(|style| style.border_color(theme.primary))
                                    .debug_selector({
                                        let name = check.name.clone();
                                        move || format!("github-pr-check-log-{name}")
                                    })
                                    .child("View logs")
                                }),
                        ),
                    )
            })),
    )
}

/// Header and tabs remain fixed; the body owns its own scrolling region.
pub struct GitHubDetailHeader {
    title: String,
    metadata: String,
    branch: Option<String>,
    status: Option<String>,
    actions: AnyElement,
    tabs: Option<AnyElement>,
}
impl GitHubDetailHeader {
    pub fn new(title: String, metadata: String, actions: AnyElement) -> Self {
        Self {
            title,
            metadata,
            actions,
            branch: None,
            status: None,
            tabs: None,
        }
    }
    pub fn branch(mut self, branch: String) -> Self {
        self.branch = Some(branch);
        self
    }
    pub fn status(mut self, status: Option<String>) -> Self {
        self.status = status;
        self
    }
    pub fn tabs(mut self, tabs: AnyElement) -> Self {
        self.tabs = Some(tabs);
        self
    }
    pub fn render(self, cx: &App) -> Div {
        let is_pr = self.tabs.is_some();
        let content = detail_content()
            .pt_4()
            .when(!is_pr, |content| content.pb_4())
            .child(
                div()
                    .id("github-detail-title")
                    .debug_selector(|| "github-detail-title".into())
                    .role(Role::Heading)
                    .aria_label(self.title.clone())
                    .min_w_0()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .whitespace_normal()
                    .child(self.title),
            )
            .child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(self.metadata),
            )
            .children(self.branch.map(|branch| {
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(branch)
            }))
            .child(div().mt_3().child(self.actions))
            .children(self.status.map(|status| {
                div()
                    .id("github-detail-status")
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .mt_2()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(status)
            }))
            .children(self.tabs.map(|tabs| div().mt_3().w_full().child(tabs)))
            .when(is_pr, |header| {
                header.child(div().mt_3().border_b_1().border_color(cx.theme().border))
            });
        div()
            .flex_none()
            .min_w_0()
            .when(!is_pr, |header| {
                header
                    .debug_selector(|| "github-issue-detail-header".into())
                    .border_b_1()
                    .border_color(cx.theme().border)
            })
            .child(content)
    }
}
pub fn github_detail_surface(header: Div, body: AnyElement) -> Div {
    div()
        .size_full()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col()
        .child(header)
        .child(div().flex_1().min_h_0().min_w_0().child(body))
}
pub fn github_pr_tabs(
    tab: PrDetailTab,
    on_select: impl Fn(PrDetailTab, &mut Window, &mut App) + 'static,
) -> TabBar {
    TabBar::new("github-pr-detail-tabs")
        .underline()
        .small()
        .selected_index(tab.ix())
        .children(
            PrDetailTab::ALL
                .into_iter()
                .map(|candidate| Tab::new().label(candidate.label())),
        )
        .on_click(move |ix, window, cx| {
            if let Some(tab) = PrDetailTab::ALL.get(*ix).copied() {
                on_select(tab, window, cx);
            }
        })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubDetailAction {
    StartTask,
    SetIssueClosed(bool),
    SuggestLabels,
    DeleteIssue,
    OpenTask,
    AddressReviews,
}
type DetailCallback = Rc<dyn Fn(GitHubDetailAction, &mut Window, &mut App)>;

/// Controlled issue action availability. The host performs or confirms each request.
pub struct GitHubIssueActions {
    url: String,
    linked_task: bool,
    closed: bool,
    pending: bool,
}
impl GitHubIssueActions {
    pub fn new(url: String) -> Self {
        Self {
            url,
            linked_task: false,
            closed: false,
            pending: false,
        }
    }
    pub fn linked_task(mut self, value: bool) -> Self {
        self.linked_task = value;
        self
    }
    pub fn closed(mut self, value: bool) -> Self {
        self.closed = value;
        self
    }
    pub fn pending(mut self, value: bool) -> Self {
        self.pending = value;
        self
    }
}
pub fn github_issue_actions(
    controls: GitHubIssueActions,
    on_action: impl Fn(GitHubDetailAction, &mut Window, &mut App) + 'static,
) -> Div {
    let callback: DetailCallback = Rc::new(on_action);
    let start = callback.clone();
    let change = callback.clone();
    let suggest = callback.clone();
    let closed = controls.closed;
    let state_label = if closed {
        "Reopen this issue"
    } else {
        "Close this issue"
    };
    div()
        .flex()
        .items_center()
        .flex_wrap()
        .gap_3()
        .child(
            Link::new("github-open-browser").href(controls.url).child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(Icon::new(IconName::ExternalLink).small())
                    .child("Open on GitHub"),
            ),
        )
        .child(
            Button::new("github-start-agent-task")
                .debug_selector(|| "github-start-agent-task".into())
                .icon(IconName::Play)
                .label(if controls.linked_task {
                    "Start another…"
                } else {
                    "Start task…"
                })
                .small()
                .on_click(move |_, window, cx| start(GitHubDetailAction::StartTask, window, cx)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_2()
                .child(
                    Button::new("github-issue-close-reopen")
                        .debug_selector(|| "github-issue-close-reopen".into())
                        .label(if closed { "Reopen" } else { "Close" })
                        .accessibility_label(state_label)
                        .tooltip(state_label)
                        .ghost()
                        .small()
                        .disabled(controls.pending)
                        .on_click(move |_, window, cx| {
                            change(GitHubDetailAction::SetIssueClosed(!closed), window, cx)
                        }),
                )
                .child(
                    Button::new("github-issue-suggest-labels")
                        .debug_selector(|| "github-issue-suggest-labels".into())
                        .label("Suggest labels")
                        .accessibility_label("Ask the model to pick repository labels (no session)")
                        .tooltip("Ask the model to pick repository labels (no session)")
                        .ghost()
                        .small()
                        .disabled(controls.pending)
                        .on_click(move |_, window, cx| {
                            suggest(GitHubDetailAction::SuggestLabels, window, cx)
                        }),
                )
                .child(
                    Button::new("github-issue-delete")
                        .debug_selector(|| "github-issue-delete".into())
                        .label("Delete…")
                        .accessibility_label("Permanently delete this issue…")
                        .tooltip("Permanently delete this issue…")
                        .ghost()
                        .small()
                        .disabled(controls.pending)
                        .on_click(move |_, window, cx| {
                            callback(GitHubDetailAction::DeleteIssue, window, cx)
                        }),
                ),
        )
}
pub fn github_pr_actions(
    url: String,
    linked_title: Option<String>,
    actionable_reviews: bool,
    on_action: impl Fn(GitHubDetailAction, &mut Window, &mut App) + 'static,
) -> Div {
    let callback: DetailCallback = Rc::new(on_action);
    let open = callback.clone();
    div()
        .flex()
        .items_center()
        .gap_3()
        .flex_wrap()
        .child(
            Link::new("github-open-pr-browser")
                .href(url)
                .child("Open on GitHub"),
        )
        .children(linked_title.map(|title| {
            Button::new("github-open-linked-pr-task")
                .debug_selector(|| "github-open-linked-pr-task".into())
                .label("Open task")
                .accessibility_label(format!("Open task: {title}"))
                .tooltip(title)
                .small()
                .on_click(move |_, window, cx| open(GitHubDetailAction::OpenTask, window, cx))
        }))
        .children(actionable_reviews.then(|| {
            Button::new("github-address-pr-reviews")
                .debug_selector(|| "github-address-pr-reviews".into())
                .label("Address reviews")
                .ghost()
                .xsmall()
                .on_click(move |_, window, cx| {
                    callback(GitHubDetailAction::AddressReviews, window, cx)
                })
        }))
}

pub struct GitHubLinkedTask {
    id: String,
    title: String,
    project: String,
    status: String,
    worktree: bool,
    branch: Option<String>,
    pr_number: Option<u64>,
}
impl GitHubLinkedTask {
    pub fn new(id: String, title: String, project: String, status: String) -> Self {
        Self {
            id,
            title,
            project,
            status,
            worktree: false,
            branch: None,
            pr_number: None,
        }
    }
    pub fn worktree(mut self, value: bool) -> Self {
        self.worktree = value;
        self
    }
    pub fn branch(mut self, branch: Option<String>) -> Self {
        self.branch = branch;
        self
    }
    pub fn pr_number(mut self, number: Option<u64>) -> Self {
        self.pr_number = number;
        self
    }
}
pub fn github_linked_task(
    task: GitHubLinkedTask,
    on_open: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    div()
        .mt_2()
        .flex()
        .items_center()
        .flex_wrap()
        .gap_2()
        .text_sm()
        .child(
            div()
                .min_w_0()
                .w_full()
                .whitespace_normal()
                .child(task.title.clone()),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(task.project),
        )
        .child(Tag::new().small().child(task.status))
        .children(task.worktree.then(|| Tag::new().small().child("Worktree")))
        .children(task.branch.map(|branch| Tag::new().small().child(branch)))
        .children(
            task.pr_number
                .map(|number| Tag::new().small().child(format!("PR #{number}"))),
        )
        .child(
            Button::new(task.id.clone())
                .debug_selector(move || task.id.clone())
                .label("Open task")
                .accessibility_label(format!("Open task: {}", task.title))
                .tooltip(task.title)
                .ghost()
                .xsmall()
                .on_click(on_open),
        )
}
/// Comment identity comes from the remote record; hosts retain virtualization state.
pub fn github_comment(id: String, author: String, time: String, body: String, cx: &App) -> Div {
    div()
        .p_3()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .child(format!("{author} · {time}")),
        )
        .child(div().mt_2().text_sm().child(if body.trim().is_empty() {
            div()
                .text_color(cx.theme().muted_foreground)
                .child("No comment body")
                .into_any_element()
        } else {
            TextView::markdown(SharedString::from(id), body)
                .style(detail_markdown_style())
                .selectable(true)
                .into_any_element()
        }))
}
pub fn github_issue_body(
    detail: &GitHubIssueDetail,
    body: &Entity<TextViewState>,
    linked_tasks: Vec<AnyElement>,
    comments: Option<(usize, AnyElement)>,
    refreshing: bool,
    cx: &App,
) -> impl IntoElement {
    div()
        .size_full()
        .min_h_0()
        .min_w_0()
        .overflow_y_scrollbar()
        .child(
            detail_content()
                .pb_5()
                .children(
                    (!detail.summary.labels.is_empty() || !detail.summary.assignees.is_empty())
                        .then(|| {
                            div()
                                .mt_4()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .children(
                                    detail
                                        .summary
                                        .labels
                                        .iter()
                                        .map(|label| Tag::new().small().child(label.name.clone())),
                                )
                                .children((!detail.summary.assignees.is_empty()).then(|| {
                                    div().child(format!(
                                        "Assigned to @{}",
                                        detail.summary.assignees.join(" @")
                                    ))
                                }))
                        }),
                )
                .child(
                    div()
                        .mt_5()
                        .debug_selector(|| "github-detail-description".into())
                        .text_sm()
                        .max_w(rems(40.))
                        .child(if detail.body.trim().is_empty() {
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child("No description provided.")
                                .into_any_element()
                        } else {
                            TextView::new(body).style(detail_markdown_style()).selectable(true).into_any_element()
                        }),
                )
                .children((!linked_tasks.is_empty()).then(|| {
                    div()
                        .mt_5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Linked tasks"),
                        )
                        .children(linked_tasks)
                }))
                .children(comments.map(|(count, comments)| {
                    div()
                        .mt_5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("Conversation ({})", count)),
                        )
                        .child(
                            div()
                                .relative()
                                .mt_2()
                                .h_64()
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded(cx.theme().radius)
                                .child(comments),
                        )
                }))
                .children(refreshing.then(|| {
                    div()
                        .mt_3()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(Spinner::new().xsmall())
                        .child("Refreshing details…")
                })),
        )
}

// One content spine keeps detail headers, descriptions and checks aligned at every width.
fn detail_content() -> Div {
    div().w_full().max_w(rems(52.)).mx_auto().px_5()
}

/// Markdown headings inside an issue/PR body stay at or below the detail title
/// (`text_lg`). The default scale renders `#`/`##` at 28/21px, outranking the
/// page they belong to.
fn detail_markdown_style() -> TextViewStyle {
    TextViewStyle::default().heading_font_size(|level, base| {
        base * match level {
            1 => 1.25,
            2 => 1.15,
            3 => 1.075,
            _ => 1.0,
        }
    })
}
