//! Local agent-panel host. Saved main/child transcripts never dispatch a run.
use gpui::{prelude::*, *};
use gpui_component::button::Button;
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Sizable, WindowExt};
use std::collections::HashSet;
use threadlane_protocol::daemon::{
    ChatMessageInfo, MessageRole, SubagentActivityInfo, SubagentActivityStatus, ToolActivityInfo,
};
use threadlane_ui_kit as kit;

pub enum AgentsPreviewEvent {
    Prompt(String),
    OpenSampleFile,
    OpenTerminal,
    OpenDiff { title: String, content: String },
    /// Back to the conversation from compact focus.
    Back,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct CapturedAgentWorktree {
    pub(super) available: bool,
    pub(super) diff: Option<String>,
}

pub struct AgentsPreview {
    main: Vec<ChatMessageInfo>,
    agents: Vec<SubagentActivityInfo>,
    selected: String,
    list: ListState,
    expanded: HashSet<String>,
    sample: bool,
    worktrees: std::collections::HashMap<String, CapturedAgentWorktree>,
    parent: std::path::PathBuf,
    surface: kit::RightPanelSurface,
    focus_back: bool,
    browser: Entity<crate::browser::BrowserPreview>,
    files: Entity<crate::files::FilesPreview>,
    review: Entity<crate::review::ReviewPreview>,
    trajectory: Entity<kit::TrajectoryView<crate::trajectory::CapturedTrajectory>>,
    _review_subscription: Subscription,
    _review_excerpt_subscription: Subscription,
}

impl EventEmitter<AgentsPreviewEvent> for AgentsPreview {}
impl EventEmitter<kit::ReviewExcerptRequest> for AgentsPreview {}

impl AgentsPreview {
    /// The host marks compact focus, where the panel header carries Back.
    pub fn set_focus_back(&mut self, focus_back: bool, cx: &mut Context<Self>) {
        if self.focus_back != focus_back {
            self.focus_back = focus_back;
            cx.notify();
        }
    }

    pub fn new(
        main: Vec<ChatMessageInfo>,
        agents: Vec<SubagentActivityInfo>,
        snapshot: Option<&crate::session::Snapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sample = agents.is_empty();
        let agents = if sample { samples() } else { agents };
        let selected = agents.first().map(run_id).unwrap_or_else(|| "main".into());
        let count = agents
            .first()
            .map_or(main.len(), |agent| agent.messages.len())
            + 1;
        let owner = cx.entity().downgrade();
        let files = cx.new(|cx| {
            crate::files::FilesPreview::new(
                move |_, cx| {
                    let _ = owner.update(cx, |_, cx| cx.emit(AgentsPreviewEvent::OpenSampleFile));
                },
                cx,
            )
        });
        let review = cx.new(|cx| crate::review::ReviewPreview::new(snapshot, window, cx));
        let review_subscription = cx.subscribe(&review, |_, _, event: &crate::review::ReviewPreviewEvent, cx| {
            let crate::review::ReviewPreviewEvent::OpenDiff { title, content } = event;
            cx.emit(AgentsPreviewEvent::OpenDiff { title: title.clone(), content: content.clone() });
        });
        let worktrees = if sample {
            agents.iter().enumerate().map(|(index,agent)| (run_id(agent),CapturedAgentWorktree {
                available: index != 1,
                diff: Some("diff --git a/ui-kit/src/agents.rs b/ui-kit/src/agents.rs\n--- a/ui-kit/src/agents.rs\n+++ b/ui-kit/src/agents.rs\n@@ -1 +1 @@\n-// separate branch controls\n+// shared agent worktree controls (preview sample)\n".into()),
            })).collect()
        } else {
            snapshot
                .map(|snapshot| snapshot.agent_worktrees.clone())
                .unwrap_or_default()
        };
        let review_excerpt_subscription = cx
            .subscribe(&review, |_, _, request: &kit::ReviewExcerptRequest, cx| {
                cx.emit(request.clone())
            });
        Self {
            worktrees,
            parent: snapshot
                .and_then(|snapshot| snapshot.session.as_ref())
                .map(|session| session.runtime_work_dir.clone())
                .unwrap_or_else(|| "/preview/project".into()),
            main,
            agents,
            selected,
            list: ListState::new(count, ListAlignment::Top, window.rem_size() * 8.0),
            expanded: HashSet::new(),
            sample,
            surface: kit::RightPanelSurface::Agents,
            focus_back: false,
            browser: cx.new(|cx| crate::browser::BrowserPreview::new(window, cx)),
            files,
            review,
            trajectory: cx.new(|cx| {
                kit::TrajectoryView::new(
                    crate::trajectory::CapturedTrajectory::new(snapshot),
                    window,
                    cx,
                )
            }),
            _review_subscription: review_subscription,
            _review_excerpt_subscription: review_excerpt_subscription,
        }
    }

    fn worktree_action(
        &mut self,
        id: &str,
        action: kit::AgentWorktreeAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(agent) = self
            .agents
            .iter()
            .find(|agent| run_id(agent) == id)
            .cloned()
        else {
            return;
        };
        let Some(isolation) = agent.isolation.clone() else {
            return;
        };
        let capture = self.worktrees.get(id).cloned();
        if !kit::agent_worktree_action_enabled(
            action,
            agent.status,
            capture.as_ref().is_some_and(|capture| capture.available),
        ) {
            return;
        }
        match action {
            kit::AgentWorktreeAction::Inspect => match capture.and_then(|capture| capture.diff) {
                Some(diff) => cx.emit(AgentsPreviewEvent::OpenDiff { title: format!("{}.diff",isolation.branch), content: if diff.is_empty() { "No committed changes on this branch.".into() } else { diff } }),
                None => window.push_notification("This branch diff was not captured. Reimport the saved session to capture its current branch diff.",cx),
            },
            kit::AgentWorktreeAction::Terminal => {
                cx.emit(AgentsPreviewEvent::OpenTerminal);
                window.push_notification("Opened the local terminal sample. No process opens in the captured worktree.",cx);
            }
            kit::AgentWorktreeAction::Apply => window.push_notification("Preview only: Apply would merge committed changes and remove the agent worktree and branch. No Git operation was run.",cx),
            kit::AgentWorktreeAction::Discard => {
                let parent = self.parent.clone(); let owner=cx.weak_entity(); let id=id.to_owned();
                window.open_alert_dialog(cx,move |alert,_,_| {
                    let owner=owner.clone(); let id=id.clone(); let expected=isolation.clone();
                    kit::agent_worktree_discard_dialog(alert,&isolation,&parent).on_ok(move |_,window,cx| {
                        owner.update(cx,|host,cx| {
                            let valid=host.agents.iter().find(|agent| run_id(agent)==id).is_some_and(|agent| agent.isolation.as_ref()==Some(&expected) && kit::agent_worktree_action_enabled(kit::AgentWorktreeAction::Discard,agent.status,true));
                            if !valid { window.push_notification("This agent worktree changed. Reopen its controls before discarding it.",cx); return false; }
                            window.push_notification("Preview discard confirmed. No branch, worktree or saved transcript was changed.",cx);
                            true
                        }).unwrap_or(false)
                    })
                });
            }
        }
        cx.notify();
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected = id;
        self.list.reset(self.messages().len() + 1);
        cx.notify();
    }

    fn selected_agent(&self) -> Option<&SubagentActivityInfo> {
        self.agents
            .iter()
            .find(|agent| run_id(agent) == self.selected)
    }

    fn messages(&self) -> &[ChatMessageInfo] {
        self.selected_agent()
            .map_or(self.main.as_slice(), |agent| agent.messages.as_slice())
    }

    fn row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if index == 0 {
            return kit::agent_error_row(
                self.selected_agent().and_then(|agent| agent.error.clone()),
                cx,
            )
            .into_any_element();
        }
        let Some(message) = self.messages().get(index - 1).cloned() else {
            return div().into_any_element();
        };
        let owner = cx.entity().downgrade();
        let tools = message
            .tool_activities
            .iter()
            .enumerate()
            .map(|(ix, activity)| {
                let tool_id = if activity.id.is_empty() {
                    ix.to_string()
                } else {
                    activity.id.clone()
                };
                let key = format!("preview:{}:{}:{tool_id}", self.selected, message.id);
                let expanded = self.expanded.contains(&key);
                let toggle_key = key.clone();
                let owner = owner.clone();
                let (row, motion) = kit::agent_tool_activity(
                    key,
                    activity,
                    expanded,
                    move |_, cx| {
                        let _ = owner.update(cx, |host, cx| {
                            if !host.expanded.insert(toggle_key.clone()) {
                                host.expanded.remove(&toggle_key);
                            }
                            cx.notify();
                        });
                    },
                    window,
                    cx,
                );
                motion.remeasure_list_row(&self.list, index, window);
                row
            })
            .collect();
        kit::agent_activity_row(cx)
            .child(kit::agent_activity_message(&message, tools, cx))
            .into_any_element()
    }
}

fn run_id(agent: &SubagentActivityInfo) -> String {
    format!("queued-{}-{}", agent.batch_run_id, agent.task_index)
}

impl Render for AgentsPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let back = self.focus_back.then(|| {
            kit::right_panel_back_button()
                .on_click(cx.listener(|_, _, _, cx| cx.emit(AgentsPreviewEvent::Back)))
        });
        let header = kit::right_panel_header(
            Some(self.surface),
            &kit::RightPanelSurface::ALL,
            None,
            back,
            cx.listener(|host, surface: &kit::RightPanelSurface, _, cx| {
                if *surface != kit::RightPanelSurface::Review {
                    host.review.update(cx, |review, cx| review.leave_review(cx));
                }
                host.surface = *surface;
                cx.notify();
            }),
            cx,
        );
        if self.surface == kit::RightPanelSurface::Browser {
            return kit::agent_panel_surface(cx)
                .child(header)
                .child(self.browser.clone());
        }
        if self.surface == kit::RightPanelSurface::Files {
            return kit::agent_panel_surface(cx)
                .child(header)
                .child(self.files.clone());
        }
        if self.surface == kit::RightPanelSurface::Review {
            return kit::agent_panel_surface(cx)
                .child(header)
                .child(self.review.clone());
        }
        if self.surface == kit::RightPanelSurface::Trajectory {
            return kit::agent_panel_surface(cx).child(header).child(self.trajectory.clone());
        }
        let tabs = kit::agent_profile_tabs(cx)
            .child(
                kit::agent_profile_button(
                    "agents-profile-main",
                    "Main",
                    "Main agent · Saved session",
                    SubagentActivityStatus::Completed,
                    self.selected == "main",
                    cx,
                )
                .debug_selector(|| "preview-agent-main".into())
                .on_click(cx.listener(|host, _, _, cx| host.select("main".into(), cx))),
            )
            .children(self.agents.iter().map(|agent| {
                let id = run_id(agent);
                let name = if self
                    .agents
                    .iter()
                    .filter(|other| other.agent == agent.agent)
                    .count()
                    > 1
                {
                    format!("{} {}", agent.agent, agent.task_index + 1)
                } else {
                    agent.agent.clone()
                };
                let description = format!(
                    "{} · {}\n{}",
                    name,
                    kit::agent_status_label(agent.status),
                    agent.task
                );
                kit::agent_profile_button(
                    SharedString::from(format!("agents-profile-{id}")),
                    name,
                    description,
                    agent.status,
                    self.selected == id,
                    cx,
                )
                .debug_selector({
                    let id = id.clone();
                    move || format!("preview-agent-{id}")
                })
                .on_click(cx.listener(move |host, _, _, cx| host.select(id.clone(), cx)))
            }));
        let selected = self.selected_agent().cloned();
        let has_messages = !self.messages().is_empty();
        let has_rows = has_messages || selected.is_some();
        let activity = div()
            .debug_selector(|| "preview-agent-activity-viewport".into())
            .flex_1()
            .min_h_0()
            .relative()
            .children((!has_messages).then(|| kit::agent_empty_state(selected.is_none(), cx)))
            .children(has_rows.then(|| {
                list(self.list.clone(), cx.processor(Self::row))
                    .size_full()
                    .with_sizing_behavior(ListSizingBehavior::Auto)
            }))
            .children(has_messages.then(|| {
                div()
                    .absolute()
                    .inset_0()
                    .child(Scrollbar::vertical(&self.list))
            }));
        let worktree_controls = selected.as_ref().and_then(|agent| {
            agent.isolation.as_ref().map(|isolation| {
                let id = run_id(agent);
                let available = self
                    .worktrees
                    .get(&id)
                    .is_some_and(|capture| capture.available);
                let owner = cx.weak_entity();
                kit::agent_worktree_controls(
                    &id.clone(),
                    isolation,
                    agent.status,
                    available,
                    move |action, window, cx| {
                        let _ = owner
                            .update(cx, |host, cx| host.worktree_action(&id, action, window, cx));
                    },
                    cx,
                )
            })
        });
        let profile = if let Some(agent) = selected {
            let live = matches!(
                agent.status,
                SubagentActivityStatus::Running | SubagentActivityStatus::Queued
            );
            let target = agent.lane.as_deref().unwrap_or(&agent.agent);
            let prompt = if live {
                format!("Send this message to subagent {target}: ")
            } else {
                format!("Continue subagent {target} with this follow-up: ")
            };
            kit::agent_detail_surface(cx)
                .child(kit::agent_detail_header(
                    agent.agent,
                    agent.status,
                    agent.task,
                    Button::new("preview-agent-message")
                        .debug_selector(|| "preview-agent-message".into())
                        .label(if live { "Message…" } else { "Continue…" })
                        .outline()
                        .xsmall()
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(AgentsPreviewEvent::Prompt(prompt.clone()))
                        })),
                    cx,
                ))
                .children(worktree_controls)
                .child(activity)
                .into_any_element()
        } else {
            let latest = self
                .main
                .iter()
                .rev()
                .find(|message| message.role == MessageRole::Assistant)
                .and_then(|message| {
                    (!message.content.trim().is_empty())
                        .then(|| message.content.trim().chars().take(140).collect())
                });
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(kit::agent_main_summary(false, latest, cx))
                .child(activity)
                .into_any_element()
        };
        kit::agent_panel_surface(cx)
            .child(header)
            .child(tabs)
            .child(profile)
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.sample {
                        "Sample agents · Main uses the saved session · No runs dispatched"
                    } else {
                        "Saved agent transcripts · No runs dispatched"
                    }),
            )
    }
}

fn samples() -> Vec<SubagentActivityInfo> {
    [
        ("Explorer", SubagentActivityStatus::Running, "Inspect the existing shared components", vec![ChatMessageInfo {
            id: "sample-explorer-message".into(), role: MessageRole::Assistant, content: "Checking the component boundaries.".into(),
            tool_activities: vec![ToolActivityInfo { id: "sample-read".into(), title: "read_file".into(), category: "Loaded".into(), display_summary: "Read ui-kit/src/lib.rs".into(),
                arguments: r#"{"path":"ui-kit/src/lib.rs"}"#.into(), detail: "1:a3f| pub mod agents;\n2:b4c| pub use agents::*;".into(), is_expanded: false }],
            streaming: false, reasoning_content: None, reasoning_expanded: false,
            retry_prompt: None,
        }]),
        ("Reviewer", SubagentActivityStatus::Failed, "Validate the shared layout at narrow widths", vec![ChatMessageInfo {
            id: "sample-review-message".into(), role: MessageRole::Error, content: "A layout check needs attention.".into(),
            tool_activities: vec![ToolActivityInfo { id: "sample-check".into(), title: "run_command".into(), category: "Error".into(), display_summary: "Check the preview layout".into(),
                arguments: r#"{"command":"cargo check -p threadlane-ui-kit-preview"}"#.into(), detail: "Exit Status: exit status: 1\n--- STDOUT ---\n\n--- STDERR ---\nSample error: profile header exceeds the panel width.".into(), is_expanded: false }],
            streaming: false, reasoning_content: None, reasoning_expanded: false,
            retry_prompt: None,
        }]),
        ("Reviewer", SubagentActivityStatus::Completed, "Review the shared tool disclosures", vec![ChatMessageInfo {
            id: "sample-completed-message".into(), role: MessageRole::Assistant, content: "The tool rows reuse the same components as chat.".into(), tool_activities: Vec::new(), streaming: false, reasoning_content: None, reasoning_expanded: false,
            retry_prompt: None,
        }]),
        ("Designer", SubagentActivityStatus::Queued, "Review the activity hierarchy", Vec::new()),
    ].into_iter().enumerate().map(|(task_index, (agent, status, task, messages))| SubagentActivityInfo {
        batch_run_id: 1, task_index, journal_run_id: None, lane: Some(format!("sample-{task_index}")), agent: agent.into(), task: task.into(), model: None,
        status, messages, isolation: Some(threadlane_protocol::events::SubagentIsolation {
            branch: format!("agents/sample-{task_index}"), workspace: format!("/preview/project/.threadlane/worktrees/sample-{task_index}").into(),
        }), error: (status == SubagentActivityStatus::Failed).then(|| "Sample validation failed. Continue to review the result.".into()),
    }).collect()
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;
