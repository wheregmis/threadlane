//! Immutable host for the same trajectory panel used by desktop.
use gpui::App;
use std::{borrow::Cow, path::PathBuf};
use threadlane_protocol::daemon::TrajectoryEntry;
use threadlane_ui_kit::{TrajectoryMode, TrajectorySource};

pub(crate) struct CapturedTrajectory {
    session_key: Option<(PathBuf, String)>,
    execution: Vec<TrajectoryEntry>,
    model_context: Vec<TrajectoryEntry>,
    durable_events: Vec<TrajectoryEntry>,
    recovery: Vec<TrajectoryEntry>,
}
impl CapturedTrajectory {
    pub(crate) fn new(snapshot: Option<&crate::session::Snapshot>) -> Self {
        Self {
            session_key: snapshot
                .and_then(|snapshot| snapshot.session.as_ref())
                .map(|session| (session.work_dir.clone(), session.id.clone())),
            execution: snapshot.map_or_else(Vec::new, |snapshot| snapshot.trajectory.clone()),
            model_context: snapshot.map_or_else(Vec::new, |snapshot| {
                snapshot.trajectory_model_context.clone()
            }),
            durable_events: snapshot.map_or_else(Vec::new, |snapshot| {
                snapshot.trajectory_durable_events.clone()
            }),
            recovery: snapshot
                .map_or_else(Vec::new, |snapshot| snapshot.trajectory_recovery.clone()),
        }
    }
}
impl TrajectorySource for CapturedTrajectory {
    fn session_key(&self, _: &App) -> Option<(PathBuf, String)> {
        self.session_key.clone()
    }
    fn revision(&self, _: TrajectoryMode, _: &App) -> (u64, u64) {
        (0, 0)
    }
    fn entries<'a>(&'a self, mode: TrajectoryMode, _: &'a App) -> Cow<'a, [TrajectoryEntry]> {
        Cow::Borrowed(match mode {
            TrajectoryMode::Execution | TrajectoryMode::Requests => &self.execution,
            TrajectoryMode::ModelContext => &self.model_context,
            TrajectoryMode::DurableEvents => &self.durable_events,
            TrajectoryMode::Recovery => &self.recovery,
        })
    }
}
