pub mod hashline;

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "read_file",
            "description": "Read content of a file with line numbers and hash anchors (e.g. 12:a3f|content), optionally specifying start and end lines (1-indexed).",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute or relative path to the file" },
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
pub fn validate_path_in_workspace(
    path_input: &str,
    workspace_root: &Path,
) -> Result<PathBuf, String> {
    let canonical_root = workspace_root
        .canonicalize()
        .map_err(|e| format!("Invalid workspace root '{}': {e}", workspace_root.display()))?;

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
    let canonical_root = workspace_root
        .canonicalize()
        .map_err(|e| format!("Invalid workspace root '{}': {e}", workspace_root.display()))?;

    let target_dir = match cwd_input {
        Some(dir) => {
            let p = Path::new(dir);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                canonical_root.join(p)
            }
        }
        None => canonical_root.clone(),
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

pub fn execute_tool(name: &str, args_json: &str) -> String {
    execute_tool_in_workspace(name, args_json, Path::new("."))
}

pub fn execute_tool_in_workspace(name: &str, args_json: &str, workspace_root: &Path) -> String {
    let args: Value = match serde_json::from_str(args_json) {
        Ok(v) => v,
        Err(e) => return format!("Error parsing tool arguments JSON: {e}"),
    };

    match name {
        "read_file" => {
            let raw_path = match args.get("path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => return "Error: 'path' parameter is required".into(),
            };
            let validated_path = match validate_path_in_workspace(raw_path, workspace_root) {
                Ok(p) => p,
                Err(err) => return err,
            };

            let start = args
                .get("start_line")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let end = args
                .get("end_line")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);

            match fs::read_to_string(&validated_path) {
                Ok(content) => {
                    let lines: Vec<&str> = content.lines().collect();
                    let start_idx = start.unwrap_or(1).saturating_sub(1);
                    let end_idx = end.unwrap_or(lines.len()).min(lines.len());
                    if start_idx >= lines.len() {
                        return format!("File only has {} lines.", lines.len());
                    }
                    if end_idx <= start_idx {
                        return format!(
                            "Invalid line range: end_line ({}) must not be before start_line ({}).",
                            end.unwrap_or(lines.len()),
                            start.unwrap_or(1),
                        );
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
                    truncate_tool_output(&formatted_lines.join("\n"))
                }
                Err(e) => format!("Error reading file '{raw_path}': {e}"),
            }
        }
        "write_file" => {
            let raw_path = match args.get("path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => return "Error: 'path' parameter is required".into(),
            };
            let validated_path = match validate_path_in_workspace(raw_path, workspace_root) {
                Ok(p) => p,
                Err(err) => return err,
            };

            let content = match args.get("content").and_then(|v| v.as_str()) {
                Some(c) => c,
                None => return "Error: 'content' parameter is required".into(),
            };

            if let Some(parent) = validated_path.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }

            match fs::write(&validated_path, content) {
                Ok(_) => {
                    let diag = run_post_edit_diagnostics(workspace_root, raw_path);
                    format!(
                        "Successfully wrote {} bytes to '{raw_path}'{diag}",
                        content.len()
                    )
                }
                Err(e) => format!("Error writing to file '{raw_path}': {e}"),
            }
        }

        "edit_file_hashline" => {
            let raw_path = match args.get("path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => return "Error: 'path' parameter is required".into(),
            };
            let validated_path = match validate_path_in_workspace(raw_path, workspace_root) {
                Ok(p) => p,
                Err(err) => return err,
            };

            let edits_value = match args.get("edits") {
                Some(e) => e,
                None => return "Error: 'edits' parameter is required".into(),
            };

            let edits: Vec<hashline::HashlineEdit> =
                match serde_json::from_value(edits_value.clone()) {
                    Ok(e) => e,
                    Err(err) => return format!("Error parsing 'edits' argument: {err}"),
                };

            match fs::read_to_string(&validated_path) {
                Ok(content) => match hashline::apply_hashline_edits(&content, &edits) {
                    Ok(new_content) => match fs::write(&validated_path, new_content) {
                        Ok(_) => {
                            let diag = run_post_edit_diagnostics(workspace_root, raw_path);
                            format!(
                                "Successfully applied {} hashline edit(s) to '{raw_path}'{diag}",
                                edits.len()
                            )
                        }
                        Err(e) => format!("Error writing file '{raw_path}': {e}"),
                    },
                    Err(e) => format!("Error applying hashline edits to '{raw_path}': {e}"),
                },
                Err(e) => format!("Error reading file '{raw_path}': {e}"),
            }
        }
        "list_dir" => {
            let raw_path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let validated_path = match validate_path_in_workspace(raw_path, workspace_root) {
                Ok(p) => p,
                Err(err) => return err,
            };

            match fs::read_dir(&validated_path) {
                Ok(entries) => {
                    let mut items = Vec::new();
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        let kind = if is_dir { "[DIR] " } else { "[FILE]" };
                        items.push(format!("{kind} {name}"));
                    }
                    items.sort();
                    truncate_tool_output(&items.join("\n"))
                }
                Err(e) => format!("Error reading directory '{raw_path}': {e}"),
            }
        }
        "run_command" => {
            let cmd_str = match args.get("command").and_then(|v| v.as_str()) {
                Some(c) => c,
                None => return "Error: 'command' parameter is required".into(),
            };
            let raw_cwd = args.get("cwd").and_then(|v| v.as_str());
            let validated_cwd = match validate_cwd_in_workspace(raw_cwd, workspace_root) {
                Ok(p) => p,
                Err(err) => return err,
            };

            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(cmd_str);
            cmd.current_dir(&validated_cwd);

            match cmd.output() {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let exit = output.status;

                    truncate_tool_output(&format!(
                        "Exit Status: {exit}\n--- STDOUT ---\n{stdout}\n--- STDERR ---\n{stderr}"
                    ))
                }
                Err(e) => format!("Error executing command '{cmd_str}': {e}"),
            }
        }
        "get_repo_map" => {
            let raw_path = args.get("path").and_then(|v| v.as_str());
            get_repo_map_impl(workspace_root, raw_path)
        }

        "manage_memory" => {
            let action = match args.get("action").and_then(|v| v.as_str()) {
                Some(a) => a,
                None => {
                    return "Error: 'action' parameter is required ('read', 'save', 'consolidate')"
                        .into()
                }
            };
            match action {
                "read" => read_memory_impl(workspace_root),
                "save" => save_memory_impl(workspace_root, &args),
                "consolidate" => consolidate_memory_impl(workspace_root, &args),
                unknown => format!("Error: Unknown action '{unknown}' for manage_memory"),
            }
        }
        "read_memory" => read_memory_impl(workspace_root),
        "save_memory" => save_memory_impl(workspace_root, &args),
        "consolidate_memory" => consolidate_memory_impl(workspace_root, &args),
        unknown => format!("Error: Unknown tool '{unknown}'"),
    }
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

fn read_memory_impl(workspace_root: &Path) -> String {
    let mem_file = workspace_root.join(".threadlane").join("memory.md");
    if mem_file.is_file() {
        match fs::read_to_string(&mem_file) {
            Ok(content) => truncate_tool_output(&content),
            Err(e) => format!("Error reading .threadlane/memory.md: {e}"),
        }
    } else {
        "No persistent memory found in .threadlane/memory.md yet.".to_string()
    }
}

fn save_memory_impl(workspace_root: &Path, args: &Value) -> String {
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return "Error: 'content' parameter is required".into(),
    };
    let mode = args
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("append");

    let dir = workspace_root.join(".threadlane");
    if let Err(e) = fs::create_dir_all(&dir) {
        return format!("Error creating .threadlane directory: {e}");
    }
    let mem_file = dir.join("memory.md");

    let new_content = if mode == "overwrite" || !mem_file.exists() {
        content.trim().to_string()
    } else {
        let existing = fs::read_to_string(&mem_file).unwrap_or_default();
        format!("{}\n\n{}", existing.trim(), content.trim())
    };

    match fs::write(&mem_file, new_content) {
        Ok(_) => "Successfully saved memory to .threadlane/memory.md".to_string(),
        Err(e) => format!("Error writing to .threadlane/memory.md: {e}"),
    }
}

fn consolidate_memory_impl(workspace_root: &Path, args: &Value) -> String {
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
    if let Err(e) = fs::create_dir_all(&dir) {
        return format!("Error creating .threadlane directory: {e}");
    }
    let mem_file = dir.join("memory.md");

    let existing = fs::read_to_string(&mem_file).unwrap_or_default();
    let merged = consolidate_memory_entries(&existing, &architecture, &gotchas, &verification);

    match fs::write(&mem_file, merged) {
        Ok(_) => "Successfully consolidated memory entries in .threadlane/memory.md".to_string(),
        Err(e) => format!("Error writing to .threadlane/memory.md: {e}"),
    }
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

fn get_repo_map_impl(workspace_root: &Path, rel_path: Option<&str>) -> String {
    let target_dir = match rel_path {
        Some(path) => match validate_path_in_workspace(path, workspace_root) {
            Ok(p) => p,
            Err(err) => return err,
        },
        None => workspace_root.to_path_buf(),
    };

    let mut lines = Vec::new();
    walk_repo_skeleton(&target_dir, workspace_root, 0, &mut lines);

    if lines.is_empty() {
        "No source code definitions found in repository map.".to_string()
    } else {
        truncate_tool_output(&lines.join("\n"))
    }
}

fn walk_repo_skeleton(dir: &Path, root: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut sorted_entries: Vec<_> = entries.flatten().collect();
    sorted_entries.sort_by_key(|e| e.file_name());

    for entry in sorted_entries {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
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

        let path = entry.path();
        if file_type.is_dir() {
            walk_repo_skeleton(&path, root, depth + 1, out);
        } else if file_type.is_file() {
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if matches!(ext, "rs" | "py" | "js" | "ts" | "go" | "toml") {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                let Ok(content) = fs::read_to_string(&path) else {
                    continue;
                };

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
    let target_clean = raw_path.replace('\\', "/");

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
                    let file_clean = file_name.replace('\\', "/");
                    if file_clean.ends_with(&target_clean) || target_clean.ends_with(&file_clean) {
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

    #[test]
    fn test_run_post_edit_diagnostics_non_rust_file() {
        let dir = tempdir().unwrap();
        let res = run_post_edit_diagnostics(dir.path(), "readme.txt");
        assert_eq!(res, "");
    }

    #[test]
    fn test_list_dir_tool() {
        let res = execute_tool("list_dir", r#"{"path": "."}"#);
        assert!(res.contains("Cargo.toml"));
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
            json!({"content": "## Architectural Decision\nUse Makepad with threadlane state."})
                .to_string();
        let save_res = execute_tool_in_workspace("save_memory", &payload, root);
        assert!(save_res.contains("Successfully saved memory"));

        let read_res = execute_tool_in_workspace("read_memory", "{}", root);
        assert!(read_res.contains("Use Makepad with threadlane state."));
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
            "architecture": ["Use Makepad UI components"],
            "gotchas": ["cargo check requires unsandboxed bypass on macOS"],
            "verification": ["cargo test --workspace"]
        })
        .to_string();

        let res = execute_tool_in_workspace("manage_memory", &consolidate_payload, root);
        assert!(res.contains("Successfully consolidated memory entries"));

        let mem_content = execute_tool_in_workspace("manage_memory", &read_payload, root);
        assert!(mem_content.contains("## Architecture"));
        assert!(mem_content.contains("Use Makepad UI components"));
        assert!(mem_content.contains("## Gotchas"));
        assert!(mem_content.contains("cargo check requires unsandboxed bypass on macOS"));
        assert!(mem_content.contains("## Verification Commands"));
        assert!(mem_content.contains("cargo test --workspace"));
    }
}
