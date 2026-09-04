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
    start_anchor: String,
    end_anchor: Option<String>,
    action: HashlineAction,
    #[serde(default)]
    new_content: String,
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
fn compute_line_hash(line: &str) -> String {
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
fn parse_anchor(anchor: &str) -> Result<(usize, String), String> {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashlineApplyResult {
    pub new_content: String,
    pub diff: String,
    pub updated_context: String,
}

/// Apply a series of hash-anchored edits to a multi-line document.
#[cfg(test)]
fn apply_hashline_edits(content: &str, edits: &[HashlineEdit]) -> Result<String, String> {
    apply_hashline_edits_detailed(content, edits, 0).map(|r| r.new_content)
}

/// Apply a series of hash-anchored edits and return the updated content, unified diff, and surrounding line hashes.
pub fn apply_hashline_edits_detailed(
    content: &str,
    edits: &[HashlineEdit],
    context_lines: usize,
) -> Result<HashlineApplyResult, String> {
    if edits.is_empty() {
        return Ok(HashlineApplyResult {
            new_content: content.to_string(),
            diff: String::new(),
            updated_context: String::new(),
        });
    }

    let original_lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    let mut lines = original_lines.clone();

    // Preserve trailing newline if present
    let has_trailing_newline = content.ends_with('\n');

    // Parse and validate all anchors first before applying any mutations
    struct ValidatedEdit {
        start_idx: usize, // 0-based
        end_idx: usize,   // 0-based inclusive
        action: HashlineAction,
        new_lines: Vec<String>,
        old_lines: Vec<String>,
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

        let old_lines = match edit.action {
            HashlineAction::Replace | HashlineAction::Delete => lines[start_idx..=end_idx].to_vec(),
            HashlineAction::InsertAfter => Vec::new(),
        };

        validated_edits.push(ValidatedEdit {
            start_idx,
            end_idx,
            action: edit.action.clone(),
            new_lines,
            old_lines,
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

    // Apply validated edits (descending order)
    for edit in &validated_edits {
        match edit.action {
            HashlineAction::Replace => {
                lines.splice(edit.start_idx..=edit.end_idx, edit.new_lines.clone());
            }
            HashlineAction::InsertAfter => {
                let insert_at = edit.end_idx + 1;
                lines.splice(insert_at..insert_at, edit.new_lines.clone());
            }
            HashlineAction::Delete => {
                lines.drain(edit.start_idx..=edit.end_idx);
            }
        }
    }

    // Sort ascending for diff and context generation
    validated_edits.sort_by_key(|edit| edit.start_idx);

    // Generate unified diff
    let mut diff_chunks = Vec::new();
    let mut current_offset: isize = 0;
    let diff_ctx = context_lines.min(3);

    for edit in &validated_edits {
        let orig_start_1based = match edit.action {
            HashlineAction::InsertAfter => edit.end_idx + 1,
            _ => edit.start_idx + 1,
        };
        let orig_count = match edit.action {
            HashlineAction::InsertAfter => 0,
            _ => edit.end_idx - edit.start_idx + 1,
        };

        let new_start_1based = ((orig_start_1based as isize) + current_offset).max(1) as usize;
        let new_count = edit.new_lines.len();

        let ctx_before_start = edit.start_idx.saturating_sub(diff_ctx);
        let ctx_before = &original_lines[ctx_before_start..edit.start_idx];

        let ctx_after_end = (edit.end_idx + 1 + diff_ctx).min(original_lines.len());
        let ctx_after = if edit.end_idx + 1 < original_lines.len() {
            &original_lines[(edit.end_idx + 1)..ctx_after_end]
        } else {
            &[]
        };

        let mut hunk = Vec::new();
        hunk.push(format!(
            "@@ -{},{} +{},{} @@",
            orig_start_1based, orig_count, new_start_1based, new_count
        ));
        for line in ctx_before {
            hunk.push(format!(" {line}"));
        }
        for line in &edit.old_lines {
            hunk.push(format!("-{line}"));
        }
        for line in &edit.new_lines {
            hunk.push(format!("+{line}"));
        }
        for line in ctx_after {
            hunk.push(format!(" {line}"));
        }
        diff_chunks.push(hunk.join("\n"));

        let diff_offset = (new_count as isize) - (orig_count as isize);
        current_offset += diff_offset;
    }
    let diff = diff_chunks.join("\n");

    // Generate surrounding line anchors in the modified document
    let mut context_ranges = Vec::new();
    let mut offset: isize = 0;
    let anchor_ctx = if context_lines == 0 { 5 } else { context_lines };

    for edit in &validated_edits {
        let orig_start = edit.start_idx;
        let orig_count = match edit.action {
            HashlineAction::InsertAfter => 0,
            _ => edit.end_idx - edit.start_idx + 1,
        };
        let new_count = edit.new_lines.len();
        let new_start = ((orig_start as isize) + offset).max(0) as usize;
        let new_end = new_start + new_count;

        let range_start = new_start.saturating_sub(anchor_ctx);
        let range_end = (new_end + anchor_ctx).min(lines.len());
        context_ranges.push((range_start, range_end));

        let diff_offset = (new_count as isize) - (orig_count as isize);
        offset += diff_offset;
    }

    // Merge overlapping context ranges
    let mut merged_ranges: Vec<(usize, usize)> = Vec::new();
    for (start, end) in context_ranges {
        if let Some(last) = merged_ranges.last_mut() {
            if start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged_ranges.push((start, end));
    }

    let mut context_lines_out = Vec::new();
    for (start_idx, end_idx) in merged_ranges {
        for idx in start_idx..end_idx {
            if idx < lines.len() {
                context_lines_out.push(format_line_hashline(idx + 1, &lines[idx]));
            }
        }
    }
    let updated_context = context_lines_out.join("\n");

    let mut result = lines.join("\n");
    if has_trailing_newline && !result.is_empty() {
        result.push('\n');
    }
    Ok(HashlineApplyResult {
        new_content: result,
        diff,
        updated_context,
    })
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

    #[test]
    fn test_apply_hashline_edits_detailed_returns_diff_and_anchors() {
        let original = "fn first() {}\nfn second() {\n    let a = 1;\n}\nfn third() {}\n";
        let h2 = compute_line_hash("fn second() {");
        let h4 = compute_line_hash("}");

        let edit = HashlineEdit {
            start_anchor: format!("2:{h2}"),
            end_anchor: Some(format!("4:{h4}")),
            action: HashlineAction::Replace,
            new_content: "fn second() {\n    let a = 2;\n    let b = 3;\n}".into(),
        };

        let result = apply_hashline_edits_detailed(original, &[edit], 3).unwrap();
        assert!(result.new_content.contains("let a = 2;"));
        assert!(result.diff.contains("@@ -2,3 +2,4 @@"));
        assert!(result.diff.contains("-    let a = 1;"));
        assert!(result.diff.contains("+    let a = 2;"));
        assert!(result.diff.contains("+    let b = 3;"));
        assert!(result.updated_context.contains("2:"));
        assert!(result.updated_context.contains("|fn second() {"));
        assert!(result.updated_context.contains("|    let a = 2;"));
    }
}
