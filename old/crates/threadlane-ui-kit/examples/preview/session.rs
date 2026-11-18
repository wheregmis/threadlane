//! Local preview host. Production components own appearance; this host owns fixture state.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::CommandState;
use gpui_component::combobox::{ComboboxEvent, ComboboxState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenu};
use gpui_component::input::{InputEvent, InputState, TextareaState};
use gpui_component::resizable::ResizableState;
use gpui_component::{ActiveTheme, Selectable, Sizable, WindowExt};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use threadlane_protocol::daemon::{
    ChatMessageInfo, MessageRole, SessionAttention, SessionInfo, ToolActivityInfo,
};
use threadlane_ui_kit::{
    self as kit,
    transcript::{TranscriptRow, TranscriptState},
};

#[path = "palette.rs"]
mod palette;
#[path = "conversation_find.rs"]
mod conversation_find;
#[path = "prompt_navigation.rs"]
mod prompt_navigation;
#[path = "message_actions.rs"]
mod message_actions;
#[path = "message_markdown.rs"]
mod message_markdown;
#[path = "file_completion.rs"]
mod file_completion;
pub(super) use palette::init as init_palette;

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Snapshot {
    pub(super) session: Option<SessionInfo>,
    pub(super) messages: Vec<ChatMessageInfo>,
    #[serde(default)]
    pub(super) trajectory: Vec<threadlane_protocol::daemon::TrajectoryEntry>,
    #[serde(default)]
    pub(super) trajectory_model_context: Vec<threadlane_protocol::daemon::TrajectoryEntry>,
    #[serde(default)]
    pub(super) trajectory_durable_events: Vec<threadlane_protocol::daemon::TrajectoryEntry>,
    #[serde(default)]
    pub(super) trajectory_recovery: Vec<threadlane_protocol::daemon::TrajectoryEntry>,
    #[serde(default)]
    pub(super) subagents: Vec<threadlane_protocol::daemon::SubagentActivityInfo>,
    #[serde(default)]
    pub(super) agent_worktrees: HashMap<String, crate::agents::CapturedAgentWorktree>,
    #[serde(default)]
    pub(super) captured_at: u64,
    #[serde(default)]
    sidebar_snooze_choices: Vec<(String, u64, String)>,
    #[serde(default)]
    pub(super) git_status: Option<threadlane_protocol::repo::GitStatus>,
    #[serde(default)]
    pub(super) review_diffs: HashMap<String, crate::review::CapturedReviewDiff>,
    #[serde(default)]
    pub(super) review_combined_diff: Option<crate::review::CapturedReviewDiff>,
    #[serde(default)]
    pub(super) review_can_create_pr: bool,
    #[serde(default)]
    pub(super) review_feedback_count: Option<usize>,
    #[serde(default)]
    pub(super) review_file_manager_label: Option<String>,
    #[serde(default)]
    pub(super) review_commits: HashMap<String, crate::review::CapturedReviewRecord>,
    #[serde(default)]
    pub(super) review_stashes: HashMap<usize, crate::review::CapturedReviewRecord>,
    #[serde(default)]
    pub(super) token_efficiency: Option<threadlane_protocol::efficiency::TokenEfficiencyReport>,
    #[serde(default)]
    pub(super) plan: threadlane_protocol::SessionPlan,
    #[serde(default)]
    pub(super) metrics: threadlane_protocol::daemon::SessionMetricsInfo,
    #[serde(default)]
    pub(super) run_timing: Option<threadlane_protocol::daemon::RunTiming>,
    #[serde(default)]
    pub(super) runnable_code_languages: Vec<String>,
    #[serde(default)]
    pub(super) file_inventory: Option<Result<threadlane_protocol::repo::GitFileInventory, String>>,
    #[serde(default)]
    pub(super) context_window: Option<threadlane_protocol::daemon::ContextWindowInfo>,
    #[serde(default)]
    pub(super) model: Option<threadlane_protocol::daemon::ComposerModel>,
    #[serde(default)]
    pub(super) model_provider_label: Option<String>,
    #[serde(default)]
    pub(super) model_provider_icon: Option<String>,
    #[serde(default)]
    pub(super) effort: Option<threadlane_protocol::ReasoningEffort>,
    #[serde(default)]
    pub(super) mode: Option<threadlane_protocol::OrchestratorMode>,
    #[serde(default)]
    pub(super) active_skills_count: Option<usize>,
    #[serde(default = "reports_usage_default")]
    pub(super) reports_usage: bool,
}

fn reports_usage_default() -> bool {
    true
}

impl Snapshot {
    fn model_picker_sections(&self) -> Vec<kit::PickerSection<String>> {
        self.model
            .as_ref()
            .map(|model| kit::PickerSection {
                header: self
                    .model_provider_label
                    .clone()
                    .unwrap_or_else(|| "Saved session".into())
                    .into(),
                items: vec![kit::PickerItem {
                    value: model.id.clone(),
                    title: model.label.clone().into(),
                    secondary: None,
                    icon_path: self.model_provider_icon.clone().map(Into::into),
                    current: true,
                    indented: false,
                    haystack: kit::PickerItem::<String>::haystack_for(&[
                        &model.id,
                        &model.label,
                        self.model_provider_label.as_deref().unwrap_or(""),
                    ]),
                }],
            })
            .into_iter()
            .collect()
    }
}

#[cfg(not(target_family = "wasm"))]
pub fn import_requested() -> bool {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("--import-session") {
        return false;
    }
    let result = (|| -> Result<(), String> {
        let source =
            std::path::PathBuf::from(args.get(2).ok_or("Provide a saved session JSONL path")?);
        let source = source.canonicalize().map_err(|error| error.to_string())?;
        let project = source
            .ancestors()
            .find(|path| path.file_name().is_some_and(|name| name == ".threadlane"))
            .and_then(std::path::Path::parent)
            .ok_or("Session must be inside a project's .threadlane directory")?;
        let session = threadlane_daemon::discovery::discover_sessions_in_project(project)
            .into_iter()
            .find(|session| session.session_file == source)
            .map(Ok)
            .unwrap_or_else(|| {
                // Discovery intentionally omits archived sessions. Import their
                // metadata through the canonical read-only store, without restoring them.
                use threadlane_runtime::harness::{JsonlStore, SessionStore};
                let store =
                    JsonlStore::open_read_only(&source).map_err(|error| error.to_string())?;
                let id = source.file_stem().unwrap().to_string_lossy().into_owned();
                let facts = store.facts();
                let runtime_work_dir =
                    threadlane_daemon::discovery::effective_session_work_dir(project, &id, &facts);
                let is_worktree = facts
                    .get("is_worktree")
                    .is_some_and(|value| value == "true");
                Ok::<_, String>(SessionInfo {
                    title: threadlane_runtime::titles::extract_session_title(&store, &id),
                    git_branch: facts.get("git_branch").cloned(),
                    worktree_available: !is_worktree || runtime_work_dir.is_dir(),
                    work_dir: project.to_owned(),
                    runtime_work_dir,
                    session_file: source.clone(),
                    id,
                    is_worktree,
                    updated_at: threadlane_daemon::discovery::file_mtime(&source),
                    ..Default::default()
                })
            })?;
        let messages = threadlane_daemon::projection::compute_session_messages(&source)?;
        let count = messages.len();
        if count == 0 {
            return Err(
                "Session has no conversation rows; choose a transcript rather than a metadata stub"
                    .into(),
            );
        }
        let projection = threadlane_daemon::projection::compute_full_session_projection(&source)?;
        let (trajectory_model_context, trajectory_durable_events, trajectory_recovery) = projection.diagnostics.as_ref().map(|diagnostics| (
            threadlane_daemon::projection::project_model_context_diagnostics(diagnostics),
            threadlane_daemon::projection::project_durable_event_diagnostics(diagnostics),
            threadlane_daemon::projection::project_recovery_diagnostics(&diagnostics.recovery),
        )).unwrap_or_default();
        // Snapshot the same checkout inspection as production; never run services in WASM.
        let git_status = session
            .worktree_available
            .then(|| threadlane_git::inspect(&session.runtime_work_dir).ok())
            .flatten();
        let (review_diffs, review_combined_diff) = git_status.as_ref()
            .map(|status| crate::review::capture_diffs(&session.runtime_work_dir, &status.files))
            .unwrap_or_default();
        let review_can_create_pr = threadlane_git::can_create_pull_request(session.worktree_available, git_status.as_ref());
        let review_feedback_count = git_status.as_ref().and_then(|status| status.pr.as_ref())
            .map(|pr| threadlane_git::collect_actionable_pr_feedback(pr).len());
        let review_file_manager_label = Some(kit::review_file_manager_label().into());
        let (review_commits, review_stashes) = git_status.as_ref()
            .map(|status| crate::review::capture_records(&session.runtime_work_dir, status))
            .unwrap_or_default();
        use threadlane_runtime::harness::{JsonlStore, SessionStore};
        let store = JsonlStore::open_read_only(&source).map_err(|error| error.to_string())?;
        let model = store
            .model()
            .filter(|model| !model.is_empty())
            .or_else(|| {
                projection
                    .context_window
                    .as_ref()
                    .map(|context| context.effective_model.clone())
                    .filter(|model| !model.is_empty())
            })
            .map(|id| threadlane_protocol::daemon::ComposerModel {
                label: threadlane_daemon::catalog::selection_label(
                    &id,
                    &threadlane_daemon::catalog::available_models_for_project(Some(project)),
                ),
                efforts: if threadlane_daemon::catalog::supports_reasoning(&id, Some(project)) {
                    threadlane_daemon::catalog::efforts_for_model(&id, Some(project))
                } else {
                    Vec::new()
                },
                id,
            });
        let effort = store
            .facts()
            .get("reasoning_effort")
            .and_then(|value| threadlane_protocol::ReasoningEffort::from_label(value));
        let provider = model.as_ref().and_then(|model| {
            threadlane_daemon::catalog::available_models_for_project(Some(project))
                .into_iter().find(|option| option.id == model.id).map(|option| option.provider)
        });
        let mode = threadlane_project::subagent_settings::load(project).orchestrator_mode;
        let active_skills_count = threadlane_skills::settings::discover_skills(Some(project))
            .iter()
            .filter(|skill| skill.enabled)
            .count();
        let reports_usage = model
            .as_ref()
            .is_none_or(|model| !threadlane_acp_engine::is_acp_model(&model.id));
        let captured_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        let sidebar_snooze_choices = threadlane_ui_state::SNOOZE_OPTIONS.iter().map(|(label, secs)| (label.to_string(), *secs, threadlane_ui_state::snooze_return_label(captured_at + secs))).collect();
        // Match the existing review import budget; retain complete patches only.
        let mut agent_diff_remaining = 5 * 1024 * 1024;
        let agent_worktrees = projection.subagents.iter().filter_map(|agent| {
            let isolation = agent.isolation.as_ref()?;
            let diff = threadlane_git::diff_branch(&session.runtime_work_dir, &isolation.branch)
                .ok()
                .filter(|diff| {
                    if diff.len() > agent_diff_remaining { return false; }
                    agent_diff_remaining -= diff.len();
                    true
                });
            Some((format!("queued-{}-{}", agent.batch_run_id, agent.task_index), crate::agents::CapturedAgentWorktree {
                available: isolation.workspace.is_dir(),
                diff,
            }))
        }).collect();
        let file_inventory = Some(threadlane_git::list_project_files(&session.runtime_work_dir)
            .map_err(|error| match error {
                threadlane_git::FileInventoryError::NotARepository => threadlane_protocol::repo::FILE_INVENTORY_NOT_A_REPOSITORY.to_owned(),
                threadlane_git::FileInventoryError::Failed(error) => error.to_string(),
            }));
        let snapshot = Snapshot {
            file_inventory,
            agent_worktrees,
            sidebar_snooze_choices,
            model,
            model_provider_label: provider.map(|provider| provider.label().to_owned()),
            model_provider_icon: provider.map(|provider| provider.icon_path().to_owned()),
            effort,
            mode: Some(mode),
            active_skills_count: Some(active_skills_count),
            reports_usage,
            trajectory: projection.trajectory,
            trajectory_model_context,
            trajectory_durable_events,
            trajectory_recovery,
            subagents: projection.subagents,
            plan: projection.plan,
            metrics: projection.metrics,
            run_timing: projection.run_timing,
            runnable_code_languages: ["bash", "sh", "zsh", "shell", "terminal", "console", "cmd", "powershell"].into_iter()
                .filter(|language| kit::markdown::active_shell_supports_language(language))
                .map(str::to_owned).collect(),
            context_window: projection.context_window,
            session: Some(session),
            git_status,
            review_diffs,
            review_combined_diff,
            review_can_create_pr,
            review_feedback_count,
            review_file_manager_label,
            review_commits,
            review_stashes,
            token_efficiency: projection.token_efficiency,
            messages,
            captured_at,
        };
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("session.local.json");
        let json = serde_json::to_vec_pretty(&snapshot).map_err(|error| error.to_string())?;
        std::fs::write(&output, json).map_err(|error| error.to_string())?;
        println!("Imported {count} messages into {}", output.display());
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("Session import: {error}");
        std::process::exit(2);
    }
    true
}

pub struct SessionPreview {
    session: SessionInfo,
    captured_at: u64,
    fixture: Arc<Snapshot>,
    messages: Arc<Vec<ChatMessageInfo>>,
    transcript: TranscriptState,
    markdown: HashMap<(SharedString, String), kit::markdown::MarkdownRenderState>,
    expanded: HashSet<String>,
    input: Entity<TextareaState>,
    selected_file_index: usize,
    dismiss_file_menu: bool,
    file_scroll_handle: ScrollHandle,
    _input_observer: Subscription,
    chat_focus: FocusHandle,
    landmarks: Vec<kit::transcript::PromptLandmark>,
    prompt_rail: ListState,
    prompt_rail_active_id: Option<String>,
    outline_list: ListState,
    outline_open: bool,
    outline_focus: FocusHandle,
    outline_focus_id: Option<String>,
    outline_selected_id: Option<String>,
    prompt_recall: Option<(String, String)>,
    segment_cache: HashMap<String, (String, Vec<kit::markdown::MarkdownSegment>)>,
    /// Per-code-block soft-wrap choices, message id then block index — the
    /// same view-only state ChatListView keeps for its blocks.
    code_wrap_blocks: HashMap<String, HashSet<usize>>,
    copied_message: Option<String>,
    copy_feedback_task: Option<Task<()>>,
    /// Quote snapshots armed per message id: a press on the Quote control
    /// clears the window selection in the capture phase, so the render-time
    /// text must survive until the press's bubble-phase mouse-down consumes
    /// it; any later render without a matching selection drops it.
    armed_quotes: std::cell::RefCell<HashMap<String, String>>,
    find_input: Entity<InputState>,
    find_open: bool,
    find_query: String,
    find_results: Vec<threadlane_protocol::transcript::ConversationMatch>,
    find_selected: Option<String>,
    find_previous_focus: Option<FocusHandle>,
    _find_subscription: Subscription,
    split: Entity<ResizableState>,
    sidebar_collapsed: bool,
    preferred_panel_sizes: [f32; 3],
    panel_layout: Option<(Size<Pixels>, Pixels, [bool; 3])>,
    gallery: Entity<crate::gallery::Gallery>,
    editor: Entity<crate::editor::EditorPreview>,
    editor_open: bool,
    terminal: Entity<crate::terminal::TerminalPreview>,
    terminal_open: bool,
    agents: Entity<crate::agents::AgentsPreview>,
    agents_open: bool,
    right_split: Entity<ResizableState>,
    _agents_subscription: Subscription,
    _review_excerpt_subscription: Subscription,
    bottom_split: Entity<ResizableState>,
    _terminal_subscription: Subscription,
    automations: Entity<crate::automation::AutomationPreview>,
    automations_open: bool,
    github: Entity<crate::github::GitHubPreview>,
    github_open: Option<bool>,
    _github_subscription: Subscription,
    settings: Entity<crate::settings::SettingsPreview>,
    settings_open: bool,
    _settings_subscription: Subscription,
    gallery_open: bool,
    context_meter_open: bool,
    palette_open: bool,
    palette_search: bool,
    palette_sessions: bool,
    palette_previous_focus: Option<FocusHandle>,
    palette_matches: Vec<threadlane_protocol::transcript::ConversationMatch>,
    palette_recent: Vec<&'static str>,
    command_state: Entity<CommandState>,
    _command_subscription: Subscription,
    project_filtered: bool,
    sidebar_pinned: bool,
    sidebar_snooze: Option<kit::SidebarSnoozeStatus>,
    sidebar_removed: bool,
    sidebar_snoozed_collapsed: bool,
    model_picker: Entity<ComboboxState<kit::PickerDelegate<String>>>,
    model_picker_open: std::rc::Rc<std::cell::Cell<bool>>,
    _automation_subscription: Subscription,
    _model_subscription: Subscription,
    _input_subscription: Subscription,
}

impl SessionPreview {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let snapshot: Arc<Snapshot> = Arc::new(
            serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/session.json")))
                .expect("valid preview snapshot"),
        );
        Self::with_snapshot(snapshot, window, cx)
    }

    fn with_snapshot(snapshot: Arc<Snapshot>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Ask a question, or type / for commands, @ for files...")
                .auto_grow(1, 8)
                .submit_on_enter(true)
        });
        input.update(cx, |input,cx| input.focus(window,cx));
        let subscription = cx.subscribe_in(&input, window, |host, input, event: &InputEvent, window, cx| {
            match event {
                InputEvent::Change => {
                    host.dismiss_file_menu = false;
                    host.selected_file_index = 0;
                    if host.prompt_recall.as_ref().is_some_and(|(_, text)| input.read(cx).value().as_str() != text.as_str()) {
                        host.prompt_recall = None;
                    }
                }
                InputEvent::PressEnter { .. } if host.file_menu_open(cx) => host.insert_selected_file(window, cx),
                _ => {}
            }
            cx.notify();
        });
        let input_observer = cx.observe(&input, |_, _, cx| cx.notify());
        let find_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in conversation"));
        let find_subscription = cx.subscribe(&find_input, |host, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && host.find_open {
                host.find_query = input.read(cx).value().to_string();
                host.refresh_conversation_find(cx);
            }
        });
        let model_picker = cx.new(|cx| ComboboxState::new(
            kit::PickerDelegate::new(snapshot.model_picker_sections()), Vec::new(), window, cx).searchable(true));
        let model_subscription = cx.subscribe_in(&model_picker, window, |_, picker, event, window, cx| {
            if matches!(event, ComboboxEvent::Change(_)) {
                // This host never writes model preferences or dispatches a turn.
                picker.update(cx, |picker, cx| picker.set_selected_indices(Vec::new(), window, cx));
                cx.notify();
            }
        });
        let automations = cx.new(|cx| {
            crate::automation::AutomationPreview::new(
                snapshot
                    .session
                    .as_ref()
                    .map(|s| s.work_dir.clone())
                    .unwrap_or_else(|| "/sample-project".into()),
                window,
                cx,
            )
        });
        let automation_subscription = cx.observe(&automations, |_, _, cx| cx.notify());
        let settings = cx.new(|cx| crate::settings::SettingsPreview::new(
            snapshot.session.as_ref().and_then(|session| session.work_dir.file_name()).map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "Sample project".into()), window, cx));
        let settings_subscription = cx.subscribe(&settings, |this, _, _: &crate::settings::SettingsPreviewEvent, cx| { this.settings_open = false; cx.notify(); });
        let github = cx.new(|cx| crate::github::GitHubPreview::new(
            snapshot.session.as_ref().and_then(|session| session.work_dir.file_name()).map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "Sample project".into()), window, cx));
        let github_subscription = cx.subscribe(&github, |this, _, event: &crate::github::GitHubPreviewEvent, cx| {
            match event {
                crate::github::GitHubPreviewEvent::Close => this.github_open = None,
                crate::github::GitHubPreviewEvent::Settings => { this.settings_open = true; this.clear_conversation_find(); this.clear_prompt_navigation(); },
            }
            cx.notify();
        });
        let terminal = cx.new(|cx| {
            crate::terminal::TerminalPreview::new(
                snapshot
                    .session
                    .as_ref()
                    .and_then(|session| session.work_dir.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Sample project".into()),
                cx,
            )
        });
        let terminal_subscription = cx.subscribe_in(
            &terminal,
            window,
            |host, _, event: &crate::terminal::TerminalPreviewEvent, window, cx| {
                match event {
                    crate::terminal::TerminalPreviewEvent::Hide => host.terminal_open = false,
                    crate::terminal::TerminalPreviewEvent::AddSelection(text) => {
                        let draft = host.input.read(cx).value();
                        let draft = if draft.is_empty() {
                            text.clone()
                        } else {
                            format!("{draft}\n\n{text}")
                        };
                        host.input
                            .update(cx, |input, cx| input.set_value(draft, window, cx));
                    }
                }
                cx.notify();
            },
        );
        let agents = cx.new(|cx| crate::agents::AgentsPreview::new(snapshot.messages.clone(), snapshot.subagents.clone(), Some(&snapshot), window, cx));
        let agents_subscription = cx.subscribe_in(&agents, window, |host, _, event: &crate::agents::AgentsPreviewEvent, window, cx| {
            match event {
                crate::agents::AgentsPreviewEvent::Prompt(prompt) => {
                    host.input.update(cx, |input, cx| input.set_value(prompt.clone(), window, cx));
                }
                crate::agents::AgentsPreviewEvent::OpenTerminal => host.terminal_open = true,
                crate::agents::AgentsPreviewEvent::Back => {
                    host.agents_open = false;
                    host.input.update(cx, |input, cx| input.focus(window, cx));
                }
                crate::agents::AgentsPreviewEvent::OpenSampleFile => {
                    host.editor.update(cx, |editor, cx| editor.open_sample(cx));
                    host.editor_open = true;
                    host.clear_conversation_find();
                    host.clear_prompt_navigation();
                }
                crate::agents::AgentsPreviewEvent::OpenDiff { title, content } => {
                    host.editor.update(cx, |editor, cx| editor.open_diff(title.clone(), content.clone(), cx));
                    host.editor_open = true;
                    host.clear_conversation_find();
                    host.clear_prompt_navigation();
                }
            }
            cx.notify();
        });
        let command_state = cx.new(|cx| CommandState::new(window, cx));
        let review_excerpt_subscription = cx.subscribe_in(
            &agents,
            window,
            |host, _, request: &kit::ReviewExcerptRequest, window, cx| {
                let destination = (
                    Some(host.session.work_dir.clone()),
                    Some(host.session.id.clone()),
                );
                if request.target().destination() != &destination
                    || !request
                        .document()
                        .read(cx)
                        .excerpt_request_is_current(request, cx)
                {
                    window.push_notification(kit::STALE_REVIEW_EXCERPT, cx);
                    return;
                }
                match kit::format_review_excerpt(request.target(), request.text()) {
                    Ok(excerpt) => {
                        host.editor_open = false;
                        host.input.update(cx, |input, cx| {
                            let existing = input.value();
                            let separator = if existing.is_empty() || existing.ends_with('\n') {
                                ""
                            } else {
                                "\n"
                            };
                            let text = format!("{existing}{separator}{excerpt}");
                            input.select_all(window, cx);
                            input.replace(text, window, cx);
                            input.focus(window, cx);
                        });
                        cx.notify();
                    }
                    Err(reason) => window.push_notification(reason, cx),
                }
            },
        );
        let command_subscription = cx.observe(&command_state, |_, _, cx| cx.notify());
        let mut transcript = TranscriptState::new(window);
        let messages = Arc::new(snapshot.messages.clone());
        transcript.sync(messages.clone(), false, true, false);
        let landmarks = kit::transcript::prompt_landmarks(&messages, false);
        Self {
            captured_at: snapshot.captured_at,
            fixture: snapshot.clone(),
            session: snapshot.session.clone().unwrap_or_else(|| SessionInfo {
                id: "sample".into(),
                title: "Refine the shared workspace components".into(),
                worktree_available: true,
                ..Default::default()
            }),
            messages,
            transcript,
            markdown: HashMap::new(),
            expanded: HashSet::new(),
            input,
            chat_focus: cx.focus_handle(),
            landmarks,
            prompt_rail: ListState::new(0, ListAlignment::Top, window.rem_size()),
            prompt_rail_active_id: None,
            outline_list: ListState::new(0, ListAlignment::Top, window.rem_size() * 20.0),
            outline_open: false, outline_focus: cx.focus_handle(), outline_focus_id: None, outline_selected_id: None,
            prompt_recall: None, segment_cache: HashMap::new(), code_wrap_blocks: HashMap::new(), copied_message: None, copy_feedback_task: None, armed_quotes: std::cell::RefCell::new(HashMap::new()),
            find_input, find_open: false, find_query: String::new(), find_results: Vec::new(),
            find_selected: None, find_previous_focus: None, _find_subscription: find_subscription,
            split: cx.new(|_| ResizableState::default()),
            sidebar_collapsed: false,
            preferred_panel_sizes: [16.5, 22.0, 14.0],
            panel_layout: None,
            gallery: cx.new(|cx| crate::gallery::Gallery::new(snapshot.clone(), window, cx)),
            editor: cx.new(|cx| crate::editor::EditorPreview::new(window, cx)),
            editor_open: false,
            terminal, terminal_open: false,
            agents, agents_open: false,
            right_split: cx.new(|_| ResizableState::default()),
            _agents_subscription: agents_subscription,
            _review_excerpt_subscription: review_excerpt_subscription,
            bottom_split: cx.new(|_| ResizableState::default()),
            _terminal_subscription: terminal_subscription,
            automations,
            automations_open: false,
            github,
            github_open: None,
            _github_subscription: github_subscription,
            settings,
            settings_open: false,
            _settings_subscription: settings_subscription,
            gallery_open: false,
            context_meter_open: false,
            palette_open: false, palette_search: false, palette_sessions: false, palette_previous_focus: None,
            palette_matches: Vec::new(), palette_recent: Vec::new(), command_state, _command_subscription: command_subscription,
            project_filtered: false,
            sidebar_pinned: false,
            sidebar_snooze: None,
            sidebar_removed: false,
            sidebar_snoozed_collapsed: false,
            model_picker,
            model_picker_open: Default::default(),
            _automation_subscription: automation_subscription,
            _model_subscription: model_subscription,
            _input_subscription: subscription,
            _input_observer: input_observer,
            selected_file_index: 0,
            dismiss_file_menu: false,
            file_scroll_handle: ScrollHandle::new(),
        }
    }

    fn render_header(&self, left_padding: Pixels, cx: &mut Context<Self>) -> Div {
        kit::chat_header_surface(left_padding, cx)
            .child(kit::chat_header_identity(
                self.session.title.clone(),
                self.session.title.clone(),
                None,
                cx,
            ))
            .when(!self.editor_open, |el| el.child(kit::conversation_find_button().on_click(
                cx.listener(|host, _, window, cx| host.open_conversation_find(&kit::FindInConversation, window, cx)))))
            .child(
                kit::chat_tab_button("central-tab-chat", "Chat", !self.editor_open, cx)
                    .tooltip("Chat (⌘1)")
                    .on_click(cx.listener(|host, _, window, cx| {
                        host.editor_open = false;
                        host.automations_open = false;
                        host.github_open = None;
                        host.input.update(cx, |input, cx| input.focus(window, cx));
                        cx.notify();
                    })),
            )
            .child(
                kit::chat_tab_button("central-tab-editor", "Editor", self.editor_open, cx)
                    .tooltip("Editor (⌘3)")
                    .on_click(cx.listener(|host, _, window, cx| {
                        host.editor_open = true;
                        host.clear_conversation_find();
                        host.clear_prompt_navigation();
                        window.focus(&host.chat_focus, cx);
                        host.automations_open = false;
                        host.github_open = None;
                        cx.notify();
                    })),
            )
    }
    fn preview_notice(title: &str, window: &mut Window, cx: &mut App) {
        let title = title.to_owned();
        window.open_alert_dialog(cx, move |dialog, _, _| dialog.title(title.clone())
            .description("This saved-session preview keeps all interaction local. This screen is still being extracted into the shared toolkit."));
    }

    fn render_tool(
        &self,
        tool: &ToolActivityInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let motion = kit::DisclosureMotion::new(
            SharedString::from(format!("tool-motion-{}", tool.id)),
            tool.is_expanded,
            window,
            cx,
        );
        let detail = motion
            .is_visible()
            .then(|| {
                let args = kit::tool_detail::args_json(&tool.arguments).unwrap_or_default();
                let path = threadlane_protocol::tool::read_file_snapshot_path(&tool.detail)
                    .or_else(|| kit::tool_detail::args_path(&args))
                    .unwrap_or_else(|| ".".into());
                kit::tool_preview::render(
                    tool,
                    path.clone(),
                    std::path::PathBuf::from(&path),
                    |id, path, line, folder| {
                        kit::tool_preview::open_button(id, &path, line, folder, false, |_, _, _| {})
                    },
                    cx,
                )
                .or_else(|| {
                    kit::tool_detail::render_activity_detail_card(
                        tool,
                        None::<fn(String, &mut App)>,
                        cx,
                    )
                })
                .unwrap_or_else(|| {
                    kit::result_surface(&cx.theme().colors)
                        .child(
                            kit::result_header(&cx.theme().colors)
                                .text_xs()
                                .child("Output"),
                        )
                        .child(
                            kit::result_viewport(format!("preview-output-{}", tool.id)).child(
                                kit::result_scroll_body(format!("preview-output-scroll-{}", tool.id), div()
                                    .p_3()
                                    .text_xs()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .child(tool.detail.clone())),
                            ),
                        )
                        .into_any_element()
                })
            })
            .map(|body| motion.content(body));
        let id = tool.id.clone();
        let entity = cx.entity().downgrade();
        kit::tool_activity(
            tool,
            !tool.detail.is_empty() || kit::tool_detail::expandable(tool),
            detail,
            false,
            move |_, cx| {
                let _ = entity.update(cx, |this, cx| {
                    for message in Arc::make_mut(&mut this.messages) {
                        if let Some(tool) = message
                            .tool_activities
                            .iter_mut()
                            .find(|tool| tool.id == id)
                        {
                            tool.is_expanded = !tool.is_expanded;
                            break;
                        }
                    }
                    this.transcript.list.remeasure();
                    cx.notify();
                });
            },
            cx,
        )
    }

    fn render_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.transcript.rows.get(index).cloned() else {
            return Empty.into_any_element();
        };
        let selected_prompt = matches!(&row, TranscriptRow::Message(index) if self.outline_selected_id.as_ref() == Some(&self.messages[*index].id));
        let selected_match = self.find_open && matches!(&row,
            TranscriptRow::Message(index) if self.find_selected.as_ref() == Some(&self.messages[*index].id));
        let content = match row {
            TranscriptRow::Message(index) => {
                let message = self.messages[index].clone();
                let (body, content_states) = if message.role == MessageRole::Assistant {
                    self.render_message_markdown(&message, cx)
                } else {
                    let markdown = kit::markdown::markdown_state(&mut self.markdown, "saved-session-preview".into(), message.id.clone(), &message.content, cx);
                    (kit::markdown::markdown_view(&markdown, |_, _| {}).into_any_element(), Vec::new())
                };
                match message.role {
                    MessageRole::User => kit::message_row(MessageRole::User)
                        .child(kit::user_message_bubble(cx).child(body).context_menu(self.message_context_menu(&message, None, cx)))
                        .children((!message.content.is_empty()).then(|| self.render_message_actions(&message, true, None, cx)))
                        .into_any_element(),
                    MessageRole::Assistant => {
                        let reasoning = message.reasoning_content.as_ref().map(|text| {
                            let motion = kit::DisclosureMotion::new(
                                SharedString::from(format!("reasoning-motion-{}", message.id)),
                                message.reasoning_expanded,
                                window,
                                cx,
                            );
                            let detail = motion
                                .is_visible()
                                .then(|| {
                                    kit::reasoning_detail(cx)
                                        .child(text.clone())
                                        .into_any_element()
                                })
                                .map(|body| motion.content(body));
                            let id = message.id.clone();
                            let entity = cx.entity().downgrade();
                            kit::reasoning_card(
                                &message,
                                detail,
                                false,
                                move |_, cx| {
                                    let _ = entity.update(cx, |this, cx| {
                                        if let Some(message) = Arc::make_mut(&mut this.messages)
                                            .iter_mut()
                                            .find(|message| message.id == id)
                                        {
                                            message.reasoning_expanded =
                                                !message.reasoning_expanded;
                                        }
                                        this.transcript.list.remeasure();
                                        cx.notify();
                                    });
                                },
                                cx,
                            )
                        });
                        let tools = message
                            .tool_activities
                            .iter()
                            .filter(|tool| tool.title != "update_plan")
                            .map(|tool| self.render_tool(tool, window, cx))
                            .collect::<Vec<_>>();
                        let quote = (!message.streaming && !message.content.is_empty())
                            .then(|| Self::quote_control(&self.armed_quotes, &message.id, &content_states, message.streaming, window, cx));
                        kit::message_row(MessageRole::Assistant)
                            .child(
                                kit::assistant_message_content()
                                    .children(reasoning)
                                    .children(
                                        (!message.content.is_empty())
                                            .then(|| body),
                                    )
                                    .children(tools)
                                    .children(quote.clone().map(|quote| self.render_message_actions(&message, false, Some(quote), cx)))
                                    .context_menu(self.message_context_menu(&message, Some(content_states), cx)),
                            )
                            .into_any_element()
                    }
                    MessageRole::Error => div()
                        .w_full()
                        .my_2()
                        .px_4()
                        .child(kit::chat_error_card(message.content.clone(), cx))
                        .into_any_element(),
                    _ => div()
                        .w_full()
                        .flex()
                        .justify_center()
                        .my_2()
                        .px_4()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(message.content)
                        .into_any_element(),
                }
            }
            TranscriptRow::Activities(range) => {
                let key = format!("activity-group-{}", self.messages[range.start].id);
                let tools = self.messages[range]
                    .iter()
                    .flat_map(|message| &message.tool_activities)
                    .filter(|tool| tool.title != "update_plan")
                    .cloned()
                    .collect::<Vec<_>>();
                let expanded = self.expanded.contains(&key);
                let motion = kit::DisclosureMotion::new(
                    SharedString::from(key.clone()),
                    expanded,
                    window,
                    cx,
                );
                let theme = cx.theme().colors;
                let entity = cx.entity().downgrade();
                kit::completed_activity_group(
                    &key.clone(),
                    expanded,
                    tools.iter(),
                    &motion,
                    |tool| self.render_tool(tool, window, cx),
                    move |_, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            if !this.expanded.remove(&key) {
                                this.expanded.insert(key.clone());
                            }
                            this.transcript.list.remeasure();
                            cx.notify();
                        });
                    },
                    &theme,
                )
            }
            TranscriptRow::Working => Empty.into_any_element(),
        };
        kit::conversation_transcript_row(if selected_prompt { Some("Selected prompt") } else { selected_match.then_some("Selected matching message") }, cx)
            .child(content)
            .into_any_element()
    }

    pub(super) fn environment_action(action: kit::EnvironmentAction, window: &mut Window, cx: &mut App) {
        Self::preview_notice(
            match action {
                kit::EnvironmentAction::Branches => "Branches",
                kit::EnvironmentAction::Review => "Workspace changes",
                kit::EnvironmentAction::Commit => "Commit",
                kit::EnvironmentAction::Pull => "Pull",
                kit::EnvironmentAction::Push => "Push",
                kit::EnvironmentAction::CreatePullRequest => "Create draft pull request",
                kit::EnvironmentAction::CreateBranch => "Create branch",
                kit::EnvironmentAction::Repository => "Repository",
                kit::EnvironmentAction::Files => "Files",
                kit::EnvironmentAction::Terminal => "Terminal",
            },
            window,
            cx,
        )
    }

    fn sidebar_action(
        &mut self,
        action: kit::SidebarSessionAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use kit::SidebarSessionAction as Action;
        match action {
            Action::Open => {
                self.gallery_open = false;
                self.automations_open = false;
                self.github_open = None;
                self.settings_open = false;
                self.editor_open = false;
            }
            Action::TogglePin => self.sidebar_pinned = !self.sidebar_pinned,
            Action::Snooze(secs) => {
                self.sidebar_snooze = self
                    .fixture
                    .sidebar_snooze_choices
                    .iter()
                    .find(|(_, duration, _)| *duration == secs)
                    .map(|(_, _, until)| kit::SidebarSnoozeStatus::Snoozed(until.clone()))
            }
            Action::Unsnooze => self.sidebar_snooze = None,
            Action::OpenTerminal => self.terminal_open = true,
            Action::CopyId => {
                cx.write_to_clipboard(ClipboardItem::new_string(self.session.id.clone()))
            }
            Action::CopyProjectPath => cx.write_to_clipboard(ClipboardItem::new_string(
                self.session.work_dir.display().to_string(),
            )),
            Action::CopySessionFile => cx.write_to_clipboard(ClipboardItem::new_string(
                self.session.session_file.display().to_string(),
            )),
            Action::RegenerateTitle => Self::preview_notice("Regenerate title", window, cx),
            Action::Fork => Self::preview_notice("Fork session", window, cx),
            Action::ExportLog => Self::preview_notice("Export session log", window, cx),
            Action::ExportTrajectory => Self::preview_notice("Export trajectory", window, cx),
            Action::Archive => {
                self.open_sidebar_removal(kit::SidebarSessionRemoval::Archive, window, cx)
            }
            Action::Remove => {
                self.open_sidebar_removal(kit::SidebarSessionRemoval::Remove, window, cx)
            }
            Action::RetrySnooze => Self::preview_notice("Retry saving snooze", window, cx),
        }
        cx.notify();
    }

    fn open_sidebar_removal(
        &mut self,
        kind: kit::SidebarSessionRemoval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self
            .session
            .work_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Project");
        let target = kit::SidebarSessionRemovalTarget::new(
            &self.session.id,
            kit::session_identity(&self.session).title,
            project,
        );
        let target = if self.session.is_worktree {
            target.worktree(self.session.git_branch.clone())
        } else {
            target
        };
        let deletion = std::rc::Rc::new(std::cell::Cell::new(true));
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let toggle = deletion.clone();
            let refresh = owner.clone();
            let confirmed = owner.clone();
            kit::sidebar_session_removal_dialog(
                alert,
                kind,
                &target,
                deletion.get(),
                move |checked, _, cx| {
                    toggle.set(checked);
                    let _ = refresh.update(cx, |_, cx| cx.notify());
                },
            )
            .on_ok(move |_, _, cx| {
                let _ = confirmed.update(cx, |host, cx| {
                    host.sidebar_removed = true;
                    cx.notify();
                });
                true
            })
        });
    }

    fn session_menu(
        owner: WeakEntity<Self>,
        menu: PopupMenu,
        scope: kit::SidebarSessionMenuScope,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let Some(entity) = owner.upgrade() else {
            return menu;
        };
        let host = entity.read(cx);
        let snooze = host
            .sidebar_snooze
            .clone()
            .map(kit::SidebarSnoozeMenu::Status)
            .unwrap_or_else(|| {
                if host.fixture.sidebar_snooze_choices.is_empty() {
                    kit::SidebarSnoozeMenu::Unavailable(
                        "Import return times to preview snoozing".into(),
                    )
                } else {
                    kit::SidebarSnoozeMenu::Available(
                        host.fixture
                            .sidebar_snooze_choices
                            .iter()
                            .map(|(label, secs, until)| {
                                kit::SidebarSnoozeChoice::new(label.clone(), *secs)
                                    .with_return_label(until.clone())
                            })
                            .collect(),
                    )
                }
            });
        let state = kit::SidebarSessionMenuState::new(snooze)
            .pinned(host.sidebar_pinned)
            .terminal_available(!host.session.is_worktree || host.session.worktree_available);
        kit::sidebar_session_menu(
            menu,
            state,
            scope,
            move |action, window, cx| {
                let _ = owner.update(cx, |host, cx| {
                    host.sidebar_action(action, window, cx);
                    cx.notify();
                });
            },
            window,
            cx,
        )
    }

    fn render_sidebar(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let project = self
            .session
            .work_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Threadlane")
            .to_owned();
        let owner = cx.entity().downgrade();
        let navigation = kit::sidebar_navigation(
            self.github_open
                .map(|pull_requests| {
                    if pull_requests {
                        kit::SidebarDestination::PullRequests
                    } else {
                        kit::SidebarDestination::Issues
                    }
                })
                .or_else(|| {
                    self.automations_open
                        .then_some(kit::SidebarDestination::Automations)
                }),
            self.automations.read(cx).attention_count(),
            0,
            move |destination, _, cx| {
                let _ = owner.update(cx, |this, cx| {
                    this.gallery_open = false;
                    this.clear_conversation_find(); this.clear_prompt_navigation();
                    this.automations_open = destination == kit::SidebarDestination::Automations;
                    this.github_open = match destination {
                        kit::SidebarDestination::Automations => None,
                        kit::SidebarDestination::Issues => Some(false),
                        kit::SidebarDestination::PullRequests => Some(true),
                    };
                    if let Some(pull_requests) = this.github_open {
                        this.github
                            .update(cx, |view, cx| view.select_kind(pull_requests, cx));
                    }
                    cx.notify();
                });
            },
            cx,
        );
        let selected = !self.gallery_open
            && !self.automations_open
            && self.github_open.is_none()
            && !self.settings_open;
        let card_owner = cx.entity().downgrade();
        let quick_owner = card_owner.clone();
        let full_owner = card_owner.clone();
        let card = kit::sidebar_session_card(
            &self.session,
            kit::SidebarSessionCardState {
                show_project: true,
                project: project.clone(),
                attention: SessionAttention::Idle,
                selected,
                pinned: self.sidebar_pinned,
                unseen_result: false,
                snooze: self.sidebar_snooze.clone(),
                git_status: self.fixture.git_status.clone(),
                pr: self
                    .fixture
                    .git_status
                    .as_ref()
                    .and_then(|status| status.pr.as_ref())
                    .filter(|pr| Some(pr.head_ref.as_str()) == self.session.git_branch.as_deref())
                    .cloned(),
                now: self.captured_at,
            },
            move |action, window, cx| {
                let _ = card_owner.update(cx, |host, cx| {
                    if action == kit::SidebarSessionAction::Archive && !host.session.is_worktree {
                        host.sidebar_removed = true;
                        cx.notify();
                    } else {
                        host.sidebar_action(action, window, cx);
                    }
                });
            },
            move |menu, window, cx| {
                Self::session_menu(
                    quick_owner.clone(),
                    menu,
                    kit::SidebarSessionMenuScope::Quick,
                    window,
                    cx,
                )
            },
            move |menu, window, cx| {
                Self::session_menu(
                    full_owner.clone(),
                    menu,
                    kit::SidebarSessionMenuScope::Full,
                    window,
                    cx,
                )
            },
            cx,
        );
        let owner = cx.entity().downgrade();
        let history = if self.sidebar_removed {
            kit::sidebar_history_empty(
                self.project_filtered,
                move |action, window, cx| match action {
                    kit::SidebarEmptyAction::ClearFilters => {
                        let _ = owner.update(cx, |host, cx| {
                            host.project_filtered = false;
                            cx.notify();
                        });
                    }
                    kit::SidebarEmptyAction::NewTask => {
                        Self::preview_notice("New task", window, cx)
                    }
                },
                cx,
            )
            .into_any_element()
        } else {
            let snoozed = self
                .sidebar_snooze
                .as_ref()
                .is_some_and(|status| !status.pending());
            let header = if snoozed {
                kit::sidebar_snoozed_header(
                    1,
                    self.sidebar_snoozed_collapsed,
                    true,
                    move |_, cx| {
                        let _ = owner.update(cx, |host, cx| {
                            host.sidebar_snoozed_collapsed = !host.sidebar_snoozed_collapsed;
                            cx.notify();
                        });
                    },
                    window,
                    cx,
                )
                .into_any_element()
            } else {
                kit::session_group_header(
                    kit::session_history_group(
                        self.sidebar_pinned,
                        SessionAttention::Idle,
                        self.session.updated_at,
                        self.captured_at,
                    ),
                    true,
                    window,
                    cx,
                )
                .into_any_element()
            };
            div()
                .child(header)
                .children(
                    (!snoozed || !self.sidebar_snoozed_collapsed)
                        .then(|| kit::session_history_card_row(card)),
                )
                .into_any_element()
        };

        kit::sidebar_surface(cx)
            .child(kit::sidebar_header(
                false,
                false,
                SessionAttention::Idle,
                navigation.into_any_element(),
                false,
                |window, cx| Self::preview_notice("New task", window, cx),
                cx,
            ))
            .child(kit::sidebar_project_filter(
                if self.project_filtered {
                    &project
                } else {
                    "All projects"
                },
                self.project_filtered,
                {
                    let selected = self.project_filtered.then(|| self.session.work_dir.clone());
                    let projects = vec![(project.clone(), self.session.work_dir.clone(), 1)];
                    let entity = cx.entity().downgrade();
                    move |menu, window, cx| {
                        kit::sidebar_project_menu(
                            menu,
                            &projects,
                            selected.as_deref(),
                            {
                                let entity = entity.clone();
                                move |project, _, cx| {
                                    let _ = entity.update(cx, |host, cx| {
                                        host.project_filtered = project.is_some();
                                        cx.notify();
                                    });
                                }
                            },
                            window,
                            cx,
                        )
                    }
                },
                kit::sidebar_attach_project_button()
                    .on_click(|_, window, cx| Self::preview_notice("Attach project", window, cx)),
                cx,
            ))
            .child(div().flex_1().min_h_0().child(history))
            .child(
                kit::sidebar_footer_surface(cx)
                    .child(
                        kit::sidebar_settings_button(self.settings_open, cx).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.settings_open = true;
                                this.clear_conversation_find(); this.clear_prompt_navigation();
                                cx.notify();
                            },
                        )),
                    )
                    .child(kit::sidebar_pairing_button(cx).on_click(|_, window, cx| {
                        Self::preview_notice("Share with mobile", window, cx)
                    })),
            )
    }


}

impl Render for SessionPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.transcript
            .sync(self.messages.clone(), false, false, self.find_open || self.outline_selected_id.is_some());
        let rem = window.rem_size();
        let viewport = window.viewport_size();
        let chat_page = !self.gallery_open
            && !self.settings_open
            && self.github_open.is_none()
            && !self.automations_open;
        let right_visible = self.agents_open && chat_page;
        let terminal_visible = self.terminal_open && chat_page;
        let sidebar_width = self
            .split
            .read(cx)
            .sizes()
            .first()
            .copied()
            .unwrap_or(rem * self.preferred_panel_sizes[0]);
        let layout = kit::WorkspaceLayout::new(
            viewport.width,
            rem,
            sidebar_width,
            self.sidebar_collapsed,
            right_visible,
        );
        let focus_back = right_visible && layout.right_panel_focus;
        self.agents.update(cx, |agents, cx| agents.set_focus_back(focus_back, cx));
        let show_environment = kit::environment_fits(layout.environment_width, rem);
        let visible_panels = [
            !self.settings_open && layout.sidebar_visible,
            right_visible && !layout.right_panel_focus,
            terminal_visible,
        ];
        let panel_layout = (viewport, rem, visible_panels);
        if self.panel_layout != Some(panel_layout) {
            self.panel_layout = Some(panel_layout);
            cx.on_next_frame(window, move |host, window, cx| {
                if host.panel_layout != Some(panel_layout) {
                    return;
                }
                kit::restore_workspace_panel_sizes(
                    [
                        (
                            visible_panels[0],
                            host.split.clone(),
                            0,
                            host.preferred_panel_sizes[0],
                        ),
                        (
                            visible_panels[1],
                            host.right_split.clone(),
                            1,
                            host.preferred_panel_sizes[1],
                        ),
                        (
                            visible_panels[2],
                            host.bottom_split.clone(),
                            1,
                            host.preferred_panel_sizes[2],
                        ),
                    ],
                    rem,
                    window,
                    cx,
                );
                cx.notify();
            });
        }
        let content = if self.gallery_open {
            self.gallery.clone().into_any_element()
        } else if self.github_open.is_some() {
            self.github.clone().into_any_element()
        } else if self.automations_open {
            self.automations.clone().into_any_element()
        } else if self.editor_open {
            kit::conversation_surface(cx)
                .role(Role::Group).track_focus(&self.chat_focus)
                .child(self.render_header(layout.header_inset, cx))
                .child(self.editor.clone())
                .into_any_element()
        } else {
            let header = self.render_header(layout.header_inset, cx);
            let context = self
                .fixture
                .context_window
                .as_ref()
                .map(kit::context_meter::ContextMeterContext::from);
            let metrics = &self.fixture.metrics;
            let meter = kit::context_meter::context_meter_view_model(
                context.as_ref(),
                &kit::context_meter::ContextMeterMetrics {
                    billed_input_tokens: metrics.billed_input_tokens(),
                    output_tokens: metrics.output_tokens,
                    cache_hit_percent: metrics.cache_hit_percent(),
                },
                self.fixture.reports_usage,
            );
            let meter_entity = cx.entity().downgrade();
            let context_meter = kit::context_meter::context_meter_popover(
                meter,
                self.context_meter_open,
                move |open, _, cx| {
                    let _ = meter_entity.update(cx, |this, cx| {
                        if this.context_meter_open != *open {
                            this.context_meter_open = *open;
                            cx.notify();
                        }
                    });
                },
                cx,
            );
            let project_name = self
                .session
                .work_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("No project")
                .to_owned();
            let work_mode = if !self.session.worktree_available {
                "Worktree unavailable"
            } else if self.session.is_worktree {
                "Worktree"
            } else {
                "Local"
            };
            let context_bar = kit::composer_context_bar()
                .child(
                    kit::composer_project_button(
                        project_name.clone(),
                        self.session.work_dir.display().to_string(),
                    )
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        kit::project_picker_menu(menu,
                            [kit::project_picker_item(project_name.clone(), true)],
                            kit::new_project_picker_item().on_click(|_, window, cx| {
                                Self::preview_notice("New project", window, cx)
                            }))
                    }),
                )
                .child(
                    kit::composer_work_mode_button(work_mode, self.session.is_worktree)
                        .on_click(|_, window, cx| Self::preview_notice("Work mode", window, cx)),
                )
                .children(self.fixture.active_skills_count.map(|count| {
                    kit::composer_skills_button(count)
                        .on_click(|_, window, cx| Self::preview_notice("Skills", window, cx))
                }))
                .children(
                    self.session
                        .git_branch
                        .clone()
                        .map(|branch| kit::composer_branch_label(branch, cx)),
                );
            let file_menu_open = self.file_menu_open(cx);
            let file_menu = self.render_file_completion(cx);
            let composer = kit::composer_container(cx)
                .child(context_bar)
                .child(
                    kit::composer_surface(
                        self.input.read(cx).focus_handle(cx).is_focused(window),
                        cx,
                    )
                    .when(self.prompt_recall.is_some() || self.input.read(cx).value().is_empty(), |el| el.key_context(kit::PROMPT_RECALL_KEY_CONTEXT)
                        .on_action(cx.listener(Self::recall_older_prompt_action))
                        .on_action(cx.listener(Self::recall_newer_prompt_action)))
                    .when(file_menu_open, |el| el.key_context(kit::file_completion::FILE_COMPLETION_KEY_CONTEXT)
                        .on_action(cx.listener(Self::complete_file_action))
                        .on_action(cx.listener(Self::previous_file_action))
                        .on_action(cx.listener(Self::next_file_action))
                        .on_action(cx.listener(Self::dismiss_file_action))
                        .on_key_down(cx.listener(|host, event: &KeyDownEvent, _, cx| {
                            if event.keystroke.key == "enter" && host.file_menu_open(cx) {
                                cx.stop_propagation();
                            }
                        })))
                    .children(file_menu)
                    .children(self.render_prompt_recall_strip(cx))
                    .child(
                        div()
                            .w_full()
                            .flex_1()
                            .min_h_6()
                            .child(kit::composer_input(&self.input)),
                    )
                    .child(
                        kit::composer_toolbar(cx)
                            .child(
                                kit::composer_picker_group()
                                    .child({
                                        if !self.model_picker_open.get() && !self.model_picker.read(cx).query(cx).is_empty() {
                                            self.model_picker.update(cx, |picker, cx| picker.set_query("", window, cx));
                                        }
                                        let open = self.model_picker_open.clone();
                                        kit::composer_model_picker(&self.model_picker,
                                            self.fixture.model.as_ref().map_or("Model unavailable", |model| model.label.as_str()),
                                            self.fixture.model.is_some(),
                                            self.fixture.model_provider_icon.clone().map(Into::into),
                                            move |is_open| open.set(is_open), cx)
                                    })
                                    .children(self.fixture.mode.map(|mode| {
                                        kit::composer_mode_button(mode.label(), true).on_click(
                                            |_, window, cx| {
                                                Self::preview_notice("Mode", window, cx)
                                            },
                                        )
                                    }))
                                    .children(
                                        self.fixture
                                            .effort
                                            .filter(|_| {
                                                self.fixture
                                                    .model
                                                    .as_ref()
                                                    .is_some_and(|model| !model.efforts.is_empty())
                                            })
                                            .map(|effort| {
                                                kit::composer_effort_button(effort)
                                                    .on_click(|_, window, cx| {
                                                        Self::preview_notice(
                                                            "Reasoning effort",
                                                            window,
                                                            cx,
                                                        )
                                                    })
                                            }),
                                    ),
                            )
                            .child(div().flex_1().min_w_2())
                            .child(kit::composer_actions_group().child(
                                kit::recall_prompt_button(self.recall_unavailable_reason(cx))
                                    .on_click(cx.listener(|host, _, window, cx| {
                                        if host.recall_unavailable_reason(cx).is_none() { host.step_prompt_recall(true, window, cx); }
                                    }))
                            ).child(context_meter).child(
                                kit::composer_send_button(false, "Saved session preview"),
                            )),
                    ),
                )
                .child(kit::composer_shortcuts(
                    "Enter to send · Shift+Enter for a new line",
                    cx,
                ));
            let rail = self.render_prompt_rail(cx);
            let list = kit::transcript_list(&self.transcript, cx.processor(Self::render_row));
            let viewport = kit::conversation_transcript_viewport(rail, list, &self.transcript.list, show_environment,
                cx.listener(|host, _, _, cx| { host.outline_selected_id = None; host.transcript.list.scroll_to_end(); cx.notify(); }));
            kit::conversation_surface(cx)
                .role(Role::Group).track_focus(&self.chat_focus)
                .key_context(if self.find_open { "Conversation ConversationFindActive" } else { "Conversation" })
                .on_action(cx.listener(Self::open_conversation_find))
                .on_action(cx.listener(Self::close_conversation_find))
                .on_action(cx.listener(Self::next_conversation_match))
                .on_action(cx.listener(Self::previous_conversation_match))
                .child(header)
                .children(self.find_open.then(|| self.render_conversation_find(layout.header_inset, cx)))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .justify_center()
                        .child(
                            kit::conversation_column(show_environment)
                                .children(self.fixture.run_timing.as_ref().and_then(|timing| timing.elapsed_seconds(self.captured_at.saturating_mul(1000), false)).map(|seconds| kit::last_run_duration(seconds, cx)))
                                .child(viewport)
                                .children(kit::plan_tracker(
                                    &self.session.id,
                                    &self.fixture.plan,
                                    false,
                                    cx,
                                ))
                                .child(composer),
                        )
                        .children(show_environment.then(|| {
                            let owner = cx.entity().downgrade();
                            saved_environment(&self.fixture, move |action, window, cx| {
                                if action == kit::EnvironmentAction::Terminal {
                                    let _ = owner.update(cx, |host, cx| { host.terminal_open = !host.terminal_open; cx.notify(); });
                                } else { Self::environment_action(action, window, cx); }
                            }, cx)
                        })),
                )
                .into_any_element()
        };
        let content = if right_visible {
            if layout.right_panel_focus {
                kit::workspace_right_panel_focus(self.agents.clone()).into_any_element()
            } else {
                kit::workspace_right_panel_split(
                    &self.right_split,
                    content,
                    self.agents.clone(),
                    rem,
                    viewport.width,
                )
                .on_resize(
                    cx.listener(|host, state: &Entity<ResizableState>, window, cx| {
                        if let Some(size) = state.read(cx).sizes().get(1) {
                            host.preferred_panel_sizes[1] = *size / window.rem_size();
                        }
                    }),
                )
                .into_any_element()
            }
        } else {
            content
        };
        let content = if terminal_visible {
            kit::workspace_terminal_split(
                &self.bottom_split,
                content,
                self.terminal.clone(),
                rem,
                viewport.height,
            )
            .on_resize(
                cx.listener(|host, state: &Entity<ResizableState>, window, cx| {
                    if let Some(size) = state.read(cx).sizes().get(1) {
                        host.preferred_panel_sizes[2] = *size / window.rem_size();
                    }
                }),
            )
            .into_any_element()
        } else {
            content
        };
        let split = if self.settings_open && !self.gallery_open {
            self.settings.clone().into_any_element()
        } else if layout.sidebar_visible {
            let sidebar = self.render_sidebar(window, cx);
            kit::workspace_sidebar_split(&self.split, sidebar, content, rem)
                .on_resize(
                    cx.listener(|host, state: &Entity<ResizableState>, window, cx| {
                        if let Some(size) = state.read(cx).sizes().first() {
                            host.preferred_panel_sizes[0] = *size / window.rem_size();
                        }
                    }),
                )
                .into_any_element()
        } else {
            content
        };
        let compact_status = viewport.width < rem * 45.0;
        let status = gpui_component::status_bar::StatusBar::new()
            .left(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(if compact_status { "Local preview" } else if self.settings_open && !self.gallery_open { "Sample settings · Local preview · Preferences are not saved" } else if self.github_open.is_some() && !self.gallery_open { "Sample GitHub collection · Local preview · No GitHub requests" } else if self.automations_open && !self.gallery_open { "Sample automations · Local preview · No prompts run" } else if self.editor_open && !self.gallery_open { "Sample editor · Local preview" } else { "Saved session · Local preview" }),
            )
            .right(
                div().flex().items_center().gap_2()
                    .children(self.sidebar_removed.then(|| Button::new("preview-restore-session").label("Restore preview session").small().ghost().on_click(cx.listener(|host, _, _, cx| { host.sidebar_removed = false; cx.notify(); }))))
                    .children((self.editor_open && self.github_open.is_none() && !self.automations_open && !self.settings_open && !self.gallery_open).then(|| {
                        Button::new("preview-editor-reset").label("Reset editor samples").small().ghost()
                            .on_click(cx.listener(|host, _, window, cx| host.editor.update(cx, |editor, cx| editor.reset(window, cx))))
                    }))
                    .children((!self.gallery_open && !self.settings_open && self.github_open.is_none() && !self.automations_open).then(|| {
                        Button::new("preview-agents-toggle").debug_selector(|| "preview-agents-toggle".into())
                            .label("Agents").small().ghost().selected(self.agents_open)
                            .on_click(cx.listener(|host, _, _, cx| { host.agents_open = !host.agents_open; cx.notify(); }))
                    }))
                    .children((!self.gallery_open && !self.settings_open && self.github_open.is_none() && !self.automations_open).then(|| {
                        Button::new("preview-terminal-toggle").debug_selector(|| "preview-terminal-toggle".into())
                            .label(if compact_status { "Terminal" } else { "Terminal sample" }).tooltip("Toggle local terminal sample").accessibility_label("Toggle local terminal sample").small().ghost()
                            .on_click(cx.listener(|host, _, _, cx| { host.terminal_open = !host.terminal_open; cx.notify(); }))
                    }))
                    .child(Button::new("preview-command-palette").debug_selector(|| "preview-command-palette".into()).icon(gpui_component::IconName::Search)
                        .small().ghost().selected(self.palette_open).tooltip("Command palette (Cmd+K)")
                        .accessibility_label("Command palette (Cmd+K)")
                        .on_click(cx.listener(|host,_,window,cx| host.toggle_palette(window,cx))))
                    .child(Button::new("preview-component-mode").debug_selector(|| "preview-component-mode".into())
                    .label(if self.gallery_open {
                        "Workspace"
                    } else {
                        "Components"
                    })
                    .small()
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.gallery_open = !this.gallery_open;
                        this.clear_conversation_find(); this.clear_prompt_navigation();
                        cx.notify();
                    }))),
            );
        kit::workspace_with_status(split, status).relative()
            .children((!self.settings_open && !self.gallery_open).then(|| kit::workspace_sidebar_toggle(self.sidebar_collapsed, layout.sidebar_available)
                .on_click(cx.listener(|host, _, _, cx| { host.sidebar_collapsed = !host.sidebar_collapsed; cx.notify(); }))))
            .on_action(cx.listener(|host, _: &palette::TogglePreviewPalette, window,cx| host.toggle_palette(window,cx)))
            .children(self.palette_open.then(|| self.render_palette(cx)))
    }
}

/// Both preview modes consume one immutable, imported fixture.
pub(super) fn saved_environment(fixture: &Arc<Snapshot>, on_action: impl Fn(kit::EnvironmentAction, &mut Window, &mut App) + 'static, cx: &App) -> AnyElement {
    let fallback = SessionInfo::default();
    let session = fixture.session.as_ref().unwrap_or(&fallback);
    let project = session
        .work_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("No project")
        .to_owned();
    let location = if !session.worktree_available {
        "Checkout unavailable"
    } else if session.is_worktree {
        "Worktree"
    } else {
        "Local"
    };
    let menu_fixture = fixture.clone();
    let on_action = std::rc::Rc::new(on_action);
    let menu_action = on_action.clone();
    kit::environment_panel(
        project,
        location,
        Some(session.runtime_work_dir.display().to_string()),
        fixture.git_status.as_ref(),
        session.worktree_available,
        move |menu, _, _| {
            // Mutating repository actions stay preview-local; no native service capability.
            kit::environment_git_menu(
                menu,
                menu_fixture.git_status.as_ref(),
                false,
                |label, action| {
                    let on_action = menu_action.clone();
                    gpui_component::menu::PopupMenuItem::new(label).on_click(
                        move |_, window, cx| on_action(action, window, cx),
                    )
                },
            )
        },
        kit::token_efficiency(fixture.token_efficiency.as_ref(), false, cx),
        move |action, window, cx| on_action(action, window, cx),
        cx,
    )
}

#[cfg(test)]
#[path = "session_layout_tests.rs"]
mod layout_tests;

#[cfg(test)]
#[path = "session_navigation_tests.rs"]
mod navigation_tests;

#[cfg(test)]
#[path = "file_completion_tests.rs"]
mod file_completion_tests;
