//! Local `git` repositories for workspace tests (docs/milestone-3.md §1
//! item 4): bare repositories in a temp directory, never a network.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Runs `git` in `dir` with a fixed identity and no signing, so tests pass
/// whatever this machine's `git` configuration says. Panics if it fails.
/// Returns its standard output.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A bare repository `<dir>/<name>.git` with one commit on `main` holding
/// `README.md`. Returns its path, which works as a remote URL.
pub fn bare_repository(dir: &Path, name: &str) -> PathBuf {
    let seed = dir.join(format!("{name}-seed"));
    std::fs::create_dir_all(&seed).expect("seed directory");
    git(&seed, &["init", "--quiet", "--initial-branch", "main"]);
    std::fs::write(seed.join("README.md"), format!("# {name}\n")).expect("seed file");
    git(&seed, &["add", "README.md"]);
    git(
        &seed,
        &["commit", "--quiet", "--no-verify", "-m", "Initial commit"],
    );
    let bare = dir.join(format!("{name}.git"));
    git(
        dir,
        &[
            "clone",
            "--quiet",
            "--bare",
            seed.to_str().expect("utf-8 path"),
            bare.to_str().expect("utf-8 path"),
        ],
    );
    bare
}
