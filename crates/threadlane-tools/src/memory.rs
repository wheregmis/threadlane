use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::dispatch::truncate_tool_output;

pub(crate) fn read_memory_impl(workspace_root: &Path) -> Result<String, String> {
    let mem_file = workspace_root.join(".threadlane").join("memory.md");
    if mem_file.is_file() {
        fs::read_to_string(&mem_file)
            .map(|content| truncate_tool_output(&content))
            .map_err(|e| format!("Error reading .threadlane/memory.md: {e}"))
    } else {
        Ok("No persistent memory found in .threadlane/memory.md yet.".to_string())
    }
}

pub(crate) fn save_memory_impl(workspace_root: &Path, args: &Value) -> Result<String, String> {
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

pub(crate) fn consolidate_memory_impl(
    workspace_root: &Path,
    args: &Value,
) -> Result<String, String> {
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

pub(crate) fn consolidate_memory_entries(
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
