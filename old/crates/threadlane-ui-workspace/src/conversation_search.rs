//! Project conversation search — the command palette's dedicated mode for
//! issue "search project conversations by message text". Enters from the
//! "Search project conversations…" command row, replaces the palette's
//! command list with live scan results (never stacked over it), and hands a
//! confirmed match to the chat find strip via
//! `ChatListView::begin_conversation_find_handoff`.
//!
//! The mode is additive: opening it keeps `command_palette_open` true and
//! simply renders [`WorkspaceView::render_conversation_search`] instead of
//! the commands palette. Every exit path (`close_command_palette`, project
//! switch/removal, re-running a command) returns the palette to Commands
//! mode, so nothing downstream learns a new surface exists.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use threadlane_daemon::conversation_search::{
    search_conversations, ConversationSearchProgress, ConversationSearchReport,
    ConversationSearchTarget,
};

use super::*;

/// Live state of the palette's conversation-search mode. Dropping this
/// struct cancels its tasks and its scan's progress receiver.
pub(super) struct ConversationSearch {
    /// The attached project this search is scoped to; a switch or detach
    /// exits the mode.
    work_dir: PathBuf,
    /// Session targets the current report was built against; comparing it
    /// to a fresh scope re-runs an open query when discovery adds a session
    /// (or a session file/recency stamp changes) instead of serving stale
    /// results.
    session_stamp: Vec<ConversationSearchTarget>,
    /// Query the latest report/in-flight scan answers.
    query: String,
    /// Monotonic generation; only results for the current one publish.
    generation: u64,
    /// 120 ms debounce before each scan; replaced on every keystroke.
    debounce_task: Option<Task<()>>,
    /// Cancellation flag for the in-flight background scan.
    cancelled: Arc<AtomicBool>,
    /// Foreground pump draining the scan's progress channel.
    pump_task: Option<Task<()>>,
    /// Sessions scanned so far in the in-flight scan.
    scanned: usize,
    /// Sessions captured when the query launched.
    total: usize,
    /// Latest completed report for `query` (rows age out on query change).
    report: Option<ConversationSearchReport>,
}

impl ConversationSearch {
    fn new(work_dir: PathBuf) -> Self {
        Self {
            work_dir,
            session_stamp: Vec::new(),
            query: String::new(),
            generation: 0,
            debounce_task: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            pump_task: None,
            scanned: 0,
            total: 0,
            report: None,
        }
    }

    fn in_flight(&self) -> bool {
        self.report.is_none() && self.query.trim().chars().count() >= 2
    }
}

/// Matches below this length stay in the hint state — single-character
/// queries would match nearly every transcript.
pub(super) const CONVERSATION_SEARCH_MIN_CHARS: usize = 2;
const CONVERSATION_SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);

/// Sessions of the currently attached project as scan targets. `None`
/// means no project is attached (the command row stays disabled).
pub(super) fn conversation_search_scope(
    state: &AppState,
) -> Option<(PathBuf, Vec<ConversationSearchTarget>)> {
    let work_dir = state.active_work_dir.clone()?;
    let project = state.projects.iter().find(|p| p.work_dir == work_dir)?;
    let targets = project
        .sessions
        .iter()
        .map(|session| ConversationSearchTarget {
            work_dir: project.work_dir.clone(),
            session_id: session.id.clone(),
            session_file: session.session_file.clone(),
            title: session.title.clone(),
            git_branch: session.git_branch.clone(),
            updated_at: session.updated_at,
        })
        .collect::<Vec<_>>();
    Some((work_dir, targets))
}

impl WorkspaceView {
    /// Switch the open palette into conversation-search mode. The palette
    /// stays open; the query field is cleared so the hint state shows.
    pub(super) fn enter_conversation_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((work_dir, _)) = conversation_search_scope(self.model.read(cx)) else {
            return;
        };
        self.conversation_search = Some(ConversationSearch::new(work_dir));
        self.command_state.update(cx, |state, cx| {
            state.set_loading(false, window, cx);
            state.set_query("", window, cx);
        });
        cx.notify();
    }

    /// Leave search mode: cancel the scan and reset the palette's query and
    /// spinner so re-opening lands on the ordinary Commands list.
    pub(super) fn exit_conversation_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = self.conversation_search.take() {
            search.cancelled.store(true, Ordering::Relaxed);
            self.command_state.update(cx, |state, cx| {
                state.set_loading(false, window, cx);
                state.set_query("", window, cx);
            });
        }
    }

    /// Route every "close the palette" call through this so a palette open in
    /// search mode also cancels its scan.
    pub(super) fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session_picker.take().is_some() {
            self.command_state.update(cx, |state, cx| {
                state.set_loading(false, window, cx);
                state.set_query("", window, cx);
            });
        }
        self.exit_conversation_search(window, cx);
        close_command_palette(
            &mut self.command_palette_open,
            &mut self.command_palette_previous_focus,
            window,
            cx,
        );
    }

    /// Called from the Command's `on_query` — arms the debounce, ages out
    /// rows from the previous query, and below the minimum length returns to
    /// the hint state without scanning.
    fn schedule_conversation_search(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = &mut self.conversation_search else {
            return;
        };
        let query = query.to_string();
        search.query = query.clone();
        search.generation += 1;
        let generation = search.generation;
        // Rows age out the moment the query changes — nothing shows answers
        // to a question the user is no longer asking.
        search.report = None;
        search.scanned = 0;
        search.cancelled.store(true, Ordering::Relaxed);
        search.cancelled = Arc::new(AtomicBool::new(false));
        search.debounce_task = None;
        search.pump_task = None;
        if query.trim().chars().count() < CONVERSATION_SEARCH_MIN_CHARS {
            self.command_state
                .update(cx, |state, cx| state.set_loading(false, window, cx));
            cx.notify();
            return;
        }
        self.command_state
            .update(cx, |state, cx| state.set_loading(true, window, cx));
        search.debounce_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(CONVERSATION_SEARCH_DEBOUNCE)
                .await;
            let _ = this.update(cx, |this, cx| this.launch_conversation_search(generation, cx));
        }));
        cx.notify();
    }

    /// After the debounce: snapshot the project's sessions and start the
    /// serial background scan plus the foreground pump that publishes its
    /// progress. `compute_session_messages` is reused, so scan fidelity is
    /// the hydration path's — no copied parser.
    fn launch_conversation_search(&mut self, generation: u64, cx: &mut Context<Self>) {
        let Some(search) = &mut self.conversation_search else {
            return;
        };
        if search.generation != generation {
            return;
        }
        let Some((_, targets)) = conversation_search_scope(self.model.read(cx)) else {
            return;
        };
        search.total = targets.len();
        search.session_stamp = targets.clone();
        let cancelled = search.cancelled.clone();
        let query = search.query.clone();
        let (progress_tx, mut progress_rx) =
            tokio::sync::mpsc::unbounded_channel::<ConversationSearchProgress>();
        cx.background_executor()
            .spawn(async move {
                search_conversations(targets, &query, &cancelled, Some(progress_tx));
            })
            .detach();
        let window_handle = self.window_handle;
        search.pump_task = Some(cx.spawn(async move |this, cx| {
            while let Some(progress) = progress_rx.recv().await {
                let report = match progress {
                    ConversationSearchProgress::Scanned(scanned) => {
                        let _ = this.update(cx, |this, cx| {
                            if let Some(search) = &mut this.conversation_search {
                                if search.generation == generation {
                                    search.scanned = scanned;
                                    cx.notify();
                                }
                            }
                        });
                        continue;
                    }
                    ConversationSearchProgress::Done(report) => report,
                };
                // The spinner lives on CommandState and needs a Window —
                // hop through the window handle like terminal-link openers.
                let _ = window_handle.update(cx, |_, window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.publish_conversation_search(generation, report, window, cx);
                    });
                });
            }
        }));
    }

    /// Publish a completed scan if it still answers the current
    /// project/query generation; superseded reports are dropped.
    fn publish_conversation_search(
        &mut self,
        generation: u64,
        report: ConversationSearchReport,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command_state
            .update(cx, |state, cx| state.set_loading(false, window, cx));
        let Some(search) = &mut self.conversation_search else {
            return;
        };
        if search.generation != generation {
            return;
        }
        search.pump_task = None;
        search.report = Some(report);
        cx.notify();
    }

    /// Model-observer hook: exit the mode when its project is detached or
    /// the user switched projects, and re-run an open query when session
    /// discovery changes the searchable set.
    pub(super) fn invalidate_conversation_search(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &self.conversation_search else {
            return;
        };
        let state = self.model.read(cx);
        let Some((work_dir, targets)) = conversation_search_scope(state) else {
            // No attached project: run the same cleanup as a manual exit so
            // the scan cancels and the palette resets to Commands mode.
            self.exit_conversation_search_deferred(cx);
            cx.notify();
            return;
        };
        if work_dir != search.work_dir {
            self.exit_conversation_search_deferred(cx);
            cx.notify();
            return;
        }
        if targets != search.session_stamp
            && !search.session_stamp.is_empty()
            && search.query.trim().chars().count() >= CONVERSATION_SEARCH_MIN_CHARS
        {
            let query = search.query.clone();
            let window_handle = self.window_handle;
            let owner = cx.weak_entity();
            cx.defer(move |cx| {
                let _ = window_handle.update(cx, |_, window, cx| {
                    let _ = owner.update(cx, |this, cx| {
                        this.schedule_conversation_search(&query, window, cx);
                    });
                });
            });
        }
    }

    /// `invalidate_conversation_search` runs inside a model observer without
    /// a window, so the CommandState reset hops through the window handle.
    fn exit_conversation_search_deferred(&mut self, cx: &mut Context<Self>) {
        let Some(search) = self.conversation_search.take() else {
            return;
        };
        search.cancelled.store(true, Ordering::Relaxed);
        let command_state = self.command_state.clone();
        let window_handle = self.window_handle;
        cx.defer(move |cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                command_state.update(cx, |state, cx| {
                    state.set_loading(false, window, cx);
                    state.set_query("", window, cx);
                });
            });
        });
    }

    /// Confirm a search row: only rows tagged with the current
    /// project/query generation are accepted, then dispatch the ordinary
    /// `SelectSession` path and queue the find handoff (which itself waits
    /// for destination hydration before seeding the strip).
    fn confirm_conversation_search(
        &mut self,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = &self.conversation_search else {
            return;
        };
        let Some(report) = &search.report else {
            return;
        };
        let Some(hit) = report.matches.get(row) else {
            // The only row past the matches is the retry affordance.
            let query = search.query.clone();
            self.schedule_conversation_search(&query, window, cx);
            return;
        };
        let hit = hit.clone();
        let work_dir = hit.work_dir.clone();
        let session_id = hit.session_id.clone();
        let query = search.query.clone();
        self.close_command_palette(window, cx);
        self.model.update(cx, |state, cx| {
            controller::dispatch(
                state,
                AppAction::SelectSession {
                    work_dir: work_dir.clone(),
                    session_id: session_id.clone(),
                },
            );
            cx.notify();
        });
        self.chat_list.update(cx, |chat, cx| {
            // The handoff only lands on the Chat tab; switch there first so a
            // palette confirmed from Editor still reaches the destination.
            chat.set_tab(threadlane_ui_chat::CentralTab::Chat, cx);
            chat.begin_conversation_find_handoff(
                ConversationFindHandoff {
                    work_dir,
                    session_id,
                    query,
                },
                window,
                cx,
            );
        });
        cx.notify();
    }

    /// The search-mode palette: same frame and backdrop as the commands
    /// palette, with its own header (scope), footer (progress/coverage), and
    /// honest empty states.
    pub(super) fn render_conversation_search(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let search = self.conversation_search.as_ref();
        let (scanned, total, report, in_flight, query_hint) = match search {
            Some(search) => (
                search.scanned,
                search.total,
                search.report.clone(),
                search.in_flight(),
                search.query.trim().chars().count() < CONVERSATION_SEARCH_MIN_CHARS,
            ),
            None => (0, 0, None, false, true),
        };
        let project_name = self
            .model
            .read(cx)
            .active_work_dir
            .as_ref()
            .and_then(|wd| {
                self.model
                    .read(cx)
                    .projects
                    .iter()
                    .find(|p| &p.work_dir == wd)
                    .map(|p| p.name.clone())
            })
            .unwrap_or_default();

        let matches = report
            .as_ref()
            .map(|report| report.matches.clone())
            .unwrap_or_default();
        let mut results_group = CommandGroup::new().label("Conversations");
        // After a partial/limited report the last row offers a retry rather
        // than pretending the scan was complete.
        let retry_row = report
            .as_ref()
            .is_some_and(|report| report.limited || !report.partial.is_empty());
        for hit in &matches {
            let title = if hit.title.is_empty() {
                hit.session_id.clone()
            } else {
                hit.title.clone()
            };
            let context = hit
                .git_branch
                .clone()
                .unwrap_or_else(|| project_name.clone());
            let excerpt = hit.excerpt.clone();
            results_group = results_group.item(threadlane_ui_kit::palette_conversation_item(
                title, context, excerpt,
            ));
        }
        if retry_row {
            results_group = results_group.item(threadlane_ui_kit::palette_item(
                "Retry search",
                "Rescan skipped sessions",
                IconName::Redo,
            ));
        }

        let footer_text =
            search_footer_text(scanned, total, report.as_ref(), in_flight, query_hint);
        let scope_text =
            format!("Conversations in {project_name} · saved user and assistant messages");
        let view_query = cx.weak_entity();
        let view_cancel = cx.weak_entity();
        let view_confirm = cx.weak_entity();

        let view_backdrop = cx.weak_entity();
        let command = threadlane_ui_kit::workspace_palette_command(&self.command_state)
            .placeholder("Search messages in this project's conversations…")
            .filterable(false)
            .header(move |_, _, cx| threadlane_ui_kit::palette_scope(scope_text.clone(), cx))
            .group(results_group)
            .empty(move |_, _, cx| {
                threadlane_ui_kit::palette_empty(search_empty_text(query_hint, in_flight), cx)
            })
            .footer(move |_, _, cx| threadlane_ui_kit::palette_footer(footer_text.clone(), cx))
            .on_query(move |query, window, cx| {
                let _ = view_query.update(cx, |this, cx| {
                    this.schedule_conversation_search(query, window, cx);
                });
            })
            .on_cancel(move |window, cx| {
                let _ = view_cancel.update(cx, |this, cx| {
                    this.close_command_palette(window, cx);
                    cx.notify();
                });
            })
            .on_confirm(move |index, window, cx| {
                let _ = view_confirm.update(cx, |this, cx| {
                    this.confirm_conversation_search(index.row, window, cx);
                });
            });
        threadlane_ui_kit::workspace_palette_frame(
            command,
            move |window, cx| {
                let _ = view_backdrop.update(cx, |this, cx| {
                    this.close_command_palette(window, cx);
                    cx.notify();
                });
            },
            cx,
        )
        .into_any_element()
    }


}

fn search_empty_text(query_hint: bool, in_flight: bool) -> &'static str {
    if query_hint {
        "Type at least 2 characters to search saved conversations"
    } else if in_flight {
        "Searching saved conversations…"
    } else {
        "No matching conversations"
    }
}

/// Footer status: progress while scanning, then coverage honesty — every
/// skipped/unreadable session or budget shows up here, never a silent zero.
fn search_footer_text(
    scanned: usize,
    total: usize,
    report: Option<&ConversationSearchReport>,
    in_flight: bool,
    query_hint: bool,
) -> String {
    if in_flight {
        return format!("Searching saved conversations… {scanned} of {total} scanned");
    }
    if query_hint {
        return "Enter keeps the first conversation result; Escape closes".to_string();
    }
    let Some(report) = report else {
        return "Searching saved conversations…".to_string();
    };
    let mut parts = Vec::new();
    if report.limited {
        parts.push("Search limited; narrow your query".to_string());
    }
    if !report.partial.is_empty() {
        parts.push(report.partial.join("; "));
    }
    if parts.is_empty() {
        if report.matches.is_empty() {
            format!("{} saved conversations scanned; no matches", report.scanned)
        } else {
            format!(
                "{} conversation(s) · {} saved conversations scanned",
                report.matches.len(),
                report.scanned
            )
        }
    } else {
        format!(
            "{} of {} saved conversations scanned — {}",
            report.scanned,
            report.total,
            parts.join(" · ")
        )
    }
}

#[cfg(test)]
mod tests {
    // No `use super::*`: the glob drags gpui macros into test scope and
    // `cargo check --tests` blows the default recursion limit (same trap as
    // threadlane-ui-mirror).
    use super::{
        conversation_search_scope, search_empty_text, search_footer_text,
        ConversationSearchReport,
    };
    use crate::view::AppState;
    use threadlane_protocol::daemon::{
        ProjectInfo, SessionCompletionSummary, SessionHealth, SessionInfo,
    };

    fn session(work_dir: &std::path::Path, id: &str, title: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            title: title.into(),
            work_dir: work_dir.to_path_buf(),
            runtime_work_dir: work_dir.to_path_buf(),
            session_file: work_dir.join(format!("{id}.jsonl")),
            updated_at: 0,
            health: SessionHealth::Healthy,
            git_branch: Some("main".into()),
            github_issue: None,
            is_worktree: false,
            worktree_available: true,
            completion_summary: SessionCompletionSummary::Unknown,
        }
    }

    #[test]
    fn scope_collects_only_the_attached_projects_sessions() {
        let mut state = AppState::default();
        let attached = std::path::Path::new("/work/attached");
        let other = std::path::Path::new("/work/other");
        state.projects = vec![
            ProjectInfo {
                name: "attached".into(),
                work_dir: attached.to_path_buf(),
                sessions: vec![
                    session(attached, "s1", "first"),
                    session(attached, "s2", "second"),
                ],
                is_expanded: true,
            },
            ProjectInfo {
                name: "other".into(),
                work_dir: other.to_path_buf(),
                sessions: vec![session(other, "s3", "third")],
                is_expanded: true,
            },
        ];
        state.active_work_dir = Some(attached.to_path_buf());

        let (work_dir, targets) = conversation_search_scope(&state).unwrap();
        assert_eq!(work_dir, attached);
        assert_eq!(
            targets.iter().map(|t| t.session_id.as_str()).collect::<Vec<_>>(),
            ["s1", "s2"]
        );
        assert_eq!(targets[0].title, "first");
        assert_eq!(targets[0].git_branch.as_deref(), Some("main"));

        state.active_work_dir = None;
        assert!(conversation_search_scope(&state).is_none());
    }

    #[test]
    fn footer_states_are_explicit_not_silent() {
        assert_eq!(
            search_empty_text(true, false),
            "Type at least 2 characters to search saved conversations"
        );
        assert_eq!(search_empty_text(false, true), "Searching saved conversations…");
        assert_eq!(search_empty_text(false, false), "No matching conversations");

        let mut report = ConversationSearchReport {
            scanned: 12,
            total: 12,
            ..ConversationSearchReport::default()
        };
        assert!(search_footer_text(12, 12, Some(&report), false, false)
            .contains("no matches"));
        report.scanned = 10;
        report.partial.push("2 saved conversation(s) could not be read".into());
        let footer = search_footer_text(10, 12, Some(&report), false, false);
        assert!(footer.contains("10 of 12"));
        assert!(footer.contains("could not be read"));
        report.limited = true;
        assert!(search_footer_text(10, 12, Some(&report), false, false)
            .contains("narrow your query"));
        assert!(search_footer_text(3, 12, None, true, false).contains("3 of 12"));
    }
}
