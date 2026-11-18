use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Sizable, WindowExt};
use threadlane_ui_kit as kit;
use std::collections::HashSet;
use threadlane_ui_state::{
    AppState, ChatMessageInfo, MessageRole, SubagentActivityInfo, SubagentActivityStatus,
};
use threadlane_ui_state::{actions::AppAction, controller};

/// Run a mutating wire-level `GitOperation` on the attached daemon and
/// surface the action's own error (transport failures included).
async fn run_git_op(
    client: &std::sync::Arc<dyn threadlane_client::DaemonClient>,
    work_dir: &std::path::Path,
    operation: threadlane_protocol::repo::GitOperation,
) -> Result<(), String> {
    let outcome = threadlane_ui_state::project_io::run_action(client, work_dir, operation).await?;
    match outcome.action_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Whether the subagent worktree still exists on the daemon host. The
/// worktree's own parent dir anchors the confined existence probe — an
/// isolated worktree lives under the *primary* checkout's `.threadlane`
/// lane, so it is never under the session root when the active session
/// itself runs from a linked worktree. On a transport failure we assume
/// present and let `git worktree` report honestly.
async fn worktree_present(
    client: &std::sync::Arc<dyn threadlane_client::DaemonClient>,
    worktree: &std::path::Path,
) -> bool {
    let (Some(parent), Some(name)) = (worktree.parent(), worktree.file_name()) else {
        return true;
    };
    threadlane_ui_state::project_io::file_exists(
        client,
        parent,
        name.to_string_lossy().into_owned(),
    )
    .await
    .unwrap_or(true)
}

fn agent_worktree_target_matches(
    expected: &SubagentActivityInfo,
    current: &SubagentActivityInfo,
    action: kit::AgentWorktreeAction,
) -> bool {
    expected.batch_run_id == current.batch_run_id
        && expected.task_index == current.task_index
        && expected.journal_run_id == current.journal_run_id
        && expected.lane == current.lane
        && expected.isolation == current.isolation
        && kit::agent_worktree_action_enabled(action, current.status, true)
}
fn agent_worktree_target_current(
    state: &AppState,
    session: Option<&str>,
    root: &std::path::Path,
    expected: &SubagentActivityInfo,
    action: kit::AgentWorktreeAction,
) -> bool {
    state.active_session_id.as_deref() == session
        && state.active_git_work_dir().as_deref() == Some(root)
        && state
            .active_subagents()
            .iter()
            .any(|current| agent_worktree_target_matches(expected, current, action))
}

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
        let latest = state
            .messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::Assistant)
            .and_then(|message| {
                let text = message.content.trim();
                (!text.is_empty()).then(|| text.chars().take(140).collect::<String>())
            });
        kit::agent_main_summary(state.is_generating, latest, cx)
    }

    fn render_message(
        &mut self,
        message: &ChatMessageInfo,
        row_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let session = self
            .model
            .read(cx)
            .active_session_id
            .clone()
            .unwrap_or_default();
        let run = self.transcript_run_id.clone().unwrap_or_default();
        let owner = cx.entity().downgrade();
        let tools = message
            .tool_activities
            .iter()
            .enumerate()
            .map(|(ix, activity)| {
                // Old imported tools can lack an ID; their append-only position is the fallback.
                let tool_id = if activity.id.is_empty() {
                    ix.to_string()
                } else {
                    activity.id.clone()
                };
                let key = format!("{session}:{run}:{}:{tool_id}", message.id);
                let expanded = self.collapsed_tool_details.contains(&key);
                let toggle_key = key.clone();
                let owner = owner.clone();
                let (row, motion) = kit::agent_tool_activity(
                    key,
                    activity,
                    expanded,
                    move |_, cx| {
                        let _ = owner.update(cx, |this, cx| {
                            if !this.collapsed_tool_details.insert(toggle_key.clone()) {
                                this.collapsed_tool_details.remove(&toggle_key);
                            }
                            cx.notify();
                        });
                    },
                    window,
                    cx,
                );
                motion.remeasure_list_row(&self.transcript_list, row_index, window);
                row
            })
            .collect();
        kit::agent_activity_message(message, tools, cx).into_any_element()
    }

    fn render_detail(
        &mut self,
        item: &SubagentActivityInfo,
        has_messages: bool,
        cx: &mut Context<Self>,
    ) -> Div {
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
        kit::agent_detail_surface(cx)
            .child(kit::agent_detail_header(
                item.agent.clone(),
                item.status,
                item.task.clone(),
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
                cx,
            ))
            .children(branch_controls)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .children((!has_messages).then(|| kit::agent_empty_state(false, cx)))
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
        window: &mut Window,
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
            return kit::agent_error_row(error, cx).into_any_element();
        }
        kit::agent_activity_row(cx)
            .children(
                message
                    .as_ref()
                    .map(|message| self.render_message(message, index, window, cx)),
            )
            .into_any_element()
    }

    fn render_branch_controls(
        &self,
        item: &SubagentActivityInfo,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let isolation = item.isolation.as_ref()?;
        let state = self.model.read(cx);
        let root = state.active_git_work_dir()?;
        let branch = isolation.branch.clone();
        let worktree = isolation.workspace.clone();
        // The worktree lives on the daemon host; remote callers verify
        // presence through project-io inside the action instead of
        // touching the client's disk.
        let worktree_available = state.daemon_remote || worktree.is_dir();
        let guard_session = state.active_session_id.clone();
        let guard_root = root.clone();
        let guard_client = state.daemon_client.clone();
        let guard_target = item.clone();

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

        Some(kit::agent_worktree_controls(
            &Self::run_id(item),
            isolation,
            item.status,
            worktree_available,
            move |action, window, cx| {
                if !agent_worktree_target_current(
                    inspect_model.read(cx),
                    guard_session.as_deref(),
                    &guard_root,
                    &guard_target,
                    action,
                ) || !std::sync::Arc::ptr_eq(
                    &inspect_model.read(cx).daemon_client,
                    &guard_client,
                ) {
                    window.push_notification(
                        "This agent worktree changed. Reopen its controls before continuing.",
                        cx,
                    );
                    return;
                }
                match action {
                    kit::AgentWorktreeAction::Inspect => {
                        let root = inspect_root.clone();
                        let branch = inspect_branch.clone();
                        let label = branch.clone();
                        let client = inspect_model.read(cx).daemon_client.clone();
                        let task = cx.background_executor().spawn(async move {
                            threadlane_ui_state::project_io::diff_branch(&client, &root, branch)
                                .await
                                .map(|diff| (root, diff))
                        });
                        let model = inspect_model.clone();
                        cx.spawn(async move |cx| {
                            let result = task.await;
                            let _ = model.update(cx, |state, cx| {
                                match result {
                                    Ok((root, diff)) => state.request_open_diff(
                                        root,
                                        format!("{label}.diff"),
                                        if diff.is_empty() {
                                            "No committed changes on this branch.".into()
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
                    }
                    kit::AgentWorktreeAction::Terminal => {
                        terminal_model.update(cx, |state, cx| {
                            controller::dispatch(
                                state,
                                AppAction::OpenTerminalAt(terminal_worktree.clone()),
                            );
                            cx.notify();
                        });
                    }
                    kit::AgentWorktreeAction::Apply => {
                        let root = apply_root.clone();
                        let branch = apply_branch.clone();
                        let worktree = apply_worktree.clone();
                        let client = apply_model.read(cx).daemon_client.clone();
                        let task = cx.background_executor().spawn(async move {
                            let parent = threadlane_ui_state::project_io::inspect(
                                &client, &root, false,
                            )
                            .await?;
                            if parent.has_changes {
                                return Err("Commit or stash parent changes before applying a subagent branch.".into());
                            }
                            let worktree_present =
                                worktree_present(&client, &worktree).await;
                            if worktree_present
                                && threadlane_ui_state::project_io::inspect(
                                    &client, &worktree, false,
                                )
                                .await?
                                .has_changes
                            {
                                return Err("The subagent worktree has uncommitted changes; commit them before applying.".into());
                            }
                            run_git_op(
                                &client,
                                &root,
                                threadlane_protocol::repo::GitOperation::Merge {
                                    branch: branch.clone(),
                                },
                            )
                            .await?;
                            if worktree_present {
                                // The daemon also clears the
                                // worktree's cargo-target lane.
                                run_git_op(
                                    &client,
                                    &root,
                                    threadlane_protocol::repo::GitOperation::RemoveWorktree {
                                        worktree: worktree.clone(),
                                        force: false,
                                    },
                                )
                                .await?;
                            }
                            run_git_op(
                                &client,
                                &root,
                                threadlane_protocol::repo::GitOperation::DeleteBranch {
                                    branch: branch.clone(),
                                    force: false,
                                },
                            )
                            .await?;
                            Ok(format!("Applied {branch}"))
                        });
                        let model = apply_model.clone();
                        cx.spawn(async move |cx| {
                            let result = task.await;
                            let _ = model.update(cx, |state, cx| {
                                state.session_status = Some(result.unwrap_or_else(|error| error));
                                cx.notify();
                            });
                        })
                        .detach();
                    }
                    kit::AgentWorktreeAction::Discard => {
                        let root = discard_root.clone();
                        let branch = discard_branch.clone();
                        let worktree = discard_worktree.clone();
                        let model = discard_model.clone();
                        let session = guard_session.clone();
                        let target = guard_target.clone();
                        let client = guard_client.clone();
                        let isolation = threadlane_protocol::events::SubagentIsolation {
                            branch: branch.clone(),
                            workspace: worktree.clone(),
                        };
                        window.open_alert_dialog(cx, move |alert, _, _| {
                            let model = model.clone();
                            let root = root.clone();
                            let branch = branch.clone();
                            let worktree = worktree.clone();
                            let session = session.clone();
                            let target = target.clone();
                            let client = client.clone();
                            kit::agent_worktree_discard_dialog(alert, &isolation, &root)
                                .on_ok(move |_,window,cx| {
                                    if !agent_worktree_target_current(model.read(cx), session.as_deref(), &root, &target, kit::AgentWorktreeAction::Discard)
                                        || !std::sync::Arc::ptr_eq(&model.read(cx).daemon_client, &client) {
                                        window.push_notification("This agent worktree changed. Reopen its controls before discarding it.",cx);
                                        return false;
                                    }
                                    let model = model.clone();
                                    let root = root.clone();
                                    let branch = branch.clone();
                                    let worktree = worktree.clone();
                                    let client = client.clone();
                                    cx.spawn(async move |cx| {
                                        let task = cx.background_executor().spawn(async move {
                                            if worktree_present(&client, &worktree).await
                                            {
                                                run_git_op(
                                                    &client,
                                                    &root,
                                                    threadlane_protocol::repo::GitOperation::RemoveWorktree {
                                                        worktree: worktree.clone(),
                                                        force: true,
                                                    },
                                                )
                                                .await?;
                                            }
                                            run_git_op(
                                                &client,
                                                &root,
                                                threadlane_protocol::repo::GitOperation::DeleteBranch {
                                                    branch: branch.clone(),
                                                    force: true,
                                                },
                                            )
                                            .await?;
                                            let _ = run_git_op(
                                                &client,
                                                &root,
                                                threadlane_protocol::repo::GitOperation::PruneWorktrees,
                                            )
                                            .await;
                                            Ok::<_, String>(format!("Discarded {branch}"))
                                        });
                                        let result = task.await;
                                        let _ = model.update(cx, |state, cx| {
                                            state.session_status =
                                                Some(result.unwrap_or_else(|error| error));
                                            cx.notify();
                                        });
                                    }).detach();
                                    true
                                })
                        });
                    }
                }
            },
            cx,
        ))
    }
}

impl Render for AgentsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    messages: Vec::new(),
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
        let main_description = if main_working {
            "Main agent · Working"
        } else {
            "Main agent · Ready"
        };
        let tabs = kit::agent_profile_tabs(cx)
            .child(
                kit::agent_profile_button(
                    "agents-profile-main",
                    "Main",
                    main_description,
                    if main_working {
                        SubagentActivityStatus::Running
                    } else {
                        SubagentActivityStatus::Completed
                    },
                    main_selected,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.selected_run_id = Some("main".to_string());
                    cx.notify();
                })),
            )
            .children(subagents.iter().map(|(item, _)| {
                let id = Self::run_id(item);
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
                let description = format!(
                    "{} · {}\n{}",
                    name,
                    kit::agent_status_label(item.status),
                    item.task
                );
                kit::agent_profile_button(
                    SharedString::from(format!("agents-profile-{id}")),
                    name,
                    description,
                    item.status,
                    selected,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_run_id = Some(id.clone());
                    cx.notify();
                }))
            }));
        let profile = if let Some((item, count)) = selected {
            self.render_detail(&item, count > 0, cx).into_any_element()
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
                        .children((main_count == 0).then(|| kit::agent_empty_state(true, cx)))
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
        kit::agent_panel_surface(cx)
            .child(tabs)
            .child(div().flex_1().min_h_0().flex().flex_col().child(profile))
    }
}

#[cfg(test)]
mod tests {
    use super::AgentsPanel;
    use threadlane_ui_state::SubagentActivityStatus;

    #[gpui::test]
    fn selected_agent_activity_fits_below_profile_strip(cx: &mut gpui::TestAppContext) {
        use gpui::{px, size, AppContext as _};
        use threadlane_protocol::{AgentEvent, SubagentProgressUpdate};
        use threadlane_ui_state::{AppState, SessionEvent};

        cx.update(gpui_component::init);
        let model = cx.new(|_| {
            let mut state = AppState::default();
            state.active_work_dir = Some(std::env::temp_dir().join("threadlane-agent-layout-test"));
            state.active_session_id = Some("session".into());
            state.drain_chat_stream(vec![
                SessionEvent::Agent {
                    session_id: "session".into(),
                    event: AgentEvent::SubagentQueued {
                        run_id: 1,
                        task_index: 0,
                        agent: "worker".into(),
                        task: "Inspect the repository".into(),
                    },
                },
                SessionEvent::Agent {
                    session_id: "session".into(),
                    event: AgentEvent::SubagentStarted {
                        run_id: 1,
                        task_index: 0,
                        journal_run_id: "child-run".into(),
                        lane: "child-lane".into(),
                        agent: "worker".into(),
                        task: "Inspect the repository".into(),
                        model: "sidekick".into(),
                        isolation: None,
                    },
                },
            ]);
            state
        });
        let panel_model = model.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let panel = cx.new(|cx| AgentsPanel::new(panel_model, window, cx));
            gpui_component::Root::new(panel, window, cx)
        });
        for width in [280.0, 640.0] {
            cx.simulate_resize(size(px(width), px(650.0)));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let detail = cx
                .debug_bounds("agent-detail")
                .expect("selected agent detail");
            assert!(
                detail.size.height > px(300.0),
                "activity area collapsed: {detail:?}"
            );
            let header = cx
                .debug_bounds("agent-detail-header")
                .expect("agent prompt and status");
            assert!(
                header.top() >= px(0.0) && header.bottom() < px(650.0),
                "header offscreen: {header:?}"
            );
        }
        for update in [
            SubagentProgressUpdate::ToolStarted {
                tool_call_id: "read-1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"README.md"}"#.into(),
            },
            SubagentProgressUpdate::TextDelta {
                delta: "Found the relevant code".into(),
            },
        ] {
            model.update(cx, |state, cx| {
                state.drain_chat_stream(vec![SessionEvent::Agent {
                    session_id: "session".into(),
                    event: AgentEvent::SubagentUpdate {
                        run_id: 1,
                        task_index: 0,
                        journal_run_id: "child-run".into(),
                        lane: "child-lane".into(),
                        update,
                    },
                }]);
                cx.notify();
            });
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let message = cx
                .debug_bounds("agent-activity-message")
                .expect("live agent activity is rendered");
            assert!(message.size.height > px(0.0));
            assert!(
                message.top() >= px(0.0) && message.bottom() <= px(650.0),
                "activity offscreen: {message:?}"
            );
        }
    }

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

#[cfg(test)]
#[path = "agents_worktree_tests.rs"]
mod worktree_tests;
