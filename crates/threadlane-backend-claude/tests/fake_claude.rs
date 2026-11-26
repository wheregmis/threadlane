//! The driver against fake `claude` executables: shell scripts that print
//! recorded stream-json lines and record how they were started.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use tempfile::TempDir;
use threadlane_backend_claude::{ClaudeBackend, ClaudeConfig};
use threadlane_core::backend::{
    AgentBackend, AskRequest, BackendError, BackendEvent, BackendRun, DENY_RULES, NativeSession,
    Probe, RunRequest,
};
use threadlane_core::model::BackendKind;
use threadlane_core::tools::{Tier, ToolDefinition, ToolEndpoint, ToolError};
use threadlane_testkit::script::write_executable;

const SESSION: &str = "5f0c2a8e-1b7d-4c39-8e0f-2d6a9b4c7e15";

/// Separates invocations in the recorded argv file.
const END: &str = "<<end>>";

#[derive(Debug)]
struct Endpoint;

#[async_trait]
impl ToolEndpoint for Endpoint {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "send_message".to_owned(),
            description: "Sends a message".to_owned(),
            input_schema: json!({"type": "object"}),
            tier: Tier::Local,
            safe_to_repeat: false,
        }]
    }

    async fn call(&self, _: Option<&str>, _: &str, _: Json) -> Result<Json, ToolError> {
        Ok(json!({"sent": true}))
    }
}

/// Runs the new script once, doing nothing, until Linux stops refusing it
/// with ETXTBSY: a child that another test forks while the script is being
/// written holds it open for writing until that child execs.
fn wait_until_executable(program: &Path) {
    for _ in 0..100 {
        match std::process::Command::new(program)
            .env("FAKE_CLAUDE_READY", "1")
            .status()
        {
            Err(error) if error.raw_os_error() == Some(26) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("fake claude does not start: {error}"),
            Ok(_) => return,
        }
    }
    panic!("fake claude stayed busy");
}

/// A fake `claude` in its own temp directory, next to the files it records.
struct Fake {
    dir: TempDir,
    work: TempDir,
}

impl Fake {
    /// Writes `body` as a `/bin/sh` script; `@DIR@` becomes the record directory.
    fn new(body: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let script = format!(
            "#!/bin/sh\n[ -n \"$FAKE_CLAUDE_READY\" ] && exit 0\n{}\n",
            body.replace("@DIR@", &dir.path().display().to_string())
        );
        let program = dir.path().join("claude");
        write_executable(&program, &script);
        wait_until_executable(&program);
        Self {
            dir,
            work: tempfile::tempdir().expect("temp dir"),
        }
    }

    fn program(&self) -> PathBuf {
        self.dir.path().join("claude")
    }

    fn backend(&self) -> ClaudeBackend {
        ClaudeBackend::new(ClaudeConfig {
            program: self.program(),
            config_dir: None,
        })
    }

    fn request(&self, session: NativeSession) -> RunRequest {
        RunRequest {
            working_dir: self.work.path().to_owned(),
            prompt: "Say hello.".to_owned(),
            images: Vec::new(),
            skills: Vec::new(),
            appended_instructions: String::new(),
            model: None,
            session,
            tools: None,
            deny_rules: Vec::new(),
            remove_builtin_tools: false,
            wall_time_limit: Duration::from_secs(60),
            max_turns: None,
            single_use: false,
        }
    }

    fn record(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name))
            .unwrap_or_else(|error| panic!("record {name}: {error}"))
    }

    /// The argument lists of every invocation, in order.
    fn invocations(&self) -> Vec<Vec<String>> {
        let argv = self.record("argv");
        let mut invocations = Vec::new();
        let mut current = Vec::new();
        for line in argv.lines() {
            if line == END {
                invocations.push(std::mem::take(&mut current));
            } else {
                current.push(line.to_owned());
            }
        }
        invocations
    }
}

const RECORD_ARGV: &str =
    "{ for arg in \"$@\"; do printf '%s\\n' \"$arg\"; done; echo '<<end>>'; } >> @DIR@/argv";

fn init(status: &str) -> String {
    json!({
        "type": "system", "subtype": "init", "session_id": SESSION, "cwd": "/somewhere",
        "tools": ["mcp__threadlane__send_message"],
        "mcp_servers": [{"name": "threadlane", "status": status}],
    })
    .to_string()
}

fn success(text: &str) -> String {
    json!({
        "type": "result", "subtype": "success", "is_error": false, "result": text,
        "session_id": SESSION, "num_turns": 1, "total_cost_usd": 0.001,
        "usage": {"input_tokens": 3, "output_tokens": 2}, "api_error_status": null,
    })
    .to_string()
}

/// A script body that prints `lines` on stdout.
fn prints(lines: &[String]) -> String {
    format!("cat <<'LINES'\n{}\nLINES", lines.join("\n"))
}

async fn collect(run: &mut dyn BackendRun) -> Vec<BackendEvent> {
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), run.next_event())
            .await
            .expect("the run produces events without hanging");
        match event {
            Some(BackendEvent::TurnBoundary) => {}
            Some(event) => events.push(event),
            None => break,
        }
    }
    assert_eq!(run.next_event().await, None, "the stream stays ended");
    events
}

fn finished(text: &str) -> BackendEvent {
    BackendEvent::Finished {
        final_text: Some(text.to_owned()),
        usage: json!({
            "usage": {"input_tokens": 3, "output_tokens": 2},
            "total_cost_usd": 0.001,
            "num_turns": 1,
        }),
    }
}

fn started() -> BackendEvent {
    BackendEvent::SessionStarted {
        native_session_id: SESSION.to_owned(),
    }
}

#[tokio::test]
async fn ask_reads_the_structured_result_with_tools_disabled() {
    let fake = Fake::new(
        r#"printf '%s\n' "$@" > "@DIR@/args"
echo '{"type":"result","subtype":"success","structured_output":{"decision":"allow"}}'"#,
    );
    assert_eq!(fake.backend().kind(), BackendKind::Claude);
    assert_eq!(
        fake.backend().ask(question()).await.unwrap(),
        json!({"decision":"allow"})
    );
    let args = fake.record("args");
    for flag in [
        "--tools\n\n",
        "--strict-mcp-config\n",
        "--output-format\njson\n",
        "--json-schema\n{}\n",
        "--model\nsmall\n",
        "--\n?\n",
    ] {
        assert!(args.contains(flag), "{args}");
    }
}

fn question() -> AskRequest {
    AskRequest {
        question: "?".into(),
        answer_schema: json!({}),
        model: Some("small".into()),
        timeout: Duration::from_secs(2),
    }
}

#[tokio::test]
async fn ask_times_out_and_kills_the_child() {
    let request = AskRequest {
        timeout: Duration::from_millis(50),
        ..question()
    };
    assert_eq!(
        Fake::new("exec sleep 5").backend().ask(request).await,
        Err(BackendError::Timeout)
    );
}

#[tokio::test]
async fn ask_recognizes_a_usage_limit_on_a_failed_exit() {
    let fake = Fake::new(
        r#"echo '{"type":"result","subtype":"error","api_error_status":429}'
exit 1"#,
    );
    assert_eq!(
        fake.backend().ask(question()).await,
        Err(BackendError::UsageLimit { resets_at: None })
    );
}

#[tokio::test]
async fn ask_refuses_garbage_and_missing_structured_output() {
    for body in [
        "echo garbage",
        r#"echo '{"type":"result","subtype":"success","result":"allow"}'"#,
    ] {
        assert!(matches!(
            Fake::new(body).backend().ask(question()).await,
            Err(BackendError::Failed(_))
        ));
    }
}

#[tokio::test]
async fn recorded_lines_become_events() {
    let lines = [
        json!({"type": "system", "subtype": "hook_started", "hook_name": "SessionStart"}).to_string(),
        init("connected"),
        json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "Sending."}]}}).to_string(),
        json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "mcp__threadlane__send_message", "input": {"text": "hi"}}]}}).to_string(),
        json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed", "resetsAt": 1_794_892_800}}).to_string(),
        "this is not JSON".to_owned(),
        json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": [{"type": "text", "text": "{\"sent\":true}"}]}]}}).to_string(),
        success("Sent."),
    ];
    let fake = Fake::new(&prints(&lines));
    let mut request = fake.request(NativeSession::Start {
        id: SESSION.to_owned(),
    });
    request.tools = Some(Arc::new(Endpoint));
    let mut run = fake.backend().run(request).await.expect("run starts");
    assert_eq!(
        collect(run.as_mut()).await,
        vec![
            started(),
            BackendEvent::Text {
                text: "Sending.".to_owned()
            },
            BackendEvent::ToolUse {
                call_id: Some("toolu_1".to_owned()),
                name: "send_message".to_owned(),
                input: json!({"text": "hi"}),
            },
            BackendEvent::ToolResult {
                call_id: Some("toolu_1".to_owned()),
                output: json!([{"type": "text", "text": "{\"sent\":true}"}]),
                is_error: false,
            },
            finished("Sent."),
        ]
    );
}

/// A resumed session first answers a leftover background-task notification
/// in a turn of its own, with its own `result`, before it takes our prompt:
/// the run ends on the last `result`, not on the first.
#[tokio::test]
async fn a_notification_turn_before_the_prompt_does_not_end_the_run() {
    let notification = [
        init("connected"),
        json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "No response requested."}]}}).to_string(),
        success("No response requested."),
    ];
    let prompt = [
        json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "cargo test"}}]}}).to_string(),
        json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}]}}).to_string(),
        success("Tests pass."),
    ];
    // The pause is the real turn's model call: no output for a while.
    let fake = Fake::new(&format!(
        "{}\nsleep 1\n{}",
        prints(&notification),
        prints(&prompt)
    ));
    let mut run = fake
        .backend()
        .run(fake.request(NativeSession::Resume {
            id: SESSION.to_owned(),
        }))
        .await
        .expect("run starts");
    assert_eq!(
        collect(run.as_mut()).await,
        vec![
            started(),
            BackendEvent::Text {
                text: "No response requested.".to_owned()
            },
            BackendEvent::ToolUse {
                call_id: Some("toolu_1".to_owned()),
                name: "Bash".to_owned(),
                input: json!({"command": "cargo test"}),
            },
            BackendEvent::ToolResult {
                call_id: Some("toolu_1".to_owned()),
                output: json!("ok"),
                is_error: false,
            },
            finished("Tests pass."),
        ]
    );
}

fn recorder() -> Fake {
    Fake::new(&format!(
        r#"{RECORD_ARGV}
prev=""
for arg in "$@"; do
  if [ "$prev" = "--mcp-config" ]; then
    cat "$arg" > @DIR@/mcp-config
    ls -l "$arg" > @DIR@/mcp-mode
    printf '%s' "$arg" > @DIR@/mcp-path
  fi
  prev="$arg"
done
pwd -P > @DIR@/cwd
printf '%s' "$CLAUDE_CONFIG_DIR" > @DIR@/config-dir
printf '%s' "$CLAUDE_CODE_DISABLE_BACKGROUND_TASKS" > @DIR@/no-background
printf '%s %s' "$BASH_DEFAULT_TIMEOUT_MS" "$BASH_MAX_TIMEOUT_MS" > @DIR@/bash-timeout
IFS= read -r prompt
printf '%s\n' "$prompt" > @DIR@/stdin
{}"#,
        prints(&[init("connected"), success("ok")])
    ))
}

#[tokio::test]
async fn start_passes_every_option() {
    let fake = recorder();
    let config_dir = fake.dir.path().join("account");
    let backend = ClaudeBackend::new(ClaudeConfig {
        program: fake.program(),
        config_dir: Some(config_dir.clone()),
    });
    let request = RunRequest {
        prompt: "Reply to the message.\nIt says: hello.".to_owned(),
        images: Vec::new(),
        skills: Vec::new(),
        appended_instructions: "You are the lead.".to_owned(),
        model: Some("haiku".to_owned()),
        tools: Some(Arc::new(Endpoint)),
        deny_rules: vec!["git push".to_owned(), "gh pr merge".to_owned()],
        remove_builtin_tools: true,
        max_turns: Some(3),
        ..fake.request(NativeSession::Start {
            id: SESSION.to_owned(),
        })
    };
    let mut run = backend.run(request).await.expect("run starts");
    assert_eq!(collect(run.as_mut()).await, vec![started(), finished("ok")]);

    let mcp_path = fake.record("mcp-path");
    let expected: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "bypassPermissions",
        "--session-id",
        SESSION,
        "--model",
        "haiku",
        "--append-system-prompt",
        "You are the lead.",
        "--mcp-config",
        &mcp_path,
        "--strict-mcp-config",
        "--setting-sources",
        "project",
        "--tools",
        "",
        "--disallowedTools",
        "Bash(git push:*)",
        "Bash(gh pr merge:*)",
        "--max-turns",
        "3",
    ]
    .map(str::to_owned)
    .to_vec();
    assert_eq!(fake.invocations(), vec![expected]);

    assert_eq!(
        recorded_prompts(&fake),
        ["Reply to the message.\nIt says: hello."]
    );
    let work = fake.work.path().canonicalize().expect("canonical work dir");
    assert_eq!(fake.record("cwd").trim(), work.display().to_string());
    assert_eq!(fake.record("config-dir"), config_dir.display().to_string());
    // Nothing backgrounded may outlive the headless turn.
    assert_eq!(fake.record("no-background"), "1");
    assert_eq!(fake.record("bash-timeout"), "7200000 7200000");

    let config: Json = serde_json::from_str(&fake.record("mcp-config")).expect("config is JSON");
    let server = &config["mcpServers"]["threadlane"];
    assert_eq!(server["type"], "http");
    let url = server["url"].as_str().expect("url");
    assert!(
        url.starts_with("http://127.0.0.1:") && url.ends_with("/mcp"),
        "{url}"
    );
    let authorization = server["headers"]["Authorization"].as_str().expect("header");
    let token = authorization.strip_prefix("Bearer ").expect("bearer token");
    assert_eq!(token.len(), 64);
    assert!(fake.record("mcp-mode").starts_with("-rw-------"));
    assert!(!Path::new(&mcp_path).starts_with(fake.work.path()));
    assert!(!format!("{run:?}").contains(token), "Debug leaks the token");

    drop(run);
    assert!(
        !Path::new(&mcp_path).exists(),
        "the config is removed with the run"
    );
}

/// Each of the system's deny rules (Plan.md §9.2 layer 2) reaches claude as
/// a `--disallowedTools` Bash prefix rule, `Bash(<command>:*)`, the form that
/// refuses every Bash command starting with those words.
#[tokio::test]
async fn each_deny_rule_reaches_claude_as_a_disallowed_bash_prefix() {
    let expected = DENY_RULES
        .iter()
        .map(|rule| (*rule, format!("Bash({rule}:*)")));
    for (rule, disallowed) in expected {
        let fake = recorder();
        let request = RunRequest {
            deny_rules: vec![rule.to_owned()],
            ..fake.request(NativeSession::Start {
                id: SESSION.to_owned(),
            })
        };
        let mut run = fake.backend().run(request).await.expect("run starts");
        collect(run.as_mut()).await;
        let args = fake.invocations().remove(0);
        let flag = args
            .iter()
            .position(|arg| arg == "--disallowedTools")
            .unwrap_or_else(|| panic!("{rule}: no --disallowedTools in {args:?}"));
        assert_eq!(args[flag + 1..], [disallowed], "{rule}");
    }
}

#[tokio::test]
async fn resume_passes_only_what_was_asked_for() {
    let fake = recorder();
    let mut run = fake
        .backend()
        .run(fake.request(NativeSession::Resume {
            id: SESSION.to_owned(),
        }))
        .await
        .expect("run starts");
    // Without tools the server's status in init does not matter.
    assert_eq!(collect(run.as_mut()).await, vec![started(), finished("ok")]);
    let expected: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "bypassPermissions",
        "--resume",
        SESSION,
        "--strict-mcp-config",
        "--setting-sources",
        "project",
    ]
    .map(str::to_owned)
    .to_vec();
    assert_eq!(fake.invocations(), vec![expected]);
    assert_eq!(recorded_prompts(&fake), ["Say hello."]);
    assert_eq!(fake.record("config-dir"), "");
    assert_eq!(fake.record("no-background"), "1");
}

#[tokio::test]
async fn start_on_an_existing_session_resumes_it_once() {
    let fake = Fake::new(&format!(
        r#"{RECORD_ARGV}
IFS= read -r prompt
printf '%s\n' "$prompt" >> @DIR@/stdin
for arg in "$@"; do
  if [ "$arg" = "--session-id" ]; then
    echo "Error: Session ID {SESSION} is already in use." >&2
    exit 1
  fi
done
{}"#,
        prints(&[init("connected"), success("resumed")])
    ));
    let mut run = fake
        .backend()
        .run(fake.request(NativeSession::Start {
            id: SESSION.to_owned(),
        }))
        .await
        .expect("run starts");
    assert_eq!(
        collect(run.as_mut()).await,
        vec![started(), finished("resumed")]
    );
    let invocations = fake.invocations();
    assert_eq!(invocations.len(), 2);
    assert_eq!(invocations[0][8..10], ["--session-id", SESSION]);
    assert_eq!(invocations[1][8..10], ["--resume", SESSION]);
    assert_eq!(recorded_prompts(&fake), ["Say hello.", "Say hello."]);
}

#[tokio::test]
async fn a_session_in_use_is_not_retried_after_resume_fails_too() {
    let fake = Fake::new(&format!(
        r#"{RECORD_ARGV}
echo "Error: Session ID {SESSION} is already in use." >&2
exit 1"#
    ));
    let mut run = fake
        .backend()
        .run(fake.request(NativeSession::Start {
            id: SESSION.to_owned(),
        }))
        .await
        .expect("run starts");
    let events = collect(run.as_mut()).await;
    assert!(
        matches!(events.as_slice(), [BackendEvent::Failed { error }] if error.contains("already in use")),
        "{events:?}"
    );
    assert_eq!(fake.invocations().len(), 2);
}

#[tokio::test]
async fn output_closing_without_a_result_fails_with_the_stderr_tail() {
    let fake = Fake::new(&format!(
        "{}\necho 'something broke badly' >&2\nexit 3",
        prints(&[init("connected")])
    ));
    let mut run = fake
        .backend()
        .run(fake.request(NativeSession::Start {
            id: SESSION.to_owned(),
        }))
        .await
        .expect("run starts");
    let events = collect(run.as_mut()).await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0], started());
    let BackendEvent::Failed { error } = &events[1] else {
        panic!("expected Failed, got {:?}", events[1]);
    };
    assert!(error.contains("something broke badly"), "{error}");
    assert!(error.contains('3'), "{error}");
}

#[tokio::test]
async fn an_unknown_session_is_missing() {
    let missing = json!({
        "type": "result", "subtype": "error_during_execution", "is_error": true,
        "errors": [format!("No conversation found with session ID: {SESSION}")],
    })
    .to_string();
    let fake = Fake::new(&prints(&[missing]));
    let mut run = fake
        .backend()
        .run(fake.request(NativeSession::Resume {
            id: SESSION.to_owned(),
        }))
        .await
        .expect("run starts");
    assert_eq!(
        collect(run.as_mut()).await,
        vec![BackendEvent::SessionMissing]
    );
}

fn hanging(first_line: &str) -> Fake {
    Fake::new(&format!(
        "echo $$ > @DIR@/pid\n{}\nexec sleep 600",
        prints(&[first_line.to_owned()])
    ))
}

fn pid(fake: &Fake) -> String {
    fake.record("pid").trim().to_owned()
}

/// Whether `pid` is a live process (a zombie counts as gone).
fn alive(pid: &str) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .expect("ps runs");
    let stat = String::from_utf8_lossy(&output.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

async fn wait_until_gone(pid: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while alive(pid) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "process {pid} still alive"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn stop_kills_a_hanging_claude() {
    let fake = hanging(&init("connected"));
    let mut run = fake
        .backend()
        .run(fake.request(NativeSession::Start {
            id: SESSION.to_owned(),
        }))
        .await
        .expect("run starts");
    assert_eq!(run.next_event().await, Some(started()));
    let waited = tokio::time::timeout(Duration::from_millis(300), run.next_event()).await;
    assert!(
        waited.is_err(),
        "a hanging claude produces nothing: {waited:?}"
    );
    let pid = pid(&fake);
    assert!(alive(&pid));

    tokio::time::timeout(Duration::from_secs(10), run.stop())
        .await
        .expect("stop returns promptly")
        .expect("stop succeeds");
    assert!(!alive(&pid));
    assert_eq!(run.next_event().await, None);
    run.stop().await.expect("stopping twice is fine");
}

#[tokio::test]
async fn dropping_the_run_kills_claude() {
    let fake = hanging(&init("connected"));
    let mut run = fake
        .backend()
        .run(fake.request(NativeSession::Start {
            id: SESSION.to_owned(),
        }))
        .await
        .expect("run starts");
    assert_eq!(run.next_event().await, Some(started()));
    let pid = pid(&fake);
    drop(run);
    wait_until_gone(&pid).await;
}

#[tokio::test]
async fn a_tool_server_that_did_not_connect_fails_the_run_and_kills_claude() {
    let fake = hanging(&init("failed"));
    let mut request = fake.request(NativeSession::Start {
        id: SESSION.to_owned(),
    });
    request.tools = Some(Arc::new(Endpoint));
    let mut run = fake.backend().run(request).await.expect("run starts");
    let events = collect(run.as_mut()).await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(
        matches!(&events[1], BackendEvent::Failed { error } if error.contains("did not connect"))
    );
    wait_until_gone(&pid(&fake)).await;
}

#[tokio::test]
async fn probe_reads_the_version_and_the_login() {
    let fake = Fake::new(
        r#"case "$1" in
  --version) echo "2.1.325 (Claude Code)" ;;
  auth) printf '{\n  "loggedIn": true,\n  "authMethod": "claude.ai"\n}\n' ;;
  *) exit 2 ;;
esac"#,
    );
    assert_eq!(
        fake.backend().probe().await,
        Ok(Probe {
            installed: true,
            version: Some("2.1.325".to_owned()),
            logged_in: true,
            account_label: None,
        })
    );
}

#[tokio::test]
async fn probe_reports_a_logged_out_claude() {
    let fake = Fake::new(
        r#"case "$1" in
  --version) echo "2.1.325 (Claude Code)" ;;
  auth) echo '{"loggedIn": false}'; exit 1 ;;
esac"#,
    );
    let probe = fake.backend().probe().await.expect("probe succeeds");
    assert!(probe.installed);
    assert!(!probe.logged_in);
    assert!(!probe.usable());
}

#[tokio::test]
async fn a_missing_program_is_not_installed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let work = tempfile::tempdir().expect("temp dir");
    let backend = ClaudeBackend::new(ClaudeConfig {
        program: dir.path().join("no-such-claude"),
        config_dir: None,
    });
    assert_eq!(
        backend.probe().await,
        Ok(Probe {
            installed: false,
            version: None,
            logged_in: false,
            account_label: None,
        })
    );
    let request = RunRequest {
        working_dir: work.path().to_owned(),
        prompt: String::new(),
        images: Vec::new(),
        skills: Vec::new(),
        appended_instructions: String::new(),
        model: None,
        session: NativeSession::Start {
            id: SESSION.to_owned(),
        },
        tools: None,
        deny_rules: Vec::new(),
        remove_builtin_tools: false,
        wall_time_limit: Duration::from_secs(1),
        max_turns: None,
        single_use: false,
    };
    assert_eq!(
        backend.run(request).await.map(drop),
        Err(BackendError::NotInstalled)
    );
}

fn recorded_prompts(fake: &Fake) -> Vec<String> {
    fake.record("stdin")
        .lines()
        .map(|line| {
            let input: Json = serde_json::from_str(line).unwrap();
            assert_eq!(input["type"], "user");
            assert_eq!(input["message"]["role"], "user");
            input["message"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn a_streaming_run_accepts_another_user_message_after_a_result() {
    let fake = Fake::new(&format!(
        r#"IFS= read -r first
printf '%s\n' "$first" > @DIR@/stdin
{}
IFS= read -r second
printf '%s\n' "$second" >> @DIR@/stdin
{}
IFS= read -r unexpected && exit 1
exit 0"#,
        prints(&[init("connected"), success("first answer")]),
        prints(&[success("second answer")]),
    ));
    let mut run = fake
        .backend()
        .run(fake.request(NativeSession::Start {
            id: SESSION.to_owned(),
        }))
        .await
        .unwrap();
    let events = tokio::time::timeout(Duration::from_secs(5), async {
        assert_eq!(run.next_event().await, Some(started()));
        assert_eq!(run.next_event().await, Some(BackendEvent::TurnBoundary));
        assert!(run.fold_in("One more question.").await.unwrap());
        assert_eq!(run.next_event().await, Some(BackendEvent::TurnBoundary));
        collect(run.as_mut()).await
    })
    .await
    .unwrap();
    assert_eq!(events, vec![finished("second answer")]);
    assert_eq!(
        recorded_prompts(&fake),
        ["Say hello.", "One more question."]
    );
}

#[tokio::test]
async fn personal_skills_reach_claude_stdin_and_a_removed_skill_is_skipped_with_a_warning() {
    let fake = recorder();
    let account = fake.dir.path().join("account");
    let backend = ClaudeBackend::new(ClaudeConfig {
        program: fake.program(),
        config_dir: Some(account.clone()),
    });
    for name in ["review", "tests"] {
        let folder = account.join("skills").join(name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("SKILL.md"), "private skill body").unwrap();
    }
    let mut request = fake.request(NativeSession::Start { id: SESSION.into() });
    request.skills = backend.installed_skills(None).await.unwrap();
    let mut run = backend.run(request.clone()).await.unwrap();
    collect(run.as_mut()).await;
    assert_eq!(
        recorded_prompts(&fake),
        ["Use these installed skills for this run: /review /tests\n\nSay hello."]
    );
    let mut single = request.clone();
    single.skills.truncate(1);
    let mut run = backend.run(single).await.unwrap();
    collect(run.as_mut()).await;
    assert_eq!(
        recorded_prompts(&fake),
        ["Use these installed skills for this run: /review\n\nSay hello."]
    );
    std::fs::remove_file(account.join("skills/review/SKILL.md")).unwrap();
    let mut run = backend.run(request).await.unwrap();
    let events = collect(run.as_mut()).await;
    assert_eq!(
        events[0],
        BackendEvent::Warning {
            message: "claude skill \"review\" was skipped for this run: it is not installed on this runner.".into()
        }
    );
    assert_eq!(
        recorded_prompts(&fake),
        ["Use these installed skills for this run: /tests\n\nSay hello."]
    );
    assert_eq!(fake.invocations().len(), 3);
}

#[tokio::test]
async fn a_skill_claude_may_not_invoke_is_not_offered_and_is_skipped_with_a_warning() {
    let fake = recorder();
    let account = fake.dir.path().join("account");
    let backend = ClaudeBackend::new(ClaudeConfig {
        program: fake.program(),
        config_dir: Some(account.clone()),
    });
    for (name, body) in [
        ("tests", "---\nname: tests\n---\nbody"),
        (
            "deploy",
            "---\nname: deploy\ndisable-model-invocation: true\n---\nbody",
        ),
    ] {
        let folder = account.join("skills").join(name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("SKILL.md"), body).unwrap();
    }
    assert_eq!(backend.installed_skills(None).await.unwrap(), ["tests"]);
    // Bound before the flag was added: the run skips it and says why.
    let mut request = fake.request(NativeSession::Start { id: SESSION.into() });
    request.skills = vec!["deploy".into(), "tests".into()];
    let mut run = backend.run(request).await.unwrap();
    let events = collect(run.as_mut()).await;
    assert_eq!(
        events[0],
        BackendEvent::Warning {
            message: "claude skill \"deploy\" was skipped for this run: it sets disable-model-invocation and can only be started by slash command; remove that line from its SKILL.md or unbind it.".into()
        }
    );
    assert_eq!(
        recorded_prompts(&fake),
        ["Use these installed skills for this run: /tests\n\nSay hello."]
    );
}
