use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::Command;

pub fn get_available_tools() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
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
            }
        }),
        json!({
            "type": "function",
            "function": {
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
            }
        }),
        json!({
            "type": "function",
            "function": {
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
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List files and subdirectories in a directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory path to list" }
                    },
                    "required": ["path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
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
            }
        }),
    ]
}

pub fn get_codex_tools() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
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
            "type": "function",
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
            "type": "function",
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
            "type": "function",
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
            "type": "function",
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

pub fn execute_tool(name: &str, args_json: &str) -> String {
    let args: Value = match serde_json::from_str(args_json) {
        Ok(v) => v,
        Err(e) => return format!("Error parsing tool arguments JSON: {e}"),
    };

    match name {
        "read_file" => {
            let path = match args.get("path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => return "Error: 'path' parameter is required".into(),
            };
            let start = args.get("start_line").and_then(|v| v.as_u64()).map(|n| n as usize);
            let end = args.get("end_line").and_then(|v| v.as_u64()).map(|n| n as usize);

            match fs::read_to_string(path) {
                Ok(content) => {
                    let lines: Vec<&str> = content.lines().collect();
                    let start_idx = start.unwrap_or(1).saturating_sub(1);
                    let end_idx = end.unwrap_or(lines.len()).min(lines.len());
                    if start_idx >= lines.len() {
                        return format!("File only has {} lines.", lines.len());
                    }
                    let selected = &lines[start_idx..end_idx];
                    selected.join("\n")
                }
                Err(e) => format!("Error reading file '{path}': {e}"),
            }
        }
        "write_file" => {
            let path = match args.get("path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => return "Error: 'path' parameter is required".into(),
            };
            let content = match args.get("content").and_then(|v| v.as_str()) {
                Some(c) => c,
                None => return "Error: 'content' parameter is required".into(),
            };

            if let Some(parent) = Path::new(path).parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }

            match fs::write(path, content) {
                Ok(_) => format!("Successfully wrote {} bytes to '{path}'", content.len()),
                Err(e) => format!("Error writing to file '{path}': {e}"),
            }
        }
        "edit_file" => {
            let path = match args.get("path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => return "Error: 'path' parameter is required".into(),
            };
            let target = match args.get("target").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => return "Error: 'target' parameter is required".into(),
            };
            let replacement = match args.get("replacement").and_then(|v| v.as_str()) {
                Some(r) => r,
                None => return "Error: 'replacement' parameter is required".into(),
            };

            match fs::read_to_string(path) {
                Ok(content) => {
                    if !content.contains(target) {
                        return format!("Target string not found in '{path}'");
                    }
                    let new_content = content.replace(target, replacement);
                    match fs::write(path, new_content) {
                        Ok(_) => format!("Successfully replaced target in '{path}'"),
                        Err(e) => format!("Error writing file '{path}': {e}"),
                    }
                }
                Err(e) => format!("Error reading file '{path}': {e}"),
            }
        }
        "list_dir" => {
            let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            match fs::read_dir(path_str) {
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
                Err(e) => format!("Error reading directory '{path_str}': {e}"),
            }
        }
        "run_command" => {
            let cmd_str = match args.get("command").and_then(|v| v.as_str()) {
                Some(c) => c,
                None => return "Error: 'command' parameter is required".into(),
            };
            let cwd = args.get("cwd").and_then(|v| v.as_str());

            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(cmd_str);
            if let Some(dir) = cwd {
                cmd.current_dir(dir);
            }

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

    #[test]
    fn test_list_dir_tool() {
        let res = execute_tool("list_dir", r#"{"path": "."}"#);
        assert!(res.contains("Cargo.toml"));
    }
}
