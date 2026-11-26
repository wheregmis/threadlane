//! Executable fake programs for tests that spawn them.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Writes `script` to `path` as an executable that no test's child process
/// can hold open for writing.
///
/// A file this process writes can leak its write descriptor into a child
/// that another test thread forks at that moment; running the file then
/// fails with "Text file busy" (ETXTBSY, os error 26) on Linux. `cp` writes
/// the executable in its own process, so this process never opens it.
pub fn write_executable(path: &Path, script: &str) {
    let mut draft = path.as_os_str().to_owned();
    draft.push(".draft");
    let draft = Path::new(&draft);
    std::fs::write(draft, script).expect("writes the script draft");
    std::fs::set_permissions(draft, std::fs::Permissions::from_mode(0o755))
        .expect("makes the script draft executable");
    let copied = std::process::Command::new("cp")
        .arg(draft)
        .arg(path)
        .status()
        .expect("runs cp");
    assert!(copied.success(), "cp {}", path.display());
}
