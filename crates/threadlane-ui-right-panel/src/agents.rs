use gpui::*;
use gpui_component::avatar::Avatar;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::{ScrollableElement, Scrollbar};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable};
use std::collections::HashSet;
use threadlane_ui_state::{
    AppState, ChatMessageInfo, MessageRole, SubagentActivityInfo, SubagentActivityStatus,
};
use threadlane_ui_state::{actions::AppAction, controller};

pub struct AgentsPanel {
    model: Entity<AppState>,
    selected_run_id: Option<String>,
    transcript_list: ListState,
    transcript_run_id: Option<String>,
    transcript_count: usize,
    collapsed_tool_details: HashSet<String>,
    _model_subscription: Subscription,
}

impl AgentsPanel {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&model, |_, _, cx| cx.notify());
        Self {
            model,
            selected_run_id: None,
            transcript_list: ListState::new(0, ListAlignment::Top, window.rem_size() * 8.0),
            transcript_run_id: None,
            transcript_count: 0,
            collapsed_tool_details: HashSet::new(),
            _model_subscription: subscription,
        }
    }

    fn run_id(item: &SubagentActivityInfo) -> String {
        format!("queued-{}-{}", item.batch_run_id, item.task_index)
    }

    fn rank(status: SubagentActivityStatus) -> u8 {
        match status {
            SubagentActivityStatus::Running => 0,
            SubagentActivityStatus::Queued => 1,
            SubagentActivityStatus::Failed => 2,
            SubagentActivityStatus::Cancelled => 3,
            SubagentActivityStatus::Completed => 4,
        }
    }

    fn group(status: SubagentActivityStatus) -> &'static str {
        match status {
            SubagentActivityStatus::Running | SubagentActivityStatus::Queued => "Working",
            SubagentActivityStatus::Failed | SubagentActivityStatus::Cancelled => "Needs attention",
            SubagentActivityStatus::Completed => "Finished",
        }
    }

    fn status(status: SubagentActivityStatus) -> &'static str {
        match status {
            SubagentActivityStatus::Queued => "Queued",
            SubagentActivityStatus::Running => "Working",
            SubagentActivityStatus::Completed => "Completed",
            SubagentActivityStatus::Failed => "Failed",
            SubagentActivityStatus::Cancelled => "Cancelled",
        }
    }

    fn latest_activity(item: &SubagentActivityInfo) -> Option<String> {
        item.messages.iter().rev().find_map(|message| {
            message
                .tool_activities
                .last()
                .map(|activity| {
                    if activity.display_summary.trim().is_empty() {
                        activity.title.clone()
                    } else {
                        activity.display_summary.clone()
                    }
                })
                .or_else(|| {
                    let text = message.content.trim();
                    (!text.is_empty()).then(|| {
                        let preview: String = text.chars().take(100).collect();
                        if text.chars().count() > 100 {
                            format!("{}…", preview.trim_end())
                        } else {
                            preview
                        }
                    })
                })
        })
    }

    fn render_main_agent(&self, cx: &App) -> Div {
        let state = self.model.read(cx);
        let theme = cx.theme().colors;
        let latest = state
            .messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::Assistant)
            .and_then(|message| {
                let text = message.content.trim();
                (!text.is_empty()).then(|| text.chars().take(120).collect::<String>())
            });
        div()
            .mx_3()
            .mt_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(IconName::Bot).small())
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Main agent"))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(if state.is_generating {
                                theme.primary
                            } else {
                                theme.muted_foreground
                            })
                            .child(if state.is_generating {
                                "Working"
                            } else {
                                "Ready"
                            }),
                    ),
            )
            .children(latest.map(|text| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(text)
            }))
    }

    fn render_message(
        &mut self,
        message: &ChatMessageInfo,
        row_index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let role = match message.role {
            MessageRole::User => "Instruction",
            MessageRole::Assistant => "Agent",
            MessageRole::System => "System",
            MessageRole::Error => "Error",
            MessageRole::ContextMarker => "Context",
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_lg()
            .bg(theme.muted.opacity(0.45))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if message.role == MessageRole::Error {
                        theme.danger
                    } else {
                        theme.muted_foreground
                    })
                    .child(role),
            )
            .children((!message.content.trim().is_empty()).then(|| {
                div()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(message.content.clone())
            }))
            .children(
                message
                    .reasoning_content
                    .as_ref()
                    .filter(|text| !text.trim().is_empty())
                    .map(|text| {
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if message.streaming {
                                "Thinking…"
                            } else {
                                "Thought process"
                            })
                            .child(div().whitespace_normal().child(text.clone()))
                    }),
            )
            .children(message.tool_activities.iter().enumerate().map(
                |(activity_index, activity)| {
                    let key = format!(
                        "{}:{row_index}:{activity_index}",
                        self.transcript_run_id.as_deref().unwrap_or("")
                    );
                    let expanded = self.collapsed_tool_details.contains(&key);
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .text_xs()
                        .child(
                            div()
                                .flex_none()
                                .text_color(if activity.category == "Error" {
                                    theme.danger
                                } else {
                                    theme.muted_foreground
                                })
                                .child(if activity.category == "Error" {
                                    "!"
                                } else {
                                    "•"
                                }),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            activity
                                                .display_summary
                                                .trim()
                                                .is_empty()
                                                .then(|| activity.title.clone())
                                                .unwrap_or_else(|| {
                                                    activity.display_summary.clone()
                                                }),
                                        )
                                        .children((!activity.detail.trim().is_empty()).then(
                                            || {
                                                let toggle_key = key.clone();
                                                Button::new(SharedString::from(format!(
                                                    "agent-tool-detail-{row_index}-{activity_index}"
                                                )))
                                                .label(if expanded {
                                                    "Hide details"
                                                } else {
                                                    "Show details"
                                                })
                                                .ghost()
                                                .xsmall()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    if !this
                                                        .collapsed_tool_details
                                                        .insert(toggle_key.clone())
                                                    {
                                                        this.collapsed_tool_details
                                                            .remove(&toggle_key);
                                                    }
                                                    cx.notify();
                                                }))
                                            },
                                        )),
                                )
                                .children((expanded && !activity.detail.trim().is_empty()).then(
                                    || {
                                        div()
                                            .mt_1()
                                            .p_2()
                                            .rounded_md()
                                            .max_h(rems(12.0))
                                            .overflow_y_scrollbar()
                                            .whitespace_normal()
                                            .text_color(if activity.category == "Error" {
                                                theme.danger
                                            } else {
                                                theme.muted_foreground
                                            })
                                            .child(activity.detail.clone())
                                    },
                                )),
                        )
                },
            ))
            .into_any_element()
    }

    fn render_detail(&mut self, item: &SubagentActivityInfo, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().colors;
        let target = item.lane.as_deref().unwrap_or(&item.agent).to_owned();
        let live = matches!(
            item.status,
            SubagentActivityStatus::Queued | SubagentActivityStatus::Running
        );
        let prompt = if live {
            format!("Send this message to subagent {target}: ")
        } else {
            format!("Continue subagent {target} with this follow-up: ")
        };
        let label = if live { "Message…" } else { "Continue…" };
        let model = self.model.clone();
        let branch_controls = self.render_branch_controls(item, cx);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .p_3()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(item.agent.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if item.status == SubagentActivityStatus::Failed {
                                        theme.danger
                                    } else {
                                        theme.muted_foreground
                                    })
                                    .child(Self::status(item.status)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(item.task.clone()),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "agents-panel-message-{}",
                            Self::run_id(item)
                        )))
                        .label(label)
                        .outline()
                        .xsmall()
                        .on_click(move |_, _, cx| {
                            model.update(cx, |state, cx| {
                                state.request_composer_prompt(prompt.clone());
                                cx.notify();
                            });
                        }),
                    ),
            )
            .children(branch_controls)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .children(item.messages.is_empty().then(|| {
                        div()
                            .p_4()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("No recorded activity for this agent yet. The prompt is shown above.")
                    }))
                    .child(
                        list(
                            self.transcript_list.clone(),
                            cx.processor(Self::render_transcript_row),
                        )
                        .size_full()
                        .with_sizing_behavior(ListSizingBehavior::Auto),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .child(Scrollbar::vertical(&self.transcript_list)),
                    ),
            )
    }

    fn render_transcript_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let message = self.transcript_run_id.as_ref().and_then(|id| {
            if id == "main" {
                return self
                    .model
                    .read(cx)
                    .messages
                    .get(index.checked_sub(1)?)
                    .cloned();
            }
            self.model
                .read(cx)
                .active_subagents()
                .iter()
                .find(|item| Self::run_id(item) == *id)
                .and_then(|item| item.messages.get(index.checked_sub(1)?))
                .cloned()
        });
        if index == 0 {
            let error = self.transcript_run_id.as_ref().and_then(|id| {
                self.model
                    .read(cx)
                    .active_subagents()
                    .iter()
                    .find(|item| Self::run_id(item) == *id)
                    .and_then(|item| item.error.clone())
            });
            return div()
                .p_3()
                .children(error.map(|error| {
                    div()
                        .p_2()
                        .rounded_lg()
                        .text_color(cx.theme().colors.danger)
                        .child(error)
                }))
                .into_any_element();
        }
        div()
            .p_3()
            .children(
                message
                    .as_ref()
                    .map(|message| self.render_message(message, index, cx)),
            )
            .into_any_element()
    }

    fn render_branch_controls(
        &self,
        item: &SubagentActivityInfo,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let isolation = item.isolation.as_ref()?;
        let root = self.model.read(cx).active_git_work_dir()?;
        let theme = cx.theme().colors;
        let branch = isolation.branch.clone();
        let worktree = isolation.workspace.clone();
        let worktree_available = worktree.is_dir();

        let inspect_model = self.model.clone();
        let inspect_root = root.clone();
        let inspect_branch = branch.clone();
        let terminal_model = self.model.clone();
        let terminal_worktree = worktree.clone();
        let apply_model = self.model.clone();
        let apply_root = root.clone();
        let apply_branch = branch.clone();
        let apply_worktree = worktree.clone();
        let discard_model = self.model.clone();
        let discard_root = root;
        let discard_branch = branch.clone();
        let discard_worktree = worktree.clone();

        Some(
            div()
                .mx_3()
                .mb_2()
                .p_2()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(Icon::default().path("icons/git/branch.svg").xsmall())
                        .child(branch.clone()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .flex_wrap()
                        .child(
                            Button::new(SharedString::from(format!("agent-inspect-{branch}")))
                                .label("Inspect diff")
                                .outline()
                                .xsmall()
                                .on_click(move |_, _, cx| {
                                    let root = inspect_root.clone();
                                    let branch = inspect_branch.clone();
                                    let task = cx.background_executor().spawn(async move {
                                        threadlane_git::diff_branch(&root, &branch)
                                            .map(|diff| (root, branch, diff))
                                            .map_err(|error| error.to_string())
                                    });
                                    let model = inspect_model.clone();
                                    cx.spawn(async move |cx| {
                                        let result = task.await;
                                        let _ = model.update(cx, |state, cx| {
                                            match result {
                                                Ok((root, branch, diff)) => state.request_open_diff(
                                                    root,
                                                    format!("{branch}.diff"),
                                                    if diff.is_empty() {
                                                        "No committed changes on this branch."
                                                            .into()
                                                    } else {
                                                        diff
                                                    },
                                                ),
                                                Err(error) => state.session_status = Some(error),
                                            }
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("agent-terminal-{branch}")))
                                .label("Terminal")
                                .ghost()
                                .xsmall()
                                .disabled(!worktree_available)
                                .tooltip(if worktree_available {
                                    format!("Open terminal in {}", worktree.display())
                                } else {
                                    "This worktree was cleaned up; the branch is still available."
                                        .into()
                                })
                                .on_click(move |_, _, cx| {
                                    terminal_model.update(cx, |state, cx| {
                                        controller::dispatch(
                                            state,
                                            AppAction::OpenTerminalAt(terminal_worktree.clone()),
                                        );
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("agent-apply-{branch}")))
                                .label("Apply")
                                .xsmall()
                                .disabled(item.status != SubagentActivityStatus::Completed)
                                .on_click(move |_, _, cx| {
                                    let root = apply_root.clone();
                                    let branch = apply_branch.clone();
                                    let worktree = apply_worktree.clone();
                                    let task = cx.background_executor().spawn(async move {
                                        let parent = threadlane_git::inspect(&root)
                                            .map_err(|error| error.to_string())?;
                                        if parent.has_changes {
                                            return Err("Commit or stash parent changes before applying a subagent branch.".into());
                                        }
                                        if worktree.is_dir()
                                            && threadlane_git::inspect(&worktree)
                                                .map_err(|error| error.to_string())?
                                                .has_changes
                                        {
                                            return Err("The subagent worktree has uncommitted changes; commit them before applying.".into());
                                        }
                                        threadlane_git::merge(&root, &branch)
                                            .map_err(|error| error.to_string())?;
                                        if worktree.is_dir() {
                                            threadlane_git::remove_worktree(&root, &worktree, false)
                                                .map_err(|error| error.to_string())?;
                                        }
                                        threadlane_git::delete_branch(&root, &branch, false)
                                            .map_err(|error| error.to_string())?;
                                        Ok(format!("Applied {branch}"))
                                    });
                                    let model = apply_model.clone();
                                    cx.spawn(async move |cx| {
                                        let result = task.await;
                                        let _ = model.update(cx, |state, cx| {
                                            state.session_status =
                                                Some(result.unwrap_or_else(|error| error));
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("agent-discard-{branch}")))
                                .label("Discard…")
                                .ghost()
                                .xsmall()
                                .disabled(matches!(
                                    item.status,
                                    SubagentActivityStatus::Queued | SubagentActivityStatus::Running
                                ))
                                .on_click(move |_, _, cx| {
                                    let root = discard_root.clone();
                                    let branch = discard_branch.clone();
                                    let worktree = discard_worktree.clone();
                                    let model = discard_model.clone();
                                    cx.spawn(async move |cx| {
                                        let confirmed = rfd::AsyncMessageDialog::new()
                                            .set_title("Discard subagent branch?")
                                            .set_description(format!(
                                                "Delete {branch} and its worktree? This cannot be undone."
                                            ))
                                            .set_buttons(rfd::MessageButtons::YesNo)
                                            .show()
                                            .await;
                                        if !matches!(confirmed, rfd::MessageDialogResult::Yes) {
                                            return;
                                        }
                                        let task = cx.background_executor().spawn(async move {
                                            if worktree.is_dir() {
                                                threadlane_git::remove_worktree(&root, &worktree, true)
                                                    .map_err(|error| error.to_string())?;
                                            }
                                            threadlane_git::delete_branch(&root, &branch, true)
                                                .map_err(|error| error.to_string())?;
                                            let _ = threadlane_git::prune_worktrees(&root);
                                            Ok::<_, String>(format!("Discarded {branch}"))
                                        });
                                        let result = task.await;
                                        let _ = model.update(cx, |state, cx| {
                                            state.session_status =
                                                Some(result.unwrap_or_else(|error| error));
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                }),
                        ),
                ),
        )
    }
}

impl Render for AgentsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let state = self.model.read(cx);
        let main_count = state.messages.len();
        let main_working = state.is_generating;
        let mut subagents: Vec<_> = state
            .active_subagents()
            .iter()
            .map(|item| {
                let metadata = SubagentActivityInfo {
                    batch_run_id: item.batch_run_id,
                    task_index: item.task_index,
                    journal_run_id: item.journal_run_id.clone(),
                    lane: item.lane.clone(),
                    agent: item.agent.clone(),
                    task: item.task.clone(),
                    model: item.model.clone(),
                    status: item.status,
                    messages: item.messages.clone(),
                    isolation: item.isolation.clone(),
                    error: item.error.clone(),
                };
                (metadata, item.messages.len())
            })
            .collect();
        subagents.sort_by_key(|item| Self::rank(item.0.status));
        let selected_id = self
            .selected_run_id
            .clone()
            .filter(|id| {
                id == "main" || subagents.iter().any(|(item, _)| Self::run_id(item) == *id)
            })
            .unwrap_or_else(|| {
                subagents
                    .first()
                    .map(|(item, _)| Self::run_id(item))
                    .unwrap_or_else(|| "main".to_string())
            });
        let selected = subagents
            .iter()
            .find(|(item, _)| Self::run_id(item) == selected_id)
            .map(|(item, count)| (item.clone(), *count));
        let count = if selected_id == "main" {
            main_count + 1
        } else {
            selected.as_ref().map_or(0, |(_, count)| count + 1)
        };
        let selected_id = Some(selected_id);
        if self.transcript_run_id != selected_id {
            self.transcript_list.reset(count);
            self.transcript_run_id = selected_id.clone();
        } else if count > self.transcript_count {
            self.transcript_list.splice(
                self.transcript_count..self.transcript_count,
                count - self.transcript_count,
            );
        } else if count < self.transcript_count {
            self.transcript_list.reset(count);
        } else {
            self.transcript_list.remeasure();
        }
        self.transcript_count = count;
        self.selected_run_id = selected_id.clone();

        let main_selected = selected_id.as_deref() == Some("main");
        let tabs = div()
            .flex()
            .flex_none()
            .gap_1()
            .px_2()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .overflow_x_scrollbar()
            .child(
                Button::new("agents-profile-main")
                    .ghost()
                    .selected(main_selected)
                    .tooltip(if main_working {
                        "Main agent · Working"
                    } else {
                        "Main agent · Ready"
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .child(Avatar::new().name("Main").small())
                            .child(div().text_xs().child("Main")),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected_run_id = Some("main".to_string());
                        cx.notify();
                    })),
            )
            .children(subagents.iter().map(|(item, _)| {
                let id = Self::run_id(item);
                let select_id = id.clone();
                let selected = selected_id.as_deref() == Some(id.as_str());
                let duplicate_count = subagents
                    .iter()
                    .filter(|(other, _)| other.agent == item.agent)
                    .count();
                let name = if duplicate_count > 1 {
                    format!("{} {}", item.agent, item.task_index + 1)
                } else {
                    item.agent.clone()
                };
                Button::new(SharedString::from(format!("agents-profile-{id}")))
                    .ghost()
                    .selected(selected)
                    .tooltip(format!(
                        "{} · {}\n{}",
                        name,
                        Self::status(item.status),
                        item.task
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .child(Avatar::new().name(name.clone()).small())
                            .child(div().text_xs().max_w(rems(5.0)).truncate().child(name)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_run_id = Some(select_id.clone());
                        cx.notify();
                    }))
            }));
        let profile = if let Some((item, _)) = selected {
            self.render_detail(&item, cx).into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(self.render_main_agent(cx))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .children((main_count == 0).then(|| {
                            div()
                                .p_4()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("No main-agent activity recorded yet. Select an agent above to inspect its work.")
                        }))
                        .children((main_count > 0).then(|| {
                            list(
                                self.transcript_list.clone(),
                                cx.processor(Self::render_transcript_row),
                            )
                            .size_full()
                            .with_sizing_behavior(ListSizingBehavior::Auto)
                        }))
                        .children((main_count > 0).then(|| {
                            div()
                                .absolute()
                                .inset_0()
                                .child(Scrollbar::vertical(&self.transcript_list))
                        })),
                )
                .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(tabs)
            .child(div().flex_1().min_h_0().child(profile))
    }
}

#[cfg(test)]
mod tests {
    use super::AgentsPanel;
    use threadlane_ui_state::SubagentActivityStatus;

    #[test]
    fn queued_selection_survives_agent_start() {
        let mut item = threadlane_ui_state::SubagentActivityInfo {
            batch_run_id: 12,
            task_index: 2,
            journal_run_id: None,
            lane: None,
            agent: "worker".into(),
            task: "task".into(),
            model: None,
            status: SubagentActivityStatus::Queued,
            messages: Vec::new(),
            isolation: None,
            error: None,
        };
        let selected = AgentsPanel::run_id(&item);
        item.journal_run_id = Some("journal-123".into());
        item.status = SubagentActivityStatus::Running;
        assert_eq!(AgentsPanel::run_id(&item), selected);
    }

    #[test]
    fn attention_and_live_agents_sort_before_finished_agents() {
        let mut statuses = [
            SubagentActivityStatus::Completed,
            SubagentActivityStatus::Failed,
            SubagentActivityStatus::Queued,
            SubagentActivityStatus::Running,
        ];
        statuses.sort_by_key(|status| AgentsPanel::rank(*status));
        assert_eq!(
            statuses,
            [
                SubagentActivityStatus::Running,
                SubagentActivityStatus::Queued,
                SubagentActivityStatus::Failed,
                SubagentActivityStatus::Completed,
            ]
        );
        assert_eq!(
            AgentsPanel::group(SubagentActivityStatus::Failed),
            "Needs attention"
        );
    }
}
