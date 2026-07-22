use crate::extension_broker::{CapabilityPolicy, BROKER_API_VERSION};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use wasmi::{Engine, Func, Linker, Module, Store};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasiExtensionInvocation {
    pub api_version: u32,
    pub kind: String,
    pub name: String,
    pub arguments: Value,
    #[serde(default)]
    pub state: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WasiExtensionEffect {
    SetToolPolicy {
        policy: String,
    },
    RequestModelTurn {
        prompt: String,
    },
    RunSubagents {
        tasks: Vec<WasiSubagentTask>,
        #[serde(default)]
        parallel: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasiSubagentTask {
    pub agent: String,
    pub task: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WasiExtensionResponse {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub state: Option<Value>,
    #[serde(default)]
    pub effects: Vec<WasiExtensionEffect>,
}

#[derive(Debug, Clone, Default)]
pub struct WasiExtensionCommandResult {
    pub message: String,
    pub effects: Vec<WasiExtensionEffect>,
}

pub struct WasiExtension {
    pub manifest: WasiExtensionManifest,
    pub file_path: Option<PathBuf>,
    wasm_bytes: Vec<u8>,
    engine: Engine,
}

impl WasiExtension {
    fn create_linker(engine: &Engine, store: &mut Store<()>) -> Linker<()> {
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
        linker
    }

    pub fn load_from_bytes(wasm_bytes: Vec<u8>) -> Result<Self, String> {
        let engine = Engine::default();
        let module = Module::new(&engine, &wasm_bytes[..])
            .map_err(|e| format!("Failed to parse WASM module: {e}"))?;
        let mut store = Store::new(&engine, ());
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

    pub fn call_tool(
        &self,
        invocation: &WasiExtensionInvocation,
    ) -> Result<WasiExtensionResponse, String> {
        self.call("execute_tool", invocation)
    }

    pub fn call_command(
        &self,
        invocation: &WasiExtensionInvocation,
    ) -> Result<WasiExtensionResponse, String> {
        self.call("execute_command", invocation)
    }

    pub fn call_hook(
        &self,
        invocation: &WasiExtensionInvocation,
    ) -> Result<WasiExtensionResponse, String> {
        self.call("handle_hook", invocation)
    }

    fn call(
        &self,
        export: &str,
        invocation: &WasiExtensionInvocation,
    ) -> Result<WasiExtensionResponse, String> {
        let module = Module::new(&self.engine, &self.wasm_bytes[..]).map_err(|e| e.to_string())?;
        let mut store = Store::new(&self.engine, ());
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
        read_json_result(&mut store, &instance, result)
    }
}

fn read_json_result<T: for<'de> Deserialize<'de>>(
    store: &mut Store<()>,
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

/// Produces a filesystem-safe, collision-free directory name for a session ID.
fn encode_state_component(component: &str) -> String {
    component
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Default)]
pub struct WasiExtensionManager {
    pub extensions: HashMap<String, WasiExtension>,
    states: Mutex<HashMap<String, Value>>,
    state_dir: Option<PathBuf>,
    /// Stateful conversational extensions are isolated by the active session.
    /// `None` retains the project-wide scope for callers that explicitly need it.
    session_id: Mutex<Option<String>>,
}

impl WasiExtensionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_extensions(&self) -> &HashMap<String, WasiExtension> {
        &self.extensions
    }

    pub fn for_project(project_dir: &Path) -> Self {
        Self {
            state_dir: Some(project_dir.join(".mypi/state/extensions")),
            ..Self::default()
        }
    }

    /// Creates a manager whose extension state belongs to one conversation.
    pub fn for_project_session(project_dir: &Path, session_id: impl Into<String>) -> Self {
        Self {
            state_dir: Some(project_dir.join(".mypi/state/extensions")),
            session_id: Mutex::new(Some(session_id.into())),
            ..Self::default()
        }
    }

    /// Switches the active state scope and reloads every registered extension.
    /// Callers should serialize this with extension invocation.
    pub fn set_session_scope(&self, session_id: impl Into<String>) -> Result<(), String> {
        let session_id = session_id.into();
        *self
            .session_id
            .lock()
            .map_err(|_| "Extension session lock poisoned".to_string())? = Some(session_id);

        let mut states = self
            .states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())?;
        states.clear();
        for name in self.extensions.keys() {
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

    /// Location used for session-owned extension state. This is public so UI
    /// code can render persisted state without duplicating the naming scheme.
    pub fn session_state_path(
        project_dir: &Path,
        session_id: &str,
        extension_name: &str,
    ) -> PathBuf {
        project_dir
            .join(".mypi/state/extensions/sessions")
            .join(encode_state_component(session_id))
            .join(format!("{extension_name}.json"))
    }

    pub fn discover_and_load(&mut self, dir: &Path) -> usize {
        let mut loaded = 0;
        for directory in [
            dir.join(".mypi/extensions"),
            dir.to_path_buf(),
            dir.join("extensions"),
        ] {
            let Ok(entries) = fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let wasm_path = if path.is_dir() {
                    path.join("extension.wasm")
                } else {
                    path
                };
                if wasm_path.extension().is_some_and(|ext| ext == "wasm")
                    && self.load_and_register(&wasm_path)
                {
                    loaded += 1;
                }
            }
        }
        loaded
    }

    fn load_and_register(&mut self, path: &Path) -> bool {
        let Ok(extension) = WasiExtension::load_from_file(path) else {
            return false;
        };
        let name = extension.manifest.name.clone();
        if let Some(state) = self.load_state(&name) {
            if let Ok(mut states) = self.states.lock() {
                states.insert(name.clone(), state);
            }
        }
        self.extensions.insert(name, extension);
        true
    }

    fn state_path(&self, extension_name: &str) -> Option<PathBuf> {
        let directory = self.state_dir.as_ref()?;
        let session_id = self.session_id.lock().ok()?.clone();
        Some(match session_id {
            Some(session_id) => {
                Self::session_state_path_from_dir(directory, &session_id, extension_name)
            }
            None => directory.join(format!("{extension_name}.json")),
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
            .join(format!("{extension_name}.json"))
    }

    fn load_state(&self, extension_name: &str) -> Option<Value> {
        let path = self.state_path(extension_name)?;
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }

    fn persist_state(&self, extension_name: &str, state: &Value) -> Result<(), String> {
        let Some(path) = self.state_path(extension_name) else {
            return Ok(());
        };
        let parent = path.parent().ok_or("Extension state path has no parent")?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?;
        fs::write(path, bytes).map_err(|error| error.to_string())
    }

    pub fn has_command(&self, name: &str) -> bool {
        self.extensions.values().any(|extension| {
            extension
                .manifest
                .commands
                .iter()
                .any(|command| command.name == name)
        })
    }

    pub fn get_tools(&self) -> Vec<Value> {
        self.extensions.values().flat_map(|extension| extension.manifest.tools.iter()).map(|tool| {
            serde_json::json!({ "type": "function", "function": {
                "name": tool.name, "description": tool.description, "parameters": tool.parameters
            }})
        }).collect()
    }

    pub fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        self.execute("tool", name, args)
    }

    pub fn execute_command(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        self.execute_command_with_effects(name, args)
            .map(|result| result.map(|result| result.message))
    }

    pub fn execute_command_with_effects(
        &self,
        name: &str,
        args: &str,
    ) -> Option<Result<WasiExtensionCommandResult, String>> {
        self.execute_response("command", name, args).map(|result| {
            result.map(|response| WasiExtensionCommandResult {
                message: response.message.unwrap_or_default(),
                effects: response.effects,
            })
        })
    }

    fn execute(&self, kind: &str, name: &str, args: &str) -> Option<Result<String, String>> {
        self.execute_response(kind, name, args)
            .map(|result| result.map(|response| response.message.unwrap_or_default()))
    }

    pub fn execute_hook(
        &self,
        name: &str,
        args: &str,
    ) -> Vec<Result<WasiExtensionResponse, String>> {
        self.extensions
            .values()
            .filter(|extension| extension.manifest.hooks.iter().any(|hook| hook == name))
            .map(|extension| self.invoke(extension, "hook", name, args))
            .collect()
    }

    fn execute_response(
        &self,
        kind: &str,
        name: &str,
        args: &str,
    ) -> Option<Result<WasiExtensionResponse, String>> {
        let extension = self.extensions.values().find(|extension| {
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
        })?;
        Some(self.invoke(extension, kind, name, args))
    }

    fn invoke(
        &self,
        extension: &WasiExtension,
        kind: &str,
        name: &str,
        args: &str,
    ) -> Result<WasiExtensionResponse, String> {
        let state = self
            .states
            .lock()
            .map_err(|_| "Extension state lock poisoned".to_string())?
            .get(&extension.manifest.name)
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let arguments =
            serde_json::from_str(args).unwrap_or_else(|_| serde_json::json!({ "raw": args }));
        let invocation = WasiExtensionInvocation {
            api_version: 1,
            kind: kind.into(),
            name: name.into(),
            arguments,
            state,
        };
        let response = match kind {
            "tool" => extension.call_tool(&invocation),
            "hook" => extension.call_hook(&invocation),
            _ => extension.call_command(&invocation),
        }?;
        if let Some(state) = response.state.clone() {
            self.states
                .lock()
                .map_err(|_| "Extension state lock poisoned".to_string())?
                .insert(extension.manifest.name.clone(), state.clone());
            self.persist_state(&extension.manifest.name, &state)?;
        }
        Ok(response)
    }
}

impl mypi_agent::ToolExecutor for WasiExtensionManager {
    fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        self.execute_tool(name, args)
    }
}
