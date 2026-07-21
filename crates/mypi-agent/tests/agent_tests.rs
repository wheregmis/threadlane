use mypi_agent::{
    compact_messages, parse_slash_command, AfterToolCallHook, AfterToolCallResult, Agent,
    AgentMessage, AgentState, AgentToolCall, AgentToolResult, BeforeToolCallHook,
    BeforeToolCallResult, CommandAction, CompactionOptions, PlanModeState, ProjectContext,
    SessionTree, ToolExecutionMode,
};
use std::fs::File;
use std::io::Write;
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

    // Compaction test
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
fn test_project_context_discovery() {
    let dir = tempdir().unwrap();
    let agents_file = dir.path().join("AGENTS.md");
    let mut f = File::create(&agents_file).unwrap();
    writeln!(f, "Rule 1: Always write tests.").unwrap();

    let ctx = ProjectContext::discover(dir.path());
    assert_eq!(ctx.context_files.len(), 1);
    assert!(ctx
        .combined_instructions
        .contains("Rule 1: Always write tests."));
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
fn test_slash_command_parsing() {
    assert_eq!(
        parse_slash_command("/model gpt-4o"),
        Some(CommandAction::SwitchModel("gpt-4o".to_string()))
    );
    assert_eq!(
        parse_slash_command("/compact"),
        Some(CommandAction::Compact)
    );
    assert_eq!(
        parse_slash_command("/plan"),
        Some(CommandAction::Unknown("plan".to_string()))
    );
    assert_eq!(
        parse_slash_command("/todos"),
        Some(CommandAction::Unknown("todos".to_string()))
    );
    assert_eq!(parse_slash_command("/quit"), Some(CommandAction::Quit));
    assert_eq!(
        parse_slash_command("/session"),
        Some(CommandAction::ShowSession)
    );
}

#[test]
fn test_plan_mode_parsing_and_guards() {
    let mut plan = PlanModeState::new();
    assert!(!plan.enabled);

    plan.toggle();
    assert!(plan.enabled);

    // Read-only tool check
    assert!(plan.is_tool_allowed("read_file"));
    assert!(!plan.is_tool_allowed("write_file"));
    assert!(!plan.is_tool_allowed("edit_file"));

    // Shell command check
    assert!(plan.is_command_allowed("cargo check"));
    assert!(plan.is_command_allowed("git status"));
    assert!(!plan.is_command_allowed("rm -rf ."));

    // Plan text parsing
    let sample_response = "Plan:\n1. Inspect main.rs\n2. Add test suite\n3. Run cargo test\n";
    let count = plan.parse_and_update_plan(sample_response);
    assert_eq!(count, 3);

    let markdown_response = "## Implementation Plan\n1) Inspect the command flow\n2) Add regression coverage\n\n## Notes\nDo not treat this as a plan item.";
    let count = plan.parse_and_update_plan(markdown_response);
    assert_eq!(count, 2);
    assert_eq!(plan.items[0].description, "Inspect the command flow");

    // Todo completion
    assert!(plan.mark_done(1));
    let todos = plan.format_todos();
    assert!(todos.contains("✅ 1. Inspect the command flow"));
}
