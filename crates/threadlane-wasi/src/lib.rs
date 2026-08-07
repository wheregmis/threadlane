pub mod broker;
pub mod packages;

pub use broker::*;
pub use packages::{
    default_global_threadlane_dir, validate_extension_id, ExtensionManager, ExtensionRecord,
    ExtensionScope,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use wasmi::{Caller, Engine, Extern, Func, Linker, Memory, Module, Store};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasiToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasiCommandDefinition {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasiExtensionManifest {
    #[serde(default = "default_api_version")]
    pub api_version: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub tools: Vec<WasiToolDefinition>,
    #[serde(default)]
    pub commands: Vec<WasiCommandDefinition>,
    #[serde(default)]
    pub hooks: Vec<String>,
}

fn default_api_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WasiExtensionEvent {
    pub topic: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasiExtensionInvocation {
    pub api_version: u32,
    pub kind: String,
    pub name: String,
    pub arguments: Value,
    #[serde(default)]
    pub state: Value,
    /// Events are queued by the host and delivered on this extension's next invocation.
    #[serde(default)]
    pub events: Vec<WasiExtensionEvent>,
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
    pub arguments: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub context: Option<Value>,
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
    pub state: Option<Value>,
    #[serde(default)]
    pub effects: Vec<WasiLegacyEffect>,
    #[serde(default)]
    pub middleware: Option<WasiHookMiddleware>,
}

#[derive(Debug, Clone, Default)]
pub struct WasiExtensionInvocationResult {
    pub api_version: u32,
    pub response: WasiExtensionResponse,
    pub broker_requests: Vec<BrokerRequest>,
    pub host_broker_requests: Vec<HostBrokerRequest>,
    /// Events supplied to this concrete WASM invocation.
    pub events: Vec<WasiExtensionEvent>,
    pub invoking_extension: String,
}

#[derive(Debug, Clone, Default)]
pub struct WasiExtensionCommandResult {
    pub api_version: u32,
    pub message: String,
    pub effects: Vec<WasiLegacyEffect>,
    pub broker_requests: Vec<BrokerRequest>,
    pub host_broker_requests: Vec<HostBrokerRequest>,
    pub events: Vec<WasiExtensionEvent>,
}

#[derive(Default)]
struct WasiStoreData {
    policy: CapabilityPolicy,
    requests: Vec<BrokerRequest>,
}

pub struct WasiExtension {
    pub manifest: WasiExtensionManifest,
    pub file_path: Option<PathBuf>,
    pub wasm_bytes: Vec<u8>,
    pub engine: Engine,
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

    pub fn load_from_bytes(wasm_bytes: Vec<u8>) -> Result<Self, String> {
        Self::load_from_bytes_inner(wasm_bytes, false)
    }

    fn load_from_bytes_inner(wasm_bytes: Vec<u8>, require_manifest: bool) -> Result<Self, String> {
        let engine = Engine::default();
        let module = Module::new(&engine, &wasm_bytes[..])
            .map_err(|e| format!("Failed to parse WASM module: {e}"))?;
        let mut store = Store::new(&engine, WasiStoreData::default());
        let linker = Self::create_linker(&engine, &mut store);
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| format!("Failed to instantiate WASM module: {e}"))?
            .start(&mut store)
            .map_err(|e| format!("Failed to start WASM module: {e}"))?;

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
        })
    }

    pub fn capability_policy(&self) -> CapabilityPolicy {
        if self.manifest.api_version < BROKER_API_VERSION {
            CapabilityPolicy::default()
        } else {
            CapabilityPolicy::new(self.manifest.capabilities.clone())
        }
    }

    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let bytes =
            fs::read(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        let mut ext = Self::load_from_bytes(bytes)?;
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

    fn call_with_policy(
        &self,
        export: &str,
        invocation: &WasiExtensionInvocation,
        policy: CapabilityPolicy,
    ) -> Result<WasiExtensionInvocationResult, String> {
        let module = Module::new(&self.engine, &self.wasm_bytes[..]).map_err(|e| e.to_string())?;
        let mut store = Store::new(
            &self.engine,
            WasiStoreData {
                policy,
                requests: vec![],
            },
        );
        let linker = Self::create_linker(&self.engine, &mut store);
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| e.to_string())?
            .start(&mut store)
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
            api_version: invocation.api_version,
            response,
            broker_requests: std::mem::take(&mut store.data_mut().requests),
            host_broker_requests: Vec::new(),
            events: invocation.events.clone(),
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

type PendingExtensionEvents = HashMap<Option<String>, HashMap<String, Vec<WasiExtensionEvent>>>;

#[derive(Default)]
pub struct WasiExtensionManager {
    extensions: RwLock<HashMap<String, Arc<WasiExtension>>>,
    states: Mutex<HashMap<String, Value>>,
    host_state: Mutex<HashMap<String, Value>>,
    subscriptions: Mutex<HashMap<String, HashSet<String>>>,
    pending_events: Mutex<PendingExtensionEvents>,
    pending_broker_requests: Mutex<HashMap<Option<String>, Vec<HostBrokerRequest>>>,
    capability_grant_policy: Mutex<HostCapabilityGrantPolicy>,
    state_dir: Option<PathBuf>,
    /// Stateful conversational extensions are isolated by the active session.
    /// `None` retains the project-wide scope for callers that explicitly need it.
    session_id: Mutex<Option<String>>,
}

impl WasiExtensionManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn extension_names(&self) -> Result<HashSet<String>, String> {
        self.extensions
            .read()
            .map(|extensions| extensions.keys().cloned().collect())
            .map_err(|_| "Extension registry lock poisoned".to_string())
    }

    pub fn extension_manifest(&self, name: &str) -> Option<WasiExtensionManifest> {
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

    pub fn register_extension(&self, extension: WasiExtension) -> Result<(), String> {
        self.extensions
            .write()
            .map_err(|_| "Extension registry lock poisoned".to_string())?
            .insert(extension.manifest.name.clone(), Arc::new(extension));
        Ok(())
    }

    pub fn reload_from_roots(
        &self,
        global_threadlane_dir: Option<&Path>,
        project_root: Option<&Path>,
    ) -> Result<usize, String> {
        let records = ExtensionManager::new(
            global_threadlane_dir.map(Path::to_path_buf),
            project_root.map(Path::to_path_buf),
        )
        .discover_checked()?;
        let mut loaded = HashMap::new();
        for record in records.into_iter().filter(|record| record.is_effective()) {
            let extension = WasiExtension::load_from_file(record.module_path())?;
            if extension.manifest.name != record.name() {
                return Err(format!(
                    "Extension manifest changed while reloading '{}'",
                    record.module_path().display()
                ));
            }
            loaded.insert(extension.manifest.name.clone(), Arc::new(extension));
        }
        let loaded_count = loaded.len();
        let persisted_states = loaded
            .keys()
            .filter_map(|name| self.load_state(name).map(|state| (name.clone(), state)))
            .collect::<Vec<_>>();
        let mut extensions = self
            .extensions
            .write()
            .map_err(|_| "Extension registry lock poisoned".to_string())?;
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
        Ok(loaded_count)
    }

    pub fn for_project(project_dir: &Path) -> Self {
        Self {
            state_dir: Some(project_dir.join(".threadlane/state/extensions")),
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
            session_id: Mutex::new(Some(session_id.into())),
            ..Self::default()
        }
    }

    /// Switches the active state scope and reloads every registered extension.
    /// Callers should serialize this with extension invocation.
    pub fn set_session_scope(&self, session_id: impl Into<String>) -> Result<(), String> {
        let session_id = session_id.into();
        let scope = Some(session_id);
        *self
            .session_id
            .lock()
            .map_err(|_| "Extension session lock poisoned".to_string())? = scope.clone();
        // Queued work is session-owned too: switching scope selects a separate
        // queue so one conversation cannot receive another's broker outcomes.
        self.pending_events
            .lock()
            .map_err(|_| "Extension event lock poisoned".to_string())?
            .entry(scope.clone())
            .or_default();
        self.pending_broker_requests
            .lock()
            .map_err(|_| "Extension broker request lock poisoned".to_string())?
            .entry(scope)
            .or_default();

        let extension_names = self.extension_names()?;
        let mut states = self
            .states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())?;
        states.clear();
        self.host_state
            .lock()
            .map_err(|_| "Host state lock poisoned".to_string())?
            .clear();
        for name in &extension_names {
            if let Some(state) = self.load_state(name) {
                states.insert(name.clone(), state);
            }
        }
        Ok(())
    }

    /// Returns the current persisted/in-memory state for an extension.
    pub fn extension_state(&self, extension_name: &str) -> Option<Value> {
        self.states.lock().ok()?.get(extension_name).cloned()
    }

    /// Updates only the state owned by the invoking extension.
    pub fn set_extension_state(&self, extension_name: &str, state: Value) -> Result<(), String> {
        self.states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())?
            .insert(extension_name.to_string(), state.clone());
        self.persist_state(extension_name, &state)
    }

    /// Returns host-owned state in the active session scope without relying on
    /// any extension identity or schema.
    pub fn host_state(&self, key: &str) -> Option<Value> {
        if let Ok(state) = self.host_state.lock() {
            if let Some(value) = state.get(key) {
                return Some(value.clone());
            }
        }
        let value = self.load_host_state(key)?;
        self.host_state
            .lock()
            .ok()?
            .insert(key.to_string(), value.clone());
        Some(value)
    }

    /// Persists host-owned state in the active session scope.
    pub fn set_host_state(&self, key: &str, value: Value) -> Result<(), String> {
        self.host_state
            .lock()
            .map_err(|_| "Host state lock poisoned".to_string())?
            .insert(key.to_string(), value.clone());
        self.persist_host_state(key, &value)
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
        let scope = self.session_scope()?;
        let subscribers = self
            .subscriptions
            .lock()
            .map_err(|_| "Extension subscription lock poisoned".to_string())?;
        let event = WasiExtensionEvent {
            topic: topic.clone(),
            payload,
        };
        let mut pending = self
            .pending_events
            .lock()
            .map_err(|_| "Extension event lock poisoned".to_string())?;
        let pending = pending.entry(scope).or_default();
        for (extension, topics) in subscribers.iter() {
            if topics.contains(&topic) {
                pending
                    .entry(extension.clone())
                    .or_default()
                    .push(event.clone());
            }
        }
        Ok(())
    }

    /// Removes events queued for one extension, matching the next-invocation delivery path.
    pub fn drain_events_for(
        &self,
        extension_name: &str,
    ) -> Result<Vec<WasiExtensionEvent>, String> {
        let scope = self.session_scope()?;
        Ok(self
            .pending_events
            .lock()
            .map_err(|_| "Extension event lock poisoned".to_string())?
            .entry(scope)
            .or_default()
            .remove(extension_name)
            .unwrap_or_default())
    }

    /// Location used for session-owned extension state. This is public so UI
    /// code can render persisted state without duplicating the naming scheme.
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

    fn load_state(&self, extension_name: &str) -> Option<Value> {
        let path = self.state_path(extension_name)?;
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }

    fn persist_state(&self, extension_name: &str, state: &Value) -> Result<(), String> {
        let Some(path) = self.state_path(extension_name) else {
            return Ok(());
        };
        persist_json_state(&path, state)
    }

    fn host_state_path(&self, key: &str) -> Option<PathBuf> {
        let directory = self.state_dir.as_ref()?;
        let session_id = self.session_id.lock().ok()?.clone();
        Some(match session_id {
            Some(session_id) => directory
                .join("sessions")
                .join(encode_state_component(&session_id))
                .join(format!(".host.{key}.json")),
            None => directory.join(format!(".host.{key}.json")),
        })
    }

    fn load_host_state(&self, key: &str) -> Option<Value> {
        let path = self.host_state_path(key)?;
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }

    fn persist_host_state(&self, key: &str, value: &Value) -> Result<(), String> {
        let Some(path) = self.host_state_path(key) else {
            return Ok(());
        };
        persist_json_state(&path, value)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn has_command(&self, name: &str) -> bool {
        self.extensions.read().is_ok_and(|extensions| {
            extensions.values().any(|extension| {
                extension
                    .manifest
                    .commands
                    .iter()
                    .any(|command| command.name == name)
            })
        })
    }

    pub fn execute_tool_with_broker_requests(
        &self,
        name: &str,
        args: &str,
    ) -> Option<Result<WasiExtensionInvocationResult, String>> {
        self.execute_response("tool", name, args)
    }

    pub fn execute_command_with_effects(
        &self,
        name: &str,
        args: &str,
    ) -> Option<Result<WasiExtensionCommandResult, String>> {
        self.execute_response("command", name, args).map(|result| {
            result.map(|mut result| {
                let broker_requests = self.take_broker_requests(&mut result);
                let host_broker_requests = self.filter_granted_requests(
                    broker_requests
                        .iter()
                        .cloned()
                        .map(|request| HostBrokerRequest {
                            request,
                            invoking_extension: result.invoking_extension.clone(),
                        })
                        .collect(),
                );
                WasiExtensionCommandResult {
                    api_version: result.api_version,
                    message: result.response.message.unwrap_or_default(),
                    effects: result.response.effects,
                    broker_requests,
                    host_broker_requests,
                    events: result.events,
                }
            })
        })
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

    /// Queues broker outcomes for delivery in a later invocation.
    pub fn enqueue_broker_results(&self, results: Vec<BrokerOperationResult>) {
        let Ok(scope) = self.session_scope() else {
            return;
        };
        if let Ok(mut pending) = self.pending_events.lock() {
            let pending = pending.entry(scope).or_default();
            for result in results {
                let payload = match result.error {
                    Some(error) => serde_json::json!({
                        "api_version": BROKER_API_VERSION,
                        "capability": result.request.capability,
                        "operation": result.request.operation,
                        "arguments": result.request.arguments,
                        "ok": false,
                        "error": {"code": error.code, "message": error.message},
                    }),
                    None => serde_json::json!({
                        "api_version": BROKER_API_VERSION,
                        "capability": result.request.capability,
                        "operation": result.request.operation,
                        "arguments": result.request.arguments,
                        "ok": true,
                        "value": result.value,
                    }),
                };
                pending
                    .entry(result.invoking_extension)
                    .or_default()
                    .push(WasiExtensionEvent {
                        topic: "broker_response".into(),
                        payload,
                    });
            }
        }
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

    fn find_response_extension(&self, kind: &str, name: &str) -> Option<Arc<WasiExtension>> {
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

    fn take_broker_requests(
        &self,
        result: &mut WasiExtensionInvocationResult,
    ) -> Vec<BrokerRequest> {
        std::mem::take(&mut result.broker_requests)
    }

    fn invoke(
        &self,
        extension: &WasiExtension,
        kind: &str,
        name: &str,
        args: &str,
    ) -> Result<WasiExtensionInvocationResult, String> {
        let state = self
            .states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())?
            .get(&extension.manifest.name)
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let arguments =
            serde_json::from_str(args).unwrap_or_else(|_| serde_json::json!({ "raw": args }));
        let events = self.drain_events_for(&extension.manifest.name)?;
        let invocation = WasiExtensionInvocation {
            api_version: extension.manifest.api_version,
            kind: kind.into(),
            name: name.into(),
            arguments,
            state,
            events,
        };
        let policy = self.effective_capability_policy(extension)?;
        let result = match kind {
            "tool" => extension.call_with_policy("execute_tool", &invocation, policy),
            "hook" => extension.call_with_policy("handle_hook", &invocation, policy),
            _ => extension.call_with_policy("execute_command", &invocation, policy),
        }?;
        if let Some(state) = result.response.state.clone() {
            self.states
                .lock()
                .map_err(|_| "Extension state lock poisoned".to_string())?
                .insert(extension.manifest.name.clone(), state.clone());
            self.persist_state(&extension.manifest.name, &state)?;
        }
        let mut result = result;
        result.invoking_extension = extension.manifest.name.clone();
        result.host_broker_requests = self.filter_granted_requests(
            result
                .broker_requests
                .iter()
                .cloned()
                .map(|request| HostBrokerRequest {
                    request,
                    invoking_extension: result.invoking_extension.clone(),
                })
                .collect(),
        );
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

fn persist_json_state(path: &Path, state: &Value) -> Result<(), String> {
    let parent = path.parent().ok_or("Extension state path has no parent")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?;
    fs::write(path, bytes).map_err(|error| error.to_string())
}

impl WasiExtensionManager {
    pub fn tool_definitions(&self) -> Vec<threadlane_agent::AgentToolDefinition> {
        let Ok(extensions) = self.extensions.read() else {
            return Vec::new();
        };
        let mut tools: Vec<_> = extensions
            .values()
            .flat_map(|extension| extension.manifest.tools.iter())
            .map(|tool| {
                threadlane_agent::AgentToolDefinition::new(
                    tool.name.clone(),
                    tool.description.clone(),
                    tool.parameters.clone(),
                )
            })
            .collect();
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        tools
    }
}

#[cfg(test)]
mod reload_tests {
    use super::*;
    use tempfile::tempdir;

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
