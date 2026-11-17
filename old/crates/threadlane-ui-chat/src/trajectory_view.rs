//! Desktop adapter for the shared trajectory panel. Live services remain here.
use gpui::*;
use std::{borrow::Cow, path::PathBuf};
use threadlane_protocol::daemon::TrajectoryEntry;
use threadlane_ui_kit::{TrajectoryMode, TrajectorySource};
use threadlane_ui_state::AppState;

struct LiveTrajectorySource(Entity<AppState>);
impl TrajectorySource for LiveTrajectorySource {
    fn session_key(&self, cx: &App) -> Option<(PathBuf, String)> {
        let state = self.0.read(cx);
        state
            .active_work_dir
            .clone()
            .zip(state.active_session_id.clone())
    }
    fn revision(&self, mode: TrajectoryMode, cx: &App) -> (u64, u64) {
        let state = self.0.read(cx);
        match mode {
            TrajectoryMode::Execution | TrajectoryMode::Requests => {
                (state.trajectory_revision(), state.trajectory_epoch())
            }
            _ => {
                let revision = state.diagnostics_revision();
                (revision, revision)
            }
        }
    }
    fn entries<'a>(&'a self, mode: TrajectoryMode, cx: &'a App) -> Cow<'a, [TrajectoryEntry]> {
        let state = self.0.read(cx);
        match mode {
            TrajectoryMode::Execution | TrajectoryMode::Requests => {
                Cow::Borrowed(state.active_trajectory())
            }
            TrajectoryMode::ModelContext => Cow::Owned(state.active_model_context_diagnostics()),
            TrajectoryMode::DurableEvents => Cow::Owned(state.active_durable_event_diagnostics()),
            TrajectoryMode::Recovery => Cow::Owned(state.active_recovery_diagnostics()),
        }
    }
}

pub struct TrajectoryView {
    view: Entity<threadlane_ui_kit::TrajectoryView<LiveTrajectorySource>>,
    _subscription: Subscription,
}
impl TrajectoryView {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let source = LiveTrajectorySource(model.clone());
        let view = cx.new(|cx| threadlane_ui_kit::TrajectoryView::new(source, window, cx));
        let subscription = cx.observe(&model, |host, _, cx| {
            host.view.update(cx, |_, cx| cx.notify());
        });
        Self {
            view,
            _subscription: subscription,
        }
    }
}
impl Render for TrajectoryView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.view.clone()
    }
}
