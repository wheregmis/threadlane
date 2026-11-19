//! Shared desktop geometry, typography, and status colors.

use std::sync::{Arc, LazyLock};

use gpui_kit::component::{Theme, scroll::Scrollbar};
use gpui_kit::{FontFeatures, Hsla, Pixels, px};
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
/// Composer radius in logical pixels.
pub(super) const COMPOSER_RADIUS: Pixels = px(24.);
/// Field radius in logical pixels.
pub(super) const FIELD_RADIUS: Pixels = px(10.);
/// Button radius in logical pixels.
pub(super) const BUTTON_RADIUS: Pixels = px(8.);
/// Row radius in logical pixels.
pub(super) const ROW_RADIUS: Pixels = px(8.);
/// Icon button radius in logical pixels.
pub(super) const ICON_BUTTON_RADIUS: Pixels = px(6.);
/// Badge radius in logical pixels.
pub(super) const BADGE_RADIUS: Pixels = px(5.);
/// Key radius in logical pixels.
pub(super) const KEY_RADIUS: Pixels = px(4.);

/// Type meta in logical pixels.
pub(super) const TYPE_META: Pixels = px(12.);
/// Type body in logical pixels.
pub(super) const TYPE_BODY: Pixels = px(14.);
/// Type section in logical pixels.
pub(super) const TYPE_SECTION: Pixels = px(16.);
/// Type emphasis in logical pixels.
pub(super) const TYPE_EMPHASIS: Pixels = px(17.);
/// Type title in logical pixels.
pub(super) const TYPE_TITLE: Pixels = px(20.);

/// Control height in logical pixels.
pub(super) const CONTROL_HEIGHT: Pixels = px(32.);
/// Small control height in logical pixels.
pub(super) const SMALL_CONTROL_HEIGHT: Pixels = px(24.);
/// Row height in logical pixels.
pub(super) const ROW_HEIGHT: Pixels = px(34.);
/// Bar height in logical pixels.
pub(super) const BAR_HEIGHT: Pixels = px(52.);
/// Reading width in logical pixels.
pub(super) const READING_WIDTH: Pixels = px(880.);

/// Sidebar width in logical pixels.
pub(super) const SIDEBAR_WIDTH: Pixels = px(220.);
/// Panel width in logical pixels.
pub(super) const PANEL_WIDTH: Pixels = px(250.);
/// Board column min in logical pixels.
pub(super) const BOARD_COLUMN_MIN: Pixels = px(152.);
/// Timestamp width in logical pixels.
pub(super) const TIMESTAMP_WIDTH: Pixels = px(110.);
/// Form width in logical pixels.
pub(super) const FORM_WIDTH: Pixels = px(560.);
/// Compact form width in logical pixels.
pub(super) const COMPACT_FORM_WIDTH: Pixels = px(420.);
/// Rules width in logical pixels.
pub(super) const RULES_WIDTH: Pixels = px(320.);
/// Memory list width in logical pixels.
pub(super) const MEMORY_LIST_WIDTH: Pixels = px(300.);
/// Label width in logical pixels.
pub(super) const LABEL_WIDTH: Pixels = px(160.);
/// Short label width in logical pixels.
pub(super) const SHORT_LABEL_WIDTH: Pixels = px(100.);
/// Short field width in logical pixels.
pub(super) const SHORT_FIELD_WIDTH: Pixels = px(90.);
/// Zone width in logical pixels.
pub(super) const ZONE_WIDTH: Pixels = px(260.);
/// Space name width in logical pixels.
pub(super) const SPACE_NAME_WIDTH: Pixels = px(240.);

/// Width at which the task timeline can sit beside its supporting panel.
pub(super) const TASK_TIMELINE_BASIS: Pixels = px(300.);

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
