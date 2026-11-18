//! Saved checkout host. Selection and drafts are local; Git actions never execute.
use gpui::{prelude::*, *};
use gpui_component::button::ButtonVariants;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::menu::ContextMenuExt;
use gpui_component::notification::Notification;
use gpui_component::separator::Separator;
use gpui_component::{ActiveTheme, Sizable, WindowExt};
use std::collections::{BTreeMap, HashMap, HashSet};
use threadlane_protocol::daemon::SessionInfo;
use threadlane_protocol::repo::{GitFile, GitStatus};
use threadlane_ui_kit as kit;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct CapturedReviewDiff {
    pub text: String,
    pub ignore_whitespace: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct CapturedReviewRecord {
    pub files: Vec<GitFile>,
    pub diffs: HashMap<String, String>,
}

#[cfg(not(target_arch = "wasm32"))]
pub fn capture_records(
    project: &std::path::Path,
    status: &GitStatus,
) -> (
    HashMap<String, CapturedReviewRecord>,
    HashMap<usize, CapturedReviewRecord>,
) {
    // Share a bounded private snapshot budget across history and stash details.
    let mut remaining = 5 * 1024 * 1024;
    let mut capture = |files: Vec<GitFile>, read: &dyn Fn(&str) -> Option<String>| {
        let metadata_size = serde_json::to_vec(&files).ok()?.len();
        if metadata_size > remaining {
            return None;
        }
        remaining -= metadata_size;
        let mut diffs = HashMap::new();
        for file in &files {
            if let Some(diff) = read(&file.path) {
                if diff.len() <= remaining {
                    remaining -= diff.len();
                    diffs.insert(file.path.clone(), diff);
                }
            }
        }
        Some(CapturedReviewRecord { files, diffs })
    };
    let mut stashes = HashMap::new();
    if let Some(stash) = &status.current_stash {
        if let Some(record) = capture(
            threadlane_git::inspect_stash_files(project, stash.index),
            &|path| threadlane_git::diff_stash_file(project, stash.index, path).ok(),
        ) {
            stashes.insert(stash.index, record);
        }
    }
    let mut commits = HashMap::new();
    for commit in &status.recent_commits {
        if let Some(record) = capture(
            threadlane_git::inspect_commit_files(project, &commit.sha),
            &|path| threadlane_git::diff_commit_file(project, &commit.sha, path).ok(),
        ) {
            commits.insert(commit.sha.clone(), record);
        }
    }
    (commits, stashes)
}

pub enum ReviewPreviewEvent {
    OpenDiff { title: String, content: String },
}

/// Gallery fixtures use the real shared command and a local undoable draft.
pub struct ReviewExcerptPreview {
    document: Entity<kit::ReviewDiffDocument>,
    draft: Entity<gpui_component::input::TextareaState>,
    fixture: usize,
    _subscription: Subscription,
}

impl ReviewExcerptPreview {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let document = cx.new(|cx| kit::ReviewDiffDocument::new(window, cx));
        let draft = cx.new(|cx| {
            gpui_component::input::TextareaState::new(window, cx)
                .placeholder("Local chat draft · Undo removes the excerpt")
                .auto_grow(2, 8)
        });
        let subscription = cx.subscribe_in(
            &document,
            window,
            |host, _, request: &kit::ReviewExcerptRequest, window, cx| {
                if !host
                    .document
                    .read(cx)
                    .excerpt_request_is_current(request, cx)
                {
                    window.push_notification(kit::STALE_REVIEW_EXCERPT, cx);
                    return;
                }
                if let Ok(excerpt) = kit::format_review_excerpt(request.target(), request.text()) {
                    host.draft.update(cx, |input, cx| {
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
                }
            },
        );
        let mut preview = Self {
            document,
            draft,
            fixture: 0,
            _subscription: subscription,
        };
        preview.set_fixture(0, cx);
        preview
    }

    fn set_fixture(&mut self, fixture: usize, cx: &mut Context<Self>) {
        self.fixture = fixture;
        self.document.update(cx, |document, cx| {
            document.loading(fixture == 1, cx);
            document.set_excerpt_target(match fixture {
                4 => Err(kit::STALE_REVIEW_EXCERPT.into()),
                5 => Err("No chat draft is available".into()),
                _ => Ok(kit::ReviewExcerptTarget::new("/preview".into(), "src/example.rs".into(), (Some("/preview".into()), None), fixture as u64, fixture == 1)),
            }, cx);
            if fixture != 3 {
                document.set_patch(if fixture == 2 { "" } else { "diff --git a/src/example.rs b/src/example.rs\n@@ -1 +1 @@\n-  old café\n+  new 🦀 ```literal```\n context" }, cx);
            }
        });
        cx.notify();
    }
}

impl Render for ReviewExcerptPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .min_w_0()
            .child(
                div().flex().flex_wrap().gap_1().children(
                    [
                        "Ready",
                        "Filtered",
                        "Empty",
                        "Loading",
                        "Stale",
                        "No destination",
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, label)| {
                        gpui_component::button::Button::new(("review-excerpt-fixture", index))
                            .label(label)
                            .small()
                            .ghost()
                            .on_click(
                                cx.listener(move |host, _, _, cx| host.set_fixture(index, cx)),
                            )
                    }),
                ),
            )
            .child(div().h(rems(18.)).flex().child(self.document.clone()))
            .child(
                gpui_component::input::Textarea::new(&self.draft)
                    .aria_label("Review excerpt sample chat draft"),
            )
    }
}

impl EventEmitter<ReviewPreviewEvent> for ReviewPreview {}
impl EventEmitter<kit::ReviewExcerptRequest> for ReviewPreview {}

#[cfg(not(target_arch = "wasm32"))]
pub fn capture_diffs(
    project: &std::path::Path,
    files: &[GitFile],
) -> (
    HashMap<String, CapturedReviewDiff>,
    Option<CapturedReviewDiff>,
) {
    // Keep private, ignored preview snapshots bounded without truncating patches.
    const LIMIT: usize = 5 * 1024 * 1024;
    let capture = |path: Option<&str>| {
        let read = |ignore_whitespace| {
            let options = threadlane_protocol::repo::DiffOptions { ignore_whitespace };
            match path {
                Some(path) => threadlane_git::diff_file_with_options(project, path, options),
                None => threadlane_git::worktree_diff_with_options(project, options),
            }
            .ok()
        };
        let text = read(false)?;
        let ignore_whitespace = read(true)?;
        (text.len() + ignore_whitespace.len() <= LIMIT).then_some(CapturedReviewDiff {
            text,
            ignore_whitespace,
        })
    };
    let mut remaining = LIMIT;
    let mut diffs = HashMap::new();
    for file in files {
        if let Some(diff) = capture(Some(&file.path)) {
            let size = diff.text.len() + diff.ignore_whitespace.len();
            if size <= remaining {
                remaining -= size;
                diffs.insert(file.path.clone(), diff);
            }
        }
    }
    (diffs, capture(None))
}

pub struct ReviewPreview {
    status: Option<GitStatus>,
    session: Option<SessionInfo>,
    diffs: HashMap<String, CapturedReviewDiff>,
    combined: Option<CapturedReviewDiff>,
    selected: HashSet<String>,
    filter: Entity<InputState>,
    commit: Entity<InputState>,
    list: ListState,
    mode: kit::ReviewViewMode,
    tab: kit::ReviewTab,
    collapsed: HashSet<String>,
    document: Option<String>,
    /// Bumped on each explicit document open so a navigated file's scroll
    /// area starts at the top rather than inheriting a prior offset.
    document_revision: u64,
    /// Non-tab-stop focus target for the navigation group when the
    /// initiating control becomes disabled at a boundary.
    review_nav_focus: FocusHandle,
    review_document: Entity<kit::ReviewDiffDocument>,
    ignore_whitespace: bool,
    can_create_pr: bool,
    draft_pr: Entity<crate::draft_pr::DraftPrPreview>,
    history_filter: Entity<InputState>,
    selected_commit: Option<String>,
    commits: HashMap<String, CapturedReviewRecord>,
    stashes: HashMap<usize, CapturedReviewRecord>,
    stash_expanded: bool,
    pr_expanded: bool,
    feedback_count: Option<usize>,
    file_manager_label: String,
    branch_open: bool,
    branch_filter: Entity<InputState>,
    branch_name: Entity<InputState>,
    merge_filter: Entity<InputState>,
    stash_message: Entity<InputState>,
    git_dialog: Option<kit::ReviewGitDialog>,
    merge_selected: Option<String>,
    switch_target: Option<String>,
    switch_stash: bool,
    include_untracked: bool,
    _git_inputs: Vec<Subscription>,
    _filter: Subscription,
    _commit: Subscription,
    _history_filter: Subscription,
    _review_document: Subscription,
    _review_excerpt: Subscription,
}

impl ReviewPreview {
    pub(super) fn leave_review(&mut self, cx: &mut Context<Self>) {
        self.document = None;
        self.review_document.update(cx, |document, cx| document.reset(cx));
        cx.notify();
    }

    pub fn new(
        snapshot: Option<&super::session::Snapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let review_document = cx.new(|cx| kit::ReviewDiffDocument::new(window, cx));
        let review_document_subscription =
            cx.subscribe_in(&review_document, window, |host, _, action, window, cx| {
                match *action {
                    kit::ReviewDiffAction::ShowWhitespace => {
                        host.ignore_whitespace = false;
                        host.update_diff(cx);
                    }
                    kit::ReviewDiffAction::Retry => Self::notice(window, cx),
                }
            });
        let status = snapshot.and_then(|snapshot| snapshot.git_status.clone());
        let excerpt_subscription = cx.subscribe_in(
            &review_document,
            window,
            |host, _, request: &kit::ReviewExcerptRequest, window, cx| {
                if host.excerpt_target().as_ref().ok() == Some(request.target())
                    && &host.review_document == request.document()
                    && host
                        .review_document
                        .read(cx)
                        .excerpt_request_is_current(request, cx)
                {
                    cx.emit(request.clone());
                } else {
                    window.push_notification(kit::STALE_REVIEW_EXCERPT, cx);
                }
            },
        );
        let selected = status
            .as_ref()
            .map(|status| status.files.iter().map(|file| file.path.clone()).collect())
            .unwrap_or_default();
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter changes…"));
        let filter_subscription = cx.subscribe(&filter, |_, _, _: &InputEvent, cx| cx.notify());
        let commit = cx.new(|cx| InputState::new(window, cx).placeholder("Summary (required)"));
        let commit_subscription = cx.subscribe(&commit, |_, _, _: &InputEvent, cx| cx.notify());
        let history_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter commits…"));
        let history_subscription =
            cx.subscribe(&history_filter, |_, _, _: &InputEvent, cx| cx.notify());
        let branch_filter = cx.new(|cx| {
            InputState::new(window, cx).placeholder(kit::REVIEW_BRANCH_FILTER_PLACEHOLDER)
        });
        let branch_name = cx
            .new(|cx| InputState::new(window, cx).placeholder(kit::REVIEW_BRANCH_NAME_PLACEHOLDER));
        let merge_filter = cx.new(|cx| {
            InputState::new(window, cx).placeholder(kit::REVIEW_MERGE_FILTER_PLACEHOLDER)
        });
        let stash_message = cx.new(|cx| {
            InputState::new(window, cx).placeholder(kit::REVIEW_STASH_MESSAGE_PLACEHOLDER)
        });
        let git_inputs = [&branch_filter, &branch_name, &merge_filter, &stash_message]
            .into_iter()
            .map(|input| cx.subscribe(input, |_, _, _: &InputEvent, cx| cx.notify()))
            .collect();
        let draft_pr = cx.new(|cx| crate::draft_pr::DraftPrPreview::new(status.as_ref(), window, cx));
        Self {
            list: ListState::new(
                status.as_ref().map_or(0, |status| status.files.len()),
                ListAlignment::Top,
                window.rem_size() * 2.0,
            ),
            status,
            session: snapshot.and_then(|snapshot| snapshot.session.clone()),
            diffs: snapshot
                .map(|snapshot| snapshot.review_diffs.clone())
                .unwrap_or_default(),
            combined: snapshot.and_then(|snapshot| snapshot.review_combined_diff.clone()),
            selected,
            filter,
            commit,
            mode: kit::ReviewViewMode::List,
            tab: kit::ReviewTab::Changes,
            collapsed: HashSet::new(),
            document: None,
            document_revision: 0,
            review_nav_focus: cx.focus_handle().tab_stop(false),
            review_document,
            ignore_whitespace: false,
            draft_pr,
            can_create_pr: snapshot.is_some_and(|snapshot| snapshot.review_can_create_pr),
            history_filter,
            selected_commit: None,
            commits: snapshot
                .map(|snapshot| snapshot.review_commits.clone())
                .unwrap_or_default(),
            stashes: snapshot
                .map(|snapshot| snapshot.review_stashes.clone())
                .unwrap_or_default(),
            stash_expanded: false,
            pr_expanded: false,
            feedback_count: snapshot.and_then(|snapshot| snapshot.review_feedback_count),
            file_manager_label: snapshot
                .and_then(|snapshot| snapshot.review_file_manager_label.clone())
                .unwrap_or_else(|| kit::review_file_manager_label().into()),
            branch_open: false,
            branch_filter,
            branch_name,
            merge_filter,
            stash_message,
            git_dialog: None,
            merge_selected: None,
            switch_target: None,
            switch_stash: true,
            include_untracked: true,
            _git_inputs: git_inputs,
            _filter: filter_subscription,
            _commit: commit_subscription,
            _history_filter: history_subscription,
            _review_document: review_document_subscription,
            _review_excerpt: excerpt_subscription,
        }
    }

    fn branch_action(
        &mut self,
        action: &kit::ReviewBranchAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            kit::ReviewBranchAction::Close => self.branch_open = false,
            kit::ReviewBranchAction::New => {
                self.open_git_dialog(kit::ReviewGitDialog::NewBranch, window, cx)
            }
            kit::ReviewBranchAction::Merge => {
                self.open_git_dialog(kit::ReviewGitDialog::Merge, window, cx)
            }
            kit::ReviewBranchAction::Select(name) => {
                if self.status.as_ref().is_some_and(|status| {
                    status.branch.as_ref() != Some(name) && !status.files.is_empty()
                }) {
                    self.switch_target = Some(name.clone());
                    self.open_git_dialog(kit::ReviewGitDialog::Switch, window, cx);
                } else {
                    Self::notice(window, cx);
                }
            }
            kit::ReviewBranchAction::Copy(name) => {
                cx.write_to_clipboard(ClipboardItem::new_string(name.clone()))
            }
            kit::ReviewBranchAction::Delete(name) => {
                let eligible =
                    kit::review_branches(self.status.as_ref(), "")
                        .iter()
                        .any(|branch| {
                            branch.name == *name
                                && !branch.is_current
                                && !branch.is_default
                                && !branch.is_remote
                        });
                if eligible {
                    let branch = name.clone();
                    let workdir = self
                        .session
                        .as_ref()
                        .map(|session| session.runtime_work_dir.display().to_string())
                        .unwrap_or_default();
                    window.open_alert_dialog(cx, move |alert, _, _| {
                        kit::review_delete_branch_alert(alert, &branch, &workdir).on_ok(
                            |_, window, cx| {
                                Self::notice(window, cx);
                                true
                            },
                        )
                    });
                }
            }
        }
        cx.notify();
    }

    fn open_git_dialog(
        &mut self,
        kind: kit::ReviewGitDialog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.git_dialog = Some(kind);
        self.branch_open = false;
        if kind == kit::ReviewGitDialog::Merge {
            self.merge_selected = None;
        }
        if kind == kit::ReviewGitDialog::Switch {
            self.switch_stash = true;
        }
        let owner = cx.entity().downgrade();
        let target = self.switch_target.clone();
        window.open_dialog(cx, move |dialog, window, cx| {
            let close_owner = owner.clone();
            let content = owner
                .update(cx, |host, cx| host.git_form(cx))
                .ok()
                .flatten();
            kit::review_git_dialog(dialog, kind, target.as_deref(), window)
                .children(content)
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    let _ = close_owner.update(cx, |host, cx| {
                        host.git_dialog = None;
                        host.switch_target = None;
                        host.merge_selected = None;
                        cx.notify();
                    });
                })
        });
        match kind {
            kit::ReviewGitDialog::NewBranch => self.branch_name.update(cx, |input, cx| {
                input.set_selected_range(0..input.value().len(), cx);
                input.focus(window, cx);
            }),
            kit::ReviewGitDialog::Merge => self
                .merge_filter
                .update(cx, |input, cx| input.focus(window, cx)),
            kit::ReviewGitDialog::Stash => self
                .stash_message
                .update(cx, |input, cx| input.focus(window, cx)),
            kit::ReviewGitDialog::Switch => {}
        }
        cx.notify();
    }

    fn git_form_action(
        &mut self,
        action: &kit::ReviewGitFormAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            kit::ReviewGitFormAction::SelectMerge(name) => self.merge_selected = Some(name.clone()),
            kit::ReviewGitFormAction::SwitchStash(stash) => self.switch_stash = *stash,
            kit::ReviewGitFormAction::IncludeUntracked(include) => {
                self.include_untracked = *include
            }
            kit::ReviewGitFormAction::Close => {
                window.close_dialog(cx);
                self.git_dialog = None;
            }
            _ => {
                // Preview submits only a local notice; recorded status, branch,
                // files and drafts are never changed by checkout/Git actions.
                window.close_dialog(cx);
                self.git_dialog = None;
                Self::notice(window, cx);
            }
        }
        cx.notify();
    }

    fn git_form(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = kit::ReviewGitFormState {
            current_branch: self
                .status
                .as_ref()
                .and_then(|status| status.branch.as_deref())
                .unwrap_or("main"),
            target_branch: self.switch_target.as_deref(),
            selected_merge: self.merge_selected.as_deref(),
            busy: false,
            stash_changes: self.switch_stash,
            include_untracked: self.include_untracked,
        };
        let action = cx.listener(|host, action: &kit::ReviewGitFormAction, window, cx| {
            host.git_form_action(action, window, cx)
        });
        Some(match self.git_dialog? {
            kit::ReviewGitDialog::NewBranch => {
                kit::review_new_branch_form(&self.branch_name, &state, action, cx)
                    .into_any_element()
            }
            kit::ReviewGitDialog::Merge => {
                kit::review_merge_form(&self.merge_filter, self.status.as_ref(), &state, action, cx)
                    .into_any_element()
            }
            kit::ReviewGitDialog::Switch => {
                kit::review_switch_form(&state, action, cx).into_any_element()
            }
            kit::ReviewGitDialog::Stash => {
                kit::review_stash_form(&self.stash_message, &state, action, cx).into_any_element()
            }
        })
    }

    fn files(&self) -> &[GitFile] {
        self.status
            .as_ref()
            .map_or(&[], |status| status.files.as_slice())
    }

    fn filtered_files(&self, cx: &App) -> Vec<GitFile> {
        let query = self.filter.read(cx).value().trim().to_lowercase();
        self.files()
            .iter()
            .filter(|file| query.is_empty() || file.path.to_lowercase().contains(&query))
            .cloned()
            .collect()
    }

    fn notice(window: &mut Window, cx: &mut App) {
        window.push_notification(
            Notification::info(
                "Git actions run in the desktop host. This saved checkout was not changed.",
            ),
            cx,
        );
    }

    fn request(&mut self, action: kit::ReviewAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            kit::ReviewAction::ClearFilter => self
                .filter
                .update(cx, |input, cx| input.set_value("", window, cx)),
            kit::ReviewAction::SelectList => self.mode = kit::ReviewViewMode::List,
            kit::ReviewAction::SelectTree => self.mode = kit::ReviewViewMode::Tree,
            kit::ReviewAction::SelectAll(selected) => {
                if selected {
                    self.selected = self.files().iter().map(|file| file.path.clone()).collect();
                } else {
                    self.selected.clear();
                }
            }
            kit::ReviewAction::OpenCombinedDiff => self.open_diff("".into(), cx),
            kit::ReviewAction::ClearCommit => self
                .commit
                .update(cx, |input, cx| input.set_value("", window, cx)),
            _ => Self::notice(window, cx),
        }
        cx.notify();
    }

    fn captured_diff(&self) -> Option<&str> {
        let path = self.document.as_deref()?;
        let captured = if path.is_empty() {
            self.combined.as_ref()
        } else {
            self.diffs.get(path)
        }?;
        Some(if self.ignore_whitespace {
            &captured.ignore_whitespace
        } else {
            &captured.text
        })
    }

    fn open_diff(&mut self, path: String, cx: &mut Context<Self>) {
        let initial_open = self.document.is_none();
        self.review_document.update(cx, |document, cx| document.reset(cx));
        self.document_revision += 1;
        self.document = Some(path);
        self.update_diff(cx);
        if initial_open {
            self.review_document
                .update(cx, |document, cx| document.request_focus(cx));
        }
    }

    fn update_diff(&mut self, cx: &mut Context<Self>) {
        let target = self.excerpt_target();
        self.review_document.update(cx, |document, cx| {
            document.set_excerpt_target(target, cx);
            document.loading(self.ignore_whitespace, cx);
            if let Some(text) = self.captured_diff() {
                document.set_patch(text, cx);
            } else {
                document.failed("This diff is not included in the bounded snapshot. Re-import the session or open desktop Review for live content.".into(), cx);
            }
        });
        cx.notify();
    }

    fn excerpt_target(&self) -> Result<kit::ReviewExcerptTarget, String> {
        let path = self
            .document
            .as_ref()
            .filter(|path| !path.is_empty())
            .ok_or("Open a single file instead of All changes")?;
        let session = self.session.as_ref().ok_or("No chat draft is available")?;
        Ok(kit::ReviewExcerptTarget::new(
            session.runtime_work_dir.clone(),
            path.clone(),
            (Some(session.work_dir.clone()), Some(session.id.clone())),
            self.document_revision,
            self.ignore_whitespace,
        ))
    }

    fn file_row(&self, file: &GitFile, tree: bool, cx: &mut Context<Self>) -> AnyElement {
        let toggle_path = file.path.clone();
        let open_path = file.path.clone();
        let absolute = self.session.as_ref().map(|session| {
            session
                .runtime_work_dir
                .join(&file.path)
                .display()
                .to_string()
        });
        let menu_file = file.clone();
        let owner = cx.entity().downgrade();
        kit::review_file_inset(
            kit::review_file_row(
                file,
                kit::ReviewFileAppearance {
                    selected: self.selected.contains(&file.path),
                    open: self.document.as_deref() == Some(&file.path),
                    tree_node: tree,
                    absolute_path: absolute.as_deref(),
                },
                cx.listener(move |host, selected: &bool, _, cx| {
                    if *selected {
                        host.selected.insert(toggle_path.clone());
                    } else {
                        host.selected.remove(&toggle_path);
                    }
                    cx.notify();
                }),
                cx.listener(move |host, _, _, cx| host.open_diff(open_path.clone(), cx)),
                cx,
            )
            .context_menu(move |menu, window, cx| {
                let Some(host) = owner.upgrade() else {
                    return menu;
                };
                let view = host.read(cx);
                let selected: Vec<_> = view.selected.iter().cloned().collect();
                let total_files = view.files().len();
                let file_manager_label = view.file_manager_label.clone();
                kit::review_file_menu(
                    menu,
                    &kit::ReviewFileMenu {
                        file: &menu_file,
                        selected_paths: &selected,
                        total_files,
                        absolute_path: absolute.as_deref(),
                        reveal_label: &file_manager_label,
                        busy: false,
                    },
                    {
                        let owner = owner.clone();
                        move |action, window, cx| {
                            let _ =
                                owner.update(cx, |host, cx| host.file_action(action, window, cx));
                        }
                    },
                    window,
                    cx,
                )
            }),
        )
        .into_any_element()
    }

    fn file_action(
        &self,
        action: &kit::ReviewFileAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            kit::ReviewFileAction::OpenDiff(path) => self.open_recorded_diff(
                path.clone(),
                self.diffs.get(path).map(|diff| diff.text.clone()),
                window,
                cx,
            ),
            kit::ReviewFileAction::CopyRelative(path)
            | kit::ReviewFileAction::CopyAbsolute(path) => {
                cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                window.push_notification(Notification::info("Copied file path"), cx);
            }
            _ => Self::notice(window, cx),
        }
    }

    fn pr_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pr = self.status.as_ref()?.pr.as_ref()?;
        Some(kit::review_pr_card(pr, &kit::ReviewPrState {
            expanded: self.pr_expanded, feedback_count: self.feedback_count, can_address: self.session.is_some(), busy: false,
        }, cx.listener(|host, action: &kit::ReviewPrAction, window, cx| {
            if *action == kit::ReviewPrAction::Toggle { host.pr_expanded = !host.pr_expanded; cx.notify(); }
            else { window.push_notification(Notification::info("PR actions run in the desktop host. No GitHub request or task was sent."), cx); }
        }), cx).into_any_element())
    }

    fn row(&mut self, index: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.filtered_files(cx)
            .get(index)
            .map(|file| self.file_row(file, false, cx))
            .unwrap_or_else(|| div().into_any_element())
    }

    fn open_recorded_diff(
        &self,
        title: String,
        content: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(content) = content {
            cx.emit(ReviewPreviewEvent::OpenDiff { title, content });
        } else {
            window.push_notification(Notification::info("This recorded diff is not included in the bounded snapshot. Re-import the session or inspect it in desktop Review."), cx);
        }
    }

    fn history(&self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.history_filter.read(cx).value();
        let commits = self
            .status
            .as_ref()
            .map_or(&[][..], |status| status.recent_commits.as_slice());
        let filtered = kit::review_filtered_commits(commits, &query);
        let content = if filtered.is_empty() {
            kit::review_history_empty(
                !query.trim().is_empty(),
                cx.listener(|host, _, window, cx| {
                    host.history_filter
                        .update(cx, |input, cx| input.set_value("", window, cx));
                    cx.notify();
                }),
                cx,
            )
            .into_any_element()
        } else {
            kit::review_history_viewport().children(filtered.into_iter().map(|commit| {
                let expanded = self.selected_commit.as_deref() == Some(&commit.sha);
                let record = self.commits.get(&commit.sha);
                let body = expanded.then(|| {
                    if let Some(record) = record {
                        let rows = record.files.iter().map(|file| {
                            let title = format!("{} @ {}", file.path, commit.short_sha);
                            let content = record.diffs.get(&file.path).cloned();
                            kit::review_commit_file(&commit.sha, file, cx.listener(move |host, _, window, cx| {
                                host.open_recorded_diff(title.clone(), content.clone(), window, cx);
                            }), cx).into_any_element()
                        }).collect();
                        kit::review_commit_files(false, rows, cx).into_any_element()
                    } else {
                        div().p_2().text_xs().text_color(cx.theme().muted_foreground)
                            .child("Changed files are not included in this snapshot. Re-import the session or inspect this commit in desktop Review.").into_any_element()
                    }
                });
                let sha = commit.sha.clone();
                kit::review_commit_card(commit, expanded, body, cx.listener(move |host, _, _, cx| {
                    host.selected_commit = if host.selected_commit.as_deref() == Some(&sha) { None } else { Some(sha.clone()) };
                    cx.notify();
                }), cx)
            })).into_any_element()
        };
        kit::review_history_surface(&self.history_filter, content, cx).into_any_element()
    }

    fn stash(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let stash = self.status.as_ref()?.current_stash.as_ref()?;
        let record = self.stashes.get(&stash.index);
        let body = self.stash_expanded.then(|| {
            if let Some(record) = record {
                let rows = record.files.iter().map(|file| {
                    let title = file.path.clone();
                    let content = record.diffs.get(&file.path).cloned();
                    kit::review_stash_file(stash.index, file, cx.listener(move |host, _, window, cx| {
                        host.open_recorded_diff(title.clone(), content.clone(), window, cx);
                    }), cx).into_any_element()
                }).collect();
                kit::review_stash_files(false, rows, cx).into_any_element()
            } else {
                div().text_xs().text_color(cx.theme().muted_foreground).child("Stashed files are not included in this snapshot. Re-import the session or inspect the stash in desktop Review.").into_any_element()
            }
        });
        Some(
            kit::review_stash_card(
                stash,
                &kit::ReviewStashState {
                    expanded: self.stash_expanded,
                    loading: false,
                    files_count: record.map(|record| record.files.len()),
                    busy: false,
                },
                body,
                cx.listener(|host, action: &kit::ReviewStashAction, window, cx| {
                    if *action == kit::ReviewStashAction::Toggle {
                        host.stash_expanded = !host.stash_expanded;
                        cx.notify();
                    } else {
                        Self::notice(window, cx);
                    }
                }),
                cx,
            )
            .into_any_element(),
        )
    }
}

impl Render for ReviewPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let repository = self
            .session
            .as_ref()
            .and_then(|session| session.work_dir.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "No repository".into());
        let branch = self
            .status
            .as_ref()
            .and_then(|status| status.branch.as_deref())
            .unwrap_or("no branch");
        let context = kit::ReviewWorkspaceContext {
            repository,
            branch: branch.into(),
            git_state: self.status.as_ref().map_or_else(
                || "Git status unavailable".into(),
                |status| {
                    if status.has_changes {
                        format!("{} files changed", status.files.len())
                    } else {
                        "No changes".into()
                    }
                },
            ),
            worktree: self
                .session
                .as_ref()
                .is_some_and(|session| session.is_worktree),
            unavailable: self
                .session
                .as_ref()
                .is_some_and(|session| !session.worktree_available),
            file: self
                .document
                .as_ref()
                .map(|path| {
                    if path.is_empty() {
                        "Review · All changes".into()
                    } else {
                        format!("Review · {path}")
                    }
                })
                .unwrap_or_else(|| "No active file".into()),
            has_changes: self
                .status
                .as_ref()
                .is_some_and(|status| status.has_changes),
        };
        let context = kit::review_workspace_context(&context, |_, _, _| {}, cx);
        if let Some(path) = &self.document {
            let title = if path.is_empty() {
                "Review · All changes".into()
            } else {
                format!("Review · {path}")
            };
            return kit::review_panel_surface()
                .key_context("ReviewDiff")
                .on_action(cx.listener(|host, _: &kit::FindInDiff, window, cx| {
                    host.review_document
                        .update(cx, |document, cx| document.open_find(window, cx));
                }))
                .child(context)
                .child(kit::panel_document_header(
                    None,
                    &title,
                    false,
                    false,
                    None,
                    true,
                    None,
                    cx.listener(|host, _: &kit::PanelDocumentAction, _, cx| {
                        host.document = None;
                        host.review_document.update(cx, |document, cx| document.reset(cx));
                        cx.notify();
                    }),
                ))
                .child(kit::review_whitespace_control(
                    self.ignore_whitespace,
                    cx.listener(|host, checked: &bool, _, cx| {
                        host.ignore_whitespace = *checked;
                        host.update_diff(cx);
                    }),
                    cx,
                ))
                .children((!path.is_empty()).then(|| {
                    let paths: Vec<String> = self
                        .filtered_files(cx)
                        .iter()
                        .map(|file| file.path.clone())
                        .collect();
                    let query = self.filter.read(cx).value().trim().to_string();
                    kit::review_diff_nav(
                        &kit::ReviewDiffNavigation {
                            paths: &paths,
                            current: Some(path.as_str()),
                            filter: (!query.is_empty()).then_some(query.as_str()),
                            unavailable: None,
                            focus: Some(&self.review_nav_focus),
                        },
                        cx.listener(|host, action: &kit::ReviewDiffNavAction, window, cx| {
                            let Some(current) = host.document.clone() else {
                                return;
                            };
                            let paths: Vec<String> = host
                                .filtered_files(cx)
                                .iter()
                                .map(|file| file.path.clone())
                                .collect();
                            let Some(adjacency) =
                                kit::review_diff_adjacency(&paths, &current)
                            else {
                                return;
                            };
                            let target = match action {
                                kit::ReviewDiffNavAction::Previous => adjacency.previous,
                                kit::ReviewDiffNavAction::Next => adjacency.next,
                            };
                            if let Some(target) = target {
                                let boundary = kit::review_diff_adjacency(&paths, &target)
                                    .is_some_and(|next_adjacency| match action {
                                        kit::ReviewDiffNavAction::Previous => {
                                            next_adjacency.previous.is_none()
                                        }
                                        kit::ReviewDiffNavAction::Next => {
                                            next_adjacency.next.is_none()
                                        }
                                    });
                                if boundary {
                                    window.focus(&host.review_nav_focus, cx);
                                }
                                host.open_diff(target, cx);
                            }
                        }),
                        cx,
                    )
                }))
                .child(Separator::horizontal())
                .child(self.review_document.clone());
        }
        let filtered = self.filtered_files(cx);
        if self.list.item_count() != filtered.len() {
            self.list
                .reset_with_uniform_height(filtered.len(), window.rem_size() * 2.0);
        }
        let total = self.files().len();
        let staged = self.files().iter().filter(|file| file.staged).count();
        let selection = kit::ReviewSelectionState {
            selected_count: self.selected.len(),
            total_files: total,
            additions: self
                .files()
                .iter()
                .filter(|file| self.selected.contains(&file.path))
                .map(|file| file.additions)
                .sum(),
            deletions: self
                .files()
                .iter()
                .filter(|file| self.selected.contains(&file.path))
                .map(|file| file.deletions)
                .sum(),
            unstaged_count: self.files().iter().filter(|file| file.unstaged).count(),
            has_staged: staged > 0,
            busy: false,
        };
        let behind = self.status.as_ref().map_or(0, |status| status.behind);
        let ahead = self.status.as_ref().map_or(0, |status| status.ahead);
        let publish = kit::review_can_publish_branch(
            self.session
                .as_ref()
                .is_some_and(|session| session.worktree_available),
            self.status.as_ref(),
        );
        let kind = if publish {
            kit::ReviewSyncKind::Publish
        } else if behind > 0 {
            kit::ReviewSyncKind::Pull
        } else if ahead > 0 {
            kit::ReviewSyncKind::Push
        } else {
            kit::ReviewSyncKind::Fetch
        };
        let branch_header = kit::review_branch_header(
            branch,
            self.branch_open,
            kit::review_sync_actions(
                kit::review_sync_button(
                    kind,
                    if behind > 0 { behind } else { ahead },
                    "Fetch latest changes",
                    false,
                )
                .on_click(|_, window, cx| Self::notice(window, cx)),
                (total > 0).then(|| {
                    kit::review_stash_button(false).on_click(cx.listener(|host, _, window, cx| {
                        host.open_git_dialog(kit::ReviewGitDialog::Stash, window, cx)
                    }))
                }),
                self.can_create_pr.then(|| {
                    kit::review_create_pr_button(false)
                        .on_click(cx.listener(|host, _, window, cx| crate::draft_pr::open(host.draft_pr.clone(), window, cx)))
                }),
            ),
            cx.listener(|host, _, _, cx| {
                host.branch_open = !host.branch_open;
                cx.notify();
            }),
            cx,
        );
        let tabs = kit::review_tabs(
            self.tab,
            total,
            staged,
            self.status
                .as_ref()
                .map_or(0, |status| status.recent_commits.len()),
            cx.listener(|host, index: &usize, _, cx| {
                host.tab = if *index == 0 {
                    kit::ReviewTab::Changes
                } else {
                    kit::ReviewTab::History
                };
                cx.notify();
            }),
            cx,
        );
        if self.status.is_none() {
            return kit::review_panel_surface()
                .child(context)
                .child(branch_header)
                .child(tabs)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .p_4()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No saved Git inspection. Import a session from a Git checkout."),
                );
        }
        if self.branch_open {
            return kit::review_panel_surface()
                .child(context)
                .child(branch_header)
                .child(kit::review_branch_manager(
                    &self.branch_filter,
                    self.status.as_ref(),
                    false,
                    self.session.is_some(),
                    cx.listener(|host, action: &kit::ReviewBranchAction, window, cx| {
                        host.branch_action(action, window, cx)
                    }),
                    cx,
                ));
        }
        if self.tab == kit::ReviewTab::History {
            return kit::review_panel_surface()
                .child(context)
                .child(branch_header)
                .child(tabs)
                .child(self.history(cx));
        }
        let toolbar = (total > 0).then(|| {
            kit::review_toolbar_surface(cx)
                .child(kit::review_filters(
                    &self.filter,
                    self.mode,
                    kit::review_discard_button(false)
                        .on_click(|_, window, cx| Self::notice(window, cx))
                        .context_menu({
                            let owner = cx.entity().downgrade();
                            move |menu, window, cx| {
                                let Some(host) = owner.upgrade() else {
                                    return menu;
                                };
                                let view = host.read(cx);
                                let paths: Vec<_> = view.selected.iter().cloned().collect();
                                kit::review_discard_menu(
                                    menu,
                                    kit::review_selection_discard_targets(
                                        &paths,
                                        view.files().len(),
                                    ),
                                    false,
                                    |_, window, cx| Self::notice(window, cx),
                                    window,
                                    cx,
                                )
                            }
                        })
                        .into_any_element(),
                    cx.listener(|host, action: &kit::ReviewAction, window, cx| {
                        host.request(*action, window, cx)
                    }),
                    cx,
                ))
                .child(kit::review_selection_bar(
                    &selection,
                    cx.listener(|host, action: &kit::ReviewAction, window, cx| {
                        host.request(*action, window, cx)
                    }),
                    cx,
                ))
                .children(kit::review_diff_ratio(
                    self.files().iter().map(|file| file.additions).sum(),
                    self.files().iter().map(|file| file.deletions).sum(),
                    cx,
                ))
        });
        let files = if total == 0 {
            kit::review_clean_state(
                kit::review_refresh_button(false)
                    .on_click(|_, window, cx| Self::notice(window, cx)),
                cx,
            )
            .into_any_element()
        } else if filtered.is_empty() {
            kit::review_no_results(
                cx.listener(|host, _, window, cx| {
                    host.request(kit::ReviewAction::ClearFilter, window, cx)
                }),
                cx,
            )
            .into_any_element()
        } else if self.mode == kit::ReviewViewMode::Tree {
            let mut folders = BTreeMap::<String, Vec<GitFile>>::new();
            for file in filtered {
                folders
                    .entry(file.path.rsplit_once('/').map_or("", |(dir, _)| dir).into())
                    .or_default()
                    .push(file);
            }
            kit::review_tree_viewport()
                .children(folders.into_iter().map(|(folder, files)| {
                    let collapsed = self.collapsed.contains(&folder);
                    let target = folder.clone();
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            kit::review_folder_header(&folder, files.len(), collapsed, cx)
                                .on_click(cx.listener(move |host, _, _, cx| {
                                    if !host.collapsed.remove(&target) {
                                        host.collapsed.insert(target.clone());
                                    }
                                    cx.notify();
                                })),
                        )
                        .children((!collapsed).then(|| {
                            kit::review_folder_files(cx)
                                .children(files.iter().map(|file| self.file_row(file, true, cx)))
                        }))
                }))
                .into_any_element()
        } else {
            kit::review_file_list(&self.list, cx.processor(Self::row)).into_any_element()
        };
        kit::review_panel_surface()
            .child(context)
            .child(branch_header)
            .child(tabs)
            .child(
                kit::review_changes_body()
                    .child(
                        kit::review_changes_content()
                            .children(self.stash(cx))
                            .children(self.pr_card(cx))
                            .children(toolbar)
                            .child(kit::review_changes_files(files)),
                    )
                    .child(kit::review_commit_footer(
                        &self.commit,
                        &kit::ReviewCommitState {
                            selected_count: self.selected.len(),
                            total_files: total,
                            busy: false,
                            generating: false,
                            can_push: ahead > 0,
                        },
                        cx.listener(|host, action: &kit::ReviewAction, window, cx| {
                            host.request(*action, window, cx)
                        }),
                        cx,
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Saved checkout · Selection and draft stay local · No Git actions executed",
                    ),
            )
    }
}

#[cfg(test)]
#[path = "review_tests.rs"]
mod tests;

/// Labeled gallery fixture; never substituted for a saved checkout's PR.
pub fn sample_review_pr(mode: usize) -> threadlane_protocol::repo::GitHubPrInfo {
    use threadlane_protocol::repo::{GitHubPrInfo, PrCheckStatus};
    let (failing, pending, passing) = match mode {
        0 | 4 => (1, 1, 2),
        1 => (0, 2, 2),
        2 => (0, 0, 4),
        _ => (0, 0, 0),
    };
    GitHubPrInfo {
        number: 42,
        title: "Sample PR · Refine the shared conversation and review components at narrow widths"
            .into(),
        url: "https://example.com/sample/pull/42".into(),
        total_checks: failing + pending + passing,
        failing_checks: failing,
        pending_checks: pending,
        passing_checks: passing,
        checks: if failing > 0 {
            vec![PrCheckStatus {
                name: "Sample UI layout checks".into(),
                conclusion: Some("FAILURE".into()),
                ..Default::default()
            }]
        } else {
            Vec::new()
        },
        ..Default::default()
    }
}
