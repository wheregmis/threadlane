//! Shared desktop geometry, typography, and status colors.

use std::sync::{Arc, LazyLock};

use gpui_kit::component::{Theme, scroll::Scrollbar};
use gpui_kit::{FontFeatures, Hsla, Pixels, Rems, px, rems};
use threadlane_core::model::MachineStatus;

/// Xxs in logical pixels.
pub(super) const XXS: Pixels = px(2.);
/// Xs in logical pixels.
pub(super) const XS: Pixels = px(4.);
/// Sm in logical pixels.
pub(super) const SM: Pixels = px(8.);
/// Md in logical pixels.
pub(super) const MD: Pixels = px(12.);
/// Lg in logical pixels.
pub(super) const LG: Pixels = px(16.);
/// Xl in logical pixels.
pub(super) const XL: Pixels = px(24.);
/// Xxl in logical pixels.
pub(super) const XXL: Pixels = px(32.);
/// Dot size in logical pixels.
pub(super) const DOT_SIZE: Pixels = px(6.);

/// Distance from the bottom that still counts as the latest position.
pub(super) const SCROLL_END_TOLERANCE: Pixels = px(4.);
/// Space for the overlay scrollbar's full track, including when hovered.
pub(super) const SCROLLBAR_GUTTER: Pixels = Scrollbar::width();
/// Visible rows before the composer starts scrolling internally.
pub(super) const COMPOSER_MAX_ROWS: usize = 8;
/// Field radius in logical pixels.
pub(super) const FIELD_RADIUS: Pixels = px(10.);
/// Row radius in logical pixels.
pub(super) const ROW_RADIUS: Pixels = px(8.);
/// Icon button radius in logical pixels.
pub(super) const ICON_BUTTON_RADIUS: Pixels = px(6.);
/// Badge radius in logical pixels.
pub(super) const BADGE_RADIUS: Pixels = px(5.);
/// Key radius in logical pixels.
pub(super) const KEY_RADIUS: Pixels = px(4.);

/// Type meta in rems (12px at 16px base).
pub(super) const TYPE_META: Rems = rems(0.75);
/// Type body in rems (14px at 16px base).
pub(super) const TYPE_BODY: Rems = rems(0.875);
/// Type section in rems (16px at 16px base).
pub(super) const TYPE_SECTION: Rems = rems(1.0);
/// Type emphasis in rems (17px at 16px base).
pub(super) const TYPE_EMPHASIS: Rems = rems(1.0625);
/// Type title in rems (20px at 16px base).
pub(super) const TYPE_TITLE: Rems = rems(1.25);

/// Control height in logical pixels.
pub(super) const CONTROL_HEIGHT: Pixels = px(32.);
/// Small control height in logical pixels.
pub(super) const SMALL_CONTROL_HEIGHT: Pixels = px(24.);
/// Row height in rems.
pub(super) const ROW_HEIGHT: Rems = rems(2.125);
/// Bar height in rems.
pub(super) const BAR_HEIGHT: Rems = rems(3.25);
/// Reading width in rems.
pub(super) const READING_WIDTH: Rems = rems(55.);

/// Sidebar default width in logical pixels.
pub(super) const SIDEBAR_WIDTH: Pixels = px(220.);
/// Sidebar minimum width in logical pixels.
pub(super) const SIDEBAR_MIN_WIDTH: Pixels = px(180.);
/// Sidebar maximum width in logical pixels.
pub(super) const SIDEBAR_MAX_WIDTH: Pixels = px(420.);
/// Minimum window width in logical pixels.
pub(super) const WINDOW_MIN_WIDTH: Pixels = px(720.);
/// Minimum window height in logical pixels.
pub(super) const WINDOW_MIN_HEIGHT: Pixels = px(520.);
/// Default window width in logical pixels.
pub(super) const WINDOW_DEFAULT_WIDTH: Pixels = px(1180.);
/// Default window height in logical pixels.
pub(super) const WINDOW_DEFAULT_HEIGHT: Pixels = px(780.);
/// Maximum height of a scrolling list inside a sidebar.
pub(super) const SIDEBAR_SECTION_MAX_HEIGHT: Rems = rems(17.5);
/// Task detail right-hand inspector rail width in rems.
pub(super) const TASK_INSPECTOR_WIDTH: Rems = rems(18.5);
/// Minimum width reserved for the task main column before moving the inspector to a sheet.
pub(super) const TASK_MAIN_MIN_WIDTH: Rems = rems(28.);
/// Inbox split queue list width in rems.
pub(super) const INBOX_LIST_WIDTH: Rems = rems(20.);
/// Minimum width reserved for the Inbox decision detail column in split layout.
pub(super) const INBOX_DETAIL_MIN_WIDTH: Rems = rems(32.);
/// Fixed key column width inside the task inspector's Details and Usage rows.
pub(super) const TASK_DETAIL_LABEL_WIDTH: Rems = rems(6.);
/// Compact tabular timestamp lane for same-day `HH:MM` timestamps on the task screen.
pub(super) const TASK_TIME_SHORT_WIDTH: Rems = rems(3.25);
/// Expanded tabular timestamp lane when any task timestamp includes a date.
pub(super) const TASK_TIME_LONG_WIDTH: Rems = rems(7.25);
/// Board column min in logical pixels.
pub(super) const BOARD_COLUMN_MIN: Pixels = px(152.);
/// Timestamp width in rems.
pub(super) const TIMESTAMP_WIDTH: Rems = rems(6.875);
/// Form width in logical pixels (passed to Dialog::width).
pub(super) const FORM_WIDTH: Pixels = px(560.);
/// Compact form width in rems.
pub(super) const COMPACT_FORM_WIDTH: Rems = rems(26.25);
/// Rules width in rems.
pub(super) const RULES_WIDTH: Rems = rems(20.);
/// Memory list width in rems.
pub(super) const MEMORY_LIST_WIDTH: Rems = rems(18.75);
/// Label width in rems.
pub(super) const LABEL_WIDTH: Rems = rems(10.);
/// Short label width in rems.
pub(super) const SHORT_LABEL_WIDTH: Rems = rems(6.25);
/// Short field width in rems.
pub(super) const SHORT_FIELD_WIDTH: Rems = rems(5.625);
/// Zone width in rems.
pub(super) const ZONE_WIDTH: Rems = rems(16.25);
/// Space name width in rems.
pub(super) const SPACE_NAME_WIDTH: Rems = rems(15.);

/// Requests fixed-width digits in the active kit font.
pub(super) static TABULAR: LazyLock<FontFeatures> =
    LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".into(), 1)])));

/// Presentation states shared by task, activity, and connection views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Running,
    Waiting,
    Done,
    Failed,
    Idle,
}

impl Status {
    /// The semantic color and its 10% overlay for the current theme.
    pub(super) fn colors(self, theme: &Theme) -> (Hsla, Hsla) {
        let ink = match self {
            Self::Running => theme.blue,
            Self::Waiting => theme.yellow,
            Self::Done => theme.green,
            Self::Failed => theme.red,
            Self::Idle => theme.muted_foreground,
        };
        (ink, ink.opacity(0.10))
    }

    /// Map the labels shared by the state layer's board columns and task chips.
    pub(super) fn task(label: &str) -> Self {
        let state = label.split_once(" · ").map_or(label, |(state, _)| state);
        match state {
            "Running" | "Stopping" => Self::Running,
            "Proposed" | "Blocked" | "Waiting on you" | "In review" => Self::Waiting,
            "Done" => Self::Done,
            "Failed" => Self::Failed,
            _ => Self::Idle,
        }
    }
}

impl From<MachineStatus> for Status {
    fn from(status: MachineStatus) -> Self {
        match status {
            MachineStatus::Online => Self::Done,
            MachineStatus::Draining => Self::Waiting,
            MachineStatus::Offline => Self::Idle,
            MachineStatus::Revoked => Self::Failed,
        }
    }
}

/// Emphasis of unchosen options in an answered decision.
pub(super) const UNCHOSEN_OPACITY: f32 = 0.64;
/// Maximum share of the window height for the shortcuts dialog's list.
pub(super) const SHORTCUTS_HEIGHT_FRACTION: f32 = 0.63;

#[cfg(test)]
mod tests {
    use super::Status;

    #[test]
    fn task_states_map_to_status_palette() {
        use Status::{Done, Failed, Idle, Running, Waiting};
        use threadlane_core::model::TaskStatus;
        use threadlane_desktop::status_label;

        for (status, reason, expected) in [
            (TaskStatus::Running, None, Running),
            (TaskStatus::Proposed, None, Waiting),
            (
                TaskStatus::Queued,
                Some("blocked by account availability"),
                Waiting,
            ),
            (TaskStatus::Queued, None, Idle),
            (TaskStatus::WaitingOnYou, None, Waiting),
            (TaskStatus::Done, None, Done),
            (TaskStatus::Failed, None, Failed),
            (TaskStatus::Sleeping, None, Idle),
            (TaskStatus::Cancelled, None, Idle),
        ] {
            let label = status_label(status, reason);
            assert_eq!(Status::task(label), expected, "{label}");
        }
        assert_eq!(Status::task("Running · 5 min"), Running);
        assert_eq!(Status::task("Stopping · 5 min"), Running);
        assert_eq!(Status::task("Stopped"), Idle);
    }

    #[test]
    fn machine_states_map_to_status_palette() {
        use threadlane_core::model::MachineStatus;

        assert_eq!(Status::from(MachineStatus::Online), Status::Done);
        assert_eq!(Status::from(MachineStatus::Draining), Status::Waiting);
        assert_eq!(Status::from(MachineStatus::Offline), Status::Idle);
        assert_eq!(Status::from(MachineStatus::Revoked), Status::Failed);
    }
}
