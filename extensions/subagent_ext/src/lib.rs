use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct WasiCommandDefinition {
    name: String,
    description: String,
}

#[derive(Serialize, Deserialize)]
struct WasiExtensionManifest {
    api_version: u32,
    name: String,
    version: String,
    description: String,
    capabilities: Vec<String>,
    commands: Vec<WasiCommandDefinition>,
    hooks: Vec<String>,
}

#[derive(Deserialize)]
struct Invocation {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

#[derive(Serialize)]
struct BrokerRequest {
    api_version: u32,
    capability: String,
    operation: String,
    arguments: serde_json::Value,
}

#[derive(Serialize)]
struct Response {
    message: String,
}

#[derive(Serialize)]
struct SubagentTask {
    agent: String,
    task: String,
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "mypi_host")]
extern "C" {
    #[link_name = "request"]
    fn broker_request(
        request_ptr: i32,
        request_len: i32,
        response_ptr: i32,
        response_capacity: i32,
    ) -> i32;
}

static mut OUTPUT_BUF: Vec<u8> = Vec::new();

#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    let mut buf = vec![0u8; size as usize];
    let ptr = buf.as_mut_ptr() as i32;
    std::mem::forget(buf);
    ptr
}

#[no_mangle]
pub extern "C" fn extension_info() -> u64 {
    write_output(&WasiExtensionManifest {
        api_version: 2,
        name: "subagent_ext".into(),
        version: "0.2.0".into(),
        description: "Subagent task delegation WASI extension".into(),
        capabilities: vec!["agent".into()],
        commands: vec![WasiCommandDefinition {
            name: "subagent".into(),
            description: "Delegate tasks to specialized subagents with isolated context windows. Modes: single (agent + task), parallel (tasks array), chain (sequential with {previous} placeholder).".into(),
        }],
        hooks: vec![],
    })
}

#[no_mangle]
pub extern "C" fn execute_command(ptr: i32, len: i32) -> u64 {
    let invocation = parse_invocation(ptr, len);
    let response = match invocation.name.as_str() {
        "subagent" => handle_subagent_invocation(&invocation.arguments),
        other => Response {
            message: format!("Unknown WASI subagent command: {other}"),
        },
    };

    write_output(&response)
}

#[no_mangle]
pub extern "C" fn handle_hook(ptr: i32, len: i32) -> u64 {
    let _invocation = parse_invocation(ptr, len);
    write_output(&Response {
        message: String::new(),
    })
}

fn parse_invocation(ptr: i32, len: i32) -> Invocation {
    let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    serde_json::from_slice(input).unwrap_or(Invocation {
        name: String::new(),
        arguments: serde_json::Value::Null,
    })
}

fn handle_subagent_invocation(args: &serde_json::Value) -> Response {
    // Slash commands arrive as {"raw": "..."}; JSON remains available for
    // multi-agent workflows documented by this extension.
    let args = match args.get("raw").and_then(|value| value.as_str()) {
        Some(raw) => serde_json::from_str(raw)
            .unwrap_or_else(|_| serde_json::json!({ "agent": "scout", "task": raw })),
        None => args.clone(),
    };

    let (tasks, parallel) =
        if let Some(tasks) = args.get("tasks").and_then(|value| value.as_array()) {
            (parse_tasks(tasks), true)
        } else if let Some(chain) = args.get("chain").and_then(|value| value.as_array()) {
            (parse_tasks(chain), false)
        } else {
            let task = args
                .get("task")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim();
            let agent = args
                .get("agent")
                .and_then(|value| value.as_str())
                .unwrap_or("scout");
            (
                if task.is_empty() {
                    vec![]
                } else {
                    vec![SubagentTask {
                        agent: agent.into(),
                        task: task.into(),
                    }]
                },
                false,
            )
        };

    if tasks.is_empty() {
        return Response {
            message: "Usage: /subagent <task>, or /subagent {\"agent\":\"scout\",\"task\":\"...\"}. Use `tasks` for parallel work or `chain` for sequential work.".into(),
        };
    }

    let count = tasks.len();
    request_broker(
        "agent",
        "run",
        serde_json::json!({"tasks": tasks, "parallel": parallel}),
    );
    Response {
        message: format!(
            "Running {} subagent task{}…",
            count,
            if count == 1 { "" } else { "s" }
        ),
    }
}

fn request_broker(capability: &str, operation: &str, arguments: serde_json::Value) {
    let request = serde_json::to_vec(&BrokerRequest {
        api_version: 2,
        capability: capability.into(),
        operation: operation.into(),
        arguments,
    })
    .expect("broker request must serialize");
    let request_ptr = alloc(request.len() as i32);
    let response_ptr = alloc(4096);
    #[cfg(target_arch = "wasm32")]
    unsafe {
        std::ptr::copy_nonoverlapping(request.as_ptr(), request_ptr as *mut u8, request.len());
        let _ = broker_request(request_ptr, request.len() as i32, response_ptr, 4096);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (request_ptr, response_ptr);
}

fn parse_tasks(values: &[serde_json::Value]) -> Vec<SubagentTask> {
    values
        .iter()
        .filter_map(|value| {
            let agent = value.get("agent")?.as_str()?.trim();
            let task = value.get("task")?.as_str()?.trim();
            (!agent.is_empty() && !task.is_empty()).then(|| SubagentTask {
                agent: agent.into(),
                task: task.into(),
            })
        })
        .collect()
}

fn write_output<T: Serialize>(value: &T) -> u64 {
    let bytes = serde_json::to_vec(value).expect("extension response must serialize");
    let len = bytes.len() as u64;
    unsafe {
        OUTPUT_BUF = bytes;
        let ptr = OUTPUT_BUF.as_ptr() as u64;
        (ptr << 32) | (len & 0xFFFF_FFFF)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_subagent_invocation() {
        let args = serde_json::json!({
            "agent": "scout",
            "task": "find auth files"
        });
        let res = handle_subagent_invocation(&args);
        assert!(res.message.contains("Running 1 subagent task"));
    }

    #[test]
    fn test_plain_slash_command_delegates_to_scout() {
        let args = serde_json::json!({ "raw": "find auth files" });
        let res = handle_subagent_invocation(&args);
        assert!(res.message.contains("Running 1 subagent task"));
    }
}
