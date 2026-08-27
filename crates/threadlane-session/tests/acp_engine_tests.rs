//! End-to-end coverage for running a turn against an external ACP agent.
//!
//! [`AcpEngine`] spawns a real subprocess and talks JSON-RPC over its pipes, so
//! the parts most likely to break — the handshake, streaming in order,
//! permission round-trips, and cancellation reaching the agent — only exist
//! once a process is on the other end. These drive `acp_stub_agent`, a fixture
//! binary in this crate, so the coverage does not depend on a third-party agent
//! being installed.
#![cfg(feature = "test-support")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use threadlane_session::acp::{AcpAgentConfig, AcpScope, AcpSettings};
use threadlane_session::acp_runtime::{generate_title, AcpEngine};
use threadlane_session::permission::{PermissionDecision, PermissionHandle};
use threadlane_session::{AgentEvent, ImageAttachment, ReasoningEffort};
use tokio::sync::broadcast;

const STUB: &str = env!("CARGO_BIN_EXE_acp_stub_agent");

/// Writes a global `acp.json` pointing at the stub in the given mode.
fn configure_stub(global_dir: &Path, mode: &str) {
    configure_stub_with_env(global_dir, mode, None)
}

fn configure_stub_with_env(global_dir: &Path, mode: &str, cancel_marker: Option<&Path>) {
    let mut config = AcpAgentConfig::from_command_line("Stub", STUB, AcpScope::Global)
        .expect("the fixture path is a usable command line");
    config.id = "stub".to_string();
    // Serializing through the real settings file keeps this honest about what
    // the engine actually reads at run time.
    let mut json = serde_json::to_value(&config).unwrap();
    let mut env = serde_json::json!({ "THREADLANE_STUB_MODE": mode });
    if let Some(marker) = cancel_marker {
        env["THREADLANE_STUB_CANCEL_MARKER"] = serde_json::json!(marker.display().to_string());
    }
    json["env"] = env;
    let file = serde_json::json!({ "agents": [json] });
    std::fs::create_dir_all(global_dir).unwrap();
    std::fs::write(
        global_dir.join("acp.json"),
        serde_json::to_vec_pretty(&file).unwrap(),
    )
    .unwrap();

    // The engine reads through AcpSettings, so prove the file round-trips.
    assert_eq!(AcpSettings::load_global(Some(global_dir)).len(), 1);
}

/// A permission handle wired to answer with `decision`.
///
/// `request_external` blocks on a UI answering `PermissionRequested`, so the
/// test has to play the part of that UI.
fn responding_handle(
    decision: Option<PermissionDecision>,
    events: &broadcast::Sender<AgentEvent>,
) -> PermissionHandle {
    let handle = PermissionHandle::for_tests(std::env::temp_dir());
    handle.set_interactive(decision.is_some());
    if let Some(decision) = decision {
        let handle_for_task = handle.clone();
        let mut rx = events.subscribe();
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                if let AgentEvent::PermissionRequested { request } = event {
                    handle_for_task.resolve(&request.id, decision);
                }
            }
        });
    }
    handle
}

fn collect(rx: &mut broadcast::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

fn text_of(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageUpdate {
                text_delta: Some(text),
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn work_dir(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join("workspace")
}

fn setup(mode: &str) -> (tempfile::TempDir, AcpEngine) {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let work = work_dir(&temp);
    std::fs::create_dir_all(&work).unwrap();
    configure_stub(&global, mode);
    let engine = AcpEngine::new(Some(global), work);
    (temp, engine)
}

#[tokio::test]
async fn a_turn_streams_the_agents_output_in_order() {
    let (_temp, mut engine) = setup("stream");
    let (tx, mut rx) = broadcast::channel(64);
    let permissions = responding_handle(None, &tx);

    engine
        .run_turn(
            "stub",
            "hi",
            &[],
            ReasoningEffort::Medium,
            &tx,
            &permissions,
        )
        .await
        .expect("the turn should complete");

    let events = collect(&mut rx);
    assert!(matches!(events.first(), Some(AgentEvent::AgentStart)));
    assert!(matches!(events.last(), Some(AgentEvent::AgentEnd { .. })));

    // Chunks must arrive in the order the agent emitted them; reordered
    // streaming text is silently wrong rather than visibly broken.
    assert_eq!(text_of(&events), "hello world");

    let reasoning: String = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageUpdate {
                reasoning_delta: Some(text),
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, "thinking");

    let started = events.iter().any(|event| {
        matches!(event, AgentEvent::ToolExecutionStart { name, .. } if name == "Read main.rs")
    });
    let finished = events.iter().any(|event| {
        matches!(event, AgentEvent::ToolExecutionEnd { result, .. }
            if result.content == "fn main() {}" && !result.is_error)
    });
    assert!(started, "expected a tool start, got {events:#?}");
    assert!(finished, "expected a tool end, got {events:#?}");

    engine.shutdown().await;
}

#[tokio::test]
async fn another_sessions_updates_never_reach_this_turn() {
    let (_temp, mut engine) = setup("stream");
    let (tx, mut rx) = broadcast::channel(64);
    let permissions = responding_handle(None, &tx);

    engine
        .run_turn(
            "stub",
            "hi",
            &[],
            ReasoningEffort::Medium,
            &tx,
            &permissions,
        )
        .await
        .unwrap();

    assert!(
        !text_of(&collect(&mut rx)).contains("LEAKED"),
        "an update for a different session id must be dropped"
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn the_conversation_is_reused_across_turns() {
    let (_temp, mut engine) = setup("stream");
    let (tx, mut rx) = broadcast::channel(128);
    let permissions = responding_handle(None, &tx);

    engine
        .run_turn(
            "stub",
            "first",
            &[],
            ReasoningEffort::Medium,
            &tx,
            &permissions,
        )
        .await
        .unwrap();
    let first = collect(&mut rx);
    engine
        .run_turn(
            "stub",
            "second",
            &[],
            ReasoningEffort::Medium,
            &tx,
            &permissions,
        )
        .await
        .unwrap();
    let second = collect(&mut rx);

    // Both turns produce output, and the second reuses the session the first
    // opened; a fresh session per turn would drop the agent's context.
    assert_eq!(text_of(&first), "hello world");
    assert_eq!(text_of(&second), "hello world");
    engine.shutdown().await;
}

#[tokio::test]
async fn a_granted_permission_reaches_the_agent() {
    let (_temp, mut engine) = setup("permission");
    let (tx, mut rx) = broadcast::channel(64);
    let permissions = responding_handle(Some(PermissionDecision::AllowOnce), &tx);

    engine
        .run_turn(
            "stub",
            "run it",
            &[],
            ReasoningEffort::Medium,
            &tx,
            &permissions,
        )
        .await
        .unwrap();

    let events = collect(&mut rx);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::PermissionRequested { .. })),
        "the user should have been prompted, got {events:#?}"
    );
    // The agent echoes the option id it received, so this asserts the answer
    // actually crossed the wire rather than just being computed locally.
    assert_eq!(text_of(&events), "permission:yes");
    engine.shutdown().await;
}

#[tokio::test]
async fn a_denied_permission_reaches_the_agent() {
    let (_temp, mut engine) = setup("permission");
    let (tx, mut rx) = broadcast::channel(64);
    let permissions = responding_handle(Some(PermissionDecision::Deny), &tx);

    engine
        .run_turn(
            "stub",
            "run it",
            &[],
            ReasoningEffort::Medium,
            &tx,
            &permissions,
        )
        .await
        .unwrap();

    assert_eq!(text_of(&collect(&mut rx)), "permission:no");
    engine.shutdown().await;
}

#[tokio::test]
async fn without_a_ui_a_permission_request_is_refused() {
    let (_temp, mut engine) = setup("permission");
    let (tx, mut rx) = broadcast::channel(64);
    // Non-interactive: nobody can consent, so nothing may be granted.
    let permissions = responding_handle(None, &tx);

    engine
        .run_turn(
            "stub",
            "run it",
            &[],
            ReasoningEffort::Medium,
            &tx,
            &permissions,
        )
        .await
        .unwrap();

    let text = text_of(&collect(&mut rx));
    assert_eq!(
        text, "permission:no",
        "an unattended client must deny rather than allow"
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn cancelling_a_turn_tells_the_agent_to_stop() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let work = work_dir(&temp);
    std::fs::create_dir_all(&work).unwrap();
    // The stub touches this file when `session/cancel` arrives, which is a
    // per-test signal rather than global process state, so this stays correct
    // while the other tests run in parallel.
    let marker = temp.path().join("cancelled");
    configure_stub_with_env(&global, "cancel", Some(&marker));

    // The engine is held outside the cancelled task, exactly as it is in the
    // app: the turn runs behind a lock on a long-lived agent, so aborting the
    // turn releases the lock without tearing down the connection. Moving the
    // engine into the task instead would kill the subprocess on abort and the
    // agent would never get to see the cancel.
    let engine = std::sync::Arc::new(tokio::sync::Mutex::new(AcpEngine::new(Some(global), work)));

    let (tx, mut rx) = broadcast::channel(64);
    let permissions = responding_handle(None, &tx);

    // Aborting the task drops the turn future, which is the only path a
    // cancelled turn has to send `session/cancel`.
    let engine_for_turn = engine.clone();
    let turn = tokio::spawn(async move {
        let _ = engine_for_turn
            .lock()
            .await
            .run_turn(
                "stub",
                "work forever",
                &[],
                ReasoningEffort::Medium,
                &tx,
                &permissions,
            )
            .await;
    });

    // Wait for the agent to be mid-turn before cancelling.
    let started = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(AgentEvent::MessageUpdate {
                text_delta: Some(text),
                ..
            }) = rx.recv().await
            {
                if text == "working" {
                    return;
                }
            }
        }
    })
    .await;
    assert!(started.is_ok(), "the agent never started the turn");

    turn.abort();

    let stopped = tokio::time::timeout(Duration::from_secs(15), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    assert!(
        stopped.is_ok(),
        "the agent was never sent session/cancel; aborting the task only stops \
         Threadlane listening, so the agent would keep working"
    );

    engine.lock().await.shutdown().await;
}

#[tokio::test]
async fn an_agent_without_image_support_still_receives_the_prompt() {
    let (_temp, mut engine) = setup("no_images");
    let (tx, mut rx) = broadcast::channel(64);
    let permissions = responding_handle(None, &tx);

    engine
        .run_turn(
            "stub",
            "describe this",
            &[ImageAttachment {
                display_name: "shot.png".into(),
                data_url: "data:image/png;base64,AAAA".into(),
            }],
            ReasoningEffort::Medium,
            &tx,
            &permissions,
        )
        .await
        .unwrap();

    // The agent echoes what it received: the question must survive even though
    // the attachment could not be sent.
    let text = text_of(&collect(&mut rx));
    assert!(text.starts_with("echo:describe this"), "got {text:?}");
    assert!(text.contains("shot.png"), "got {text:?}");
    engine.shutdown().await;
}

#[tokio::test]
async fn a_missing_agent_reports_a_usable_error() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let work = work_dir(&temp);
    std::fs::create_dir_all(&work).unwrap();
    configure_stub(&global, "stream");
    let mut engine = AcpEngine::new(Some(global), work);

    let (tx, mut rx) = broadcast::channel(16);
    let permissions = responding_handle(None, &tx);

    let error = engine
        .run_turn(
            "not-configured",
            "hi",
            &[],
            ReasoningEffort::Medium,
            &tx,
            &permissions,
        )
        .await
        .expect_err("an unconfigured agent id cannot run");
    assert!(error.contains("not-configured"), "got {error}");

    // The UI leaves the generating state on AgentError, so a failure that
    // reports nothing would hang the composer.
    assert!(
        collect(&mut rx)
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentError { .. })),
        "a failed turn must report an error event"
    );
}

#[tokio::test]
async fn a_title_comes_from_the_selected_agent() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let work = work_dir(&temp);
    std::fs::create_dir_all(&work).unwrap();
    configure_stub(&global, "stream");

    let title = generate_title(Some(global), work, "stub", "add retries to the uploader")
        .await
        .expect("the agent should answer with a title");

    // Only the agent's message text becomes the title; its thoughts and tool
    // output must not leak into a session name.
    assert_eq!(title, "hello world");
}

#[tokio::test]
async fn a_title_from_an_unconfigured_agent_fails_rather_than_hanging() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let work = work_dir(&temp);
    std::fs::create_dir_all(&work).unwrap();
    configure_stub(&global, "stream");

    let error = generate_title(Some(global), work, "nope", "anything")
        .await
        .expect_err("an unconfigured agent id cannot name a session");
    assert!(error.contains("nope"), "got {error}");
}

/// Writes a project-scoped `acp.json` so a test never reads the developer's
/// real `~/.threadlane` config.
fn configure_project_stub(work_dir: &Path, mode: &str) {
    let dir = work_dir.join(".threadlane");
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = AcpAgentConfig::from_command_line("Stub", STUB, AcpScope::Project).unwrap();
    config.id = "stub".to_string();
    let mut json = serde_json::to_value(&config).unwrap();
    json["env"] = serde_json::json!({ "THREADLANE_STUB_MODE": mode });
    std::fs::write(
        dir.join("acp.json"),
        serde_json::to_vec_pretty(&serde_json::json!({ "agents": [json] })).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn an_acp_turn_is_journaled_so_the_transcript_survives_a_reload() {
    use threadlane_session::harness::{read_transcript_page, TranscriptItem};
    use threadlane_session::{AgentMessage, CodingAgent, CodingAgentOptions};

    let temp = tempfile::tempdir().unwrap();
    let work = work_dir(&temp);
    std::fs::create_dir_all(&work).unwrap();
    configure_project_stub(&work, "stream");

    let session_file = work.join(".threadlane/sessions/session_test.jsonl");
    std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();

    let mut agent = CodingAgent::new(CodingAgentOptions {
        // An ACP agent authenticates itself, so there is no key to supply.
        api_key: String::new(),
        account_id: None,
        model: "acp/stub".into(),
        work_dir: work.clone(),
        session_file: Some(session_file.clone()),
        system_prompt: Default::default(),
        agent_config: None,
        coding_config: None,
    });

    let result = agent.handle_input_with_images("hi", Vec::new()).await;
    assert!(result.is_none(), "the turn should succeed, got {result:?}");

    // Reading the journal back is what the UI does when a session is reopened,
    // so this is the check that a named session is not an empty one.
    let page = read_transcript_page(&session_file, None, 100).unwrap();
    let messages: Vec<_> = page
        .items
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::Message(message) => Some(message),
            _ => None,
        })
        .collect();

    let prompt = messages
        .iter()
        .find(|message| matches!(message, AgentMessage::User { .. }))
        .expect("the user prompt must be journaled");
    let AgentMessage::User { content } = prompt else {
        unreachable!()
    };
    assert_eq!(content, "hi");

    let reply = messages
        .iter()
        .find(|message| matches!(message, AgentMessage::Assistant { .. }))
        .expect("the agent's reply must be journaled");
    let AgentMessage::Assistant { content, .. } = reply else {
        unreachable!()
    };
    assert_eq!(content.as_deref(), Some("hello world"));
}

#[tokio::test]
async fn the_reasoning_picker_reaches_the_agent() {
    let (_temp, mut engine) = setup("config");
    let (tx, mut rx) = broadcast::channel(64);
    let permissions = responding_handle(None, &tx);

    engine
        .run_turn("stub", "hi", &[], ReasoningEffort::High, &tx, &permissions)
        .await
        .unwrap();

    // The agent echoes the settings it is actually holding, so this asserts
    // the picker crossed the wire rather than being set locally and dropped.
    assert_eq!(text_of(&collect(&mut rx)), "model:default effort:high");

    // A later turn re-applies the current pick: the picker can change between
    // turns, and the agent would otherwise keep the first one forever.
    engine
        .run_turn(
            "stub",
            "again",
            &[],
            ReasoningEffort::Low,
            &tx,
            &permissions,
        )
        .await
        .unwrap();
    assert_eq!(text_of(&collect(&mut rx)), "model:default effort:low");

    engine.shutdown().await;
}

#[tokio::test]
async fn the_agents_model_is_reported_once_it_connects() {
    let (_temp, mut engine) = setup("config");
    let (tx, _rx) = broadcast::channel(64);
    let permissions = responding_handle(None, &tx);

    // Nothing to report before a session exists; the agent names its own model.
    assert_eq!(engine.model_label("stub"), None);

    engine
        .run_turn(
            "stub",
            "hi",
            &[],
            ReasoningEffort::Medium,
            &tx,
            &permissions,
        )
        .await
        .unwrap();

    // The agent names the concrete model in its description; the option name
    // ("Default (recommended)") does not say what is actually running.
    assert_eq!(engine.model_label("stub").as_deref(), Some("Opus 4.8"));
    engine.shutdown().await;
}

#[tokio::test]
async fn an_agent_without_an_effort_setting_still_runs() {
    // The "stream" stub exposes no config options at all. Failing to apply a
    // setting the agent does not have must not fail the turn.
    let (_temp, mut engine) = setup("stream");
    let (tx, mut rx) = broadcast::channel(64);
    let permissions = responding_handle(None, &tx);

    engine
        .run_turn("stub", "hi", &[], ReasoningEffort::Max, &tx, &permissions)
        .await
        .expect("the turn should still complete");
    assert_eq!(text_of(&collect(&mut rx)), "hello world");
    assert_eq!(engine.model_label("stub"), None);
    engine.shutdown().await;
}

#[tokio::test]
async fn the_picker_lists_the_agents_settings_without_running_a_turn() {
    use threadlane_session::{config_option_for, ACP_CONFIG_CATEGORY_MODEL};

    let (_temp, mut engine) = setup("config");
    let (tx, _rx) = broadcast::channel(64);
    let permissions = responding_handle(None, &tx);

    // Nothing to offer before connecting: the agent defines its own settings
    // and sends them on session/new.
    assert!(engine.user_config_options("stub").is_empty());

    let options = engine
        .ensure_connected("stub", &tx, &permissions)
        .await
        .expect("connecting should list the agent's settings");

    let model = config_option_for(&options, ACP_CONFIG_CATEGORY_MODEL)
        .expect("the agent advertises a model setting");
    assert_eq!(model.current_value(), Some("default"));
    assert!(model.has_choice("sonnet"));

    engine.shutdown().await;
}

#[tokio::test]
async fn the_effort_setting_is_hidden_from_the_picker() {
    use threadlane_session::{config_option_for, ACP_CONFIG_CATEGORY_EFFORT};

    let (_temp, mut engine) = setup("config");
    let (tx, _rx) = broadcast::channel(64);
    let permissions = responding_handle(None, &tx);

    let options = engine
        .ensure_connected("stub", &tx, &permissions)
        .await
        .unwrap();

    // The reasoning picker already owns effort and re-applies it every turn.
    // A second control for it would let the two disagree, with the reasoning
    // picker silently winning.
    assert!(config_option_for(&options, ACP_CONFIG_CATEGORY_EFFORT).is_none());
    assert!(
        !options.is_empty(),
        "the other settings must still be shown"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn picking_a_setting_applies_it_on_the_agent() {
    use threadlane_session::{config_option_for, ACP_CONFIG_CATEGORY_MODEL};

    let (_temp, mut engine) = setup("config");
    let (tx, mut rx) = broadcast::channel(64);
    let permissions = responding_handle(None, &tx);

    let options = engine
        .set_config_option("stub", "model", "sonnet", &tx, &permissions)
        .await
        .expect("the agent offers this value");

    // The returned set is the agent's own answer, not an assumption.
    assert_eq!(
        config_option_for(&options, ACP_CONFIG_CATEGORY_MODEL).and_then(|o| o.current_value()),
        Some("sonnet")
    );

    // And the agent is actually holding it: it echoes its settings back.
    engine
        .run_turn(
            "stub",
            "hi",
            &[],
            ReasoningEffort::Medium,
            &tx,
            &permissions,
        )
        .await
        .unwrap();
    assert_eq!(text_of(&collect(&mut rx)), "model:sonnet effort:medium");

    engine.shutdown().await;
}

#[tokio::test]
async fn a_value_the_agent_does_not_offer_is_refused_rather_than_sent() {
    let (_temp, mut engine) = setup("config");
    let (tx, mut rx) = broadcast::channel(64);
    let permissions = responding_handle(None, &tx);

    engine
        .ensure_connected("stub", &tx, &permissions)
        .await
        .unwrap();

    // Refusing locally matters because agents do not reliably reject: Claude
    // Code silently coerces an unknown model id onto its nearest known alias,
    // so a value sent blind would report success while running something else.
    let error = engine
        .set_config_option("stub", "model", "gpt-4o", &tx, &permissions)
        .await
        .expect_err("a value outside the agent's own options cannot be applied");
    assert!(error.contains("gpt-4o"), "got {error}");

    let error = engine
        .set_config_option("stub", "not-a-setting", "x", &tx, &permissions)
        .await
        .expect_err("an unknown setting cannot be applied");
    assert!(error.contains("not-a-setting"), "got {error}");

    // A refused change must leave the agent exactly as it was.
    engine
        .run_turn(
            "stub",
            "hi",
            &[],
            ReasoningEffort::Medium,
            &tx,
            &permissions,
        )
        .await
        .unwrap();
    assert_eq!(text_of(&collect(&mut rx)), "model:default effort:medium");

    engine.shutdown().await;
}

#[tokio::test]
async fn a_control_label_is_the_option_name_not_its_description() {
    use threadlane_session::{config_option_for, ACP_CONFIG_CATEGORY_MODE};

    let (_temp, mut engine) = setup("config");
    let (tx, _rx) = broadcast::channel(64);
    let permissions = responding_handle(None, &tx);

    let options = engine
        .ensure_connected("stub", &tx, &permissions)
        .await
        .unwrap();
    let mode = config_option_for(&options, ACP_CONFIG_CATEGORY_MODE).expect("a mode setting");

    // A mode's description is a whole sentence. Using it as a button label
    // renders "Standard behavior, prompts for dangerous operations" on a
    // control that has room for one word.
    assert_eq!(mode.current_label().as_deref(), Some("Default"));
    assert_eq!(
        mode.current_detail_label().as_deref(),
        Some("Standard behavior, prompts for dangerous operations")
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn the_agent_persona_setting_is_hidden_from_the_picker() {
    let (_temp, mut engine) = setup("config");
    let (tx, _rx) = broadcast::channel(64);
    let permissions = responding_handle(None, &tx);

    let options = engine
        .ensure_connected("stub", &tx, &permissions)
        .await
        .unwrap();

    // Persona routing is the agent's own concern, and listing every installed
    // one crowds the picker without saying anything about this session.
    assert!(
        !options.iter().any(|option| option.id == "agent"),
        "the persona setting must not be offered, got {:?}",
        options.iter().map(|o| &o.id).collect::<Vec<_>>()
    );
    // It is filtered by id because the agent gives it no category at all.
    assert!(options.iter().any(|option| option.id == "mode"));
    assert!(options.iter().any(|option| option.id == "model"));

    engine.shutdown().await;
}
