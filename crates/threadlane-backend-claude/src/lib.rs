//! Drives the Claude command-line agent (Plan.md §5.7).
//!
//! See `docs/milestone-1.md` §5. [`ClaudeBackend`] runs `claude -p` with the
//! stream-json output format, serves the run's tools to it through a
//! [`threadlane_mcp::McpServer`], and maps its output to
//! [`BackendEvent`](threadlane_core::backend::BackendEvent)s.

mod run;
mod stream;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use threadlane_core::backend::{
    AgentBackend, AskRequest, BackendError, BackendRun, Probe, RunRequest, with_warnings,
};
use threadlane_core::model::BackendKind;
use threadlane_mcp::{McpServer, SERVER_NAME};
use tokio::process::Command;

use crate::run::{ClaudeRun, Launch};

/// How long `claude --version` and `claude auth status` may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Where and how to run Claude.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeConfig {
    /// The `claude` executable: a path, or a name looked up on `PATH`.
    pub program: PathBuf,
    /// Sets `CLAUDE_CONFIG_DIR`, so this membership uses a separate login
    /// (Plan.md §9.3). `None` uses the machine's ordinary login.
    pub config_dir: Option<PathBuf>,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            program: PathBuf::from("claude"),
            config_dir: None,
        }
    }
}

/// The Claude command-line agent as an [`AgentBackend`].
#[derive(Debug, Clone)]
pub struct ClaudeBackend {
    config: ClaudeConfig,
}

impl ClaudeBackend {
    /// A backend that runs `config.program`.
    #[must_use]
    pub fn new(config: ClaudeConfig) -> Self {
        Self { config }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.config.program);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = &self.config.config_dir {
            command.env("CLAUDE_CONFIG_DIR", dir);
        }
        command
    }

    /// Runs `claude <args>` to completion and returns its output.
    async fn output(&self, args: &[&str]) -> Result<std::process::Output, ProbeError> {
        let mut command = self.command();
        command.args(args);
        match tokio::time::timeout(PROBE_TIMEOUT, command.output()).await {
            Err(_) => Err(ProbeError::Timeout),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(ProbeError::NotInstalled)
            }
            Ok(Err(error)) => Err(ProbeError::Io(error)),
            Ok(Ok(output)) => Ok(output),
        }
    }
}

impl ClaudeBackend {
    /// Personal skills: `skills` under the configured Claude home.
    fn skills_dir(&self) -> Option<PathBuf> {
        self.config
            .config_dir
            .clone()
            .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
            .or_else(|| std::env::home_dir().map(|dir| dir.join(".claude")))
            .map(|dir| dir.join("skills"))
    }
}

/// Whether the SKILL.md frontmatter sets `disable-model-invocation: true`, which keeps
/// Claude's Skill tool from starting it. Reads only that one key.
fn model_invocation_disabled(file: &Path) -> bool {
    std::fs::read_to_string(file).is_ok_and(|text| frontmatter_disables_model_invocation(&text))
}

// shortcut: a top-level `key: value` line, not full YAML; switch to a YAML parser if skills use block forms.
fn frontmatter_disables_model_invocation(text: &str) -> bool {
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return false;
    }
    lines
        .take_while(|line| line.trim_end() != "---")
        .filter_map(|line| line.split_once(':'))
        .any(|(key, value)| {
            let unquote = |text: &str| text.trim().trim_matches(['"', '\'']).to_owned();
            let value = value.split(" #").next().unwrap_or_default();
            !key.starts_with(char::is_whitespace)
                && unquote(key) == "disable-model-invocation"
                && matches!(unquote(value).as_str(), "true" | "True" | "TRUE")
        })
}

impl Default for ClaudeBackend {
    fn default() -> Self {
        Self::new(ClaudeConfig::default())
    }
}

enum ProbeError {
    NotInstalled,
    Timeout,
    Io(std::io::Error),
}

#[async_trait]
impl AgentBackend for ClaudeBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Claude
    }

    async fn installed_skills(&self, _cwd: Option<&Path>) -> Result<Vec<String>, BackendError> {
        let mut names = Vec::new();
        if let Some(root) = self.skills_dir() {
            let entries = match std::fs::read_dir(root) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => {
                    return Err(BackendError::Failed(format!(
                        "cannot list personal skills: {e}"
                    )));
                }
            };
            for entry in entries {
                let entry = entry.map_err(|e| BackendError::Failed(e.to_string()))?;
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if !threadlane_core::model::SkillBinding::valid_name(&name) {
                    continue;
                }
                match std::fs::metadata(entry.path().join("SKILL.md")) {
                    Ok(meta) if meta.is_file() => {
                        if !model_invocation_disabled(&entry.path().join("SKILL.md")) {
                            names.push(name);
                        }
                    }
                    Ok(_) => {}
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                        ) => {}
                    Err(e) => {
                        return Err(BackendError::Failed(format!(
                            "cannot read personal skill {name}: {e}"
                        )));
                    }
                }
            }
        }
        names.sort();
        Ok(names)
    }

    async fn unusable_skill(&self, name: &str) -> Option<String> {
        let file = self.skills_dir()?.join(name).join("SKILL.md");
        (threadlane_core::model::SkillBinding::valid_name(name) && model_invocation_disabled(&file))
            .then(|| {
                "sets disable-model-invocation and can only be started by slash command; \
                 remove that line from its SKILL.md or unbind it"
                    .to_owned()
            })
    }

    async fn probe(&self) -> Result<Probe, BackendError> {
        let failed = |what: &str, error: ProbeError| match error {
            ProbeError::NotInstalled => BackendError::NotInstalled,
            ProbeError::Timeout => BackendError::Timeout,
            ProbeError::Io(error) => BackendError::Failed(format!("{what}: {error}")),
        };
        let version = match self.output(&["--version"]).await {
            Ok(output) if output.status.success() => output,
            Ok(output) => {
                return Err(BackendError::Failed(format!(
                    "claude --version failed ({}): {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            Err(ProbeError::NotInstalled) => {
                return Ok(Probe {
                    installed: false,
                    version: None,
                    logged_in: false,
                    account_label: None,
                });
            }
            Err(error) => return Err(failed("claude --version", error)),
        };
        let version = String::from_utf8_lossy(&version.stdout)
            .split_whitespace()
            .next()
            .map(str::to_owned);

        let auth = self
            .output(&["auth", "status"])
            .await
            .map_err(|error| failed("claude auth status", error))?;
        let logged_in = match serde_json::from_slice::<Json>(&auth.stdout) {
            Ok(status) => status.get("loggedIn").and_then(Json::as_bool) == Some(true),
            // Logged out may be reported only through the exit status.
            Err(_) if !auth.status.success() => false,
            Err(error) => {
                return Err(BackendError::Failed(format!(
                    "claude auth status printed something other than JSON: {error}"
                )));
            }
        };
        Ok(Probe {
            installed: true,
            version,
            logged_in,
            account_label: None,
        })
    }

    async fn run(&self, mut request: RunRequest) -> Result<Box<dyn BackendRun>, BackendError> {
        let warnings = self.apply_skills(&mut request).await?;
        if !request.working_dir.is_dir() {
            return Err(BackendError::Failed(format!(
                "working directory {} does not exist",
                request.working_dir.display()
            )));
        }
        let (server, config_dir, mcp_config) = match request.tools {
            Some(endpoint) => {
                let server = McpServer::start(endpoint).await.map_err(|error| {
                    BackendError::Failed(format!("could not start the tool server: {error}"))
                })?;
                let dir = tempfile::Builder::new()
                    .prefix("threadlane-claude-")
                    .tempdir()
                    .map_err(|error| {
                        BackendError::Failed(format!("could not create a temp directory: {error}"))
                    })?;
                let path = dir.path().join("mcp.json");
                write_private(&path, &mcp_config(&server))
                    .await
                    .map_err(|error| {
                        BackendError::Failed(format!("could not write the MCP config: {error}"))
                    })?;
                (Some(server), Some(dir), Some(path))
            }
            None => (None, None, None),
        };
        let launch = Launch {
            program: self.config.program.clone(),
            config_dir: self.config.config_dir.clone(),
            working_dir: request.working_dir,
            prompt: request.prompt,
            images: request.images,
            model: request.model,
            appended_instructions: request.appended_instructions,
            mcp_config,
            remove_builtin_tools: request.remove_builtin_tools,
            deny_rules: request.deny_rules,
            max_turns: request.max_turns,
        };
        match ClaudeRun::start(launch, request.session, server, config_dir) {
            Ok(run) => Ok(with_warnings(warnings, Box::new(run))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(BackendError::NotInstalled)
            }
            Err(error) => Err(BackendError::Failed(format!(
                "could not start claude: {error}"
            ))),
        }
    }

    async fn ask(&self, request: AskRequest) -> Result<Json, BackendError> {
        let dir = tempfile::tempdir().map_err(|e| BackendError::Failed(e.to_string()))?;
        let mut command = self.command();
        command
            .current_dir(dir.path())
            .env("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1")
            .args([
                "-p",
                "--output-format",
                "json",
                "--tools",
                "",
                "--strict-mcp-config",
                "--no-session-persistence",
                "--setting-sources",
                "",
                "--json-schema",
            ])
            .arg(request.answer_schema.to_string());
        if let Some(model) = request.model {
            command.arg("--model").arg(model);
        }
        command.arg("--").arg(request.question);
        let output = tokio::time::timeout(request.timeout, command.output())
            .await
            .map_err(|_| BackendError::Timeout)?
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    BackendError::NotInstalled
                } else {
                    BackendError::Failed(e.to_string())
                }
            })?;
        let result: Json = serde_json::from_slice(&output.stdout)
            .map_err(|e| BackendError::Failed(format!("invalid Claude result: {e}")))?;
        if result["type"] != "result" {
            return Err(BackendError::Failed("missing Claude result".into()));
        }
        match stream::StreamParser::new(false).result(
            result
                .as_object()
                .ok_or_else(|| BackendError::Failed("invalid result".into()))?,
        ) {
            threadlane_core::backend::BackendEvent::UsageLimit { resets_at } => {
                Err(BackendError::UsageLimit { resets_at })
            }
            threadlane_core::backend::BackendEvent::Finished { .. } if output.status.success() => {
                result
                    .get("structured_output")
                    .cloned()
                    .ok_or_else(|| BackendError::Failed("missing structured answer".into()))
            }
            _ => Err(BackendError::Failed(
                "Claude could not answer the question".into(),
            )),
        }
    }
}

fn mcp_config(server: &McpServer) -> String {
    json!({
        "mcpServers": {
            SERVER_NAME: {
                "type": "http",
                "url": server.url(),
                "headers": {"Authorization": format!("Bearer {}", server.token())},
            }
        }
    })
    .to_string()
}

/// Writes a file only the current user can read: it holds the bearer token.
async fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).await?;
    file.write_all(contents.as_bytes()).await?;
    file.flush().await
}

#[cfg(test)]
mod frontmatter_tests {
    use super::frontmatter_disables_model_invocation as disables;

    #[test]
    fn reads_the_flag_in_its_yaml_spellings() {
        for text in [
            "---\ndisable-model-invocation: true\n---",
            "---\ndisable-model-invocation: true # manual only\n---",
            "---\n\"disable-model-invocation\": True\n---",
            "---\r\n'disable-model-invocation':TRUE\r\n---\r\n",
        ] {
            assert!(disables(text), "{text:?}");
        }
        for text in [
            "---\ndisable-model-invocation: false\n---",
            "---\nname: x\n---\ndisable-model-invocation: true",
            "disable-model-invocation: true",
            "---\n  disable-model-invocation: true\n---",
        ] {
            assert!(!disables(text), "{text:?}");
        }
    }
}
