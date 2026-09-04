pub mod hashline;
pub mod search;
mod virtual_read;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

const READ_FILE_SNAPSHOT_PREFIX: &str = "[Threadlane read_file SHA-256: ";
const READ_FILE_SNAPSHOT_PATH_PREFIX: &str = "[Threadlane read_file path: ";

pub fn read_file_snapshot_digest(output: &str) -> Option<&str> {
    output.lines().take(2).find_map(|line| {
        let digest = line
            .strip_prefix(READ_FILE_SNAPSHOT_PREFIX)?
            .strip_suffix(']')?;
        (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then_some(digest)
    })
}

pub fn read_file_snapshot_path(output: &str) -> Option<String> {
    output.lines().take(3).find_map(|line| {
        let path = line
            .strip_prefix(READ_FILE_SNAPSHOT_PATH_PREFIX)?
            .strip_suffix(']')?;
        serde_json::from_str(path).ok()
    })
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "read_file",
            "description": "Read content of a file or virtual scheme (e.g. pr://70, mr://15, issue://12, skill://name, agent://name, or GitHub/GitLab PR/issue URL) with line numbers and hash anchors (e.g. 12:a3f|content), optionally specifying start and end lines (1-indexed).",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute or relative file path, virtual URI (pr://70, mr://15, issue://12, skill://name, agent://name), or GitHub/GitLab PR/issue URL" },
                    "start_line": { "type": "integer", "description": "Optional starting line number (1-based)" },
                    "end_line": { "type": "integer", "description": "Optional ending line number (1-based)" }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "write_file",
            "description": "Write or overwrite content to a file.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to file to write" },
                    "content": { "type": "string", "description": "Content to write into the file" }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "edit_file_hashline",
            "description": "Edit a file using hash-anchored lines obtained from read_file. Supports line and range replace, insert_after, and delete operations. Format of start_anchor/end_anchor is 'line_number:hash' (e.g. '12:a3f'). Always batch multiple edits for the same file in one tool call.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to file to edit" },
                    "edits": {
                        "type": "array",
                        "description": "List of hash-anchored edit operations to apply atomically (sorted descending automatically by start line).",
                        "items": {
                            "type": "object",
                            "properties": {
                                "start_anchor": { "type": "string", "description": "Starting line anchor formatted as 'line_number:hash' (e.g. '12:a3f')." },
                                "end_anchor": { "type": "string", "description": "Optional ending line anchor for multi-line range edits (e.g. '15:9b2'). If omitted, edit targets single start_anchor line." },
                                "action": { "type": "string", "enum": ["replace", "insert_after", "delete"], "description": "Edit action: 'replace' (replaces target line or range with new_content), 'insert_after' (inserts new_content after target line or range), or 'delete' (removes target line or range; new_content omitted/empty)." },
                                "new_content": { "type": "string", "description": "New replacement or inserted content. Omit or leave empty for 'delete' actions." }
                            },
                            "required": ["start_anchor", "action"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }
        }),
        json!({
            "name": "edit_files_hashline",
            "description": "Atomically edit multiple workspace files using hash-anchored operations. Every path and anchor is preflighted before any file changes; overlapping targets and stale anchors abort the whole transaction.",
            "parameters": {
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "description": "Files and their hashline edits to commit as one transaction.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" },
                                "edits": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "start_anchor": { "type": "string" },
                                            "end_anchor": { "type": "string" },
                                            "action": { "type": "string", "enum": ["replace", "insert_after", "delete"] },
                                            "new_content": { "type": "string" }
                                        },
                                        "required": ["start_anchor", "action"]
                                    }
                                }
                            },
                            "required": ["path", "edits"]
                        }
                    }
                },
                "required": ["files"]
            }
        }),
        json!({
            "name": "apply_workspace_edit_plan",
            "description": "Validate and atomically apply a structured LSP workspace-edit plan against current workspace files. LSP UTF-16 ranges are converted only after all files and ranges preflight successfully.",
            "parameters": {
                "type": "object",
                "properties": {
                    "plan": { "type": "object", "description": "The lsp_workspace_edit_plan returned by an LSP semantic tool." }
                },
                "required": ["plan"]
            }
        }),
        json!({
            "name": "grep_search",
            "description": "Search workspace files in-process without spawning a child process.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "glob": { "type": "string" }
                },
                "required": ["pattern"]
            }
        }),
        json!({
            "name": "list_dir",
            "description": "List files and subdirectories in a directory.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory path to list" }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "run_command",
            "description": "Run a shell command on the host system and return stdout/stderr.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to run" },
                    "cwd": { "type": "string", "description": "Working directory for the command" }
                },
                "required": ["command"]
            }
        }),
        json!({
            "name": "get_repo_map",
            "description": "Generate a compact workspace skeleton showing files, subdirectories, and top-level exported symbols (structs, functions, traits, modules) without full file bodies to save tokens.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Optional relative subdirectory path to scope the map to. Defaults to workspace root." }
                }
            }
        }),
        json!({
            "name": "manage_memory",
            "description": "Manage persistent project architectural insights, conventions, build instructions, or gotchas in .threadlane/memory.md. Actions: 'read' (reads memory.md), 'save' (saves or appends content to memory.md), 'consolidate' (consolidates structured entries under ## Architecture, ## Gotchas, ## Verification Commands).",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["read", "save", "consolidate"],
                        "description": "Memory management action: 'read', 'save', or 'consolidate'."
                    },
                    "content": {
                        "type": "string",
                        "description": "Memory note/content to write when action is 'save'."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["append", "overwrite"],
                        "description": "Mode when action is 'save': 'append' (default) adds to memory.md; 'overwrite' replaces memory.md content."
                    },
                    "architecture": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of architectural decisions or patterns to merge when action is 'consolidate'."
                    },
                    "gotchas": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of gotchas, pitfalls, or non-obvious rules to merge when action is 'consolidate'."
                    },
                    "verification": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of build, test, or verification commands to merge when action is 'consolidate'."
                    }
                },
                "required": ["action"]
            }
        }),
    ]
}

pub fn get_available_tools() -> Vec<Value> {
    tool_definitions()
        .into_iter()
        .map(|def| {
            json!({
                "type": "function",
                "function": def
            })
        })
        .collect()
}

pub fn get_codex_tools() -> Vec<Value> {
    tool_definitions()
        .into_iter()
        .map(|def| {
            let mut obj = json!({
                "type": "function"
            });
            if let Some(map) = obj.as_object_mut() {
                if let Value::Object(def_map) = def {
                    map.extend(def_map);
                }
            }
            obj
        })
        .collect()
}

/// Resolves `path_input` against `workspace_root` and rejects anything that
/// escapes the workspace, including via symlinks or `..` components.
///
/// Accepts absolute and relative inputs, and tolerates paths that do not exist
/// yet so callers can validate a write destination before creating it.
fn canonical_workspace_root(workspace_root: &Path) -> Result<PathBuf, String> {
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

fn validate_cwd_in_workspace(
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

static FUZZY_PATH_CACHE: LazyLock<RwLock<HashMap<(PathBuf, String), (PathBuf, String)>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Searches the workspace for candidate files matching `path_input` as a relative suffix
/// or filename when an exact path lookup fails.
fn find_fuzzy_workspace_path(
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

#[cfg(test)]
fn execute_tool(name: &str, args_json: &str) -> String {
    try_execute_tool(name, args_json).unwrap_or_else(|error| error)
}

#[cfg(test)]
fn execute_tool_in_workspace(name: &str, args_json: &str, workspace_root: &Path) -> String {
    try_execute_tool_in_workspace(name, args_json, workspace_root).unwrap_or_else(|error| error)
}

pub fn try_execute_tool(name: &str, args_json: &str) -> Result<String, String> {
    try_execute_tool_in_workspace(name, args_json, Path::new("."))
}

pub fn try_execute_tool_in_workspace(
    name: &str,
    args_json: &str,
    workspace_root: &Path,
) -> Result<String, String> {
    let args: Value = serde_json::from_str(args_json)
        .map_err(|error| format!("Error parsing tool arguments JSON: {error}"))?;

    match name {
        "accept_edit" => Err("Error: accept_edit is deprecated; edits are applied directly via edit_file_hashline or write_file.".to_string()),
        "grep_search" => {
            let pattern = args
                .get("pattern")
                .and_then(Value::as_str)
                .ok_or_else(|| "Error: 'pattern' parameter is required".to_string())?;
            let glob = args.get("glob").and_then(Value::as_str);
            search::grep_search(workspace_root, pattern, glob)
                .map_err(|error| format!("Error searching workspace: {error}"))
        }
        "read_file" => {
            let raw_path = match args.get("path").and_then(|v| v.as_str()) {
                Some(path) if path.strip_prefix("skill://").is_some() => {
                    let name = path.strip_prefix("skill://").expect("prefix checked");
                    return virtual_read::try_skill(workspace_root, name);
                }
                Some(path) if path.strip_prefix("agent://").is_some() => {
                    let name = path.strip_prefix("agent://").expect("prefix checked");
                    return virtual_read::try_agent(workspace_root, name);
                }
                Some(path)
                    if path.starts_with("pr://")
                        || path.starts_with("mr://")
                        || path.starts_with("issue://")
                        || path.starts_with("https://github.com/")
                        || path.starts_with("http://github.com/")
                        || path.starts_with("https://gitlab.com/")
                        || path.starts_with("http://gitlab.com/") =>
                {
                    return virtual_read::try_remote_ref_path(workspace_root, path);
                }
                Some(p) => p,
                None => return Err("Error: 'path' parameter is required".into()),
            };

            let (validated_path, auto_resolved_notice) =
                match validate_path_in_workspace(raw_path, workspace_root) {
                    Ok(p) if p.is_file() => (p, None),
                    Ok(p) => match find_fuzzy_workspace_path(raw_path, workspace_root)? {
                        Some((fuzzy_path, rel_name)) => {
                            let notice = format!(
                                "[Notice: Auto-resolved '{raw_path}' to '{rel_name}']\n"
                            );
                            (fuzzy_path, Some(notice))
                        }
                        None => (p, None),
                    },
                    Err(e) => match find_fuzzy_workspace_path(raw_path, workspace_root)? {
                        Some((fuzzy_path, rel_name)) => {
                            let notice = format!(
                                "[Notice: Auto-resolved '{raw_path}' to '{rel_name}']\n"
                            );
                            (fuzzy_path, Some(notice))
                        }
                        None => return Err(e),
                    },
                };

            let start = args.get("start_line").and_then(|v| v.as_u64()).map(|n| n as usize);
            let end = args.get("end_line").and_then(|v| v.as_u64()).map(|n| n as usize);

            let content = fs::read_to_string(&validated_path)
                .map_err(|e| format!("Error reading file '{raw_path}': {e}"))?;
            let lines: Vec<&str> = content.lines().collect();
            let start_idx = start.unwrap_or(1).saturating_sub(1);
            let end_idx = end.unwrap_or(lines.len()).min(lines.len());
            if start_idx >= lines.len() {
                return Err(format!("File only has {} lines.", lines.len()));
            }
            if end_idx <= start_idx {
                return Err(format!(
                    "Invalid line range: end_line ({}) must not be before start_line ({}).",
                    end.unwrap_or(lines.len()),
                    start.unwrap_or(1),
                ));
            }
            let selected = &lines[start_idx..end_idx];
            let formatted_lines: Vec<String> = selected
                .iter()
                .enumerate()
                .map(|(idx, line)| {
                    let line_no = start_idx + idx + 1;
                    hashline::format_line_hashline(line_no, line)
                })
                .collect();
            let body = formatted_lines.join("\n");
            let canonical_root = canonical_workspace_root(workspace_root)?;
            let snapshot_path = validated_path
                .strip_prefix(&canonical_root)
                .map_err(|_| {
                    format!(
                        "read path '{}' is outside workspace",
                        validated_path.display()
                    )
                })?
                .to_string_lossy();
            let snapshot = format!(
                "{READ_FILE_SNAPSHOT_PREFIX}{:x}]\n{READ_FILE_SNAPSHOT_PATH_PREFIX}{}]\n",
                Sha256::digest(content.as_bytes()),
                serde_json::to_string(snapshot_path.as_ref()).map_err(|error| error.to_string())?,
            );
            let output = match auto_resolved_notice {
                Some(notice) => format!("{notice}{snapshot}{body}"),
                None => format!("{snapshot}{body}"),
            };
            Ok(truncate_tool_output(&output))
        }
        "write_file" => {
            let raw_path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Error: 'path' parameter is required".to_string())?;
            let (validated_path, auto_notice) = match validate_path_in_workspace(raw_path, workspace_root) {
                Ok(p) if p.is_file() => (p, None),
                Ok(p) => match find_fuzzy_workspace_path(raw_path, workspace_root) {
                    Ok(Some((resolved, rel))) => (resolved, Some(format!("[Notice: Auto-resolved '{raw_path}' to '{rel}']\n"))),
                    _ => (p, None),
                },
                Err(err) => match find_fuzzy_workspace_path(raw_path, workspace_root) {
                    Ok(Some((resolved, rel))) => (resolved, Some(format!("[Notice: Auto-resolved '{raw_path}' to '{rel}']\n"))),
                    _ => return Err(err),
                },
            };

            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Error: 'content' parameter is required".to_string())?;

            if let Some(parent) = validated_path.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }

            fs::write(&validated_path, content)
                .map_err(|e| format!("Error writing to file '{raw_path}': {e}"))?;
            let diag = run_post_edit_diagnostics(workspace_root, raw_path);
            let notice_str = auto_notice.as_deref().unwrap_or("");
            Ok(format!(
                "{notice_str}Successfully wrote {} bytes to '{raw_path}'{diag}",
                content.len()
            ))
        }
        "edit_file_hashline" => {
            let raw_path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Error: 'path' parameter is required".to_string())?;
            let (validated_path, auto_notice) = match validate_path_in_workspace(raw_path, workspace_root) {
                Ok(p) if p.is_file() => (p, None),
                Ok(p) => match find_fuzzy_workspace_path(raw_path, workspace_root) {
                    Ok(Some((resolved, rel))) => (resolved, Some(format!("[Notice: Auto-resolved '{raw_path}' to '{rel}']\n"))),
                    Ok(None) => (p, None),
                    Err(suggestion_err) => return Err(suggestion_err),
                },
                Err(err) => match find_fuzzy_workspace_path(raw_path, workspace_root) {
                    Ok(Some((resolved, rel))) => (resolved, Some(format!("[Notice: Auto-resolved '{raw_path}' to '{rel}']\n"))),
                    Ok(None) => return Err(err),
                    Err(suggestion_err) => return Err(suggestion_err),
                },
            };

            let edits_value = args
                .get("edits")
                .ok_or_else(|| "Error: 'edits' parameter is required".to_string())?;
            let edits: Vec<hashline::HashlineEdit> = serde_json::from_value(edits_value.clone())
                .map_err(|err| format!("Error parsing 'edits' argument: {err}"))?;

            let content = fs::read_to_string(&validated_path)
                .map_err(|e| format!("Error reading file '{raw_path}': {e}"))?;
            let result = hashline::apply_hashline_edits_detailed(&content, &edits, 5)
                .map_err(|e| format!("Error applying hashline edits to '{raw_path}': {e}"))?;
            fs::write(&validated_path, result.new_content)
                .map_err(|e| format!("Error writing file '{raw_path}': {e}"))?;
            let diag = run_post_edit_diagnostics(workspace_root, raw_path);
            let diff_section = if !result.diff.is_empty() {
                format!("\n\nDiff:\n{}", result.diff)
            } else {
                String::new()
            };
            let anchors_section = if !result.updated_context.is_empty() {
                format!("\n\nUpdated Line Hashes:\n{}", result.updated_context)
            } else {
                String::new()
            };
            let notice_str = auto_notice.as_deref().unwrap_or("");
            Ok(format!(
                "{notice_str}Successfully applied {} hashline edit(s) to '{raw_path}'{diag}{diff_section}{anchors_section}",
                edits.len()
            ))
        }
        "edit_files_hashline" => {
            let files = args.get("files").and_then(Value::as_array)
                .ok_or_else(|| "Error: 'files' parameter is required".to_string())?;
            if files.is_empty() { return Err("Error: 'files' must not be empty".into()); }
            struct PlannedFile { raw_path: String, path: PathBuf, original: String, result: hashline::HashlineApplyResult }
            let mut planned = Vec::with_capacity(files.len());
            let mut seen = std::collections::HashSet::new();
            for file in files {
                let raw_path = file.get("path").and_then(Value::as_str)
                    .ok_or_else(|| "Error: every file requires 'path'".to_string())?;
                let path = match validate_path_in_workspace(raw_path, workspace_root) {
                    Ok(p) if p.is_file() => p,
                    Ok(p) => match find_fuzzy_workspace_path(raw_path, workspace_root) {
                        Ok(Some((resolved, _))) => resolved,
                        Ok(None) => p,
                        Err(suggestion_err) => return Err(suggestion_err),
                    },
                    Err(err) => match find_fuzzy_workspace_path(raw_path, workspace_root) {
                        Ok(Some((resolved, _))) => resolved,
                        Ok(None) => return Err(err),
                        Err(suggestion_err) => return Err(suggestion_err),
                    },
                };
                if !seen.insert(path.clone()) { return Err(format!("Error: duplicate transaction path '{raw_path}'")); }
                let edits: Vec<hashline::HashlineEdit> = serde_json::from_value(
                    file.get("edits").cloned().ok_or_else(|| format!("Error: '{raw_path}' requires 'edits'"))?
                ).map_err(|error| format!("Error parsing edits for '{raw_path}': {error}"))?;
                let original = fs::read_to_string(&path)
                    .map_err(|error| format!("Error reading file '{raw_path}': {error}"))?;
                let result = hashline::apply_hashline_edits_detailed(&original, &edits, 5)
                    .map_err(|error| format!("Error preflighting '{raw_path}': {error}"))?;
                planned.push(PlannedFile { raw_path: raw_path.into(), path, original, result });
            }
            let transaction = planned.iter().map(|item| (
                item.raw_path.clone(), item.path.clone(), item.original.clone(), item.result.new_content.clone()
            )).collect::<Vec<_>>();
            commit_text_transaction(&transaction)?;
            let details = planned.iter().map(|item| format!(
                "{}\nDiff:\n{}\nUpdated Line Hashes:\n{}",
                item.raw_path, item.result.diff, item.result.updated_context
            )).collect::<Vec<_>>().join("\n\n");
            Ok(format!("Successfully committed {} files atomically.\n\n{details}", planned.len()))
        }
        "apply_workspace_edit_plan" => {
            fn offset(text: &str, line: u64, character: u64) -> Result<usize, String> {
                let start = text.split_inclusive('\n').take(line as usize).map(str::len).sum::<usize>();
                let current = text.get(start..).ok_or("line is outside the document")?;
                let mut units = 0usize;
                for (byte, ch) in current.char_indices() {
                    if ch == '\n' || units == character as usize { return Ok(start + byte); }
                    units += ch.len_utf16();
                    if units > character as usize { return Err("character splits a UTF-16 code point".into()); }
                }
                if units == character as usize { Ok(text.len()) } else { Err("character is outside the line".into()) }
            }
            fn apply(text: &str, edits: &Value) -> Result<String, String> {
                let mut ranges = Vec::new();
                for edit in edits.as_array().ok_or("text_edits must be an array")? {
                    let range = edit.get("range").ok_or("text edit is missing range")?;
                    let pos = |key: &str| -> Result<usize, String> {
                        let value = range.get(key).ok_or_else(|| format!("range.{key} is missing"))?;
                        offset(text, value.get("line").and_then(Value::as_u64).ok_or("line is missing")?, value.get("character").and_then(Value::as_u64).ok_or("character is missing")?)
                    };
                    let start = pos("start")?;
                    let end = pos("end")?;
                    if start > end { return Err("text edit range is reversed".into()); }
                    ranges.push((start, end, edit.get("newText").and_then(Value::as_str).unwrap_or("").to_owned()));
                }
                ranges.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
                for pair in ranges.windows(2) { if pair[1].1 > pair[0].0 { return Err("text edit ranges overlap".into()); } }
                let mut output = text.to_owned();
                for (start, end, replacement) in ranges { output.replace_range(start..end, &replacement); }
                Ok(output)
            }
            let plan = args.get("plan").ok_or("Error: 'plan' parameter is required")?;
            if plan.get("kind").and_then(Value::as_str) != Some("lsp_workspace_edit_plan") { return Err("Error: unsupported workspace edit plan kind".into()); }
            let files = plan.get("files").and_then(Value::as_array).ok_or("Error: plan.files must be an array")?;
            let mut planned = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for file in files {
                let raw_path = file.get("path").and_then(Value::as_str).ok_or("Error: plan file path is required")?;
                let path = validate_path_in_workspace(raw_path, workspace_root)?;
                if !seen.insert(path.clone()) { return Err(format!("Error: duplicate plan path '{raw_path}'")); }
                let original = fs::read_to_string(&path).map_err(|error| format!("Error reading '{raw_path}': {error}"))?;
                let updated = apply(&original, file.get("text_edits").unwrap_or(&Value::Null)).map_err(|error| format!("Error preflighting '{raw_path}': {error}"))?;
                planned.push((raw_path.to_owned(), path, original, updated));
            }
            commit_text_transaction(&planned)?;
            Ok(format!("Successfully applied workspace edit plan to {} files atomically.", planned.len()))
        }
        "list_dir" => {
            let raw_path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let validated_path = validate_path_in_workspace(raw_path, workspace_root)?;

            let mut entries = Vec::new();
            for entry in fs::read_dir(&validated_path)
                .map_err(|e| format!("Error reading directory '{raw_path}': {e}"))?
            {
                let entry = entry.map_err(|e| {
                    format!("Error reading directory entry in '{raw_path}': {e}")
                })?;
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = entry.file_type().map_err(|e| {
                    format!("Error reading file type for '{raw_path}/{name}': {e}")
                })?.is_dir();
                let kind = if is_dir { "[DIR] " } else { "[FILE]" };
                entries.push(format!("{kind} {name}"));
            }
            entries.sort();
            Ok(truncate_tool_output(&entries.join("\n")))
        }
        "run_command" => {
            let cmd_str = args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Error: 'command' parameter is required".to_string())?;

            let trimmed_cmd = cmd_str.trim();
            let raw_cwd = args.get("cwd").and_then(|v| v.as_str());
            let validated_cwd = validate_cwd_in_workspace(raw_cwd, workspace_root)?;
            if trimmed_cmd == "dyn" || trimmed_cmd.starts_with("dyn ") {
                let dyn_args = trimmed_cmd.strip_prefix("dyn").unwrap_or("").trim();
                return execute_dyn_cli(dyn_args, &validated_cwd);
            }

            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(cmd_str);
            cmd.current_dir(&validated_cwd);

            match cmd.output() {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let rendered = truncate_tool_output(&format!(
                        "Exit Status: {}\n--- STDOUT ---\n{}\n--- STDERR ---\n{}",
                        output.status, stdout, stderr
                    ));
                    if output.status.success() {
                        Ok(rendered)
                    } else {
                        Err(rendered)
                    }
                }
                Err(e) => Err(format!("Error executing command '{cmd_str}': {e}")),
            }
        }
        "get_repo_map" => {
            let raw_path = args.get("path").and_then(|v| v.as_str());
            get_repo_map_impl(workspace_root, raw_path)
        }

        "manage_memory" => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    "Error: 'action' parameter is required ('read', 'save', 'consolidate')".to_string()
                })?;
            match action {
                "read" => read_memory_impl(workspace_root),
                "save" => save_memory_impl(workspace_root, &args),
                "consolidate" => consolidate_memory_impl(workspace_root, &args),
                unknown => Err(format!("Error: Unknown action '{unknown}' for manage_memory")),
            }
        }
        "read_memory" => read_memory_impl(workspace_root),
        "save_memory" => save_memory_impl(workspace_root, &args),
        "consolidate_memory" => consolidate_memory_impl(workspace_root, &args),
        unknown => Err(format!("Error: Unknown tool '{unknown}'")),
    }
}

/// Dispatches an in-process CLI tool invocation via `dyn <tool> [args]`.
fn execute_dyn_cli(input: &str, workspace_root: &Path) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() || input == "--help" || input == "-h" {
        let mut lines = vec![
            "Threadlane In-Process Tool Runner (dyn)".to_string(),
            "Usage: dyn <tool_name> [json_args] | dyn <tool_name> --help".to_string(),
            "".to_string(),
            "Available auxiliary tools:".to_string(),
        ];
        for def in tool_definitions() {
            let name = def.get("name").and_then(Value::as_str).unwrap_or("");
            let desc = def.get("description").and_then(Value::as_str).unwrap_or("");
            let first_sentence = desc.split('.').next().unwrap_or(desc).trim();
            lines.push(format!("  {:<26} {}", name, first_sentence));
        }
        lines.push("".to_string());
        lines.push("Tip: Core tools (read_file, edit_file_hashline, write_file, run_command, subagent) are in active schema.".to_string());
        return Ok(format!(
            "Exit Status: exit status: 0\n--- STDOUT ---\n{}\n--- STDERR ---",
            lines.join("\n")
        ));
    }

    let mut parts = input.split_whitespace();
    let tool_name = parts.next().unwrap_or("");
    let remaining = input[tool_name.len()..].trim();

    if remaining == "--help" || remaining == "-h" {
        for def in tool_definitions() {
            if def.get("name").and_then(Value::as_str) == Some(tool_name) {
                let formatted = serde_json::to_string_pretty(&def).unwrap_or_default();
                return Ok(format!("Exit Status: exit status: 0\n--- STDOUT ---\nTool Schema for '{tool_name}':\n{formatted}\n--- STDERR ---"));
            }
        }
        return Err(format!(
            "Unknown tool '{tool_name}'. Run 'dyn' to list tools."
        ));
    }

    let args_json = if remaining.starts_with('{') {
        remaining.to_string()
    } else if remaining.is_empty() {
        "{}".to_string()
    } else {
        return Err("dyn requires JSON arguments as an object".into());
    };

    let result = try_execute_tool_in_workspace(tool_name, &args_json, workspace_root)?;
    Ok(format!(
        "Exit Status: exit status: 0\n--- STDOUT ---\n{result}\n--- STDERR ---"
    ))
}

const MAX_TOOL_OUTPUT_CHARS: usize = 3_000;
const TRUNCATE_HEAD_CHARS: usize = 1_200;
const TRUNCATE_TAIL_CHARS: usize = 1_200;

fn truncate_tool_output(output: &str) -> String {
    let output_chars = output.chars().count();
    if output_chars <= MAX_TOOL_OUTPUT_CHARS {
        output.to_string()
    } else {
        let head: String = output.chars().take(TRUNCATE_HEAD_CHARS).collect();
        let tail_chars: Vec<char> = output.chars().rev().take(TRUNCATE_TAIL_CHARS).collect();
        let tail: String = tail_chars.into_iter().rev().collect();
        let hidden = output_chars.saturating_sub(TRUNCATE_HEAD_CHARS + TRUNCATE_TAIL_CHARS);
        format!(
            "{head}\n\n[... Output truncated: {hidden} characters hidden to reduce token explosion ...]\n\n{tail}"
        )
    }
}

fn read_memory_impl(workspace_root: &Path) -> Result<String, String> {
    let mem_file = workspace_root.join(".threadlane").join("memory.md");
    if mem_file.is_file() {
        fs::read_to_string(&mem_file)
            .map(|content| truncate_tool_output(&content))
            .map_err(|e| format!("Error reading .threadlane/memory.md: {e}"))
    } else {
        Ok("No persistent memory found in .threadlane/memory.md yet.".to_string())
    }
}

fn save_memory_impl(workspace_root: &Path, args: &Value) -> Result<String, String> {
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Error: 'content' parameter is required".to_string())?;
    let mode = args
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("append");

    let dir = workspace_root.join(".threadlane");
    fs::create_dir_all(&dir).map_err(|e| format!("Error creating .threadlane directory: {e}"))?;
    let mem_file = dir.join("memory.md");

    let new_content = if mode == "overwrite" || !mem_file.exists() {
        content.trim().to_string()
    } else {
        let existing = fs::read_to_string(&mem_file)
            .map_err(|e| format!("Error reading .threadlane/memory.md: {e}"))?;
        format!("{}\n\n{}", existing.trim(), content.trim())
    };

    fs::write(&mem_file, new_content)
        .map(|_| "Successfully saved memory to .threadlane/memory.md".to_string())
        .map_err(|e| format!("Error writing to .threadlane/memory.md: {e}"))
}

fn consolidate_memory_impl(workspace_root: &Path, args: &Value) -> Result<String, String> {
    let parse_array = |key: &str| -> Vec<String> {
        args.get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    };

    let architecture = parse_array("architecture");
    let gotchas = parse_array("gotchas");
    let verification = parse_array("verification");

    let dir = workspace_root.join(".threadlane");
    fs::create_dir_all(&dir).map_err(|e| format!("Error creating .threadlane directory: {e}"))?;
    let mem_file = dir.join("memory.md");

    let existing = if mem_file.is_file() {
        fs::read_to_string(&mem_file)
            .map_err(|e| format!("Error reading .threadlane/memory.md: {e}"))?
    } else {
        String::new()
    };
    let merged = consolidate_memory_entries(&existing, &architecture, &gotchas, &verification);

    fs::write(&mem_file, merged)
        .map(|_| "Successfully consolidated memory entries in .threadlane/memory.md".to_string())
        .map_err(|e| format!("Error writing to .threadlane/memory.md: {e}"))
}

fn consolidate_memory_entries(
    existing: &str,
    architecture: &[String],
    gotchas: &[String],
    verification: &[String],
) -> String {
    fn append_section(existing: &str, heading: &str, items: &[String]) -> String {
        let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
        let section_range = lines
            .iter()
            .position(|line| line.trim() == heading)
            .map(|start| {
                let end = lines
                    .iter()
                    .enumerate()
                    .skip(start + 1)
                    .find(|(_, line)| line.trim().starts_with('#'))
                    .map(|(index, _)| index)
                    .unwrap_or(lines.len());
                (start, end)
            });
        let existing_items: Vec<String> = section_range
            .map(|(start, end)| {
                lines[start + 1..end]
                    .iter()
                    .filter_map(|line| {
                        let trimmed = line.trim();
                        trimmed
                            .strip_prefix("- ")
                            .or_else(|| trimmed.strip_prefix("* "))
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let items: Vec<String> = items
            .iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .filter(|item| !existing_items.contains(item))
            .collect();
        if items.is_empty() {
            return existing.to_string();
        }

        if let Some((_, end)) = section_range {
            lines.splice(end..end, items.into_iter().map(|item| format!("- {item}")));
        } else {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(heading.to_string());
            lines.extend(items.into_iter().map(|item| format!("- {item}")));
        }
        lines.join("\n")
    }

    let mut out = existing.trim().to_string();
    if out.is_empty() {
        out = "# Project Memory".to_string();
    }
    out = append_section(&out, "## Architecture", architecture);
    out = append_section(&out, "## Gotchas", gotchas);
    append_section(&out, "## Verification Commands", verification)
}

fn get_repo_map_impl(workspace_root: &Path, rel_path: Option<&str>) -> Result<String, String> {
    let target_dir = match rel_path {
        Some(path) => validate_path_in_workspace(path, workspace_root)?,
        None => workspace_root.to_path_buf(),
    };

    let mut lines = Vec::new();
    walk_repo_skeleton(&target_dir, workspace_root, 0, &mut lines)?;

    if lines.is_empty() {
        Ok("No source code definitions found in repository map.".to_string())
    } else {
        Ok(truncate_tool_output(&lines.join("\n")))
    }
}

fn walk_repo_skeleton(
    dir: &Path,
    root: &Path,
    depth: usize,
    out: &mut Vec<String>,
) -> Result<(), String> {
    if depth > 4 {
        return Ok(());
    }
    let entries = fs::read_dir(dir)
        .map_err(|e| format!("Error reading directory '{}': {e}", dir.display()))?;
    let mut sorted_entries = Vec::new();
    for entry in entries {
        sorted_entries.push(
            entry.map_err(|e| {
                format!("Error reading directory entry in '{}': {e}", dir.display())
            })?,
        );
    }
    sorted_entries.sort_by_key(|e| e.file_name());

    for entry in sorted_entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Error reading file type for '{}': {e}", path.display()))?;
        if file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.')
            || name == "target"
            || name == "node_modules"
            || name == "dist"
            || name == "packaging"
        {
            continue;
        }

        if file_type.is_dir() {
            walk_repo_skeleton(&path, root, depth + 1, out)?;
        } else if file_type.is_file() {
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if matches!(ext, "rs" | "py" | "js" | "ts" | "go" | "toml") {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                let content = fs::read_to_string(&path)
                    .map_err(|e| format!("Error reading source file '{}': {e}", path.display()))?;

                let mut symbols = Vec::new();
                for (idx, line) in content.lines().enumerate() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("pub fn ")
                        || trimmed.starts_with("pub struct ")
                        || trimmed.starts_with("pub enum ")
                        || trimmed.starts_with("pub trait ")
                        || trimmed.starts_with("pub mod ")
                        || trimmed.starts_with("pub type ")
                        || trimmed.starts_with("pub const ")
                        || trimmed.starts_with("fn ")
                        || trimmed.starts_with("struct ")
                        || trimmed.starts_with("enum ")
                        || trimmed.starts_with("trait ")
                        || trimmed.starts_with("mod ")
                        || trimmed.starts_with("impl ")
                        || trimmed.starts_with("class ")
                        || trimmed.starts_with("def ")
                        || (ext == "toml" && trimmed.starts_with('[') && trimmed.ends_with(']'))
                    {
                        let sig = if trimmed.len() > 100 {
                            format!("{}...", &trimmed[..97])
                        } else {
                            trimmed.to_string()
                        };
                        symbols.push(format!("  L{}: {sig}", idx + 1));
                    }
                }

                if !symbols.is_empty() {
                    out.push(format!("{}", rel.display()));
                    out.extend(symbols);
                }
            }
        }
    }
    Ok(())
}

fn path_matches(file_name: &str, target_path: &str) -> bool {
    let file_clean = file_name.replace('\\', "/");
    let file_trimmed = file_clean.strip_prefix("./").unwrap_or(&file_clean);
    let target_clean = target_path.replace('\\', "/");
    let target_trimmed = target_clean.strip_prefix("./").unwrap_or(&target_clean);

    if file_trimmed == target_trimmed {
        return true;
    }

    if target_trimmed.ends_with(&format!("/{file_trimmed}")) {
        return true;
    }

    if file_trimmed.ends_with(&format!("/{target_trimmed}")) {
        return true;
    }

    false
}

fn commit_text_transaction(files: &[(String, PathBuf, String, String)]) -> Result<(), String> {
    use std::io::Write;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let mut staged = Vec::with_capacity(files.len());
    for (index, (raw_path, path, original, updated)) in files.iter().enumerate() {
        let current = fs::read_to_string(path)
            .map_err(|error| format!("Error re-reading '{raw_path}': {error}"))?;
        if &current != original {
            for (stage, _) in &staged {
                let _ = fs::remove_file(stage);
            }
            return Err(format!(
                "Error: '{raw_path}' changed after preflight; transaction aborted"
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| format!("Invalid path '{raw_path}'"))?;
        let stem = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file");
        let stage = parent.join(format!(".{stem}.threadlane-{nonce}-{index}.stage"));
        let backup = parent.join(format!(".{stem}.threadlane-{nonce}-{index}.backup"));
        let mut handle = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)
            .map_err(|error| format!("Error staging '{raw_path}': {error}"))?;
        handle
            .write_all(updated.as_bytes())
            .map_err(|error| format!("Error staging '{raw_path}': {error}"))?;
        handle
            .sync_all()
            .map_err(|error| format!("Error syncing '{raw_path}': {error}"))?;
        fs::set_permissions(
            &stage,
            fs::metadata(path)
                .map_err(|error| error.to_string())?
                .permissions(),
        )
        .map_err(|error| format!("Error preserving permissions for '{raw_path}': {error}"))?;
        staged.push((stage, backup));
    }
    let mut committed: Vec<usize> = Vec::new();
    for (index, ((raw_path, path, _, _), (stage, backup))) in
        files.iter().zip(staged.iter()).enumerate()
    {
        if let Err(error) = fs::rename(path, backup).and_then(|_| fs::rename(stage, path)) {
            if !path.exists() && backup.exists() {
                let _ = fs::rename(backup, path);
            }
            for old in committed.into_iter().rev() {
                let (_, old_path, _, _) = &files[old];
                let (_, old_backup) = &staged[old];
                let _ = fs::remove_file(old_path);
                let _ = fs::rename(old_backup, old_path);
            }
            for (pending_stage, _) in &staged[index..] {
                let _ = fs::remove_file(pending_stage);
            }
            return Err(format!(
                "Error committing '{raw_path}': {error}; transaction rolled back"
            ));
        }
        committed.push(index);
    }
    for (_, backup) in staged {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

fn run_post_edit_diagnostics(workspace_root: &Path, raw_path: &str) -> String {
    if !raw_path.ends_with(".rs") {
        return String::new();
    }

    let mut cmd = Command::new("cargo");
    cmd.arg("check")
        .arg("--message-format=json")
        .current_dir(workspace_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let output = match cmd.output() {
        Ok(o) => o,
        Err(_) => return String::new(),
    };

    let stdout_str = String::from_utf8_lossy(&output.stdout);

    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    for line in stdout_str.lines() {
        let Ok(val) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        if val.get("reason").and_then(|v| v.as_str()) != Some("compiler-message") {
            continue;
        }

        let Some(msg) = val.get("message") else {
            continue;
        };

        let level = msg.get("level").and_then(|v| v.as_str()).unwrap_or("info");
        let text_msg = msg.get("message").and_then(|v| v.as_str()).unwrap_or("");
        let spans = msg.get("spans").and_then(|v| v.as_array());

        let mut matched = false;
        let mut line_no = 0;
        let mut col_no = 0;

        if let Some(spans) = spans {
            for span in spans {
                if let Some(file_name) = span.get("file_name").and_then(|v| v.as_str()) {
                    if path_matches(file_name, raw_path) {
                        matched = true;
                        line_no = span.get("line_start").and_then(|v| v.as_u64()).unwrap_or(0);
                        col_no = span
                            .get("column_start")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        break;
                    }
                }
            }
        }

        if matched {
            if level == "error" {
                errors.push(format!(
                    "- [ERROR] Line {line_no}, Col {col_no}: {text_msg}"
                ));
            } else if level == "warning" {
                warnings.push(format!(
                    "- [WARNING] Line {line_no}, Col {col_no}: {text_msg}"
                ));
            }
        }
    }

    if errors.is_empty() && warnings.is_empty() {
        "\n\n[LSP Diagnostics Post-Check]\n✓ Clean (0 errors, 0 warnings)".to_string()
    } else {
        let mut res = format!(
            "\n\n[LSP Diagnostics Post-Check]\n⚠ Found {} error(s), {} warning(s):",
            errors.len(),
            warnings.len()
        );
        for err in errors.iter().take(10) {
            res.push('\n');
            res.push_str(err);
        }
        for warn in warnings.iter().take(10) {
            res.push('\n');
            res.push_str(warn);
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn canonical_workspace_root_reuses_successful_resolution() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let expected = root.canonicalize().unwrap();
        assert_eq!(canonical_workspace_root(&root).unwrap(), expected);
        std::fs::remove_dir(&root).unwrap();
        assert_eq!(canonical_workspace_root(&root).unwrap(), expected);
    }

    #[test]
    fn validate_path_allows_a_new_absolute_destination_under_a_symlinked_root() {
        // `tempdir()` lives under a symlinked prefix on macOS (`/var` ->
        // `/private/var`), which is exactly the shape a caller sends back after
        // being handed a non-canonical workspace path.
        let dir = tempdir().unwrap();
        let root = dir.path();
        let target = root.join("nested").join("new.txt");

        let resolved = validate_path_in_workspace(&target.to_string_lossy(), root)
            .expect("a new file inside the workspace must be allowed");

        let canonical_root = root.canonicalize().unwrap();
        assert!(resolved.starts_with(&canonical_root));
        assert!(resolved.ends_with("nested/new.txt"));
    }

    #[test]
    fn validate_path_resolves_existing_and_new_paths_to_the_same_root() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("exists.txt"), "x").unwrap();

        let existing =
            validate_path_in_workspace(&root.join("exists.txt").to_string_lossy(), root).unwrap();
        let new =
            validate_path_in_workspace(&root.join("new.txt").to_string_lossy(), root).unwrap();

        assert_eq!(existing.parent(), new.parent());
    }

    #[test]
    fn validate_path_still_denies_escapes_for_paths_that_do_not_exist() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();

        let relative = validate_path_in_workspace("../escaped.txt", dir.path());
        assert!(relative.is_err(), "got: {relative:?}");

        let absolute = validate_path_in_workspace(
            &outside.path().join("new.txt").to_string_lossy(),
            dir.path(),
        );
        assert!(absolute.is_err(), "got: {absolute:?}");
    }

    #[cfg(unix)]
    #[test]
    fn fuzzy_path_ignores_symlinks_outside_the_workspace() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.rs"), "secret").unwrap();
        symlink(
            outside.path().join("secret.rs"),
            root.path().join("secret.rs"),
        )
        .unwrap();

        assert_eq!(
            find_fuzzy_workspace_path("secret.rs", root.path()).unwrap(),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn fuzzy_path_rechecks_cached_symlink_matches() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let linked = root.path().join("secret.rs");
        fs::write(outside.path().join("secret.rs"), "secret").unwrap();
        symlink(outside.path().join("secret.rs"), &linked).unwrap();
        let canonical_root = canonical_workspace_root(root.path()).unwrap();
        FUZZY_PATH_CACHE.write().unwrap().insert(
            (canonical_root, "secret.rs".into()),
            (linked, "secret.rs".into()),
        );

        assert_eq!(
            find_fuzzy_workspace_path("secret.rs", root.path()).unwrap(),
            None
        );
    }

    #[test]
    fn test_run_post_edit_diagnostics_non_rust_file() {
        let dir = tempdir().unwrap();
        let res = run_post_edit_diagnostics(dir.path(), "readme.txt");
        assert_eq!(res, "");
    }

    #[test]
    fn rust_write_surfaces_compile_diagnostics_in_the_same_tool_result() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"diagnostic-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let result = execute_tool_in_workspace(
            "write_file",
            &serde_json::json!({
                "path": "src/lib.rs",
                "content": "pub fn broken() { let _: = 1; }"
            })
            .to_string(),
            dir.path(),
        );
        assert!(result.starts_with("Successfully wrote"));
        assert!(result.contains("[LSP Diagnostics Post-Check]"));
        assert!(result.contains("Found 1 error(s)"), "{result}");
        assert!(result.contains("Line 1"), "{result}");
    }

    #[test]
    fn test_path_matches_normalization() {
        assert!(path_matches("src/main.rs", "./src/main.rs"));
        assert!(path_matches("./src/main.rs", "src/main.rs"));
        assert!(path_matches("crates/threadlane/src/main.rs", "src/main.rs"));
        assert!(path_matches(
            "src/main.rs",
            "/Users/foo/project/src/main.rs"
        ));
        assert!(!path_matches("src/other_main.rs", "main.rs"));
    }

    #[test]
    fn test_list_dir_tool() {
        let res = execute_tool("list_dir", r#"{"path": "."}"#);
        assert!(res.contains("Cargo.toml"));
    }

    #[test]
    fn list_dir_returns_typed_error_for_regular_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file.txt"), "content").unwrap();

        let result =
            try_execute_tool_in_workspace("list_dir", r#"{"path":"file.txt"}"#, dir.path());

        assert!(result.is_err());
    }

    #[test]
    fn list_dir_preserves_successful_output() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("alpha")).unwrap();
        fs::write(dir.path().join("beta.txt"), "content").unwrap();

        let typed =
            try_execute_tool_in_workspace("list_dir", r#"{"path":"."}"#, dir.path()).unwrap();
        let wrapped = execute_tool_in_workspace("list_dir", r#"{"path":"."}"#, dir.path());

        assert_eq!(typed, "[DIR]  alpha\n[FILE] beta.txt");
        assert_eq!(wrapped, typed);
    }
    #[test]
    fn edit_files_hashline_is_atomic_on_stale_anchor() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "three\nfour\n").unwrap();
        let a_anchor = hashline::format_line_hashline(1, "one")
            .split('|')
            .next()
            .unwrap()
            .to_string();
        let args = serde_json::json!({"files": [
            {"path":"a.txt","edits":[{"start_anchor":a_anchor,"action":"replace","new_content":"changed"}]},
            {"path":"b.txt","edits":[{"start_anchor":"1:bad","action":"replace","new_content":"broken"}]}
        ]});
        let result =
            try_execute_tool_in_workspace("edit_files_hashline", &args.to_string(), dir.path());
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\ntwo\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "three\nfour\n"
        );
    }

    #[test]
    fn edit_files_hashline_commits_all_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        let anchor = |line: &str| {
            hashline::format_line_hashline(1, line)
                .split('|')
                .next()
                .unwrap()
                .to_string()
        };
        let args = serde_json::json!({"files": [
            {"path":"a.txt","edits":[{"start_anchor":anchor("one"),"action":"replace","new_content":"first"}]},
            {"path":"b.txt","edits":[{"start_anchor":anchor("two"),"action":"replace","new_content":"second"}]}
        ]});
        try_execute_tool_in_workspace("edit_files_hashline", &args.to_string(), dir.path())
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "first\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "second\n"
        );
    }

    #[test]
    fn apply_workspace_edit_plan_handles_utf16_and_is_atomic() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let rocket = \"🚀\";\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "rocket();\n").unwrap();
        let plan = serde_json::json!({"kind":"lsp_workspace_edit_plan","version":1,"files":[
            {"path":"a.rs","text_edits":[{"range":{"start":{"line":0,"character":4},"end":{"line":0,"character":10}},"newText":"ship"}]},
            {"path":"b.rs","text_edits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":6}},"newText":"ship"}]}
        ]});
        try_execute_tool_in_workspace(
            "apply_workspace_edit_plan",
            &serde_json::json!({"plan":plan}).to_string(),
            dir.path(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "let ship = \"🚀\";\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.rs")).unwrap(),
            "ship();\n"
        );
    }

    #[test]
    fn apply_workspace_edit_plan_preflights_every_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "old\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "old\n").unwrap();
        let plan = serde_json::json!({"kind":"lsp_workspace_edit_plan","files":[
            {"path":"a.rs","text_edits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":3}},"newText":"new"}]},
            {"path":"b.rs","text_edits":[{"range":{"start":{"line":9,"character":0},"end":{"line":9,"character":1}},"newText":"bad"}]}
        ]});
        assert!(try_execute_tool_in_workspace(
            "apply_workspace_edit_plan",
            &serde_json::json!({"plan":plan}).to_string(),
            dir.path()
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "old\n"
        );
    }

    #[test]
    fn test_edit_file_hashline_schema_description() {
        let tools = get_available_tools();
        let hashline_tool = tools
            .iter()
            .find(|t| t["function"]["name"] == "edit_file_hashline")
            .expect("edit_file_hashline tool should exist");

        let desc = hashline_tool["function"]["description"].as_str().unwrap();
        assert!(
            desc.contains("Supports line and range replace, insert_after, and delete operations")
        );
        assert!(desc.contains("Always batch multiple edits for the same file in one tool call"));

        let params = &hashline_tool["function"]["parameters"]["properties"];
        let start_anchor_desc = params["edits"]["items"]["properties"]["start_anchor"]
            ["description"]
            .as_str()
            .unwrap();
        assert!(start_anchor_desc.contains("formatted as 'line_number:hash'"));

        let end_anchor_desc = params["edits"]["items"]["properties"]["end_anchor"]["description"]
            .as_str()
            .unwrap();
        assert!(end_anchor_desc.contains("multi-line range edits"));

        let action_desc = params["edits"]["items"]["properties"]["action"]["description"]
            .as_str()
            .unwrap();
        assert!(action_desc.contains("'replace'"));
        assert!(action_desc.contains("'insert_after'"));
        assert!(action_desc.contains("'delete'"));

        let new_content_desc = params["edits"]["items"]["properties"]["new_content"]["description"]
            .as_str()
            .unwrap();
        assert!(new_content_desc.contains("Omit or leave empty for 'delete' actions"));
    }

    #[test]
    fn test_workspace_containment_read_escape() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let res = execute_tool_in_workspace("read_file", r#"{"path": "../secret.txt"}"#, root);
        assert!(res.contains("Access denied"));
    }

    #[test]
    fn test_workspace_containment_command_cwd_escape() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let res =
            execute_tool_in_workspace("run_command", r#"{"command": "ls", "cwd": "/tmp"}"#, root);
        assert!(res.contains("Access denied"));
    }

    #[test]
    fn test_read_file_rejects_reversed_line_range_without_panicking() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        fs::write(&file, "one\ntwo\nthree\n").unwrap();

        let res = execute_tool_in_workspace(
            "read_file",
            r#"{"path": "sample.txt", "start_line": 3, "end_line": 2}"#,
            dir.path(),
        );

        assert_eq!(
            res,
            "Invalid line range: end_line (2) must not be before start_line (3)."
        );
    }

    #[test]
    fn test_save_and_read_memory_tool() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let initial_read = execute_tool_in_workspace("read_memory", "{}", root);
        assert!(initial_read.contains("No persistent memory found"));

        let payload =
            json!({"content": "## Architectural Decision\nUse GPUI with threadlane state."})
                .to_string();
        let save_res = execute_tool_in_workspace("save_memory", &payload, root);
        assert!(save_res.contains("Successfully saved memory"));

        let read_res = execute_tool_in_workspace("read_memory", "{}", root);
        assert!(read_res.contains("Use GPUI with threadlane state."));
    }

    #[test]
    fn test_save_memory_fails_on_existing_non_utf8_file_and_preserves_bytes() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let memory_dir = root.join(".threadlane");
        fs::create_dir_all(&memory_dir).unwrap();
        let memory_file = memory_dir.join("memory.md");
        let original_bytes: Vec<u8> = vec![0xff, 0x00, 0xfe, b'a', b'b'];
        fs::write(&memory_file, &original_bytes).unwrap();

        let payload = json!({"content": "## Append Attempt"}).to_string();
        let typed = try_execute_tool_in_workspace("save_memory", &payload, root)
            .expect_err("Expected invalid UTF-8 memory file to produce read error");
        assert!(typed.starts_with("Error reading .threadlane/memory.md:"));

        let wrapped = execute_tool_in_workspace("save_memory", &payload, root);
        assert_eq!(wrapped, typed);

        let on_disk = fs::read(&memory_file).unwrap();
        assert_eq!(on_disk, original_bytes);
    }

    #[test]
    fn test_get_repo_map_tool() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let src_dir = root.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let rs_file = src_dir.join("main.rs");
        fs::write(
            &rs_file,
            "pub struct AppState {}\npub fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        let map_res = execute_tool_in_workspace("get_repo_map", "{}", root);
        assert!(map_res.contains("src/main.rs"));
        assert!(map_res.contains("pub struct AppState"));
        assert!(map_res.contains("pub fn main()"));
    }

    #[test]
    fn get_repo_map_returns_typed_error_for_regular_file() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("not-a-directory.rs"),
            "pub fn visible() {}\n",
        )
        .unwrap();

        let args = r#"{"path":"not-a-directory.rs"}"#;
        let error = try_execute_tool_in_workspace("get_repo_map", args, dir.path())
            .expect_err("regular files must not produce a successful map");

        assert_eq!(
            execute_tool_in_workspace("get_repo_map", args, dir.path()),
            error
        );
    }

    #[test]
    fn get_repo_map_returns_typed_error_when_source_file_cannot_be_read_as_text() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("invalid.rs"), [0xff, 0xfe]).unwrap();

        let result = try_execute_tool_in_workspace("get_repo_map", "{}", dir.path());

        assert!(result.is_err(), "source read failures must not be skipped");
    }

    #[test]
    fn get_repo_map_preserves_successful_output() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "pub struct App {}\nfn helper() {}\n",
        )
        .unwrap();

        let typed = try_execute_tool_in_workspace("get_repo_map", "{}", dir.path()).unwrap();
        let wrapped = execute_tool_in_workspace("get_repo_map", "{}", dir.path());

        assert_eq!(
            typed,
            "src/lib.rs\n  L1: pub struct App {}\n  L2: fn helper() {}"
        );
        assert_eq!(wrapped, typed);
    }

    #[test]
    fn test_truncate_tool_output() {
        let long_string = "a".repeat(5000);
        let truncated = truncate_tool_output(&long_string);
        assert!(truncated.contains("[... Output truncated:"));
        assert!(truncated.len() < 3000);
    }

    #[test]
    fn test_truncate_tool_output_uses_characters_for_unicode() {
        let output = "😀".repeat(2_000);
        let truncated = truncate_tool_output(&output);

        assert_eq!(truncated, output);
    }

    #[cfg(unix)]
    #[test]
    fn test_code_tools_skip_symlinked_directories_outside_workspace() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(
            outside.path().join("secret.rs"),
            "pub fn external_secret_symbol() {}\n",
        )
        .unwrap();
        symlink(outside.path(), dir.path().join("linked")).unwrap();

        let map = execute_tool_in_workspace("get_repo_map", "{}", dir.path());

        assert!(!map.contains("external_secret_symbol"));
    }

    #[test]
    fn test_consolidate_memory_preserves_unmanaged_content() {
        let existing = "# Project Memory\n\nPersonal notes with\nmultiple lines.\n\n## Other Notes\n- Keep this.\n\n## Architecture\n- Existing architecture\n";
        assert_eq!(
            consolidate_memory_entries(existing, &[], &[], &[]),
            existing.trim()
        );
        let merged =
            consolidate_memory_entries(existing, &["New architecture".to_string()], &[], &[]);

        assert!(merged.contains("Personal notes with\nmultiple lines."));
        assert!(merged.contains("## Other Notes\n- Keep this."));
        assert!(merged.contains("- Existing architecture"));
        assert!(merged.contains("- New architecture"));
    }

    #[test]
    fn test_manage_memory_tool() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let save_payload = json!({
            "action": "save",
            "content": "Rule: Always check cargo diff"
        })
        .to_string();
        let save_res = execute_tool_in_workspace("manage_memory", &save_payload, root);
        assert!(save_res.contains("Successfully saved memory to .threadlane/memory.md"));

        let read_payload = json!({ "action": "read" }).to_string();
        let read_res = execute_tool_in_workspace("manage_memory", &read_payload, root);
        assert!(read_res.contains("Rule: Always check cargo diff"));

        let consolidate_payload = json!({
            "action": "consolidate",
            "architecture": ["Use GPUI UI components"],
            "gotchas": ["cargo check requires unsandboxed bypass on macOS"],
            "verification": ["cargo test --workspace"]
        })
        .to_string();

        let res = execute_tool_in_workspace("manage_memory", &consolidate_payload, root);
        assert!(res.contains("Successfully consolidated memory entries"));

        let mem_content = execute_tool_in_workspace("manage_memory", &read_payload, root);
        assert!(mem_content.contains("## Architecture"));
        assert!(mem_content.contains("Use GPUI UI components"));

        assert!(mem_content.contains("## Gotchas"));
        assert!(mem_content.contains("cargo check requires unsandboxed bypass on macOS"));
        assert!(mem_content.contains("## Verification Commands"));
        assert!(mem_content.contains("cargo test --workspace"));
    }

    #[test]
    fn test_read_file_virtual_schemes_and_urls() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Test skill:// discovery in .agents/skills/<name>/SKILL.md
        let skill_dir = root.join(".agents/skills/ponytail");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "ponytail instructions").unwrap();

        let read_skill_payload = json!({ "path": "skill://ponytail" }).to_string();
        let skill_res = execute_tool_in_workspace("read_file", &read_skill_payload, root);
        assert_eq!(skill_res, "ponytail instructions");

        // Test PR URL parsing error format when gh CLI or git remote is missing
        let pr_url_payload =
            json!({ "path": "https://github.com/wheregmis/threadlane/pull/70" }).to_string();
        let pr_res = execute_tool_in_workspace("read_file", &pr_url_payload, root);
        assert!(
            pr_res.contains("pr://70")
                || pr_res.contains("\"number\": 70")
                || pr_res.contains("https://github.com/"),
            "unexpected PR response: {pr_res}"
        );

        // Test GitLab MR URL parsing error format when glab CLI or git remote is missing
        let mr_url_payload =
            json!({ "path": "https://gitlab.com/gitlab-org/gitlab/-/merge_requests/99" })
                .to_string();
        let mr_res = execute_tool_in_workspace("read_file", &mr_url_payload, root);
        assert!(
            mr_res.contains("mr://99")
                || mr_res.contains("GitLab mr #99")
                || mr_res.contains("gitlab.com")
        );
    }
    #[test]
    fn typed_execution_marks_hashline_mismatch_as_error() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("sample.txt"), "original\n").unwrap();
        let result = try_execute_tool_in_workspace(
            "edit_file_hashline",
            &serde_json::json!({
                "path": "sample.txt",
                "edits": [{
                    "start_anchor": "1:bad",
                    "action": "replace",
                    "new_content": "changed"
                }]
            })
            .to_string(),
            dir.path(),
        );

        let error = result.expect_err("a stale hashline anchor must fail");
        assert!(error.contains("Error applying hashline edits"), "{error}");
        assert!(error.contains("Hashline mismatch"), "{error}");
        assert_eq!(
            fs::read_to_string(dir.path().join("sample.txt")).unwrap(),
            "original\n"
        );
    }

    #[test]
    fn typed_execution_marks_invalid_arguments_as_error() {
        let result = try_execute_tool("read_file", "{}");
        assert_eq!(result, Err("Error: 'path' parameter is required".into()));
    }

    #[test]
    fn typed_execution_marks_nonzero_command_exit_as_error() {
        let dir = tempdir().unwrap();
        let result = try_execute_tool_in_workspace(
            "run_command",
            r#"{"command":"printf 'out'; printf 'err' >&2; exit 7"}"#,
            dir.path(),
        );

        let error = result.expect_err("a non-zero command exit must fail");
        assert!(error.contains("Exit Status: exit status: 7"), "{error}");
        assert!(error.contains(&format!("--- STDOUT ---\nout")), "{error}");
        assert!(error.contains(&format!("--- STDERR ---\nerr")), "{error}");
    }

    #[test]
    fn typed_execution_keeps_successful_calls_successful() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("sample.txt"), "contents").unwrap();

        let read =
            try_execute_tool_in_workspace("read_file", r#"{"path":"sample.txt"}"#, dir.path())
                .unwrap();
        fs::write(dir.path().join("sample.txt"), "changed after read").unwrap();
        assert_eq!(
            read_file_snapshot_digest(&read),
            Some("d1b2a59fbea7e20077af9f91b27e95e865061b270be03ff539ab3b73587882e8")
        );

        let command =
            try_execute_tool_in_workspace("run_command", r#"{"command":"printf ok"}"#, dir.path());
        assert!(command.is_ok(), "{command:?}");
    }

    #[test]
    fn typed_execution_marks_unavailable_virtual_skill_as_error_without_changing_output() {
        let dir = tempdir().unwrap();
        let args = r#"{"path":"skill://definitely-not-installed-review-fixture"}"#;
        let expected = "Unknown skill reference 'definitely-not-installed-review-fixture': No skill file found in workspace or user skills directories";

        assert_eq!(
            try_execute_tool_in_workspace("read_file", args, dir.path()),
            Err(expected.to_string())
        );
        assert_eq!(
            execute_tool_in_workspace("read_file", args, dir.path()),
            expected
        );
    }

    #[test]
    fn typed_execution_marks_malformed_virtual_skill_as_error_without_changing_output() {
        let dir = tempdir().unwrap();
        let args = r#"{"path":"skill://"}"#;
        let expected = "Error: 'skill://' reference requires a skill name";

        assert_eq!(
            try_execute_tool_in_workspace("read_file", args, dir.path()),
            Err(expected.to_string())
        );
        assert_eq!(
            execute_tool_in_workspace("read_file", args, dir.path()),
            expected
        );
    }

    #[test]
    fn typed_execution_marks_unavailable_virtual_agent_as_error_without_changing_output() {
        let dir = tempdir().unwrap();
        let args = r#"{"path":"agent://definitely-not-installed-review-fixture"}"#;
        let expected = "Unknown agent reference 'definitely-not-installed-review-fixture': No agent file found in workspace or user agent directories";

        assert_eq!(
            try_execute_tool_in_workspace("read_file", args, dir.path()),
            Err(expected.to_string())
        );
        assert_eq!(
            execute_tool_in_workspace("read_file", args, dir.path()),
            expected
        );
    }

    #[test]
    fn typed_execution_marks_malformed_virtual_agent_as_error_without_changing_output() {
        let dir = tempdir().unwrap();
        let args = r#"{"path":"agent://"}"#;
        let expected = "Error: 'agent://' reference requires an agent name";

        assert_eq!(
            try_execute_tool_in_workspace("read_file", args, dir.path()),
            Err(expected.to_string())
        );
        assert_eq!(
            execute_tool_in_workspace("read_file", args, dir.path()),
            expected
        );
    }

    #[test]
    fn typed_execution_marks_unavailable_virtual_remote_refs_as_errors_without_changing_output() {
        let dir = tempdir().unwrap();

        for path in ["pr://7", "mr://7", "issue://7"] {
            let args = json!({ "path": path }).to_string();
            let kind = path.split_once("://").unwrap().0;
            let expected = format!(
                "{kind}://7 requires a git origin remote or an explicit repository URL (e.g. pr://owner/repo/7)"
            );

            assert_eq!(
                try_execute_tool_in_workspace("read_file", &args, dir.path()),
                Err(expected.clone()),
                "typed result for {path}"
            );
            assert_eq!(
                execute_tool_in_workspace("read_file", &args, dir.path()),
                expected,
                "compatibility output for {path}"
            );
        }
    }

    #[test]
    fn typed_execution_marks_malformed_virtual_remote_refs_as_errors_without_changing_output() {
        let dir = tempdir().unwrap();

        for path in [
            "pr://not-a-number",
            "mr://not-a-number",
            "issue://not-a-number",
        ] {
            let args = json!({ "path": path }).to_string();
            let expected = format!(
                "Invalid repository reference '{path}': expected pr://<num>, issue://<num>, mr://<num>, or GitHub/GitLab URL"
            );

            assert_eq!(
                try_execute_tool_in_workspace("read_file", &args, dir.path()),
                Err(expected.clone()),
                "typed result for {path}"
            );
            assert_eq!(
                execute_tool_in_workspace("read_file", &args, dir.path()),
                expected,
                "compatibility output for {path}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn typed_execution_marks_signal_terminated_command_as_error_without_changing_output() {
        let dir = tempdir().unwrap();
        let args = r#"{"command":"kill -TERM $$"}"#;

        let typed = try_execute_tool_in_workspace("run_command", args, dir.path());
        let error = typed.expect_err("signal termination must fail");
        assert!(error.starts_with("Exit Status: signal:"), "{error}");
        assert_eq!(
            execute_tool_in_workspace("run_command", args, dir.path()),
            error
        );
    }

    #[test]
    fn test_read_file_auto_resolves_unique_workspace_suffix() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("crates").join("my-crate").join("src");
        fs::create_dir_all(&sub).unwrap();
        let file_path = sub.join("view.rs");
        fs::write(&file_path, "pub fn render() {}\n").unwrap();

        // Reading with partial path "src/view.rs"
        let args = json!({ "path": "src/view.rs" }).to_string();
        let result = try_execute_tool_in_workspace("read_file", &args, dir.path()).unwrap();
        assert!(result
            .contains("[Notice: Auto-resolved 'src/view.rs' to 'crates/my-crate/src/view.rs']"));
        assert!(result.contains("pub fn render()"));

        // Reading with bare filename "view.rs"
        let args_bare = json!({ "path": "view.rs" }).to_string();
        let result_bare =
            try_execute_tool_in_workspace("read_file", &args_bare, dir.path()).unwrap();
        assert!(result_bare
            .contains("[Notice: Auto-resolved 'view.rs' to 'crates/my-crate/src/view.rs']"));
        assert!(result_bare.contains("pub fn render()"));
    }

    #[test]
    fn test_read_file_errors_with_suggestions_when_path_is_ambiguous() {
        let dir = tempdir().unwrap();
        let sub1 = dir.path().join("crate_a").join("src");
        let sub2 = dir.path().join("crate_b").join("src");
        fs::create_dir_all(&sub1).unwrap();
        fs::create_dir_all(&sub2).unwrap();
        fs::write(sub1.join("lib.rs"), "pub mod a;\n").unwrap();
        fs::write(sub2.join("lib.rs"), "pub mod b;\n").unwrap();

        let args = json!({ "path": "lib.rs" }).to_string();
        let err = try_execute_tool_in_workspace("read_file", &args, dir.path()).unwrap_err();
        assert!(err.contains("File 'lib.rs' not found. Did you mean one of:"));
        assert!(err.contains("crate_a/src/lib.rs"));
        assert!(err.contains("crate_b/src/lib.rs"));
    }

    #[test]
    fn test_run_command_dyn_cli() {
        let dir = tempdir().unwrap();
        // 1. "dyn" list
        let args_list = json!({ "command": "dyn" }).to_string();
        let out_list =
            try_execute_tool_in_workspace("run_command", &args_list, dir.path()).unwrap();
        assert!(out_list.contains("Threadlane In-Process Tool Runner (dyn)"));
        assert!(out_list.contains("manage_memory"));

        // 2. "dyn --help manage_memory"
        let args_help = json!({ "command": "dyn manage_memory --help" }).to_string();
        let out_help =
            try_execute_tool_in_workspace("run_command", &args_help, dir.path()).unwrap();
        assert!(out_help.contains("Tool Schema for 'manage_memory'"));

        // 3. JSON arguments preserve the tool schema's types and quoting.
        let args_run = json!({ "command": "dyn manage_memory {\"action\":\"read\"}" }).to_string();
        let out_run = try_execute_tool_in_workspace("run_command", &args_run, dir.path()).unwrap();
        assert!(out_run.contains("No persistent memory found"));

        let flags = json!({ "command": "dyn manage_memory --action read" }).to_string();
        assert!(
            try_execute_tool_in_workspace("run_command", &flags, dir.path())
                .unwrap_err()
                .contains("JSON arguments")
        );

        let escaped_cwd = json!({ "command": "dyn", "cwd": "../outside" }).to_string();
        assert!(try_execute_tool_in_workspace("run_command", &escaped_cwd, dir.path()).is_err());
    }

    #[test]
    fn test_edit_file_hashline_auto_resolves_unique_workspace_suffix() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("crates").join("my-crate").join("src");
        fs::create_dir_all(&sub).unwrap();
        let file_path = sub.join("view.rs");
        fs::write(&file_path, "hello world\n").unwrap();

        // 1. read_file with shorthand path
        let read_args = json!({ "path": "src/view.rs" }).to_string();
        let read_res = try_execute_tool_in_workspace("read_file", &read_args, dir.path()).unwrap();
        assert!(read_res
            .contains("[Notice: Auto-resolved 'src/view.rs' to 'crates/my-crate/src/view.rs']"));

        // Extract hash from line 1
        let hash = read_res
            .lines()
            .find(|l| l.starts_with("1:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|s| s.split('|').next())
            .unwrap();

        // 2. edit_file_hashline with the same shorthand path
        let edit_args = json!({
            "path": "src/view.rs",
            "edits": [{
                "start_anchor": format!("1:{hash}"),
                "action": "replace",
                "new_content": "hello threadlane"
            }]
        })
        .to_string();

        let edit_res =
            try_execute_tool_in_workspace("edit_file_hashline", &edit_args, dir.path()).unwrap();
        assert!(edit_res
            .contains("[Notice: Auto-resolved 'src/view.rs' to 'crates/my-crate/src/view.rs']"));
        assert!(edit_res.contains("Successfully applied 1 hashline edit(s)"));

        let updated = fs::read_to_string(&file_path).unwrap();
        assert_eq!(updated, "hello threadlane\n");
    }
}
