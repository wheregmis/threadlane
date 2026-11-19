//! Serializes the hub's update requests and asks the node to shut down after installation.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use threadlane_core::client::{ApiError, HubUpdate};
use threadlane_updater::Component;
use tokio::sync::watch;

#[derive(Debug)]
pub(crate) struct HubUpdates {
    restart: watch::Sender<Option<PathBuf>>,
    guard: Mutex<()>,
}

impl HubUpdates {
    pub(crate) fn new(restart: watch::Sender<Option<PathBuf>>) -> Self {
        Self {
            restart,
            guard: Mutex::new(()),
        }
    }

    pub(crate) async fn request(
        self: &Arc<Self>,
        request: HubUpdate,
    ) -> Result<Option<String>, ApiError> {
        let updater = Arc::clone(self);
        tokio::task::spawn_blocking(move || updater.apply(request))
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?
    }

    fn apply(&self, request: HubUpdate) -> Result<Option<String>, ApiError> {
        let busy = || ApiError::Invalid("A hub update is already in progress".into());
        let _guard = self.guard.try_lock().map_err(|_| busy())?;
        if self.restart.borrow().is_some() {
            return Err(busy());
        }
        let component = Component::Hub;
        let (version, path) = match request {
            HubUpdate::Check => component.check().map(|version| (version, None)),
            HubUpdate::Install { version } => component
                .install(&version)
                .map(|path| (Some(version), Some(path))),
        }
        .map_err(|error| ApiError::Internal(format!("{error:#}")))?;
        if let Some(path) = path {
            self.restart.send_replace(Some(path));
        }
        Ok(version)
    }
}
