use crate::agent::Agent;
use crate::commands::{execute_slash_command, parse_slash_command, CommandAction};
use crate::context::ProjectContext;
use crate::events::AgentEvent;
use crate::hooks::{AfterToolCallHook, BeforeToolCallHook};
use crate::plan_mode::PlanModeState;
use crate::session_tree::SessionTree;
use crate::types::{
    AfterToolCallResult, AgentMessage, AgentState, AgentToolCall, AgentToolResult,
    BeforeToolCallResult,
};
use crate::wasi_extension::WasiExtensionManager;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

pub struct CodingAgentOptions {
    pub api_key: String,
    pub account_id: Option<String>,
    pub model: String,
    pub work_dir: PathBuf,
    pub session_file: Option<PathBuf>,
    pub enable_plan_mode: bool,
}

pub struct PlanModeBeforeHook {
    pub plan_state: Arc<tokio::sync::Mutex<PlanModeState>>,
}

#[async_trait]
impl BeforeToolCallHook for PlanModeBeforeHook {
    async fn before_tool_call(
        &self,
        tool_call: &AgentToolCall,
        _state: &AgentState,
    ) -> BeforeToolCallResult {
        let plan = self.plan_state.lock().await;
        let decision = plan
            .harness_policy()
            .evaluate_tool_call(&tool_call.name, &tool_call.arguments);

        BeforeToolCallResult {
            block: decision.block,
            reason: decision.reason,
        }
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
    pub plan_mode: Arc<tokio::sync::Mutex<PlanModeState>>,
    pub work_dir: PathBuf,
    base_system_prompt: String,
}

impl CodingAgent {
    pub fn new(options: CodingAgentOptions) -> Self {
        let mut agent = Agent::new(&options.api_key, options.account_id, &options.model);
        let project_context = ProjectContext::discover(&options.work_dir);

        let mut wasi_extensions = WasiExtensionManager::for_project(&options.work_dir);
        let loaded_ext_count = wasi_extensions.discover_and_load(&options.work_dir);

        let mut plan_mode_state = PlanModeState::new();
        if options.enable_plan_mode {
            plan_mode_state.enabled = true;
        }
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

        let base_system_prompt = system_prompt;
        let active_system_prompt = format!(
            "{}{}",
            base_system_prompt,
            plan_mode_state.system_prompt_instructions()
        );
        let plan_mode_arc = Arc::new(tokio::sync::Mutex::new(plan_mode_state));
        let wasi_extensions = Arc::new(wasi_extensions);
        agent.loop_engine.extension_manager = Some(wasi_extensions.clone());
        agent.loop_engine.work_dir = Some(options.work_dir.clone());

        agent.loop_engine.before_tool_call_hook = Some(Arc::new(PlanModeBeforeHook {
            plan_state: plan_mode_arc.clone(),
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
            state.system_prompt = active_system_prompt.clone();
            state.tools.extend(wasi_extensions.get_tools());
            state.messages.push(AgentMessage::System {
                content: active_system_prompt,
            });
        }

        let session_path = options
            .session_file
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

        Self {
            agent,
            session_tree,
            project_context,
            wasi_extensions,
            plan_mode: plan_mode_arc,
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

    /// Swap the active session transcript file and rebuild in-memory agent
    /// messages from that session's active branch. System prompt / tools /
    /// extensions stay as configured for this `CodingAgent` (same work_dir).
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
                                    let mut plan = self.plan_mode.lock().await;
                                    match policy.as_str() {
                                        "read_only" => plan.enabled = true,
                                        "full" => plan.enabled = false,
                                        _ => continue,
                                    }
                                    self.agent
                                        .set_system_prompt(format!(
                                            "{}{}",
                                            self.base_system_prompt,
                                            plan.system_prompt_instructions()
                                        ))
                                        .await;
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
                let mut plan_lock = self.plan_mode.lock().await;
                let output = execute_slash_command(
                    cmd_action,
                    &mut self.agent,
                    &mut self.session_tree,
                    &mut plan_lock,
                )
                .await;
                return Some(output);
            }
        }

        let msg = AgentMessage::User {
            content: effective_input.to_string(),
        };
        self.session_tree.add_message(msg);
        self.agent.prompt(effective_input).await;

        let mut plan_lock = self.plan_mode.lock().await;
        let st = self.agent.get_state().await;
        for msg in st.messages.iter().rev() {
            if let AgentMessage::Assistant {
                content: Some(ref text),
                ..
            } = msg
            {
                if plan_lock.parse_and_update_plan(text) > 0 {
                    break;
                }
            }
        }
        drop(plan_lock);
        self.dispatch_assistant_message_hooks().await;

        None
    }
}
