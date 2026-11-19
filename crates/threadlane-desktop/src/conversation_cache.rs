//! Render-time projections, separate from the authoritative conversation state.

use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use crate::{AppState, Line, When};

// Bound retention when navigating through many long conversations.
const RETAINED_CONVERSATIONS: usize = 4;

#[derive(Debug)]
struct Projection {
    space: String,
    project: String,
    day: i64,
    offset: i64,
    lines: Arc<[Line]>,
}

/// Desktop state with reusable ordered conversation rows.
///
/// Read-only renders reuse the same allocation. Every mutable borrow invalidates
/// the projections *before* handing out state, so feed/history merges, optimistic
/// sends, decisions, disclosure toggles, unread state and future mutators cannot
/// leave stale rows. Invalidation is deliberately conservative across projects.
/// The underlying state remains the sole authority; this does not cache elements,
/// alter row identities or change scrolling/focus/pagination behavior.
#[derive(Debug)]
pub struct ConversationState {
    state: AppState,
    projections: RefCell<Vec<Projection>>,
}

impl From<AppState> for ConversationState {
    fn from(state: AppState) -> Self {
        Self {
            state,
            projections: RefCell::new(Vec::new()),
        }
    }
}

impl Deref for ConversationState {
    type Target = AppState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl DerefMut for ConversationState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.projections.get_mut().clear();
        &mut self.state
    }
}

impl ConversationState {
    /// Ordered rows for one conversation. Time labels depend on the local date
    /// and offset, not the frame's exact timestamp (`conversation` uses day/time,
    /// never relative durations). Midnight and timezone changes rebuild them.
    #[must_use]
    pub fn conversation(&self, space: &str, project: &str, when: &When) -> Arc<[Line]> {
        let day =
            (when.now.as_millis().div_euclid(60_000) + when.offset_minutes).div_euclid(24 * 60);
        let mut projections = self.projections.borrow_mut();
        if let Some(cached) = projections.iter().find(|cached| {
            cached.space == space
                && cached.project == project
                && cached.day == day
                && cached.offset == when.offset_minutes
        }) {
            return Arc::clone(&cached.lines);
        }
        let lines: Arc<[Line]> = self.state.conversation(space, project, when).into();
        projections.retain(|cached| cached.space != space || cached.project != project);
        if projections.len() == RETAINED_CONVERSATIONS {
            projections.remove(0);
        }
        projections.push(Projection {
            space: space.to_owned(),
            project: project.to_owned(),
            day,
            offset: when.offset_minutes,
            lines: Arc::clone(&lines),
        });
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_core::ids::EventId;
    use threadlane_core::time::Timestamp;

    fn when(ms: i64, offset_minutes: i64) -> When {
        When {
            now: Timestamp::from_millis(ms),
            offset_minutes,
        }
    }

    #[test]
    fn renders_reuse_rows_but_mutable_access_and_clock_changes_rebuild() {
        let mut state = ConversationState::from(AppState::paired(["work"]));
        state.sent(
            "work",
            "api",
            "hello",
            EventId(1),
            Timestamp::from_millis(0),
        );
        let first = state.conversation("work", "api", &when(0, 0));
        assert!(Arc::ptr_eq(
            &first,
            &state.conversation("work", "api", &when(60_000, 0))
        ));
        assert_eq!(
            &*first,
            state.state.conversation("work", "api", &when(0, 0))
        );
        state.sent("work", "api", "new", EventId(2), Timestamp::from_millis(1));
        let updated = state.conversation("work", "api", &when(0, 0));
        assert!(!Arc::ptr_eq(&first, &updated));
        assert_eq!(
            &*updated,
            state.state.conversation("work", "api", &when(0, 0))
        );
        // Existing readers keep their immutable snapshot, without blocking a mutation.
        assert_eq!(first.len() + 1, updated.len());
        let tomorrow = state.conversation("work", "api", &when(86_400_000, 0));
        assert_eq!(tomorrow[0], Line::Day("Yesterday".to_owned()));
        let shifted = state.conversation("work", "api", &when(86_400_000, -60));
        assert!(!Arc::ptr_eq(&tomorrow, &shifted));
        assert_eq!(
            &*shifted,
            state
                .state
                .conversation("work", "api", &when(86_400_000, -60))
        );
        state.visit_conversation("work", "api");
        assert!(!Arc::ptr_eq(
            &shifted,
            &state.conversation("work", "api", &when(86_400_000, -60))
        ));
    }

    #[test]
    fn conversations_are_isolated_and_retention_is_bounded() {
        let state = ConversationState::from(AppState::default());
        let first = state.conversation("work", "one", &when(0, 0));
        let other = state.conversation("home", "one", &when(0, 0));
        assert!(!Arc::ptr_eq(&first, &other));
        assert!(Arc::ptr_eq(
            &first,
            &state.conversation("work", "one", &when(0, 0))
        ));
        for project in ["two", "three", "four", "five"] {
            let _rows = state.conversation("work", project, &when(0, 0));
        }
        assert_eq!(state.projections.borrow().len(), RETAINED_CONVERSATIONS);
        assert!(!Arc::ptr_eq(
            &first,
            &state.conversation("work", "one", &when(0, 0))
        ));
    }

    #[test]
    fn long_chat_repeated_renders_do_not_reproject_or_clone_payloads() {
        let mut state = ConversationState::from(AppState::default());
        for id in 0..10_000 {
            state.sent(
                "work",
                "api",
                &"synthetic long message ".repeat(50),
                EventId(id),
                Timestamp::from_millis(id),
            );
        }
        let first = state.conversation("work", "api", &when(0, 0));
        assert_eq!(first.len(), 10_001);
        // Deterministic work measurement: 100 render reads retain the very same
        // 10,001-row allocation rather than sorting/formatting/cloning it again.
        for frame in 0..100 {
            assert!(Arc::ptr_eq(
                &first,
                &state.conversation("work", "api", &when(frame, 0))
            ));
        }
        assert_eq!(
            &*first,
            state.state.conversation("work", "api", &when(0, 0))
        );
    }
}
