use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::*;
use gpui_component::input::{EditorState, InputEvent, TabSize};
use gpui_component::menu::ContextMenuExt;
use gpui_component::text::TextViewState;
use gpui_component::WindowExt;

use crate::closed_files::{ClosedFileHistory, FileTarget};
use threadlane_client::DaemonClient;
use threadlane_ui_state::AppState;
use threadlane_ui_kit::EditorSaveStatus;

#[path = "guarded_save.rs"]
mod guarded_save;

#[path = "lsp.rs"]
mod lsp;

actions!(editor, [SaveFile]);

/// How long a save/open status message stays visible before auto-expiring.
const STATUS_MSG_TTL: std::time::Duration = std::time::Duration::from_secs(3);

pub use threadlane_ui_kit::tool_preview::detect_language;


pub struct EditorTab {
    project_dir: PathBuf,
    relative_path: String,
    file_name: String,
    _language: &'static str,
    saved_content: String,
    saved_version: Option<String>,
    save_status: EditorSaveStatus,
    save_generation: u64,
    buffer_revision: u64,
    is_dirty: bool,
    is_diff: bool,
    /// File content loaded off the UI thread, awaiting application to the
    /// editor on the next render (which owns the `Window` that `set_value`
    /// requires). Applied once by `sync_pending_content`, then cleared.
    pending_content: Option<String>,
    pending_content_version: Option<String>,
    pending_line: Option<usize>,
    pending_lsp_position: Option<lsp_types::Position>,
    loading: bool,
    open_error: Option<String>,
    client_origin: Option<Arc<dyn DaemonClient>>,
    client_epoch: u64,
    client_connected: bool,
    client_invalidated: bool,
    request_generation: u64,
    baseline_loaded: bool,
    editor_state: Option<Entity<EditorState>>,
    workbench: Option<Entity<threadlane_ui_kit::EditorWorkbench>>,
    language_service: Option<Entity<lsp::LanguageService>>,
    text_view_state: Option<Entity<TextViewState>>,
    markdown_preview: threadlane_ui_kit::MarkdownPreview,
    _subscription: Option<Subscription>,
    /// Re-renders the host whenever the buffer notifies (selection moves
    /// included — the editor emits no `InputEvent` for selection-only
    /// changes, and the add-selection control must track them).
    _observe: Option<Subscription>,
}

#[derive(Clone)]
struct ClientSnapshot {
    client: Arc<dyn DaemonClient>,
    epoch: u64,
    connected: bool,
}

#[derive(Clone)]
struct CloseSnapshot {
    original_index: usize,
    was_active: bool,
    project: PathBuf,
    path: String,
    file_name: String,
    editor: Option<Entity<EditorState>>,
    contents: String,
    is_dirty: bool,
    is_diff: bool,
    loading: bool,
    has_pending_content: bool,
    saved_content: String,
    client: ClientSnapshot,
}

#[derive(Clone, Debug)]
enum PendingOpen {
    File {
        project: PathBuf,
        path: String,
        line: Option<usize>,
    },
    Diff { path: String, content: String },
}

pub struct EditorView {
    model: Entity<AppState>,
    tabs: Vec<EditorTab>,
    active_tab_index: Option<usize>,
    pending_open: Option<PendingOpen>,
    status_msg: Option<(String, bool, std::time::Instant)>,
    closed_files: ClosedFileHistory,
    client_context: Option<ClientSnapshot>,
    next_request_generation: u64,
    language_activation: u64,
    reopen_request: Option<(Entity<EditorState>, u64)>,
    focus_handle: FocusHandle,
    focus_restore_pending: Option<FocusHandle>,
    _subscriptions: Vec<Subscription>,
}

impl EditorView {
    pub fn new(
        model: Entity<AppState>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let model_clone = model.clone();
        let sub = cx.observe(&model_clone, |_this, _model, cx| {
            _this.sync_client_context(cx);
            cx.notify();
        });
        let state = model.read(cx);
        let client = state.daemon_client.clone();
        let client_context = ClientSnapshot {
            epoch: client.file_search_connection_epoch(),
            connected: client.is_connected(),
            client,
        };

        Self {
            model,
            tabs: Vec::new(),
            active_tab_index: None,
            pending_open: None,
            status_msg: None,
            closed_files: ClosedFileHistory::default(),
            client_context: Some(client_context),
            next_request_generation: 0,
            language_activation: 0,
            reopen_request: None,
            focus_handle: cx.focus_handle(),
            focus_restore_pending: None,
            _subscriptions: vec![sub],
        }
    }

    fn has_tabs(&self) -> bool {
        !self.tabs.is_empty()
    }

    fn set_active_tab_index(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        if self.active_tab_index != index {
            if let Some(service) = self
                .active_tab_index
                .and_then(|index| self.tabs.get(index))
                .and_then(|tab| tab.language_service.clone())
            {
                service.update(cx, |service, cx| service.deactivate(cx));
            }
            self.focus_restore_pending = None;
            self.language_activation = self.language_activation.wrapping_add(1);
        }
        self.active_tab_index = index;
    }

    fn focused_editor_handle(&self, window: &Window, cx: &App) -> Option<FocusHandle> {
        let focused = window.focused(cx)?;
        self.focus_handle.contains(&focused, window).then_some(focused)
    }

    fn current_client_snapshot(&self, cx: &App) -> ClientSnapshot {
        let client = self.model.read(cx).daemon_client.clone();
        ClientSnapshot {
            epoch: client.file_search_connection_epoch(),
            connected: client.is_connected(),
            client,
        }
    }

    /// Reconciles the daemon identity used by file reads and saves.
    ///
    /// Replacing the client invalidates both pending reads and this window's
    /// closed-file history. A reconnect on the same client preserves history,
    /// but any in-flight read belongs to the previous connection epoch.
    fn sync_client_context(&mut self, cx: &mut Context<Self>) {
        let current = self.current_client_snapshot(cx);
        let Some(previous) = self.client_context.as_ref() else {
            self.client_context = Some(current);
            return;
        };
        let replaced = !Arc::ptr_eq(&previous.client, &current.client);
        let epoch_changed = previous.epoch != current.epoch;
        let connection_changed = previous.connected != current.connected;

        if replaced {
            self.focus_restore_pending = None;
            self.closed_files.clear();
            for tab in self.tabs.iter_mut().filter(|tab| !tab.is_diff) {
                tab.client_invalidated = true;
                tab.loading = false;
                tab.pending_content = None;
                tab.pending_content_version = None;
                tab.open_error = Some(
                    "This file belongs to a previous daemon connection. Close it, then open it from the current checkout."
                        .into(),
                );
            }
            self.reopen_request = None;
        } else if epoch_changed || connection_changed {
            self.focus_restore_pending = None;
            for tab in self.tabs.iter_mut().filter(|tab| !tab.is_diff) {
                if tab.loading || tab.pending_content.is_some() {
                    tab.loading = false;
                    tab.pending_content = None;
                    tab.pending_content_version = None;
                    tab.open_error = Some(
                        "The daemon connection changed while this file was loading. Retry."
                            .into(),
                    );
                }
            }
            self.reopen_request = None;
        }

        self.client_context = Some(current);
    }

    fn reopen_control(
        &self,
        _cx: &App,
    ) -> threadlane_ui_kit::ReopenClosedFileControl {
        let in_flight = self.reopen_request.as_ref().and_then(|(editor, generation)| {
            self.tabs.iter().find(|tab| {
                tab.editor_state.as_ref() == Some(editor)
                    && tab.request_generation == *generation
            })
        });
        let loading = in_flight.is_some_and(|tab| tab.loading);
        let target = if loading {
            in_flight.map(|tab| format!("{} / {}", tab.project_dir.display(), tab.relative_path))
        } else {
            self.closed_files
                .targets()
                .next()
                .map(|target| format!("{} / {}", target.project.display(), target.path))
        };
        threadlane_ui_kit::ReopenClosedFileControl::default()
            .with_target(target)
            .with_loading(loading)
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    fn is_active_dirty(&self) -> bool {
        self.active_tab_index
            .and_then(|idx| self.tabs.get(idx))
            .map(|tab| tab.is_dirty && !tab.is_diff)
            .unwrap_or(false)
    }

    fn is_active_diff(&self) -> bool {
        self.active_tab_index
            .and_then(|idx| self.tabs.get(idx))
            .map(|tab| tab.is_diff)
            .unwrap_or(false)
    }

    /// The active tab's buffer entity — `None` for diff/loading documents.
    /// Selection guards still apply; this is the underlying buffer for
    /// command surfaces and tests.
    pub fn active_editor(&self) -> Option<Entity<EditorState>> {
        self.active_tab_index
            .and_then(|index| self.tabs.get(index))
            .and_then(|tab| tab.editor_state.clone())
    }

    /// Reason the **Add selection to chat** command is unavailable for the
    /// active tab — `None` when ready. Diff documents, loading buffers,
    /// files outside the active checkout, and empty or oversized selections
    /// each carry a textual reason.
    pub fn selection_block_reason(&self, cx: &App) -> Option<SharedString> {
        let Some(tab) = self
            .active_tab_index
            .and_then(|index| self.tabs.get(index))
        else {
            return Some("Open a file first".into());
        };
        if tab.markdown_preview.is_active() {
            return Some(threadlane_ui_kit::PREVIEW_SELECTION_REASON.into());
        }
        if tab.is_diff {
            return Some("Diffs can't be added to chat — open the file itself".into());
        }
        if tab.loading || tab.pending_content.is_some() {
            return Some("The file is still loading".into());
        }
        let Some(editor) = tab.editor_state.as_ref() else {
            return Some("The document is not editable".into());
        };
        if self.model.read(cx).active_git_work_dir().as_ref() != Some(&tab.project_dir) {
            return Some("The file is not in the active checkout".into());
        }
        threadlane_ui_kit::editor_excerpt_block_reason(
            threadlane_ui_kit::editor_selection_snapshot(editor.read(cx)).as_ref(),
        )
        .map(SharedString::from)
    }

    /// Activates **Add selection to chat** for the active tab: validates the
    /// selection, captures buffer identity plus the composer destination,
    /// and emits `EditorSelectionRequest` for the parent surface to append.
    /// On any guard failure the reason is shown and nothing is appended.
    pub fn request_add_selection_to_chat(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(reason) = self.selection_block_reason(cx) {
            window.push_notification(reason.to_string(), cx);
            return;
        }
        let Some(tab) = self
            .active_tab_index
            .and_then(|index| self.tabs.get(index))
        else {
            return;
        };
        let Some(editor) = tab.editor_state.clone() else {
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
            checkout: tab.project_dir.clone(),
            relative_path: tab.relative_path.clone(),
            dirty: tab.is_dirty,
            snapshot,
            destination,
        });
    }

    /// Whether `request` still names the active tab's buffer and the
    /// buffer's live selection is the captured one.
    pub fn selection_request_is_current(
        &self,
        request: &threadlane_ui_kit::EditorSelectionRequest,
        cx: &App,
    ) -> bool {
        let Some(tab) = self
            .active_tab_index
            .and_then(|index| self.tabs.get(index))
        else {
            return false;
        };
        if tab.markdown_preview.is_active()
            || tab.is_diff
            || tab.loading
            || tab.pending_content.is_some()
            || tab.project_dir != request.checkout
            || tab.relative_path != request.relative_path
        {
            return false;
        }
        let Some(editor) = tab.editor_state.as_ref() else {
            return false;
        };
        editor.entity_id() == request.editor.entity_id()
            && editor.read(cx).selected_range() == request.snapshot.byte_range
    }

    fn sync_pending_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_open.take() else {
            return;
        };
        match pending {
            PendingOpen::File {
                project,
                path,
                line,
            } => {
                self.open_file_internal(&project, &path, window, cx);
                if let Some(tab) = self
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.project_dir == project && tab.relative_path == path)
                {
                    tab.pending_line = line;
                    tab.pending_lsp_position = None;
                    if line.is_some() {
                        tab.markdown_preview.show_source();
                    }
                    if let Some(editor) = &tab.editor_state {
                        if tab.markdown_preview.is_active() {
                            return;
                        }
                        editor.update(cx, |editor, cx| editor.focus(window, cx));
                    }
                }
            }
            PendingOpen::Diff { path, content } => {
                self.open_diff_internal(&path, &content, window, cx)
            }
        }
    }

    pub fn open_file(&mut self, project: PathBuf, relative_path: &str, cx: &mut Context<Self>) {
        self.open_file_at_line(project, relative_path, None, cx);
    }

    pub fn open_file_at_line(
        &mut self,
        project: PathBuf,
        relative_path: &str,
        line: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.pending_open = Some(PendingOpen::File {
            project,
            line,
            path: relative_path.to_string(),
        });
        cx.notify();
    }

    /// Reopens the newest closed ordinary file in this window.
    ///
    /// History is consumed when the request is accepted. An already-open
    /// non-diff tab is selected without refreshing or changing its buffer.
    fn reopen_closed_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_client_context(cx);
        if window.has_active_dialog(cx)
            || window.has_active_sheet(cx)
            || !self.focus_handle.contains_focused(window, cx)
        {
            return;
        }
        let reopen_is_loading = self.reopen_request.as_ref().is_some_and(|(editor, generation)| {
            self.tabs.iter().any(|tab| {
                tab.editor_state.as_ref() == Some(editor)
                    && tab.request_generation == *generation
                    && tab.loading
            })
        });
        if reopen_is_loading {
            return;
        }
        let Some(target) = self.closed_files.pop() else {
            return;
        };

        if let Some(index) = self.tabs.iter().position(|tab| {
            !tab.is_diff && tab.project_dir == target.project && tab.relative_path == target.path
        }) {
            self.set_active_tab_index(Some(index), cx);
            self.reopen_request = if self.tabs[index].loading {
                self.tabs[index]
                    .editor_state
                    .clone()
                    .map(|editor| (editor, self.tabs[index].request_generation))
            } else {
                None
            };
            if let Some(editor) = self.tabs[index].editor_state.clone() {
                editor.update(cx, |editor, cx| editor.focus(window, cx));
            }
            self.status_msg = None;
            cx.notify();
            return;
        }

        self.open_file_internal(&target.project, &target.path, window, cx);
        if let Some(index) = self.tabs.iter().position(|tab| {
            !tab.is_diff && tab.project_dir == target.project && tab.relative_path == target.path
        }) {
            self.reopen_request = if self.tabs[index].loading {
                self.tabs[index]
                    .editor_state
                    .clone()
                    .map(|editor| (editor, self.tabs[index].request_generation))
            } else {
                None
            };
            if let Some(editor) = self.tabs[index].editor_state.clone() {
                editor.update(cx, |editor, cx| editor.focus(window, cx));
            }
        }
        cx.notify();
    }

    /// Retries reading the saved file into the existing tab.
    ///
    /// The editor entity and any edits remain intact. Tabs invalidated by a
    /// daemon replacement cannot be retried against the replacement client.
    fn retry_file(&mut self, index: usize, cx: &mut Context<Self>) {
        self.sync_client_context(cx);
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if tab.is_diff || tab.loading {
            return;
        }
        if tab.client_invalidated {
            self.set_status(
                "This file belongs to a previous daemon connection. Close it, then open it from the current checkout."
                    .into(),
                true,
            );
            cx.notify();
            return;
        }
        self.start_file_read(index, cx);
        cx.notify();
    }

    pub fn open_diff(&mut self, relative_path: &str, content: &str, cx: &mut Context<Self>) {
        self.pending_open = Some(PendingOpen::Diff {
            path: relative_path.to_string(),
            content: content.to_string(),
        });
        cx.notify();
    }

    fn open_diff_internal(
        &mut self,
        relative_path: &str,
        content: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab_key = format!("diff:{relative_path}");
        let markdown = format!("```diff\n{}\n```", content.replace("```", "` ` `"));

        if let Some(existing_idx) = self.tabs.iter().position(|t| t.relative_path == tab_key) {
            if let Some(tab) = self.tabs.get_mut(existing_idx) {
                tab.saved_content = content.to_string();
                if let Some(ref text_view) = tab.text_view_state {
                    text_view.update(cx, |state, cx| {
                        state.set_text(&markdown, cx);
                    });
                }
            }
            self.set_active_tab_index(Some(existing_idx), cx);
            cx.notify();
            return;
        }

        let markdown_state = cx.new(|cx| TextViewState::markdown(&markdown, cx));
        let tab_title = threadlane_ui_kit::editor_tab_title(relative_path, true);

        self.tabs.push(EditorTab {
            project_dir: self
                .model
                .read(cx)
                .active_work_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from(".")),
            relative_path: tab_key,
            file_name: tab_title,
            _language: "diff",
            saved_content: content.to_string(),
            saved_version: None,
            save_status: EditorSaveStatus::Ready,
            save_generation: 0,
            buffer_revision: 0,
            is_dirty: false,
            is_diff: true,
            pending_content: None,
            pending_content_version: None,
            pending_line: None,
            pending_lsp_position: None,
            loading: false,
            open_error: None,
            client_origin: None,
            client_epoch: 0,
            client_connected: false,
            client_invalidated: false,
            request_generation: 0,
            baseline_loaded: true,
            editor_state: None,
            workbench: None,
            language_service: None,
            markdown_preview: threadlane_ui_kit::MarkdownPreview::new(cx),
            text_view_state: Some(markdown_state),
            _subscription: None,
            _observe: None,
        });

        self.set_active_tab_index(Some(self.tabs.len() - 1), cx);
        self.status_msg = None;
        cx.notify();
    }

    fn open_file_internal(
        &mut self,
        project_dir: &Path,
        relative_path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Reuse the tab, but refresh saved content before navigating a clean buffer.
        if let Some(existing_idx) = self
            .tabs
            .iter()
            .position(|t| t.project_dir == project_dir && t.relative_path == relative_path)
        {
            if self.tabs[existing_idx].client_invalidated {
                if self.tabs[existing_idx].is_dirty {
                    self.set_active_tab_index(Some(existing_idx), cx);
                    self.set_status(
                        "This buffer has unsaved edits from a previous daemon. Discard or copy them before reopening from the current checkout."
                            .into(),
                        true,
                    );
                    cx.notify();
                    return;
                }
                self.remove_tab_at(existing_idx, cx);
            } else {
                self.set_active_tab_index(Some(existing_idx), cx);
                if self.tabs[existing_idx].is_dirty {
                    self.set_status(
                        "Unsaved buffer preserved; saved-file line numbers may differ.".into(),
                        false,
                    );
                } else if !self.tabs[existing_idx].loading
                    && self.tabs[existing_idx].pending_content.is_none()
                {
                    self.start_file_read(existing_idx, cx);
                }
                cx.notify();
                return;
            }
        }

        let lang = detect_language(relative_path);
        // The tab (and its editor entity, which needs a Window) is created
        // synchronously with a loading placeholder; only the filesystem read
        // moves to the background executor so a large file never stalls the
        // UI thread. Content fills in when the read completes.
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
                .default_value("")
        });

        let workbench = cx.new(|cx| {
            threadlane_ui_kit::EditorWorkbench::new(editor.clone(), lang, window, cx)
        });
        let language_service = lsp::attach(
            cx.weak_entity(),
            self.model.clone(),
            &editor,
            &workbench,
            relative_path,
            cx,
        );
        let target_path = relative_path.to_string();
        let target_project = project_dir.to_path_buf();
        let subscription = cx.subscribe(&editor, move |this, editor, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let current = editor.read(cx).value();
                if let Some(tab) = this
                    .tabs
                    .iter_mut()
                    .find(|t| t.project_dir == target_project && t.relative_path == target_path)
                {
                    // An edit after a read completed must also cancel its queued replacement.
                    let canceled_pending_content = tab.pending_content.take().is_some();
                    tab.pending_content_version = None;
                    tab.buffer_revision = tab.buffer_revision.wrapping_add(1);
                    tab.pending_lsp_position = None;
                    tab.markdown_preview.refresh(current.clone(), cx);
                    let dirty = current.as_str() != tab.saved_content.as_str();
                    if canceled_pending_content && !tab.baseline_loaded {
                        tab.save_status = EditorSaveStatus::Failed {
                            message: "The file changed while it was loading. Your edits are preserved, but saving is blocked until the file is reloaded.".into(),
                            save_blocked: true,
                        };
                    }
                    if tab.is_dirty != dirty {
                        tab.is_dirty = dirty;
                        cx.notify();
                    }
                }
            }
        });

        let tab_title = threadlane_ui_kit::editor_tab_title(relative_path, false);

        // Cursor/selection notifications only update controls. Content changes
        // refresh preview in the Change subscription (or explicit reload path).
        let observe = cx.observe(&editor, |_this, _editor, cx| cx.notify());

        self.tabs.push(EditorTab {
            project_dir: project_dir.to_path_buf(),
            relative_path: relative_path.to_string(),
            file_name: tab_title,
            _language: lang,
            saved_content: String::new(),
            saved_version: None,
            save_status: EditorSaveStatus::Ready,
            save_generation: 0,
            buffer_revision: 0,
            is_dirty: false,
            is_diff: false,
            pending_content: None,
            pending_content_version: None,
            pending_line: None,
            pending_lsp_position: None,
            loading: false,
            open_error: None,
            client_origin: None,
            client_epoch: 0,
            client_connected: false,
            client_invalidated: false,
            request_generation: 0,
            baseline_loaded: false,
            editor_state: Some(editor.clone()),
            workbench: Some(workbench),
            language_service,
            markdown_preview: threadlane_ui_kit::MarkdownPreview::new(cx),
            text_view_state: None,
            _subscription: Some(subscription),
            _observe: Some(observe),
        });

        self.set_active_tab_index(Some(self.tabs.len() - 1), cx);
        self.status_msg = None;
        cx.notify();

        self.start_file_read(self.tabs.len() - 1, cx);
    }

    fn start_file_read(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if tab.is_diff || tab.loading {
            return;
        }
        if let Some(service) = tab.language_service.clone() {
            service.update(cx, |service, cx| service.deactivate(cx));
        }
        self.sync_client_context(cx);
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if tab.client_invalidated {
            return;
        }
        if tab.client_origin.as_ref().is_some_and(|origin| {
            !Arc::ptr_eq(origin, &self.current_client_snapshot(cx).client)
        }) {
            return;
        }
        let Some(load_editor) = tab.editor_state.clone() else {
            return;
        };
        let buffer_revision = tab.buffer_revision;
        let snapshot = self.current_client_snapshot(cx);
        self.next_request_generation = self.next_request_generation.wrapping_add(1);
        let request_generation = self.next_request_generation;
        let tab = &mut self.tabs[index];
        tab.loading = true;
        tab.pending_content = None;
        tab.pending_content_version = None;
        tab.open_error = None;
        tab.client_origin = Some(snapshot.client.clone());
        tab.client_epoch = snapshot.epoch;
        tab.client_connected = snapshot.connected;
        tab.request_generation = request_generation;
        if self
            .reopen_request
            .as_ref()
            .is_some_and(|(editor, _)| editor == &load_editor)
        {
            self.reopen_request = Some((load_editor.clone(), request_generation));
        }
        let load_project = tab.project_dir.clone();
        let load_path = tab.relative_path.clone();
        let read_client = snapshot.client.clone();
        let read_project = load_project.clone();
        let read_path = load_path.clone();
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
                this.finish_file_open_for_request(
                    &load_project,
                    &load_path,
                    &load_editor,
                    request_generation,
                    buffer_revision,
                    snapshot,
                    result,
                    cx,
                );
            });
        })
        .detach();
    }

    /// Completes an asynchronous file open started by `open_file_internal`.
    ///
    /// Window-free: the loaded bytes and version land on the tab as pending
    /// content; `sync_pending_content` applies them and advances the baseline.
    ///
    /// Never clobbers user input: a read is only queued for application when
    /// the buffer has not changed since the read started. The saved baseline
    /// advances only when `sync_pending_content` applies the queued content.
    #[cfg(test)]
    fn finish_file_open(
        &mut self,
        project_dir: &Path,
        relative_path: &str,
        editor: &Entity<EditorState>,
        result: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.iter().find(|tab| {
            tab.project_dir == project_dir
                && tab.relative_path == relative_path
                && tab.editor_state.as_ref() == Some(editor)
        }) else {
            return;
        };
        let snapshot = tab
            .client_origin
            .clone()
            .map(|client| ClientSnapshot {
                epoch: tab.client_epoch,
                connected: client.is_connected(),
                client,
            })
            .unwrap_or_else(|| self.current_client_snapshot(cx));
        let request_generation = tab.request_generation;
        let buffer_revision = tab.buffer_revision;
        self.finish_file_open_for_request(
            project_dir,
            relative_path,
            editor,
            request_generation,
            buffer_revision,
            snapshot,
            result.map(|content| (content, None)),
            cx,
        );
    }

    fn finish_file_open_for_request(
        &mut self,
        project_dir: &Path,
        relative_path: &str,
        editor: &Entity<EditorState>,
        request_generation: u64,
        buffer_revision_at_start: u64,
        snapshot: ClientSnapshot,
        result: Result<(String, Option<String>), String>,
        cx: &mut Context<Self>,
    ) {
        let current_client = self.current_client_snapshot(cx);
        let context_matches = Arc::ptr_eq(&snapshot.client, &current_client.client)
            && snapshot.epoch == current_client.epoch
            && snapshot.connected == current_client.connected;
        let replaced = !Arc::ptr_eq(&snapshot.client, &current_client.client);
        let Some(tab) = self.tabs.iter_mut().find(|t| {
            t.project_dir == project_dir
                && t.relative_path == relative_path
                && !t.is_diff
                && t.editor_state.as_ref() == Some(editor)
                && t.request_generation == request_generation
        }) else {
            return;
        };
        let is_reopen_request = self
            .reopen_request
            .as_ref()
            .is_some_and(|(reopen_editor, generation)| {
                reopen_editor == editor && *generation == request_generation
            });
        if is_reopen_request {
            self.reopen_request = None;
        }
        if !context_matches {
            tab.loading = false;
            tab.pending_content = None;
            tab.pending_content_version = None;
            tab.pending_line = None;
            tab.pending_lsp_position = None;
            tab.open_error = Some(if replaced {
                "This file belongs to a previous daemon connection. Close it, then open it from the current checkout."
                    .into()
            } else {
                "The daemon connection changed while this file was loading. Retry.".into()
            });
            tab.client_invalidated = replaced;
            cx.notify();
            return;
        }
        tab.loading = false;
        match result {
            Ok((content, version)) => {
                let current = editor.read(cx).value();
                if tab.buffer_revision == buffer_revision_at_start
                    && !tab.is_dirty
                    && current.as_str() == tab.saved_content
                {
                    tab.pending_content = Some(content);
                    tab.pending_content_version = version;
                    tab.save_status = if snapshot.client.supports_guarded_saves() {
                        EditorSaveStatus::Ready
                    } else {
                        EditorSaveStatus::Unsupported
                    };
                } else {
                    tab.is_dirty = current.as_str() != tab.saved_content;
                    if !tab.baseline_loaded {
                        tab.save_status = if snapshot.client.supports_guarded_saves() {
                            EditorSaveStatus::Failed {
                                message: "The file changed while it was loading. Your edits are preserved, but saving is blocked until the file is reloaded.".into(),
                                save_blocked: true,
                            }
                        } else {
                            EditorSaveStatus::Unsupported
                        };
                    }
                }
                tab.open_error = None;
                tab.client_invalidated = false;
            }
            Err(error) => {
                tab.pending_line = None;
                tab.pending_lsp_position = None;
                tab.pending_content = None;
                tab.pending_content_version = None;
                tracing::error!(
                    "Failed to open file {}: {}",
                    project_dir.join(relative_path).display(),
                    error
                );
                tab.open_error = Some(format!(
                    "Couldn't open {relative_path}: {error}. Retry after checking the file."
                ));
                self.set_status(
                    format!(
                        "Couldn't open {relative_path}: {error}. Check that the file still exists and is readable, then retry."
                    ),
                    true,
                );
            }
        }
        cx.notify();
    }

    /// Applies background-loaded file content to editors. Called from
    /// `render` (which owns the `Window`), mirroring `sync_pending_file`.
    /// Each tab applies at most once: content is taken, never re-read.
    fn sync_pending_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut applied = false;
        let current_client = self.current_client_snapshot(cx);
        for (ix, tab) in self
            .tabs
            .iter_mut()
            .enumerate()
            .filter(|(_, tab)| !tab.is_diff)
        {
            if let Some(editor) = tab.editor_state.clone() {
                if tab.pending_content.is_some()
                    && (!tab
                        .client_origin
                        .as_ref()
                        .is_some_and(|client| Arc::ptr_eq(client, &current_client.client))
                        || tab.client_epoch != current_client.epoch
                        || tab.client_connected != current_client.connected
                        || tab.client_invalidated)
                {
                    let replaced = tab
                        .client_origin
                        .as_ref()
                        .is_none_or(|client| !Arc::ptr_eq(client, &current_client.client));
                    tab.pending_content = None;
                    tab.pending_content_version = None;
                    tab.loading = false;
                    tab.open_error = Some(if replaced {
                        "This file belongs to a previous daemon connection. Close it, then open it from the current checkout."
                            .into()
                    } else {
                        "The daemon connection changed while this file was loading. Retry."
                            .into()
                    });
                    tab.client_invalidated = replaced;
                    continue;
                }
                if let Some(content) = tab.pending_content.take() {
                    tab.saved_content = content.clone();
                    tab.saved_version = tab.pending_content_version.take();
                    tab.baseline_loaded = true;
                    tab.save_status = if current_client.client.supports_guarded_saves() {
                        EditorSaveStatus::Ready
                    } else {
                        EditorSaveStatus::Unsupported
                    };
                    let content: SharedString = content.into();
                    tab.markdown_preview.refresh(content.clone(), cx);
                    editor.update(cx, |editor, cx| editor.set_value(content, window, cx));
                    tab.is_dirty = false;
                    applied = true;
                }
                if Some(ix) == self.active_tab_index
                    && editor.read(cx).value().as_str() != "Loading…"
                    && (tab.pending_line.is_some() || tab.pending_lsp_position.is_some())
                    && !tab.loading
                {
                    // Cursor scrolling needs the loaded document's completed layout.
                    // Keep the request pending if the user switches tabs before then.
                    cx.on_next_frame(window, move |this, window, cx| {
                        let Some(tab) = this.active_tab_index.and_then(|ix| this.tabs.get_mut(ix))
                        else {
                            return;
                        };
                        if tab.editor_state.as_ref() != Some(&editor)
                            || tab.pending_content.is_some()
                            || tab.loading
                        {
                            return;
                        }
                        let position = if let Some(position) = tab.pending_lsp_position.take() {
                            let text = editor.read(cx).value();
                            let range = lsp_types::Range::new(position, position);
                            let Ok(range) = crate::lsp_mapping::scalar_range(&text, range) else {
                                return;
                            };
                            tab.pending_line = None;
                            range.start
                        } else if let Some(line) = tab.pending_line.take() {
                            gpui_component::input::Position::new(
                                line.saturating_sub(1).min(u32::MAX as usize) as u32,
                                0,
                            )
                        } else {
                            return;
                        };
                        editor.update(cx, |editor, cx| {
                            editor.set_cursor_position(position, window, cx)
                        });
                        cx.notify();
                    });
                }
            }
        }
        if applied {
            cx.notify();
        }
    }

    fn set_status(&mut self, msg: String, is_error: bool) {
        self.status_msg = Some((msg, is_error, std::time::Instant::now()));
    }

    fn visible_status(&self) -> Option<(String, bool)> {
        match &self.status_msg {
            Some((msg, is_error, at)) if at.elapsed() < STATUS_MSG_TTL => {
                Some((msg.clone(), *is_error))
            }
            _ => None,
        }
    }

    fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.set_active_tab_index(Some(index), cx);
            self.status_msg = None;
            cx.notify();
        }
    }

    fn close_snapshot(&self, index: usize, cx: &App) -> Option<CloseSnapshot> {
        let tab = self.tabs.get(index)?;
        let contents = tab
            .editor_state
            .as_ref()
            .map(|editor| editor.read(cx).value().to_string())
            .unwrap_or_default();
        Some(CloseSnapshot {
            original_index: index,
            was_active: self.active_tab_index == Some(index),
            project: tab.project_dir.clone(),
            path: tab.relative_path.clone(),
            file_name: tab.file_name.clone(),
            editor: tab.editor_state.clone(),
            contents,
            is_dirty: tab.is_dirty,
            is_diff: tab.is_diff,
            loading: tab.loading,
            has_pending_content: tab.pending_content.is_some(),
            saved_content: tab.saved_content.clone(),
            client: self.current_client_snapshot(cx),
        })
    }

    fn close_snapshot_matches(&self, snapshot: &CloseSnapshot, cx: &App) -> bool {
        let current_client = self.current_client_snapshot(cx);
        let Some(tab) = self.tabs.iter().find(|tab| {
            tab.project_dir == snapshot.project
                && tab.relative_path == snapshot.path
                && tab.editor_state == snapshot.editor
        }) else {
            return false;
        };
        let contents = tab
            .editor_state
            .as_ref()
            .map(|editor| editor.read(cx).value().to_string())
            .unwrap_or_default();
        Arc::ptr_eq(&snapshot.client.client, &current_client.client)
            && snapshot.client.epoch == current_client.epoch
            && snapshot.client.connected == current_client.connected
            && tab.is_dirty == snapshot.is_dirty
            && tab.is_diff == snapshot.is_diff
            && tab.loading == snapshot.loading
            && tab.pending_content.is_some() == snapshot.has_pending_content
            && tab.saved_content == snapshot.saved_content
            && contents == snapshot.contents
    }

    fn remove_confirmed_tabs(
        &mut self,
        snapshots: &[CloseSnapshot],
        retained: Option<&CloseSnapshot>,
        restore_focus: Option<FocusHandle>,
        cx: &mut Context<Self>,
    ) {
        if snapshots
            .iter()
            .any(|snapshot| !self.close_snapshot_matches(snapshot, cx))
            || retained.is_some_and(|snapshot| !self.close_snapshot_matches(snapshot, cx))
        {
            return;
        }

        let active_original_index = snapshots
            .iter()
            .find(|snapshot| snapshot.was_active)
            .map(|snapshot| snapshot.original_index);
        let mut ordered = snapshots.to_vec();
        ordered.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.original_index));
        if let Some(active) = active_original_index {
            ordered.sort_by(|left, right| {
                match (
                    left.original_index == active,
                    right.original_index == active,
                ) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => right.original_index.cmp(&left.original_index),
                }
            });
        }
        let current_client = self.current_client_snapshot(cx);
        let mut removed = Vec::new();
        let mut removed_active = false;
        for snapshot in ordered {
            if let Some(index) = self.tabs.iter().position(|tab| {
                tab.project_dir == snapshot.project
                    && tab.relative_path == snapshot.path
                    && tab.editor_state == snapshot.editor
            }) {
                removed_active |= snapshot.was_active;
                if let Some(tab) = self.remove_tab_at(index, cx) {
                    if !tab.is_diff
                        && tab.client_origin.as_ref().is_some_and(|client| {
                            Arc::ptr_eq(client, &current_client.client)
                        })
                    {
                        removed.push(FileTarget::new(tab.project_dir, tab.relative_path));
                    }
                }
            }
        }
        for target in removed.into_iter().rev() {
            self.closed_files.record(target);
        }
        if let Some(retained) = retained {
            self.active_tab_index = self.tabs.iter().position(|tab| {
                tab.project_dir == retained.project
                    && tab.relative_path == retained.path
                && tab.editor_state == retained.editor
            });
        }
        if removed_active {
            self.focus_restore_pending = restore_focus;
        }
        self.status_msg = None;
        cx.notify();
    }

    fn remove_tab_at(&mut self, index: usize, cx: &mut Context<Self>) -> Option<EditorTab> {
        if index < self.tabs.len() {
            let removed = self.tabs.remove(index);
            self.focus_restore_pending = None;
            if self
                .reopen_request
                .as_ref()
                .is_some_and(|(editor, _)| removed.editor_state.as_ref() == Some(editor))
            {
                self.reopen_request = None;
            }
            if self.tabs.is_empty() {
                self.set_active_tab_index(None, cx);
            } else if let Some(active) = self.active_tab_index {
                if active >= self.tabs.len() {
                    self.set_active_tab_index(Some(self.tabs.len() - 1), cx);
                } else if active > index {
                    self.set_active_tab_index(Some(active - 1), cx);
                }
            }
            self.status_msg = None;
            cx.notify();
            Some(removed)
        } else {
            None
        }
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }

        let restore_focus = self.focused_editor_handle(window, cx);
        let Some(snapshot) = self.close_snapshot(index, cx) else {
            return;
        };
        if snapshot.is_dirty && !snapshot.is_diff {
            let file_name = snapshot.file_name.clone();

            cx.spawn(async move |this, cx| {
                let result = rfd::AsyncMessageDialog::new()
                    .set_title("Discard unsaved changes?")
                    .set_description(format!(
                        "Do you want to discard unsaved changes to \"{file_name}\"?"
                    ))
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    .await;
                if matches!(result, rfd::MessageDialogResult::Yes) {
                    let _ = this.update(cx, |this, cx| {
                        this.remove_confirmed_tabs(&[snapshot], None, restore_focus, cx);
                    });
                }
            })
            .detach();
        } else {
            self.remove_confirmed_tabs(&[snapshot], None, restore_focus, cx);
        }
    }

    fn close_other_tabs(
        &mut self,
        keep_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if keep_index >= self.tabs.len() {
            return;
        }

        let restore_focus = self.focused_editor_handle(window, cx);
        let Some(retained) = self.close_snapshot(keep_index, cx) else {
            return;
        };
        let snapshots: Vec<CloseSnapshot> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != keep_index)
            .filter_map(|(index, _)| self.close_snapshot(index, cx))
            .collect();
        let dirty_names: Vec<String> = snapshots
            .iter()
            .filter(|snapshot| snapshot.is_dirty && !snapshot.is_diff)
            .map(|snapshot| snapshot.file_name.clone())
            .collect();

        if dirty_names.is_empty() {
            self.remove_confirmed_tabs(&snapshots, Some(&retained), restore_focus, cx);
        } else {
            let description = if dirty_names.len() == 1 {
                format!(
                    "Do you want to discard unsaved changes to \"{}\"?",
                    dirty_names[0]
                )
            } else {
                format!(
                    "Do you want to discard unsaved changes to {} files ({})?",
                    dirty_names.len(),
                    dirty_names.join(", ")
                )
            };

            cx.spawn(async move |this, cx| {
                let result = rfd::AsyncMessageDialog::new()
                    .set_title("Discard unsaved changes?")
                    .set_description(description)
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    .await;
                if matches!(result, rfd::MessageDialogResult::Yes) {
                    let _ = this.update(cx, |this, cx| {
                        this.remove_confirmed_tabs(
                            &snapshots,
                            Some(&retained),
                            restore_focus,
                            cx,
                        );
                    });
                }
            })
            .detach();
        }
    }

    fn close_all_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let restore_focus = self.focused_editor_handle(window, cx);
        let snapshots: Vec<CloseSnapshot> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(index, _)| self.close_snapshot(index, cx))
            .collect();
        let dirty_names: Vec<String> = snapshots
            .iter()
            .filter(|snapshot| snapshot.is_dirty && !snapshot.is_diff)
            .map(|snapshot| snapshot.file_name.clone())
            .collect();

        if dirty_names.is_empty() {
            self.remove_confirmed_tabs(&snapshots, None, restore_focus, cx);
        } else {
            let description = if dirty_names.len() == 1 {
                format!(
                    "Do you want to discard unsaved changes to \"{}\"?",
                    dirty_names[0]
                )
            } else {
                format!(
                    "Do you want to discard unsaved changes to {} files ({})?",
                    dirty_names.len(),
                    dirty_names.join(", ")
                )
            };

            cx.spawn(async move |this, cx| {
                let result = rfd::AsyncMessageDialog::new()
                    .set_title("Discard unsaved changes?")
                    .set_description(description)
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    .await;
                if matches!(result, rfd::MessageDialogResult::Yes) {
                    let _ = this.update(cx, |this, cx| {
                        this.remove_confirmed_tabs(&snapshots, None, restore_focus, cx);
                    });
                }
            })
            .detach();
        }
    }

    fn save_active_file(&mut self, cx: &mut Context<Self>) {
        let Some(idx) = self.active_tab_index else {
            return;
        };
        self.save_tab_at(idx, cx);
    }

    fn save_file_action(&mut self, _: &SaveFile, _window: &mut Window, cx: &mut Context<Self>) {
        self.save_active_file(cx);
    }

    fn save_tab_at(&mut self, index: usize, cx: &mut Context<Self>) {
        self.save_guarded_tab(index, cx);
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity();
        let selection_reason = self.selection_block_reason(cx);
        threadlane_ui_kit::editor_tab_bar(cx)
            .child(
                threadlane_ui_kit::editor_tabs().children(self.tabs.iter().enumerate().map(
                    |(index, tab)| {
                        let project = tab.project_dir.clone();
                        let path = tab.relative_path.clone();
                        let select_owner = owner.clone();
                        let select_project = project.clone();
                        let select_path = path.clone();
                        let close_owner = owner.clone();
                        let close_project = project.clone();
                        let close_path = path.clone();
                        let menu_owner = owner.clone();
                        let raw_path = path.strip_prefix("diff:").unwrap_or(&path).to_owned();
                        threadlane_ui_kit::editor_tab(
                            format!("editor-tab-{project:?}:{path}"),
                            tab.file_name.clone(),
                            raw_path,
                            self.active_tab_index == Some(index),
                            tab.is_dirty,
                            tab.is_diff,
                            move |_, _, cx| {
                                select_owner.update(cx, |view, cx| {
                                    if let Some(index) = view.tabs.iter().position(|tab| {
                                        tab.project_dir == select_project
                                            && tab.relative_path == select_path
                                    }) {
                                        view.select_tab(index, cx);
                                    }
                                });
                            },
                            move |_, window, cx| {
                                close_owner.update(cx, |view, cx| {
                                    if let Some(index) = view.tabs.iter().position(|tab| {
                                        tab.project_dir == close_project
                                            && tab.relative_path == close_path
                                    }) {
                                        view.close_tab(index, window, cx);
                                    }
                                });
                            },
                            cx,
                        )
                        .context_menu(move |menu, _, _| {
                            let owner = menu_owner.clone();
                            let project = project.clone();
                            let path = path.clone();
                            threadlane_ui_kit::editor_tab_menu(menu, move |action, window, cx| {
                                owner.update(cx, |view, cx| {
                                    let Some(index) = view.tabs.iter().position(|tab| {
                                        tab.project_dir == project && tab.relative_path == path
                                    }) else {
                                        return;
                                    };
                                    match action {
                                        threadlane_ui_kit::EditorTabAction::Close => {
                                            view.close_tab(index, window, cx)
                                        }
                                        threadlane_ui_kit::EditorTabAction::CloseOthers => {
                                            view.close_other_tabs(index, window, cx)
                                        }
                                        threadlane_ui_kit::EditorTabAction::CloseAll => {
                                            view.close_all_tabs(window, cx)
                                        }
                                    }
                                });
                            })
                        })
                    },
                )),
            )
            .child(threadlane_ui_kit::editor_actions(
                self.active_tab_index
                    .and_then(|ix| self.tabs.get(ix))
                    .filter(|tab| {
                        !tab.is_diff
                            && threadlane_ui_kit::markdown_preview_eligible(&tab.relative_path)
                    })
                    .map(|tab| {
                        tab.markdown_preview
                            .control(tab.loading || tab.pending_content.is_some(), cx)
                    }),
                self.visible_status(),
                self.reopen_button(cx),
                Some(
                    threadlane_ui_kit::editor_add_selection_button(
                        "editor-add-selection",
                        &threadlane_ui_kit::AddSelectionControl {
                            enabled: selection_reason.is_none(),
                            reason: selection_reason,
                        },
                    )
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.request_add_selection_to_chat(window, cx)
                    })),
                ),
                threadlane_ui_kit::editor_save_button(
                    self.active_tab_index.is_some_and(|index| self.tab_can_save(index, cx)),
                    self.is_active_diff(),
                )
                .on_click(cx.listener(|view, _, _, cx| view.save_active_file(cx))),
                cx,
            ))
    }

    fn toggle_markdown_preview(
        &mut self,
        _: &threadlane_ui_kit::ToggleMarkdownPreview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab_index.and_then(|ix| self.tabs.get_mut(ix)) else {
            return;
        };
        if tab.markdown_preview.is_active() {
            tab.markdown_preview.show_source();
            tab.markdown_preview.focus_control(window, cx);
            cx.notify();
            return;
        }
        if tab.is_diff
            || tab.loading
            || tab.pending_content.is_some()
            || !threadlane_ui_kit::markdown_preview_eligible(&tab.relative_path)
        {
            return;
        }
        if let Some(editor) = &tab.editor_state {
            tab.markdown_preview.toggle(editor.read(cx).value(), cx);
            tab.markdown_preview.focus_control(window, cx);
            cx.notify();
        }
    }

    fn render_empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        threadlane_ui_kit::editor_empty_state(
            self.reopen_control(cx)
                .is_available()
                .then(|| self.reopen_button(cx)),
            cx,
        )
    }

    fn reopen_button(&self, cx: &Context<Self>) -> gpui_component::button::Button {
        let focus = self.focus_handle.clone();
        threadlane_ui_kit::editor_reopen_button(&self.reopen_control(cx))
            .on_click(move |_, window, cx| {
                window.focus(&focus, cx);
                window.dispatch_action(Box::new(threadlane_ui_kit::ReopenClosedFile), cx);
            })
    }

}

impl EventEmitter<threadlane_ui_kit::EditorSelectionRequest> for EditorView {}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_client_context(cx);
        if let Some(original_focus) = self.focus_restore_pending.take() {
            let focus_unchanged = window
                .focused(cx)
                .is_none_or(|current| current == original_focus);
            if focus_unchanged
                && !window.has_active_dialog(cx)
                && !window.has_active_sheet(cx)
            {
                window.focus(&self.focus_handle, cx);
            }
        }
        self.sync_pending_file(window, cx);
        self.sync_pending_content(window, cx);
        threadlane_ui_kit::editor_surface(cx)
            .id("central-editor")
            .role(Role::Group)
            .track_focus(&self.focus_handle)
            .key_context("CentralEditor")
            .on_action(cx.listener(|view, _: &threadlane_ui_kit::ReopenClosedFile, window, cx| {
                view.reopen_closed_file(window, cx)
            }))
            .on_action(cx.listener(Self::save_file_action))
            .on_action(cx.listener(Self::toggle_markdown_preview))
            .on_action(cx.listener(|view, _: &threadlane_ui_kit::AddSelectionToChat, window, cx| {
                view.request_add_selection_to_chat(window, cx)
            }))
            .children(self.has_tabs().then(|| self.render_tab_bar(cx)))
            .children(self.render_save_recovery(cx))
            .children(self.active_tab_index.and_then(|index| self.tabs.get(index).map(|tab| (index, tab)))
                .filter(|(_, tab)| tab.loading || tab.open_error.is_some())
                .map(|(index, tab)| {
                    let target = format!("{} / {}", tab.project_dir.display(), tab.relative_path);
                    let editor = tab.editor_state.clone();
                    threadlane_ui_kit::editor_file_status(
                        target.clone(),
                        tab.open_error.clone(),
                        threadlane_ui_kit::editor_retry_button(&target, tab.loading)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if view.tabs.get(index).is_some_and(|tab| tab.editor_state == editor) {
                                    view.retry_file(index, cx);
                                }
                            })),
                        cx,
                    )
                }))
            .children(
                self.active_tab_index
                    .and_then(|ix| self.tabs.get(ix))
                    .and_then(|tab| tab.markdown_preview.notice(tab.is_dirty))
                    .map(|notice| threadlane_ui_kit::markdown_preview_notice(notice, cx)),
            )
            .child(if let Some(idx) = self.active_tab_index {
                if let Some(active_tab) = self.tabs.get(idx) {
                    if active_tab.is_diff {
                        if let Some(ref text_view) = active_tab.text_view_state {
                            threadlane_ui_kit::editor_diff(text_view, cx)
                                .into_any_element()
                        } else {
                            self.render_empty_state(cx).into_any_element()
                        }
                    } else if active_tab.markdown_preview.is_active() {
                        active_tab.markdown_preview.body(cx)
                    } else if let Some(ref workbench) = active_tab.workbench {
                        workbench.clone().into_any_element()
                    } else if let Some(ref editor) = active_tab.editor_state {
                        threadlane_ui_kit::editor_buffer(editor)
                            .into_any_element()
                    } else {
                        self.render_empty_state(cx).into_any_element()
                    }
                } else {
                    self.render_empty_state(cx).into_any_element()
                }
            } else {
                self.render_empty_state(cx).into_any_element()
            })
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::EditorView;
    use gpui::AppContext as _;

    #[gpui::test]
    fn markdown_preview_preserves_buffer_and_forces_source_for_line(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| threadlane_ui_state::AppState::for_tests());
        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| EditorView::new(model, window, cx));
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_diff_internal("README.md", "", window, cx);
                let editor = cx.new(|cx| {
                    gpui_component::input::EditorState::new(window, cx).default_value("# Unsaved")
                });
                let tab = &mut view.tabs[0];
                tab.is_diff = false;
                tab.relative_path = "README.md".into();
                tab.is_dirty = true;
                tab.editor_state = Some(editor);
            })
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let original = view.read_with(cx, |view, _| view.active_editor().unwrap());
        original.update(cx, |editor, cx| editor.set_selected_range(2..5, cx));
        let request = view.read_with(cx, |view, cx| threadlane_ui_kit::EditorSelectionRequest {
            editor: original.clone(),
            checkout: view.tabs[0].project_dir.clone(),
            relative_path: "README.md".into(),
            dirty: true,
            snapshot: threadlane_ui_kit::editor_selection_snapshot(original.read(cx)).unwrap(),
            destination: (None, None),
        });
        assert!(view.read_with(cx, |view, cx| view
            .selection_request_is_current(&request, cx)));
        let bounds = cx.debug_bounds("markdown-preview-mode").unwrap();
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(cx, |view, cx| {
            assert!(view.tabs[0].markdown_preview.is_active());
            assert_eq!(view.active_editor().unwrap(), original);
            assert!(view.tabs[0].is_dirty);
            assert_eq!(original.read(cx).selected_range(), 2..5);
            assert!(!view.selection_request_is_current(&request, cx));
            assert_eq!(
                view.selection_block_reason(cx).as_deref(),
                Some(threadlane_ui_kit::PREVIEW_SELECTION_REASON)
            );
        });
        // The document-scoped action also works from the focused native mode control.
        cx.update(|window, cx| {
            window.dispatch_action(Box::new(threadlane_ui_kit::ToggleMarkdownPreview), cx)
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.tabs[0].markdown_preview.is_active())
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.toggle_markdown_preview(&threadlane_ui_kit::ToggleMarkdownPreview, window, cx);
                let project = view.tabs[0].project_dir.clone();
                view.open_file_at_line(project, "README.md", Some(1), cx);
                view.sync_pending_file(window, cx);
                assert!(!view.tabs[0].markdown_preview.is_active());
                assert_eq!(view.active_editor().unwrap(), original);
            })
        });
    }

    #[gpui::test]
    fn shared_tabs_support_keyboard_selection_and_close_without_reselecting(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let project = std::path::PathBuf::from("/editor-preview-test");
        let model = cx.new(|_| {
            let mut state = threadlane_ui_state::AppState::default();
            state.projects.clear();
            state.pending_hydrations.clear();
            state.active_session_id = None;
            state.active_work_dir = Some(project.clone());
            state
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let editor = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_diff("one.rs", "+one", cx);
                view
            });
            gpui_component::Root::new(editor, window, cx)
        });
        let editor = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });
        for name in ["two.rs", "three.rs"] {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            editor.update(cx, |view, cx| view.open_diff(name, "+sample", cx));
        }
        cx.update(|window, cx| window.draw(cx).clear(cx));
        editor.update(cx, |view, cx| view.select_tab(0, cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let close = cx
            .debug_bounds(r#"editor-tab-"/editor-preview-test":diff:two.rs-close"#)
            .unwrap();
        cx.simulate_click(close.center(), gpui::Modifiers::default());
        editor.read_with(cx, |view, _| {
            assert_eq!(view.tabs.len(), 2);
            assert_eq!(
                view.tabs[view.active_tab_index.unwrap()].relative_path,
                "diff:one.rs"
            );
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.blur(cx);
            window.focus_next(cx); // Select one.rs
            window.focus_next(cx); // Close one.rs
            window.focus_next(cx); // Select three.rs
            assert!(
                window.focused(cx).is_some(),
                "editor controls are tab stops"
            );
            window.draw(cx).clear(cx);
        });
        let keystroke = gpui::Keystroke::parse("enter").unwrap();
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
        editor.read_with(cx, |view, _| {
            assert_eq!(
                view.tabs[view.active_tab_index.unwrap()].relative_path,
                "diff:three.rs"
            );
        });
        // Domain-derived identity stays usable when an earlier tab is removed.
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let close = cx
            .debug_bounds(r#"editor-tab-"/editor-preview-test":diff:one.rs-close"#)
            .unwrap();
        cx.simulate_click(close.center(), gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let close = cx
            .debug_bounds(r#"editor-tab-"/editor-preview-test":diff:three.rs-close"#)
            .unwrap();
        cx.simulate_click(close.center(), gpui::Modifiers::default());
        editor.read_with(cx, |view, _| {
            assert!(view.tabs.is_empty());
            assert!(view.active_tab_index.is_none());
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("editor-save-btn").is_none());
    }
    #[gpui::test]
    fn add_selection_reports_disabled_reasons(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let project = std::path::PathBuf::from("/editor-preview-test");
        let model = cx.new(|_| {
            let mut state = threadlane_ui_state::AppState::for_tests();
            state.active_session_id = None;
            state.active_work_dir = Some(project.clone());
            state
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let editor = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_diff("one.rs", "+one", cx);
                view
            });
            gpui_component::Root::new(editor, window, cx)
        });
        let editor = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });
        // A diff document can never hand a selection to chat.
        editor.read_with(cx, |view, cx| {
            assert_eq!(
                view.selection_block_reason(cx).as_deref(),
                Some("Diffs can't be added to chat — open the file itself")
            );
        });
        cx.update(|window, cx| {
            editor.update(cx, |view, cx| view.close_tab(0, window, cx))
        });
        // No open file at all.
        editor.read_with(cx, |view, cx| {
            assert_eq!(
                view.selection_block_reason(cx).as_deref(),
                Some("Open a file first")
            );
        });
    }

    #[gpui::test]
    fn opens_at_requested_line_after_loading_and_reuses_tab(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sample.rs"), "fn sample() {}\n".repeat(400)).unwrap();
        let project = dir.path().to_path_buf();
        let mut app_state = threadlane_ui_state::AppState::for_tests();
        app_state.active_work_dir = Some(project.clone());
        let model = cx.new(|_| app_state);
        let holder = std::rc::Rc::new(std::cell::RefCell::new(None));
        let holder_clone = holder.clone();
        let project_clone = project.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let editor = cx.new(|cx| {
                let mut editor = EditorView::new(model, window, cx);
                editor.open_file_at_line(project_clone, "sample.rs", Some(200), cx);
                editor
            });
            holder_clone.borrow_mut().replace(editor.clone());
            gpui_component::Root::new(editor, window, cx)
        });
        cx.run_until_parked();
        for _ in 0..4 {
            cx.update(|window, cx| {
                window.simulate_next_frame(cx);
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
        }
        let editor = holder.borrow().as_ref().unwrap().clone();
        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.tabs.len(), 1);
            assert_eq!(
                editor.tabs[0]
                    .editor_state
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .cursor_position()
                    .line,
                199
            );
            let state = editor.tabs[0].editor_state.as_ref().unwrap().read(cx);
            let (mut caret, _) = state.cursor_layout().expect("caret laid out");
            let viewport = state.input_bounds();
            // cursor_layout reports unscrolled Y; painting adds the text's scroll offset.
            caret.origin.y += state.text_bounds().unwrap().top() - viewport.top();
            assert!(
                caret.top() >= viewport.top() && caret.bottom() <= viewport.bottom(),
                "requested line must be visibly revealed: {caret:?} in {viewport:?}"
            );
        });
        let updated = "// saved externally\n".repeat(500);
        std::fs::write(project.join("sample.rs"), &updated).unwrap();
        editor.update(cx, |editor, cx| {
            editor.open_file_at_line(project.clone(), "sample.rs", Some(450), cx);
        });
        cx.run_until_parked();
        for _ in 0..4 {
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
        }
        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.tabs.len(), 1);
            let tab = &editor.tabs[0];
            assert!(!tab.is_dirty);
            let state = tab.editor_state.as_ref().unwrap().read(cx);
            assert_eq!(state.value().as_str(), updated);
            assert_eq!(state.cursor_position().line, 449);
        });
        cx.simulate_input("unsaved");
        let unsaved = editor.read_with(cx, |editor, cx| {
            assert!(editor.tabs[0].is_dirty);
            editor.tabs[0].editor_state.as_ref().unwrap().read(cx).value().to_string()
        });
        // A daemon read finishing after typing must retain those edits, too.
        editor.update(cx, |editor, cx| {
            let input = editor.tabs[0].editor_state.clone().unwrap();
            editor.tabs[0].loading = true;
            editor.finish_file_open(&project, "sample.rs", &input, Ok("new saved text\n".into()), cx);
            assert!(!editor.tabs[0].loading);
            assert!(editor.tabs[0].pending_content.is_none());
            assert_eq!(input.read(cx).value().as_str(), unsaved);
            editor.open_file_at_line(project.clone(), "sample.rs", Some(3), cx)
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.tabs.len(), 1);
            assert_eq!(
                editor.tabs[0]
                    .editor_state
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .cursor_position()
                    .line,
                2
            );
            assert!(editor.tabs[0].is_dirty);
            assert_eq!(editor.tabs[0].editor_state.as_ref().unwrap().read(cx).value().as_str(), unsaved);
            assert!(editor.visible_status().unwrap().0.contains("Unsaved buffer preserved"));
        });
        // Failure must not jump into stale text or strand the tab as loading.
        editor.update(cx, |editor, cx| {
            let input = editor.tabs[0].editor_state.clone().unwrap();
            editor.tabs[0].loading = true;
            editor.tabs[0].pending_line = Some(99);
            editor.finish_file_open(&project, "sample.rs", &input, Err("disconnected".into()), cx);
            assert!(!editor.tabs[0].loading);
            assert!(editor.tabs[0].pending_line.is_none());
            assert_eq!(input.read(cx).value().as_str(), unsaved);
        });
    }
}

#[cfg(test)]
mod closed_file_host_tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::AppContext as _;

    use crate::closed_files::FileTarget;
    use super::{ClientSnapshot, EditorView};
    use threadlane_ui_kit::EditorSaveStatus;

    fn test_state(project: &std::path::Path) -> threadlane_ui_state::AppState {
        let mut state = threadlane_ui_state::AppState::for_tests();
        state.active_session_id = None;
        state.active_work_dir = Some(project.to_path_buf());
        state
    }

    #[gpui::test]
    fn bulk_close_history_reopens_active_first_and_excludes_diffs(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        for path in ["one.rs", "two.rs"] {
            std::fs::write(dir.path().join(path), path).unwrap();
        }
        let project = dir.path().to_path_buf();
        let model = cx.new(|_| test_state(&project));
        let project_for_view = project.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_file_internal(&project_for_view, "one.rs", window, cx);
                view.open_file_internal(&project_for_view, "two.rs", window, cx);
                view.open_diff_internal("review.diff", "+new", window, cx);
                view.select_tab(0, cx);
                view.close_all_tabs(window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });

        view.read_with(cx, |view, _| {
            assert!(view.tabs.is_empty());
            assert_eq!(
                view.closed_files.targets().cloned().collect::<Vec<_>>(),
                vec![
                    FileTarget::new(project.clone(), "one.rs"),
                    FileTarget::new(project.clone(), "two.rs"),
                ]
            );
        });
    }

    #[gpui::test]
    fn reopen_reads_current_bytes_but_selecting_open_dirty_file_does_not_refresh(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sample.rs"), "initial bytes\n").unwrap();
        let project = dir.path().to_path_buf();
        let model = cx.new(|_| test_state(&project));
        let project_for_view = project.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_file_internal(&project_for_view, "sample.rs", window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });

        cx.run_until_parked();
        for _ in 0..4 {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
        }
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.tabs[0]
                    .editor_state
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .value()
                    .as_str(),
                "initial bytes\n"
            );
        });

        cx.update(|window, cx| {
            let focus = view.read_with(cx, |view, _| view.focus_handle.clone());
            window.focus(&focus, cx);
            view.update(cx, |view, cx| view.close_tab(0, window, cx));
        });
        std::fs::write(dir.path().join("sample.rs"), "updated disk bytes\n").unwrap();
        cx.update(|window, cx| {
            let focus = view.read_with(cx, |view, _| view.focus_handle.clone());
            window.focus(&focus, cx);
            view.update(cx, |view, cx| view.reopen_closed_file(window, cx));
        });
        cx.run_until_parked();
        for _ in 0..4 {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
        }
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.tabs[0]
                    .editor_state
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .value()
                    .as_str(),
                "updated disk bytes\n"
            );
        });

        std::fs::write(dir.path().join("sample.rs"), "external change\n").unwrap();
        cx.update(|window, cx| {
            let focus = view.read_with(cx, |view, _| view.focus_handle.clone());
            window.focus(&focus, cx);
            view.update(cx, |view, cx| {
                view.open_diff_internal("review.diff", "+diff", window, cx);
                let file_index = view
                    .tabs
                    .iter()
                    .position(|tab| tab.relative_path == "sample.rs")
                    .unwrap();
                let editor = view.tabs[file_index].editor_state.clone().unwrap();
                editor.update(cx, |editor, cx| {
                    editor.set_value("typed edits", window, cx);
                });
                view.tabs[file_index].is_dirty = true;
                view.closed_files
                    .record(FileTarget::new(project.clone(), "sample.rs"));
                view.reopen_closed_file(window, cx);
            });
        });
        view.read_with(cx, |view, cx| {
            let active = view.active_tab_index.unwrap();
            assert_eq!(view.tabs[active].relative_path, "sample.rs");
            assert!(view.tabs[active].is_dirty);
            assert_eq!(
                view.tabs[active]
                    .editor_state
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .value()
                    .as_str(),
                "typed edits"
            );
        });
    }

    #[gpui::test]
    fn stale_bulk_confirmation_preserves_edits_and_new_tabs(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sample.rs"), "saved\n").unwrap();
        let project = dir.path().to_path_buf();
        let model = cx.new(|_| test_state(&project));
        let project_for_view = project.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_file_internal(&project_for_view, "sample.rs", window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let stale = view.close_snapshot(0, cx).unwrap();
                let editor = view.tabs[0].editor_state.clone().unwrap();
                editor.update(cx, |editor, cx| editor.set_value("new edits", window, cx));
                view.tabs[0].is_dirty = true;
                view.remove_confirmed_tabs(std::slice::from_ref(&stale), None, None, cx);
                assert_eq!(view.tabs.len(), 1);
                assert!(view.closed_files.is_empty());

                let current = view.close_snapshot(0, cx).unwrap();
                view.open_diff_internal("new.diff", "+new", window, cx);
                view.remove_confirmed_tabs(std::slice::from_ref(&current), None, None, cx);
                assert_eq!(view.tabs.len(), 1);
                assert!(view.tabs[0].is_diff);
                assert_eq!(
                    view.closed_files.targets().next(),
                    Some(&FileTarget::new(project.clone(), "sample.rs"))
                );
            });
        });
    }

    #[gpui::test]
    fn replacement_tab_rejects_old_read_and_old_client_history(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sample.rs"), "disk bytes\n").unwrap();
        let project = dir.path().to_path_buf();
        let model = cx.new(|_| test_state(&project));
        let model_for_view = model.clone();
        let replacement = threadlane_ui_state::AppState::for_tests()
            .daemon_client
            .clone();
        let project_for_view = project.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model_for_view, window, cx);
                view.open_file_internal(&project_for_view, "sample.rs", window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });
        let old_editor = view.read_with(cx, |view, _| {
            view.tabs[0].editor_state.clone().unwrap()
        });

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.closed_files
                    .record(FileTarget::new(project.clone(), "sample.rs"));
                let old_index = view.tabs.iter().position(|tab| tab.editor_state.as_ref() == Some(&old_editor)).unwrap();
                view.remove_tab_at(old_index, cx);
                view.open_file_internal(&project, "sample.rs", window, cx);
                let replacement_editor = view.tabs[0].editor_state.clone().unwrap();
                view.finish_file_open(
                    &project,
                    "sample.rs",
                    &old_editor,
                    Ok("stale bytes".into()),
                    cx,
                );
                assert_ne!(replacement_editor, old_editor);
                assert!(view.tabs[0].pending_content.is_none());
            });
        });

        cx.update(|_, cx| {
            model.update(cx, |state, _| state.daemon_client = replacement);
            view.update(cx, |view, cx| view.sync_client_context(cx));
        });
        view.read_with(cx, |view, _| {
            assert!(view.closed_files.is_empty());
            assert!(view.tabs[0].client_invalidated);
            assert!(view.tabs[0]
                .open_error
                .as_deref()
                .is_some_and(|message| message.contains("previous daemon")));
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.close_tab(0, window, cx));
        });
        view.read_with(cx, |view, _| assert!(view.closed_files.is_empty()));
    }

    #[gpui::test]
    fn stale_epoch_read_is_rejected_and_same_client_retry_uses_current_epoch(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sample.rs"), "disk bytes\n").unwrap();
        let project = dir.path().to_path_buf();
        let model = cx.new(|_| test_state(&project));
        let project_for_view = project.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_file_internal(&project_for_view, "sample.rs", window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });

        view.update(cx, |view, cx| {
            let current = view.current_client_snapshot(cx);
            let editor = view.tabs[0].editor_state.clone().unwrap();
            let generation = view.tabs[0].request_generation;
            view.tabs[0].loading = true;
            view.tabs[0].pending_content = None;
            view.tabs[0].pending_content_version = None;
            view.tabs[0].client_origin = Some(current.client.clone());
            view.tabs[0].client_epoch = current.epoch.wrapping_add(1);
            view.client_context = Some(ClientSnapshot {
                epoch: current.epoch.wrapping_add(1),
                ..current.clone()
            });
            view.closed_files
                .record(FileTarget::new(project.clone(), "closed.rs"));
            view.sync_client_context(cx);
            assert!(!view.tabs[0].loading);
            assert!(view.tabs[0]
                .open_error
                .as_deref()
                .is_some_and(|message| message.contains("connection changed")));
            let stale_epoch = ClientSnapshot {
                epoch: current.epoch.wrapping_add(1),
                ..current.clone()
            };
            let buffer_revision = view.tabs[0].buffer_revision;
            view.finish_file_open_for_request(
                &project,
                "sample.rs",
                &editor,
                generation,
                buffer_revision,
                stale_epoch,
                Ok(("stale epoch bytes".into(), Some("stale-version".into()))),
                cx,
            );
            assert!(!view.tabs[0].loading);
            assert!(view.tabs[0]
                .open_error
                .as_deref()
                .is_some_and(|message| message.contains("connection changed")));
            assert_eq!(view.closed_files.targets().count(), 1);

            view.retry_file(0, cx);
            assert!(view.tabs[0].loading);
            assert_eq!(view.tabs[0].client_epoch, current.epoch);
            assert!(!view.tabs[0].client_invalidated);
        });
    }

    #[gpui::test]
    fn save_requires_a_baseline_and_rejects_stale_client_completion(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sample.rs"), "initial bytes\n").unwrap();
        let project = dir.path().to_path_buf();
        let model = cx.new(|_| test_state(&project));
        let model_for_view = model.clone();
        let replacement = threadlane_ui_state::AppState::for_tests()
            .daemon_client
            .clone();
        let project_for_view = project.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model_for_view, window, cx);
                view.open_file_internal(&project_for_view, "missing.rs", window, cx);
                view.open_file_internal(&project_for_view, "sample.rs", window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });

        cx.run_until_parked();
        for _ in 0..4 {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
        }
        view.read_with(cx, |view, _| {
            assert!(!view.tabs[0].baseline_loaded);
            assert!(view.tabs[1].baseline_loaded);
        });

        let notifications = Rc::new(Cell::new(0));
        let observed_notifications = notifications.clone();
        let _notification_observer = cx.update(|_, cx| {
            cx.observe(&view, move |_, _| {
                observed_notifications.set(observed_notifications.get() + 1);
            })
        });
        let before_baseline_rejection = notifications.get();
        view.update(cx, |view, cx| {
            view.save_tab_at(0, cx);
            assert!(view
                .status_msg
                .as_ref()
                .is_some_and(|(message, _, _)| message.contains("not loaded successfully")));
        });
        cx.run_until_parked();
        assert!(
            notifications.get() > before_baseline_rejection,
            "rejecting a save without a loaded baseline must notify the editor view"
        );
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(!project.join("missing.rs").exists());

        let sample_editor = view.read_with(cx, |view, _| {
            view.tabs[1].editor_state.clone().unwrap()
        });
        let original_sample_origin = view.read_with(cx, |view, _| {
            view.tabs[1].client_origin.clone().unwrap()
        });
        let original_epoch = view.read_with(cx, |view, _| view.tabs[1].client_epoch);
        let before_origin_rejection = notifications.get();
        view.update(cx, |view, cx| {
            view.tabs[1].client_origin = Some(replacement.clone());
            view.save_tab_at(1, cx);
            assert!(view.status_msg.as_ref().is_some_and(|(message, _, _)| {
                message.contains("previous daemon connection")
            }));
        });
        cx.run_until_parked();
        assert!(
            notifications.get() > before_origin_rejection,
            "rejecting a save from a different daemon origin must notify the editor view"
        );
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "initial bytes\n"
        );
        view.update(cx, |view, _| {
            view.tabs[1].client_origin = Some(original_sample_origin);
        });

        let focus = view.read_with(cx, |view, _| view.focus_handle.clone());
        cx.update(|window, cx| {
            window.focus(&focus, cx);
            view.update(cx, |view, cx| {
                sample_editor.update(cx, |editor, cx| {
                    editor.set_value("same client save", window, cx);
                });
                view.tabs[1].is_dirty = true;
                view.tabs[1].client_epoch = original_epoch.wrapping_add(1);
                view.save_tab_at(1, cx);
            });
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(matches!(
                view.tabs[1].save_status,
                EditorSaveStatus::Failed {
                    save_blocked: true,
                    ..
                }
            ));
            assert!(view.tabs[1].is_dirty);
        });
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "initial bytes\n"
        );

        view.update(cx, |view, _| {
            view.tabs[1].client_epoch = original_epoch;
            view.tabs[1].save_status = EditorSaveStatus::Ready;
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                sample_editor.update(cx, |editor, cx| {
                    editor.set_value("same client save", window, cx);
                });
                view.tabs[1].is_dirty = true;
                view.save_tab_at(1, cx);
            });
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.tabs[1].save_status, EditorSaveStatus::Ready);
            assert!(!view.tabs[1].is_dirty);
        });
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "same client save"
        );

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                sample_editor.update(cx, |editor, cx| {
                    editor.set_value("stale completion".to_string(), window, cx);
                });
                view.tabs[1].is_dirty = true;
                view.save_tab_at(1, cx);
            });
            model.update(cx, |state, _| state.daemon_client = replacement);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(matches!(
                view.tabs[1].save_status,
                EditorSaveStatus::Failed {
                    save_blocked: true,
                    ..
                }
            ));
            assert!(view.tabs[1].is_dirty);
        });
    }

    #[gpui::test]
    fn stale_close_completion_does_not_steal_focus_and_last_tab_keeps_reopen_shortcut(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.update(threadlane_ui_kit::init_editor);
        let dir = tempfile::tempdir().unwrap();
        for path in ["sample.rs", "second.rs"] {
            std::fs::write(dir.path().join(path), path).unwrap();
        }
        let project = dir.path().to_path_buf();
        let model = cx.new(|_| test_state(&project));
        let project_for_view = project.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_file_internal(&project_for_view, "sample.rs", window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let editor_focus = view.read_with(cx, |view, _| view.focus_handle.clone());
        let outsider_focus = cx.update(|_, cx| cx.focus_handle());
        cx.update(|window, cx| {
            window.focus(&editor_focus, cx);
            view.update(cx, |view, cx| view.close_tab(0, window, cx));
            window.focus(&outsider_focus, cx);
            window.draw(cx).clear(cx);
        });
        assert_eq!(
            cx.update(|window, cx| window.focused(cx)),
            Some(outsider_focus.clone())
        );

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_file_internal(&project, "second.rs", window, cx);
            });
            window.draw(cx).clear(cx);
            window.focus(&editor_focus, cx);
            view.update(cx, |view, cx| view.close_tab(0, window, cx));
            window.blur(cx);
            window.draw(cx).clear(cx);
        });
        assert_eq!(
            cx.update(|window, cx| window.focused(cx)),
            Some(editor_focus.clone())
        );

        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-shift-t"
        } else {
            "ctrl-shift-t"
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.tabs.len(), 1);
            assert_eq!(view.tabs[0].relative_path, "second.rs");
        });
    }

    #[gpui::test]
    fn reopen_action_is_single_flight_and_reports_its_in_flight_target(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.update(threadlane_ui_kit::init_editor);
        let dir = tempfile::tempdir().unwrap();
        for path in ["one.rs", "two.rs", "other.rs"] {
            std::fs::write(dir.path().join(path), path).unwrap();
        }
        let project = dir.path().to_path_buf();
        let model = cx.new(|_| test_state(&project));
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| EditorView::new(model, window, cx));
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let editor_focus = view.read_with(cx, |view, _| view.focus_handle.clone());
        cx.update(|window, cx| {
            window.focus(&editor_focus, cx);
            view.update(cx, |view, _cx| {
                view.closed_files
                    .record(FileTarget::new(project.clone(), "two.rs"));
                view.closed_files
                    .record(FileTarget::new(project.clone(), "one.rs"));
            });
        });
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-shift-t"
        } else {
            "ctrl-shift-t"
        });
        view.update(cx, |view, _| {
            let (editor, generation) = {
                let tab = &mut view.tabs[0];
                let generation = tab.request_generation.wrapping_add(1);
                tab.request_generation = generation;
                tab.loading = true;
                (tab.editor_state.clone().unwrap(), generation)
            };
            view.reopen_request = Some((editor, generation));
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.tabs.len(), 1);
            assert!(view.tabs[0].loading);
            assert!(view
                .reopen_control(cx)
                .description()
                .starts_with("Reopening "));
            assert!(view.reopen_control(cx).description().contains("one.rs"));
        });

        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-shift-t"
        } else {
            "ctrl-shift-t"
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.tabs.len(), 1);
            assert_eq!(view.closed_files.targets().count(), 1);
        });

        let reopened_editor = view.read_with(cx, |view, _| {
            view.tabs
                .iter()
                .find(|tab| tab.relative_path == "one.rs")
                .unwrap()
                .editor_state
                .clone()
                .unwrap()
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_file_internal(&project, "other.rs", window, cx);
                let other_editor = view
                    .tabs
                    .iter()
                    .find(|tab| tab.relative_path == "other.rs")
                    .unwrap()
                    .editor_state
                    .clone()
                    .unwrap();
                view.finish_file_open(
                    &project,
                    "other.rs",
                    &other_editor,
                    Ok("other.rs".into()),
                    cx,
                );
            });
        });
        view.read_with(cx, |view, cx| {
            assert!(view.tabs.iter().any(|tab| tab.relative_path == "other.rs"));
            assert!(view
                .reopen_control(cx)
                .description()
                .starts_with("Reopening "));
            assert!(view.reopen_control(cx).description().contains("one.rs"));
        });

        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.finish_file_open(
                    &project,
                    "one.rs",
                    &reopened_editor,
                    Ok("one.rs".into()),
                    cx,
                );
            });
        });
        view.read_with(cx, |view, cx| {
            assert!(view.reopen_control(cx).description().starts_with("Reopen "));
            assert!(view.reopen_control(cx).description().contains("two.rs"));
        });

        let outsider_focus = cx.update(|_, cx| cx.focus_handle());
        cx.update(|window, cx| window.focus(&outsider_focus, cx));
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-shift-t"
        } else {
            "ctrl-shift-t"
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.closed_files.targets().count(), 1);
            assert_eq!(view.tabs.len(), 2);
        });
    }
}
