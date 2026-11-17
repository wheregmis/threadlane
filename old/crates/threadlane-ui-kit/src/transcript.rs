//! Transcript row projection lives in `threadlane_protocol::transcript` so
//! GPUI-free consumers (the daemon's saved-conversation scan) share the exact
//! matching semantics used by the find strip. Everything except the GPUI
//! [`TranscriptState`] is re-exported verbatim, so `ui_kit::transcript::*`
//! paths keep working unchanged.
pub use threadlane_protocol::transcript::*;

use threadlane_protocol::daemon::ChatMessageInfo;

use gpui::{FollowMode, ListAlignment, ListState, Window};
use std::sync::Arc;

/// Retained virtual transcript behavior shared by desktop and iOS.
/// Message rendering and native actions are supplied by the owning screen.
pub struct TranscriptState {
    pub list: ListState,
    pub messages: Arc<Vec<ChatMessageInfo>>,
    pub rows: Vec<TranscriptRow>,
    pub generating: bool,
}
impl TranscriptState {
    pub fn new(window: &Window) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, window.rem_size() * 37.5);
        list.set_follow_mode(FollowMode::Tail);
        Self {
            list,
            messages: Arc::new(Vec::new()),
            rows: Vec::new(),
            generating: false,
        }
    }
    pub fn sync(
        &mut self,
        messages: Arc<Vec<ChatMessageInfo>>,
        generating: bool,
        session_changed: bool,
        preserve_reading: bool,
    ) {
        if !session_changed
            && Arc::ptr_eq(&messages, &self.messages)
            && generating == self.generating
        {
            return;
        }

        let old_message_count = self.messages.len();
        let old_row_count = self.rows.len();
        let new_message_count = messages.len();

        if !session_changed
            && new_message_count == old_message_count
            && generating == self.generating
            // Hydration and streaming can change row topology without changing
            // message count (an activity-only reply gains visible content).
            && messages.iter().zip(self.messages.iter()).all(|(new, old)| {
                is_activity_only(new) == is_activity_only(old)
                    && is_queued_message(new, generating) == is_queued_message(old, generating)
            })
        {
            let last_changed =
                messages
                    .last()
                    .zip(self.messages.last())
                    .is_some_and(|(new, old)| {
                        new.id != old.id
                            || new.content.len() != old.content.len()
                            || new.reasoning_content.as_ref().map(String::len)
                                != old.reasoning_content.as_ref().map(String::len)
                            || new.tool_activities.len() != old.tool_activities.len()
                            || new.streaming != old.streaming
                    });
            self.messages = messages;
            if last_changed {
                self.list
                    .remeasure_items(old_row_count.saturating_sub(1)..old_row_count);
            } else {
                self.list.remeasure();
            }
            return;
        }

        let new_rows = build_transcript_rows(&messages, generating);
        let new_row_count = new_rows.len();
        // Only splice the Working row when the remaining rows are unchanged.
        // Generation toggles also filter queued messages, which requires a reset.
        let working_changed = !session_changed
            && new_message_count == old_message_count
            && generating != self.generating
            && new_rows
                .strip_suffix(&[TranscriptRow::Working])
                .unwrap_or(&new_rows)
                == self
                    .rows
                    .strip_suffix(&[TranscriptRow::Working])
                    .unwrap_or(&self.rows);
        let prepended = !session_changed
            && new_message_count > old_message_count
            && self
                .messages
                .first()
                .zip(messages.get(new_message_count - old_message_count))
                .is_some_and(|(old, new)| old.id == new.id)
            && self
                .messages
                .last()
                .zip(messages.last())
                .is_some_and(|(old, new)| old.id == new.id)
            && new_row_count >= old_row_count;
        let appended = !session_changed
            && new_message_count > old_message_count
            && self
                .messages
                .first()
                .zip(messages.first())
                .is_some_and(|(old, new)| old.id == new.id)
            && self
                .messages
                .last()
                .zip(messages.get(old_message_count.saturating_sub(1)))
                .is_some_and(|(old, new)| old.id == new.id)
            && new_row_count >= old_row_count;

        self.messages = messages;
        self.rows = new_rows;
        self.generating = generating;
        if working_changed && generating {
            self.list.splice(old_row_count..old_row_count, 1);
        } else if working_changed {
            self.list.splice(new_row_count..old_row_count, 0);
        } else if prepended {
            self.list.splice(0..0, new_row_count - old_row_count);
        } else if appended {
            self.list
                .splice(old_row_count..old_row_count, new_row_count - old_row_count);
        } else {
            // Reconciliation can replace every optimistic ID. Keep a find
            // reader's viewport, without guessing a new selected message.
            let reading_position =
                (preserve_reading && !session_changed && !self.list.is_following_tail())
                    .then(|| self.list.logical_scroll_top());
            self.list.reset(new_row_count);
            if let Some(mut position) = reading_position {
                position.item_ix = position.item_ix.min(new_row_count.saturating_sub(1));
                self.list.scroll_to(position);
            }
        }
        if session_changed {
            self.list.set_follow_mode(FollowMode::Tail);
        }
    }
}
