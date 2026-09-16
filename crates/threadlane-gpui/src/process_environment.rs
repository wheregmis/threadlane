//! Process-environment normalization for desktop launches.
//!
//! GUI launchers on macOS do not inherit shell startup files, so their `PATH`
//! can omit locations where user-installed developer tools live.

#[cfg(target_os = "macos")]
use std::env;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
const MACOS_SYSTEM_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// Adds conventional, existing user-tool directories to `PATH` for child
/// processes started by the application.
pub fn initialize_child_process_path() {
    #[cfg(target_os = "macos")]
    {
        let current =
            env::var_os("PATH").unwrap_or_else(|| std::ffi::OsString::from(MACOS_SYSTEM_PATH));
        let home = env::var_os("HOME").map(PathBuf::from);
        let candidates = [
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            home.as_ref()
                .map(|home| home.join(".cargo/bin"))
                .unwrap_or_default(),
        ];

        let path = prepend_existing_paths(&current, candidates.iter().map(PathBuf::as_path));
        env::set_var("PATH", path);
    }
}

#[cfg(target_os = "macos")]
fn prepend_existing_paths<'a>(
    current: &std::ffi::OsStr,
    candidates: impl IntoIterator<Item = &'a Path>,
) -> std::ffi::OsString {
    let current_paths = env::split_paths(current).collect::<Vec<_>>();
    let additions = candidates
        .into_iter()
        .filter(|path| path.is_dir() && !current_paths.iter().any(|entry| entry == *path))
        .map(PathBuf::from)
        .collect::<Vec<_>>();

    env::join_paths(additions.into_iter().chain(current_paths))
        .expect("existing PATH entries must be valid path components")
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn preserves_existing_path_order_and_skips_duplicate_directories() {
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join("existing");
        let addition = temp.path().join("addition");
        std::fs::create_dir(&existing).unwrap();
        std::fs::create_dir(&addition).unwrap();
        let current = env::join_paths([Path::new("/usr/bin"), existing.as_path()]).unwrap();

        let path = prepend_existing_paths(&current, [existing.as_path(), addition.as_path()]);
        let entries = env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(entries, vec![addition, PathBuf::from("/usr/bin"), existing]);
    }
}
