use mypi_agent::{
    compact_messages, AfterToolCallHook, AfterToolCallResult, Agent, AgentMessage, AgentState,
    AgentToolCall, AgentToolResult, BeforeToolCallHook, BeforeToolCallResult, CompactionOptions,
    SessionTree, ToolExecutionMode,
};
use tempfile::tempdir;

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
    use mypi_provider::openai::ToolCallFunction;

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
