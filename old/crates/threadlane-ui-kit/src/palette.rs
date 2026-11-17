//! Shared workspace palette presentation. Hosts retain command state, routing and search.
use gpui::{prelude::*, *};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName};

pub struct WorkspaceCommand {
    name: &'static str,
    description: &'static str,
    key: &'static str,
    icon: Icon,
    keywords: &'static [&'static str],
    shortcut: &'static str,
}
impl WorkspaceCommand {
    pub fn key(&self) -> &'static str {
        self.key
    }
    pub fn item(&self, disabled_reason: Option<&str>) -> CommandItem {
        let title: SharedString = self.name.into();
        let description: SharedString = disabled_reason
            .unwrap_or(self.description)
            .to_owned()
            .into();
        let shortcut: SharedString = self.shortcut.into();
        CommandItem::new()
            .label(title.clone())
            .icon(self.icon.clone())
            .keywords(self.keywords.iter().copied())
            .disabled(disabled_reason.is_some())
            .child(move |_, cx| {
                palette_row(
                    title.clone(),
                    description.clone(),
                    shortcut.clone(),
                    None,
                    cx,
                )
            })
    }
}

/// One catalogue for every host. Capability policy is supplied when rendering each item.
pub fn workspace_commands() -> Vec<WorkspaceCommand> {
    let commands: [(&str, &str, &str, Icon, &[&str], &str); 27] = [
        (
            "New Task",
            "Start a fresh session",
            "new",
            Icon::from(IconName::Plus),
            &["task", "fresh", "session", "new"],
            crate::navigation::new_task_shortcut(),
        ),
        (
            "Go to Task…",
            "Switch to a recently visited session",
            "go_task",
            Icon::from(IconName::Search),
            &["go", "task", "jump", "find", "session", "recent"],
            if cfg!(target_os = "macos") {
                "⇧⌘K"
            } else {
                "Ctrl+Shift+K"
            },
        ),
        (
            "Open File…",
            "Browse project files in the right panel",
            "open_file",
            Icon::from(IconName::File),
            &["open", "file", "browse", "tree", "explorer"],
            "",
        ),
        (
            "Run Terminal Command…",
            "Open or focus the integrated terminal",
            "run_terminal",
            Icon::from(IconName::SquareTerminal),
            &["run", "terminal", "command", "shell", "exec"],
            "⌘J",
        ),
        (
            "Terminal: Open link…",
            "Links in visible output",
            "open_terminal_link",
            Icon::from(IconName::ExternalLink),
            &["terminal", "link", "url", "browser", "open"],
            "",
        ),
        (
            "Add Selection to Chat",
            "Append the selected terminal text to the chat draft",
            "add_terminal_selection",
            Icon::default().path("icons/square-pen.svg"),
            &["terminal", "selection", "chat", "draft", "add", "output"],
            "",
        ),
        (
            "Add File Selection to Chat",
            "Append the selected editor code to the chat draft",
            "add_editor_selection",
            Icon::default().path("icons/square-pen.svg"),
            &["editor", "selection", "code", "chat", "draft", "add", "file", "excerpt"],
            "",
        ),
        (
            "Find in files…",
            "Search saved files in the active checkout",
            "find_files",
            Icon::from(IconName::Search),
            &["find", "files", "search", "text", "contents"],
            "",
        ),
        (
            "Search project conversations…",
            "Find saved messages across this project's sessions",
            "search_conversations",
            Icon::from(IconName::Search),
            &["search", "conversations", "messages", "find", "transcript"],
            "",
        ),
        (
            "Open Issue/PR…",
            "Browse GitHub issues and pull requests",
            "open_issue",
            Icon::from(IconName::Github),
            &["issue", "pr", "pull", "request", "github", "browse"],
            "",
        ),
        (
            "Toggle Worktree Mode",
            "Toggle new-task execution between local and worktree mode",
            "switch_worktree",
            Icon::from(IconName::FolderOpen),
            &["switch", "worktree", "mode", "local", "branch"],
            "",
        ),
        (
            "Ask Agent to…",
            "Focus the composer to prompt the agent",
            "ask_agent",
            Icon::from(IconName::Bot),
            &["ask", "agent", "prompt", "chat", "ai", "help"],
            "⌘L",
        ),
        (
            "Add Project",
            "Attach a project folder to your workspace",
            "attach",
            Icon::from(IconName::FolderOpen),
            &["folder", "workspace", "attach", "open", "project"],
            "",
        ),
        (
            "Goal Planning (/goal)",
            "Autonomous goal loop extension",
            "goal",
            Icon::from(IconName::Bot),
            &["goal", "planning", "loop", "agent", "autonomous"],
            "",
        ),
        (
            "Model Selection (/model)",
            "Switch model or provider",
            "model",
            Icon::from(IconName::Cpu),
            &["model", "llm", "switch", "provider", "select"],
            "",
        ),
        (
            "Compact History (/compact)",
            "Compact context conversation",
            "compact",
            Icon::from(IconName::Minimize),
            &["compact", "history", "context", "clean"],
            "",
        ),
        (
            "Git Review & Commit",
            "Review changed files and commit",
            "git",
            Icon::default().path("icons/git/commit.svg"),
            &["git", "diff", "review", "commit", "stage"],
            "",
        ),
        (
            "Automations",
            "Schedule recurring prompts and review runs",
            "automations",
            Icon::from(IconName::Calendar),
            &["automation", "schedule", "recurring", "runs"],
            "",
        ),
        (
            "GitHub",
            "Browse project issues and pull requests",
            "github",
            Icon::default().path("icons/git/comments.svg"),
            &["github", "issues", "pull requests", "repository"],
            "",
        ),
        (
            "Git: Switch Branch",
            "Switch or checkout a Git branch",
            "git_branch",
            Icon::default().path("icons/git/branch.svg"),
            &["git", "branch", "switch", "checkout"],
            "",
        ),
        (
            "Git: New Branch",
            "Create a new branch from current HEAD",
            "git_new_branch",
            Icon::from(IconName::Plus),
            &["git", "branch", "new", "create"],
            "",
        ),
        (
            "Git: Merge Branch",
            "Merge another branch into current branch",
            "git_merge",
            Icon::default().path("icons/git/pull-request.svg"),
            &["git", "merge", "branch", "integrate"],
            "",
        ),
        (
            "Git: Restore Stashed Changes",
            "Restore changes previously stashed on this branch",
            "git_stash_pop",
            Icon::from(IconName::Undo2),
            &["git", "stash", "pop", "restore", "unstash"],
            "",
        ),
        (
            "Git: Pull Origin",
            "Pull latest commits from remote origin",
            "git_pull",
            Icon::from(IconName::ArrowDown),
            &["git", "pull", "origin", "fetch", "sync"],
            "",
        ),
        (
            "Toggle Sidebar",
            "Show or hide your projects and tasks",
            "sidebar",
            Icon::from(IconName::PanelLeft),
            &["sidebar", "toggle", "hide", "show", "projects"],
            "⌘B",
        ),
        (
            "Toggle Right Panel",
            "Show review / files / terminal",
            "panel",
            Icon::from(IconName::PanelRight),
            &["panel", "right", "terminal", "review", "toggle"],
            "⌘R",
        ),
        (
            "Settings",
            "Configure API keys and providers",
            "settings",
            Icon::from(IconName::Settings),
            &["settings", "keys", "provider", "preferences", "config"],
            "⌘,",
        ),
    ];
    commands
        .into_iter()
        .map(
            |(name, description, key, icon, keywords, shortcut)| WorkspaceCommand {
                name,
                description,
                key,
                icon,
                keywords,
                shortcut,
            },
        )
        .collect()
}

fn palette_row(
    title: SharedString,
    description: SharedString,
    shortcut: SharedString,
    excerpt: Option<SharedString>,
    cx: &App,
) -> Stateful<Div> {
    let colors = cx.theme().colors;
    let accessible = format!(
        "{title}. {description}{}{}",
        if shortcut.is_empty() {
            String::new()
        } else {
            format!(". {shortcut}")
        },
        excerpt
            .as_ref()
            .map(|text| format!(". {text}"))
            .unwrap_or_default()
    );
    div()
        .id("palette-row-content")
        .role(Role::Label)
        .aria_label(accessible)
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .truncate()
                        .child(description),
                )
                .when_some(excerpt, |el, text| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(colors.muted_foreground)
                            .truncate()
                            .child(text),
                    )
                }),
        )
        .when(!shortcut.is_empty(), |el| {
            el.child(
                div()
                    .flex_none()
                    .ml_2()
                    .px_1p5()
                    .py_0p5()
                    .rounded_sm()
                    .bg(colors.muted.opacity(0.5))
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(shortcut),
            )
        })
}

pub fn palette_item(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    icon: impl Into<Icon>,
) -> CommandItem {
    let title = title.into();
    let description = description.into();
    CommandItem::new()
        .label(title.clone())
        .icon(icon)
        .child(move |_, cx| palette_row(title.clone(), description.clone(), "".into(), None, cx))
}

pub fn palette_conversation_item(
    title: impl Into<SharedString>,
    context: impl Into<SharedString>,
    excerpt: impl Into<SharedString>,
) -> CommandItem {
    let title = title.into();
    let context = context.into();
    let excerpt = excerpt.into();
    CommandItem::new()
        .label(title.clone())
        .icon(IconName::SquareTerminal)
        .child(move |_, cx| {
            palette_row(
                title.clone(),
                context.clone(),
                "".into(),
                Some(excerpt.clone()),
                cx,
            )
        })
}

pub fn workspace_palette_command(state: &Entity<CommandState>) -> Command {
    Command::new(state)
        .bordered(false)
        .placeholder("Search commands, settings, or sessions…")
        .max_h(rems(26.25))
}

pub fn workspace_palette_frame(
    content: impl IntoElement,
    on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let colors = cx.theme().colors;
    div()
        .id("command-palette-backdrop")
        .absolute()
        .inset_0()
        .bg(threadlane_ui_theme::overlay_scrim())
        .flex()
        .items_start()
        .justify_center()
        .pt_20()
        .px_4()
        .pb_4()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            on_dismiss(window, cx)
        })
        .child(
            div()
                .id("command-palette-modal")
                .debug_selector(|| "command-palette-modal".into())
                .role(Role::Dialog)
                .aria_label("Command palette")
                .w(rems(35.0))
                .max_w_full()
                .min_w_0()
                .rounded_lg()
                .border_1()
                .border_color(colors.border)
                .bg(colors.title_bar)
                .shadow_lg()
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(content),
        )
}

pub fn palette_scope(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .px_3()
        .pt_2()
        .pb_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}
pub fn palette_empty(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .px_3()
        .py_4()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}
pub fn palette_footer(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(cx.theme().border)
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// Session-only mode uses stable groups so metadata updates never retarget a row.
pub fn session_switcher_command(
    state: &Entity<CommandState>,
    recent: Vec<CommandItem>,
    other: Vec<CommandItem>,
    searching: bool,
    footer: &'static str,
) -> Command {
    use gpui_component::command::CommandGroup;
    workspace_palette_command(state)
        .placeholder("Search sessions by title, project, ID, or branch…")
        .header(|_, _, cx| palette_scope("Switch session", cx))
        .group(
            CommandGroup::new()
                .label(if searching {
                    "Search results"
                } else {
                    "Recently visited"
                })
                .items(recent),
        )
        .group(
            CommandGroup::new()
                .label(if searching {
                    "More results"
                } else {
                    "Other sessions"
                })
                .items(other),
        )
        .footer(move |_, _, cx| palette_footer(footer, cx))
}

pub fn palette_session_item(
    title: &str,
    project: &str,
    branch: Option<&str>,
    id: &str,
) -> CommandItem {
    let title = if title.is_empty() { id } else { title };
    let subtitle = branch.map_or_else(
        || project.to_owned(),
        |branch| format!("{project} · {branch}"),
    );
    palette_item(title.to_owned(), subtitle, IconName::SquareTerminal).keywords([
        project.to_owned(),
        id.to_owned(),
        branch.unwrap_or_default().to_owned(),
    ])
}

pub fn session_switcher_empty(
    searching: bool,
    clear: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    use gpui_component::button::{Button, ButtonVariants};
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(palette_empty(
            if searching {
                "No matching sessions"
            } else {
                "No other sessions to switch to"
            },
            cx,
        ))
        .when(searching, |view| {
            view.child(
                Button::new("clear-session-query")
                    .label("Clear search")
                    .ghost()
                    .on_click(move |_, window, cx| clear(window, cx)),
            )
        })
        .into_any_element()
}

/// Session switching is choice-only: Escape dismisses even with a query and
/// cannot bubble into the workspace's stop-generation action.
pub fn session_switcher_frame(
    content: impl IntoElement,
    on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let dismiss = std::rc::Rc::new(on_dismiss);
    let backdrop = dismiss.clone();
    workspace_palette_frame(
        div()
            .capture_action(move |_: &gpui_kit::base::actions::Cancel, window, cx| {
                dismiss(window, cx);
                cx.stop_propagation();
            })
            .child(content),
        move |window, cx| backdrop(window, cx),
        cx,
    )
}
