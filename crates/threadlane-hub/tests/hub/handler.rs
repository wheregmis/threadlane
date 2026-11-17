//! Defaults and the handler (Plan.md §6.3).

use std::time::Duration;

use serde_json::json;
use threadlane_core::client::{ApiError, UserMessage};
use threadlane_core::link::HubMessage;
use threadlane_core::model::{BackendKind, EventKind, EventStatus, Mode, NewProject, Trust};
use threadlane_core::runtime::NoHooks;
use threadlane_core::store::{InboxStore, MachineStore, OrgStore};
use threadlane_core::time::Clock;
use threadlane_hub::{HUB_ENDPOINT, LEAD_INSTRUCTIONS, LEAD_NAME, LEAD_ROLE};

use crate::common::TestHub;
use std::sync::Arc;

#[tokio::test]
async fn a_fresh_hub_has_a_lead_a_binding_and_a_machine_and_new_is_idempotent() {
    let hub = TestHub::start().await;
    let agents = hub.db.agents().await.unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].name, LEAD_NAME);
    assert_eq!(agents[0].role, LEAD_ROLE);
    assert_eq!(agents[0].instructions, LEAD_INSTRUCTIONS);
    assert_eq!(
        agents[0].backends,
        [BackendKind::Claude],
        "a lead runs on Claude only"
    );

    let general = hub.general().await;
    let binding = hub.db.lead_binding(general.id).await.unwrap().unwrap();
    assert_eq!(binding.agent_id, agents[0].id);
    assert_eq!(binding.mode, Mode::OnRequest);
    assert!(binding.is_lead);

    let machines = hub.db.machines().await.unwrap();
    assert_eq!(machines.len(), 1);
    assert!(machines[0].is_hub);
    assert_eq!(machines[0].endpoint_id, HUB_ENDPOINT);
    assert_eq!(hub.core.local_machine().await.unwrap(), machines[0].id);

    // A project created later gets its lead binding at the next start, and
    // nothing else is created twice.
    let alpha = hub
        .db
        .create_project(&NewProject {
            name: "Alpha".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let again = hub.restart(Arc::new(NoHooks)).await;
    assert_eq!(again.db.agents().await.unwrap(), agents);
    assert_eq!(again.db.machines().await.unwrap().len(), 1);
    assert_eq!(again.db.bindings(general.id).await.unwrap(), vec![binding]);
    let alpha_lead = again.db.lead_binding(alpha.id).await.unwrap().unwrap();
    assert_eq!(alpha_lead.agent_id, agents[0].id);
    assert_eq!(alpha_lead.mode, Mode::OnRequest);
    let third = again.restart(Arc::new(NoHooks)).await;
    assert_eq!(third.db.bindings(alpha.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_message_is_one_pending_event_in_the_general_lead_lane_and_nudges_links() {
    let hub = TestHub::start().await;
    let general = hub.general().await;
    let accepted = hub.send("m-1", "What is on today?").await;
    assert!(!accepted.duplicate);
    assert_eq!(accepted.project_id, general.id);

    let pending = hub.db.pending_events(general.lead_lane_id).await.unwrap();
    assert_eq!(pending.len(), 1);
    let event = &pending[0];
    assert_eq!(event.id, accepted.event_id);
    assert_eq!(event.source, "client");
    assert_eq!(event.source_event_id, "m-1");
    assert_eq!(event.kind, EventKind::UserMessage);
    assert_eq!(event.payload, json!({ "text": "What is on today?" }));
    assert_eq!(event.trust, Trust::User);
    assert!(event.requested);
    assert_eq!(event.status, EventStatus::Pending);
    assert_eq!(event.received_at, hub.clock.now());

    let nudge = tokio::time::timeout(Duration::from_secs(1), hub.link.next_message())
        .await
        .expect("the link was nudged");
    assert_eq!(nudge, Some(HubMessage::Nudge));

    let again = hub.send("m-1", "What is on today?").await;
    assert!(again.duplicate);
    assert_eq!(again.event_id, accepted.event_id);
    assert_eq!(again.project_id, general.id);
    assert_eq!(
        hub.db
            .pending_events(general.lead_lane_id)
            .await
            .unwrap()
            .len(),
        1,
        "no second event"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), hub.link.next_message())
            .await
            .is_err(),
        "a duplicate does not wake anyone"
    );
}

#[tokio::test]
async fn bad_messages_are_refused() {
    let hub = TestHub::start().await;
    let message = |project: Option<&str>, text: &str, id: &str| UserMessage {
        project: project.map(str::to_owned),
        text: text.to_owned(),
        client_message_id: id.to_owned(),
        task: None,
    };
    assert!(matches!(
        hub.api
            .send_message(&message(Some("Nowhere"), "hi", "m-1"))
            .await,
        Err(ApiError::NotFound(_))
    ));
    assert!(matches!(
        hub.api.send_message(&message(None, "  \n", "m-2")).await,
        Err(ApiError::Invalid(_))
    ));
    assert!(matches!(
        hub.api.send_message(&message(None, "hi", " ")).await,
        Err(ApiError::Invalid(_))
    ));
    let lane = hub.general().await.lead_lane_id;
    assert!(hub.db.pending_events(lane).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_named_project_routes_to_its_own_lead_lane() {
    let hub = TestHub::start().await;
    let alpha = hub
        .db
        .create_project(&NewProject {
            name: "Alpha".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let accepted = hub
        .api
        .send_message(&UserMessage {
            project: Some("Alpha".into()),
            text: "Ship it".into(),
            client_message_id: "a-1".into(),
            task: None,
        })
        .await
        .unwrap();
    assert_eq!(accepted.project_id, alpha.id);
    let event = hub.event(accepted.event_id).await;
    assert_eq!(event.lane_id, alpha.lead_lane_id);
    assert_eq!(event.project_id, alpha.id);
    let general = hub.general().await;
    assert!(
        hub.db
            .pending_events(general.lead_lane_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn info_and_projects_describe_the_space() {
    let hub = TestHub::start().await;
    let info = hub.api.info().await.unwrap();
    assert_eq!(info.space, "Space");
    assert_eq!(info.mode_ceiling, Mode::OnRequest);
    assert!(info.schema_version > 0);
    let projects = hub.api.projects().await.unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].name, "General");
    assert!(projects[0].is_general);
}
