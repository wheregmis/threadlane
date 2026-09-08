use std::fs;
use std::path::Path;

use crate::dispatch::truncate_tool_output;
use crate::workspace::validate_path_in_workspace;

pub(crate) fn get_repo_map_impl(workspace_root: &Path, rel_path: Option<&str>) -> Result<String, String> {
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

pub(crate) fn walk_repo_skeleton(
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

pub(crate) fn path_matches(file_name: &str, target_path: &str) -> bool {
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
