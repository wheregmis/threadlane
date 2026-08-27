//! Updater service client — update check/download/install via the daemon.

use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender as Sender;
use threadlane_protocol::{
    DownloadUpdateRequest, InstallUpdateRequest, UpdateReleaseInfo,
    UpdateStatus,
};

#[derive(Clone, Debug)]
pub enum UpdaterEvent {
    Status(UpdateStatus),
}

fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    crate::services::chat::executor()
}

pub(crate) fn is_configured() -> bool {
    // Daemon manages updater configuration; enable client check on macOS
    cfg!(target_os = "macos")
}

pub(crate) fn check(tx: Sender<UpdaterEvent>) {
    let _ = tx.send(UpdaterEvent::Status(UpdateStatus::Checking));
    if let Ok(rt) = executor() {
        let tx2 = tx.clone();
        rt.spawn(async move {
            let status = match crate::services::daemon_client::get_daemon_client().await {
                Ok(client) => match client.check_for_update().await {
                    Ok(res) => match res.version {
                        Some(version) => UpdateStatus::Available(UpdateReleaseInfo {
                            version,
                            url: String::new(),
                            signature: String::new(),
                            notes: None,
                        }),
                        None => UpdateStatus::UpToDate,
                    },
                    Err(e) => UpdateStatus::Error(e),
                },
                Err(e) => UpdateStatus::Error(e),
            };
            let _ = tx2.send(UpdaterEvent::Status(status));
        });
    }
}

pub(crate) fn download(info: UpdateReleaseInfo, tx: Sender<UpdaterEvent>) {
    let version = info.version.clone();
    let _ = tx.send(UpdaterEvent::Status(UpdateStatus::Downloading {
        version: version.clone(),
        progress: 0.0,
    }));
    if let Ok(rt) = executor() {
        let tx2 = tx.clone();
        rt.spawn(async move {
            let result = match crate::services::daemon_client::get_daemon_client().await {
                Ok(client) => {
                    client
                        .download_update(DownloadUpdateRequest {
                            version: version.clone(),
                            url: info.url.clone(),
                            signature: info.signature.clone(),
                        })
                        .await
                }
                Err(e) => Err(e),
            };
            match result {
                Ok(()) => {
                    let _ = tx2.send(UpdaterEvent::Status(UpdateStatus::ReadyToInstall {
                        info,
                        bytes: Arc::new(Vec::new()),
                    }));
                }
                Err(e) => {
                    let _ = tx2.send(UpdaterEvent::Status(UpdateStatus::Error(e)));
                }
            }
        });
    }
}

pub(crate) fn install(info: UpdateReleaseInfo, _bytes: Arc<Vec<u8>>, tx: Sender<UpdaterEvent>) {
    let _ = tx.send(UpdaterEvent::Status(UpdateStatus::Installing));
    if let Ok(rt) = executor() {
        let tx2 = tx.clone();
        rt.spawn(async move {
            let result = match crate::services::daemon_client::get_daemon_client().await {
                Ok(client) => {
                    client
                        .install_update(InstallUpdateRequest {
                            version: info.version.clone(),
                        })
                        .await
                }
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                let _ = tx2.send(UpdaterEvent::Status(UpdateStatus::Error(e)));
            }
            // On success the daemon relaunches the app; no further events needed.
        });
    }
}
