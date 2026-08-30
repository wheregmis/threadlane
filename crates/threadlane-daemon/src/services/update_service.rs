//! Update service: wraps threadlane-updater so GPUI never imports it directly.

use std::sync::{Arc, Mutex};
use threadlane_protocol::update::*;
use threadlane_updater as updater;

#[derive(Clone, Default)]
pub struct UpdateService {
    cached_release: Arc<Mutex<Option<updater::UpdateReleaseInfo>>>,
    downloaded_bytes: Arc<Mutex<Option<(String, Vec<u8>)>>>,
}

impl UpdateService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check(&self) -> Result<CheckForUpdateResponse, String> {
        let update = updater::check_for_update()?;
        let mut lock = self.cached_release.lock().unwrap();
        *lock = update.clone();

        match update {
            Some(info) => Ok(CheckForUpdateResponse {
                status: "available".to_string(),
                version: Some(info.version),
            }),
            None => Ok(CheckForUpdateResponse {
                status: "up_to_date".to_string(),
                version: None,
            }),
        }
    }

    /// Begin downloading an update. Runs in a detached thread and pushes
    /// `UpdateProgressEvent` notifications via the supplied sender.
    pub fn download(
        &self,
        req: DownloadUpdateRequest,
        progress_tx: tokio::sync::broadcast::Sender<UpdateProgressEvent>,
    ) -> Result<(), String> {
        let release_info = {
            let lock = self.cached_release.lock().unwrap();
            lock.as_ref()
                .filter(|info| info.version == req.version)
                .cloned()
        };

        let release_info = match release_info {
            Some(info) => info,
            None => {
                let fresh = updater::check_for_update()?
                    .ok_or_else(|| "No update currently available".to_string())?;
                if fresh.version != req.version {
                    return Err(format!(
                        "Requested version {} does not match available version {}",
                        req.version, fresh.version
                    ));
                }
                *self.cached_release.lock().unwrap() = Some(fresh.clone());
                fresh
            }
        };

        let version = req.version;
        let downloaded_bytes_store = Arc::clone(&self.downloaded_bytes);

        std::thread::spawn(move || {
            let version_clone = version.clone();
            let progress_tx_cb = progress_tx.clone();
            let result = updater::download_update(&release_info, move |progress| {
                let _ = progress_tx_cb.send(UpdateProgressEvent {
                    version: version_clone.clone(),
                    progress: progress.clamp(0.0, 1.0),
                    done: false,
                    error: None,
                });
            });
            match result {
                Ok(bytes) => {
                    let mut lock = downloaded_bytes_store.lock().unwrap();
                    *lock = Some((version.clone(), bytes));
                    let _ = progress_tx.send(UpdateProgressEvent {
                        version,
                        progress: 1.0,
                        done: true,
                        error: None,
                    });
                }
                Err(e) => {
                    let _ = progress_tx.send(UpdateProgressEvent {
                        version,
                        progress: 0.0,
                        done: true,
                        error: Some(e),
                    });
                }
            }
        });
        Ok(())
    }

    /// Install a previously downloaded update (calls `install_and_relaunch`).
    pub fn install(&self, req: InstallUpdateRequest) -> Result<(), String> {
        let info = {
            let lock = self.cached_release.lock().unwrap();
            lock.as_ref()
                .filter(|info| info.version == req.version)
                .cloned()
                .ok_or_else(|| "Update info not found for installation".to_string())?
        };

        let bytes = {
            let lock = self.downloaded_bytes.lock().unwrap();
            lock.as_ref()
                .filter(|(ver, _)| ver == &req.version)
                .map(|(_, bytes)| bytes.clone())
                .ok_or_else(|| "No downloaded update bytes found. Download first.".to_string())?
        };

        updater::install_and_relaunch(info, bytes).map_err(|e| format!("Install failed: {e}"))
    }
}
