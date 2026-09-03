use threadlane_session::{AgentEvent, PermissionRequest, SessionPlan, TokenUsage};

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
    Ignore,
}

pub(crate) fn adapt_agent_event(event: AgentEvent) -> ChatAgentUpdate {
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
            items: vec![threadlane_session::PlanItem {
                step: "Inspect the UI".into(),
                status: threadlane_session::PlanItemStatus::InProgress,
            }],
        };

        assert_eq!(
            adapt_agent_event(AgentEvent::PlanUpdated { plan: plan.clone() }),
            ChatAgentUpdate::PlanUpdated(plan)
        );
    }
}
