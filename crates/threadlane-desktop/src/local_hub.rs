//! Hubs of the spaces this app created. A space made here is a folder under
//! the client's `spaces/`; the app starts a hub in it, so a new space works
//! at once and comes back after a restart of the computer.

use std::fs::{self, OpenOptions};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use threadlane_client::{PairedHub, Reach};

const HUB: &str = "threadlane-hub";

/// Whether `data_dir` is a space folder this app made: `<root>/spaces/<name>`.
/// A hub paired from anywhere else is yours to start.
pub(crate) fn owned(root: &Path, data_dir: &Path) -> bool {
    data_dir.parent() == Some(&root.join("spaces"))
}

/// The hub program, `<root>/bin/threadlane-hub`, copied there once from
/// beside this app. It does not run from inside the app, where replacing
/// either one would break the other.
fn program(root: &Path) -> Result<PathBuf, String> {
    let installed = root.join("bin").join(HUB);
    if installed.is_file() {
        return Ok(installed);
    }
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(HUB)))
        .filter(|bundled| bundled.is_file())
        .ok_or("this app has no threadlane-hub beside it")?;
    fs::create_dir_all(root.join("bin")).map_err(|error| error.to_string())?;
    fs::copy(&bundled, &installed).map_err(|error| error.to_string())?;
    Ok(installed)
}

/// Starts a hub for the space `name` in `data_dir`, detached from this app
/// so it keeps working after the window closes. Returns once it is spawned,
/// not once it listens.
pub(crate) fn start(root: &Path, data_dir: &Path, name: &str) -> Result<(), String> {
    let program = program(root)?;
    let text = |error: std::io::Error| error.to_string();
    fs::create_dir_all(data_dir).map_err(text)?;
    let config = data_dir.join("hub.toml");
    if !config.exists() {
        fs::write(&config, format!("[space]\nname = {name:?}\n")).map_err(text)?;
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("hub.log"))
        .map_err(text)?;
    // Through a login shell: an app opened from the Dock has a bare PATH, and
    // the hub must find claude, codex, git and gh as your terminal does.
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    Command::new(shell)
        .args(["-lc", "exec \"$0\" \"$@\""])
        .arg(program)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("run")
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(text)?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .map(drop)
        .map_err(text)
}

/// Starts the hub of every space this app made whose hub is not answering.
pub(crate) fn start_missing(root: &Path, hubs: &[PairedHub]) {
    for hub in hubs {
        if let Reach::Socket { socket } = &hub.reach
            && let Some(data_dir) = socket.parent()
            && owned(root, data_dir)
            && UnixStream::connect(socket).is_err()
            && let Err(error) = start(root, data_dir, &hub.name)
        {
            eprintln!(
                "threadlane-desktop: could not start the hub of {}: {error}",
                hub.name
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_folder_directly_under_spaces_is_this_apps_to_start() {
        let root = Path::new("/home/a/.config/threadlane");
        assert!(owned(root, &root.join("spaces/Work")));
        assert!(!owned(root, &root.join("spaces/Work/nested")));
        assert!(!owned(root, Path::new("/srv/hub")));
    }
}
