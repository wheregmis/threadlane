use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Editor, EditorState, Input, InputEvent, InputState, TabSize};
use gpui_component::list::ListItem;
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::scroll::ScrollableElement;
use gpui_component::separator::Separator;
use gpui_component::spinner::Spinner;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::text::{TextView, TextViewState};
use gpui_component::tree::{Tree, TreeEvent, TreeItem, TreeState};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, WindowExt};
use threadlane_git::{GitBranchInfo, GitCommitInfo, GitFile, GitStatus};

use crate::services::watcher::WorkspaceWatcher;
use crate::state::AppState;

fn can_publish_branch(status: Option<&GitStatus>) -> bool {
    status.is_some_and(|status| {
        !status.has_upstream
            && !status.detached
            && status.branch.is_some()
            && status.remote.is_some()
    })
}

fn normalize_generated_commit_message(raw: &str) -> String {
    let trimmed = raw.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(trimmed)
        .trim();
    let without_fences = if unquoted.starts_with("```") {
        unquoted
            .lines()
            .filter(|line| !line.trim().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    } else {
        unquoted.to_string()
    };
    without_fences
}

fn detect_language(path_str: &str) -> &'static str {
    let path = Path::new(path_str);
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("ts" | "mts" | "cts" | "jsx" | "tsx") => "typescript",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("html" | "htm") => "html",
        Some("css") => "css",
        Some("md" | "markdown") => "markdown",
        Some("sh" | "bash" | "zsh") => "bash",
        Some("go") => "go",
        Some("c" | "h") => "c",
        Some("cpp" | "hpp" | "cc" | "cxx" | "hh") => "cpp",
        Some("diff" | "patch") => "diff",
        Some("zig") => "zig",
        _ => match path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|s| s.to_lowercase())
            .as_deref()
        {
            Some("dockerfile") => "bash",
            Some("cargo.lock") => "toml",
            _ => "text",
        },
    }
}
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ReviewTab {
    #[default]
    Changes,
    History,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Surface {
    Review,
    Files,
}

impl Surface {
    fn label(self) -> &'static str {
        match self {
            Self::Review => "Review",
            Self::Files => "Files",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Review => IconName::File,
            Self::Files => IconName::Folder,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitAction {
    Commit,
    CommitAndPush,
    Push,
    Pull,
    Fetch,
    Checkout(String),
    CheckoutStash(String),
    CheckoutCarry(String),
    CreateBranch(String),
    Merge(String),
    PopStash(Option<usize>),
    DropStash(Option<usize>),
    DiscardFile(String),
    IgnoreFile(String),
    IgnoreExtension(String),
}

#[derive(Clone, Debug)]
struct FileNode {
    relative_path: String,
    name: String,
    is_dir: bool,
    children: Vec<FileNode>,
}

enum PanelEvent {
    FilesLoaded {
        project: PathBuf,
        nodes: Vec<FileNode>,
    },
    ReviewLoaded {
        project: PathBuf,
        status: Option<GitStatus>,
        files: Vec<GitFile>,
        error: Option<String>,
    },
    WorkspaceChanged {
        project: PathBuf,
        git_dirty: bool,
        files_dirty: bool,
    },
    MessageGenerated(Result<String, String>),
    ActionFinished(Result<GitStatus, String>),
    CommitFilesLoaded {
        sha: String,
        files: Vec<GitFile>,
    },
    StashFilesLoaded {
        project: PathBuf,
        index: usize,
        files: Vec<GitFile>,
    },
}

pub struct RightPanelView {
    model: Entity<AppState>,
    active_surface: Option<Surface>,
    project: Option<PathBuf>,
    tree_state: Entity<TreeState>,
    expanded_paths: HashSet<String>,
    review_tab: ReviewTab,
    history_filter_input: Entity<InputState>,
    selected_commit_sha: Option<String>,
    selected_commit_files: Vec<GitFile>,
    loading_commit_sha: Option<String>,
    review_files: Vec<GitFile>,
    selected_files: HashSet<String>,
    git_status: Option<GitStatus>,
    review_error: Option<String>,
    commit_message_input: Entity<InputState>,
    generated_commit_message: Option<String>,
    should_clear_commit_message: bool,
    git_busy: bool,
    git_message_pending: bool,
    git_feedback: Option<String>,
    branch_popover_open: bool,
    branch_filter_input: Entity<InputState>,
    new_branch_dialog_open: bool,
    new_branch_name_input: Entity<InputState>,
    merge_dialog_open: bool,
    merge_filter_input: Entity<InputState>,
    merge_selected_branch: Option<String>,
    switch_dialog_open: bool,
    switch_target_branch: Option<String>,
    switch_stash_mode: bool,
    stash_expanded: bool,
    stash_files: Option<(usize, Vec<GitFile>)>,
    loading_stash_index: Option<usize>,
    last_fetched_time: Option<std::time::Instant>,
    document_title: Option<String>,
    document_state: Entity<TextViewState>,
    editor_state: Option<Entity<EditorState>>,
    editor_subscription: Option<Subscription>,
    saved_content: String,
    is_dirty: bool,
    pending_document: Option<(String, String)>,
    event_tx: mpsc::Sender<PanelEvent>,
    _watcher: Option<WorkspaceWatcher>,
    _subscriptions: Vec<Subscription>,
}

impl RightPanelView {
    pub(crate) fn new(
        model: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let document_state = cx.new(|cx| TextViewState::markdown("", cx));
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let commit_message_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Summary (required)"));
        let branch_filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter branches…"));
        let new_branch_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. feature/new-workflow"));
        let merge_filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter branches to merge…"));
        let history_filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter commits…"));
        let (event_tx, event_rx) = mpsc::channel();

        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let events = event_rx.try_iter().collect::<Vec<_>>();
            if events.is_empty() {
                continue;
            }
            let _ = this.update(cx, |this, cx| {
                for event in events {
                    this.apply_event(event, cx);
                }
                cx.notify();
            });
        })
        .detach();

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

        let mut panel = Self {
            model,
            active_surface: None,
            project: None,
            tree_state,
            expanded_paths: HashSet::new(),
            review_tab: ReviewTab::Changes,
            history_filter_input,
            selected_commit_sha: None,
            selected_commit_files: Vec::new(),
            loading_commit_sha: None,
            review_files: Vec::new(),
            selected_files: HashSet::new(),
            git_status: None,
            review_error: None,
            commit_message_input,
            generated_commit_message: None,
            should_clear_commit_message: false,
            git_busy: false,
            git_message_pending: false,
            git_feedback: None,
            branch_popover_open: false,
            branch_filter_input,
            new_branch_dialog_open: false,
            new_branch_name_input,
            merge_dialog_open: false,
            merge_filter_input,
            merge_selected_branch: None,
            switch_dialog_open: false,
            switch_target_branch: None,
            switch_stash_mode: true,
            stash_expanded: false,
            stash_files: None,
            loading_stash_index: None,
            last_fetched_time: None,
            document_title: None,
            document_state,
            editor_state: None,
            editor_subscription: None,
            saved_content: String::new(),
            is_dirty: false,
            pending_document: None,
            event_tx,
            _watcher: None,
            _subscriptions: vec![observe_model, tree_subscription],
        };
        panel.sync_project(cx);
        panel
    }

    fn sync_project(&mut self, cx: &mut Context<Self>) {
        let project = self.model.read(cx).active_work_dir.clone();
        if self.project == project {
            return;
        }
        self.project = project.clone();
        self.tree_state
            .update(cx, |state, cx| state.set_items(Vec::new(), cx));
        self.expanded_paths.clear();
        self.review_files.clear();
        self.selected_files.clear();
        self.review_error = None;
        self.stash_files = None;
        self.loading_stash_index = None;
        self.stash_expanded = false;
        self.document_title = None;
        self.document_state
            .update(cx, |state, cx| state.set_text("", cx));

        if let Some(work_dir) = project {
            let tx = self.event_tx.clone();
            let proj = work_dir.clone();
            self._watcher =
                WorkspaceWatcher::start(work_dir, Duration::from_millis(200), move |change| {
                    let _ = tx.send(PanelEvent::WorkspaceChanged {
                        project: proj.clone(),
                        git_dirty: change.git_dirty,
                        files_dirty: change.files_dirty,
                    });
                })
                .ok();
        } else {
            self._watcher = None;
        }

        self.refresh_active_surface();
    }

    pub(crate) fn open_review(&mut self, cx: &mut Context<Self>) {
        self.open_surface(Surface::Review, cx);
    }

    pub(crate) fn open_branch_popover(&mut self, cx: &mut Context<Self>) {
        self.open_surface(Surface::Review, cx);
        self.branch_popover_open = true;
        cx.notify();
    }

    pub(crate) fn open_new_branch_dialog(&mut self, cx: &mut Context<Self>) {
        self.open_surface(Surface::Review, cx);
        self.new_branch_dialog_open = true;
        self.branch_popover_open = false;
        cx.notify();
    }

    pub(crate) fn open_merge_dialog(&mut self, cx: &mut Context<Self>) {
        self.open_surface(Surface::Review, cx);
        self.merge_dialog_open = true;
        self.merge_selected_branch = None;
        self.branch_popover_open = false;
        cx.notify();
    }

    pub(crate) fn open_files(&mut self, cx: &mut Context<Self>) {
        self.open_surface(Surface::Files, cx);
    }

    fn open_surface(&mut self, surface: Surface, cx: &mut Context<Self>) {
        if self.active_surface != Some(surface) {
            self.document_title = None;
            self.document_state
                .update(cx, |state, cx| state.set_text("", cx));
        }
        self.active_surface = Some(surface);
        self.refresh_surface(surface);
        cx.notify();
    }

    fn refresh_active_surface(&mut self) {
        if let Some(surface) = self.active_surface {
            self.refresh_surface(surface);
        }
    }

    fn refresh_surface(&self, surface: Surface) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let tx = self.event_tx.clone();
        std::thread::spawn(move || match surface {
            Surface::Files => {
                let nodes = scan_project_tree(&project, 500);
                let _ = tx.send(PanelEvent::FilesLoaded { project, nodes });
            }
            Surface::Review => {
                // Keep ahead/behind and PR checks current when the user refreshes Review.
                // Fetch failures are tolerated so local status remains available offline.
                let _ = threadlane_git::sync_remote(&project);
                let (status, files, error) = match threadlane_git::inspect(&project) {
                    Ok(status) => {
                        let files = status.files.clone();
                        (Some(status), files, None)
                    }
                    Err(error) => (None, Vec::new(), Some(error.to_string())),
                };
                let _ = tx.send(PanelEvent::ReviewLoaded {
                    project,
                    status,
                    files,
                    error,
                });
            }
        });
    }

    fn close_document(&mut self, cx: &mut Context<Self>) {
        self.document_title = None;
        self.editor_state = None;
        self.editor_subscription = None;
        self.saved_content.clear();
        self.is_dirty = false;
        self.pending_document = None;
        self.document_state
            .update(cx, |state, cx| state.set_text("", cx));
        cx.notify();
    }

    fn sync_pending_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((title, content)) = self.pending_document.take() else {
            return;
        };
        self.document_title = Some(title.clone());
        self.saved_content = content.clone();
        self.is_dirty = false;

        if title.starts_with("Review ·") {
            self.editor_state = None;
            self.editor_subscription = None;
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
                    let current = editor.read(cx).value();
                    let dirty = current.as_str() != this.saved_content.as_str();
                    if this.is_dirty != dirty {
                        this.is_dirty = dirty;
                        cx.notify();
                    }
                }
            });
            self.editor_state = Some(editor);
            self.editor_subscription = Some(subscription);
        }
    }

    fn save_active_document(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor_state.as_ref() else {
            return;
        };
        let Some(title) = self.document_title.as_ref() else {
            return;
        };
        let Some(project) = self.project.as_ref() else {
            return;
        };
        let target_path = project.join(title);
        let content = editor.read(cx).value().to_string();
        if std::fs::write(&target_path, &content).is_ok() {
            self.saved_content = content;
            self.is_dirty = false;
            cx.notify();
        }
    }

    fn apply_event(&mut self, event: PanelEvent, cx: &mut Context<Self>) {
        match event {
            PanelEvent::WorkspaceChanged {
                project,
                git_dirty,
                files_dirty,
            } if self.project.as_ref() == Some(&project) => {
                if git_dirty {
                    self.refresh_surface(Surface::Review);
                }
                if files_dirty {
                    self.refresh_surface(Surface::Files);
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
                self.git_status = status;
                let current_set: HashSet<String> = files.iter().map(|f| f.path.clone()).collect();
                if self.selected_files.is_empty() {
                    self.selected_files = current_set;
                } else {
                    let kept: HashSet<String> = self
                        .selected_files
                        .iter()
                        .filter(|p| current_set.contains(*p))
                        .cloned()
                        .collect();
                    if kept.is_empty() {
                        self.selected_files = current_set;
                    } else {
                        self.selected_files = kept;
                    }
                }
                self.review_files = files;
                self.review_error = error;
                self.stash_files = None;
                self.loading_stash_index = None;
            }
            PanelEvent::MessageGenerated(result) => {
                self.git_message_pending = false;
                match result {
                    Ok(message) => {
                        self.generated_commit_message = Some(message);
                        self.git_feedback = None;
                    }
                    Err(error) => {
                        self.git_feedback = Some(error);
                    }
                }
            }
            PanelEvent::ActionFinished(result) => {
                self.git_busy = false;
                match result {
                    Ok(status) => {
                        if let Some(project) = &self.project {
                            self.model.update(cx, |state, cx| {
                                state.git_statuses.insert(project.clone(), status.clone());
                                cx.notify();
                            });
                        }
                        self.git_status = Some(status.clone());
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
                        self.git_feedback = Some("Git action completed successfully.".into());
                    }
                    Err(error) => {
                        self.git_feedback = Some(error);
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

    pub(crate) fn refresh_review(&mut self, _cx: &mut Context<Self>) {
        self.refresh_surface(Surface::Review);
    }

    pub(crate) fn restore_current_stash(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        let (api_key, account_id) = crate::state::provider_credentials(&model);
        let tx = self.event_tx.clone();
        let Ok(executor) = crate::services::chat::executor() else {
            self.git_feedback = Some("Unable to start the model runtime.".into());
            cx.notify();
            return;
        };

        self.git_message_pending = true;
        self.git_feedback = Some("Generating a commit message…".into());
        executor.spawn(async move {
            let result = async {
                let diff = if selected_paths.len() == total_count {
                    threadlane_git::commit_message_diff(&work_dir)
                        .map_err(|error| error.to_string())?
                } else {
                    let mut diffs = Vec::new();
                    for path in &selected_paths {
                        if let Ok(d) = threadlane_git::diff_file(&work_dir, path) {
                            if !d.trim().is_empty() {
                                diffs.push(d);
                            }
                        }
                    }
                    diffs.join("\n")
                };
                let diff = if diff.chars().count() > 24_000 {
                    format!(
                        "{}\n\n[Diff truncated for message generation]",
                        diff.chars().take(24_000).collect::<String>()
                    )
                } else {
                    diff
                };
                let raw = threadlane_provider::ProviderClient::new(api_key, account_id)
                    .generate_commit_message(&model, &diff)
                    .await?;
                let message = normalize_generated_commit_message(&raw);
                if message.is_empty() {
                    Err("The model returned an empty commit message.".to_string())
                } else {
                    Ok(message)
                }
            }
            .await;
            let _ = tx.send(PanelEvent::MessageGenerated(result));
        });
        cx.notify();
    }

    pub(crate) fn run_git_action(
        &mut self,
        action: GitAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(work_dir) = self.project.clone() else {
            self.git_feedback = Some("Attach a project to use Git actions.".into());
            window.push_notification(
                Notification::warning("Attach a project to use Git actions"),
                cx,
            );
            cx.notify();
            return;
        };
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
                window.push_notification(
                    Notification::warning("Select at least one file to commit"),
                    cx,
                );
                cx.notify();
                return;
            }
            if message.is_empty() {
                self.git_feedback = Some("Enter a commit message first.".into());
                window.push_notification(Notification::warning("Enter a commit message first"), cx);
                cx.notify();
                return;
            }
        }

        self.git_busy = true;
        let feedback = match &action {
            GitAction::Commit => "Committing…".to_string(),
            GitAction::CommitAndPush => "Committing and pushing…".to_string(),
            GitAction::Push => "Pushing…".to_string(),
            GitAction::Pull => "Pulling from origin…".to_string(),
            GitAction::Fetch => "Fetching origin…".to_string(),
            GitAction::Checkout(b) => format!("Switching to {b}…"),
            GitAction::CheckoutStash(b) => format!("Stashing changes & switching to {b}…"),
            GitAction::CheckoutCarry(b) => format!("Switching to {b} with changes…"),
            GitAction::CreateBranch(b) => format!("Creating branch {b}…"),
            GitAction::Merge(b) => format!("Merging {b}…"),
            GitAction::PopStash(_) => "Restoring stashed changes…".to_string(),
            GitAction::DropStash(_) => "Discarding stash…".to_string(),
            GitAction::DiscardFile(p) => format!("Discarding changes in {p}…"),
            GitAction::IgnoreFile(p) => format!("Adding {p} to .gitignore…"),
            GitAction::IgnoreExtension(ext) => format!("Ignoring *.{ext} files…"),
        };
        self.git_feedback = Some(feedback.clone());
        window.push_notification(Notification::info(feedback), cx);
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                match &action {
                    GitAction::Commit | GitAction::CommitAndPush => {
                        let status = threadlane_git::inspect(&work_dir).map_err(|e| e.to_string())?;
                        let selected_set: HashSet<&str> =
                            selected_paths.iter().map(String::as_str).collect();
                        for file in &status.files {
                            if selected_set.contains(file.path.as_str()) {
                                threadlane_git::stage_file(&work_dir, &file.path)
                                    .map_err(|e| e.to_string())?;
                            } else {
                                let _ = threadlane_git::unstage_file(&work_dir, &file.path);
                            }
                        }
                        threadlane_git::commit_staged(&work_dir, &message)
                            .map_err(|e| e.to_string())?;
                        if matches!(action, GitAction::CommitAndPush) {
                            threadlane_git::push(&work_dir).map_err(|e| e.to_string())?;
                        }
                    }
                    GitAction::Push => {
                        threadlane_git::push(&work_dir).map_err(|e| e.to_string())?;
                    }
                    GitAction::Pull => {
                        threadlane_git::pull(&work_dir).map_err(|e| e.to_string())?;
                    }
                    GitAction::Fetch => {
                        threadlane_git::fetch(&work_dir).map_err(|e| e.to_string())?;
                    }
                    GitAction::Checkout(branch) => {
                        threadlane_git::checkout(&work_dir, branch).map_err(|e| e.to_string())?;
                    }
                    GitAction::CheckoutStash(branch) => {
                        threadlane_git::checkout_with_stash(&work_dir, branch).map_err(|e| e.to_string())?;
                    }
                    GitAction::CheckoutCarry(branch) => {
                        threadlane_git::checkout_carrying_changes(&work_dir, branch).map_err(|e| e.to_string())?;
                    }
                    GitAction::CreateBranch(branch) => {
                        threadlane_git::create_branch(&work_dir, branch).map_err(|e| e.to_string())?;
                    }
                    GitAction::Merge(branch) => {
                        threadlane_git::merge(&work_dir, branch).map_err(|e| e.to_string())?;
                    }
                    GitAction::PopStash(idx) => {
                        threadlane_git::pop_stash(&work_dir, *idx).map_err(|e| e.to_string())?;
                    }
                    GitAction::DropStash(idx) => {
                        threadlane_git::drop_stash(&work_dir, *idx).map_err(|e| e.to_string())?;
                    }
                    GitAction::DiscardFile(path) => {
                        threadlane_git::discard_file_changes(&work_dir, path).map_err(|e| e.to_string())?;
                    }
                    GitAction::IgnoreFile(path) => {
                        threadlane_git::ignore_file(&work_dir, path).map_err(|e| e.to_string())?;
                    }
                    GitAction::IgnoreExtension(ext) => {
                        threadlane_git::ignore_extension(&work_dir, ext).map_err(|e| e.to_string())?;
                    }
                }
                threadlane_git::inspect(&work_dir).map_err(|e| e.to_string())
            })();
            let _ = tx.send(PanelEvent::ActionFinished(result));
        });
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let active = self.active_surface;
        div()
            .flex_none()
            .pt(px(44.0))
            .pb_2()
            .px_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .children([Surface::Review, Surface::Files].map(|surface| {
                        Button::new(SharedString::from(format!(
                            "right-panel-tab-{}",
                            surface.label().to_lowercase()
                        )))
                        .icon(surface.icon())
                        .label(surface.label())
                        .ghost()
                        .selected(active == Some(surface))
                        .small()
                        .on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                this.open_surface(surface, cx);
                            },
                        ))
                    }))
                    .child(div().flex_1())
                    .child(
                        Button::new("right-panel-refresh")
                            .icon(IconName::Redo)
                            .tooltip("Refresh surface")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.refresh_active_surface();
                                cx.notify();
                            })),
                    ),
            )
    }

    fn render_chooser(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .p_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(420.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Open a surface"),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Choose what to show in the right panel"),
                    )
                    .child(div().mt_4().w_full().flex().gap_2().children(
                        [Surface::Review, Surface::Files].map(|surface| {
                            Button::new(SharedString::from(format!(
                                "right-panel-card-{}",
                                surface.label().to_lowercase()
                            )))
                            .child(
                                div()
                                    .size_full()
                                    .p_3()
                                    .flex()
                                    .flex_col()
                                    .items_start()
                                    .justify_center()
                                    .gap_2()
                                    .text_sm()
                                    .child(surface.icon())
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(surface.label()),
                                    ),
                            )
                            .outline()
                            .flex_1()
                            .h(px(104.0))
                            .p_0()
                            .on_click(cx.listener(
                                move |this, _event, _window, cx| {
                                    this.open_surface(surface, cx);
                                },
                            ))
                        }),
                    )),
            )
    }

    fn render_files(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(title) = &self.document_title {
            let is_dirty = self.is_dirty;
            let has_editor = self.editor_state.is_some();
            let lang = detect_language(title);
            return div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(38.0))
                        .px_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .min_w_0()
                                .flex_1()
                                .child(
                                    Button::new("right-panel-document-back")
                                        .icon(IconName::ArrowLeft)
                                        .tooltip(match self.active_surface {
                                            Some(Surface::Review) => "Back to changed files",
                                            _ => "Back to project files",
                                        })
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.close_document(cx);
                                        })),
                                )
                                .child(IconName::File)
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(title.clone()),
                                )
                                .children(
                                    is_dirty.then(|| Tag::warning().child("modified").xsmall()),
                                )
                                .children(
                                    has_editor
                                        .then(|| Tag::secondary().child(lang).outline().xsmall()),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .children(has_editor.then(|| {
                                    Button::new("save-document")
                                        .small()
                                        .label("Save")
                                        .icon(IconName::Check)
                                        .disabled(!is_dirty)
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.save_active_document(cx);
                                        }))
                                }))
                                .child(
                                    Button::new("close-document")
                                        .small()
                                        .ghost()
                                        .icon(IconName::Close)
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.close_document(cx);
                                        })),
                                ),
                        ),
                )
                .child(Separator::horizontal())
                .child(if let Some(ref editor) = self.editor_state {
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .h_full()
                        .child(Editor::new(editor).bordered(false).size_full())
                        .into_any_element()
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

        div()
            .flex_1()
            .min_h_0()
            .py_2()
            .child(
                Tree::new(
                    &self.tree_state,
                    move |ix, entry, is_selected, _window, cx| {
                        let relative_path = entry.item().id.to_string();
                        let name = entry.item().label.to_string();
                        let is_folder = entry.is_folder();
                        let is_expanded = entry.is_expanded();
                        let depth = entry.depth();

                        let target_path = relative_path.clone();
                        let click_model = model.clone();
                        let theme = cx.theme().colors;

                        ListItem::new(format!("tree-item-{ix}"))
                            .mx_1()
                            .rounded_md()
                            .px_1p5()
                            .py_1()
                            .pl(px(6.0 + depth as f32 * 12.0))
                            .selected(is_selected)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .text_xs()
                                    .text_color(if is_selected {
                                        theme.foreground
                                    } else {
                                        theme.muted_foreground
                                    })
                                    .child(if is_folder {
                                        div()
                                            .w(px(14.0))
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(if is_expanded {
                                                Icon::new(IconName::ChevronDown)
                                                    .xsmall()
                                                    .into_any_element()
                                            } else {
                                                Icon::new(IconName::ChevronRight)
                                                    .xsmall()
                                                    .into_any_element()
                                            })
                                            .into_any_element()
                                    } else {
                                        div().w(px(14.0)).flex_none().into_any_element()
                                    })
                                    .child(if is_folder {
                                        Icon::new(IconName::Folder).xsmall().into_any_element()
                                    } else {
                                        Icon::new(IconName::File).xsmall().into_any_element()
                                    })
                                    .child(name),
                            )
                            .when(!is_folder, move |item| {
                                item.on_click(move |_event, _window, cx| {
                                    click_model.update(cx, |state, cx| {
                                        state.request_open_file(target_path.clone());
                                        cx.notify();
                                    });
                                })
                            })
                    },
                )
                .context_menu({
                    let model = self.model.clone();
                    let project = self.project.clone();
                    move |_ix, entry, menu, _window, _cx| {
                        let relative_path = entry.item().id.to_string();
                        let is_folder = entry.is_folder();
                        let absolute_path = project
                            .as_ref()
                            .map(|p| p.join(&relative_path).display().to_string());
                        let ed_path = relative_path.clone();
                        let text = relative_path.clone();
                        let model_ref = model.clone();

                        let mut menu = menu;
                        if !is_folder {
                            menu = menu.item(PopupMenuItem::new("Open in Editor Tab").on_click(
                                move |_event, _window, cx| {
                                    model_ref.update(cx, |state, cx| {
                                        state.request_open_file(ed_path.clone());
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        menu = menu.item(PopupMenuItem::new("Copy Relative Path").on_click(
                            move |_event, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                window.push_notification(
                                    Notification::info("Copied relative path"),
                                    cx,
                                );
                            },
                        ));
                        if let Some(abs) = absolute_path {
                            menu = menu.item(PopupMenuItem::new("Copy Absolute Path").on_click(
                                move |_event, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(abs.clone()));
                                    window.push_notification(
                                        Notification::info("Copied absolute path"),
                                        cx,
                                    );
                                },
                            ));
                        }
                        menu
                    }
                }),
            )
            .into_any_element()
    }

    fn render_review(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        if let Some(error) = &self.review_error {
            return self.render_empty("Review unavailable", error, cx);
        }
        let total_files = self.review_files.len();
        let selected_count = self.selected_files.len();
        let all_selected = total_files > 0 && selected_count == total_files;

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

        let can_commit = !self.git_busy && !self.git_message_pending && selected_count > 0;
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
        let can_publish = can_publish_branch(status);

        let sync_button = if can_publish {
            Button::new("git-sync-action-btn")
                .icon(IconName::ArrowUp)
                .label("Publish Branch")
                .primary()
                .small()
                .tooltip("Publish this branch to origin")
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.run_git_action(GitAction::Push, window, cx);
                }))
        } else if behind > 0 {
            Button::new("git-sync-action-btn")
                .icon(IconName::ArrowDown)
                .label(format!("Pull ({behind})"))
                .primary()
                .small()
                .tooltip("Pull latest changes from origin")
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.run_git_action(GitAction::Pull, window, cx);
                }))
        } else if ahead > 0 {
            Button::new("git-sync-action-btn")
                .icon(IconName::ArrowUp)
                .label(format!("Push ({ahead})"))
                .primary()
                .small()
                .tooltip("Push local commits to origin")
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.run_git_action(GitAction::Push, window, cx);
                }))
        } else {
            Button::new("git-sync-action-btn")
                .icon(IconName::Redo)
                .label("Fetch")
                .ghost()
                .small()
                .tooltip(last_fetched_str)
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.run_git_action(GitAction::Fetch, window, cx);
                }))
        };

        let branch_header = div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.3))
            .child(
                div()
                    .id("git-branch-selector-btn")
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .flex_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.muted.opacity(0.6)))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.branch_popover_open = !this.branch_popover_open;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(16.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(Icon::default().path("icons/git/branch.svg")),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(branch.to_string()),
                    )
                    .child(
                        div()
                            .size(px(14.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(if self.branch_popover_open {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronDown
                            }),
                    ),
            )
            .child(sync_button);

        let pr_card = self.git_status.as_ref().and_then(|s| s.pr.as_ref()).map(|pr| {
            let pr_url = pr.url.clone();
            let pr_num = pr.number;
            let pr_title = pr.title.clone();
            let pr_title_display = if pr.title.is_empty() {
                format!("PR #{pr_num}")
            } else {
                format!("#{pr_num} {}", pr.title)
            };

            let failing_checks = pr.failing_checks;
            let pending_checks = pr.pending_checks;
            let total_checks = pr.total_checks;
            let comments_count = pr.comments_count;

            let failing_check_names: Vec<String> = pr
                .checks
                .iter()
                .filter(|c| {
                    let concl = c.conclusion.as_deref().unwrap_or("").to_uppercase();
                    matches!(
                        concl.as_str(),
                        "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED" | "ERROR"
                    )
                })
                .map(|c| c.name.clone())
                .collect();
            let failed_summary = failing_check_names.join(", ");

            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .p_2p5()
                .mx_2()
                .my_1p5()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted.opacity(0.2))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .min_w_0()
                                .flex_1()
                                .child(
                                    div()
                                        .size(px(16.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(theme.muted_foreground)
                                        .child(Icon::default().path("icons/git/actions.svg")),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.foreground)
                                        .child(pr_title_display),
                                ),
                        )
                        .when(!pr_url.is_empty(), |row| {
                            let target_url = pr_url.clone();
                            row.child(
                                Button::new("pr-link-btn")
                                    .icon(IconName::ExternalLink)
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Open pull request in browser")
                                    .on_click(move |_event, _window, cx| {
                                        cx.open_url(&target_url);
                                    }),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .pt_0p5()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .min_w_0()
                                .child(
                                    div()
                                        .size(px(14.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(if failing_checks > 0 {
                                            theme.danger
                                        } else if pending_checks > 0 {
                                            theme.warning
                                        } else {
                                            theme.success
                                        })
                                        .child(if failing_checks > 0 {
                                            IconName::Close
                                        } else if pending_checks > 0 {
                                            IconName::Asterisk
                                        } else {
                                            IconName::Check
                                        }),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(if failing_checks > 0 {
                                            theme.danger
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .child(if failing_checks > 0 {
                                            format!(
                                                "{failing_checks} failing check{}",
                                                if failing_checks == 1 { "" } else { "s" }
                                            )
                                        } else if pending_checks > 0 {
                                            format!("{pending_checks} in progress")
                                        } else {
                                            format!("All {} checks passed", total_checks.max(1))
                                        }),
                                ),
                        )
                        .child(if failing_checks > 0 {
                            let fix_pr_num = pr_num;
                            let fix_pr_title = pr_title.clone();
                            let fix_failed_summary = failed_summary.clone();
                            Button::new("fix-ci-btn")
                                .label("Fix CI")
                                .danger()
                                .xsmall()
                                .tooltip("Ask AI to fix failing CI checks")
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    let prompt = format!(
                                        "Please inspect and fix the failing CI check on PR #{fix_pr_num} ({fix_pr_title}): {fix_failed_summary}"
                                    );
                                    this.model.update(cx, |state, _cx| {
                                        state.request_composer_prompt(prompt);
                                    });
                                    cx.notify();
                                }))
                                .into_any_element()
                        } else {
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{}/{}", pr.passing_checks, pr.total_checks))
                                .into_any_element()
                        }),
                )
                .when(comments_count > 0, |card| {
                    let comments_pr_num = pr_num;
                    let comments_pr_title = pr_title.clone();
                    card.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .pt_0p5()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .size(px(14.0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_color(theme.muted_foreground)
                                            .child(IconName::File),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!(
                                                "{comments_count} review comment{}",
                                                if comments_count == 1 { "" } else { "s" }
                                            )),
                                    ),
                            )
                            .child(
                                Button::new("address-comments-btn")
                                    .label("Address")
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Ask AI to address PR comments")
                                    .on_click(cx.listener(move |this, _event, _window, cx| {
                                        let prompt = format!(
                                            "Please review and address comments and feedback on PR #{comments_pr_num} ({comments_pr_title})."
                                        );
                                        this.model.update(cx, |state, _cx| {
                                            state.request_composer_prompt(prompt);
                                        });
                                        cx.notify();
                                    })),
                            ),
                    )
                })
        });

        let selection_bar = (total_files > 0).then(|| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .px_3()
                .py_1p5()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.muted.opacity(0.15))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Checkbox::new("select-all-files")
                                .checked(all_selected)
                                .small()
                                .on_click(cx.listener(move |this, checked, _window, cx| {
                                    if *checked {
                                        this.selected_files = this
                                            .review_files
                                            .iter()
                                            .map(|f| f.path.clone())
                                            .collect();
                                    } else {
                                        this.selected_files.clear();
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child(if all_selected {
                                    format!("{total_files} changed files")
                                } else {
                                    format!("{selected_count} of {total_files} selected")
                                }),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            Tag::new()
                                .child(format!("+{selected_additions}"))
                                .with_variant(TagVariant::Success)
                                .small(),
                        )
                        .child(
                            Tag::new()
                                .child(format!("−{selected_deletions}"))
                                .with_variant(TagVariant::Danger)
                                .small(),
                        ),
                )
        });

        let panel_entity = cx.entity().clone();
        let file_list_content = if self.review_files.is_empty() {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .p_4()
                .text_center()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child("No changes"),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("The working tree is clean."),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .py_1()
                .children(self.review_files.iter().cloned().map(|file| {
                    let path = file.path.clone();
                    let path_for_chk = path.clone();
                    let is_selected = self.selected_files.contains(&path);
                    let absolute_path = self
                        .project
                        .as_ref()
                        .map(|root| root.join(&path).display().to_string());
                    let status = file.status_char().to_string();
                    let status_color = match file.status_char() {
                        'A' | '?' => theme.success,
                        'D' => theme.danger,
                        _ => theme.warning,
                    };
                    let context_path = path.clone();
                    div()
                        .id(SharedString::from(format!("review-file-{path}")))
                        .h(px(32.0))
                        .mx_2()
                        .px_2()
                        .rounded_md()
                        .flex()
                        .items_center()
                        .gap_2()
                        .hover(|row| row.bg(theme.muted))
                        .child(
                            Checkbox::new(SharedString::from(format!("chk-{path}")))
                                .checked(is_selected)
                                .small()
                                .on_click(cx.listener(move |this, checked, _window, cx| {
                                    if *checked {
                                        this.selected_files.insert(path_for_chk.clone());
                                    } else {
                                        this.selected_files.remove(&path_for_chk);
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("review-file-btn-{path}")))
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .child(
                                    div()
                                        .size(px(14.0))
                                        .text_color(theme.muted_foreground)
                                        .child(IconName::File),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .truncate()
                                        .text_xs()
                                        .text_color(theme.foreground)
                                        .child(path.clone()),
                                )
                                .when(file.additions > 0, |row| {
                                    row.child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.success)
                                            .child(format!("+{}", file.additions)),
                                    )
                                })
                                .when(file.deletions > 0, |row| {
                                    row.child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.danger)
                                            .child(format!("-{}", file.deletions)),
                                    )
                                })
                                .child(
                                    div()
                                        .size(px(16.0))
                                        .rounded_sm()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_xs()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(status_color)
                                        .child(status),
                                )
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    let target_path = path.clone();
                                    let Some(project) = this.project.clone() else {
                                        return;
                                    };
                                    let diff_project = project.clone();
                                    let model = this.model.clone();
                                    cx.spawn(async move |_this, cx| {
                                        let diff_target = target_path.clone();
                                        let content = cx
                                            .background_executor()
                                            .spawn(async move {
                                                threadlane_git::diff_file(
                                                    &diff_project,
                                                    &diff_target,
                                                )
                                                .unwrap_or_else(|error| error.to_string())
                                            })
                                            .await;
                                        let _ = model.update(cx, |state, cx| {
                                            state.request_open_diff(project, target_path, content);
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                })),
                        )
                        .context_menu({
                            let path = context_path.clone();
                            let absolute_path = absolute_path.clone();
                            let project = self.project.clone();
                            let model = self.model.clone();
                            let panel = panel_entity.clone();
                            let ext = std::path::Path::new(&path)
                                .extension()
                                .and_then(|e| e.to_str())
                                .map(|e| e.to_string());
                            move |menu, _window, _cx| {
                                let diff_path = path.clone();
                                let discard_path = path.clone();
                                let ignore_path = path.clone();
                                let rel_path_1 = path.clone();
                                let rel_path_2 = path.clone();
                                let project_ref = project.clone();
                                let model_ref = model.clone();
                                let panel_discard = panel.clone();
                                let panel_ignore = panel.clone();
                                let panel_ignore_ext = panel.clone();

                                let mut menu = menu
                                    .item(
                                        PopupMenuItem::new("Discard Changes...").on_click(
                                            move |_event, window, cx| {
                                                let p = discard_path.clone();
                                                panel_discard.update(cx, |this, cx| {
                                                    this.run_git_action(
                                                        GitAction::DiscardFile(p),
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            },
                                        ),
                                    )
                                    .item(
                                        PopupMenuItem::new("Ignore File (Add to .gitignore)")
                                            .on_click(move |_event, window, cx| {
                                                let p = ignore_path.clone();
                                                panel_ignore.update(cx, |this, cx| {
                                                    this.run_git_action(
                                                        GitAction::IgnoreFile(p),
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            }),
                                    );

                                if let Some(ext_str) = ext.clone() {
                                    let ext_action = ext_str.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(format!(
                                            "Ignore All .{ext_str} Files (Add to .gitignore)"
                                        ))
                                        .on_click(move |_event, window, cx| {
                                            let e = ext_action.clone();
                                            panel_ignore_ext.update(cx, |this, cx| {
                                                this.run_git_action(
                                                    GitAction::IgnoreExtension(e),
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }),
                                    );
                                }

                                menu = menu.separator().item(
                                    PopupMenuItem::new("Open Diff in Editor Tab").on_click(
                                        move |_event, _window, cx| {
                                            let Some(proj) = project_ref.clone() else {
                                                return;
                                            };
                                            let diff_project = proj.clone();
                                            let target = diff_path.clone();
                                            let m = model_ref.clone();
                                            cx.spawn(async move |cx| {
                                                let diff_target = target.clone();
                                                let content = cx
                                                    .background_executor()
                                                    .spawn(async move {
                                                        threadlane_git::diff_file(
                                                            &diff_project,
                                                            &diff_target,
                                                        )
                                                        .unwrap_or_else(|error| error.to_string())
                                                    })
                                                    .await;
                                                let _ = m.update(cx, |state, cx| {
                                                    state.request_open_diff(proj, target, content);
                                                    cx.notify();
                                                });
                                            })
                                            .detach();
                                        },
                                    ),
                                );

                                menu = menu
                                    .separator()
                                    .item(
                                        PopupMenuItem::new("Copy File Path").on_click(
                                            move |_event, window, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    rel_path_1.clone(),
                                                ));
                                                window.push_notification(
                                                    Notification::info("Copied file path"),
                                                    cx,
                                                );
                                            },
                                        ),
                                    )
                                    .item(
                                        PopupMenuItem::new("Copy Relative File Path").on_click(
                                            move |_event, window, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    rel_path_2.clone(),
                                                ));
                                                window.push_notification(
                                                    Notification::info(
                                                        "Copied relative file path",
                                                    ),
                                                    cx,
                                                );
                                            },
                                        ),
                                    );

                                if let Some(absolute_path) = absolute_path.clone() {
                                    let abs_text = absolute_path.clone();
                                    let reveal_text = absolute_path.clone();
                                    let reveal_label = if cfg!(target_os = "macos") {
                                        "Reveal in Finder"
                                    } else if cfg!(target_os = "windows") {
                                        "Reveal in File Explorer"
                                    } else {
                                        "Reveal in File Manager"
                                    };

                                    menu = menu
                                        .item(
                                            PopupMenuItem::new("Copy Absolute File Path").on_click(
                                                move |_event, window, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(abs_text.clone()),
                                                    );
                                                    window.push_notification(
                                                        Notification::info(
                                                            "Copied absolute file path",
                                                        ),
                                                        cx,
                                                    );
                                                },
                                            ),
                                        )
                                        .separator()
                                        .item(
                                            PopupMenuItem::new(reveal_label).on_click(
                                                move |_event, _window, _cx| {
                                                    threadlane_git::reveal_in_file_manager(
                                                        std::path::Path::new(&reveal_text),
                                                    );
                                                },
                                            ),
                                        );
                                }
                                menu
                            }
                        })
                }))
                .into_any_element()
        };

        let commit_label = if selected_count > 0 && selected_count < total_files {
            format!("Commit {selected_count}")
        } else {
            "Commit".to_string()
        };
        let commit_push_label = if selected_count > 0 && selected_count < total_files {
            format!("Commit {selected_count} & push")
        } else {
            "Commit & push".to_string()
        };

        let commit_footer = div()
            .flex_none()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child("COMMIT"),
                    )
                    .child(
                        Button::new("git-generate-message")
                            .when(self.git_message_pending, |button| {
                                button.child(Spinner::new().xsmall()).label("Generating…")
                            })
                            .when(!self.git_message_pending, |button| button.label("Generate"))
                            .ghost()
                            .xsmall()
                            .disabled(
                                self.git_busy || self.git_message_pending || selected_count == 0,
                            )
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.generate_commit_message(cx);
                            })),
                    ),
            )
            .child(Input::new(&self.commit_message_input).disabled(self.git_busy))
            .children(self.git_feedback.as_ref().map(|feedback| {
                div()
                    .rounded_md()
                    .bg(theme.muted)
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(feedback.clone())
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("git-commit-push")
                            .icon(Icon::default().path("icons/git/commit.svg"))
                            .label(commit_push_label)
                            .primary()
                            .small()
                            .flex_1()
                            .disabled(!can_commit)
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.run_git_action(GitAction::CommitAndPush, window, cx);
                            })),
                    )
                    .child(
                        Button::new("git-commit-only")
                            .label(commit_label)
                            .outline()
                            .small()
                            .disabled(!can_commit)
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.run_git_action(GitAction::Commit, window, cx);
                            })),
                    )
                    .when(can_push, |row| {
                        row.child(
                            Button::new("git-push-only")
                                .icon(Icon::default().path("icons/git/actions.svg"))
                                .tooltip("Push commits")
                                .ghost()
                                .small()
                                .on_click(cx.listener(|this, _event, window, cx| {
                                    this.run_git_action(GitAction::Push, window, cx);
                                })),
                        )
                    }),
            );

        let stash_banner = self.git_status.as_ref().and_then(|s| s.current_stash.as_ref()).map(|stash| {
            let stash_msg = if stash.message.is_empty() {
                "Stashed changes on this branch".to_string()
            } else {
                stash.message.clone()
            };
            let time_str = if stash.relative_time.is_empty() {
                String::new()
            } else {
                format!(" • {}", stash.relative_time)
            };
            let idx = stash.index;
            let is_expanded = self.stash_expanded;
            let files_clone = self
                .stash_files
                .as_ref()
                .filter(|(index, _)| *index == idx)
                .map(|(_, files)| files.clone())
                .unwrap_or_default();
            let is_loading = self.loading_stash_index == Some(idx);
            let count_label = if is_loading {
                "Loading files…".to_string()
            } else if self.stash_files.as_ref().is_some_and(|(index, _)| *index == idx) {
                if files_clone.len() == 1 {
                    "1 file".to_string()
                } else {
                    format!("{} files", files_clone.len())
                }
            } else {
                "Stashed changes".to_string()
            };
            let project = self.project.clone();
            let model = self.model.clone();

            div()
                .id("stash-banner")
                .mx_3()
                .my_2()
                .p_2p5()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted.opacity(0.5))
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .id("stash-header-toggle")
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            this.stash_expanded = !this.stash_expanded;
                            if this.stash_expanded
                                && this.loading_stash_index != Some(idx)
                                && this
                                    .stash_files
                                    .as_ref()
                                    .is_none_or(|(index, _)| *index != idx)
                            {
                                if let Some(project) = this.project.clone() {
                                    this.loading_stash_index = Some(idx);
                                    let tx = this.event_tx.clone();
                                    std::thread::spawn(move || {
                                        let files = threadlane_git::inspect_stash_files(&project, idx);
                                        let _ = tx.send(PanelEvent::StashFilesLoaded {
                                            project,
                                            index: idx,
                                            files,
                                        });
                                    });
                                }
                            }
                            cx.notify();
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .min_w_0()
                                .flex_1()
                                .child(
                                    div()
                                        .size(px(14.0))
                                        .text_color(theme.primary)
                                        .child(if is_expanded { IconName::ChevronDown } else { IconName::ChevronRight }),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(theme.foreground)
                                        .child("Stashed Changes"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("({count_label}{time_str})")),
                                ),
                        ),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(stash_msg),
                )
                .children(is_expanded.then(|| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .my_1()
                        .p_1p5()
                        .rounded_md()
                        .bg(theme.background)
                        .border_1()
                        .border_color(theme.border)
                        .children(files_clone.into_iter().map(|file| {
                            let path = file.path.clone();
                            let status = file.status_char().to_string();
                            let status_color = match file.status_char() {
                                'A' | '?' => theme.success,
                                'D' => theme.danger,
                                _ => theme.warning,
                            };
                            let adds = file.additions;
                            let dels = file.deletions;
                            let file_path_for_click = path.clone();
                            let project_for_click = project.clone();
                            let model_for_click = model.clone();

                            div()
                                .id(SharedString::from(format!("stash-file-{path}")))
                                .h(px(26.0))
                                .px_2()
                                .rounded_sm()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .cursor_pointer()
                                .hover(|row| row.bg(theme.muted))
                                .on_click(cx.listener(move |_this, _event, _window, cx| {
                                    let Some(proj) = project_for_click.clone() else { return; };
                                    let target_path = file_path_for_click.clone();
                                    let diff_project = proj.clone();
                                    let m = model_for_click.clone();
                                    cx.spawn(async move |_this, cx| {
                                        let diff_target = target_path.clone();
                                        let content = cx
                                            .background_executor()
                                            .spawn(async move {
                                                threadlane_git::diff_stash_file(
                                                    &diff_project,
                                                    idx,
                                                    &diff_target,
                                                )
                                                .unwrap_or_else(|err| err.to_string())
                                            })
                                            .await;
                                        let _ = m.update(cx, |state, cx| {
                                            state.request_open_diff(proj, target_path, content);
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .min_w_0()
                                        .flex_1()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(FontWeight::BOLD)
                                                .text_color(status_color)
                                                .child(status),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_xs()
                                                .text_color(theme.foreground)
                                                .child(path),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_xs()
                                        .child(
                                            div()
                                                .text_color(theme.success)
                                                .child(format!("+{adds}")),
                                        )
                                        .child(
                                            div()
                                                .text_color(theme.danger)
                                                .child(format!("-{dels}")),
                                        ),
                                )
                        }))
                        .when(is_loading, |container| {
                            container.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Loading stashed files…"),
                            )
                        })
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .pt_1()
                        .child(
                            Button::new("discard-stash-btn")
                                .label("Discard")
                                .ghost()
                                .xsmall()
                                .disabled(self.git_busy)
                                .on_click(cx.listener(move |this, _event, window, cx| {
                                    this.run_git_action(GitAction::DropStash(Some(idx)), window, cx);
                                })),
                        )
                        .child(
                            Button::new("restore-stash-btn")
                                .label("Restore Stash")
                                .primary()
                                .xsmall()
                                .disabled(self.git_busy)
                                .on_click(cx.listener(move |this, _event, window, cx| {
                                    this.run_git_action(GitAction::PopStash(Some(idx)), window, cx);
                                })),
                        ),
                )
        });

        let changes_active = self.review_tab == ReviewTab::Changes;
        let history_active = self.review_tab == ReviewTab::History;
        let total_changes = self.review_files.len();

        let review_sub_tabs = div()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(
                div()
                    .id("review-tab-changes")
                    .flex_1()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1p5()
                    .cursor_pointer()
                    .border_b_2()
                    .border_color(if changes_active {
                        theme.primary
                    } else {
                        gpui::transparent_black()
                    })
                    .hover(|s| s.bg(theme.muted.opacity(0.4)))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.review_tab = ReviewTab::Changes;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(if changes_active {
                                FontWeight::BOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(if changes_active {
                                theme.foreground
                            } else {
                                theme.muted_foreground
                            })
                            .child("Changes"),
                    )
                    .children((total_changes > 0).then(|| {
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_full()
                            .bg(if changes_active {
                                theme.muted
                            } else {
                                theme.muted.opacity(0.5)
                            })
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(if changes_active {
                                theme.foreground
                            } else {
                                theme.muted_foreground
                            })
                            .child(format!("{total_changes}"))
                    })),
            )
            .child(
                div()
                    .id("review-tab-history")
                    .flex_1()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1p5()
                    .cursor_pointer()
                    .border_b_2()
                    .border_color(if history_active {
                        theme.primary
                    } else {
                        gpui::transparent_black()
                    })
                    .hover(|s| s.bg(theme.muted.opacity(0.4)))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.review_tab = ReviewTab::History;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(if history_active {
                                FontWeight::BOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(if history_active {
                                theme.foreground
                            } else {
                                theme.muted_foreground
                            })
                            .child("History"),
                    ),
            );

        let review_body = if self.branch_popover_open {
            self.render_branch_manager(cx).into_any_element()
        } else if self.review_tab == ReviewTab::History {
            self.render_history(cx).into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .children(stash_banner)
                .children(pr_card)
                .children(selection_bar)
                .child(file_list_content)
                .child(commit_footer)
                .into_any_element()
        };

        div()
            .flex_1()
            .min_h_0()
            .relative()
            .flex()
            .flex_col()
            .child(branch_header)
            .children((!self.branch_popover_open).then(|| review_sub_tabs))
            .child(review_body)
            .into_any_element()
    }

    fn render_history(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let filter_text = self.history_filter_input.read(cx).value().trim().to_lowercase();
        let commits = self.git_status.as_ref().map(|s| &s.recent_commits);

        let filtered_commits: Vec<&GitCommitInfo> = if let Some(commits) = commits {
            if filter_text.is_empty() {
                commits.iter().collect()
            } else {
                commits
                    .iter()
                    .filter(|c| {
                        c.summary.to_lowercase().contains(&filter_text)
                            || c.author_name.to_lowercase().contains(&filter_text)
                            || c.short_sha.to_lowercase().contains(&filter_text)
                    })
                    .collect()
            }
        } else {
            Vec::new()
        };

        let commit_list = if filtered_commits.is_empty() {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .p_4()
                .text_center()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child("No commits found"),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(if filter_text.is_empty() {
                            "This branch has no recent commits."
                        } else {
                            "No commits match your filter."
                        }),
                )
                .into_any_element()
        } else {
            let project = self.project.clone();
            let selected_sha = self.selected_commit_sha.clone();
            let selected_files = self.selected_commit_files.clone();
            let loading_sha = self.loading_commit_sha.clone();
            let model = self.model.clone();
            let event_tx = self.event_tx.clone();

            div()
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .py_1()
                .children(filtered_commits.into_iter().map(|commit| {
                    let sha = commit.sha.clone();
                    let short_sha = commit.short_sha.clone();
                    let summary = commit.summary.clone();
                    let author = commit.author_name.clone();
                    let rel_time = commit.relative_time.clone();
                    let is_expanded = selected_sha.as_deref() == Some(&sha);
                    let is_loading = loading_sha.as_deref() == Some(&sha);
                    let click_sha = sha.clone();
                    let click_tx = event_tx.clone();
                    let click_project = project.clone();

                    div()
                        .id(SharedString::from(format!("commit-{sha}")))
                        .flex()
                        .flex_col()
                        .mx_2()
                        .my_0p5()
                        .rounded_md()
                        .border_1()
                        .border_color(if is_expanded {
                            theme.primary.opacity(0.6)
                        } else {
                            theme.border.opacity(0.3)
                        })
                        .bg(if is_expanded {
                            theme.muted.opacity(0.4)
                        } else {
                            theme.title_bar.opacity(0.5)
                        })
                        .child(
                            div()
                                .id(SharedString::from(format!("commit-header-{sha}")))
                                .p_2p5()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .cursor_pointer()
                                .hover(|row| row.bg(theme.muted.opacity(0.6)))
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    if this.selected_commit_sha.as_deref() == Some(&click_sha) {
                                        this.selected_commit_sha = None;
                                        this.loading_commit_sha = None;
                                        this.selected_commit_files.clear();
                                    } else {
                                        this.selected_commit_sha = Some(click_sha.clone());
                                        this.loading_commit_sha = Some(click_sha.clone());
                                        this.selected_commit_files.clear();
                                        if let Some(proj) = click_project.clone() {
                                            let tx = click_tx.clone();
                                            let fetch_sha = click_sha.clone();
                                            std::thread::spawn(move || {
                                                let files = threadlane_git::inspect_commit_files(&proj, &fetch_sha);
                                                let _ = tx.send(PanelEvent::CommitFilesLoaded {
                                                    sha: fetch_sha,
                                                    files,
                                                });
                                            });
                                        }
                                    }
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .items_start()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_xs()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme.foreground)
                                                .child(summary),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .flex_shrink_0()
                                                .px_1p5()
                                                .py_0p5()
                                                .rounded_sm()
                                                .bg(theme.muted)
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child(short_sha.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(
                                            div()
                                                .size(px(12.0))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .child(IconName::User),
                                        )
                                        .child(format!("{author} • {rel_time}")),
                                ),
                        )
                        .children(is_expanded.then(|| {
                            let commit_files = selected_files.clone();
                            let commit_sha = sha.clone();
                            let short_sha_disp = short_sha.clone();
                            let proj_for_diff = project.clone();
                            let model_ref = model.clone();

                            div()
                                .border_t_1()
                                .border_color(theme.border)
                                .bg(theme.background)
                                .p_2()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .children(is_loading.then(|| {
                                    div()
                                        .p_2()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(Spinner::new().xsmall())
                                        .child("Loading changed files…")
                                }))
                                .children((!is_loading && commit_files.is_empty()).then(|| {
                                    div()
                                        .p_2()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("No files changed in this commit.")
                                }))
                                .children(commit_files.into_iter().map(|file| {
                                    let path = file.path.clone();
                                    let status = file.status_char().to_string();
                                    let status_color = match file.status_char() {
                                        'A' | '?' => theme.success,
                                        'D' => theme.danger,
                                        _ => theme.warning,
                                    };
                                    let target_path = path.clone();
                                    let diff_sha = commit_sha.clone();
                                    let disp_sha = short_sha_disp.clone();
                                    let diff_proj = proj_for_diff.clone();
                                    let m = model_ref.clone();

                                    div()
                                        .id(SharedString::from(format!("commit-file-{commit_sha}-{path}")))
                                        .h(px(26.0))
                                        .px_2()
                                        .rounded_md()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .cursor_pointer()
                                        .hover(|row| row.bg(theme.muted))
                                        .on_click(cx.listener(move |_this, _event, _window, cx| {
                                            let Some(proj) = diff_proj.clone() else {
                                                return;
                                            };
                                            let p = proj.clone();
                                            let target = target_path.clone();
                                            let sha_str = diff_sha.clone();
                                            let label = format!("{target} @ {disp_sha}");
                                            let state_model = m.clone();
                                            cx.spawn(async move |_this, cx| {
                                                let content = cx
                                                    .background_executor()
                                                    .spawn(async move {
                                                        threadlane_git::diff_commit_file(&p, &sha_str, &target)
                                                            .unwrap_or_else(|e| e.to_string())
                                                    })
                                                    .await;
                                                let _ = state_model.update(cx, |state, cx| {
                                                    state.request_open_diff(proj, label, content);
                                                    cx.notify();
                                                });
                                            })
                                            .detach();
                                        }))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .flex()
                                                .items_center()
                                                .gap_1p5()
                                                .child(
                                                    div()
                                                        .size(px(12.0))
                                                        .text_color(theme.muted_foreground)
                                                        .child(IconName::File),
                                                )
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .text_xs()
                                                        .text_color(theme.foreground)
                                                        .child(path),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_1p5()
                                                .when(file.additions > 0, |r| {
                                                    r.child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme.success)
                                                            .child(format!("+{}", file.additions)),
                                                    )
                                                })
                                                .when(file.deletions > 0, |r| {
                                                    r.child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme.danger)
                                                            .child(format!("-{}", file.deletions)),
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .size(px(14.0))
                                                        .rounded_sm()
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .text_xs()
                                                        .font_weight(FontWeight::BOLD)
                                                        .text_color(status_color)
                                                        .child(status),
                                                ),
                                        )
                                }))
                        }))
                }))
                .into_any_element()
        };

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar.opacity(0.3))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .child(
                                div()
                                    .size(px(14.0))
                                    .text_color(theme.muted_foreground)
                                    .child(IconName::Search),
                            )
                            .child(
                                div().flex_1().child(
                                    Input::new(&self.history_filter_input)
                                        .appearance(false)
                                        .bordered(false),
                                ),
                            ),
                    ),
            )
            .child(commit_list)
    }

    fn render_branch_manager(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let filter_text = self.branch_filter_input.read(cx).value().trim().to_lowercase();
        let current_branch = self.git_status.as_ref().and_then(|s| s.branch.as_deref()).unwrap_or("main");

        let branch_details = self.git_status.as_ref().map(|s| &s.branch_details);
        let default_branch_name = self.git_status.as_ref().and_then(|s| s.default_branch.as_deref()).unwrap_or("main");

        let all_branches: Vec<GitBranchInfo> = if let Some(details) = branch_details {
            details.clone()
        } else if let Some(status) = &self.git_status {
            status.branches.iter().filter(|b| b.as_str() != "origin" && !b.ends_with("/HEAD")).map(|b| GitBranchInfo {
                name: b.clone(),
                is_current: b == current_branch,
                is_default: b == default_branch_name,
                is_remote: b.starts_with("origin/"),
                relative_time: String::new(),
                committer_date_unix: 0,
                upstream: None,
            }).collect()
        } else {
            Vec::new()
        };

        let filtered_branches: Vec<GitBranchInfo> = all_branches
            .into_iter()
            .filter(|b| b.name != "origin" && !b.name.ends_with("/HEAD") && (filter_text.is_empty() || b.name.to_lowercase().contains(&filter_text)))
            .collect();

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

        div()
            .id("git-branch-manager")
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
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .px_2()
                            .h(px(32.0))
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .child(
                                div()
                                    .size(px(14.0))
                                    .text_color(theme.muted_foreground)
                                    .child(IconName::Search),
                            )
                            .child(
                                div().flex_1().child(
                                    Input::new(&self.branch_filter_input)
                                        .appearance(false)
                                        .bordered(false),
                                ),
                            ),
                    )
                    .child(
                        Button::new("open-new-branch-modal-btn")
                            .icon(IconName::Plus)
                            .label("New Branch")
                            .outline()
                            .small()
                            .tooltip("Create a new branch")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.new_branch_dialog_open = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("close-branch-manager-btn")
                            .icon(IconName::Close)
                            .ghost()
                            .small()
                            .tooltip("Back to review")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.branch_popover_open = false;
                                cx.notify();
                            })),
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
                        div()
                            .id("quick-merge-banner")
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_2p5()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.muted.opacity(0.35))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.muted.opacity(0.7)))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.merge_dialog_open = true;
                                this.merge_selected_branch = None;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .size(px(16.0))
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
                                    .size(px(14.0))
                                    .text_color(theme.muted_foreground)
                                    .child(IconName::ChevronRight),
                            ),
                    )
                    .when(!default_branches.is_empty(), |el| {
                        el.child(self.render_branch_section("DEFAULT BRANCH", default_branches, cx))
                    })
                    .when(!recent_branches.is_empty(), |el| {
                        el.child(self.render_branch_section("RECENT BRANCHES", recent_branches, cx))
                    })
                    .when(!other_branches.is_empty(), |el| {
                        el.child(self.render_branch_section("OTHER BRANCHES", other_branches, cx))
                    }),
            )
    }

    fn render_branch_section(
        &self,
        title: &'static str,
        branches: Vec<GitBranchInfo>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
                div()
                    .id(SharedString::from(format!("branch-row-{}", name)))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2p5()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(if is_current {
                        theme.muted.opacity(0.7)
                    } else {
                        gpui::transparent_black()
                    })
                    .hover(|s| s.bg(theme.muted))
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        if !is_current {
                            let has_dirty = this.git_status.as_ref().map_or(false, |s| !s.files.is_empty());
                            if has_dirty {
                                this.switch_target_branch = Some(branch_name_for_click.clone());
                                this.switch_dialog_open = true;
                                this.switch_stash_mode = true;
                                cx.notify();
                            } else {
                                this.run_git_action(GitAction::Checkout(branch_name_for_click.clone()), window, cx);
                                this.branch_popover_open = false;
                            }
                        }
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .size(px(16.0))
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
                            )
                            .children(is_current.then(|| {
                                Tag::new()
                                    .child("current")
                                    .with_variant(TagVariant::Info)
                                    .small()
                            })),
                    )
                    .children((!rel_time.is_empty()).then(|| {
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(rel_time)
                    }))
            }))
    }

    pub(crate) fn close_all_git_dialogs(&mut self) {
        self.new_branch_dialog_open = false;
        self.merge_dialog_open = false;
        self.merge_selected_branch = None;
        self.switch_dialog_open = false;
        self.switch_target_branch = None;
    }

    pub(crate) fn render_git_dialog_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.new_branch_dialog_open {
            Some(self.render_new_branch_dialog(cx).into_any_element())
        } else if self.merge_dialog_open {
            Some(self.render_merge_dialog(cx).into_any_element())
        } else if self.switch_dialog_open {
            Some(self.render_switch_branch_dialog(cx).into_any_element())
        } else {
            None
        }
    }

    fn render_new_branch_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let current_branch = self.git_status.as_ref().and_then(|s| s.branch.as_deref()).unwrap_or("main").to_string();
        let name = self.new_branch_name_input.read(cx).value().trim().to_string();
        let can_create = !name.is_empty() && !self.git_busy;

        div()
            .id("new-branch-modal-backdrop")
            .absolute()
            .inset_0()
            .bg(hsla(0.0, 0.0, 0.0, 0.6))
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _event, _window, cx| {
                this.close_all_git_dialogs();
                cx.notify();
            }))
            .child(
                div()
                    .id("new-branch-dialog")
                    .w(px(420.0))
                    .p_5()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .shadow_xl()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                        cx.stop_propagation()
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.foreground)
                                    .child("Create a branch"),
                            )
                            .child(
                                Button::new("close-new-branch-dialog-btn")
                                    .icon(IconName::Close)
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.close_all_git_dialogs();
                                        cx.notify();
                                    })),
                            ),
                    )
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
                                    .child(current_branch)
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
                                        Input::new(&self.new_branch_name_input)
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
                                    .label("Cancel")
                                    .ghost()
                                    .small()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.close_all_git_dialogs();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("submit-new-branch-btn")
                                    .label("Create Branch")
                                    .primary()
                                    .small()
                                    .disabled(!can_create)
                                    .on_click(cx.listener(move |this, _event, window, cx| {
                                        let name = this.new_branch_name_input.read(cx).value().trim().to_string();
                                        if !name.is_empty() {
                                            this.run_git_action(GitAction::CreateBranch(name), window, cx);
                                            this.close_all_git_dialogs();
                                        }
                                    })),
                            ),
                    ),
            )
    }

    fn render_merge_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let current_branch = self.git_status.as_ref().and_then(|s| s.branch.as_deref()).unwrap_or("main").to_string();
        let filter = self.merge_filter_input.read(cx).value().trim().to_lowercase();

        let branch_details = self.git_status.as_ref().map(|s| &s.branch_details);
        let branches: Vec<GitBranchInfo> = if let Some(details) = branch_details {
            details.iter().filter(|b| b.name != "origin" && !b.name.ends_with("/HEAD") && b.name != current_branch && (filter.is_empty() || b.name.to_lowercase().contains(&filter))).cloned().collect()
        } else if let Some(status) = &self.git_status {
            status.branches.iter().filter(|b| b.as_str() != "origin" && !b.ends_with("/HEAD") && b.as_str() != current_branch.as_str() && (filter.is_empty() || b.to_lowercase().contains(&filter))).map(|b| GitBranchInfo {
                name: b.clone(),
                is_current: false,
                is_default: false,
                is_remote: b.starts_with("origin/"),
                relative_time: String::new(),
                committer_date_unix: 0,
                upstream: None,
            }).collect()
        } else {
            Vec::new()
        };

        let selected = self.merge_selected_branch.clone();
        let can_merge = selected.is_some() && !self.git_busy;

        div()
            .id("merge-branch-modal-backdrop")
            .absolute()
            .inset_0()
            .bg(hsla(0.0, 0.0, 0.0, 0.6))
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _event, _window, cx| {
                this.close_all_git_dialogs();
                cx.notify();
            }))
            .child(
                div()
                    .id("merge-branch-dialog")
                    .w(px(460.0))
                    .max_h(px(520.0))
                    .p_5()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .shadow_xl()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                        cx.stop_propagation()
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.foreground)
                                    .child(format!("Merge into {current_branch}")),
                            )
                            .child(
                                Button::new("close-merge-dialog-btn")
                                    .icon(IconName::Close)
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.close_all_git_dialogs();
                                        cx.notify();
                                    })),
                            ),
                    )
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
                            .h(px(32.0))
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .child(
                                div()
                                    .size(px(14.0))
                                    .text_color(theme.muted_foreground)
                                    .child(IconName::Search),
                            )
                            .child(
                                div().flex_1().child(
                                    Input::new(&self.merge_filter_input)
                                        .appearance(false)
                                        .bordered(false),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .max_h(px(240.0))
                            .overflow_y_scrollbar()
                            .gap_1()
                            .children(branches.into_iter().map(|b| {
                                let name = b.name.clone();
                                let is_selected = selected.as_deref() == Some(&name);
                                let name_for_click = name.clone();
                                div()
                                    .id(SharedString::from(format!("merge-select-{}", name)))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .px_2p5()
                                    .py_2()
                                    .rounded_md()
                                    .cursor_pointer()
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
                                    .hover(|s| s.bg(theme.muted))
                                    .on_click(cx.listener(move |this, _event, _window, cx| {
                                        this.merge_selected_branch = Some(name_for_click.clone());
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .size(px(16.0))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .text_color(if is_selected {
                                                        theme.primary
                                                    } else {
                                                        theme.muted_foreground
                                                    })
                                                    .child(Icon::default().path("icons/git/branch.svg")),
                                            )
                                            .child(
                                                div()
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
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(b.relative_time)
                                    }))
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
                                    .label("Cancel")
                                    .ghost()
                                    .small()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.close_all_git_dialogs();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("submit-merge-btn")
                                    .label(if let Some(target) = &selected {
                                        format!("Merge {target} into {current_branch}")
                                    } else {
                                        format!("Merge into {current_branch}")
                                    })
                                    .primary()
                                    .small()
                                    .disabled(!can_merge)
                                    .on_click(cx.listener(move |this, _event, window, cx| {
                                        if let Some(branch_to_merge) = this.merge_selected_branch.clone() {
                                            this.run_git_action(GitAction::Merge(branch_to_merge), window, cx);
                                            this.close_all_git_dialogs();
                                        }
                                    })),
                            ),
                    ),
            )
    }

    fn render_switch_branch_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let current_branch = self.git_status.as_ref().and_then(|s| s.branch.as_deref()).unwrap_or("main").to_string();
        let target_branch = self.switch_target_branch.clone().unwrap_or_else(|| "main".to_string());
        let is_stash = self.switch_stash_mode;

        div()
            .id("switch-branch-modal-backdrop")
            .absolute()
            .inset_0()
            .bg(hsla(0.0, 0.0, 0.0, 0.6))
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _event, _window, cx| {
                this.close_all_git_dialogs();
                cx.notify();
            }))
            .child(
                div()
                    .id("switch-branch-dialog")
                    .w(px(460.0))
                    .p_5()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .shadow_xl()
                    .flex()
                    .flex_col()
                    .gap_3p5()
                    .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                        cx.stop_propagation()
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.foreground)
                                    .child("Switch Branch"),
                            )
                            .child(
                                Button::new("close-switch-dialog-btn")
                                    .icon(IconName::Close)
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.close_all_git_dialogs();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("You have uncommitted changes on {current_branch}. What would you like to do with them?")),
                    )
                    .child(
                        div()
                            .id("switch-opt-stash")
                            .flex()
                            .items_start()
                            .gap_2p5()
                            .p_3()
                            .rounded_lg()
                            .border_1()
                            .border_color(if is_stash { theme.primary } else { theme.border })
                            .bg(if is_stash { theme.muted.opacity(0.8) } else { theme.background })
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.muted))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.switch_stash_mode = true;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .size(px(16.0))
                                    .mt(px(2.0))
                                    .rounded_full()
                                    .border_2()
                                    .border_color(if is_stash { theme.primary } else { theme.muted_foreground })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .children(is_stash.then(|| {
                                        div()
                                            .size(px(8.0))
                                            .rounded_full()
                                            .bg(theme.primary)
                                    })),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .min_w_0()
                                    .flex_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.foreground)
                                            .child(format!("Leave my changes on {current_branch} (Stash)")),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("Your in-progress changes will be stashed and restored when you switch back."),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("switch-opt-carry")
                            .flex()
                            .items_start()
                            .gap_2p5()
                            .p_3()
                            .rounded_lg()
                            .border_1()
                            .border_color(if !is_stash { theme.primary } else { theme.border })
                            .bg(if !is_stash { theme.muted.opacity(0.8) } else { theme.background })
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.muted))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.switch_stash_mode = false;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .size(px(16.0))
                                    .mt(px(2.0))
                                    .rounded_full()
                                    .border_2()
                                    .border_color(if !is_stash { theme.primary } else { theme.muted_foreground })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .children((!is_stash).then(|| {
                                        div()
                                            .size(px(8.0))
                                            .rounded_full()
                                            .bg(theme.primary)
                                    })),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .min_w_0()
                                    .flex_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.foreground)
                                            .child(format!("Bring my changes to {target_branch}")),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("Your in-progress changes will be carried over to {target_branch}.")),
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
                                Button::new("cancel-switch-dialog-btn")
                                    .label("Cancel")
                                    .ghost()
                                    .small()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.close_all_git_dialogs();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("submit-switch-dialog-btn")
                                    .label("Switch Branch")
                                    .primary()
                                    .small()
                                    .disabled(self.git_busy)
                                    .on_click(cx.listener(move |this, _event, window, cx| {
                                        let target = this.switch_target_branch.clone().unwrap_or_else(|| "main".to_string());
                                        if this.switch_stash_mode {
                                            this.run_git_action(GitAction::CheckoutStash(target), window, cx);
                                        } else {
                                            this.run_git_action(GitAction::CheckoutCarry(target), window, cx);
                                        }
                                        this.close_all_git_dialogs();
                                        this.branch_popover_open = false;
                                    })),
                            ),
                    ),
            )
    }

    fn render_empty(&self, title: &str, description: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
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
            .into_any_element()
    }
}

impl Render for RightPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(message) = self.generated_commit_message.take() {
            self.commit_message_input
                .update(cx, |input, cx| input.set_value(message, window, cx));
        }
        if self.should_clear_commit_message {
            self.should_clear_commit_message = false;
            self.commit_message_input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.sync_project(cx);
        self.sync_pending_document(window, cx);
        let theme = cx.theme().colors;
        let body = match self.active_surface {
            None => self.render_chooser(cx).into_any_element(),
            Some(Surface::Review) => self.render_review(cx),
            Some(Surface::Files) => self.render_files(cx),
        };
        div()
            .w_full()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(self.render_header(cx))
            .child(body)
    }
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

fn scan_project_tree(root: &Path, limit: usize) -> Vec<FileNode> {
    fn visit(
        root: &Path,
        relative: &Path,
        depth: usize,
        limit: usize,
        count: &mut usize,
    ) -> Vec<FileNode> {
        if *count >= limit || depth > 6 {
            return Vec::new();
        }
        let Ok(read_dir) = std::fs::read_dir(root.join(relative)) else {
            return Vec::new();
        };
        let mut children = read_dir
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == ".git" || name == "target" || name == ".threadlane" {
                    return None;
                }
                Some((name, entry.file_type().ok()?.is_dir()))
            })
            .collect::<Vec<_>>();
        children.sort_by_key(|(name, is_dir)| (!*is_dir, name.to_ascii_lowercase()));

        let mut nodes = Vec::new();
        for (name, is_dir) in children {
            if *count >= limit {
                break;
            }
            *count += 1;
            let path = relative.join(&name);
            let rel_str = path.to_string_lossy().into_owned();
            if is_dir {
                let sub_children = visit(root, &path, depth + 1, limit, count);
                nodes.push(FileNode {
                    relative_path: rel_str,
                    name,
                    is_dir: true,
                    children: sub_children,
                });
            } else {
                nodes.push(FileNode {
                    relative_path: rel_str,
                    name,
                    is_dir: false,
                    children: Vec::new(),
                });
            }
        }
        nodes
    }

    let mut count = 0;
    visit(root, Path::new(""), 0, limit, &mut count)
}

#[cfg(test)]
mod tests {
    use super::{can_publish_branch, scan_project_tree};
    use std::time::{SystemTime, UNIX_EPOCH};
    use threadlane_git::GitStatus;

    #[test]
    fn only_publishable_branches_without_upstreams_use_the_publish_action() {
        let unpublished = GitStatus {
            branch: Some("feature/demo".into()),
            remote: Some("git@github.com:threadlane/threadlane.git".into()),
            ahead: 731,
            ..GitStatus::default()
        };
        assert!(can_publish_branch(Some(&unpublished)));

        let published = GitStatus {
            has_upstream: true,
            ..unpublished.clone()
        };
        assert!(!can_publish_branch(Some(&published)));

        let detached = GitStatus {
            detached: true,
            branch: None,
            ..unpublished
        };
        assert!(!can_publish_branch(Some(&detached)));
        assert!(!can_publish_branch(None));
    }

    #[test]
    fn project_scan_is_bounded_and_skips_generated_roots() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("threadlane-panel-{nonce}"));
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join(".threadlane/sessions")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("src/nested/lib.rs"), "pub fn value() {}\n").unwrap();
        std::fs::write(root.join("target/debug/generated"), "ignored").unwrap();

        let items = scan_project_tree(&root, 10);
        assert_eq!(
            items
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["src"]
        );
        assert!(items[0]
            .children
            .iter()
            .any(|item| item.relative_path == "src/main.rs"));
        assert!(items[0]
            .children
            .iter()
            .any(|item| item.relative_path == "src/nested"));

        std::fs::remove_dir_all(root).unwrap();
    }
}
