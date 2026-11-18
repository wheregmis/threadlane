//! Drives Google Antigravity CLI in headless mode.
mod run;

use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tempfile::TempDir;
use threadlane_core::{
    backend::{AgentBackend, AskRequest, BackendError, BackendRun, Probe, RunRequest},
    model::BackendKind,
};
use threadlane_mcp::{McpServer, SERVER_NAME};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct AntigravityBackend {
    program: PathBuf,
}

impl AntigravityBackend {
    #[must_use]
    pub fn new(program: PathBuf) -> Self {
        Self { program }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.kill_on_drop(true).stdin(Stdio::null());
        cmd
    }
}

fn failed(error: impl std::fmt::Display) -> BackendError {
    BackendError::Failed(error.to_string())
}

fn spawn_error(error: std::io::Error) -> BackendError {
    if error.kind() == std::io::ErrorKind::NotFound {
        BackendError::NotInstalled
    } else {
        failed(error)
    }
}

// agy resumes the agent name recorded in the conversation, even with a new --agent.
const AGENT: &str = "threadlane-runner";

// agy persists the server declaration in its conversation. Resolve the lease-bound
// endpoint from the current process on each launch, including native resume.
const MCP_BRIDGE: &str = r#"while IFS= read -r line; do
    reply=$(printf '%s' "$line" | curl -q --silent --show-error --fail --noproxy '*' --header "Authorization: Bearer $THREADLANE_AGY_MCP_TOKEN" --header 'Content-Type: application/json' --data-binary @- "$THREADLANE_AGY_MCP_URL") || exit 1
    if [ -n "$reply" ]; then printf '%s\n' "$reply"; fi
done"#;

struct Profile {
    dir: TempDir,
    server: Option<McpServer>,
}

impl Profile {
    async fn create(request: Option<&RunRequest>) -> Result<Self, BackendError> {
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let dir = builder
            .prefix("threadlane-agy-")
            .tempdir()
            .map_err(failed)?;
        let agents = dir.path().join(".agents/agents");
        std::fs::create_dir_all(&agents).map_err(failed)?;
        // A nonempty allowlist is required. finish returns the answer to the caller.
        let mut tools = vec!["finish".to_owned()];
        let instructions = request.map_or("Answer the question using the supplied schema.", |r| {
            r.appended_instructions.as_str()
        });
        if request.is_some_and(|r| !r.remove_builtin_tools) {
            tools.extend(
                [
                    "view_file",
                    "list_dir",
                    "grep_search",
                    "find_by_name",
                    "write_to_file",
                    "replace_file_content",
                    "run_command",
                    "command_status",
                ]
                .map(str::to_owned),
            );
        }
        let mut server = None;
        let mut specs = Vec::new();
        if let Some(endpoint) = request.and_then(|r| r.tools.as_ref()) {
            let mcp = McpServer::start(endpoint.clone()).await.map_err(failed)?;
            let eager: serde_json::Map<_, _> = endpoint
                .definitions()
                .into_iter()
                .map(|definition| (definition.name, json!({"eager": true})))
                .collect();
            let config = json!({
                "name": SERVER_NAME,
                "command": "sh",
                "args": ["-c", MCP_BRIDGE],
                "tools": eager
            });
            specs.push(config);
            server = Some(mcp);
        }
        let profile = format!(
            "---\nname: {AGENT}\ndescription: Threadlane run\nmainAgent: true\nsubagent: false\ninheritMcp: false\nmcpServers: {}\ntools: {}\n---\n{instructions}\n",
            json!(specs),
            json!(tools)
        );
        std::fs::write(agents.join(format!("{AGENT}.md")), profile).map_err(failed)?;
        Ok(Self { dir, server })
    }

    fn configure(&self, cmd: &mut Command, work: &Path, model: Option<&str>) {
        if let Some(server) = &self.server {
            cmd.env("THREADLANE_AGY_MCP_URL", server.url())
                .env("THREADLANE_AGY_MCP_TOKEN", server.token());
        }
        cmd.current_dir(work)
            .arg("--add-dir")
            .arg(self.dir.path())
            .arg("--agent")
            .arg(AGENT)
            .arg("--disable-slash-commands");
        if let Some(model) = model {
            cmd.arg("--model").arg(model);
        }
    }
}

#[async_trait]
impl AgentBackend for AntigravityBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Antigravity
    }

    async fn probe(&self) -> Result<Probe, BackendError> {
        let probe = async {
            let mut report = Probe {
                installed: false,
                version: None,
                logged_in: false,
                account_label: None,
            };
            let dir = tempfile::tempdir().map_err(failed)?;
            let output = match self
                .command()
                .current_dir(dir.path())
                .arg("--version")
                .output()
                .await
            {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
                other => other.map_err(failed)?,
            };
            if !output.status.success() {
                return Err(failed("agy --version failed"));
            }
            report.installed = true;
            report.version = Some(String::from_utf8_lossy(&output.stdout).trim().to_owned());
            let usage = self
                .command()
                .current_dir(dir.path())
                .args(["-p", "/usage"])
                .output()
                .await
                .map_err(failed)?;
            report.logged_in = usage.status.success() && !usage.stdout.is_empty();
            Ok(report)
        };
        tokio::time::timeout(Duration::from_secs(30), probe)
            .await
            .map_err(|_| BackendError::Timeout)?
    }

    async fn run(&self, request: RunRequest) -> Result<Box<dyn BackendRun>, BackendError> {
        if !request.images.is_empty() {
            return Err(failed("Antigravity CLI stream input supports text only"));
        }
        let profile = Profile::create(Some(&request)).await?;
        let mut cmd = self.command();
        profile.configure(&mut cmd, &request.working_dir, request.model.as_deref());
        let deny: Vec<_> = request
            .deny_rules
            .iter()
            .map(|r| format!("command({r})"))
            .collect();
        cmd.args(["--sandbox", "--settings"])
            .arg(json!({"toolPermission":"proceed-in-sandbox","permissions":{"deny":deny,"allow":["mcp(threadlane/*)"]}}).to_string());
        Ok(Box::new(run::Run::start(cmd, profile, request)?))
    }

    async fn ask(&self, request: AskRequest) -> Result<Value, BackendError> {
        let profile = Profile::create(None).await?;
        let mut cmd = self.command();
        profile.configure(&mut cmd, profile.dir.path(), request.model.as_deref());
        cmd.args(["--output-format", "json", "--json-schema"])
            .arg(request.answer_schema.to_string())
            .arg("--print")
            .arg(request.question);
        let output = tokio::time::timeout(request.timeout, cmd.output())
            .await
            .map_err(|_| BackendError::Timeout)?
            .map_err(spawn_error)?;
        let result: Value = serde_json::from_slice(&output.stdout).map_err(failed)?;
        if !output.status.success() || result["status"] != "SUCCESS" {
            return Err(run::result_error(&result));
        }
        result
            .get("structured_output")
            .cloned()
            .ok_or_else(|| failed("agy returned no structured answer"))
    }
}
