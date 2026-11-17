//! The driver against fake `codex` executables: shell scripts that print
//! `codex exec --json` lines and record how they were started. The
//! conformance suite (Plan.md §16) runs against the fake that answers its
//! prompts as the real tool would, given how the driver started it.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use tempfile::TempDir;
use threadlane_backend_codex::{CodexBackend, CodexConfig, RULES_FILE};
use threadlane_core::backend::{
    AgentBackend, AskRequest, BackendError, BackendEvent, BackendRun, DENY_RULES, NativeSession,
    Probe, RunRequest,
};
use threadlane_core::model::BackendKind;
use threadlane_core::tools::{Tier, ToolDefinition, ToolEndpoint, ToolError};
use threadlane_testkit::conformance::Conformance;

/// The fake the conformance suite runs on. It records its arguments, working
/// directory, `CODEX_HOME`, whether it got the tool server's token, and the
/// deny rules file; keeps sessions; calls the system's tool with `curl`;
/// writes a file only with full access; and refuses a command only when a
/// rule in a trusted project's rules file covers it.
const FAKE: &str = r#"#!/bin/sh
prompt=$(cat)
dir=@DIR@
{ for arg in "$@"; do printf '%s\n' "$arg"; done; echo '<<end>>'; } >> "$dir/argv"
pwd -P > "$dir/cwd"
printf '%s' "$prompt" > "$dir/stdin"
printf '%s' "$CODEX_HOME" > "$dir/home"
if [ -f @RULES@ ]; then cp @RULES@ "$dir/rules"; fi
resume=0; url=""; tokvar=""; sandbox=""; approval=""; trusted=""; prev=""; last=""; before_last=""
for arg in "$@"; do
  if [ "$arg" = resume ] && [ "$prev" = exec ]; then resume=1; fi
  if [ "$prev" = -c ]; then
    case "$arg" in
      sandbox_mode=*) sandbox=${arg#*=} ;;
      approval_policy=*) approval=${arg#*=} ;;
      mcp_servers.threadlane.url=*) url=${arg#*=}; url=${url#\"}; url=${url%\"} ;;
      mcp_servers.threadlane.bearer_token_env_var=*) tokvar=${arg#*=}; tokvar=${tokvar#\"}; tokvar=${tokvar%\"} ;;
      projects=*) trusted="$arg" ;;
    esac
  fi
  before_last="$last"; last="$arg"; prev="$arg"
done
token=""
if [ -n "$tokvar" ]; then eval "token=\${$tokvar}"; printf '%s' "$token" > "$dir/token"; fi
touch "$dir/sessions"
if [ "$resume" = 1 ]; then
  id="$before_last"
  if ! grep -qxF "$id" "$dir/sessions"; then
    echo "Error: No saved session found with ID $id" >&2; exit 1
  fi
else
  n=$(wc -l < "$dir/sessions" | tr -d ' ')
  id="0199a213-81c0-7800-8aa1-00000000000$n"
  echo "$id" >> "$dir/sessions"
fi
echo "{\"type\":\"thread.started\",\"thread_id\":\"$id\"}"
echo '{"type":"turn.started"}'
case "$prompt" in *send_message*)
  echo '{"type":"item.started","item":{"id":"item_1","type":"mcp_tool_call","server":"threadlane","tool":"send_message","arguments":{"text":"hi"},"status":"in_progress"}}'
  reply=$(curl -s -X POST "$url" -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_message","arguments":{"text":"hi"}}}')
  case "$reply" in
    *'"isError":false'*) echo '{"type":"item.completed","item":{"id":"item_1","type":"mcp_tool_call","server":"threadlane","tool":"send_message","arguments":{"text":"hi"},"result":{"content":[{"type":"text","text":"sent"}]},"error":null,"status":"completed"}}' ;;
    *) echo '{"type":"item.completed","item":{"id":"item_1","type":"mcp_tool_call","server":"threadlane","tool":"send_message","arguments":{"text":"hi"},"error":{"message":"unreachable"},"status":"failed"}}' ;;
  esac
;; esac
case "$prompt" in *conformance.txt*)
  if [ "$sandbox" = '"danger-full-access"' ] && [ "$approval" = '"never"' ]; then
    echo ok > conformance.txt
    echo '{"type":"item.completed","item":{"id":"item_2","type":"command_execution","command":"echo ok > conformance.txt","aggregated_output":"","exit_code":0,"status":"completed"}}'
  else
    echo '{"type":"item.completed","item":{"id":"item_2","type":"command_execution","command":"echo ok > conformance.txt","aggregated_output":"","exit_code":null,"status":"declined"}}'
  fi
;; esac
case "$prompt" in *'`'*)
  cmd=$(printf '%s' "$prompt" | sed -n 's/^[^`]*`\([^`]*\)`.*$/\1/p')
  refused=false
  case "$trusted" in *"\"$(pwd -P)\""*)
    set -f; set -- $cmd; set +f
    for pattern in "[\"$1\", \"$2\"]" "[\"$1\", \"$2\", \"$3\"]"; do
      if grep -qF "pattern = $pattern, decision = \"forbidden\"" @RULES@; then refused=true; fi
    done
  ;; esac
  if [ "$refused" = true ]; then
    echo "{\"type\":\"item.completed\",\"item\":{\"id\":\"item_3\",\"type\":\"command_execution\",\"command\":\"$cmd\",\"aggregated_output\":\"execpolicy forbids this command\",\"exit_code\":null,\"status\":\"declined\"}}"
  else
    echo "{\"type\":\"item.completed\",\"item\":{\"id\":\"item_3\",\"type\":\"command_execution\",\"command\":\"$cmd\",\"aggregated_output\":\"USAGE [new branch]\",\"exit_code\":0,\"status\":\"completed\"}}"
  fi
;; esac
case "$prompt" in *"usage limit"*)
  echo "{\"type\":\"error\",\"message\":\"You've hit your usage limit. Try again at 3:45 PM.\"}"
  echo "{\"type\":\"turn.failed\",\"error\":{\"message\":\"You've hit your usage limit. Try again at 3:45 PM.\"}}"
  exit 1
;; esac
echo '{"type":"item.completed","item":{"id":"item_9","type":"agent_message","text":"done"}}'
echo '{"type":"turn.completed","usage":{"input_tokens":3,"cached_input_tokens":0,"output_tokens":2}}'
"#;

/// Separates invocations in the recorded argv file.
const END: &str = "<<end>>";

/// A fake `codex` in its own temp directory, next to the files it records,
/// and a working directory.
struct Fake {
    dir: TempDir,
    work: TempDir,
}

impl Fake {
    /// Writes `body` as a `/bin/sh` script; `@DIR@` becomes the record
    /// directory and `@RULES@` the deny rules file relative to the working
    /// directory.
    fn new(body: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let script = body
            .replace("@DIR@", &dir.path().display().to_string())
            .replace("@RULES@", RULES_FILE);
        let program = dir.path().join("codex");
        std::fs::write(&program, script).expect("writes fake codex");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("makes fake codex executable");
        Self {
            dir,
            work: tempfile::tempdir().expect("temp dir"),
        }
    }

    fn conforming() -> Self {
        Self::new(FAKE)
    }

    fn program(&self) -> PathBuf {
        self.dir.path().join("codex")
    }

    fn backend(&self) -> CodexBackend {
        CodexBackend::new(CodexConfig {
            program: self.program(),
            home: None,
        })
    }

    fn request(&self, session: NativeSession) -> RunRequest {
        RunRequest {
            working_dir: self.work.path().to_owned(),
            prompt: "Say hello.".to_owned(),
            appended_instructions: String::new(),
            model: None,
            session,
            tools: None,
            deny_rules: Vec::new(),
            remove_builtin_tools: false,
            wall_time_limit: Duration::from_secs(60),
            max_turns: None,
        }
    }

    fn record(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name))
            .unwrap_or_else(|error| panic!("record {name}: {error}"))
    }

    /// The argument lists of every invocation, in order.
    fn invocations(&self) -> Vec<Vec<String>> {
        let mut invocations = Vec::new();
        let mut current = Vec::new();
        for line in self.record("argv").lines() {
            if line == END {
                invocations.push(std::mem::take(&mut current));
            } else {
                current.push(line.to_owned());
            }
        }
        invocations
    }

    fn suite(&self) -> Conformance {
        Conformance {
            backend: Arc::new(self.backend()),
            model: None,
            new_session_id: String::new,
            limit: Duration::from_secs(30),
        }
    }
}

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

async fn collect(run: &mut dyn BackendRun) -> Vec<BackendEvent> {
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), run.next_event())
            .await
            .expect("the run produces events without hanging");
        match event {
            Some(event) => events.push(event),
            None => break,
        }
    }
    assert_eq!(run.next_event().await, None, "the stream stays ended");
    events
}

#[tokio::test]
async fn conformance_an_unattended_run_reaches_the_system_s_tools_and_resumes_by_id() {
    let fake = Fake::conforming();
    fake.suite()
        .run_with_tools_then_resume(fake.work.path())
        .await;
}

#[tokio::test]
async fn conformance_each_deny_rule_refuses_its_command() {
    let fake = Fake::conforming();
    fake.suite()
        .each_deny_rule_refuses_its_command(fake.work.path())
        .await;
}

#[tokio::test]
async fn conformance_a_usage_limit_is_reported_as_the_runner_expects() {
    let fake = Fake::conforming();
    fake.suite().usage_limit_is_reported(fake.work.path()).await;
}

#[tokio::test]
async fn kind_is_codex() {
    assert_eq!(CodexBackend::default().kind(), BackendKind::Codex);
}

#[tokio::test]
async fn start_passes_every_option_and_keeps_the_token_off_the_command_line() {
    let fake = Fake::conforming();
    let home = fake.dir.path().join("account");
    let backend = CodexBackend::new(CodexConfig {
        program: fake.program(),
        home: Some(home.clone()),
    });
    let request = RunRequest {
        prompt: "Reply to the message.\nIt says: hello.".to_owned(),
        appended_instructions: "You are a task agent.\nUse \"the tools\".".to_owned(),
        model: Some("gpt-test".to_owned()),
        tools: Some(Arc::new(Endpoint)),
        deny_rules: vec!["git push".to_owned(), "gh pr merge".to_owned()],
        max_turns: Some(3),
        ..fake.request(NativeSession::Start {
            id: "chosen-by-the-hub".to_owned(),
        })
    };
    let mut run = backend.run(request).await.expect("run starts");
    let events = collect(run.as_mut()).await;
    assert!(
        matches!(events.last(), Some(BackendEvent::Finished { .. })),
        "{events:?}"
    );

    let work = fake.work.path().canonicalize().expect("canonical work dir");
    let invocation = fake.invocations().remove(0);
    let url_at = invocation
        .iter()
        .position(|arg| arg.starts_with("mcp_servers.threadlane.url="))
        .expect("the tool server's url");
    let url = invocation[url_at].clone();
    assert!(
        url.starts_with("mcp_servers.threadlane.url=\"http://127.0.0.1:"),
        "{url}"
    );
    let expected: Vec<String> = [
        "exec",
        "--json",
        "--skip-git-repo-check",
        "-c",
        "sandbox_mode=\"danger-full-access\"",
        "-c",
        "approval_policy=\"never\"",
        "-c",
        "developer_instructions=\"You are a task agent.\\nUse \\\"the tools\\\".\"",
        "-c",
        &url,
        "-c",
        "mcp_servers.threadlane.bearer_token_env_var=\"THREADLANE_MCP_TOKEN\"",
        "-c",
        "mcp_servers.threadlane.required=true",
        "-c",
        "mcp_servers.threadlane.default_tools_approval_mode=\"approve\"",
        "-c",
        &format!(
            "projects={{\"{}\"={{trust_level=\"trusted\"}}}}",
            work.display()
        ),
        "--model",
        "gpt-test",
        "-",
    ]
    .map(str::to_owned)
    .to_vec();
    assert_eq!(invocation, expected);
    assert_eq!(
        fake.record("stdin"),
        "Reply to the message.\nIt says: hello."
    );
    assert_eq!(fake.record("cwd").trim(), work.display().to_string());
    assert_eq!(fake.record("home"), home.display().to_string());

    let token = fake.record("token");
    assert_eq!(token.len(), 64, "the token arrives in the environment");
    assert!(
        !fake.record("argv").contains(&token),
        "and not on the command line"
    );
    assert!(
        !format!("{run:?}").contains(&token),
        "Debug leaks the token"
    );

    let rules = fake.record("rules");
    assert_eq!(rules.lines().count(), 2, "{rules}");
    assert!(rules.contains(r#"pattern = ["git", "push"], decision = "forbidden""#));
    assert!(rules.contains(r#"pattern = ["gh", "pr", "merge"], decision = "forbidden""#));
    assert!(
        !work.join(".codex").exists(),
        "the rules file is gone once the run ended"
    );
}

#[tokio::test]
async fn a_lead_run_is_read_only_without_web_search_or_rules() {
    let fake = Fake::conforming();
    let request = RunRequest {
        remove_builtin_tools: true,
        ..fake.request(NativeSession::Start { id: String::new() })
    };
    let mut run = fake.backend().run(request).await.expect("run starts");
    collect(run.as_mut()).await;
    let invocation = fake.invocations().remove(0);
    assert_eq!(
        invocation[3..9],
        [
            "-c",
            "sandbox_mode=\"read-only\"",
            "-c",
            "approval_policy=\"never\"",
            "-c",
            "web_search=\"disabled\""
        ]
    );
    assert!(!invocation.iter().any(|arg| arg.starts_with("projects=")));
}

#[tokio::test]
async fn resume_names_the_session_and_passes_only_what_was_asked_for() {
    let fake = Fake::conforming();
    std::fs::write(fake.dir.path().join("sessions"), "abc\n").unwrap();
    let mut run = fake
        .backend()
        .run(fake.request(NativeSession::Resume { id: "abc".into() }))
        .await
        .expect("run starts");
    let events = collect(run.as_mut()).await;
    assert_eq!(
        events.first(),
        Some(&BackendEvent::SessionStarted {
            native_session_id: "abc".into()
        })
    );
    let expected: Vec<String> = [
        "exec",
        "resume",
        "--json",
        "--skip-git-repo-check",
        "-c",
        "sandbox_mode=\"danger-full-access\"",
        "-c",
        "approval_policy=\"never\"",
        "abc",
        "-",
    ]
    .map(str::to_owned)
    .to_vec();
    assert_eq!(fake.invocations(), vec![expected]);
    assert_eq!(fake.record("home"), "");
}

#[tokio::test]
async fn a_resume_that_codex_answers_with_a_new_session_is_missing_and_stopped() {
    let fake = Fake::new(&format!(
        "echo $$ > @DIR@/pid\necho '{}'\nexec sleep 600",
        json!({"type": "thread.started", "thread_id": "a-new-thread"})
    ));
    let mut run = fake
        .backend()
        .run(fake.request(NativeSession::Resume { id: "old".into() }))
        .await
        .expect("run starts");
    assert_eq!(
        collect(run.as_mut()).await,
        vec![BackendEvent::SessionMissing]
    );
    wait_until_gone(&fake.record("pid")).await;
}

#[tokio::test]
async fn output_closing_without_a_result_fails_with_the_stderr_tail() {
    let fake = Fake::new(&format!(
        "echo '{}'\necho 'something broke badly' >&2\nexit 3",
        json!({"type": "thread.started", "thread_id": "t"})
    ));
    let mut run = fake
        .backend()
        .run(fake.request(NativeSession::Start { id: String::new() }))
        .await
        .expect("run starts");
    let events = collect(run.as_mut()).await;
    assert_eq!(events.len(), 2, "{events:?}");
    let BackendEvent::Failed { error } = &events[1] else {
        panic!("expected Failed, got {:?}", events[1]);
    };
    assert!(
        error.contains("something broke badly") && error.contains('3'),
        "{error}"
    );
}

/// Whether `pid` is a live process (a zombie counts as gone).
fn alive(pid: &str) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid.trim()])
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

fn hanging() -> Fake {
    Fake::new(&format!(
        "echo $$ > @DIR@/pid\necho '{}'\nexec sleep 600",
        json!({"type": "thread.started", "thread_id": "t"})
    ))
}

#[tokio::test]
async fn stop_kills_a_hanging_codex_and_removes_the_rules() {
    let fake = hanging();
    let request = RunRequest {
        deny_rules: vec!["git push".into()],
        ..fake.request(NativeSession::Start { id: String::new() })
    };
    let mut run = fake.backend().run(request).await.expect("run starts");
    assert!(matches!(
        run.next_event().await,
        Some(BackendEvent::SessionStarted { .. })
    ));
    assert!(
        fake.work.path().join(RULES_FILE).is_file(),
        "rules are in place while it runs"
    );
    let pid = fake.record("pid");
    assert!(alive(&pid));
    tokio::time::timeout(Duration::from_secs(10), run.stop())
        .await
        .expect("stop returns promptly")
        .expect("stop succeeds");
    assert!(!alive(&pid));
    assert!(!fake.work.path().join(".codex").exists());
    assert_eq!(run.next_event().await, None);
    run.stop().await.expect("stopping twice is fine");
}

#[tokio::test]
async fn dropping_the_run_kills_codex() {
    let fake = hanging();
    let mut run = fake
        .backend()
        .run(fake.request(NativeSession::Start { id: String::new() }))
        .await
        .expect("run starts");
    assert!(run.next_event().await.is_some());
    let pid = fake.record("pid");
    drop(run);
    wait_until_gone(&pid).await;
}

#[tokio::test]
async fn ask_answers_with_the_structured_final_message_in_a_read_only_one_shot() {
    let fake = Fake::new(&format!(
        r#"{{ for arg in "$@"; do printf '%s\n' "$arg"; done; echo '<<end>>'; }} >> @DIR@/argv
prev=""
for arg in "$@"; do
  if [ "$prev" = --output-schema ]; then cp "$arg" @DIR@/schema; fi
  prev="$arg"
done
cat > @DIR@/stdin
echo '{}'
echo '{}'"#,
        json!({"type": "item.completed", "item": {"id": "i", "type": "agent_message", "text": "{\"verdict\":\"safe\"}"}}),
        json!({"type": "turn.completed", "usage": {}}),
    ));
    let schema = json!({"type": "object", "properties": {"verdict": {"type": "string"}}});
    let answer = fake
        .backend()
        .ask(AskRequest {
            question: "Is it safe?".into(),
            answer_schema: schema.clone(),
            model: Some("gpt-test".into()),
            timeout: Duration::from_secs(20),
        })
        .await;
    assert_eq!(answer, Ok(json!({"verdict": "safe"})));
    assert_eq!(fake.record("stdin"), "Is it safe?");
    assert_eq!(
        serde_json::from_str::<Json>(&fake.record("schema")).unwrap(),
        schema
    );
    let args = fake.invocations().remove(0);
    for expected in [
        "sandbox_mode=\"read-only\"",
        "web_search=\"disabled\"",
        "--ephemeral",
        "--output-schema",
    ] {
        assert!(
            args.iter().any(|arg| arg == expected),
            "{expected} in {args:?}"
        );
    }
}

#[tokio::test]
async fn ask_reports_a_usage_limit_and_a_timeout() {
    let limited = Fake::new(&format!(
        "cat > /dev/null\ncat <<'LINES'\n{}\nLINES",
        json!({"type": "turn.failed", "error": {"message": "You've hit your usage limit."}})
    ));
    let ask = |timeout| AskRequest {
        question: "?".into(),
        answer_schema: json!({}),
        model: None,
        timeout,
    };
    assert_eq!(
        limited.backend().ask(ask(Duration::from_secs(20))).await,
        Err(BackendError::UsageLimit { resets_at: None })
    );
    let slow = hanging();
    assert_eq!(
        slow.backend().ask(ask(Duration::from_millis(300))).await,
        Err(BackendError::Timeout)
    );
    wait_until_gone(&slow.record("pid")).await;
}

#[tokio::test]
async fn probe_reads_the_version_and_the_login_from_the_exit_status() {
    let fake = Fake::new(
        r#"case "$1" in
  --version) echo "codex-cli 0.167.0" ;;
  login) echo "Logged in using an API key - sk-proj-***ABCD" ;;
  *) exit 2 ;;
esac"#,
    );
    assert_eq!(
        fake.backend().probe().await,
        Ok(Probe {
            installed: true,
            version: Some("0.167.0".to_owned()),
            logged_in: true,
            account_label: None,
        })
    );
    let logged_out = Fake::new(
        r#"case "$1" in
  --version) echo "codex-cli 0.167.0" ;;
  login) echo "Not logged in"; exit 1 ;;
esac"#,
    );
    let probe = logged_out.backend().probe().await.expect("probe succeeds");
    assert!(probe.installed && !probe.logged_in && !probe.usable());
}

#[tokio::test]
async fn a_missing_program_is_not_installed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let backend = CodexBackend::new(CodexConfig {
        program: dir.path().join("no-such-codex"),
        home: None,
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
    let fake = Fake::new("exit 0");
    let request = fake.request(NativeSession::Start { id: String::new() });
    assert_eq!(
        backend.run(request).await.map(drop),
        Err(BackendError::NotInstalled)
    );
}

/// Every rule the system has reaches the rules file, one forbidding prefix
/// rule each.
#[test]
fn every_deny_rule_becomes_a_forbidding_prefix_rule() {
    let rules: Vec<String> = DENY_RULES.iter().map(|rule| (*rule).to_owned()).collect();
    let policy = threadlane_backend_codex::execution_policy(&rules);
    assert_eq!(policy.lines().count(), DENY_RULES.len());
    for rule in DENY_RULES {
        let words: Vec<String> = rule.split(' ').map(|word| format!("\"{word}\"")).collect();
        assert!(
            policy.contains(&format!(
                "pattern = [{}], decision = \"forbidden\"",
                words.join(", ")
            )),
            "{rule}: {policy}"
        );
    }
}
