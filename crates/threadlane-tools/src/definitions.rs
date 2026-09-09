use serde_json::{json, Value};

pub(crate) fn tool_definitions() -> Vec<Value> {
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
