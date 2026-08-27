use threadlane_protocol::{
    AdvisorNote, ModelRoles, PermissionRequest, SessionEvent, SessionPlan, TokenUsage,
};

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
    AdvisorNote(AdvisorNote),
    ModelRolesUpdated(ModelRoles),
    Usage(TokenUsage),
    Error(String),
    PermissionRequested(PermissionRequest),
    Ignore,
}

pub(crate) fn adapt_agent_event(event: SessionEvent) -> ChatAgentUpdate {
    match event {
        SessionEvent::TurnCompleted { usage, .. } => ChatAgentUpdate::Usage(usage),
        SessionEvent::SessionCompleted { total_usage, .. } => ChatAgentUpdate::Usage(total_usage),
        SessionEvent::TokenDelta { delta } => ChatAgentUpdate::TextDelta(delta),
        SessionEvent::ReasoningDelta { delta } => ChatAgentUpdate::ReasoningDelta(delta),
        SessionEvent::ToolCallStarted {
            tool_call_id,
            name,
            arguments,
        } => ChatAgentUpdate::ToolStarted {
            tool_call_id,
            name,
            arguments,
        },
        SessionEvent::ToolCallUpdated {
            tool_call_id,
            partial_result,
        } => ChatAgentUpdate::ToolUpdated {
            tool_call_id,
            partial_result,
        },
        SessionEvent::ToolCallFinished {
            tool_call_id,
            result,
            ..
        } => ChatAgentUpdate::ToolFinished {
            tool_call_id,
            content: result.content,
            is_error: result.is_error,
        },
        SessionEvent::PlanUpdated { plan } => ChatAgentUpdate::PlanUpdated(plan),
        SessionEvent::AdvisorNote { note } => ChatAgentUpdate::AdvisorNote(note),
        SessionEvent::ModelRolesUpdated { roles } => ChatAgentUpdate::ModelRolesUpdated(roles),
        SessionEvent::Error { message } => ChatAgentUpdate::Error(message),
        SessionEvent::PermissionRequested { request } => {
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
        let update = adapt_agent_event(SessionEvent::TokenDelta {
            delta: "hello".into(),
        });
        assert_eq!(update, ChatAgentUpdate::TextDelta("hello".into()));
    }

    #[test]
    fn plan_update_preserves_the_canonical_session_plan() {
        let plan = SessionPlan {
            explanation: Some("Ship incrementally".into()),
            items: vec![threadlane_protocol::PlanItem {
                step: "Inspect the UI".into(),
                status: threadlane_protocol::PlanItemStatus::InProgress,
            }],
        };

        assert_eq!(
            adapt_agent_event(SessionEvent::PlanUpdated { plan: plan.clone() }),
            ChatAgentUpdate::PlanUpdated(plan)
        );
    }
}
