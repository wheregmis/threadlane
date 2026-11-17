//! Ephemeral editor requests. Contents and results are never session events.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Bound both unsaved snapshots and language-server replies at the host boundary.
pub const MAX_EDITOR_LSP_BYTES: usize = 1024 * 1024;

/// Zero-based LSP wire coordinates: character is a UTF-16 code-unit column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorLspPosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorLspRange {
    pub start: EditorLspPosition,
    pub end: EditorLspPosition,
}

/// A closed allowlist; clients cannot invoke arbitrary extension tools or RPCs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum EditorLspOperation {
    Completion { trigger_character: Option<String> },
    Hover,
    Definition,
    Diagnostics,
    CodeActions { range: EditorLspRange },
    Close,
}

/// A complete immutable buffer snapshot, scoped to its owning session/checkout.
/// `document_id` is unique for the client's open buffer, not its mutable tab index.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditorLspRequest {
    pub session_id: String,
    pub work_dir: PathBuf,
    pub path: String,
    pub document_id: u64,
    pub version: u64,
    pub expected_runtime_id: Option<u64>,
    pub text: String,
    pub position: EditorLspPosition,
    pub operation: EditorLspOperation,
}

/// LSP JSON retains its UTF-16 ranges until the UI adapter converts them.
/// The requester must still validate its buffer, client connection and session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditorLspResponse {
    pub document_id: u64,
    pub version: u64,
    pub runtime_id: u64,
    pub server: String,
    pub server_document_version: Option<i32>,
    pub result: serde_json::Value,
    /// Only diagnostics known to belong to this snapshot. `None` means pending
    /// or unsupported, not a clean buffer; an empty array means no diagnostics.
    pub diagnostics: Option<Vec<serde_json::Value>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::CommandResponse;

    #[test]
    fn editor_lsp_request_and_reply_round_trip() {
        let request = EditorLspRequest {
            session_id: "session".into(),
            work_dir: "/workspace".into(),
            path: "src/main.rs".into(),
            document_id: 42,
            version: 7,
            expected_runtime_id: Some(9),
            text: "fn main() {}".into(),
            position: EditorLspPosition {
                line: 0,
                character: 3,
            },
            operation: EditorLspOperation::CodeActions {
                range: EditorLspRange {
                    start: EditorLspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: EditorLspPosition {
                        line: 0,
                        character: 2,
                    },
                },
            },
        };
        let wire = serde_json::to_vec(&request).unwrap();
        let decoded: EditorLspRequest = serde_json::from_slice(&wire).unwrap();
        assert_eq!(decoded.session_id, request.session_id);
        assert_eq!(decoded.path, request.path);
        assert_eq!(decoded.position, request.position);
        assert_eq!(decoded.operation, request.operation);
        assert_eq!(decoded.text, request.text);

        let response = CommandResponse::EditorLsp {
            result: Ok(EditorLspResponse {
                document_id: 42,
                version: 7,
                runtime_id: 9,
                server: "rust-analyzer".into(),
                server_document_version: Some(4),
                result: serde_json::json!({"items":[]}),
                diagnostics: Some(vec![]),
            }),
        };
        let wire = serde_json::to_vec(&response).unwrap();
        let decoded: CommandResponse = serde_json::from_slice(&wire).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            serde_json::to_value(response).unwrap()
        );
    }
}
