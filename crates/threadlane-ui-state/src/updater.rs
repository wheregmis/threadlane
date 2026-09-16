use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender as Sender;

use threadlane_updater::{UpdateReleaseInfo, UpdateStatus};

#[derive(Clone, Debug)]
pub enum UpdaterEvent {
    Status(UpdateStatus),
}

pub fn check(tx: Sender<UpdaterEvent>) {
    let _ = tx.send(UpdaterEvent::Status(UpdateStatus::Checking));
    std::thread::spawn(move || {
        let status = match threadlane_updater::check_for_update() {
            Ok(Some(info)) => UpdateStatus::Available(info),
            Ok(None) => UpdateStatus::UpToDate,
            Err(error) => UpdateStatus::Error(error),
        };
        let _ = tx.send(UpdaterEvent::Status(status));
    });
}

pub fn download(info: UpdateReleaseInfo, tx: Sender<UpdaterEvent>) {
    let version = info.version.clone();
    let _ = tx.send(UpdaterEvent::Status(UpdateStatus::Downloading {
        version: version.clone(),
        progress: 0.0,
    }));
    std::thread::spawn(move || {
        let progress_tx = tx.clone();
        let progress_version = version.clone();
        let result = threadlane_updater::download_update(&info, move |progress| {
            let _ = progress_tx.send(UpdaterEvent::Status(UpdateStatus::Downloading {
                version: progress_version.clone(),
                progress: progress.clamp(0.0, 1.0),
            }));
        });
        let status = match result {
            Ok(bytes) => UpdateStatus::ReadyToInstall {
                info,
                bytes: Arc::new(bytes),
            },
            Err(error) => UpdateStatus::Error(error),
        };
        let _ = tx.send(UpdaterEvent::Status(status));
    });
}

pub fn install(info: UpdateReleaseInfo, bytes: Arc<Vec<u8>>, tx: Sender<UpdaterEvent>) {
    let _ = tx.send(UpdaterEvent::Status(UpdateStatus::Installing));
    std::thread::spawn(move || {
        let bytes = Arc::try_unwrap(bytes).unwrap_or_else(|bytes| (*bytes).clone());
        if let Err(error) = threadlane_updater::install_and_relaunch(info, bytes) {
            let _ = tx.send(UpdaterEvent::Status(UpdateStatus::Error(error)));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_check_maps_to_error_status() {
        let status = match Err::<Option<UpdateReleaseInfo>, _>("offline".to_string()) {
            Ok(Some(info)) => UpdateStatus::Available(info),
            Ok(None) => UpdateStatus::UpToDate,
            Err(error) => UpdateStatus::Error(error),
        };
        assert!(matches!(status, UpdateStatus::Error(error) if error == "offline"));
    }
}
