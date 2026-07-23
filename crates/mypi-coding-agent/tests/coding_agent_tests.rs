use mypi_agent::{Agent, AgentMessage, SessionTree};
use mypi_coding_agent::{
    execute_slash_command, parse_slash_command, CommandAction, ProjectContext,
};
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;

#[test]
fn test_project_context_discovery() {
    let dir = tempdir().unwrap();
    let agents_file = dir.path().join("AGENTS.md");
    let mut f = File::create(&agents_file).unwrap();
    writeln!(f, "Rule 1: Always write tests.").unwrap();

    let ctx = ProjectContext::discover(dir.path());
    assert_eq!(ctx.context_files.len(), 1);
    assert_eq!(ctx.instructions.len(), 1);
    assert_eq!(ctx.instructions[0].path, agents_file);
    assert_eq!(ctx.instructions[0].content, "Rule 1: Always write tests.");
    assert!(ctx
        .combined_instructions
        .contains("Rule 1: Always write tests."));
}

#[tokio::test]
async fn compact_command_stays_in_current_session() {
    let mut agent = Agent::new("fake", None, "gpt-4o");
    let mut tree = SessionTree::new("current_session");
    for index in 0..60 {
        let message = AgentMessage::User {
            content: format!("message {index}"),
        };
        agent
            .loop_engine
            .state
            .lock()
            .await
            .messages
            .push(message.clone());
        tree.add_message(message);
    }

    let output = execute_slash_command(CommandAction::Compact, &mut agent, &mut tree).await;

    assert_eq!(tree.session_id, "current_session");
    assert_eq!(output, "Context compacted in the current session.");
    assert!(tree
        .get_active_branch_messages()
        .iter()
        .any(|message| mypi_agent::compaction_summary_text(message).is_some()));
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
