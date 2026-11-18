//! Bounded saved-file search; queries and snippets never enter the journal.
use std::{
    fs::{File, Metadata},
    io::Read,
    path::{Component, Path},
    time::{Duration, Instant},
};
use threadlane_protocol::repo::{FileSearchMatch, FileSearchResult};
const FILE_BYTES: u64 = 2 * 1024 * 1024;
const TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const RESPONSE_BYTES: usize = 1024 * 1024;

#[cfg(all(test, unix))]
thread_local! {
    static FILE_OPENS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub fn validate_target(root: &Path, relative: &str) -> Result<(), String> {
    open_validated(root, relative).map(|_| ())
}

/// Share the validated descriptor and its metadata with the scan, avoiding a
/// second traversal/open and a repeated descriptor metadata lookup.
fn open_validated(root: &Path, relative: &str) -> Result<(File, Metadata), String> {
    let path = threadlane_project::files::resolve_project_path(root, relative)?;
    let mut current = root.to_path_buf();
    for part in Path::new(relative).components() {
        let Component::Normal(part) = part else {
            return Err("Invalid relative file path".into());
        };
        if part == ".git" || part == ".threadlane" {
            return Err("Repository metadata is excluded from search".into());
        }
        current.push(part);
        let metadata = std::fs::symlink_metadata(&current)
            .map_err(|_| "File disappeared; refresh the search")?;
        if metadata.file_type().is_symlink() {
            return Err("Symlinks are excluded".into());
        }
    }
    if !path.is_file() {
        return Err("Not a regular file; refresh the search".into());
    }
    open_regular(root, relative)
}

/// Walk directory descriptors instead of reopening a checked string path: a
/// rename or symlink swap cannot redirect a scan outside the captured root.
#[cfg(unix)]
fn open_regular(root: &Path, relative: &str) -> Result<(File, Metadata), String> {
    #[cfg(test)]
    FILE_OPENS.set(FILE_OPENS.get() + 1);
    use rustix::fs::{open, openat, Mode, OFlags};
    let mut fd = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?;
    let parts = Path::new(relative).components().collect::<Vec<_>>();
    for (index, part) in parts.iter().enumerate() {
        let Component::Normal(part) = part else {
            return Err("Invalid relative path".into());
        };
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        if index + 1 < parts.len() {
            flags |= OFlags::DIRECTORY;
        }
        fd = openat(&fd, *part, flags, Mode::empty()).map_err(|e| e.to_string())?;
    }
    let file = File::from(fd);
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("Not a regular file".into());
    }
    Ok((file, metadata))
}

#[cfg(not(unix))]
fn open_regular(root: &Path, relative: &str) -> Result<(File, Metadata), String> {
    let path = threadlane_project::files::resolve_project_path(root, relative)?;
    let file = File::open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("Not a regular file".into());
    }
    Ok((file, metadata))
}

pub fn search(root: &Path, query: &str) -> Result<FileSearchResult, String> {
    if query.is_empty() || query.contains(['\n', '\r']) || query.len() > 4096 {
        return Err("Enter a single line of text (at most 4096 UTF-8 bytes)".into());
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let (inventory, inventory_partial) = threadlane_git::list_search_files(root, deadline)?;
    let mut result = FileSearchResult::default();
    if let Some(reason) = inventory_partial {
        result.partial.push(reason.into());
    }
    if inventory.non_utf8_skipped > 0 {
        result.partial.push(format!(
            "{} unsupported UTF-8 filenames skipped",
            inventory.non_utf8_skipped
        ));
    }
    let (mut skipped, mut oversized, mut binary) = (0, 0, 0);
    let (mut total, mut response_bytes) = (0, 0);
    'files: for path in inventory.paths {
        if Instant::now() >= deadline {
            result.partial.push("3-second work budget reached".into());
            break;
        }
        if total >= TOTAL_BYTES {
            result
                .partial
                .push("64 MiB total-read budget reached".into());
            break;
        }
        let Ok((file, meta)) = open_validated(root, &path) else {
            skipped += 1;
            continue;
        };
        if meta.len() > FILE_BYTES {
            oversized += 1;
            continue;
        }
        if TOTAL_BYTES - total < FILE_BYTES + 1 {
            result
                .partial
                .push("64 MiB total-read budget reached".into());
            break;
        }
        let mut bytes = Vec::new();
        if file.take(FILE_BYTES + 1).read_to_end(&mut bytes).is_err() {
            skipped += 1;
            continue;
        }
        total += bytes.len() as u64;
        if bytes.len() as u64 > FILE_BYTES {
            oversized += 1;
            continue;
        }
        if bytes.contains(&0) {
            binary += 1;
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            binary += 1;
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            if Instant::now() >= deadline {
                result.partial.push("3-second work budget reached".into());
                break 'files;
            }
            let Some(start) = line.find(query) else {
                continue;
            };
            if result.matches.len() >= 500 {
                result
                    .partial
                    .push("500 matching-line limit reached".into());
                break 'files;
            }
            let mut from = start.saturating_sub(80);
            while !line.is_char_boundary(from) {
                from += 1;
            }
            let mut to = (start + query.len() + 160).min(line.len());
            while !line.is_char_boundary(to) {
                to -= 1;
            }
            let row = FileSearchMatch {
                path: path.clone(),
                line: index + 1,
                snippet: line[from..to].into(),
                match_start: start - from,
                match_end: start + query.len() - from,
            };
            let size = (row.path.len() + row.snippet.len()) * 6 + 160;
            if response_bytes + size > RESPONSE_BYTES - 4096 {
                result.partial.push("1 MiB response limit reached".into());
                break 'files;
            }
            response_bytes += size;
            result.matches.push(row);
        }
    }
    if skipped > 0 {
        result.partial.push(format!(
            "{skipped} symlink, nonregular, missing or unreadable files skipped"
        ));
    }
    if oversized > 0 {
        result
            .partial
            .push(format!("{oversized} files exceeding 2 MiB skipped"));
    }
    if binary > 0 {
        result
            .partial
            .push(format!("{binary} binary or non-UTF-8 files skipped"));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(dir.path())
            .status()
            .unwrap()
            .success());
        dir
    }
    #[test]
    fn file_search_literal_saved_utf8_and_git_coverage() {
        let dir = repo();
        let root = dir.path();
        std::fs::write(root.join("tracked.txt"), "needle\nNeedle\nλ : needle\n").unwrap();
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["add", "tracked.txt"])
            .status()
            .unwrap()
            .success());
        std::fs::write(root.join(".gitignore"), "*.txt\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "needle").unwrap();
        std::fs::write(root.join("colon:name"), " needle \n").unwrap();
        std::fs::write(root.join("binary"), b"needle\0").unwrap();
        std::fs::create_dir(root.join(".threadlane")).unwrap();
        std::fs::write(root.join(".threadlane/private"), "needle").unwrap();
        let result = search(root, "needle").unwrap();
        assert_eq!(result.matches.len(), 3);
        assert_eq!(result.matches[0].path, "colon:name");
        assert_eq!(result.matches[2].line, 3);
        assert!(result
            .partial
            .iter()
            .any(|reason| reason.contains("binary")));
        assert_eq!(search(root, " needle ").unwrap().matches.len(), 1);
        assert!(search(root, "a\nb").is_err());
        assert!(search(root, "").is_err());
    }
    #[test]
    fn file_search_limits_and_symlinks() {
        let dir = repo();
        let root = dir.path();
        std::fs::write(root.join("many"), "needle\n".repeat(501)).unwrap();
        let result = search(root, "needle").unwrap();
        assert_eq!(result.matches.len(), 500);
        assert!(result.partial.iter().any(|reason| reason.contains("500")));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("many"), root.join("link")).unwrap();
            assert!(validate_target(root, "link").is_err());
        }
        assert!(validate_target(root, "../outside").is_err());
        assert!(validate_target(root, "missing").is_err());
        assert!(validate_target(root, ".git/config").is_err());
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::{search, validate_target, FILE_BYTES};
    #[test]
    fn file_search_reports_skips_and_bounds_unicode_snippets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(root)
            .status()
            .unwrap();
        std::fs::write(root.join("large"), vec![b'a'; FILE_BYTES as usize + 1]).unwrap();
        std::fs::write(
            root.join("unicode"),
            format!("{} λ:needle {}", "λ".repeat(1000), "λ".repeat(1000)),
        )
        .unwrap();
        std::fs::write(root.join("invalid"), [0xff, 0xfe]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::ffi::OsStrExt;
                std::fs::write(
                    root.join(std::ffi::OsStr::from_bytes(b"invalid-\xff")),
                    "needle",
                )
                .unwrap();
            }
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("secret"), "needle").unwrap();
            symlink(outside.path(), root.join("escape")).unwrap();
            assert!(validate_target(root, "escape/secret").is_err());
            // FIFO inventory entries must not block a scan.
            assert!(std::process::Command::new("mkfifo")
                .arg(root.join("pipe"))
                .status()
                .unwrap()
                .success());
        }
        let result = search(root, "λ:needle").unwrap();
        assert_eq!(result.matches.len(), 1);
        let row = &result.matches[0];
        assert_eq!(&row.snippet[row.match_start..row.match_end], "λ:needle");
        assert!(row.snippet.len() < 260);
        assert!(result.partial.iter().any(|p| p.contains("2 MiB")));
        assert!(result.partial.iter().any(|p| p.contains("non-UTF-8")));
    }

    #[test]
    fn file_search_non_git_is_not_exhaustive_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(search(dir.path(), "needle")
            .unwrap_err()
            .contains("Git checkout"));
    }

    #[test]
    fn file_search_inventory_timeout_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(dir.path())
            .status()
            .unwrap();
        let (_, partial) =
            threadlane_git::list_search_files(dir.path(), std::time::Instant::now()).unwrap();
        assert!(partial.unwrap().contains("3-second"));
    }

    #[test]
    fn file_search_daemon_transport_does_not_journal_query_or_results() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(root)
            .status()
            .unwrap();
        std::fs::write(root.join("remote.txt"), "private-query-and-snippet").unwrap();
        crate::chat::executor().unwrap().block_on(async {
            use threadlane_protocol::daemon::{CommandResponse, SessionCommand};
            let core = crate::core::DaemonCore::new().unwrap();
            let mut events = core.subscribe();
            let reply = core
                .dispatch(SessionCommand::SearchProjectFiles {
                    work_dir: root.into(),
                    query: "private-query".into(),
                })
                .await
                .unwrap();
            let CommandResponse::FileSearch { result } = reply else {
                panic!("wrong response")
            };
            assert_eq!(result.matches[0].path, "remote.txt");
            assert!(matches!(
                core.dispatch(SessionCommand::ValidateSearchTarget {
                    work_dir: root.into(),
                    path: "remote.txt".into()
                })
                .await
                .unwrap(),
                CommandResponse::Ack
            ));
            assert!(core
                .dispatch(SessionCommand::SearchProjectFiles {
                    work_dir: root.into(),
                    query: "bad\nquery".into()
                })
                .await
                .is_err());
            tokio::task::yield_now().await;
            while let Ok((_, event)) = events.try_recv() {
                assert!(
                    matches!(
                        event,
                        threadlane_protocol::daemon::SessionEvent::AutomationChanged { .. }
                    ),
                    "unexpected search event"
                );
            }
            for (_, event) in core.journal_tail() {
                assert!(
                    matches!(
                        event,
                        threadlane_protocol::daemon::SessionEvent::AutomationChanged { .. }
                    ),
                    "search must not emit journal events"
                );
            }
        });
    }
}

#[cfg(test)]
mod budget_and_worktree_tests {
    use super::search;
    fn git(root: &std::path::Path, args: &[&str]) {
        assert!(std::process::Command::new("git").arg("-C").arg(root).args(args).output().unwrap().status.success());
    }
    #[test]
    fn file_search_uses_captured_worktree_not_primary() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("primary");
        let checkout = dir.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q"]);
        std::fs::write(root.join("saved"), "primary needle").unwrap();
        git(&root, &["add", "saved"]);
        git(&root, &["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "initial"]);
        git(&root, &["worktree", "add", "-b", "search", checkout.to_str().unwrap()]);
        std::fs::write(checkout.join("saved"), "worktree needle").unwrap();
        assert_eq!(search(&checkout, "needle").unwrap().matches[0].snippet, "worktree needle");
        assert_eq!(search(&root, "needle").unwrap().matches[0].snippet, "primary needle");
        std::fs::remove_file(checkout.join("saved")).unwrap();
        assert!(super::validate_target(&checkout, "saved").is_err());
        assert!(search(&checkout, "needle").unwrap().matches.is_empty());
    }
    #[test]
    fn file_search_response_and_read_budgets_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        let query = "λ".repeat(2048);
        std::fs::write(root.join("many"), (query.clone() + "\n").repeat(100)).unwrap();
        let result = search(root, &query).unwrap();
        assert!(!result.matches.is_empty());
        assert!(result.partial.iter().any(|p| p.contains("1 MiB")));
        assert!(serde_json::to_vec(&result).unwrap().len() < 1024 * 1024);
        std::fs::remove_file(root.join("many")).unwrap();
        for i in 0..34 {
            std::fs::File::create(root.join(format!("file-{i}"))).unwrap().set_len(super::FILE_BYTES).unwrap();
        }
        let result = search(root, "needle").unwrap();
        assert!(result.partial.iter().any(|p| p.contains("64 MiB") || p.contains("3-second")));
    }
}

#[cfg(all(test, unix))]
mod root_replacement_tests {
    #[test]
    fn file_search_rejects_checkout_root_replaced_by_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("checkout");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(root.join("same.txt"), "checkout text").unwrap();
        std::fs::write(outside.join("same.txt"), "host secret").unwrap();
        assert!(super::open_regular(&root, "same.txt").is_ok());
        std::fs::rename(&root, dir.path().join("original")).unwrap();
        std::os::unix::fs::symlink(&outside, &root).unwrap();
        assert!(super::open_regular(&root, "same.txt").is_err());
        assert!(super::validate_target(&root, "same.txt").is_err());
    }
}

#[cfg(all(test, unix))]
mod descriptor_reuse_tests {
    #[test]
    fn file_search_opens_each_inventory_entry_once() {
        let dir = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q"]).arg(dir.path()).status().unwrap().success());
        std::fs::write(dir.path().join("sample.txt"), "needle").unwrap();
        super::FILE_OPENS.set(0);
        let result = super::search(dir.path(), "needle").unwrap();
        assert_eq!(result.matches.len(), 1);
        assert!(result.partial.is_empty());
        assert_eq!(super::FILE_OPENS.get(), 1);
    }
}
