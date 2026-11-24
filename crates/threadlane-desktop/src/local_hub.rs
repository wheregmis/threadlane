//! Hubs of the spaces this app created. A space made here is a folder under
//! the client's `spaces/`; the app starts a hub in it, so a new space works
//! at once and comes back after a restart of the computer.
//!
//! The app keeps those hubs on its own build: it launches the hub bundled
//! with it, and on start restarts a hub it launched that runs an older build.

use std::fs::{self, OpenOptions};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use threadlane_client::{PairedHub, Reach};
use threadlane_core::client::HubBuild;

const HUB: &str = "threadlane-hub";

/// How long a hub gets to stop its runs and exit before a restart gives up.
const STOP_WITHIN: Duration = Duration::from_secs(120);

/// This app's build, which the hubs it launches should run.
pub(crate) fn app_build() -> HubBuild {
    HubBuild {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        commit: env!("THREADLANE_BUILD_COMMIT").to_owned(),
    }
}

/// How a hub's build stands against the app's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Standing {
    Current,
    /// Older, or the same version from another commit: the app's replaces it.
    Behind,
    /// A newer release: replacing it would downgrade its database.
    Ahead,
}

pub(crate) fn standing(app: &HubBuild, hub: Option<&HubBuild>) -> Standing {
    // A hub that reports no build predates build reports.
    let Some(hub) = hub else {
        return Standing::Behind;
    };
    let parse = |version: &str| semver::Version::parse(version).ok();
    match (parse(&hub.version), parse(&app.version)) {
        (Some(hub_version), Some(app_version)) => match hub_version.cmp_precedence(&app_version) {
            std::cmp::Ordering::Greater => Standing::Ahead,
            std::cmp::Ordering::Less => Standing::Behind,
            std::cmp::Ordering::Equal if hub.commit == app.commit => Standing::Current,
            std::cmp::Ordering::Equal => Standing::Behind,
        },
        _ if hub == app => Standing::Current,
        _ => Standing::Behind,
    }
}

/// What the app does about a running hub's build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Plan {
    Keep,
    /// Stop it and start it again on the app's build.
    Restart,
    /// Leave it running and tell the user.
    Notice(String),
}

/// `ours`: the app started this hub, in a space folder it made.
pub(crate) fn plan(name: &str, app: &HubBuild, hub: Option<&HubBuild>, ours: bool) -> Plan {
    let on = hub.map_or_else(|| "an older build".to_owned(), |hub| format!("build {hub}"));
    match standing(app, hub) {
        Standing::Current => Plan::Keep,
        Standing::Behind if ours => Plan::Restart,
        Standing::Behind => Plan::Notice(format!(
            "The hub of {name} is on {on}; this app is on build {app}. This app did not start that hub, so it cannot restart it: restart it on build {app} where it runs."
        )),
        Standing::Ahead => Plan::Notice(format!(
            "The hub of {name} is on {on}, newer than this app's build {app}. Update the app."
        )),
    }
}

/// The process id of the hub serving `socket`, if it is one this app
/// launched in a space folder it made, so it may restart it. A hub you run
/// yourself, from another program or folder, is never stopped.
/// `reported`: the id the hub gave; older hubs give none, and the process
/// holding the folder's lock is asked instead.
pub(crate) fn ours(root: &Path, socket: &Path, reported: Option<u32>) -> Option<u32> {
    let data_dir = socket.parent().filter(|dir| owned(root, dir))?;
    let pid = reported.or_else(|| {
        let output = Command::new("lsof")
            .arg("-t")
            .arg(data_dir.join("hub.lock"))
            .output()
            .ok()?;
        let text = String::from_utf8(output.stdout).ok()?;
        let mut holders = text.split_whitespace();
        let pid = holders.next()?.parse().ok()?;
        holders.next().is_none().then_some(pid)
    })?;
    let output = Command::new("ps")
        .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    launched_here(root, data_dir, &String::from_utf8_lossy(&output.stdout)).then_some(pid)
}

/// Whether `command`, as `ps` shows it, is the hub [start] launches.
fn launched_here(root: &Path, data_dir: &Path, command: &str) -> bool {
    let program = root.join("bin").join(HUB);
    command.trim()
        == format!(
            "{} --data-dir {} run",
            program.display(),
            data_dir.display()
        )
}

/// Whether `data_dir` is a space folder this app made: `<root>/spaces/<name>`.
/// A hub paired from anywhere else is yours to start.
pub(crate) fn owned(root: &Path, data_dir: &Path) -> bool {
    data_dir.parent() == Some(&root.join("spaces"))
}

/// The hub program, `<root>/bin/threadlane-hub`, copied there from beside
/// this app whenever the copy is behind the app's build. It does not run from
/// inside the app, where replacing either one would break the other.
fn program(root: &Path) -> Result<PathBuf, String> {
    let installed = root.join("bin").join(HUB);
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(HUB)))
        .filter(|bundled| bundled.is_file());
    let current = || {
        installed.is_file()
            && standing(&app_build(), built(&installed).as_ref()) != Standing::Behind
    };
    let Some(bundled) = bundled.filter(|_| !current()) else {
        return if installed.is_file() {
            Ok(installed)
        } else {
            Err("this app has no threadlane-hub beside it".to_owned())
        };
    };
    let text = |error: std::io::Error| error.to_string();
    fs::create_dir_all(root.join("bin")).map_err(text)?;
    // Renamed into place: a hub still running keeps its old file.
    let staged = root.join("bin").join(format!(".{HUB}.new"));
    fs::copy(&bundled, &staged).map_err(text)?;
    fs::rename(&staged, &installed).map_err(text)?;
    Ok(installed)
}

/// The build `program --version` reports; `None` from a hub too old to say.
fn built(program: &Path) -> Option<HubBuild> {
    let output = Command::new(program).arg("--version").output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    parse_version(&text).filter(|_| output.status.success())
}

/// Reads `threadlane-hub <version> <commit>`.
fn parse_version(text: &str) -> Option<HubBuild> {
    let mut words = text.split_whitespace();
    (words.next()? == HUB).then_some(())?;
    Some(HubBuild {
        version: words.next()?.to_owned(),
        commit: words.next()?.to_owned(),
    })
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
        .map_err(text)
        .map(|mut child| {
            // Reaped, so a hub that exits leaves no zombie that looks alive.
            std::thread::spawn(move || child.wait());
        })
}

/// Stops the hub with process id `pid` as Ctrl-C does, so its runs stop
/// cleanly and resume as after any hub restart, then starts it again on this
/// app's build. Blocks until it answers again.
pub(crate) fn restart(root: &Path, socket: &Path, name: &str, pid: u32) -> Result<(), String> {
    let data_dir = socket.parent().ok_or("the hub has no data folder")?;
    // Before stopping it: a missing or stale program must not leave the hub
    // stopped, nor restart it on the build it already runs every launch.
    let program = program(root)?;
    replaceable(&app_build(), built(&program).as_ref())?;
    let kill = |signal: &str| {
        Command::new("kill")
            .args([signal, &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    if !kill("-INT") {
        return Err("could not ask it to stop".to_owned());
    }
    let asked = Instant::now();
    while kill("-0") {
        if asked.elapsed() > STOP_WITHIN {
            return Err(format!(
                "it did not stop within {} s and is still running",
                STOP_WITHIN.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    start(root, data_dir, name)?;
    for _ in 0..50 {
        if UnixStream::connect(socket).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(format!(
        "it was started again but is not answering; see {}",
        data_dir.join("hub.log").display()
    ))
}

/// Whether the hub program, on build `replacement`, may replace a running
/// hub: only when it is on the app's build or newer. A developer build of the
/// app beside a hub not rebuilt with it gets this refusal, not a restart loop.
fn replaceable(app: &HubBuild, replacement: Option<&HubBuild>) -> Result<(), String> {
    if standing(app, replacement) == Standing::Behind {
        let on = replacement.map_or_else(|| "an older build".to_owned(), ToString::to_string);
        return Err(format!(
            "the hub program beside this app is on {on}, not build {app}; rebuild or reinstall threadlane-hub"
        ));
    }
    Ok(())
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

    fn build(version: &str, commit: &str) -> HubBuild {
        HubBuild {
            version: version.to_owned(),
            commit: commit.to_owned(),
        }
    }

    #[test]
    fn a_hub_behind_the_app_is_restarted_only_when_the_app_started_it() {
        let app = build("0.1.57", "aaaa1111");
        for hub in [
            None,
            Some(build("0.1.56", "cbe0ada0")),
            Some(build("0.1.57", "cbe0ada0")),
        ] {
            assert_eq!(plan("Work", &app, hub.as_ref(), true), Plan::Restart);
            let Plan::Notice(notice) = plan("Work", &app, hub.as_ref(), false) else {
                panic!("a hub this app did not start is never restarted");
            };
            assert!(
                notice.contains("this app is on build 0.1.57 (aaaa1111)"),
                "{notice}"
            );
            assert!(notice.contains("it cannot restart it"), "{notice}");
        }
        let old = plan("Work", &app, Some(&build("0.1.56", "cbe0ada0")), false);
        assert!(matches!(old, Plan::Notice(notice) if notice.contains("build 0.1.56 (cbe0ada0)")));
    }

    #[test]
    fn a_current_hub_is_kept_and_a_newer_one_is_never_downgraded() {
        let app = build("0.1.57", "aaaa1111");
        assert_eq!(plan("Work", &app, Some(&app), true), Plan::Keep);
        assert_eq!(plan("Work", &app, Some(&app), false), Plan::Keep);
        for ours in [true, false] {
            let newer = plan("Work", &app, Some(&build("0.1.60", "bbbb2222")), ours);
            assert!(matches!(newer, Plan::Notice(notice) if notice.contains("Update the app")));
        }
        // Numeric, not textual: 0.1.10 is newer than 0.1.9.
        assert_eq!(
            standing(&build("0.1.9", "a"), Some(&build("0.1.10", "b"))),
            Standing::Ahead
        );
    }

    #[test]
    fn only_the_hub_this_app_launches_in_its_own_space_folder_is_ours() {
        let root = Path::new("/Users/a/Library/Application Support/threadlane");
        let work = root.join("spaces/Work");
        let launched = format!(
            "{}/bin/threadlane-hub --data-dir {} run\n",
            root.display(),
            work.display()
        );
        assert!(launched_here(root, &work, &launched));
        // Run by hand from a checkout, or with other arguments.
        let by_hand = format!(
            "/src/threadlane/target/debug/threadlane-hub --data-dir {} run",
            work.display()
        );
        assert!(!launched_here(root, &work, &by_hand));
        assert!(!launched_here(
            root,
            &work,
            &launched.replace(" run", " check")
        ));
        assert!(!launched_here(root, &work, ""));
        // A folder the app did not make is never ours, whatever runs there.
        assert_eq!(ours(root, Path::new("/srv/hub/hub.sock"), Some(1)), None);
    }

    #[test]
    fn versions_compare_by_semver_precedence_with_pre_releases() {
        let app = build("0.1.57", "aaaa1111");
        // A pre-release of the next version is newer than this app.
        assert_eq!(
            standing(&app, Some(&build("0.1.58-rc.1", "bbbb2222"))),
            Standing::Ahead
        );
        assert_eq!(
            standing(&app, Some(&build("0.1.57-rc.1", "bbbb2222"))),
            Standing::Behind
        );
        // An app pre-release is older than its release, newer than an earlier rc.
        let rc = build("0.1.58-rc.2", "cccc3333");
        assert_eq!(standing(&rc, Some(&build("0.1.58", "d"))), Standing::Ahead);
        assert_eq!(
            standing(&rc, Some(&build("0.1.58-rc.1", "d"))),
            Standing::Behind
        );
        assert_eq!(standing(&rc, Some(&build("0.1.57", "d"))), Standing::Behind);
        assert_eq!(standing(&rc, Some(&rc)), Standing::Current);
        assert_eq!(
            standing(&rc, Some(&build("0.1.58-rc.2", "other"))),
            Standing::Behind
        );
    }

    #[test]
    fn only_a_program_on_the_apps_build_or_newer_replaces_a_hub() {
        let app = build("0.1.57", "aaaa1111");
        assert_eq!(replaceable(&app, Some(&app)), Ok(()));
        assert_eq!(replaceable(&app, Some(&build("0.1.58", "b"))), Ok(()));
        // A stale dev hub at the same version, or an older or silent one.
        for stale in [
            Some(build("0.1.57", "cbe0ada0")),
            Some(build("0.1.56", "a")),
            None,
        ] {
            let refused = replaceable(&app, stale.as_ref()).unwrap_err();
            assert!(refused.contains("rebuild or reinstall"), "{refused}");
        }
    }

    #[test]
    fn a_stale_installed_hub_is_refused_before_the_hub_is_stopped() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let program = bin.join(HUB);
        fs::write(&program, "#!/bin/sh\necho threadlane-hub 0.0.1 0123abcd\n").unwrap();
        let mut perms = fs::metadata(&program).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&program, perms).unwrap();
        let mut hub = Command::new("sleep").arg("30").spawn().unwrap();
        let socket = root.path().join("spaces/Work/hub.sock");
        let refused = restart(root.path(), &socket, "Work", hub.id()).unwrap_err();
        assert!(refused.contains("0.0.1 (0123abcd)"), "{refused}");
        assert!(
            hub.try_wait().unwrap().is_none(),
            "the hub must keep running"
        );
        hub.kill().unwrap();
        hub.wait().unwrap();
    }

    #[test]
    fn reads_the_hub_version_line() {
        assert_eq!(
            parse_version("threadlane-hub 0.1.57 cbe0ada0f29c\n"),
            Some(build("0.1.57", "cbe0ada0f29c"))
        );
        assert_eq!(parse_version("error: unknown argument \"--version\""), None);
    }
}
