use crate::harness::ToolReplaySafety;

pub fn classify_tool_replay_safety(tool_name: &str) -> ToolReplaySafety {
    match tool_name {
        "view_file" | "read_file" | "list_dir" | "grep_search" | "list_permissions" => {
            ToolReplaySafety::Safe
        }
        _ => ToolReplaySafety::Never,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_tool_replay_safety() {
        assert_eq!(
            classify_tool_replay_safety("view_file"),
            ToolReplaySafety::Safe
        );
        assert_eq!(
            classify_tool_replay_safety("list_dir"),
            ToolReplaySafety::Safe
        );
        assert_eq!(
            classify_tool_replay_safety("grep_search"),
            ToolReplaySafety::Safe
        );
        assert_eq!(
            classify_tool_replay_safety("replace_file_content"),
            ToolReplaySafety::Never
        );
        assert_eq!(
            classify_tool_replay_safety("run_command"),
            ToolReplaySafety::Never
        );
    }
}
