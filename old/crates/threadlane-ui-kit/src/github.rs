//! GitHub presentation. Hosts own requests, selection, list state and workflows.
#[path = "github_detail.rs"]
mod detail;
pub use detail::*;
#[path = "github_issue_form.rs"]
mod issue_form;
pub use issue_form::*;
#[path = "github_issue_task.rs"]
mod issue_task;
pub use issue_task::*;
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::resizable::{h_resizable, resizable_panel, ResizableState};
use gpui_component::spinner::Spinner;
use gpui_component::status_bar::StatusBar;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubStateFilter {
    Open,
    Closed,
    Merged,
}
impl GitHubStateFilter {
    pub fn value(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Merged => "merged",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubListAction {
    State(GitHubStateFilter),
    Refresh,
    NewIssue,
    LoadMore,
    Settings,
    AttachProject,
}
type Callback = Rc<dyn Fn(GitHubListAction, &mut Window, &mut App)>;

/// Controlled availability for the collection toolbar; no services run here.
pub struct GitHubListControls {
    state: GitHubStateFilter,
    pull_requests: bool,
    loading: bool,
    has_results: bool,
    has_scope: bool,
    single_project: bool,
}
impl GitHubListControls {
    pub fn new(state: GitHubStateFilter) -> Self {
        Self {
            state,
            pull_requests: false,
            loading: false,
            has_results: false,
            has_scope: true,
            single_project: false,
        }
    }
    pub fn pull_requests(mut self, value: bool) -> Self {
        self.pull_requests = value;
        self
    }
    pub fn loading(mut self, value: bool) -> Self {
        self.loading = value;
        self
    }
    pub fn has_results(mut self, value: bool) -> Self {
        self.has_results = value;
        self
    }
    pub fn has_scope(mut self, value: bool) -> Self {
        self.has_scope = value;
        self
    }
    pub fn single_project(mut self, value: bool) -> Self {
        self.single_project = value;
        self
    }
}

pub fn github_toolbar(
    title: &'static str,
    inset: Option<Pixels>,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    div()
        .flex_none()
        .border_b_1()
        .border_color(cx.theme().title_bar_border)
        .bg(cx.theme().title_bar)
        // One shared header row: the title sits in the window-controls band
        // like Chat's header; `inset` clears the traffic lights when the
        // sidebar is hidden.
        .min_h(threadlane_ui_theme::WINDOW_CONTROLS_CLEARANCE)
        .px_4()
        .when_some(inset, |row, inset| row.pl(inset))
        .flex()
        .items_center()
        .gap_3()
        .child(
            Button::new("github-close")
                .accessibility_label("Back to chat")
                .icon(IconName::Close)
                .tooltip("Back to chat")
                .ghost()
                .small()
                .on_click(on_close),
        )
        .child(
            div()
                .id("github-page-heading")
                .debug_selector(|| "github-page-heading".into())
                .role(Role::Heading)
                .aria_label(title)
                .min_w_0()
                .flex_1()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(title),
        )
}

/// Hosts supply exact scope values; desktop uses optional project paths.
pub fn github_scope_row<T: Clone + PartialEq + 'static>(
    label: String,
    choices: Vec<(T, String)>,
    selected: T,
    on_select: impl Fn(T, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    div()
        .debug_selector(|| "github-scope-row".into())
        .flex_none()
        .border_b_1()
        .border_color(cx.theme().border)
        .px_3()
        .py_2()
        .flex()
        .items_center()
        .gap_2()
        .child(crate::choice_menu(
            Button::new("github-scope-picker")
                .label(label.clone())
                .dropdown_caret(true)
                .debug_selector(|| "github-scope-all".into())
                .icon(IconName::Folder)
                .accessibility_label(format!("Project scope: {label}"))
                .tooltip(format!("Project scope: {label}"))
                .ghost()
                .small(),
            choices,
            selected,
            on_select,
        ))
}

pub fn github_filters(
    query: &Entity<InputState>,
    controls: &GitHubListControls,
    on_action: impl Fn(GitHubListAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let callback: Callback = Rc::new(on_action);
    let mut choices = div().flex().flex_wrap().items_center().gap_2();
    for state in [
        GitHubStateFilter::Open,
        GitHubStateFilter::Closed,
        GitHubStateFilter::Merged,
    ] {
        if state == GitHubStateFilter::Merged && !controls.pull_requests {
            continue;
        }
        let id = format!("github-state-{}", state.value());
        let label = match state {
            GitHubStateFilter::Open => "Open",
            GitHubStateFilter::Closed => "Closed",
            GitHubStateFilter::Merged => "Merged",
        };
        let select = callback.clone();
        choices = choices.child(
            Button::new(id.clone())
                .debug_selector(move || id.clone())
                .label(label)
                .ghost()
                .small()
                .selected(controls.state == state)
                .accessibility_label(format!(
                    "{label}{}",
                    if controls.state == state {
                        ", selected"
                    } else {
                        ""
                    }
                ))
                .on_click(move |_, window, cx| select(GitHubListAction::State(state), window, cx)),
        );
    }
    let refresh = callback.clone();
    let refresh_hint = if !controls.has_scope {
        "Attach a project to refresh GitHub"
    } else if controls.loading {
        "GitHub is refreshing"
    } else {
        "Refresh GitHub"
    };
    choices = choices.child(
        Button::new("github-refresh")
            .debug_selector(|| "github-refresh".into())
            .accessibility_label(refresh_hint)
            .icon(Icon::default().path("icons/refresh-cw.svg"))
            .tooltip(refresh_hint)
            .ghost()
            .small()
            .disabled(!controls.has_scope || controls.loading)
            .on_click(move |_, window, cx| refresh(GitHubListAction::Refresh, window, cx)),
    );
    if !controls.pull_requests {
        let create = callback.clone();
        let hint = if controls.single_project {
            "Create an issue in this project"
        } else {
            "Scope to one project to create an issue"
        };
        choices = choices.child(
            Button::new("github-new-issue")
                .debug_selector(|| "github-new-issue".into())
                .label("New issue…")
                .accessibility_label(hint)
                .tooltip(hint)
                .ghost()
                .small()
                .disabled(!controls.single_project)
                .on_click(move |_, window, cx| create(GitHubListAction::NewIssue, window, cx)),
        );
    }
    choices = choices.children(controls.loading.then(|| {
        div()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(Spinner::new().xsmall())
            .child(if controls.has_results {
                "Refreshing…"
            } else {
                "Loading…"
            })
    }));
    div()
        .flex_none()
        .border_b_1()
        .border_color(cx.theme().border)
        .px_3()
        .py_2()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .debug_selector(|| "github-search-field".into())
                .min_w_0()
                .w_full()
                .flex_none()
                .child(
                    Input::new(query)
                        .aria_label("Search issues and pull requests")
                        .small(),
                ),
        )
        .child(choices)
}

#[derive(Clone, Copy)]
pub enum GitHubRowKind {
    OpenIssue,
    ClosedIssue,
    PullRequest,
}
/// A visible row snapshot. Identity includes kind, project and repository item number.
pub struct GitHubListRow {
    id: SharedString,
    kind: GitHubRowKind,
    title: String,
    context: String,
    author: String,
    suffix: Option<String>,
    metadata: Vec<(String, bool)>,
    selected: bool,
}
impl GitHubListRow {
    pub fn new(
        id: impl Into<SharedString>,
        kind: GitHubRowKind,
        title: String,
        context: String,
        author: String,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            title,
            context,
            author,
            suffix: None,
            metadata: Vec::new(),
            selected: false,
        }
    }
    pub fn suffix(mut self, value: Option<String>) -> Self {
        self.suffix = value;
        self
    }
    /// The boolean marks a failure, not a decorative label color.
    pub fn metadata(mut self, value: Vec<(String, bool)>) -> Self {
        self.metadata = value;
        self
    }
    pub fn selected(mut self, value: bool) -> Self {
        self.selected = value;
        self
    }
}

pub fn github_list_row(row: GitHubListRow, cx: &App) -> Button {
    let label = format!("{}: {}", row.context, row.title);
    let selector = row.id.clone();
    let icon = match row.kind {
        GitHubRowKind::OpenIssue => IconName::Asterisk,
        GitHubRowKind::ClosedIssue => IconName::CircleCheck,
        GitHubRowKind::PullRequest => IconName::Github,
    };
    Button::new(row.id)
        .debug_selector(move || selector.to_string())
        .accessibility_label(label.clone())
        .tooltip(label)
        .ghost()
        .h_auto()
        .w_full()
        .p_0()
        .selected(row.selected)
        .child(
            div()
                .w_full()
                .min_w_0()
                .whitespace_normal()
                .px_3()
                .py_3()
                .border_b_1()
                .border_color(cx.theme().border)
                .flex()
                .items_start()
                .gap_2()
                .child(
                    Icon::new(icon)
                        .small()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(row.title),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .truncate()
                                .child(row.context),
                        )
                        .child(
                            div()
                                .mt_1()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(div().flex_1().min_w_0().truncate().child(row.author))
                                .children(row.suffix.map(|value| div().flex_none().child(value))),
                        )
                        .children((!row.metadata.is_empty()).then(|| {
                            div().mt_1().flex().flex_wrap().gap_2().text_xs().children(
                                row.metadata.into_iter().map(|(label, failed)| {
                                    div()
                                        .text_color(if failed {
                                            cx.theme().danger
                                        } else {
                                            cx.theme().muted_foreground
                                        })
                                        .child(label)
                                }),
                            )
                        })),
                ),
        )
}

/// Host attaches its retained focus handle, key context, actions and virtual list.
pub fn github_list_surface(cx: &App) -> Stateful<Div> {
    div()
        .debug_selector(|| "github-result-list".into())
        .id("github-result-list")
        .role(Role::List)
        .relative()
        .size_full()
        .min_h_0()
        .border_1()
        .border_color(cx.theme().border)
        .focus(|style| style.border_color(cx.theme().primary))
}

pub fn github_load_more(loading: bool) -> Button {
    Button::new("github-load-more")
        .debug_selector(|| "github-load-more".into())
        .label(if loading { "Loading…" } else { "Load more" })
        .ghost()
        .small()
        .disabled(loading)
}

pub fn github_back_to_pr_list() -> Button {
    Button::new("github-back-to-pr-list")
        .debug_selector(|| "github-back-to-pr-list".into())
        .icon(IconName::ArrowLeft)
        .label("Back to pull requests")
        .ghost()
        .small()
}

pub fn github_list_warning(
    error: String,
    loading: bool,
    on_retry: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let hint = format!("Some projects couldn’t load: {error}");
    div()
        .debug_selector(|| "github-list-warning".into())
        .id("github-list-warning")
        .role(Role::Alert)
        .aria_label(hint.clone())
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(hint.clone()).build(window, cx)
        })
        .w_full()
        .flex_none()
        .px_3()
        .py_1p5()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .bg(cx.theme().warning.opacity(0.12))
        .text_xs()
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_color(cx.theme().foreground)
                .child("Some projects unavailable"),
        )
        .child(
            Button::new("github-list-warning-retry")
                .debug_selector(|| "github-list-warning-retry".into())
                .label("Retry")
                .ghost()
                .xsmall()
                .disabled(loading)
                .on_click(on_retry),
        )
        .child(
            Button::new("github-list-warning-copy")
                .debug_selector(|| "github-list-warning-copy".into())
                .label("Copy")
                .accessibility_label("Copy the complete GitHub error")
                .tooltip("Copy the complete GitHub error")
                .ghost()
                .xsmall()
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(error.clone()))
                }),
        )
}

pub fn github_error(
    id: &'static str,
    message: String,
    details: String,
    loading: bool,
    on_action: impl Fn(GitHubListAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let callback: Callback = Rc::new(on_action);
    let retry = callback.clone();
    div()
        .debug_selector(move || format!("github-{id}-error"))
        .w_full()
        .flex_none()
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .text_sm()
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().danger)
                .child("GitHub request failed"),
        )
        .child(div().text_color(cx.theme().foreground).child(message))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    Button::new(format!("github-{id}-error-retry"))
                        .debug_selector(move || format!("github-{id}-error-retry"))
                        .label("Retry")
                        .small()
                        .disabled(loading)
                        .on_click(move |_, window, cx| {
                            retry(GitHubListAction::Refresh, window, cx)
                        }),
                )
                .child(
                    Button::new(format!("github-{id}-error-copy"))
                        .debug_selector(move || format!("github-{id}-error-copy"))
                        .label("Copy details")
                        .accessibility_label("Copy the complete GitHub error")
                        .tooltip("Copy the complete GitHub error")
                        .ghost()
                        .small()
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(details.clone()))
                        }),
                )
                .child(
                    Button::new(format!("github-{id}-settings"))
                        .label("Open repository settings")
                        .ghost()
                        .small()
                        .on_click(move |_, window, cx| {
                            callback(GitHubListAction::Settings, window, cx)
                        }),
                ),
        )
}

pub fn github_empty(message: String, loading: bool, cx: &App) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .px_6()
        .text_sm()
        .text_center()
        .text_color(cx.theme().muted_foreground)
        .children(loading.then(|| Spinner::new().small()))
        .child(message)
}

pub fn github_no_project(
    on_action: impl Fn(GitHubListAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let callback: Callback = Rc::new(on_action);
    let attach = callback.clone();
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .px_6()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child("No project or repository is attached")
        .child(
            div()
                .text_xs()
                .text_center()
                .child("Attach a project to load its GitHub issues and pull requests."),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(
                    Button::new("github-attach-project")
                        .label("Attach project…")
                        .small()
                        .on_click(move |_, window, cx| {
                            attach(GitHubListAction::AttachProject, window, cx)
                        }),
                )
                .child(
                    Button::new("github-open-settings")
                        .label("Configure GitHub")
                        .ghost()
                        .small()
                        .on_click(move |_, window, cx| {
                            callback(GitHubListAction::Settings, window, cx)
                        }),
                ),
        )
}

pub fn github_master(scope: AnyElement, filters: AnyElement, list: AnyElement) -> Div {
    div()
        .size_full()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col()
        .child(scope)
        .child(filters)
        .child(div().flex_1().min_h_0().child(list))
}

pub fn github_master_detail(
    master: AnyElement,
    detail: AnyElement,
    split: &Entity<ResizableState>,
    window: &Window,
    cx: &App,
) -> AnyElement {
    if window.bounds().size.width < window.rem_size() * 56.25 {
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().child(master))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(detail),
            )
            .into_any_element()
    } else {
        h_resizable("github-master-detail")
            .with_state(split)
            .child(
                resizable_panel()
                    .size(window.rem_size() * 24.)
                    .size_range((window.rem_size() * 19.)..(window.rem_size() * 34.))
                    .child(master),
            )
            .child(resizable_panel().child(detail))
            .into_any_element()
    }
}

/// List summary ("2 issues · all projects · open"). The desktop shows it in the
/// shared workspace status bar so every page keeps one full-width bar.
pub fn github_status_text(
    count: usize,
    title: &'static str,
    scope: String,
    state: GitHubStateFilter,
    has_draft: bool,
) -> String {
    let item = match (title, count) {
        ("Issues", 1) => "issue".into(),
        ("Pull requests", 1) => "pull request".into(),
        _ => title.to_lowercase(),
    };
    format!(
        "{count} {} · {} · {}{}",
        item,
        scope.to_lowercase(),
        state.value(),
        if has_draft { " · Unsaved draft" } else { "" }
    )
}

/// Standalone status bar for hosts without a workspace bar (the preview).
pub fn github_status(
    count: usize,
    title: &'static str,
    scope: String,
    state: GitHubStateFilter,
    has_draft: bool,
) -> StatusBar {
    StatusBar::new().left(github_status_text(count, title, scope, state, has_draft))
}
