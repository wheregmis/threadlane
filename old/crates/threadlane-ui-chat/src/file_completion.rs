use super::*;

use threadlane_git::GitFileInventory;
use threadlane_protocol::repo::FILE_INVENTORY_NOT_A_REPOSITORY;

pub(super) use threadlane_ui_kit::file_completion::{
    CompleteFileCompletion, DismissFileCompletion, SelectNextFileCompletion,
    SelectPreviousFileCompletion,
};

pub(super) fn init_file_completion(cx: &mut App) {
    threadlane_ui_kit::file_completion::init_file_completion(cx);
}

#[derive(Debug)]
pub(super) enum FileCompletionStatus {
    /// `git ls-files` is still running for the resolved root.
    Loading,
    /// The resolved root is not usable for completion; `String` explains why
    /// (no project, preparing/missing worktree, or not a Git workspace).
    Unsupported(String),
    /// Git ran but failed — never rendered as "No matching files".
    Failed(String),
    Ready(GitFileInventory),
}

/// The picker's transient state for one resolved root under one composer.
/// Root and composer key are captured at open time; a session, project, or
/// draft switch clears the whole state instead of reusing stale results.
pub(super) struct FileCompletionState {
    /// The Git checkout `paths` were enumerated from. `None` when the root is
    /// unsupported — never silently retargeted to another checkout.
    pub(super) root: Option<PathBuf>,
    pub(super) composer_key: ComposerKey,
    pub(super) status: FileCompletionStatus,
}

impl ChatListView {
    /// The caret-local `@` trigger, or `None` while completion is suppressed:
    /// away from the chat tab, on a non-collapsed selection, or inside IME
    /// composition (`cursor()` then reports the marked range end, which the
    /// collapsed selection does not match).
    pub(super) fn current_file_trigger(&self, cx: &App) -> Option<FileQueryTrigger> {
        if self.current_tab != CentralTab::Chat {
            return None;
        }
        let input = self.input_state.read(cx);
        let selection = input.selected_range();
        if selection.start != selection.end {
            return None;
        }
        let caret = input.cursor();
        if caret != selection.end {
            return None;
        }
        active_file_query(&input.value(), caret)
    }

    /// Whether the picker is open — an active trigger that was not dismissed.
    /// Open in any state (loading/empty/error/unsupported) means Enter/Tab are
    /// owned by the picker and must never reach Send/Queue/Steer.
    pub(super) fn file_menu_open(&self, cx: &App) -> bool {
        !self.dismiss_file_menu && self.current_file_trigger(cx).is_some()
    }

    /// Open, refresh, or close the picker for the current trigger. Called from
    /// the composer `Change` subscription and deferred when a trigger exists
    /// without state (e.g. a restored draft ending in `@`).
    pub(super) fn sync_file_completion(&mut self, cx: &mut Context<Self>) {
        if self.current_file_trigger(cx).is_none() {
            if self.file_completion.is_some() {
                self.clear_file_completion();
                cx.notify();
            }
            return;
        }
        let composer_key = self.composer_key.clone();
        let root = self.model.read(cx).active_git_work_dir();
        let keep = self
            .file_completion
            .as_ref()
            .is_some_and(|state| state.composer_key == composer_key && state.root == root);
        if keep {
            return;
        }
        match root {
            Some(root) => self.request_file_inventory(root, cx),
            None => {
                self.file_completion_generation = self.file_completion_generation.wrapping_add(1);
                self.file_completion_task = None;
                self.file_completion = Some(FileCompletionState {
                    root: None,
                    composer_key,
                    status: FileCompletionStatus::Unsupported(
                        self.file_scope_unavailable_reason(cx),
                    ),
                });
                self.selected_file_index = 0;
                cx.notify();
            }
        }
    }

    /// Enumerate `root` off the UI thread. Results apply only if the
    /// generation, composer key, and resolved root are all still current.
    fn request_file_inventory(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.file_completion_generation = self.file_completion_generation.wrapping_add(1);
        let generation = self.file_completion_generation;
        let composer_key = self.composer_key.clone();
        self.file_completion_task = None;
        self.file_completion = Some(FileCompletionState {
            root: Some(root.clone()),
            composer_key: composer_key.clone(),
            status: FileCompletionStatus::Loading,
        });
        self.selected_file_index = 0;
        self.file_scroll_handle.scroll_to_item(0);
        cx.notify();
        let task_root = root.clone();
        let client = self.model.read(cx).daemon_client.clone();
        self.file_completion_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    threadlane_ui_state::project_io::file_inventory(&client, &task_root).await
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.file_completion_generation != generation
                    || this.composer_key != composer_key
                    || this.model.read(cx).active_git_work_dir().as_deref() != Some(root.as_path())
                {
                    return;
                }
                this.file_completion_task = None;
                this.selected_file_index = 0;
                this.file_completion = Some(FileCompletionState {
                    root: Some(root),
                    composer_key,
                    status: match result {
                        Ok(inventory) => FileCompletionStatus::Ready(inventory),
                        Err(error) if error == FILE_INVENTORY_NOT_A_REPOSITORY => {
                            FileCompletionStatus::Unsupported(
                                "File completion requires a Git workspace".to_string(),
                            )
                        }
                        Err(error) => FileCompletionStatus::Failed(error),
                    },
                });
                cx.notify();
            });
        }));
    }

    pub(super) fn clear_file_completion(&mut self) {
        self.file_completion = None;
        self.file_completion_generation = self.file_completion_generation.wrapping_add(1);
        self.file_completion_task = None;
        self.selected_file_index = 0;
        self.file_scroll_handle.scroll_to_item(0);
        self.dismiss_file_menu = false;
    }

    /// Why `@` completion cannot search right now. Each case is named so the
    /// user can act (attach a project, wait for the worktree, use a Git
    /// workspace); never claims "no matches".
    fn file_scope_unavailable_reason(&self, cx: &App) -> String {
        let state = self.model.read(cx);
        if state.active_work_dir.is_none() {
            "Attach a project to search files".to_string()
        } else if state.active_worktree_setup().is_some() {
            "Preparing the session workspace…".to_string()
        } else if state.active_worktree_unavailable() {
            "This session's worktree is unavailable; file completion is off".to_string()
        } else {
            "File completion requires a Git workspace".to_string()
        }
    }

    /// Rows for `query` from the current inventory, plus whether more matches
    /// exist beyond the cap. Empty for any non-Ready status.
    fn file_completion_matches(&self, query: &str) -> (Vec<String>, bool) {
        let Some(FileCompletionState {
            status: FileCompletionStatus::Ready(inventory),
            ..
        }) = &self.file_completion
        else {
            return (Vec::new(), false);
        };
        let (matches, has_more) =
            filter_file_matches(query, &inventory.paths, FILE_COMPLETION_RESULT_LIMIT);
        (matches.into_iter().cloned().collect(), has_more)
    }

    /// Replace the live trigger range with `path` as a Markdown code span.
    /// Draft, composer key, resolved root, live trigger, membership in the
    /// current match list, and on-disk existence are all re-checked at apply
    /// time so a late click or session switch can never edit unexpected text.
    fn apply_file_completion(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = &self.file_completion else {
            return;
        };
        if state.composer_key != self.composer_key {
            self.clear_file_completion();
            return;
        }
        let Some(root) = state.root.clone() else {
            return;
        };
        if self.model.read(cx).active_git_work_dir().as_deref() != Some(root.as_path()) {
            self.clear_file_completion();
            cx.notify();
            return;
        }
        let Some(trigger) = self.current_file_trigger(cx) else {
            return;
        };
        let (matches, _) = self.file_completion_matches(&trigger.query);
        if !matches.iter().any(|candidate| candidate == path) {
            return;
        }
        // Existence is verified on the daemon host through the guarded
        // project-io path. A pre-3 remote daemon answers UNSUPPORTED:
        // probing the *client's* disk at the daemon-side root would
        // judge the wrong filesystem, so unsupported counts as absent —
        // the inventory refresh below re-asks the daemon and degrades
        // to its own unsupported state.
        let client = self.model.read(cx).daemon_client.clone();
        let composer_key = self.composer_key.clone();
        let generation = self.file_completion_generation;
        let path = path.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let exists = client.supports_project_io() && is_safe_relative_path(&path) && {
                // The in-process daemon answers inline on the caller's
                // thread, so the probe hops to a background executor
                // instead of `stat`ing on the UI thread.
                let probe_client = client.clone();
                let probe_root = root.clone();
                let probe_path = path.clone();
                cx.background_executor()
                    .spawn(async move {
                        threadlane_ui_state::project_io::file_exists(
                            &probe_client,
                            &probe_root,
                            probe_path,
                        )
                        .await
                        .unwrap_or(false)
                    })
                    .await
            };
            let _ = this.update_in(cx, |this, window, cx| {
                // A remote probe may finish after switching session, checkout,
                // or dismissing/reopening the picker with the same query.
                if this.composer_key != composer_key
                    || this.file_completion_generation != generation
                    || !this.file_menu_open(cx)
                    || this.model.read(cx).active_git_work_dir().as_deref() != Some(root.as_path())
                {
                    return;
                }
                if !exists {
                    this.request_file_inventory(root, cx);
                    return;
                }
                // The remote existence check yielded; re-verify the
                // trigger before editing the composer.
                if this.current_file_trigger(cx).as_ref() != Some(&trigger) {
                    return;
                }
                let insertion = format_path_insertion(&path);
                this.input_state.update(cx, |input, cx| {
                    input.set_selected_range(trigger.range.clone(), cx);
                    input.replace(insertion, window, cx);
                    input.focus(window, cx);
                });
                this.clear_file_completion();
                cx.notify();
            });
        })
        .detach();
    }

    /// Enter/Tab: insert the current result if one is valid. `true` means the
    /// picker consumed the key — callers must not fall through to submission.
    /// A `false` return with the picker open means there is no valid result:
    /// Enter stays swallowed by the caller, Tab may leave the list.
    pub(super) fn apply_selected_file_completion(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(trigger) = self.current_file_trigger(cx) else {
            return false;
        };
        let (matches, _) = self.file_completion_matches(&trigger.query);
        if matches.is_empty() {
            return false;
        }
        let index = self.selected_file_index.min(matches.len() - 1);
        let path = matches[index].clone();
        self.apply_file_completion(&path, window, cx);
        true
    }

    fn file_match_count(&self, cx: &App) -> usize {
        let Some(trigger) = self.current_file_trigger(cx) else {
            return 0;
        };
        self.file_completion_matches(&trigger.query).0.len()
    }

    pub(super) fn select_previous_file_completion_action(
        &mut self,
        _: &SelectPreviousFileCompletion,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.file_match_count(cx);
        if count == 0 {
            return;
        }
        self.selected_file_index = if self.selected_file_index == 0 {
            count - 1
        } else {
            self.selected_file_index - 1
        };
        self.file_scroll_handle
            .scroll_to_item(self.selected_file_index);
        cx.notify();
    }

    pub(super) fn select_next_file_completion_action(
        &mut self,
        _: &SelectNextFileCompletion,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.file_match_count(cx);
        if count == 0 {
            return;
        }
        self.selected_file_index = (self.selected_file_index + 1) % count;
        self.file_scroll_handle
            .scroll_to_item(self.selected_file_index);
        cx.notify();
    }

    pub(super) fn complete_file_completion_action(
        &mut self,
        _: &CompleteFileCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dismiss_file_menu {
            return;
        }
        if !self.apply_selected_file_completion(window, cx) {
            // Tab may leave the list without inserting when nothing is valid.
            cx.propagate();
        }
    }

    pub(super) fn dismiss_file_completion_action(
        &mut self,
        _: &DismissFileCompletion,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_file_menu = true;
        cx.notify();
    }

    /// Same keys as the `FileCompletionMenu > Input` bindings for events that
    /// bubble to the composer container instead of dispatching an action.
    pub(super) fn handle_file_completion_key_down(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.file_menu_open(cx) {
            return false;
        }
        match key {
            "escape" => {
                // Keep the typed `@query`; only close the list. Stopping
                // propagation keeps Escape away from global cancel handlers.
                self.dismiss_file_menu = true;
                cx.stop_propagation();
                cx.notify();
            }
            "up" => {
                self.select_previous_file_completion_action(
                    &SelectPreviousFileCompletion,
                    window,
                    cx,
                );
                cx.stop_propagation();
            }
            "down" => {
                self.select_next_file_completion_action(&SelectNextFileCompletion, window, cx);
                cx.stop_propagation();
            }
            "tab" => {
                if self.apply_selected_file_completion(window, cx) {
                    cx.stop_propagation();
                }
            }
            "enter" => {
                // Never fall through to Send/Queue/Steer while the list is
                // open, in any state.
                cx.stop_propagation();
            }
            _ => return false,
        }
        true
    }

    pub(super) fn render_file_menu(
        &mut self,
        trigger: &FileQueryTrigger,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use threadlane_ui_kit::file_completion::{
            file_completion_menu, file_completion_popup, FileCompletionMenuStatus,
            FileCompletionRequest,
        };
        let (matches, has_more) = self.file_completion_matches(&trigger.query);
        let scope = self
            .file_completion
            .as_ref()
            .and_then(|state| state.root.as_ref())
            .and_then(|root| root.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "workspace".into());
        let status = match self.file_completion.as_ref().map(|state| &state.status) {
            None | Some(FileCompletionStatus::Loading) => FileCompletionMenuStatus::Loading,
            Some(FileCompletionStatus::Unsupported(reason)) => {
                FileCompletionMenuStatus::Unavailable(reason)
            }
            Some(FileCompletionStatus::Failed(error)) => FileCompletionMenuStatus::Failed(error),
            Some(FileCompletionStatus::Ready(inventory)) => FileCompletionMenuStatus::Ready {
                matches: &matches,
                selected_index: self.selected_file_index,
                has_more,
                non_utf8_skipped: inventory.non_utf8_skipped,
            },
        };
        let owner = cx.entity().downgrade();
        file_completion_popup(file_completion_menu(
            &scope,
            status,
            &self.file_scroll_handle,
            move |request, window, cx| {
                let _ = owner.update(cx, |host, cx| match request {
                    FileCompletionRequest::Insert(path) => {
                        host.apply_file_completion(path, window, cx)
                    }
                    FileCompletionRequest::Retry => {
                        if let Some(root) = host
                            .file_completion
                            .as_ref()
                            .and_then(|state| state.root.clone())
                        {
                            host.request_file_inventory(root, cx);
                        }
                    }
                });
            },
            cx,
        ))
        .into_any_element()
    }
}
