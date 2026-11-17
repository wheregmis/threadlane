//! One `claude -p` process and the events read from it (M1 §5 "Run").

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::mem;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tempfile::TempDir;
use threadlane_core::backend::{BackendError, BackendEvent, BackendRun, NativeSession};
use threadlane_mcp::McpServer;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::task::JoinHandle;

use crate::stream::StreamParser;

/// How much of stderr is kept for error messages.
const STDERR_TAIL_BYTES: usize = 8 * 1024;

/// How long claude may take to exit after closing stdout.
const EXIT_AFTER_EOF: Duration = Duration::from_secs(10);

/// How long to wait for a killed process to be reaped.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// Set to `1` for every run: a headless `claude -p` exits when its turn
/// ends, so a backgrounded command would die unseen. Documented on Claude
/// Code's environment variables page: it removes `run_in_background` and
/// stops timed-out shell commands moving to the background.
const DISABLE_BACKGROUND_TASKS: &str = "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS";

/// How long to wait for the rest of stderr once the process has exited.
const STDERR_DRAIN: Duration = Duration::from_secs(1);

/// What claude prints on stderr when `--session-id` names an existing session.
const SESSION_IN_USE: &str = "is already in use";

/// Everything needed to spawn claude for a run, again if it must be retried.
pub(crate) struct Launch {
    pub(crate) program: PathBuf,
    pub(crate) config_dir: Option<PathBuf>,
    pub(crate) working_dir: PathBuf,
    pub(crate) prompt: String,
    pub(crate) model: Option<String>,
    pub(crate) appended_instructions: String,
    pub(crate) mcp_config: Option<PathBuf>,
    pub(crate) remove_builtin_tools: bool,
    pub(crate) deny_rules: Vec<String>,
    pub(crate) max_turns: Option<u32>,
}

impl Launch {
    /// The command line, without the program. The flags that take several
    /// values come last and are followed only by other flags.
    pub(crate) fn args(&self, session: &NativeSession) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let (flag, id) = match session {
            NativeSession::Start { id } => ("--session-id", id),
            NativeSession::Resume { id } => ("--resume", id),
        };
        args.extend([flag.into(), id.into()]);
        if let Some(model) = &self.model {
            args.extend(["--model".into(), model.into()]);
        }
        if !self.appended_instructions.is_empty() {
            args.extend([
                "--append-system-prompt".into(),
                (&self.appended_instructions).into(),
            ]);
        }
        if let Some(config) = &self.mcp_config {
            args.extend(["--mcp-config".into(), config.into()]);
        }
        if self.remove_builtin_tools {
            args.extend(["--tools".into(), "".into(), "--strict-mcp-config".into()]);
        }
        if !self.deny_rules.is_empty() {
            args.push("--disallowedTools".into());
            args.extend(
                self.deny_rules
                    .iter()
                    .map(|rule| disallowed_tool(rule).into()),
            );
        }
        if let Some(max_turns) = self.max_turns {
            args.extend(["--max-turns".into(), max_turns.to_string().into()]);
        }
        args
    }

    fn spawn(&self, session: &NativeSession) -> std::io::Result<Process> {
        let mut command = Command::new(&self.program);
        command
            .args(self.args(session))
            .current_dir(&self.working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env(DISABLE_BACKGROUND_TASKS, "1")
            .kill_on_drop(true);
        if let Some(dir) = &self.config_dir {
            command.env("CLAUDE_CONFIG_DIR", dir);
        }
        let mut child = command.spawn()?;
        let (Some(mut stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(std::io::Error::other("claude's pipes were not set up"));
        };

        let prompt = self.prompt.clone();
        let stdin_task = tokio::spawn(async move {
            // Dropping `stdin` at the end closes it, which ends the prompt.
            if let Err(error) = stdin.write_all(prompt.as_bytes()).await {
                tracing::debug!(%error, "could not write the prompt to claude");
            }
        });

        let tail = Arc::new(Mutex::new(Tail::default()));
        let stderr_task = tokio::spawn({
            let tail = Arc::clone(&tail);
            async move {
                let mut chunk = [0u8; 4096];
                loop {
                    match stderr.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => tail.lock().expect("lock poisoned").push(&chunk[..read]),
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

/// A deny rule, a command prefix such as `git push`, as a Claude permission
/// rule: `Bash(git push:*)` refuses every Bash command that starts with it.
pub(crate) fn disallowed_tool(rule: &str) -> String {
    format!("Bash({}:*)", rule.trim())
}

impl fmt::Debug for Launch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Launch")
            .field("program", &self.program)
            .field("config_dir", &self.config_dir)
            .field("working_dir", &self.working_dir)
            .field("prompt_len", &self.prompt.len())
            .field("model", &self.model)
            .field(
                "appended_instructions_len",
                &self.appended_instructions.len(),
            )
            .field("mcp_config", &self.mcp_config)
            .field("remove_builtin_tools", &self.remove_builtin_tools)
            .field("deny_rules", &self.deny_rules)
            .field("max_turns", &self.max_turns)
            .finish()
    }
}

/// The last bytes written to stderr.
#[derive(Debug, Default)]
struct Tail(Vec<u8>);

impl Tail {
    fn push(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
        if self.0.len() > STDERR_TAIL_BYTES {
            let excess = self.0.len() - STDERR_TAIL_BYTES;
            self.0.drain(..excess);
        }
    }
}

/// A spawned claude process.
#[derive(Debug)]
struct Process {
    child: Child,
    stdout: BufReader<ChildStdout>,
    stderr: Arc<Mutex<Tail>>,
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
                "claude did not exit after being killed",
            )),
        }
    }

    /// Waits up to `limit` for the process to exit on its own, then kills it.
    async fn exit_within(&mut self, limit: Duration) -> Option<ExitStatus> {
        if let Ok(Ok(status)) = tokio::time::timeout(limit, self.child.wait()).await {
            return Some(status);
        }
        tracing::debug!("claude did not exit in time; killing it");
        if let Err(error) = self.kill().await {
            tracing::warn!(%error, "could not kill claude");
        }
        None
    }

    async fn drain_stderr(&mut self) {
        if let Some(task) = self.stderr_task.take() {
            let _ = tokio::time::timeout(STDERR_DRAIN, task).await;
        }
    }

    fn stderr_text(&self, secret: Option<&str>) -> String {
        let tail = self.stderr.lock().expect("lock poisoned");
        let text = String::from_utf8_lossy(&tail.0).trim().to_owned();
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

/// A run of the Claude backend.
pub(crate) struct ClaudeRun {
    launch: Launch,
    session: NativeSession,
    process: Option<Process>,
    parser: StreamParser,
    expects_tools: bool,
    pending: VecDeque<BackendEvent>,
    /// The line being read. Kept here so a cancelled `next_event` loses nothing.
    line: Vec<u8>,
    retried: bool,
    done: bool,
    /// The terminal event of the latest `result` line, held until stdout
    /// closes. One process can run several turns, each ending in a `result`
    /// (a resumed session first answers a leftover background-task
    /// notification), and the stream ends on the last one, so the run ends
    /// on the last `result` seen, never on the first.
    held: Option<BackendEvent>,
    server: Option<McpServer>,
    /// Holds the MCP config file; removed when the run is dropped.
    _config_dir: Option<TempDir>,
}

impl fmt::Debug for ClaudeRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeRun")
            .field("launch", &self.launch)
            .field("session", &self.session)
            .field("pid", &self.process.as_ref().and_then(|p| p.child.id()))
            .field("pending", &self.pending.len())
            .field("retried", &self.retried)
            .field("done", &self.done)
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}

impl ClaudeRun {
    /// Spawns claude for `session`.
    pub(crate) fn start(
        launch: Launch,
        session: NativeSession,
        server: Option<McpServer>,
        config_dir: Option<TempDir>,
    ) -> std::io::Result<Self> {
        let process = launch.spawn(&session)?;
        let expects_tools = server.is_some();
        Ok(Self {
            launch,
            session,
            process: Some(process),
            parser: StreamParser::new(expects_tools),
            expects_tools,
            pending: VecDeque::new(),
            line: Vec::new(),
            retried: false,
            done: false,
            held: None,
            server,
            _config_dir: config_dir,
        })
    }

    fn secret(&self) -> Option<&str> {
        self.server.as_ref().map(McpServer::token)
    }

    fn end_with(&mut self, event: BackendEvent) {
        self.pending.push_back(event);
        self.done = true;
    }

    async fn take_line(&mut self) {
        let bytes = mem::take(&mut self.line);
        let mut events = self.parser.line(&String::from_utf8_lossy(&bytes));
        if self.parser.ended_by_result() {
            // Another turn may follow; the last result ends the run at EOF.
            self.held = events.pop();
            self.parser.next_turn();
        }
        self.pending.extend(events);
        if !self.parser.ended() {
            return;
        }
        self.done = true;
        if let Some(process) = self.process.as_mut()
            && let Err(error) = process.kill().await
        {
            tracing::warn!(%error, "could not kill claude");
        }
    }

    async fn on_eof(&mut self) {
        let secret = self.secret().map(str::to_owned);
        let Some(process) = self.process.as_mut() else {
            self.done = true;
            return;
        };
        let status = process.exit_within(EXIT_AFTER_EOF).await;
        if let Some(result) = self.held.take() {
            self.end_with(result);
            return;
        }
        process.drain_stderr().await;
        let stderr = process.stderr_text(secret.as_deref());

        if let NativeSession::Start { id } = &self.session
            && !self.retried
            && !self.parser.seen_init()
            && stderr.contains(SESSION_IN_USE)
        {
            tracing::info!("claude session already exists; resuming it instead");
            self.retried = true;
            self.session = NativeSession::Resume { id: id.clone() };
            self.parser = StreamParser::new(self.expects_tools);
            self.line.clear();
            match self.launch.spawn(&self.session) {
                Ok(process) => self.process = Some(process),
                Err(error) => {
                    self.process = None;
                    self.end_with(BackendEvent::Failed {
                        error: format!("could not start claude again to resume: {error}"),
                    });
                }
            }
            return;
        }

        let status = status.map_or_else(|| "did not exit".to_owned(), |status| status.to_string());
        let error = if stderr.is_empty() {
            format!("claude ended without a result ({status})")
        } else {
            format!("claude ended without a result ({status}): {stderr}")
        };
        self.end_with(BackendEvent::Failed { error });
    }
}

#[async_trait]
impl BackendRun for ClaudeRun {
    async fn next_event(&mut self) -> Option<BackendEvent> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(event);
            }
            if self.done {
                return None;
            }
            let Some(process) = self.process.as_mut() else {
                self.done = true;
                return None;
            };
            match process.stdout.read_until(b'\n', &mut self.line).await {
                Ok(0) if self.line.is_empty() => self.on_eof().await,
                Ok(_) => self.take_line().await,
                Err(error) => {
                    if let Err(error) = process.kill().await {
                        tracing::warn!(%error, "could not kill claude");
                    }
                    self.end_with(BackendEvent::Failed {
                        error: format!("could not read claude's output: {error}"),
                    });
                }
            }
        }
    }

    async fn stop(&mut self) -> Result<(), BackendError> {
        self.done = true;
        self.pending.clear();
        let killed = match self.process.as_mut() {
            Some(process) => process.kill().await,
            None => Ok(()),
        };
        if let Some(server) = self.server.take() {
            server.shutdown().await;
        }
        killed.map_err(|error| BackendError::Failed(format!("could not stop claude: {error}")))?;
        self.process = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch() -> Launch {
        Launch {
            program: "claude".into(),
            config_dir: None,
            working_dir: "/tmp".into(),
            prompt: String::new(),
            model: None,
            appended_instructions: String::new(),
            mcp_config: None,
            remove_builtin_tools: false,
            deny_rules: Vec::new(),
            max_turns: None,
        }
    }

    #[test]
    fn minimal_command_line() {
        let args = launch().args(&NativeSession::Resume {
            id: "abc".to_owned(),
        });
        assert_eq!(
            args,
            [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "bypassPermissions",
                "--resume",
                "abc"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn the_tail_keeps_the_last_bytes() {
        let mut tail = Tail::default();
        tail.push(&vec![b'a'; STDERR_TAIL_BYTES]);
        tail.push(b"end");
        assert_eq!(tail.0.len(), STDERR_TAIL_BYTES);
        assert!(tail.0.ends_with(b"aend"));
    }
}
