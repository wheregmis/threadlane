use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HashlineAction {
    Replace,
    InsertAfter,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashlineEdit {
    pub start_anchor: String,
    pub end_anchor: Option<String>,
    pub action: HashlineAction,
    #[serde(default)]
    pub new_content: String,
}

impl HashlineEdit {
    pub fn new(
        start_anchor: impl Into<String>,
        end_anchor: Option<impl Into<String>>,
        action: HashlineAction,
        new_content: impl Into<String>,
    ) -> Self {
        Self {
            start_anchor: start_anchor.into(),
            end_anchor: end_anchor.map(Into::into),
            action,
            new_content: new_content.into(),
        }
    }
}

/// Compute a 3-character hex hash for a line of text.
pub fn compute_line_hash(line: &str) -> String {
    let clean = line.trim_end_matches(['\r', '\n']);
    let mut hash: u32 = 2166136261;
    for byte in clean.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16777619);
    }
    format!("{:03x}", hash & 0xfff)
}

/// Format a line with its line number and hash tag (e.g., `12:a3f|fn main() {`).
pub fn format_line_hashline(line_no: usize, line: &str) -> String {
    let hash = compute_line_hash(line);
    format!("{line_no}:{hash}|{line}")
}

/// Parse a line anchor string like `"12:a3f"` into line index (1-based) and lowercased hash.
pub fn parse_anchor(anchor: &str) -> Result<(usize, String), String> {
    let (first, second) = anchor.split_once(':').ok_or_else(|| {
        format!(
            "Invalid anchor format '{anchor}'. Expected format 'line_number:hash' (e.g. '12:a3f')."
        )
    })?;
    if second.contains(':') {
        return Err(format!(
            "Invalid anchor format '{anchor}'. Expected format 'line_number:hash' (e.g. '12:a3f')."
        ));
    }
    let line_no: usize = first.trim().parse().map_err(|_| {
        format!("Invalid line number in anchor '{anchor}'. Must be a positive integer.")
    })?;
    if line_no == 0 {
        return Err(format!(
            "Invalid line number 0 in anchor '{anchor}'. Line numbers are 1-indexed."
        ));
    }
    let hash = second.trim().to_lowercase();
    if hash.is_empty() {
        return Err(format!("Hash missing in anchor '{anchor}'."));
    }
    Ok((line_no, hash))
}

/// Apply a series of hash-anchored edits to a multi-line document.
pub fn apply_hashline_edits(content: &str, edits: &[HashlineEdit]) -> Result<String, String> {
    if edits.is_empty() {
        return Ok(content.to_string());
    }

    let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

    // Preserve trailing newline if present
    let has_trailing_newline = content.ends_with('\n');

    // Parse and validate all anchors first before applying any mutations
    struct ValidatedEdit {
        start_idx: usize, // 0-based
        end_idx: usize,   // 0-based inclusive
        action: HashlineAction,
        new_lines: Vec<String>,
    }

    let mut validated_edits = Vec::with_capacity(edits.len());

    for edit in edits {
        let (start_line, expected_start_hash) = parse_anchor(&edit.start_anchor)?;
        if start_line > lines.len() {
            return Err(format!(
                "Anchor '{}' line number {} exceeds total line count ({}) of file.",
                edit.start_anchor,
                start_line,
                lines.len()
            ));
        }

        let start_idx = start_line - 1;
        let actual_start_line = &lines[start_idx];
        let actual_start_hash = compute_line_hash(actual_start_line);
        if actual_start_hash != expected_start_hash {
            return Err(format!(
                "Hashline mismatch at line {start_line}: expected hash '{expected_start_hash}', but found '{actual_start_line}' with hash '{actual_start_hash}'. Edit aborted to prevent file corruption. Please read the file again to refresh line hashes."
            ));
        }

        let end_idx = if let Some(ref end_anchor) = edit.end_anchor {
            let (end_line, expected_end_hash) = parse_anchor(end_anchor)?;
            if end_line < start_line {
                return Err(format!(
                    "Invalid range in edit: end_anchor line {} is before start_anchor line {}.",
                    end_line, start_line
                ));
            }
            if end_line > lines.len() {
                return Err(format!(
                    "End anchor '{}' line number {} exceeds total line count ({}) of file.",
                    end_anchor,
                    end_line,
                    lines.len()
                ));
            }
            let idx = end_line - 1;
            let actual_end_line = &lines[idx];
            let actual_end_hash = compute_line_hash(actual_end_line);
            if actual_end_hash != expected_end_hash {
                return Err(format!(
                    "Hashline mismatch at end line {end_line}: expected hash '{expected_end_hash}', but found '{actual_end_line}' with hash '{actual_end_hash}'. Edit aborted to prevent file corruption. Please read the file again to refresh line hashes."
                ));
            }
            idx
        } else {
            start_idx
        };

        let new_lines: Vec<String> = if edit.new_content.is_empty() {
            Vec::new()
        } else {
            edit.new_content.lines().map(|s| s.to_string()).collect()
        };

        validated_edits.push(ValidatedEdit {
            start_idx,
            end_idx,
            action: edit.action.clone(),
            new_lines,
        });
    }

    // Sort edits descending by start_idx to avoid shifting target indices when modifying `lines`
    validated_edits.sort_by_key(|edit| std::cmp::Reverse(edit.start_idx));

    // Ensure no overlapping edit ranges
    for i in 0..validated_edits.len().saturating_sub(1) {
        let current = &validated_edits[i];
        let next = &validated_edits[i + 1];
        if next.end_idx >= current.start_idx {
            return Err(format!(
                "Overlapping edit ranges detected between lines {}-{} and {}-{}.",
                next.start_idx + 1,
                next.end_idx + 1,
                current.start_idx + 1,
                current.end_idx + 1
            ));
        }
    }

    // Apply validated edits
    for edit in validated_edits {
        match edit.action {
            HashlineAction::Replace => {
                lines.splice(edit.start_idx..=edit.end_idx, edit.new_lines);
            }
            HashlineAction::InsertAfter => {
                let insert_at = edit.end_idx + 1;
                lines.splice(insert_at..insert_at, edit.new_lines);
            }
            HashlineAction::Delete => {
                lines.drain(edit.start_idx..=edit.end_idx);
            }
        }
    }

    let mut result = lines.join("\n");
    if has_trailing_newline && !result.is_empty() {
        result.push('\n');
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_line_hash() {
        let hash1 = compute_line_hash("fn main() {");
        let hash2 = compute_line_hash("fn main() {\n");
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 3);
    }

    #[test]
    fn test_format_line_hashline() {
        let formatted = format_line_hashline(12, "let x = 42;");
        assert!(formatted.starts_with("12:"));
        assert!(formatted.contains("|let x = 42;"));
    }

    #[test]
    fn test_apply_hashline_edits_replace_single_line() {
        let original = "line1\nline2\nline3\n";
        let h2 = compute_line_hash("line2");
        let edit = HashlineEdit {
            start_anchor: format!("2:{h2}"),
            end_anchor: None,
            action: HashlineAction::Replace,
            new_content: "line2_modified".into(),
        };

        let result = apply_hashline_edits(original, &[edit]).unwrap();
        assert_eq!(result, "line1\nline2_modified\nline3\n");
    }

    #[test]
    fn test_apply_hashline_edits_hash_mismatch_rejected() {
        let original = "line1\nline2\nline3\n";
        let edit = HashlineEdit {
            start_anchor: "2:wrong_hash".into(),
            end_anchor: None,
            action: HashlineAction::Replace,
            new_content: "line2_modified".into(),
        };

        let err = apply_hashline_edits(original, &[edit]).unwrap_err();
        assert!(err.contains("Hashline mismatch at line 2"));
    }

    #[test]
    fn test_apply_hashline_edits_range_replace() {
        let original = "head\nstart_line\nmiddle_line\nend_line\ntail\n";
        let h_start = compute_line_hash("start_line");
        let h_end = compute_line_hash("end_line");

        let edit = HashlineEdit {
            start_anchor: format!("2:{h_start}"),
            end_anchor: Some(format!("4:{h_end}")),
            action: HashlineAction::Replace,
            new_content: "replaced_range".into(),
        };

        let result = apply_hashline_edits(original, &[edit]).unwrap();
        assert_eq!(result, "head\nreplaced_range\ntail\n");
    }

    #[test]
    fn test_apply_hashline_edits_insert_after() {
        let original = "line1\nline2\n";
        let h1 = compute_line_hash("line1");

        let edit = HashlineEdit {
            start_anchor: format!("1:{h1}"),
            end_anchor: None,
            action: HashlineAction::InsertAfter,
            new_content: "line1_b".into(),
        };

        let result = apply_hashline_edits(original, &[edit]).unwrap();
        assert_eq!(result, "line1\nline1_b\nline2\n");
    }
}
