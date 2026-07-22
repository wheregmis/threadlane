use mypi_coding_agent::{
    parse_slash_command, CommandAction, HarnessPolicy, PlanModeState, ProjectContext,
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
    assert!(ctx
        .combined_instructions
        .contains("Rule 1: Always write tests."));
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
fn test_harness_policy_centralizes_plan_mode_enforcement() {
    let decision = HarnessPolicy::ReadOnly.evaluate_tool_call("write_file", "{}");
    assert!(decision.block);
    assert!(decision
        .reason
        .as_deref()
        .unwrap_or_default()
        .contains("blocked because Plan Mode is ACTIVE"));

    let decision = HarnessPolicy::ReadOnly.evaluate_tool_call("run_command", "cargo check");
    assert!(!decision.block);

    let decision = HarnessPolicy::ReadOnly.evaluate_tool_call("run_command", "rm -rf .");
    assert!(decision.block);
    assert!(decision
        .reason
        .as_deref()
        .unwrap_or_default()
        .contains("restricted in Plan Mode"));

    let decision = HarnessPolicy::FullAccess.evaluate_tool_call("write_file", "{}");
    assert!(!decision.block);
}

#[test]
fn test_plan_mode_parsing_and_guards() {
    let mut plan = PlanModeState::new();
    assert!(!plan.enabled);

    plan.toggle();
    assert!(plan.enabled);

    assert!(plan.is_tool_allowed("read_file"));
    assert!(!plan.is_tool_allowed("write_file"));
    assert!(!plan.is_tool_allowed("edit_file"));

    assert!(plan.is_command_allowed("cargo check"));
    assert!(plan.is_command_allowed("git status"));
    assert!(!plan.is_command_allowed("rm -rf ."));

    let sample_response = "Plan:\n1. Inspect main.rs\n2. Add test suite\n3. Run cargo test\n";
    let count = plan.parse_and_update_plan(sample_response);
    assert_eq!(count, 3);

    let markdown_response = "## Implementation Plan\n1) Inspect the command flow\n2) Add regression coverage\n\n## Notes\nDo not treat this as a plan item.";
    let count = plan.parse_and_update_plan(markdown_response);
    assert_eq!(count, 2);
    assert_eq!(plan.items[0].description, "Inspect the command flow");

    assert!(plan.mark_done(1));
    let todos = plan.format_todos();
    assert!(todos.contains("✅ 1. Inspect the command flow"));
}
