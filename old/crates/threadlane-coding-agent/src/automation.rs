//! Preparation for a fresh automation chat. Never selects a foreground session.
use crate::{
    controller::{spawn_session_runtime_construction, SessionController},
    harness::CodingSessionHarness,
    CodingAgentOptions,
};
use std::{path::PathBuf, sync::Arc};
use threadlane_automation::Run;
use threadlane_protocol::{OrchestratorMode, ReasoningEffort};

pub struct PreparedRun {
    pub runtime: Arc<SessionController>,
    pub work_dir: PathBuf,
    pub effort: ReasoningEffort,
}

pub async fn prepare(run: Run) -> Result<PreparedRun, String> {
    let (options, effort) = threadlane_provider::exec::get_runtime()
        .spawn_blocking(move || prepare_options(&run))
        .await
        .map_err(|e| e.to_string())??;
    let work_dir = options.work_dir.clone();
    let runtime = spawn_session_runtime_construction(options)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(error) = runtime.harness_error() {
        return Err(format!(
            "Could not open automation runtime in {}: {error}",
            work_dir.display()
        ));
    }
    Ok(PreparedRun {
        runtime,
        work_dir,
        effort,
    })
}

fn prepare_options(run: &Run) -> Result<(CodingAgentOptions, ReasoningEffort), String> {
    run.definition.validate()?;
    if !run
        .session_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("Invalid automation session identity".into());
    }
    let d = &run.definition;
    let project =
        std::fs::canonicalize(&d.project).map_err(|e| format!("Project unavailable: {e}"))?;
    if !threadlane_project::load_project_registry()
        .iter()
        .any(|p| p.path == project)
    {
        return Err("Attach this automation's project before running it".into());
    }
    let (api_key, account_id) = crate::credentials::provider_credentials(&d.model);
    if api_key.is_empty() {
        return Err("Provider credentials unavailable. Sign in in Settings".into());
    }
    let (work_dir, session_file) = prepare_session(run, &project)?;
    let effort = threadlane_provider::model_registry::effective_effort(
        &d.model,
        ReasoningEffort::from_label(&d.effort).ok_or("Invalid reasoning effort")?,
        Some(&project),
    );
    let mut config = threadlane_runtime::AgentConfig::default();
    config.orchestrator_mode = OrchestratorMode::Normal;
    Ok((
        CodingAgentOptions {
            api_key,
            account_id,
            model: d.model.clone(),
            work_dir,
            session_file: Some(session_file),
            system_prompt: Default::default(),
            agent_config: Some(config),
            coding_config: None,
            browser: Default::default(),
        },
        effort,
    ))
}

fn prepare_session(run: &Run, project: &std::path::Path) -> Result<(PathBuf, PathBuf), String> {
    let d = &run.definition;
    let stub = project
        .join(".threadlane/sessions")
        .join(format!("{}.jsonl", run.session_id));
    if stub.exists() {
        return Err(
            "Automation session already exists; review it before starting another run".into(),
        );
    }
    let mut work_dir = project.to_path_buf();
    let mut facts = vec![
        ("automation_id", d.id.clone()),
        ("automation_run_id", run.id.clone()),
        ("automation_revision", d.revision.to_string()),
        ("automation_result_contract", "1".into()),
        ("name", format!("{} · automation", d.name)),
        ("model", d.model.clone()),
        ("reasoning_effort", d.effort.clone()),
        ("orchestrator_mode", "normal".into()),
    ];
    if d.worktree {
        if !threadlane_git::is_git_repo(project) {
            return Err("This automation requires a Git repository for its worktree".into());
        }
        work_dir = project.join(".threadlane/worktrees").join(&run.session_id);
        let branch = format!("automation/{}", run.id);
        threadlane_git::create_worktree(project, &work_dir, &branch).map_err(|e| {
            format!(
                "Could not create automation worktree {}: {e}",
                work_dir.display()
            )
        })?;
        facts.extend([
            ("is_worktree", "true".into()),
            ("worktree_path", work_dir.to_string_lossy().into_owned()),
            ("git_branch", branch),
        ]);
    }
    let session_file = work_dir
        .join(".threadlane/sessions")
        .join(format!("{}.jsonl", run.session_id));
    // Metadata stubs and the actual transcript use the existing discovery contract.
    // Setup failures retain created worktrees for inspection instead of deleting possible work.
    for path in if stub == session_file {
        vec![&stub]
    } else {
        vec![&stub, &session_file]
    } {
        for (key, value) in &facts {
            CodingSessionHarness::append_fact_to_path(path, "main", key, value, None).map_err(
                |e| format!("Could not prepare automation chat {}: {e}", path.display()),
            )?;
        }
    }
    Ok((work_dir, session_file))
}

pub fn record_outcome(runtime: &SessionController, outcome: &str) -> Result<(), String> {
    CodingSessionHarness::append_fact_to_path(
        runtime.session_file(),
        "main",
        "automation_outcome",
        outcome,
        None,
    )
}

/// Inspect the first foreground operation, not a later interactive follow-up in this chat.
pub fn durable_status(path: &std::path::Path) -> Option<threadlane_automation::RunStatus> {
    durable_result(path).map(|(status, _)| status)
}

/// Keep task completion separate from provider/agent-loop completion. Older chats
/// without the result contract retain their original completion semantics.
pub fn durable_result(
    path: &std::path::Path,
) -> Option<(threadlane_automation::RunStatus, Option<String>)> {
    use threadlane_automation::RunStatus;
    use threadlane_runtime::harness::{JsonlStore, OperationOutcome, Record, SessionStore};
    let store = JsonlStore::open_read_only(path).ok()?;
    let id = store.records().iter().find_map(|r| match r {
        Record::OperationStarted { id, lane, .. } if lane == "main" => Some(id),
        _ => None,
    })?;
    store.records().iter().rev().find_map(|record| match record {
        Record::OperationFinished { run_id, outcome, error, .. } if run_id == id => {
            let status = match outcome {
                OperationOutcome::Completed => RunStatus::Succeeded,
                OperationOutcome::Aborted => RunStatus::Cancelled,
                OperationOutcome::Failed | OperationOutcome::Declined => RunStatus::Failed,
            };
            if status != RunStatus::Succeeded
                || store.facts().get("automation_result_contract").map(String::as_str) != Some("1")
            {
                return Some((status, error.clone()));
            }
            let result = store.records().iter().rev().find_map(|r| match r {
                Record::FactSet { run_id: Some(run_id), lane, key, value, seq, .. }
                    if run_id == id && lane == "main" && key == "automation_task_result"
                        && *seq < record.seq() => Some(value),
                _ => None,
            }).and_then(|value| serde_json::from_str::<TaskResult>(value).ok())
                .filter(|result| !result.summary.trim().is_empty() && result.summary.chars().count() <= 4000);
            Some(match result {
                Some(result) if result.status == TaskStatus::Succeeded => (RunStatus::Succeeded, None),
                Some(result) => (RunStatus::Failed, Some(result.summary)),
                None => (RunStatus::Failed, Some(
                    "Automation ended without reporting a task result. Review the chat and rerun with report_automation_result.".into(),
                )),
            })
        }
        Record::AbortRequested { run_id, .. } if run_id == id => Some((RunStatus::Cancelled, None)),
        _ => None,
    })
}

#[derive(Clone, Copy, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum TaskStatus {
    Succeeded,
    Blocked,
    Failed,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct TaskResult {
    status: TaskStatus,
    summary: String,
}

/// Session-owned reporting remains available under read-only policy: it only
/// records an outcome, never grants permissions or mutates project files.
#[derive(Clone)]
pub(crate) struct AutomationResultCapability {
    pub session_file: PathBuf,
    pub run_id: Arc<std::sync::Mutex<Option<String>>>,
}

impl threadlane_runtime::Capability for AutomationResultCapability {
    fn id(&self) -> &str {
        "automation_result"
    }
    fn tool_executors(&self) -> Vec<Arc<dyn threadlane_protocol::ToolExecutor>> {
        vec![Arc::new(self.clone())]
    }
}

#[async_trait::async_trait]
impl threadlane_protocol::ToolExecutor for AutomationResultCapability {
    fn tool_definitions(&self) -> Arc<[threadlane_protocol::AgentToolDefinition]> {
        vec![threadlane_protocol::AgentToolDefinition {
            name: "report_automation_result".into(),
            description: Some("Required before the final response of the original automation operation, not later interactive follow-ups. Report succeeded only when the requested task is completed, including a fully researched no-candidate result when permitted. Report blocked for unresolved access, permission, or missing-capability blockers; failed for other unmet requirements. Include concrete evidence or the exact blocker in summary. A normal final response alone does not mean success. Does not end the agent turn; report again if the blocker is resolved before finishing.".into()),
            parameters: serde_json::json!({"type":"object","additionalProperties":false,
                "required":["status","summary"],"properties":{
                    "status":{"type":"string","enum":["succeeded","blocked","failed"]},
                    "summary":{"type":"string","minLength":1,"maxLength":4000}
                }}),
            strict: Some(false),
        }].into()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        if name != "report_automation_result" {
            return None;
        }
        Some(self.report(args))
    }
}

impl AutomationResultCapability {
    fn report(&self, args: &str) -> Result<String, String> {
        use threadlane_runtime::harness::{JsonlStore, Record, SessionStore};
        let result: TaskResult = serde_json::from_str(args).map_err(|e| e.to_string())?;
        if result.summary.trim().is_empty() || result.summary.chars().count() > 4000 {
            return Err("Provide a nonempty summary of at most 4000 characters".into());
        }
        let run_id = self
            .run_id
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
            .ok_or("No active automation operation")?;
        let store = JsonlStore::open_read_only(&self.session_file).map_err(|e| e.to_string())?;
        let first_run = store.records().iter().find_map(|r| match r {
            Record::OperationStarted { id, lane, .. } if lane == "main" => Some(id),
            _ => None,
        });
        if store
            .facts()
            .get("automation_result_contract")
            .map(String::as_str)
            != Some("1")
            || first_run != Some(&run_id)
            || store.records().iter().any(|r| {
                matches!(r,
                Record::OperationFinished { run_id: id, .. } if id == &run_id)
            })
        {
            return Err(
                "Task results can only be reported during the original automation operation".into(),
            );
        }
        CodingSessionHarness::append_fact_to_path(
            &self.session_file,
            "main",
            "automation_task_result",
            &serde_json::to_string(&result).map_err(|e| e.to_string())?,
            Some(&run_id),
        )?;
        Ok("Automation task result recorded".into())
    }
}

#[cfg(test)]
mod tests {
    use super::prepare_session;
    use threadlane_automation::{Definition, Run, RunStatus, Schedule};
    use threadlane_runtime::harness::{JsonlStore, SessionStore};

    fn result_fixture(contract: bool) -> (tempfile::TempDir, super::AutomationResultCapability) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("automation.jsonl");
        if contract {
            super::CodingSessionHarness::append_fact_to_path(
                &path,
                "main",
                "automation_result_contract",
                "1",
                None,
            )
            .unwrap();
        }
        let mut harness = super::CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run(
                "original",
                threadlane_protocol::AgentMessage::user("Research", vec![]),
            )
            .unwrap();
        (
            temp,
            super::AutomationResultCapability {
                session_file: path,
                run_id: std::sync::Arc::new(std::sync::Mutex::new(Some("original".into()))),
            },
        )
    }

    fn finish(
        tool: &super::AutomationResultCapability,
        outcome: threadlane_runtime::harness::OperationOutcome,
    ) {
        super::CodingSessionHarness::open(&tool.session_file)
            .unwrap()
            .finish_run("original", outcome, None)
            .unwrap();
    }

    #[tokio::test]
    async fn automation_result_reporting_is_allowed_while_shell_remains_read_only() {
        use super::super::capabilities::{
            build_broker_dispatcher, extension_before_tool_hook_handler,
        };
        use threadlane_runtime::{harness::HookContext, ToolPolicy};
        let (temp, tool) = result_fixture(true);
        let policy = std::sync::Arc::new(tokio::sync::Mutex::new(ToolPolicy::ReadOnly));
        let extensions = std::sync::Arc::new(threadlane_wasi::WasiExtensionManager::new());
        let (events, _) = tokio::sync::broadcast::channel(8);
        let (broker, _, _, _) = build_broker_dispatcher(
            policy.clone(),
            extensions.clone(),
            false,
            temp.path().into(),
            events,
            crate::scheduler::AgentWorkScheduler::default(),
            None,
            None,
        );
        let hook = extension_before_tool_hook_handler(policy.clone(), extensions, broker);
        let error = hook(HookContext {
            tool_name: Some("run_command".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
        assert!(error.contains("read-only tool policy"));
        hook(HookContext {
            tool_name: Some("report_automation_result".into()),
            ..Default::default()
        })
        .await
        .unwrap();
        tool.report(r#"{"status":"blocked","summary":"read-only tool policy"}"#)
            .unwrap();
        assert_eq!(*policy.lock().await, ToolPolicy::ReadOnly);
        finish(
            &tool,
            threadlane_runtime::harness::OperationOutcome::Completed,
        );
        assert_eq!(
            super::durable_result(&tool.session_file),
            Some((RunStatus::Failed, Some("read-only tool policy".into())))
        );
    }

    #[test]
    fn automation_task_results_distinguish_blocked_failed_success_and_missing() {
        use threadlane_runtime::harness::OperationOutcome;
        for (report, expected) in [
            (Some("blocked"), RunStatus::Failed),
            (Some("failed"), RunStatus::Failed),
            (Some("succeeded"), RunStatus::Succeeded),
            (None, RunStatus::Failed),
        ] {
            let (_temp, tool) = result_fixture(true);
            if let Some(status) = report {
                tool.report(
                    &serde_json::json!({"status":status,"summary":"evidence or exact blocker"})
                        .to_string(),
                )
                .unwrap();
            }
            assert_eq!(super::durable_result(&tool.session_file), None);
            finish(&tool, OperationOutcome::Completed);
            let (status, error) = super::durable_result(&tool.session_file).unwrap();
            assert_eq!(status, expected);
            if report == Some("blocked") || report == Some("failed") {
                assert_eq!(error.as_deref(), Some("evidence or exact blocker"));
            } else if report.is_none() {
                assert!(error.unwrap().contains("without reporting"));
            } else {
                assert!(error.is_none());
            }
        }
    }

    #[test]
    fn automation_task_results_preserve_legacy_and_runtime_outcomes() {
        use threadlane_runtime::harness::OperationOutcome;
        let (_temp, tool) = result_fixture(false);
        assert!(tool
            .report(r#"{"status":"succeeded","summary":"done"}"#)
            .is_err());
        finish(&tool, OperationOutcome::Completed);
        assert_eq!(
            super::durable_status(&tool.session_file),
            Some(RunStatus::Succeeded)
        );
        for (outcome, expected) in [
            (OperationOutcome::Aborted, RunStatus::Cancelled),
            (OperationOutcome::Failed, RunStatus::Failed),
            (OperationOutcome::Declined, RunStatus::Failed),
        ] {
            let (_temp, tool) = result_fixture(true);
            tool.report(r#"{"status":"succeeded","summary":"done"}"#)
                .unwrap();
            finish(&tool, outcome);
            assert_eq!(super::durable_status(&tool.session_file), Some(expected));
        }
    }

    #[test]
    fn automation_task_result_can_recover_but_followups_cannot_rewrite_it() {
        use threadlane_runtime::harness::OperationOutcome;
        let (_temp, tool) = result_fixture(true);
        for bad in [
            r#"{"status":"unknown","summary":"x"}"#,
            r#"{"status":"succeeded","summary":" "}"#,
            r#"{"status":"succeeded","summary":"x","extra":true}"#,
        ] {
            assert!(tool.report(bad).is_err());
        }
        tool.report(r#"{"status":"blocked","summary":"Access unavailable"}"#)
            .unwrap();
        tool.report(r#"{"status":"succeeded","summary":"Access restored; issue created"}"#)
            .unwrap();
        finish(&tool, OperationOutcome::Completed);
        assert!(tool
            .report(r#"{"status":"blocked","summary":"too late"}"#)
            .is_err());
        let mut harness = super::CodingSessionHarness::open(&tool.session_file).unwrap();
        harness
            .begin_run(
                "followup",
                threadlane_protocol::AgentMessage::user("followup", vec![]),
            )
            .unwrap();
        *tool.run_id.lock().unwrap() = Some("followup".into());
        assert!(tool
            .report(r#"{"status":"blocked","summary":"too late"}"#)
            .is_err());
        // Even a later fact written outside the tool must not change the first result.
        super::CodingSessionHarness::append_fact_to_path(
            &tool.session_file,
            "main",
            "automation_task_result",
            r#"{"status":"blocked","summary":"later"}"#,
            Some("followup"),
        )
        .unwrap();
        assert_eq!(
            super::durable_status(&tool.session_file),
            Some(RunStatus::Succeeded)
        );
    }

    #[test]
    fn automation_prepares_local_and_worktree_chats_without_touching_project_files() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().canonicalize().unwrap();
        let mut run = Run {
            id: "local".into(),
            session_id: "automation_local".into(),
            definition: Definition {
                id: "research".into(),
                revision: 1,
                name: "Research".into(),
                prompt: "Research without editing files".into(),
                project: project.clone(),
                model: "model".into(),
                effort: "medium".into(),
                worktree: false,
                schedule: Schedule::Manual,
                enabled: false,
                notify_all: false,
                anchor: 0,
                next_at: None,
                failures: 0,
                paused_reason: None,
            },
            scheduled_for: None,
            created_at: 0,
            finished_at: None,
            status: RunStatus::Starting,
            session_file: None,
            error: None,
            reviewed: false,
        };
        std::fs::write(project.join("notes.txt"), "keep my research").unwrap();
        let (checkout, chat) = prepare_session(&run, &project).unwrap();
        assert_eq!(checkout, project);
        assert!(chat.exists());
        assert!(!project.join(".threadlane/worktrees").exists());
        assert_eq!(
            JsonlStore::open_read_only(&chat).unwrap().facts()["automation_id"],
            "research"
        );
        assert!(prepare_session(&run, &project)
            .unwrap_err()
            .contains("already exists"));
        run.id = "code".into();
        run.session_id = "automation_code".into();
        run.definition.worktree = true;
        assert!(prepare_session(&run, &project)
            .unwrap_err()
            .contains("Git repository"));
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-qm",
                "initial",
            ],
        ] {
            let result = std::process::Command::new("git")
                .args(args)
                .current_dir(&project)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        let (checkout, chat) = prepare_session(&run, &project).unwrap();
        assert_ne!(checkout, project);
        assert!(checkout.join(".git").exists());
        assert!(chat.starts_with(&checkout));
        assert!(project
            .join(".threadlane/sessions/automation_code.jsonl")
            .exists());
        let facts = JsonlStore::open_read_only(&chat).unwrap().facts();
        assert_eq!(facts["worktree_path"], checkout.to_string_lossy());
        assert_eq!(
            std::fs::read_to_string(project.join("notes.txt")).unwrap(),
            "keep my research"
        );
        let broken = tempfile::tempdir().unwrap();
        std::fs::write(broken.path().join(".threadlane"), "keep this file").unwrap();
        run.definition.worktree = false;
        let error = prepare_session(&run, broken.path()).unwrap_err();
        assert!(error.contains("Could not prepare automation chat"), "{error}");
        assert!(error.contains("automation_code.jsonl"), "{error}");
    }
}
