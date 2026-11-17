//! Project file browsing and confined single-file I/O for the project root.
//!
//! Canonical home of the Files-surface tree scan (previously in
//! `threadlane-ui-right-panel`) plus the path confinement a remote daemon
//! needs to serve `ReadProjectFile`/`WriteProjectFile`/`ProjectFileExists`
//! without letting a client-supplied relative path escape `work_dir`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use sha2::{Digest, Sha256};
use threadlane_protocol::daemon::{ProjectFileError, VersionedFile};
use threadlane_protocol::repo::ProjectFileNode;

/// Maximum depth `scan_project_tree` descends from the project root.
const TREE_MAX_DEPTH: usize = 6;
/// Largest file `read_project_file` will serve as one UTF-8 payload.
const READ_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Resolve a client-supplied relative path against `root`, refusing any
/// escape: absolute paths, `..`/`.` components, and symlinks that
/// resolve outside `root`.
pub fn resolve_project_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(relative);
    let mut parts = Vec::new();
    for component in rel_path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_os_string()),
            _ => return Err(format!("Path {relative:?} escapes the project root")),
        }
    }
    if parts.is_empty() {
        return Err("Empty path".to_string());
    }
    let candidate = parts.iter().collect::<PathBuf>();
    let joined = root.join(&candidate);
    let real_root = root
        .canonicalize()
        .map_err(|error| format!("Project root does not resolve: {error}"))?;
    // The target may not exist yet (a write creating a file), so
    // canonicalize the deepest ancestor that does — a symlink anywhere
    // above the missing tail still resolves to its real directory.
    let mut ancestor = joined.clone();
    let real_ancestor = loop {
        if let Ok(real) = ancestor.canonicalize() {
            break real;
        }
        if !ancestor.pop() {
            return Err(format!("Path {relative:?} escapes the project root"));
        }
    };
    if !real_ancestor.starts_with(&real_root) {
        return Err(format!("Path {relative:?} escapes the project root"));
    }
    // A dangling symlink at the target is "missing" to canonicalize, but
    // `fs::write` would follow it and create the outside file.
    if let Ok(meta) = std::fs::symlink_metadata(&joined) {
        if meta.file_type().is_symlink() && joined.canonicalize().is_err() {
            return Err(format!("Path {relative:?} escapes the project root"));
        }
    }
    Ok(joined)
}

/// Read `relative` under `root` as UTF-8 text.
pub fn read_project_file(root: &Path, relative: &str) -> Result<String, String> {
    let path = resolve_project_path(root, relative)?;
    let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
    if metadata.len() > READ_MAX_BYTES {
        return Err(format!(
            "File is {} bytes; the {}-byte limit keeps replies bounded",
            metadata.len(),
            READ_MAX_BYTES
        ));
    }
    std::fs::read_to_string(&path).map_err(|error| error.to_string())
}

/// Read an existing regular project file as UTF-8, preserving its exact byte
/// representation and returning a SHA-256 version of those bytes.
pub fn read_project_file_versioned(
    root: &Path,
    relative: &str,
) -> Result<VersionedFile, ProjectFileError> {
    let path = guarded_existing_file(root, relative)?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .open(&path)
        .map_err(project_file_io_error)?;
    if !file.metadata().map_err(project_file_io_error)?.is_file() {
        return Err(ProjectFileError::Io {
            message: format!("Path {relative:?} is not a regular file"),
        });
    }
    reject_symlink_target(&path, relative)?;
    let bytes = read_bounded(&mut file)?;
    let content = String::from_utf8(bytes.clone()).map_err(|error| ProjectFileError::Io {
        message: format!("File is not valid UTF-8: {error}"),
    })?;
    Ok(VersionedFile {
        content,
        version: version_for_bytes(&bytes),
    })
}

/// Overwrite `relative` under `root` with UTF-8 `content`.
pub fn write_project_file(root: &Path, relative: &str, content: &str) -> Result<(), String> {
    let path = resolve_project_path(root, relative)?;
    let lock_path = canonical_write_path(&path);
    let _guard = target_write_lock(&lock_path);
    std::fs::write(&path, content).map_err(|error| error.to_string())
}

/// Overwrite an existing regular project file only if its content still has
/// `expected_version`; the returned version hashes the bytes written. The
/// target lock serializes daemon writers only, not arbitrary external writers.
pub fn write_project_file_guarded(
    root: &Path,
    relative: &str,
    content: &str,
    expected_version: &str,
) -> Result<String, ProjectFileError> {
    let path = guarded_existing_file(root, relative)?;
    let lock_path = path.canonicalize().map_err(project_file_io_error)?;
    let _guard = target_write_lock(&lock_path);
    let path = guarded_existing_file(root, relative)?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(project_file_io_error)?;
    if !file.metadata().map_err(project_file_io_error)?.is_file() {
        return Err(ProjectFileError::Io {
            message: format!("Path {relative:?} is not a regular file"),
        });
    }
    reject_symlink_target(&path, relative)?;
    let current = read_bounded(&mut file)?;
    if version_for_bytes(&current) != expected_version {
        return Err(ProjectFileError::Changed);
    }
    file.set_len(0).map_err(project_file_io_error)?;
    file.seek(SeekFrom::Start(0))
        .map_err(project_file_io_error)?;
    file.write_all(content.as_bytes())
        .map_err(project_file_io_error)?;
    file.flush().map_err(project_file_io_error)?;
    Ok(version_for_bytes(content.as_bytes()))
}

fn guarded_existing_file(root: &Path, relative: &str) -> Result<PathBuf, ProjectFileError> {
    let canonical_root = root.canonicalize().map_err(project_file_io_error)?;
    let path = resolve_project_path(root, relative).map_err(|message| ProjectFileError::Io {
        message,
    })?;
    let relative_path = Path::new(relative);
    let mut current = root.to_path_buf();
    let components = relative_path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(ProjectFileError::Io {
                message: format!("Path {relative:?} escapes the project root"),
            });
        };
        current.push(name);
        let metadata = std::fs::symlink_metadata(&current).map_err(project_file_io_error)?;
        if metadata.file_type().is_symlink() {
            return Err(ProjectFileError::Io {
                message: format!("Path {relative:?} contains a symbolic link"),
            });
        }
        if index + 1 < components.len() && !metadata.is_dir() {
            return Err(ProjectFileError::Io {
                message: format!("Path component in {relative:?} is not a directory"),
            });
        }
        if index + 1 == components.len() && !metadata.is_file() {
            return Err(ProjectFileError::Io {
                message: format!("Path {relative:?} is not a regular file"),
            });
        }
    }
    let canonical_path = path.canonicalize().map_err(project_file_io_error)?;
    if !canonical_path.starts_with(canonical_root) {
        return Err(ProjectFileError::Io {
            message: format!("Path {relative:?} escapes the project root"),
        });
    }
    Ok(path)
}

fn reject_symlink_target(path: &Path, relative: &str) -> Result<(), ProjectFileError> {
    if std::fs::symlink_metadata(path)
        .map_err(project_file_io_error)?
        .file_type()
        .is_symlink()
    {
        return Err(ProjectFileError::Io {
            message: format!("Path {relative:?} contains a symbolic link"),
        });
    }
    Ok(())
}

fn read_bounded(file: &mut std::fs::File) -> Result<Vec<u8>, ProjectFileError> {
    let mut bytes = Vec::new();
    file.take(READ_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(project_file_io_error)?;
    if bytes.len() as u64 > READ_MAX_BYTES {
        return Err(ProjectFileError::Io {
            message: format!(
                "File is larger than the {}-byte limit keeps replies bounded",
                READ_MAX_BYTES
            ),
        });
    }
    Ok(bytes)
}

fn version_for_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut version = String::with_capacity("sha256:".len() + digest.len() * 2);
    version.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(version, "{byte:02x}").expect("writing to a String cannot fail");
    }
    version
}

fn project_file_io_error(error: std::io::Error) -> ProjectFileError {
    if error.kind() == std::io::ErrorKind::NotFound {
        ProjectFileError::Deleted
    } else {
        ProjectFileError::Io {
            message: error.to_string(),
        }
    }
}

fn canonical_write_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    path.parent()
        .and_then(|parent| parent.canonicalize().ok())
        .and_then(|parent| path.file_name().map(|name| parent.join(name)))
        .unwrap_or_else(|| path.to_path_buf())
}

fn target_write_lock(path: &Path) -> MutexGuard<'static, ()> {
    const LOCK_COUNT: usize = 64;
    static LOCKS: OnceLock<Vec<Mutex<()>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| (0..LOCK_COUNT).map(|_| Mutex::new(())).collect());
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    let index = hasher.finish() as usize % LOCK_COUNT;
    locks[index].lock().expect("project file write lock poisoned")
}

/// Whether `relative` names an existing file entry under `root`.
pub fn project_file_exists(root: &Path, relative: &str) -> Result<bool, String> {
    let path = resolve_project_path(root, relative)?;
    Ok(std::fs::symlink_metadata(&path).is_ok())
}

/// Enumerate the project's file tree for the Files surface.
///
/// Skips `.git`, `target`, and `.threadlane`, stops at `limit` entries and
/// `TREE_MAX_DEPTH` deep; directories sort before files, then by name.
pub fn scan_project_tree(root: &Path, limit: usize) -> Vec<ProjectFileNode> {
    // Breadth-first: every level is listed before anything deeper, so one large early
    // directory cannot spend the whole budget and leave later folders unlisted.
    struct Slot {
        node: ProjectFileNode,
        parent: Option<usize>,
    }
    let mut slots: Vec<Slot> = Vec::new();
    let mut queue: std::collections::VecDeque<(Option<usize>, PathBuf, usize)> =
        std::collections::VecDeque::from([(None, PathBuf::new(), 0)]);
    while let Some((parent, relative, depth)) = queue.pop_front() {
        let mark_truncated = |slots: &mut Vec<Slot>| {
            if let Some(parent) = parent {
                slots[parent].node.truncated = true;
            }
        };
        if slots.len() >= limit || depth > TREE_MAX_DEPTH {
            mark_truncated(&mut slots);
            continue;
        }
        let Ok(read_dir) = std::fs::read_dir(root.join(&relative)) else {
            continue;
        };
        let mut children = read_dir
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == ".git" || name == "target" || name == ".threadlane" {
                    return None;
                }
                Some((name, entry.file_type().ok()?.is_dir()))
            })
            .collect::<Vec<_>>();
        children.sort_by_key(|(name, is_dir)| (!*is_dir, name.to_ascii_lowercase()));
        for (name, is_dir) in children {
            if slots.len() >= limit {
                mark_truncated(&mut slots);
                break;
            }
            let path = relative.join(&name);
            let index = slots.len();
            slots.push(Slot {
                node: ProjectFileNode {
                    relative_path: path.to_string_lossy().into_owned(),
                    name,
                    is_dir,
                    children: Vec::new(),
                    truncated: false,
                },
                parent,
            });
            if is_dir {
                queue.push_back((Some(index), path, depth + 1));
            }
        }
    }

    // Children always follow their parent, so folding from the back completes each
    // node's children before the node itself moves into its parent.
    let mut roots = Vec::new();
    while let Some(Slot { mut node, parent }) = slots.pop() {
        node.children.reverse();
        match parent {
            Some(parent) => slots[parent].node.children.push(node),
            None => roots.push(node),
        }
    }
    roots.reverse();
    roots
}

#[cfg(test)]
mod tests {
    use super::{
        project_file_exists, read_project_file, resolve_project_path, scan_project_tree,
        read_project_file_versioned, write_project_file, write_project_file_guarded,
        ProjectFileError, READ_MAX_BYTES,
    };
    use std::path::Path;

    #[test]
    fn resolve_rejects_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for bad in ["../outside", "a/../../outside", "./x", "", "/etc/passwd"] {
            assert!(
                resolve_project_path(root, bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn resolve_accepts_nested_relative() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_project_path(dir.path(), "src/main.rs").unwrap();
        assert_eq!(resolved, dir.path().join("src/main.rs"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_rejects_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "leak").unwrap();
        std::os::unix::fs::symlink(&secret, dir.path().join("link.txt")).unwrap();
        assert!(resolve_project_path(dir.path(), "link.txt").is_err());
        assert!(read_project_file(dir.path(), "link.txt").is_err());
        assert!(project_file_exists(dir.path(), "link.txt").is_err());

        // A symlinked directory cannot smuggle a not-yet-existing file
        // outside the root: writes resolve through the deepest existing
        // ancestor, not just an existing target.
        let outside_dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside_dir.path(), dir.path().join("linkdir"))
            .unwrap();
        assert!(resolve_project_path(dir.path(), "linkdir/new.txt").is_err());
        assert!(write_project_file(dir.path(), "linkdir/new.txt", "x").is_err());
        assert!(!outside_dir.path().join("new.txt").exists());

        // A dangling symlink to a missing outside target must not let a
        // write create that file either.
        let dangling_target = outside_dir.path().join("created.txt");
        std::os::unix::fs::symlink(&dangling_target, dir.path().join("dangling.txt"))
            .unwrap();
        assert!(write_project_file(dir.path(), "dangling.txt", "x").is_err());
        assert!(!dangling_target.exists());
    }

    #[test]
    fn read_write_exists_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        write_project_file(dir.path(), "sub/a.txt", "hello").unwrap();
        assert_eq!(
            read_project_file(dir.path(), "sub/a.txt").unwrap(),
            "hello"
        );
        assert!(project_file_exists(dir.path(), "sub/a.txt").unwrap());
        assert!(!project_file_exists(dir.path(), "sub/missing.txt").unwrap());
    }

    #[test]
    fn guarded_versioned_read_preserves_utf8_bytes_and_write_returns_new_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.txt");
        std::fs::write(&path, "line\r\nλ").unwrap();
        let original = read_project_file_versioned(dir.path(), "source.txt").unwrap();
        assert_eq!(original.content, "line\r\nλ");

        let next_version =
            write_project_file_guarded(dir.path(), "source.txt", "new contents", &original.version)
                .unwrap();
        assert_eq!(
            next_version,
            read_project_file_versioned(dir.path(), "source.txt")
                .unwrap()
                .version
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"new contents",
            "writes preserve the exact supplied UTF-8 bytes"
        );
    }

    #[test]
    fn guarded_write_detects_external_change_and_preserves_current_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.txt");
        std::fs::write(&path, "A").unwrap();
        let original = read_project_file_versioned(dir.path(), "source.txt").unwrap();
        std::fs::write(&path, "external C").unwrap();

        assert_eq!(
            write_project_file_guarded(dir.path(), "source.txt", "buffer B", &original.version),
            Err(ProjectFileError::Changed)
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "external C");
    }

    #[test]
    fn guarded_write_rejects_deleted_files_without_recreating_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.txt");
        std::fs::write(&path, "A").unwrap();
        let original = read_project_file_versioned(dir.path(), "source.txt").unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(
            write_project_file_guarded(dir.path(), "source.txt", "replacement", &original.version),
            Err(ProjectFileError::Deleted)
        );
        assert!(!path.exists());
    }

    #[test]
    fn guarded_versioned_reads_reject_escape_binary_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("outside.txt"), "outside").unwrap();
        std::fs::write(dir.path().join("binary.bin"), [0xff, 0xfe]).unwrap();
        assert!(matches!(
            read_project_file_versioned(dir.path(), "../outside.txt"),
            Err(ProjectFileError::Io { .. })
        ));
        assert!(matches!(
            read_project_file_versioned(dir.path(), "binary.bin"),
            Err(ProjectFileError::Io { .. })
        ));

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                outside.path().join("outside.txt"),
                dir.path().join("link.txt"),
            )
            .unwrap();
            assert!(matches!(
                read_project_file_versioned(dir.path(), "link.txt"),
                Err(ProjectFileError::Io { .. })
            ));
            assert!(matches!(
                write_project_file_guarded(
                    dir.path(),
                    "link.txt",
                    "must not write outside",
                    "sha256:expected"
                ),
                Err(ProjectFileError::Io { .. })
            ));
            assert_eq!(
                std::fs::read_to_string(outside.path().join("outside.txt")).unwrap(),
                "outside"
            );
        }

        std::fs::write(
            dir.path().join("large.txt"),
            vec![b'x'; READ_MAX_BYTES as usize + 1],
        )
        .unwrap();
        assert!(matches!(
            read_project_file_versioned(dir.path(), "large.txt"),
            Err(ProjectFileError::Io { .. })
        ));
    }

    #[test]
    fn concurrent_guarded_writes_allow_exactly_one_matching_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.txt");
        std::fs::write(&path, "original").unwrap();
        let version = read_project_file_versioned(dir.path(), "source.txt")
            .unwrap()
            .version;
        let root = dir.path().to_path_buf();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

        std::thread::scope(|scope| {
            let first_barrier = barrier.clone();
            let first_root = root.clone();
            let first_version = version.clone();
            let first = scope.spawn(move || {
                first_barrier.wait();
                write_project_file_guarded(
                    &first_root,
                    "source.txt",
                    "first",
                    &first_version,
                )
            });
            let second_barrier = barrier.clone();
            let second_root = root.clone();
            let second_version = version.clone();
            let second = scope.spawn(move || {
                second_barrier.wait();
                write_project_file_guarded(
                    &second_root,
                    "source.txt",
                    "second",
                    &second_version,
                )
            });
            barrier.wait();
            let results = [first.join().unwrap(), second.join().unwrap()];
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
            assert_eq!(
                results
                    .iter()
                    .filter(|result| {
                        result.as_ref().err() == Some(&ProjectFileError::Changed)
                    })
                    .count(),
                1
            );
        });
    }

    #[test]
    fn scan_skips_noise_and_sorts_dirs_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("b_dir")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join(".threadlane")).unwrap();
        std::fs::write(root.join("a.txt"), "x").unwrap();

        let nodes = scan_project_tree(root, 100);
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["b_dir", "a.txt"]);
        assert!(nodes[0].is_dir);
        assert!(!nodes[1].is_dir);
    }

    #[test]
    fn scan_respects_limit_and_depth() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "x").unwrap();
        std::fs::write(root.join("b.txt"), "x").unwrap();
        assert_eq!(scan_project_tree(root, 1).len(), 1);

        // Beyond TREE_MAX_DEPTH the subtree is not descended.
        let mut deep = root.to_path_buf();
        for _ in 0..8 {
            deep = deep.join("d");
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("deep.txt"), "x").unwrap();
        let tree = scan_project_tree(root, 10_000);
        fn contains(node_path: &str, nodes: &[super::ProjectFileNode]) -> bool {
            nodes.iter().any(|n| {
                n.relative_path == node_path || contains(node_path, &n.children)
            })
        }
        let rel = Path::new("d/d/d/d/d/d/d/d/deep.txt")
            .to_string_lossy()
            .into_owned();
        assert!(!contains(&rel, &tree));
    }

    #[test]
    fn scan_is_breadth_first_and_marks_unlisted_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a/deep")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        for index in 0..10 {
            std::fs::write(root.join(format!("a/deep/{index}")), "x").unwrap();
        }
        std::fs::write(root.join("b/kept.txt"), "x").unwrap();

        // Root (a, b), then depth 1 (a/deep, b/kept.txt): the large `a/deep` cannot
        // starve `b`, and is marked as unlisted rather than looking empty.
        let tree = scan_project_tree(root, 4);
        let names = |nodes: &[super::ProjectFileNode]| {
            nodes.iter().map(|n| n.name.clone()).collect::<Vec<_>>()
        };
        assert_eq!(names(&tree), ["a", "b"]);
        assert_eq!(names(&tree[0].children), ["deep"]);
        assert_eq!(names(&tree[1].children), ["kept.txt"]);
        assert!(tree[0].children[0].truncated && tree[0].children[0].children.is_empty());
        assert!(!tree[0].truncated && !tree[1].truncated);

        // A folder cut off partway keeps what was listed and is marked.
        let tree = scan_project_tree(root, 7);
        assert_eq!(tree[0].children[0].children.len(), 3);
        assert!(tree[0].children[0].truncated);
    }
}
