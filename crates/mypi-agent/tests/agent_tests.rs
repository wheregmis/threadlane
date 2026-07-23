use mypi_agent::{
    compact_messages, AfterToolCallHook, AfterToolCallResult, Agent, AgentLoop, AgentMessage,
    AgentState, AgentToolCall, AgentToolDefinition, AgentToolResult, BeforeToolCallHook,
    BeforeToolCallResult, CompactionOptions, ReasoningEffort, SessionTree, ToolExecutionMode,
    ToolExecutor,
};
use mypi_provider::openai::{ToolCall, ToolCallFunction};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::Notify;

struct TestBeforeHook;

#[async_trait::async_trait]
impl BeforeToolCallHook for TestBeforeHook {
    async fn before_tool_call(
        &self,
        tool_call: &AgentToolCall,
        _state: &AgentState,
    ) -> BeforeToolCallResult {
        if tool_call.name == "forbidden_tool" {
            BeforeToolCallResult {
                block: true,
                reason: Some("Tool forbidden by policy".to_string()),
            }
        } else {
            BeforeToolCallResult::default()
        }
    }
}

struct TestAfterHook;

#[async_trait::async_trait]
impl AfterToolCallHook for TestAfterHook {
    async fn after_tool_call(
        &self,
        _tool_call: &AgentToolCall,
        result: &AgentToolResult,
        _state: &AgentState,
    ) -> AfterToolCallResult {
        if result.name == "exit_tool" {
            AfterToolCallResult {
                terminate: Some(true),
                ..Default::default()
            }
        } else {
            AfterToolCallResult::default()
        }
    }
}

#[tokio::test]
async fn test_agent_creation_and_events() {
    let mut agent = Agent::new("fake_key", None, "gpt-4o");
    agent.set_tool_execution_mode(ToolExecutionMode::Parallel);
    agent.loop_engine.before_tool_call_hook = Some(std::sync::Arc::new(TestBeforeHook));
    agent.loop_engine.after_tool_call_hook = Some(std::sync::Arc::new(TestAfterHook));
    let _rx = agent.subscribe();

    agent.steer(AgentMessage::User {
        content: "Steering prompt".to_string(),
    });

    let st = agent.get_state().await;
    assert_eq!(st.model, "gpt-4o");

    agent
        .compact_history(Some(CompactionOptions::default()))
        .await;
}

#[test]
fn test_compaction_logic() {
    let mut msgs = vec![AgentMessage::System {
        content: "System prompt".to_string(),
    }];
    for i in 0..60 {
        msgs.push(AgentMessage::User {
            content: format!("Msg {}", i),
        });
    }

    let options = CompactionOptions {
        max_messages: 20,
        preserve_recent: 5,
    };

    let compacted = compact_messages(&msgs, &options);
    assert!(compacted.len() <= 10);
    assert_eq!(compacted[0].role_str(), "system");
}

#[test]
fn test_session_tree_persistence_and_branching() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("session.jsonl");

    let mut tree = SessionTree::new("sess_1");
    tree.file_path = Some(file_path.clone());

    let n1 = tree.add_message(AgentMessage::User {
        content: "Hello".to_string(),
    });
    let _n2 = tree.add_message(AgentMessage::Assistant {
        content: Some("Hi there".to_string()),
        tool_calls: None,
    });

    assert_eq!(tree.nodes.len(), 2);
    tree.save_to_file(&file_path).unwrap();

    let loaded = SessionTree::load_from_file(&file_path).unwrap();
    assert_eq!(loaded.nodes.len(), 2);

    let forked = tree.fork_branch(&n1).unwrap();
    assert_eq!(forked.nodes.len(), 1);
}

#[test]
fn test_convert_to_codex_llm_structure() {
    use mypi_agent::loop_engine::convert_to_codex_llm;

    let messages = vec![
        AgentMessage::System {
            content: "Be helpful.".to_string(),
        },
        AgentMessage::User {
            content: "List files".to_string(),
        },
        AgentMessage::Assistant {
            content: Some("Listing files:".to_string()),
            tool_calls: Some(vec![mypi_provider::openai::ToolCall {
                id: "call_abc123".to_string(),
                r#type: "function".to_string(),
                function: ToolCallFunction {
                    name: "list_dir".to_string(),
                    arguments: "{\"path\":\".\"}".to_string(),
                },
            }]),
        },
        AgentMessage::Tool {
            tool_call_id: "call_abc123".to_string(),
            name: "list_dir".to_string(),
            content: "file1.txt\nfile2.txt".to_string(),
            is_error: false,
        },
    ];

    let (instructions, items) = convert_to_codex_llm(&messages);

    assert_eq!(instructions, "Be helpful.");
    assert_eq!(items.len(), 4);

    // User message item
    assert_eq!(items[0]["type"], "message");
    assert_eq!(items[0]["role"], "user");

    // Assistant message item
    assert_eq!(items[1]["type"], "message");
    assert_eq!(items[1]["role"], "assistant");

    // Function call item
    assert_eq!(items[2]["type"], "function_call");
    assert_eq!(items[2]["call_id"], "call_abc123");
    assert_eq!(items[2]["name"], "list_dir");

    // Function call output item
    assert_eq!(items[3]["type"], "function_call_output");
    assert_eq!(items[3]["call_id"], "call_abc123");
}

#[derive(Clone)]
struct RecordingExecutor {
    id: &'static str,
    definitions: Vec<AgentToolDefinition>,
    response: Option<Result<String, String>>,
    calls: Arc<Mutex<Vec<String>>>,
    panic_on: Option<&'static str>,
}

#[async_trait::async_trait]
impl ToolExecutor for RecordingExecutor {
    fn executor_id(&self) -> &str {
        self.id
    }

    fn tool_definitions(&self) -> Vec<AgentToolDefinition> {
        self.definitions.clone()
    }

    async fn execute_tool(&self, name: &str, _args: &str) -> Option<Result<String, String>> {
        self.calls.lock().unwrap().push(self.id.to_string());
        if self.panic_on == Some(name) {
            panic!("executor panic for test");
        }
        self.response.clone()
    }
}

fn test_definition(name: &str, description: &str) -> AgentToolDefinition {
    AgentToolDefinition::new(
        name,
        description,
        serde_json::json!({
            "type": "object",
            "properties": { "value": { "type": "string" } }
        }),
    )
}

fn tool_call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        r#type: "function".to_string(),
        function: ToolCallFunction {
            name: name.to_string(),
            arguments: "{}".to_string(),
        },
    }
}

fn executor(
    id: &'static str,
    tool_names: &[&str],
    response: Option<Result<&str, &str>>,
    calls: Arc<Mutex<Vec<String>>>,
) -> RecordingExecutor {
    RecordingExecutor {
        id,
        definitions: tool_names
            .iter()
            .map(|name| test_definition(name, "test definition"))
            .collect(),
        response: response.map(|result| {
            result
                .map(|value| value.to_string())
                .map_err(|error| error.to_string())
        }),
        calls,
        panic_on: None,
    }
}

#[test]
fn test_agent_tool_definition_provider_shapes_round_trip() {
    let mut definition = test_definition("lookup", "Looks up a value");
    definition.strict = Some(true);

    let chat = definition.to_chat_completions_tool();
    assert_eq!(chat["type"], "function");
    assert_eq!(chat["function"]["name"], "lookup");
    assert_eq!(chat["function"]["strict"], true);
    assert!(chat.get("name").is_none());

    let codex = definition.to_codex_responses_tool();
    assert_eq!(codex["type"], "function");
    assert_eq!(codex["name"], "lookup");
    assert_eq!(codex["strict"], true);
    assert!(codex.get("function").is_none());

    assert_eq!(
        AgentToolDefinition::from_provider_schema(&chat).unwrap(),
        definition
    );
    assert_eq!(
        AgentToolDefinition::from_provider_schema(&codex).unwrap(),
        definition
    );
}

#[tokio::test]
async fn test_reasoning_effort_is_added_to_provider_payloads() {
    let agent_loop = AgentLoop::new("fake_key", None, "gpt-5.6-luna");
    agent_loop.state.lock().await.reasoning_effort = ReasoningEffort::High;

    let (chat_payload, codex_payload) = agent_loop.build_api_payloads().await;

    assert_eq!(chat_payload["reasoning_effort"], "high");
    assert_eq!(codex_payload["reasoning"]["effort"], "high");
    assert_eq!(codex_payload["reasoning"]["summary"], "auto");
}

#[tokio::test]
async fn test_off_reasoning_effort_is_omitted_from_provider_payloads() {
    let agent_loop = AgentLoop::new("fake_key", None, "gpt-5.6-luna");
    agent_loop.state.lock().await.reasoning_effort = ReasoningEffort::Off;

    let (chat_payload, codex_payload) = agent_loop.build_api_payloads().await;

    assert!(chat_payload.get("reasoning_effort").is_none());
    assert!(codex_payload.get("reasoning").is_none());
}

#[tokio::test]
async fn test_dynamic_tool_payloads_are_provider_specific_and_deduplicated() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut agent_loop = AgentLoop::new("fake_key", None, "gpt-4o");
    agent_loop
        .register_tool_executor(Arc::new(RecordingExecutor {
            id: "dynamic",
            definitions: vec![test_definition("dynamic_tool", "executor definition")],
            response: None,
            calls,
            panic_on: None,
        }))
        .unwrap();

    {
        let mut state = agent_loop.state.lock().await;
        state
            .tools
            .push(test_definition("state_tool", "state definition").to_chat_completions_tool());
        state
            .tools
            .push(test_definition("dynamic_tool", "state wins").to_chat_completions_tool());
        state
            .tools
            .push(test_definition("read_file", "duplicate core").to_chat_completions_tool());
    }

    let (chat_payload, codex_payload) = agent_loop.build_api_payloads().await;
    let chat_tools = chat_payload["tools"].as_array().unwrap();
    let codex_tools = codex_payload["tools"].as_array().unwrap();

    let chat_dynamic: Vec<_> = chat_tools
        .iter()
        .filter(|tool| tool["function"]["name"] == "dynamic_tool")
        .collect();
    let codex_dynamic: Vec<_> = codex_tools
        .iter()
        .filter(|tool| tool["name"] == "dynamic_tool")
        .collect();
    assert_eq!(chat_dynamic.len(), 1);
    assert_eq!(codex_dynamic.len(), 1);
    assert_eq!(
        chat_dynamic[0]["function"]["description"],
        "executor definition"
    );
    assert_eq!(codex_dynamic[0]["description"], "executor definition");
    assert!(chat_dynamic[0].get("name").is_none());
    assert!(codex_dynamic[0].get("function").is_none());

    assert_eq!(
        chat_tools
            .iter()
            .filter(|tool| tool["function"]["name"] == "read_file")
            .count(),
        1
    );
    assert_eq!(
        codex_tools
            .iter()
            .filter(|tool| tool["name"] == "read_file")
            .count(),
        1
    );
    assert!(chat_tools
        .iter()
        .any(|tool| tool["function"]["name"] == "state_tool"));
    assert!(codex_tools.iter().any(|tool| tool["name"] == "state_tool"));
}

#[tokio::test]
async fn test_tool_allowlist_filters_core_and_dynamic_payload_definitions() {
    let mut agent_loop = AgentLoop::new("fake_key", None, "gpt-4o");
    agent_loop
        .register_tool_executor(Arc::new(RecordingExecutor {
            id: "allowlist-payload",
            definitions: vec![
                test_definition("allowed_dynamic", "allowed"),
                test_definition("blocked_dynamic", "blocked"),
            ],
            response: None,
            calls: Arc::new(Mutex::new(Vec::new())),
            panic_on: None,
        }))
        .unwrap();
    agent_loop.set_allowed_tool_names(Some(HashSet::from([
        "read_file".to_string(),
        "allowed_dynamic".to_string(),
    ])));

    let (chat_payload, codex_payload) = agent_loop.build_api_payloads().await;
    let mut chat_names: Vec<_> = chat_payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect();
    let mut codex_names: Vec<_> = codex_payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    chat_names.sort_unstable();
    codex_names.sort_unstable();

    assert_eq!(chat_names, vec!["allowed_dynamic", "read_file"]);
    assert_eq!(codex_names, vec!["allowed_dynamic", "read_file"]);
}

#[tokio::test]
async fn test_tool_allowlist_blocks_core_and_dynamic_execution() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut agent_loop = AgentLoop::new("fake_key", None, "gpt-4o");
    agent_loop.tool_execution_mode = ToolExecutionMode::Sequential;
    agent_loop
        .register_tool_executor(Arc::new(executor(
            "blocked-executor",
            &["blocked_dynamic"],
            Some(Ok("should not execute")),
            calls.clone(),
        )))
        .unwrap();
    agent_loop.set_allowed_tool_names(Some(HashSet::new()));

    let results = agent_loop
        .execute_tools(&[
            tool_call("call_core", "read_file"),
            tool_call("call_dynamic", "blocked_dynamic"),
        ])
        .await;

    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|result| result.is_error));
    assert!(results
        .iter()
        .all(|result| result.content.contains("not allowed")));
}

#[test]
fn test_registration_rejects_duplicate_and_core_tool_schemas() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut agent_loop = AgentLoop::new("fake_key", None, "gpt-4o");
    agent_loop
        .register_tool_executor(Arc::new(RecordingExecutor {
            id: "first",
            definitions: vec![test_definition("custom", "first")],
            response: None,
            calls: calls.clone(),
            panic_on: None,
        }))
        .unwrap();

    let duplicate = agent_loop.register_tool_executor(Arc::new(RecordingExecutor {
        id: "second",
        definitions: vec![test_definition("custom", "duplicate")],
        response: None,
        calls: calls.clone(),
        panic_on: None,
    }));
    assert!(duplicate.unwrap_err().contains("conflicts"));

    let core_duplicate = agent_loop.register_tool_executor(Arc::new(RecordingExecutor {
        id: "third",
        definitions: vec![test_definition("read_file", "duplicate")],
        response: None,
        calls,
        panic_on: None,
    }));
    assert!(core_duplicate.unwrap_err().contains("read_file"));
    assert_eq!(agent_loop.tool_executor_count(), 1);
}

#[tokio::test]
async fn test_async_executors_route_to_declared_owner_and_mark_errors() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut agent_loop = AgentLoop::new("fake_key", None, "gpt-4o");
    agent_loop.tool_execution_mode = ToolExecutionMode::Sequential;
    agent_loop
        .register_tool_executor(Arc::new(executor(
            "first",
            &["other"],
            Some(Ok("hijacked")),
            calls.clone(),
        )))
        .unwrap();
    agent_loop
        .register_tool_executor(Arc::new(executor(
            "second",
            &["custom"],
            Some(Err("failed")),
            calls.clone(),
        )))
        .unwrap();
    agent_loop
        .register_tool_executor(Arc::new(executor(
            "third",
            &["third_tool"],
            Some(Ok("not reached")),
            calls.clone(),
        )))
        .unwrap();

    let results = agent_loop
        .execute_tools(&[tool_call("call_error", "custom")])
        .await;

    assert_eq!(*calls.lock().unwrap(), vec!["second"]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id, "call_error");
    assert!(results[0].is_error);
    assert!(results[0].content.contains("failed"));
}

#[tokio::test]
async fn test_executor_cannot_hijack_core_or_peer_owned_tools() {
    let attacker_calls = Arc::new(Mutex::new(Vec::new()));
    let owner_calls = Arc::new(Mutex::new(Vec::new()));
    let mut agent_loop = AgentLoop::new("fake_key", None, "gpt-4o");
    agent_loop.tool_execution_mode = ToolExecutionMode::Sequential;
    agent_loop
        .register_tool_executor(Arc::new(executor(
            "attacker",
            &["attacker_tool"],
            Some(Ok("hijacked")),
            attacker_calls.clone(),
        )))
        .unwrap();
    agent_loop
        .register_tool_executor(Arc::new(executor(
            "owner",
            &["owned_tool"],
            Some(Ok("owner result")),
            owner_calls.clone(),
        )))
        .unwrap();

    let results = agent_loop
        .execute_tools(&[
            tool_call("call_core", "read_file"),
            tool_call("call_owned", "owned_tool"),
        ])
        .await;

    assert!(attacker_calls.lock().unwrap().is_empty());
    assert_eq!(*owner_calls.lock().unwrap(), vec!["owner"]);
    assert_eq!(results.len(), 2);
    assert_ne!(results[0].content, "hijacked");
    assert_eq!(results[1].content, "owner result");
}

struct WaitingBeforeHook {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl BeforeToolCallHook for WaitingBeforeHook {
    async fn before_tool_call(
        &self,
        _tool_call: &AgentToolCall,
        _state: &AgentState,
    ) -> BeforeToolCallResult {
        self.entered.notify_one();
        self.release.notified().await;
        BeforeToolCallResult::default()
    }
}

struct WaitingAfterHook {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl AfterToolCallHook for WaitingAfterHook {
    async fn after_tool_call(
        &self,
        _tool_call: &AgentToolCall,
        _result: &AgentToolResult,
        _state: &AgentState,
    ) -> AfterToolCallResult {
        self.entered.notify_one();
        self.release.notified().await;
        AfterToolCallResult::default()
    }
}

async fn assert_state_is_unlocked_while_hook_waits(mut agent_loop: AgentLoop, before: bool) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    if before {
        agent_loop.before_tool_call_hook = Some(Arc::new(WaitingBeforeHook {
            entered: entered.clone(),
            release: release.clone(),
        }));
    } else {
        agent_loop.after_tool_call_hook = Some(Arc::new(WaitingAfterHook {
            entered: entered.clone(),
            release: release.clone(),
        }));
    }
    agent_loop.tool_execution_mode = ToolExecutionMode::Sequential;
    agent_loop
        .register_tool_executor(Arc::new(executor(
            "hook-executor",
            &["custom"],
            Some(Ok("ok")),
            Arc::new(Mutex::new(Vec::new())),
        )))
        .unwrap();

    let state = agent_loop.state.clone();
    let entered_wait = entered.notified();
    let execution = tokio::spawn(async move {
        agent_loop
            .execute_tools(&[tool_call("call_hook", "custom")])
            .await
    });
    entered_wait.await;

    let lock_result = tokio::time::timeout(Duration::from_millis(250), state.lock()).await;
    let state_was_unlocked = lock_result.is_ok();
    drop(lock_result);
    release.notify_one();
    let results = execution.await.unwrap();

    assert!(
        state_was_unlocked,
        "state mutex was held across a hook await"
    );
    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn test_state_mutex_is_not_held_across_before_hook_await() {
    assert_state_is_unlocked_while_hook_waits(AgentLoop::new("fake_key", None, "gpt-4o"), true)
        .await;
}

#[tokio::test]
async fn test_state_mutex_is_not_held_across_after_hook_await() {
    assert_state_is_unlocked_while_hook_waits(AgentLoop::new("fake_key", None, "gpt-4o"), false)
        .await;
}

#[tokio::test]
async fn test_parallel_join_error_preserves_result_count_and_order() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut agent_loop = AgentLoop::new("fake_key", None, "gpt-4o");
    agent_loop
        .register_tool_executor(Arc::new(RecordingExecutor {
            id: "panic-executor",
            definitions: vec![
                test_definition("panic_tool", "panic test"),
                test_definition("ok_tool", "success test"),
            ],
            response: Some(Ok("ok".to_string())),
            calls,
            panic_on: Some("panic_tool"),
        }))
        .unwrap();

    let results = agent_loop
        .execute_tools(&[
            tool_call("call_panic", "panic_tool"),
            tool_call("call_ok", "ok_tool"),
        ])
        .await;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].tool_call_id, "call_panic");
    assert_eq!(results[1].tool_call_id, "call_ok");
    assert!(results[0].is_error);
    assert!(!results[1].is_error);
    assert_eq!(results[1].content, "ok");
}
