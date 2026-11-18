//! Controlled branch management and Git forms. Hosts own validation and Git operations.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::dialog::{AlertDialog, Dialog, DialogButtonProps};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Sizable, ThemeStyled};
use gpui_kit::base::{Radio, RadioGroup};
use std::rc::Rc;
use threadlane_protocol::repo::{GitBranchInfo, GitStatus};

pub const REVIEW_BRANCH_FILTER_PLACEHOLDER: &str = "Filter branches…";
pub const REVIEW_BRANCH_NAME_PLACEHOLDER: &str = "e.g. feature/new-workflow";
pub const REVIEW_MERGE_FILTER_PLACEHOLDER: &str = "Filter branches to merge…";
pub const REVIEW_STASH_MESSAGE_PLACEHOLDER: &str = "Stash message (optional)";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewBranchAction {
    New,
    Merge,
    Close,
    Select(String),
    Copy(String),
    Delete(String),
}

/// Recorded branch inventory, including legacy snapshots without detailed metadata.
pub fn review_branches(status: Option<&GitStatus>, query: &str) -> Vec<GitBranchInfo> {
    let Some(status) = status else {
        return Vec::new();
    };
    let query = query.trim().to_lowercase();
    let branches = if status.branch_details.is_empty() {
        status
            .branches
            .iter()
            .map(|name| GitBranchInfo {
                name: name.clone(),
                is_current: status.branch.as_ref() == Some(name),
                is_default: status.default_branch.as_deref().unwrap_or("main") == name,
                is_remote: name.starts_with("origin/"),
                ..Default::default()
            })
            .collect()
    } else {
        status.branch_details.clone()
    };
    branches
        .into_iter()
        .filter(|branch| {
            branch.name != "origin"
                && !(branch.is_remote && branch.name.ends_with("/HEAD"))
                && (query.is_empty() || branch.name.to_lowercase().contains(&query))
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewGitDialog {
    NewBranch,
    Merge,
    Switch,
    Stash,
}
impl ReviewGitDialog {
    pub fn title(self, target: Option<&str>) -> String {
        match self {
            Self::NewBranch => "Create a branch".into(),
            Self::Merge => "Merge branches".into(),
            Self::Stash => "Stash changes".into(),
            Self::Switch => format!("Switch to {}", target.unwrap_or("main")),
        }
    }
    pub fn width_rem(self) -> f32 {
        match self {
            Self::NewBranch | Self::Stash => 26.25,
            _ => 28.75,
        }
    }
}
/// Shared shell; hosts provide the live content and dismissal hooks.
pub fn review_git_dialog(
    dialog: Dialog,
    kind: ReviewGitDialog,
    target: Option<&str>,
    window: &Window,
) -> Dialog {
    dialog
        .w(window.rem_size() * kind.width_rem())
        .max_w_full()
        .title(kind.title(target))
}

pub fn review_delete_branch_alert(alert: AlertDialog, branch: &str, workdir: &str) -> AlertDialog {
    alert.title(format!("Delete branch “{branch}”?"))
        .description(format!("Delete the local branch in {workdir}. Remote branches and worktrees will not be removed. Unmerged branches and branches checked out in a worktree cannot be deleted."))
        .button_props(DialogButtonProps::default().ok_text("Delete").ok_variant(ButtonVariant::Danger).show_cancel(true))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewGitFormAction {
    Close,
    Create,
    SelectMerge(String),
    Merge,
    SwitchStash(bool),
    Switch,
    IncludeUntracked(bool),
    Stash,
}
/// A render snapshot; text-input entities and selection remain owned by the host.
pub struct ReviewGitFormState<'a> {
    pub current_branch: &'a str,
    pub target_branch: Option<&'a str>,
    pub selected_merge: Option<&'a str>,
    pub busy: bool,
    pub stash_changes: bool,
    pub include_untracked: bool,
}

pub fn review_branch_manager(
    filter_input: &Entity<InputState>,
    status: Option<&GitStatus>,
    busy: bool,
    has_project: bool,
    on_action: impl Fn(&ReviewBranchAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let on_action = Rc::new(on_action);
    let theme = cx.theme().colors;
    let filter_text = filter_input.read(cx).value().trim().to_lowercase();
    let current_branch = status.and_then(|s| s.branch.as_deref()).unwrap_or("main");

    let filtered_branches = review_branches(status, &filter_text);

    let default_branches: Vec<GitBranchInfo> = filtered_branches
        .iter()
        .filter(|b| b.is_default && !b.is_remote)
        .cloned()
        .collect();

    let recent_branches: Vec<GitBranchInfo> = filtered_branches
        .iter()
        .filter(|b| !b.is_default && !b.is_remote)
        .cloned()
        .collect();

    let other_branches: Vec<GitBranchInfo> = filtered_branches
        .iter()
        .filter(|b| b.is_remote)
        .cloned()
        .collect();

    let current_branch_str = current_branch.to_string();
    let empty =
        default_branches.is_empty() && recent_branches.is_empty() && other_branches.is_empty();

    div()
            .id("git-branch-manager").debug_selector(|| "review-branch-manager".into()).min_w_0()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(theme.title_bar)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1().min_w_0()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .px_2()
                            .h_8()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .child(
                                div()
                                    .size(rems(0.875))
                                    .text_color(theme.muted_foreground)
                                    .child(IconName::Search),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(filter_input).aria_label("Filter branches")
                                        .appearance(false)
                                        .bordered(false),
                                ),
                            ),
                    )
                    .child(
                        Button::new("open-new-branch-modal-btn").debug_selector(|| "review-branch-new".into()).disabled(busy)
                            .icon(IconName::Plus)
                            .label("New branch…")
                            .outline()
                            .small()
                            .tooltip("Create a new branch…")
                            .on_click({ let request = on_action.clone(); move |_, window, cx| request(&ReviewBranchAction::New, window, cx) }),
                    )
                    .child(
                        Button::new("close-branch-manager-btn").debug_selector(|| "review-branch-close".into())
                                    .accessibility_label("Back to review")
                            .icon(IconName::Close)
                            .ghost()
                            .small()
                            .tooltip("Back to review")
                            .on_click({ let request = on_action.clone(); move |_, window, cx| request(&ReviewBranchAction::Close, window, cx) }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        Button::new("quick-merge-banner").debug_selector(|| "review-branch-merge".into()).disabled(busy)
                            .accessibility_label("Merge a branch…")
                            .ghost().h_auto().w_full().p_0()
                            .on_click({ let request = on_action.clone(); move |_, window, cx| request(&ReviewBranchAction::Merge, window, cx) })
                            .child(div().w_full().whitespace_normal()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_2p5()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.muted.opacity(0.35))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .size_4()
                                            .text_color(theme.primary)
                                            .child(Icon::default().path("icons/git/branch.svg")),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child(format!("Choose a branch to merge into {current_branch_str}…")),
                                    ),
                            )
                            .child(
                                div()
                                    .size(rems(0.875))
                                    .text_color(theme.muted_foreground)
                                    .child(IconName::ChevronRight),
                            )),
                    )
                    .when(!default_branches.is_empty(), |el| {
                        el.child(review_branch_section("DEFAULT BRANCH", default_branches, busy, has_project, { let request = on_action.clone(); move |action, window, cx| request(action, window, cx) }, cx))
                    })
                    .when(!recent_branches.is_empty(), |el| {
                        el.child(review_branch_section("RECENT BRANCHES", recent_branches, busy, has_project, { let request = on_action.clone(); move |action, window, cx| request(action, window, cx) }, cx))
                    })
                    .when(empty, |el| {
                        el.child(div().id("review-branch-empty").debug_selector(|| "review-branch-empty".into())
                            .text_xs().text_color(theme.muted_foreground).p_2().child("No matching branches"))
                    })
                    .when(!other_branches.is_empty(), |el| {
                        el.child(review_branch_section("OTHER BRANCHES", other_branches, busy, has_project, { let request = on_action.clone(); move |action, window, cx| request(action, window, cx) }, cx))
                    }),
            )
}

pub fn review_branch_section(
    title: &'static str,
    branches: Vec<GitBranchInfo>,
    busy: bool,
    has_project: bool,
    on_action: impl Fn(&ReviewBranchAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let on_action = Rc::new(on_action);
    let theme = cx.theme().colors;
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(theme.muted_foreground)
                .px_1()
                .pb_0p5()
                .child(title),
        )
        .children(branches.into_iter().map(|branch| {
            let name = branch.name.clone();
            let is_current = branch.is_current;
            let rel_time = branch.relative_time.clone();
            let branch_name_for_click = name.clone();
            let menu_name = name.clone();
            let request = on_action.clone();
            let menu_request = on_action.clone();
            let can_delete =
                has_project && !busy && !is_current && !branch.is_default && !branch.is_remote;
            Button::new(SharedString::from(format!("branch-row-{}", name)))
                .debug_selector({
                    let name = name.clone();
                    move || format!("branch-row-{name}")
                })
                .accessibility_label(if is_current {
                    format!("Current branch {name}, already checked out")
                } else {
                    format!("Switch to branch {name}")
                })
                .ghost()
                .h_auto()
                .w_full()
                .p_0()
                .disabled(busy)
                .on_click(move |_, window, cx| {
                    if !is_current {
                        request(
                            &ReviewBranchAction::Select(branch_name_for_click.clone()),
                            window,
                            cx,
                        );
                    }
                })
                .child(
                    div()
                        .w_full()
                        .whitespace_normal()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .px_2p5()
                        .py_2()
                        .rounded_md()
                        .bg(if is_current {
                            theme.muted.opacity(0.7)
                        } else {
                            gpui::transparent_black()
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .min_w_0()
                                .flex_1()
                                .child(
                                    div()
                                        .size_4()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(if is_current {
                                            theme.primary
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .child(if is_current {
                                            Icon::new(IconName::Check)
                                        } else {
                                            Icon::default().path("icons/git/branch.svg")
                                        }),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .font_weight(if is_current {
                                            FontWeight::BOLD
                                        } else {
                                            FontWeight::MEDIUM
                                        })
                                        .text_color(if is_current {
                                            theme.foreground
                                        } else {
                                            theme.foreground.opacity(0.9)
                                        })
                                        .child(name),
                                ),
                        )
                        .children((!rel_time.is_empty()).then(|| {
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(rel_time)
                        })),
                )
                .context_menu(move |menu, window, cx| {
                    // Transfer focus before prepaint, matching file and terminal menus.
                    menu.focus_handle(cx).focus(window, cx);
                    let copy_name = menu_name.clone();
                    let delete_name = menu_name.clone();
                    let copy_request = menu_request.clone();
                    let delete_request = menu_request.clone();
                    menu.item(PopupMenuItem::new("Copy branch name").on_click(
                        move |_, window, cx| {
                            copy_request(&ReviewBranchAction::Copy(copy_name.clone()), window, cx);
                        },
                    ))
                    .separator()
                    .item(
                        PopupMenuItem::new("Delete branch…")
                            .disabled(!can_delete)
                            .on_click(move |_, window, cx| {
                                delete_request(
                                    &ReviewBranchAction::Delete(delete_name.clone()),
                                    window,
                                    cx,
                                );
                            }),
                    )
                })
        }))
}

pub fn review_stash_form(
    message: &Entity<InputState>,
    state: &ReviewGitFormState<'_>,
    on_action: impl Fn(&ReviewGitFormAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let on_action = Rc::new(on_action);
    let theme = cx.theme().colors;
    let include_untracked = state.include_untracked;

    div()
        .id("stash-dialog")
        .debug_selector(|| "stash-dialog".into())
        .min_w_0()
        .w_full()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child("Stash message (optional)"),
                )
                .child(
                    div()
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .bg(theme.input)
                        .border_1()
                        .border_color(theme.border)
                        .child(Input::new(message).aria_label("Stash message")),
                ),
        )
        .child(
            div().flex().items_center().gap_2().child(
                Checkbox::new("stash-include-untracked-chk")
                    .debug_selector(|| "stash-include-untracked-chk".into())
                    .disabled(state.busy)
                    .label("Include untracked files")
                    .checked(include_untracked)
                    .on_click({
                        let request = on_action.clone();
                        move |checked: &bool, window, cx| {
                            request(&ReviewGitFormAction::IncludeUntracked(*checked), window, cx)
                        }
                    }),
            ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .pt_2()
                .child(
                    Button::new("cancel-stash-btn")
                        .debug_selector(|| "cancel-stash-btn".into())
                        .label("Cancel")
                        .ghost()
                        .small()
                        .on_click({
                            let request = on_action.clone();
                            move |_, window, cx| request(&ReviewGitFormAction::Close, window, cx)
                        }),
                )
                .child(
                    Button::new("confirm-stash-btn")
                        .min_w_0()
                        .max_w(relative(0.75))
                        .debug_selector(|| "confirm-stash-btn".into())
                        .label("Stash changes")
                        .primary()
                        .small()
                        .disabled(state.busy)
                        .on_click({
                            let request = on_action.clone();
                            move |_, window, cx| request(&ReviewGitFormAction::Stash, window, cx)
                        }),
                ),
        )
}

pub fn review_new_branch_form(
    input: &Entity<InputState>,
    state: &ReviewGitFormState<'_>,
    on_action: impl Fn(&ReviewGitFormAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let on_action = Rc::new(on_action);
    let theme = cx.theme().colors;
    let current_branch = state.current_branch.to_string();
    let name = input.read(cx).value().trim().to_string();
    let can_create = !name.is_empty() && !state.busy;

    div()
        .id("new-branch-dialog")
        .debug_selector(|| "new-branch-dialog".into())
        .min_w_0()
        .w_full()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("Based on")
                .child(
                    Tag::new()
                        .min_w_0()
                        .max_w(relative(0.8))
                        .child(
                            div()
                                .id("review-new-base")
                                .debug_selector(|| "review-new-base".into())
                                .min_w_0()
                                .truncate()
                                .child(current_branch),
                        )
                        .with_variant(TagVariant::Secondary)
                        .small(),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child("Branch name"),
                )
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.background)
                        .child(
                            Input::new(input)
                                .aria_label("New branch name")
                                .bordered(false),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .pt_2()
                .child(
                    Button::new("cancel-new-branch-btn")
                        .debug_selector(|| "cancel-new-branch-btn".into())
                        .label("Cancel")
                        .ghost()
                        .small()
                        // Synara busy guard: block dismiss while Git is running.
                        .disabled(state.busy)
                        .on_click({
                            let request = on_action.clone();
                            move |_, window, cx| request(&ReviewGitFormAction::Close, window, cx)
                        }),
                )
                .child(
                    Button::new("submit-new-branch-btn")
                        .min_w_0()
                        .max_w(relative(0.75))
                        .debug_selector(|| "submit-new-branch-btn".into())
                        .label(if name.is_empty() {
                            "Create branch".to_string()
                        } else {
                            format!("Create {name}")
                        })
                        .accessibility_label(if name.is_empty() {
                            "Create branch".to_string()
                        } else {
                            format!("Create branch {name}")
                        })
                        .tooltip(if name.is_empty() {
                            "Enter a branch name".to_string()
                        } else {
                            format!("Create branch {name}")
                        })
                        .primary()
                        .small()
                        .disabled(!can_create)
                        .on_click({
                            let request = on_action.clone();
                            move |_, window, cx| request(&ReviewGitFormAction::Create, window, cx)
                        }),
                ),
        )
}

pub fn review_merge_form(
    filter_input: &Entity<InputState>,
    status: Option<&GitStatus>,
    state: &ReviewGitFormState<'_>,
    on_action: impl Fn(&ReviewGitFormAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let on_action = Rc::new(on_action);
    let theme = cx.theme().colors;
    let current_branch = state.current_branch.to_string();
    let filter = filter_input.read(cx).value().trim().to_lowercase();

    let branches: Vec<_> = review_branches(status, &filter)
        .into_iter()
        .filter(|branch| branch.name != current_branch)
        .collect();
    let selected = state.selected_merge.map(str::to_owned);
    let can_merge = selected.is_some() && !state.busy;

    div()
        .id("merge-branch-dialog")
        .debug_selector(|| "merge-branch-dialog".into())
        .min_w_0()
        .w_full()
        .max_h(rems(32.5))
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("Select a branch to merge into your current working tree:"),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .px_2()
                .h_8()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .child(
                    div()
                        .size(rems(0.875))
                        .text_color(theme.muted_foreground)
                        .child(IconName::Search),
                )
                .child(
                    div().flex_1().child(
                        Input::new(filter_input)
                            .aria_label("Filter branches to merge")
                            .appearance(false)
                            .bordered(false),
                    ),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .max_h(rems(15.0))
                .overflow_y_scrollbar()
                .gap_1()
                .when(branches.is_empty(), |this| {
                    this.child(
                        div()
                            .id("review-merge-empty")
                            .debug_selector(|| "review-merge-empty".into())
                            .p_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("No matching branches"),
                    )
                })
                .children(branches.into_iter().map(|b| {
                    let name = b.name.clone();
                    let is_selected = selected.as_deref() == Some(&name);
                    let name_for_click = name.clone();
                    Button::new(SharedString::from(format!("merge-select-{}", name)))
                        .debug_selector({
                            let name = name.clone();
                            move || format!("merge-select-{name}")
                        })
                        .accessibility_label(format!("Select branch {name} to merge"))
                        .toggled(is_selected)
                        .disabled(state.busy)
                        .ghost()
                        .h_auto()
                        .w_full()
                        .p_0()
                        .on_click({
                            let request = on_action.clone();
                            move |_, window, cx| {
                                request(
                                    &ReviewGitFormAction::SelectMerge(name_for_click.clone()),
                                    window,
                                    cx,
                                )
                            }
                        })
                        .child(
                            div()
                                .w_full()
                                .whitespace_normal()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .px_2p5()
                                .py_2()
                                .rounded_md()
                                .border_1()
                                .border_color(if is_selected {
                                    theme.primary
                                } else {
                                    gpui::transparent_black()
                                })
                                .bg(if is_selected {
                                    theme.muted.opacity(0.8)
                                } else {
                                    gpui::transparent_black()
                                })
                                .child(
                                    div()
                                        .flex()
                                        .min_w_0()
                                        .flex_1()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .size_4()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .text_color(if is_selected {
                                                    theme.primary
                                                } else {
                                                    theme.muted_foreground
                                                })
                                                .child(
                                                    Icon::default().path("icons/git/branch.svg"),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .flex_1()
                                                .truncate()
                                                .text_xs()
                                                .font_weight(if is_selected {
                                                    FontWeight::BOLD
                                                } else {
                                                    FontWeight::NORMAL
                                                })
                                                .text_color(theme.foreground)
                                                .child(name),
                                        ),
                                )
                                .children((!b.relative_time.is_empty()).then(|| {
                                    div()
                                        .flex_shrink_0()
                                        .max_w(relative(0.35))
                                        .truncate()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(b.relative_time)
                                })),
                        )
                })),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .pt_2()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    Button::new("cancel-merge-btn")
                        .debug_selector(|| "cancel-merge-btn".into())
                        .label("Cancel")
                        .ghost()
                        .small()
                        .disabled(state.busy)
                        .on_click({
                            let request = on_action.clone();
                            move |_, window, cx| request(&ReviewGitFormAction::Close, window, cx)
                        }),
                )
                .child(
                    Button::new("submit-merge-btn")
                        .min_w_0()
                        .max_w(relative(0.75))
                        .debug_selector(|| "submit-merge-btn".into())
                        .label(if let Some(target) = &selected {
                            format!("Merge {target} into {current_branch}")
                        } else {
                            format!("Merge into {current_branch}")
                        })
                        .primary()
                        .small()
                        .disabled(!can_merge)
                        .on_click({
                            let request = on_action.clone();
                            move |_, window, cx| request(&ReviewGitFormAction::Merge, window, cx)
                        }),
                ),
        )
}

// The styled Kit radio gives its text column the entire row width beside its
// indicator. Use the same interaction primitive with a shrinking text column.
type GitFormRequest = Rc<dyn Fn(&ReviewGitFormAction, &mut Window, &mut App)>;

#[derive(IntoElement)]
struct ReviewSwitchOption {
    id: &'static str,
    description_id: &'static str,
    label: String,
    description: String,
    accessibility_label: String,
    checked: bool,
    disabled: bool,
    stash: bool,
    on_action: GitFormRequest,
}

impl RenderOnce for ReviewSwitchOption {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = window
            .use_keyed_state(self.id, cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let colors = cx.theme().colors;
        let selected = if self.checked {
            colors.primary
        } else {
            colors.input
        };
        Radio::new(self.id)
            .debug_selector(move || self.id.into())
            .checked(self.checked)
            .disabled(self.disabled)
            .track_focus(&focus)
            .set_position(if self.stash { 1 } else { 2 }, 2)
            .accessibility_label(self.accessibility_label)
            .text_sm()
            .flex()
            .items_start()
            .gap_2()
            .w_full()
            .min_w_0()
            .rounded(cx.theme().radius * 0.5)
            .when(focus.is_focused(window), |this| {
                this.focus_ring_style(window, cx)
            })
            .when(self.disabled, |this| this.opacity(0.5))
            .child(
                div()
                    .size_4()
                    .mt(rems(0.125))
                    .flex_shrink_0()
                    .rounded_full()
                    .border_1()
                    .border_color(selected)
                    .bg(if self.checked {
                        selected
                    } else {
                        colors.background
                    })
                    .when(self.checked, |this| {
                        this.child(
                            Icon::new(IconName::Check)
                                .size_4()
                                .text_color(colors.primary_foreground),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .line_height(relative(1.25))
                    .child(
                        div()
                            .id(format!("{}-label", self.id))
                            .debug_selector(move || format!("{}-label", self.id))
                            .whitespace_normal()
                            .child(self.label),
                    )
                    .child(
                        div()
                            .id(self.description_id)
                            .debug_selector(move || self.description_id.into())
                            .whitespace_normal()
                            .text_xs()
                            .text_color(colors.muted_foreground)
                            .child(self.description),
                    ),
            )
            .on_change(move |_, _, window, cx| {
                (self.on_action)(&ReviewGitFormAction::SwitchStash(self.stash), window, cx)
            })
    }
}

pub fn review_switch_form(
    state: &ReviewGitFormState<'_>,
    on_action: impl Fn(&ReviewGitFormAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let on_action = Rc::new(on_action);
    let theme = cx.theme().colors;
    let current_branch = state.current_branch.to_string();
    let target_branch = state.target_branch.unwrap_or("main").to_string();

    div().id("switch-branch-dialog").debug_selector(|| "switch-branch-dialog".into())
        .min_w_0().w_full().flex().flex_col().gap_3p5()
        .child(div().text_xs().text_color(theme.muted_foreground)
            .child(format!("You have uncommitted changes on {current_branch}. What would you like to do with them?")))
        .child(RadioGroup::new("switch-stash-mode").flex().flex_col().gap_3().w_full().min_w_0()
            .child(ReviewSwitchOption {
                id: "switch-opt-stash", description_id: "switch-stash-description",
                label: format!("Leave my changes on {current_branch} (Stash)"),
                accessibility_label: format!("Leave changes on {current_branch} using a stash"),
                description: "Your in-progress changes will be stashed and restored when you switch back.".into(),
                checked: state.stash_changes, disabled: state.busy, stash: true, on_action: on_action.clone(),
            })
            .child(ReviewSwitchOption {
                id: "switch-opt-carry", description_id: "switch-carry-description",
                label: format!("Bring my changes to {target_branch}"),
                accessibility_label: format!("Carry changes to branch {target_branch}"),
                description: format!("Your in-progress changes will be carried over to {target_branch}."),
                checked: !state.stash_changes, disabled: state.busy, stash: false, on_action: on_action.clone(),
            }))
        .child(div().flex().items_center().justify_end().gap_2().pt_2()
            .child(Button::new("cancel-switch-dialog-btn").debug_selector(|| "cancel-switch-dialog-btn".into())
                .label("Cancel").ghost().small().disabled(state.busy)
                .on_click({ let request = on_action.clone(); move |_, window, cx| request(&ReviewGitFormAction::Close, window, cx) }))
            .child(Button::new("submit-switch-dialog-btn").min_w_0().max_w(relative(0.75)).debug_selector(|| "submit-switch-dialog-btn".into())
                .label(format!("Switch to {target_branch}"))
                .accessibility_label(format!("Switch to branch {target_branch}"))
                .tooltip(format!("Check out {target_branch}")).primary().small().disabled(state.busy)
                .on_click(move |_, window, cx| on_action(&ReviewGitFormAction::Switch, window, cx))))
}

#[cfg(test)]
mod tests {
    use crate::review_branches::review_branches;
    use threadlane_protocol::repo::{GitBranchInfo, GitStatus};

    #[test]
    fn recorded_branch_inventory_preserves_metadata_and_reads_legacy_names() {
        let mut status = GitStatus {
            branch: Some("feature".into()),
            default_branch: Some("main".into()),
            branches: vec![
                "main".into(),
                "feature".into(),
                "origin/feature".into(),
                "origin".into(),
                "origin/HEAD".into(),
            ],
            ..Default::default()
        };
        let branches = review_branches(Some(&status), "");
        assert_eq!(branches.len(), 3);
        assert!(branches[0].is_default);
        assert!(branches[1].is_current);
        assert!(branches[2].is_remote);
        assert_eq!(review_branches(Some(&status), " FEATURE ").len(), 2);
        assert!(review_branches(Some(&status), "unmatched").is_empty());
        status.branch_details = vec![GitBranchInfo {
            name: "feature".into(),
            is_current: true,
            relative_time: "2 hours ago".into(),
            committer_date_unix: 42,
            upstream: Some("origin/feature".into()),
            ..Default::default()
        }];
        let branches = review_branches(Some(&status), "feature");
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].upstream.as_deref(), Some("origin/feature"));
        assert_eq!(branches[0].committer_date_unix, 42);
        assert_eq!(branches[0].relative_time, "2 hours ago");
    }

    #[test]
    fn recorded_branch_inventory_keeps_local_head_names_only() {
        let status = GitStatus {
            branch_details: vec![
                GitBranchInfo {
                    name: "nested/HEAD".into(),
                    ..Default::default()
                },
                GitBranchInfo {
                    name: "origin/HEAD".into(),
                    ..Default::default()
                },
                GitBranchInfo {
                    name: "origin/HEAD".into(),
                    is_remote: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let branches = review_branches(Some(&status), "HEAD");
        let names = branches
            .iter()
            .map(|branch| branch.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["nested/HEAD", "origin/HEAD"]);
    }
}
