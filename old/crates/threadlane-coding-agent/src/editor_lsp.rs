use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Semaphore;

use crate::capabilities::BrokerAwareWasiToolExecutor;
use threadlane_protocol::editor_lsp::{EditorLspOperation, EditorLspRequest, MAX_EDITOR_LSP_BYTES};
use threadlane_wasi::broker::CapabilityDispatcher;
use threadlane_wasi::WasiExtensionManager;

pub struct EditorLspService {
    work_dir: PathBuf,
    executor: BrokerAwareWasiToolExecutor,
    admission: Arc<Semaphore>,
}

impl EditorLspService {
    pub(crate) fn new(
        work_dir: PathBuf,
        extensions: Arc<WasiExtensionManager>,
        broker_dispatcher: Arc<CapabilityDispatcher>,
    ) -> Self {
        Self {
            work_dir,
            executor: BrokerAwareWasiToolExecutor::new(extensions, broker_dispatcher),
            admission: Arc::new(Semaphore::new(1)),
        }
    }

    pub fn work_dir(&self) -> &std::path::Path {
        &self.work_dir
    }

    pub async fn execute(&self, request: &EditorLspRequest) -> Result<Value, String> {
        if request.text.len() > MAX_EDITOR_LSP_BYTES {
            return Err("Editor LSP buffer snapshot exceeds the 1 MiB limit".into());
        }
        let _permit = self
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| "Editor LSP busy; retry shortly".to_string())?;
        let (operation, trigger_character, range) = match &request.operation {
            EditorLspOperation::Completion { trigger_character } => {
                ("completion", trigger_character.clone(), None)
            }
            EditorLspOperation::Hover => ("hover", None, None),
            EditorLspOperation::Definition => ("definition", None, None),
            EditorLspOperation::Diagnostics => ("diagnostics", None, None),
            EditorLspOperation::CodeActions { range } => (
                "code_actions",
                None,
                Some(serde_json::to_value(range).map_err(|error| error.to_string())?),
            ),
            EditorLspOperation::Close => ("close", None, None),
        };
        let arguments = json!({
            "operation": operation,
            "path": request.path,
            "document_id": request.document_id,
            "version": request.version,
            "text": request.text,
            "position": request.position,
            "trigger_character": trigger_character,
            "range": range,
        });
        let arguments = serde_json::to_string(&arguments).map_err(|error| error.to_string())?;
        if arguments.len() > MAX_EDITOR_LSP_BYTES {
            return Err("Editor LSP command payload exceeds the 1 MiB limit".into());
        }
        let reply = self.executor.execute_editor_lsp_command(&arguments).await?;
        if reply.len() > MAX_EDITOR_LSP_BYTES {
            return Err("Editor LSP reply exceeds the 1 MiB limit".into());
        }
        serde_json::from_str(&reply)
            .map_err(|error| format!("Invalid editor LSP extension response: {error}"))
    }
}
