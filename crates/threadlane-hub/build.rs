//! Records the commit the hub is built from, so runs can tell a stale hub
//! from a missing tool (#652).

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

fn main() {
    println!("cargo:rerun-if-env-changed=THREADLANE_BUILD_COMMIT");
    // Rebuild when HEAD moves: its file, the branch it names, and packed refs.
    for path in ["HEAD", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &branch])
    {
        println!("cargo:rerun-if-changed={path}");
    }
    let commit = std::env::var("THREADLANE_BUILD_COMMIT")
        .ok()
        .filter(|commit| !commit.is_empty())
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=THREADLANE_BUILD_COMMIT={commit}");
}
