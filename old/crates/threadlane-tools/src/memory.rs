use std::fs;
use std::io::Write;
use std::path::Path;

use serde_json::Value;

use crate::dispatch::truncate_tool_output;
use crate::workspace::{canonical_workspace_root, validate_path_in_workspace};

mod notes;
pub(crate) use notes::manage_notes;
pub use notes::recall_project_memory;

fn memory_path(root: &Path, name: &str) -> Result<std::path::PathBuf, String> {
    let dir = canonical_workspace_root(root)?.join(".threadlane");
    let target = dir.join(name);
    for path in [&dir, &target] {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err("Memory storage must not be a symlink".into());
            }
            Ok(meta) if path == &dir && !meta.is_dir() || path == &target && !meta.is_file() => {
                return Err("Memory storage requires a directory and regular files".into());
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("Memory storage unavailable: {error}"));
            }
            _ => {}
        }
    }
    validate_path_in_workspace(&target.to_string_lossy(), root)
}

fn with_memory_writer<T>(
    root: &Path,
    change: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let lock_path = memory_path(root, "memory.lock")?;
    fs::create_dir_all(lock_path.parent().expect("memory directory"))
        .map_err(|error| format!("Memory storage unavailable: {error}"))?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|error| format!("Memory writer unavailable: {error}"))?;
    lock.lock()
        .map_err(|error| format!("Memory writer lock failed: {error}"))?;
    // The stable lock file is never renamed; its OS lock is released on drop.
    change()
}

fn write_memory_document(root: &Path, name: &str, content: &[u8]) -> Result<(), String> {
    let path = memory_path(root, name)?;
    let dir = path.parent().expect("memory directory");
    let mut staged = tempfile::NamedTempFile::new_in(dir).map_err(|error| error.to_string())?;
    staged
        .write_all(content)
        .and_then(|_| staged.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    staged.persist(&path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    fs::File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn read_memory_impl(workspace_root: &Path) -> Result<String, String> {
    let mem_file = memory_path(workspace_root, "memory.md")?;
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

    with_memory_writer(workspace_root, || {
        let mem_file = memory_path(workspace_root, "memory.md")?;

        let new_content = if mode == "overwrite" || !mem_file.exists() {
            content.trim().to_string()
        } else {
            let existing = fs::read_to_string(&mem_file)
                .map_err(|e| format!("Error reading .threadlane/memory.md: {e}"))?;
            format!("{}\n\n{}", existing.trim(), content.trim())
        };

        write_memory_document(workspace_root, "memory.md", new_content.as_bytes())
            .map(|_| "Successfully saved memory to .threadlane/memory.md".to_string())
            .map_err(|e| format!("Error writing to .threadlane/memory.md: {e}"))
    })
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

    with_memory_writer(workspace_root, || {
        let mem_file = memory_path(workspace_root, "memory.md")?;

        let existing = if mem_file.is_file() {
            fs::read_to_string(&mem_file)
                .map_err(|e| format!("Error reading .threadlane/memory.md: {e}"))?
        } else {
            String::new()
        };
        let merged = consolidate_memory_entries(&existing, &architecture, &gotchas, &verification);

        write_memory_document(workspace_root, "memory.md", merged.as_bytes())
            .map(|_| {
                "Successfully consolidated memory entries in .threadlane/memory.md".to_string()
            })
            .map_err(|e| format!("Error writing to .threadlane/memory.md: {e}"))
    })
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
