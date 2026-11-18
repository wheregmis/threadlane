use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::repo_map::path_matches;

pub(crate) fn commit_text_transaction(
    files: &[(String, PathBuf, String, String)],
) -> Result<(), String> {
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

pub(crate) fn run_post_edit_diagnostics(workspace_root: &Path, raw_path: &str) -> String {
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
