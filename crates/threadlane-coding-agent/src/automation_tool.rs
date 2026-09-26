//! Chat creation forwards to the app's single scheduler owner; it never opens a second store.
use serde::Deserialize;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use threadlane_automation::{Definition, Schedule};
use threadlane_protocol::{AgentToolDefinition, ReasoningEffort, ToolExecutor};
use threadlane_runtime::{
    harness::{JsonlStore, SessionStore},
    Capability,
};
use tokio::sync::{mpsc, oneshot};

pub struct CreationRequest {
    pub definition: Definition,
    pub reply: oneshot::Sender<Result<Definition, String>>,
}
static CREATOR: OnceLock<mpsc::UnboundedSender<CreationRequest>> = OnceLock::new();

/// Installed once by the desktop service. Headless tools fail promptly instead of writing elsewhere.
pub fn install_creator(sender: mpsc::UnboundedSender<CreationRequest>) {
    let _ = CREATOR.set(sender);
}

#[derive(Clone)]
pub(crate) struct AutomationCapability {
    pub work_dir: PathBuf,
    pub session_file: PathBuf,
    pub model: String,
}
impl Capability for AutomationCapability {
    fn id(&self) -> &str {
        "automation"
    }
    fn tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        vec![Arc::new(self.clone())]
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    request_key: String,
    name: String,
    prompt: String,
    schedule: Schedule,
    project: Option<PathBuf>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    worktree: Option<bool>,
    #[serde(default)]
    notify_all: bool,
    #[serde(default = "enabled_default")]
    enabled: bool,
}
fn enabled_default() -> bool {
    true
}

#[async_trait::async_trait]
impl ToolExecutor for AutomationCapability {
    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        vec![AgentToolDefinition {
            name: "create_automation".into(),
            description: Some("Create a Threadlane automation only when the user explicitly asks to schedule or automate work. Saves it in the Automations sidebar, without running immediately. Runs require the app open and computer awake. Default project/model/effort come from this chat; Git projects default to a fresh worktree. Ask the user when the prompt, cadence, or timezone is unclear; never guess a wall-clock timezone. Reuse request_key on retries of the SAME request, and use a new key for a distinct automation. Report success only after this tool succeeds, including project, timezone, enabled state, and next run. Schedule examples: \"Manual\", {\"Interval\":{\"minutes\":60}}, {\"Calendar\":{\"hour\":9,\"minute\":0,\"days\":[0,1,2,3,4],\"timezone\":\"America/Toronto\"}}. Calendar days are Monday=0 through Sunday=6.".into()),
            parameters: json!({
                "type":"object", "additionalProperties":false,
                "required":["request_key","name","prompt","schedule"],
                "properties": {
                    "request_key":{"type":"string","description":"Stable slug for this request, e.g. weekday-review. Letters, digits, hyphens, underscores; max 80 characters."},
                    "name":{"type":"string"}, "prompt":{"type":"string"},
                    "schedule":{"anyOf":[
                        {"type":"string","enum":["Manual"]},
                        {"type":"object","additionalProperties":false,"required":["Interval"],"properties":{"Interval":{"type":"object","additionalProperties":false,"required":["minutes"],"properties":{"minutes":{"type":"integer","minimum":1,"maximum":525600}}}}},
                        {"type":"object","additionalProperties":false,"required":["Calendar"],"properties":{"Calendar":{"type":"object","additionalProperties":false,"required":["hour","minute","days","timezone"],"properties":{"hour":{"type":"integer","minimum":0,"maximum":23},"minute":{"type":"integer","minimum":0,"maximum":59},"days":{"type":"array","minItems":1,"uniqueItems":true,"items":{"type":"integer","minimum":0,"maximum":6}},"timezone":{"type":"string"}}}}}
                    ]},
                    "project":{"type":"string","description":"Optional absolute attached-project path. Defaults to this chat's owning project."},
                    "model":{"type":"string","description":"Optional native model ID, preserving its provider prefix. Defaults to this chat's saved model."},
                    "reasoning_effort":{"type":"string"},
                    "worktree":{"type":"boolean","description":"False permits edits in the project checkout. Only choose false if requested."},
                    "enabled":{"type":"boolean","description":"Defaults true. False saves paused."},
                    "notify_all":{"type":"boolean","description":"Notify for every completion; defaults false (requests and failures only)."}
                }
            }), strict: Some(false),
        }].into()
    }
    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        if name != "create_automation" {
            return None;
        }
        Some(match CREATOR.get() {
            Some(sender) => self.create(args, sender).await,
            None => {
                Err("Automation service unavailable. Open Threadlane to create automations.".into())
            }
        })
    }
}
impl AutomationCapability {
    fn definition(
        &self,
        args: &str,
        registry: &[threadlane_project::ProjectRecord],
    ) -> Result<Definition, String> {
        let args: CreateArgs =
            serde_json::from_str(args).map_err(|e| format!("Invalid automation: {e}"))?;
        if args.request_key.is_empty()
            || args.request_key.len() > 80
            || !args
                .request_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(
                "request_key must be a slug of 1–80 letters, digits, hyphens, or underscores"
                    .into(),
            );
        }
        args.schedule.validate()?;
        let project = if let Some(project) = args.project {
            std::fs::canonicalize(project).map_err(|e| format!("Project unavailable: {e}"))?
        } else {
            let work_dir = std::fs::canonicalize(&self.work_dir).map_err(|e| e.to_string())?;
            registry
                .iter()
                .filter(|p| work_dir.starts_with(&p.path))
                .max_by_key(|p| p.path.components().count())
                .map(|p| p.path.clone())
                .or_else(|| threadlane_git::primary_worktree_root(&work_dir).ok())
                .ok_or("This chat has no attached project. Specify an attached project path")?
        };
        if !registry.iter().any(|p| p.path == project) {
            return Err("Choose an attached project".into());
        }
        let store = JsonlStore::open_read_only(&self.session_file).map_err(|e| e.to_string())?;
        let model = args
            .model
            .or_else(|| store.facts().get("model").cloned())
            .unwrap_or_else(|| self.model.clone());
        let effort = args
            .reasoning_effort
            .or_else(|| store.facts().get("reasoning_effort").cloned())
            .unwrap_or_else(|| ReasoningEffort::default().label().into());
        let effort = ReasoningEffort::from_label(&effort).ok_or("Invalid reasoning effort")?;
        let effort =
            threadlane_provider::model_registry::effective_effort(&model, effort, Some(&project));
        let session_id = self
            .session_file
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("Missing chat identity")?;
        let definition = Definition {
            id: format!("chat-{session_id}-{}", args.request_key),
            revision: 0,
            name: args.name,
            prompt: args.prompt,
            worktree: args
                .worktree
                .unwrap_or_else(|| threadlane_git::is_git_repo(&project)),
            project,
            model,
            effort: effort.label().into(),
            schedule: args.schedule,
            enabled: args.enabled,
            notify_all: args.notify_all,
            anchor: 0,
            next_at: None,
            failures: 0,
            paused_reason: None,
        };
        definition.validate()?;
        Ok(definition)
    }
    async fn create(
        &self,
        args: &str,
        sender: &mpsc::UnboundedSender<CreationRequest>,
    ) -> Result<String, String> {
        let owner = self.clone();
        let args = args.to_owned();
        let definition = threadlane_provider::exec::get_runtime()
            .spawn_blocking(move || {
                owner.definition(&args, &threadlane_project::load_project_registry())
            })
            .await
            .map_err(|e| e.to_string())??;
        Self::persist(definition, sender).await
    }
    async fn persist(
        definition: Definition,
        sender: &mpsc::UnboundedSender<CreationRequest>,
    ) -> Result<String, String> {
        let (reply, result) = oneshot::channel();
        sender
            .send(CreationRequest { definition, reply })
            .map_err(|_| "Automation service unavailable")?;
        let saved = result.await.map_err(|_| {
            "Automation service stopped before confirming creation; retry with the same request_key"
        })??;
        let next_run = saved
            .next_at
            .map(|at| threadlane_automation::display_time(at, saved.schedule.timezone()));
        Ok(json!({"automation":saved, "next_run":next_run, "runs_while":"Threadlane is open and computer is awake", "location":"Automations sidebar", "started_immediately":false}).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chat_creation_resolves_defaults_and_waits_for_durable_save() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().canonicalize().unwrap();
        let session_file = project.join("chat.jsonl");
        crate::harness::CodingSessionHarness::append_fact_to_path(
            &session_file,
            "main",
            "model",
            "opencode-go/glm-5",
            None,
        )
        .unwrap();
        let tool = AutomationCapability {
            work_dir: project.clone(),
            session_file,
            model: "old-model".into(),
        };
        let projects = [threadlane_project::ProjectRecord::from_path(
            project.clone(),
        )];
        let args = json!({"request_key":"daily-review","name":"Review","prompt":"Review changes","worktree":false,
            "schedule":{"Calendar":{"hour":9,"minute":0,"days":[0,1,2,3,4],"timezone":"America/Toronto"}}}).to_string();
        let definition = tool.definition(&args, &projects).unwrap();
        assert_eq!(definition.project, project);
        assert_eq!(definition.model, "opencode-go/glm-5");
        assert!(definition.enabled);
        assert_eq!(definition.id, "chat-chat-daily-review");
        assert!(tool.definition(&args, &[]).is_err());
        assert!(tool
            .definition(
                &args.replace("America/Toronto", "invalid/timezone"),
                &projects
            )
            .is_err());
        assert!(tool.execute_tool("unknown_tool", "{}").await.is_none());
        assert_eq!(tool.tool_definitions()[0].name, "create_automation");

        let storage = project.join("automations");
        let mut store = threadlane_automation::Store::open(&storage).unwrap();
        let (sender, mut receiver) = mpsc::unbounded_channel::<CreationRequest>();
        let owner = tokio::spawn(async move {
            for _ in 0..2 {
                let request = receiver.recv().await.unwrap();
                let saved = store.create_once(request.definition, threadlane_automation::now());
                request.reply.send(saved).unwrap();
            }
            assert_eq!(store.snapshot().definitions.len(), 1);
            assert!(store.snapshot().runs.is_empty());
        });
        let first = AutomationCapability::persist(definition.clone(), &sender)
            .await
            .unwrap();
        let retry = AutomationCapability::persist(definition.clone(), &sender)
            .await
            .unwrap();
        assert_eq!(first, retry);
        let saved: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(saved["automation"]["revision"], 1);
        assert!(saved["next_run"].as_str().unwrap().contains("09:00"));
        owner.await.unwrap();
        let reloaded = threadlane_automation::Store::open(&storage).unwrap();
        assert_eq!(reloaded.snapshot().definitions.len(), 1);
        assert!(AutomationCapability::persist(definition, &sender)
            .await
            .is_err());
    }
}
