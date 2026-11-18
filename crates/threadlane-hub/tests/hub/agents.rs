//! Editing and deleting agents through the Organization capability.

use std::collections::BTreeMap;

use threadlane_core::client::{ApiError, OrgReply, OrgRequest, OverviewReply, OverviewRequest};
use threadlane_core::memory::INSTRUCTIONS_BUDGET;
use threadlane_core::model::{
    Agent, BackendKind, Mode, NewBinding, NewTask, TaskOrigin, TaskStatus,
};
use threadlane_core::store::{OrgStore, TaskStore};
use threadlane_core::time::Clock;
use threadlane_hub::LEAD_NAME;

use crate::common::TestHub;

async fn create(hub: &TestHub, name: &str) -> Agent {
    let reply = hub
        .api
        .org(OrgRequest::CreateAgent {
            name: name.into(),
            role: "reviewer".into(),
            instructions: "Review.".into(),
            backends: vec![BackendKind::Claude],
        })
        .await
        .unwrap();
    let OrgReply::Agent { agent } = reply else {
        panic!("expected an agent, got {reply:?}");
    };
    agent
}

#[derive(Default)]
struct Changes {
    name: Option<String>,
    role: Option<String>,
    instructions: Option<String>,
    backends: Option<Vec<String>>,
    models: Option<BTreeMap<String, String>>,
}

fn update(agent: &str) -> OrgRequest {
    edit(agent, Changes::default())
}

fn edit(agent: &str, changes: Changes) -> OrgRequest {
    OrgRequest::UpdateAgent {
        agent: agent.into(),
        name: changes.name,
        role: changes.role,
        instructions: changes.instructions,
        backends: changes.backends,
        models: changes.models,
    }
}

fn delete(agent: &str, confirm: &str) -> OrgRequest {
    OrgRequest::DeleteAgent {
        agent: agent.into(),
        confirm_name: confirm.into(),
    }
}

fn refused(reply: Result<OrgReply, ApiError>, needle: &str) {
    match reply {
        Err(ApiError::Invalid(message)) => assert!(message.contains(needle), "{message}"),
        other => panic!("expected a refusal naming {needle:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn an_update_changes_only_the_fields_it_names() {
    let hub = TestHub::start().await;
    create(&hub, "Reviewer").await;
    let reply = hub
        .api
        .org(edit(
            "Reviewer",
            Changes {
                name: Some(" Critic ".into()),
                backends: Some(vec!["codex".into(), "claude".into(), "codex".into()]),
                models: Some(BTreeMap::from([("codex".into(), "gpt-5".into())])),
                ..Changes::default()
            },
        ))
        .await
        .unwrap();
    let OrgReply::Agent { agent } = reply else {
        panic!("expected an agent, got {reply:?}");
    };
    assert_eq!(agent.name, "Critic");
    assert_eq!(agent.role, "reviewer", "kept");
    assert_eq!(agent.instructions, "Review.", "kept");
    assert_eq!(agent.backends, [BackendKind::Codex, BackendKind::Claude]);
    assert_eq!(agent.models["codex"], "gpt-5");

    let reply = hub
        .api
        .org(edit(
            "Critic",
            Changes {
                models: Some(BTreeMap::from([("codex".into(), String::new())])),
                ..Changes::default()
            },
        ))
        .await
        .unwrap();
    let OrgReply::Agent { agent } = reply else {
        panic!("expected an agent, got {reply:?}");
    };
    assert!(
        agent.models.get("codex").is_none(),
        "an empty model clears it"
    );
}

#[tokio::test]
async fn simultaneous_edits_keep_fields_saved_by_both_clients() {
    let hub = TestHub::start().await;
    create(&hub, "Reviewer").await;
    let role = hub.api.org(edit(
        "Reviewer",
        Changes {
            role: Some("quality".into()),
            ..Changes::default()
        },
    ));
    let instructions = hub.api.org(edit(
        "Reviewer",
        Changes {
            instructions: Some("Check the entire patch.".into()),
            ..Changes::default()
        },
    ));
    let (role, instructions) = tokio::join!(role, instructions);
    role.unwrap();
    instructions.unwrap();
    let OrgReply::Agents { agents } = hub.api.org(OrgRequest::Agents).await.unwrap() else {
        panic!("expected agents");
    };
    let agent = agents
        .iter()
        .find(|agent| agent.name == "Reviewer")
        .unwrap();
    assert_eq!(agent.role, "quality");
    assert_eq!(agent.instructions, "Check the entire patch.");
}

#[tokio::test]
async fn an_update_refuses_bad_fields() {
    let hub = TestHub::start().await;
    create(&hub, "Reviewer").await;
    create(&hub, "Tester").await;
    let api = &hub.api;
    refused(
        api.org(edit(
            "Reviewer",
            Changes {
                name: Some(" ".into()),
                ..Changes::default()
            },
        ))
        .await,
        "name is empty",
    );
    refused(
        api.org(edit(
            "Reviewer",
            Changes {
                role: Some(String::new()),
                ..Changes::default()
            },
        ))
        .await,
        "role is empty",
    );
    refused(
        api.org(edit(
            "Reviewer",
            Changes {
                name: Some("Tester".into()),
                ..Changes::default()
            },
        ))
        .await,
        "already exists",
    );
    refused(
        api.org(edit(
            "Reviewer",
            Changes {
                backends: Some(vec!["gemini".into()]),
                ..Changes::default()
            },
        ))
        .await,
        "not a backend",
    );
    refused(
        api.org(edit(
            "Reviewer",
            Changes {
                models: Some(BTreeMap::from([("gemini".into(), "x".into())])),
                ..Changes::default()
            },
        ))
        .await,
        "not a backend",
    );
    refused(
        api.org(edit(
            "Reviewer",
            Changes {
                instructions: Some("x".repeat(INSTRUCTIONS_BUDGET + 1)),
                ..Changes::default()
            },
        ))
        .await,
        "over their budget",
    );
    refused(
        api.org(edit(
            LEAD_NAME,
            Changes {
                role: Some("reviewer".into()),
                ..Changes::default()
            },
        ))
        .await,
        "lead's role",
    );
    let missing = api.org(update("Nobody")).await;
    assert!(matches!(missing, Err(ApiError::NotFound(_))), "{missing:?}");
    let OrgReply::Agent { agent } = api
        .org(edit(
            LEAD_NAME,
            Changes {
                name: Some("Chief".into()),
                backends: Some(vec!["codex".into()]),
                ..Changes::default()
            },
        ))
        .await
        .unwrap()
    else {
        panic!("expected an agent");
    };
    assert_eq!(agent.backends, [BackendKind::Codex]);
    assert_eq!(agent.name, "Chief", "the lead may be renamed");
}

#[tokio::test]
async fn delete_refuses_the_lead_a_wrong_name_and_open_tasks() {
    let hub = TestHub::start().await;
    let agent = create(&hub, "Reviewer").await;
    let general = hub.db.general_project().await.unwrap();
    hub.db
        .bind(&NewBinding {
            project_id: general.id,
            agent_id: agent.id,
            mode: Mode::OnRequest,
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();
    let task = hub
        .db
        .create_task(
            None,
            &NewTask {
                project_id: general.id,
                agent_id: agent.id,
                title: "review".into(),
                goal: "x".into(),
                repositories: Vec::new(),
                parent_task_id: None,
                depends_on: Vec::new(),
                origin: TaskOrigin::User,
                status: TaskStatus::Queued,
                priority: 0,
                created_by_call: None,
                created_at: hub.clock.now(),
            },
        )
        .await
        .unwrap();

    refused(hub.api.org(delete(LEAD_NAME, LEAD_NAME)).await, "lead");
    refused(
        hub.api.org(delete("Reviewer", "reviewer")).await,
        "type the agent's name",
    );
    refused(
        hub.api.org(delete("Reviewer", "Reviewer")).await,
        &task.id.to_string(),
    );

    hub.db
        .cancel_task(None, task.id, hub.clock.now())
        .await
        .unwrap();
    let reply = hub.api.org(delete("Reviewer", "Reviewer")).await.unwrap();
    assert_eq!(
        reply,
        OrgReply::AgentDeleted {
            agent: "Reviewer".into()
        }
    );
    assert!(
        hub.db
            .bindings(general.id)
            .await
            .unwrap()
            .iter()
            .all(|b| b.agent_id != agent.id)
    );
    let reply = hub
        .api
        .overview(OverviewRequest::Task { task: task.id })
        .await
        .unwrap();
    let OverviewReply::Task { task } = reply else {
        panic!("expected a task, got {reply:?}");
    };
    assert_eq!(
        task.summary.agent, "Reviewer (deleted)",
        "history keeps the name"
    );
    // The name is free for a new agent.
    create(&hub, "Reviewer").await;
}
