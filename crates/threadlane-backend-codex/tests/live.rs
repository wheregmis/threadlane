//! Against the real `codex`. Ignored by default.
//!
//! `the_deny_rules_are_what_codex_s_own_checker_forbids` runs only `codex
//! execpolicy check`, which reads the rules file and calls no model. The
//! conformance test uses the account `codex` is logged in to. Run them with
//! `cargo test -p threadlane-backend-codex --test live -- --ignored`.

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value as Json;
use threadlane_backend_codex::{CodexBackend, CodexConfig, execution_policy};
use threadlane_core::backend::{AgentBackend, DENY_RULES};
use threadlane_testkit::conformance::Conformance;

/// What `codex execpolicy check` decides for `command` under `rules`.
fn decision(rules: &std::path::Path, command: &[&str]) -> Option<String> {
    let output = Command::new("codex")
        .args(["execpolicy", "check", "--rules"])
        .arg(rules)
        .args(command)
        .output()
        .expect("codex runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Json = serde_json::from_slice(&output.stdout).expect("a JSON report");
    report["decision"].as_str().map(str::to_owned)
}

#[test]
#[ignore = "needs the real codex; calls no model"]
fn the_deny_rules_are_what_codex_s_own_checker_forbids() {
    let dir = tempfile::tempdir().unwrap();
    let rules = dir.path().join("threadlane-deny.rules");
    let deny: Vec<String> = DENY_RULES.iter().map(|rule| (*rule).to_owned()).collect();
    std::fs::write(&rules, execution_policy(&deny)).unwrap();
    for rule in DENY_RULES {
        let mut command: Vec<&str> = rule.split(' ').collect();
        assert_eq!(
            decision(&rules, &command).as_deref(),
            Some("forbidden"),
            "{rule}"
        );
        command.extend(["--force", "anything"]);
        assert_eq!(
            decision(&rules, &command).as_deref(),
            Some("forbidden"),
            "{rule} with more arguments"
        );
    }
    for allowed in [
        &["git", "status"][..],
        &["git", "pushx"],
        &["gh", "pr", "view", "1"],
        &["gh", "repo", "view"],
    ] {
        assert_eq!(decision(&rules, allowed), None, "{allowed:?}");
    }
}

#[tokio::test]
#[ignore = "runs the real codex on the logged-in account"]
async fn real_codex_conforms() {
    let backend = CodexBackend::new(CodexConfig::default());
    let probe = backend.probe().await.expect("probe succeeds");
    eprintln!("probe: {probe:?}");
    assert!(probe.usable(), "codex must be installed and logged in");
    let suite = Conformance {
        backend: Arc::new(backend),
        model: std::env::var("THREADLANE_CODEX_MODEL").ok(),
        new_session_id: String::new,
        limit: Duration::from_secs(300),
    };
    let work = tempfile::tempdir().unwrap();
    suite.run_with_tools_then_resume(work.path()).await;
    let work = tempfile::tempdir().unwrap();
    suite.each_deny_rule_refuses_its_command(work.path()).await;
    // A real usage limit cannot be provoked; the fake covers that row.
}
