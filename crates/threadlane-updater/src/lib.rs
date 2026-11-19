//! Verification and installation for published Threadlane packages.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs::{self, File, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const RELEASES: &str = "https://github.com/wheregmis/threadlane/releases";
const MANIFEST: &str = "threadlane-update.json";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
const PUBLIC_KEY: &str = match option_env!("THREADLANE_UPDATER_PUBLIC_KEY") {
    Some(key) => key,
    None => "",
};

/// An installed component that can be replaced by a release package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Desktop,
    Hub,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    packages: BTreeMap<String, String>,
}

impl Component {
    fn target(self) -> Result<PathBuf> {
        let executable = std::env::current_exe()?;
        let name = match self {
            Self::Desktop => "threadlane-desktop",
            Self::Hub => "threadlane-hub",
        };
        ensure!(
            executable.file_name().is_some_and(|file| file == name) && executable.is_file(),
            "Install the official Threadlane package to enable updates."
        );
        #[cfg(target_os = "macos")]
        if self == Self::Desktop {
            let bundle = executable
                .ancestors()
                .nth(3)
                .filter(|path| executable == path.join("Contents/MacOS/threadlane-desktop"))
                .context("Updates require the Threadlane app bundle")?;
            check_bundle(bundle)?;
            return Ok(bundle.to_owned());
        }
        Ok(executable)
    }

    /// Whether this build and its installation support checking for updates.
    pub fn available(self) -> bool {
        platform().is_ok() && !PUBLIC_KEY.trim().is_empty() && self.target().is_ok()
    }

    /// Checks the signed manifest and returns a newer version if available.
    pub fn check(self) -> Result<Option<String>> {
        self.target()?;
        let (platform, extension) = platform()?;
        let Some(manifest) = latest()? else {
            return Ok(None);
        };
        ensure!(
            manifest
                .packages
                .contains_key(&package_name(&manifest.version, platform, extension)),
            "No update package is published for this platform"
        );
        Ok(Some(manifest.version))
    }

    /// Installs the version the user selected and returns its launch path.
    pub fn install(self, expected: &str) -> Result<PathBuf> {
        let target = self.target()?;
        let (platform, extension) = platform()?;
        let manifest = latest()?
            .filter(|manifest| manifest.version == expected)
            .context("The available release changed. Check for updates again.")?;
        let name = package_name(expected, platform, extension);
        let checksum = manifest
            .packages
            .get(&name)
            .context("No package for this platform")?;
        let parent = target
            .parent()
            .context("Cannot locate the installation directory")?;
        let staging = tempfile::tempdir_in(parent)
            .context("Cannot stage the update in the installation directory")?;
        let archive = staging.path().join(&name);
        fetch_package(
            &format!("{RELEASES}/download/v{expected}/{name}"),
            checksum,
            &archive,
        )?;

        let unpacked = staging.path().join("unpacked");
        fs::create_dir(&unpacked)?;
        let status = Command::new("/usr/bin/tar")
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(&unpacked)
            .status()
            .context("Cannot unpack the release package")?;
        ensure!(status.success(), "Cannot unpack the release package");

        #[cfg(target_os = "macos")]
        if self == Self::Desktop {
            let app = unpacked.join("Threadlane.app");
            check_bundle(&app)?;
            ensure!(
                Command::new("/usr/bin/codesign")
                    .args(["--verify", "--deep", "--strict"])
                    .arg(&app)
                    .status()?
                    .success(),
                "The updated app failed macOS signature verification"
            );
            use rustix::fs::{CWD, RenameFlags, renameat_with};
            renameat_with(CWD, &app, CWD, &target, RenameFlags::EXCHANGE)
                .context("Cannot replace the installed app")?;
            return Ok(target);
        }
        let binary = unpacked.join(target.file_name().context("Missing executable name")?);
        let metadata = fs::symlink_metadata(&binary)?;
        ensure!(
            metadata.is_file() && metadata.len() > 0,
            "The release executable is missing"
        );
        fs::set_permissions(
            &binary,
            Permissions::from_mode(fs::metadata(&target)?.permissions().mode() & 0o777),
        )?;
        File::open(&binary)?.sync_all()?;
        fs::rename(binary, &target).context("Cannot replace the installed executable")?;
        Ok(target)
    }
}

#[cfg(target_os = "macos")]
fn check_bundle(bundle: &Path) -> Result<()> {
    ensure!(
        bundle.extension().is_some_and(|ext| ext == "app"),
        "Updates require the Threadlane app bundle"
    );
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
        .arg(bundle.join("Contents/Info.plist"))
        .output()?;
    ensure!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).trim() == "dev.threadlane.desktop",
        "Updates require the official Threadlane app bundle"
    );
    Ok(())
}

fn platform() -> Result<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok(("macos-arm64", "zip")),
        ("linux", "x86_64") => Ok(("linux-x86_64", "tar.gz")),
        ("linux", "aarch64") => Ok(("linux-aarch64", "tar.gz")),
        _ => bail!("No update package is published for this platform"),
    }
}

fn package_name(version: &str, platform: &str, extension: &str) -> String {
    format!("Threadlane-{version}-{platform}.{extension}")
}

fn download(url: &str, timeout: Duration) -> Result<impl Read> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(timeout))
        .build()
        .into();
    let response = agent
        .get(url)
        .call()
        .context("Cannot reach Threadlane releases")?;
    Ok(response.into_body().into_reader())
}

fn latest() -> Result<Option<Manifest>> {
    ensure!(
        !PUBLIC_KEY.trim().is_empty(),
        "Updates require a signed release build"
    );
    let url = format!("{RELEASES}/latest/download/{MANIFEST}");
    let mut manifest = Vec::new();
    download(&url, Duration::from_secs(12))?
        .take(64 * 1024)
        .read_to_end(&mut manifest)?;
    let mut signature = String::new();
    download(&format!("{url}.sig"), Duration::from_secs(12))?
        .take(4096)
        .read_to_string(&mut signature)?;
    verified_update(&manifest, &signature, PUBLIC_KEY, CURRENT)
}

fn verified_update(
    bytes: &[u8],
    signature: &str,
    key: &str,
    current: &str,
) -> Result<Option<Manifest>> {
    let key = PublicKey::decode(&String::from_utf8(STANDARD.decode(key.trim())?)?)?;
    let signature = Signature::decode(&String::from_utf8(STANDARD.decode(signature.trim())?)?)?;
    key.verify(bytes, &signature, false)
        .context("The release manifest signature is invalid")?;
    let manifest: Manifest = serde_json::from_slice(bytes)?;
    let version = Version::parse(&manifest.version)?;
    ensure!(
        version.pre.is_empty() && version.build.is_empty(),
        "Expected a stable release version"
    );
    Ok((version > Version::parse(current)?).then_some(manifest))
}

fn fetch_package(url: &str, checksum: &str, archive: &Path) -> Result<()> {
    const MAX_BYTES: u64 = 512 * 1024 * 1024;
    let mut response = download(url, Duration::from_secs(300))?.take(MAX_BYTES + 1);
    let mut file = File::create(archive)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 32 * 1024];
    loop {
        let size = response.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
        file.write_all(&buffer[..size])?;
    }
    ensure!(
        response.limit() > 0,
        "The release package exceeds the download limit"
    );
    ensure!(
        hex::encode(hash.finalize()) == checksum,
        "The release package checksum is invalid"
    );
    Ok(())
}

/// Starts the updated app with the same arguments.
pub fn relaunch_desktop(path: &Path) -> Result<()> {
    let args = std::env::args_os().skip(1);
    if cfg!(target_os = "macos") {
        let status = Command::new("/usr/bin/open")
            .arg("-n")
            .arg(path)
            .arg("--args")
            .args(args)
            .status()?;
        ensure!(
            status.success(),
            "The update was installed. Open Threadlane to finish restarting."
        );
    } else {
        Command::new(path)
            .args(args)
            .spawn()
            .context("The update was installed. Open Threadlane to finish restarting.")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated with Cargo Packager's signer and a disposable test key.
    const KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IENDRjk2MzQyOEQzQzI5NDgKUldSSUtUeU5RbVA1ekhMWUVqd3lYQjFJV2VSOWxOQXhsZExCczdLN1BSYUpHeWd6RnVZb1dSZmQK";
    const SIGNATURE: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIGNhcmdvLXBhY2thZ2VyIHNlY3JldCBrZXkKUlVSSUtUeU5RbVA1ekg3RWJ0b1BpTmJ2dzI3UURXMmdidVgxZnE5R08wanpKY2M5UXQ5cW9YY0hGbzhYTDRoWGlXZ0ErQlNabHVRLzZkUWJmc3dNNEt0ZWY0Z0gyRkpIZVEwPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzk1MDk5ODgwCWZpbGU6bWFuaWZlc3QuanNvbgpzcUhNbTIraGhEeHRaZkdXMFVWSlJ2VDQyVW1JaU0rTGpEbEprMTFQT1ozQnRZUGd2THFtNElJSDczbXUxdXFqaUMyRHBZWElWM1BiRm5URE5aUVhCdz09Cg==";
    const MANIFEST: &[u8] =
        b"{\"version\":\"1.2.3\",\"packages\":{\"Threadlane-1.2.3-macos-arm64.zip\":\"example\"}}\n";

    #[test]
    fn offers_only_newer_authentic_releases() {
        let update = verified_update(MANIFEST, SIGNATURE, KEY, "1.2.2")
            .unwrap()
            .unwrap();
        assert_eq!(update.version, "1.2.3");
        assert!(
            update
                .packages
                .contains_key(&package_name(&update.version, "macos-arm64", "zip"))
        );
        for current in ["1.2.3", "1.2.4"] {
            assert!(
                verified_update(MANIFEST, SIGNATURE, KEY, current)
                    .unwrap()
                    .is_none()
            );
        }
        let changed = String::from_utf8_lossy(MANIFEST).replace("example", "tampered");
        assert!(verified_update(changed.as_bytes(), SIGNATURE, KEY, "1.2.2").is_err());
    }
}
