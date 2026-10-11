use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{EditorState, InputEvent, InputState, TabSize};
use gpui_component::menu::ContextMenuExt;
use gpui_component::notification::Notification;
use gpui_component::scroll::ScrollableElement;
use gpui_component::separator::Separator;
use gpui_component::spinner::Spinner;
use gpui_component::text::{TextView, TextViewState};
use gpui_component::tree::{TreeEvent, TreeItem, TreeState};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, WindowExt};
use threadlane_git::{can_create_pull_request, GitCommitInfo, GitFile, GitStatus};

#[cfg(test)]
use threadlane_git::GitBranchInfo;

use threadlane_project::watcher::WorkspaceWatcher;
use threadlane_ui_state::AppState;
use threadlane_ui_state::next_event_batch;
use threadlane_ui_kit::EditorSaveStatus;

#[path = "guarded_save.rs"]
mod guarded_save;
use guarded_save::DocumentOrigin;

use super::agents::AgentsPanel;
use super::browser::BrowserView;
use super::draft_pr::{DraftPrContextKey, DraftPrDialogView};
use super::types::discard_git_action;

pub use super::types::{
    can_publish_branch, detect_language, message_generated_matches_active_project,
    normalize_generated_commit_message, DiscardOption, FileNode,
    GitAction, PanelEvent, ReviewTab, ReviewViewMode, Surface,
};
use super::types::{available_surfaces, ReviewDiffRequest, ReviewDiffState, ReviewDiffTarget};

pub struct RightPanelView {
    pub(crate) model: Entity<AppState>,
    agents: Entity<AgentsPanel>,
    /// Live trajectory surface mounted from the workspace (owned by
    /// threadlane-ui-chat; injected type-erased to keep this crate chat-free).
    trajectory_view: Option<AnyView>,
    active_surface: Option<Surface>,
    visible: bool,
    project: Option<PathBuf>,
    worktree_unavailable: bool,
    /// Whether the attached daemon answered `supports_project_io` at the
    /// last sync. Part of the sync key: a remote client's handshake can
    /// still be in flight when the panel first syncs, and the capability
    /// flipping true later must re-run the watch arm instead of leaving
    /// the client-side `WorkspaceWatcher` in place.
    project_io_supported: bool,
    tree_state: Entity<TreeState>,
    expanded_paths: HashSet<String>,
    review_tab: ReviewTab,
    history_filter_input: Entity<InputState>,
    selected_commit_sha: Option<String>,
    selected_commit_files: Vec<GitFile>,
    loading_commit_sha: Option<String>,
    review_files: Vec<GitFile>,
    review_files_list_state: ListState,
    selected_files: HashSet<String>,
    review_selection_initialized: bool,
    review_filter_input: Entity<InputState>,
    review_view_mode: ReviewViewMode,
    stash_dialog_open: bool,
    stash_message_input: Entity<InputState>,
    stash_include_untracked: bool,
    collapsed_tree_folders: HashSet<String>,
    review_diff_revision: u64,
    review_diff_options: threadlane_git::DiffOptions,
    review_diff_request: Option<ReviewDiffRequest>,
    review_diff_state: Option<ReviewDiffState>,
    review_document: Entity<threadlane_ui_kit::ReviewDiffDocument>,
    review_session_id: Option<String>,
    /// Stable focus target for the adjacent-file navigation group: when the
    /// initiating control becomes disabled at a boundary, focus lands here
    /// instead of being dropped.
    review_nav_focus: FocusHandle,
    #[cfg(test)]
    review_diff_load_count: usize,
    git_status: Option<GitStatus>,
    draft_pr_context_revision: u64,
    review_error: Option<String>,
    commit_message_input: Entity<InputState>,
    generated_commit_message: Option<String>,
    should_clear_commit_message: bool,
    git_busy: bool,
    git_checkout_pending: bool,
    git_message_pending: bool,
    pub(crate) git_feedback: Option<String>,
    pending_git_notifications: Vec<Notification>,
    branch_popover_open: bool,
    branch_filter_input: Entity<InputState>,
    new_branch_dialog_open: bool,
    git_dialog_presented: bool,
    new_branch_name_input: Entity<InputState>,
    merge_dialog_open: bool,
    merge_filter_input: Entity<InputState>,
    merge_selected_branch: Option<String>,
    switch_dialog_open: bool,
    switch_target_branch: Option<String>,
    switch_stash_mode: bool,
    stash_expanded: bool,
    pr_expanded: bool,
    stash_files: Option<(usize, Vec<GitFile>)>,
    loading_stash_index: Option<usize>,
    last_fetched_time: Option<std::time::Instant>,
    document_title: Option<String>,
    document_state: Entity<TextViewState>,
    markdown_preview: threadlane_ui_kit::MarkdownPreview,
    editor_state: Option<Entity<EditorState>>,
    editor_subscription: Option<Subscription>,
    /// Re-renders when the open document buffer notifies; selection moves
    /// emit no `InputEvent`, but the add-selection control tracks them.
    editor_observe: Option<Subscription>,
    saved_content: String,
    saved_version: Option<String>,
    document_origin: Option<DocumentOrigin>,
    save_status: EditorSaveStatus,
    save_generation: u64,
    buffer_revision: u64,
    is_dirty: bool,
    pending_document: Option<(String, String)>,
    /// Monotonic id of the latest panel-file open request; async reads older
    /// than it are discarded so a slower earlier read can never reopen over a
    /// newer document (or its unsaved edits).
    panel_document_request: u64,
    document_loading: bool,
    document_error: Option<String>,
    browser: Option<Entity<BrowserView>>,
    event_tx: tokio::sync::mpsc::UnboundedSender<PanelEvent>,
    _watcher: Option<WorkspaceWatcher>,
    /// Project root currently held by a daemon-side `WatchProject`
    /// subscription; unwatched on switch. `None` when the attached daemon
    /// predates project-io (the local `WorkspaceWatcher` covers that
    /// single-host case) or when nothing is selected.
    watched_project: Option<PathBuf>,
    _subscriptions: Vec<Subscription>,
}

impl RightPanelView {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let agents = cx.new(|cx| AgentsPanel::new(model.clone(), window, cx));
        let document_state = cx.new(|cx| TextViewState::markdown("", cx));
        let review_document = cx.new(|cx| threadlane_ui_kit::ReviewDiffDocument::new(window, cx));
        let review_document_subscription = cx.subscribe(&review_document, |this, _, action, cx| {
            match action {
                threadlane_ui_kit::ReviewDiffAction::Retry => this.reload_review_diff(cx),
                threadlane_ui_kit::ReviewDiffAction::ShowWhitespace => this.set_ignore_whitespace(false, cx),
            }
        });
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let commit_message_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Summary (required)"));
        let branch_filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(threadlane_ui_kit::REVIEW_BRANCH_FILTER_PLACEHOLDER));
        let new_branch_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(threadlane_ui_kit::REVIEW_BRANCH_NAME_PLACEHOLDER));
        let merge_filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(threadlane_ui_kit::REVIEW_MERGE_FILTER_PLACEHOLDER));
        let history_filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter commits…"));
        let review_filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter changes…"));
        let stash_message_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(threadlane_ui_kit::REVIEW_STASH_MESSAGE_PLACEHOLDER));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

        cx.spawn(async move |this, cx| {
            while let Some(events) = next_event_batch(&mut event_rx).await {
                let _ = this.update(cx, |this, cx| {
                    for event in events {
                        this.apply_event(event, cx);
                    }
                    cx.notify();
                });
            }
        })
        .detach();

        // Daemon-side `WorkspaceChanged` events (emitted by its
        // `WatchProject` registry) reach this client's own subscription;
        // forwarding them as `PanelEvent`s gives the surfaces the same
        // refresh signal the local watcher used to send. A pre-3 daemon
        // emits none — the local watcher remains the source there.
        {
            let daemon_client = model.read(cx).daemon_client.clone();
            let mut daemon_events = daemon_client.subscribe();
            let workspace_tx = event_tx.clone();
            if let Ok(executor) = threadlane_ui_state::chat::executor() {
                executor.spawn(async move {
                    while let Some(event) = daemon_events.recv().await {
                        if let threadlane_protocol::daemon::SessionEvent::WorkspaceChanged {
                            work_dir,
                            git_dirty,
                            files_dirty,
                        } = event
                        {
                            let _ = workspace_tx.send(PanelEvent::WorkspaceChanged {
                                project: work_dir,
                                git_dirty,
                                files_dirty,
                            });
                        }
                    }
                });
            }
        }

        // Agent browser commands arrive from tokio tool workers, which cannot
        // touch entities directly. The first panel to construct claims the
        // shared receiver and pumps commands into the live browser view.
        // Script evaluations park here on a oneshot without blocking the UI;
        // the session side bounds every round-trip with its own timeout.
        if let Some(mut browser_rx) = model.read(cx).browser_bridge.take_receiver() {
            cx.spawn_in(window, async move |this, cx| {
                while let Some(request) = browser_rx.recv().await {
                    let step = this
                        .update_in(cx, |this, window, cx| {
                            start_browser_request(this, request.command, window, cx)
                        })
                        .unwrap_or_else(|_| {
                            BrowserReply::Ready(Err(
                                "The browser panel is no longer available.".to_string()
                            ))
                        });
                    let reply = match step {
                        BrowserReply::Ready(reply) => reply,
                        BrowserReply::PendingEval(rx) => rx
                            .await
                            .map_err(|_| "The browser dropped the evaluation.".to_string())
                            .map(|payload| finalize_browser_eval(&payload)),
                        BrowserReply::PendingSnapshot(rx) => match rx.await {
                            Ok(Ok((bytes, width, height))) => {
                                let (path, data_url) = this
                                    .update(cx, |this, _cx| this.save_browser_screenshot(&bytes))
                                    .unwrap_or_else(|_| (None, base64_data_url(&bytes)));
                                let payload = serde_json::json!({
                                    "width": width,
                                    "height": height,
                                    "data_url": data_url,
                                    "path": path,
                                })
                                .to_string();
                                Ok(payload)
                            }
                            Ok(Err(err)) => Err(err),
                            Err(_) => Err("The browser dropped the snapshot.".to_string()),
                        },
                        BrowserReply::PendingWait {
                            selector,
                            text,
                            deadline,
                        } => {
                            let mut outcome =
                                Err("Timed out waiting for condition in browser.".to_string());
                            while std::time::Instant::now() < deadline {
                                let check_script = super::browser::wait_check_js(
                                    selector.as_deref(),
                                    text.as_deref(),
                                );
                                let eval_rx = this.update(cx, |this, cx| {
                                    this.start_browser_eval(&check_script, cx)
                                });
                                match eval_rx {
                                    Ok(Ok(rx)) => {
                                        if let Ok(raw) = rx.await {
                                            let payload =
                                                super::browser::unwrap_callback_payload(&raw);
                                            if let Ok(v) =
                                                serde_json::from_str::<serde_json::Value>(&payload)
                                            {
                                                if v.get("ok").and_then(|b| b.as_bool())
                                                    == Some(true)
                                                {
                                                    let sel_found = v
                                                        .get("selectorFound")
                                                        .and_then(|b| b.as_bool());
                                                    let txt_found = v
                                                        .get("textFound")
                                                        .and_then(|b| b.as_bool());
                                                    let ready = v
                                                        .get("readyState")
                                                        .and_then(|s| s.as_str())
                                                        == Some("complete");

                                                    let sel_ok = selector.is_none()
                                                        || sel_found == Some(true);
                                                    let txt_ok =
                                                        text.is_none() || txt_found == Some(true);
                                                    let ready_ok = (selector.is_some()
                                                        || text.is_some())
                                                        || ready;

                                                    if sel_ok && txt_ok && ready_ok {
                                                        outcome =
                                                            Ok("Condition satisfied in browser."
                                                                .to_string());
                                                        break;
                                                    }
                                                } else if let Some(err) =
                                                    v.get("error").and_then(|s| s.as_str())
                                                {
                                                    outcome = Err(err.to_string());
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    _ => {
                                        outcome =
                                            Err("Browser panel closed during wait.".to_string());
                                        break;
                                    }
                                }
                                // The bridge pump runs on GPUI, outside Tokio's reactor.
                                cx.background_executor()
                                    .timer(Duration::from_millis(150))
                                    .await;
                            }
                            outcome
                        }
                    };
                    let _ = request.reply.send(reply);
                }
            })
            .detach();
        }

        let observe_model = cx.observe(&model, |this, _model, cx| {
            this.sync_project(cx);
            cx.notify();
        });
        let tree_subscription =
            cx.subscribe(
                &tree_state,
                |this, _tree, event: &TreeEvent, _cx| match event {
                    TreeEvent::Expanded(id) => {
                        this.expanded_paths.insert(id.to_string());
                    }
                    TreeEvent::Collapsed(id) => {
                        this.expanded_paths.remove(id.as_ref());
                    }
                },
            );
        // Eager so agent browser commands always have a live view to act on,
        // even before the user opens the tab. Hidden until selected.
        // GPUI test windows have no native handle for Wry; Git UI tests do not use the browser.
        let browser_model = model.clone();
        let browser = (!cfg!(test)).then(|| {
            let browser = cx.new(|cx| BrowserView::new(browser_model.clone(), window, cx));
            browser.update(cx, |browser, cx| browser.set_visible(false, cx));
            browser
        });

        let mut panel = Self {
            model,
            agents,
            trajectory_view: None,
            active_surface: None,
            visible: false,
            project: None,
            worktree_unavailable: false,
            project_io_supported: false,
            tree_state,
            expanded_paths: HashSet::new(),
            review_tab: ReviewTab::Changes,
            history_filter_input,
            selected_commit_sha: None,
            selected_commit_files: Vec::new(),
            loading_commit_sha: None,
            review_files: Vec::new(),
            review_files_list_state: ListState::new(
                0,
                ListAlignment::Top,
                window.rem_size() * 10.0,
            )
            .with_uniform_item_height(window.rem_size() * 2.0),
            selected_files: HashSet::new(),
            review_selection_initialized: false,
            review_filter_input,
            review_view_mode: ReviewViewMode::List,
            stash_dialog_open: false,
            stash_message_input,
            stash_include_untracked: true,
            collapsed_tree_folders: HashSet::new(),
            review_diff_revision: 0,
            review_diff_options: threadlane_git::DiffOptions::default(),
            review_diff_request: None,
            review_diff_state: None,
            review_document,
            review_session_id: None,
            review_nav_focus: cx.focus_handle().tab_stop(false),
            #[cfg(test)]
            review_diff_load_count: 0,
            git_status: None,
            draft_pr_context_revision: 0,
            review_error: None,
            commit_message_input,
            generated_commit_message: None,
            should_clear_commit_message: false,
            git_busy: false,
            git_checkout_pending: false,
            git_message_pending: false,
            git_feedback: None,
            pending_git_notifications: Vec::new(),
            branch_popover_open: false,
            branch_filter_input,
            new_branch_dialog_open: false,
            git_dialog_presented: false,
            new_branch_name_input,
            merge_dialog_open: false,
            merge_filter_input,
            merge_selected_branch: None,
            switch_dialog_open: false,
            switch_target_branch: None,
            switch_stash_mode: true,
            stash_expanded: false,
            pr_expanded: true,
            stash_files: None,
            loading_stash_index: None,
            last_fetched_time: None,
            document_title: None,
            document_state,
            markdown_preview: threadlane_ui_kit::MarkdownPreview::new(cx),
            editor_state: None,
            editor_subscription: None,
            editor_observe: None,
            saved_content: String::new(),
            saved_version: None,
            document_origin: None,
            save_status: EditorSaveStatus::Ready,
            save_generation: 0,
            buffer_revision: 0,
            is_dirty: false,
            pending_document: None,
            panel_document_request: 0,
            document_loading: false,
            document_error: None,
            browser,
            event_tx,
            _watcher: None,
            watched_project: None,
            _subscriptions: vec![observe_model, tree_subscription, review_document_subscription],
        };
        panel.sync_project(cx);
        panel
    }

    fn sync_project(&mut self, cx: &mut Context<Self>) {
        let session_id = self.model.read(cx).active_session_id.clone();
        if self.review_session_id != session_id {
            if self.review_diff_request.is_some() {
                self.close_document(cx);
            }
            self.review_session_id = session_id;
        }
        let (project, worktree_unavailable, project_io_supported) = {
            let state = self.model.read(cx);
            let project = state.active_git_work_dir();
            let unavailable = state.active_work_dir.is_some()
                && state.active_session_id.is_some()
                && project.is_none();
            (
                project,
                unavailable,
                state.daemon_client.supports_project_io(),
            )
        };
        if self.project == project
            && self.worktree_unavailable == worktree_unavailable
        {
            self.project_io_supported = project_io_supported;
            return;
        }
        self.close_document(cx);
        self.project = project.clone();
        self.worktree_unavailable = worktree_unavailable;
        self.project_io_supported = project_io_supported;
        self.draft_pr_context_revision = self.draft_pr_context_revision.wrapping_add(1);
        self.tree_state
            .update(cx, |state, cx| state.set_items(Vec::new(), cx));
        self.expanded_paths.clear();
        self.review_files.clear();
        self.selected_files.clear();
        self.review_selection_initialized = false;
        self.review_diff_revision = self.review_diff_revision.wrapping_add(1);
        self.pending_document = None;
        self.review_diff_options = threadlane_git::DiffOptions::default();
        self.review_diff_request = None;
        self.review_diff_state = None;
        self.git_checkout_pending = false;
        self.git_status = None;
        self.review_error = None;
        self.git_feedback = None;
        self.git_message_pending = false;
        self.generated_commit_message = None;
        self.should_clear_commit_message = false;
        self.selected_commit_sha = None;
        self.selected_commit_files.clear();
        self.loading_commit_sha = None;
        self.stash_files = None;
        self.loading_stash_index = None;
        self.stash_expanded = false;
        self.document_title = None;
        self.document_state
            .update(cx, |state, cx| state.set_text("", cx));

        let daemon_client = self.model.read(cx).daemon_client.clone();
        if let Some(work_dir) = project {
            if daemon_client.supports_project_io() {
                self._watcher = None;
                if self.watched_project.as_ref() != Some(&work_dir) {
                    if let Some(previous) = self.watched_project.take() {
                        let client = daemon_client.clone();
                        if let Ok(executor) = threadlane_ui_state::chat::executor() {
                            executor.spawn(async move {
                                let _ = threadlane_ui_state::project_io::unwatch_project(
                                    &client, &previous,
                                )
                                .await;
                            });
                        }
                    }
                    if let Ok(executor) = threadlane_ui_state::chat::executor() {
                        let proj = work_dir.clone();
                        let client = daemon_client.clone();
                        executor.spawn(async move {
                            let _ =
                                threadlane_ui_state::project_io::watch_project(&client, &proj)
                                    .await;
                        });
                    }
                    self.watched_project = Some(work_dir);
                }
            } else {
                // Pre-3 remote daemon: the only reachable filesystem is
                // this client's, so the local watcher stays.
                self.watched_project = None;
                let tx = self.event_tx.clone();
                let proj = work_dir.clone();
                self._watcher = WorkspaceWatcher::start(
                    work_dir,
                    Duration::from_millis(200),
                    move |change| {
                        let _ = tx.send(PanelEvent::WorkspaceChanged {
                            project: proj.clone(),
                            git_dirty: change.git_dirty,
                            files_dirty: change.files_dirty,
                        });
                    },
                )
                .ok();
            }
        } else {
            self._watcher = None;
            if let Some(previous) = self.watched_project.take() {
                if let Ok(executor) = threadlane_ui_state::chat::executor() {
                    let client = daemon_client.clone();
                    executor.spawn(async move {
                        let _ = threadlane_ui_state::project_io::unwatch_project(
                            &client, &previous,
                        )
                        .await;
                    });
                }
            }
        }

        self.refresh_active_surface(cx);
    }

    pub fn open_review(&mut self, cx: &mut Context<Self>) {
        self.open_surface(Surface::Review, cx);
    }

    pub fn open_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_review(cx);
        self.close_document(cx);
        self.review_tab = ReviewTab::Changes;
        self.commit_message_input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub fn open_branch_popover(&mut self, cx: &mut Context<Self>) {
        self.open_surface(Surface::Review, cx);
        self.branch_popover_open = true;
        cx.notify();
    }

    pub fn open_new_branch_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_surface(Surface::Review, cx);
        self.new_branch_dialog_open = true;
        self.branch_popover_open = false;
        // Synara ThreadWorktreeHandoffDialog pattern: autofocus the name
        // input and select existing text so typing replaces it.
        self.new_branch_name_input.update(cx, |input, cx| {
            let len = input.value().len();
            input.set_selected_range(0..len, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub fn open_merge_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_surface(Surface::Review, cx);
        self.merge_dialog_open = true;
        self.merge_selected_branch = None;
        self.branch_popover_open = false;
        self.merge_filter_input.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn replace_git_status(&mut self, status: Option<GitStatus>, cx: &mut Context<Self>) {
        let previous_branch = self
            .git_status
            .as_ref()
            .and_then(|status| status.branch.as_deref());
        let next_branch = status.as_ref().and_then(|status| status.branch.as_deref());
        if previous_branch != next_branch {
            self.draft_pr_context_revision = self.draft_pr_context_revision.wrapping_add(1);
            if self.git_status.is_some() && status.is_some() {
                self.review_diff_options = threadlane_git::DiffOptions::default();
                if self.review_diff_request.is_some() {
                    self.close_document(cx);
                }
            }
        }
        self.git_status = status;
    }

    pub(crate) fn draft_pr_checkout_key(&self) -> Option<DraftPrContextKey> {
        let project = self.project.clone()?;
        let status = self.git_status.as_ref()?;
        if self.worktree_unavailable || status.detached {
            return None;
        }
        let branch = status
            .branch
            .as_deref()
            .filter(|branch| !branch.trim().is_empty())?
            .to_string();
        Some(DraftPrContextKey {
            project,
            branch,
            revision: self.draft_pr_context_revision,
        })
    }

    pub(crate) fn draft_pr_creation_key(&self) -> Option<DraftPrContextKey> {
        can_create_pull_request(!self.worktree_unavailable, self.git_status.as_ref())
            .then(|| self.draft_pr_checkout_key())
            .flatten()
    }

    pub fn open_draft_pr_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_project(cx);
        if self.git_busy {
            return;
        }
        // The hidden panel may still hold status for an earlier branch in this checkout.
        let status = self
            .project
            .as_ref()
            .and_then(|project| self.model.read(cx).git_statuses.get(project).cloned());
        self.replace_git_status(status, cx);
        let Some(key) = self.draft_pr_creation_key() else {
            let message = "Publish this named branch and refresh pull request status before creating a draft.";
            self.git_feedback = Some(message.into());
            window.push_notification(Notification::warning(message), cx);
            cx.notify();
            return;
        };
        let Some(status) = self.git_status.as_ref() else {
            return;
        };
        let fields = threadlane_ui_kit::review_draft_pr_prefill(status);
        let panel = cx.entity().downgrade();
        let dialog_state =
            cx.new(|dialog_cx| DraftPrDialogView::new(panel, key, fields, window, dialog_cx));
        let content = dialog_state.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let submit_state = content.clone();
            let cancel_state = content.clone();
            threadlane_ui_kit::review_draft_pr_dialog(dialog)
                .child(content.clone())
                .on_ok(move |_, window, cx| {
                    submit_state.update(cx, |state, cx| state.start_request(false, window, cx));
                    false
                })
                .on_cancel(move |_, _, cx| {
                    let state = cancel_state.read(cx);
                    !state.attempts.is_busy()
                        || state.current_key(false, cx).as_ref() != Some(&state.key)
                })
        });
        let base_input = dialog_state.read(cx).base_input.clone();
        base_input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Mounts the workspace-owned trajectory surface. `AnyView` keeps this
    /// crate independent of threadlane-ui-chat while letting the same live
    /// view render inside the right panel.
    pub fn set_trajectory_view(&mut self, view: AnyView) {
        self.trajectory_view = Some(view);
    }

    pub fn open_surface(&mut self, surface: Surface, cx: &mut Context<Self>) {
        self.sync_project(cx);
        if self.active_surface != Some(surface) {
            self.close_document(cx);
        }
        self.active_surface = Some(surface);
        self.refresh_surface(surface, cx);
        self.sync_browser_visibility(cx);
        cx.notify();
    }

    fn refresh_active_surface(&mut self, cx: &mut Context<Self>) {
        if let Some(surface) = self.active_surface {
            self.refresh_surface(surface, cx);
        }
    }

    pub(crate) fn refresh_surface(&mut self, surface: Surface, cx: &mut Context<Self>) {
        if surface == Surface::Review {
            self.invalidate_review_diff(cx);
        }
        let Some(project) = self.project.clone() else {
            return;
        };
        let tx = self.event_tx.clone();
        let daemon_client = self.model.read(cx).daemon_client.clone();
        let Ok(executor) = threadlane_ui_state::chat::executor() else {
            if surface == Surface::Review {
                let _ = tx.send(PanelEvent::ReviewLoaded {
                    project,
                    status: None,
                    files: Vec::new(),
                    error: Some("The git runtime is unavailable.".into()),
                });
            }
            return;
        };
        executor.spawn(async move {
            match surface {
                Surface::Agents => {}
                Surface::Trajectory => {
                    // Renders live off AppState; nothing to fetch.
                }
                Surface::Files => {
                    let nodes = threadlane_ui_state::project_io::project_files(
                        &daemon_client,
                        &project,
                        500,
                    )
                    .await
                    .unwrap_or_default();
                    let _ = tx.send(PanelEvent::FilesLoaded { project, nodes });
                }
                Surface::Review => {
                    // Keep ahead/behind and PR checks current when the user
                    // refreshes Review; the daemon tolerates fetch failures so
                    // local status remains available offline.
                    let (status, files, error) = match threadlane_ui_state::project_io::inspect(
                        &daemon_client,
                        &project,
                        true,
                    )
                    .await
                    {
                        Ok(status) => {
                            let files = status.files.clone();
                            (Some(status), files, None)
                        }
                        Err(error) => (None, Vec::new(), Some(error)),
                    };
                    let _ = tx.send(PanelEvent::ReviewLoaded {
                        project,
                        status,
                        files,
                        error,
                    });
                }
                Surface::Browser => {
                    // The live webview needs no background refresh.
                }
            }
        });
    }

    fn open_file_diff(&mut self, path: String, cx: &mut Context<Self>) {
        let initial_open = self.review_diff_request.is_none();
        self.review_document.update(cx, |document, cx| document.reset(cx));
        self.open_review_diff(ReviewDiffTarget::File(path), cx);
        if initial_open {
            self.review_document
                .update(cx, |document, cx| document.request_focus(cx));
        }
    }

    fn open_combined_diff(&mut self, cx: &mut Context<Self>) {
        let initial_open = self.review_diff_request.is_none();
        self.review_document.update(cx, |document, cx| document.reset(cx));
        self.open_review_diff(ReviewDiffTarget::AllChanges, cx);
        if initial_open {
            self.review_document
                .update(cx, |document, cx| document.request_focus(cx));
        }
    }

    fn set_ignore_whitespace(&mut self, checked: bool, cx: &mut Context<Self>) {
        self.review_diff_options.ignore_whitespace = checked;
        self.reload_review_diff(cx);
    }

    fn reload_review_diff(&mut self, cx: &mut Context<Self>) {
        if self.git_checkout_pending {
            return;
        }
        if let Some(request) = &self.review_diff_request {
            self.open_review_diff(request.target.clone(), cx);
        }
    }

    fn invalidate_review_diff(&mut self, cx: &mut Context<Self>) {
        self.review_diff_revision = self.review_diff_revision.wrapping_add(1);
        if let Some(request) = &mut self.review_diff_request {
            request.revision = self.review_diff_revision;
            self.pending_document = None;
            self.review_diff_state = Some(ReviewDiffState::Loading);
            self.review_document.update(cx, |document, cx| document.loading(self.review_diff_options.ignore_whitespace, cx));
            self.document_state
                .update(cx, |state, cx| state.set_text("", cx));
            cx.notify();
        }
    }

    fn open_review_diff(&mut self, target: ReviewDiffTarget, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        if self.review_diff_request.as_ref().is_none_or(|request| request.target != target) {
            self.close_document(cx);
        }
        self.review_diff_revision = self.review_diff_revision.wrapping_add(1);
        let request = ReviewDiffRequest {
            project,
            target,
            options: self.review_diff_options,
            revision: self.review_diff_revision,
        };
        self.document_title = Some(request.target.title());
        self.review_diff_request = Some(request.clone());
        self.review_diff_state = Some(ReviewDiffState::Loading);
        self.review_document.update(cx, |document, cx| document.loading(self.review_diff_options.ignore_whitespace, cx));
        self.editor_state = None;
        self.editor_subscription = None;
        self.editor_observe = None;
        self.saved_content.clear();
        self.is_dirty = false;
        self.document_state
            .update(cx, |state, cx| state.set_text("", cx));
        if self.git_checkout_pending {
            cx.notify();
            return;
        }
        #[cfg(test)]
        {
            self.review_diff_load_count += 1;
        }
        let daemon_client = self.model.read(cx).daemon_client.clone();
        cx.spawn(async move |this, cx| {
            let background_request = request.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    match &background_request.target {
                        ReviewDiffTarget::File(path) => {
                            threadlane_ui_state::project_io::diff_file(
                                &daemon_client,
                                &background_request.project,
                                path.clone(),
                                background_request.options,
                            )
                            .await
                        }
                        ReviewDiffTarget::AllChanges => {
                            threadlane_ui_state::project_io::diff_worktree(
                                &daemon_client,
                                &background_request.project,
                                background_request.options,
                            )
                            .await
                        }
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.apply_review_diff_result(request, result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn close_document(&mut self, cx: &mut Context<Self>) {
        self.review_document.update(cx, |document, cx| document.reset(cx));
        self.saved_version = None;
        self.document_origin = None;
        self.save_status = EditorSaveStatus::Ready;
        self.save_generation = self.save_generation.wrapping_add(1);
        self.buffer_revision = self.buffer_revision.wrapping_add(1);
        self.panel_document_request = self.panel_document_request.wrapping_add(1);
        self.document_loading = false;
        self.document_error = None;
        self.review_diff_revision = self.review_diff_revision.wrapping_add(1);
        self.review_diff_request = None;
        self.review_diff_state = None;
        self.document_title = None;
        self.markdown_preview = threadlane_ui_kit::MarkdownPreview::new(cx);
        self.editor_state = None;
        self.editor_subscription = None;
        self.editor_observe = None;
        self.saved_content.clear();
        self.is_dirty = false;
        self.pending_document = None;
        self.document_state
            .update(cx, |state, cx| state.set_text("", cx));
        cx.notify();
    }

    fn apply_review_diff_result(
        &mut self,
        request: ReviewDiffRequest,
        result: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        let active_session_id = self.model.read(cx).active_session_id.clone();
        if self.git_checkout_pending
            || self.review_session_id != active_session_id
            || self.review_diff_request.as_ref() != Some(&request)
            || self.project.as_ref() != Some(&request.project)
            || self.model.read(cx).active_git_work_dir().as_ref() != Some(&request.project)
        {
            return;
        }
        match result {
            Ok(content) => {
                self.review_diff_state = Some(ReviewDiffState::Ready {
                    empty: content.is_empty(),
                });
                self.review_document.update(cx, |document, cx| document.set_patch(&content, cx));
            }
            Err(error) => {
                self.review_document.update(cx, |document, cx| document.failed(error.clone(), cx));
                self.review_diff_state = Some(ReviewDiffState::Failed(error));
            }
        }
        cx.notify();
    }

    /// Fulfills an `AppState::request_open_panel_file` request: reads the file
    /// on the background executor, then installs it via `pending_document` so
    /// `sync_pending_document` (which owns the `Window`) opens it as the
    /// editable document. Checkout or project drift before the read lands
    /// silently drops it; read failures publish `session_status`.
    fn start_panel_document_read(
        &mut self,
        project: PathBuf,
        relative_path: String,
        cx: &mut Context<Self>,
    ) {
        if self.is_dirty {
            self.model.update(cx, |state, _| {
                state.client.session_status =
                    Some("Save or close the modified document before opening another file.".into());
            });
            return;
        }
        self.close_document(cx);
        self.document_title = Some(relative_path.clone());
        self.document_loading = true;
        let request_id = self.panel_document_request;
        let read_client = self.model.read(cx).daemon_client.clone();
        let origin = DocumentOrigin::new(read_client.clone(), project.clone(), relative_path.clone());
        self.document_origin = Some(origin.clone());
        let read_project = project.clone();
        let read_path = relative_path.clone();
        let read = cx.background_executor().spawn(async move {
            if read_client.supports_guarded_saves() {
                threadlane_ui_state::project_io::read_file_versioned(&read_client, &read_project, read_path)
                    .await.map(|file| (file.content, Some(file.version))).map_err(|e| e.to_string())
            } else {
                threadlane_ui_state::project_io::read_file(&read_client, &read_project, read_path)
                    .await.map(|content| (content, None))
            }
        });
        cx.spawn(async move |this, cx| {
            let result = read.await;
            let _ = this.update(cx, |this, cx| {
                if this.panel_document_request != request_id
                    || this.project.as_ref() != Some(&project)
                    || this.model.read(cx).active_git_work_dir().as_ref() != Some(&project)
                {
                    return;
                }
                this.document_loading = false;
                if !origin.is_current(this, cx) {
                    this.document_error = Some("The daemon connection changed while loading. Reopen this file from the current checkout.".into());
                    cx.notify();
                    return;
                }
                match result {
                    Ok((content, version)) => {
                        this.saved_version = version;
                        this.save_status = if origin.client.supports_guarded_saves() { EditorSaveStatus::Ready } else { EditorSaveStatus::Unsupported };
                        this.pending_document = Some((relative_path, content));
                    }
                    Err(error) => {
                        this.document_error = Some(error.clone());
                        this.model.update(cx, |state, _| {
                            state.client.session_status = Some(error);
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn sync_pending_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((title, content)) = self.pending_document.take() else {
            return;
        };
        if self.document_origin.as_ref().is_some_and(|origin| !origin.is_current(self, cx)) {
            self.document_error = Some("The daemon connection changed while loading. Reopen this file from the current checkout.".into());
            return;
        }
        let origin = self.document_origin.take();
        let version = self.saved_version.take();
        let status = self.save_status.clone();
        self.close_document(cx);
        self.document_origin = origin;
        self.saved_version = version;
        self.save_status = status;
        self.document_title = Some(title.clone());
        self.saved_content = content.clone();
        self.is_dirty = false;

        if title.starts_with("Review ·") {
            self.editor_state = None;
            self.editor_subscription = None;
        self.editor_observe = None;
            let markdown = format!("```diff\n{}\n```", content.replace("```", "` ` `"));
            self.document_state
                .update(cx, |state, cx| state.set_text(&markdown, cx));
        } else {
            let lang = detect_language(&title);
            let editor = cx.new(|cx| {
                EditorState::new(window, cx)
                    .language(lang)
                    .line_number(true)
                    .folding(true)
                    .show_whitespaces(false)
                    .tab_size(TabSize {
                        tab_size: 4,
                        hard_tabs: false,
                    })
                    .default_value(&content)
            });
            let subscription = cx.subscribe(&editor, |this, editor, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.buffer_revision = this.buffer_revision.wrapping_add(1);
                    let current = editor.read(cx).value();
                    this.markdown_preview.refresh(current.clone(), cx);
                    let dirty = current.as_str() != this.saved_content.as_str();
                    if this.is_dirty != dirty {
                        this.is_dirty = dirty;
                        cx.notify();
                    }
                }
            });
            // Selection-only notifications must not materialize the source Rope.
            // The Change subscription owns preview refreshes.
            let observe = cx.observe(&editor, |_this, _editor, cx| cx.notify());
            self.editor_state = Some(editor);
            self.editor_subscription = Some(subscription);
            self.editor_observe = Some(observe);
        }
    }

    /// Whether the open document is an editable file buffer (eligible host
    /// for **Add selection to chat**); Review/diff documents and the plain
    /// text view are not.
    pub fn editable_file_open(&self) -> bool {
        self.editor_state.is_some()
    }

    /// Reason the **Add selection to chat** command is unavailable for the
    /// open document — `None` when ready. Review/diff documents, pending
    /// loads, files outside the active checkout, and empty or oversized
    /// selections each carry a textual reason.
    pub fn files_selection_block_reason(&self, cx: &App) -> Option<SharedString> {
        if self.markdown_preview.is_active() {
            return Some(threadlane_ui_kit::PREVIEW_SELECTION_REASON.into());
        }
        if self.document_title.is_none() {
            return Some("Open a file first".into());
        }
        if self.pending_document.is_some() {
            return Some("The file is still loading".into());
        }
        let Some(editor) = self.editor_state.as_ref() else {
            return Some("This document can't be added to chat".into());
        };
        match &self.project {
            Some(project)
                if self.model.read(cx).active_git_work_dir().as_ref() == Some(project) => {}
            _ => return Some("The file is not in the active checkout".into()),
        }
        threadlane_ui_kit::editor_excerpt_block_reason(
            threadlane_ui_kit::editor_selection_snapshot(editor.read(cx)).as_ref(),
        )
        .map(SharedString::from)
    }

    /// Activates **Add selection to chat** for the open document: validates
    /// the selection, captures buffer identity plus the composer
    /// destination, and emits `EditorSelectionRequest` for the workspace to
    /// append. On any guard failure the reason is shown and nothing is
    /// appended.
    pub fn request_add_selection_to_chat(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(reason) = self.files_selection_block_reason(cx) {
            window.push_notification(reason.to_string(), cx);
            return;
        }
        let (Some(editor), Some(project), Some(title)) = (
            self.editor_state.clone(),
            self.project.clone(),
            self.document_title.clone(),
        ) else {
            return;
        };
        let Some(snapshot) =
            threadlane_ui_kit::editor_selection_snapshot(editor.read(cx))
        else {
            return;
        };
        let destination = {
            let state = self.model.read(cx);
            (
                state.active_work_dir.clone(),
                state.active_session_id.clone(),
            )
        };
        cx.emit(threadlane_ui_kit::EditorSelectionRequest {
            editor,
            checkout: project,
            relative_path: title,
            dirty: self.is_dirty,
            snapshot,
            destination,
        });
    }

    /// Whether `request` still names the open document's buffer and the
    /// buffer's live selection is the captured one.
    pub fn selection_request_is_current(
        &self,
        request: &threadlane_ui_kit::EditorSelectionRequest,
        cx: &App,
    ) -> bool {
        if self.markdown_preview.is_active()
            || self.pending_document.is_some()
            || self.document_title.as_ref() != Some(&request.relative_path)
            || self.project.as_ref() != Some(&request.checkout)
        {
            return false;
        }
        let Some(editor) = self.editor_state.as_ref() else {
            return false;
        };
        editor.entity_id() == request.editor.entity_id()
            && editor.read(cx).selected_range() == request.snapshot.byte_range
    }

    fn save_active_document(&mut self, cx: &mut Context<Self>) {
        self.save_guarded_document(cx);
    }

    fn apply_event(&mut self, event: PanelEvent, cx: &mut Context<Self>) {
        match event {
            PanelEvent::WorkspaceChanged {
                project,
                git_dirty,
                files_dirty,
            } if self.project.as_ref() == Some(&project) => {
                if git_dirty {
                    self.refresh_surface(Surface::Review, cx);
                }
                if files_dirty {
                    self.refresh_surface(Surface::Files, cx);
                }
            }
            PanelEvent::FilesLoaded { project, nodes }
                if self.project.as_ref() == Some(&project) =>
            {
                let expanded_paths = &self.expanded_paths;
                let items = nodes
                    .into_iter()
                    .map(|node| convert_node_to_tree_item(node, expanded_paths))
                    .collect::<Vec<_>>();
                self.tree_state
                    .update(cx, |state, cx| state.set_items(items, cx));
            }
            PanelEvent::ReviewLoaded {
                project,
                status,
                files,
                error,
            } if self.project.as_ref() == Some(&project) => {
                if let Some(status_ref) = &status {
                    self.model.update(cx, |state, cx| {
                        state
                            .git_statuses
                            .insert(project.clone(), status_ref.clone());
                        cx.notify();
                    });
                }
                self.replace_git_status(status, cx);
                let current_set: HashSet<String> = files.iter().map(|f| f.path.clone()).collect();
                retain_review_selection(
                    &mut self.selected_files,
                    current_set,
                    &mut self.review_selection_initialized,
                );
                self.review_files = files;
                self.review_error = error;
                self.reload_review_diff(cx);
                self.stash_files = None;
                self.loading_stash_index = None;
            }
            PanelEvent::MessageGenerated {
                project,
                result,
                diff_truncated,
            } if message_generated_matches_active_project(
                &project,
                self.model.read(cx).active_git_work_dir().as_deref(),
            ) =>
            {
                self.git_message_pending = false;
                match result {
                    Ok(message) => {
                        self.generated_commit_message = Some(message);
                        // Synara DiffTruncationWarning pattern: never present a
                        // truncated diff as complete.
                        self.git_feedback = diff_truncated.then(|| {
                            "Partial diff — message generated from the first 24,000 characters."
                                .into()
                        });
                        self.pending_git_notifications
                            .push(Notification::success("Commit message generated."));
                    }
                    Err(error) => {
                        self.git_feedback = Some(error.clone());
                        self.pending_git_notifications
                            .push(Notification::error(error));
                    }
                }
            }
            PanelEvent::ActionFinished {
                project,
                status,
                action_error,
                action_message,
                checkout_succeeded,
            } => {
                self.git_checkout_pending = false;
                if self.project.as_ref() != Some(&project) {
                    // The action is complete, but its project is no longer active.
                    // Release the guard without applying stale status to the new project.
                    self.git_busy = false;
                    return;
                }
                self.git_busy = false;
                if checkout_succeeded {
                    self.review_diff_options = threadlane_git::DiffOptions::default();
                    if self.review_diff_request.is_some() {
                        self.close_document(cx);
                    }
                }
                match status {
                    Ok(status) => {
                        self.model.update(cx, |state, cx| {
                            state.git_statuses.insert(project, status.clone());
                            cx.notify();
                        });
                        self.replace_git_status(Some(status.clone()), cx);
                        self.reload_review_diff(cx);
                        self.stash_files = None;
                        self.loading_stash_index = None;
                        self.selected_files = status.files.iter().map(|f| f.path.clone()).collect();
                        self.review_files = status.files;
                        self.review_error = None;
                        self.should_clear_commit_message = true;
                        self.branch_popover_open = false;
                        self.new_branch_dialog_open = false;
                        self.merge_dialog_open = false;
                        self.switch_dialog_open = false;
                        self.switch_target_branch = None;
                        self.last_fetched_time = Some(std::time::Instant::now());
                        let action_failed = action_error.is_some();
                        let message = action_error
                            .or(action_message)
                            .unwrap_or_else(|| "Git action completed successfully.".into());
                        self.git_feedback = Some(message.clone());
                        self.pending_git_notifications.push(if action_failed {
                            Notification::error(message)
                        } else {
                            Notification::success(message)
                        });
                    }
                    Err(status_error) => {
                        self.review_error = Some(status_error.clone());
                        self.reload_review_diff(cx);
                        let message = action_error.unwrap_or(status_error);
                        self.git_feedback = Some(message.clone());
                        self.pending_git_notifications
                            .push(Notification::error(message));
                    }
                }
            }
            PanelEvent::CommitFilesLoaded { sha, files } => {
                if self.loading_commit_sha.as_deref() == Some(&sha) {
                    self.loading_commit_sha = None;
                    self.selected_commit_sha = Some(sha);
                    self.selected_commit_files = files;
                }
            }
            PanelEvent::StashFilesLoaded {
                project,
                index,
                files,
            } if self.project.as_ref() == Some(&project) => {
                if self.loading_stash_index == Some(index) {
                    self.loading_stash_index = None;
                    self.stash_files = Some((index, files));
                }
            }
            _ => {}
        }
    }

    pub fn restore_current_stash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self
            .git_status
            .as_ref()
            .and_then(|status| status.current_stash.as_ref())
            .map(|stash| stash.index)
        else {
            self.git_feedback = Some("No stash found for the current branch.".into());
            cx.notify();
            return;
        };
        self.run_git_action(GitAction::PopStash(Some(index)), window, cx);
    }

    fn generate_commit_message(&mut self, cx: &mut Context<Self>) {
        let Some(work_dir) = self.project.clone() else {
            self.git_feedback = Some("Attach a project to generate a commit message.".into());
            cx.notify();
            return;
        };
        let selected_paths: Vec<String> = self.selected_files.iter().cloned().collect();
        if selected_paths.is_empty() {
            self.git_feedback =
                Some("Select at least one file to generate a commit message.".into());
            cx.notify();
            return;
        }
        let total_count = self.review_files.len();
        let model = self.model.read(cx).selected_model.clone();
        if model.is_empty() {
            self.git_feedback =
                Some("Select a model in chat before generating a commit message.".into());
            cx.notify();
            return;
        }
        let (api_key, account_id) =
            threadlane_coding_agent::credentials::provider_credentials(&model);
        let tx = self.event_tx.clone();
        let Ok(executor) = threadlane_ui_state::chat::executor() else {
            self.git_feedback = Some("Unable to start the model runtime.".into());
            cx.notify();
            return;
        };

        self.git_message_pending = true;
        self.git_feedback = Some("Generating a commit message…".into());
        let daemon_client = self.model.read(cx).daemon_client.clone();
        executor.spawn(async move {
            let (result, diff_truncated) = async {
                let diff = if selected_paths.len() == total_count {
                    match threadlane_ui_state::project_io::commit_message_diff(
                        &daemon_client,
                        &work_dir,
                    )
                    .await
                    {
                        Ok(diff) => diff,
                        Err(error) => return (Err(error), false),
                    }
                } else {
                    match threadlane_ui_state::project_io::diff_files(
                        &daemon_client,
                        &work_dir,
                        selected_paths.clone(),
                        threadlane_git::DiffOptions::default(),
                    )
                    .await
                    {
                        Ok(diff) => diff,
                        Err(error) => return (Err(error), false),
                    }
                };
                let diff_truncated = diff.chars().count() > 24_000;
                let diff = if diff_truncated {
                    format!(
                        "{}\n\n[Diff truncated for message generation]",
                        diff.chars().take(24_000).collect::<String>()
                    )
                } else {
                    diff
                };
                let generated = if let Some(agent_id) = threadlane_acp_engine::acp_agent_id(&model)
                {
                    threadlane_acp_engine::generate_commit_message(
                        threadlane_project::default_global_threadlane_dir(),
                        work_dir.clone(),
                        agent_id,
                        &diff,
                    )
                    .await
                } else {
                    threadlane_coding_agent::credentials::provider_client_for(api_key, account_id)
                        .generate_commit_message(&model, &diff)
                        .await
                };
                let raw = match generated {
                    Ok(raw) => raw,
                    Err(error) => return (Err(error.to_string()), diff_truncated),
                };
                let message = normalize_generated_commit_message(&raw);
                if message.is_empty() {
                    (
                        Err("The model returned an empty commit message.".to_string()),
                        diff_truncated,
                    )
                } else {
                    (Ok(message), diff_truncated)
                }
            }
            .await;
            let _ = tx.send(PanelEvent::MessageGenerated {
                project: work_dir,
                result,
                diff_truncated,
            });
        });
        cx.notify();
    }

    pub fn run_git_action(
        &mut self,
        action: GitAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.execute_git_action(action, Some(window), cx);
    }

    pub(crate) fn run_git_action_without_window(
        &mut self,
        action: GitAction,
        cx: &mut Context<Self>,
    ) {
        self.execute_git_action(action, None, cx);
    }

    fn execute_git_action(
        &mut self,
        action: GitAction,
        mut window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let Some(work_dir) = self.project.clone() else {
            self.git_feedback = Some("Attach a project to use Git actions.".into());
            let notif = Notification::warning("Attach a project to use Git actions");
            if let Some(ref mut window) = window {
                window.push_notification(notif, cx);
            } else {
                self.pending_git_notifications.push(notif);
            }
            cx.notify();
            return;
        };
        if self.git_busy {
            return;
        }
        let message = self
            .commit_message_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let selected_paths: Vec<String> = self.selected_files.iter().cloned().collect();

        if matches!(action, GitAction::Commit | GitAction::CommitAndPush) {
            if selected_paths.is_empty() {
                self.git_feedback = Some("Select at least one file to commit.".into());
                let notif = Notification::warning("Select at least one file to commit");
                if let Some(ref mut window) = window {
                    window.push_notification(notif, cx);
                } else {
                    self.pending_git_notifications.push(notif);
                }
                cx.notify();
                return;
            }
        }
        if matches!(action, GitAction::Commit | GitAction::CommitAndPush) && message.is_empty()
        {
            self.git_feedback = Some("Enter a commit message first.".into());
            let notif = Notification::warning("Enter a commit message first");
            if let Some(ref mut window) = window {
                window.push_notification(notif, cx);
            } else {
                self.pending_git_notifications.push(notif);
            }
            cx.notify();
            return;
        }

        let checkout = matches!(
            action,
            GitAction::Checkout(_)
                | GitAction::CheckoutStash(_)
                | GitAction::CheckoutCarry(_)
                | GitAction::CreateBranch(_)
        );
        self.git_checkout_pending = checkout;
        if checkout {
            self.invalidate_review_diff(cx);
        }
        self.git_busy = true;
        let feedback = match &action {
            GitAction::Commit => "Committing…".to_string(),
            GitAction::CommitAndPush => "Committing and pushing…".to_string(),
            GitAction::StageFile(p) => format!("Staging {p}…"),
            GitAction::UnstageFile(p) => format!("Unstaging {p}…"),
            GitAction::StageFiles(paths) => format!("Staging {} files…", paths.len()),
            GitAction::UnstageFiles(paths) => format!("Unstaging {} files…", paths.len()),
            GitAction::StashPush { .. } => "Stashing changes…".to_string(),
            GitAction::Push => "Pushing…".to_string(),
            GitAction::Pull => "Pulling from origin…".to_string(),
            GitAction::Fetch => "Fetching origin…".to_string(),
            GitAction::StageAll => "Staging all changes…".to_string(),
            GitAction::UnstageAll => "Unstaging all changes…".to_string(),
            GitAction::CreatePullRequest => "Creating pull request…".to_string(),
            GitAction::Checkout(b) => format!("Switching to {b}…"),
            GitAction::CheckoutStash(b) => format!("Stashing changes & switching to {b}…"),
            GitAction::CheckoutCarry(b) => format!("Switching to {b} with changes…"),
            GitAction::CreateBranch(b) => format!("Creating branch {b}…"),
            GitAction::DeleteBranch(b) => format!("Deleting branch {b}…"),
            GitAction::Merge(b) => format!("Merging {b}…"),
            GitAction::PopStash(_) => "Restoring stashed changes…".to_string(),
            GitAction::DropStash(_) => "Discarding stash…".to_string(),
            GitAction::DiscardFile(p) => format!("Discarding changes in {p}…"),
            GitAction::DiscardFiles(paths) => {
                if paths.len() == 1 {
                    format!("Discarding changes in {}…", paths[0])
                } else {
                    format!("Discarding changes in {} files…", paths.len())
                }
            }
            GitAction::DiscardAll => "Discarding all changes…".to_string(),
            GitAction::IgnoreFile(p) => format!("Adding {p} to .gitignore…"),
            GitAction::IgnoreExtension(ext) => format!("Ignoring *.{ext} files…"),
        };
        self.git_feedback = Some(feedback);
        let tx = self.event_tx.clone();
        let daemon_client = self.model.read(cx).daemon_client.clone();
        let operation = git_action_to_operation(&action, message, selected_paths);
        let Ok(executor) = threadlane_ui_state::chat::executor() else {
            self.git_busy = false;
            self.git_checkout_pending = false;
            self.git_feedback = Some("The git runtime is unavailable.".into());
            cx.notify();
            return;
        };
        executor.spawn(async move {
            let outcome =
                threadlane_ui_state::project_io::run_action(&daemon_client, &work_dir, operation)
                    .await;
            let (action_error, action_message, status) = match outcome {
                Ok(outcome) => (outcome.action_error, outcome.message, outcome.status),
                Err(error) => (Some(error.clone()), None, Err(error)),
            };
            let _ = tx.send(PanelEvent::ActionFinished {
                project: work_dir,
                status,
                checkout_succeeded: checkout && action_error.is_none(),
                action_error,
                action_message,
            });
        });
        cx.notify();
    }

    /// Returns the live browser view, creating it on first use.
    fn ensure_browser(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<BrowserView> {
        if let Some(browser) = &self.browser {
            return browser.clone();
        }
        let browser_model = self.model.clone();
        let browser = cx.new(|cx| BrowserView::new(browser_model, window, cx));
        self.browser = Some(browser.clone());
        browser
    }

    /// Human terminal navigation uses a new tab, never the address/search
    /// resolver. Opens in the embedded browser where supported, else errors
    /// so the caller can fall back to the system browser.
    pub fn open_terminal_url(
        &mut self,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.is_dirty {
            return Err("Save or discard the editor's changes, then retry Open link…".into());
        }
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        {
            let browser = self.ensure_browser(window, cx);
            browser.update(cx, |browser, cx| browser.try_open_tab(url, window, cx))?;
            self.visible = true;
            self.open_surface(Surface::Browser, cx);
            browser.update(cx, |browser, cx| browser.focus_address(window, cx));
            Ok(())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            let _ = (url, window, cx);
            Err(
                "Threadlane browser is not supported on this platform. Choose Open in default browser."
                    .into(),
            )
        }
    }

    /// Native browser views must be hidden explicitly when the panel leaves the layout.
    /// The workspace retains its panel-open preference on other pages.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible
            && self.model.read(cx).workspace_page == threadlane_ui_state::WorkspacePage::Chat;
        if !self.visible && self.review_diff_request.is_some() {
            self.review_document
                .update(cx, |document, cx| document.dismiss_find(cx));
        }
        self.sync_browser_visibility(cx);
    }

    /// Shows the browser's webviews only while the Browser surface is
    /// active and the panel is open; hides them otherwise.
    fn sync_browser_visibility(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = self.browser.clone() else {
            return;
        };
        let visible = self.visible && self.active_surface == Some(Surface::Browser);
        browser.update(cx, |browser, cx| browser.set_visible(visible, cx));
    }

    /// Start a script evaluation against the live view, returning the
    /// pending result channel. The caller awaits it off the UI thread.
    fn start_browser_eval(
        &mut self,
        script: &str,
        cx: &mut Context<Self>,
    ) -> Result<tokio::sync::oneshot::Receiver<String>, String> {
        let Some(browser) = self.browser.clone() else {
            return Err("The browser panel is not ready.".to_string());
        };
        browser.update(cx, |browser, cx| browser.evaluate_script(script, cx))
    }

    /// Writes a browser snapshot into `.threadlane/previews/` and returns
    /// the saved path (if any) plus a base64 data URL for the reply.
    fn save_browser_screenshot(&self, bytes: &[u8]) -> (Option<String>, String) {
        let data_url = base64_data_url(bytes);
        let Some(project) = &self.project else {
            return (None, data_url);
        };
        let dir = project.join(".threadlane").join("previews");
        if std::fs::create_dir_all(&dir).is_err() {
            return (None, data_url);
        }
        let ext = snapshot_file_ext(bytes);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let path = dir.join(format!("browser-{stamp}.{ext}"));
        let _ = std::fs::write(&path, bytes);
        let _ = std::fs::write(dir.join(format!("latest-browser.{ext}")), bytes);
        (Some(path.display().to_string()), data_url)
    }

    /// Apply one agent `BrowserCommand` on the UI thread; called from the
    /// bridge pump, never from a tool worker directly. Unsupported
    /// platforms return a user-facing error.
    fn apply_browser_command(
        &mut self,
        command: threadlane_protocol::browser::BrowserCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            let _ = (command, window, cx);
            return Err("The embedded browser is not supported on this platform.".to_string());
        }
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        {
            use super::browser::{AddressTarget, resolve_address, search_url};
            use threadlane_protocol::browser::BrowserCommand;
            let Some(browser) = self.browser.clone() else {
                return Err("The browser panel is not ready.".to_string());
            };
            match command {
                BrowserCommand::Tabs { action } => {
                    use threadlane_protocol::browser::BrowserTabAction;
                    let reveal = !matches!(action, BrowserTabAction::List);
                    browser.update(cx, |browser, cx| -> Result<(), String> {
                        match action {
                            BrowserTabAction::List => {}
                            BrowserTabAction::Open { url } => {
                                let url = match resolve_address(&url) {
                                    Some(AddressTarget::Url(url)) => url,
                                    Some(AddressTarget::Search(query)) => search_url(&query),
                                    None => return Err("`browser_tabs` open requires a non-empty `url`.".into()),
                                };
                                browser.open_tab(&url, window, cx);
                            }
                            BrowserTabAction::Select { tab_id } | BrowserTabAction::Close { tab_id } => {
                                if !browser.tabs(cx).iter().any(|(id, _)| *id == tab_id) {
                                    return Err(format!("Browser tab {tab_id} does not exist. Use browser_tabs list to get current IDs."));
                                }
                                if matches!(action, BrowserTabAction::Select { .. }) {
                                    browser.switch_tab(tab_id, window, cx);
                                } else {
                                    browser.close_tab(tab_id, window, cx);
                                }
                            }
                        }
                        Ok(())
                    })?;
                    if reveal {
                        self.open_surface(Surface::Browser, cx);
                    }
                    let browser = browser.read(cx);
                    let tabs: Vec<_> = browser
                        .tabs(cx)
                        .into_iter()
                        .map(|(id, url)| serde_json::json!({"tab_id": id, "url": url}))
                        .collect();
                    Ok(serde_json::json!({
                        "active_tab_id": browser.active_tab_id(),
                        "tabs": tabs,
                    }).to_string())
                }
                BrowserCommand::Navigate { url } => {
                    let final_url = match resolve_address(&url) {
                        None => {
                            return Err(
                                "`browser_navigate` requires a non-empty `url`.".to_string()
                            );
                        }
                        Some(AddressTarget::Url(url)) => url,
                        Some(AddressTarget::Search(query)) => search_url(&query),
                    };
                    browser.update(cx, |browser, cx| browser.load_url(&final_url, cx));
                    // Keep the agent's browsing visible to the user.
                    self.open_surface(Surface::Browser, cx);
                    Ok(format!("Opened {final_url} in the browser panel."))
                }
                BrowserCommand::Back => {
                    browser.update(cx, |browser, cx| browser.go_back(cx));
                    self.open_surface(Surface::Browser, cx);
                    Ok("Went back in the browser panel.".to_string())
                }
                BrowserCommand::Reload => {
                    browser.update(cx, |browser, cx| browser.reload(cx));
                    Ok("Reloaded the browser panel.".to_string())
                }
                BrowserCommand::CurrentUrl => {
                    let url = browser.read(cx).current_url(cx).unwrap_or_default();
                    Ok(if url.is_empty() {
                        "The browser panel has no page open yet.".to_string()
                    } else {
                        url
                    })
                }
                // Script-backed commands route through start_browser_eval;
                // reaching here is a pump bug, not a page problem.
                _ => Err("Internal browser routing error.".to_string()),
            }
        }
    }

    /// Renders the browser surface, or the platform-stub message where the
    /// embedded browser is unavailable.
    fn render_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let browser = self.ensure_browser(window, cx);
        self.sync_browser_visibility(cx);
        div().flex_1().min_h_0().child(browser).into_any_element()
    }

    fn render_workspace_context(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // `self.project` is the session's git checkout: for worktree sessions
        // that is a `session_<id>` directory, which is meaningless to show.
        // Name the attached project and flag the worktree instead.
        let (repository, is_worktree_session) = {
            let state = self.model.read(cx);
            let project_name = state
                .active_work_dir
                .as_ref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .map(str::to_owned);
            let is_worktree_session = match (&state.active_work_dir, &self.project) {
                (Some(root), Some(git_dir)) => root != git_dir,
                _ => false,
            };
            (project_name, is_worktree_session)
        };
        let repository = repository
            .or_else(|| {
                self.project
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "No repository".to_owned());
        let branch = self
            .git_status
            .as_ref()
            .and_then(|status| status.branch.as_deref())
            .unwrap_or("no branch")
            .to_owned();
        let git_state = match self.git_status.as_ref() {
            Some(status) if status.has_changes => format!("{} files changed", status.files.len()),
            Some(_) => "No changes".into(),
            None => "Git status unavailable".into(),
        };
        let has_changes = self
            .git_status
            .as_ref()
            .map(|status| status.has_changes)
            .unwrap_or(false);
        let file_context = self
            .document_title
            .clone()
            .unwrap_or_else(|| "No active file".to_owned());
        threadlane_ui_kit::review_workspace_context(
            &threadlane_ui_kit::ReviewWorkspaceContext { repository, branch, git_state, worktree: is_worktree_session,
                unavailable: self.worktree_unavailable, file: file_context, has_changes },
            cx.listener(|this, _, _, cx| this.open_surface(Surface::Review, cx)), cx)
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let refresh = (!matches!(self.active_surface, Some(Surface::Agents | Surface::Trajectory))).then(|| {
            threadlane_ui_kit::right_panel_refresh_button()
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.refresh_active_surface(cx);
                    cx.notify();
                }))
        });
        threadlane_ui_kit::right_panel_header(
            self.active_surface,
            &available_surfaces(),
            refresh,
            cx.listener(|this, surface: &Surface, _window, cx| this.open_surface(*surface, cx)),
            cx,
        )
    }

    fn render_chooser(&self, cx: &mut Context<Self>) -> impl IntoElement {
        threadlane_ui_kit::right_panel_chooser(
            &available_surfaces(),
            cx.listener(|this, surface: &Surface, _window, cx| this.open_surface(*surface, cx)),
            cx,
        )
    }

    fn toggle_markdown_preview(
        &mut self,
        _: &threadlane_ui_kit::ToggleMarkdownPreview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.markdown_preview.is_active() {
            self.markdown_preview.show_source();
            self.markdown_preview.focus_control(window, cx);
            cx.notify();
            return;
        }
        if self.pending_document.is_some()
            || !self
                .document_title
                .as_deref()
                .is_some_and(threadlane_ui_kit::markdown_preview_eligible)
        {
            return;
        }
        if let Some(editor) = &self.editor_state {
            self.markdown_preview.toggle(editor.read(cx).value(), cx);
            self.markdown_preview.focus_control(window, cx);
            cx.notify();
        }
    }

    fn render_files(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(title) = &self.document_title {
            let is_dirty = self.is_dirty;
            let has_editor = self.editor_state.is_some();
            let lang = detect_language(title);
            let selection_reason = self.files_selection_block_reason(cx);
            return div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .key_context("PanelDocument")
                .on_action(cx.listener(|this, _: &threadlane_ui_kit::SavePanelDocument, _, cx| this.save_active_document(cx)))
                .on_action(cx.listener(|this, _: &threadlane_ui_kit::AddSelectionToChat, window, cx| {
                    this.request_add_selection_to_chat(window, cx)
                }))
                .on_action(cx.listener(Self::toggle_markdown_preview))
                .when(self.review_diff_request.is_some(), |this| {
                    this.key_context("PanelDocument ReviewDiff")
                        .on_action(cx.listener(
                            |this, _: &threadlane_ui_kit::FindInDiff, window, cx| {
                                this.review_document
                                    .update(cx, |document, cx| document.open_find(window, cx));
                            },
                        ))
                })
                .child(threadlane_ui_kit::panel_document_header(
                    (threadlane_ui_kit::markdown_preview_eligible(title)
                        && !title.starts_with("Review ·"))
                    .then(|| {
                        self.markdown_preview.control(
                            !has_editor || self.document_loading || self.pending_document.is_some(),
                            cx,
                        )
                    }),
                    title,
                    is_dirty,
                    self.document_can_save(cx),
                    has_editor.then_some(lang),
                    self.active_surface == Some(Surface::Review),
                    has_editor.then_some(threadlane_ui_kit::AddSelectionControl {
                        enabled: selection_reason.is_none(),
                        reason: selection_reason,
                    }),
                    cx.listener(|this, action: &threadlane_ui_kit::PanelDocumentAction, window, cx| {
                        use threadlane_ui_kit::PanelDocumentAction;
                        match action {
                            PanelDocumentAction::Save => this.save_active_document(cx),
                            PanelDocumentAction::AddSelectionToChat => this.request_add_selection_to_chat(window, cx),
                            PanelDocumentAction::Back | PanelDocumentAction::Close => this.close_document(cx),
                        }
                    }),
                ))
                .children(self.render_save_recovery(cx))
                .children(
                    self.markdown_preview
                        .notice(is_dirty)
                        .map(|notice| threadlane_ui_kit::markdown_preview_notice(notice, cx)),
                )
                .children(self.review_diff_request.as_ref().map(|_| {
                    threadlane_ui_kit::review_whitespace_control(
                        self.review_diff_options.ignore_whitespace,
                        cx.listener(|this, checked: &bool, _, cx| this.set_ignore_whitespace(*checked, cx)),
                        cx,
                    )
                }))
                .children(self.review_diff_nav_row(cx))
                .child(Separator::horizontal())
                .child(if self.document_loading {
                    threadlane_ui_kit::markdown_document_message("Loading file…", cx)
                        .into_any_element()
                } else if let Some(error) = &self.document_error {
                    threadlane_ui_kit::markdown_document_message(error, cx).into_any_element()
                } else if self.markdown_preview.is_active() {
                    self.markdown_preview.body(cx)
                } else if let Some(ref editor) = self.editor_state {
                    threadlane_ui_kit::editor_buffer(editor).into_any_element()
                } else if self.review_diff_state.is_some() {
                    self.review_document.clone().into_any_element()
                } else {
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .p_3()
                        .child(TextView::new(&self.document_state).selectable(true))
                        .into_any_element()
                })
                .into_any_element();
        }
        let model = self.model.clone();

        threadlane_ui_kit::project_files_surface()
            .child(threadlane_ui_kit::project_files_find_button()
                .on_click(cx.listener(|this, _, window, cx| {
                    super::file_search::open(this.model.clone(), window, cx);
                })))
            .child(threadlane_ui_kit::project_file_tree(
                &self.tree_state,
                move |path, _, cx| {
                    model.update(cx, |state, cx| {
                        state.request_open_file(path.to_owned());
                        cx.notify();
                    });
                },
                {
                    let model = self.model.clone();
                    let project = self.project.clone();
                    move |path, folder, menu, _, _| {
                        let absolute = project.as_ref().map(|project| project.join(path).display().to_string());
                        let model = model.clone();
                        threadlane_ui_kit::project_file_menu(menu, path, folder, absolute, move |action, window, cx| {
                            use threadlane_ui_kit::ProjectFileAction;
                            match action {
                                ProjectFileAction::Open(path) => model.update(cx, |state, cx| {
                                    state.request_open_file(path.clone());
                                    cx.notify();
                                }),
                                ProjectFileAction::OpenInPanel(path) => model.update(cx, |state, cx| {
                                    state.request_open_panel_file(path.clone());
                                    cx.notify();
                                }),
                                ProjectFileAction::CopyRelative(path) | ProjectFileAction::CopyAbsolute(path) => {
                                    cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                                    window.push_notification(Notification::info(match action {
                                        ProjectFileAction::CopyRelative(_) => "Copied relative path",
                                        _ => "Copied absolute path",
                                    }), cx);
                                }
                            }
                        })
                    }
                },
            ))
            .into_any_element()
    }

    pub(crate) fn handle_discard_option(
        panel: Entity<RightPanelView>,
        opt: DiscardOption,
        window: &mut Window,
        cx: &mut App,
    ) {
        if opt.requires_confirmation() {
            let (title, description) = opt.confirmation_prompt().unwrap_or((
                "Discard changes?".into(),
                "Are you sure you want to discard these changes? This cannot be undone.".into(),
            ));
            let action = discard_git_action(&opt);
            cx.spawn(async move |cx| {
                let confirmed = rfd::AsyncMessageDialog::new()
                    .set_title(&title)
                    .set_description(&description)
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    .await;
                if matches!(confirmed, rfd::MessageDialogResult::Yes) {
                    let _ = panel.update(cx, |this, cx| {
                        this.run_git_action_without_window(action, cx);
                    });
                }
            })
            .detach();
        } else {
            let action = discard_git_action(&opt);
            panel.update(cx, |this, cx| {
                this.run_git_action(action, window, cx);
            });
        }
    }

    /// `Previous file · File X of Y · Next file` for a single-file local
    /// Review diff. Absent for All changes, editor documents and non-Review
    /// surfaces. Position and identities resolve by exact path against the
    /// filtered inventory on every render, so list refreshes cannot retarget
    /// a displayed patch.
    fn review_diff_nav_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.active_surface != Some(Surface::Review) {
            return None;
        }
        let request = self.review_diff_request.as_ref()?;
        let ReviewDiffTarget::File(path) = &request.target else {
            return None;
        };
        let paths: Vec<String> = self
            .filtered_review_files(cx)
            .iter()
            .map(|file| file.path.clone())
            .collect();
        let query = self.review_filter_input.read(cx).value().trim().to_string();
        let unavailable = if self.git_checkout_pending {
            Some("Checkout is switching…".to_owned())
        } else if self.review_error.is_some() {
            Some("Changes list unavailable".to_owned())
        } else {
            None
        };
        Some(threadlane_ui_kit::review_diff_nav(
            &threadlane_ui_kit::ReviewDiffNavigation {
                paths: &paths,
                current: Some(path.as_str()),
                filter: (!query.is_empty()).then_some(query.as_str()),
                unavailable: unavailable.as_deref(),
                focus: Some(&self.review_nav_focus),
            },
            cx.listener(|this, action: &threadlane_ui_kit::ReviewDiffNavAction, window, cx| {
                this.navigate_review_diff(*action, window, cx)
            }),
            cx,
        ))
    }

    /// Load the bounded adjacent file through the same `open_file_diff` path,
    /// revalidating checkout state and inventory membership at activation.
    /// Advances from the currently requested path so rapid actions stay
    /// deterministic; never stages, commits, discards or marks files.
    fn navigate_review_diff(
        &mut self,
        action: threadlane_ui_kit::ReviewDiffNavAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.git_checkout_pending || self.review_error.is_some() {
            return;
        }
        let Some(request) = &self.review_diff_request else {
            return;
        };
        let ReviewDiffTarget::File(current) = &request.target else {
            return;
        };
        let paths: Vec<String> = self
            .filtered_review_files(cx)
            .iter()
            .map(|file| file.path.clone())
            .collect();
        let Some(adjacency) = threadlane_ui_kit::review_diff_adjacency(&paths, current) else {
            return;
        };
        use threadlane_ui_kit::ReviewDiffNavAction;
        let target = match action {
            ReviewDiffNavAction::Previous => adjacency.previous,
            ReviewDiffNavAction::Next => adjacency.next,
        };
        let Some(target) = target else {
            return;
        };
        // When the direction just taken ends at a boundary the initiating
        // button becomes disabled; move focus to the stable navigation group
        // rather than dropping it.
        if let Some(next_adjacency) = threadlane_ui_kit::review_diff_adjacency(&paths, &target) {
            let boundary = match action {
                ReviewDiffNavAction::Previous => next_adjacency.previous.is_none(),
                ReviewDiffNavAction::Next => next_adjacency.next.is_none(),
            };
            if boundary {
                window.focus(&self.review_nav_focus, cx);
            }
        }
        self.open_file_diff(target, cx);
    }

    fn filtered_review_files(&self, cx: &Context<Self>) -> Vec<GitFile> {
        let query = self
            .review_filter_input
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        if query.is_empty() {
            self.review_files.clone()
        } else {
            self.review_files
                .iter()
                .filter(|f| f.path.to_lowercase().contains(&query))
                .cloned()
                .collect()
        }
    }

    pub(crate) fn diff_addition_percent(additions: u32, deletions: u32) -> f32 {
        threadlane_ui_kit::review_diff_addition_percent(additions, deletions)
    }

    fn render_file_item(
        &self,
        file: &GitFile,
        is_tree_node: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel_entity = cx.entity().clone();
        let path = file.path.clone();
        let path_for_chk = path.clone();
        let is_selected = self.selected_files.contains(&path);
        let absolute_path = self.project.as_ref().map(|root| root.join(&path).display().to_string());
        let is_open = self.document_title.as_deref().is_some_and(|title| title == format!("Review · {path}").as_str());
        let row = threadlane_ui_kit::review_file_row(
            file,
            threadlane_ui_kit::ReviewFileAppearance {
                selected: is_selected, open: is_open, tree_node: is_tree_node, absolute_path: absolute_path.as_deref(),
            },
            cx.listener(move |this, checked: &bool, _, cx| {
                if *checked { this.selected_files.insert(path_for_chk.clone()); }
                else { this.selected_files.remove(&path_for_chk); }
                cx.notify();
            }),
            cx.listener({ let path = path.clone(); move |this, _, _, cx| this.open_file_diff(path.clone(), cx) }),
            cx,
        )
            .context_menu({
                let file = file.clone();
                move |menu, window, cx| {
                    let host = panel_entity.read(cx);
                    let selected: Vec<_> = host.selected_files.iter().cloned().collect();
                    let panel = panel_entity.clone();
                    let project = host.project.clone();
                    threadlane_ui_kit::review_file_menu(menu, &threadlane_ui_kit::ReviewFileMenu {
                        file: &file, selected_paths: &selected, total_files: host.review_files.len(),
                        absolute_path: absolute_path.as_deref(), reveal_label: threadlane_ui_kit::review_file_manager_label(), busy: host.git_busy,
                    }, move |action, window, cx| {
                        Self::handle_review_file_action(panel.clone(), project.clone(), action, window, cx);
                    }, window, cx)
                }
            });
        threadlane_ui_kit::review_file_inset(row).into_any_element()
    }

    fn handle_review_file_action(panel: Entity<Self>, project: Option<PathBuf>, action: &threadlane_ui_kit::ReviewFileAction, window: &mut Window, cx: &mut App) {
        use threadlane_ui_kit::ReviewFileAction;
        if panel.read(cx).project != project {
            window.push_notification(Notification::info("The review checkout changed. Open the file menu again."), cx);
            return;
        }
        let git_action = match action {
            ReviewFileAction::Stage(path) => Some(GitAction::StageFile(path.clone())),
            ReviewFileAction::Unstage(path) => Some(GitAction::UnstageFile(path.clone())),
            ReviewFileAction::IgnoreFile(path) => Some(GitAction::IgnoreFile(path.clone())),
            ReviewFileAction::IgnoreExtension(ext) => Some(GitAction::IgnoreExtension(ext.clone())),
            _ => None,
        };
        if let Some(action) = git_action { panel.update(cx, |host, cx| host.run_git_action(action, window, cx)); return; }
        match action {
            ReviewFileAction::Discard(target) => Self::handle_discard_option(panel, target.clone(), window, cx),
            ReviewFileAction::OpenDiff(target) => {
                let host = panel.read(cx);
                let Some(project) = host.project.clone() else { return; };
                let model = host.model.clone();
                let client = model.read(cx).daemon_client.clone();
                let target = target.clone();
                cx.spawn(async move |cx| {
                    let diff_project = project.clone();
                    let diff_target = target.clone();
                    let content = cx.background_executor().spawn(async move {
                        threadlane_ui_state::project_io::diff_file(&client, &diff_project, diff_target, threadlane_git::DiffOptions::default()).await.unwrap_or_else(|error| error)
                    }).await;
                    let _ = model.update(cx, |state, cx| { state.request_open_diff(project, target, content); cx.notify(); });
                }).detach();
            }
            ReviewFileAction::CopyRelative(path) | ReviewFileAction::CopyAbsolute(path) => {
                cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                window.push_notification(Notification::info(if matches!(action, ReviewFileAction::CopyAbsolute(_)) {
                    "Copied absolute file path" } else { "Copied file path" }), cx);
            }
            ReviewFileAction::Reveal(path) => threadlane_git::reveal_in_file_manager(std::path::Path::new(path)),
            _ => {}
        }
    }

    fn render_review_file_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let files = self.filtered_review_files(cx);
        let Some(file) = files.get(index).cloned() else {
            return div().into_any_element();
        };
        self.render_file_item(&file, false, cx)
    }

    fn handle_review_ui_action(&mut self, action: threadlane_ui_kit::ReviewAction, window: &mut Window, cx: &mut Context<Self>) {
        use threadlane_ui_kit::ReviewAction;
        match action {
            ReviewAction::ClearFilter => self.review_filter_input.update(cx, |input, cx| input.set_value("", window, cx)),
            ReviewAction::SelectList => self.review_view_mode = ReviewViewMode::List,
            ReviewAction::SelectTree => self.review_view_mode = ReviewViewMode::Tree,
            ReviewAction::SelectAll(selected) => {
                if selected { self.selected_files = self.review_files.iter().map(|file| file.path.clone()).collect(); }
                else { self.selected_files.clear(); }
            }
            ReviewAction::OpenCombinedDiff => self.open_combined_diff(cx),
            ReviewAction::StageAll => self.run_git_action(GitAction::StageAll, window, cx),
            ReviewAction::UnstageAll => self.run_git_action(GitAction::UnstageAll, window, cx),
            ReviewAction::ClearCommit => self.commit_message_input.update(cx, |input, cx| input.set_value("", window, cx)),
            ReviewAction::GenerateCommit => self.generate_commit_message(cx),
            ReviewAction::Commit => self.run_git_action(GitAction::Commit, window, cx),
            ReviewAction::CommitAndPush => self.run_git_action(GitAction::CommitAndPush, window, cx),
            ReviewAction::Push => self.run_git_action(GitAction::Push, window, cx),
        }
        cx.notify();
    }

    fn render_review(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let filtered_count = self.filtered_review_files(cx).len();
        if self.review_files_list_state.item_count() != filtered_count {
            self.review_files_list_state
                .reset_with_uniform_height(filtered_count, window.rem_size() * 2.0);
        }
        let panel_entity = cx.entity().clone();
        if let Some(error) = self.review_error.clone() {
            return self.render_review_error(&error, cx);
        }
        let total_files = self.review_files.len();
        let selected_count = self.selected_files.len();

        let selected_additions: u32 = self
            .review_files
            .iter()
            .filter(|f| self.selected_files.contains(&f.path))
            .map(|f| f.additions)
            .sum();
        let selected_deletions: u32 = self
            .review_files
            .iter()
            .filter(|f| self.selected_files.contains(&f.path))
            .map(|f| f.deletions)
            .sum();

        let branch = self
            .git_status
            .as_ref()
            .and_then(|s| s.branch.as_deref())
            .unwrap_or("No branch");

        let can_push = !self.git_busy
            && !self.git_message_pending
            && self
                .git_status
                .as_ref()
                .is_some_and(|status| status.ahead > 0);

        let last_fetched_str = if let Some(instant) = self.last_fetched_time {
            let secs = instant.elapsed().as_secs();
            if secs < 60 {
                "Last fetched just now".to_string()
            } else if secs < 3600 {
                format!("Last fetched {} minutes ago", secs / 60)
            } else {
                format!("Last fetched {} hours ago", secs / 3600)
            }
        } else {
            "Fetch latest changes".to_string()
        };

        let status = self.git_status.as_ref();
        let behind = status.map_or(0, |s| s.behind);
        let ahead = status.map_or(0, |s| s.ahead);
        let can_publish = can_publish_branch(self.project.is_some(), status);
        let can_create_pr =
            can_create_pull_request(self.project.is_some() && !self.worktree_unavailable, status);

        let sync_kind = if can_publish { threadlane_ui_kit::ReviewSyncKind::Publish }
            else if behind > 0 { threadlane_ui_kit::ReviewSyncKind::Pull }
            else if ahead > 0 { threadlane_ui_kit::ReviewSyncKind::Push }
            else { threadlane_ui_kit::ReviewSyncKind::Fetch };
        let sync_button = threadlane_ui_kit::review_sync_button(sync_kind, if behind > 0 { behind } else { ahead }, &last_fetched_str, self.git_busy)
            .on_click(cx.listener(move |this, _, window, cx| {
                let action = match sync_kind {
                    threadlane_ui_kit::ReviewSyncKind::Publish | threadlane_ui_kit::ReviewSyncKind::Push => GitAction::Push,
                    threadlane_ui_kit::ReviewSyncKind::Pull => GitAction::Pull,
                    threadlane_ui_kit::ReviewSyncKind::Fetch => GitAction::Fetch,
                };
                this.run_git_action(action, window, cx);
            }));
        let sync_actions = threadlane_ui_kit::review_sync_actions(sync_button,
            (total_files > 0).then(|| threadlane_ui_kit::review_stash_button(self.git_busy)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.close_all_git_dialogs(); this.stash_dialog_open = true;
                    this.stash_message_input.update(cx, |input, cx| input.focus(window, cx));
                    cx.notify();
                }))),
            can_create_pr.then(|| threadlane_ui_kit::review_create_pr_button(self.git_busy)
                .on_click(cx.listener(|this, _, window, cx| this.open_draft_pr_dialog(window, cx)))),
        );

        let branch_header = threadlane_ui_kit::review_branch_header(branch, self.branch_popover_open, sync_actions,
            cx.listener(|this, _, _, cx| { this.branch_popover_open = !this.branch_popover_open; cx.notify(); }), cx);

        let pr_card = self.git_status.as_ref().and_then(|status| status.pr.as_ref())
            .map(|pr| self.render_review_pr(pr, cx));

        let staged_count = self.review_files.iter().filter(|f| f.staged).count();
        let unstaged_count = self.review_files.iter().filter(|f| f.unstaged).count();
        let has_staged = staged_count > 0;

        let total_additions_all: u32 = self.review_files.iter().map(|f| f.additions).sum();
        let total_deletions_all: u32 = self.review_files.iter().map(|f| f.deletions).sum();
        let diff_ratio_bar = threadlane_ui_kit::review_diff_ratio(total_additions_all, total_deletions_all, cx);

        let review_toolbar = (total_files > 0).then(|| {
            let panel_sb = panel_entity.clone();
            let discard = div()
                                .child(
                                    threadlane_ui_kit::review_discard_button(self.git_busy)
                                        .on_click(cx.listener(|this, _event, window, cx| {
                                            let paths: Vec<String> =
                                                this.selected_files.iter().cloned().collect();
                                            let total = this.review_files.len();
                                            Self::handle_discard_option(
                                                cx.entity().clone(),
                                                if paths.is_empty() {
                                                    DiscardOption::All(total)
                                                } else {
                                                    DiscardOption::Selected(paths)
                                                },
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                                .context_menu({
                                    let panel = panel_sb.clone();
                                    move |menu, window, cx| {
                                        let (selected_paths, total_files) = {
                                            let panel_ref = panel.read(cx);
                                            let selected_paths: Vec<String> =
                                                panel_ref.selected_files.iter().cloned().collect();
                                            (selected_paths, panel_ref.review_files.len())
                                        };
                                        let busy = panel.read(cx).git_busy;
                                        let panel = panel.clone();
                                        threadlane_ui_kit::review_discard_menu(menu,
                                            threadlane_ui_kit::review_selection_discard_targets(&selected_paths, total_files), busy,
                                            move |target, window, cx| Self::handle_discard_option(panel.clone(), target.clone(), window, cx), window, cx)
                                    }
                                }).into_any_element();
            threadlane_ui_kit::review_toolbar_surface(cx)
                .child(threadlane_ui_kit::review_filters(
                    &self.review_filter_input, self.review_view_mode, discard,
                    cx.listener(|this, action: &threadlane_ui_kit::ReviewAction, window, cx| this.handle_review_ui_action(*action, window, cx)), cx,
                ))
                .child(threadlane_ui_kit::review_selection_bar(
                    &threadlane_ui_kit::ReviewSelectionState {
                        selected_count, total_files, additions: selected_additions, deletions: selected_deletions,
                        unstaged_count, has_staged, busy: self.git_busy,
                    },
                    cx.listener(|this, action: &threadlane_ui_kit::ReviewAction, window, cx| this.handle_review_ui_action(*action, window, cx)), cx,
                ))
                .children(diff_ratio_bar)
        });
        let file_list_content = if self.review_files.is_empty() {
            threadlane_ui_kit::review_clean_state(
                threadlane_ui_kit::review_refresh_button(self.git_busy)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_active_surface(cx))), cx).into_any_element()
        } else if filtered_count == 0 {
            threadlane_ui_kit::review_no_results(
                cx.listener(|this, _, window, cx| this.handle_review_ui_action(threadlane_ui_kit::ReviewAction::ClearFilter, window, cx)), cx).into_any_element()
        } else if self.review_view_mode == ReviewViewMode::Tree {
            let filtered = self.filtered_review_files(cx);
            let mut dir_map: std::collections::BTreeMap<String, Vec<GitFile>> =
                std::collections::BTreeMap::new();
            for f in filtered {
                let (dir, _) = f.path.rsplit_once('/').unwrap_or(("", &f.path));
                dir_map.entry(dir.to_string()).or_default().push(f);
            }
            threadlane_ui_kit::review_tree_viewport()
                .children(dir_map.into_iter().map(|(dir, dir_files)| {
                    let is_collapsed = self.collapsed_tree_folders.contains(&dir);
                    let dir_key = dir.clone();
                    let count = dir_files.len();
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            threadlane_ui_kit::review_folder_header(&dir, count, is_collapsed, cx)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.collapsed_tree_folders.remove(&dir_key) { this.collapsed_tree_folders.insert(dir_key.clone()); }
                                    cx.notify();
                                })),
                        )
                        .children((!is_collapsed).then(|| {
                            threadlane_ui_kit::review_folder_files(cx)
                                .children(
                                    dir_files
                                        .into_iter()
                                        .map(|f| self.render_file_item(&f, true, cx)),
                                )
                        }))
                }))
                .into_any_element()
        } else {
            threadlane_ui_kit::review_file_list(&self.review_files_list_state, cx.processor(Self::render_review_file_row)).into_any_element()
        };

        let commit_footer = threadlane_ui_kit::review_commit_footer(
            &self.commit_message_input,
            &threadlane_ui_kit::ReviewCommitState {
                selected_count, total_files, busy: self.git_busy, generating: self.git_message_pending, can_push,
            },
            cx.listener(|this, action: &threadlane_ui_kit::ReviewAction, window, cx| this.handle_review_ui_action(*action, window, cx)), cx,
        );
        let stash_banner = self.git_status.as_ref().and_then(|status| status.current_stash.as_ref())
            .map(|stash| self.render_current_stash(stash, cx));

        let commit_count = self.git_status.as_ref().map_or(0, |status| status.recent_commits.len());
        let review_sub_tabs = threadlane_ui_kit::review_tabs(self.review_tab, total_files, staged_count, commit_count,
            cx.listener(|this, ix: &usize, _, cx| {
                this.review_tab = if *ix == 0 { ReviewTab::Changes } else { ReviewTab::History };
                cx.notify();
            }), cx);

        let review_body = if self.branch_popover_open {
            self.render_branch_manager(cx).into_any_element()
        } else if self.review_tab == ReviewTab::History {
            self.render_history(cx).into_any_element()
        } else {
            threadlane_ui_kit::review_changes_body()
                .child(threadlane_ui_kit::review_changes_content()
                    .children(stash_banner)
                    .children(pr_card)
                    .children(review_toolbar)
                    .child(threadlane_ui_kit::review_changes_files(file_list_content)))
                .child(commit_footer)
                .into_any_element()
        };

        threadlane_ui_kit::review_panel_surface()
            .child(branch_header)
            .children((!self.branch_popover_open).then(|| review_sub_tabs))
            .child(review_body)
            .into_any_element()
    }

    fn render_review_pr(&self, pr: &threadlane_protocol::repo::GitHubPrInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let pr = pr.clone();
        let project = self.project.clone();
        threadlane_ui_kit::review_pr_card(&pr, &threadlane_ui_kit::ReviewPrState {
            expanded: self.pr_expanded,
            feedback_count: Some(threadlane_git::collect_actionable_pr_feedback(&pr).len()),
            can_address: self.project.is_some(),
            busy: self.git_busy,
        }, cx.listener({ let pr = pr.clone(); move |this, action: &threadlane_ui_kit::ReviewPrAction, _, cx| {
            use threadlane_ui_kit::ReviewPrAction;
            match action {
                ReviewPrAction::Toggle => this.pr_expanded = !this.pr_expanded,
                ReviewPrAction::Open => cx.open_url(&pr.url),
                ReviewPrAction::FixCi => {
                    let prompt = threadlane_git::build_fix_ci_prompt(&pr);
                    this.model.update(cx, |state, _| state.request_composer_prompt(prompt));
                }
                ReviewPrAction::AddressComments => {
                    let Some(work_dir) = project.clone() else { return; };
                    this.model.update(cx, |state, cx| {
                        state.session_status = Some(match state.address_pr_reviews_manual(work_dir, pr.head_ref.clone(), &pr) {
                            Ok(_) => "Addressing PR review feedback…".into(), Err(error) => error,
                        });
                        cx.notify();
                    });
                }
            }
            cx.notify();
        }}), cx)
    }

    fn render_current_stash(&self, stash: &threadlane_protocol::repo::GitStashInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let index = stash.index;
        let loading = self.loading_stash_index == Some(index);
        let files = self.stash_files.as_ref().filter(|(loaded, _)| *loaded == index);
        let body = self.stash_expanded.then(|| {
            let rows = files.into_iter().flat_map(|(_, files)| files).map(|file| {
                let project = self.project.clone();
                let model = self.model.clone();
                let target = file.path.clone();
                threadlane_ui_kit::review_stash_file(index, file, cx.listener(move |_, _, _, cx| {
                    let Some(project) = project.clone() else { return; };
                    let target = target.clone();
                    let diff_project = project.clone();
                    let model = model.clone();
                    let client = model.read(cx).daemon_client.clone();
                    cx.spawn(async move |_, cx| {
                        let diff_target = target.clone();
                        let content = cx.background_executor().spawn(async move {
                            threadlane_ui_state::project_io::diff_stash_file(&client, &diff_project, index, diff_target).await.unwrap_or_else(|error| error)
                        }).await;
                        let _ = model.update(cx, |state, cx| {
                            state.request_open_diff(project, target, content);
                            cx.notify();
                        });
                    }).detach();
                }), cx).into_any_element()
            }).collect();
            threadlane_ui_kit::review_stash_files(loading, rows, cx).into_any_element()
        });
        threadlane_ui_kit::review_stash_card(stash, &threadlane_ui_kit::ReviewStashState {
            expanded: self.stash_expanded, loading, files_count: files.map(|(_, files)| files.len()), busy: self.git_busy,
        }, body, cx.listener(move |this, action: &threadlane_ui_kit::ReviewStashAction, window, cx| {
            match action {
                threadlane_ui_kit::ReviewStashAction::Discard => this.run_git_action(GitAction::DropStash(Some(index)), window, cx),
                threadlane_ui_kit::ReviewStashAction::Restore => this.run_git_action(GitAction::PopStash(Some(index)), window, cx),
                threadlane_ui_kit::ReviewStashAction::Toggle => {
                    this.stash_expanded = !this.stash_expanded;
                    if this.stash_expanded && this.loading_stash_index != Some(index)
                        && this.stash_files.as_ref().is_none_or(|(loaded, _)| *loaded != index) {
                        if let Some(project) = this.project.clone() {
                            this.loading_stash_index = Some(index);
                            let tx = this.event_tx.clone();
                            let client = this.model.read(cx).daemon_client.clone();
                            if let Ok(executor) = threadlane_ui_state::chat::executor() {
                                executor.spawn(async move {
                                    let files = threadlane_ui_state::project_io::stash_files(&client, &project, index).await.unwrap_or_default();
                                    let _ = tx.send(PanelEvent::StashFilesLoaded { project, index, files });
                                });
                            }
                        }
                    }
                }
            }
            cx.notify();
        }), cx)
    }

    fn render_history(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.history_filter_input.read(cx).value();
        let commits = self.git_status.as_ref().map_or(&[][..], |status| status.recent_commits.as_slice());
        let filtered = threadlane_ui_kit::review_filtered_commits(commits, &query);
        let content = if filtered.is_empty() {
            threadlane_ui_kit::review_history_empty(!query.trim().is_empty(), cx.listener(|this, _, window, cx| {
                this.history_filter_input.update(cx, |input, cx| input.set_value("", window, cx));
                cx.notify();
            }), cx).into_any_element()
        } else {
            threadlane_ui_kit::review_history_viewport().children(filtered.into_iter().map(|commit| {
                let expanded = self.selected_commit_sha.as_deref() == Some(&commit.sha);
                let loading = self.loading_commit_sha.as_deref() == Some(&commit.sha);
                let body = expanded.then(|| {
                    let rows = self.selected_commit_files.iter().map(|file| {
                        let project = self.project.clone();
                        let model = self.model.clone();
                        let target = file.path.clone();
                        let sha = commit.sha.clone();
                        let short = commit.short_sha.clone();
                        threadlane_ui_kit::review_commit_file(&commit.sha, file, cx.listener(move |_, _, _, cx| {
                            let Some(project) = project.clone() else { return; };
                            let diff_project = project.clone();
                            let target = target.clone();
                            let diff_target = target.clone();
                            let sha = sha.clone();
                            let label = format!("{target} @ {short}");
                            let model = model.clone();
                            let client = model.read(cx).daemon_client.clone();
                            cx.spawn(async move |_, cx| {
                                let content = cx.background_executor().spawn(async move {
                                    threadlane_ui_state::project_io::diff_commit_file(&client, &diff_project, sha, diff_target).await.unwrap_or_else(|error| error)
                                }).await;
                                let _ = model.update(cx, |state, cx| {
                                    state.request_open_diff(project, label, content);
                                    cx.notify();
                                });
                            }).detach();
                        }), cx).into_any_element()
                    }).collect();
                    threadlane_ui_kit::review_commit_files(loading, rows, cx).into_any_element()
                });
                let sha = commit.sha.clone();
                threadlane_ui_kit::review_commit_card(commit, expanded, body, cx.listener(move |this, _, _, cx| {
                    if this.selected_commit_sha.as_deref() == Some(&sha) {
                        this.selected_commit_sha = None;
                        this.loading_commit_sha = None;
                        this.selected_commit_files.clear();
                    } else {
                        this.selected_commit_sha = Some(sha.clone());
                        this.loading_commit_sha = Some(sha.clone());
                        this.selected_commit_files.clear();
                        if let Some(project) = this.project.clone() {
                            let tx = this.event_tx.clone();
                            let fetch_sha = sha.clone();
                            let client = this.model.read(cx).daemon_client.clone();
                            if let Ok(executor) = threadlane_ui_state::chat::executor() {
                                executor.spawn(async move {
                                    let files = threadlane_ui_state::project_io::commit_files(&client, &project, fetch_sha.clone()).await.unwrap_or_default();
                                    let _ = tx.send(PanelEvent::CommitFilesLoaded { sha: fetch_sha, files });
                                });
                            }
                        }
                    }
                    cx.notify();
                }), cx)
            })).into_any_element()
        };
        threadlane_ui_kit::review_history_surface(&self.history_filter_input, content, cx)
    }

    fn render_branch_manager(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let project = self.project.clone();
        threadlane_ui_kit::review_branch_manager(&self.branch_filter_input, self.git_status.as_ref(), self.git_busy,
            self.project.is_some(), cx.listener(move |this, action: &threadlane_ui_kit::ReviewBranchAction, window, cx| {
                this.handle_review_branch_action(&project, action, window, cx);
            }), cx)
    }

    fn handle_review_branch_action(&mut self, project: &Option<PathBuf>, action: &threadlane_ui_kit::ReviewBranchAction, window: &mut Window, cx: &mut Context<Self>) {
        use threadlane_ui_kit::ReviewBranchAction;
        if self.project != *project { return; }
        match action {
            ReviewBranchAction::Close => self.branch_popover_open = false,
            ReviewBranchAction::Copy(name) => cx.write_to_clipboard(ClipboardItem::new_string(name.clone())),
            _ if self.git_busy => return,
            ReviewBranchAction::New => self.open_new_branch_dialog(window, cx),
            ReviewBranchAction::Merge => self.open_merge_dialog(window, cx),
            ReviewBranchAction::Select(name) => {
                let branches = threadlane_ui_kit::review_branches(self.git_status.as_ref(), "");
                if !branches.iter().any(|branch| branch.name == *name && !branch.is_current) { return; }
                if self.git_status.as_ref().is_some_and(|status| !status.files.is_empty()) {
                    self.switch_target_branch = Some(name.clone());
                    self.switch_dialog_open = true;
                    self.switch_stash_mode = true;
                } else {
                    self.run_git_action(GitAction::Checkout(name.clone()), window, cx);
                    self.branch_popover_open = false;
                }
            }
            ReviewBranchAction::Delete(name) => {
                if let Some(project) = project { self.confirm_delete_branch(project.clone(), name.clone(), window, cx); }
            }
        }
        cx.notify();
    }

    fn can_delete_branch(&self, project: &Path, branch: &str) -> bool {
        !self.git_busy
            && self.project.as_deref() == Some(project)
            && self.git_status.as_ref().is_some_and(|status| {
                status.branch.as_deref() != Some(branch)
                    && status.default_branch.as_deref() != Some(branch)
                    && status.branch_details.iter().any(|info| {
                        info.name == branch && !info.is_current && !info.is_default && !info.is_remote
                    })
            })
    }

    fn confirm_delete_branch(
        &mut self,
        project: PathBuf,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_delete_branch(&project, &branch) {
            return;
        }
        let panel = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let panel = panel.clone();
            let project = project.clone();
            let branch = branch.clone();
            threadlane_ui_kit::review_delete_branch_alert(alert, &branch, &project.display().to_string())
                .on_ok(move |_, window, cx| {
                    let _ = panel.update(cx, |panel, cx| {
                        // Recheck after confirmation: the panel may now show another project.
                        if panel.can_delete_branch(&project, &branch) {
                            panel.run_git_action(GitAction::DeleteBranch(branch.clone()), window, cx);
                        }
                    });
                    true
                })
        });
    }

    #[cfg(test)]
    fn render_branch_section(&self, title: &'static str, branches: Vec<GitBranchInfo>, cx: &mut Context<Self>) -> impl IntoElement {
        let project = self.project.clone();
        threadlane_ui_kit::review_branch_section(title, branches, self.git_busy, self.project.is_some(),
            cx.listener(move |this, action: &threadlane_ui_kit::ReviewBranchAction, window, cx| {
                this.handle_review_branch_action(&project, action, window, cx);
            }), cx)
    }

    fn close_all_git_dialogs(&mut self) {
        self.new_branch_dialog_open = false;
        self.merge_dialog_open = false;
        self.merge_selected_branch = None;
        self.switch_dialog_open = false;
        self.switch_target_branch = None;
        self.stash_dialog_open = false;
    }

    pub fn sync_git_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.new_branch_dialog_open
            || self.merge_dialog_open
            || self.switch_dialog_open
            || self.stash_dialog_open;
        if open == self.git_dialog_presented {
            return;
        }
        self.git_dialog_presented = open;
        if !open {
            window.close_dialog(cx);
            return;
        }
        let panel = cx.entity().downgrade();
        let kind = if self.new_branch_dialog_open { threadlane_ui_kit::ReviewGitDialog::NewBranch }
            else if self.merge_dialog_open { threadlane_ui_kit::ReviewGitDialog::Merge }
            else if self.stash_dialog_open { threadlane_ui_kit::ReviewGitDialog::Stash }
            else { threadlane_ui_kit::ReviewGitDialog::Switch };
        let target = self.switch_target_branch.clone();
        window.open_dialog(cx, move |dialog, window, cx| {
            let close_panel = panel.clone();
            let content = panel
                .update(cx, |panel, cx| panel.render_git_dialog_layer(cx))
                .ok()
                .flatten();
            threadlane_ui_kit::review_git_dialog(dialog, kind, target.as_deref(), window)
                .children(content)
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    let _ = close_panel.update(cx, |panel, cx| {
                        panel.git_dialog_presented = false;
                        panel.close_all_git_dialogs();
                        cx.notify();
                    });
                })
        });
    }

    fn render_git_dialog_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.new_branch_dialog_open {
            Some(self.render_new_branch_dialog(cx).into_any_element())
        } else if self.merge_dialog_open {
            Some(self.render_merge_dialog(cx).into_any_element())
        } else if self.switch_dialog_open {
            Some(self.render_switch_branch_dialog(cx).into_any_element())
        } else if self.stash_dialog_open {
            Some(self.render_stash_dialog(cx).into_any_element())
        } else {
            None
        }
    }

    fn review_git_form_state(&self) -> threadlane_ui_kit::ReviewGitFormState<'_> {
        threadlane_ui_kit::ReviewGitFormState {
            current_branch: self.git_status.as_ref().and_then(|status| status.branch.as_deref()).unwrap_or("main"),
            target_branch: self.switch_target_branch.as_deref(), selected_merge: self.merge_selected_branch.as_deref(),
            busy: self.git_busy, stash_changes: self.switch_stash_mode, include_untracked: self.stash_include_untracked,
        }
    }

    fn handle_review_git_form_action(&mut self, action: &threadlane_ui_kit::ReviewGitFormAction, window: &mut Window, cx: &mut Context<Self>) {
        use threadlane_ui_kit::ReviewGitFormAction;
        match action {
            ReviewGitFormAction::Close => { if !self.git_busy || self.stash_dialog_open { self.close_all_git_dialogs(); } }
            _ if self.git_busy => return,
            ReviewGitFormAction::IncludeUntracked(include) => self.stash_include_untracked = *include,
            ReviewGitFormAction::SwitchStash(stash) => self.switch_stash_mode = *stash,
            ReviewGitFormAction::SelectMerge(name) => self.merge_selected_branch = Some(name.clone()),
            ReviewGitFormAction::Create => {
                let name = self.new_branch_name_input.read(cx).value().trim().to_string();
                if !name.is_empty() { self.run_git_action(GitAction::CreateBranch(name), window, cx); self.close_all_git_dialogs(); }
            }
            ReviewGitFormAction::Merge => {
                if let Some(name) = self.merge_selected_branch.clone() { self.run_git_action(GitAction::Merge(name), window, cx); self.close_all_git_dialogs(); }
            }
            ReviewGitFormAction::Switch => {
                if let Some(target) = self.switch_target_branch.clone() {
                    let action = if self.switch_stash_mode { GitAction::CheckoutStash(target) } else { GitAction::CheckoutCarry(target) };
                    self.run_git_action(action, window, cx); self.close_all_git_dialogs(); self.branch_popover_open = false;
                }
            }
            ReviewGitFormAction::Stash => {
                let message = self.stash_message_input.read(cx).value().trim().to_string();
                self.run_git_action(GitAction::StashPush { message: (!message.is_empty()).then_some(message), include_untracked: self.stash_include_untracked }, window, cx);
                self.close_all_git_dialogs();
            }
        }
        cx.notify();
    }

    fn render_stash_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        threadlane_ui_kit::review_stash_form(&self.stash_message_input, &self.review_git_form_state(),
            cx.listener(|this, action: &threadlane_ui_kit::ReviewGitFormAction, window, cx| this.handle_review_git_form_action(action, window, cx)), cx)
    }

    fn render_new_branch_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        threadlane_ui_kit::review_new_branch_form(&self.new_branch_name_input, &self.review_git_form_state(),
            cx.listener(|this, action: &threadlane_ui_kit::ReviewGitFormAction, window, cx| this.handle_review_git_form_action(action, window, cx)), cx)
    }

    fn render_merge_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        threadlane_ui_kit::review_merge_form(&self.merge_filter_input, self.git_status.as_ref(), &self.review_git_form_state(),
            cx.listener(|this, action: &threadlane_ui_kit::ReviewGitFormAction, window, cx| this.handle_review_git_form_action(action, window, cx)), cx)
    }

    fn render_switch_branch_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        threadlane_ui_kit::review_switch_form(&self.review_git_form_state(),
            cx.listener(|this, action: &threadlane_ui_kit::ReviewGitFormAction, window, cx| this.handle_review_git_form_action(action, window, cx)), cx)
    }

    fn render_review_error(&self, error: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let details = error.to_owned();
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .p_6()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("Couldn't load Git status."),
            )
            .child(
                div()
                    .max_w(rems(24.0))
                    .text_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(error.to_owned()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("review-error-retry")
                            .label("Retry")
                            .small()
                            .tooltip("Reload Git status")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.refresh_active_surface(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("review-error-copy")
                            .label("Copy details")
                            .ghost()
                            .small()
                            .tooltip("Copy full error to clipboard")
                            .on_click(move |_event, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(details.clone()));
                                window.push_notification(
                                    Notification::info("Copied error details"),
                                    cx,
                                );
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_empty(&self, title: &str, description: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(title.to_string()),
            )
            .child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(description.to_string()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("right-panel-recreate-worktree")
                            .label("Recreate worktree")
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.update(cx, |state, cx| {
                                    threadlane_ui_state::controller::dispatch(
                                        state,
                                        threadlane_ui_state::actions::AppAction::RecreateActiveWorktree,
                                    );
                                    cx.notify();
                                });
                            })),
                    )
                    .child(
                        Button::new("right-panel-use-local")
                            .label("Use project folder")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.update(cx, |state, cx| {
                                    if let Some(work_dir) = state.active_work_dir.clone() {
                                        threadlane_ui_state::controller::dispatch(
                                            state,
                                            threadlane_ui_state::actions::AppAction::SelectDraftProject(work_dir),
                                        );
                                    }
                                    cx.notify();
                                });
                            })),
                    ),
            )
            .into_any_element()
    }
}

impl EventEmitter<threadlane_ui_kit::EditorSelectionRequest> for RightPanelView {}

impl Render for RightPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_project(cx);
        for notification in self.pending_git_notifications.drain(..) {
            window.push_notification(notification, cx);
        }
        if let Some(message) = self.generated_commit_message.take() {
            self.commit_message_input
                .update(cx, |input, cx| input.set_value(message, window, cx));
        }
        if self.should_clear_commit_message {
            self.should_clear_commit_message = false;
            self.commit_message_input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        if let Some((project, relative_path)) = self
            .model
            .update(cx, |state, _| state.requested_panel_document.take())
        {
            self.start_panel_document_read(project, relative_path, cx);
        }
        self.sync_pending_document(window, cx);
        let theme = cx.theme().colors;
        let unavailable_non_browser = self.worktree_unavailable
            && !matches!(
                self.active_surface,
                Some(Surface::Browser | Surface::Agents | Surface::Trajectory)
            );
        let body = if unavailable_non_browser {
            self.render_empty(
                "Worktree unavailable",
                "This worktree is not checked out",
                cx,
            )
        } else {
            match self.active_surface {
                None => self.render_chooser(cx).into_any_element(),
                Some(Surface::Trajectory) => self
                    .trajectory_view
                    .clone()
                    .map(|view| view.into_any_element())
                    .unwrap_or_else(|| {
                        self.render_empty("Trajectory", "Trajectory is unavailable", cx)
                    }),
                Some(Surface::Agents) => self.agents.clone().into_any_element(),
                Some(Surface::Review) if self.document_title.is_some() => self.render_files(cx),
                Some(Surface::Review) => self.render_review(window, cx),
                Some(Surface::Files) => self.render_files(cx),
                Some(Surface::Browser) => self.render_browser(window, cx),
            }
        };
        div()
            .w_full()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(self.render_header(cx))
            .when(self.active_surface == Some(Surface::Review), |panel| {
                panel.child(self.render_workspace_context(cx))
            })
            .child(body)
    }
}

/// One bridge-pump step: either a finished reply or a pending script
/// evaluation/snapshot/wait whose channel the pump awaits without blocking the UI.
enum BrowserReply {
    Ready(Result<String, String>),
    PendingEval(tokio::sync::oneshot::Receiver<String>),
    PendingSnapshot(tokio::sync::oneshot::Receiver<Result<(Vec<u8>, u32, u32), String>>),
    PendingWait {
        selector: Option<String>,
        text: Option<String>,
        deadline: std::time::Instant,
    },
}

/// Cap for evaluated script results. Snapshot JSON keeps url/title/count up
/// front so a cut tail still orients the model.
const MAX_BROWSER_EVAL_CHARS: usize = 8_000;

/// Whether a browser command should force the panel open on the Browser
/// surface (commands that visibly change the page).
// Keep this aligned with the surface switches in the browser command handlers.
fn browser_command_reveals_surface(command: &threadlane_protocol::browser::BrowserCommand) -> bool {
    use threadlane_protocol::browser::{BrowserCommand, BrowserTabAction};
    matches!(command,
        BrowserCommand::Tabs { action: BrowserTabAction::Open { .. } | BrowserTabAction::Select { .. } | BrowserTabAction::Close { .. } }
        | BrowserCommand::Navigate { .. }
        | BrowserCommand::Back
        | BrowserCommand::Screenshot
        | BrowserCommand::Wait { .. }
    )
}

/// Dispatches a `BrowserCommand` from the agent bridge: immediate replies,
/// or a pending eval/snapshot/wait the pump resolves later.
fn start_browser_request(
    panel: &mut RightPanelView,
    command: threadlane_protocol::browser::BrowserCommand,
    window: &mut Window,
    cx: &mut Context<RightPanelView>,
) -> BrowserReply {
    use threadlane_protocol::browser::BrowserCommand;
    // open_surface closes the document. Refuse before mutating any tabs or pages,
    // rather than silently dropping an unsaved editor buffer to reveal the browser.
    if panel.is_dirty && browser_command_reveals_surface(&command) {
        return BrowserReply::Ready(Err(
            "The editor has unsaved changes. Ask the user to save or discard them before switching to the browser.".into(),
        ));
    }
    panel.ensure_browser(window, cx);
    match command {
        BrowserCommand::Screenshot => {
            #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
            {
                panel.open_surface(Surface::Browser, cx);
                let Some(browser) = panel.browser.clone() else {
                    return BrowserReply::Ready(Err("The browser panel is not ready.".to_string()));
                };
                match browser.update(cx, |browser, cx| browser.take_snapshot(None, cx)) {
                    Ok(rx) => BrowserReply::PendingSnapshot(rx),
                    Err(err) => BrowserReply::Ready(Err(err)),
                }
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            {
                BrowserReply::Ready(Err(
                    "The embedded browser is not supported on this platform.".to_string()
                ))
            }
        }
        BrowserCommand::Wait {
            selector,
            text,
            timeout_ms,
        } => {
            panel.open_surface(Surface::Browser, cx);
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms.max(100));
            BrowserReply::PendingWait {
                selector,
                text,
                deadline,
            }
        }
        BrowserCommand::ConsoleLogs { clear, level } => {
            let script = super::browser::drain_console_logs_js(clear, &level);
            match panel.start_browser_eval(&script, cx) {
                Ok(rx) => BrowserReply::PendingEval(rx),
                Err(error) => BrowserReply::Ready(Err(error)),
            }
        }
        _ => {
            let script = match &command {
                BrowserCommand::Snapshot => Some(super::browser::snapshot_js()),
                BrowserCommand::Act {
                    action,
                    target,
                    text,
                    key,
                } => {
                    let target_json = match target {
                        threadlane_protocol::browser::ActTarget::Ref(number) => {
                            serde_json::json!({"ref": number, "selector": serde_json::Value::Null})
                        }
                        threadlane_protocol::browser::ActTarget::Selector(selector) => {
                            serde_json::json!({"ref": serde_json::Value::Null, "selector": selector})
                        }
                    }
                    .to_string();
                    let text_json = serde_json::to_string(text).unwrap_or_else(|_| "null".into());
                    let key_json = serde_json::to_string(key).unwrap_or_else(|_| "null".into());
                    Some(super::browser::act_script(
                        action,
                        &target_json,
                        &text_json,
                        &key_json,
                    ))
                }
                BrowserCommand::Evaluate { script } => {
                    Some(super::browser::evaluate_script_wrap(script))
                }
                _ => None,
            };
            match script {
                Some(script) => match panel.start_browser_eval(&script, cx) {
                    Ok(rx) => BrowserReply::PendingEval(rx),
                    Err(error) => BrowserReply::Ready(Err(error)),
                },
                None => BrowserReply::Ready(panel.apply_browser_command(command, window, cx)),
            }
        }
    }
}

/// Shapes a script-eval reply for the agent: pretty-prints the captured
/// console-log ring buffer and truncates oversized payloads.
fn finalize_browser_eval(payload: &str) -> String {
    let inner = super::browser::unwrap_callback_payload(payload);
    // Format console logs if this payload is from drain_console_logs_js
    if inner.contains("\"logs\":[") && inner.contains("\"count\":") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&inner) {
            if let Some(logs) = v.get("logs").and_then(|a| a.as_array()) {
                if logs.is_empty() {
                    return "No console errors or warnings recorded on the current page."
                        .to_string();
                }
                let mut out = format!("Recorded console messages ({}):\n", logs.len());
                for log in logs {
                    let level = log
                        .get("level")
                        .and_then(|s| s.as_str())
                        .unwrap_or("log")
                        .to_uppercase();
                    let msg = log.get("message").and_then(|s| s.as_str()).unwrap_or("");
                    let line_info = match (
                        log.get("source").and_then(|s| s.as_str()),
                        log.get("line").and_then(|l| l.as_i64()),
                    ) {
                        (Some(src), Some(l)) => format!(" ({src}:{l})"),
                        (Some(src), None) => format!(" ({src})"),
                        _ => String::new(),
                    };
                    out.push_str(&format!("- [{level}]{line_info} {msg}\n"));
                }
                return out.trim_end().to_string();
            }
        }
    }
    if inner.chars().count() <= MAX_BROWSER_EVAL_CHARS {
        return inner;
    }
    let head: String = inner.chars().take(MAX_BROWSER_EVAL_CHARS).collect();
    format!("{head}\n[... browser result truncated to {MAX_BROWSER_EVAL_CHARS} characters ...]")
}

/// WebKitGTK snapshots are PNG, WKWebView's are JPEG — name files and data
/// URLs after the actual bytes rather than the producing platform.
fn snapshot_file_ext(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else {
        "jpg"
    }
}

/// A `data:` URL for snapshot bytes, with the mime sniffed from the
/// image magic bytes rather than assumed per platform.
fn base64_data_url(bytes: &[u8]) -> String {
    use base64::Engine as _;
    let mime = if snapshot_file_ext(bytes) == "png" {
        "image/png"
    } else {
        "image/jpeg"
    };
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn convert_node_to_tree_item(node: FileNode, expanded_paths: &HashSet<String>) -> TreeItem {
    let is_expanded = expanded_paths.contains(&node.relative_path);
    if node.is_dir {
        let children = node
            .children
            .into_iter()
            .map(|child| convert_node_to_tree_item(child, expanded_paths))
            .collect::<Vec<_>>();
        TreeItem::new(node.relative_path, node.name)
            .expanded(is_expanded)
            .children(children)
    } else {
        TreeItem::new(node.relative_path, node.name)
    }
}

/// Reconciles the review file selection against a fresh scan.
///
/// The first refresh selects everything so an unreviewed tree starts fully
/// checked; later refreshes only drop selected paths that disappeared, so an
/// explicit empty selection is preserved rather than re-defaulted.
pub(crate) fn retain_review_selection(
    selected: &mut HashSet<String>,
    available: HashSet<String>,
    initialized: &mut bool,
) {
    if !*initialized {
        *selected = available;
        *initialized = true;
    } else {
        selected.retain(|path| available.contains(path));
    }
}

/// Maps a panel `GitAction` onto the wire-level `GitOperation` the daemon
/// executes (protocol v3). The daemon re-inspects afterwards, so the UI
/// only needs the operation — status and messages come back in
/// `GitActionOutcome`.
fn git_action_to_operation(
    action: &GitAction,
    message: String,
    selected_paths: Vec<String>,
) -> threadlane_protocol::repo::GitOperation {
    use threadlane_protocol::repo::{CheckoutMode, GitOperation};
    match action {
        GitAction::Commit | GitAction::CommitAndPush => GitOperation::Commit {
            message,
            selected_paths,
            push: matches!(action, GitAction::CommitAndPush),
        },
        GitAction::Push => GitOperation::Push,
        GitAction::Pull => GitOperation::Pull,
        GitAction::Fetch => GitOperation::Fetch,
        GitAction::StageAll => GitOperation::StageAll,
        GitAction::UnstageAll => GitOperation::UnstageAll,
        GitAction::CreatePullRequest => GitOperation::CreatePullRequest,
        GitAction::Checkout(branch) => GitOperation::Checkout {
            branch: branch.clone(),
            mode: CheckoutMode::Clean,
        },
        GitAction::CheckoutStash(branch) => GitOperation::Checkout {
            branch: branch.clone(),
            mode: CheckoutMode::Stash,
        },
        GitAction::CheckoutCarry(branch) => GitOperation::Checkout {
            branch: branch.clone(),
            mode: CheckoutMode::Carry,
        },
        GitAction::CreateBranch(branch) => GitOperation::CreateBranch {
            name: branch.clone(),
        },
        GitAction::DeleteBranch(branch) => GitOperation::DeleteBranch {
            branch: branch.clone(),
            force: false,
        },
        GitAction::Merge(branch) => GitOperation::Merge {
            branch: branch.clone(),
        },
        GitAction::PopStash(index) => GitOperation::PopStash { index: *index },
        GitAction::DropStash(index) => GitOperation::DropStash { index: *index },
        GitAction::DiscardFile(path) => GitOperation::Discard {
            paths: vec![path.clone()],
        },
        GitAction::DiscardFiles(paths) => GitOperation::Discard {
            paths: paths.clone(),
        },
        GitAction::DiscardAll => GitOperation::DiscardAll,
        GitAction::IgnoreFile(path) => GitOperation::IgnoreFile {
            path: path.clone(),
        },
        GitAction::IgnoreExtension(ext) => GitOperation::IgnoreExtension {
            extension: ext.clone(),
        },
        GitAction::StageFile(path) => GitOperation::Stage {
            paths: vec![path.clone()],
        },
        GitAction::UnstageFile(path) => GitOperation::Unstage {
            paths: vec![path.clone()],
        },
        GitAction::StageFiles(paths) => GitOperation::Stage {
            paths: paths.clone(),
        },
        GitAction::UnstageFiles(paths) => GitOperation::Unstage {
            paths: paths.clone(),
        },
        GitAction::StashPush {
            message,
            include_untracked,
        } => GitOperation::StashPush {
            message: message.clone(),
            include_untracked: *include_untracked,
        },
    }
}

#[cfg(test)]
mod dialog_keyboard_tests {
    use super::RightPanelView;
    use gpui::{
        AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, Render, Role,
        ParentElement, StatefulInteractiveElement, Styled, TestAppContext, Window, div,
    };
    use gpui_component::{Root, WindowExt};
    use threadlane_ui_state::AppState;

    struct Host {
        panel: Entity<RightPanelView>,
        trigger: FocusHandle,
    }
    impl Render for Host {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.panel
                .update(cx, |panel, cx| panel.sync_git_dialog(window, cx));
            div()
                .id("dialog-host")
                .track_focus(&self.trigger)
                .role(Role::Application)
                .tab_group()
                .size_full()
                .child(self.panel.update(cx, |panel, cx| {
                    let branches = panel.git_status.as_ref()
                        .map(|s| s.branch_details.clone()).unwrap_or_default();
                    panel.render_branch_section("BRANCHES", branches, cx)
                        .into_any_element()
                }))
        }
    }

    #[gpui::test]
    fn branch_deletion_requires_confirmation_and_keeps_project_scope(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| AppState::default());
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let panel = cx.new(|cx| RightPanelView::new(model, window, cx));
            let trigger = cx.focus_handle();
            trigger.focus(window, cx);
            *capture.borrow_mut() = Some(panel.clone());
            Root::new(cx.new(|_| Host { panel, trigger }), window, cx)
        });
        let panel = captured.borrow_mut().take().unwrap();
        let project = std::path::PathBuf::from("/test/project");
        cx.update(|window, cx| {
            panel.update(cx, |panel, cx| {
                panel.project = Some(project.clone());
                panel.git_status = Some(threadlane_git::GitStatus {
                    branch: Some("current".into()),
                    branch_details: vec![
                        threadlane_git::GitBranchInfo { name: "main".into(), is_default: true, ..Default::default() },
                        threadlane_git::GitBranchInfo { name: "current".into(), is_current: true, ..Default::default() },
                        threadlane_git::GitBranchInfo { name: "feature".into(), ..Default::default() },
                        threadlane_git::GitBranchInfo {
                            name: "origin/feature".into(),
                            is_remote: true,
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                });
                for branch in ["main", "current", "missing", "origin/feature"] {
                    assert!(!panel.can_delete_branch(&project, branch));
                    panel.confirm_delete_branch(project.clone(), branch.into(), window, cx);
                    assert!(!window.has_active_dialog(cx));
                }
                assert!(!panel.can_delete_branch(std::path::Path::new("/other"), "feature"));
                panel.git_busy = true;
                assert!(!panel.can_delete_branch(&project, "feature"));
                panel.git_busy = false;
                assert!(panel.can_delete_branch(&project, "feature"));
                cx.notify();
            });
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let row = cx.debug_bounds("branch-row-feature").unwrap();
        for keys in ["down enter", "down down enter"] {
            cx.simulate_event(gpui::MouseDownEvent {
                button: gpui::MouseButton::Right,
                position: row.center(),
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            });
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.simulate_keystrokes(keys);
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
                assert!(
                    !panel.read(cx).git_busy,
                    "right-click must not checkout or delete"
                );
                if keys == "down enter" {
                    assert_eq!(
                        cx.read_from_clipboard().unwrap().text().as_deref(),
                        Some("feature")
                    );
                    assert!(!window.has_active_dialog(cx));
                } else {
                    assert!(window.has_active_dialog(cx));
                }
            });
        }
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.update(|window, cx| {
            assert!(!window.has_active_dialog(cx));
            assert!(!panel.read(cx).git_busy);
            panel.update(cx, |panel, cx| {
                panel.confirm_delete_branch(project.clone(), "feature".into(), window, cx);
                panel.project = Some("/test/other-project".into());
            });
            window.draw(cx).clear(cx);
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.update(|window, cx| {
            assert!(!window.has_active_dialog(cx));
            assert!(
                !panel.read(cx).git_busy,
                "stale confirmation must not run Git in another project"
            );
        });
    }
    #[gpui::test]
    fn git_dialogs_dismiss_with_escape_and_restore_focus(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| AppState::default());
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let panel = cx.new(|cx| RightPanelView::new(model, window, cx));
            let trigger = cx.focus_handle();
            trigger.focus(window, cx);
            *capture.borrow_mut() = Some((panel.clone(), trigger.clone()));
            Root::new(cx.new(|_| Host { panel, trigger }), window, cx)
        });
        let (panel, trigger) = captured.borrow_mut().take().unwrap();
        for kind in 0..3 {
            cx.update(|window, cx| {
                panel.update(cx, |panel, cx| {
                    panel.new_branch_dialog_open = kind == 0;
                    panel.merge_dialog_open = kind == 1;
                    panel.switch_dialog_open = kind == 2;
                    panel.switch_target_branch = Some("test-target".into());
                    panel.sync_git_dialog(window, cx);
                })
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
                assert!(window.has_active_dialog(cx));
            });
            if kind == 2 {
                cx.update(|window, cx| {
                    window.focus_next(cx); // Stash
                    window.focus_next(cx); // Carry
                    window.draw(cx).clear(cx);
                });
                let keystroke = gpui::Keystroke::parse("space").unwrap();
                cx.simulate_event(gpui::KeyDownEvent {
                    keystroke: keystroke.clone(),
                    is_held: false,
                    prefer_character_input: false,
                });
                cx.simulate_event(gpui::KeyUpEvent { keystroke });
                panel.read_with(cx, |panel, _| {
                    assert!(!panel.switch_stash_mode, "Carry is keyboard selectable");
                    assert!(!panel.git_busy, "choosing a mode does not switch branches");
                });
            }
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
            cx.update(|window, cx| {
                assert!(!window.has_active_dialog(cx));
                assert!(trigger.is_focused(window));
                let panel = panel.read(cx);
                assert!(
                    !panel.new_branch_dialog_open
                        && !panel.merge_dialog_open
                        && !panel.switch_dialog_open
                );
                assert!(panel.switch_target_branch.is_none());
                assert!(!panel.git_busy, "dismissing a dialog must not run Git");
            });
        }
    }
}

#[cfg(test)]
mod review_layout_tests {
    use super::{available_surfaces, RightPanelView};
    use gpui::{
        AppContext, Context, Entity, IntoElement, ListSizingBehavior, ParentElement, Render, Styled,
        TestAppContext, Window, div, list, px,
    };
    use threadlane_git::GitFile;
    use threadlane_ui_state::AppState;

    struct RowHost {
        panel: Entity<RightPanelView>,
        width: f32,
    }

    struct SurfaceHost {
        panel: Entity<RightPanelView>,
        width: f32,
    }

    impl Render for SurfaceHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.panel.update(cx, |panel, cx| {
                div()
                    .w(px(self.width))
                    .h(px(600.0))
                    .flex()
                    .flex_col()
                    .child(panel.render_header(cx))
                    .child(panel.render_chooser(cx))
            })
        }
    }

    #[gpui::test]
    fn surface_controls_fit_narrow_panels(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| AppState::default());
        let (host, cx) = cx.add_window_view(move |window, cx| SurfaceHost {
            panel: cx.new(|cx| RightPanelView::new(model, window, cx)),
            width: 280.0,
        });
        for rem_size in [16.0, 20.0] {
            for width in [280.0, 320.0, 480.0] {
                host.update(cx, |host, cx| {
                    host.width = width;
                    cx.notify();
                });
                cx.update(|window, cx| {
                    window.set_rem_size(px(rem_size));
                    window.draw(cx).clear(cx);
                });
                let title = cx.debug_bounds("right-panel-title").expect("tool title rendered");
                assert!(
                    title.left() >= px(0.0) && title.right() <= px(width),
                    "tool title overflows at width {width}, rem {rem_size}: {title:?}"
                );
                let mut previous_choice = None;
                for (tab, choice) in [
                    ("right-panel-tab-Trajectory", "right-panel-choice-Trajectory"),
                    ("right-panel-tab-Agents", "right-panel-choice-Agents"),
                    ("right-panel-tab-Review", "right-panel-choice-Review"),
                    ("right-panel-tab-Files", "right-panel-choice-Files"),
                    ("right-panel-tab-Browser", "right-panel-choice-Browser"),
                ].into_iter().take(available_surfaces().len()) {
                    // Nothing is open yet, so the chooser is the only control.
                    assert!(cx.debug_bounds(tab).is_none(), "{tab} repeats the chooser");
                    for selector in [choice] {
                        let bounds = cx.debug_bounds(selector).expect("surface control rendered");
                        assert!(bounds.left() >= px(0.0) && bounds.right() <= px(width),
                            "{selector} overflows at width {width}, rem {rem_size}: {bounds:?}");
                        if selector == choice {
                            if let Some(bottom) = previous_choice {
                                assert!(bounds.top() >= bottom, "surface choices overlap");
                            }
                            previous_choice = Some(bounds.bottom());
                        }
                    }
                }
            }
        }
    }

    impl Render for RowHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().w(px(self.width)).h(px(200.0)).child(
                self.panel.update(cx, |panel, cx| {
                    list(
                        panel.review_files_list_state.clone(),
                        cx.processor(RightPanelView::render_review_file_row),
                    )
                    .size_full()
                    .with_sizing_behavior(ListSizingBehavior::Auto)
                }),
            )
        }
    }

    #[gpui::test]
    fn long_review_paths_keep_filename_and_stats_visible(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| AppState::default());
        let (host, cx) = cx.add_window_view(move |window, cx| {
            let panel = cx.new(|cx| RightPanelView::new(model, window, cx));
            panel.update(cx, |panel, _| {
                let mut file = GitFile::default();
                file.path = format!("crates/{}/tests.rs", "long-directory-name/".repeat(12));
                file.additions = 1234;
                file.deletions = 567;
                panel.review_files = vec![file];
                panel
                    .review_files_list_state
                    .reset_with_uniform_height(1, px(32.0));
            });
            RowHost {
                panel,
                width: 320.0,
            }
        });
        for path in [
            format!("crates/{}tests.rs", "long-directory-name/".repeat(12)),
            format!("src/{}.rs", "long-filename".repeat(12)),
            format!("{}.rs", "root-filename".repeat(12)),
            "Cargo.toml".to_owned(),
        ] {
            for width in [320.0, 480.0, 640.0] {
                host.update(cx, |host, cx| {
                    host.width = width;
                    host.panel.update(cx, |panel, cx| {
                        panel.review_files[0].path = path.clone();
                        cx.notify();
                    });
                    cx.notify();
                });
                cx.update(|window, cx| window.draw(cx).clear(cx));
                let row = cx.debug_bounds("review-file-row").expect("row rendered");
                assert_eq!(row.size.width, px(width - 16.0), "rows must share the panel inset");
                assert_eq!(row.size.height, px(32.0), "row height changed at {width}");
                let filename = cx
                    .debug_bounds("review-filename")
                    .expect("filename rendered");
                let status = cx
                    .debug_bounds("review-file-status")
                    .expect("status rendered");
                let stats = cx
                    .debug_bounds("review-file-stats")
                    .expect("stats rendered");
                assert!(
                    filename.size.height <= row.size.height,
                    "filename wraps beyond row height at {width}: {filename:?}"
                );
                assert!(
                    filename.size.width >= px(40.0),
                    "filename collapsed at {width}: {filename:?}"
                );
                assert!(
                    filename.right() <= status.left(),
                    "filename overlaps status at {width}"
                );
                assert!(
                    status.right() <= stats.left(),
                    "status overlaps stats at {width}"
                );
                assert!(
                    stats.right() <= px(width),
                    "stats overflow at {width}: {stats:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod browser_editor_safety_tests {
    use super::{browser_command_reveals_surface, start_browser_request, BrowserReply, RightPanelView, Surface};
    use gpui::{AppContext, TestAppContext};
    use threadlane_protocol::browser::{BrowserCommand, BrowserTabAction};
    use threadlane_ui_state::AppState;

    #[test]
    fn listing_tabs_does_not_reveal_browser() {
        assert!(!browser_command_reveals_surface(&BrowserCommand::Tabs { action: BrowserTabAction::List }));
    }

    #[gpui::test]
    fn retained_browser_panel_hides_on_other_workspace_pages(cx: &mut TestAppContext) {
        use threadlane_ui_state::WorkspacePage;

        cx.update(gpui_component::init);
        let model = cx.new(|_| AppState::default());
        let retained = model.clone();
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let panel = cx.new(|cx| RightPanelView::new(model, window, cx));
            *capture.borrow_mut() = Some(panel.clone());
            gpui_component::Root::new(panel, window, cx)
        });
        let panel = captured.borrow_mut().take().expect("mounted panel");
        panel.update(cx, |panel, _| panel.active_surface = Some(Surface::Browser));
        for page in [
            WorkspacePage::Chat,
            WorkspacePage::GitHub,
            WorkspacePage::Automations,
            WorkspacePage::Settings,
            WorkspacePage::Chat,
        ] {
            retained.update(cx, |state, cx| {
                state.workspace_page = page;
                cx.notify();
            });
            panel.update(cx, |panel, cx| {
                panel.set_visible(true, cx);
                assert_eq!(panel.visible, page == WorkspacePage::Chat);
                assert_eq!(
                    panel.active_surface, Some(Surface::Browser),
                    "returning to chat must retain the selected tool"
                );
            });
        }
        panel.update(cx, |panel, cx| {
            panel.set_visible(false, cx);
            assert!(!panel.visible);
        });
    }

    #[gpui::test]
    fn browser_commands_preserve_dirty_document(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| AppState::default());
        let (panel, cx) = cx.add_window_view(|window, cx| RightPanelView::new(model, window, cx));
        panel.update_in(cx, |panel, window, cx| {
            for surface in [Surface::Review, Surface::Files] {
                panel.active_surface = Some(surface);
                panel.pending_document = Some(("draft.rs".into(), "unsaved buffer".into()));
                panel.sync_pending_document(window, cx);
                panel.is_dirty = true;
                let editor = panel.editor_state.clone().expect("editor");
                let error = panel.open_terminal_url("http://localhost:3000/", window, cx).unwrap_err();
                assert!(error.contains("Save or discard"));
                assert_eq!(panel.active_surface, Some(surface));
                assert!(panel.is_dirty);
                assert_eq!(panel.editor_state.as_ref(), Some(&editor));
                assert!(panel.browser.is_none());
                for command in [
                    BrowserCommand::Tabs { action: BrowserTabAction::Open { url: "example.com".into() } },
                    BrowserCommand::Tabs { action: BrowserTabAction::Select { tab_id: 1 } },
                    BrowserCommand::Tabs { action: BrowserTabAction::Close { tab_id: 1 } },
                    BrowserCommand::Navigate { url: "example.com".into() },
                    BrowserCommand::Back,
                    BrowserCommand::Screenshot,
                    BrowserCommand::Wait { selector: None, text: None, timeout_ms: 100 },
                ] {
                    let result = start_browser_request(panel, command, window, cx);
                    assert!(matches!(result, BrowserReply::Ready(Err(error)) if error.contains("unsaved changes")));
                    assert_eq!(panel.active_surface, Some(surface));
                    assert!(panel.is_dirty);
                    assert_eq!(panel.editor_state.as_ref(), Some(&editor));
                    assert_eq!(panel.saved_content, "unsaved buffer");
                    assert!(panel.browser.is_none());
                }
            }
        });
    }
}

#[cfg(test)]
mod review_diff_tests {
    use super::{
        GitAction, PanelEvent, ReviewDiffRequest, ReviewDiffState, ReviewDiffTarget,
        RightPanelView, Surface,
    };
    use gpui::{
        AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, TestAppContext,
        Window, div, px,
    };
    use gpui_component::Root;
    use std::path::PathBuf;
    use threadlane_ui_state::AppState;

    fn seed_review_diff(
        panel: &mut RightPanelView,
        project: &std::path::Path,
        content: &str,
        cx: &mut Context<RightPanelView>,
    ) -> ReviewDiffRequest {
        let request = ReviewDiffRequest {
            project: project.to_path_buf(),
            target: ReviewDiffTarget::AllChanges,
            options: threadlane_git::DiffOptions::default(),
            revision: panel.review_diff_revision.wrapping_add(1),
        };
        panel.review_diff_revision = request.revision;
        panel.review_diff_request = Some(request.clone());
        panel.review_diff_state = Some(ReviewDiffState::Ready {
            empty: content.is_empty(),
        });
        panel.document_title = Some(request.target.title());
        panel
            .review_document
            .update(cx, |document, cx| document.set_patch(content, cx));
        request
    }

    struct DiffHost {
        panel: Entity<RightPanelView>,
        width: f32,
    }

    impl Render for DiffHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .flex_col()
                .w(px(self.width))
                .h_96()
                .child(self.panel.update(cx, |panel, cx| panel.render_files(cx)))
        }
    }

    #[gpui::test]
    fn review_menu_rejects_changed_checkout(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| {
            let mut state = AppState::default();
            state.active_session_id = None;
            state.active_work_dir = Some(PathBuf::from("/workspace"));
            state
        });
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let panel = cx.new(|cx| RightPanelView::new(model, window, cx));
            *capture.borrow_mut() = Some(panel.clone());
            Root::new(panel, window, cx)
        });
        let panel = captured.borrow_mut().take().unwrap();
        cx.update(|window, cx| {
            panel.update(cx, |host, _| host.project = Some(PathBuf::from("/new-checkout")));
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("unchanged".into()));
            for action in [threadlane_ui_kit::ReviewFileAction::Stage("src/file.rs".into()),
                threadlane_ui_kit::ReviewFileAction::CopyRelative("src/file.rs".into())] {
                RightPanelView::handle_review_file_action(panel.clone(), Some(PathBuf::from("/workspace")), &action, window, cx);
            }
            assert!(!panel.read(cx).git_busy);
            assert_eq!(cx.read_from_clipboard().unwrap().text().as_deref(), Some("unchanged"));
            RightPanelView::handle_review_file_action(panel.clone(), Some(PathBuf::from("/new-checkout")),
                &threadlane_ui_kit::ReviewFileAction::CopyRelative("src/file.rs".into()), window, cx);
            assert_eq!(cx.read_from_clipboard().unwrap().text().as_deref(), Some("src/file.rs"));
        });
    }

    #[gpui::test]
    fn review_document_resets_for_session_switch_and_leaving_review(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| {
            let mut state = AppState::for_tests();
            let checkout = PathBuf::from("/workspace");
            state.active_session_id = Some("first".into());
            state.active_work_dir = Some(checkout.clone());
            state.projects = vec![threadlane_ui_state::ProjectInfo {
                name: "workspace".into(),
                work_dir: checkout.clone(),
                sessions: ["first", "second"]
                    .into_iter()
                    .map(|id| threadlane_ui_state::SessionInfo {
                        id: id.into(),
                        work_dir: checkout.clone(),
                        runtime_work_dir: checkout.clone(),
                        worktree_available: true,
                        ..Default::default()
                    })
                    .collect(),
                is_expanded: true,
            }];
            state
        });
        let (panel, cx) =
            cx.add_window_view(|window, cx| RightPanelView::new(model.clone(), window, cx));

        panel.update_in(cx, |panel, window, cx| {
            panel.active_surface = Some(Surface::Review);
            let checkout = PathBuf::from("/workspace");
            let initial = seed_review_diff(panel, &checkout, "first session patch", cx);
            assert!(matches!(
                panel.review_diff_state,
                Some(ReviewDiffState::Ready { empty: false })
            ));
            let retained_request = panel.review_diff_request.clone();
            let retained_title = panel.document_title.clone();
            panel.set_visible(false, cx);
            assert_eq!(panel.review_diff_request, retained_request);
            assert_eq!(panel.document_title, retained_title);
            assert!(matches!(
                panel.review_diff_state,
                Some(ReviewDiffState::Ready { empty: false })
            ));
            panel.set_visible(true, cx);
            assert_eq!(panel.review_diff_request, retained_request);
            assert_eq!(panel.document_title, retained_title);
            assert!(matches!(
                panel.review_diff_state,
                Some(ReviewDiffState::Ready { empty: false })
            ));

            model.update(cx, |state, cx| {
                state.active_session_id = Some("second".into());
                cx.notify();
            });
            panel.apply_review_diff_result(
                initial.clone(),
                Err("stale result before observer sync".into()),
                cx,
            );
            assert!(matches!(
                panel.review_diff_state,
                Some(ReviewDiffState::Ready { empty: false })
            ));
            panel.sync_project(cx);
            assert_eq!(
                panel.project.as_deref(),
                Some(std::path::Path::new("/workspace"))
            );
            assert!(panel.review_diff_request.is_none());
            assert!(panel.review_diff_state.is_none());
            assert!(panel.document_title.is_none());
            panel.apply_review_diff_result(
                initial,
                Ok("stale first session patch".into()),
                cx,
            );
            assert!(panel.review_diff_request.is_none());

            let review_request = seed_review_diff(panel, &checkout, "second session patch", cx);
            panel.open_surface(Surface::Agents, cx);
            assert!(panel.review_diff_request.is_none());
            assert!(panel.review_diff_state.is_none());
            assert!(panel.document_title.is_none());
            panel.apply_review_diff_result(
                review_request,
                Ok("stale patch after leaving Review".into()),
                cx,
            );
            assert!(panel.review_diff_request.is_none());
            assert!(panel.review_diff_state.is_none());

            panel.pending_document = Some(("draft.rs".into(), "saved buffer".into()));
            panel.sync_pending_document(window, cx);
            let editor = panel.editor_state.clone().expect("editable file editor");
            panel.is_dirty = true;
            model.update(cx, |state, cx| {
                state.active_session_id = Some("first".into());
                cx.notify();
            });
            panel.sync_project(cx);
            assert_eq!(
                panel.project.as_deref(),
                Some(std::path::Path::new("/workspace"))
            );
            assert!(panel.is_dirty, "same-checkout session switch keeps dirty state");
            assert_eq!(panel.saved_content, "saved buffer");
            assert_eq!(panel.editor_state.as_ref(), Some(&editor));
        });
    }

    #[gpui::test]
    fn review_refresh_invalidates_then_starts_one_diff_after_status(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| {
            let mut state = AppState::default();
            // Default restores a persisted chat; this fixture owns only a project.
            state.active_session_id = None;
            state.active_work_dir = Some(PathBuf::from("/workspace"));
            state
        });
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let panel = cx.new(|cx| RightPanelView::new(model, window, cx));
            *capture.borrow_mut() = Some(panel.clone());
            Root::new(panel, window, cx)
        });
        let panel = captured.borrow_mut().take().unwrap();
        panel.update(cx, |panel, cx| {
            let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
            panel.event_tx = event_tx;
            panel.open_combined_diff(cx);
            panel.set_ignore_whitespace(true, cx);
            let old = panel.review_diff_request.clone().unwrap();
            panel.apply_review_diff_result(old.clone(), Ok("old patch".into()), cx);
            let loads = panel.review_diff_load_count;
            panel.refresh_surface(Surface::Review, cx);
            assert_eq!(panel.review_diff_load_count, loads);
            assert!(panel.pending_document.is_none());
            assert!(matches!(
                panel.review_diff_state,
                Some(ReviewDiffState::Loading)
            ));
            panel.apply_review_diff_result(old, Ok("stale patch".into()), cx);
            assert!(matches!(
                panel.review_diff_state,
                Some(ReviewDiffState::Loading)
            ));
            panel.apply_event(
                PanelEvent::ReviewLoaded {
                    project: PathBuf::from("/workspace"),
                    status: Some(threadlane_git::GitStatus::default()),
                    files: Vec::new(),
                    error: None,
                },
                cx,
            );
            assert_eq!(panel.review_diff_load_count, loads + 1);
            let request = panel.review_diff_request.as_ref().unwrap();
            assert_eq!(request.target, ReviewDiffTarget::AllChanges);
            assert!(request.options.ignore_whitespace);
        });
    }

    #[gpui::test]
    fn review_checkout_failure_retains_target_and_filter(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| {
            let mut state = AppState::default();
            // Default restores a persisted chat; this fixture owns only a project.
            state.active_session_id = None;
            state.active_work_dir = Some(PathBuf::from("/workspace"));
            state
        });
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let panel = cx.new(|cx| RightPanelView::new(model, window, cx));
            *capture.borrow_mut() = Some(panel.clone());
            Root::new(panel, window, cx)
        });
        let panel = captured.borrow_mut().take().unwrap();
        panel.update(cx, |panel, cx| {
            let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
            panel.event_tx = event_tx;
            let status = threadlane_git::GitStatus {
                branch: Some("main".into()),
                ..threadlane_git::GitStatus::default()
            };
            panel.replace_git_status(Some(status.clone()), cx);
            for target in [
                ReviewDiffTarget::AllChanges,
                ReviewDiffTarget::File("selected.txt".into()),
            ] {
                for action in [
                    GitAction::Checkout("other".into()),
                    GitAction::CheckoutStash("other".into()),
                    GitAction::CheckoutCarry("other".into()),
                    GitAction::CreateBranch("other".into()),
                ] {
                    panel.open_review_diff(target.clone(), cx);
                    panel.set_ignore_whitespace(true, cx);
                    let old = panel.review_diff_request.clone().unwrap();
                    let loads = panel.review_diff_load_count;
                    panel.run_git_action_without_window(action, cx);
                    assert!(panel.review_diff_options.ignore_whitespace);
                    assert_eq!(panel.review_diff_request.as_ref().unwrap().target, target);
                    panel.apply_review_diff_result(old, Ok("old checkout patch".into()), cx);
                    assert!(matches!(
                        panel.review_diff_state,
                        Some(ReviewDiffState::Loading)
                    ));
                    panel.set_ignore_whitespace(false, cx);
                    panel.set_ignore_whitespace(true, cx);
                    assert_eq!(panel.review_diff_load_count, loads);
                    let pending = panel.review_diff_request.clone().unwrap();
                    panel.apply_review_diff_result(
                        pending,
                        Ok("pending checkout patch".into()),
                        cx,
                    );
                    assert!(matches!(
                        panel.review_diff_state,
                        Some(ReviewDiffState::Loading)
                    ));
                    panel.apply_event(
                        PanelEvent::ActionFinished {
                            project: PathBuf::from("/workspace"),
                            status: if target == ReviewDiffTarget::AllChanges {
                                Ok(status.clone())
                            } else {
                                Err("status unavailable".into())
                            },
                            action_error: Some("checkout rejected".into()),
                            action_message: None,
                            checkout_succeeded: false,
                        },
                        cx,
                    );
                    assert!(!panel.git_busy);
                    assert_eq!(panel.review_diff_load_count, loads + 1);
                    let retry = panel.review_diff_request.clone().unwrap();
                    assert_eq!(retry.target, target);
                    assert!(retry.options.ignore_whitespace);
                    panel.apply_review_diff_result(
                        retry,
                        Ok("retained checkout patch".into()),
                        cx,
                    );
                    assert!(matches!(
                        panel.review_diff_state,
                        Some(ReviewDiffState::Ready { empty: false })
                    ));
                }
            }
            panel.run_git_action_without_window(GitAction::Checkout("main".into()), cx);
            panel.apply_event(
                PanelEvent::ActionFinished {
                    project: PathBuf::from("/workspace"),
                    status: Err("status unavailable".into()),
                    action_error: None,
                    action_message: None,
                    checkout_succeeded: true,
                },
                cx,
            );
            assert!(!panel.review_diff_options.ignore_whitespace);
            assert!(panel.review_diff_request.is_none());
            assert!(panel.document_title.is_none());
        });
    }

    #[gpui::test]
    fn review_diff_rejects_results_after_toggles_refresh_navigation_and_checkout_change(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| {
            let mut state = AppState::default();
            // Default restores a persisted chat; this fixture owns only a project.
            state.active_session_id = None;
            state.active_work_dir = Some(PathBuf::from("/workspace"));
            state
        });
        let (panel, cx) =
            cx.add_window_view(|window, cx| RightPanelView::new(model.clone(), window, cx));
        panel.update_in(cx, |panel, window, cx| {
            panel.open_file_diff("first.txt".into(), cx);
            let first = panel.review_diff_request.clone().unwrap();
            panel.set_ignore_whitespace(true, cx);
            let filtered = panel.review_diff_request.clone().unwrap();
            panel.apply_review_diff_result(first, Ok("old unfiltered patch".into()), cx);
            assert!(matches!(
                panel.review_diff_state,
                Some(ReviewDiffState::Loading)
            ));
            panel.reload_review_diff(cx);
            panel.apply_review_diff_result(filtered, Ok(String::new()), cx);
            assert!(matches!(
                panel.review_diff_state,
                Some(ReviewDiffState::Loading)
            ));
            let refreshed = panel.review_diff_request.clone().unwrap();
            panel.open_combined_diff(cx);
            panel.apply_review_diff_result(refreshed, Ok(String::new()), cx);
            assert_eq!(
                panel.review_diff_request.as_ref().unwrap().target,
                ReviewDiffTarget::AllChanges
            );
            assert!(matches!(
                panel.review_diff_state,
                Some(ReviewDiffState::Loading)
            ));
            let combined = panel.review_diff_request.clone().unwrap();
            panel.pending_document = Some(("editable.rs".into(), "editable".into()));
            panel.sync_pending_document(window, cx);
            panel.apply_review_diff_result(combined, Ok("stale patch".into()), cx);
            assert!(panel.review_diff_request.is_none());
            assert!(panel.editor_state.is_some());
            panel.open_combined_diff(cx);
            let closing = panel.review_diff_request.clone().unwrap();
            panel.close_document(cx);
            panel.apply_review_diff_result(closing, Ok("stale patch".into()), cx);
            assert!(panel.document_title.is_none());
            assert!(panel.review_diff_state.is_none());
            panel.open_combined_diff(cx);
            let previous_checkout = panel.review_diff_request.clone().unwrap();
            model.update(cx, |state, _| {
                state.active_work_dir = Some(PathBuf::from("/other-worktree"))
            });
            panel.sync_project(cx);
            panel.apply_review_diff_result(previous_checkout, Ok("stale patch".into()), cx);
            assert!(panel.review_diff_request.is_none());
            assert!(!panel.review_diff_options.ignore_whitespace);
            assert!(panel.document_title.is_none());
        });
    }

    #[gpui::test]
    fn review_diff_empty_failure_retry_and_target_retention(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| {
            let mut state = AppState::default();
            // Default restores a persisted chat; this fixture owns only a project.
            state.active_session_id = None;
            state.active_work_dir = Some(PathBuf::from("/workspace"));
            state
        });
        let (panel, cx) = cx.add_window_view(|window, cx| RightPanelView::new(model, window, cx));
        panel.update(cx, |panel, cx| {
            panel.selected_files.insert("selected.txt".into());
            panel.open_combined_diff(cx);
            panel.set_ignore_whitespace(true, cx);
            let request = panel.review_diff_request.clone().unwrap();
            panel.apply_review_diff_result(request, Ok(String::new()), cx);
            assert!(matches!(panel.review_diff_state, Some(ReviewDiffState::Ready { empty: true })));
            panel.reload_review_diff(cx);
            let request = panel.review_diff_request.clone().unwrap();
            let error = threadlane_git::diff_file_with_options(&request.project, "../outside", request.options).unwrap_err();
            panel.apply_review_diff_result(request.clone(), Err(error.to_string()), cx);
            assert!(matches!(&panel.review_diff_state, Some(ReviewDiffState::Failed(error)) if error.contains("outside")));
            panel.reload_review_diff(cx);
            let retry = panel.review_diff_request.clone().unwrap();
            assert!(retry.revision > request.revision);
            assert_eq!(retry.target, ReviewDiffTarget::AllChanges);
            assert!(retry.options.ignore_whitespace);
            assert!(matches!(panel.review_diff_state, Some(ReviewDiffState::Loading)));
            panel.apply_review_diff_result(retry, Ok("Binary files differ".into()), cx);
            assert!(matches!(panel.review_diff_state, Some(ReviewDiffState::Ready { empty: false })));
            panel.set_ignore_whitespace(false, cx);
            panel.open_file_diff("next.txt".into(), cx);
            assert!(!panel.review_diff_options.ignore_whitespace);
            panel.set_ignore_whitespace(true, cx);
            panel.open_file_diff("last.txt".into(), cx);
            assert!(panel.review_diff_request.as_ref().unwrap().options.ignore_whitespace);
            assert!(panel.selected_files.contains("selected.txt"));
            assert!(!panel.git_busy);
            let status = threadlane_git::GitStatus {
                branch: Some("main".into()),
                ..threadlane_git::GitStatus::default()
            };
            panel.replace_git_status(Some(status.clone()), cx);
            panel.replace_git_status(Some(status), cx);
            assert!(panel.review_diff_options.ignore_whitespace);
            let old_branch = panel.review_diff_request.clone().unwrap();
            panel.replace_git_status(Some(threadlane_git::GitStatus {
                branch: Some("other".into()),
                ..threadlane_git::GitStatus::default()
            }), cx);
            panel.apply_review_diff_result(old_branch, Ok("old branch patch".into()), cx);
            assert!(!panel.review_diff_options.ignore_whitespace);
            assert!(panel.review_diff_request.is_none());
            assert!(panel.document_title.is_none());
        });
    }

    #[gpui::test]
    fn review_diff_navigation_follows_filtered_inventory(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| {
            let mut state = AppState::for_tests();
            state.active_session_id = None;
            state.active_work_dir = Some(PathBuf::from("/workspace"));
            state
        });
        let (panel, cx) =
            cx.add_window_view(|window, cx| RightPanelView::new(model.clone(), window, cx));
        panel.update_in(cx, |panel, window, cx| {
            use threadlane_git::GitFile;
            use threadlane_ui_kit::ReviewDiffNavAction;
            let file = |path: &str| GitFile {
                path: path.into(),
                ..Default::default()
            };
            panel.review_files = vec![
                file("a.rs"),
                file("dir/b.rs"),
                file("b.rs"),
                file("z/last.rs"),
            ];
            panel.open_file_diff("a.rs".into(), cx);
            panel.set_ignore_whitespace(true, cx);
            // First file: Previous is a bounded no-op.
            panel.navigate_review_diff(ReviewDiffNavAction::Previous, window, cx);
            assert_eq!(
                panel.review_diff_request.as_ref().unwrap().target,
                ReviewDiffTarget::File("a.rs".into())
            );
            panel.navigate_review_diff(ReviewDiffNavAction::Next, window, cx);
            assert_eq!(
                panel.review_diff_request.as_ref().unwrap().target,
                ReviewDiffTarget::File("dir/b.rs".into())
            );
            // Ignore whitespace survives navigation, and rapid actions while
            // still loading advance from the requested path deterministically.
            assert!(panel
                .review_diff_request
                .as_ref()
                .unwrap()
                .options
                .ignore_whitespace);
            panel.navigate_review_diff(ReviewDiffNavAction::Next, window, cx);
            panel.navigate_review_diff(ReviewDiffNavAction::Next, window, cx);
            assert_eq!(
                panel.review_diff_request.as_ref().unwrap().target,
                ReviewDiffTarget::File("z/last.rs".into())
            );
            // The direction just taken ended at the last file, so the disabled
            // button yielded focus to the stable navigation group.
            assert!(panel.review_nav_focus.is_focused(window));
            panel.navigate_review_diff(ReviewDiffNavAction::Next, window, cx);
            assert_eq!(
                panel.review_diff_request.as_ref().unwrap().target,
                ReviewDiffTarget::File("z/last.rs".into())
            );
            // Exact paths distinguish duplicate basenames.
            panel.navigate_review_diff(ReviewDiffNavAction::Previous, window, cx);
            assert_eq!(
                panel.review_diff_request.as_ref().unwrap().target,
                ReviewDiffTarget::File("b.rs".into())
            );
            panel.navigate_review_diff(ReviewDiffNavAction::Next, window, cx);
            // A refresh that removes the current file disables navigation
            // instead of retargeting a stored row index.
            panel
                .review_files
                .retain(|file| file.path != "z/last.rs");
            panel.navigate_review_diff(ReviewDiffNavAction::Previous, window, cx);
            assert_eq!(
                panel.review_diff_request.as_ref().unwrap().target,
                ReviewDiffTarget::File("z/last.rs".into())
            );
            // A switching checkout blocks navigation entirely.
            panel.git_checkout_pending = true;
            panel.navigate_review_diff(ReviewDiffNavAction::Previous, window, cx);
            assert_eq!(
                panel.review_diff_request.as_ref().unwrap().target,
                ReviewDiffTarget::File("z/last.rs".into())
            );
            panel.git_checkout_pending = false;
            // Filtering the current file out behaves like a removed target.
            panel.review_files.push(file("z/last.rs"));
            panel
                .review_filter_input
                .update(cx, |input, cx| input.set_value("b.rs", window, cx));
            panel.navigate_review_diff(ReviewDiffNavAction::Previous, window, cx);
            assert_eq!(
                panel.review_diff_request.as_ref().unwrap().target,
                ReviewDiffTarget::File("z/last.rs".into())
            );
            panel
                .review_filter_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            panel.navigate_review_diff(ReviewDiffNavAction::Previous, window, cx);
            assert_eq!(
                panel.review_diff_request.as_ref().unwrap().target,
                ReviewDiffTarget::File("b.rs".into())
            );
            // Navigation never touches selection or staged state.
            assert!(panel.selected_files.is_empty());
            assert!(!panel.git_busy);
            panel.review_files.clear();
        });
    }

    #[gpui::test]
    fn review_checkbox_toggles_with_keyboard_and_retains_focus_at_narrow_width(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| {
            let mut state = AppState::default();
            // Default restores a persisted chat; this fixture owns only a project.
            state.active_session_id = None;
            state.active_work_dir = Some(PathBuf::from("/workspace"));
            state
        });
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let panel = cx.new(|cx| RightPanelView::new(model, window, cx));
            panel.update(cx, |panel, cx| {
                panel.active_surface = Some(Surface::Review);
                panel.selected_files.insert("keep.rs".into());
                panel.open_file_diff(format!("src/{}.rs", "long-file-name".repeat(12)), cx);
            });
            let host = cx.new(|_| DiffHost {
                panel: panel.clone(),
                width: 320.0,
            });
            *capture.borrow_mut() = Some((panel, host.clone()));
            Root::new(host, window, cx)
        });
        let (panel, host) = captured.borrow_mut().take().unwrap();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.blur(cx);
            window.focus_next(cx);
            window.focus_next(cx);
            window.focus_next(cx);
            window.draw(cx).clear(cx);
        });
        let checkbox = cx
            .debug_bounds("review-ignore-whitespace")
            .expect("checkbox rendered");
        assert!(checkbox.left() >= px(0.0));
        assert!(checkbox.right() <= px(320.0));
        let focused = cx.update(|window, cx| window.focused(cx).expect("checkbox focused"));
        for checked in [true, false] {
            let keystroke = gpui::Keystroke::parse("space").unwrap();
            cx.simulate_event(gpui::KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            cx.simulate_event(gpui::KeyUpEvent { keystroke });
            panel.read_with(cx, |panel, _| {
                assert_eq!(panel.review_diff_options.ignore_whitespace, checked);
                assert!(panel.selected_files.contains("keep.rs"));
                assert!(!panel.git_busy);
            });
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
                assert!(focused.is_focused(window));
            });
        }
        for theme in [
            gpui_component::ThemeMode::Light,
            gpui_component::ThemeMode::Dark,
        ] {
            for rem_size in [16.0, 20.0] {
                for width in [320.0, 480.0, 640.0] {
                    cx.update(|window, cx| {
                        gpui_component::Theme::change(theme, Some(window), cx);
                        window.set_rem_size(px(rem_size));
                        host.update(cx, |host, cx| {
                            host.width = width;
                            cx.notify();
                        });
                        window.draw(cx).clear(cx);
                    });
                    for selector in [
                        "review-ignore-whitespace",
                        "right-panel-document-back",
                        "close-document",
                    ] {
                        let bounds = cx.debug_bounds(selector).expect("control rendered");
                        assert!(bounds.left() >= px(0.0));
                        assert!(bounds.right() <= px(width));
                    }
                }
            }
        }
        panel.update(cx, |panel, cx| {
            panel.set_ignore_whitespace(true, cx);
        });
        cx.run_until_parked();
        panel.update(cx, |panel, cx| {
            let request = panel.review_diff_request.clone().unwrap();
            panel.apply_review_diff_result(request, Ok(String::new()), cx);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let show = cx
            .debug_bounds("show-whitespace-changes")
            .expect("empty recovery rendered");
        cx.simulate_mouse_move(show.center(), None, Default::default());
        cx.simulate_click(show.center(), Default::default());
        panel.read_with(cx, |panel, _| {
            assert!(!panel.review_diff_options.ignore_whitespace);
            assert!(panel.selected_files.contains("keep.rs"));
        });
        panel.update(cx, |panel, cx| {
            let request = panel.review_diff_request.clone().unwrap();
            let error = threadlane_git::diff_file_with_options(
                &request.project,
                "../outside",
                request.options,
            )
            .unwrap_err();
            panel.apply_review_diff_result(request, Err(error.to_string()), cx);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let retry = cx
            .debug_bounds("retry-review-diff")
            .expect("error recovery rendered");
        let revision = panel.read_with(cx, |panel, _| {
            panel.review_diff_request.as_ref().unwrap().revision
        });
        cx.simulate_mouse_move(retry.center(), None, Default::default());
        cx.simulate_click(retry.center(), Default::default());
        panel.read_with(cx, |panel, _| {
            assert!(panel.review_diff_request.as_ref().unwrap().revision > revision);
            assert!(panel.selected_files.contains("keep.rs"));
        });
    }
}

#[cfg(test)]
mod environment_shortcut_tests {
    use super::RightPanelView;
    use crate::{ReviewTab, Surface};
    use gpui::{AppContext, Focusable, TestAppContext};
    use gpui_component::{Root, WindowExt};
    use threadlane_ui_state::AppState;

    #[gpui::test]
    fn environment_commit_opens_changes_and_focuses_summary_without_committing(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| AppState::default());
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let panel = cx.new(|cx| RightPanelView::new(model, window, cx));
            *capture.borrow_mut() = Some(panel.clone());
            Root::new(panel, window, cx)
        });
        let panel = captured.borrow_mut().take().unwrap();
        cx.update(|window, cx| {
            panel.update(cx, |panel, cx| {
                panel.active_surface = Some(Surface::Review);
                panel.review_tab = ReviewTab::History;
                panel.document_title = Some("Review · changed.rs".into());
                panel.open_commit(window, cx);
                assert_eq!(panel.active_surface, Some(Surface::Review));
                assert_eq!(panel.review_tab, ReviewTab::Changes);
                assert!(panel.document_title.is_none());
                assert!(!panel.git_busy, "Opening the commit UI must not run Git");
                assert!(panel
                    .commit_message_input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window));
            })
        });
    }

    #[gpui::test]
    fn environment_pr_uses_current_checkout_before_hidden_panel_renders(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| {
            let mut state = AppState::default();
            state.active_work_dir = None;
            state.active_session_id = None;
            state
        });
        let retained = model.clone();
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let panel = cx.new(|cx| RightPanelView::new(model, window, cx));
            *capture.borrow_mut() = Some(panel.clone());
            Root::new(panel, window, cx)
        });
        let panel = captured.borrow_mut().take().unwrap();
        cx.update(|window, cx| {
            retained.update(cx, |state, _| {
                state.active_work_dir = Some("/current-checkout".into());
                state.git_statuses.insert(
                    "/current-checkout".into(),
                    threadlane_git::GitStatus {
                        branch: Some("feature".into()),
                        remote: Some("git@github.com:owner/repo.git".into()),
                        has_upstream: true,
                        pr_ready: true,
                        pr_lookup_available: true,
                        ..Default::default()
                    },
                );
            });
            panel.update(cx, |panel, cx| {
                panel.project = Some("/previous-checkout".into());
                panel.open_draft_pr_dialog(window, cx);
                let key = panel.draft_pr_creation_key().unwrap();
                assert_eq!(key.project, std::path::PathBuf::from("/current-checkout"));
                assert_eq!(key.branch, "feature");
                assert!(!panel.git_busy);
            });
            assert!(
                window.has_active_dialog(cx),
                "The existing draft PR form opens without a second click"
            );
            window.close_dialog(cx);
            retained.update(cx, |state, _| {
                state
                    .git_statuses
                    .get_mut(std::path::Path::new("/current-checkout"))
                    .unwrap()
                    .branch = Some("new-feature".into());
            });
            panel.update(cx, |panel, cx| {
                panel.open_draft_pr_dialog(window, cx);
                assert_eq!(panel.draft_pr_creation_key().unwrap().branch, "new-feature");
            });
            assert!(window.has_active_dialog(cx));
            window.close_dialog(cx);
        });
    }
}

#[cfg(test)]
mod panel_document_tests {
    use super::RightPanelView;
    use gpui::{AppContext, TestAppContext};
    use threadlane_ui_state::AppState;

    #[gpui::test]
    fn markdown_preview_panel_keeps_buffer_and_resets_on_close(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| AppState::for_tests());
        let (root, cx) = cx.add_window_view(|window, cx| {
            let panel = cx.new(|cx| RightPanelView::new(model, window, cx));
            gpui_component::Root::new(panel, window, cx)
        });
        let panel = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<RightPanelView>().unwrap()
        });
        cx.update(|window, cx| {
            panel.update(cx, |panel, cx| {
                panel.active_surface = Some(super::Surface::Files);
                panel.pending_document = Some(("README.markdown".into(), "# Buffer".into()));
                panel.sync_pending_document(window, cx);
            })
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let original = panel.read_with(cx, |panel, _| panel.editor_state.clone().unwrap());
        let bounds = cx.debug_bounds("markdown-preview-mode").unwrap();
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        panel.read_with(cx, |panel, cx| {
            assert!(panel.markdown_preview.is_active());
            assert_eq!(panel.editor_state.as_ref(), Some(&original));
            assert_eq!(
                panel.files_selection_block_reason(cx).as_deref(),
                Some(threadlane_ui_kit::PREVIEW_SELECTION_REASON)
            );
        });
        // Programmatic/agent updates notify even while the dirty flag is already true.
        for text in ["# First edit", "# Second edit"] {
            cx.update(|window, cx| {
                original.update(cx, |editor, cx| {
                    editor.set_value(text, window, cx);
                    cx.emit(gpui_component::input::InputEvent::Change);
                })
            });
            cx.run_until_parked();
            panel.read_with(cx, |panel, _| {
                assert!(panel.markdown_preview.is_active());
                assert!(panel.is_dirty);
            });
        }
        // Space on the focused Preview control must not flip an already-selected mode.
        let keystroke = gpui::Keystroke::parse("space").unwrap();
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
        panel.read_with(cx, |panel, _| assert!(panel.markdown_preview.is_active()));
        cx.update(|window, cx| {
            window.focus_prev(cx);
            window.draw(cx).clear(cx);
        });
        let keystroke = gpui::Keystroke::parse("enter").unwrap();
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| assert!(!panel.markdown_preview.is_active()));
        panel.update(cx, |panel, cx| {
            panel.close_document(cx);
            assert!(!panel.markdown_preview.is_active());
            assert!(panel.editor_state.is_none());
        });
    }

    /// `request_open_panel_file` must produce a live editable document —
    /// the Files host Codex flagged as unreachable when only tests wrote
    /// `pending_document`.
    #[gpui::test]
    fn open_in_panel_loads_an_editable_document(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("note.rs"), "fn note() {}\n").unwrap();
        let project = dir.path().to_path_buf();
        let model = cx.new(|_| {
            let mut state = AppState::for_tests();
            state.active_session_id = None;
            state.active_work_dir = Some(project.clone());
            state
        });
        let (panel, cx) =
            cx.add_window_view(|window, cx| RightPanelView::new(model.clone(), window, cx));

        model.update(cx, |state, _| {
            state.request_open_panel_file("note.rs".into())
        });
        panel.update(cx, |_panel, cx| cx.notify());
        cx.run_until_parked();
        for _ in 0..8 {
            cx.update(|window, cx| {
                window.simulate_next_frame(cx);
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
        }

        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.document_title.as_deref(), Some("note.rs"));
            assert!(panel.editor_state.is_some());
            assert!(panel.editable_file_open());
        });
    }
}
