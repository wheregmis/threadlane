//! Signed application updates powered by cargo-packager-updater.

use cargo_packager_updater::{Config, Update, UpdaterBuilder};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const UPDATE_ENDPOINT: &str = match option_env!("THREADLANE_UPDATER_ENDPOINT") {
    Some(endpoint) => endpoint,
    None => "https://github.com/wheregmis/threadlane/releases/latest/download/latest.json",
};
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const UPDATER_PUBLIC_KEY: &str = match option_env!("THREADLANE_UPDATER_PUBLIC_KEY") {
    Some(key) => key,
    None => "",
};

#[derive(Clone, Debug)]
pub struct UpdateReleaseInfo {
    pub version: String,
    pub notes: String,
    update: Update,
}

#[derive(Clone, Debug, Default)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    Available(UpdateReleaseInfo),
    UpToDate,
    Downloading {
        version: String,
        progress: f32,
    },
    ReadyToInstall {
        info: UpdateReleaseInfo,
        bytes: Arc<Vec<u8>>,
    },
    Installing,
    Error(String),
}

pub fn check_for_update() -> Result<Option<UpdateReleaseInfo>, String> {
    if UPDATER_PUBLIC_KEY.trim().is_empty() {
        return Err(
            "Updater public key is not configured in this build. Set THREADLANE_UPDATER_PUBLIC_KEY when compiling Threadlane."
                .to_string(),
        );
    }

    let current_version = CURRENT_VERSION
        .parse()
        .map_err(|error| format!("Invalid current version '{CURRENT_VERSION}': {error}"))?;
    let endpoint = UPDATE_ENDPOINT
        .parse()
        .map_err(|error| format!("Invalid updater endpoint: {error}"))?;
    let config = Config {
        endpoints: vec![endpoint],
        pubkey: UPDATER_PUBLIC_KEY.to_string(),
        windows: None,
    };

    let update = UpdaterBuilder::new(current_version, config)
        .timeout(Duration::from_secs(30))
        .build()
        .and_then(|updater| updater.check())
        .map_err(|error| format!("Failed to check for updates: {error}"))?;

    Ok(update.map(|update| UpdateReleaseInfo {
        version: update.version.clone(),
        notes: update.body.clone().unwrap_or_default(),
        update,
    }))
}

pub fn download_update<F>(info: &UpdateReleaseInfo, on_progress: F) -> Result<Vec<u8>, String>
where
    F: Fn(f32),
{
    let downloaded = std::cell::Cell::new(0_u64);
    info.update
        .download_extended(
            |chunk_size, total_size| {
                let current = downloaded.get().saturating_add(chunk_size as u64);
                downloaded.set(current);
                if let Some(total) = total_size.filter(|total| *total > 0) {
                    on_progress(((current as f32) / (total as f32)).min(1.0));
                }
            },
            || on_progress(1.0),
        )
        .map_err(|error| format!("Failed to download or verify update: {error}"))
}

pub fn install_and_relaunch(info: UpdateReleaseInfo, bytes: Vec<u8>) -> Result<(), String> {
    let app_bundle = current_app_bundle()?;
    info.update
        .install(bytes)
        .map_err(|error| format!("Failed to install update: {error}"))?;

    std::process::Command::new("open")
        .arg("-n")
        .arg(&app_bundle)
        .spawn()
        .map_err(|error| format!("Update installed, but relaunch failed: {error}"))?;

    std::process::exit(0);
}

fn current_app_bundle() -> Result<PathBuf, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("Failed to locate the current executable: {error}"))?;

    app_bundle_for_executable(&executable).ok_or_else(|| {
        "Installing updates is only available from a packaged Threadlane.app. cargo run can check and download updates, but cannot replace target/debug."
            .to_string()
    })
}

fn app_bundle_for_executable(executable: &Path) -> Option<PathBuf> {
    executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updater_endpoint_is_valid() {
        assert!(UPDATE_ENDPOINT
            .parse::<cargo_packager_updater::url::Url>()
            .is_ok());
    }

    #[test]
    fn package_version_is_valid_semver() {
        assert!(CURRENT_VERSION
            .parse::<cargo_packager_updater::semver::Version>()
            .is_ok());
    }

    #[test]
    fn only_installed_app_executables_resolve_to_an_install_target() {
        assert_eq!(
            app_bundle_for_executable(Path::new(
                "/Applications/Threadlane.app/Contents/MacOS/threadlane"
            )),
            Some(PathBuf::from("/Applications/Threadlane.app"))
        );
        assert_eq!(
            app_bundle_for_executable(Path::new("/workspace/target/debug/threadlane")),
            None
        );
    }
}
