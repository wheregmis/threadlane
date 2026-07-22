use mypi_coding_agent::{parse_slash_command, CommandAction, ProjectContext};
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
