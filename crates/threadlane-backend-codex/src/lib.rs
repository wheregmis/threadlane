//! Drives the Codex command-line agent (Plan.md §5.7).
//!
//! [`CodexBackend`] runs `codex exec --json` (`codex exec resume <id>` to
//! resume), serves the run's tools to it through a [`McpServer`] it is
//! given with `-c mcp_servers.threadlane.*` overrides, and maps its output
//! to [`BackendEvent`](threadlane_core::backend::BackendEvent)s. Nothing is
//! written to the user's Codex home. How each need of Plan.md §5.7 is met,
//! checked against `codex-cli` 0.167.0:
//!
//! - **Unattended, full access**: `exec` with `-c
//!   sandbox_mode="danger-full-access"` and `-c approval_policy="never"`
//!   (`exec resume` has no `--sandbox` flag, so both use `-c`).
//! - **Resume**: `exec resume <id>`. Codex picks the id and reports it in
//!   `thread.started`; the id of [`NativeSession::Start`] is ignored. A
//!   resume that reports another id started a new session, so it ends the
//!   run as `SessionMissing`.
//! - **The system's tools**: a streamable HTTP server at the loopback URL,
//!   `required` so the run fails if it cannot connect, its tools approved
//!   without asking. The bearer token is passed in an environment variable
//!   named by `bearer_token_env_var`, so it is on no command line and on no
//!   disk.
//! - **Deny rules**: one `prefix_rule(..., decision = "forbidden")` each, in
//!   `<working dir>/.codex/rules/threadlane-deny.rules`, with the working
//!   directory trusted for the run (`-c projects=...`), because codex loads
//!   rules only from its home and from a trusted project. The file is
//!   removed when the run ends.
//! - **Appended instructions**: `-c developer_instructions=...`, added after
//!   Codex's own.
//! - **Usage**: the `usage` of `turn.completed`. **Usage limit**: recognised
//!   from the error text, without a reset time (see `stream.rs`).
//! - **Ask**: `exec --output-schema` in a read-only sandbox, ephemeral, in a
//!   temporary directory, ignoring user tool configuration and disabling shell, image, app,
//!   plugin and multi-agent features. Tool activity invalidates the answer.
//! - **Lead runs**: read-only sandbox, user configuration ignored, web search
//!   and local execution features disabled. The run receives its Threadlane
//!   tools through the same scoped MCP endpoint as other backends.
//! - **Turn cap, mid-run messages**: none; the runner's wall-time limit
//!   applies.

mod run;
mod stream;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value as Json;
use threadlane_core::backend::{
    AgentBackend, AskRequest, BackendError, BackendEvent, BackendRun, NativeSession, Probe,
    RunRequest,
};
use threadlane_core::model::BackendKind;
use threadlane_mcp::McpServer;
use tokio::process::Command;

use crate::run::{CodexRun, Launch, RulesFile};
pub use crate::run::{RULES_FILE, execution_policy};

/// How long `codex --version` and `codex login status` may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Where and how to run Codex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexConfig {
    /// The `codex` executable: a path, or a name looked up on `PATH`.
    pub program: PathBuf,
    /// Sets `CODEX_HOME`, so this membership uses a separate login
    /// (Plan.md §9.3). `None` uses the machine's ordinary login.
    pub home: Option<PathBuf>,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            program: PathBuf::from("codex"),
            home: None,
        }
    }
}

/// The Codex command-line agent as an [`AgentBackend`].
#[derive(Debug, Clone, Default)]
pub struct CodexBackend {
    config: CodexConfig,
}

impl CodexBackend {
    /// A backend that runs `config.program`.
    #[must_use]
    pub fn new(config: CodexConfig) -> Self {
        Self { config }
    }

    fn launch(&self, working_dir: PathBuf, prompt: String, model: Option<String>) -> Launch {
        Launch {
            program: self.config.program.clone(),
            home: self.config.home.clone(),
            working_dir,
            prompt,
            model,
            appended_instructions: String::new(),
            mcp_url: None,
            read_only: false,
            trusted: false,
            output_schema: None,
            ephemeral: false,
        }
    }

    /// Runs `codex <args>` to completion, with its output thrown away (a
    /// login status may name the account's key), and returns its exit
    /// status and the first line it printed.
    async fn status(
        &self,
        args: &[&str],
        keep_stdout: bool,
    ) -> Result<(bool, String), BackendError> {
        let mut command = Command::new(&self.config.program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(if keep_stdout {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(home) = &self.config.home {
            command.env("CODEX_HOME", home);
        }
        match tokio::time::timeout(PROBE_TIMEOUT, command.output()).await {
            Err(_) => Err(BackendError::Timeout),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(BackendError::NotInstalled)
            }
            Ok(Err(error)) => Err(BackendError::Failed(format!(
                "codex {}: {error}",
                args.join(" ")
            ))),
            Ok(Ok(output)) => Ok((
                output.status.success(),
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            )),
        }
    }
}

#[async_trait]
impl AgentBackend for CodexBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Codex
    }

    async fn probe(&self) -> Result<Probe, BackendError> {
        let version = match self.status(&["--version"], true).await {
            Ok((true, line)) => line,
            Ok((false, _)) => {
                return Err(BackendError::Failed("codex --version failed".to_owned()));
            }
            Err(BackendError::NotInstalled) => {
                return Ok(Probe {
                    installed: false,
                    version: None,
                    logged_in: false,
                    account_label: None,
                });
            }
            Err(error) => return Err(error),
        };
        // `codex login status` exits 0 when logged in; it names no account
        // label, and for an API key it prints part of the key, so only its
        // exit status is read.
        let (logged_in, _) = self.status(&["login", "status"], false).await?;
        Ok(Probe {
            installed: true,
            // "codex-cli 0.167.0"
            version: version.split_whitespace().last().map(str::to_owned),
            logged_in,
            account_label: None,
        })
    }

    async fn run(&self, request: RunRequest) -> Result<Box<dyn BackendRun>, BackendError> {
        let working_dir = request.working_dir.canonicalize().map_err(|error| {
            BackendError::Failed(format!(
                "working directory {}: {error}",
                request.working_dir.display()
            ))
        })?;
        let server = match request.tools {
            Some(endpoint) => Some(McpServer::start(endpoint).await.map_err(|error| {
                BackendError::Failed(format!("could not start the tool server: {error}"))
            })?),
            None => None,
        };
        let rules = if request.deny_rules.is_empty() {
            None
        } else {
            Some(
                RulesFile::write(&working_dir, &request.deny_rules).map_err(|error| {
                    BackendError::Failed(format!("could not write the deny rules: {error}"))
                })?,
            )
        };
        let launch = Launch {
            appended_instructions: request.appended_instructions,
            mcp_url: server.as_ref().map(|server| server.url().to_owned()),
            read_only: request.remove_builtin_tools,
            trusted: rules.is_some(),
            ..self.launch(working_dir, request.prompt, request.model)
        };
        start(launch, &request.session, server, rules, None)
    }

    async fn ask(&self, request: AskRequest) -> Result<Json, BackendError> {
        let failed = |what: &str, error: std::io::Error| {
            BackendError::Failed(format!("could not {what}: {error}"))
        };
        let dir = tempfile::Builder::new()
            .prefix("threadlane-codex-ask-")
            .tempdir()
            .map_err(|error| failed("create a temp directory", error))?;
        let schema = dir.path().join("answer-schema.json");
        std::fs::write(&schema, request.answer_schema.to_string())
            .map_err(|error| failed("write the answer schema", error))?;
        let launch = Launch {
            read_only: true,
            output_schema: Some(schema),
            ephemeral: true,
            ..self.launch(dir.path().to_owned(), request.question, request.model)
        };
        let session = NativeSession::Start { id: String::new() };
        let mut run = start(launch, &session, None, None, Some(dir))?;
        let answer = tokio::time::timeout(request.timeout, answer(run.as_mut())).await;
        if answer.is_err()
            && let Err(error) = run.stop().await
        {
            tracing::warn!(%error, "could not stop codex after the question timed out");
        }
        answer.unwrap_or(Err(BackendError::Timeout))
    }
}

fn start(
    launch: Launch,
    session: &NativeSession,
    server: Option<McpServer>,
    rules: Option<RulesFile>,
    scratch: Option<tempfile::TempDir>,
) -> Result<Box<dyn BackendRun>, BackendError> {
    match CodexRun::start(launch, session, server, rules, scratch) {
        Ok(run) => Ok(Box::new(run)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(BackendError::NotInstalled)
        }
        Err(error) => Err(BackendError::Failed(format!(
            "could not start codex: {error}"
        ))),
    }
}

/// The structured answer a one-shot run ends with.
async fn answer(run: &mut dyn BackendRun) -> Result<Json, BackendError> {
    while let Some(event) = run.next_event().await {
        match event {
            BackendEvent::Finished { final_text, .. } => {
                let text = final_text.unwrap_or_default();
                return serde_json::from_str(&text).map_err(|error| {
                    BackendError::Failed(format!("codex's answer is not JSON: {error}"))
                });
            }
            BackendEvent::ToolUse { .. } | BackendEvent::ToolResult { .. } => {
                return Err(BackendError::Failed(
                    "codex used a tool during a one-shot question".into(),
                ));
            }
            BackendEvent::UsageLimit { resets_at } => {
                return Err(BackendError::UsageLimit { resets_at });
            }
            BackendEvent::Failed { error } => return Err(BackendError::Failed(error)),
            _ => {}
        }
    }
    Err(BackendError::Failed(
        "codex ended without an answer".to_owned(),
    ))
}
