use serde::{Deserialize, Serialize};

const MAX_SUBAGENT_TASKS: usize = 8;
const MAX_AGENT_CHARS: usize = 128;
const MAX_TASK_CHARS: usize = 32_000;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct WasiToolDefinition {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct WasiCommandDefinition {
    name: String,
    description: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct WasiExtensionManifest {
    api_version: u32,
    name: String,
    version: String,
    description: String,
    capabilities: Vec<String>,
    tools: Vec<WasiToolDefinition>,
    commands: Vec<WasiCommandDefinition>,
    hooks: Vec<String>,
}

#[derive(Deserialize)]
struct Invocation {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

#[derive(Debug, Serialize, PartialEq)]
struct BrokerRequest {
    api_version: u32,
    capability: String,
    operation: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Serialize, PartialEq)]
struct Response {
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Response {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error: None,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            message: message.clone(),
            error: Some(message),
        }
    }
}

#[derive(Debug, Serialize, PartialEq)]
struct SubagentTask {
    agent: String,
    task: String,
}

#[derive(Debug, PartialEq)]
struct NormalizedRun {
    tasks: Vec<SubagentTask>,
    parallel: bool,
}

#[derive(Clone, Copy)]
enum InvocationRoute {
    Command,
    Tool,
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
    write_output(&extension_manifest())
}

fn extension_manifest() -> WasiExtensionManifest {
    WasiExtensionManifest {
        api_version: 2,
        name: "subagent_ext".into(),
        version: "0.3.0".into(),
        description: "Subagent task delegation WASI extension".into(),
        capabilities: vec!["agent".into()],
        tools: vec![WasiToolDefinition {
            name: "subagent".into(),
            description: "Delegate one or more tasks to specialized subagents. Set parallel to true for independent tasks or false for a sequential chain; later sequential tasks may reference {previous}.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "description": "Ordered subagent tasks to run.",
                        "minItems": 1,
                        "maxItems": 8,
                        "items": {
                            "type": "object",
                            "properties": {
                                "agent": {
                                    "type": "string",
                                    "minLength": 1,
                                    "maxLength": 128,
                                    "description": "Subagent preset name, for example scout, planner, reviewer, or worker."
                                },
                                "task": {
                                    "type": "string",
                                    "minLength": 1,
                                    "maxLength": 32000,
                                    "description": "Task prompt. In sequential mode, {previous} is replaced with the prior result."
                                }
                            },
                            "required": ["agent", "task"],
                            "additionalProperties": false
                        }
                    },
                    "parallel": {
                        "type": "boolean",
                        "description": "Run tasks concurrently when true, or sequentially in array order when false."
                    }
                },
                "required": ["tasks", "parallel"],
                "additionalProperties": false
            }),
        }],
        commands: vec![WasiCommandDefinition {
            name: "subagent".into(),
            description: "Delegate tasks to specialized subagents with isolated context windows. Modes: single (agent + task), parallel (tasks array), chain/sequential (with {previous} placeholder).".into(),
        }],
        hooks: vec![],
    }
}

#[no_mangle]
pub extern "C" fn execute_command(ptr: i32, len: i32) -> u64 {
    let invocation = parse_invocation(ptr, len);
    let response = match invocation.name.as_str() {
        "subagent" => handle_subagent_invocation(&invocation.arguments, InvocationRoute::Command),
        other => Response::error(format!("Unknown WASI subagent command: {other}")),
    };

    write_output(&response)
}

#[no_mangle]
pub extern "C" fn execute_tool(ptr: i32, len: i32) -> u64 {
    let invocation = parse_invocation(ptr, len);
    let response = match invocation.name.as_str() {
        "subagent" => handle_subagent_invocation(&invocation.arguments, InvocationRoute::Tool),
        other => Response::error(format!("Unknown WASI subagent tool: {other}")),
    };

    write_output(&response)
}

#[no_mangle]
pub extern "C" fn handle_hook(ptr: i32, len: i32) -> u64 {
    let _invocation = parse_invocation(ptr, len);
    write_output(&Response::ok(String::new()))
}

fn parse_invocation(ptr: i32, len: i32) -> Invocation {
    let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    serde_json::from_slice(input).unwrap_or(Invocation {
        name: String::new(),
        arguments: serde_json::Value::Null,
    })
}

fn handle_subagent_invocation(args: &serde_json::Value, route: InvocationRoute) -> Response {
    let run = match normalize_subagent_args(args, route) {
        Ok(run) => run,
        Err(()) => {
            return Response::error("Usage: /subagent <task>, or /subagent {\"agent\":\"scout\",\"task\":\"...\"}. Use `tasks` for parallel work or `chain`/`sequential` for sequential work.");
        }
    };

    let count = run.tasks.len();
    request_broker(&agent_run_request(&run));
    Response::ok(format!(
        "Running {} subagent task{}…",
        count,
        if count == 1 { "" } else { "s" }
    ))
}

fn normalize_subagent_args(
    args: &serde_json::Value,
    route: InvocationRoute,
) -> Result<NormalizedRun, ()> {
    match route {
        InvocationRoute::Tool => normalize_canonical_args(args),
        InvocationRoute::Command => {
            if let Some(raw) = args.get("raw").and_then(serde_json::Value::as_str) {
                let raw = raw.trim();
                if raw.is_empty() {
                    return Err(());
                }
                match serde_json::from_str(raw) {
                    Ok(value) => normalize_command_args(&value),
                    Err(_) => Ok(single_task("scout", raw)),
                }
            } else {
                normalize_command_args(args)
            }
        }
    }
}

fn normalize_canonical_args(args: &serde_json::Value) -> Result<NormalizedRun, ()> {
    let tasks = args
        .get("tasks")
        .and_then(serde_json::Value::as_array)
        .ok_or(())?;
    let parallel = args
        .get("parallel")
        .and_then(serde_json::Value::as_bool)
        .ok_or(())?;
    Ok(NormalizedRun {
        tasks: parse_tasks(tasks)?,
        parallel,
    })
}

fn normalize_command_args(args: &serde_json::Value) -> Result<NormalizedRun, ()> {
    if let Some(tasks) = args.get("tasks") {
        let tasks = tasks.as_array().ok_or(())?;
        let parallel = match args.get("parallel") {
            Some(value) => value.as_bool().ok_or(())?,
            None => true,
        };
        return Ok(NormalizedRun {
            tasks: parse_tasks(tasks)?,
            parallel,
        });
    }

    for alias in ["chain", "sequential"] {
        if let Some(tasks) = args.get(alias) {
            return Ok(NormalizedRun {
                tasks: parse_tasks(tasks.as_array().ok_or(())?)?,
                parallel: false,
            });
        }
    }

    let task = args
        .get("task")
        .and_then(serde_json::Value::as_str)
        .ok_or(())?
        .trim();
    let agent = args
        .get("agent")
        .map(|value| value.as_str().ok_or(()))
        .transpose()?
        .unwrap_or("scout")
        .trim();
    if agent.is_empty()
        || task.is_empty()
        || agent.chars().count() > MAX_AGENT_CHARS
        || task.chars().count() > MAX_TASK_CHARS
    {
        return Err(());
    }
    Ok(single_task(agent, task))
}

fn single_task(agent: &str, task: &str) -> NormalizedRun {
    NormalizedRun {
        tasks: vec![SubagentTask {
            agent: agent.into(),
            task: task.into(),
        }],
        parallel: false,
    }
}

fn parse_tasks(values: &[serde_json::Value]) -> Result<Vec<SubagentTask>, ()> {
    if values.is_empty() || values.len() > MAX_SUBAGENT_TASKS {
        return Err(());
    }
    values
        .iter()
        .map(|value| {
            let agent = value
                .get("agent")
                .and_then(serde_json::Value::as_str)
                .ok_or(())?
                .trim();
            let task = value
                .get("task")
                .and_then(serde_json::Value::as_str)
                .ok_or(())?
                .trim();
            if agent.is_empty()
                || task.is_empty()
                || agent.chars().count() > MAX_AGENT_CHARS
                || task.chars().count() > MAX_TASK_CHARS
            {
                return Err(());
            }
            Ok(SubagentTask {
                agent: agent.into(),
                task: task.into(),
            })
        })
        .collect()
}

fn agent_run_request(run: &NormalizedRun) -> BrokerRequest {
    BrokerRequest {
        api_version: 2,
        capability: "agent".into(),
        operation: "run".into(),
        arguments: serde_json::json!({
            "tasks": run.tasks,
            "parallel": run.parallel,
        }),
    }
}

fn request_broker(request: &BrokerRequest) {
    #[cfg(target_arch = "wasm32")]
    {
        let request = serde_json::to_vec(request).expect("broker request must serialize");
        let request_ptr = alloc(request.len() as i32);
        let response_ptr = alloc(4096);
        unsafe {
            std::ptr::copy_nonoverlapping(request.as_ptr(), request_ptr as *mut u8, request.len());
            let _ = broker_request(request_ptr, request.len() as i32, response_ptr, 4096);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = request;
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

    fn task(agent: &str, prompt: &str) -> SubagentTask {
        SubagentTask {
            agent: agent.into(),
            task: prompt.into(),
        }
    }

    #[test]
    fn manifest_declares_provider_compatible_subagent_tool() {
        let manifest = extension_manifest();
        assert_eq!(manifest.api_version, 2);
        assert_eq!(manifest.capabilities, vec!["agent"]);
        assert_eq!(manifest.tools.len(), 1);

        let tool = &manifest.tools[0];
        assert_eq!(tool.name, "subagent");
        assert_eq!(tool.parameters["type"], "object");
        assert_eq!(
            tool.parameters["required"],
            serde_json::json!(["tasks", "parallel"])
        );
        assert_eq!(tool.parameters["properties"]["tasks"]["type"], "array");
        assert_eq!(tool.parameters["properties"]["parallel"]["type"], "boolean");
        assert_eq!(
            tool.parameters["properties"]["tasks"]["items"]["required"],
            serde_json::json!(["agent", "task"])
        );
    }

    #[test]
    fn slash_plain_text_defaults_to_single_scout_task() {
        let run = normalize_subagent_args(
            &serde_json::json!({"raw": "  find auth files  "}),
            InvocationRoute::Command,
        )
        .unwrap();
        assert_eq!(
            run,
            NormalizedRun {
                tasks: vec![task("scout", "find auth files")],
                parallel: false,
            }
        );
    }

    #[test]
    fn slash_single_json_defaults_agent_to_scout() {
        let run = normalize_subagent_args(
            &serde_json::json!({"raw": r#"{"task":"find auth files"}"#}),
            InvocationRoute::Command,
        )
        .unwrap();
        assert_eq!(run.tasks, vec![task("scout", "find auth files")]);
        assert!(!run.parallel);
    }

    #[test]
    fn slash_tasks_are_parallel_by_default_and_can_use_canonical_flag() {
        let tasks = serde_json::json!([
            {"agent": "scout", "task": "find auth files"},
            {"agent": "reviewer", "task": "inspect auth risks"}
        ]);
        let default_parallel = normalize_subagent_args(
            &serde_json::json!({"tasks": tasks.clone()}),
            InvocationRoute::Command,
        )
        .unwrap();
        assert!(default_parallel.parallel);

        let explicit_sequential = normalize_subagent_args(
            &serde_json::json!({"tasks": tasks, "parallel": false}),
            InvocationRoute::Command,
        )
        .unwrap();
        assert!(!explicit_sequential.parallel);
        assert_eq!(explicit_sequential.tasks.len(), 2);
    }

    #[test]
    fn slash_chain_aliases_preserve_previous_placeholder() {
        for alias in ["chain", "sequential"] {
            let run = normalize_subagent_args(
                &serde_json::json!({
                    alias: [
                        {"agent": "scout", "task": "find auth files"},
                        {"agent": "planner", "task": "plan from {previous}"}
                    ]
                }),
                InvocationRoute::Command,
            )
            .unwrap();
            assert!(!run.parallel);
            assert_eq!(run.tasks[1], task("planner", "plan from {previous}"));
        }
    }

    #[test]
    fn tool_uses_canonical_tasks_and_parallel_arguments() {
        let run = normalize_subagent_args(
            &serde_json::json!({
                "tasks": [
                    {"agent": "scout", "task": "find auth files"},
                    {"agent": "reviewer", "task": "review {previous}"}
                ],
                "parallel": false
            }),
            InvocationRoute::Tool,
        )
        .unwrap();
        let request = agent_run_request(&run);

        assert_eq!(request.api_version, 2);
        assert_eq!(request.capability, "agent");
        assert_eq!(request.operation, "run");
        assert_eq!(request.arguments["parallel"], false);
        assert_eq!(request.arguments["tasks"][1]["task"], "review {previous}");
    }

    #[test]
    fn command_and_tool_canonical_inputs_produce_the_same_request() {
        let args = serde_json::json!({
            "tasks": [{"agent": "worker", "task": "implement the fix"}],
            "parallel": true
        });
        let command = normalize_subagent_args(&args, InvocationRoute::Command).unwrap();
        let tool = normalize_subagent_args(&args, InvocationRoute::Tool).unwrap();
        assert_eq!(agent_run_request(&command), agent_run_request(&tool));
    }

    #[test]
    fn empty_and_malformed_inputs_are_rejected() {
        let malformed_cases = [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"tasks": [], "parallel": true}),
            serde_json::json!({"tasks": [{"agent": "scout"}], "parallel": true}),
            serde_json::json!({"tasks": [{"agent": "", "task": "work"}], "parallel": true}),
            serde_json::json!({"tasks": [{"agent": "scout", "task": "work"}]}),
            serde_json::json!({"tasks": [{"agent": "scout", "task": "work"}], "parallel": "yes"}),
        ];
        for args in malformed_cases {
            assert!(normalize_subagent_args(&args, InvocationRoute::Tool).is_err());
        }

        assert!(normalize_subagent_args(
            &serde_json::json!({"raw": "   "}),
            InvocationRoute::Command
        )
        .is_err());
        assert!(normalize_subagent_args(
            &serde_json::json!({"chain": []}),
            InvocationRoute::Command
        )
        .is_err());
    }

    #[test]
    fn invalid_input_returns_compatible_usage_response() {
        let response = handle_subagent_invocation(
            &serde_json::json!({"tasks": [], "parallel": true}),
            InvocationRoute::Tool,
        );
        assert!(response.message.starts_with("Usage: /subagent"));
    }
}
