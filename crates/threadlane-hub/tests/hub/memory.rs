//! The memory tools at the hub (Plan.md §8, docs/milestone-5.md §2): each
//! tool, every write versioned with its run and written once per call, the
//! scope refusals of invariant 12, and instructions protected outside a run
//! you triggered.

use serde_json::{Value as Json, json};
use threadlane_core::client::{ApiError, MemoryReply, MemoryRequest};
use threadlane_core::ids::DecisionId;
use threadlane_core::link::{Begin, HubLink, Perform, RunAssignment};
use threadlane_core::model::{LaneKind, Lease, Mode, NewAgent, NewBinding, NewProject, RunOutcome};
use threadlane_core::store::{DecisionStore, MemoryStore, OrgStore};
use threadlane_core::time::Clock as _;
use threadlane_core::tools::ToolError;
use threadlane_hub::{
    CREATE_TASK, FINISH_TASK, MEMORY_CREATE, MEMORY_DELETE, MEMORY_EDIT, MEMORY_RENAME, MEMORY_VIEW,
};

use crate::common::{TestHub, begin};

/// One run in progress, and the link it calls tools through.
struct Running<'a> {
    link: &'a dyn HubLink,
    lease: Lease,
    run: RunAssignment,
}

impl Running<'_> {
    async fn call(&self, call_id: &str, tool: &str, arguments: Json) -> Result<Json, ToolError> {
        self.link
            .call_tool(
                &self.lease,
                self.run.run_id,
                Some(call_id),
                tool,
                &arguments,
            )
            .await
    }

    async fn end(&self) -> RunOutcome {
        let outcome = self
            .link
            .complete(&self.lease, self.run.run_id, None)
            .await
            .unwrap();
        assert_eq!(self.link.begin_run(&self.lease).await.unwrap(), Begin::Idle);
        self.link.release(&self.lease).await.unwrap();
        outcome
    }
}

/// A run of General's lead triggered by a message from you.
async fn lead_run<'a>(hub: &'a TestHub, id: &str) -> Running<'a> {
    hub.send(id, "Remember this").await;
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Lead);
    Running {
        link: &*hub.link,
        lease,
        run,
    }
}

/// A started hub.
async fn hub() -> TestHub {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub
}

/// The first run of a task in General, made by the lead.
async fn task_run(hub: &TestHub) -> Running<'_> {
    let agent = hub
        .db
        .create_agent(&NewAgent::named("Engineer", "engineer"))
        .await
        .unwrap();
    hub.db
        .bind(&NewBinding {
            project_id: hub.general().await.id,
            agent_id: agent.id,
            mode: Some(Mode::OnRequest),
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();
    let lead = lead_run(hub, "m-task").await;
    lead.call(
        "toolu_c",
        CREATE_TASK,
        json!({ "title": "page", "goal": "the page is up", "agent": "Engineer" }),
    )
    .await
    .unwrap();
    lead.end().await;
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Task);
    Running {
        link: &*hub.link,
        lease,
        run,
    }
}

fn refused(result: Result<Json, ToolError>, why: &str) {
    match result {
        Err(ToolError::InvalidArguments(message)) => {
            assert!(message.contains(why), "{why:?} in {message:?}");
        }
        other => panic!("expected a refusal saying {why:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn each_memory_tool_works_and_every_write_carries_its_run() {
    let hub = hub().await;
    let run = lead_run(&hub, "m-1").await;
    let note = "# Build\nHow to build and test.\n\nRun the tests.";
    let created = run
        .call(
            "c1",
            MEMORY_CREATE,
            json!({ "path": "project/notes/build.md", "content": note }),
        )
        .await
        .unwrap();
    assert_eq!(
        created,
        json!({ "path": "project/notes/build.md", "version": 1 })
    );
    // The same call again, as after a crash, writes nothing more.
    assert_eq!(
        run.call(
            "c1",
            MEMORY_CREATE,
            json!({ "path": "project/notes/build.md", "content": note }),
        )
        .await
        .unwrap(),
        created
    );
    let viewed = run
        .call(
            "v1",
            MEMORY_VIEW,
            json!({ "path": "project/notes/build.md" }),
        )
        .await
        .unwrap();
    assert_eq!(viewed["content"], json!(note));
    assert_eq!(
        run.call(
            "v2",
            MEMORY_VIEW,
            json!({ "path": "project/notes/INDEX.md" })
        )
        .await
        .unwrap()["content"],
        json!("- [Build](build.md): How to build and test.\n"),
        "the hub wrote the index"
    );

    let edited = run
        .call(
            "e1",
            MEMORY_EDIT,
            json!({
                "path": "project/notes/build.md",
                "old_text": "Run the tests.",
                "new_text": "Run cargo test.",
            }),
        )
        .await
        .unwrap();
    assert_eq!(edited["version"], json!(2));
    refused(
        run.call(
            "e2",
            MEMORY_EDIT,
            json!({ "path": "project/notes/build.md", "old_text": "absent", "new_text": "x" }),
        )
        .await,
        "does not occur",
    );
    refused(
        run.call(
            "e3",
            MEMORY_EDIT,
            json!({ "path": "project/notes/build.md", "old_text": "t", "new_text": "x" }),
        )
        .await,
        "times",
    );

    let renamed = run
        .call(
            "r1",
            MEMORY_RENAME,
            json!({ "from": "project/notes/build.md", "to": "project/notes/testing.md" }),
        )
        .await
        .unwrap();
    assert_eq!(
        renamed,
        json!({
            "from": "project/notes/build.md",
            "to": "project/notes/testing.md",
            "version": 1,
        })
    );
    run.call(
        "c2",
        MEMORY_CREATE,
        json!({ "path": "space/notes/you.md", "content": "# You\nPrefer short replies." }),
    )
    .await
    .unwrap();
    assert_eq!(
        run.call("v3", MEMORY_VIEW, json!({ "path": "project" }))
            .await
            .unwrap(),
        json!({ "files": ["project/notes/INDEX.md", "project/notes/testing.md"] })
    );
    assert_eq!(
        run.call("v4", MEMORY_VIEW, json!({ "path": "space" }))
            .await
            .unwrap(),
        json!({ "files": ["space/notes/INDEX.md", "space/notes/you.md"] })
    );

    assert_eq!(
        run.call(
            "d1",
            MEMORY_DELETE,
            json!({ "path": "project/notes/testing.md" })
        )
        .await
        .unwrap(),
        json!({ "path": "project/notes/testing.md", "deleted": true })
    );
    refused(
        run.call(
            "v5",
            MEMORY_VIEW,
            json!({ "path": "project/notes/testing.md" }),
        )
        .await,
        "no file or directory",
    );
    // The instructions file and the fixed directories exist before anything
    // is written to them.
    assert_eq!(
        run.call(
            "v6",
            MEMORY_VIEW,
            json!({ "path": "project/instructions.md" })
        )
        .await
        .unwrap()["content"],
        json!("")
    );
    assert_eq!(
        run.call("v7", MEMORY_VIEW, json!({ "path": "project/repos" }))
            .await
            .unwrap(),
        json!({ "files": [] })
    );
    refused(
        run.call(
            "d2",
            MEMORY_DELETE,
            json!({ "path": "project/notes/testing.md" }),
        )
        .await,
        "no file at project/notes/testing.md",
    );
    refused(
        run.call(
            "c3",
            MEMORY_CREATE,
            json!({ "path": "project/notes/INDEX.md", "content": "# Mine\nForged." }),
        )
        .await,
        "written by the system",
    );
    refused(
        run.call(
            "c4",
            MEMORY_CREATE,
            json!({ "path": "project/notes/bare.md", "content": "no description" }),
        )
        .await,
        "title line",
    );
    refused(
        run.call(
            "c5",
            MEMORY_CREATE,
            json!({ "path": "project/instructions.md", "content": "x".repeat(9000) }),
        )
        .await,
        "project/instructions.md would be 9000 bytes",
    );
    assert_eq!(run.end().await, RunOutcome::Silent);

    for path in [
        "projects/1/notes/build.md",
        "projects/1/notes/testing.md",
        "projects/1/notes/INDEX.md",
        "space/notes/you.md",
    ] {
        let history = hub.db.history(path).await.unwrap();
        assert!(!history.is_empty(), "{path}");
        assert!(
            history
                .iter()
                .all(|version| version.written_by_run == Some(run.run.run_id)),
            "{path}: {history:?}"
        );
    }
    assert_eq!(
        hub.db
            .history("projects/1/notes/build.md")
            .await
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn a_task_run_has_the_memory_tools_too() {
    let hub = hub().await;
    let run = task_run(&hub).await;
    run.call(
        "c1",
        MEMORY_CREATE,
        json!({
            "path": "project/tasks/T-0001.md",
            "content": "# T-0001: the page\nThe form is done; tests are left.",
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        run.call("v1", MEMORY_VIEW, json!({ "path": "project/tasks" }))
            .await
            .unwrap(),
        json!({ "files": ["project/tasks/INDEX.md", "project/tasks/T-0001.md"] })
    );
    run.call(
        "c2",
        MEMORY_CREATE,
        json!({ "path": "space/notes/style.md", "content": "# Style\nShort sentences." }),
    )
    .await
    .unwrap();
    assert_eq!(run.end().await, RunOutcome::Silent);
}

#[tokio::test]
async fn a_task_agent_writes_only_its_own_progress_file_and_finishes_only_once_it_exists() {
    let hub = hub().await;
    let run = task_run(&hub).await;
    let progress = |title: &str| format!("# {title}\nWhat is done and what is left.");
    let finish = json!({ "summary": "Done." });
    refused(
        run.call("f1", FINISH_TASK, finish.clone()).await,
        "project/tasks/T-0001.md does not exist",
    );

    let other = "another task's file";
    for (id, tool, arguments) in [
        (
            "o1",
            MEMORY_CREATE,
            json!({ "path": "project/tasks/T-0002.md", "content": progress(other) }),
        ),
        (
            "o2",
            MEMORY_CREATE,
            json!({ "path": "project/tasks/T-0001/extra.md", "content": progress(other) }),
        ),
        (
            "o3",
            MEMORY_RENAME,
            json!({ "from": "project/notes/a.md", "to": "project/tasks/T-0002.md" }),
        ),
        (
            "o4",
            MEMORY_RENAME,
            json!({ "from": "project/tasks/T-0002.md", "to": "project/notes/a.md" }),
        ),
        (
            "o5",
            MEMORY_DELETE,
            json!({ "path": "project/tasks/T-0002.md" }),
        ),
    ] {
        refused(
            run.call(id, tool, arguments).await,
            "only your own task's progress file, project/tasks/T-0001.md",
        );
    }

    let written = run
        .call(
            "p1",
            MEMORY_CREATE,
            json!({ "path": "project/tasks/T-0001.md", "content": progress("T-0001") }),
        )
        .await
        .unwrap();
    assert_eq!(written["progress_written"], json!(true), "{written}");
    let note = run
        .call(
            "n1",
            MEMORY_CREATE,
            json!({ "path": "project/notes/page.md", "content": progress("Page") }),
        )
        .await
        .unwrap();
    assert_eq!(note.get("progress_written"), None, "{note}");
    assert_eq!(
        run.call("f2", FINISH_TASK, finish).await.unwrap()["finished"],
        json!(true)
    );
    run.end().await;

    // The lead reads every task's file, and writing one is not its progress.
    let lead = lead_run(&hub, "m-2").await;
    let viewed = lead
        .call(
            "v1",
            MEMORY_VIEW,
            json!({ "path": "project/tasks/T-0001.md" }),
        )
        .await
        .unwrap();
    assert_eq!(viewed["content"], json!(progress("T-0001")));
    let edited = lead
        .call(
            "e1",
            MEMORY_EDIT,
            json!({
                "path": "project/tasks/T-0001.md",
                "old_text": "what is left",
                "new_text": "nothing left",
            }),
        )
        .await
        .unwrap();
    assert_eq!(edited.get("progress_written"), None, "{edited}");
}

#[tokio::test]
async fn a_run_never_reaches_memory_outside_its_project_and_the_spaces_notes() {
    let hub = hub().await;
    let alpha = hub
        .db
        .create_project(&NewProject {
            name: "Alpha".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let lead = hub
        .db
        .lead_binding(hub.general().await.id)
        .await
        .unwrap()
        .unwrap();
    hub.db
        .bind(&NewBinding {
            project_id: alpha.id,
            agent_id: lead.agent_id,
            mode: Some(Mode::OnRequest),
            is_lead: true,
            max_parallel: 1,
        })
        .await
        .unwrap();
    hub.api
        .send_message(&threadlane_core::client::UserMessage {
            project: Some("Alpha".into()),
            text: "Remember the secret".into(),
            images: Vec::new(),
            client_message_id: "a-1".into(),
            task: None,
        })
        .await
        .unwrap();
    let (lease, assignment) = begin(&*hub.link).await;
    assert_eq!(assignment.project_id, alpha.id);
    let in_alpha = Running {
        link: &*hub.link,
        lease,
        run: assignment,
    };
    in_alpha
        .call(
            "a1",
            MEMORY_CREATE,
            json!({ "path": "project/notes/secret.md", "content": "# Secret\nAlpha's." }),
        )
        .await
        .unwrap();
    in_alpha.end().await;
    let secret = format!("projects/{}/notes/secret.md", alpha.id);
    assert!(hub.db.read(&secret).await.unwrap().is_some());

    let run = lead_run(&hub, "m-1").await;
    run.call(
        "s1",
        MEMORY_CREATE,
        json!({ "path": "space/instructions.md", "content": "Be brief." }),
    )
    .await
    .unwrap();
    run.call(
        "n1",
        MEMORY_CREATE,
        json!({ "path": "project/notes/mine.md", "content": "# Mine\nGeneral's." }),
    )
    .await
    .unwrap();
    let elsewhere = [
        secret.clone(),
        format!("project/../{secret}"),
        "projects".to_owned(),
        format!("/{secret}"),
        "notes/secret.md".to_owned(),
        "space/../projects/2/notes/secret.md".to_owned(),
    ];
    for (n, path) in elsewhere.iter().enumerate() {
        let id = |verb: &str| format!("{verb}{n}");
        // Reading or listing it.
        refused(
            run.call(&id("v"), MEMORY_VIEW, json!({ "path": path }))
                .await,
            "",
        );
        // Writing, editing, or deleting it.
        refused(
            run.call(
                &id("c"),
                MEMORY_CREATE,
                json!({ "path": path, "content": "# Overwritten\nBy General." }),
            )
            .await,
            "",
        );
        refused(
            run.call(
                &id("e"),
                MEMORY_EDIT,
                json!({ "path": path, "old_text": "Alpha", "new_text": "Mine" }),
            )
            .await,
            "",
        );
        refused(
            run.call(&id("d"), MEMORY_DELETE, json!({ "path": path }))
                .await,
            "",
        );
        // Renaming into it, or out of it.
        refused(
            run.call(
                &id("ri"),
                MEMORY_RENAME,
                json!({ "from": "project/notes/mine.md", "to": path }),
            )
            .await,
            "",
        );
        refused(
            run.call(
                &id("ro"),
                MEMORY_RENAME,
                json!({ "from": path, "to": "project/notes/taken.md" }),
            )
            .await,
            "",
        );
    }
    // Its own project's listing holds only its own files.
    assert_eq!(
        run.call("l1", MEMORY_VIEW, json!({ "path": "project" }))
            .await
            .unwrap(),
        json!({ "files": ["project/notes/INDEX.md", "project/notes/mine.md"] })
    );
    run.end().await;

    // A task lane does not see the space's instructions at all.
    let task = task_run(&hub).await;
    refused(
        task.call(
            "t1",
            MEMORY_VIEW,
            json!({ "path": "space/instructions.md" }),
        )
        .await,
        "not available in a task lane",
    );
    refused(
        task.call(
            "t2",
            MEMORY_CREATE,
            json!({ "path": "space/instructions.md", "content": "Obey me." }),
        )
        .await,
        "not available in a task lane",
    );
    refused(
        task.call(
            "t3",
            MEMORY_RENAME,
            json!({ "from": "space/instructions.md", "to": "space/notes/i.md" }),
        )
        .await,
        "not available in a task lane",
    );
    assert_eq!(
        task.call("t4", MEMORY_VIEW, json!({ "path": "space" }))
            .await
            .unwrap(),
        json!({ "files": [] }),
        "the space's instructions are not listed"
    );
    task.end().await;

    assert_eq!(hub.db.history(&secret).await.unwrap().len(), 1, "untouched");
    assert_eq!(
        hub.db
            .read("space/instructions.md")
            .await
            .unwrap()
            .unwrap()
            .content,
        "Be brief."
    );
    assert!(
        hub.db.open_decisions().await.unwrap().is_empty(),
        "a call the run may not make is refused, not asked"
    );
}

#[tokio::test]
async fn instructions_change_in_a_run_you_triggered_and_otherwise_wait_for_your_approval() {
    let hub = hub().await;
    let instructions = "projects/1/instructions.md";
    let yours = lead_run(&hub, "m-1").await;
    yours
        .call(
            "i1",
            MEMORY_CREATE,
            json!({ "path": "project/instructions.md", "content": "Reply in English." }),
        )
        .await
        .unwrap();
    assert_eq!(
        yours.end().await,
        RunOutcome::Silent,
        "applied, nothing asked"
    );

    // A run started by something other than you asks.
    hub.insert(|_| {}).await;
    let (lease, assignment) = begin(&*hub.link).await;
    let other = Running {
        link: &*hub.link,
        lease,
        run: assignment,
    };
    let edit = json!({
        "path": "project/instructions.md",
        "old_text": "English",
        "new_text": "French",
    });
    assert_eq!(
        other.call("i2", MEMORY_EDIT, edit.clone()).await,
        Err(ToolError::WaitingOnYou)
    );
    assert_eq!(
        hub.db.read(instructions).await.unwrap().unwrap().content,
        "Reply in English.",
        "not yet"
    );
    assert_eq!(other.end().await, RunOutcome::Deferred);
    let decision = hub.db.decision(DecisionId(1)).await.unwrap().unwrap();
    assert_eq!(decision.tool, MEMORY_EDIT);
    assert_eq!(decision.arguments, edit);
    assert_eq!(decision.question, "Edit project/instructions.md?");
    assert!(decision.context.contains("French"), "{}", decision.context);

    hub.api
        .decisions(threadlane_core::client::DecisionRequest::Answer {
            decision: decision.id,
            option: "approve".into(),
            note: None,
        })
        .await
        .unwrap();
    let (lease, after) = begin(&*hub.link).await;
    assert_eq!(after.perform, [decision.id]);
    let performed = hub
        .link
        .begin_perform(&lease, after.run_id, decision.id)
        .await
        .unwrap();
    let Perform::Recorded { outcome } = &performed else {
        panic!("the hub performs a memory change itself: {performed:?}");
    };
    assert_eq!(outcome["performed"], json!(true));
    assert_eq!(
        outcome["summary"],
        json!("project/instructions.md is written.")
    );
    assert_eq!(
        hub.link
            .begin_perform(&lease, after.run_id, decision.id)
            .await
            .unwrap(),
        performed,
        "performed once"
    );
    let history = hub.db.history(instructions).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].content, "Reply in French.");
    assert_eq!(history[1].written_by_run, Some(after.run_id));
    Running {
        link: &*hub.link,
        lease,
        run: after,
    }
    .end()
    .await;
}

/// The Memory capability's answer as the files it lists, by path.
fn paths(reply: MemoryReply) -> Vec<String> {
    match reply {
        MemoryReply::Files { files } => files.into_iter().map(|file| file.path).collect(),
        other => panic!("not a listing: {other:?}"),
    }
}

#[tokio::test]
async fn you_see_a_projects_memory_as_its_lead_does_and_change_it_as_yourself() {
    let hub = TestHub::start().await;
    let other = hub
        .db
        .create_project(&NewProject {
            name: "Other".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let now = hub.clock.now();
    hub.db
        .write_by_owner(
            &format!("projects/{}/notes/secret.md", other.id.get()),
            "# S\nSecret.",
            now,
        )
        .await
        .unwrap();
    hub.db
        .write_by_owner("space/notes/style.md", "# Style\nShort.", now)
        .await
        .unwrap();
    let memory = |request| hub.api.memory(request);
    let write = |path: &str, content: &str| MemoryRequest::Write {
        project: None,
        path: path.to_owned(),
        content: content.to_owned(),
    };

    // You may write instructions: you are the owner (Plan.md §8.3).
    let written = memory(write("project/instructions.md", "Be brief."))
        .await
        .unwrap();
    let MemoryReply::File { file } = written else {
        panic!("not a file: {written:?}");
    };
    assert_eq!(
        (file.path.as_str(), file.version, file.written_by_run),
        ("project/instructions.md", 1, None)
    );
    memory(write("project/notes/build.md", "# Build\nRun make."))
        .await
        .unwrap();
    assert_eq!(
        paths(memory(MemoryRequest::List { project: None }).await.unwrap()),
        [
            "space/notes/INDEX.md",
            "space/notes/style.md",
            "project/instructions.md",
            "project/notes/INDEX.md",
            "project/notes/build.md",
        ],
        "the other project's files are not listed"
    );
    assert_eq!(
        memory(MemoryRequest::Read {
            project: None,
            path: "project/notes/INDEX.md".to_owned(),
        })
        .await
        .unwrap(),
        MemoryReply::File {
            file: hub
                .db
                .read(&format!(
                    "projects/{}/notes/INDEX.md",
                    hub.general().await.id.get()
                ))
                .await
                .unwrap()
                .map(|mut file| {
                    file.path = "project/notes/INDEX.md".to_owned();
                    file
                })
                .unwrap()
        }
    );

    // The store's refusals, with paths as you name them.
    let over = "x".repeat(threadlane_core::memory::INSTRUCTIONS_BUDGET + 1);
    for (path, content, why) in [
        (
            "project/instructions.md",
            over.as_str(),
            "project/instructions.md would be",
        ),
        ("project/notes/a.md", "no description", "title line"),
        (
            "project/notes/INDEX.md",
            "# A\nForged.",
            "written by the system",
        ),
    ] {
        let refused = memory(write(path, content)).await;
        assert!(
            matches!(&refused, Err(ApiError::Invalid(message)) if message.contains(why)
                && !message.contains("projects/")),
            "{path}: {refused:?}"
        );
    }

    memory(MemoryRequest::Delete {
        project: None,
        path: "project/notes/build.md".to_owned(),
    })
    .await
    .unwrap();
    let history = memory(MemoryRequest::History {
        project: None,
        path: "project/notes/build.md".to_owned(),
    })
    .await
    .unwrap();
    let MemoryReply::History { versions } = history else {
        panic!("not a history: {history:?}");
    };
    assert_eq!(
        versions
            .iter()
            .map(|v| (v.path.as_str(), v.version, v.deleted, v.written_by_run))
            .collect::<Vec<_>>(),
        [
            ("project/notes/build.md", 1, false, None),
            ("project/notes/build.md", 2, true, None)
        ]
    );
    assert!(matches!(
        memory(MemoryRequest::Read {
            project: None,
            path: "project/notes/build.md".to_owned(),
        })
        .await,
        Err(ApiError::NotFound(_))
    ));
}

#[tokio::test]
async fn no_memory_request_reaches_another_project() {
    // Invariant 12: every path maps into the named project or the space.
    let hub = TestHub::start().await;
    let other = hub
        .db
        .create_project(&NewProject {
            name: "Other".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let theirs = format!("projects/{}/notes/secret.md", other.id.get());
    hub.db
        .write_by_owner(&theirs, "# S\nSecret.", hub.clock.now())
        .await
        .unwrap();
    for path in [
        theirs.clone(),
        format!("project/../projects/{}/notes/secret.md", other.id.get()),
        "/space/notes/a.md".to_owned(),
        "space/other.md".to_owned(),
        "notes/a.md".to_owned(),
    ] {
        for request in [
            MemoryRequest::Read {
                project: None,
                path: path.clone(),
            },
            MemoryRequest::History {
                project: None,
                path: path.clone(),
            },
            MemoryRequest::Write {
                project: None,
                path: path.clone(),
                content: "# X\nOverwritten.".to_owned(),
            },
            MemoryRequest::Delete {
                project: None,
                path: path.clone(),
            },
        ] {
            let answer = hub.api.memory(request.clone()).await;
            assert!(
                matches!(answer, Err(ApiError::Invalid(_))),
                "{request:?}: {answer:?}"
            );
        }
    }
    assert_eq!(hub.db.history(&theirs).await.unwrap().len(), 1);
    assert!(matches!(
        hub.api
            .memory(MemoryRequest::List {
                project: Some("Nowhere".to_owned())
            })
            .await,
        Err(ApiError::NotFound(_))
    ));
}
