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

pub fn current_version() -> &'static str {
    CURRENT_VERSION
}
const UPDATER_PUBLIC_KEY: &str = match option_env!("THREADLANE_UPDATER_PUBLIC_KEY") {
    Some(key) => key,
    None => "",
};

#[derive(Clone, Debug)]
pub struct UpdateReleaseInfo {
    pub version: String,
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

pub fn is_configured() -> bool {
    !UPDATER_PUBLIC_KEY.trim().is_empty()
}

pub fn check_for_update() -> Result<Option<UpdateReleaseInfo>, String> {
    if !is_configured() {
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

    let bundle = app_bundle_for_executable(&executable).ok_or_else(|| {
        "Installing updates is only available from a packaged Threadlane.app. cargo run can check and download updates, but cannot replace target/debug."
            .to_string()
    })?;
    // Any `*.app` ancestor resolves above — including a hostile bundle or a
    // throwaway profiling build. Verify the bundle identity before letting
    // an update overwrite it.
    match bundle_identifier(&bundle) {
        Some(id) if is_allowed_bundle_id(&id) => Ok(bundle),
        Some(id) => Err(format!(
            "Refusing to install an update into {bundle} (bundle id '{id}'): updates only install into dev.threadlane.app bundles.",
            bundle = bundle.display()
        )),
        None => Err(format!(
            "Refusing to install an update into {bundle}: no readable bundle identifier (Contents/Info.plist).",
            bundle = bundle.display()
        )),
    }
}

/// Production (`dev.threadlane.app`) plus dotted dev variants such as the
/// `dev.threadlane.sourceprofile` profiling builds. Anything else —
/// `/tmp/Evil.app`, unrelated apps — is refused.
fn is_allowed_bundle_id(id: &str) -> bool {
    id == "dev.threadlane.app" || id.starts_with("dev.threadlane.")
}

/// Reads `CFBundleIdentifier` from a bundle's `Info.plist` without a plist
/// dependency: our own generated plist uses the canonical long form.
fn bundle_identifier(app_bundle: &Path) -> Option<String> {
    let plist =
        std::fs::read_to_string(app_bundle.join("Contents").join("Info.plist")).ok()?;
    let key = "<key>CFBundleIdentifier</key>";
    let after_key = plist.find(key).map(|index| &plist[index + key.len()..])?;
    let value = after_key.trim_start().strip_prefix("<string>")?;
    let end = value.find("</string>")?;
    let id = value[..end].trim();
    (!id.is_empty()).then(|| id.to_string())
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
    fn updater_configuration_matches_embedded_key() {
        assert_eq!(is_configured(), !UPDATER_PUBLIC_KEY.trim().is_empty());
    }

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
    fn current_version_matches_constant() {
        assert_eq!(current_version(), CURRENT_VERSION);
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

    #[test]
    fn bundle_identity_allowlist() {
        assert!(is_allowed_bundle_id("dev.threadlane.app"));
        assert!(is_allowed_bundle_id("dev.threadlane.sourceprofile"));
        assert!(!is_allowed_bundle_id("com.evil.app"));
        assert!(!is_allowed_bundle_id(""));
        assert!(!is_allowed_bundle_id("dev.threadlane"));
    }

    #[test]
    fn bundle_identifier_parses_our_info_plist() {
        let dir = std::env::temp_dir().join(format!(
            "threadlane-updater-test-{}",
            std::process::id()
        ));
        let bundle = dir.join("Threadlane.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        std::fs::write(
            bundle.join("Contents").join("Info.plist"),
            "<?xml version=\"1.0\"?>\n<plist><dict>\n<key>CFBundleIdentifier</key>\n<string>dev.threadlane.app</string>\n</dict></plist>",
        )
        .unwrap();
        assert_eq!(
            bundle_identifier(&bundle).as_deref(),
            Some("dev.threadlane.app")
        );
        assert_eq!(bundle_identifier(&dir.join("Missing.app")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
