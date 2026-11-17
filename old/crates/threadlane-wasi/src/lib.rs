pub mod broker;
mod checkpoint;
#[cfg(test)]
mod checkpoint_tests;
pub mod packages;
pub mod settings;

pub use broker::*;
use checkpoint::SavedToolReply;
pub(crate) use packages::validate_extension_id;

use checkpoint::{persist_checkpoint, BrokerIntent, ExtensionCheckpoint};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use threadlane_protocol::{AgentToolDefinition, ToolExecutionIdentity};
use wasmi::{Caller, Engine, Extern, Func, Linker, Memory, Module, Store};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasiToolDefinition {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasiCommandDefinition {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasiExtensionManifest {
    #[serde(default = "default_api_version")]
    api_version: u32,
    pub name: String,
    version: String,
    description: String,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    tools: Vec<WasiToolDefinition>,
    #[serde(default)]
    pub commands: Vec<WasiCommandDefinition>,
    #[serde(default)]
    hooks: Vec<String>,
}

fn default_api_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WasiExtensionEvent {
    topic: String,
    payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasiExtensionInvocation {
    api_version: u32,
    kind: String,
    name: String,
    arguments: Value,
    #[serde(default)]
    state: Value,
    /// Events are queued by the host and delivered on this extension's next invocation.
    #[serde(default)]
    events: Vec<WasiExtensionEvent>,
}

/// Effects retained for API v1 compatibility. Bundled v2 extensions use
/// broker requests instead of this legacy response channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WasiLegacyEffect {
    SetToolPolicy { policy: String },
    RequestModelTurn { prompt: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WasiHookMiddleware {
    #[serde(default)]
    pub block: Option<bool>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    arguments: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    context: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WasiExtensionResponse {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// Reinvoke the same tool after queued broker outcomes are available.
    #[serde(default)]
    pub continue_after_broker: bool,
    #[serde(default)]
    state: Option<Value>,
    #[serde(default)]
    effects: Vec<WasiLegacyEffect>,
    #[serde(default)]
    pub middleware: Option<WasiHookMiddleware>,
}

#[derive(Debug, Clone, Default)]
pub struct WasiExtensionInvocationResult {
    pub api_version: u32,
    pub response: WasiExtensionResponse,
    broker_requests: Vec<BrokerRequest>,
    pub host_broker_requests: Vec<HostBrokerRequest>,
    invoking_extension: String,
}

#[derive(Debug, Clone, Default)]
pub struct WasiExtensionCommandResult {
    pub api_version: u32,
    pub message: String,
    pub effects: Vec<WasiLegacyEffect>,
    pub host_broker_requests: Vec<HostBrokerRequest>,
}

impl WasiExtensionInvocationResult {
    pub fn into_command_result(self) -> Result<WasiExtensionCommandResult, String> {
        if let Some(error) = self.response.error {
            return Err(error);
        }
        Ok(WasiExtensionCommandResult {
            api_version: self.api_version,
            message: self.response.message.unwrap_or_default(),
            effects: self.response.effects,
            host_broker_requests: self.host_broker_requests,
        })
    }
}

fn merge_extension_state(
    previous: &Value,
    response_state: Option<Value>,
    ephemeral: bool,
    ephemeral_keys: &HashSet<String>,
) -> Result<Value, String> {
    if ephemeral {
        let Some(response_state) = response_state else {
            return Ok(previous.clone());
        };
        let Some(previous) = previous.as_object() else {
            if response_state
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
            {
                return Ok(previous.clone());
            }
            return Err(
                "Cannot preserve non-ephemeral extension state because it is not an object".into(),
            );
        };
        let mut next = response_state
            .as_object()
            .cloned()
            .ok_or("Ephemeral extension state response must be an object")?;
        for (key, value) in previous {
            if !ephemeral_keys.contains(key) {
                next.insert(key.clone(), value.clone());
            }
        }
        return Ok(Value::Object(next));
    }

    let mut next = response_state.unwrap_or_else(|| serde_json::json!({}));
    let Some(previous) = previous.as_object() else {
        return Ok(next);
    };
    let reserved = previous
        .iter()
        .filter(|(key, _)| ephemeral_keys.contains(*key))
        .collect::<Vec<_>>();
    if reserved.is_empty() {
        return Ok(next);
    }
    let next = next
        .as_object_mut()
        .ok_or("Cannot preserve ephemeral extension state because the response state is not an object")?;
    for (key, value) in reserved {
        next.insert(key.clone(), value.clone());
    }
    Ok(Value::Object(next.clone()))
}

#[derive(Default)]
struct WasiStoreData {
    policy: CapabilityPolicy,
    requests: Vec<BrokerRequest>,
}

pub struct WasiExtension {
    pub manifest: WasiExtensionManifest,
    file_path: Option<PathBuf>,
    wasm_bytes: Vec<u8>,
    engine: Engine,
    module: Arc<Module>,
    trusted_global_editor_lsp: bool,
}

fn is_trusted_global_editor_lsp_record(
    record: &packages::ExtensionRecord,
    project_root: Option<&Path>,
) -> bool {
    if record.name() != "lsp_ext"
        || record.scope() != packages::ExtensionScope::Global
        || !record.is_enabled()
        || !record.is_effective()
    {
        return false;
    }
    let Some(project_root) = project_root.and_then(|root| root.canonicalize().ok()) else {
        return false;
    };
    let Ok(module_path) = record.module_path().canonicalize() else {
        return false;
    };
    !module_path.starts_with(project_root)
}

impl WasiExtension {
    fn create_linker(engine: &Engine, store: &mut Store<WasiStoreData>) -> Linker<WasiStoreData> {
        let mut linker = Linker::new(engine);
        let _ = linker.define(
            "wasi_snapshot_preview1",
            "environ_get",
            Func::wrap(&mut *store, |_: i32, _: i32| -> i32 { 0 }),
        );
        let _ = linker.define(
            "wasi_snapshot_preview1",
            "environ_sizes_get",
            Func::wrap(&mut *store, |_: i32, _: i32| -> i32 { 0 }),
        );
        let _ = linker.define(
            "wasi_snapshot_preview1",
            "fd_write",
            Func::wrap(&mut *store, |_: i32, _: i32, _: i32, _: i32| -> i32 { 0 }),
        );
        let _ = linker.define(
            "wasi_snapshot_preview1",
            "fd_seek",
            Func::wrap(&mut *store, |_: i32, _: i64, _: i32, _: i32| -> i32 { 0 }),
        );
        let _ = linker.define(
            "wasi_snapshot_preview1",
            "fd_close",
            Func::wrap(&mut *store, |_: i32| -> i32 { 0 }),
        );
        let _ = linker.define(
            "wasi_snapshot_preview1",
            "proc_exit",
            Func::wrap(&mut *store, |_: i32| {}),
        );
        let _ = linker.define(
            "wasi_snapshot_preview1",
            "args_get",
            Func::wrap(&mut *store, |_: i32, _: i32| -> i32 { 0 }),
        );
        let _ = linker.define(
            "wasi_snapshot_preview1",
            "args_sizes_get",
            Func::wrap(&mut *store, |_: i32, _: i32| -> i32 { 0 }),
        );
        let _ = linker.define(
            "wasi_snapshot_preview1",
            "clock_time_get",
            Func::wrap(&mut *store, |_: i32, _: i64, _: i32| -> i32 { 0 }),
        );
        let _ = linker.define(
            "threadlane_host",
            "request",
            Func::wrap(
                &mut *store,
                |mut caller: Caller<WasiStoreData>,
                 request_ptr: i32,
                 request_len: i32,
                 response_ptr: i32,
                 response_capacity: i32| {
                    broker_request(
                        &mut caller,
                        request_ptr,
                        request_len,
                        response_ptr,
                        response_capacity,
                    )
                },
            ),
        );
        linker
    }

    /// Lenient loader for tests only; production discovery goes through
    /// [`Self::load_from_file`], which requires a manifest.
    #[cfg(test)]
    fn load_from_bytes(wasm_bytes: Vec<u8>) -> Result<Self, String> {
        Self::load_from_bytes_inner(wasm_bytes, false)
    }

    fn load_from_bytes_inner(wasm_bytes: Vec<u8>, require_manifest: bool) -> Result<Self, String> {
        let engine = Engine::default();
        let module = Module::new(&engine, &wasm_bytes[..])
            .map_err(|e| format!("Failed to parse WASM module: {e}"))?;
        let mut store = Store::new(&engine, WasiStoreData::default());
        let linker = Self::create_linker(&engine, &mut store);
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|e| format!("Failed to instantiate or start WASM module: {e}"))?;

        let manifest = match instance.get_typed_func::<(), u64>(&store, "extension_info") {
            Ok(info) => {
                let result = info.call(&mut store, ()).map_err(|e| e.to_string())?;
                read_json_result(&mut store, &instance, result)?
            }
            Err(_) if require_manifest => {
                return Err(
                    "WASM module must export `extension_info() -> i64` with its manifest".into(),
                )
            }
            Err(_) => WasiExtensionManifest {
                api_version: 1,
                name: "unnamed_wasi_ext".into(),
                version: "0.1.0".into(),
                description: "WASI extension".into(),
                capabilities: vec![],
                tools: vec![],
                commands: vec![],
                hooks: vec![],
            },
        };

        if manifest.api_version != 1 && manifest.api_version != BROKER_API_VERSION {
            return Err(format!(
                "Unsupported extension API version: {}",
                manifest.api_version
            ));
        }

        Ok(Self {
            manifest,
            file_path: None,
            wasm_bytes,
            engine,
            module: Arc::new(module),
            trusted_global_editor_lsp: false,
        })
    }

    pub fn capability_policy(&self) -> CapabilityPolicy {
        if self.manifest.api_version < BROKER_API_VERSION {
            CapabilityPolicy::default()
        } else {
            CapabilityPolicy::new(self.manifest.capabilities.clone())
        }
    }

    /// Loads a module from disk for production discovery. Unlike
    /// [`Self::load_from_bytes`] (kept lenient for tests), a manifest is
    /// mandatory here: the synthesized `unnamed_wasi_ext` with empty
    /// capabilities loads a silently nonfunctional extension, so
    /// manifest-less modules are denied with a rebuild pointer instead.
    fn load_from_file(path: &Path) -> Result<Self, String> {
        let bytes =
            fs::read(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        let mut ext = Self::load_from_bytes_inner(bytes, true)?;
        ext.file_path = Some(path.to_path_buf());
        Ok(ext)
    }

    pub fn load_from_file_requiring_manifest(path: &Path) -> Result<Self, String> {
        let bytes =
            fs::read(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        let mut extension = Self::load_from_bytes_inner(bytes, true)?;
        extension.file_path = Some(path.to_path_buf());
        Ok(extension)
    }

    fn call_with_policy<T: Serialize>(
        &self,
        export: &str,
        invocation: &T,
        api_version: u32,
        policy: CapabilityPolicy,
    ) -> Result<WasiExtensionInvocationResult, String> {
        let mut store = Store::new(
            &self.engine,
            WasiStoreData {
                policy,
                requests: vec![],
            },
        );
        let linker = Self::create_linker(&self.engine, &mut store);
        let instance = linker
            .instantiate_and_start(&mut store, &self.module)
            .map_err(|e| e.to_string())?;
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or("Memory export not found")?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&store, "alloc")
            .map_err(|_| "WASM module must export `alloc(size: i32) -> i32`")?;
        let input = serde_json::to_vec(invocation).map_err(|e| e.to_string())?;
        let ptr = alloc
            .call(&mut store, input.len() as i32)
            .map_err(|e| e.to_string())?;
        memory
            .write(&mut store, ptr as usize, &input)
            .map_err(|e| e.to_string())?;
        let function = instance
            .get_typed_func::<(i32, i32), u64>(&store, export)
            .map_err(|_| format!("WASM module must export `{export}`"))?;
        let result = function
            .call(&mut store, (ptr, input.len() as i32))
            .map_err(|e| e.to_string())?;
        let response = read_json_result(&mut store, &instance, result)?;
        Ok(WasiExtensionInvocationResult {
            api_version,
            response,
            broker_requests: std::mem::take(&mut store.data_mut().requests),
            host_broker_requests: Vec::new(),
            invoking_extension: String::new(),
        })
    }
}

fn read_json_result<T: for<'de> Deserialize<'de>, D>(
    store: &mut Store<D>,
    instance: &wasmi::Instance,
    result: u64,
) -> Result<T, String> {
    let ptr = (result >> 32) as usize;
    let len = (result & 0xFFFF_FFFF) as usize;
    if len > MAX_WASM_JSON_BYTES {
        return Err(format!(
            "WASM extension returned {len} bytes, exceeding the {MAX_WASM_JSON_BYTES}-byte JSON cap"
        ));
    }
    let memory = instance.get_memory(&*store, "memory").ok_or("No memory")?;
    let mut buffer = vec![0; len];
    memory
        .read(&*store, ptr, &mut buffer)
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&buffer).map_err(|e| e.to_string())
}

fn broker_request(
    caller: &mut Caller<WasiStoreData>,
    request_ptr: i32,
    request_len: i32,
    response_ptr: i32,
    response_capacity: i32,
) -> i32 {
    if request_ptr < 0 || request_len < 0 || response_ptr < 0 || response_capacity < 0 {
        return -1;
    }
    let request = match read_memory(caller, request_ptr, request_len) {
        Ok(request) => request,
        Err(()) => return -1,
    };
    let response = match serde_json::from_slice::<BrokerRequest>(&request) {
        Ok(request) if request.api_version == BROKER_API_VERSION => {
            if caller.data().policy.allows(&request.capability) {
                caller.data_mut().requests.push(request);
                BrokerResponse::ok(Value::Null)
            } else {
                caller.data().policy.denied_response(&request.capability)
            }
        }
        Ok(request) => BrokerResponse::error(
            "invalid_request",
            format!("Unsupported broker API version: {}", request.api_version),
        ),
        Err(error) => BrokerResponse::error("invalid_request", error.to_string()),
    };
    write_broker_response(caller, response_ptr, response_capacity, &response)
}

fn read_memory(caller: &Caller<WasiStoreData>, ptr: i32, len: i32) -> Result<Vec<u8>, ()> {
    let memory = exported_memory(caller)?;
    let range = checked_memory_range(caller, ptr, len)?;
    let mut bytes = vec![0; range.len()];
    memory
        .read(caller, range.start, &mut bytes)
        .map_err(|_| ())?;
    Ok(bytes)
}

fn checked_memory_range(
    caller: &Caller<WasiStoreData>,
    ptr: i32,
    len: i32,
) -> Result<Range<usize>, ()> {
    if ptr < 0 || len < 0 {
        return Err(());
    }
    let start = ptr as usize;
    let end = start.checked_add(len as usize).ok_or(())?;
    let memory = exported_memory(caller)?;
    if end > memory.data_size(caller) {
        return Err(());
    }
    Ok(start..end)
}

fn write_memory(caller: &mut Caller<WasiStoreData>, ptr: i32, bytes: &[u8]) -> Result<(), ()> {
    exported_memory(caller)?
        .write(caller, ptr as usize, bytes)
        .map_err(|_| ())
}

fn exported_memory(caller: &Caller<WasiStoreData>) -> Result<Memory, ()> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or(())
}

fn write_broker_response(
    caller: &mut Caller<WasiStoreData>,
    response_ptr: i32,
    response_capacity: i32,
    response: &BrokerResponse,
) -> i32 {
    let bytes = match serde_json::to_vec(response) {
        Ok(bytes) => bytes,
        Err(_) => return -1,
    };
    let len = match i32::try_from(bytes.len()) {
        Ok(len) => len,
        Err(_) => return -1,
    };
    if len > response_capacity {
        return -len;
    }
    if write_memory(caller, response_ptr, &bytes).is_err() {
        return -1;
    }
    len
}

/// Produces a filesystem-safe, collision-free directory name for a session ID.
fn encode_state_component(component: &str) -> String {
    component
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn extension_state_file_name(extension_name: &str) -> String {
    if validate_extension_id(extension_name).is_ok() {
        format!("{extension_name}.json")
    } else {
        format!(".encoded-{}.json", encode_state_component(extension_name))
    }
}

fn host_state_file_name(key: &str) -> String {
    if validate_extension_id(key).is_ok() {
        format!(".host.{key}.json")
    } else {
        format!(".host..encoded-{}.json", encode_state_component(key))
    }
}

type PendingExtensionEvents =
    HashMap<Option<String>, HashMap<String, Vec<Arc<WasiExtensionEvent>>>>;

/// Bound published notifications (oldest evicted first). Broker outcomes must
/// survive until delivery; their lifetime follows the invoking operation.
const MAX_PENDING_EVENTS_PER_EXTENSION: usize = 256;

/// Cap bytes read back from WASM memory for a JSON result: without a bound,
/// a corrupt or hostile `len` allocates gigabytes before the bounds check
/// can fail.
const MAX_WASM_JSON_BYTES: usize = 8 * 1024 * 1024;

struct StateOwner {
    scope: Option<String>,
    directory: PathBuf,
    receipts_restored: bool,
    replies_restored: bool,
    _lease: fs::File,
}

#[derive(Debug)]
pub(crate) struct StateWriteError {
    message: String,
    replaced: bool,
}

impl StateWriteError {
    fn before(error: impl std::fmt::Display) -> Self {
        Self {
            message: error.to_string(),
            replaced: false,
        }
    }
}

impl std::fmt::Display for StateWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

enum UnconfirmedState {
    Extension {
        name: String,
        state: Value,
        events: Vec<Arc<WasiExtensionEvent>>,
        acknowledged: Vec<Arc<WasiExtensionEvent>>,
        last_broker_id: u64,
        intents: Vec<Arc<BrokerIntent>>,
        unpublished: bool,
        terminal_reply: Option<Arc<SavedToolReply>>,
    },
    Host {
        key: String,
        value: Value,
    },
}

struct UnconfirmedWrite {
    path: PathBuf,
    state: UnconfirmedState,
    error: String,
}

impl UnconfirmedWrite {
    fn blocked_message(&self) -> String {
        format!("{}; preserve {} and repair storage, then have the owning host call recover_state_commit. Do not retry the original invocation, overwrite the checkpoint, or replay broker operations while its commit is unconfirmed", self.error, self.path.display())
    }
}

#[derive(Default)]
pub struct WasiExtensionManager {
    extensions: RwLock<HashMap<String, Arc<WasiExtension>>>,
    tool_definitions: RwLock<Arc<[AgentToolDefinition]>>,
    states: Mutex<HashMap<String, Value>>,
    host_state: Mutex<HashMap<String, Value>>,
    /// Serializes storage/cache commits and reloads without holding cache locks over I/O.
    state_commit: Mutex<()>,
    /// A scope's caches and checkpoints have one writer, including across processes.
    state_owner: Mutex<Option<StateOwner>>,
    /// A replaced file must be confirmed before stale caches can write again.
    unconfirmed_write: Mutex<Option<UnconfirmedWrite>>,
    subscriptions: Mutex<HashMap<String, HashSet<String>>>,
    pending_events: Mutex<PendingExtensionEvents>,
    /// A batch remains unacknowledged until its next state checkpoint commits.
    /// Presence also claims the extension slot, including calls with no events.
    in_flight_events: Mutex<HashMap<String, Vec<Arc<WasiExtensionEvent>>>>,
    /// Delivery ends at each checkpoint; call ownership spans broker awaits.
    active_operations: Mutex<HashSet<String>>,
    /// Serializes broker-backed operations owned by the LSP extension.
    lsp_operation_gate: tokio::sync::Mutex<()>,
    unsettled_broker: Mutex<HashMap<String, Vec<Arc<BrokerIntent>>>>,
    ephemeral_broker_receipts: Mutex<HashMap<String, HashSet<u64>>>,
    ephemeral_state_keys: Mutex<HashMap<String, HashSet<String>>>,
    terminal_replies: Mutex<HashMap<String, Arc<SavedToolReply>>>,
    last_broker_id: AtomicU64,
    pending_broker_requests: Mutex<HashMap<Option<String>, Vec<HostBrokerRequest>>>,
    capability_grant_policy: Mutex<HostCapabilityGrantPolicy>,
    state_dir: Option<PathBuf>,
    /// Existing work directory anchors directory-entry durability for state storage.
    project_root: Option<PathBuf>,
    /// Stateful conversational extensions are isolated by the active session.
    /// `None` retains the project-wide scope for callers that explicitly need it.
    session_id: Mutex<Option<String>>,
}

/// Owns one extension's state slot through an entire call and its continuations.
/// Keep this guard until broker outcomes commit. Dropping it releases the slot;
/// durable unsettled receipts still prevent replay after cancellation.
pub struct WasiExtensionOperation<'a> {
    manager: &'a WasiExtensionManager,
    extension: Arc<WasiExtension>,
    kind: &'static str,
    name: String,
    ephemeral: bool,
}

impl WasiExtensionOperation<'_> {
    pub fn invoke(&mut self, args: &str) -> Result<WasiExtensionInvocationResult, String> {
        self.manager.invoke_owned(
            &self.extension,
            self.kind,
            &self.name,
            args,
            true,
            None,
            self.ephemeral,
        )
    }

    pub fn invoke_for_execution(
        &mut self,
        args: &str,
        identity: &ToolExecutionIdentity,
    ) -> Result<WasiExtensionInvocationResult, String> {
        self.manager.invoke_owned(
            &self.extension,
            self.kind,
            &self.name,
            args,
            true,
            Some(identity),
            false,
        )
    }

    pub fn invoke_after_tool(
        &mut self,
        args: &str,
        identity: &ToolExecutionIdentity,
    ) -> Result<WasiExtensionInvocationResult, String> {
        if self.kind != "hook" || self.name != "after_tool_call" {
            return Err("Only the owning after-tool hook may retain a saved tool reply".into());
        }
        self.manager.invoke_owned(
            &self.extension,
            self.kind,
            &self.name,
            args,
            true,
            Some(identity),
            false,
        )
    }

    /// Settle requests proven not dispatched and save a host terminal failure
    /// in that same checkpoint, without entering the extension again.
    pub fn finish_with_error(
        &mut self,
        args: &str,
        identity: &ToolExecutionIdentity,
        results: Vec<BrokerOperationResult>,
        error: String,
    ) -> Result<(), String> {
        if self.kind != "tool"
            || !identity.matches_call(&identity.tool_call_id, &identity.tool_name)
        {
            return Err("Terminal failure requires its committed tool identity".into());
        }
        if results
            .iter()
            .any(|result| result.invoking_extension != self.extension.manifest.name)
        {
            return Err("Terminal failure outcomes belong to another extension".into());
        }
        let reply = Arc::new(SavedToolReply {
            identity: identity.clone(),
            extension_name: self.extension.manifest.name.clone(),
            tool_name: self.name.clone(),
            arguments: serde_json::from_str(args)
                .unwrap_or_else(|_| serde_json::json!({"raw":args})),
            result: Err(error),
            broker_receipt_ids: vec![],
            canonical_result: None,
        });
        self.manager
            .enqueue_broker_results_with_reply(results, Some(&reply))
    }
}

impl Drop for WasiExtensionOperation<'_> {
    fn drop(&mut self) {
        if self.ephemeral {
            self.manager
                .discard_ephemeral_broker_receipts(&self.extension.manifest.name);
        }
        if let Ok(mut active) = self.manager.active_operations.lock() {
            active.remove(&self.extension.manifest.name);
        }
    }
}

impl WasiExtensionManager {
    fn discard_ephemeral_broker_receipts(&self, extension_name: &str) {
        let Ok(_commit) = self.state_commit.lock() else {
            return;
        };
        let receipts = self
            .ephemeral_broker_receipts
            .lock()
            .ok()
            .and_then(|mut receipts| receipts.remove(extension_name))
            .unwrap_or_default();
        if receipts.is_empty() {
            return;
        }
        if let Ok(mut unsettled) = self.unsettled_broker.lock() {
            if let Some(intents) = unsettled.get_mut(extension_name) {
                intents.retain(|intent| !receipts.contains(&intent.receipt.id));
            }
        }
        if let Ok(mut pending) = self.pending_events.lock() {
            for queues in pending.values_mut() {
                if let Some(events) = queues.get_mut(extension_name) {
                    events.retain(|event| {
                        event
                            .payload
                            .get("receipt_id")
                            .and_then(Value::as_u64)
                            .is_none_or(|id| !receipts.contains(&id))
                    });
                }
            }
        }
        if let Ok(mut in_flight) = self.in_flight_events.lock() {
            if let Some(events) = in_flight.get_mut(extension_name) {
                events.retain(|event| {
                    event
                        .payload
                        .get("receipt_id")
                        .and_then(Value::as_u64)
                        .is_none_or(|id| !receipts.contains(&id))
                });
            }
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    fn extension_names(&self) -> Result<HashSet<String>, String> {
        self.extensions
            .read()
            .map(|extensions| extensions.keys().cloned().collect())
            .map_err(|_| "Extension registry lock poisoned".to_string())
    }

    #[cfg(test)]
    fn extension_manifest(&self, name: &str) -> Option<WasiExtensionManifest> {
        self.extensions
            .read()
            .ok()?
            .get(name)
            .map(|extension| extension.manifest.clone())
    }

    pub fn extension_manifests(&self) -> Vec<WasiExtensionManifest> {
        let Ok(extensions) = self.extensions.read() else {
            return Vec::new();
        };
        let mut manifests = extensions
            .values()
            .map(|extension| extension.manifest.clone())
            .collect::<Vec<_>>();
        manifests.sort_by(|left, right| left.name.cmp(&right.name));
        manifests
    }

    #[cfg(test)]
    fn register_extension(&self, extension: WasiExtension) -> Result<(), String> {
        {
            self.extensions
                .write()
                .map_err(|_| "Extension registry lock poisoned".to_string())?
                .insert(extension.manifest.name.clone(), Arc::new(extension));
        }
        self.rebuild_tool_definitions()
    }

    pub fn reload_from_roots(
        &self,
        global_threadlane_dir: Option<&Path>,
        project_root: Option<&Path>,
    ) -> Result<usize, String> {
        let records = packages::ExtensionManager::new(
            global_threadlane_dir.map(Path::to_path_buf),
            project_root.map(Path::to_path_buf),
        )
        .discover_checked()?;
        let mut loaded = HashMap::new();
        for record in records.into_iter().filter(|record| record.is_effective()) {
            // A module that changed (or vanished) between discovery and load
            // skips like a discovery-time failure: warn, retire the stale
            // registration below, and keep the other extensions running.
            let mut extension = match WasiExtension::load_from_file(record.module_path()) {
                Ok(extension) => extension,
                Err(error) => {
                    tracing::warn!(
                        "Skipping unloadable extension '{}': {error}",
                        record.module_path().display()
                    );
                    continue;
                }
            };
            if extension.manifest.name != record.name() {
                return Err(format!(
                    "Extension manifest changed while reloading '{}'",
                    record.module_path().display()
                ));
            }
            extension.trusted_global_editor_lsp =
                is_trusted_global_editor_lsp_record(&record, project_root);
            loaded.insert(extension.manifest.name.clone(), Arc::new(extension));
        }
        let loaded_count = loaded.len();
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        self.ensure_storage_confirmed()?;
        self.ensure_no_in_flight_invocations()?;
        self.ensure_state_ownership()?;
        let scope = self.session_scope()?;
        let mut persisted_states = HashMap::new();
        let mut restored_events = HashMap::new();
        let mut unsettled = HashMap::new();
        let mut terminal_replies = HashMap::new();
        let mut last_id = 0;
        for name in loaded.keys() {
            if let Some(checkpoint) = self.load_checkpoint_in_scope(name, &scope)? {
                if let Some(reply) = checkpoint.terminal_reply {
                    terminal_replies.insert(name.clone(), Arc::new(reply));
                }
                last_id = last_id.max(checkpoint.last_broker_id);
                unsettled.insert(
                    name.clone(),
                    checkpoint.unsettled.into_iter().map(Arc::new).collect(),
                );
                persisted_states.insert(name.clone(), checkpoint.state);
                restored_events.insert(
                    name.clone(),
                    checkpoint.broker_events.into_iter().map(Arc::new).collect(),
                );
            }
        }
        let mut extensions = self
            .extensions
            .write()
            .map_err(|_| "Extension registry lock poisoned".to_string())?;
        self.last_broker_id.fetch_max(last_id, Ordering::SeqCst);
        *self
            .unsettled_broker
            .lock()
            .map_err(|_| "Broker intent lock poisoned".to_string())? = unsettled;
        self.terminal_replies
            .lock()
            .map_err(|_| "Extension reply lock poisoned".to_string())?
            .extend(terminal_replies);
        let retired = extensions
            .iter()
            .filter(|(name, previous)| {
                loaded.get(*name).is_none_or(|current| {
                    previous.file_path != current.file_path
                        || previous.wasm_bytes != current.wasm_bytes
                })
            })
            .map(|(name, _)| name.clone())
            .collect::<HashSet<_>>();
        {
            let mut states = self
                .states
                .lock()
                .map_err(|_| "Extension state lock poisoned".to_string())?;
            states.extend(persisted_states);
        }
        {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .map_err(|_| "Extension subscription lock poisoned".to_string())?;
            subscriptions.retain(|name, _| !retired.contains(name));
        }
        {
            let mut pending_events = self
                .pending_events
                .lock()
                .map_err(|_| "Extension event lock poisoned".to_string())?;
            for events in pending_events.values_mut() {
                events.retain(|name, _| !retired.contains(name));
            }
            Self::replace_broker_events(pending_events.entry(scope).or_default(), restored_events);
        }
        {
            let mut pending_requests = self
                .pending_broker_requests
                .lock()
                .map_err(|_| "Extension broker request lock poisoned".to_string())?;
            for requests in pending_requests.values_mut() {
                requests.retain(|request| !retired.contains(&request.invoking_extension));
            }
        }
        *extensions = loaded;
        drop(extensions);
        self.rebuild_tool_definitions()?;
        Ok(loaded_count)
    }

    #[cfg(test)]
    fn for_project(project_dir: &Path) -> Self {
        Self {
            state_dir: Some(project_dir.join(".threadlane/state/extensions")),
            project_root: Some(project_dir.to_path_buf()),
            ..Self::default()
        }
    }

    pub fn with_capability_grant_policy(policy: HostCapabilityGrantPolicy) -> Self {
        Self {
            capability_grant_policy: Mutex::new(policy),
            ..Self::default()
        }
    }

    fn capability_grant_policy(&self) -> Result<HostCapabilityGrantPolicy, String> {
        self.capability_grant_policy
            .lock()
            .map(|policy| policy.clone())
            .map_err(|_| "Extension capability policy lock poisoned".to_string())
    }

    /// Creates a manager whose extension state belongs to one conversation.
    pub fn for_project_session(project_dir: &Path, session_id: impl Into<String>) -> Self {
        Self {
            state_dir: Some(project_dir.join(".threadlane/state/extensions")),
            project_root: Some(project_dir.to_path_buf()),
            session_id: Mutex::new(Some(session_id.into())),
            ..Self::default()
        }
    }

    /// Switches the active state scope and reloads every registered extension.
    /// Callers should serialize this with extension invocation.
    ///
    /// Queues belonging to other scopes are evicted: they are session-owned,
    /// and retaining them grows without bound plus risks delivering a stale
    /// `broker_response` if a session id is ever reused.
    pub fn set_session_scope(&self, session_id: impl Into<String>) -> Result<(), String> {
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        self.ensure_storage_confirmed()?;
        self.ensure_no_in_flight_invocations()?;
        let session_id = session_id.into();
        let scope = Some(session_id);
        let changing_scope = self.session_scope()? != scope;
        self.ensure_state_ownership()?;
        self.restore_terminal_reply_inventory()?;
        if changing_scope
            && !self
                .terminal_replies
                .lock()
                .map_err(|_| "Extension reply lock poisoned".to_string())?
                .is_empty()
        {
            return Err("Finished extension replies are unacknowledged; commit their original tool results before switching scope".into());
        }
        let target_owner = if changing_scope {
            self.acquire_state_owner(&scope)?
        } else {
            self.ensure_state_ownership()?;
            None
        };
        // Prepare every state before changing identity or evicting live queues.
        let extension_names = self.extension_names()?;
        let mut states = HashMap::new();
        let mut restored_events = HashMap::new();
        let mut unsettled = HashMap::new();
        let mut terminal_replies = HashMap::new();
        let mut last_id = 0;
        for name in extension_names {
            if let Some(checkpoint) = self.load_checkpoint_in_scope(&name, &scope)? {
                if let Some(reply) = checkpoint.terminal_reply {
                    terminal_replies.insert(name.clone(), Arc::new(reply));
                }
                last_id = last_id.max(checkpoint.last_broker_id);
                unsettled.insert(
                    name.clone(),
                    checkpoint.unsettled.into_iter().map(Arc::new).collect(),
                );
                states.insert(name.clone(), checkpoint.state);
                restored_events.insert(
                    name,
                    checkpoint.broker_events.into_iter().map(Arc::new).collect(),
                );
            }
        }
        *self
            .session_id
            .lock()
            .map_err(|_| "Extension session lock poisoned".to_string())? = scope.clone();
        self.last_broker_id.fetch_max(last_id, Ordering::SeqCst);
        *self
            .unsettled_broker
            .lock()
            .map_err(|_| "Broker intent lock poisoned".to_string())? = unsettled;
        *self
            .terminal_replies
            .lock()
            .map_err(|_| "Extension reply lock poisoned".to_string())? = terminal_replies;
        // Queued work is session-owned too: switching scope selects a separate
        // queue so one conversation cannot receive another's broker outcomes.
        // Evict every other scope's queues rather than leaking them.
        {
            let mut pending = self
                .pending_events
                .lock()
                .map_err(|_| "Extension event lock poisoned".to_string())?;
            pending.retain(|existing, _| existing == &scope);
            Self::replace_broker_events(pending.entry(scope.clone()).or_default(), restored_events);
        }
        {
            let mut pending = self
                .pending_broker_requests
                .lock()
                .map_err(|_| "Extension broker request lock poisoned".to_string())?;
            pending.retain(|existing, _| existing == &scope);
            pending.entry(scope).or_default();
        }

        *self
            .states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())? = states;
        self.host_state
            .lock()
            .map_err(|_| "Host state lock poisoned".to_string())?
            .clear();
        if changing_scope {
            *self
                .state_owner
                .lock()
                .map_err(|_| "Extension state owner lock poisoned".to_string())? = target_owner;
        }
        Ok(())
    }

    /// Called under `state_commit` before accessing a durable scope's caches.
    fn ensure_state_ownership(&self) -> Result<(), String> {
        if self.state_dir.is_none() {
            return Ok(());
        }
        let scope = self.session_scope()?;
        let mut owner = self
            .state_owner
            .lock()
            .map_err(|_| "Extension state owner lock poisoned".to_string())?;
        if let Some(owner) = owner.as_ref() {
            return if owner.scope == scope {
                Ok(())
            } else {
                Err("Extension state ownership does not match the active scope; preserve checkpoints and reload the session".into())
            };
        }
        *owner = self.acquire_state_owner(&scope)?;
        Ok(())
    }

    fn ensure_storage_confirmed(&self) -> Result<(), String> {
        let pending = self
            .unconfirmed_write
            .lock()
            .map_err(|_| "Extension commit confirmation lock poisoned".to_string())?;
        match pending.as_ref() {
            Some(write) => Err(write.blocked_message()),
            None => Ok(()),
        }
    }

    fn record_state_write_error(
        &self,
        path: &Path,
        error: StateWriteError,
        state: impl FnOnce() -> UnconfirmedState,
    ) -> String {
        if !error.replaced {
            return error.to_string();
        }
        let write = UnconfirmedWrite {
            path: path.to_owned(),
            state: state(),
            error: error.to_string(),
        };
        // Fence the candidate for readers that never take the owner lease.
        if let Some(marker) = unconfirmed_marker_path(path) {
            if let Err(marker_error) = fs::write(&marker, &write.error) {
                tracing::warn!(
                    "Cannot mark unconfirmed replacement {} for concurrent readers: {marker_error}",
                    marker.display()
                );
            }
        }
        let message = write.blocked_message();
        match self.unconfirmed_write.lock() {
            Ok(mut pending) => *pending = Some(write),
            Err(_) => return format!("{message}; extension commit confirmation lock poisoned"),
        }
        message
    }

    /// Confirms a replaced checkpoint before allowing cached state to write again.
    /// Only requests never exposed by the failed invocation are marked not dispatched.
    pub fn recover_state_commit(&self) -> Result<(), String> {
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        let mut pending = self
            .unconfirmed_write
            .lock()
            .map_err(|_| "Extension commit confirmation lock poisoned".to_string())?;
        let Some(write) = pending.as_ref() else {
            return Ok(());
        };
        self.ensure_no_active_invocations()?;
        self.ensure_state_ownership()?;
        let value = read_json_state(&write.path)?.ok_or_else(|| format!("Unconfirmed replacement {} is missing; preserve state and inspect storage before recovery", write.path.display()))?;
        let matches = match &write.state {
            UnconfirmedState::Host {
                value: expected, ..
            } => &value == expected,
            UnconfirmedState::Extension {
                state,
                events,
                intents,
                last_broker_id,
                terminal_reply,
                ..
            } => {
                let checkpoint = ExtensionCheckpoint::decode(value).map_err(|error| {
                    format!(
                        "{}: {error}; preserve the replacement and repair storage before recovery",
                        write.path.display()
                    )
                })?;
                checkpoint.state == *state
                    && checkpoint.terminal_reply.as_ref() == terminal_reply.as_deref()
                    && checkpoint.last_broker_id == *last_broker_id
                    && checkpoint
                        .unsettled
                        .iter()
                        .eq(intents.iter().map(Arc::as_ref))
                    && checkpoint.broker_events.iter().eq(events
                        .iter()
                        .filter(|event| event.topic == "broker_response")
                        .map(Arc::as_ref))
            }
        };
        if !matches {
            return Err(format!("Unconfirmed replacement {} changed; preserve it and inspect storage before recovery. Do not overwrite it or replay broker operations", write.path.display()));
        }
        #[cfg(unix)]
        fs::File::open(&write.path)
            .and_then(|file| file.sync_all())
            .and_then(|_| {
                sync_state_parent(
                    write.path.parent().ok_or_else(|| {
                        std::io::Error::other("Unconfirmed state path has no parent")
                    })?,
                )
            })
            .map_err(|error| {
                format!(
                    "{}; confirmation still failed: {error}",
                    write.blocked_message()
                )
            })?;
        if let Some(marker) = unconfirmed_marker_path(&write.path) {
            match fs::remove_file(&marker) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "Confirmed {} but could not clear its unconfirmed marker {}: {error}; preserve the marker and repair storage",
                        write.path.display(),
                        marker.display()
                    ))
                }
            }
        }

        // Acquire caches only after I/O; leave the fence intact if a lock fails.
        let mut states = self
            .states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())?;
        let mut host_state = self
            .host_state
            .lock()
            .map_err(|_| "Host state lock poisoned".to_string())?;
        let mut events = self
            .pending_events
            .lock()
            .map_err(|_| "Extension event lock poisoned".to_string())?;
        let mut unsettled = self
            .unsettled_broker
            .lock()
            .map_err(|_| "Broker intent lock poisoned".to_string())?;
        let scope = self.session_scope()?;
        let write = pending
            .take()
            .expect("unconfirmed write held under confirmation lock");
        let mut undispatched = Vec::new();
        match write.state {
            UnconfirmedState::Host { key, value } => {
                host_state.insert(key, value);
            }
            UnconfirmedState::Extension {
                name,
                state,
                events: saved,
                acknowledged,
                intents,
                last_broker_id,
                unpublished,
                terminal_reply,
            } => {
                // Identity preserves later arrivals even when their payloads match
                // events acknowledged or saved by the interrupted commit.
                let replaced = acknowledged
                    .iter()
                    .chain(
                        saved
                            .iter()
                            .filter(|event| event.topic == "broker_response"),
                    )
                    .map(Arc::as_ptr)
                    .collect::<HashSet<_>>();
                let queue = events
                    .entry(scope)
                    .or_default()
                    .entry(name.clone())
                    .or_default();
                queue.retain(|event| !replaced.contains(&Arc::as_ptr(event)));
                let mut restored = saved
                    .into_iter()
                    .filter(|event| event.topic == "broker_response")
                    .collect::<Vec<_>>();
                restored.append(queue);
                *queue = restored;
                if unpublished {
                    undispatched = intents
                        .iter()
                        .map(|intent| HostBrokerRequest {
                            invoking_extension: name.clone(),
                            request: intent.request.clone(),
                            receipt: Some(intent.receipt.clone()),
                        })
                        .collect();
                }
                self.last_broker_id
                    .fetch_max(last_broker_id, Ordering::SeqCst);
                unsettled.insert(name.clone(), intents);
                let mut replies = self
                    .terminal_replies
                    .lock()
                    .map_err(|_| "Extension reply lock poisoned".to_string())?;
                if let Some(reply) = terminal_reply {
                    replies.insert(name.clone(), reply);
                } else {
                    replies.remove(&name);
                }
                states.insert(name, state);
            }
        }
        drop(unsettled);
        drop(events);
        drop(host_state);
        drop(states);
        drop(pending);
        drop(_commit);
        self.enqueue_broker_results(undispatched.into_iter().map(|request| request.not_dispatched(BrokerError {
            code: "not_dispatched".into(),
            message: "The checkpoint directory sync failed before this request was exposed to the broker; no external operation began".into(),
        })).collect())
    }

    fn acquire_state_owner(&self, scope: &Option<String>) -> Result<Option<StateOwner>, String> {
        let Some(directory) = self.state_dir.as_ref() else {
            return Ok(None);
        };
        let directory = match scope {
            Some(session) => directory
                .join("sessions")
                .join(encode_state_component(session)),
            None => directory.clone(),
        };
        let project_root = self
            .project_root
            .as_ref()
            .ok_or("Extension storage has no project root")?;
        if !directory.starts_with(project_root) {
            return Err("Extension storage is outside its project root".into());
        }
        let root_path = if project_root.as_os_str().is_empty() {
            Path::new(".")
        } else {
            project_root
        };
        let root_metadata = fs::metadata(root_path).map_err(|error| {
            format!(
                "Extension project root {} is unavailable: {error}; restore the work directory before opening state",
                root_path.display()
            )
        })?;
        if !root_metadata.is_dir() {
            return Err(format!(
                "Extension project root {} must be an existing work directory",
                root_path.display()
            ));
        }
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let path = directory.join(".owner.lock");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Invalid extension owner lock {}; preserve the file",
                    path.display()
                ));
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error.to_string())
            }
            _ => {}
        }
        let lease = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        lease.try_lock().map_err(|error| format!("Extension state {} is owned by another manager/process, or storage is unavailable: {error}; use the owning manager or wait for it to close. Do not replace checkpoints or delete the owner lock", directory.display()))?;
        // A prior owner's `.unconfirmed` markers die with its lease: this
        // manager is the only possible writer now, so the files on disk are
        // authoritative. Sweep before returning ownership to readers.
        if let Ok(entries) = fs::read_dir(&directory) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name
                    .to_str()
                    .is_some_and(|name| name.ends_with(".unconfirmed"))
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        // Sync all containing directories, including existing ones: a prior
        // interrupted creation may have left visible but unsynced entries.
        #[cfg(unix)]
        sync_state_directories(&directory, project_root, |path| {
            fs::File::open(if path.as_os_str().is_empty() {
                Path::new(".")
            } else {
                path
            })?
            .sync_all()
        })?;
        Ok(Some(StateOwner {
            scope: scope.clone(),
            directory,
            receipts_restored: false,
            replies_restored: false,
            _lease: lease,
        }))
    }

    /// Called under `state_commit`, once before this owner's first allocation.
    /// Policy reads and non-broker invocations need no receipt inventory scan.
    fn restore_receipt_counter(&self) -> Result<(), String> {
        let directory = {
            let owner = self
                .state_owner
                .lock()
                .map_err(|_| "Extension state owner lock poisoned".to_string())?;
            let Some(owner) = owner.as_ref() else {
                return Ok(());
            };
            if owner.receipts_restored {
                return Ok(());
            }
            owner.directory.clone()
        };
        let high_water = checkpoint::receipt_high_water(&directory)
            .map_err(|error| format!("Cannot allocate broker receipts: {error}; preserve the checkpoint and do not retry unchanged"))?;
        self.last_broker_id.fetch_max(high_water, Ordering::SeqCst);
        if let Some(owner) = self
            .state_owner
            .lock()
            .map_err(|_| "Extension state owner lock poisoned".to_string())?
            .as_mut()
        {
            owner.receipts_restored = true;
        }
        Ok(())
    }

    /// Read the preserved inventory once per scope, including disabled modules.
    /// Called under state_commit; only terminal metadata is retained from scans.
    fn restore_terminal_reply_inventory(&self) -> Result<(), String> {
        let directory = {
            let owner = self
                .state_owner
                .lock()
                .map_err(|_| "Extension owner lock poisoned".to_string())?;
            let Some(owner) = owner.as_ref() else {
                return Ok(());
            };
            if owner.replies_restored {
                return Ok(());
            }
            owner.directory.clone()
        };
        let mut restored = HashMap::new();
        let mut high_water = 0;
        let scope = self.session_scope()?;
        checkpoint::visit_checkpoints(&directory, |path, checkpoint| {
            high_water = high_water.max(checkpoint.last_broker_id);
            if let Some(reply) = checkpoint.terminal_reply {
                if self.state_path(&reply.extension_name).as_deref() != Some(path)
                    || scope.as_deref() != Some(reply.identity.session_id.as_str())
                {
                    return Err(format!(
                        "Saved reply ownership disagrees with {}; preserve the checkpoint",
                        path.display()
                    ));
                }
                restored.insert(reply.extension_name.clone(), Arc::new(reply));
            }
            Ok(())
        })?;
        self.terminal_replies
            .lock()
            .map_err(|_| "Extension reply lock poisoned".to_string())?
            .extend(restored);
        // The same scan establishes the receipt floor; avoid rereading every
        // preserved checkpoint before this owner's first broker allocation.
        self.last_broker_id.fetch_max(high_water, Ordering::SeqCst);
        if let Some(owner) = self
            .state_owner
            .lock()
            .map_err(|_| "Extension owner lock poisoned".to_string())?
            .as_mut()
        {
            owner.replies_restored = true;
            owner.receipts_restored = true;
        }
        Ok(())
    }

    pub fn pending_tool_reply_identities(&self) -> Result<Vec<ToolExecutionIdentity>, String> {
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        self.ensure_state_ownership()?;
        self.ensure_storage_confirmed()?;
        self.restore_terminal_reply_inventory()?;
        Ok(self
            .terminal_replies
            .lock()
            .map_err(|_| "Extension reply lock poisoned".to_string())?
            .values()
            .map(|reply| reply.identity.clone())
            .collect())
    }

    /// Pure reply delivery: never enters the VM or dispatches a broker request.
    pub fn recover_tool_reply(
        &self,
        identity: &ToolExecutionIdentity,
        tool_name: &str,
        args: &str,
    ) -> Result<Option<Result<String, String>>, String> {
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        self.ensure_state_ownership()?;
        self.ensure_storage_confirmed()?;
        self.restore_terminal_reply_inventory()?;
        let reply = self
            .terminal_replies
            .lock()
            .map_err(|_| "Extension reply lock poisoned".to_string())?
            .values()
            .find(|reply| &reply.identity == identity)
            .cloned();
        let Some(reply) = reply else { return Ok(None) };
        let arguments =
            serde_json::from_str(args).unwrap_or_else(|_| serde_json::json!({"raw":args}));
        if reply.tool_name != tool_name || reply.arguments != arguments {
            return Err("Saved terminal reply belongs to different tool arguments; preserve it and reconcile the original declaration, do not execute the tool again".into());
        }
        self.hydrate_extension_checkpoint(&reply.extension_name)?;
        self.settle_reply_outcomes(&reply.extension_name)?;
        let scope = self.session_scope()?;
        let events = self
            .pending_events
            .lock()
            .map_err(|_| "Extension event lock poisoned".to_string())?
            .get(&scope)
            .and_then(|events| events.get(&reply.extension_name))
            .cloned()
            .unwrap_or_default();
        Ok(Some(reply.result(&events)?))
    }

    /// Recommit retained, known outcomes, never redispatch them. Called under
    /// state_commit; unknown original or hook effects keep reply delivery fenced.
    fn settle_reply_outcomes(&self, extension_name: &str) -> Result<(), String> {
        let state = self
            .states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())?
            .get(extension_name)
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        self.commit_retained_outcomes(extension_name, &state, false)?;
        if self
            .unsettled_broker
            .lock()
            .map_err(|_| "Broker intent lock poisoned".to_string())?
            .get(extension_name)
            .is_some_and(|intents| !intents.is_empty())
        {
            return Err("Tool or after-tool broker outcomes remain unsettled; preserve the reply and reconcile the original outcomes without replaying the tool or hook".into());
        }
        Ok(())
    }

    /// Saves the prepared host reply before committing the canonical result.
    pub fn prepare_tool_reply(
        &self,
        identity: &ToolExecutionIdentity,
        result: &threadlane_protocol::AgentToolResult,
    ) -> Result<(), String> {
        if !identity.matches_call(&result.tool_call_id, &result.name) {
            return Err("Canonical tool reply disagrees with its committed identity".into());
        }
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        self.ensure_state_ownership()?;
        self.ensure_storage_confirmed()?;
        self.restore_terminal_reply_inventory()?;
        let reply = self
            .terminal_replies
            .lock()
            .map_err(|_| "Extension reply lock poisoned".to_string())?
            .values()
            .find(|reply| &reply.identity == identity)
            .cloned();
        let Some(reply) = reply else { return Ok(()) };
        self.hydrate_extension_checkpoint(&reply.extension_name)?;
        self.settle_reply_outcomes(&reply.extension_name)?;
        if let Some(prepared) = &reply.canonical_result {
            return if prepared == result {
                Ok(())
            } else {
                Err("Saved canonical reply is already prepared; preserve it instead of replacing the original result".into())
            };
        }
        let scope = self.session_scope()?;
        let events = self
            .pending_events
            .lock()
            .map_err(|_| "Extension event lock poisoned".to_string())?
            .get(&scope)
            .and_then(|events| events.get(&reply.extension_name))
            .cloned()
            .unwrap_or_default();
        let _ = reply.result(&events)?;
        let mut prepared = (*reply).clone();
        prepared.canonical_result = Some(result.clone());
        let prepared = Arc::new(prepared);
        let state = self
            .states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())?
            .get(&reply.extension_name)
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        self.persist_state_events_reply(
            &reply.extension_name,
            &state,
            true,
            None,
            Some(&prepared),
        )?;
        self.terminal_replies
            .lock()
            .map_err(|_| "Extension reply lock poisoned".to_string())?
            .insert(reply.extension_name.clone(), prepared);
        Ok(())
    }

    /// Reads a prepared reply, restoring preserved checkpoints without invoking WASM.
    pub fn recovered_canonical_reply(
        &self,
        identity: &ToolExecutionIdentity,
    ) -> Result<Option<threadlane_protocol::AgentToolResult>, String> {
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        self.ensure_state_ownership()?;
        self.ensure_storage_confirmed()?;
        self.restore_terminal_reply_inventory()?;
        Ok(self
            .terminal_replies
            .lock()
            .map_err(|_| "Extension reply lock poisoned".to_string())?
            .values()
            .find(|reply| &reply.identity == identity)
            .and_then(|reply| reply.canonical_result.clone()))
    }

    pub fn acknowledge_tool_reply(&self, identity: &ToolExecutionIdentity) -> Result<(), String> {
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        self.ensure_state_ownership()?;
        self.ensure_storage_confirmed()?;
        self.restore_terminal_reply_inventory()?;
        let reply = self
            .terminal_replies
            .lock()
            .map_err(|_| "Extension reply lock poisoned".to_string())?
            .values()
            .find(|reply| &reply.identity == identity)
            .cloned();
        let Some(reply) = reply else { return Ok(()) };
        if self
            .active_operations
            .lock()
            .map_err(|_| "Extension operation lock poisoned".to_string())?
            .contains(&reply.extension_name)
        {
            return Err(
                "Cannot acknowledge a reply while its extension call is still active".into(),
            );
        }
        self.hydrate_extension_checkpoint(&reply.extension_name)?;
        self.settle_reply_outcomes(&reply.extension_name)?;
        let scope = self.session_scope()?;
        let queued = self
            .pending_events
            .lock()
            .map_err(|_| "Extension event lock poisoned".to_string())?
            .get(&scope)
            .and_then(|events| events.get(&reply.extension_name))
            .cloned()
            .unwrap_or_default();
        let _ = reply.result(&queued)?;
        let state = self
            .states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())?
            .get(&reply.extension_name)
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        self.persist_state_events_reply(&reply.extension_name, &state, true, None, None)?;
        self.terminal_replies
            .lock()
            .map_err(|_| "Extension reply lock poisoned".to_string())?
            .remove(&reply.extension_name);
        Ok(())
    }

    /// A successor can write before module reload; preserve the saved ledger
    /// and undelivered outcomes rather than starting from empty cache entries.
    fn hydrate_extension_checkpoint(&self, name: &str) -> Result<(), String> {
        if self
            .states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())?
            .contains_key(name)
            || self
                .unsettled_broker
                .lock()
                .map_err(|_| "Broker intent lock poisoned".to_string())?
                .contains_key(name)
        {
            return Ok(());
        }
        let scope = self.session_scope()?;
        let Some(checkpoint) = self.load_checkpoint_in_scope(name, &scope)? else {
            self.unsettled_broker
                .lock()
                .map_err(|_| "Broker intent lock poisoned".to_string())?
                .insert(name.into(), Vec::new());
            return Ok(());
        };
        if let Some(reply) = checkpoint.terminal_reply {
            self.terminal_replies
                .lock()
                .map_err(|_| "Extension reply lock poisoned".to_string())?
                .insert(name.into(), Arc::new(reply));
        }
        self.last_broker_id
            .fetch_max(checkpoint.last_broker_id, Ordering::SeqCst);
        self.unsettled_broker
            .lock()
            .map_err(|_| "Broker intent lock poisoned".to_string())?
            .insert(
                name.into(),
                checkpoint.unsettled.into_iter().map(Arc::new).collect(),
            );
        let mut pending = self
            .pending_events
            .lock()
            .map_err(|_| "Extension event lock poisoned".to_string())?;
        Self::replace_broker_events(
            pending.entry(scope).or_default(),
            HashMap::from([(
                name.to_owned(),
                checkpoint.broker_events.into_iter().map(Arc::new).collect(),
            )]),
        );
        drop(pending);
        self.states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())?
            .insert(name.into(), checkpoint.state);
        Ok(())
    }

    fn ensure_no_in_flight_invocations(&self) -> Result<(), String> {
        self.ensure_no_active_invocations()?;
        if self
            .unsettled_broker
            .lock()
            .map_err(|_| "Broker intent lock poisoned".to_string())?
            .values()
            .any(|intents| !intents.is_empty())
        {
            return Err("Broker outcomes are unsettled; reconcile the original operations before reloading or switching scope. Do not replay them".into());
        }
        Ok(())
    }

    fn ensure_no_active_invocations(&self) -> Result<(), String> {
        if !self
            .active_operations
            .lock()
            .map_err(|_| "Extension operation lock poisoned".to_string())?
            .is_empty()
        {
            return Err("An extension call is active; wait for its broker work to finish before reloading or switching scope".into());
        }
        if !self
            .in_flight_events
            .lock()
            .map_err(|_| "Extension delivery lock poisoned".to_string())?
            .is_empty()
        {
            return Err("An extension invocation is in flight; wait for it to finish before reloading or switching scope".into());
        }
        Ok(())
    }

    fn replace_broker_events(
        pending: &mut HashMap<String, Vec<Arc<WasiExtensionEvent>>>,
        checkpoints: HashMap<String, Vec<Arc<WasiExtensionEvent>>>,
    ) {
        for (name, mut restored) in checkpoints {
            let queue = pending.entry(name).or_default();
            queue.retain(|event| event.topic != "broker_response");
            restored.append(queue);
            *queue = restored;
        }
    }

    /// Returns the current persisted/in-memory state for an extension.
    pub fn extension_state(&self, extension_name: &str) -> Option<Value> {
        self.ensure_storage_confirmed().ok()?;
        self.states.lock().ok()?.get(extension_name).cloned()
    }

    /// Updates only the state owned by the invoking extension.
    pub fn set_extension_state(&self, extension_name: &str, state: Value) -> Result<(), String> {
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        self.ensure_state_ownership()?;
        self.hydrate_extension_checkpoint(extension_name)?;
        if self
            .terminal_replies
            .lock()
            .map_err(|_| "Extension reply lock poisoned".to_string())?
            .contains_key(extension_name)
        {
            return Err("This extension has a saved terminal reply awaiting its canonical tool result; recover and acknowledge that reply, do not execute the extension again".into());
        }
        self.persist_state(extension_name, &state)?;
        self.states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())?
            .insert(extension_name.to_string(), state);
        Ok(())
    }

    /// Returns host-owned state in the active session scope without relying on
    /// any extension identity or schema. Only absent files return `Ok(None)`;
    /// unreadable or malformed files return an error instead of a default.
    pub fn host_state(&self, key: &str) -> Result<Option<Value>, String> {
        self.ensure_storage_confirmed()?;
        if let Some(value) = self
            .host_state
            .lock()
            .map_err(|_| "Host state lock poisoned".to_string())?
            .get(key)
            .cloned()
        {
            return Ok(Some(value));
        }
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        self.ensure_state_ownership()?;
        // A writer may have committed while this reader waited for the gate.
        if let Some(value) = self
            .host_state
            .lock()
            .map_err(|_| "Host state lock poisoned".to_string())?
            .get(key)
            .cloned()
        {
            return Ok(Some(value));
        }
        let Some(value) = self.load_host_state(key)? else {
            return Ok(None);
        };
        self.host_state
            .lock()
            .map_err(|_| "Host state lock poisoned".to_string())?
            .insert(key.to_string(), value.clone());
        Ok(Some(value))
    }

    /// Reads persisted host-owned state for the active session scope without
    /// acquiring the state-owner lease. Callers that run before this manager
    /// has proven ownership — for example tool-policy restore during runtime
    /// construction — must observe the persisted value rather than fail on
    /// another live owner's lock; ownership serializes writes, not reads.
    ///
    /// Two fences still apply so the read never upgrades what the owner
    /// fenced: unavailable project storage errors instead of reporting an
    /// absent value, and a `.unconfirmed` marker left by a replaced-but-
    /// unsynced write errors instead of treating the candidate as committed.
    pub fn peek_host_state(&self, key: &str) -> Result<Option<Value>, String> {
        let Some(directory) = self.state_dir.as_ref() else {
            return Ok(None);
        };
        let directory = match self.session_scope()? {
            Some(session) => directory
                .join("sessions")
                .join(encode_state_component(&session)),
            None => directory.clone(),
        };
        let project_root = self
            .project_root
            .as_ref()
            .ok_or("Extension storage has no project root")?;
        if !directory.starts_with(project_root) {
            return Err("Extension storage is outside its project root".into());
        }
        let root_path = if project_root.as_os_str().is_empty() {
            Path::new(".")
        } else {
            project_root
        };
        let root_metadata = fs::metadata(root_path).map_err(|error| {
            format!(
                "Extension project root {} is unavailable: {error}; restore the work directory before opening state",
                root_path.display()
            )
        })?;
        if !root_metadata.is_dir() {
            return Err(format!(
                "Extension project root {} must be an existing work directory",
                root_path.display()
            ));
        }
        let path = directory.join(host_state_file_name(key));
        if let Some(marker) = unconfirmed_marker_path(&path) {
            match fs::symlink_metadata(&marker) {
                Ok(_) => {
                    return Err(format!(
                        "Extension state {} has an unconfirmed replacement; preserve {} and have the owning host call recover_state_commit",
                        path.display(),
                        marker.display()
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        read_json_state(&path)
    }

    /// Persists host-owned state in the active session scope.
    pub fn set_host_state(&self, key: &str, value: Value) -> Result<(), String> {
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        self.ensure_state_ownership()?;
        self.persist_host_state(key, &value)?;
        self.host_state
            .lock()
            .map_err(|_| "Host state lock poisoned".to_string())?
            .insert(key.to_string(), value);
        Ok(())
    }

    /// Subscribe an extension to a topic. Delivery is queued until its next invocation.
    pub fn subscribe_event(&self, extension_name: &str, topic: String) -> Result<(), String> {
        if extension_name.is_empty() || topic.trim().is_empty() {
            return Err("Event subscription requires extension identity and topic".into());
        }
        self.subscriptions
            .lock()
            .map_err(|_| "Extension subscription lock poisoned".to_string())?
            .entry(extension_name.to_string())
            .or_default()
            .insert(topic);
        Ok(())
    }

    pub fn publish_event(&self, topic: String, payload: Value) -> Result<(), String> {
        if topic == "broker_response" {
            return Err("broker_response is reserved for host broker outcomes".into());
        }
        let scope = self.session_scope()?;
        let subscribers = self
            .subscriptions
            .lock()
            .map_err(|_| "Extension subscription lock poisoned".to_string())?;
        let event = Arc::new(WasiExtensionEvent {
            topic: topic.clone(),
            payload,
        });
        let mut pending = self
            .pending_events
            .lock()
            .map_err(|_| "Extension event lock poisoned".to_string())?;
        let pending = pending.entry(scope).or_default();
        for (extension, topics) in subscribers.iter() {
            if topics.contains(&topic) {
                let queue = pending.entry(extension.clone()).or_default();
                // Notifications must never evict an already-consumed protocol reply.
                if queue.len() >= MAX_PENDING_EVENTS_PER_EXTENSION
                    && queue
                        .iter()
                        .filter(|event| event.topic != "broker_response")
                        .count()
                        >= MAX_PENDING_EVENTS_PER_EXTENSION
                {
                    if let Some(oldest) = queue
                        .iter()
                        .position(|event| event.topic != "broker_response")
                    {
                        queue.remove(oldest);
                    }
                }
                queue.push(event.clone());
            }
        }
        Ok(())
    }

    /// Takes one delivery batch; `invoke` restores it if execution or commit fails.
    fn drain_events_for(
        &self,
        extension_name: &str,
        scope: &Option<String>,
    ) -> Result<Vec<Arc<WasiExtensionEvent>>, String> {
        Ok(self
            .pending_events
            .lock()
            .map_err(|_| "Extension event lock poisoned".to_string())?
            .get_mut(scope)
            .and_then(|pending| pending.remove(extension_name))
            .unwrap_or_default())
    }

    fn restore_events_for(
        &self,
        extension_name: &str,
        scope: &Option<String>,
        mut events: Vec<Arc<WasiExtensionEvent>>,
    ) -> Result<(), String> {
        if events.is_empty() {
            return Ok(());
        }
        let mut pending = self
            .pending_events
            .lock()
            .map_err(|_| "Extension event lock poisoned".to_string())?;
        // Scope switches retire their queues; a late failure must not resurrect one.
        let Some(pending) = pending.get_mut(scope) else {
            return Ok(());
        };
        let queue = pending.entry(extension_name.to_string()).or_default();
        events.append(queue);
        // Retried notifications keep priority over arrivals during the failed call.
        // Preserve all broker outcomes and retain the existing notification bound.
        let mut notifications = 0;
        events.retain(|event| {
            if event.topic == "broker_response" {
                true
            } else {
                notifications += 1;
                notifications <= MAX_PENDING_EVENTS_PER_EXTENSION
            }
        });
        *queue = events;
        Ok(())
    }

    /// Location of the session-owned checkpoint. Its versioned envelope is
    /// internal; use `extension_state` after reload to render the state value.
    pub fn session_state_path(
        project_dir: &Path,
        session_id: &str,
        extension_name: &str,
    ) -> PathBuf {
        project_dir
            .join(".threadlane/state/extensions/sessions")
            .join(encode_state_component(session_id))
            .join(extension_state_file_name(extension_name))
    }

    pub fn discover_and_load(&self, project_root: &Path) -> usize {
        self.reload_from_roots(None, Some(project_root))
            .unwrap_or_default()
    }

    fn state_path(&self, extension_name: &str) -> Option<PathBuf> {
        let directory = self.state_dir.as_ref()?;
        let session_id = self.session_id.lock().ok()?.clone();
        Some(match session_id {
            Some(session_id) => {
                Self::session_state_path_from_dir(directory, &session_id, extension_name)
            }
            None => directory.join(extension_state_file_name(extension_name)),
        })
    }

    fn session_state_path_from_dir(
        state_dir: &Path,
        session_id: &str,
        extension_name: &str,
    ) -> PathBuf {
        state_dir
            .join("sessions")
            .join(encode_state_component(session_id))
            .join(extension_state_file_name(extension_name))
    }

    #[cfg(test)]
    fn load_state(&self, extension_name: &str) -> Result<Option<Value>, String> {
        Ok(self
            .load_checkpoint_in_scope(extension_name, &self.session_scope()?)?
            .map(|checkpoint| checkpoint.state))
    }

    fn load_checkpoint_in_scope(
        &self,
        extension_name: &str,
        scope: &Option<String>,
    ) -> Result<Option<ExtensionCheckpoint>, String> {
        let Some(directory) = self.state_dir.as_ref() else {
            return Ok(None);
        };
        let path = match scope {
            Some(session_id) => {
                Self::session_state_path_from_dir(directory, session_id, extension_name)
            }
            None => directory.join(extension_state_file_name(extension_name)),
        };
        let checkpoint = read_json_state(&path)?
            .map(ExtensionCheckpoint::decode)
            .transpose()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if checkpoint.as_ref().is_some_and(|checkpoint| {
            checkpoint
                .unsettled
                .iter()
                .any(|intent| &intent.receipt.scope != scope)
        }) {
            return Err(format!(
                "{}: broker intent belongs to another session; preserve the file",
                path.display()
            ));
        }
        Ok(checkpoint)
    }

    fn persist_state(&self, extension_name: &str, state: &Value) -> Result<(), String> {
        self.persist_state_and_events(extension_name, state, true, None)
    }

    fn persist_state_and_events(
        &self,
        extension_name: &str,
        state: &Value,
        include_in_flight: bool,
        unsettled: Option<&[Arc<BrokerIntent>]>,
    ) -> Result<(), String> {
        let reply = self
            .terminal_replies
            .lock()
            .map_err(|_| "Extension reply lock poisoned".to_string())?
            .get(extension_name)
            .cloned();
        self.persist_state_events_reply(
            extension_name,
            state,
            include_in_flight,
            unsettled,
            reply.as_ref(),
        )
    }

    fn persist_state_events_reply(
        &self,
        extension_name: &str,
        state: &Value,
        include_in_flight: bool,
        unsettled: Option<&[Arc<BrokerIntent>]>,
        terminal_reply: Option<&Arc<SavedToolReply>>,
    ) -> Result<(), String> {
        self.ensure_storage_confirmed()?;
        let Some(path) = self.state_path(extension_name) else {
            return if self.state_dir.is_none() {
                Ok(())
            } else {
                Err("Extension session lock poisoned".into())
            };
        };
        let scope = self.session_scope()?;
        let mut events = if include_in_flight {
            self.in_flight_events
                .lock()
                .map_err(|_| "Extension delivery lock poisoned".to_string())?
                .get(extension_name)
                .cloned()
                .unwrap_or_default()
        } else {
            vec![]
        };
        if let Some(queued) = self
            .pending_events
            .lock()
            .map_err(|_| "Extension event lock poisoned".to_string())?
            .get(&scope)
            .and_then(|pending| pending.get(extension_name))
        {
            events.extend(queued.iter().cloned());
        }
        let existing;
        let unsettled = if let Some(unsettled) = unsettled {
            unsettled
        } else {
            existing = self
                .unsettled_broker
                .lock()
                .map_err(|_| "Broker intent lock poisoned".to_string())?
                .get(extension_name)
                .cloned()
                .unwrap_or_default();
            &existing
        };
        let acknowledged = if include_in_flight {
            vec![]
        } else {
            self.in_flight_events
                .lock()
                .map_err(|_| "Extension delivery lock poisoned".to_string())?
                .get(extension_name)
                .cloned()
                .unwrap_or_default()
        };
        let ephemeral_receipts = self
            .ephemeral_broker_receipts
            .lock()
            .map_err(|_| "Ephemeral broker receipt lock poisoned".to_string())?
            .values()
            .flat_map(|receipts| receipts.iter().copied())
            .collect::<HashSet<_>>();
        events.retain(|event| {
            event
                .payload
                .get("receipt_id")
                .and_then(Value::as_u64)
                .is_none_or(|id| !ephemeral_receipts.contains(&id))
        });
        let durable_unsettled = unsettled
            .iter()
            .filter(|intent| !ephemeral_receipts.contains(&intent.receipt.id))
            .cloned()
            .collect::<Vec<_>>();
        let ephemeral_state_keys = self
            .ephemeral_state_keys
            .lock()
            .map_err(|_| "Ephemeral extension state lock poisoned".to_string())?
            .get(extension_name)
            .cloned()
            .unwrap_or_default();
        let mut persistent_state = state.clone();
        if let Some(object) = persistent_state.as_object_mut() {
            for key in ephemeral_state_keys {
                object.remove(&key);
            }
        }
        let last_broker_id = self.last_broker_id.load(Ordering::SeqCst);
        persist_checkpoint(
            &path,
            &persistent_state,
            &events,
            last_broker_id,
            &durable_unsettled,
            terminal_reply.map(Arc::as_ref),
        )
        .map_err(|error| {
            self.record_state_write_error(&path, error, || UnconfirmedState::Extension {
                name: extension_name.into(),
                state: persistent_state.clone(),
                events,
                acknowledged,
                last_broker_id,
                intents: durable_unsettled,
                unpublished: !include_in_flight,
                terminal_reply: terminal_reply.cloned(),
            })
        })
    }

    fn host_state_path(&self, key: &str) -> Option<PathBuf> {
        let directory = self.state_dir.as_ref()?;
        let session_id = self.session_id.lock().ok()?.clone();
        Some(match session_id {
            Some(session_id) => directory
                .join("sessions")
                .join(encode_state_component(&session_id))
                .join(host_state_file_name(key)),
            None => directory.join(host_state_file_name(key)),
        })
    }

    fn load_host_state(&self, key: &str) -> Result<Option<Value>, String> {
        if self.state_dir.is_none() {
            return Ok(None);
        }
        let path = self
            .host_state_path(key)
            .ok_or("Extension session lock poisoned")?;
        read_json_state(&path)
    }

    fn persist_host_state(&self, key: &str, value: &Value) -> Result<(), String> {
        self.ensure_storage_confirmed()?;
        let Some(path) = self.host_state_path(key) else {
            return if self.state_dir.is_none() {
                Ok(())
            } else {
                Err("Extension session lock poisoned".into())
            };
        };
        persist_json_state(&path, value).map_err(|error| {
            self.record_state_write_error(&path, error, || UnconfirmedState::Host {
                key: key.into(),
                value: value.clone(),
            })
        })
    }

    /// Single VM invocation. Hosts driving broker work must hold
    /// `begin_tool_operation` through dispatch, outcome commit, and continuations.
    pub fn execute_tool_with_broker_requests(
        &self,
        name: &str,
        args: &str,
    ) -> Option<Result<WasiExtensionInvocationResult, String>> {
        self.execute_response("tool", name, args)
    }

    pub fn begin_tool_operation(
        &self,
        name: &str,
    ) -> Option<Result<WasiExtensionOperation<'_>, String>> {
        self.begin_response_operation("tool", name)
    }

    pub fn begin_command_operation(
        &self,
        name: &str,
    ) -> Option<Result<WasiExtensionOperation<'_>, String>> {
        self.begin_response_operation("command", name)
    }

    pub fn begin_ephemeral_command_operation(
        &self,
        name: &str,
    ) -> Option<Result<WasiExtensionOperation<'_>, String>> {
        if name != "editor_lsp" {
            return Some(Err(
                "Only the fixed editor_lsp command can use ephemeral broker state".into(),
            ));
        }
        let extension = match self.extensions.read() {
            Ok(extensions) => extensions.get("lsp_ext").cloned(),
            Err(_) => return Some(Err("Extension registry lock poisoned".into())),
        }?;
        if !extension.trusted_global_editor_lsp {
            return Some(Err(
                "Automatic editor LSP requires `lsp_ext` installed and enabled in the daemon's global extension scope; project-only or shadowing copies are not eligible".into(),
            ));
        }
        if !extension
            .manifest
            .commands
            .iter()
            .any(|command| command.name == name)
        {
            return Some(Err(
                "The global `lsp_ext` extension must declare the fixed `editor_lsp` command"
                    .into(),
            ));
        }
        let mut operation = match self.claim_operation(extension, "command", name) {
            Ok(operation) => operation,
            Err(error) => return Some(Err(error)),
        };
        let mut ephemeral_keys = match self.ephemeral_state_keys.lock() {
            Ok(keys) => keys,
            Err(_) => {
                return Some(Err("Ephemeral extension state lock poisoned".into()));
            }
        };
        ephemeral_keys
            .entry("lsp_ext".into())
            .or_default()
            .insert("editor".into());
        operation.ephemeral = true;
        Some(Ok(operation))
    }

    pub async fn lock_lsp_extension(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lsp_operation_gate.lock().await
    }

    pub async fn lock_lsp_tool_owner(
        &self,
        name: &str,
    ) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        let is_lsp_owner = self
            .find_response_extension("tool", name)
            .is_some_and(|extension| extension.manifest.name == "lsp_ext");
        if is_lsp_owner {
            Some(self.lsp_operation_gate.lock().await)
        } else {
            None
        }
    }

    pub async fn lock_lsp_hook_dispatch(
        &self,
        hook_name: &str,
    ) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        let has_lsp_hook = self
            .find_hook_extensions(hook_name)
            .iter()
            .any(|extension| extension.manifest.name == "lsp_ext");
        if has_lsp_hook {
            Some(self.lsp_operation_gate.lock().await)
        } else {
            None
        }
    }

    /// Acquire each hook only when visited, preserving name order without
    /// reserving unrelated extensions during another hook's broker dispatch.
    pub fn begin_hook_operations(
        &self,
        name: &str,
    ) -> impl Iterator<Item = Result<WasiExtensionOperation<'_>, String>> {
        let name = name.to_owned();
        self.find_hook_extensions(&name)
            .into_iter()
            .map(move |extension| self.claim_operation(extension, "hook", &name))
    }

    fn begin_response_operation(
        &self,
        kind: &'static str,
        name: &str,
    ) -> Option<Result<WasiExtensionOperation<'_>, String>> {
        let extension = self.find_response_extension(kind, name)?;
        Some(self.claim_operation(extension, kind, name))
    }

    fn claim_operation(
        &self,
        extension: Arc<WasiExtension>,
        kind: &'static str,
        name: &str,
    ) -> Result<WasiExtensionOperation<'_>, String> {
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        self.ensure_state_ownership()?;
        self.ensure_storage_confirmed()?;
        if !self
            .extensions
            .read()
            .map_err(|_| "Extension registry lock poisoned".to_string())?
            .get(&extension.manifest.name)
            .is_some_and(|current| Arc::ptr_eq(current, &extension))
        {
            return Err(
                "Extension inventory changed while selecting the call; use the current inventory"
                    .into(),
            );
        }
        let mut active = self
            .active_operations
            .lock()
            .map_err(|_| "Extension operation lock poisoned".to_string())?;
        if active.contains(&extension.manifest.name)
            || self
                .in_flight_events
                .lock()
                .map_err(|_| "Extension delivery lock poisoned".to_string())?
                .contains_key(&extension.manifest.name)
        {
            return Err(format!("Extension `{}` already has an active call; wait for it to finish and do not restart its broker work", extension.manifest.name));
        }
        active.insert(extension.manifest.name.clone());
        Ok(WasiExtensionOperation {
            manager: self,
            extension,
            kind,
            name: name.to_owned(),
            ephemeral: false,
        })
    }

    /// Single VM invocation; use `begin_command_operation` across broker work.
    pub fn execute_command_with_effects(
        &self,
        name: &str,
        args: &str,
    ) -> Option<Result<WasiExtensionCommandResult, String>> {
        self.execute_response("command", name, args)
            .map(|result| result.and_then(WasiExtensionInvocationResult::into_command_result))
    }

    pub fn take_pending_broker_requests(&self) -> Vec<HostBrokerRequest> {
        let scope = match self.session_scope() {
            Ok(scope) => scope,
            Err(_) => return Vec::new(),
        };
        let requests = self
            .pending_broker_requests
            .lock()
            .map(|mut requests| requests.remove(&scope).unwrap_or_default())
            .unwrap_or_default();
        self.filter_granted_requests(requests)
    }

    /// Commits broker outcomes before acknowledging delivery to the producer.
    /// On storage failure, retain them in memory and report the failed commit;
    /// callers must not repeat the external operation to repair the checkpoint.
    pub fn enqueue_broker_results(
        &self,
        results: Vec<BrokerOperationResult>,
    ) -> Result<(), String> {
        self.enqueue_broker_results_with_reply(results, None)
    }

    fn enqueue_broker_results_with_reply(
        &self,
        results: Vec<BrokerOperationResult>,
        terminal_reply: Option<&Arc<SavedToolReply>>,
    ) -> Result<(), String> {
        if results.is_empty() {
            return Ok(());
        }
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        self.ensure_state_ownership()?;
        for result in &results {
            self.hydrate_extension_checkpoint(&result.invoking_extension)?;
        }
        let scope = self.session_scope()?;
        let mut affected = HashSet::new();
        {
            let pending = self
                .pending_events
                .lock()
                .map_err(|_| "Extension event lock poisoned".to_string())?;
            let unsettled = self
                .unsettled_broker
                .lock()
                .map_err(|_| "Broker intent lock poisoned".to_string())?;
            let mut seen = HashSet::new();
            for result in &results {
                if let Some(receipt) = &result.receipt {
                    let matches =
                        unsettled
                            .get(&result.invoking_extension)
                            .is_some_and(|intents| {
                                intents.iter().any(|intent| {
                                    intent.receipt == *receipt && intent.request == result.request
                                })
                            });
                    let already_queued = pending
                        .get(&scope)
                        .and_then(|queues| queues.get(&result.invoking_extension))
                        .is_some_and(|events| {
                            events.iter().any(|event| {
                                event.topic == "broker_response"
                                    && event.payload["receipt_id"].as_u64() == Some(receipt.id)
                            })
                        });
                    if receipt.scope != scope
                        || !seen.insert(receipt.id)
                        || already_queued
                        || !matches
                    {
                        return Err("Broker outcome does not match an unsettled receipt, or is a duplicate; preserve the checkpoint and do not replay the operation".into());
                    }
                }
            }
        }
        {
            let mut pending = self
                .pending_events
                .lock()
                .map_err(|_| "Extension event lock poisoned".to_string())?;
            let pending = pending.entry(scope).or_default();
            for result in results {
                affected.insert(result.invoking_extension.clone());
                let mut payload = serde_json::json!({
                    "api_version": BROKER_API_VERSION,
                    "ok": result.error.is_none(),
                });
                payload["capability"] = result.request.capability.into();
                payload["operation"] = result.request.operation.into();
                // JSON's macro serializes its arguments; assign owned values
                // directly so large protocol replies are moved, not copied.
                payload["arguments"] = result.request.arguments;
                if let Some(receipt) = result.receipt {
                    payload["receipt_id"] = receipt.id.into();
                }
                match result.error {
                    Some(error) => {
                        payload["error"] =
                            serde_json::json!({"code": error.code, "message": error.message})
                    }
                    None => payload["value"] = result.value,
                }
                pending
                    .entry(result.invoking_extension)
                    .or_default()
                    .push(Arc::new(WasiExtensionEvent {
                        topic: "broker_response".into(),
                        payload,
                    }));
            }
        }
        for name in affected {
            let state = self
                .states
                .lock()
                .map_err(|_| "Extension state lock poisoned".to_string())?
                .get(&name)
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            self.commit_retained_outcomes_with_reply(&name, &state, true, terminal_reply.filter(|reply| reply.extension_name == name)).map_err(|error| format!("Broker outcomes for `{name}` were retained in memory but could not be committed: {error}; do not repeat the broker operations"))?;
        }
        Ok(())
    }

    /// Recommit a received outcome after storage recovers, without re-running it.
    fn commit_retained_outcomes(
        &self,
        extension_name: &str,
        state: &Value,
        force: bool,
    ) -> Result<(), String> {
        self.commit_retained_outcomes_with_reply(extension_name, state, force, None)
    }

    fn commit_retained_outcomes_with_reply(
        &self,
        extension_name: &str,
        state: &Value,
        force: bool,
        terminal_reply: Option<&Arc<SavedToolReply>>,
    ) -> Result<(), String> {
        let mut remaining = self
            .unsettled_broker
            .lock()
            .map_err(|_| "Broker intent lock poisoned".to_string())?
            .get(extension_name)
            .cloned()
            .unwrap_or_default();
        let before = remaining.len();
        let scope = self.session_scope()?;
        let queued = self
            .pending_events
            .lock()
            .map_err(|_| "Extension event lock poisoned".to_string())?
            .get(&scope)
            .and_then(|queues| queues.get(extension_name))
            .cloned()
            .unwrap_or_default();
        remaining.retain(|intent| !queued.iter().any(|event| intent.matches_event(event)));
        if !force && remaining.len() == before {
            return Ok(());
        }
        if let Some(reply) = terminal_reply {
            self.persist_state_events_reply(
                extension_name,
                state,
                true,
                Some(&remaining),
                Some(reply),
            )?;
            self.terminal_replies
                .lock()
                .map_err(|_| "Extension reply lock poisoned".to_string())?
                .insert(extension_name.into(), reply.clone());
        } else {
            self.persist_state_and_events(extension_name, state, true, Some(&remaining))?;
        }
        self.unsettled_broker
            .lock()
            .map_err(|_| "Broker intent lock poisoned".to_string())?
            .insert(extension_name.into(), remaining);
        Ok(())
    }

    fn session_scope(&self) -> Result<Option<String>, String> {
        self.session_id
            .lock()
            .map(|scope| scope.clone())
            .map_err(|_| "Extension session lock poisoned".to_string())
    }

    pub fn active_session_scope(&self) -> Result<Option<String>, String> {
        self.session_scope()
    }

    fn filter_granted_requests(&self, requests: Vec<HostBrokerRequest>) -> Vec<HostBrokerRequest> {
        let Ok(policy) = self.capability_grant_policy() else {
            return Vec::new();
        };
        let Ok(extensions) = self.extensions.read() else {
            return Vec::new();
        };
        requests
            .into_iter()
            .filter(|request| {
                extensions
                    .get(&request.invoking_extension)
                    .is_some_and(|extension| {
                        extension.manifest.api_version == BROKER_API_VERSION
                            && policy.allows_declared(
                                &extension.manifest.capabilities,
                                &request.request.capability,
                            )
                    })
            })
            .collect()
    }

    fn find_response_extension_cached(&self, kind: &str, name: &str) -> Option<Arc<WasiExtension>> {
        self.extensions
            .read()
            .ok()?
            .values()
            .find(|extension| {
                let contributions = if kind == "tool" {
                    &extension.manifest.tools
                } else {
                    return extension
                        .manifest
                        .commands
                        .iter()
                        .any(|command| command.name == name);
                };
                contributions.iter().any(|tool| tool.name == name)
            })
            .cloned()
    }

    fn find_response_extension(&self, kind: &str, name: &str) -> Option<Arc<WasiExtension>> {
        self.find_response_extension_cached(kind, name)
    }

    fn find_hook_extensions(&self, name: &str) -> Vec<Arc<WasiExtension>> {
        let Ok(registry) = self.extensions.read() else {
            return Vec::new();
        };
        let mut extensions = registry
            .values()
            .filter(|extension| extension.manifest.hooks.iter().any(|hook| hook == name))
            .cloned()
            .collect::<Vec<_>>();
        extensions.sort_by(|left, right| left.manifest.name.cmp(&right.manifest.name));
        extensions
    }

    /// Single VM invocation per hook; use `begin_hook_operations` across dispatch.
    pub fn execute_hook_with_broker_requests(
        &self,
        name: &str,
        args: &str,
    ) -> Vec<Result<WasiExtensionInvocationResult, String>> {
        self.find_hook_extensions(name)
            .into_iter()
            .map(|extension| self.invoke(&extension, "hook", name, args))
            .collect()
    }

    pub fn execute_hook_with_effects(
        &self,
        name: &str,
        args: &str,
    ) -> Vec<Result<WasiExtensionInvocationResult, String>> {
        self.execute_hook_with_broker_requests(name, args)
    }

    fn execute_response(
        &self,
        kind: &str,
        name: &str,
        args: &str,
    ) -> Option<Result<WasiExtensionInvocationResult, String>> {
        let extension = self.find_response_extension(kind, name)?;
        Some(self.invoke(&extension, kind, name, args))
    }

    fn begin_delivery(
        &self,
        extension_name: &str,
        owns_operation: bool,
        reply_owner: Option<&ToolExecutionIdentity>,
    ) -> Result<(Value, Option<String>, Vec<Arc<WasiExtensionEvent>>), String> {
        let _commit = self
            .state_commit
            .lock()
            .map_err(|_| "Extension state commit lock poisoned".to_string())?;
        self.ensure_state_ownership()?;
        self.ensure_storage_confirmed()?;
        self.hydrate_extension_checkpoint(extension_name)?;
        let saved = self
            .terminal_replies
            .lock()
            .map_err(|_| "Extension reply lock poisoned".to_string())?
            .get(extension_name)
            .cloned();
        if let Some(saved) = saved {
            if reply_owner != Some(&saved.identity) || saved.canonical_result.is_some() {
                return Err("This extension has a saved terminal reply awaiting its canonical tool result; recover and acknowledge that reply, do not execute the extension again".into());
            }
            // Seal the known reply before its own after hook consumes broker
            // events. A crash during the hook must never require replaying it.
            let scope = self.session_scope()?;
            let events = self
                .pending_events
                .lock()
                .map_err(|_| "Extension event lock poisoned".to_string())?
                .get(&scope)
                .and_then(|queues| queues.get(extension_name))
                .cloned()
                .unwrap_or_default();
            let mut sealed = (*saved).clone();
            sealed.result = saved.result(&events)?;
            sealed.broker_receipt_ids.clear();
            let sealed = Arc::new(sealed);
            let state = self
                .states
                .lock()
                .map_err(|_| "Extension state lock poisoned".to_string())?
                .get(extension_name)
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            self.persist_state_events_reply(extension_name, &state, true, None, Some(&sealed))?;
            self.terminal_replies
                .lock()
                .map_err(|_| "Extension reply lock poisoned".to_string())?
                .insert(extension_name.into(), sealed);
        }
        if !owns_operation
            && self
                .active_operations
                .lock()
                .map_err(|_| "Extension operation lock poisoned".to_string())?
                .contains(extension_name)
        {
            return Err(format!("Extension `{extension_name}` already has an active call; wait for it to finish and do not restart its broker work"));
        }
        if self
            .in_flight_events
            .lock()
            .map_err(|_| "Extension delivery lock poisoned".to_string())?
            .contains_key(extension_name)
        {
            return Err(format!("Extension `{extension_name}` is already executing; wait for that invocation to finish instead of restarting its broker operations"));
        }
        let state = self
            .states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())?
            .get(extension_name)
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let scope = self.session_scope()?;
        self.commit_retained_outcomes(extension_name, &state, false)?;
        if self
            .unsettled_broker
            .lock()
            .map_err(|_| "Broker intent lock poisoned".to_string())?
            .get(extension_name)
            .is_some_and(|intents| !intents.is_empty())
        {
            let path = self
                .state_path(extension_name)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "in-memory checkpoint".into());
            return Err(format!("Extension `{extension_name}` has unsettled broker operations that may already have executed; do not retry or replay them. Inspect `{path}` and reconcile the original outcomes before continuing"));
        }
        let events = self.drain_events_for(extension_name, &scope)?;
        self.in_flight_events
            .lock()
            .map_err(|_| "Extension delivery lock poisoned".to_string())?
            .insert(extension_name.to_string(), events.clone());
        Ok((state, scope, events))
    }

    fn invoke(
        &self,
        extension: &WasiExtension,
        kind: &str,
        name: &str,
        args: &str,
    ) -> Result<WasiExtensionInvocationResult, String> {
        self.invoke_owned(extension, kind, name, args, false, None, false)
    }

    fn invoke_owned(
        &self,
        extension: &WasiExtension,
        kind: &str,
        name: &str,
        args: &str,
        owns_operation: bool,
        identity: Option<&ToolExecutionIdentity>,
        ephemeral: bool,
    ) -> Result<WasiExtensionInvocationResult, String> {
        if let Some(identity) = identity {
            if !identity.matches_call(&identity.tool_call_id, &identity.tool_name)
                || self.state_dir.is_none()
                || self.session_scope()?.as_deref() != Some(identity.session_id.as_str())
                || !(kind == "tool" || kind == "hook" && name == "after_tool_call")
            {
                return Err("Durable extension execution requires a valid committed tool identity and checkpoint storage".into());
            }
            if kind == "tool" {
                if let Some(reply) = self.recover_tool_reply(identity, name, args)? {
                    return Ok(WasiExtensionInvocationResult {
                        api_version: extension.manifest.api_version,
                        response: match reply {
                            Ok(message) => WasiExtensionResponse {
                                message: Some(message),
                                ..Default::default()
                            },
                            Err(error) => WasiExtensionResponse {
                                error: Some(error),
                                ..Default::default()
                            },
                        },
                        invoking_extension: extension.manifest.name.clone(),
                        ..Default::default()
                    });
                }
            }
        }
        let arguments =
            serde_json::from_str(args).unwrap_or_else(|_| serde_json::json!({ "raw": args }));
        let policy = self.effective_capability_policy(extension)?;
        let (state, scope, events) = self.begin_delivery(
            &extension.manifest.name,
            owns_operation,
            identity.filter(|_| kind == "hook"),
        )?;
        let previous_state = state.clone();
        #[derive(Serialize)]
        struct Invocation<'a> {
            api_version: u32,
            kind: &'a str,
            name: &'a str,
            arguments: &'a Value,
            state: &'a Value,
            events: Vec<&'a WasiExtensionEvent>,
        }
        let invocation = Invocation {
            api_version: extension.manifest.api_version,
            kind,
            name,
            arguments: &arguments,
            state: &state,
            events: events.iter().map(Arc::as_ref).collect(),
        };
        let result = match kind {
            "tool" => extension.call_with_policy(
                "execute_tool",
                &invocation,
                invocation.api_version,
                policy,
            ),
            "hook" => extension.call_with_policy(
                "handle_hook",
                &invocation,
                invocation.api_version,
                policy,
            ),
            _ => extension.call_with_policy(
                "execute_command",
                &invocation,
                invocation.api_version,
                policy,
            ),
        }
        .and_then(|mut result| {
            // Merge state before acknowledging events or exposing broker work.
            let _commit = self
                .state_commit
                .lock()
                .map_err(|_| "Extension state commit lock poisoned".to_string())?;
            let ephemeral_keys = self
                .ephemeral_state_keys
                .lock()
                .map_err(|_| "Ephemeral extension state lock poisoned".to_string())?
                .get(&extension.manifest.name)
                .cloned()
                .unwrap_or_default();
            let state = merge_extension_state(
                &previous_state,
                result.response.state.clone(),
                ephemeral,
                &ephemeral_keys,
            )?;
            let mut requests = if result.response.error.is_none() {
                self.filter_granted_requests(
                    std::mem::take(&mut result.broker_requests)
                        .into_iter()
                        .map(|request| HostBrokerRequest {
                            request,
                            invoking_extension: extension.manifest.name.clone(),
                            receipt: None,
                        })
                        .collect(),
                )
            } else {
                vec![]
            };
            if result.response.continue_after_broker && requests.is_empty() {
                result.response.continue_after_broker = false;
                result.response.error = Some(format!(
                    "WASI tool `{name}` requested a broker continuation without any requests; \
                     check capability grants and clear `continue_after_broker` when finished"
                ));
            }
            if !requests.is_empty() {
                self.restore_receipt_counter()?;
            }
            let count = u64::try_from(requests.len())
                .map_err(|_| "Broker receipt batch is too large".to_string())?;
            let previous_id = self
                .last_broker_id
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |id| {
                    id.checked_add(count)
                })
                .map_err(|_| "Broker receipt ID exhausted".to_string())?;
            let mut intents = Vec::with_capacity(requests.len());
            for (index, request) in requests.iter_mut().enumerate() {
                let id = previous_id + index as u64 + 1;
                let receipt = BrokerReceipt {
                    id,
                    scope: scope.clone(),
                };
                intents.push(Arc::new(BrokerIntent {
                    receipt: receipt.clone(),
                    request: request.request.clone(),
                }));
                request.receipt = Some(receipt);
            }
            if ephemeral {
                let mut ephemeral_receipts = self
                    .ephemeral_broker_receipts
                    .lock()
                    .map_err(|_| "Ephemeral broker receipt lock poisoned".to_string())?;
                ephemeral_receipts
                    .entry(extension.manifest.name.clone())
                    .or_default()
                    .extend(intents.iter().map(|intent| intent.receipt.id));
            }
            let terminal_reply = if kind == "hook" {
                self.terminal_replies
                    .lock()
                    .map_err(|_| "Extension reply lock poisoned".to_string())?
                    .get(&extension.manifest.name)
                    .cloned()
            } else {
                identity
                    .filter(|_| !result.response.continue_after_broker)
                    .map(|identity| {
                        Arc::new(SavedToolReply {
                            identity: identity.clone(),
                            extension_name: extension.manifest.name.clone(),
                            tool_name: name.into(),
                            arguments: arguments.clone(),
                            result: match &result.response.error {
                                Some(error) => Err(error.clone()),
                                None => Ok(result.response.message.clone().unwrap_or_default()),
                            },
                            broker_receipt_ids: requests
                                .iter()
                                .filter_map(|request| {
                                    request.receipt.as_ref().map(|receipt| receipt.id)
                                })
                                .collect(),
                            canonical_result: None,
                        })
                    })
            };
            self.persist_state_events_reply(
                &extension.manifest.name,
                &state,
                false,
                Some(&intents),
                terminal_reply.as_ref(),
            )?;
            if let Some(reply) = terminal_reply {
                self.terminal_replies
                    .lock()
                    .map_err(|_| "Extension reply lock poisoned".to_string())?
                    .insert(extension.manifest.name.clone(), reply);
            }
            self.unsettled_broker
                .lock()
                .map_err(|_| "Broker intent lock poisoned".to_string())?
                .insert(extension.manifest.name.clone(), intents);
            result.host_broker_requests = requests;
            self.states
                .lock()
                .map_err(|_| "Extension state lock poisoned".to_string())?
                .insert(extension.manifest.name.clone(), state);
            self.in_flight_events
                .lock()
                .map_err(|_| "Extension delivery lock poisoned".to_string())?
                .remove(&extension.manifest.name);
            Ok(result)
        });
        let mut result = match result {
            Ok(result) => result,
            Err(error) => {
                let _commit = self
                    .state_commit
                    .lock()
                    .map_err(|_| "Extension state commit lock poisoned".to_string())?;
                self.in_flight_events
                    .lock()
                    .map_err(|_| "Extension delivery lock poisoned".to_string())?
                    .remove(&extension.manifest.name);
                self.restore_events_for(&extension.manifest.name, &scope, events)
                    .map_err(|restore_error| {
                        format!("{error}; could not restore undelivered events: {restore_error}")
                    })?;
                return Err(error);
            }
        };
        result.invoking_extension = extension.manifest.name.clone();
        Ok(result)
    }

    fn effective_capability_policy(
        &self,
        extension: &WasiExtension,
    ) -> Result<CapabilityPolicy, String> {
        let host_policy = self.capability_grant_policy()?;
        if extension.manifest.api_version < BROKER_API_VERSION {
            return Ok(CapabilityPolicy::default());
        }
        Ok(CapabilityPolicy::new(
            extension
                .manifest
                .capabilities
                .iter()
                .filter(|capability| {
                    host_policy.allows_declared(&extension.manifest.capabilities, capability)
                })
                .cloned(),
        ))
    }
}

/// Marks a replaced-but-unsynced state file so lease-free readers
/// (`peek_host_state`) do not treat the visible candidate as committed. The
/// owning manager removes the marker when `recover_state_commit` confirms the
/// write; a new owner sweeps stale markers once it holds the lease.
fn unconfirmed_marker_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    Some(path.with_file_name(format!("{name}.unconfirmed")))
}

fn read_json_state(path: &Path) -> Result<Option<Value>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Cannot inspect extension state {}: {error}",
                path.display()
            ))
        }
    };
    if !metadata.is_file() {
        return Err(format!(
            "Extension state {} must be a regular file, not a symlink or directory",
            path.display()
        ));
    }
    let bytes = fs::read(path)
        .map_err(|error| format!("Cannot read extension state {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        format!(
            "Invalid extension state {}: {error}; preserve the file and repair it before reloading",
            path.display()
        )
    })
}

#[cfg(unix)]
fn sync_state_directories(
    directory: &Path,
    project_root: &Path,
    mut sync: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    if !directory.starts_with(project_root) {
        return Err("Extension storage is outside its project root".into());
    }
    for path in directory.ancestors() {
        sync(path).map_err(|error| format!("Cannot sync extension storage directory {}: {error}; preserve state and repair storage before retrying", path.display()))?;
        if path == project_root {
            return Ok(());
        }
    }
    Err("Extension storage is outside its project root".into())
}

#[cfg(all(test, unix))]
thread_local! {
    static FAIL_NEXT_STATE_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(unix)]
fn sync_state_parent(parent: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_STATE_DIRECTORY_SYNC.with(|fault| fault.replace(false)) {
        return Err(std::io::Error::other(
            "injected directory sync failure after replacement",
        ));
    }
    fs::File::open(parent)?.sync_all()
}

fn persist_json_state<T: Serialize>(path: &Path, state: &T) -> Result<(), StateWriteError> {
    let parent = path
        .parent()
        .ok_or_else(|| StateWriteError::before("Extension state path has no parent"))?;
    fs::create_dir_all(parent).map_err(StateWriteError::before)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(StateWriteError::before)?;
    {
        // Keep pretty JSON without allocating another checkpoint-sized buffer.
        let mut writer = BufWriter::new(staged.as_file_mut());
        serde_json::to_writer_pretty(&mut writer, state).map_err(StateWriteError::before)?;
        writer.flush().map_err(StateWriteError::before)?;
    }
    staged
        .as_file()
        .sync_all()
        .map_err(StateWriteError::before)?;
    staged.persist(path).map_err(StateWriteError::before)?;
    #[cfg(unix)]
    sync_state_parent(parent).map_err(|error| StateWriteError {
        message: format!(
            "Replacement {} is visible but its directory sync failed: {error}",
            path.display()
        ),
        replaced: true,
    })?;
    Ok(())
}

impl WasiExtensionManager {
    fn rebuild_tool_definitions(&self) -> Result<(), String> {
        let Ok(extensions) = self.extensions.read() else {
            return Err("Extension registry lock poisoned".to_string());
        };
        let mut tools: Vec<_> = extensions
            .values()
            .flat_map(|extension| extension.manifest.tools.iter())
            .map(|tool| {
                AgentToolDefinition::new(
                    tool.name.clone(),
                    tool.description.clone(),
                    tool.parameters.clone(),
                )
            })
            .collect();
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        *self
            .tool_definitions
            .write()
            .map_err(|_| "Extension tool definitions lock poisoned".to_string())? = tools.into();
        Ok(())
    }

    pub fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        self.tool_definitions
            .read()
            .map(|tools| tools.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod state_commit_tests {
    use super::{
        persist_json_state, BrokerOperationResult, BrokerRequest, WasiExtension,
        WasiExtensionManager, MAX_PENDING_EVENTS_PER_EXTENSION,
    };
    use serde_json::json;
    use std::fs;
    #[cfg(unix)]
    use std::path::Path;

    #[test]
    fn state_reader_distinguishes_absence_from_invalid_storage() {
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join("state.json");
        assert_eq!(super::read_json_state(&path).unwrap(), None);
        for value in [json!({"phase":"waiting"}), json!("legacy"), json!(null)] {
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(super::read_json_state(&path).unwrap(), Some(value));
        }
        fs::write(&path, b"{incomplete state").unwrap();
        let error = super::read_json_state(&path).unwrap_err();
        assert!(error.contains("preserve the file"), "{error}");
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(super::read_json_state(&path)
            .unwrap_err()
            .contains("regular file"));
    }

    #[cfg(unix)]
    #[test]
    fn state_reader_rejects_symlinks_including_dangling_ones() {
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join("state.json");
        let target = project.path().join("target.json");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(super::read_json_state(&path).is_err());
        fs::write(&target, b"{}").unwrap();
        assert!(super::read_json_state(&path).is_err());
        assert_eq!(fs::read(target).unwrap(), b"{}");
    }

    #[test]
    fn scope_switch_preserves_the_active_scope_on_invalid_state() {
        let project = tempfile::tempdir().unwrap();
        let manager = WasiExtensionManager::for_project_session(project.path(), "a");
        let extension = WasiExtension::load_from_bytes(b"\0asm\x01\0\0\0".to_vec()).unwrap();
        let name = extension.manifest.name.clone();
        manager.register_extension(extension).unwrap();
        manager
            .set_extension_state(&name, json!({"phase":"waiting"}))
            .unwrap();
        manager
            .set_host_state("tools.policy", json!("read_only"))
            .unwrap();
        manager.subscribe_event(&name, "updates".into()).unwrap();
        manager
            .publish_event("updates".into(), json!("kept"))
            .unwrap();
        let path = WasiExtensionManager::session_state_path(project.path(), "b", &name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"{incomplete state").unwrap();
        let error = manager.set_session_scope("b").unwrap_err();
        assert!(error.contains("state"), "{error}");
        assert_eq!(manager.active_session_scope().unwrap(), Some("a".into()));
        assert_eq!(
            manager.extension_state(&name),
            Some(json!({"phase":"waiting"}))
        );
        assert_eq!(
            manager.host_state("tools.policy").unwrap(),
            Some(json!("read_only"))
        );
        assert_eq!(
            manager.drain_events_for(&name, &Some("a".into())).unwrap()[0].payload,
            json!("kept")
        );
        assert_eq!(fs::read(path).unwrap(), b"{incomplete state");
    }

    #[test]
    fn reload_preserves_the_live_registry_and_state_on_invalid_storage() {
        let project = tempfile::tempdir().unwrap();
        let manifest = r#"{"name":"test","version":"1","description":"test"}"#;
        let module = format!(
            r#"(module
            (memory (export "memory") 1)
            (data (i32.const 0) "{}")
            (func (export "extension_info") (result i64) (i64.const {})))"#,
            manifest.replace('"', "\\\""),
            manifest.len()
        );
        let path = project.path().join(".threadlane/extensions/test.wasm");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, module).unwrap();
        let manager = WasiExtensionManager::for_project(project.path());
        assert_eq!(
            manager
                .reload_from_roots(None, Some(project.path()))
                .unwrap(),
            1
        );
        manager
            .set_extension_state("test", json!({"phase":"waiting"}))
            .unwrap();
        manager.subscribe_event("test", "updates".into()).unwrap();
        manager
            .publish_event("updates".into(), json!("kept"))
            .unwrap();
        let state = manager.state_path("test").unwrap();
        fs::write(&state, b"{incomplete state").unwrap();
        assert!(manager
            .reload_from_roots(None, Some(project.path()))
            .is_err());
        assert!(manager.extension_manifest("test").is_some());
        assert_eq!(
            manager.extension_state("test"),
            Some(json!({"phase":"waiting"}))
        );
        assert_eq!(
            manager.drain_events_for("test", &None).unwrap()[0].payload,
            json!("kept")
        );
        assert_eq!(fs::read(state).unwrap(), b"{incomplete state");
    }

    #[test]
    fn failed_writes_preserve_cached_state() {
        let project = tempfile::tempdir().unwrap();
        let manager = WasiExtensionManager::for_project(project.path());
        manager.set_extension_state("planner", json!(1)).unwrap();
        manager.set_host_state("tools.policy", json!(1)).unwrap();
        for path in [
            manager.state_path("planner"),
            manager.host_state_path("tools.policy"),
        ] {
            let path = path.unwrap();
            fs::remove_file(&path).unwrap();
            fs::create_dir(&path).unwrap();
        }
        assert!(manager.set_extension_state("planner", json!(2)).is_err());
        assert_eq!(manager.extension_state("planner"), Some(json!(1)));
        assert!(manager.set_host_state("tools.policy", json!(2)).is_err());
        assert_eq!(manager.host_state("tools.policy").unwrap(), Some(json!(1)));
        assert_eq!(
            fs::read_dir(manager.state_dir.as_ref().unwrap())
                .unwrap()
                .count(),
            3 // two preserved destinations plus the stable owner lock
        );
    }

    #[test]
    fn unsafe_host_keys_are_single_distinct_file_names() {
        let project = tempfile::tempdir().unwrap();
        let manager = WasiExtensionManager::for_project_session(project.path(), "session/../a");
        let parent = manager
            .host_state_path("tools.policy")
            .unwrap()
            .parent()
            .unwrap()
            .to_owned();
        let keys = [
            "tools.policy",
            "../../escaped",
            "a/b",
            "a\\b",
            "",
            ".encoded-612f62",
        ];
        let mut paths = std::collections::HashSet::new();
        for key in keys {
            let path = manager.host_state_path(key).unwrap();
            assert_eq!(path.parent(), Some(parent.as_path()), "{key}: {path:?}");
            assert!(paths.insert(path));
            manager.set_host_state(key, json!(key)).unwrap();
        }
        drop(manager);
        let reloaded = WasiExtensionManager::for_project_session(project.path(), "session/../a");
        for key in keys {
            assert_eq!(reloaded.host_state(key).unwrap(), Some(json!(key)));
        }
    }

    #[test]
    fn atomic_replacement_keeps_previous_inode_intact() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        persist_json_state(&path, &json!({"old": "durable"})).unwrap();
        let old = fs::read(&path).unwrap();
        let previous = directory.path().join("previous.json");
        fs::hard_link(&path, &previous).unwrap();
        persist_json_state(&path, &json!({"new": "durable"})).unwrap();
        assert_eq!(fs::read(previous).unwrap(), old);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(path).unwrap()).unwrap(),
            json!({"new": "durable"})
        );
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn cold_state_ownership_requires_an_existing_work_directory() {
        let project = tempfile::tempdir().unwrap();
        let missing = project.path().join("missing");
        let manager = WasiExtensionManager::for_project_session(&missing, "a");
        let error = manager
            .set_extension_state("probe", json!({"saved": true}))
            .unwrap_err();
        assert!(error.contains("restore the work directory"), "{error}");
        assert!(!missing.exists());
        assert!(manager.state_owner.lock().unwrap().is_none());
        assert!(manager.extension_state("probe").is_none());
        fs::create_dir(&missing).unwrap();
        manager
            .set_extension_state("probe", json!({"saved": true}))
            .unwrap();
        assert_eq!(
            manager.load_state("probe").unwrap(),
            Some(json!({"saved": true}))
        );
    }

    #[cfg(unix)]
    #[test]
    fn scope_directory_sync_retries_existing_ancestors_after_failure() {
        let project = tempfile::tempdir().unwrap();
        let manager = WasiExtensionManager::for_project_session(project.path(), "a");
        manager
            .set_extension_state("probe", json!({"saved": true}))
            .unwrap();
        let directory = manager
            .state_path("probe")
            .unwrap()
            .parent()
            .unwrap()
            .to_owned();
        let expected = [
            project
                .path()
                .join(".threadlane/state/extensions/sessions/61"),
            project.path().join(".threadlane/state/extensions/sessions"),
            project.path().join(".threadlane/state/extensions"),
            project.path().join(".threadlane/state"),
            project.path().join(".threadlane"),
            project.path().to_owned(),
        ];
        let mut attempted = Vec::new();
        let error = super::sync_state_directories(&directory, project.path(), |path| {
            attempted.push(path.to_owned());
            if path == expected[3] {
                return Err(std::io::Error::other("injected sync failure"));
            }
            fs::File::open(path)?.sync_all()
        })
        .unwrap_err();
        assert_eq!(attempted, expected[..4]);
        assert!(
            error.contains(&expected[3].display().to_string()),
            "{error}"
        );
        assert!(error.contains("preserve state"), "{error}");
        assert!(expected.iter().all(|path| path.is_dir()));

        attempted.clear();
        super::sync_state_directories(&directory, project.path(), |path| {
            attempted.push(path.to_owned());
            fs::File::open(path)?.sync_all()
        })
        .unwrap();
        assert_eq!(attempted, expected);
        drop(manager);
        let successor = WasiExtensionManager::for_project_session(project.path(), "a");
        assert_eq!(
            successor.load_state("probe").unwrap(),
            Some(json!({"saved": true}))
        );
    }

    #[test]
    fn streamed_state_matches_pretty_json_and_preserves_target_after_partial_serialization() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let text = "source 🦀\n\"quoted\"\\\t".repeat(10_000);
        let state = json!({"documents": [{"text": text, "version": 1}], "optional": null});
        let expected = serde_json::to_vec_pretty(&state).unwrap();
        persist_json_state(&path, &state).unwrap();
        assert_eq!(fs::read(&path).unwrap(), expected);

        struct FailsAfterPrefix<'a>(&'a str);
        impl serde::Serialize for FailsAfterPrefix<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeSeq;
                let mut sequence = serializer.serialize_seq(Some(2))?;
                sequence.serialize_element(self.0)?;
                Err(serde::ser::Error::custom(
                    "injected partial serialization failure",
                ))
            }
        }
        let error = persist_json_state(&path, &FailsAfterPrefix(&text)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected partial serialization failure"),
            "{error}"
        );
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn scope_directory_sync_stays_within_the_project_anchor() {
        let mut attempted = Vec::new();
        let error = super::sync_state_directories(
            Path::new("/other/state"),
            Path::new("/project"),
            |path| {
                attempted.push(path.to_owned());
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.contains("outside its project root"), "{error}");
        assert!(attempted.is_empty());
        for root in [Path::new("."), Path::new("")] {
            attempted.clear();
            super::sync_state_directories(&root.join(".threadlane/state"), root, |path| {
                attempted.push(path.to_owned());
                Ok(())
            })
            .unwrap();
            assert_eq!(attempted.last().unwrap(), root);
            assert_eq!(attempted.len(), 3);
        }
    }

    #[test]
    fn invocation_does_not_publish_state_when_storage_fails() {
        let project = tempfile::tempdir().unwrap();
        let manager = WasiExtensionManager::for_project(project.path());
        let response = r#"{"state":{"phase":"requesting"},"continue_after_broker":true}"#;
        let wasm = format!(
            r#"(module
            (memory (export "memory") 1)
            (data (i32.const 0) "{}")
            (func (export "alloc") (param i32) (result i32) (i32.const 8192))
            (func (export "execute_tool") (param i32 i32) (result i64) (i64.const {})))"#,
            response.replace('"', "\\\""),
            response.len()
        );
        let extension = WasiExtension::load_from_bytes(wasm.into_bytes()).unwrap();
        let name = &extension.manifest.name;
        manager.subscribe_event(name, "updates".into()).unwrap();
        manager.publish_event("updates".into(), json!(1)).unwrap();
        manager
            .enqueue_broker_results(vec![BrokerOperationResult {
                receipt: None,
                invoking_extension: name.clone(),
                request: BrokerRequest {
                    api_version: 2,
                    capability: "process".into(),
                    operation: "recv".into(),
                    arguments: json!({}),
                },
                value: json!({"reply":"already consumed from the process"}),
                error: None,
            }])
            .unwrap();
        let expected = manager.pending_events.lock().unwrap()[&None][name].clone();
        manager
            .set_extension_state(name, json!({"phase":"ready"}))
            .unwrap();
        let path = manager.state_path(name).unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(manager.invoke(&extension, "tool", "test", "{}").is_err());
        assert_eq!(
            manager.extension_state(name),
            Some(json!({"phase":"ready"}))
        );
        assert_eq!(
            manager.pending_events.lock().unwrap()[&None][name],
            expected
        );
        fs::remove_dir(path).unwrap();
        assert!(manager.invoke(&extension, "tool", "test", "{}").is_ok());
        assert!(manager.drain_events_for(name, &None).unwrap().is_empty());
    }

    #[test]
    fn trapped_invocations_preserve_their_events() {
        let manager = WasiExtensionManager::new();
        let extension = WasiExtension::load_from_bytes(
            br#"(module
            (memory (export "memory") 1)
            (func (export "alloc") (param i32) (result i32) (i32.const 0))
            (func (export "execute_tool") (param i32 i32) (result i64) unreachable))"#
                .to_vec(),
        )
        .unwrap();
        let name = &extension.manifest.name;
        manager.subscribe_event(name, "updates".into()).unwrap();
        manager.publish_event("updates".into(), json!(1)).unwrap();
        let expected = manager.pending_events.lock().unwrap()[&None][name].clone();
        for _ in 0..2 {
            let error = manager
                .invoke(&extension, "tool", "test", "{}")
                .unwrap_err();
            assert!(error.contains("unreachable"), "{error}");
            assert_eq!(
                manager.pending_events.lock().unwrap()[&None][name],
                expected
            );
        }
    }

    #[test]
    fn notification_flood_preserves_broker_outcomes() {
        let manager = WasiExtensionManager::new();
        manager.subscribe_event("test", "updates".into()).unwrap();
        manager
            .enqueue_broker_results(vec![BrokerOperationResult {
                receipt: None,
                invoking_extension: "test".into(),
                request: BrokerRequest {
                    api_version: 2,
                    capability: "process".into(),
                    operation: "recv".into(),
                    arguments: json!({}),
                },
                value: json!("protocol reply"),
                error: None,
            }])
            .unwrap();
        for n in 0..MAX_PENDING_EVENTS_PER_EXTENSION + 10 {
            manager.publish_event("updates".into(), json!(n)).unwrap();
        }
        let events = manager.drain_events_for("test", &None).unwrap();
        assert_eq!(events.len(), MAX_PENDING_EVENTS_PER_EXTENSION + 1);
        assert_eq!(events[0].topic, "broker_response");
        assert_eq!(events[0].payload["value"], json!("protocol reply"));
        assert_eq!(events[1].payload, json!(10));
        assert!(manager
            .publish_event("broker_response".into(), json!({}))
            .is_err());
    }

    #[test]
    fn restored_batches_precede_new_events_and_do_not_revive_retired_scopes() {
        let manager = WasiExtensionManager::new();
        manager.set_session_scope("a").unwrap();
        manager.subscribe_event("test", "updates".into()).unwrap();
        manager
            .publish_event("updates".into(), json!("old"))
            .unwrap();
        let scope = Some("a".into());
        let batch = manager.drain_events_for("test", &scope).unwrap();
        manager
            .publish_event("updates".into(), json!("new"))
            .unwrap();
        manager.restore_events_for("test", &scope, batch).unwrap();
        let batch = manager.drain_events_for("test", &scope).unwrap();
        assert_eq!(
            batch.iter().map(|event| &event.payload).collect::<Vec<_>>(),
            vec![&json!("old"), &json!("new")]
        );
        manager.set_session_scope("b").unwrap();
        manager.restore_events_for("test", &scope, batch).unwrap();
        assert!(!manager.pending_events.lock().unwrap().contains_key(&scope));
        assert!(manager
            .drain_events_for("test", &Some("b".into()))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn failed_batches_preserve_the_notification_bound() {
        let manager = WasiExtensionManager::new();
        manager.subscribe_event("test", "updates".into()).unwrap();
        for n in 0..MAX_PENDING_EVENTS_PER_EXTENSION {
            manager.publish_event("updates".into(), json!(n)).unwrap();
        }
        let batch = manager.drain_events_for("test", &None).unwrap();
        for n in 0..MAX_PENDING_EVENTS_PER_EXTENSION {
            manager
                .publish_event(
                    "updates".into(),
                    json!(n + MAX_PENDING_EVENTS_PER_EXTENSION),
                )
                .unwrap();
        }
        manager.restore_events_for("test", &None, batch).unwrap();
        let restored = manager.drain_events_for("test", &None).unwrap();
        assert_eq!(restored.len(), MAX_PENDING_EVENTS_PER_EXTENSION);
        assert_eq!(restored.first().unwrap().payload, json!(0));
        assert_eq!(
            restored.last().unwrap().payload,
            json!(MAX_PENDING_EVENTS_PER_EXTENSION - 1)
        );
    }

    #[test]
    fn broker_event_queue_moves_large_values_without_copying() {
        let manager = WasiExtensionManager::new();
        let value = serde_json::Value::String("reply".repeat(100_000));
        let arguments = serde_json::Value::String("input".repeat(100_000));
        let value_ptr = value.as_str().unwrap().as_ptr();
        let arguments_ptr = arguments.as_str().unwrap().as_ptr();
        manager
            .enqueue_broker_results(vec![BrokerOperationResult {
                receipt: None,
                invoking_extension: "test".into(),
                request: BrokerRequest {
                    api_version: 2,
                    capability: "process".into(),
                    operation: "recv".into(),
                    arguments,
                },
                value,
                error: None,
            }])
            .unwrap();
        let events = manager.drain_events_for("test", &None).unwrap();
        assert_eq!(
            events[0].payload["value"].as_str().unwrap().as_ptr(),
            value_ptr
        );
        assert_eq!(
            events[0].payload["arguments"].as_str().unwrap().as_ptr(),
            arguments_ptr
        );
    }

    #[test]
    fn concurrent_commits_reload_and_match_the_cache() {
        let project = tempfile::tempdir().unwrap();
        let manager = WasiExtensionManager::for_project_session(project.path(), "a");
        let barrier = std::sync::Barrier::new(4);
        std::thread::scope(|threads| {
            for writer in 0..4 {
                let manager = &manager;
                let barrier = &barrier;
                threads.spawn(move || {
                    barrier.wait();
                    for sequence in 0..8 {
                        let value = json!({"writer":writer,"sequence":sequence});
                        manager
                            .set_extension_state("planner", value.clone())
                            .unwrap();
                        manager.set_host_state("tools.policy", value).unwrap();
                    }
                });
            }
        });
        let expected_state = manager.extension_state("planner");
        let expected_policy = manager.host_state("tools.policy").unwrap();
        manager.set_session_scope("b").unwrap();
        let reloaded = WasiExtensionManager::for_project_session(project.path(), "a");
        assert_eq!(reloaded.load_state("planner").unwrap(), expected_state);
        assert_eq!(
            reloaded.host_state("tools.policy").unwrap(),
            expected_policy
        );
        drop(reloaded);
        assert_eq!(manager.host_state("tools.policy").unwrap(), None);
        manager.set_host_state("tools.policy", json!("b")).unwrap();
        manager.set_session_scope("a").unwrap();
        assert_eq!(manager.host_state("tools.policy").unwrap(), expected_policy);
    }

    #[cfg(unix)]
    #[test]
    fn replacing_state_symlink_preserves_its_target() {
        let directory = tempfile::tempdir().unwrap();
        let external = directory.path().join("external.json");
        fs::write(&external, b"external content").unwrap();
        let state = directory.path().join("state.json");
        std::os::unix::fs::symlink(&external, &state).unwrap();
        persist_json_state(&state, &json!({"stored":true})).unwrap();
        assert_eq!(fs::read(external).unwrap(), b"external content");
        assert!(fs::symlink_metadata(state).unwrap().is_file());
    }
}

#[cfg(test)]
mod reload_tests {
    use super::*;
    use tempfile::tempdir;

    fn write_editor_lsp_extension(
        extensions_root: &Path,
        name: &str,
        include_command: bool,
    ) -> PathBuf {
        let commands = if include_command {
            vec![serde_json::json!({
                "name": "editor_lsp",
                "description": "test editor command",
            })]
        } else {
            Vec::new()
        };
        let manifest = serde_json::json!({
            "api_version": BROKER_API_VERSION,
            "name": name,
            "version": "1",
            "description": "test",
            "capabilities": ["process"],
            "commands": commands,
        })
        .to_string();
        let module = format!(
            r#"(module
                (memory (export "memory") 1)
                (data (i32.const 8) "{manifest}")
                (func (export "extension_info") (result i64) (i64.const {result})))"#,
            manifest = manifest.replace('\\', "\\\\").replace('"', "\\\""),
            result = (8u64 << 32) | manifest.len() as u64,
        );
        let path = extensions_root.join(format!("{name}.wasm"));
        fs::create_dir_all(extensions_root).unwrap();
        fs::write(&path, module).unwrap();
        path
    }

    fn editor_lsp_manager(global_root: Option<&Path>, project_root: &Path) -> WasiExtensionManager {
        let manager =
            WasiExtensionManager::for_project_session(project_root, "editor-lsp-provenance");
        manager
            .reload_from_roots(global_root, Some(project_root))
            .unwrap();
        manager
    }

    fn expect_unavailable_editor_lsp(manager: &WasiExtensionManager) -> String {
        match manager.begin_ephemeral_command_operation("editor_lsp") {
            Some(Err(error)) => error,
            Some(Ok(_)) => panic!("untrusted editor LSP command must not be claimed"),
            None => panic!("expected a registered but ineligible lsp_ext"),
        }
    }

    #[test]
    fn ephemeral_editor_lsp_requires_an_effective_global_installation() {
        let global = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_editor_lsp_extension(
            &project.path().join(".threadlane/extensions"),
            "lsp_ext",
            true,
        );
        let manager = editor_lsp_manager(Some(global.path()), project.path());
        let error = expect_unavailable_editor_lsp(&manager);
        assert!(error.contains("global extension scope"), "{error}");
        assert!(!manager
            .active_operations
            .lock()
            .unwrap()
            .contains("lsp_ext"));

        let global = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_editor_lsp_extension(&global.path().join("extensions"), "lsp_ext", true);
        let manager = editor_lsp_manager(Some(global.path()), project.path());
        let operation = manager
            .begin_ephemeral_command_operation("editor_lsp")
            .unwrap()
            .unwrap();
        assert!(manager
            .active_operations
            .lock()
            .unwrap()
            .contains("lsp_ext"));
        drop(operation);
        assert!(!manager
            .active_operations
            .lock()
            .unwrap()
            .contains("lsp_ext"));

        let global = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_editor_lsp_extension(&global.path().join("extensions"), "lsp_ext", true);
        let project_module = write_editor_lsp_extension(
            &project.path().join(".threadlane/extensions"),
            "lsp_ext",
            true,
        );
        let project_module = project_module.canonicalize().unwrap();
        let manager = editor_lsp_manager(Some(global.path()), project.path());
        assert_eq!(
            manager
                .extensions
                .read()
                .unwrap()
                .get("lsp_ext")
                .unwrap()
                .file_path
                .as_deref(),
            Some(project_module.as_path())
        );
        let error = expect_unavailable_editor_lsp(&manager);
        assert!(error.contains("global extension scope"), "{error}");
        assert!(!manager
            .active_operations
            .lock()
            .unwrap()
            .contains("lsp_ext"));
    }

    #[test]
    fn ephemeral_editor_lsp_rejects_a_canonical_global_module_inside_the_project() {
        let project = tempdir().unwrap();
        let alias_parent = project.path().join("alias-parent");
        fs::create_dir_all(&alias_parent).unwrap();
        let global_root = alias_parent.join("..").join("nested-global");
        write_editor_lsp_extension(&global_root.join("extensions"), "lsp_ext", true);

        let manager = editor_lsp_manager(Some(&global_root), project.path());
        let error = expect_unavailable_editor_lsp(&manager);

        assert!(error.contains("global extension scope"), "{error}");
        assert!(!manager
            .active_operations
            .lock()
            .unwrap()
            .contains("lsp_ext"));
    }

    #[test]
    fn ephemeral_editor_lsp_requires_the_declared_fixed_command() {
        let global = tempdir().unwrap();
        let project = tempdir().unwrap();
        write_editor_lsp_extension(&global.path().join("extensions"), "lsp_ext", false);

        let manager = editor_lsp_manager(Some(global.path()), project.path());
        let error = expect_unavailable_editor_lsp(&manager);

        assert!(error.contains("must declare"), "{error}");
        assert!(!manager
            .active_operations
            .lock()
            .unwrap()
            .contains("lsp_ext"));
    }

    #[test]
    fn ephemeral_editor_lsp_ignores_missing_and_disabled_global_modules() {
        let global = tempdir().unwrap();
        let project = tempdir().unwrap();
        let manager = WasiExtensionManager::for_project_session(project.path(), "editor-lsp");
        assert_eq!(
            manager
                .reload_from_roots(Some(global.path()), Some(project.path()))
                .unwrap(),
            0
        );
        assert!(manager
            .begin_ephemeral_command_operation("editor_lsp")
            .is_none());

        let module = write_editor_lsp_extension(&global.path().join("extensions"), "lsp_ext", true);
        fs::write(format!("{}.disabled", module.display()), "").unwrap();
        assert_eq!(
            manager
                .reload_from_roots(Some(global.path()), Some(project.path()))
                .unwrap(),
            0
        );
        assert!(manager
            .begin_ephemeral_command_operation("editor_lsp")
            .is_none());
    }

    #[test]
    fn tool_definition_slice_is_reused() {
        let manager = WasiExtensionManager::new();
        let mut extension = WasiExtension::load_from_bytes(b"\0asm\x01\0\0\0".to_vec()).unwrap();
        extension.manifest.tools.push(WasiToolDefinition {
            name: "echo".into(),
            description: "echo".into(),
            parameters: serde_json::json!({"type": "object"}),
        });
        manager.register_extension(extension).unwrap();

        let first = manager.tool_definitions();
        let second = manager.tool_definitions();

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn invocation_selection_owns_extensions_without_holding_registry_read_lock() {
        let manager = WasiExtensionManager::new();
        let mut extension = WasiExtension::load_from_bytes(b"\0asm\x01\0\0\0".to_vec()).unwrap();
        extension.manifest.commands.push(WasiCommandDefinition {
            name: "ping".into(),
            description: "test command".into(),
        });
        extension.manifest.hooks.push("before_tool_call".into());
        manager.register_extension(extension).unwrap();

        let command = manager
            .find_response_extension("command", "ping")
            .expect("registered command");
        let hooks = manager.find_hook_extensions("before_tool_call");

        let _writer = manager
            .extensions
            .try_write()
            .expect("selected invocations must not retain a registry read guard");
        assert_eq!(command.manifest.name, "unnamed_wasi_ext");
        assert_eq!(hooks.len(), 1);
    }

    #[test]
    fn ephemeral_editor_command_uses_the_fixed_registry_key() {
        let manager = WasiExtensionManager::new();
        let mut extension = WasiExtension::load_from_bytes(b"\0asm\x01\0\0\0".to_vec()).unwrap();
        extension.manifest.name = "spoof".into();
        extension.manifest.commands.push(WasiCommandDefinition {
            name: "editor_lsp".into(),
            description: "test command".into(),
        });
        manager.register_extension(extension).unwrap();

        assert!(manager
            .begin_ephemeral_command_operation("editor_lsp")
            .is_none());
    }

    #[test]
    fn ephemeral_editor_command_rejects_untrusted_fixed_extension() {
        let manager = WasiExtensionManager::new();
        let mut extension = WasiExtension::load_from_bytes(b"\0asm\x01\0\0\0".to_vec()).unwrap();
        extension.manifest.name = "lsp_ext".into();
        extension.manifest.commands.push(WasiCommandDefinition {
            name: "editor_lsp".into(),
            description: "test command".into(),
        });
        manager.register_extension(extension).unwrap();

        let error = expect_unavailable_editor_lsp(&manager);
        assert!(error.contains("global extension scope"), "{error}");
    }

    #[test]
    fn invocation_uses_module_compiled_at_load() {
        let mut extension = WasiExtension::load_from_bytes(b"\0asm\x01\0\0\0".to_vec()).unwrap();
        extension.wasm_bytes = vec![0xff];
        let invocation = WasiExtensionInvocation {
            api_version: 1,
            kind: "command".into(),
            name: "missing".into(),
            arguments: serde_json::json!({}),
            state: Value::Null,
            events: Vec::new(),
        };

        let error = extension
            .call_with_policy(
                "execute_command",
                &invocation,
                invocation.api_version,
                CapabilityPolicy::default(),
            )
            .unwrap_err();

        assert_eq!(error, "Memory export not found");
    }

    #[test]
    fn response_lookup_miss_does_not_reload_registry() {
        let project = tempdir().unwrap();
        let manager = WasiExtensionManager::for_project(project.path());
        let extension = WasiExtension::load_from_bytes(b"\0asm\x01\0\0\0".to_vec()).unwrap();
        let extension_name = extension.manifest.name.clone();
        manager.register_extension(extension).unwrap();

        assert!(manager
            .find_response_extension("command", "missing")
            .is_none());
        assert!(manager.extension_manifest(&extension_name).is_some());
    }

    #[test]
    fn reload_retires_changed_extension_queues_but_preserves_state() {
        let project = tempdir().unwrap();
        let manager = WasiExtensionManager::for_project(project.path());
        let old_extension = WasiExtension::load_from_bytes(b"\0asm\x01\0\0\0".to_vec()).unwrap();
        let extension_name = old_extension.manifest.name.clone();
        manager.register_extension(old_extension).unwrap();
        manager
            .set_extension_state(&extension_name, serde_json::json!({"kept": true}))
            .unwrap();
        manager
            .subscribe_event(&extension_name, "updates".into())
            .unwrap();
        manager
            .publish_event("updates".into(), serde_json::json!({"stale": true}))
            .unwrap();
        manager
            .pending_broker_requests
            .lock()
            .unwrap()
            .entry(None)
            .or_default()
            .push(HostBrokerRequest {
                receipt: None,
                invoking_extension: extension_name.clone(),
                request: BrokerRequest {
                    api_version: BROKER_API_VERSION,
                    capability: "tools".into(),
                    operation: "set_policy".into(),
                    arguments: serde_json::json!({}),
                },
            });

        let module = project.path().join(".threadlane/extensions/changed.wasm");
        fs::create_dir_all(module.parent().unwrap()).unwrap();
        fs::write(&module, b"\0asm\x01\0\0\0\0\x01\0").unwrap();

        manager
            .reload_from_roots(None, Some(project.path()))
            .unwrap();

        assert_eq!(
            manager.extension_state(&extension_name),
            Some(serde_json::json!({"kept": true}))
        );
        assert!(!manager
            .subscriptions
            .lock()
            .unwrap()
            .contains_key(&extension_name));
        assert!(manager
            .pending_events
            .lock()
            .unwrap()
            .values()
            .all(|events| !events.contains_key(&extension_name)));
        assert!(manager
            .pending_broker_requests
            .lock()
            .unwrap()
            .values()
            .flatten()
            .all(|request| request.invoking_extension != extension_name));
    }
}

#[cfg(test)]
mod ephemeral_broker_tests {
    use super::*;

    #[test]
    fn ephemeral_editor_broker_payloads_are_excluded_from_checkpoints() {
        let project = tempfile::tempdir().unwrap();
        let manager = WasiExtensionManager::for_project_session(project.path(), "editor-session");
        manager
            .reload_from_roots(None, Some(project.path()))
            .unwrap();
        let name = "lsp_ext";
        let source = "unsaved-private-buffer";
        let intent = Arc::new(BrokerIntent {
            receipt: BrokerReceipt {
                id: 44,
                scope: Some("editor-session".into()),
            },
            request: BrokerRequest {
                api_version: BROKER_API_VERSION,
                capability: "process".into(),
                operation: "send".into(),
                arguments: serde_json::json!({"data":source}),
            },
        });
        let event = Arc::new(WasiExtensionEvent {
            topic: "broker_response".into(),
            payload: serde_json::json!({"receipt_id":44,"value":{"message":source}}),
        });
        manager
            .unsettled_broker
            .lock()
            .unwrap()
            .insert(name.into(), vec![intent.clone()]);
        manager
            .in_flight_events
            .lock()
            .unwrap()
            .insert(name.into(), vec![event]);
        manager
            .ephemeral_broker_receipts
            .lock()
            .unwrap()
            .insert(name.into(), HashSet::from([44]));
        manager
            .ephemeral_state_keys
            .lock()
            .unwrap()
            .insert(name.into(), HashSet::from(["editor".into()]));

        manager
            .persist_state_events_reply(
                name,
                &serde_json::json!({
                    "editor":{"diagnostics":[{"message":source}]},
                    "ordinary":{"kept":true},
                }),
                true,
                Some(&[intent]),
                None,
            )
            .unwrap();

        let checkpoint = manager
            .load_checkpoint_in_scope(name, &Some("editor-session".into()))
            .unwrap()
            .unwrap();
        assert!(checkpoint.unsettled.is_empty());
        assert!(checkpoint.broker_events.is_empty());
        assert!(checkpoint.state.get("editor").is_none());
        assert_eq!(checkpoint.state["ordinary"]["kept"], true);
        let persisted = fs::read_to_string(manager.state_path(name).unwrap()).unwrap();
        assert!(!persisted.contains(source));

        manager.discard_ephemeral_broker_receipts(name);
        assert!(manager
            .unsettled_broker
            .lock()
            .unwrap()
            .get(name)
            .is_none_or(Vec::is_empty));
    }

    #[test]
    fn ordinary_state_reset_preserves_ephemeral_editor_memory_but_not_checkpoint() {
        let project = tempfile::tempdir().unwrap();
        let manager = WasiExtensionManager::for_project_session(project.path(), "editor-session");
        manager
            .reload_from_roots(None, Some(project.path()))
            .unwrap();
        let name = "lsp_ext";
        let keys = HashSet::from(["editor".to_owned()]);
        manager
            .ephemeral_state_keys
            .lock()
            .unwrap()
            .insert(name.into(), keys.clone());
        let previous = serde_json::json!({
            "editor":{"phase":"ready","document_id":7},
            "agent_lsp":{"server":"rust-analyzer"},
        });

        let state = merge_extension_state(&previous, None, false, &keys).unwrap();
        assert_eq!(state["editor"]["document_id"], 7);
        assert!(state.get("agent_lsp").is_none());
        manager
            .persist_state_events_reply(name, &state, false, None, None)
            .unwrap();
        manager.states.lock().unwrap().insert(name.into(), state.clone());

        assert_eq!(manager.extension_state(name), Some(state));
        let checkpoint = manager
            .load_checkpoint_in_scope(name, &Some("editor-session".into()))
            .unwrap()
            .unwrap();
        assert!(checkpoint.state.get("editor").is_none());
        assert!(checkpoint.state.as_object().unwrap().is_empty());
    }

    #[test]
    fn ephemeral_failure_without_state_preserves_agent_state() {
        let project = tempfile::tempdir().unwrap();
        let manager = WasiExtensionManager::for_project_session(project.path(), "editor-session");
        manager
            .reload_from_roots(None, Some(project.path()))
            .unwrap();
        let name = "lsp_ext";
        let keys = HashSet::from(["editor".to_owned()]);
        manager
            .ephemeral_state_keys
            .lock()
            .unwrap()
            .insert(name.into(), keys.clone());
        let previous = serde_json::json!({
            "editor":{"phase":"querying","document_id":7},
            "agent_lsp":{"phase":"requesting","server":"rust-analyzer"},
        });

        let state = merge_extension_state(&previous, None, true, &keys).unwrap();
        assert_eq!(state, previous);
        manager
            .persist_state_events_reply(name, &state, false, None, None)
            .unwrap();
        manager.states.lock().unwrap().insert(name.into(), state.clone());

        assert_eq!(
            manager.extension_state(name).unwrap()["agent_lsp"]["phase"],
            "requesting"
        );
        let checkpoint = manager
            .load_checkpoint_in_scope(name, &Some("editor-session".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            checkpoint.state["agent_lsp"]["server"],
            "rust-analyzer"
        );
        assert!(checkpoint.state.get("editor").is_none());
    }
}

#[cfg(test)]
mod native_stack_tests {
    //! wasmi's native stack use must stay bounded no matter how many Wasm
    //! instructions an extension executes.
    //!
    //! wasmi 2.0 selects a tail-call dispatch backend in optimized builds and
    //! relies on LLVM emitting sibling calls between instruction handlers. When
    //! a build blocks that optimization (the dev profile did through
    //! `debug-assertions`; see the `[profile.dev.package.wasmi]` note in the
    //! workspace `Cargo.toml`), every executed instruction nests one native
    //! frame and `extension_info` overflows the 512 KiB stack of GPUI's GCD
    //! worker threads after roughly six thousand instructions. The probe below
    //! runs an `extension_info` that executes far more than that on a thread
    //! with exactly that stack size. An overflow aborts the whole process, so
    //! the probe runs in a child process and the parent asserts on its status.
    use super::*;

    const PROBE_ENV: &str = "THREADLANE_WASI_NATIVE_STACK_PROBE";
    /// GCD worker threads, GPUI's background executor on macOS, get 512 KiB.
    const GCD_WORKER_STACK_SIZE: usize = 512 * 1024;
    const MANIFEST: &str =
        r#"{"name":"stack_probe","version":"0.1.0","description":"native stack probe"}"#;

    /// `extension_info` spins for 300k iterations (over a million executed
    /// instructions) before returning the manifest. The module is written in
    /// text form; wasmi parses it directly because its `wat` feature is on.
    fn probe_module() -> Vec<u8> {
        format!(
            r#"(module
  (memory (export "memory") 1)
  (data (i32.const 8) "{manifest}")
  (func (export "extension_info") (result i64)
    (local $n i32)
    (local.set $n (i32.const 300000))
    (block $done
      (loop $spin
        (local.set $n (i32.sub (local.get $n) (i32.const 1)))
        (br_if $done (i32.eqz (local.get $n)))
        (br $spin)))
    (i64.or (i64.shl (i64.const 8) (i64.const 32)) (i64.const {len}))))"#,
            manifest = MANIFEST.replace('"', "\\\""),
            len = MANIFEST.len(),
        )
        .into_bytes()
    }

    fn run_probe() {
        let loaded = std::thread::Builder::new()
            .name("wasi-native-stack-probe".into())
            .stack_size(GCD_WORKER_STACK_SIZE)
            .spawn(|| WasiExtension::load_from_bytes(probe_module()))
            .expect("spawn probe thread")
            .join()
            .expect("probe thread must not panic")
            .expect("probe extension must load");
        assert_eq!(loaded.manifest.name, "stack_probe");
    }

    #[test]
    fn extension_info_runs_on_a_gcd_sized_native_stack() {
        if std::env::var_os(PROBE_ENV).is_some() {
            run_probe();
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().expect("test binary path"))
            .args([
                "--exact",
                "native_stack_tests::extension_info_runs_on_a_gcd_sized_native_stack",
            ])
            .env(PROBE_ENV, "1")
            .output()
            .expect("run probe child");
        assert!(
            output.status.success(),
            "extension_info overflowed a {GCD_WORKER_STACK_SIZE}-byte native stack \
             (wasmi is nesting a native frame per Wasm instruction): {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[cfg(test)]
mod scope_safety_tests {
    use super::*;

    fn manager_with_extension() -> (WasiExtensionManager, String) {
        let manager = WasiExtensionManager::new();
        let extension = WasiExtension::load_from_bytes(b"\0asm\x01\0\0\0".to_vec()).unwrap();
        let name = extension.manifest.name.clone();
        manager.register_extension(extension).unwrap();
        (manager, name)
    }

    #[test]
    fn scope_switch_evicts_other_scope_queues() {
        let (manager, _name) = manager_with_extension();
        manager.set_session_scope("session-a").unwrap();
        manager
            .pending_broker_requests
            .lock()
            .unwrap()
            .entry(Some("session-a".into()))
            .or_default()
            .push(HostBrokerRequest {
                receipt: None,
                request: BrokerRequest {
                    api_version: 1,
                    capability: "process".into(),
                    operation: "run".into(),
                    arguments: serde_json::json!({}),
                },
                invoking_extension: "ext".into(),
            });
        manager.set_session_scope("session-b").unwrap();
        let pending = manager.pending_broker_requests.lock().unwrap();
        assert!(!pending.contains_key(&Some("session-a".into())));
        assert!(pending.contains_key(&Some("session-b".into())));
    }

    #[test]
    fn per_extension_event_queues_are_bounded() {
        let (manager, name) = manager_with_extension();
        manager.set_session_scope("session-a").unwrap();
        manager.subscribe_event(&name, "topic".into()).unwrap();
        for _ in 0..(MAX_PENDING_EVENTS_PER_EXTENSION + 10) {
            manager
                .publish_event("topic".into(), serde_json::json!({}))
                .unwrap();
        }
        let pending = manager.pending_events.lock().unwrap();
        let queue = &pending[&Some("session-a".into())][&name];
        assert_eq!(queue.len(), MAX_PENDING_EVENTS_PER_EXTENSION);
    }

    #[test]
    fn discovery_denies_manifest_less_modules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.wasm");
        std::fs::write(&path, b"\0asm\x01\0\0\0").unwrap();
        let error = match WasiExtension::load_from_file(&path) {
            Ok(_) => panic!("manifest-less module must be denied"),
            Err(error) => error,
        };
        assert!(error.contains("extension_info"), "{error}");
    }
}
