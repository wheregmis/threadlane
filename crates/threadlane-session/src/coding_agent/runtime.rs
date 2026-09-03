use super::cancellation::*;
use super::durable::*;
use super::options::*;
use super::scheduler::*;
use super::subagents::*;

use super::broker::ManagedProcessRegistry;
use super::capabilities::{
    build_broker_dispatcher, render_agent_catalog, restored_tool_policy, ContextCapability,
    McpCapability, PlanCapability, PrewalkCapability, SkillCapability, SubagentCapability,
    WasiCapability,
};
use super::harness::{CodingSessionHarness, HarnessWatch, InterruptedSubagentRecoveryState};
use crate::commands::{execute_slash_command, parse_slash_command, CommandAction};
use crate::context::ProjectContext;
use crate::extension_broker::CapabilityDispatcher;
use crate::plan::SessionPlanStore;
use crate::policy::ToolPolicy;
use crate::system_prompt::{build_system_prompt, SystemPromptBuildOptions};
use log::warn;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use threadlane_mcp::McpManager;
use threadlane_protocol::ProviderPort;
use threadlane_provider::openai::fetch_available_models;
use threadlane_provider::router::ProviderClient;
use threadlane_runtime::harness::{OperationOutcome, QueueKind, Reducer, SessionStore, Snapshot};
use threadlane_runtime::{
    AgentEvent, AgentMessage, AgentRuntime, ImageAttachment, ReasoningEffort, TokenUsage,
};
use threadlane_skills::{SkillManager, SkillRegistry};
use threadlane_wasi::packages::default_global_threadlane_dir;
use threadlane_wasi::{WasiExtensionManager, WasiLegacyEffect};
use tokio::sync::broadcast;

pub struct CodingAgent {
    pub(crate) agent: AgentRuntime,
    pub session_id: String,
    pub session_file: Option<PathBuf>,
    pub wasi_extensions: Arc<WasiExtensionManager>,
    pub(crate) tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    pub(crate) work_dir: PathBuf,
    pub(crate) agent_config: threadlane_runtime::AgentConfig,
    pub(crate) skills: Arc<SkillRegistry>,
    pub(crate) agent_runner: AgentRunner,
    pub(crate) broker_dispatcher: Arc<CapabilityDispatcher>,
    pub(crate) managed_processes: ManagedProcessRegistry,
    pub(crate) permission_handle: crate::permission::PermissionHandle,
    pub(crate) agent_work: AgentWorkScheduler,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) prompt_templates: Option<Vec<crate::prompt_templates::PromptTemplate>>,
    pub(crate) dispatch_parent_leaf: Arc<std::sync::Mutex<Option<String>>>,
    pub(crate) completed_subagent_lanes: Arc<std::sync::Mutex<Vec<CompletedSubagentLane>>>,
    pub(crate) harness: Option<CodingSessionHarness>,
    pub(crate) harness_journal_error: Option<String>,
    pub(crate) harness_run_id: Arc<std::sync::Mutex<Option<String>>>,
    pub(crate) prewalk: Arc<std::sync::Mutex<Option<crate::orchestrator::PrewalkState>>>,
    pub(crate) cancellation: CodingAgentCancellation,
    pub(crate) interrupted_subagent_recovery: InterruptedSubagentRecoveryState,
    /// Connection to an external ACP agent, opened on first use.
    ///
    /// An ACP agent keeps its own conversation state, so this is held for the
    /// life of the session rather than rebuilt per turn.
    pub(crate) acp: crate::acp_runtime::AcpEngine,
    #[cfg(test)]
    pub(crate) subagent_work_observer: SubagentObserverState,
    #[cfg(test)]
    pub(crate) subagent_branch_observer: Option<SubagentBoundaryObserver>,
}

impl CodingAgent {
    pub fn permission_handle(&self) -> crate::permission::PermissionHandle {
        self.permission_handle.clone()
    }

    pub(crate) fn set_tool_intent_recorder(
        &mut self,
        recorder: Option<threadlane_runtime::ToolIntentRecorder>,
    ) {
        self.agent.tool_dispatcher.tool_intent_recorder = recorder;
    }

    pub(crate) fn set_tool_completion_recorder(
        &mut self,
        recorder: Option<threadlane_runtime::ToolCompletionRecorder>,
    ) {
        self.agent.tool_dispatcher.tool_completion_recorder = recorder;
    }

    pub(crate) async fn run_scheduled_agent_work(&mut self) {
        while self
            .agent_work
            .run_executor(&mut self.agent, self.session_file.as_deref())
            .await
        {
            self.sync_harness_and_dispatch_assistant_hooks().await;
            if let Some(path) = self.session_file.as_deref() {
                if let Err(error) = consume_harness_follow_ups(path) {
                    warn!("Failed to consume queued follow-up: {error}");
                }
                if let Err(error) = consume_harness_queue(path, QueueKind::Steer) {
                    warn!("Failed to consume queued steer: {error}");
                }
                if let Err(error) = consume_harness_queue(path, QueueKind::NextRun) {
                    warn!("Failed to consume queued next-run input: {error}");
                }
            }
        }
    }

    pub fn work_handle(&self) -> CodingAgentWorkHandle {
        CodingAgentWorkHandle::new(self.agent_work.clone(), self.session_file.clone())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.agent.subscribe()
    }

    pub fn harness_snapshot(&mut self) -> Result<Option<Snapshot>, String> {
        let Some(journal) = self.harness.as_mut() else {
            return Ok(None);
        };
        journal.refresh()?;
        journal
            .store
            .snapshot()
            .map(Some)
            .map_err(|error| error.to_string())
    }

    pub fn harness_error(&self) -> Option<&str> {
        self.harness_journal_error.as_deref()
    }

    /// Returns the fully built system prompt used by this runtime when the
    /// agent state is not currently locked by an active turn.
    pub fn system_prompt_snapshot(&self) -> Option<String> {
        self.agent
            .turn
            .try_lock()
            .ok()
            .map(|state| state.system_prompt.clone())
    }

    pub(crate) fn watch_harness(&mut self) -> Result<Option<HarnessWatch>, String> {
        let Some(journal) = self.harness.as_mut() else {
            return Ok(None);
        };
        journal.watch().map(Some)
    }

    pub fn cancellation_handle(&self) -> CodingAgentCancellation {
        self.cancellation.clone()
    }

    pub fn has_interrupted_work(&self) -> bool {
        matches!(
            self.interrupted_subagent_recovery,
            InterruptedSubagentRecoveryState::Pending
        )
    }

    pub async fn resume_interrupted_turn(&mut self) -> Result<usize, String> {
        self.recover_interrupted_subagent_lanes().await
    }

    pub fn set_model_roles(&mut self, roles: threadlane_runtime::ModelRoles) {
        self.agent.set_model_roles(roles);
    }

    pub fn set_needle_enabled(&mut self, enabled: bool) {
        self.agent.set_needle_enabled(enabled);
    }

    pub fn model_roles(&self) -> &threadlane_runtime::ModelRoles {
        self.agent.model_roles()
    }

    /// Settings the selected external agent offers, without connecting.
    ///
    /// Empty when no agent is selected or none has connected yet, which is
    /// what lets a caller read them after a turn without paying to start one.
    pub fn acp_user_config_options(&self) -> Vec<crate::acp::AcpConfigOption> {
        let model = self.agent.model();
        crate::acp_bridge::acp_agent_id(&model)
            .map(|agent_id| self.acp.user_config_options(agent_id))
            .unwrap_or_default()
    }

    /// Settings the selected external agent offers the user, connecting to it
    /// if necessary.
    ///
    /// Returns an empty list for a non-ACP model rather than an error: asking
    /// what an agent offers is a question the UI may ask about any selection.
    pub async fn acp_config_options(&mut self) -> Result<Vec<crate::acp::AcpConfigOption>, String> {
        let model = self.agent.model();
        let Some(agent_id) = crate::acp_bridge::acp_agent_id(&model) else {
            return Ok(Vec::new());
        };
        let event_tx = self.agent.event_tx.clone();
        let permissions = self.permission_handle.clone();
        self.acp
            .ensure_connected(agent_id, &event_tx, &permissions)
            .await
    }

    /// Applies one of the selected external agent's settings.
    pub async fn set_acp_config_option(
        &mut self,
        config_id: &str,
        value: &str,
    ) -> Result<Vec<crate::acp::AcpConfigOption>, String> {
        let model = self.agent.model();
        let agent_id = crate::acp_bridge::acp_agent_id(&model)
            .ok_or_else(|| format!("Model '{model}' is not an ACP agent"))?;
        let event_tx = self.agent.event_tx.clone();
        let permissions = self.permission_handle.clone();
        self.acp
            .set_config_option(agent_id, config_id, value, &event_tx, &permissions)
            .await
    }

    /// Model the live external agent reports it is running, if one is selected
    /// and connected.
    ///
    /// The agent names its own model, so this is only known once a session
    /// exists; before that there is nothing truthful to show.
    pub fn acp_model_label(&self) -> Option<String> {
        let model = self.agent.model();
        let agent_id = crate::acp_bridge::acp_agent_id(&model)?;
        self.acp.model_label(agent_id)
    }

    pub fn model(&self) -> String {
        self.agent.model()
    }

    pub async fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.agent.set_reasoning_effort(effort).await;
    }

    pub async fn available_models(&self) -> Vec<String> {
        let api_key = self.agent.api_key.clone();
        let account_id = self.agent.account_id.clone();
        fetch_available_models(&api_key, account_id.as_deref()).await
    }

    pub async fn reload_extensions(&mut self) -> Result<usize, String> {
        let global_threadlane_dir = default_global_threadlane_dir();
        let loaded = self
            .wasi_extensions
            .reload_from_roots(global_threadlane_dir.as_deref(), Some(&self.work_dir))?;
        self.managed_processes.lock().await.clear();
        Ok(loaded)
    }

    /// Rediscover skills for this project, applying any persisted enable/disable
    /// overrides, and refresh the shared registry and the model-facing system prompt.
    pub fn refresh_skills(&mut self) {
        let mut skill_manager = SkillManager::new();
        skill_manager.discover_skills(Some(&self.work_dir));
        let skills = skill_manager.snapshot();
        self.skills = skills;
    }

    pub async fn refresh_mcp(&self) {
        self.mcp_manager.discover_and_connect().await;
    }

    pub(crate) async fn set_model(&mut self, model: String) -> Result<(), String> {
        let model = model.trim();
        if model.is_empty() {
            return Err("model cannot be empty".into());
        }
        if let Some(journal) = self.harness.as_mut() {
            journal.refresh()?;
            journal
                .store
                .set_fact("main", "model", model.to_string(), None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            self.sync_turn_from_model_context().await?;
        }
        self.agent.turn.lock().await.model = model.to_string();
        Ok(())
    }

    pub(crate) fn set_name(&mut self, name: String) -> Result<(), String> {
        if let Some(journal) = self.harness.as_mut() {
            journal.refresh().map_err(|error| error.to_string())?;
            journal
                .store
                .set_fact("main", "name", name, None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn set_fact(&mut self, key: &str, value: &str) -> Result<(), String> {
        if let Some(journal) = self.harness.as_mut() {
            journal.refresh().map_err(|error| error.to_string())?;
            journal
                .store
                .set_fact("main", key, value.to_string(), None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn new(options: CodingAgentOptions) -> Self {
        let provider = Arc::new(ProviderClient::new(
            &options.api_key,
            options.account_id.clone(),
        ));
        Self::new_with_provider(options, provider)
    }

    pub(crate) fn new_with_provider(
        options: CodingAgentOptions,
        provider: Arc<dyn ProviderPort>,
    ) -> Self {
        let coding_config = options.coding_config.unwrap_or_default();
        let agent_config = options.agent_config.unwrap_or_default();
        let project_context = ProjectContext::discover(&options.work_dir);
        let mut skill_manager = SkillManager::new();
        skill_manager.discover_skills(Some(&options.work_dir));
        let skills = skill_manager.snapshot();
        let skill_catalog = skills.render_model_catalog();

        let session_file = options.session_file.clone();
        let session_id = session_file
            .as_ref()
            .and_then(|path| path.file_stem())
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "draft".into());

        if let Some(ref path) = session_file {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
        }

        let mut effective_model = options.model.clone();
        let (mut harness, harness_journal_error) = match session_file.as_deref() {
            Some(path) => match super::harness::CodingSessionHarness::open(path) {
                Ok(h) => (Some(h), None),
                Err(error) => (None, Some(error)),
            },
            None => (None, None),
        };
        let mut initial_plan = threadlane_runtime::SessionPlan::default();
        if let Some(h) = harness.as_ref() {
            if let Some(model) = h.store.facts().get("model") {
                effective_model = model.clone();
            }
            if let Some(plan_json) = h.store.facts().get("session_plan") {
                if let Ok(plan) = serde_json::from_str::<threadlane_runtime::SessionPlan>(plan_json)
                {
                    initial_plan = plan;
                }
            }
        }
        let has_interrupted_subagents = match harness.as_mut() {
            Some(h) => h
                .snapshot()
                .map(|snapshot| snapshot.has_open_subagent_lanes())
                .unwrap_or(false),
            None => session_file.is_some(),
        };
        let interrupted_subagent_recovery = if has_interrupted_subagents {
            InterruptedSubagentRecoveryState::Pending
        } else {
            InterruptedSubagentRecoveryState::Complete
        };
        let plan_store = SessionPlanStore::new(initial_plan, session_file.clone());
        let mut agent = if let Some(h) = harness.as_ref() {
            let runtime_harness = threadlane_runtime::harness::AgentHarness::with_events_and_hooks(
                h.store.store().clone(),
                h.events.clone(),
                h.hooks.clone(),
            );
            AgentRuntime::from_harness_with_provider(
                &options.api_key,
                options.account_id.clone(),
                &effective_model,
                runtime_harness,
                agent_config.clone(),
                provider.clone(),
            )
        } else {
            AgentRuntime::new_with_provider(
                &options.api_key,
                options.account_id.clone(),
                &effective_model,
                options.session_file.as_deref(),
                agent_config.clone(),
                provider,
            )
            .unwrap_or_else(|error| {
                panic!("Failed to create agent runtime: {error}");
            })
        };
        agent.session_id = session_id.clone();
        let harness_run_id: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));
        let cancellation =
            CodingAgentCancellation::new(session_file.clone(), agent.event_tx.clone());

        agent.set_prompt_cache_key(Some(session_id.clone()));

        let wasi_extensions =
            WasiExtensionManager::for_project_session(&options.work_dir, session_id.clone());
        let global_threadlane_dir = default_global_threadlane_dir();
        let loaded_ext_count = wasi_extensions
            .reload_from_roots(global_threadlane_dir.as_deref(), Some(&options.work_dir))
            .unwrap_or_default();
        let agent_catalog = render_agent_catalog(&options.work_dir);
        let initial_tool_policy = restored_tool_policy(&wasi_extensions);
        let tool_policy = Arc::new(tokio::sync::Mutex::new(initial_tool_policy));
        let wasi_extensions = Arc::new(wasi_extensions);
        let agent_work = AgentWorkScheduler::default();
        if let Some(h) = harness.as_ref() {
            if let Ok(state) = Reducer::reduce(&h.store) {
                if let Some(lane) = state.lane("main") {
                    for queued in &lane.queued {
                        if queued.run_id.is_none() {
                            agent_work.schedule(AgentWork::DurableQueueWake {
                                queue: queued.queue.clone(),
                                entry_id: queued.target.id.clone(),
                            });
                        }
                    }
                }
            }
        }
        #[cfg(test)]
        let subagent_work_observer = Arc::new(std::sync::Mutex::new(None));
        #[cfg(test)]
        let runner_observer: Option<SubagentObserverState> = Some(subagent_work_observer.clone());
        let runner_api_key = agent.api_key.clone();
        let runner_account_id = agent.account_id.clone();
        let runner_state = agent.turn.clone();
        let runner_config = agent_config.clone();
        let runner_work_dir = options.work_dir.clone();
        let runner_extensions = wasi_extensions.clone();
        let runner_event_tx = agent.event_tx.clone();
        let runner_session_file = session_file.clone();
        let runner_semaphore = Arc::new(tokio::sync::Semaphore::new(
            coding_config.subagent_concurrency_limit,
        ));
        let dispatch_parent_leaf = Arc::new(std::sync::Mutex::new(None));
        let completed_subagent_lanes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner_parent_leaf = dispatch_parent_leaf.clone();
        let runner_completed_lanes = completed_subagent_lanes.clone();
        let parent_session_id = session_id.clone();
        let agent_runner: AgentRunner = Arc::new(move |tasks, parallel, tool_call_id| {
            #[cfg(test)]
            let observer = runner_observer.clone();
            let api_key = runner_api_key.clone();
            let account_id = runner_account_id.clone();
            let state = runner_state.clone();
            let runner_config = runner_config.clone();
            let work_dir = runner_work_dir.clone();
            let extensions = runner_extensions.clone();
            let event_tx = runner_event_tx.clone();
            let session_file = runner_session_file.clone();
            let semaphore = runner_semaphore.clone();
            let parent_leaf_id = runner_parent_leaf.lock().ok().and_then(|leaf| leaf.clone());
            let completed_lanes = runner_completed_lanes.clone();
            let parent_session_id = parent_session_id.clone();
            Box::pin(async move {
                let (model, parent_reasoning_effort) = {
                    let state = state.lock().await;
                    (state.model.clone(), state.reasoning_effort())
                };
                let child_model = runner_config
                    .subagent_model
                    .clone()
                    .unwrap_or_else(|| model.clone());
                let child_reasoning_effort = runner_config
                    .subagent_reasoning_effort
                    .unwrap_or(parent_reasoning_effort);
                #[cfg(test)]
                let observer = observer
                    .and_then(|observer| observer.lock().ok().and_then(|value| value.clone()));
                let (output, thinking, lanes) = run_subagents_with_context(
                    tasks,
                    parallel,
                    tool_call_id,
                    SubagentRunContext {
                        api_key,
                        account_id,
                        child_model,
                        child_reasoning_effort,
                        parent_session_id: parent_session_id.clone(),
                        work_dir,
                        extensions,
                        parent_event_tx: event_tx,
                        parent_leaf_id,
                        session_file,
                        #[cfg(test)]
                        scheduler_observer: observer,
                        #[cfg(test)]
                        child_work_observer: None,
                        #[cfg(test)]
                        child_tool_observer: None,
                        semaphore,
                    },
                )
                .await?;
                accept_completed_subagent_lanes(&completed_lanes, lanes)?;
                Ok(serde_json::json!({
                    "message": output,
                    "output": output,
                    "thinking": thinking
                }))
            })
        });
        let (broker_dispatcher, managed_processes, permission_handle) = build_broker_dispatcher(
            tool_policy.clone(),
            wasi_extensions.clone(),
            true,
            options.work_dir.clone(),
            agent.event_tx.clone(),
            agent_work.clone(),
            Some(agent_runner.clone()),
            options.session_file.clone(),
        );
        let mcp_manager = Arc::new(McpManager::new(
            default_global_threadlane_dir(),
            Some(options.work_dir.clone()),
        ));
        let mut registry = threadlane_runtime::CapabilityRegistry::new();
        registry.register(Box::new(SkillCapability {
            skills: skills.clone(),
        }));
        registry.register(Box::new(SubagentCapability {
            agent_runner: agent_runner.clone(),
        }));
        registry.register(Box::new(PlanCapability {
            plan_store: plan_store.clone(),
            event_tx: agent.event_tx.clone(),
        }));
        if let Some(session_file) = options.session_file.clone() {
            registry.register(Box::new(ContextCapability {
                session_file,
                work_dir: options.work_dir.clone(),
            }));
        }
        registry.register(Box::new(PrewalkCapability));

        registry.register(Box::new(WasiCapability {
            extensions: wasi_extensions.clone(),
            broker_dispatcher: broker_dispatcher.clone(),
            tool_policy: tool_policy.clone(),
        }));
        registry.register(Box::new(McpCapability {
            mcp_manager: mcp_manager.clone(),
        }));
        let (_wired, errors) = registry.wire_all(&mut agent.tool_dispatcher, &agent.hook_registry);
        for error in &errors {
            eprintln!("{error}");
        }

        let manager_clone = mcp_manager.clone();
        threadlane_runtime::get_runtime().spawn(async move {
            manager_clone.discover_and_connect().await;
        });
        agent.work_dir = Some(options.work_dir.clone());

        let mut system_prompt_config = options.system_prompt.clone();
        if initial_tool_policy == ToolPolicy::ReadOnly {
            system_prompt_config.guidelines.push(
                "The current workspace tool policy is read-only; do not request file mutations or host commands."
                    .to_string(),
            );
        }
        let prompt_tools = agent.configured_tool_definitions();
        let base_system_prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &system_prompt_config,
            work_dir: &options.work_dir,
            tools: &prompt_tools,
            project_context: &project_context,
            skill_catalog: Some(&skill_catalog),
            agent_catalog: Some(&agent_catalog),
            loaded_extension_count: loaded_ext_count,
        });

        {
            let mut turn = agent.turn.try_lock().expect("Failed to lock initial state");
            turn.system_prompt = base_system_prompt.clone();
            turn.messages.push(AgentMessage::System {
                content: base_system_prompt.clone(),
            });
            if let Some(h) = harness.as_ref() {
                if let Ok(context) = h.store.model_context("main") {
                    turn.messages.extend(context.messages());
                }
            }
        }

        let acp = crate::acp_runtime::AcpEngine::new(
            default_global_threadlane_dir(),
            options.work_dir.clone(),
        );

        Self {
            agent,
            session_id,
            session_file,
            wasi_extensions,
            tool_policy,
            work_dir: options.work_dir,
            agent_config,
            skills,
            agent_runner,
            broker_dispatcher,
            managed_processes,
            permission_handle,
            agent_work,
            mcp_manager,
            prompt_templates: None,
            dispatch_parent_leaf,
            completed_subagent_lanes,
            harness,
            harness_journal_error,
            harness_run_id,
            prewalk: Arc::new(std::sync::Mutex::new(None)),
            cancellation,
            interrupted_subagent_recovery,
            acp,
            #[cfg(test)]
            subagent_work_observer,
            #[cfg(test)]
            subagent_branch_observer: None,
        }
    }

    /// Runs one turn against an external ACP agent and journals it.
    ///
    /// The agent owns its own conversation, so nothing here replays a message
    /// list; the journal still has to record the exchange or the transcript is
    /// empty when the session is reopened and the session list shows a named
    /// session with no content.
    async fn run_acp_turn(
        &mut self,
        agent_id: &str,
        input: &str,
        images: Vec<ImageAttachment>,
    ) -> Option<Result<String, String>> {
        let msg = AgentMessage::user(input, images.clone());
        let harness_run_id = match self.begin_harness_run(msg).await {
            Ok(run_id) => run_id,
            Err(error) => {
                let message = format!("Harness Error: {error}");
                let _ = self.agent.event_tx.send(AgentEvent::AgentError {
                    error: message.clone(),
                });
                return Some(Err(message));
            }
        };

        let event_tx = self.agent.event_tx.clone();
        let permissions = self.permission_handle.clone();
        let outcome = self
            .acp
            .run_turn_detailed(
                agent_id,
                input,
                &images,
                self.agent.reasoning_effort(),
                &event_tx,
                &permissions,
            )
            .await;

        let run_id = harness_run_id.as_ref().map(|run| run.run_id.as_str());
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                // `run_turn` already reported the failure as an event; closing
                // the run keeps the journal from holding an open operation.
                let _ = self
                    .finish_harness_run(run_id, OperationOutcome::Failed, Some(error.clone()))
                    .await;
                return Some(Err(error));
            }
        };

        if let (Some(run_id), Some(journal)) = (run_id, self.harness.as_mut()) {
            let tool_calls = (!outcome.tools.is_empty()).then(|| {
                outcome
                    .tools
                    .iter()
                    .map(|tool| threadlane_provider::openai::ToolCall {
                        id: tool.tool_call_id.clone(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: tool.name.clone(),
                            arguments: tool.arguments.clone(),
                        },
                        thought_signature: None,
                    })
                    .collect()
            });
            let recorded = journal.append_message_to_lane(
                "main",
                run_id,
                AgentMessage::Assistant {
                    content: Some(outcome.reply),
                    tool_calls,
                    stop_reason: None,
                    deferred_handle: None,
                },
            );
            if let Err(error) = recorded {
                let _ = self
                    .finish_harness_run(Some(run_id), OperationOutcome::Failed, Some(error.clone()))
                    .await;
                return Some(Err(format!("Harness Error: {error}")));
            }

            // ACP tools execute inside the external agent, but their ordered
            // lifecycle still belongs in the canonical trajectory.
            for tool in outcome.tools {
                let arguments = serde_json::from_str(&tool.arguments)
                    .unwrap_or_else(|_| serde_json::Value::String(tool.arguments.clone()));
                let recorded = journal
                    .tool_started_on_lane("main", run_id, &tool.tool_call_id, &tool.name, arguments)
                    .and_then(|_| {
                        let mut result = tool.result.unwrap_or_else(|| {
                            threadlane_runtime::types::AgentToolResult::external(
                                tool.tool_call_id,
                                tool.name.clone(),
                                "ACP tool call ended without a terminal update",
                                true,
                            )
                        });
                        // ACP terminal updates are patches and commonly omit the
                        // start event's title. The durable harness requires the
                        // result name to match its intent exactly.
                        result.name = tool.name;
                        journal.finish_tool_result(run_id, &result)
                    });
                if let Err(error) = recorded {
                    let _ = self
                        .finish_harness_run(
                            Some(run_id),
                            OperationOutcome::Failed,
                            Some(error.clone()),
                        )
                        .await;
                    return Some(Err(format!("Harness Error: {error}")));
                }
            }

            // ACP reports no token accounting, so the attempt records zero
            // usage rather than a number the agent never sent.
            let recorded = journal.record_assistant_attempt(run_id, TokenUsage::default());
            if let Err(error) = recorded {
                let _ = self
                    .finish_harness_run(Some(run_id), OperationOutcome::Failed, Some(error.clone()))
                    .await;
                return Some(Err(format!("Harness Error: {error}")));
            }
        }

        if let Err(error) = self
            .finish_harness_run(run_id, OperationOutcome::Completed, None)
            .await
        {
            return Some(Err(format!("Harness Error: {error}")));
        }
        // The reply already streamed as events; returning it would render the
        // whole turn a second time.
        None
    }

    pub async fn handle_input_with_images(
        &mut self,
        input: &str,
        images: Vec<ImageAttachment>,
    ) -> Option<Result<String, String>> {
        self.cancellation.clear_cancellation_guard();
        if let Err(error) = self.recover_interrupted_subagent_lanes().await {
            return Some(Err(error));
        }
        if let Some(error) = self.harness_journal_error.as_ref() {
            let error = format!("Harness Error: {error}");
            let _ = self.agent.event_tx.send(AgentEvent::AgentError {
                error: error.clone(),
            });
            return Some(Err(error));
        }
        let adopted_harness_run = self
            .harness_run_id
            .lock()
            .ok()
            .is_some_and(|run_id| run_id.is_some());
        if !adopted_harness_run {
            if let Some(journal) = self.harness.as_mut() {
                match journal.recover_abort() {
                    Ok(_) => {}
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                }
            }
        }
        *self.dispatch_parent_leaf.lock().unwrap() = None;
        let trimmed = input.trim();

        if self.prompt_templates.is_none() {
            let global_dir = std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".threadlane"))
                .unwrap_or_else(|| self.work_dir.join(".threadlane"));
            self.prompt_templates = Some(crate::prompt_templates::load_prompt_templates(
                &self.work_dir,
                &global_dir,
            ));
        }
        let templates = self.prompt_templates.as_ref().unwrap();
        let expanded_input = crate::prompt_templates::expand_prompt_template(trimmed, templates);
        let mut effective_input = expanded_input.trim().to_string();
        let mut architect_directive: Option<String> = None;

        if let Some(command_input) = effective_input.strip_prefix('/') {
            let mut parts = command_input.split_whitespace();
            let cmd_name = parts.next().unwrap_or("");
            let cmd_args = parts.collect::<Vec<&str>>().join(" ");

            if cmd_name.starts_with("skill:") || cmd_name == "skill" {
                let skill_name = if let Some(skill_name) = cmd_name.strip_prefix("skill:") {
                    skill_name
                } else {
                    cmd_args.trim()
                };

                match self.skills.get_skill_instructions(skill_name) {
                    Ok(instructions) => {
                        let prompt = format!(
                            "Use the following Skill instructions for '{}':\n\n{}",
                            skill_name, instructions
                        );
                        let visible_prompt = AgentMessage::user(input, images.clone());
                        let harness_run_id = match self.begin_harness_run(visible_prompt).await {
                            Ok(run_id) => run_id,
                            Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                        };
                        let parent_leaf = self.prompt_parent_leaf(
                            AgentMessage::user(input, images.clone()),
                            harness_run_id.is_some(),
                        );
                        *self.dispatch_parent_leaf.lock().unwrap() = parent_leaf;
                        if let Some(accepted) = harness_run_id.as_ref() {
                            if let Err(error) = self.execute_accepted_run(accepted).await {
                                self.harness_journal_error = Some(error);
                            }
                        } else {
                            self.agent.steer(AgentMessage::user(prompt, images.clone()));
                            self.agent.run_steer().await;
                        }
                        self.sync_harness_and_dispatch_assistant_hooks().await;
                        self.run_scheduled_agent_work().await;
                        if let Err(error) = self.commit_completed_subagent_lanes() {
                            *self.dispatch_parent_leaf.lock().unwrap() = None;
                            let _ = self
                                .finish_harness_run(
                                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(error));
                        }
                        *self.dispatch_parent_leaf.lock().unwrap() = None;
                        if let Err(error) = self
                            .finish_harness_run(
                                harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                OperationOutcome::Completed,
                                None,
                            )
                            .await
                        {
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                        return Some(Ok(format!("Loaded skill '{}'", skill_name)));
                    }
                    Err(err) => return Some(Err(format!("Skill Error: {}", err))),
                }
            }

            if cmd_name == "subagent" {
                let task_prompt = cmd_args.trim();
                if task_prompt.is_empty() {
                    let err = "Usage: /subagent <task description>".to_string();
                    let run_id = self.harness_run_id.lock().ok().and_then(|r| r.clone());
                    let _ = self
                        .finish_harness_run(
                            run_id.as_deref(),
                            OperationOutcome::Failed,
                            Some(err.clone()),
                        )
                        .await;
                    return Some(Err(err));
                }
                let task = AgentRunTask {
                    agent: "worker".to_string(),
                    task: task_prompt.to_string(),
                    instructions: None,
                    tools: None,
                    model: None,
                };
                let visible_prompt = AgentMessage::user(input, images.clone());
                let harness_run_id = match self.begin_harness_run(visible_prompt).await {
                    Ok(run_id) => run_id,
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                };
                if let Some(run_id) = harness_run_id.as_ref().map(|run| run.run_id.as_str()) {
                    if let Some(journal) = self.harness.as_mut() {
                        if let Err(error) = journal.prepare_assistant_attempt(run_id) {
                            let _ = self
                                .finish_harness_run(
                                    Some(run_id),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                    }
                }
                let parent_leaf = self.prompt_parent_leaf(
                    AgentMessage::user(input, images.clone()),
                    harness_run_id.is_some(),
                );
                *self.dispatch_parent_leaf.lock().unwrap() = parent_leaf;
                let result = match (self.agent_runner)(vec![task], false, None).await {
                    Ok(result) => result,
                    Err(err) => {
                        *self.dispatch_parent_leaf.lock().unwrap() = None;
                        let _ = self
                            .finish_harness_run(
                                harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                OperationOutcome::Failed,
                                Some(err.clone()),
                            )
                            .await;
                        return Some(Err(format!("Subagent Error: {err}")));
                    }
                };
                let output = result["output"].as_str().unwrap_or_default().to_string();
                if let Err(error) = self.commit_completed_subagent_lanes() {
                    *self.dispatch_parent_leaf.lock().unwrap() = None;
                    let _ = self
                        .finish_harness_run(
                            harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                            OperationOutcome::Failed,
                            Some(error.clone()),
                        )
                        .await;
                    return Some(Err(error));
                }
                *self.dispatch_parent_leaf.lock().unwrap() = None;
                let assistant = AgentMessage::Assistant {
                    content: Some(output.clone()),
                    tool_calls: None,
                    stop_reason: Some("subagent".into()),
                    deferred_handle: None,
                };
                if let Some(run_id) = harness_run_id.as_ref().map(|run| run.run_id.as_str()) {
                    if let Some(journal) = self.harness.as_mut() {
                        if let Err(error) =
                            journal.append_message(assistant.clone()).and_then(|_| {
                                journal.record_assistant_attempt(run_id, TokenUsage::default())
                            })
                        {
                            let _ = self
                                .finish_harness_run(
                                    Some(run_id),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                    }
                }
                self.run_scheduled_agent_work().await;
                if let Err(error) = self
                    .finish_harness_run(
                        harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                        OperationOutcome::Completed,
                        None,
                    )
                    .await
                {
                    return Some(Err(format!("Harness Error: {error}")));
                }
                return Some(Ok(output));
            }

            if let Some(res) = self
                .wasi_extensions
                .execute_command_with_effects(cmd_name, &cmd_args)
            {
                let visible_prompt = AgentMessage::user(input, images.clone());
                let harness_run_id = match self.begin_harness_run(visible_prompt).await {
                    Ok(run_id) => run_id,
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                };
                let parent_leaf = self.prompt_parent_leaf(
                    AgentMessage::user(input, images.clone()),
                    harness_run_id.is_some(),
                );
                *self.dispatch_parent_leaf.lock().unwrap() = parent_leaf;
                return match res {
                    Ok(result) => {
                        let message = if result.message.is_empty() {
                            None
                        } else {
                            Some(result.message)
                        };
                        let dispatch = match self
                            .broker_dispatcher
                            .dispatch_envelopes(result.host_broker_requests)
                            .await
                        {
                            Ok(dispatch) => dispatch,
                            Err(error) => {
                                let _ = self
                                    .finish_harness_run(
                                        harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                        OperationOutcome::Failed,
                                        Some(error.message.clone()),
                                    )
                                    .await;
                                return Some(Err(format!("WASI Broker Error: {}", error.message)));
                            }
                        };
                        let agent_run_output =
                            dispatch.operation_results.iter().find_map(|result| {
                                if result.request.capability != "agent"
                                    || result.request.operation != "run"
                                {
                                    return None;
                                }
                                if let Some(error) = &result.error {
                                    return Some(Err(format!(
                                        "WASI Broker Error: {}",
                                        error.message
                                    )));
                                }
                                let output = result.value["output"].as_str().ok_or_else(|| {
                                    "agent.run returned no formatted output".to_string()
                                });
                                let thinking = serde_json::from_value::<Vec<AgentMessage>>(
                                    result.value["thinking"].clone(),
                                )
                                .map_err(|error| {
                                    format!("agent.run returned invalid thinking: {error}")
                                });
                                match (output, thinking) {
                                    (Ok(output), Ok(thinking)) => {
                                        for message in thinking {
                                            if let Err(error) = self.append_command_message(message)
                                            {
                                                return Some(Err(error));
                                            }
                                        }
                                        if let Err(error) =
                                            self.append_command_message(AgentMessage::Assistant {
                                                content: Some(output.to_string()),
                                                tool_calls: None,
                                                stop_reason: None,
                                                deferred_handle: None,
                                            })
                                        {
                                            return Some(Err(error));
                                        }
                                        Some(Ok(output.to_string()))
                                    }
                                    (Err(error), _) | (_, Err(error)) => Some(Err(error)),
                                }
                            });
                        self.wasi_extensions
                            .enqueue_broker_results(dispatch.operation_results);
                        self.run_scheduled_agent_work().await;
                        if result.api_version == 1 {
                            for effect in result.effects {
                                match effect {
                                    WasiLegacyEffect::SetToolPolicy { policy } => {
                                        let mut pol = self.tool_policy.lock().await;
                                        match policy.as_str() {
                                            "read_only" => *pol = ToolPolicy::ReadOnly,
                                            "full" => *pol = ToolPolicy::FullAccess,
                                            _ => continue,
                                        }
                                    }
                                    WasiLegacyEffect::RequestModelTurn { prompt } => {
                                        self.agent
                                            .follow_up(AgentMessage::user(prompt, Vec::new()));
                                        self.agent.run_follow_up().await;
                                        self.sync_harness_and_dispatch_assistant_hooks().await;
                                    }
                                }
                            }
                        }
                        if let Err(error) = self.commit_completed_subagent_lanes() {
                            *self.dispatch_parent_leaf.lock().unwrap() = None;
                            let _ = self
                                .finish_harness_run(
                                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(error));
                        }
                        *self.dispatch_parent_leaf.lock().unwrap() = None;
                        if let Some(agent_run_output) = agent_run_output {
                            let result = agent_run_output;
                            let outcome = if result.is_ok() {
                                OperationOutcome::Completed
                            } else {
                                OperationOutcome::Failed
                            };
                            if let Err(error) = self
                                .finish_harness_run(
                                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                    outcome,
                                    result.as_ref().err().cloned(),
                                )
                                .await
                            {
                                return Some(Err(format!("Harness Error: {error}")));
                            }
                            return Some(result);
                        }
                        let result = message.map(Ok);
                        let outcome = if result.is_some() {
                            OperationOutcome::Completed
                        } else {
                            OperationOutcome::Failed
                        };
                        if let Err(error) = self
                            .finish_harness_run(
                                harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                outcome,
                                None,
                            )
                            .await
                        {
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                        result
                    }
                    Err(err) => {
                        let message = format!("WASI Extension Error: {err}");
                        let _ = self
                            .finish_harness_run(
                                harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                OperationOutcome::Failed,
                                Some(message.clone()),
                            )
                            .await;
                        Some(Err(message))
                    }
                };
            }

            if let Some(cmd_action) = parse_slash_command(&effective_input) {
                if cmd_action == CommandAction::Quit {
                    return Some(Ok("quitting".to_string()));
                }
                if cmd_action == CommandAction::Compact {
                    return Some(match self.compact_history_with_harness().await {
                        Ok(true) => Ok("Context compacted in the current session.".into()),
                        Ok(false) => Ok("Nothing to compact yet.".into()),
                        Err(error) => Err(format!("Harness Error: {error}")),
                    });
                }
                if let CommandAction::SwitchModel(model) = &cmd_action {
                    if !model.is_empty() {
                        return Some(
                            self.set_model(model.clone())
                                .await
                                .map(|_| format!("Switched model to: {model}")),
                        );
                    }
                }
                if let CommandAction::SetName(name) = &cmd_action {
                    return Some(
                        self.set_name(name.clone())
                            .map(|_| format!("Session name set to: {name}")),
                    );
                }
                if let CommandAction::Prewalk(objective) = &cmd_action {
                    let task_prompt = objective.trim();
                    if task_prompt.is_empty() {
                        return Some(Ok("Usage: /prewalk <task objective> - explore with frontier model, land first edit, then transition to fast model.".into()));
                    }
                    let active_model = self.agent.turn.lock().await.model.clone();
                    let fast_model = self
                        .agent
                        .model_roles()
                        .resolve_fast(&active_model)
                        .to_string();
                    let fast_reasoning = self.agent.config().fast_reasoning_effort;
                    *self.prewalk.lock().unwrap() = Some(crate::orchestrator::PrewalkState {
                        target_model: fast_model.clone(),
                        target_reasoning: fast_reasoning,
                        started_at: std::time::Instant::now(),
                    });

                    let _ = self.agent.event_tx.send(AgentEvent::PrewalkCompleted {
                        model: active_model.clone(),
                        message: format!("Prewalk started with `{active_model}`. Target fast model: `{fast_model}`."),
                    });

                    effective_input = task_prompt.to_string();
                    architect_directive =
                        Some(crate::orchestrator::build_architect_directive(&fast_model));
                } else {
                    let output = execute_slash_command(cmd_action, &mut self.agent).await;
                    return Some(Ok(output));
                }
            }
        }

        // --- OMP-style Entrypoint Orchestrator: Intent Classification & Automatic Prewalk ---
        if architect_directive.is_none() && self.prewalk.lock().unwrap().is_none() {
            let active_model = self.agent.turn.lock().await.model.clone();
            let fast_model = self
                .agent
                .model_roles()
                .resolve_fast(&active_model)
                .to_string();
            let fast_reasoning = self.agent.config().fast_reasoning_effort;
            let orchestrator_mode = self.agent.config().orchestrator_mode;
            let provider_client = Some(self.agent.provider_client_arc());

            let decision = crate::orchestrator::Orchestrator::evaluate(
                &effective_input,
                orchestrator_mode,
                &active_model,
                &fast_model,
                fast_reasoning,
                provider_client,
            )
            .await;

            if let crate::orchestrator::OrchestratorDecision::EngagePrewalk {
                fast_model: target_fast,
                fast_reasoning: target_effort,
                architect_system_directive,
            } = decision
            {
                *self.prewalk.lock().unwrap() = Some(crate::orchestrator::PrewalkState {
                    target_model: target_fast.clone(),
                    target_reasoning: target_effort,
                    started_at: std::time::Instant::now(),
                });

                let _ = self.agent.event_tx.send(AgentEvent::PrewalkCompleted {
                    model: active_model.clone(),
                    message: format!(
                        "Auto-Prewalk engaged: Frontier architect (`{active_model}`) exploring and landing first edit before handoff to `{target_fast}`."
                    ),
                });

                architect_directive = Some(architect_system_directive);
            }
        }

        if let Some(directive) = architect_directive {
            let mut turn = self.agent.turn.lock().await;
            if !turn
                .system_prompt
                .contains(crate::orchestrator::ARCHITECT_PROTOCOL_HEADER)
            {
                turn.system_prompt.push_str(&directive);
            }
        }

        // An ACP agent runs its own loop behind the protocol: it does not use
        // Threadlane's provider, tools, or message replay, so it is dispatched
        // here rather than through the provider run below.
        if let Some(agent_id) = crate::acp_bridge::acp_agent_id(&self.agent.model()) {
            let agent_id = agent_id.to_string();
            return self.run_acp_turn(&agent_id, &effective_input, images).await;
        }

        let msg = AgentMessage::user(effective_input, images);
        let harness_run_id = match self.begin_harness_run(msg.clone()).await {
            Ok(run_id) => run_id,
            Err(error) => {
                let message = format!("Harness Error: {error}");
                let _ = self.agent.event_tx.send(AgentEvent::AgentError {
                    error: message.clone(),
                });
                return Some(Err(message));
            }
        };
        let parent_leaf = self.prompt_parent_leaf(msg.clone(), harness_run_id.is_some());
        *self.dispatch_parent_leaf.lock().unwrap() = parent_leaf;
        if let (Some(run_id), Some(harness)) = (
            harness_run_id.as_ref().map(|run| run.run_id.as_str()),
            self.harness.as_mut(),
        ) {
            if let Err(error) = harness.prepare_assistant_attempt(run_id) {
                let _ = self
                    .finish_harness_run(Some(run_id), OperationOutcome::Failed, Some(error.clone()))
                    .await;
                return Some(Err(format!("Harness Error: {error}")));
            }
        }
        let mut harness_events = self.subscribe();
        if let Some(accepted) = harness_run_id.as_ref() {
            if let Err(error) = self.execute_accepted_run(accepted).await {
                self.harness_journal_error = Some(error);
            }
        } else {
            self.agent.steer(msg);
            self.agent.run_steer().await;
            self.sync_harness_and_dispatch_assistant_hooks().await;
        }
        if let Some(error) = self.harness_journal_error.clone() {
            *self.dispatch_parent_leaf.lock().unwrap() = None;
            let _ = self
                .finish_harness_run(
                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                    OperationOutcome::Failed,
                    Some(error.clone()),
                )
                .await;
            return Some(Err(format!("Harness Error: {error}")));
        }
        self.run_scheduled_agent_work().await;
        if let Some(error) = self.harness_journal_error.clone() {
            *self.dispatch_parent_leaf.lock().unwrap() = None;
            let _ = self
                .finish_harness_run(
                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                    OperationOutcome::Failed,
                    Some(error.clone()),
                )
                .await;
            return Some(Err(format!("Harness Error: {error}")));
        }
        if let Err(error) = self.commit_completed_subagent_lanes() {
            *self.dispatch_parent_leaf.lock().unwrap() = None;
            let _ = self
                .finish_harness_run(
                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                    OperationOutcome::Failed,
                    Some(error.clone()),
                )
                .await;
            let _ = self.agent.event_tx.send(AgentEvent::AgentError {
                error: error.clone(),
            });
            return Some(Err(error));
        }
        *self.dispatch_parent_leaf.lock().unwrap() = None;
        let mut tool_termination = HashMap::new();
        let (usage, failure) = loop {
            match harness_events.try_recv() {
                Ok(AgentEvent::ToolExecutionEnd {
                    tool_call_id,
                    result,
                    ..
                }) => {
                    tool_termination.insert(tool_call_id, result.terminates());
                }
                Ok(AgentEvent::AgentEnd { usage }) => break (usage, None),
                Ok(AgentEvent::AgentError { error }) => break (TokenUsage::default(), Some(error)),
                Ok(_) => continue,
                Err(error) => {
                    if let Some(message) = generation_event_drain_error(error) {
                        break (TokenUsage::default(), Some(message.into()));
                    }
                }
            }
        };
        if let Some(error) = failure {
            if let Some(run_id) = harness_run_id.as_ref().map(|run| run.run_id.as_str()) {
                let completion = self.harness.as_mut().map(|journal| {
                    journal.record_completed_tools_with_termination(run_id, &tool_termination)
                });
                if let Some(Err(completion_error)) = completion {
                    let _ = self
                        .finish_harness_run(
                            Some(run_id),
                            OperationOutcome::Failed,
                            Some(completion_error.clone()),
                        )
                        .await;
                    return Some(Err(format!("Harness Error: {completion_error}")));
                }
                if is_retryable_generation_error(&error) {
                    let scheduled = self
                        .harness
                        .as_mut()
                        .map(|journal| journal.schedule_retry(run_id, &error));
                    if matches!(scheduled, Some(Ok(_))) {
                        return Some(Err(error));
                    }
                }
                let _ = self
                    .finish_harness_run(Some(run_id), OperationOutcome::Failed, Some(error.clone()))
                    .await;
            }
            return Some(Err(error));
        }
        if let Some(run_id) = harness_run_id.as_ref().map(|run| run.run_id.as_str()) {
            let attempt_result = self.harness.as_mut().map(|journal| {
                journal
                    .record_completed_tools_with_termination(run_id, &tool_termination)
                    .and_then(|_| journal.record_assistant_attempt(run_id, usage))
            });
            if let Some(Err(error)) = attempt_result {
                let _ = self
                    .finish_harness_run(Some(run_id), OperationOutcome::Failed, Some(error.clone()))
                    .await;
                return Some(Err(format!("Harness Error: {error}")));
            }
        }
        if let Err(error) = self
            .finish_harness_run(
                harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                OperationOutcome::Completed,
                None,
            )
            .await
        {
            return Some(Err(format!("Harness Error: {error}")));
        }

        None
    }
}

#[cfg(test)]
mod compaction_sync_tests {
    use super::{
        durable_prompt_snapshot, requires_harness_compaction_reset, CodingAgent,
        CodingAgentOptions, CompletedSubagentLane, SubagentLaneStatus,
        MAX_PERSISTED_SYSTEM_PROMPT_BYTES,
    };
    use crate::system_prompt::SystemPromptConfig;
    use async_trait::async_trait;
    use std::{
        collections::HashSet,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };
    use threadlane_protocol::{
        DeferredResponse, ProviderPort, RuntimeRequest, RuntimeStreamEvent, RuntimeToolCall,
        RuntimeToolCallFunction, RuntimeUsage,
    };
    use threadlane_runtime::{
        harness::{
            read_transcript_page, CompactionReason, JsonlStore, SessionStore, TranscriptItem,
        },
        AgentConfig, AgentMessage, Record,
    };

    fn summary() -> AgentMessage {
        AgentMessage::Custom {
            custom_type: "compaction_summary".into(),
            payload: serde_json::json!({"summary": "older context"}),
        }
    }

    #[test]
    fn oversized_system_prompt_is_redacted_with_a_digest() {
        let content = "x".repeat(MAX_PERSISTED_SYSTEM_PROMPT_BYTES + 1);
        assert!(matches!(
            durable_prompt_snapshot(&content),
            threadlane_runtime::harness::PromptSnapshot::Redacted {
                sha256,
                byte_len,
                ..
            } if sha256.as_str().len() == 64 && byte_len == content.len()
        ));
    }

    #[test]
    fn in_loop_compaction_requires_a_durable_branch_reset() {
        let durable = vec![
            AgentMessage::user("old prompt", vec![]),
            AgentMessage::Assistant {
                content: Some("old response".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
        ];
        let state = vec![summary(), AgentMessage::user("current prompt", vec![])];

        assert!(requires_harness_compaction_reset(&durable, &state));
    }

    #[test]
    fn already_persisted_compaction_uses_normal_incremental_sync() {
        let durable = vec![summary(), AgentMessage::user("current prompt", vec![])];
        let mut state = durable.clone();
        state.push(AgentMessage::Assistant {
            content: Some("new response".into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        });

        assert!(!requires_harness_compaction_reset(&durable, &state));
    }

    #[tokio::test]
    async fn invalid_compatibility_source_does_not_break_delayed_passive_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut agent = CodingAgent::new(CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "test-model".into(),
            work_dir: dir.path().to_path_buf(),
            session_file: Some(path.clone()),
            system_prompt: SystemPromptConfig::default(),
            agent_config: None,
            coding_config: None,
        });
        agent
            .begin_harness_run(AgentMessage::user("prompt", vec![]))
            .await
            .unwrap();

        let identity = agent
            .harness
            .as_mut()
            .unwrap()
            .start_subagent_lane("worker", "inspect", Some("node_69"))
            .unwrap();
        assert!(identity.identity.source_leaf_id.is_none());
        agent
            .completed_subagent_lanes
            .lock()
            .unwrap()
            .push(CompletedSubagentLane {
                lane_name: identity.identity.lane_name,
                run_id: identity.identity.run_id,
                task: "inspect".into(),
                agent: "worker".into(),
                model: "test-model".into(),
                status: SubagentLaneStatus::Completed,
                messages: vec![AgentMessage::Assistant {
                    content: Some("done".into()),
                    tool_calls: None,
                    stop_reason: Some("end_turn".into()),
                    deferred_handle: None,
                }],
                error: None,
            });

        agent.commit_completed_subagent_lanes().unwrap();

        let store = JsonlStore::open(&path).unwrap();
        assert!(store.entries().iter().any(|entry| matches!(
            &entry.message,
            AgentMessage::Custom { custom_type, .. } if custom_type == "subagent_lane"
        )));
        assert!(store
            .entries()
            .iter()
            .all(|entry| entry.parent_id.as_deref() != Some("node_69")));
    }

    struct LongToolLoopProvider {
        attempts: AtomicUsize,
        max_request_estimate: AtomicUsize,
        previous_serialized_request: Mutex<Option<String>>,
    }

    impl LongToolLoopProvider {
        fn attempts(&self) -> usize {
            self.attempts.load(Ordering::SeqCst)
        }

        fn max_request_estimate(&self) -> usize {
            self.max_request_estimate.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ProviderPort for LongToolLoopProvider {
        async fn stream_request(
            &self,
            request: RuntimeRequest,
            events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
        ) {
            let serialized_request = format!("{}\n{}", request.messages, request.tools);
            let estimate = serialized_request.len().div_ceil(4);
            let cache_read_tokens = {
                let mut previous = self.previous_serialized_request.lock().unwrap();
                let repeated_prefix_bytes = previous
                    .as_ref()
                    .map(|prior| {
                        prior
                            .bytes()
                            .zip(serialized_request.bytes())
                            .take_while(|(left, right)| left == right)
                            .count()
                    })
                    .unwrap_or(0);
                *previous = Some(serialized_request);
                repeated_prefix_bytes / 4
            };
            self.max_request_estimate
                .fetch_max(estimate, Ordering::SeqCst);
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            let tool_calls = if attempt < 102 {
                vec![RuntimeToolCall {
                    id: format!("loop-{attempt}"),
                    r#type: "function".into(),
                    function: RuntimeToolCallFunction {
                        name: threadlane_skills::LOAD_SKILL_TOOL_NAME.into(),
                        arguments: serde_json::json!({ "name": "reported-shape" }).to_string(),
                    },
                    thought_signature: None,
                }]
            } else {
                Vec::new()
            };
            if tool_calls.is_empty() {
                let _ = events
                    .send(RuntimeStreamEvent::ContentToken("complete".into()))
                    .await;
            }
            let estimated_tokens = u32::try_from(estimate).expect("test request fits u32");
            let cache_read_tokens =
                u32::try_from(cache_read_tokens).expect("test cache prefix fits u32");
            let input_tokens = estimated_tokens.saturating_sub(cache_read_tokens);
            let output_tokens = 20;
            let usage = RuntimeUsage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens: 0,
                total_tokens: estimated_tokens.saturating_add(output_tokens),
            };
            let _ = events
                .send(RuntimeStreamEvent::Finished { tool_calls, usage })
                .await;
        }

        async fn fetch_deferred(
            &self,
            _model: &str,
            _handle_id: &str,
        ) -> Result<DeferredResponse, String> {
            Ok(DeferredResponse::Pending)
        }

        async fn cancel_deferred(&self, _model: &str, _handle_id: &str) -> Result<(), String> {
            Ok(())
        }

        fn provider_kind(&self, _model: &str) -> &'static str {
            "test"
        }
    }

    #[tokio::test]
    async fn long_cached_tool_loop_compacts_before_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reported-session-shape.jsonl");
        let skill_dir = dir.path().join(".agents/skills/reported-shape");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let skill_body = "segment ".repeat(1_000);
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!(
                "---\nname: reported-shape\ndescription: deterministic compaction input\n---\n{skill_body}"
            ),
        )
        .unwrap();
        let provider = Arc::new(LongToolLoopProvider {
            attempts: AtomicUsize::new(0),
            max_request_estimate: AtomicUsize::new(0),
            previous_serialized_request: Mutex::new(None),
        });
        let mut agent = CodingAgent::new_with_provider(
            CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "reported-session-shape-model".into(),
                work_dir: dir.path().to_path_buf(),
                session_file: Some(path.clone()),
                system_prompt: SystemPromptConfig::default(),
                agent_config: Some(AgentConfig::default()),
                coding_config: None,
            },
            provider.clone(),
        );

        let result = agent
            .handle_input_with_images("continue the cached tool loop", vec![])
            .await;
        assert!(result.is_none(), "foreground run failed: {result:?}");
        assert_eq!(provider.attempts(), 102);

        // Reopen the durable journal rather than relying on in-memory runtime state.
        drop(agent);
        let store = JsonlStore::open(&path).unwrap();
        let records = store.records();
        let emitted_context_limit = records
            .iter()
            .filter_map(|record| match record {
                Record::ContextManifestCaptured { context_limit, .. } => *context_limit,
                _ => None,
            })
            .next_back()
            .unwrap();
        assert!(provider.max_request_estimate() < emitted_context_limit);

        let cumulative_processed = records
            .iter()
            .filter_map(|record| match record {
                Record::Usage { usage, .. } => Some(
                    u64::from(usage.input_tokens)
                        .saturating_add(u64::from(usage.cache_read_tokens))
                        .saturating_add(u64::from(usage.output_tokens)),
                ),
                _ => None,
            })
            .sum::<u64>();
        assert!(
            cumulative_processed > emitted_context_limit as u64,
            "processed={cumulative_processed}, limit={emitted_context_limit}"
        );

        let (compaction_seq, generation) = records
            .iter()
            .find_map(|record| match record {
                Record::ContextCompacted {
                    seq,
                    generation,
                    reason: CompactionReason::AdaptiveBudget,
                    ..
                } => Some((*seq, *generation)),
                _ => None,
            })
            .expect("adaptive compaction telemetry");
        let (manifest_seq, manifest_generation, manifest_tokens) = records
            .iter()
            .filter_map(|record| match record {
                Record::ContextManifestCaptured {
                    seq,
                    compaction_generation,
                    total_estimated_tokens,
                    ..
                } if *seq > compaction_seq => {
                    Some((*seq, *compaction_generation, *total_estimated_tokens))
                }
                _ => None,
            })
            .next()
            .expect("post-compaction context manifest");
        let next_provider_start_seq = records
            .iter()
            .filter_map(|record| match record {
                Record::ProviderRequestStarted { seq, .. } if *seq > compaction_seq => Some(*seq),
                _ => None,
            })
            .next()
            .expect("post-compaction provider request");
        assert_eq!(manifest_generation, generation);
        assert!(manifest_tokens.unwrap() < emitted_context_limit as u32);

        // The checkpoint summary, compaction telemetry, provider start, and
        // request manifest are all recovered from the durable journal in order.
        let checkpoint_seq = store
            .entries()
            .iter()
            .filter_map(|entry| match &entry.message {
                AgentMessage::Custom { custom_type, .. }
                    if custom_type == "compaction_summary" && entry.seq < compaction_seq =>
                {
                    Some(entry.seq)
                }
                _ => None,
            })
            .next_back()
            .expect("durable checkpoint preceding adaptive compaction");
        assert!(
            checkpoint_seq < compaction_seq
                && compaction_seq < next_provider_start_seq
                && next_provider_start_seq < manifest_seq,
            "checkpoint={checkpoint_seq}, compaction={compaction_seq}, provider_start={next_provider_start_seq}, manifest={manifest_seq}"
        );

        // The reopened branch selects the latest durable checkpoint and a descendant leaf.
        let model_context = store.model_context("main").unwrap();
        let checkpoint = model_context.checkpoint.expect("durable checkpoint");
        assert!(model_context
            .leaf_id
            .as_deref()
            .is_some_and(|leaf| leaf != checkpoint.entry_id));
        assert!(model_context
            .entries
            .iter()
            .any(|entry| entry.id == checkpoint.entry_id));

        let page = read_transcript_page(&path, None, 1_000).unwrap();
        assert!(!page.has_older);
        assert!(page.items.iter().any(|item| matches!(
            item,
            TranscriptItem::ContextCompacted(marker)
                if marker.reason == CompactionReason::AdaptiveBudget
        )));
        let messages = page
            .items
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::Message(message) => Some(message),
                TranscriptItem::ContextCompacted(_) => None,
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            messages.first(),
            Some(AgentMessage::User { content }) if content == "continue the cached tool loop"
        ));
        assert!(messages.iter().any(|message| matches!(
            message,
            AgentMessage::Assistant { content: Some(content), .. } if content == "complete"
        )));

        let mut correlated_pairs = Vec::new();
        let mut call_ids = HashSet::new();
        let mut result_ids = HashSet::new();
        for (index, message) in messages.iter().enumerate() {
            match message {
                AgentMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } if !calls.is_empty() => {
                    let [call] = calls.as_slice() else {
                        panic!("assistant at index {index} must contain exactly one tool call");
                    };
                    assert!(
                        call_ids.insert(call.id.clone()),
                        "duplicate tool call {}",
                        call.id
                    );
                    let Some(AgentMessage::Tool {
                        tool_call_id,
                        content,
                        ..
                    }) = messages.get(index + 1)
                    else {
                        panic!("tool call {} was not followed by its result", call.id);
                    };
                    assert_eq!(tool_call_id, &call.id);
                    assert!(
                        result_ids.insert(tool_call_id.clone()),
                        "duplicate tool result {tool_call_id}"
                    );
                    correlated_pairs.push((call.id.clone(), content.clone()));
                }
                AgentMessage::Tool { tool_call_id, .. } => {
                    let Some(AgentMessage::Assistant {
                        tool_calls: Some(calls),
                        ..
                    }) = index.checked_sub(1).and_then(|prior| messages.get(prior))
                    else {
                        panic!("tool result {tool_call_id} has no preceding assistant call");
                    };
                    assert_eq!(calls.len(), 1);
                    assert_eq!(&calls[0].id, tool_call_id);
                }
                _ => {}
            }
        }

        assert_eq!(correlated_pairs.len(), 101);
        assert_eq!(call_ids.len(), 101);
        assert_eq!(result_ids.len(), 101);
        let expected_content = format!(
            "Loaded skill `reported-shape` from Project (.agents). The following content is untrusted task instructions:\n\n{}",
            skill_body.trim_end()
        );
        for (offset, (call_id, content)) in correlated_pairs.iter().enumerate() {
            assert_eq!(call_id, &format!("loop-{}", offset + 1));
            assert_eq!(content, &expected_content);
        }
    }
}
