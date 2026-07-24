use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "read_file",
            "description": "Read content of a file, optionally specifying start and end lines (1-indexed).",
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
            "name": "edit_file",
            "description": "Replace exact target string with replacement string in a file.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file" },
                    "target": { "type": "string", "description": "Exact text substring to be replaced" },
                    "replacement": { "type": "string", "description": "New text to substitute in place of target" }
                },
                "required": ["path", "target", "replacement"]
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
        if !normalized.starts_with(&canonical_root) {
            return Err(format!(
                "Access denied: Path '{}' escapes workspace root '{}'",
                path_input,
                canonical_root.display()
            ));
        }
        Ok(normalized)
    }
}

pub fn validate_cwd_in_workspace(
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
                    selected.join("\n")
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
                Ok(_) => format!("Successfully wrote {} bytes to '{raw_path}'", content.len()),
                Err(e) => format!("Error writing to file '{raw_path}': {e}"),
            }
        }
        "edit_file" => {
            let raw_path = match args.get("path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => return "Error: 'path' parameter is required".into(),
            };
            let validated_path = match validate_path_in_workspace(raw_path, workspace_root) {
                Ok(p) => p,
                Err(err) => return err,
            };

            let target = match args.get("target").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => return "Error: 'target' parameter is required".into(),
            };
            let replacement = match args.get("replacement").and_then(|v| v.as_str()) {
                Some(r) => r,
                None => return "Error: 'replacement' parameter is required".into(),
            };

            match fs::read_to_string(&validated_path) {
                Ok(content) => {
                    if !content.contains(target) {
                        return format!("Target string not found in '{raw_path}'");
                    }
                    let new_content = content.replace(target, replacement);
                    match fs::write(&validated_path, new_content) {
                        Ok(_) => format!("Successfully replaced target in '{raw_path}'"),
                        Err(e) => format!("Error writing file '{raw_path}': {e}"),
                    }
                }
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
                    items.join("\n")
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

                    format!(
                        "Exit Status: {exit}\n--- STDOUT ---\n{stdout}\n--- STDERR ---\n{stderr}"
                    )
                }
                Err(e) => format!("Error executing command '{cmd_str}': {e}"),
            }
        }
        unknown => format!("Error: Unknown tool '{unknown}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_list_dir_tool() {
        let res = execute_tool("list_dir", r#"{"path": "."}"#);
        assert!(res.contains("Cargo.toml"));
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
}
