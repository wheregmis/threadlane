use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::definitions::tool_definitions;
use crate::hashline;
use crate::memory::{consolidate_memory_impl, read_memory_impl, save_memory_impl};
use crate::repo_map::get_repo_map_impl;
use crate::search;
use crate::transaction::{commit_text_transaction, run_post_edit_diagnostics};
use crate::virtual_read;
use crate::workspace::{
    canonical_workspace_root, find_fuzzy_workspace_path, validate_cwd_in_workspace,
    validate_path_in_workspace,
};

pub const READ_FILE_SNAPSHOT_PREFIX: &str = "[Threadlane read_file SHA-256: ";
pub const READ_FILE_SNAPSHOT_PATH_PREFIX: &str = "[Threadlane read_file path: ";

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

pub(crate) const MAX_TOOL_OUTPUT_CHARS: usize = 3_000;
pub(crate) const TRUNCATE_HEAD_CHARS: usize = 1_200;
pub(crate) const TRUNCATE_TAIL_CHARS: usize = 1_200;

pub(crate) fn truncate_tool_output(output: &str) -> String {
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

#[cfg(test)]
pub(crate) fn execute_tool(name: &str, args_json: &str) -> String {
    try_execute_tool(name, args_json).unwrap_or_else(|error| error)
}

#[cfg(test)]
pub(crate) fn execute_tool_in_workspace(
    name: &str,
    args_json: &str,
    workspace_root: &Path,
) -> String {
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
    try_execute_tool_in_workspace_with(
        name,
        args_json,
        workspace_root,
        &virtual_read::RemoteCredentials::default(),
    )
}

/// Executes a tool with explicit remote-forge credentials.
///
/// Stored credential lookup lives with the host; pass stored tokens via
/// `credentials` and ambient (`gh`/`glab`/env) fallbacks still apply.
pub fn try_execute_tool_in_workspace_with(
    name: &str,
    args_json: &str,
    workspace_root: &Path,
    credentials: &virtual_read::RemoteCredentials,
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
                    return virtual_read::try_remote_ref_path_with(workspace_root, path, credentials);
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
            if let Some(target_dir) = worktree_cargo_target_dir(workspace_root) {
                cmd.env("CARGO_TARGET_DIR", target_dir);
            }

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

pub(crate) fn worktree_cargo_target_dir(workspace_root: &Path) -> Option<PathBuf> {
    let worktrees = workspace_root
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "worktrees"))?;
    let threadlane = worktrees
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == ".threadlane"))?;
    let lane = workspace_root
        .strip_prefix(worktrees)
        .ok()?
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("-");
    (!lane.is_empty()).then(|| threadlane.join("cache/target").join(lane))
}

/// Dispatches an in-process CLI tool invocation via `dyn <tool> [args]`.
pub(crate) fn execute_dyn_cli(input: &str, workspace_root: &Path) -> Result<String, String> {
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
