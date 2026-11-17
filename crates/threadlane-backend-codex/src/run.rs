//! One `codex exec --json` process and the events read from it.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::mem;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tempfile::TempDir;
use threadlane_core::backend::{BackendError, BackendEvent, BackendRun, NativeSession};
use threadlane_mcp::{McpServer, SERVER_NAME};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::task::JoinHandle;

use crate::stream::StreamParser;

/// The variable that carries the tool server's bearer token to codex, which
/// reads it through `bearer_token_env_var`. Codex leaves variables whose
/// names contain `TOKEN` out of the shells it runs by default.
pub(crate) const TOKEN_ENV: &str = "THREADLANE_MCP_TOKEN";

/// Where a task run's deny rules are written, under its working directory:
/// codex reads execution policy rules only from `rules/` of its own home and
/// of a trusted project's `.codex/`.
pub const RULES_FILE: &str = ".codex/rules/threadlane-deny.rules";

/// How much of stderr is kept for error messages.
const STDERR_TAIL_BYTES: usize = 8 * 1024;
/// How long codex may take to exit after its turn ended.
const EXIT_AFTER_TURN: Duration = Duration::from_secs(10);
/// How long codex may take to exit after closing stdout.
const EXIT_AFTER_EOF: Duration = Duration::from_secs(10);
/// How long to wait for a killed process to be reaped.
const KILL_WAIT: Duration = Duration::from_secs(5);
/// How long to wait for the rest of stderr once the process has exited.
const STDERR_DRAIN: Duration = Duration::from_secs(1);

/// Everything needed to spawn codex for a run.
pub(crate) struct Launch {
    pub(crate) program: PathBuf,
    pub(crate) home: Option<PathBuf>,
    pub(crate) working_dir: PathBuf,
    pub(crate) prompt: String,
    pub(crate) model: Option<String>,
    pub(crate) appended_instructions: String,
    /// The tool server's URL; its token goes in [`TOKEN_ENV`].
    pub(crate) mcp_url: Option<String>,
    /// A read-only sandbox with web search off, instead of full access.
    pub(crate) read_only: bool,
    /// Trust the working directory, so codex loads [`RULES_FILE`].
    pub(crate) trusted: bool,
    pub(crate) output_schema: Option<PathBuf>,
    /// Keep no session on disk (one-shot questions).
    pub(crate) ephemeral: bool,
}

impl Launch {
    /// The command line, without the program.
    pub(crate) fn args(&self, session: &NativeSession) -> Vec<OsString> {
        let mut args: Vec<String> = vec!["exec".into()];
        if let NativeSession::Resume { .. } = session {
            args.push("resume".into());
        }
        args.extend(["--json".into(), "--skip-git-repo-check".into()]);
        let sandbox = if self.read_only {
            "read-only"
        } else {
            "danger-full-access"
        };
        let mut config = vec![
            format!("sandbox_mode={}", toml_string(sandbox)),
            "approval_policy=\"never\"".to_owned(),
        ];
        if self.read_only {
            config.push("web_search=\"disabled\"".to_owned());
        }
        if !self.appended_instructions.is_empty() {
            config.push(format!(
                "developer_instructions={}",
                toml_string(&self.appended_instructions)
            ));
        }
        if let Some(url) = &self.mcp_url {
            let server = format!("mcp_servers.{SERVER_NAME}");
            config.extend([
                format!("{server}.url={}", toml_string(url)),
                format!("{server}.bearer_token_env_var={}", toml_string(TOKEN_ENV)),
                format!("{server}.required=true"),
                // The gate is in the system's tools, so codex asks nothing.
                format!("{server}.default_tools_approval_mode=\"approve\""),
            ]);
        }
        if self.trusted {
            config.push(format!(
                "projects={{{}={{trust_level=\"trusted\"}}}}",
                toml_string(&self.working_dir.display().to_string())
            ));
        }
        for value in config {
            args.extend(["-c".into(), value]);
        }
        if let Some(model) = &self.model {
            args.extend(["--model".into(), model.clone()]);
        }
        if let Some(schema) = &self.output_schema {
            args.extend(["--output-schema".into(), schema.display().to_string()]);
        }
        if self.ephemeral {
            args.push("--ephemeral".into());
        }
        if let NativeSession::Resume { id } = session {
            args.push(id.clone());
        }
        // The prompt is read from stdin.
        args.push("-".into());
        args.into_iter().map(OsString::from).collect()
    }

    fn spawn(&self, session: &NativeSession, token: Option<&str>) -> std::io::Result<Process> {
        let mut command = Command::new(&self.program);
        command
            .args(self.args(session))
            .current_dir(&self.working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(home) = &self.home {
            command.env("CODEX_HOME", home);
        }
        if let Some(token) = token {
            command.env(TOKEN_ENV, token);
        }
        let mut child = command.spawn()?;
        let (Some(mut stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(std::io::Error::other("codex's pipes were not set up"));
        };
        let prompt = self.prompt.clone();
        let stdin_task = tokio::spawn(async move {
            // Dropping `stdin` at the end closes it, which ends the prompt.
            if let Err(error) = stdin.write_all(prompt.as_bytes()).await {
                tracing::debug!(%error, "could not write the prompt to codex");
            }
        });
        let tail = Arc::new(Mutex::new(Vec::new()));
        let stderr_task = tokio::spawn({
            let tail = Arc::clone(&tail);
            async move {
                let mut chunk = [0u8; 4096];
                loop {
                    match stderr.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            let mut tail = tail.lock().expect("lock poisoned");
                            tail.extend_from_slice(&chunk[..read]);
                            let excess = tail.len().saturating_sub(STDERR_TAIL_BYTES);
                            tail.drain(..excess);
                        }
                    }
                }
            }
        });
        Ok(Process {
            child,
            stdout: BufReader::new(stdout),
            stderr: tail,
            stderr_task: Some(stderr_task),
            stdin_task,
        })
    }
}

impl fmt::Debug for Launch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Launch")
            .field("program", &self.program)
            .field("home", &self.home)
            .field("working_dir", &self.working_dir)
            .field("prompt_len", &self.prompt.len())
            .field("model", &self.model)
            .field(
                "appended_instructions_len",
                &self.appended_instructions.len(),
            )
            .field("mcp_url", &self.mcp_url)
            .field("read_only", &self.read_only)
            .field("trusted", &self.trusted)
            .field("output_schema", &self.output_schema)
            .field("ephemeral", &self.ephemeral)
            .finish()
    }
}

/// `value` as a TOML basic string, the form `-c key=value` parses.
pub(crate) fn toml_string(value: &str) -> String {
    // JSON string escapes are TOML's, except that TOML also escapes DEL.
    serde_json::to_string(value)
        .unwrap_or_default()
        .replace('\u{7f}', "\\u007f")
}

/// The deny rules, command prefixes such as `git push`, as a codex execution
/// policy file: one `prefix_rule` per rule that forbids every command
/// starting with those words.
pub fn execution_policy(deny_rules: &[String]) -> String {
    deny_rules
        .iter()
        .map(|rule| {
            let words: Vec<String> = rule.split_whitespace().map(toml_string).collect();
            format!(
                "prefix_rule(pattern = [{}], decision = \"forbidden\", justification = \"Shared effects go through the system's tools, such as publish_branch and open_pull_request.\")\n",
                words.join(", ")
            )
        })
        .collect()
}

/// The deny rules file of one run, removed when the run ends so the task's
/// checkpoint never commits it.
///
/// shortcut: a process killed mid-run leaves the file for the next run to
/// overwrite; a checkpoint taken before that run would commit it. Upgrade
/// by excluding it in the clone's `info/exclude` if that is ever seen.
#[derive(Debug)]
pub(crate) struct RulesFile(PathBuf);

impl RulesFile {
    pub(crate) fn write(working_dir: &Path, deny_rules: &[String]) -> std::io::Result<Self> {
        let path = working_dir.join(RULES_FILE);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, execution_policy(deny_rules))?;
        Ok(Self(path))
    }
}

impl Drop for RulesFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        // `.codex/rules`, then `.codex`, only if nothing else is in them.
        let mut dir = self.0.parent();
        for _ in 0..2 {
            match dir {
                Some(path) if std::fs::remove_dir(path).is_ok() => dir = path.parent(),
                _ => break,
            }
        }
    }
}

/// A spawned codex process.
#[derive(Debug)]
struct Process {
    child: Child,
    stdout: BufReader<ChildStdout>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_task: Option<JoinHandle<()>>,
    stdin_task: JoinHandle<()>,
}

impl Process {
    /// Kills the process unless it has already exited, and reaps it.
    async fn kill(&mut self) -> std::io::Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        self.child.start_kill()?;
        match tokio::time::timeout(KILL_WAIT, self.child.wait()).await {
            Ok(status) => status.map(drop),
            Err(_) => Err(std::io::Error::other(
                "codex did not exit after being killed",
            )),
        }
    }

    /// Waits up to `limit` for the process to exit on its own, then kills it.
    async fn exit_within(&mut self, limit: Duration) -> Option<ExitStatus> {
        if let Ok(Ok(status)) = tokio::time::timeout(limit, self.child.wait()).await {
            return Some(status);
        }
        tracing::debug!("codex did not exit in time; killing it");
        if let Err(error) = self.kill().await {
            tracing::warn!(%error, "could not kill codex");
        }
        None
    }

    async fn stderr_text(&mut self, secret: Option<&str>) -> String {
        if let Some(task) = self.stderr_task.take() {
            let _ = tokio::time::timeout(STDERR_DRAIN, task).await;
        }
        let tail = self.stderr.lock().expect("lock poisoned");
        let text = String::from_utf8_lossy(&tail).trim().to_owned();
        match secret {
            Some(secret) if !secret.is_empty() => text.replace(secret, "<redacted>"),
            _ => text,
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stdin_task.abort();
        if let Some(task) = &self.stderr_task {
            task.abort();
        }
    }
}

/// A run of the Codex backend.
pub(crate) struct CodexRun {
    launch: Launch,
    process: Option<Process>,
    parser: StreamParser,
    pending: VecDeque<BackendEvent>,
    /// The line being read. Kept here so a cancelled `next_event` loses nothing.
    line: Vec<u8>,
    done: bool,
    server: Option<McpServer>,
    rules: Option<RulesFile>,
    /// Holds a one-shot question's answer schema; removed with the run.
    _scratch: Option<TempDir>,
}

impl fmt::Debug for CodexRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodexRun")
            .field("launch", &self.launch)
            .field("pid", &self.process.as_ref().and_then(|p| p.child.id()))
            .field("pending", &self.pending.len())
            .field("done", &self.done)
            .field("server", &self.server)
            .field("rules", &self.rules)
            .finish_non_exhaustive()
    }
}

impl CodexRun {
    /// Spawns codex for `session`.
    pub(crate) fn start(
        launch: Launch,
        session: &NativeSession,
        server: Option<McpServer>,
        rules: Option<RulesFile>,
        scratch: Option<TempDir>,
    ) -> std::io::Result<Self> {
        let process = launch.spawn(session, server.as_ref().map(McpServer::token))?;
        let resuming = match session {
            NativeSession::Resume { id } => Some(id.clone()),
            NativeSession::Start { .. } => None,
        };
        Ok(Self {
            launch,
            process: Some(process),
            parser: StreamParser::new(resuming),
            pending: VecDeque::new(),
            line: Vec::new(),
            done: false,
            server,
            rules,
            _scratch: scratch,
        })
    }

    /// Marks the run ended and removes its deny rules file.
    fn end(&mut self) {
        self.done = true;
        self.rules = None;
    }

    async fn take_line(&mut self) {
        let bytes = mem::take(&mut self.line);
        let events = self.parser.line(&String::from_utf8_lossy(&bytes));
        self.pending.extend(events);
        if !self.parser.ended() {
            return;
        }
        let by_turn = self.parser.ended_by_turn();
        if let Some(process) = self.process.as_mut() {
            if by_turn {
                process.exit_within(EXIT_AFTER_TURN).await;
            } else if let Err(error) = process.kill().await {
                tracing::warn!(%error, "could not kill codex");
            }
        }
        self.end();
    }

    async fn on_eof(&mut self) {
        let secret = self.server.as_ref().map(|server| server.token().to_owned());
        if let Some(process) = self.process.as_mut() {
            let status = process.exit_within(EXIT_AFTER_EOF).await;
            let stderr = process.stderr_text(secret.as_deref()).await;
            let status =
                status.map_or_else(|| "did not exit".to_owned(), |status| status.to_string());
            let event = self.parser.closed(&stderr, &status);
            self.pending.push_back(event);
        }
        self.end();
    }
}

#[async_trait]
impl BackendRun for CodexRun {
    async fn next_event(&mut self) -> Option<BackendEvent> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(event);
            }
            if self.done {
                return None;
            }
            let Some(process) = self.process.as_mut() else {
                self.end();
                return None;
            };
            match process.stdout.read_until(b'\n', &mut self.line).await {
                Ok(0) if self.line.is_empty() => self.on_eof().await,
                Ok(_) => self.take_line().await,
                Err(error) => {
                    if let Err(error) = process.kill().await {
                        tracing::warn!(%error, "could not kill codex");
                    }
                    self.pending.push_back(BackendEvent::Failed {
                        error: format!("could not read codex's output: {error}"),
                    });
                    self.end();
                }
            }
        }
    }

    async fn stop(&mut self) -> Result<(), BackendError> {
        self.end();
        self.pending.clear();
        let killed = match self.process.as_mut() {
            Some(process) => process.kill().await,
            None => Ok(()),
        };
        if let Some(server) = self.server.take() {
            server.shutdown().await;
        }
        killed.map_err(|error| BackendError::Failed(format!("could not stop codex: {error}")))?;
        self.process = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch() -> Launch {
        Launch {
            program: "codex".into(),
            home: None,
            working_dir: "/work/app".into(),
            prompt: String::new(),
            model: None,
            appended_instructions: String::new(),
            mcp_url: None,
            read_only: false,
            trusted: false,
            output_schema: None,
            ephemeral: false,
        }
    }

    #[test]
    fn minimal_command_lines() {
        let start = launch().args(&NativeSession::Start {
            id: "ignored".into(),
        });
        assert_eq!(
            start,
            [
                "exec",
                "--json",
                "--skip-git-repo-check",
                "-c",
                "sandbox_mode=\"danger-full-access\"",
                "-c",
                "approval_policy=\"never\"",
                "-"
            ]
            .map(OsString::from)
        );
        let resume = launch().args(&NativeSession::Resume { id: "abc".into() });
        assert_eq!(resume[..2], ["exec", "resume"].map(OsString::from));
        assert_eq!(resume[resume.len() - 2..], ["abc", "-"].map(OsString::from));
    }

    #[test]
    fn values_are_toml_strings() {
        assert_eq!(toml_string("a \"b\"\n\\ \u{7f}"), r#""a \"b\"\n\\ \u007f""#);
    }

    #[test]
    fn each_rule_is_a_forbidding_prefix_rule() {
        let file = execution_policy(&["git push".into(), "gh  pr create".into()]);
        let lines: Vec<&str> = file.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0]
                .starts_with(r#"prefix_rule(pattern = ["git", "push"], decision = "forbidden""#)
        );
        assert!(lines[1].starts_with(
            r#"prefix_rule(pattern = ["gh", "pr", "create"], decision = "forbidden""#
        ));
    }

    #[test]
    fn the_rules_file_goes_away_with_its_directories_unless_they_hold_more() {
        let dir = tempfile::tempdir().unwrap();
        let rules = RulesFile::write(dir.path(), &["git push".into()]).unwrap();
        assert!(dir.path().join(RULES_FILE).is_file());
        drop(rules);
        assert!(!dir.path().join(".codex").exists());

        std::fs::create_dir_all(dir.path().join(".codex")).unwrap();
        std::fs::write(dir.path().join(".codex/config.toml"), "").unwrap();
        drop(RulesFile::write(dir.path(), &["git push".into()]).unwrap());
        assert!(!dir.path().join(".codex/rules").exists());
        assert!(
            dir.path().join(".codex/config.toml").is_file(),
            "the repository's own file stays"
        );
    }
}
