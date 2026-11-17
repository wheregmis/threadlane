//! GPUI uses Unicode scalar columns; language servers use UTF-16 code units.
use std::{ops::Range, path::Path};

use anyhow::{Result, anyhow, bail, ensure};
use lsp_types::{
    CodeAction, CompletionItem, CompletionResponse, CompletionTextEdit, Diagnostic,
    DocumentChanges, InsertTextFormat, OneOf, Position, TextEdit,
};

pub(super) fn wire_position(text: &str, offset: usize) -> Result<Position> {
    let prefix = text
        .get(..offset)
        .ok_or_else(|| anyhow!("Invalid cursor boundary"))?;
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let column = prefix
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .encode_utf16()
        .count();
    Ok(Position::new(u32::try_from(line)?, u32::try_from(column)?))
}

pub(super) fn byte_offset(text: &str, position: Position) -> Result<usize> {
    let mut start = 0;
    for _ in 0..position.line {
        let Some(next_line) = text[start..].find('\n') else {
            return Ok(text.len());
        };
        start += next_line + 1;
    }
    let line = text[start..]
        .split('\n')
        .next()
        .unwrap_or("")
        .trim_end_matches('\r');
    let mut units = 0;
    for (offset, character) in line.char_indices() {
        if units == position.character {
            return Ok(start + offset);
        }
        units += character.len_utf16() as u32;
        ensure!(
            units <= position.character,
            "LSP range splits a surrogate pair"
        );
    }
    Ok(start + line.len())
}

pub(super) fn byte_range(text: &str, range: lsp_types::Range) -> Result<Range<usize>> {
    ensure!(range.start <= range.end, "LSP range is reversed");
    let start = byte_offset(text, range.start)?;
    let end = byte_offset(text, range.end)?;
    ensure!(start <= end, "LSP range is reversed");
    Ok(start..end)
}

pub(super) fn scalar_range(text: &str, range: lsp_types::Range) -> Result<lsp_types::Range> {
    ensure!(range.start <= range.end, "LSP range is reversed");
    Ok(lsp_types::Range::new(
        scalar_position(text, byte_offset(text, range.start)?)?,
        scalar_position(text, byte_offset(text, range.end)?)?,
    ))
}

pub(super) fn map_diagnostics(
    text: &str,
    items: Vec<serde_json::Value>,
) -> (Vec<Diagnostic>, usize) {
    let total = items.len();
    let mut diagnostics = Vec::with_capacity(total);
    for value in items {
        let Ok(mut diagnostic) = serde_json::from_value::<Diagnostic>(value) else {
            continue;
        };
        let Ok(range) = scalar_range(text, diagnostic.range) else {
            continue;
        };
        diagnostic.range = range;
        diagnostics.push(diagnostic);
    }
    let ignored = total - diagnostics.len();
    (diagnostics, ignored)
}

fn scalar_position(text: &str, offset: usize) -> Result<Position> {
    let prefix = text
        .get(..offset)
        .ok_or_else(|| anyhow!("Invalid scalar boundary"))?;
    Ok(Position::new(
        u32::try_from(prefix.bytes().filter(|byte| *byte == b'\n').count())?,
        u32::try_from(prefix.rsplit('\n').next().unwrap_or("").chars().count())?,
    ))
}

/// GPUI's completion insertion currently ignores commands and additional edits.
/// Reject those items rather than silently inserting an incomplete auto-import.
pub(super) fn completions(
    value: serde_json::Value,
    text: &str,
    cursor: usize,
) -> Result<CompletionResponse> {
    if value.is_null() {
        return Ok(CompletionResponse::Array(vec![]));
    }
    let response: CompletionResponse = serde_json::from_value(value)?;
    let items = match response {
        CompletionResponse::Array(items) => items,
        CompletionResponse::List(list) => list.items,
    };
    let items = items
        .into_iter()
        .filter_map(|item| completion(item, text, cursor).ok())
        .take(100)
        .collect();
    Ok(CompletionResponse::Array(items))
}

fn completion(mut item: CompletionItem, text: &str, cursor: usize) -> Result<CompletionItem> {
    ensure!(
        item.insert_text_format != Some(InsertTextFormat::SNIPPET),
        "Snippet completion is unsupported"
    );
    ensure!(
        item.command.is_none(),
        "Completion requires a server command"
    );
    ensure!(
        item.additional_text_edits
            .as_ref()
            .is_none_or(Vec::is_empty),
        "Completion requires additional edits"
    );
    item.text_edit = Some(match item.text_edit {
        Some(CompletionTextEdit::Edit(mut edit)) => {
            edit.range = scalar_range(text, edit.range)?;
            CompletionTextEdit::Edit(edit)
        }
        Some(CompletionTextEdit::InsertAndReplace(mut edit)) => {
            edit.insert = scalar_range(text, edit.insert)?;
            edit.replace = scalar_range(text, edit.replace)?;
            CompletionTextEdit::InsertAndReplace(edit)
        }
        None => {
            // GPUI otherwise inserts insertText at the caret without replacing
            // the prefix ("prin" + "print" -> "prinprint"). Supply a text edit.
            let before = text
                .get(..cursor)
                .ok_or_else(|| anyhow!("Invalid completion cursor"))?;
            let start = before
                .char_indices()
                .rev()
                .find(|(_, ch)| !(*ch == '_' || ch.is_alphanumeric()))
                .map_or(0, |(index, ch)| index + ch.len_utf8());
            let end = text[cursor..]
                .char_indices()
                .find(|(_, ch)| !(*ch == '_' || ch.is_alphanumeric()))
                .map_or(text.len(), |(index, _)| cursor + index);
            CompletionTextEdit::Edit(TextEdit {
                range: lsp_types::Range::new(
                    scalar_position(text, start)?,
                    scalar_position(text, end)?,
                ),
                new_text: item
                    .insert_text
                    .take()
                    .unwrap_or_else(|| item.label.clone()),
            })
        }
    });
    Ok(item)
}

/// Decode paths lexically, using the daemon's path syntax, not the client OS.
/// The daemon still validates the target before the editor opens it.
pub(super) fn relative_uri(uri: &lsp_types::Uri, root: &Path) -> Result<String> {
    let raw = uri.as_str();
    let raw_path = raw_file_uri_path(raw)
        .ok_or_else(|| anyhow!("Only workspace file locations are supported"))?;
    let decoded_raw_path = percent_encoding::percent_decode_str(raw_path).decode_utf8()?;
    ensure!(
        !decoded_raw_path.contains('\\')
            && decoded_raw_path
                .split('/')
                .all(|part| part != "." && part != ".."),
        "Invalid workspace file location"
    );
    let uri = url::Url::parse(uri.as_str())?;
    ensure!(
        uri.scheme() == "file" && uri.query().is_none() && uri.fragment().is_none(),
        "Only workspace file locations are supported"
    );
    let mut path = percent_encoding::percent_decode_str(uri.path())
        .decode_utf8()?
        .into_owned();
    if let Some(host) = uri
        .host_str()
        .filter(|host| *host != "localhost" && !host.is_empty())
    {
        path = format!("//{host}{path}");
    }
    let root = normalize_windows_path(&root.to_string_lossy());
    if is_windows_drive_path(&root) && path.starts_with('/') {
        path.remove(0);
    }
    path = normalize_windows_path(&path);
    let prefix = format!("{}/", root.trim_end_matches('/'));
    let relative = path
        .strip_prefix(&prefix)
        .ok_or_else(|| anyhow!("Definition is outside this checkout"))?;
    ensure!(
        !relative.is_empty()
            && relative.split('/').all(|part| !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains('\\')),
        "Invalid workspace file location"
    );
    Ok(relative.into())
}

fn raw_file_uri_path(uri: &str) -> Option<&str> {
    let (_, rest) = uri.split_once(':')?;
    let rest = rest.split(['?', '#']).next()?;
    let Some(authority_or_path) = rest.strip_prefix("//") else {
        return Some(rest);
    };
    if authority_or_path.starts_with('/') {
        return Some(authority_or_path);
    }
    authority_or_path
        .find('/')
        .map(|separator| &authority_or_path[separator..])
        .or(Some(""))
}

fn normalize_windows_path(path: &str) -> String {
    let mut path = path.replace('\\', "/");
    if let Some(verbatim) = path.strip_prefix("//?/") {
        if verbatim
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("UNC/"))
        {
            path = format!("//{}", &verbatim[4..]);
        } else if is_windows_drive_path(verbatim) {
            path = verbatim.to_owned();
        }
    }
    if is_windows_drive_path(&path) {
        let drive_letter = path[..1].to_ascii_uppercase();
        path.replace_range(..1, &drive_letter);
    }
    path
}

fn is_windows_drive_path(path: &str) -> bool {
    path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && path.as_bytes().get(1) == Some(&b':')
}

/// Produce one atomic, undoable buffer replacement. Never perform file I/O.
pub(super) fn action_text(
    action: &CodeAction,
    text: &str,
    root: &Path,
    path: &str,
    server_version: Option<i32>,
) -> Result<String> {
    ensure!(
        action.disabled.is_none() && action.command.is_none(),
        "This action requires an unsupported server command"
    );
    let edit = action
        .edit
        .as_ref()
        .ok_or_else(|| anyhow!("This action needs server-side resolution"))?;
    ensure!(
        edit.change_annotations
            .as_ref()
            .is_none_or(|items| items.is_empty()),
        "Annotated workspace edits require confirmation"
    );
    let mut edits = Vec::new();
    if let Some(changes) = &edit.changes {
        ensure!(edit.document_changes.is_none(), "Ambiguous workspace edit");
        for (uri, file_edits) in changes {
            ensure!(
                relative_uri(uri, root)? == path,
                "Only edits to the current buffer are supported"
            );
            edits.extend(file_edits.iter().cloned());
        }
    }
    if let Some(changes) = &edit.document_changes {
        let DocumentChanges::Edits(changes) = changes else {
            bail!("File creation, deletion and rename actions are not supported");
        };
        for change in changes {
            ensure!(
                relative_uri(&change.text_document.uri, root)? == path,
                "Only edits to the current buffer are supported"
            );
            ensure!(
                change.text_document.version.is_none()
                    || change.text_document.version == server_version,
                "Code action targets an older document version"
            );
            for edit in &change.edits {
                let OneOf::Left(edit) = edit else {
                    bail!("Annotated edits require confirmation");
                };
                edits.push(edit.clone());
            }
        }
    }
    ensure!(!edits.is_empty(), "This action has no current-buffer edits");
    let mut edits = edits
        .into_iter()
        .map(|edit| Ok((byte_range(text, edit.range)?, edit.new_text)))
        .collect::<Result<Vec<_>>>()?;
    edits.sort_by_key(|(range, _)| (range.start, range.end));
    for pair in edits.windows(2) {
        ensure!(
            pair[0].0.end <= pair[1].0.start && pair[0].0.start != pair[1].0.start,
            "Overlapping code action edits"
        );
    }
    let mut result = text.to_owned();
    for (range, replacement) in edits.into_iter().rev() {
        result.replace_range(range, &replacement);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{
        action_text, byte_offset, byte_range, completions, map_diagnostics, relative_uri,
        scalar_range, wire_position,
    };
    use lsp_types::{CodeAction, CompletionResponse, CompletionTextEdit, Position, Range, TextEdit};
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn lsp_mapping_utf16_round_trip_and_invalid_surrogate() {
        let text = "first\r\n中😀name";
        let offset = "first\r\n中😀".len();
        assert_eq!(wire_position(text, offset).unwrap(), Position::new(1, 3));
        assert_eq!(byte_offset(text, Position::new(1, 3)).unwrap(), offset);
        let crlf = "first\r\n中😀name\r\n";
        let line_end = "first\r\n中😀name".len();
        assert_eq!(byte_offset(crlf, Position::new(1, 100)).unwrap(), line_end);
        assert_eq!(byte_offset(crlf, Position::new(9, 0)).unwrap(), crlf.len());
        assert_eq!(
            scalar_range(
                crlf,
                Range::new(Position::new(1, 100), Position::new(9, 0))
            )
            .unwrap(),
            Range::new(Position::new(1, 6), Position::new(2, 0))
        );
        assert!(byte_offset(text, Position::new(1, 2)).is_err());
        assert_eq!(
            scalar_range(text, Range::new(Position::new(1, 3), Position::new(1, 7))).unwrap(),
            Range::new(Position::new(1, 2), Position::new(1, 6))
        );
        assert!(byte_range(text, Range::new(Position::new(1, 100), Position::new(1, 2))).is_err());
        assert!(
            scalar_range(text, Range::new(Position::new(1, 100), Position::new(1, 2))).is_err()
        );
    }

    #[test]
    fn lsp_mapping_diagnostics_keep_valid_items_and_count_ignored_items() {
        let (diagnostics, ignored) = map_diagnostics(
            "a😀b\r\n",
            vec![
                serde_json::json!({
                    "range":{"start":{"line":0,"character":1},"end":{"line":0,"character":3}},
                    "message":"valid"
                }),
                serde_json::json!({
                    "range":{"start":{"line":0,"character":3},"end":{"line":0,"character":1}},
                    "message":"reversed"
                }),
                serde_json::json!({"message":"malformed"}),
            ],
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].message, "valid");
        assert_eq!(
            diagnostics[0].range,
            Range::new(Position::new(0, 1), Position::new(0, 2))
        );
        assert_eq!(ignored, 2);

        let (diagnostics, ignored) =
            map_diagnostics("a", vec![serde_json::json!({"message":"malformed"})]);
        assert!(diagnostics.is_empty());
        assert_eq!(ignored, 1);
    }

    #[test]
    fn lsp_mapping_completion_replaces_prefix_and_filters_unsafe_items() {
        let CompletionResponse::Array(items) = completions(
            serde_json::json!([
                {"label":"print", "insertText":"print"},
                {"label":"snippet", "insertTextFormat":2},
                {"label":"command", "command":{"title":"run", "command":"run"}},
            ]),
            "😀 prin_suffix",
            "😀 prin".len(),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(items.len(), 1);
        let Some(CompletionTextEdit::Edit(edit)) = &items[0].text_edit else {
            panic!()
        };
        assert_eq!(
            edit.range,
            Range::new(Position::new(0, 2), Position::new(0, 13))
        );
        assert_eq!(edit.new_text, "print");
    }

    #[test]
    fn lsp_mapping_file_uris_use_the_daemon_path_syntax() {
        assert_eq!(
            relative_uri(
                &"file:///remote/a%20b/%E4%B8%AD.rs".parse().unwrap(),
                Path::new("/remote")
            )
            .unwrap(),
            "a b/中.rs"
        );
        assert_eq!(
            relative_uri(
                &"file:///C:/repo/src/lib.rs".parse().unwrap(),
                Path::new("C:\\repo")
            )
            .unwrap(),
            "src/lib.rs"
        );
        assert_eq!(
            relative_uri(
                &"file:///c:/repo/src/lib.rs".parse().unwrap(),
                Path::new("C:\\repo")
            )
            .unwrap(),
            "src/lib.rs"
        );
        assert_eq!(
            relative_uri(
                &"file:///C:/repo/src/lib.rs".parse().unwrap(),
                Path::new("c:\\repo")
            )
            .unwrap(),
            "src/lib.rs"
        );
        assert_eq!(
            relative_uri(
                &"file:///C:/repo/src/lib.rs".parse().unwrap(),
                Path::new("\\\\?\\C:\\repo")
            )
            .unwrap(),
            "src/lib.rs"
        );
        assert_eq!(
            relative_uri(
                &"file://server/share/src/lib.rs".parse().unwrap(),
                Path::new("\\\\?\\UNC\\server\\share")
            )
            .unwrap(),
            "src/lib.rs"
        );
        assert!(
            relative_uri(
                &"file:///elsewhere/lib.rs".parse().unwrap(),
                Path::new("/remote")
            )
            .is_err()
        );
        assert!(
            relative_uri(
                &"file:///D:/repo/src/lib.rs".parse().unwrap(),
                Path::new("C:\\repo")
            )
            .is_err()
        );
        assert!(
            relative_uri(
                &"file:///c:/REPO/src/lib.rs".parse().unwrap(),
                Path::new("C:\\repo")
            )
            .is_err(),
            "drive normalization must not case-fold path components"
        );
        assert!(
            relative_uri(
                &"file:///C:/repo/../repo/src/lib.rs".parse().unwrap(),
                Path::new("C:\\repo")
            )
            .is_err()
        );
        assert!(
            relative_uri(
                &"file:///C:/repo/%5C..%5Coutside.rs".parse().unwrap(),
                Path::new("C:\\repo")
            )
            .is_err()
        );
        assert!(
            relative_uri(
                &"https://example.com/file.rs".parse().unwrap(),
                Path::new("/remote")
            )
            .is_err()
        );
    }

    #[test]
    fn lsp_mapping_actions_are_atomic_and_confined() {
        let mut action: CodeAction = serde_json::from_value(serde_json::json!({
            "title":"Fix", "edit":{"changes":{"file:///repo/lib.rs":[
                {"range":{"start":{"line":0,"character":3},"end":{"line":0,"character":4}},"newText":"B"},
                {"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"newText":"A"}
            ]}}
        })).unwrap();
        assert_eq!(
            action_text(&action, "a😀b", Path::new("/repo"), "lib.rs", Some(2)).unwrap(),
            "A😀B"
        );
        action.edit.as_mut().unwrap().changes = Some(HashMap::from([(
            "file:///c:/repo/lib.rs".parse().unwrap(),
            vec![TextEdit {
                range: Range::new(Position::new(0, 3), Position::new(0, 4)),
                new_text: "B".into(),
            }],
        )]));
        assert_eq!(
            action_text(&action, "a😀b", Path::new("C:\\repo"), "lib.rs", Some(2)).unwrap(),
            "a😀B"
        );
        assert!(action_text(&action, "a😀b", Path::new("/repo"), "other.rs", Some(2)).is_err());
        action.command = Some(
            serde_json::from_value(serde_json::json!({"title":"Run", "command":"run"})).unwrap(),
        );
        assert!(action_text(&action, "a😀b", Path::new("/repo"), "lib.rs", Some(2)).is_err());
    }
}
