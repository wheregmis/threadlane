//! Off at run start (invariant 14) and the Organization capability
//! (Plan.md §11.2).

use std::sync::Arc;

use threadlane_core::client::{ApiError, BindingSummary, OrgReply, OrgRequest, UserMessage};
use threadlane_core::ids::RunId;
use threadlane_core::link::Begin;
use threadlane_core::model::{EventStatus, Mode, NewProject};
use threadlane_core::runtime::NoHooks;
use threadlane_core::store::{InboxStore, OrgStore, SessionStore};
use threadlane_hub::{LEAD_NAME, LEAD_ROLE, OFF_REASON};

use crate::common::{Reply, TestHub, play};

fn message(project: Option<&str>, id: &str) -> UserMessage {
    UserMessage {
        project: project.map(str::to_owned),
        text: "are you there?".to_owned(),
        client_message_id: id.to_owned(),
        task: None,
    }
}

async fn org(hub: &TestHub, request: OrgRequest) -> Result<OrgReply, ApiError> {
    hub.api.org(request).await
}

fn set_mode(project: Option<&str>, agent: &str, mode: Mode) -> OrgRequest {
    OrgRequest::SetMode {
        project: project.map(str::to_owned),
        agent: agent.to_owned(),
        mode,
    }
}

fn lead(mode: Mode) -> BindingSummary {
    BindingSummary {
        agent: LEAD_NAME.to_owned(),
        role: LEAD_ROLE.to_owned(),
        mode,
        is_lead: true,
    }
}

/// Grants the only ready lane and expects `begin_run` to start nothing.
async fn begins_idle(hub: &TestHub) {
    let lease = hub.link.request_work().await.unwrap().expect("granted");
    assert_eq!(hub.link.begin_run(&lease).await.unwrap(), Begin::Idle);
    hub.link.release(&lease).await.unwrap();
    assert!(hub.db.run(RunId(1)).await.unwrap().is_none(), "no run row");
}

#[tokio::test]
async fn a_message_to_a_project_whose_lead_is_off_is_refused_and_not_stored() {
    let hub = TestHub::start().await;
    let reply = org(&hub, set_mode(None, LEAD_NAME, Mode::Off)).await;
    assert_eq!(
        reply,
        Ok(OrgReply::Binding {
            binding: lead(Mode::Off)
        })
    );

    let refused = hub.api.send_message(&message(None, "m-1")).await;
    match refused {
        Err(ApiError::Invalid(reason)) => assert!(reason.contains("General"), "{reason}"),
        other => panic!("expected Invalid, got {other:?}"),
    }
    let lane = hub.general().await.lead_lane_id;
    assert!(hub.db.pending_events(lane).await.unwrap().is_empty());

    // Back on, the same message goes through.
    org(&hub, set_mode(None, LEAD_NAME, Mode::OnRequest))
        .await
        .unwrap();
    assert!(!hub.send("m-1", "are you there?").await.duplicate);
}

#[tokio::test]
async fn events_pending_when_the_lead_is_turned_off_are_skipped_without_a_run() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let sent = hub.send("m-1", "before it was turned off").await.event_id;
    let inserted = hub.insert(|_| {}).await;
    let binding = hub.db.lead_binding(hub.general().await.id).await.unwrap();
    hub.db
        .set_binding_mode(binding.unwrap().id, Mode::Off)
        .await
        .unwrap();

    begins_idle(&hub).await;
    for id in [sent, inserted] {
        let event = hub.event(id).await;
        assert_eq!(event.status, EventStatus::Skipped);
        assert_eq!(event.status_reason.as_deref(), Some(OFF_REASON));
    }
}

#[tokio::test]
async fn a_project_ceiling_of_off_stops_its_lead_too() {
    let hub = TestHub::start().await;
    let quiet = hub
        .db
        .create_project(&NewProject {
            name: "Quiet".into(),
            mode_ceiling: Some(Mode::Off),
            ..NewProject::default()
        })
        .await
        .unwrap();
    // The lead binding comes at start; its own mode stays On request.
    let hub = hub.restart(Arc::new(NoHooks)).await;
    hub.hello().await;
    assert_eq!(
        hub.db.lead_binding(quiet.id).await.unwrap().unwrap().mode,
        Mode::OnRequest
    );

    let refused = hub.api.send_message(&message(Some("Quiet"), "m-1")).await;
    assert!(matches!(refused, Err(ApiError::Invalid(_))), "{refused:?}");

    let event = hub
        .insert(|event| {
            event.project_id = quiet.id;
            event.lane_id = quiet.lead_lane_id;
        })
        .await;
    begins_idle(&hub).await;
    let event = hub.event(event).await;
    assert_eq!(event.status, EventStatus::Skipped);
    assert_eq!(event.status_reason.as_deref(), Some(OFF_REASON));
}

#[tokio::test]
async fn a_project_created_through_the_api_has_a_lead_and_runs_at_once() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let reply = org(
        &hub,
        OrgRequest::CreateProject {
            name: "  Alpha ".into(),
            description: "the first one".into(),
        },
    )
    .await
    .unwrap();
    let OrgReply::Project { project } = reply else {
        panic!("expected a project, got {reply:?}");
    };
    assert_eq!(project.name, "Alpha", "the name is trimmed");
    assert!(!project.is_general);
    assert_eq!(hub.api.projects().await.unwrap().len(), 2);
    assert_eq!(
        org(
            &hub,
            OrgRequest::Bindings {
                project: Some("Alpha".into())
            }
        )
        .await,
        Ok(OrgReply::Bindings {
            bindings: vec![lead(Mode::OnRequest)]
        })
    );

    let accepted = hub
        .api
        .send_message(&message(Some("Alpha"), "m-1"))
        .await
        .unwrap();
    assert_eq!(accepted.project_id, project.id);
    let played = play(&*hub.link, Reply::Silent).await.unwrap();
    assert_eq!(played.only_run().project_id, project.id);
    assert_eq!(hub.event(accepted.event_id).await.status, EventStatus::Done);
}

#[tokio::test]
async fn project_names_must_be_present_and_unique() {
    let hub = TestHub::start().await;
    let create = |name: &str| OrgRequest::CreateProject {
        name: name.to_owned(),
        description: String::new(),
    };
    for name in ["", "   "] {
        let refused = org(&hub, create(name)).await;
        assert!(matches!(refused, Err(ApiError::Invalid(_))), "{refused:?}");
    }
    org(&hub, create("Alpha")).await.unwrap();
    for name in ["Alpha", " Alpha", "General"] {
        match org(&hub, create(name)).await {
            Err(ApiError::Invalid(reason)) => {
                assert!(reason.contains("already exists"), "{reason}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }
    assert_eq!(hub.api.projects().await.unwrap().len(), 2);
}

#[tokio::test]
async fn agents_are_listed_and_created_with_unique_names_and_a_role() {
    let hub = TestHub::start().await;
    let create = |name: &str, role: &str| OrgRequest::CreateAgent {
        name: name.to_owned(),
        role: role.to_owned(),
        instructions: "Review every change.".to_owned(),
    };
    let reply = org(&hub, create(" Reviewer ", "reviewer")).await.unwrap();
    let OrgReply::Agent { agent } = reply else {
        panic!("expected an agent, got {reply:?}");
    };
    assert_eq!(agent.name, "Reviewer");
    assert_eq!(agent.role, "reviewer");
    assert_eq!(agent.instructions, "Review every change.");

    for (name, role) in [("", "reviewer"), ("Tester", " "), (LEAD_NAME, "lead")] {
        let refused = org(&hub, create(name, role)).await;
        assert!(matches!(refused, Err(ApiError::Invalid(_))), "{refused:?}");
    }
    let Ok(OrgReply::Agents { agents }) = org(&hub, OrgRequest::Agents).await else {
        panic!("expected agents");
    };
    let names: Vec<_> = agents.iter().map(|agent| agent.name.as_str()).collect();
    assert_eq!(names, [LEAD_NAME, "Reviewer"]);
}

#[tokio::test]
async fn set_mode_changes_a_binding_or_creates_one() {
    let hub = TestHub::start().await;
    org(
        &hub,
        OrgRequest::CreateAgent {
            name: "Reviewer".into(),
            role: "reviewer".into(),
            instructions: String::new(),
        },
    )
    .await
    .unwrap();
    let reviewer = |mode| BindingSummary {
        agent: "Reviewer".into(),
        role: "reviewer".into(),
        mode,
        is_lead: false,
    };

    // Not bound yet: a non-lead binding with the mode, stored as given.
    assert_eq!(
        org(&hub, set_mode(None, "Reviewer", Mode::Autonomous)).await,
        Ok(OrgReply::Binding {
            binding: reviewer(Mode::Autonomous)
        })
    );
    let general = hub.general().await;
    let bound = hub.db.bindings(general.id).await.unwrap();
    assert_eq!(bound.len(), 2);
    assert_eq!(bound[1].max_parallel, 1);

    // Bound: the same binding changes.
    org(&hub, set_mode(None, "Reviewer", Mode::Off))
        .await
        .unwrap();
    assert_eq!(
        org(&hub, OrgRequest::Bindings { project: None }).await,
        Ok(OrgReply::Bindings {
            bindings: vec![lead(Mode::OnRequest), reviewer(Mode::Off)]
        })
    );

    for request in [
        set_mode(None, "Nobody", Mode::Off),
        set_mode(Some("Nowhere"), "Reviewer", Mode::Off),
        OrgRequest::Bindings {
            project: Some("Nowhere".into()),
        },
    ] {
        let missing = org(&hub, request).await;
        assert!(matches!(missing, Err(ApiError::NotFound(_))), "{missing:?}");
    }
}

fn add_repository(project: Option<&str>, name: &str, remote: &str) -> OrgRequest {
    OrgRequest::AddRepository {
        project: project.map(str::to_owned),
        name: name.to_owned(),
        remote_url: remote.to_owned(),
        default_branch: None,
    }
}

#[tokio::test]
async fn repositories_are_registered_per_project_and_a_remote_only_once_in_the_space() {
    let hub = TestHub::start().await;
    org(
        &hub,
        OrgRequest::CreateProject {
            name: "Payments".into(),
            description: String::new(),
        },
    )
    .await
    .unwrap();
    let Ok(OrgReply::Repository { repository }) = org(
        &hub,
        add_repository(Some("Payments"), "api", "git@example.com:me/api.git"),
    )
    .await
    else {
        panic!("registered");
    };
    assert_eq!(repository.name, "api");
    assert_eq!(repository.default_branch, "main", "the default branch");
    let Ok(OrgReply::Repository { repository: docs }) = org(
        &hub,
        OrgRequest::AddRepository {
            project: None,
            name: "docs".into(),
            remote_url: "git@example.com:me/docs.git".into(),
            default_branch: Some("trunk".into()),
        },
    )
    .await
    else {
        panic!("registered in General");
    };
    assert_eq!(docs.default_branch, "trunk");

    let again = org(
        &hub,
        add_repository(None, "api", "git@example.com:me/api.git"),
    )
    .await;
    assert_eq!(
        again,
        Err(ApiError::Invalid(
            "the remote git@example.com:me/api.git is already registered in this space, as \
             repository \"api\" of project \"Payments\""
                .into()
        ))
    );
    let same_name = org(
        &hub,
        add_repository(Some("Payments"), "api", "git@example.com:me/other.git"),
    )
    .await;
    assert!(
        matches!(&same_name, Err(ApiError::Invalid(message)) if message.contains("already has a repository named")),
        "{same_name:?}"
    );
    for (name, remote) in [("-x", "r1"), ("a/b", "r2"), ("ok", "--upload-pack=x")] {
        let refused = org(&hub, add_repository(None, name, remote)).await;
        assert!(
            matches!(refused, Err(ApiError::Invalid(_))),
            "{name} {refused:?}"
        );
    }
    let bad_branch = org(
        &hub,
        OrgRequest::AddRepository {
            project: None,
            name: "web".into(),
            remote_url: "git@example.com:me/web.git".into(),
            default_branch: Some("--orphan".into()),
        },
    )
    .await;
    assert!(
        matches!(bad_branch, Err(ApiError::Invalid(_))),
        "{bad_branch:?}"
    );

    let Ok(OrgReply::Repositories { repositories }) = org(
        &hub,
        OrgRequest::Repositories {
            project: Some("Payments".into()),
        },
    )
    .await
    else {
        panic!("listed");
    };
    assert_eq!(repositories, vec![repository]);
    let Ok(OrgReply::Repositories { repositories }) =
        org(&hub, OrgRequest::Repositories { project: None }).await
    else {
        panic!("listed");
    };
    assert_eq!(repositories, vec![docs]);
}
