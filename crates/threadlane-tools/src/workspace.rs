use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, OnceLock, RwLock};

/// Resolves `path_input` against `workspace_root` and rejects anything that
/// escapes the workspace, including via symlinks or `..` components.
///
/// Accepts absolute and relative inputs, and tolerates paths that do not exist
/// yet so callers can validate a write destination before creating it.
pub(crate) fn canonical_workspace_root(workspace_root: &Path) -> Result<PathBuf, String> {
    static ROOTS: OnceLock<Mutex<HashMap<PathBuf, PathBuf>>> = OnceLock::new();
    let roots = ROOTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(canonical) = roots
        .lock()
        .ok()
        .and_then(|roots| roots.get(workspace_root).cloned())
    {
        return Ok(canonical);
    }
    let canonical = workspace_root
        .canonicalize()
        .map_err(|e| format!("Invalid workspace root '{}': {e}", workspace_root.display()))?;
    if let Ok(mut roots) = roots.lock() {
        roots.insert(workspace_root.to_path_buf(), canonical.clone());
    }
    Ok(canonical)
}

pub fn validate_path_in_workspace(
    path_input: &str,
    workspace_root: &Path,
) -> Result<PathBuf, String> {
    let canonical_root = canonical_workspace_root(workspace_root)?;

    let p = Path::new(path_input);
    let absolute_path = if p.is_absolute() {
        p.to_path_buf()
    } else {
        canonical_root.join(p)
    };

    let mut normalized = PathBuf::new();
    for comp in absolute_path.components() {
        match comp {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            c => normalized.push(c),
        }
    }

    if normalized.exists() {
        let canonical_target = normalized.canonicalize().map_err(|e| {
            format!(
                "Failed to canonicalize path '{}': {e}",
                normalized.display()
            )
        })?;
        if !canonical_target.starts_with(&canonical_root) {
            return Err(format!(
                "Access denied: Path '{}' escapes workspace root '{}'",
                path_input,
                canonical_root.display()
            ));
        }
        Ok(canonical_target)
    } else {
        let mut ancestor = normalized.as_path();
        while !ancestor.exists() {
            if let Some(parent) = ancestor.parent() {
                ancestor = parent;
            } else {
                break;
            }
        }
        let canonical_ancestor = ancestor.canonicalize().map_err(|e| {
            format!(
                "Failed to canonicalize ancestor '{}': {e}",
                ancestor.display()
            )
        })?;
        if !canonical_ancestor.starts_with(&canonical_root) {
            return Err(format!(
                "Access denied: Path '{}' escapes workspace root '{}'",
                path_input,
                canonical_root.display()
            ));
        }
        // Rebuild the target on top of the canonical ancestor. Comparing
        // `normalized` against the canonical root directly would reject valid
        // destinations whenever the workspace path traverses a symlink, because
        // the two sides are then spelled differently (`/tmp/...` against
        // `/private/tmp/...` on macOS). The trailing components do not exist, so
        // they cannot themselves be symlinks that escape.
        let tail = normalized.strip_prefix(ancestor).map_err(|_| {
            format!(
                "Failed to resolve path '{}' inside workspace root '{}'",
                path_input,
                canonical_root.display()
            )
        })?;
        let resolved = canonical_ancestor.join(tail);
        if !resolved.starts_with(&canonical_root) {
            return Err(format!(
                "Access denied: Path '{}' escapes workspace root '{}'",
                path_input,
                canonical_root.display()
            ));
        }
        Ok(resolved)
    }
}

pub(crate) fn validate_cwd_in_workspace(
    cwd_input: Option<&str>,
    workspace_root: &Path,
) -> Result<PathBuf, String> {
    let canonical_root = canonical_workspace_root(workspace_root)?;

    if cwd_input.is_none() {
        return Ok(canonical_root);
    }

    let target_dir = match cwd_input {
        Some(dir) => {
            let p = Path::new(dir);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                canonical_root.join(p)
            }
        }
        None => unreachable!("handled above"),
    };

    let canonical_target = target_dir
        .canonicalize()
        .map_err(|e| format!("Invalid working directory '{}': {e}", target_dir.display()))?;

    if !canonical_target.starts_with(&canonical_root) {
        return Err(format!(
            "Access denied: Working directory '{}' is outside workspace root '{}'",
            target_dir.display(),
            canonical_root.display()
        ));
    }
    Ok(canonical_target)
}

pub(crate) static FUZZY_PATH_CACHE: LazyLock<RwLock<HashMap<(PathBuf, String), (PathBuf, String)>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Searches the workspace for candidate files matching `path_input` as a relative suffix
/// or filename when an exact path lookup fails.
pub(crate) fn find_fuzzy_workspace_path(
    path_input: &str,
    workspace_root: &Path,
) -> Result<Option<(PathBuf, String)>, String> {
    let raw = Path::new(path_input);
    if raw.is_absolute() {
        return Ok(None);
    }
    let canonical_root = canonical_workspace_root(workspace_root)?;
    let cache_key = (canonical_root.clone(), path_input.to_string());
    if let Ok(guard) = FUZZY_PATH_CACHE.read() {
        if let Some((cached_abs, _)) = guard.get(&cache_key) {
            if let Ok(cached_abs) = cached_abs.canonicalize() {
                if cached_abs.starts_with(&canonical_root) && cached_abs.is_file() {
                    let cached_rel = cached_abs
                        .strip_prefix(&canonical_root)
                        .expect("checked workspace boundary")
                        .to_string_lossy()
                        .to_string();
                    return Ok(Some((cached_abs, cached_rel)));
                }
            }
        }
    }
    let mut candidates = Vec::new();

    fn scan_dir(
        root: &Path,
        current: &Path,
        target_suffix: &Path,
        target_name: Option<&std::ffi::OsStr>,
        out: &mut Vec<PathBuf>,
    ) {
        if out.len() > 10 {
            return;
        }
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name == ".git" || name == "target" || name == ".threadlane" || name == "node_modules"
            {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let Ok(path) = path.canonicalize() else {
                continue;
            };
            if !path.starts_with(root) {
                continue;
            }
            if file_type.is_dir() {
                scan_dir(root, &path, target_suffix, target_name, out);
            } else if file_type.is_file() {
                if path.ends_with(target_suffix)
                    || (target_suffix.components().count() == 1
                        && target_name.is_some_and(|n| n == name))
                {
                    out.push(path);
                }
            }
        }
    }

    let target_name = raw.file_name();
    scan_dir(
        &canonical_root,
        &canonical_root,
        raw,
        target_name,
        &mut candidates,
    );

    if candidates.len() == 1 {
        let matched = candidates.remove(0);
        let rel = matched
            .strip_prefix(&canonical_root)
            .unwrap_or(&matched)
            .to_string_lossy()
            .to_string();
        if let Ok(mut guard) = FUZZY_PATH_CACHE.write() {
            guard.insert(cache_key, (matched.clone(), rel.clone()));
        }
        Ok(Some((matched, rel)))
    } else if candidates.len() > 1 {
        let mut suggestions: Vec<String> = candidates
            .iter()
            .map(|c| {
                c.strip_prefix(&canonical_root)
                    .unwrap_or(c)
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        suggestions.sort();
        Err(format!(
            "File '{path_input}' not found. Did you mean one of: [{}]?",
            suggestions.join(", ")
        ))
    } else {
        Ok(None)
    }
}
