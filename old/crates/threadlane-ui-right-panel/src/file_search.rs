//! Ephemeral saved-file navigation. No query or snippet enters AppState or chat.
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Disableable, Selectable, Sizable, WindowExt};
use std::{path::PathBuf, sync::Arc, time::Duration};
use threadlane_client::DaemonClient;
use threadlane_protocol::repo::FileSearchResult;
use threadlane_ui_state::{project_io, AppState, RequestedEditorTarget};

pub fn open(model: Entity<AppState>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| FileSearch::new(model, window, cx));
    let input = view.read(cx).input.clone();
    window.open_dialog(cx, move |dialog, window, _| {
        let close = view.clone();
        dialog
            .title("Find in files…")
            .w((window.rem_size() * 42.0).min(window.viewport_size().width - window.rem_size() * 2.0))
            .child(view.clone())
            .on_close(move |_, _, cx| {
                close.update(cx, |view, cx| {
                    view.closed = true;
                    view.task = None;
                    view.rows = FileSearchResult::default();
                    cx.notify();
                })
            })
    });
    input.update(cx, |input, cx| input.focus(window, cx));
}

struct FileSearch {
    model: Entity<AppState>,
    client: Arc<dyn DaemonClient>,
    root: Option<PathBuf>,
    epoch: u64,
    project: Option<PathBuf>,
    session: Option<String>,
    input: Entity<InputState>,
    query: String,
    completed: Option<String>,
    rows: FileSearchResult,
    status: String,
    selected: usize,
    scroll: ScrollHandle,
    generation: u64,
    closed: bool,
    invalidated: bool,
    opening: bool,
    task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl FileSearch {
    fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = model.read(cx);
        let root = state.active_git_work_dir();
        let client = state.daemon_client.clone();
        let epoch = client.file_search_connection_epoch();
        let project = state.client.active_work_dir.clone();
        let session = state.client.active_session_id.clone();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Enter text to search"));
        let change = cx.subscribe_in(&input, window, |this, input, event, window, cx| {
            match event {
                InputEvent::Change => {
                    this.query = input.read(cx).value().to_string();
                    this.generation += 1;
                    this.completed = None;
                    this.rows = FileSearchResult::default();
                    this.selected = 0;
                    this.schedule(cx);
                }
                InputEvent::PressEnter { .. } => this.activate(this.selected, window, cx),
                _ => {}
            }
            cx.notify();
        });
        let observe = cx.observe(&model, |this, _, cx| {
            if !this.scope_matches(cx) {
                this.invalidated = true;
                this.completed = None;
                this.status = "Checkout or daemon changed. Close and reopen Find in files.".into();
                this.rows = FileSearchResult::default();
                cx.notify();
            }
        });
        let status = if project.is_none() {
            "No project selected"
        } else if root.is_none() {
            "Worktree unavailable or preparing; try again when ready"
        } else if !client.is_connected() {
            "Daemon disconnected. Reconnect and retry."
        } else if !client.supports_file_search() {
            "Unsupported daemon version. Update to protocol v6 or newer."
        } else {
            "Enter text to search"
        }
        .into();
        Self {
            model,
            client,
            root,
            epoch,
            project,
            session,
            input,
            query: String::new(),
            completed: None,
            rows: FileSearchResult::default(),
            status,
            selected: 0,
            scroll: ScrollHandle::new(),
            generation: 0,
            closed: false,
            invalidated: false,
            opening: false,
            task: None,
            _subscriptions: vec![change, observe],
        }
    }

    fn scope_matches(&self, cx: &App) -> bool {
        let state = self.model.read(cx);
        !self.closed
            && !self.invalidated
            && Arc::ptr_eq(&self.client, &state.daemon_client)
            && self.project == state.client.active_work_dir
            && self.session == state.client.active_session_id
            && self.root == state.active_git_work_dir()
            && self.epoch == self.client.file_search_connection_epoch()
    }

    fn ready(&self, cx: &App) -> bool {
        self.scope_matches(cx)
            && self.client.supports_file_search()
            && !self.opening
            && self.completed.as_deref() == Some(self.query.as_str())
            && !self.query.is_empty()
    }

    fn schedule(&mut self, cx: &mut Context<Self>) {
        if !self.scope_matches(cx) || self.root.is_none() {
            return;
        }
        if self.query.is_empty() {
            self.status = "Enter text to search".into();
            return;
        }
        if self.query.contains(['\n', '\r']) || self.query.len() > 4096 {
            self.status = "Enter one line, at most 4096 UTF-8 bytes. Whitespace is literal.".into();
            return;
        }
        self.status = "Searching…".into();
        if self.task.is_some() {
            return;
        }
        // One request per dialog at a time. Edits during a scan coalesce into its successor.
        self.task = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let Ok(Some((client, root, query, generation))) = this.update(cx, |this, cx| {
                if !this.scope_matches(cx)
                    || this.query.is_empty()
                    || this.query.contains(['\n', '\r'])
                    || this.query.len() > 4096
                {
                    this.task = None;
                    return None;
                }
                Some((
                    this.client.clone(),
                    this.root.clone()?,
                    this.query.clone(),
                    this.generation,
                ))
            }) else {
                break;
            };
            let result = match threadlane_ui_state::chat::executor() {
                Ok(runtime) => runtime
                    .spawn(async move { project_io::search_files(&client, &root, query).await })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string())),
                Err(error) => Err(error),
            };
            let again = this
                .update(cx, |this, cx| this.complete(generation, result, cx))
                .unwrap_or(false);
            if !again {
                break;
            }
        }));
    }

    /// Apply only the currently displayed query/scope; stale completions coalesce.
    fn complete(
        &mut self,
        generation: u64,
        result: Result<FileSearchResult, String>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.scope_matches(cx) {
            self.task = None;
            return false;
        }
        if generation != self.generation {
            return true;
        }
        match result {
            Ok(rows) => {
                self.status = if rows.partial.is_empty() {
                    if rows.matches.is_empty() {
                        "No matches".into()
                    } else {
                        format!("{} matching lines", rows.matches.len())
                    }
                } else {
                    format!(
                        "Partial: {} matching lines. {}",
                        rows.matches.len(),
                        rows.partial.join("; ")
                    )
                };
                self.rows = rows;
                self.completed = Some(self.query.clone());
            }
            Err(error) => {
                self.status = format!("Search failed: {error}");
                self.completed = None;
            }
        }
        self.task = None;
        cx.notify();
        false
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ready(cx) {
            return;
        }
        let Some(row) = self.rows.matches.get(index).cloned() else {
            return;
        };
        let Some(root) = self.root.clone() else {
            return;
        };
        let generation = self.generation;
        let client = self.client.clone();
        let Ok(runtime) = threadlane_ui_state::chat::executor() else {
            return;
        };
        let target_root = root.clone();
        let path = row.path.clone();
        self.opening = true;
        self.status = "Checking saved file…".into();
        let task = runtime.spawn(async move {
            project_io::validate_search_target(&client, &target_root, path).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update_in(cx, |this, window, cx| {
                this.opening = false;
                if generation != this.generation || !this.scope_matches(cx) {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(()) => {
                        let opened = this.model.update(cx, |state, cx| {
                            if state.requested_editor_target.is_some() {
                                return false;
                            }
                            state.requested_editor_target =
                                Some(RequestedEditorTarget::SearchFile {
                                    project: root,
                                    path: row.path,
                                    line: row.line,
                                    owner_project: this.project.clone(),
                                    owner_session: this.session.clone(),
                                    daemon_identity: Arc::as_ptr(&this.client) as *const ()
                                        as usize,
                                    connection_epoch: this.epoch,
                                });
                            cx.notify();
                            true
                        });
                        if opened {
                            this.closed = true;
                            window.close_dialog(cx);
                        } else {
                            this.status = "Another editor request is pending. Try again.".into();
                        }
                    }
                    Err(error) => {
                        this.completed = None;
                        this.status =
                            format!("Cannot open file: {error}. Refresh to search again.");
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for FileSearch {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.scope_matches(cx) && !self.closed {
            self.invalidated = true;
            self.completed = None;
            self.status = "Checkout or daemon changed. Close and reopen Find in files.".into();
        }
        if !self.client.is_connected() {
            self.status = "Daemon disconnected. Reconnect and reopen Find in files.".into();
        }
        let ready = self.ready(cx);
        let theme = cx.theme().colors;
        let root = self
            .root
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "No active checkout".into());
        div().flex().flex_col().gap_2().min_w_0()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if !this.ready(cx) || this.rows.matches.is_empty() { return; }
                match event.keystroke.key.as_str() {
                    "down" => this.selected = (this.selected + 1).min(this.rows.matches.len() - 1),
                    "up" => this.selected = this.selected.saturating_sub(1),
                    _ => return,
                }
                this.scroll.scroll_to_item(this.selected);
                cx.stop_propagation();
                cx.notify();
            }))
            .child(div().text_xs().child(root))
            .child(div().text_xs().text_color(theme.muted_foreground).child("Saved files · Literal · Case-sensitive"))
            .child(Input::new(&self.input).aria_label("Find text in saved files"))
            .child(div().id("file-search-status").role(Role::Status).a11y_synthetic_children(|builder| builder.parent_node().set_live(gpui::accesskit::Live::Polite)).aria_label(self.status.clone()).text_sm().child(self.status.clone()))
            .child(Button::new("file-search-refresh").label("Refresh / Retry").ghost().small()
                .disabled(self.opening || self.task.is_some() || !self.scope_matches(cx))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.generation += 1; this.completed = None; this.schedule(cx); cx.notify();
                })))
            .child(div().id("file-search-results").h((window.viewport_size().height * 0.4).min(window.rem_size() * 20.0)).overflow_y_scroll().track_scroll(&self.scroll)
                .children(self.rows.matches.iter().enumerate().map(|(index, row)| {
                    let label = format!("{}, line {}, {}", row.path, row.line, row.snippet);
                    let snippet = &row.snippet;
                    let start = row.match_start.min(snippet.len());
                    let end = row.match_end.min(snippet.len());
                    let highlighted = if start <= end && snippet.is_char_boundary(start) && snippet.is_char_boundary(end) {
                        format!("{}【{}】{}", &snippet[..start], &snippet[start..end], &snippet[end..])
                    } else { snippet.clone() };
                    Button::new(SharedString::from(format!("{}:{}", row.path, row.line)))
                        .accessibility_label(label).w_full().h_auto().ghost()
                        .child(div().flex().flex_col().items_start().w_full().min_w_0()
                            .child(div().text_xs().child(format!("{} · line {}{}", row.path, row.line, if index == self.selected { " · Selected" } else { "" })))
                            .child(div().text_sm().child(highlighted)))
                        .disabled(!ready)
                        .selected(index == self.selected)
                        .tab_stop(index == self.selected)
                        .on_click(cx.listener(move |this, _, window, cx| this.activate(index, window, cx)))
                })))
            .child(div().text_xs().text_color(theme.muted_foreground).child("Search reads saved files. Unsaved editor buffers are preserved; their line numbers may differ."))
    }
}

#[cfg(test)]
mod tests {
    use super::{open, FileSearch};
    use gpui::{
        div, prelude::*, AppContext, Context, Entity, FocusHandle, IntoElement, Render,
        TestAppContext, Window,
    };
    use gpui_component::Root;
    use threadlane_protocol::repo::{FileSearchMatch, FileSearchResult};
    use threadlane_ui_state::AppState;

    struct Host {
        focus: FocusHandle,
    }
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("search-test-trigger")
                .role(gpui::Role::Button)
                .track_focus(&self.focus)
        }
    }

    #[gpui::test]
    fn file_search_escape_restores_trigger_focus(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| AppState::default());
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            *capture.borrow_mut() = Some(focus.clone());
            Root::new(cx.new(|_| Host { focus }), window, cx)
        });
        let focus = captured.borrow_mut().take().unwrap();
        cx.update(|window, cx| open(model, window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.update(|window, cx| {
            use gpui_component::WindowExt;
            assert!(!window.has_active_dialog(cx));
            assert!(focus.is_focused(window));
        });
    }

    #[gpui::test]
    fn file_search_input_invalidates_rows_before_debounce_and_scope_switch(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let model = cx.new(|_| {
            let mut state = AppState::default();
            state.client.active_work_dir = Some("/daemon-only/checkout".into());
            state
        });
        let model_copy = model.clone();
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None::<Entity<FileSearch>>));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| FileSearch::new(model_copy, window, cx));
            *capture.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = captured.borrow_mut().take().unwrap();
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.query = "needle".into();
                view.completed = Some(view.query.clone());
                view.input.update(cx, |input, cx| {
                    input.set_value("needle", window, cx);
                    input.focus(window, cx);
                });
                view.rows = FileSearchResult {
                    matches: vec![FileSearchMatch {
                        path: "a".into(),
                        line: 1,
                        snippet: "needle".into(),
                        match_start: 0,
                        match_end: 6,
                    }],
                    partial: vec![],
                };
                assert!(view.ready(cx));
            })
        });
        view.update(cx, |view, cx| {
            view.rows.matches.push(FileSearchMatch { path: "b".into(), line: 2, snippet: "needle".into(), match_start: 0, match_end: 6 });
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("down");
        view.read_with(cx, |view, _| assert_eq!(view.selected, 1));
        cx.simulate_keystrokes("up");
        view.read_with(cx, |view, _| assert_eq!(view.selected, 0));
        cx.simulate_input("x");
        view.read_with(cx, |view, cx| {
            assert!(!view.ready(cx));
            assert!(view.completed.is_none());
            assert!(view.rows.matches.is_empty());
        });
        cx.update(|window, cx| view.update(cx, |view, cx| view.activate(0, window, cx)));
        model.read_with(cx, |model, _| {
            assert!(model.requested_editor_target.is_none())
        });
        view.update(cx, |view, cx| {
            let stale = FileSearchResult { matches: vec![FileSearchMatch { path: "stale".into(), line: 9, snippet: "old".into(), match_start: 0, match_end: 3 }], partial: vec![] };
            assert!(view.complete(0, Ok(stale), cx));
            assert!(view.rows.matches.is_empty());
            assert!(!view.ready(cx));
            let generation = view.generation;
            assert!(!view.complete(generation, Ok(FileSearchResult::default()), cx));
            assert!(view.ready(cx));
        });
        model.update(cx, |state, cx| {
            state.client.active_session_id = Some("other".into());
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, cx| {
            assert!(view.invalidated);
            assert!(!view.ready(cx));
        });
    }
}
