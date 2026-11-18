use crate::{AgentEvent, PermissionRequest, QuestionRequest, SessionPlan, TokenUsage};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatAgentUpdate {
    TextDelta(String),
    ReasoningDelta(String),
    ToolStarted {
        tool_call_id: String,
        name: String,
        arguments: String,
    },
    ToolUpdated {
        tool_call_id: String,
        partial_result: String,
    },
    ToolFinished {
        tool_call_id: String,
        content: String,
        is_error: bool,
    },
    PlanUpdated(SessionPlan),
    Usage(TokenUsage),
    Error(String),
    PermissionRequested(PermissionRequest),
    QuestionRequested(QuestionRequest),
    Ignore,
}

pub fn adapt_agent_event(event: AgentEvent) -> ChatAgentUpdate {
    match event {
        AgentEvent::AgentEnd { usage } => ChatAgentUpdate::Usage(usage),
        AgentEvent::MessageUpdate {
            text_delta: Some(delta),
            ..
        } => ChatAgentUpdate::TextDelta(delta),
        AgentEvent::MessageUpdate {
            reasoning_delta: Some(delta),
            ..
        } => ChatAgentUpdate::ReasoningDelta(delta),
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            name,
            arguments,
        } => ChatAgentUpdate::ToolStarted {
            tool_call_id,
            name,
            arguments,
        },
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            partial_result,
        } => ChatAgentUpdate::ToolUpdated {
            tool_call_id,
            partial_result,
        },
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            result,
            ..
        } => ChatAgentUpdate::ToolFinished {
            tool_call_id,
            content: result.content,
            is_error: result.is_error,
        },
        AgentEvent::PlanUpdated { plan } => ChatAgentUpdate::PlanUpdated(plan),
        AgentEvent::AgentError { error } => ChatAgentUpdate::Error(error),
        AgentEvent::PermissionRequested { request } => {
            ChatAgentUpdate::PermissionRequested(request)
        }
        AgentEvent::QuestionRequested { request } => ChatAgentUpdate::QuestionRequested(request),
        _ => ChatAgentUpdate::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_delta_is_projected_without_provider_details() {
        let update = adapt_agent_event(AgentEvent::MessageUpdate {
            text_delta: Some("hello".into()),
            reasoning_delta: None,
            tool_call_name: None,
        });
        assert_eq!(update, ChatAgentUpdate::TextDelta("hello".into()));
    }

    #[test]
    fn plan_update_preserves_the_canonical_session_plan() {
        let plan = SessionPlan {
            explanation: Some("Ship incrementally".into()),
            items: vec![crate::PlanItem {
                step: "Inspect the UI".into(),
                status: crate::PlanItemStatus::InProgress,
            }],
        };

        assert_eq!(
            adapt_agent_event(AgentEvent::PlanUpdated { plan: plan.clone() }),
            ChatAgentUpdate::PlanUpdated(plan)
        );
    }
}

pub fn tool_activity_summary(name: &str, arguments: &str) -> String {
    let display_name = name.replace('_', " ");
    let Ok(args_val) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return display_name;
    };
    let context = [
        "path",
        "file_path",
        "FilePath",
        "TargetFile",
        "command",
        "CommandLine",
        "query",
        "Query",
        "regex",
        "glob",
        "pattern",
        "Pattern",
        "prompt",
        "Prompt",
        "description",
        "Description",
    ]
    .iter()
    .find_map(|key| args_val.get(key).and_then(|v| v.as_str()));

    if let Some(ctx) = context {
        let trimmed = ctx.trim();
        if !trimmed.is_empty() {
            let first_line = trimmed.lines().next().unwrap_or(trimmed).trim();
            let has_more_lines = trimmed.lines().nth(1).is_some();
            let mut summary_ctx = first_line.to_string();
            if has_more_lines && !summary_ctx.ends_with('…') && !summary_ctx.ends_with("...") {
                summary_ctx.push_str(" …");
            }
            return format!("{display_name}: {summary_ctx}");
        }
    }
    display_name
}

pub fn tool_activity_display_summary(summary: &str) -> String {
    let first_line = summary.lines().next().unwrap_or(summary).trim();
    let first_line = [
        ("run command: ", "Run"),
        ("read file: ", "Read"),
        ("list dir: ", "List"),
        ("grep search: ", "Search"),
        ("edit file hashline: ", "Edit"),
    ]
    .into_iter()
    .find_map(|(prefix, label)| first_line.strip_prefix(prefix).map(|detail| format!("{label} · {detail}")))
    .unwrap_or_else(|| first_line.to_string());
    if summary.lines().nth(1).is_some()
        && !first_line.ends_with('…')
        && !first_line.ends_with("...")
    {
        format!("{first_line} …")
    } else {
        first_line
    }
}

#[cfg(test)]
mod activity_display_tests {
    #[test]
    fn compact_activity_labels_preserve_details_and_unknown_tools() {
        use super::tool_activity_display_summary as display;
        assert_eq!(display("read file: src/main.rs"), "Read · src/main.rs");
        assert_eq!(display("run command: cargo check\nmore output"), "Run · cargo check …");
        assert_eq!(display("grep search: user_id"), "Search · user_id");
        assert_eq!(display("custom tool: exact_value"), "custom tool: exact_value");
        assert_eq!(display("Read · src/main.rs"), "Read · src/main.rs");
    }
}
