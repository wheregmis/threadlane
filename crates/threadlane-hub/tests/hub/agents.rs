//! Editing and deleting agents through the Organization capability.

use std::collections::BTreeMap;
use std::sync::Arc;

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
    skills: Option<Vec<threadlane_core::model::SkillBinding>>,
}

fn update(agent: &str) -> OrgRequest {
    edit(agent, Changes::default())
}

fn edit(agent: &str, changes: Changes) -> OrgRequest {
    OrgRequest::UpdateAgent {
        skills: changes.skills,
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
    refused(
        api.org(edit(
            "Reviewer",
            Changes {
                skills: Some(vec![threadlane_core::model::SkillBinding {
                    backend: BackendKind::Antigravity,
                    name: "/review".into(),
                }]),
                ..Changes::default()
            },
        ))
        .await,
        "invalid or duplicate",
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
            mode: Some(Mode::OnRequest),
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

#[tokio::test]
async fn a_lead_s_saved_skills_stay_out_of_its_runs() {
    use threadlane_core::model::SkillBinding;
    let hub = TestHub::start().await;
    hub.hello().await; // Only Claude is usable on the runner.
    let bindings = vec![
        SkillBinding {
            backend: BackendKind::Codex,
            name: "codex-only".into(),
        },
        SkillBinding {
            backend: BackendKind::Claude,
            name: "claude-only".into(),
        },
    ];
    let change = edit(
        LEAD_NAME,
        Changes {
            backends: Some(vec!["codex".into(), "claude".into()]),
            skills: Some(bindings.clone()),
            ..Changes::default()
        },
    );

    let OrgReply::Agent { agent } = hub.api.org(change).await.unwrap() else {
        panic!("agent")
    };
    assert_eq!(
        hub.db.agent(agent.id).await.unwrap().unwrap().skills,
        bindings
    );
    hub.send("bound", "hello").await;
    let (_, assignment) = crate::common::begin(&*hub.link).await;
    assert_eq!(assignment.backend, BackendKind::Claude);
    // The lead runs without native tools, so its skills stay out of the run.
    assert!(assignment.skills.is_empty());
    let clear = edit(
        LEAD_NAME,
        Changes {
            skills: Some(Vec::new()),
            ..Changes::default()
        },
    );
    hub.api.org(clear).await.unwrap();
    assert!(
        hub.db
            .agent(agent.id)
            .await
            .unwrap()
            .unwrap()
            .skills
            .is_empty()
    );
}

#[tokio::test]
async fn one_backend_failing_to_list_skills_leaves_the_others_listed() {
    use threadlane_core::backend::BackendError;
    use threadlane_core::model::SkillBinding;
    use threadlane_testkit::ScriptedBackend;
    let hub = TestHub::start().await;
    let clock = Arc::new(hub.clock.clone());
    let claude = ScriptedBackend::new(BackendKind::Claude, clock.clone());
    claude.set_skills(Ok(vec!["review".into()]));
    let codex = ScriptedBackend::new(BackendKind::Codex, clock);
    codex.set_skills(Err(BackendError::Failed("no home".into())));
    let hub = TestHub::around(
        hub.core
            .with_backends(vec![Arc::new(claude), Arc::new(codex)]),
        hub.clock,
    )
    .await;

    let reply = hub.api.org(OrgRequest::InstalledSkills).await.unwrap();
    let OrgReply::InstalledSkills {
        skills,
        unavailable,
    } = reply
    else {
        panic!("{reply:?}")
    };
    assert_eq!(
        skills,
        [SkillBinding {
            backend: BackendKind::Claude,
            name: "review".into(),
        }]
    );
    assert_eq!(unavailable, [BackendKind::Codex]);
}

#[tokio::test]
async fn a_task_run_falling_back_from_codex_keeps_only_claude_s_skills() {
    use threadlane_core::model::{LaneKind, SkillBinding};
    let hub = TestHub::start().await;
    let agent = create(&hub, "Reviewer").await;
    let bindings = vec![
        SkillBinding {
            backend: BackendKind::Codex,
            name: "codex-only".into(),
        },
        SkillBinding {
            backend: BackendKind::Claude,
            name: "claude-only".into(),
        },
        SkillBinding {
            backend: BackendKind::Antigravity,
            name: "antigravity-only".into(),
        },
    ];
    let change = edit(
        "Reviewer",
        Changes {
            backends: Some(vec!["codex".into(), "claude".into()]),
            skills: Some(bindings),
            ..Changes::default()
        },
    );
    hub.api.org(change).await.unwrap();
    let general = hub.db.general_project().await.unwrap();
    hub.db
        .bind(&NewBinding {
            project_id: general.id,
            agent_id: agent.id,
            mode: Some(Mode::OnRequest),
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();
    hub.db
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
    hub.hello().await; // Only Claude is usable on the runner.

    let (_, assignment) = crate::common::begin(&*hub.link).await;
    assert_eq!(assignment.lane_kind, LaneKind::Task);
    assert_eq!(assignment.backend, BackendKind::Claude);
    assert_eq!(assignment.skills, ["claude-only"]);
}

#[tokio::test]
async fn binding_a_claude_skill_claude_may_not_invoke_is_rejected() {
    use threadlane_backend_claude::{ClaudeBackend, ClaudeConfig};
    use threadlane_core::model::SkillBinding;
    let account = tempfile::tempdir().unwrap();
    let folder = account.path().join("skills/deploy");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("SKILL.md"),
        "---\ndisable-model-invocation: true\n---\nbody",
    )
    .unwrap();
    let hub = TestHub::start().await;
    let claude = ClaudeBackend::new(ClaudeConfig {
        config_dir: Some(account.path().into()),
        ..ClaudeConfig::default()
    });
    let hub = TestHub::around(hub.core.with_backends(vec![Arc::new(claude)]), hub.clock).await;
    create(&hub, "Reviewer").await;
    let change = edit(
        "Reviewer",
        Changes {
            skills: Some(vec![SkillBinding {
                backend: BackendKind::Claude,
                name: "deploy".into(),
            }]),
            ..Changes::default()
        },
    );
    let error = hub.api.org(change).await.unwrap_err();
    assert_eq!(
        error,
        ApiError::Invalid("claude skill \"deploy\" sets disable-model-invocation and can only be started by slash command; remove that line from its SKILL.md or unbind it".into())
    );
}
