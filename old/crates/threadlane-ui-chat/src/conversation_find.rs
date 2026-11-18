use super::*;

pub use threadlane_ui_kit::{
    init_conversation_find, CloseConversationFind, FindInConversation,
    NextConversationMatch, PreviousConversationMatch,
};

/// One-shot handoff from project conversation search: select this session,
/// then seed the find strip with `query` once the destination transcript has
/// hydrated. Owned by the view — the pending request is dropped as soon as
/// the active session diverges from `work_dir`/`session_id` or the user
/// leaves the Chat tab.
#[derive(Clone, Debug)]
pub struct ConversationFindHandoff {
    pub work_dir: PathBuf,
    pub session_id: String,
    pub query: String,
}


impl ChatListView {
    /// Find needs a conversation: a new task has nothing to search yet.
    pub(super) fn can_find_in_conversation(&self, cx: &App) -> bool {
        self.current_tab == CentralTab::Chat && !self.model.read(cx).is_new_task
    }

    pub(super) fn open_conversation_find(
        &mut self,
        _: &FindInConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_find_in_conversation(cx) || window.has_active_dialog(cx) {
            cx.propagate();
            return;
        }
        // Find supersedes the outline: clear any outline jump marker.
        self.outline_open = false;
        self.outline_selected_id = None;
        if !self.find_open {
            self.find_previous_focus = window.focused(cx);
            let state = self.model.read(cx);
            self.find_session = (
                state.active_work_dir.clone(),
                state.active_session_id.clone(),
            );
            self.find_open = true;
            self.find_query.clear();
            self.find_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.refresh_conversation_find(true, cx);
        }
        self.find_input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.stop_propagation();
        cx.notify();
    }

    /// Queue a search result for the named session. Applies immediately when
    /// that session is already active and hydrated; otherwise it waits for
    /// the `SelectSession` dispatch and its hydration to settle (the model
    /// observer re-runs `progress_find_handoff` on every change). Only the
    /// ordinary selection path runs — no unsnooze, worktree recreation, or
    /// runtime start beyond what hydration already does.
    pub fn begin_conversation_find_handoff(
        &mut self,
        handoff: ConversationFindHandoff,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_find_handoff = Some(handoff);
        self.progress_find_handoff(window, cx);
    }

    pub(super) fn progress_find_handoff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(handoff) = self.pending_find_handoff.clone() else {
            return;
        };
        let (active_session, still_loading) = {
            let state = self.model.read(cx);
            (
                (
                    state.active_work_dir.clone(),
                    state.active_session_id.clone(),
                ),
                state.active_session_is_loading(),
            )
        };
        if active_session != (Some(handoff.work_dir.clone()), Some(handoff.session_id.clone())) {
            // The user navigated elsewhere before the handoff landed; drop it.
            self.pending_find_handoff = None;
            return;
        }
        if still_loading || self.current_tab != CentralTab::Chat {
            // Wait for hydration; a tab switch cancels via
            // `clear_conversation_find`.
            return;
        }
        self.pending_find_handoff = None;
        self.apply_find_handoff(handoff.query, window, cx);
    }

    /// Opens the find strip pre-seeded with `query` against the now-hydrated
    /// transcript. `InputState::set_value` does not emit `Change`, so the
    /// query is stored directly and an explicit refresh is scheduled — its
    /// completion navigates to the first current match, recomputing matches
    /// rather than trusting the search row's stale index.
    fn apply_find_handoff(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.outline_open = false;
        self.outline_selected_id = None;
        if !self.find_open {
            self.find_previous_focus = window.focused(cx);
            self.find_open = true;
        }
        let state = self.model.read(cx);
        self.find_session = (
            state.active_work_dir.clone(),
            state.active_session_id.clone(),
        );
        self.find_query = query.clone();
        self.find_results.clear();
        self.find_selected = None;
        self.find_input.update(cx, |input, cx| {
            input.set_value(query, window, cx);
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.refresh_conversation_find(true, cx);
        cx.notify();
    }

    pub(super) fn clear_conversation_find(&mut self) {
        self.find_open = false;
        self.pending_find_handoff = None;
        self.find_generation += 1;
        self.find_task = None;
        self.find_results.clear();
        self.find_selected = None;
        self.find_source = None;
        self.find_pending = false;
        self.find_previous_focus = None;
        self.find_query.clear();
    }

    pub(super) fn close_conversation_find(
        &mut self,
        _: &CloseConversationFind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.find_open || window.has_active_dialog(cx) {
            cx.propagate();
            return;
        }
        let focus = self.find_previous_focus.take();
        self.clear_conversation_find();
        if let Some(focus) = focus {
            window.focus(&focus, cx);
        }
        cx.stop_propagation();
        cx.notify();
    }

    // Called by input/model subscriptions, never render. Arc identity catches
    // same-length replacements without scanning on unrelated model notifications.
    pub(super) fn refresh_conversation_find(
        &mut self,
        explicit_query: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.find_open {
            return;
        }
        let state = self.model.read(cx);
        let session = (
            state.active_work_dir.clone(),
            state.active_session_id.clone(),
        );
        if session != self.find_session || self.current_tab != CentralTab::Chat {
            self.clear_conversation_find();
            cx.notify();
            return;
        }
        let source = state.messages.clone();
        let generating = state.is_generating;
        let loading = state.active_session_is_loading();
        if !explicit_query
            && self
                .find_source
                .as_ref()
                .is_some_and(|(old, old_generating, old_loading)| {
                    Arc::ptr_eq(old, &source)
                        && *old_generating == generating
                        && *old_loading == loading
                })
        {
            return;
        }
        // Coalesce streamed snapshots into the retained task rather than
        // restarting its timer indefinitely on a busy turn. Query edits still
        // cancel immediately, so Enter cannot act on the previous query.
        if !explicit_query && self.find_pending && self.find_task.is_some() && !loading {
            return;
        }
        self.find_generation += 1;
        let generation = self.find_generation;
        self.find_task = None;
        self.find_source = Some((source.clone(), generating, loading));
        if explicit_query || loading {
            self.find_results.clear();
        }
        self.find_pending = !self.find_query.trim().is_empty() && !loading;
        cx.notify();
        if !self.find_pending {
            return;
        }
        let query = self.find_query.clone();
        self.find_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let Ok(Some((source, generating, explicit_query))) = this.update(cx, |this, cx| {
                if !this.find_open || this.find_generation != generation {
                    return None;
                }
                let state = this.model.read(cx);
                let latest = state.messages.clone();
                // A source refresh is not an explicit navigation request.
                let explicit_query = explicit_query && Arc::ptr_eq(&source, &latest);
                let generating = state.is_generating;
                this.find_source = Some((
                    latest.clone(),
                    generating,
                    state.active_session_is_loading(),
                ));
                Some((latest, generating, explicit_query))
            }) else {
                return;
            };
            let scan_source = source.clone();
            let results = cx
                .background_executor()
                .spawn(async move { find_conversation_messages(&scan_source, generating, &query) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let state = this.model.read(cx);
                if !this.find_open
                    || this.find_generation != generation
                    || this.find_session != session
                    || (
                        state.active_work_dir.clone(),
                        state.active_session_id.clone(),
                    ) != session
                {
                    return;
                }
                if state.active_session_is_loading() {
                    this.find_pending = false;
                    this.find_task = None;
                    this.refresh_conversation_find(false, cx);
                    return;
                }
                let stale =
                    !Arc::ptr_eq(&state.messages, &source) || state.is_generating != generating;
                this.find_pending = false;
                this.find_task = None;
                this.find_results = results;
                // A reconciled ID is not the same message, even if text matches.
                if !this
                    .find_results
                    .iter()
                    .any(|hit| Some(&hit.message_id) == this.find_selected.as_ref())
                {
                    this.find_selected = None;
                }
                if explicit_query && !stale {
                    this.navigate_conversation_find(false, cx);
                }
                if stale {
                    this.refresh_conversation_find(false, cx);
                }
                cx.notify();
            });
        }));
    }

    fn find_ready(&self, cx: &App) -> bool {
        let state = self.model.read(cx);
        self.find_open
            && !self.find_results.is_empty()
            && !state.active_session_is_loading()
            && !state
                .session_status
                .as_ref()
                .is_some_and(|status| status.starts_with("Could not load session:"))
            && self.find_session
                == (
                    state.active_work_dir.clone(),
                    state.active_session_id.clone(),
                )
    }

    pub(super) fn navigate_conversation_find(&mut self, previous: bool, cx: &mut Context<Self>) {
        if !self.find_ready(cx) {
            return;
        }
        let selected = self
            .find_results
            .iter()
            .position(|hit| Some(&hit.message_id) == self.find_selected.as_ref());
        let Some(index) = next_find_match(selected, self.find_results.len(), previous) else {
            return;
        };
        let state = self.model.read(cx);
        let messages = state.messages.clone();
        let generating = state.is_generating;
        self.sync_transcript_rows(messages, generating, false);
        let hit = &self.find_results[index];
        // Results may precede the latest streamed snapshot. Validate row identity
        // before navigating so a replaced message can never be selected.
        let row = hit.row_index;
        if !matches!(self.transcript.rows.get(row),
            Some(TranscriptRow::Message(index)) if self.transcript.messages[*index].id == hit.message_id)
        {
            return;
        }
        self.find_selected = Some(hit.message_id.clone());
        self.initial_scroll_frames = 0;
        self.transcript.list.pause_following_tail();
        self.transcript.list.scroll_to(ListOffset {
            item_ix: row,
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    pub(super) fn next_conversation_match(
        &mut self,
        _: &NextConversationMatch,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_conversation_find(false, cx);
        cx.stop_propagation();
    }

    pub(super) fn previous_conversation_match(
        &mut self,
        _: &PreviousConversationMatch,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_conversation_find(true, cx);
        cx.stop_propagation();
    }

    pub(super) fn conversation_find_status(&self, cx: &App) -> String {
        let state = self.model.read(cx);
        let selected = self
            .find_results
            .iter()
            .position(|hit| Some(&hit.message_id) == self.find_selected.as_ref());
        if state.active_session_is_loading() {
            "Loading conversation…".to_owned()
        } else if let Some(error) = state
            .session_status
            .as_ref()
            .filter(|status| status.starts_with("Could not load session:"))
        {
            error.clone()
        } else {
            threadlane_ui_kit::conversation_find_status(
                &self.find_query, self.find_pending, self.find_results.len(), selected,
            )
        }
    }

    pub(super) fn render_conversation_find(&self, cx: &mut Context<Self>) -> AnyElement {
        let status = self.conversation_find_status(cx);
        let selected = self
            .find_results
            .iter()
            .position(|hit| Some(&hit.message_id) == self.find_selected.as_ref());
        threadlane_ui_kit::ConversationFindStrip::new(
            &self.find_input, status, self.find_ready(cx),
        )
        .leading_inset(self.header_left_padding)
        .excerpt(selected.map(|index| self.find_results[index].excerpt.clone().into()))
        .render(cx.listener(|this, action, window, cx| {
            use threadlane_ui_kit::ConversationFindAction;
            match action {
                ConversationFindAction::Previous => this.navigate_conversation_find(true, cx),
                ConversationFindAction::Next => this.navigate_conversation_find(false, cx),
                ConversationFindAction::Close => this.close_conversation_find(&CloseConversationFind, window, cx),
            }
        }), cx)
        .into_any_element()
    }
}
