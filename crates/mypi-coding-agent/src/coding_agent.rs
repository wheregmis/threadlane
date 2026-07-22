use crate::commands::{execute_slash_command, parse_slash_command, CommandAction};
use crate::context::ProjectContext;
use crate::wasi_extension::WasiExtensionManager;
use async_trait::async_trait;
use mypi_agent::{
    AfterToolCallHook, AfterToolCallResult, Agent, AgentEvent, AgentMessage, AgentState,
    AgentToolCall, AgentToolResult, BeforeToolCallHook, BeforeToolCallResult, SessionTree,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    FullAccess,
    ReadOnly,
}

pub struct CodingAgentOptions {
    pub api_key: String,
    pub account_id: Option<String>,
    pub model: String,
    pub work_dir: PathBuf,
    pub session_file: Option<PathBuf>,
    pub enable_plan_mode: bool,
}

pub struct ExtensionBeforeToolHook {
    pub tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    pub extensions: Arc<WasiExtensionManager>,
}

#[async_trait]
impl BeforeToolCallHook for ExtensionBeforeToolHook {
    async fn before_tool_call(
        &self,
        tool_call: &AgentToolCall,
        _state: &AgentState,
    ) -> BeforeToolCallResult {
        let policy = *self.tool_policy.lock().await;
        if policy == ToolPolicy::ReadOnly {
            if matches!(
                tool_call.name.as_str(),
                "write_file" | "edit_file" | "write" | "edit"
            ) {
                return BeforeToolCallResult {
                    block: true,
                    reason: Some(format!(
                        "Tool `{}` is blocked because read-only tool policy is ACTIVE.",
                        tool_call.name
                    )),
                };
            }
        }

        let arguments = serde_json::json!({
            "tool_name": tool_call.name,
            "tool_arguments": tool_call.arguments,
        });
        let hook_responses = self
            .extensions
            .execute_hook("before_tool_call", &arguments.to_string());
        for resp in hook_responses {
            if let Ok(res) = resp {
                if let Some(msg) = res.message {
                    if msg.contains("blocked") {
                        return BeforeToolCallResult {
                            block: true,
                            reason: Some(msg),
                        };
                    }
                }
            }
        }

        BeforeToolCallResult::default()
    }
}

pub struct ExtensionAfterToolHook {
    pub extensions: Arc<WasiExtensionManager>,
}

#[async_trait]
impl AfterToolCallHook for ExtensionAfterToolHook {
    async fn after_tool_call(
        &self,
        tool_call: &AgentToolCall,
        result: &AgentToolResult,
        _state: &AgentState,
    ) -> AfterToolCallResult {
        let arguments = serde_json::json!({
            "tool_name": tool_call.name,
            "tool_arguments": tool_call.arguments,
            "result": result.content,
            "is_error": result.is_error,
        });
        let _ = self
            .extensions
            .execute_hook("after_tool_call", &arguments.to_string());
        AfterToolCallResult::default()
    }
}

pub struct CodingAgent {
    pub agent: Agent,
    pub session_tree: SessionTree,
    pub project_context: ProjectContext,
    pub wasi_extensions: Arc<WasiExtensionManager>,
    pub tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    pub work_dir: PathBuf,
    base_system_prompt: String,
}

impl CodingAgent {
    pub fn new(options: CodingAgentOptions) -> Self {
        let mut agent = Agent::new(&options.api_key, options.account_id, &options.model);
        let project_context = ProjectContext::discover(&options.work_dir);

        let session_path = options
            .session_file
            .clone()
            .unwrap_or_else(|| options.work_dir.join(".mypi/sessions/default.jsonl"));
        let session_tree = if session_path.exists() {
            SessionTree::load_from_file(&session_path)
                .unwrap_or_else(|_| SessionTree::new("default"))
        } else {
            if let Some(parent) = session_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut session = SessionTree::new("default");
            session.file_path = Some(session_path);
            session
        };

        let mut wasi_extensions = WasiExtensionManager::for_project_session(
            &options.work_dir,
            session_tree.session_id.clone(),
        );
        let loaded_ext_count = wasi_extensions.discover_and_load(&options.work_dir);
        let restored_plan_mode = wasi_extensions
            .extension_state("plan_mode_ext")
            .and_then(|state| state.get("enabled").and_then(serde_json::Value::as_bool))
            .unwrap_or(false);
        let tool_policy = Arc::new(tokio::sync::Mutex::new(
            if options.enable_plan_mode || restored_plan_mode {
                ToolPolicy::ReadOnly
            } else {
                ToolPolicy::FullAccess
            },
        ));

        let mut system_prompt = format!(
            "You are mypi, an AI coding agent with tool execution capability in workspace: {}.\n\
            Always use the provided tools (read_file, write_file, edit_file, list_dir, run_command) \
            to inspect code, modify files, and run tests. Be precise, concise, and double-check your work.",
            options.work_dir.display()
        );

        if loaded_ext_count > 0 {
            system_prompt.push_str(&format!(
                "\n\nLoaded {} WASI extensions into sandboxed execution environment.",
                loaded_ext_count
            ));
        }

        if !project_context.combined_instructions.is_empty() {
            system_prompt.push_str("\n\n=== Workspace Instructions ===");
            system_prompt.push_str(&project_context.combined_instructions);
        }

        let base_system_prompt = system_prompt.clone();
        let wasi_extensions = Arc::new(wasi_extensions);
        agent.loop_engine.extension_manager = Some(wasi_extensions.clone());
        agent.loop_engine.work_dir = Some(options.work_dir.clone());

        agent.loop_engine.before_tool_call_hook = Some(Arc::new(ExtensionBeforeToolHook {
            tool_policy: tool_policy.clone(),
            extensions: wasi_extensions.clone(),
        }));
        agent.loop_engine.after_tool_call_hook = Some(Arc::new(ExtensionAfterToolHook {
            extensions: wasi_extensions.clone(),
        }));

        {
            let mut state = agent
                .loop_engine
                .state
                .try_lock()
                .expect("Failed to lock initial state");
            state.system_prompt = base_system_prompt.clone();
            state.tools.extend(wasi_extensions.get_tools());
            state.messages.push(AgentMessage::System {
                content: base_system_prompt.clone(),
            });
        }

        Self {
            agent,
            session_tree,
            project_context,
            wasi_extensions,
            tool_policy,
            work_dir: options.work_dir,
            base_system_prompt,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.agent.subscribe()
    }

    async fn dispatch_assistant_message_hooks(&mut self) {
        let state = self.agent.get_state().await;
        if let Some(AgentMessage::Assistant {
            content: Some(content),
            tool_calls,
        }) = state
            .messages
            .iter()
            .rev()
            .find(|message| matches!(message, AgentMessage::Assistant { .. }))
        {
            let arguments = serde_json::json!({ "content": content });
            let _ = self
                .wasi_extensions
                .execute_hook("assistant_message", &arguments.to_string());
            self.session_tree.add_message(AgentMessage::Assistant {
                content: Some(content.clone()),
                tool_calls: tool_calls.clone(),
            });
        }
    }

    pub async fn switch_session_file(&mut self, session_file: PathBuf) {
        let session_tree = if session_file.exists() {
            SessionTree::load_from_file(&session_file).unwrap_or_else(|_| {
                let mut tree = SessionTree::new(
                    session_file
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "session".into()),
                );
                tree.file_path = Some(session_file.clone());
                tree
            })
        } else {
            if let Some(parent) = session_file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut tree = SessionTree::new(
                session_file
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "session".into()),
            );
            tree.file_path = Some(session_file);
            tree
        };

        let branch = session_tree.get_active_branch_messages();
        self.wasi_extensions
            .set_session_scope(session_tree.session_id.clone())
            .unwrap_or_else(|error| {
                eprintln!("Failed to restore session extension state: {error}")
            });
        let restored_plan_mode = self
            .wasi_extensions
            .extension_state("plan_mode_ext")
            .and_then(|state| state.get("enabled").and_then(serde_json::Value::as_bool))
            .unwrap_or(false);
        *self.tool_policy.lock().await = if restored_plan_mode {
            ToolPolicy::ReadOnly
        } else {
            ToolPolicy::FullAccess
        };
        self.session_tree = session_tree;

        let mut state = self.agent.loop_engine.state.lock().await;
        let system_prompt = state.system_prompt.clone();
        state.messages.clear();
        state.messages.push(AgentMessage::System {
            content: system_prompt,
        });
        for msg in branch {
            if matches!(msg, AgentMessage::System { .. }) {
                continue;
            }
            state.messages.push(msg);
        }
        state.is_streaming = false;
        state.pending_tool_calls.clear();
    }

    pub fn session_file_path(&self) -> Option<&PathBuf> {
        self.session_tree.file_path.as_ref()
    }

    pub async fn handle_input(&mut self, input: &str) -> Option<String> {
        let trimmed = input.trim();

        // 1. Expand prompt templates (e.g. /review, /component Button) if match
        let global_dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join(".mypi"))
            .unwrap_or_else(|| self.work_dir.join(".mypi"));
        let templates = crate::prompt_templates::load_prompt_templates(&self.work_dir, &global_dir);
        let expanded_input = crate::prompt_templates::expand_prompt_template(trimmed, &templates);
        let effective_input = expanded_input.trim();

        if effective_input.starts_with('/') {
            let mut parts = effective_input[1..].split_whitespace();
            let cmd_name = parts.next().unwrap_or("");
            let cmd_args = parts.collect::<Vec<&str>>().join(" ");

            if cmd_name.starts_with("skill:") || cmd_name == "skill" {
                let skill_name = if cmd_name.starts_with("skill:") {
                    &cmd_name[6..]
                } else {
                    cmd_args.trim()
                };

                let mut skill_mgr = crate::skills::SkillManager::new();
                skill_mgr.discover_skills(Some(&self.work_dir));
                match skill_mgr.get_skill_instructions(skill_name) {
                    Ok(instructions) => {
                        let prompt = format!(
                            "Use the following Skill instructions for '{}':\n\n{}",
                            skill_name, instructions
                        );
                        self.session_tree.add_message(AgentMessage::User {
                            content: input.to_string(),
                        });
                        self.agent.prompt(&prompt).await;
                        self.dispatch_assistant_message_hooks().await;
                        return Some(format!("Loaded skill '{}'", skill_name));
                    }
                    Err(err) => return Some(format!("Skill Error: {}", err)),
                }
            }

            if let Some(res) = self
                .wasi_extensions
                .execute_command_with_effects(cmd_name, &cmd_args)
            {
                self.session_tree.add_message(AgentMessage::User {
                    content: input.to_string(),
                });
                return match res {
                    Ok(result) => {
                        for effect in result.effects {
                            match effect {
                                crate::wasi_extension::WasiExtensionEffect::SetToolPolicy {
                                    policy,
                                } => {
                                    let mut pol = self.tool_policy.lock().await;
                                    match policy.as_str() {
                                        "read_only" => *pol = ToolPolicy::ReadOnly,
                                        "full" => *pol = ToolPolicy::FullAccess,
                                        _ => continue,
                                    }
                                }
                                crate::wasi_extension::WasiExtensionEffect::RequestModelTurn {
                                    prompt,
                                } => {
                                    self.agent.prompt(&prompt).await;
                                    self.dispatch_assistant_message_hooks().await;
                                }
                            }
                        }
                        Some(result.message)
                    }
                    Err(err) => Some(format!("WASI Extension Error: {}", err)),
                };
            }

            if let Some(cmd_action) = parse_slash_command(effective_input) {
                if cmd_action == CommandAction::Quit {
                    return Some("quitting".to_string());
                }
                let output =
                    execute_slash_command(cmd_action, &mut self.agent, &mut self.session_tree)
                        .await;
                return Some(output);
            }
        }

        let msg = AgentMessage::User {
            content: effective_input.to_string(),
        };
        self.session_tree.add_message(msg);
        self.agent.prompt(effective_input).await;
        self.dispatch_assistant_message_hooks().await;

        None
    }
}
