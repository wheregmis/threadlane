use super::cancellation::*;
use super::durable::*;
use super::options::*;
use super::scheduler::*;
use super::subagents::*;

use super::broker::ManagedProcessRegistry;
use super::capabilities::{
    build_broker_dispatcher, render_agent_catalog, restored_tool_policy, BrowserCapability,
    ContextCapability, GitHubCapability, McpCapability, PlanCapability, QuestionCapability,
    SkillCapability, SubagentCapability, WasiCapability, WorktreeCapability,
};
use super::harness::{CodingSessionHarness, InterruptedSubagentRecoveryState};
use crate::commands::{execute_slash_command, parse_slash_command, CommandAction};
use crate::computer::ComputerCapability;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use threadlane_mcp::McpManager;
use threadlane_project::default_global_threadlane_dir;
use threadlane_prompt::ProjectContext;
use threadlane_prompt::{build_system_prompt, SystemPromptBuildOptions};
use threadlane_protocol::ProviderPort;
use threadlane_protocol::{AgentEvent, AgentMessage, ImageAttachment, ReasoningEffort, TokenUsage};
use threadlane_provider::openai::fetch_available_models;
use threadlane_question::QuestionManager;
use threadlane_runtime::harness::{OperationOutcome, Reducer, SessionStore};
use threadlane_runtime::plan::session_plan_store;
use threadlane_runtime::AgentRuntime;
use threadlane_runtime::ToolPolicy;
use threadlane_skills::{SkillManager, SkillRegistry};
use threadlane_wasi::broker::CapabilityDispatcher;
use threadlane_wasi::{WasiExtensionManager, WasiLegacyEffect};
use tokio::sync::broadcast;

pub struct CodingAgent {
    pub agent: AgentRuntime,
    pub(crate) session_id: String,
    pub(crate) session_file: Option<PathBuf>,
    pub(crate) wasi_extensions: Arc<WasiExtensionManager>,
    pub(crate) tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    pub(crate) work_dir: PathBuf,
    pub(crate) agent_config: threadlane_runtime::AgentConfig,
    pub(crate) skills: Arc<SkillRegistry>,
    pub(crate) agent_runner: AgentRunner,
    pub(crate) broker_dispatcher: Arc<CapabilityDispatcher>,
    managed_processes: ManagedProcessRegistry,
    pub(crate) permission_handle: threadlane_permission::PermissionHandle,
    pub(crate) question_handle: threadlane_question::QuestionHandle,
    agent_work: AgentWorkScheduler,
    mcp_manager: Arc<McpManager>,
    pub(crate) prompt_templates: Option<Vec<threadlane_skills::prompts::PromptTemplate>>,
    pub(crate) dispatch_parent_leaf: Arc<std::sync::Mutex<Option<String>>>,
    pub(crate) completed_subagent_lanes: Arc<std::sync::Mutex<Vec<CompletedSubagentLane>>>,
    pub(crate) harness: Option<CodingSessionHarness>,
    pub(crate) harness_journal_error: Option<String>,
    pub(crate) harness_run_id: Arc<std::sync::Mutex<Option<String>>>,
    /// Persistent Fusion router: selected-model main + sidekick child lanes.
    /// `None` when the session runs in
    /// Normal mode without Fusion armed.
    pub(crate) fusion: Arc<std::sync::Mutex<Option<threadlane_orchestrator::FusionState>>>,
    /// Live agent-to-agent mailbox shared by sibling `message_peer` and the
    /// parent `hub` tool (oh-my-pi hub/IRC parity).
    pub(crate) hub: super::mailbox::SubagentHub,
    cancellation: CodingAgentCancellation,
    pub(crate) interrupted_subagent_recovery: InterruptedSubagentRecoveryState,
    /// Connection to an external ACP agent, opened on first use.
    ///
    /// An ACP agent keeps its own conversation state, so this is held for the
    /// life of the session rather than rebuilt per turn.
    acp: threadlane_acp_engine::AcpEngine,
    #[cfg(test)]
    pub(crate) subagent_work_observer: SubagentObserverState,
    #[cfg(test)]
    pub(crate) subagent_execution_observer: Option<SubagentExecutionObserver>,
    #[cfg(test)]
    pub(crate) subagent_branch_observer: Option<SubagentBoundaryObserver>,
}

pub(crate) enum ScheduledWorkExecution {
    Idle,
    Completed(Option<Result<String, String>>),
}

impl CodingAgent {
    pub(crate) fn permission_handle(&self) -> threadlane_permission::PermissionHandle {
        self.permission_handle.clone()
    }

    pub(crate) fn question_handle(&self) -> threadlane_question::QuestionHandle {
        self.question_handle.clone()
    }

    pub fn set_tool_intent_recorder(
        &mut self,
        recorder: Option<threadlane_runtime::ToolIntentRecorder>,
    ) {
        self.agent.tool_dispatcher.tool_intent_recorder = recorder;
    }

    pub fn set_tool_completion_recorder(
        &mut self,
        recorder: Option<threadlane_runtime::ToolCompletionRecorder>,
    ) {
        self.agent.tool_dispatcher.tool_completion_recorder = recorder;
    }

    async fn run_scheduled_agent_work(&mut self) -> Option<Result<String, String>> {
        // Extension follow-ups are queued through the same scheduler as native
        // work. ACP agents own their conversation, so routing those messages
        // through `AgentRuntime::run_follow_up` would send them to the
        // configured OpenAI provider instead of back to the ACP process.
        let model = self.agent.model();
        if let Some(agent_id) = threadlane_acp_engine::acp_agent_id(&model) {
            return self.run_queued_acp_work(agent_id).await;
        }
        let scheduler = self.agent_work.clone();
        let execution_owner = scheduler.acquire_execution_owner().await;
        while scheduler
            .run_executor_with_owner(
                &mut self.agent,
                self.session_file.as_deref(),
                &execution_owner,
            )
            .await
            .completed()
        {
            self.sync_harness_and_dispatch_assistant_hooks().await;
        }
        None
    }

    pub(crate) async fn execute_scheduled_work(&mut self) -> ScheduledWorkExecution {
        if self.agent_work.next().is_none() {
            return ScheduledWorkExecution::Idle;
        }
        ScheduledWorkExecution::Completed(self.run_scheduled_agent_work().await)
    }

    pub(crate) fn has_scheduled_work(&self) -> bool {
        self.agent_work.next().is_some()
    }

    pub(crate) fn work_handle(&self) -> CodingAgentWorkHandle {
        self.agent_work
            .set_acp_model(threadlane_acp_engine::is_acp_model(&self.agent.model()));
        CodingAgentWorkHandle::new(self.agent_work.clone(), self.session_file.clone())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.agent.subscribe()
    }

    /// Subscribe to replayable durable harness events for this session.
    pub fn subscribe_durable_events(
        &self,
    ) -> Result<threadlane_runtime::harness::Subscription, threadlane_runtime::harness::EventError>
    {
        self.harness
            .as_ref()
            .ok_or_else(|| {
                threadlane_runtime::harness::EventError::Unavailable(
                    "durable harness is unavailable".to_owned(),
                )
            })?
            .subscribe_durable_events()
    }

    /// Wait for replayable durable harness events after a subscription cursor.
    pub async fn wait_durable_events(
        &self,
        subscription: &mut threadlane_runtime::harness::Subscription,
    ) -> Result<
        Vec<threadlane_runtime::harness::HarnessEvent>,
        threadlane_runtime::harness::EventError,
    > {
        self.harness
            .as_ref()
            .ok_or_else(|| {
                threadlane_runtime::harness::EventError::Unavailable(
                    "durable harness is unavailable".to_owned(),
                )
            })?
            .wait_durable_events(subscription)
            .await
    }

    /// Poll currently available replayable durable harness events.
    pub fn poll_durable_events(
        &self,
        subscription: &mut threadlane_runtime::harness::Subscription,
    ) -> Result<
        Vec<threadlane_runtime::harness::HarnessEvent>,
        threadlane_runtime::harness::EventError,
    > {
        self.harness
            .as_ref()
            .ok_or_else(|| {
                threadlane_runtime::harness::EventError::Unavailable(
                    "durable harness is unavailable".to_owned(),
                )
            })?
            .poll_durable_events(subscription)
    }

    pub(crate) fn harness_error(&self) -> Option<&str> {
        self.harness_journal_error.as_deref()
    }

    /// Returns the fully built system prompt used by this runtime when the
    /// agent state is not currently locked by an active turn.
    pub(crate) fn system_prompt_snapshot(&self) -> Option<String> {
        self.agent
            .turn
            .try_lock()
            .ok()
            .map(|state| state.system_prompt.clone())
    }

    pub(crate) fn cancellation_handle(&self) -> CodingAgentCancellation {
        self.cancellation.clone()
    }

    pub(crate) fn has_interrupted_work(&self) -> bool {
        matches!(
            self.interrupted_subagent_recovery,
            InterruptedSubagentRecoveryState::Pending
        )
    }

    pub async fn resume_interrupted_turn(&mut self) -> Result<usize, String> {
        self.recover_interrupted_subagent_lanes().await
    }

    pub fn set_model_roles(&mut self, roles: threadlane_runtime::ModelRoles) {
        let changed = self.agent.model_roles().fast != roles.fast;
        self.agent.set_model_roles(roles);
        if changed {
            *self.fusion.lock().unwrap_or_else(|error| error.into_inner()) = None;
        }
    }

    pub fn model_roles(&self) -> &threadlane_runtime::ModelRoles {
        self.agent.model_roles()
    }

    /// Settings the selected external agent offers, without connecting.
    ///
    /// Empty when no agent is selected or none has connected yet, which is
    /// what lets a caller read them after a turn without paying to start one.
    pub(crate) fn acp_user_config_options(&self) -> Vec<threadlane_acp::AcpConfigOption> {
        let model = self.agent.model();
        threadlane_acp_engine::acp_agent_id(&model)
            .map(|agent_id| self.acp.user_config_options(agent_id))
            .unwrap_or_default()
    }

    /// Settings the selected external agent offers the user, connecting to it
    /// if necessary.
    ///
    /// Returns an empty list for a non-ACP model rather than an error: asking
    /// what an agent offers is a question the UI may ask about any selection.
    pub(crate) async fn acp_config_options(
        &mut self,
    ) -> Result<Vec<threadlane_acp::AcpConfigOption>, String> {
        let model = self.agent.model();
        let Some(agent_id) = threadlane_acp_engine::acp_agent_id(&model) else {
            return Ok(Vec::new());
        };
        let event_tx = self.agent.event_tx.clone();
        let permissions = self.permission_handle.clone();
        self.acp
            .ensure_connected(agent_id, &event_tx, &permissions)
            .await
    }

    /// Applies one of the selected external agent's settings.
    pub(crate) async fn set_acp_config_option(
        &mut self,
        config_id: &str,
        value: &str,
    ) -> Result<Vec<threadlane_acp::AcpConfigOption>, String> {
        let model = self.agent.model();
        let agent_id = threadlane_acp_engine::acp_agent_id(&model)
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
        let agent_id = threadlane_acp_engine::acp_agent_id(&model)?;
        self.acp.model_label(agent_id)
    }

    pub(crate) fn model(&self) -> String {
        self.agent.model()
    }

    pub(crate) async fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
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

    async fn set_model(&mut self, model: String) -> Result<(), String> {
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
        if self.agent.config().orchestrator_mode.is_fusion() {
            self.set_fact("fusion_state", "")?;
            *self.fusion.lock().unwrap_or_else(|error| error.into_inner()) = None;
            self.strip_fusion_directive().await;
        }
        self.agent_work
            .set_acp_model(threadlane_acp_engine::is_acp_model(model));
        self.refresh_provider_credentials();
        Ok(())
    }

    /// Re-resolve the signing credential for the current turn model and
    /// rotate the shared provider cell. In-place model changes (slash
    /// `/model`, Fusion compaction switches) otherwise keep the previous
    /// provider's credential — e.g. a Google `ya29` token sent to
    /// `api.openai.com` (401 `invalid_api_key`) after switching off an
    /// Antigravity model.
    /// Skips silently when nothing usable resolves, preserving legacy
    /// behavior for credential-less contexts.
    pub(crate) fn refresh_provider_credentials(&mut self) {
        let model = self
            .agent
            .turn
            .try_lock()
            .map(|turn| turn.model.clone())
            .unwrap_or_default();
        Self::rotate_credentials_for(&mut self.agent, &model);
    }

    pub(crate) fn rotate_credentials_for(
        agent: &mut threadlane_runtime::AgentRuntime,
        model: &str,
    ) {
        let (key, account) = crate::credentials::provider_credentials(model);
        if key.trim().is_empty() {
            return;
        }
        agent.set_credentials(key, account);
        crate::credentials::refresh_provider_for_model(&agent.provider_client_arc(), model);
    }

    /// Resolve the Fusion model from the project's single configured choice.
    fn resolve_fusion_sidekick(&self, active_model: &str) -> (String, Option<ReasoningEffort>) {
        let fast = self.agent.model_roles().resolve_fast(active_model);
        let fast_opt = if fast == active_model {
            None
        } else {
            Some(fast)
        };
        let sidekick = threadlane_orchestrator::resolve_sidekick_model(active_model, fast_opt);
        let effort = self.agent.config().fast_reasoning_effort;
        (sidekick, effort)
    }

    /// Arm Fusion for one run, including when the child uses the main model.
    pub(crate) async fn arm_fusion(&mut self, prompt: &str) -> Result<String, String> {
        let active_model = self.agent.turn.lock().await.model.clone();
        let (sidekick, effort) = self.resolve_fusion_sidekick(&active_model);
        let state = threadlane_orchestrator::FusionState::new(
            active_model.clone(),
            sidekick.clone(),
            effort,
        );
        self.set_fact(
            "fusion_state",
            &serde_json::to_string(&state).map_err(|error| error.to_string())?,
        )?;
        if let Some(harness) = self.harness.as_mut() {
            harness.record_fusion_audit(
                "main",
                None,
                serde_json::json!({
                    "version": 1,
                    "kind": "arm",
                    "main_model": state.main_model,
                    "sidekick_model": state.sidekick_model,
                    "classifier_version": state.classifier_version,
                    "compaction_generation": state.compaction_generation,
                }),
            )?;
        }
        *self
            .fusion
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(state);
        let route = threadlane_orchestrator::evaluate_fusion_prompt(prompt, &sidekick);
        let route_note = match route {
            threadlane_orchestrator::FusionDecision::DelegateToSidekick { reason } => {
                format!(" Keyword hint: consider delegation ({reason}); main validates the route.")
            }
            threadlane_orchestrator::FusionDecision::KeepOnMain { reason } => {
                format!(" Keyword hint: investigate on main ({reason}); delegate after scope is clear.")
            }
        };
        Ok(format!(
            "Fusion armed: main `{active_model}` + sidekick `{sidekick}`.{route_note}"
        ))
    }

    /// Keep the system prefix stable across tasks; per-task triage belongs in
    /// the audit, not in the cacheable delegation contract.
    fn fusion_directive(&self) -> Option<String> {
        let sidekick = self
            .fusion
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|state| state.sidekick_model.clone()))?;
        Some(threadlane_orchestrator::build_fusion_main_directive(
            &sidekick,
        ))
    }

    /// Remove the complete prior contract, including legacy triage, before
    /// installing the current stable directive.
    async fn strip_fusion_directive(&mut self) {
        let mut turn = self.agent.turn.lock().await;
        let Some(start) = turn
            .system_prompt
            .find(threadlane_orchestrator::FUSION_MAIN_HEADER)
        else {
            return;
        };
        let end = turn
            .system_prompt
            .find(threadlane_orchestrator::FUSION_MAIN_FOOTER)
            .map(|pos| pos + threadlane_orchestrator::FUSION_MAIN_FOOTER.len())
            .unwrap_or(turn.system_prompt.len());
        turn.system_prompt.replace_range(start..end, "");
        turn.system_prompt = turn.system_prompt.trim_end().to_string();
    }

    /// Compaction-boundary routing: restore legacy downgraded main lanes to
    /// their selected model and acknowledge pending escalation. Emits a
    /// `FusionUpdate` event on switch and rotates provider credentials. An
    /// upgrade back to the frontier model consumes the error streak via
    /// `record_escalation`; without that the streak could never accumulate.
    pub(crate) async fn apply_fusion_compaction_routing(&mut self) {
        let (target, is_escalation, mut snapshot) = {
            let guard = self
                .fusion
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(state) = guard.as_ref() else {
                return;
            };
            let active = self
                .agent
                .turn
                .try_lock()
                .map(|turn| turn.model.clone())
                .unwrap_or_default();
            let target = threadlane_orchestrator::select_model_at_compaction(&active, state);
            let is_escalation = state.escalation_needed();
            (target, is_escalation, state.clone())
        };
        snapshot.compaction_generation = snapshot.compaction_generation.saturating_add(1);
        if is_escalation {
            snapshot.record_escalation();
        }
        if let Some(target) = target.as_deref() {
            if let Err(error) = self.set_fact("model", target) {
                let _ = self.agent.event_tx.send(AgentEvent::AgentError { error });
                return;
            }
        }
        if let Err(error) = self.set_fact(
            "fusion_state",
            &serde_json::to_string(&snapshot).expect("Fusion state is serializable"),
        ) {
            let _ = self.agent.event_tx.send(AgentEvent::AgentError { error });
            return;
        }
        *self.fusion.lock().unwrap_or_else(|error| error.into_inner()) = Some(snapshot);
        let Some(target) = target else {
            if is_escalation {
                if let Some(harness) = self.harness.as_mut() {
                    if let Err(error) = harness.record_fusion_audit(
                        "main",
                        None,
                        serde_json::json!({
                            "version": 1,
                            "kind": "escalation_acknowledged",
                            "target_model": self.agent.model(),
                            "compaction_generation": self.fusion.lock().ok().and_then(|state| state.as_ref().map(|state| state.compaction_generation)),
                        }),
                    ) {
                        let _ = self.agent.event_tx.send(AgentEvent::AgentError { error });
                    }
                }
            }
            return;
        };
        if let Some(harness) = self.harness.as_mut() {
            if let Err(error) = harness.record_fusion_audit(
                "main",
                None,
                serde_json::json!({
                    "version": 1,
                    "kind": if is_escalation { "escalation_switch" } else { "compaction_switch" },
                    "target_model": target,
                    "compaction_generation": self.fusion.lock().ok().and_then(|state| state.as_ref().map(|state| state.compaction_generation)),
                    "provider_cache_hit": null,
                    "estimated_cost_usd": null,
                }),
            ) {
                let _ = self.agent.event_tx.send(AgentEvent::AgentError { error });
            }
        }
        {
            let mut turn = self.agent.turn.lock().await;
            turn.model = target.clone();
        }
        self.refresh_provider_credentials();
        let message = if is_escalation {
            format!(
                "Fusion escalated: main lane switched back to frontier `{target}` at compaction."
            )
        } else {
            format!(
                "Fusion routing at compaction: main lane restored to selected model `{target}`."
            )
        };
        let _ = self.agent.event_tx.send(AgentEvent::FusionUpdate {
            model: target.clone(),
            message,
        });
    }

    fn set_name(&mut self, name: String) -> Result<(), String> {
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
        let provider = Arc::new(crate::credentials::provider_client_for(
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
        let mut effective_reasoning_effort = ReasoningEffort::default();
        let (mut harness, mut harness_journal_error) = match session_file.as_deref() {
            Some(path) => match super::harness::CodingSessionHarness::open(path) {
                Ok(h) => (Some(h), None),
                Err(error) => (None, Some(error)),
            },
            None => (None, None),
        };
        let github_issue_work = harness
            .as_ref()
            .is_some_and(|harness| harness.store.facts().contains_key("github_issue"));
        let mut initial_plan = threadlane_protocol::SessionPlan::default();
        if let Some(h) = harness.as_ref() {
            if let Some(model) = h.store.facts().get("model") {
                effective_model = model.clone();
            }
            if let Some(effort) = h
                .store
                .facts()
                .get("reasoning_effort")
                .and_then(|effort| ReasoningEffort::from_label(effort))
            {
                effective_reasoning_effort = effort;
            }
            if let Some(plan_json) = h.store.facts().get("session_plan") {
                if let Ok(plan) =
                    serde_json::from_str::<threadlane_protocol::SessionPlan>(plan_json)
                {
                    initial_plan = plan;
                }
            }
        }
        let configured_fusion_model = agent_config
            .model_roles
            .resolve_fast(&effective_model)
            .to_string();
        let mut restored_fusion = agent_config.orchestrator_mode.is_fusion().then(|| {
            harness
                .as_ref()?
                .store
                .facts()
                .get("fusion_state")
                .and_then(|json| {
                    serde_json::from_str::<threadlane_orchestrator::FusionState>(json).ok()
                })
                .filter(|state| {
                    state.compatible_with(
                        &effective_model,
                        &configured_fusion_model,
                        agent_config.fast_reasoning_effort,
                    )
                })
        }).flatten();
        // A process can stop after a child lifecycle commit but before the
        // router snapshot is written. Replay only completions newer than the
        // last state fact so the failure streak and delegation count survive.
        if let (Some(harness), Some(state)) = (harness.as_mut(), restored_fusion.as_mut()) {
            let state_seq = harness
                .store
                .records()
                .iter()
                .rev()
                .find_map(|record| match record {
                    threadlane_runtime::harness::Record::FactSet { key, seq, .. }
                        if key == "fusion_state" => Some(*seq),
                    _ => None,
                })
                .unwrap_or(0);
            let recovered = harness
                .store
                .records()
                .iter()
                .filter_map(|record| match record {
                    threadlane_runtime::harness::Record::SubagentLifecycle { seq, phase, .. }
                        if *seq > state_seq
                            && matches!(phase, threadlane_runtime::harness::SubagentLifecyclePhase::Completed | threadlane_runtime::harness::SubagentLifecyclePhase::Failed) =>
                    {
                        Some(*phase == threadlane_runtime::harness::SubagentLifecyclePhase::Failed)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            for failed in &recovered {
                state.record_delegation();
                state.record_sidekick_result(*failed);
            }
            let signals = harness.store.records().iter().filter_map(|record| match record {
                threadlane_runtime::harness::Record::FactSet { key, value, seq, .. }
                    if *seq > state_seq && key.starts_with("fusion_audit:") =>
                {
                    serde_json::from_str::<serde_json::Value>(value).ok().and_then(|event| {
                        (event.get("kind")?.as_str()? == "sidekick_outcome")
                            .then(|| event.get("escalation_reason")?.as_str().map(str::to_owned))?
                    })
                }
                _ => None,
            }).collect::<Vec<_>>();
            for reason in &signals {
                state.request_escalation(reason);
            }
            if !recovered.is_empty() || !signals.is_empty() {
                if let Err(error) = harness.set_fact(
                    "main",
                    "fusion_state",
                    serde_json::to_string(state).expect("Fusion state is serializable"),
                ) {
                    harness_journal_error = Some(error);
                }
            }
        }
        // Older Fusion runs could downgrade the main lane to the sidekick.
        // Restore the selected main model before constructing the runtime.
        if let Some(state) = restored_fusion.as_ref() {
            if effective_model != state.main_model {
                effective_model = state.main_model.clone();
                if let Some(harness) = harness.as_mut() {
                    if let Err(error) = harness.set_fact("main", "model", effective_model.clone()) {
                        harness_journal_error = Some(error);
                    }
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
        let plan_store = session_plan_store(initial_plan, session_file.clone());
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
        if effective_model != options.model {
            let (key, account) = crate::credentials::provider_credentials(&effective_model);
            if !key.trim().is_empty() {
                agent.set_credentials(key, account);
            }
            crate::credentials::refresh_provider_for_model(
                &agent.provider_client_arc(),
                &effective_model,
            );
        }
        agent
            .turn
            .try_lock()
            .expect("new agent turn must be unlocked")
            .reasoning_effort = effective_reasoning_effort;
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
            .unwrap_or_else(|error| {
                tracing::warn!("Cannot reload extensions: {error}");
                0
            });
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
        let runner_work_dir = options.work_dir.clone();
        let runner_extensions = wasi_extensions.clone();
        let runner_event_tx = agent.event_tx.clone();
        let runner_session_file = session_file.clone();
        let runner_semaphore = Arc::new(tokio::sync::Semaphore::new(
            coding_config.subagent_concurrency_limit,
        ));
        let dispatch_parent_leaf = Arc::new(std::sync::Mutex::new(None));
        let completed_subagent_lanes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hub = super::mailbox::SubagentHub::new();
        let fusion: Arc<std::sync::Mutex<Option<threadlane_orchestrator::FusionState>>> =
            Arc::new(std::sync::Mutex::new(restored_fusion));
        let runner_fusion = fusion.clone();
        // Cloned separately for the `hub revive` spawner below; the
        // `agent_runner` closure moves its own copies.
        let revive_api_key = runner_api_key.clone();
        let revive_account_id = runner_account_id.clone();
        let revive_state = runner_state.clone();
        let revive_config = agent_config.clone();
        let revive_work_dir = runner_work_dir.clone();
        let revive_extensions = runner_extensions.clone();
        let revive_event_tx = runner_event_tx.clone();
        let revive_session_file = runner_session_file.clone();
        let revive_semaphore = runner_semaphore.clone();
        let revive_hub = hub.clone();
        let revive_parent_leaf = dispatch_parent_leaf.clone();
        let revive_completed_lanes = completed_subagent_lanes.clone();
        let revive_parent_session_id = session_id.clone();
        let revive_fusion = fusion.clone();
        let runner_parent_leaf = dispatch_parent_leaf.clone();
        let runner_completed_lanes = completed_subagent_lanes.clone();
        let runner_hub = hub.clone();
        let parent_session_id = session_id.clone();
        let agent_runner: AgentRunner = Arc::new(move |tasks, parallel, tool_call_id| {
            #[cfg(test)]
            let observer = runner_observer.clone();
            let api_key = runner_api_key.clone();
            let account_id = runner_account_id.clone();
            let state = runner_state.clone();
            let work_dir = runner_work_dir.clone();
            let extensions = runner_extensions.clone();
            let event_tx = runner_event_tx.clone();
            let session_file = runner_session_file.clone();
            let semaphore = runner_semaphore.clone();
            let hub = runner_hub.clone();
            let fusion = runner_fusion.clone();
            let parent_leaf_id = runner_parent_leaf.lock().ok().and_then(|leaf| leaf.clone());
            let completed_lanes = runner_completed_lanes.clone();
            let parent_session_id = parent_session_id.clone();
            Box::pin(async move {
                let (model, parent_reasoning_effort) = {
                    let state = state.lock().await;
                    (state.model.clone(), state.reasoning_effort())
                };
                // Fusion uses the single configured child model.
                let (fusion_sidekick, fusion_effort) = fusion
                    .lock()
                    .ok()
                    .and_then(|guard| {
                        guard
                            .as_ref()
                            .map(|state| (state.sidekick_model.clone(), state.sidekick_effort))
                    })
                    .map(|(m, e)| (Some(m), e))
                    .unwrap_or((None, None));
                let fusion_armed = fusion_sidekick.is_some();
                let child_model = fusion_sidekick.unwrap_or_else(|| model.clone());
                // Resolve live: the parent may have switched providers since
                // construction (slash `/model`, Fusion routing). Falls back
                // to the construction key when nothing is stored.
                let (api_key, account_id) = {
                    let (key, account) = crate::credentials::provider_credentials(&child_model);
                    if key.trim().is_empty() {
                        (api_key, account_id)
                    } else {
                        (key, account)
                    }
                };
                let child_reasoning_effort = fusion_effort.unwrap_or(parent_reasoning_effort);
                // Fusion sidekick lanes run under the sidekick contract:
                // implement and verify mechanically, never guess at ambiguous
                // intent. Stamped onto every delegated child while armed so
                // the directive the main agent was given actually reaches the
                // lane doing the work.
                let tasks = if fusion_armed {
                    let directive = threadlane_orchestrator::build_fusion_sidekick_directive();
                    tasks
                        .into_iter()
                        .map(|mut task| {
                            task.instructions = Some(match task.instructions.take() {
                                Some(existing) => format!("{existing}\n{directive}"),
                                None => directive.clone(),
                            });
                            task
                        })
                        .collect()
                } else {
                    tasks
                };
                #[cfg(test)]
                let observer = observer
                    .and_then(|observer| observer.lock().ok().and_then(|value| value.clone()));
                let (output, thinking, _) = run_subagents_with_context(
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
                        completed_lanes,
                        hub,
                        #[cfg(test)]
                        scheduler_observer: observer,
                        #[cfg(test)]
                        child_work_observer: None,
                        #[cfg(test)]
                        child_tool_observer: None,
                        #[cfg(test)]
                        child_run_override: None,
                        #[cfg(test)]
                        child_execution_observer: None,
                        semaphore,
                    },
                )
                .await?;
                Ok(serde_json::json!({
                    "message": output,
                    "output": output,
                    "thinking": thinking
                }))
            })
        });
        // `hub revive` spawner: reuses the subagent ingredients above to open
        // a follow-up operation on the settled lane (same history).
        let revive_hook: super::mailbox::ReviveHook =
            Arc::new(move |req: super::mailbox::ReviveRequest| {
                let api_key = revive_api_key.clone();
                let account_id = revive_account_id.clone();
                let state = revive_state.clone();
                let runner_config = revive_config.clone();
                let work_dir = revive_work_dir.clone();
                let extensions = revive_extensions.clone();
                let event_tx = revive_event_tx.clone();
                let session_file = revive_session_file.clone();
                let semaphore = revive_semaphore.clone();
                let hub = revive_hub.clone();
                let parent_leaf = revive_parent_leaf.clone();
                let completed_lanes = revive_completed_lanes.clone();
                let parent_session_id = revive_parent_session_id.clone();
                let fusion = revive_fusion.clone();
                Box::pin(async move {
                    let compatible = fusion.lock().ok().and_then(|state| {
                        state.as_ref().map(|state| state.sidekick_model == req.model)
                    });
                    if compatible != Some(true) {
                        return Err("Fusion lane model changed; start a new child instead of reviving stale context".into());
                    }
                    let parent_reasoning_effort = {
                        let state = state.lock().await;
                        state.reasoning_effort()
                    };
                    let child_reasoning_effort = runner_config
                        .fast_reasoning_effort
                        .unwrap_or(parent_reasoning_effort);
                    let parent_leaf_id = parent_leaf.lock().ok().and_then(|leaf| leaf.clone());
                    // The revived run keeps the lane's original model so history
                    // and behavior stay continuous.
                    let child_model = req.model.clone();
                    // Resolve live (see the foreground runner above).
                    let (api_key, account_id) = {
                        let (key, account) = crate::credentials::provider_credentials(&child_model);
                        if key.trim().is_empty() {
                            (api_key, account_id)
                        } else {
                            (key, account)
                        }
                    };
                    revive_subagent_lane(
                        ReviveLaneRequest {
                            lane_name: req.lane_name,
                            agent: req.agent,
                            task: req.task,
                            model: req.model,
                            message: req.message,
                        },
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
                            completed_lanes,
                            hub,
                            #[cfg(test)]
                            scheduler_observer: None,
                            #[cfg(test)]
                            child_work_observer: None,
                            #[cfg(test)]
                            child_tool_observer: None,
                            #[cfg(test)]
                            child_run_override: None,
                            #[cfg(test)]
                            child_execution_observer: None,
                            semaphore,
                        },
                    )
                    .await
                })
            });
        let (broker_dispatcher, managed_processes, permission_handle, permissions) =
            build_broker_dispatcher(
                tool_policy.clone(),
                wasi_extensions.clone(),
                true,
                options.work_dir.clone(),
                agent.event_tx.clone(),
                agent_work.clone(),
                agent_config
                    .orchestrator_mode
                    .is_fusion()
                    .then(|| agent_runner.clone()),
                options.session_file.clone(),
            );
        let mcp_manager = Arc::new(McpManager::new(
            default_global_threadlane_dir(),
            Some(options.work_dir.clone()),
        ));
        let mut registry = threadlane_runtime::CapabilityRegistry::new();
        let automation_result_required = harness.as_ref().is_some_and(|h| {
            h.store
                .store()
                .facts()
                .get("automation_result_contract")
                .map(String::as_str)
                == Some("1")
        });
        if let Some(session_file) = session_file.as_ref().filter(|_| automation_result_required) {
            registry.register(Box::new(crate::automation::AutomationResultCapability {
                session_file: session_file.clone(),
                run_id: harness_run_id.clone(),
            }));
        }
        if let Some(session_file) = options.session_file.clone() {
            registry.register(Box::new(crate::automation_tool::AutomationCapability {
                work_dir: options.work_dir.clone(), session_file, model: effective_model.clone(),
            }));
        }
        registry.register(Box::new(SkillCapability {
            skills: skills.clone(),
        }));
        if agent_config.orchestrator_mode.is_fusion() {
            registry.register(Box::new(SubagentCapability {
                agent_runner: agent_runner.clone(),
                hub: hub.clone(),
                session_file: session_file.clone(),
                revive_hook: Some(revive_hook),
            }));
        }
        registry.register(Box::new(PlanCapability {
            plan_store: plan_store.clone(),
            event_tx: agent.event_tx.clone(),
        }));
        let question_manager = QuestionManager::new();
        let question_handle = question_manager.handle();
        registry.register(Box::new(QuestionCapability {
            handle: question_handle.clone(),
            event_tx: agent.event_tx.clone(),
        }));
        if let Some(session_file) = options.session_file.clone() {
            registry.register(Box::new(ContextCapability {
                session_file,
                work_dir: options.work_dir.clone(),
            }));
        }
        if github_issue_work || threadlane_git::is_git_repo(&options.work_dir) {
            registry.register(Box::new(GitHubCapability {
                work_dir: options.work_dir.clone(),
            }));
        }
        if threadlane_git::is_git_repo(&options.work_dir) {
            registry.register(Box::new(WorktreeCapability {
                work_dir: options.work_dir.clone(),
            }));
        }
        // No orchestrator handoff tool: Fusion delegation flows through the
        // existing `subagent` tool with the sidekick model forced by the
        // runner, not an explicit model-invoked handoff.

        registry.register(Box::new(WasiCapability {
            extensions: wasi_extensions.clone(),
            broker_dispatcher: broker_dispatcher.clone(),
            tool_policy: tool_policy.clone(),
        }));
        registry.register(Box::new(McpCapability {
            mcp_manager: mcp_manager.clone(),
        }));
        registry.register(Box::new(BrowserCapability {
            bridge: options.browser.clone(),
        }));
        registry.register(Box::new(ComputerCapability {
            permissions: Some(permissions.clone()),
        }));
        let (_wired, errors) = registry.wire_all(&mut agent.tool_dispatcher, &agent.hook_registry);
        for error in &errors {
            eprintln!("{error}");
        }

        let manager_clone = mcp_manager.clone();
        threadlane_provider::exec::get_runtime().spawn(async move {
            manager_clone.discover_and_connect().await;
        });
        agent.work_dir = Some(options.work_dir.clone());
        agent
            .turn
            .try_lock()
            .expect("new runtime turn is unlocked")
            .project_root = Some(options.work_dir.clone());

        let mut system_prompt_config = options.system_prompt.clone();
        if automation_result_required {
            system_prompt_config.guidelines.push(
                "For the original automation operation (not later interactive follow-ups), before your final response call report_automation_result with evidence of success or the exact unresolved blocker. If access restrictions prevent the task, report blocked; do not treat a normal final response or unperformed research as success. Do not bypass read-only policy.".into(),
            );
        }
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
            // Spin briefly on transient contention instead of panicking:
            // construction-time locking only races a concurrent holder for
            // an instant. Poison still panics (state is unrecoverable).
            let mut turn_guard = None;
            for _ in 0..100 {
                match agent.turn.try_lock() {
                    Ok(guard) => {
                        turn_guard = Some(guard);
                        break;
                    }
                    Err(_) => std::thread::yield_now(),
                }
            }
            let mut turn = turn_guard.expect("Failed to lock initial state");
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

        let acp = threadlane_acp_engine::AcpEngine::new(
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
            question_handle,
            agent_work,
            mcp_manager,
            prompt_templates: None,
            dispatch_parent_leaf,
            completed_subagent_lanes,
            harness,
            harness_journal_error,
            harness_run_id,
            fusion,
            hub,
            cancellation,
            interrupted_subagent_recovery,
            acp,
            #[cfg(test)]
            subagent_work_observer,
            #[cfg(test)]
            subagent_execution_observer: None,
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
        queued: Option<(threadlane_runtime::harness::QueueKind, &str)>,
    ) -> Option<Result<String, String>> {
        // A retry can arrive while the external agent is still answering Stop.
        // Retain that input as existing queue intent before waiting, then open
        // the new operation only after all old permission responses are sent.
        let staged_entry = if queued.is_none() && self.acp.has_pending_turn(agent_id) {
            if let Some(journal) = self.harness.as_mut() {
                let queue = threadlane_runtime::harness::QueueKind::NextRun;
                let entry_id = match journal.enqueue_unbound_with_images(
                    queue.clone(),
                    input.to_string(),
                    images.clone(),
                ) {
                    Ok(entry_id) => entry_id,
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                };
                self.agent_work.schedule(AgentWork::DurableQueueWake {
                    queue,
                    entry_id: entry_id.clone(),
                });
                Some(entry_id)
            } else {
                None
            }
        } else {
            None
        };
        let queued = queued.or_else(|| {
            staged_entry
                .as_deref()
                .map(|entry_id| (threadlane_runtime::harness::QueueKind::NextRun, entry_id))
        });
        self.acp.finish_pending_turn(agent_id).await;
        let msg = AgentMessage::user(input, images.clone());
        let harness_run_id = match self.begin_harness_run_with_queue(msg, queued).await {
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

        if let Some(plan) = outcome.plan {
            let saved = serde_json::to_string(&plan)
                .map_err(|error| error.to_string())
                .and_then(|plan| self.set_fact("session_plan", &plan));
            if let Err(error) = saved {
                let _ = self
                    .finish_harness_run(run_id, OperationOutcome::Failed, Some(error.clone()))
                    .await;
                return Some(Err(format!("Harness Error: {error}")));
            }
        }

        if let (Some(run_id), Some(journal)) = (run_id, self.harness.as_mut()) {
            // Hook view of this turn for WASI extensions (goal loop, etc.).
            // ACP tools carry display titles as names, so the hook gets the
            // same nested `ToolCall` shape the native path emits; goal
            // completion from an external agent arrives via the
            // `<!-- GOAL_COMPLETE -->` content marker instead.
            let hook_tool_calls = outcome
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
                .collect::<Vec<_>>();
            let hook_message = AgentMessage::Assistant {
                content: Some(outcome.reply.clone()),
                tool_calls: if hook_tool_calls.is_empty() {
                    None
                } else {
                    Some(hook_tool_calls)
                },
                stop_reason: None,
                deferred_handle: None,
            };
            // ACP tools execute inside the external agent, but their ordered
            // preambles and results must precede the final reply after reload.
            let has_tools = !outcome.tools.is_empty();
            for tool in outcome.tools {
                let arguments = serde_json::from_str(&tool.arguments)
                    .unwrap_or_else(|_| serde_json::Value::String(tool.arguments.clone()));
                let recorded = journal
                    .append_message(AgentMessage::Assistant {
                        content: (!tool.preamble.is_empty()).then_some(tool.preamble),
                        tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                            id: tool.tool_call_id.clone(),
                            r#type: "function".into(),
                            function: threadlane_provider::openai::ToolCallFunction {
                                name: tool.name.clone(),
                                arguments: tool.arguments.clone(),
                            },
                            thought_signature: None,
                        }]),
                        stop_reason: None,
                        deferred_handle: None,
                    })
                    .and_then(|_| {
                        journal.tool_started_on_lane(
                            "main",
                            run_id,
                            &tool.tool_call_id,
                            &tool.name,
                            arguments,
                        )
                    })
                    .and_then(|_| {
                        let mut result = tool.result.unwrap_or_else(|| {
                            threadlane_protocol::AgentToolResult::external(
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

            if !outcome.reply.is_empty() || !has_tools {
                let recorded = journal.append_message(AgentMessage::Assistant {
                    content: Some(outcome.reply),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
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
            // Fire the same `assistant_message` extension hooks the native
            // turn path fires so autonomous loops (goal) keep chaining on
            // ACP models. The hook's `agent.request_turn` schedules the next
            // ACP follow-up, drained by the caller.
            self.dispatch_assistant_hook(&hook_message).await;
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

    async fn run_queued_acp_work(&mut self, agent_id: &str) -> Option<Result<String, String>> {
        while let Some(work) = self.agent_work.next() {
            let result = match &work {
                AgentWork::DurableQueueWake { queue, entry_id } => {
                    let message = match self.harness.as_mut() {
                        Some(journal) => journal.unbound_queue_message(queue.clone(), entry_id),
                        None => Err("session persistence is unavailable".into()),
                    };
                    let message = match message {
                        Ok(Some(message)) => message,
                        Ok(None) => {
                            self.agent_work.finish_next();
                            continue;
                        }
                        Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                    };
                    let (content, images) = match message {
                        AgentMessage::User { content } => (content, Vec::new()),
                        AgentMessage::UserWithImages { content, images } => (content, images),
                        _ => return Some(Err("Queued ACP input must be a user message".into())),
                    };
                    self.run_acp_turn(agent_id, &content, images, Some((queue.clone(), entry_id)))
                        .await
                }
                AgentWork::QueueMessage { content, images, .. }
                | AgentWork::SteerMessage { content, images, .. } => {
                    // Legacy pending steer inputs are retained as later prompts.
                    self.run_acp_turn(agent_id, content, images.clone(), None)
                        .await
                }
            };
            if result.is_some() {
                // The durable wake stays until its accepted input is observed
                // as consumed on retry. All later queued inputs remain pending.
                return result;
            }
            self.agent_work.finish_next();
        }
        None
    }

    pub async fn handle_input_with_images(
        &mut self,
        input: &str,
        images: Vec<ImageAttachment>,
    ) -> Option<Result<String, String>> {
        let first_entry = self.harness.as_ref().map_or(0, |h| h.store.entries().len());
        let result = self.handle_input_inner(input, images.clone()).await;
        if let Some(Err(error)) = &result {
            // Pre-acceptance failures have no finish_harness_run to persist them.
            // Save them before the surface reloads its durable transcript.
            if let Err(persistence_error) =
                self.persist_prompt_error(input, &images, error, first_entry)
            {
                return Some(Err(format!(
                    "{error}\nCould not save error: {persistence_error}"
                )));
            }
        }
        result
    }

    /// Save failures that occur before a prompt reaches the turn driver as well
    /// as its fallback errors. Never infer a retry from a previous user row.
    pub(crate) fn persist_prompt_error(
        &mut self,
        input: &str,
        images: &[ImageAttachment],
        error: &str,
        // usize::MAX disables deduplication for failures before input handling.
        first_entry: usize,
    ) -> Result<(), String> {
        let Some(journal) = self.harness.as_mut() else {
            return Ok(());
        };
        journal.ensure_fresh()?;
        let retry_prompt = parse_slash_command(input)
            .is_none()
            .then(|| threadlane_protocol::RetryPrompt {
                text: input.to_owned(),
                images: images.to_vec(),
            })
            .filter(threadlane_protocol::RetryPrompt::is_sendable);
        let retry_prompt_value = serde_json::to_value(&retry_prompt).map_err(|error| error.to_string())?;
        let already_saved = journal.store.entries().iter().skip(first_entry).any(|entry| {
            entry.lane == "main" && matches!(&entry.message,
                AgentMessage::Custom { custom_type, payload }
                    if custom_type == "agent_error" && payload.get("error").and_then(|v| v.as_str()) == Some(error)
                        && payload.get("retry_prompt") == Some(&retry_prompt_value))
        });
        if already_saved {
            return Ok(());
        }
        journal.append_message(AgentMessage::Custom {
            custom_type: "agent_error".into(),
            payload: serde_json::json!({ "error": error, "retry_prompt": retry_prompt }),
        })?;
        Ok(())
    }

    async fn handle_input_inner(
        &mut self,
        input: &str,
        images: Vec<ImageAttachment>,
    ) -> Option<Result<String, String>> {
        self.cancellation.clear_cancellation_guard();
        if let Err(error) = self.recover_saved_extension_replies().await {
            return Some(Err(format!("Harness Error: {error}")));
        }
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
        // This entry point accepts a new prompt; adopted runs execute through
        // execute_accepted_run. Stop drops the old future before its cleanup,
        // so reconcile its journal and clear its stale handle for every provider.
        if let Some(journal) = self.harness.as_mut() {
            if let Err(error) = journal.recover_abort() {
                return Some(Err(format!("Harness Error: {error}")));
            }
        }
        if let Ok(mut run_id) = self.harness_run_id.lock() {
            *run_id = None;
        }
        *self
            .dispatch_parent_leaf
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        let trimmed = input.trim();

        if self.prompt_templates.is_none() {
            let global_dir = std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".threadlane"))
                .unwrap_or_else(|| self.work_dir.join(".threadlane"));
            self.prompt_templates = Some(threadlane_skills::prompts::load_prompt_templates(
                &self.work_dir,
                &global_dir,
            ));
        }
        let templates = self.prompt_templates.as_ref().unwrap();
        let expanded_input = threadlane_skills::prompts::expand_prompt_template(trimmed, templates);
        let mut effective_input = expanded_input.trim().to_string();
        let mut fusion_directive: Option<String> = None;

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
                        *self
                            .dispatch_parent_leaf
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) = parent_leaf;
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
                            *self
                                .dispatch_parent_leaf
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = None;
                            let _ = self
                                .finish_harness_run(
                                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(error));
                        }
                        *self
                            .dispatch_parent_leaf
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) = None;
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
                    context_refs: Vec::new(),
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
                *self
                    .dispatch_parent_leaf
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = parent_leaf;
                let result = match (self.agent_runner)(vec![task], false, None).await {
                    Ok(result) => result,
                    Err(err) => {
                        *self
                            .dispatch_parent_leaf
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) = None;
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
                    *self
                        .dispatch_parent_leaf
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = None;
                    let _ = self
                        .finish_harness_run(
                            harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                            OperationOutcome::Failed,
                            Some(error.clone()),
                        )
                        .await;
                    return Some(Err(error));
                }
                *self
                    .dispatch_parent_leaf
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = None;
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
                // Close the command's harness run before draining follow-up work.
                // ACP turns open their own run and reject nested runs while this
                // extension command is still active.
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
                if let Some(result) = self.run_scheduled_agent_work().await {
                    return Some(result);
                }
                return Some(Ok(output));
            }

            let command_extensions = self.wasi_extensions.clone();
            if let Some(operation) = command_extensions.begin_command_operation(cmd_name) {
                let mut operation = match operation {
                    Ok(operation) => operation,
                    Err(error) => return Some(Err(format!("WASI Extension Error: {error}"))),
                };
                let visible_prompt = AgentMessage::user(input, images.clone());
                let harness_run_id = match self.begin_harness_run(visible_prompt).await {
                    Ok(run_id) => run_id,
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                };
                let res = operation
                    .invoke(&cmd_args)
                    .and_then(|result| result.into_command_result());
                let parent_leaf = self.prompt_parent_leaf(
                    AgentMessage::user(input, images.clone()),
                    harness_run_id.is_some(),
                );
                *self
                    .dispatch_parent_leaf
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = parent_leaf;
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
                        if let Err(error) = self
                            .wasi_extensions
                            .enqueue_broker_results(dispatch.operation_results)
                        {
                            let _ = self
                                .finish_harness_run(
                                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(error));
                        }
                        drop(operation);
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
                            *self
                                .dispatch_parent_leaf
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = None;
                            let _ = self
                                .finish_harness_run(
                                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(error));
                        }
                        *self
                            .dispatch_parent_leaf
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) = None;
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
                        // The command run must be closed before an ACP follow-up
                        // can open its own harness run. Propagate follow-up errors
                        // so the command is not reported as successfully complete.
                        if let Some(scheduled_result) = self.run_scheduled_agent_work().await {
                            return Some(scheduled_result);
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
                if let CommandAction::Fusion(objective) = &cmd_action {
                    if !self.agent.config().orchestrator_mode.is_fusion() {
                        return Some(Err(
                            "Switch the session to Fusion mode to delegate work.".into()
                        ));
                    }
                    let task_prompt = objective.trim();
                    if task_prompt.is_empty() {
                        return Some(Ok("Usage: /fusion <task objective> - run a main agent with sidekick child lanes.".into()));
                    }
                    let message = match self.arm_fusion(task_prompt).await {
                        Ok(message) => message,
                        Err(error) => return Some(Err(error)),
                    };
                    let _ = self.agent.event_tx.send(AgentEvent::FusionUpdate {
                        model: self.agent.model(),
                        message,
                    });
                    effective_input = task_prompt.to_string();
                    // Explicit re-arm replaces the router state, so refresh
                    // the injected directive too instead of keeping the
                    // previous task's triage.
                    self.strip_fusion_directive().await;
                    fusion_directive = self.fusion_directive();
                } else {
                    let output = execute_slash_command(cmd_action, &mut self.agent).await;
                    return Some(Ok(output));
                }
            }
        }

        // --- Fusion router. Stored `OrchestratorMode::Fusion` arms every
        // prompt; `/fusion` can explicitly re-arm a task in that mode.
        if fusion_directive.is_none()
            && self.agent.config().orchestrator_mode.is_fusion()
            && !effective_input.trim().is_empty()
        {
            if self
                .fusion
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_none()
            {
                let message = match self.arm_fusion(&effective_input.clone()).await {
                    Ok(message) => message,
                    Err(error) => return Some(Err(error)),
                };
                let _ = self.agent.event_tx.send(AgentEvent::FusionUpdate {
                    model: self.agent.model(),
                    message,
                });
            }
            self.strip_fusion_directive().await;
            fusion_directive = self.fusion_directive();
        }

        if let Some(directive) = fusion_directive {
            let mut turn = self.agent.turn.lock().await;
            if !turn
                .system_prompt
                .contains(threadlane_orchestrator::FUSION_MAIN_HEADER)
            {
                let directive = if turn.system_prompt.contains(threadlane_prompt::workflow::IMPLEMENTATION_HANDOFF) {
                    directive.replace(threadlane_prompt::workflow::IMPLEMENTATION_HANDOFF, "")
                } else {
                    directive
                };
                turn.system_prompt.push_str(&directive);
            }
        }

        // An ACP agent runs its own loop behind the protocol: it does not use
        // Threadlane's provider, tools, or message replay, so it is dispatched
        // here rather than through the provider run below.
        if let Some(agent_id) = threadlane_acp_engine::acp_agent_id(&self.agent.model()) {
            let agent_id = agent_id.to_string();
            let result = self
                .run_acp_turn(&agent_id, &effective_input, images, None)
                .await;
            return if result.is_some() {
                result
            } else {
                self.run_queued_acp_work(&agent_id).await
            };
        }

        let fusion_route = self
            .fusion
            .lock()
            .ok()
            .and_then(|state| state.as_ref().map(|state| (state.sidekick_model.clone(), state.compaction_generation)))
            .map(|(sidekick, generation)| {
                (
                    threadlane_orchestrator::evaluate_fusion_prompt(&effective_input, &sidekick),
                    threadlane_orchestrator::classify_fusion_prompt(&effective_input),
                    sidekick,
                    generation,
                )
            });
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
        if let (Some((route, classification, sidekick, generation)), Some(run), Some(harness)) = (
            fusion_route,
            harness_run_id.as_ref(),
            self.harness.as_mut(),
        ) {
            let active_model = self.agent.model();
            let cache_capabilities = self
                .agent
                .provider_client()
                .cache_capabilities(&active_model);
            let sidekick_cache_capabilities = self
                .agent
                .provider_client()
                .cache_capabilities(&sidekick);
            let (decision, reason) = match route {
                threadlane_orchestrator::FusionDecision::DelegateToSidekick { reason } => {
                    ("delegate", reason)
                }
                threadlane_orchestrator::FusionDecision::KeepOnMain { reason } => {
                    ("main", reason)
                }
            };
            if let Err(error) = harness.record_fusion_audit(
                "main",
                Some(&run.run_id),
                serde_json::json!({
                    "version": 1,
                    "kind": "route",
                    "decision": decision,
                    "reason": reason,
                    "classifier_version": classification.version,
                    "confidence_percent": classification.confidence_percent,
                    "reason_codes": classification.reason_codes,
                    "abstain": classification.abstain,
                    "active_model": active_model,
                    "sidekick_model": sidekick,
                    "compaction_generation": generation,
                    "cache_capabilities": cache_capabilities,
                    "sidekick_cache_capabilities": sidekick_cache_capabilities,
                    "cache_key_identity": if cache_capabilities.accepts_cache_key { Some(self.session_id.as_str()) } else { None },
                    "provider_cache_hit": null,
                    "estimated_cost_usd": null,
                }),
            ) {
                let _ = self
                    .finish_harness_run(
                        Some(&run.run_id),
                        OperationOutcome::Failed,
                        Some(error.clone()),
                    )
                    .await;
                return Some(Err(format!("Harness Error: {error}")));
            }
        }
        let parent_leaf = self.prompt_parent_leaf(msg.clone(), harness_run_id.is_some());
        *self
            .dispatch_parent_leaf
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = parent_leaf;
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
            *self
                .dispatch_parent_leaf
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
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
            *self
                .dispatch_parent_leaf
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
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
            *self
                .dispatch_parent_leaf
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
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
        *self
            .dispatch_parent_leaf
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
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
#[path = "broker_repair_tests.rs"]
pub(crate) mod broker_repair_tests;

#[cfg(test)]
mod tool_identity_tests {
    use super::{CodingAgent, CodingAgentOptions};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use threadlane_prompt::SystemPromptConfig;
    use threadlane_protocol::browser::BrowserBridge;
    use threadlane_protocol::{
        AgentMessage, AgentToolCall, AgentToolDefinition, ImageAttachment, RuntimeToolCall,
        RuntimeToolCallFunction, ToolExecutionError, ToolExecutionIdentity, ToolExecutor,
        ToolOutput,
    };
    use threadlane_runtime::harness::{JsonlStore, OperationOutcome, Record, SessionStore};

    struct CommittedIntentProbe {
        path: PathBuf,
        observed: Arc<Mutex<Vec<ToolExecutionIdentity>>>,
    }

    #[cfg(unix)]
    fn install_terminal_broker_tool(directory: &Path) {
        let manifest = serde_json::json!({"api_version":2,"name":"reply_probe","version":"1","description":"test","capabilities":["process"],"tools":[{"name":"reply_probe","description":"test","parameters":{}}],"hooks":["after_tool_call"]}).to_string();
        let response = r#"{"state":{"finished":true},"message":"raw reply"}"#;
        let request = serde_json::json!({"api_version":2,"capability":"process","operation":"run","arguments":{"program":"sh","args":["-c","printf effect >> effects.log; printf broker-reply"]}}).to_string();
        let hook_request = serde_json::json!({"api_version":2,"capability":"process","operation":"run","arguments":{"program":"sh","args":["-c","printf hook >> hooks.log"]}}).to_string();
        let escape = |value: &str| value.replace('"', "\\\"");
        let wasm = format!(
            r#"(module
            (import "threadlane_host" "request" (func $request (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "{}")
            (data (i32.const 1024) "{}")
            (data (i32.const 2048) "{}")
            (data (i32.const 3072) "{}")
            (func (export "extension_info") (result i64) (i64.const {}))
            (func (export "alloc") (param i32) (result i32) (i32.const 8192))
            (func (export "execute_tool") (param i32 i32) (result i64)
                (drop (call $request (i32.const 2048) (i32.const {}) (i32.const 4096) (i32.const 1024)))
                (i64.const {}))
            (func (export "handle_hook") (param i32 i32) (result i64)
                (drop (call $request (i32.const 3072) (i32.const {}) (i32.const 4096) (i32.const 1024)))
                (i64.const {})))"#,
            escape(&manifest),
            escape(response),
            escape(&request),
            escape(&hook_request),
            manifest.len(),
            request.len(),
            (1024u64 << 32) | response.len() as u64,
            hook_request.len(),
            (1024u64 << 32) | response.len() as u64
        );
        let path = directory.join(".threadlane/extensions/reply_probe.wasm");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, wasm).unwrap();
    }

    #[cfg(unix)]
    fn reply_probe_options(directory: &Path, path: &Path) -> CodingAgentOptions {
        CodingAgentOptions {
            api_key: "test".into(),
            account_id: None,
            model: "test-model".into(),
            work_dir: directory.to_owned(),
            session_file: Some(path.to_owned()),
            system_prompt: SystemPromptConfig::default(),
            agent_config: None,
            coding_config: None,
            browser: BrowserBridge::unavailable(),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_reply_recovery_preserves_unknown_after_hook_effects_without_replay() {
        let directory = tempfile::tempdir().unwrap();
        install_terminal_broker_tool(directory.path());
        let path = directory.path().join("session.jsonl");
        let mut agent = CodingAgent::new(reply_probe_options(directory.path(), &path));
        let accepted = agent
            .begin_harness_run(AgentMessage::user("probe", vec![]))
            .await
            .unwrap()
            .unwrap();
        let call = RuntimeToolCall {
            id: "original-call".into(),
            r#type: "function".into(),
            function: RuntimeToolCallFunction {
                name: "reply_probe".into(),
                arguments: "{}".into(),
            },
            thought_signature: None,
        };
        agent
            .harness
            .as_mut()
            .unwrap()
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![call]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        let identity = agent
            .harness
            .as_mut()
            .unwrap()
            .append_tool_intent_after_hook(
                &accepted.run_id,
                "original-call",
                "reply_probe",
                serde_json::json!({}),
            )
            .await
            .unwrap();
        let mut operation = agent
            .wasi_extensions
            .begin_tool_operation("reply_probe")
            .unwrap()
            .unwrap();
        let terminal = operation.invoke_for_execution("{}", &identity).unwrap();
        let dispatch = agent
            .broker_dispatcher
            .dispatch_envelopes(terminal.host_broker_requests)
            .await
            .unwrap();
        agent
            .wasi_extensions
            .enqueue_broker_results(dispatch.operation_results)
            .unwrap();
        drop(operation);
        let mut hook = agent
            .wasi_extensions
            .begin_hook_operations("after_tool_call")
            .next()
            .unwrap()
            .unwrap();
        let invocation = hook.invoke_after_tool("{}", &identity).unwrap();
        let dispatch = agent
            .broker_dispatcher
            .dispatch_envelopes(invocation.host_broker_requests)
            .await
            .unwrap();
        assert!(dispatch
            .operation_results
            .iter()
            .all(|result| result.error.is_none()));
        // The physical hook completed, but its outcome was lost before persistence.
        drop(dispatch);
        drop(hook);
        drop(agent);
        std::fs::remove_file(
            directory
                .path()
                .join(".threadlane/extensions/reply_probe.wasm"),
        )
        .unwrap();
        let mut recovered = CodingAgent::new(reply_probe_options(directory.path(), &path));
        let before = std::fs::read(&path).unwrap();
        let error = recovered
            .recover_saved_extension_replies()
            .await
            .unwrap_err();
        assert!(error.contains("remain unsettled"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            recovered
                .wasi_extensions
                .pending_tool_reply_identities()
                .unwrap(),
            vec![identity.clone()]
        );
        assert!(
            !JsonlStore::open(&path)
                .unwrap()
                .tool_state_for_call(&identity.run_id, "original-call")
                .unwrap()
                .1
                .completed
        );
        assert_eq!(
            std::fs::read(directory.path().join("effects.log")).unwrap(),
            b"effect"
        );
        assert_eq!(
            std::fs::read(directory.path().join("hooks.log")).unwrap(),
            b"hook"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_reply_recovers_original_journal_result_without_tool_or_hook_replay() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use threadlane_runtime::harness::{HookEffect, HookKind};

        for dyn_form in [false, true] {
            for committed_state in [0, 1, 2, 3] {
                let already_committed = matches!(committed_state, 1 | 2);
                let directory = tempfile::tempdir().unwrap();
                install_terminal_broker_tool(directory.path());
                let path = directory.path().join("session.jsonl");
                let options = || reply_probe_options(directory.path(), &path);
                let mut agent = CodingAgent::new(options());
                agent.agent.set_allowed_tool_names(None);
                let hooks = Arc::new(AtomicUsize::new(0));
                let hook_count = hooks.clone();
                agent
                    .agent
                    .hook_registry
                    .register(
                        HookKind::AfterTool,
                        "reply-marker",
                        Arc::new(move |_| {
                            let hooks = hook_count.clone();
                            Box::pin(async move {
                                hooks.fetch_add(1, Ordering::SeqCst);
                                if committed_state == 3 {
                                    panic!("after-hook panic");
                                }
                                let mut effect = HookEffect::default();
                                effect.append_content = Some("hook reply".into());
                                Ok(effect)
                            })
                        }),
                    )
                    .unwrap();
                let accepted = agent
                    .begin_harness_run(AgentMessage::user("probe", vec![]))
                    .await
                    .unwrap()
                    .unwrap();
                let name = if dyn_form {
                    "run_command"
                } else {
                    "reply_probe"
                };
                let call = RuntimeToolCall {
                    id: "original-call".into(),
                    r#type: "function".into(),
                    function: RuntimeToolCallFunction {
                        name: name.into(),
                        arguments: if dyn_form {
                            serde_json::json!({"command":"dyn reply_probe '{}'"}).to_string()
                        } else {
                            "{}".into()
                        },
                    },
                    thought_signature: None,
                };
                agent
                    .harness
                    .as_mut()
                    .unwrap()
                    .append_message(AgentMessage::Assistant {
                        content: None,
                        tool_calls: Some(vec![call.clone()]),
                        stop_reason: None,
                        deferred_handle: None,
                    })
                    .unwrap();
                agent.set_tool_completion_recorder(Some(Arc::new(|_| {
                    Box::pin(async { Err("injected result commit failure".into()) })
                })));
                let failure = agent
                    .agent
                    .execute_tools(&[call.clone()])
                    .await
                    .unwrap_err();
                assert!(
                    failure.to_string().contains(if committed_state == 3 {
                        "execution task"
                    } else {
                        "result commit failure"
                    }),
                    "{failure}"
                );
                assert_eq!(hooks.load(Ordering::SeqCst), 1);
                assert_eq!(
                    std::fs::read(directory.path().join("effects.log")).unwrap(),
                    b"effect"
                );
                assert_eq!(
                    std::fs::read(directory.path().join("hooks.log")).unwrap(),
                    b"hook"
                );
                let identities = agent
                    .wasi_extensions
                    .pending_tool_reply_identities()
                    .unwrap();
                assert_eq!(identities.len(), 1);
                let identity = &identities[0];
                assert_eq!(identity.run_id, accepted.run_id);
                assert_eq!(identity.tool_call_id, "original-call");
                assert_eq!(identity.tool_name, name);
                let prepared = agent
                    .wasi_extensions
                    .recovered_canonical_reply(identity)
                    .unwrap();
                assert_eq!(prepared.is_some(), committed_state != 3);
                let expected = match prepared {
                    Some(result) => result,
                    None => agent
                        .agent
                        .recover_tool_reply(&call, identity)
                        .await
                        .unwrap()
                        .unwrap(),
                };
                assert!(expected.content.contains("broker-reply"));
                assert_eq!(
                    expected.content.contains("hook reply"),
                    committed_state != 3
                );
                assert_eq!(expected.content.starts_with("Exit Status:"), dyn_form);
                assert!(!expected.is_error);
                if already_committed {
                    let mut committed = expected.clone();
                    if committed_state == 2 {
                        committed.content.push_str("different reply");
                    }
                    agent
                        .harness
                        .as_mut()
                        .unwrap()
                        .record_tool_result(&identity.run_id, &committed)
                        .unwrap();
                }
                drop(agent);
                std::fs::remove_file(
                    directory
                        .path()
                        .join(".threadlane/extensions/reply_probe.wasm"),
                )
                .unwrap();
                let mut recovered = CodingAgent::new(options());
                // The original executor module is absent. Recovery cannot enter its VM.
                if committed_state == 2 {
                    let before = std::fs::read(&path).unwrap();
                    let error = recovered
                        .recover_saved_extension_replies()
                        .await
                        .unwrap_err();
                    assert!(error.contains("replies disagree"), "{error}");
                    assert_eq!(std::fs::read(&path).unwrap(), before);
                    assert_eq!(
                        recovered
                            .wasi_extensions
                            .pending_tool_reply_identities()
                            .unwrap(),
                        identities
                    );
                    assert_eq!(
                        std::fs::read(directory.path().join("effects.log")).unwrap(),
                        b"effect"
                    );
                    assert_eq!(
                        std::fs::read(directory.path().join("hooks.log")).unwrap(),
                        b"hook"
                    );
                    continue;
                }
                assert_eq!(
                    recovered.recover_saved_extension_replies().await.unwrap(),
                    usize::from(!already_committed)
                );
                assert_eq!(
                    recovered.recover_saved_extension_replies().await.unwrap(),
                    0
                );
                assert!(recovered
                    .wasi_extensions
                    .pending_tool_reply_identities()
                    .unwrap()
                    .is_empty());
                assert_eq!(hooks.load(Ordering::SeqCst), 1);
                assert_eq!(
                    std::fs::read(directory.path().join("effects.log")).unwrap(),
                    b"effect"
                );
                assert_eq!(
                    std::fs::read(directory.path().join("hooks.log")).unwrap(),
                    b"hook"
                );
                let store = JsonlStore::open(&path).unwrap();
                let (_, tool) = store
                    .tool_state_for_call(&identity.run_id, "original-call")
                    .unwrap();
                assert!(tool.completed);
                assert_eq!(tool.result_entry_id, identity.result_entry_id);
                let AgentMessage::Tool {
                    content,
                    tool_call_id,
                    name: result_name,
                    ..
                } = &store.entry(&identity.result_entry_id).unwrap().message
                else {
                    panic!("missing canonical result")
                };
                assert_eq!(content, &expected.content);
                assert_eq!(tool_call_id, "original-call");
                assert_eq!(result_name, name);
                assert_eq!(store.records().iter().filter(|record| matches!(record, Record::ToolFinished { tool_call_id, .. } if tool_call_id == "original-call")).count(), 1);
            }
        }
    }

    #[async_trait::async_trait]
    impl ToolExecutor for CommittedIntentProbe {
        fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
            vec![AgentToolDefinition::new(
                "committed_probe",
                "",
                serde_json::json!({"type":"object","properties":{}}),
            )]
            .into()
        }

        async fn execute_tool(&self, _: &str, _: &str) -> Option<Result<String, String>> {
            panic!("canonical dispatch must retain the committed intent")
        }

        async fn execute_tool_with_call(
            &self,
            call: &AgentToolCall,
            args: &str,
            _: Option<&Path>,
            identity: Option<&ToolExecutionIdentity>,
        ) -> Option<Result<ToolOutput, ToolExecutionError>> {
            let identity = identity.expect("durable execution requires a committed intent");
            assert_eq!(call.id, identity.tool_call_id);
            assert_eq!(call.name, "committed_probe");
            assert_eq!(call.arguments, args);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(args).unwrap(),
                serde_json::json!({"x":1})
            );
            // Reopen the real journal at the physical execution boundary.
            let store = JsonlStore::open(&self.path).unwrap();
            assert_eq!(store.session_id(), identity.session_id);
            let (lane, tool) = store
                .tool_state_for_call(&identity.run_id, &call.id)
                .unwrap();
            assert_eq!(lane, identity.lane);
            assert_eq!(tool.assistant_entry_id, identity.assistant_entry_id);
            assert_eq!(tool.tool_name, identity.tool_name);
            assert_eq!(tool.result_entry_id, identity.result_entry_id);
            assert!(!tool.completed);
            self.observed.lock().unwrap().push(identity.clone());
            Some(Ok(ToolOutput {
                content: "committed reply".into(),
                images: vec![ImageAttachment {
                    display_name: "fixture.png".into(),
                    data_url: "data:image/png;base64,AA==".into(),
                }],
            }))
        }
    }

    #[tokio::test]
    async fn execution_identity_matches_committed_intent_and_result_across_runs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let mut agent = CodingAgent::new(CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "test-model".into(),
            work_dir: directory.path().to_owned(),
            session_file: Some(path.clone()),
            system_prompt: SystemPromptConfig::default(),
            agent_config: None,
            coding_config: None,
            browser: BrowserBridge::unavailable(),
        });
        let observed = Arc::new(Mutex::new(Vec::new()));
        agent
            .agent
            .register_tool_executor(Arc::new(CommittedIntentProbe {
                path: path.clone(),
                observed: observed.clone(),
            }))
            .unwrap();
        agent.agent.set_allowed_tool_names(None);
        for dyn_form in [false, true] {
            let accepted = agent
                .begin_harness_run(AgentMessage::user("probe", vec![]))
                .await
                .unwrap()
                .unwrap();
            let name = if dyn_form {
                "run_command"
            } else {
                "committed_probe"
            };
            let arguments = if dyn_form {
                serde_json::json!({"command":"dyn committed_probe '{\"x\":1}'"}).to_string()
            } else {
                r#"{"x":1}"#.into()
            };
            let call = RuntimeToolCall {
                id: "reused-call".into(),
                r#type: "function".into(),
                function: RuntimeToolCallFunction {
                    name: name.into(),
                    arguments: arguments.clone(),
                },
                thought_signature: None,
            };
            let assistant = agent
                .harness
                .as_mut()
                .unwrap()
                .append_message(AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![call.clone()]),
                    stop_reason: None,
                    deferred_handle: None,
                })
                .unwrap();
            let results = agent.agent.execute_tools(&[call]).await.unwrap();
            assert!(!results[0].is_error, "{}", results[0].content);
            assert_eq!(results[0].tool_call_id, "reused-call");
            assert_eq!(results[0].name, name);
            assert_eq!(results[0].images.len(), 1);
            assert!(results[0].content.contains("committed reply"));
            agent
                .finish_harness_run(Some(&accepted.run_id), OperationOutcome::Completed, None)
                .await
                .unwrap();
            let store = JsonlStore::open(&path).unwrap();
            let identity = observed.lock().unwrap().last().unwrap().clone();
            assert_eq!(identity.run_id, accepted.run_id);
            assert_eq!(identity.assistant_entry_id, assistant);
            assert_eq!(identity.tool_name, name);
            let (lane, tool) = store
                .tool_state_for_call(&accepted.run_id, "reused-call")
                .unwrap();
            assert_eq!(lane, "main");
            assert!(tool.completed);
            let entry = store.entry(&identity.result_entry_id).unwrap();
            assert!(
                matches!(&entry.message, AgentMessage::Tool { tool_call_id, name: result_name, content, images, .. }
                if tool_call_id == "reused-call" && result_name == name && content == &results[0].content && images == &results[0].images)
            );
            let intents = store
                .records()
                .iter()
                .filter_map(|record| match record {
                    Record::ToolStarted {
                        run_id,
                        tool_call_id,
                        tool_name,
                        effective_args,
                        ..
                    } if run_id == &accepted.run_id => {
                        Some((tool_call_id, tool_name, effective_args))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(intents.len(), 1);
            assert_eq!(intents[0].0, "reused-call");
            assert_eq!(intents[0].1, name);
            let mut expected_arguments =
                serde_json::from_str::<serde_json::Value>(&arguments).unwrap();
            if dyn_form {
                expected_arguments["cwd"] = directory.path().to_string_lossy().into_owned().into();
            }
            assert_eq!(intents[0].2, &expected_arguments);
        }
        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].session_id, observed[1].session_id);
        assert_ne!(observed[0].run_id, observed[1].run_id);
        assert_ne!(
            observed[0].assistant_entry_id,
            observed[1].assistant_entry_id
        );
        assert_ne!(observed[0].result_entry_id, observed[1].result_entry_id);
    }
}

#[cfg(test)]
mod compaction_sync_tests {
    use super::{
        durable_prompt_snapshot, requires_harness_compaction_reset, CodingAgent,
        CodingAgentOptions, CompletedSubagentLane, SubagentLaneStatus,
        MAX_PERSISTED_SYSTEM_PROMPT_BYTES,
    };
    use async_trait::async_trait;
    use std::{
        collections::HashSet,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };
    use threadlane_prompt::SystemPromptConfig;
    use threadlane_protocol::browser::BrowserBridge;
    use threadlane_protocol::{AgentMessage, AgentToolResult};
    use threadlane_protocol::OrchestratorMode;
    use threadlane_protocol::{
        DeferredResponse, ProviderPort, RuntimeRequest, RuntimeStreamEvent, RuntimeToolCall,
        RuntimeToolCallFunction, RuntimeUsage,
    };
    use threadlane_runtime::{
        harness::{
            read_transcript_page, CompactionReason, JsonlStore, OperationOutcome, SessionStore,
            TranscriptItem,
        },
        Record,
    };

    fn summary() -> AgentMessage {
        AgentMessage::Custom {
            custom_type: "compaction_summary".into(),
            payload: serde_json::json!({"summary": "older context"}),
        }
    }

    #[derive(Clone, Default)]
    struct RecordingProvider {
        refreshed: Arc<Mutex<Vec<(String, Option<String>)>>>,
        block_once: Arc<Mutex<Option<Arc<tokio::sync::Notify>>>>,
        system_prompts: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ProviderPort for RecordingProvider {
        async fn stream_request(
            &self,
            request: RuntimeRequest,
            events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
        ) {
            let messages: Vec<AgentMessage> = serde_json::from_value(request.messages).unwrap();
            if let Some(prompt) = messages.into_iter().find_map(|message| match message {
                AgentMessage::System { content } => Some(content),
                _ => None,
            }) {
                self.system_prompts.lock().unwrap().push(prompt);
            }
            let started = self.block_once.lock().unwrap().take();
            if let Some(started) = started {
                started.notify_one();
                std::future::pending::<()>().await;
            }
            let _ = events
                .send(RuntimeStreamEvent::ContentToken("done".into()))
                .await;
            let _ = events
                .send(RuntimeStreamEvent::Finished {
                    tool_calls: vec![],
                    usage: RuntimeUsage::default(),
                })
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

        fn refresh_openai_credentials(&self, key: String, account: Option<String>) {
            self.refreshed.lock().unwrap().push((key, account));
        }

        fn provider_kind(&self, _model: &str) -> &'static str {
            "test"
        }
    }

    #[tokio::test]
    async fn set_model_rotates_provider_credentials_across_providers() {
        // Reproduces the mid-task 401: a session constructed with a Google
        // `ya29` token that switches to an OpenAI model must stop signing
        // with the stale key. Assertions use the same resolver production
        // uses, so they hold with or without stored credentials.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let refreshed = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(RecordingProvider {
            refreshed: refreshed.clone(),
            ..Default::default()
        });
        let mut agent = CodingAgent::new_with_provider(
            CodingAgentOptions {
                api_key: "ya29.stale-google-token".into(),
                account_id: None,
                model: "antigravity/gemini-3.7-flash".into(),
                work_dir: dir.path().to_path_buf(),
                session_file: Some(path),
                system_prompt: SystemPromptConfig::default(),
                agent_config: None,
                coding_config: None,
                browser: BrowserBridge::unavailable(),
            },
            provider,
        );
        let (openai_key, openai_account) = crate::credentials::provider_credentials("gpt-4o");
        agent.set_model("gpt-4o".into()).await.unwrap();
        if openai_key.trim().is_empty() {
            // Credential-less contexts keep legacy behavior: untouched.
            assert_eq!(agent.agent.api_key, "ya29.stale-google-token");
            assert!(refreshed.lock().unwrap().is_empty());
        } else {
            assert_eq!(agent.agent.api_key, openai_key);
            assert_eq!(
                *refreshed.lock().unwrap(),
                vec![(openai_key, openai_account)]
            );
        }
        // Switching to a non-OpenAI model never touches the shared cell.
        let before = refreshed.lock().unwrap().len();
        let (ag_key, _) = crate::credentials::provider_credentials("antigravity/gemini-3.1-pro");
        agent
            .set_model("antigravity/gemini-3.1-pro".into())
            .await
            .unwrap();
        assert_eq!(refreshed.lock().unwrap().len(), before);
        if !ag_key.trim().is_empty() {
            assert_eq!(agent.agent.api_key, ag_key);
        }
    }

    #[tokio::test]
    async fn fusion_router_and_compaction_model_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fusion.jsonl");
        let mut config = threadlane_runtime::AgentConfig::default();
        config.orchestrator_mode = OrchestratorMode::Fusion;
        config.model_roles.fast = Some("side".into());
        let options = |config: threadlane_runtime::AgentConfig| CodingAgentOptions {
            api_key: "test".into(),
            account_id: None,
            model: "main".into(),
            work_dir: dir.path().to_path_buf(),
            session_file: Some(path.clone()),
            system_prompt: SystemPromptConfig::default(),
            agent_config: Some(config),
            coding_config: None,
            browser: BrowserBridge::unavailable(),
        };
        let provider = Arc::new(RecordingProvider::default());
        let mut first = CodingAgent::new_with_provider(options(config.clone()), provider.clone());
        first.arm_fusion("Remove deprecated code").await.unwrap();
        let state = {
            let mut guard = first.fusion.lock().unwrap();
            let state = guard.as_mut().unwrap();
            for _ in 0..3 {
                state.record_delegation();
                state.record_sidekick_result(false);
            }
            state.clone()
        };
        first
            .set_fact("fusion_state", &serde_json::to_string(&state).unwrap())
            .unwrap();
        first.apply_fusion_compaction_routing().await;
        assert_eq!(first.agent.model(), "main");
        // Simulate a session persisted by the older main-lane downgrade rule.
        first.set_fact("model", "side").unwrap();
        drop(first);

        let mut resumed = CodingAgent::new_with_provider(options(config.clone()), provider.clone());
        assert_eq!(resumed.agent.model(), "main");
        assert_eq!(JsonlStore::open_read_only(&path).unwrap().facts()["model"], "main");
        let state = resumed.fusion.lock().unwrap().clone().unwrap();
        assert_eq!(state.delegated, 3);
        assert_eq!(state.compaction_generation, 1);
        assert!(resumed.fusion_directive().is_some());
        let facts = JsonlStore::open_read_only(&path).unwrap().facts().clone();
        assert!(facts.keys().any(|key| key.starts_with("fusion_audit:")));

        let state = {
            let mut guard = resumed.fusion.lock().unwrap();
            let state = guard.as_mut().unwrap();
            state.record_sidekick_result(true);
            state.record_sidekick_result(true);
            state.clone()
        };
        resumed
            .set_fact("fusion_state", &serde_json::to_string(&state).unwrap())
            .unwrap();
        resumed.apply_fusion_compaction_routing().await;
        assert_eq!(resumed.agent.model(), "main");
        drop(resumed);
        let mut recovered = CodingAgent::new_with_provider(options(config.clone()), provider);
        let state = recovered.fusion.lock().unwrap().clone().unwrap();
        assert_eq!(state.escalated, 1);
        assert_eq!(state.consecutive_sidekick_errors, 0);
        assert_eq!(state.compaction_generation, 2);
        recovered.set_model("new-main".into()).await.unwrap();
        assert!(recovered.fusion.lock().unwrap().is_none());
        assert_eq!(
            JsonlStore::open_read_only(&path).unwrap().facts()["fusion_state"],
            ""
        );

        config.model_roles.fast = Some("different".into());
        let stale = CodingAgent::new_with_provider(options(config), Arc::new(RecordingProvider::default()));
        assert!(stale.fusion.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn fusion_handoff_contract_is_added_once_to_a_custom_base() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = threadlane_runtime::AgentConfig::default();
        config.orchestrator_mode = OrchestratorMode::Fusion;
        config.model_roles.fast = Some("side".into());
        let provider = Arc::new(RecordingProvider::default());
        let mut agent = CodingAgent::new_with_provider(
            CodingAgentOptions {
                api_key: "test".into(),
                account_id: None,
                model: "main".into(),
                work_dir: dir.path().to_path_buf(),
                session_file: Some(dir.path().join("custom-fusion.jsonl")),
                system_prompt: SystemPromptConfig::default(),
                agent_config: Some(config),
                coding_config: None,
                browser: BrowserBridge::unavailable(),
            },
            provider.clone(),
        );
        agent.agent.turn.lock().await.system_prompt = "Custom base without default guidelines".into();
        for _ in 0..2 {
            assert!(agent.handle_input_with_images("Inspect the helper.", vec![]).await.is_none());
        }
        let prompts = provider.system_prompts.lock().unwrap();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0], prompts[1]);
        assert!(prompts[0].starts_with("Custom base without default guidelines"));
        assert_eq!(prompts[0].matches(threadlane_prompt::workflow::IMPLEMENTATION_HANDOFF).count(), 1);
    }

    #[tokio::test]
    async fn fusion_directive_stays_stable_while_route_audit_tracks_each_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fusion-stable-directive.jsonl");
        let mut config = threadlane_runtime::AgentConfig::default();
        config.orchestrator_mode = OrchestratorMode::Fusion;
        config.model_roles.fast = Some("side".into());
        let provider = Arc::new(RecordingProvider::default());
        let mut agent = CodingAgent::new_with_provider(
            CodingAgentOptions {
                api_key: "test".into(),
                account_id: None,
                model: "main".into(),
                work_dir: dir.path().to_path_buf(),
                session_file: Some(path.clone()),
                system_prompt: SystemPromptConfig::default(),
                agent_config: Some(config),
                coding_config: None,
                browser: BrowserBridge::unavailable(),
            },
            provider.clone(),
        );
        let prompts = [
            "Rename the local variable and remove the unused helper.",
            "Which approach best balances security and usability? Explain the tradeoff.",
        ];
        for prompt in prompts {
            let result = agent.handle_input_with_images(prompt, vec![]).await;
            assert!(result.is_none(), "prompt failed: {result:?}");
        }

        let system_prompts = provider.system_prompts.lock().unwrap().clone();
        assert_eq!(system_prompts.len(), 2);
        assert_eq!(system_prompts[0].as_bytes(), system_prompts[1].as_bytes());
        assert_eq!(
            system_prompts[0].matches(threadlane_prompt::workflow::IMPLEMENTATION_HANDOFF).count(),
            1
        );
        assert_eq!(
            system_prompts[0]
                .matches(threadlane_orchestrator::FUSION_MAIN_HEADER)
                .count(),
            1
        );
        assert_eq!(
            system_prompts[0]
                .matches(threadlane_orchestrator::FUSION_MAIN_FOOTER)
                .count(),
            1
        );

        let store = JsonlStore::open_read_only(&path).unwrap();
        let mut actual = store
            .records()
            .iter()
            .filter_map(|record| match record {
                Record::FactSet {
                    seq, key, value, ..
                } if key.as_str().starts_with("fusion_audit:") => {
                    let audit: serde_json::Value = serde_json::from_str(value).ok()?;
                    (audit.get("kind")?.as_str()? == "route").then(|| {
                        let codes: Vec<String> = audit
                            .get("reason_codes")
                            .and_then(serde_json::Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_owned)
                            .collect();
                        (*seq, codes)
                    })
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        actual.sort_by_key(|(seq, _)| *seq);
        let expected = prompts
            .iter()
            .map(|prompt| {
                threadlane_orchestrator::classify_fusion_prompt(prompt)
                    .reason_codes
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual
                .into_iter()
                .map(|(_, codes)| codes)
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[tokio::test]
    async fn fusion_replays_child_completion_after_torn_state_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fusion-replay.jsonl");
        let mut config = threadlane_runtime::AgentConfig::default();
        config.orchestrator_mode = OrchestratorMode::Fusion;
        config.model_roles.fast = Some("side".into());
        let options = || CodingAgentOptions {
            api_key: "test".into(),
            account_id: None,
            model: "main".into(),
            work_dir: dir.path().to_path_buf(),
            session_file: Some(path.clone()),
            system_prompt: SystemPromptConfig::default(),
            agent_config: Some(config.clone()),
            coding_config: None,
            browser: BrowserBridge::unavailable(),
        };
        let provider = Arc::new(RecordingProvider::default());
        let mut first = CodingAgent::new_with_provider(options(), provider.clone());
        first.arm_fusion("Remove deprecated code").await.unwrap();
        drop(first);
        let mut journal = crate::harness::CodingSessionHarness::open(&path).unwrap();
        let child = journal.start_subagent_lane("worker", "remove code", None).unwrap();
        journal
            .append_message_to_lane(
                &child.identity.lane_name,
                &child.identity.run_id,
                AgentMessage::Assistant {
                    content: Some("done".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
            )
            .unwrap();
        journal
            .finish_subagent_lane(
                &child.identity.lane_name,
                &child.identity.run_id,
                OperationOutcome::Failed,
                Some("verification failed".into()),
            )
            .unwrap();
        drop(journal);

        let resumed = CodingAgent::new_with_provider(options(), provider);
        let state = resumed.fusion.lock().unwrap().clone().unwrap();
        assert_eq!(state.delegated, 1);
        assert_eq!(state.consecutive_sidekick_errors, 1);
        assert_eq!(
            serde_json::from_str::<threadlane_orchestrator::FusionState>(
                &JsonlStore::open_read_only(&path).unwrap().facts()["fusion_state"]
            )
            .unwrap()
            .delegated,
            1
        );
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
    async fn coding_agent_exposes_durable_event_subscription() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut agent = CodingAgent::new(CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "test-model".into(),
            work_dir: dir.path().to_path_buf(),
            session_file: Some(path),
            system_prompt: SystemPromptConfig::default(),
            agent_config: None,
            coding_config: None,
            browser: BrowserBridge::unavailable(),
        });
        let mut subscription = agent.subscribe_durable_events().unwrap();
        let harness = agent.harness.as_mut().unwrap();
        harness
            .begin_run("runtime-events", AgentMessage::user("prompt", vec![]))
            .unwrap();
        let events = agent.wait_durable_events(&mut subscription).await.unwrap();
        assert!(!events.is_empty());
        assert!(agent
            .poll_durable_events(&mut subscription)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn agent_mode_has_no_subagent_tools() {
        let dir = tempfile::tempdir().unwrap();
        for (mode, expected) in [
            (threadlane_protocol::OrchestratorMode::Normal, false),
            (threadlane_protocol::OrchestratorMode::Fusion, true),
        ] {
            let mut config = threadlane_runtime::AgentConfig::default();
            config.orchestrator_mode = mode;
            let agent = CodingAgent::new(CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "test-model".into(),
                work_dir: dir.path().to_path_buf(),
                session_file: None,
                system_prompt: SystemPromptConfig::default(),
                agent_config: Some(config),
                coding_config: None,
                browser: BrowserBridge::unavailable(),
            });
            let names: Vec<_> = agent
                .agent
                .configured_tool_definitions()
                .into_iter()
                .map(|tool| tool.name)
                .collect();
            assert_eq!(names.iter().any(|name| name == "subagent"), expected);
            assert_eq!(names.iter().any(|name| name == "hub"), expected);
        }
    }

    #[tokio::test]
    async fn manual_compaction_preserves_summary_and_structured_snapshot_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(dir.path().join("README.md"), "snapshot body").unwrap();
        let mut agent = CodingAgent::new(CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "test-model".into(),
            work_dir: dir.path().to_path_buf(),
            session_file: Some(path.clone()),
            system_prompt: SystemPromptConfig::default(),
            agent_config: None,
            coding_config: None,
            browser: BrowserBridge::unavailable(),
        });
        let harness = agent.harness.as_mut().unwrap();
        let run_id = harness.unique_run_id("snapshot").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![RuntimeToolCall {
                    id: "read-1".into(),
                    r#type: "function".into(),
                    function: RuntimeToolCallFunction {
                        name: "read_file".into(),
                        arguments: r#"{\"path\":\"README.md\",\"start_line\":1,\"end_line\":1}"#
                            .into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        harness
            .append_tool_intent(
                &run_id,
                "read-1",
                "read_file",
                serde_json::json!({"path": "README.md", "start_line": 1, "end_line": 1}),
            )
            .await
            .unwrap();
        let read_output = threadlane_tools::try_execute_tool_in_workspace(
            "read_file",
            r#"{"path":"README.md","start_line":1,"end_line":1}"#,
            dir.path(),
        )
        .unwrap();
        harness
            .record_tool_result(
                &run_id,
                &AgentToolResult::external("read-1", "read_file", &read_output, false),
            )
            .unwrap();
        let source_entry_id = harness.store.entries().last().unwrap().id.clone();
        let context_id = harness
            .index_read_snapshot(
                &run_id,
                dir.path(),
                "read-1",
                &source_entry_id,
                read_output.chars().count(),
            )
            .unwrap()
            .unwrap();
        harness
            .finish_run(&run_id, OperationOutcome::Completed, None)
            .unwrap();

        let summary = "Keep this user-authored summary exactly.";
        agent
            .persist_harness_compaction(summary, &[], 100, 2)
            .unwrap();

        let store = JsonlStore::open(&path).unwrap();
        let checkpoint = store
            .model_context("main")
            .unwrap()
            .messages()
            .into_iter()
            .find(|message| threadlane_compaction::compaction_summary_text(message).is_some())
            .unwrap();
        assert_eq!(
            threadlane_compaction::compaction_summary_text(&checkpoint),
            Some(summary)
        );
        let AgentMessage::Custom { payload, .. } = checkpoint else {
            unreachable!();
        };
        let index = payload["context_snapshot_index"]
            .as_array()
            .expect("structured snapshot index");
        assert_eq!(index.len(), 1);
        assert_eq!(index[0]["context_id"], context_id);
        assert_eq!(index[0]["path"], "README.md");
        assert_eq!(index[0]["start_line"], 1);
        assert_eq!(index[0]["end_line"], 1);
        assert!(index[0]["file_sha256"].is_string());
        assert!(!payload.to_string().contains("snapshot body"));
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
            browser: BrowserBridge::unavailable(),
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
                escalation_reason: None,
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
        request_estimates: Mutex<Vec<usize>>,
        previous_serialized_request: Mutex<Option<String>>,
        use_cache: bool,
    }

    #[derive(Default)]
    struct ReadContextProvider {
        requests: Mutex<Vec<Vec<AgentMessage>>>,
    }

    struct ProjectMemoryProvider {
        requests: Mutex<Vec<Vec<AgentMessage>>>,
        update: Option<String>,
    }

    #[async_trait]
    impl ProviderPort for ProjectMemoryProvider {
        async fn stream_request(
            &self,
            request: RuntimeRequest,
            events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
        ) {
            let attempt = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(serde_json::from_value(request.messages).unwrap());
                requests.len()
            };
            let call = self.update.as_ref().and_then(|update| match attempt {
                1 => Some(("manage_memory", update.clone())),
                2 => Some((
                    "write_file",
                    serde_json::json!({"path":"parser.rs", "content":"changed source"}).to_string(),
                )),
                _ => None,
            });
            let tool_calls = call
                .map(|(name, arguments)| {
                    vec![RuntimeToolCall {
                        id: format!("memory-{attempt}"),
                        r#type: "function".into(),
                        function: RuntimeToolCallFunction {
                            name: name.into(),
                            arguments,
                        },
                        thought_signature: None,
                    }]
                })
                .unwrap_or_default();
            if tool_calls.is_empty() {
                events
                    .send(RuntimeStreamEvent::ContentToken("done".into()))
                    .await
                    .unwrap();
            }
            events
                .send(RuntimeStreamEvent::Finished {
                    tool_calls,
                    usage: RuntimeUsage::default(),
                })
                .await
                .unwrap();
        }
        async fn fetch_deferred(&self, _: &str, _: &str) -> Result<DeferredResponse, String> {
            Ok(DeferredResponse::Pending)
        }
        async fn cancel_deferred(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn provider_kind(&self, _: &str) -> &'static str {
            "test"
        }
    }

    #[tokio::test]
    async fn failed_tool_result_commit_stops_provider_and_preserves_unfinished_intent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(directory.path().join("proof.rs"), "evidence").unwrap();
        let provider = Arc::new(ProjectMemoryProvider {
            requests: Mutex::new(Vec::new()),
            update: Some(serde_json::json!({"action":"remember", "key":"commit-proof", "content":"Physical tool execution happened.", "sources":[{"path":"proof.rs", "sha256":crate::durable::sha256_hex(b"evidence")}]}).to_string()),
        });
        let mut agent = CodingAgent::new_with_provider(
            CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "test-model".into(),
                work_dir: directory.path().to_owned(),
                session_file: Some(path.clone()),
                system_prompt: SystemPromptConfig::default(),
                agent_config: None,
                coding_config: None,
                browser: BrowserBridge::unavailable(),
            },
            provider.clone(),
        );
        let mut events = agent.subscribe();
        let accepted = agent
            .begin_harness_run(AgentMessage::user("Remember a fact", vec![]))
            .await
            .unwrap()
            .unwrap();
        agent.agent.tool_dispatcher.tool_completion_recorder = Some(Arc::new(|result| {
            assert!(!result.is_error, "{}", result.content);
            Box::pin(async { Err("injected result journal failure".into()) })
        }));
        agent.execute_accepted_run(&accepted).await.unwrap();
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
        assert!(directory.path().join(".threadlane/memory.json").exists());
        assert!(!agent
            .agent
            .messages()
            .await
            .iter()
            .any(|message| matches!(message, AgentMessage::Tool { .. })));
        let mut failures = 0;
        while let Ok(event) = events.try_recv() {
            match event {
                threadlane_protocol::AgentEvent::AgentError { error } => {
                    assert!(error.contains("injected result journal failure"), "{error}");
                    failures += 1;
                }
                threadlane_protocol::AgentEvent::ToolExecutionEnd { .. }
                | threadlane_protocol::AgentEvent::TurnEnd { .. } => {
                    panic!("uncommitted completion was published")
                }
                _ => {}
            }
        }
        assert_eq!(failures, 1);
        drop(agent);
        let store = JsonlStore::open_read_only(&path).unwrap();
        let (_, tool) = store
            .tool_state_for_call(&accepted.run_id, "memory-1")
            .unwrap();
        assert!(!tool.completed);
        assert!(store.entry(&tool.result_entry_id).is_none());
        assert_eq!(
            store.open_operation_lane(&accepted.run_id).unwrap().0,
            "main"
        );
        assert!(!store
            .entries()
            .iter()
            .any(|entry| matches!(entry.message, AgentMessage::Tool { .. })));
        assert!(!store.records().iter().any(|record| matches!(record, Record::ToolFinished { run_id, .. } if run_id == &accepted.run_id)));
    }

    fn memory_messages(messages: &[AgentMessage]) -> Vec<&str> {
        messages
            .iter()
            .filter_map(|message| match message {
                AgentMessage::User { content }
                    if content.starts_with("<threadlane-project-memory>") =>
                {
                    Some(content.as_str())
                }
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn project_memory_refreshes_each_request_without_entering_durable_history() {
        for checkpoint in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let path = root.join("session.jsonl");
            std::fs::write(root.join("parser.rs"), "original source").unwrap();
            let digest = crate::durable::sha256_hex(b"original source");
            let mut note = serde_json::json!({"action":"remember", "key":"parser-storage", "content":"Parser owns initial storage.",
                "sources":[{"path":"parser.rs", "sha256":digest}]});
            threadlane_tools::try_execute_tool_in_workspace("manage_memory", &note.to_string(), root)
                .unwrap();
            note["content"] = serde_json::json!("Parser owns updated storage.");
            let provider = Arc::new(ProjectMemoryProvider {
                requests: Mutex::new(Vec::new()),
                update: Some(note.to_string()),
            });
            let options = |root: &std::path::Path, file: std::path::PathBuf| CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "test-model".into(),
                work_dir: root.into(),
                session_file: Some(file),
                system_prompt: SystemPromptConfig::default(),
                agent_config: None,
                coding_config: None,
                browser: BrowserBridge::unavailable(),
            };
            let mut agent =
                CodingAgent::new_with_provider(options(root, path.clone()), provider.clone());
            if checkpoint {
                agent
                    .persist_harness_compaction("Earlier task context", &[], 0, 0)
                    .unwrap();
            }
            assert!(agent
                .handle_input_with_images("Inspect parser storage", vec![])
                .await
                .is_none());
            {
                let requests = provider.requests.lock().unwrap();
                assert_eq!(requests.len(), 3);
                assert!(memory_messages(&requests[0])[0].contains("initial storage"));
                assert!(memory_messages(&requests[1])[0].contains("updated storage"));
                assert_eq!(
                    memory_messages(&requests[1]).len(),
                    1,
                    "recall must not accumulate"
                );
                assert!(
                    memory_messages(&requests[2]).is_empty(),
                    "changed evidence is stale"
                );
            }
            assert!(memory_messages(&agent.agent.messages().await).is_empty());
            drop(agent);
            let store = JsonlStore::open_read_only(&path).unwrap();
            assert!(memory_messages(&store.model_context("main").unwrap().messages()).is_empty());
            let manifests: Vec<_> = store
                .records()
                .iter()
                .filter_map(|record| match record {
                    Record::ContextManifestCaptured {
                        items,
                        total_estimated_tokens,
                        context_limit,
                        ..
                    } => {
                        assert!((total_estimated_tokens.unwrap() as usize) < context_limit.unwrap());
                        let items = serde_json::to_value(items).unwrap();
                        Some(
                            items
                                .as_array()
                                .unwrap()
                                .iter()
                                .filter(|item| item["label"] == "project memory recall")
                                .count(),
                        )
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(manifests, vec![1, 1, 0]);
            assert!(store.model_context("main").unwrap().messages().iter().any(|message| matches!(message,
                AgentMessage::Tool { name, content, is_error:false, .. } if name == "manage_memory" && content.contains("remembered"))));

            std::fs::write(root.join("parser.rs"), "original source").unwrap();
            let next_provider = Arc::new(ProjectMemoryProvider {
                requests: Mutex::new(Vec::new()),
                update: None,
            });
            let mut next = CodingAgent::new_with_provider(
                options(root, root.join("next.jsonl")),
                next_provider.clone(),
            );
            assert!(next
                .handle_input_with_images("Inspect parser storage", vec![])
                .await
                .is_none());
            assert!(
                memory_messages(&next_provider.requests.lock().unwrap()[0])[0]
                    .contains("updated storage")
            );
            assert!(next
                .handle_input_with_images("Continue", vec![])
                .await
                .is_none());
            assert_eq!(
                memory_messages(&next_provider.requests.lock().unwrap()[1]).len(),
                1
            );

            // Child agents use AgentRuntime directly and can have narrow tool
            // whitelists. Exercise that same shared request seam without adding
            // manage_memory to their allowed tools.
            let child_provider = Arc::new(ProjectMemoryProvider {
                requests: Mutex::new(Vec::new()),
                update: None,
            });
            let child_path = root.join("child.jsonl");
            let mut child_harness =
                threadlane_runtime::harness::AgentHarness::new(JsonlStore::open(&child_path).unwrap());
            let accepted = child_harness
                .accept_prompt_and_drive_on_lane(
                    "subagent-worker",
                    "child-run",
                    AgentMessage::user("Inspect parser storage", Vec::new()),
                )
                .unwrap();
            drop(child_harness);
            let mut child = threadlane_runtime::AgentRuntime::new_with_provider(
                "test-key",
                None,
                "test-model",
                Some(&child_path),
                threadlane_runtime::AgentConfig::default(),
                child_provider.clone(),
            )
            .unwrap();
            child.work_dir = Some(root.into());
            child.turn.lock().await.project_root = Some(root.into());
            child.set_allowed_tool_names(Some(["read_file".to_string()].into_iter().collect()));
            child
                .sync_turn_from_model_context_on_lane("subagent-worker")
                .await
                .unwrap();
            child
                .run_accepted(
                    "child-run",
                    "subagent-worker",
                    accepted.accepted_through_seq,
                )
                .await;
            assert_eq!(
                memory_messages(&child_provider.requests.lock().unwrap()[0]).len(),
                1
            );
            assert!(memory_messages(&child.messages().await).is_empty());

            let other = tempfile::tempdir().unwrap();
            let other_provider = Arc::new(ProjectMemoryProvider {
                requests: Mutex::new(Vec::new()),
                update: None,
            });
            let mut unrelated = CodingAgent::new_with_provider(
                options(other.path(), other.path().join("session.jsonl")),
                other_provider.clone(),
            );
            assert!(unrelated
                .handle_input_with_images("Inspect parser storage", vec![])
                .await
                .is_none());
            assert!(memory_messages(&other_provider.requests.lock().unwrap()[0]).is_empty());
            assert!(!other.path().join(".threadlane/memory.json").exists());
        }
    }

    #[async_trait]
    impl ProviderPort for ReadContextProvider {
        async fn stream_request(
            &self,
            request: RuntimeRequest,
            events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
        ) {
            let messages: Vec<AgentMessage> = serde_json::from_value(request.messages).unwrap();
            let attempt = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(messages);
                requests.len()
            };
            let paths = ["a.rs", "a.rs", "b.rs", "c.rs", "d.rs", "a.rs"];
            let calls = paths
                .get(attempt - 1)
                .map(|path| {
                    vec![RuntimeToolCall {
                        id: format!("read-{attempt}"),
                        r#type: "function".into(),
                        function: RuntimeToolCallFunction {
                            name: "read_file".into(),
                            arguments: serde_json::json!({"path": path}).to_string(),
                        },
                        thought_signature: None,
                    }]
                })
                .unwrap_or_default();
            if calls.is_empty() {
                events
                    .send(RuntimeStreamEvent::ContentToken(
                        "inspection complete".into(),
                    ))
                    .await
                    .unwrap();
            }
            events
                .send(RuntimeStreamEvent::Finished {
                    tool_calls: calls,
                    usage: RuntimeUsage {
                        input_tokens: 100,
                        output_tokens: 10,
                        cache_read_tokens: 50,
                        cache_write_tokens: 0,
                        total_tokens: 160,
                    },
                })
                .await
                .unwrap();
        }
        async fn fetch_deferred(&self, _: &str, _: &str) -> Result<DeferredResponse, String> {
            Ok(DeferredResponse::Pending)
        }
        async fn cancel_deferred(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn provider_kind(&self, _: &str) -> &'static str {
            "test"
        }
    }

    #[tokio::test]
    async fn read_context_reduces_provider_tokens_without_changing_durable_results() {
        for prior_checkpoint in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("session.jsonl");
            for name in ["a.rs", "b.rs", "c.rs", "d.rs"] {
                std::fs::write(
                    dir.path().join(name),
                    format!("{name} {}", "body ".repeat(1400)),
                )
                .unwrap();
            }
            let provider = Arc::new(ReadContextProvider::default());
            let mut agent = CodingAgent::new_with_provider(
                CodingAgentOptions {
                    api_key: "test-key".into(),
                    account_id: None,
                    model: "test-model".into(),
                    work_dir: dir.path().into(),
                    session_file: Some(path.clone()),
                    system_prompt: SystemPromptConfig::default(),
                    agent_config: None,
                    coding_config: None,
                    browser: BrowserBridge::unavailable(),
                },
                provider.clone(),
            );
            if prior_checkpoint {
                agent
                    .persist_harness_compaction("Earlier task context", &[], 0, 0)
                    .unwrap();
            }
            let result = agent
                .handle_input_with_images("Inspect these files; preserve public APIs", vec![])
                .await;
            assert!(result.is_none(), "{result:?}");
            let requests = provider.requests.lock().unwrap();
            assert_eq!(requests.len(), 7);
            let contents = |messages: &[AgentMessage], id: &str| {
                messages
                    .iter()
                    .find_map(|message| match message {
                        AgentMessage::Tool {
                            tool_call_id,
                            content,
                            ..
                        } if tool_call_id == id => Some(content.clone()),
                        _ => None,
                    })
                    .unwrap()
            };
            assert!(contents(&requests[2], "read-1").contains("body body"));
            assert!(contents(&requests[2], "read-2").contains("Unchanged read"));
            assert!(contents(&requests[5], "read-1").contains("manage_context"));
            // When a new read makes this file recent again, restore a full copy
            // before emitting duplicate references. Never reference an evicted body.
            assert!(contents(&requests[6], "read-1").contains("body body"));
            assert!(contents(&requests[6], "read-6").contains("Unchanged read"));
            drop(requests);
            drop(agent);
            let store = JsonlStore::open_read_only(&path).unwrap();
            let canonical = store.model_context("main").unwrap().messages();
            for id in ["read-1", "read-2", "read-6"] {
                assert!(contents(&canonical, id).contains("body body"));
            }
            let requests = provider.requests.lock().unwrap();
            let full_read_bytes: usize = (1..=5)
                .map(|index| contents(&canonical, &format!("read-{index}")).len())
                .sum();
            let sent_read_bytes: usize = (1..=5)
                .map(|index| contents(&requests[5], &format!("read-{index}")).len())
                .sum();
            assert!(sent_read_bytes * 100 < full_read_bytes * 70,
            "expected >30% reduction in this fixture: sent={sent_read_bytes}, full={full_read_bytes}");
            drop(requests);
            assert_eq!(
                store
                    .records()
                    .iter()
                    .filter(|record| matches!(record, Record::ContextCompacted { .. }))
                    .count(),
                usize::from(prior_checkpoint),
                "request-only reduction must not manufacture compaction during reconciliation"
            );
            let report = threadlane_runtime::harness::project_token_efficiency(&store);
            assert_eq!(report.completed_foreground_runs, 1);
            assert_eq!(report.usage.uncached_input_tokens, 700);
            assert_eq!(report.usage.cache_read_tokens, 350);
            assert_eq!(report.usage.output_tokens, 70);
            assert_eq!(report.lanes["main"].provider_requests, 7);
            assert!(report.lanes["main"].reduced_context_items > 0);
            assert_eq!(report.calibrated_requests, 7);
            assert_eq!(report.repeated_snapshot_reads, 2);
            let snapshot = store
                .records()
                .iter()
                .find_map(|record| match record {
                    Record::ContextSnapshotIndexed { snapshot, .. }
                        if snapshot.source_tool_call_id == "read-1" =>
                    {
                        Some(snapshot.context_id.clone())
                    }
                    _ => None,
                })
                .unwrap();
            let loaded =
                crate::context_snapshots::resolve_context_snapshot(&path, dir.path(), &snapshot)
                    .unwrap();
            assert!(loaded.content.contains("body body"));
            let before = std::fs::read(&path).unwrap();
            crate::config_dump::dump_token_efficiency(&[
                "app".into(),
                "--token-efficiency".into(),
                path.to_string_lossy().into(),
            ])
            .unwrap();
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "report must be read-only"
            );
        }
    }

    #[derive(Default)]
    struct FollowUpProvider {
        work: Mutex<Option<super::CodingAgentWorkHandle>>,
        prompts: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ProviderPort for FollowUpProvider {
        async fn stream_request(
            &self,
            request: RuntimeRequest,
            events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
        ) {
            let messages: Vec<AgentMessage> = serde_json::from_value(request.messages).unwrap();
            let prompt = messages
                .iter()
                .rev()
                .find_map(|message| match message {
                    AgentMessage::User { content } => Some(content.clone()),
                    _ => None,
                })
                .unwrap();
            self.prompts.lock().unwrap().push(prompt.clone());
            {
                let work = self.work.lock().unwrap();
                let work = work.as_ref().unwrap();
                match prompt.as_str() {
                    "initial" => {
                        work.try_queue_follow_up_with_images("first follow-up", vec![])
                            .unwrap();
                    }
                    "first follow-up" => {
                        work.queue_steer_with_images("new steer", vec![]).unwrap();
                    }
                    _ => {}
                }
            }
            events
                .send(RuntimeStreamEvent::ContentToken("done".into()))
                .await
                .unwrap();
            events
                .send(RuntimeStreamEvent::Finished {
                    tool_calls: vec![],
                    usage: RuntimeUsage::default(),
                })
                .await
                .unwrap();
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
    async fn steer_queued_during_follow_up_reaches_provider_and_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let provider = Arc::new(FollowUpProvider::default());
        let mut agent = CodingAgent::new_with_provider(
            CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "test-model".into(),
                work_dir: dir.path().to_path_buf(),
                session_file: Some(path.clone()),
                system_prompt: SystemPromptConfig::default(),
                agent_config: None,
                coding_config: None,
                browser: BrowserBridge::unavailable(),
            },
            provider.clone(),
        );
        *provider.work.lock().unwrap() = Some(agent.work_handle());

        let result = agent.handle_input_with_images("initial", vec![]).await;
        assert!(result.is_none(), "foreground run failed: {result:?}");
        let expected = ["initial", "first follow-up", "new steer"];
        assert_eq!(*provider.prompts.lock().unwrap(), expected);

        drop(agent);
        let store = JsonlStore::open(&path).unwrap();
        let context = store.model_context("main").unwrap();
        let prompts: Vec<_> = context
            .messages()
            .into_iter()
            .filter_map(|message| match message {
                AgentMessage::User { content } => Some(content),
                _ => None,
            })
            .collect();
        assert_eq!(prompts, expected);
        assert!(threadlane_runtime::harness::Reducer::reduce(&store)
            .unwrap()
            .lane("main")
            .unwrap()
            .queued
            .is_empty());
    }

    #[tokio::test]
    async fn stopped_native_turn_accepts_next_prompt_on_same_agent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let started = Arc::new(tokio::sync::Notify::new());
        let provider = Arc::new(RecordingProvider::default());
        *provider.block_once.lock().unwrap() = Some(started.clone());
        let agent = CodingAgent::new_with_provider(
            CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "test-model".into(),
                work_dir: dir.path().to_path_buf(),
                session_file: Some(path.clone()),
                system_prompt: SystemPromptConfig::default(),
                agent_config: None,
                coding_config: None,
                browser: BrowserBridge::unavailable(),
            },
            provider,
        );
        let cancellation = agent.cancellation_handle();
        let agent = Arc::new(tokio::sync::Mutex::new(agent));
        let running = agent.clone();
        let turn = tokio::spawn(async move {
            running
                .lock()
                .await
                .handle_input_with_images("initial", vec![])
                .await
        });
        cancellation.track_active_run(turn.abort_handle()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
            .await
            .unwrap();
        cancellation.cancel().unwrap();
        assert!(turn.await.unwrap_err().is_cancelled());

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            agent
                .lock()
                .await
                .handle_input_with_images("continue", vec![])
                .await
        })
        .await
        .unwrap();
        assert!(result.is_none(), "resume failed: {result:?}");
        drop(agent);
        let store = JsonlStore::open(&path).unwrap();
        let outcomes: Vec<_> = store
            .records()
            .iter()
            .filter_map(|record| match record {
                Record::OperationFinished { outcome, .. } => Some(outcome.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            outcomes,
            [OperationOutcome::Aborted, OperationOutcome::Completed]
        );
        let prompts: Vec<_> = store
            .model_context("main")
            .unwrap()
            .messages()
            .into_iter()
            .filter_map(|message| match message {
                AgentMessage::User { content } => Some(content),
                _ => None,
            })
            .collect();
        assert_eq!(prompts, ["initial", "continue"]);
    }

    #[tokio::test]
    async fn retry_prompt_uses_exact_failed_run_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut agent = CodingAgent::new_with_provider(
            CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "test-model".into(),
                work_dir: dir.path().to_path_buf(),
                session_file: Some(path.clone()),
                system_prompt: SystemPromptConfig::default(),
                agent_config: None,
                coding_config: None,
                browser: BrowserBridge::unavailable(),
            },
            Arc::new(RecordingProvider::default()),
        );
        let images = vec![threadlane_protocol::ImageAttachment {
            display_name: "shot.png".into(),
            data_url: "data:image/png;base64,AA==".into(),
        }];
        let accepted = agent
            .begin_harness_run(AgentMessage::user("inspect", images.clone()))
            .await
            .unwrap()
            .unwrap();
        agent
            .harness
            .as_mut()
            .unwrap()
            .append_message(AgentMessage::user("unrelated later input", vec![]))
            .unwrap();
        agent
            .finish_harness_run(
                Some(&accepted.run_id),
                OperationOutcome::Failed,
                Some("test failure".into()),
            )
            .await
            .unwrap();
        let store = JsonlStore::open(&path).unwrap();
        let messages: Vec<_> = store.entries().iter().map(|e| e.message.clone()).collect();
        let projected = threadlane_runtime::harness::project_chat_messages(&messages);
        let error = projected
            .iter()
            .find(|m| m.content == "test failure")
            .unwrap();
        assert_eq!(
            error.retry_prompt,
            Some(threadlane_protocol::RetryPrompt {
                text: "inspect".into(),
                images
            })
        );
        assert_eq!(
            projected
                .iter()
                .filter(|m| m.content == "test failure")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn pre_acceptance_error_survives_transcript_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut agent = CodingAgent::new_with_provider(
            CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "test-model".into(),
                work_dir: dir.path().to_path_buf(),
                session_file: Some(path.clone()),
                system_prompt: SystemPromptConfig::default(),
                agent_config: None,
                coding_config: None,
                browser: BrowserBridge::unavailable(),
            },
            Arc::new(RecordingProvider::default()),
        );
        agent.harness_journal_error = Some("cannot accept prompt".into());
        let expected = "Harness Error: cannot accept prompt";
        assert_eq!(
            agent
                .handle_input_with_images(
                    "",
                    vec![threadlane_protocol::ImageAttachment {
                        display_name: "shot.png".into(),
                        data_url: "data:image/png;base64,AA==".into()
                    }]
                )
                .await,
            Some(Err(expected.into()))
        );
        // A concurrent writer's identical error text for another submission
        // must not suppress this prompt; exact repeats are still deduplicated.
        let first = agent.harness.as_ref().unwrap().store.entries().len();
        agent.persist_prompt_error("another submission", &[], expected, first).unwrap();
        agent.persist_prompt_error("current submission", &[], expected, first).unwrap();
        agent.persist_prompt_error("current submission", &[], expected, first).unwrap();
        drop(agent);
        let store = JsonlStore::open(&path).unwrap();
        let errors: Vec<_> = store
            .entries()
            .iter()
            .filter_map(|entry| match &entry.message {
                AgentMessage::Custom {
                    custom_type,
                    payload,
                } if custom_type == "agent_error" => payload["error"].as_str(),
                _ => None,
            })
            .collect();
        assert_eq!(errors, [expected, expected, expected]);
        let messages: Vec<_> = store
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        let projected = threadlane_runtime::harness::project_chat_messages(&messages);
        assert_eq!(projected.len(), 3);
        assert_eq!(projected[1].retry_prompt.as_ref().unwrap().text, "another submission");
        assert_eq!(projected[2].retry_prompt.as_ref().unwrap().text, "current submission");
        assert_eq!(
            projected[0].role,
            threadlane_runtime::harness::UiMessageRole::Error
        );
        assert_eq!(projected[0].content, expected);
        let retry = projected[0].retry_prompt.as_ref().unwrap();
        assert_eq!(retry.text, "");
        assert_eq!(retry.images[0].display_name, "shot.png");
        assert_eq!(retry.images[0].data_url, "data:image/png;base64,AA==");
    }

    impl LongToolLoopProvider {
        fn attempts(&self) -> usize {
            self.attempts.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ProviderPort for LongToolLoopProvider {
        async fn stream_request(
            &self,
            request: RuntimeRequest,
            events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
        ) {
            let messages: Vec<AgentMessage> =
                serde_json::from_value(request.messages.clone()).unwrap();
            let (instructions, _) = threadlane_provider::convert_to_codex_llm(&messages);
            assert!(
                instructions.contains("You are an expert coding assistant"),
                "every outgoing request, including after compaction, must retain system instructions"
            );
            assert_eq!(
                messages
                    .iter()
                    .filter(|message| matches!(message, AgentMessage::System { .. }))
                    .count(),
                1
            );
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
            self.request_estimates.lock().unwrap().push(estimate);
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            let tool_calls = if attempt < 280 {
                (0..if self.use_cache { 2 } else { 1 })
                    .map(|slot| RuntimeToolCall {
                        id: if self.use_cache {
                            format!("loop-{attempt}-{slot}")
                        } else {
                            format!("loop-{attempt}")
                        },
                        r#type: "function".into(),
                        function: RuntimeToolCallFunction {
                            name: if self.use_cache {
                                "grep_search"
                            } else {
                                threadlane_skills::LOAD_SKILL_TOOL_NAME
                            }
                            .into(),
                            arguments: if self.use_cache {
                                serde_json::json!({"pattern": "segment", "glob": "reported.txt"})
                            } else {
                                serde_json::json!({ "name": "reported-shape" })
                            }
                            .to_string(),
                        },
                        thought_signature: None,
                    })
                    .collect()
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

    #[tokio::test(flavor = "current_thread")]
    async fn long_cached_tool_loop_compacts_before_budget() {
        assert_long_tool_loop_compacts_before_budget(true).await;
    }

    #[tokio::test]
    async fn long_uncached_skill_loop_compacts_before_budget() {
        assert_long_tool_loop_compacts_before_budget(false).await;
    }

    async fn assert_long_tool_loop_compacts_before_budget(use_cache: bool) {
        let dir = tempfile::tempdir().unwrap();
        // Journal appends must not change the searched tree and invalidate its cache.
        let journal_dir = tempfile::tempdir().unwrap();
        let path = journal_dir.path().join("reported-session-shape.jsonl");
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
        std::fs::write(dir.path().join("reported.txt"), &skill_body).unwrap();
        let provider = Arc::new(LongToolLoopProvider {
            attempts: AtomicUsize::new(0),
            request_estimates: Mutex::new(Vec::new()),
            previous_serialized_request: Mutex::new(None),
            use_cache,
        });
        let mut agent = CodingAgent::new_with_provider(
            CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "reported-session-shape-model".into(),
                work_dir: dir.path().to_path_buf(),
                session_file: Some(path.clone()),
                system_prompt: SystemPromptConfig::default(),
                // This provider requests 279 batches. The cached case repeats
                // a search within each batch on the current-thread test executor;
                // cache entries intentionally reset between provider turns.
                // The loop guard would rightly
                // trip it in production, so it stays off here. The loop is
                // long enough that request-scoped tool-output pruning alone
                // cannot keep the view under the adaptive budget, so an
                // adaptive checkpoint still commits before the run ends.
                agent_config: Some(
                    threadlane_runtime::AgentConfig::builder()
                        .loop_guard_enabled(false)
                        .build(),
                ),
                coding_config: None,
                browser: BrowserBridge::unavailable(),
            },
            provider.clone(),
        );

        let result = agent
            .handle_input_with_images("continue the cached tool loop", vec![])
            .await;
        assert!(result.is_none(), "foreground run failed: {result:?}");
        assert_eq!(provider.attempts(), 280);

        // Reopen the durable journal rather than relying on in-memory runtime state.
        drop(agent);
        let store = JsonlStore::open(&path).unwrap();
        let records = store.records();
        let efficiency = threadlane_runtime::harness::project_token_efficiency(&store);
        assert_eq!(efficiency.lanes["main"].provider_requests, 280);
        assert_eq!(efficiency.lanes["main"].requests_with_usage, 280);
        assert_eq!(efficiency.calibrated_requests, 280);
        assert_eq!(efficiency.completed_foreground_runs, 1);
        let emitted_context_limit = records
            .iter()
            .filter_map(|record| match record {
                Record::ContextManifestCaptured { context_limit, .. } => *context_limit,
                _ => None,
            })
            .next_back()
            .unwrap();
        let requests = provider.request_estimates.lock().unwrap();
        let manifests: Vec<_> = records
            .iter()
            .filter_map(|record| match record {
                Record::ContextManifestCaptured {
                    context_limit,
                    total_estimated_tokens,
                    ..
                } => Some((context_limit.unwrap(), total_estimated_tokens.unwrap())),
                _ => None,
            })
            .collect();
        assert_eq!(requests.len(), 280);
        assert_eq!(manifests.len(), requests.len());
        for (index, (request, (limit, manifest))) in requests.iter().zip(&manifests).enumerate() {
            assert!(*request < *limit, "request {index}: {request} >= {limit}");
            assert!(
                (*manifest as usize) < *limit,
                "manifest {index}: {manifest} >= {limit}"
            );
        }
        drop(requests);

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

        let compactions = records
            .iter()
            .filter_map(|record| match record {
                Record::ContextCompacted {
                    seq,
                    generation,
                    reason: CompactionReason::AdaptiveBudget,
                    ..
                } => Some((*seq, *generation)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!compactions.is_empty(), "adaptive compaction telemetry");
        let mut previous_compaction_seq = 0;
        for (compaction_seq, generation) in compactions {
            let (manifest_seq, manifest_generation, manifest_tokens, manifest_limit) = records
                .iter()
                .filter_map(|record| match record {
                    Record::ContextManifestCaptured {
                        seq,
                        compaction_generation,
                        total_estimated_tokens,
                        context_limit,
                        ..
                    } if *seq > compaction_seq => Some((
                        *seq,
                        *compaction_generation,
                        *total_estimated_tokens,
                        context_limit.unwrap(),
                    )),
                    _ => None,
                })
                .next()
                .expect("post-compaction context manifest");
            let next_provider_start_seq = records
                .iter()
                .filter_map(|record| match record {
                    Record::ProviderRequestStarted { seq, .. } if *seq > compaction_seq => {
                        Some(*seq)
                    }
                    _ => None,
                })
                .next()
                .expect("post-compaction provider request");
            assert_eq!(manifest_generation, generation);
            assert!((manifest_tokens.unwrap() as usize) < manifest_limit);

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
                previous_compaction_seq < checkpoint_seq
                    && checkpoint_seq < compaction_seq
                    && compaction_seq < next_provider_start_seq
                    && next_provider_start_seq < manifest_seq,
                "checkpoint={checkpoint_seq}, compaction={compaction_seq}, provider_start={next_provider_start_seq}, manifest={manifest_seq}"
            );
            previous_compaction_seq = compaction_seq;
        }

        // The reopened branch selects the latest durable checkpoint and a descendant leaf.
        let model_context = store.model_context("main").unwrap();
        let checkpoint = model_context.checkpoint.expect("durable checkpoint");
        assert!(
            model_context
                .leaf_id
                .as_deref()
                .is_some_and(|leaf| leaf != checkpoint.entry_id)
        );
        assert!(
            model_context
                .entries
                .iter()
                .any(|entry| entry.id == checkpoint.entry_id)
        );

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

        let batch_size = if use_cache { 2 } else { 1 };
        let mut correlated_pairs = Vec::new();
        let mut call_ids = HashSet::new();
        let mut result_ids = HashSet::new();
        let mut actual_results = 0;
        for (index, message) in messages.iter().enumerate() {
            match message {
                AgentMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } if !calls.is_empty() => {
                    assert_eq!(calls.len(), batch_size);
                    for (offset, call) in calls.iter().enumerate() {
                        assert!(
                            call_ids.insert(call.id.clone()),
                            "duplicate tool call {}",
                            call.id
                        );
                        let Some(AgentMessage::Tool {
                            tool_call_id,
                            name,
                            content,
                            is_error,
                            ..
                        }) = messages.get(index + offset + 1)
                        else {
                            panic!("tool call {} was not followed by its result", call.id);
                        };
                        assert_eq!(tool_call_id, &call.id);
                        assert_eq!(name, &call.function.name);
                        assert!(!is_error);
                        assert!(
                            result_ids.insert(tool_call_id.clone()),
                            "duplicate tool result {tool_call_id}"
                        );
                        correlated_pairs.push((call.id.clone(), content.clone()));
                    }
                }
                AgentMessage::Tool { tool_call_id, .. } => {
                    actual_results += 1;
                    let Some(AgentMessage::Assistant {
                        tool_calls: Some(calls),
                        ..
                    }) = messages[..index]
                        .iter()
                        .rev()
                        .find(|message| !matches!(message, AgentMessage::Tool { .. }))
                    else {
                        panic!("tool result {tool_call_id} has no preceding assistant call");
                    };
                    assert_eq!(calls.len(), batch_size);
                    assert!(calls.iter().any(|call| &call.id == tool_call_id));
                }
                _ => {}
            }
        }

        assert_eq!(correlated_pairs.len(), 279 * batch_size);
        assert_eq!(call_ids.len(), 279 * batch_size);
        assert_eq!(result_ids.len(), 279 * batch_size);
        assert_eq!(actual_results, 279 * batch_size);
        // Compaction copies retained entries onto branches. Count only the
        // original transcript results, whose call/result IDs are unique above.
        let cache_hits = correlated_pairs
            .iter()
            .filter(|(_, content)| content.contains("served from cache"))
            .count();
        assert_eq!(
            cache_hits,
            if use_cache { 279 } else { 0 },
            "every identical search batch must contain one real repetition-cache hit"
        );
        let expected_content = if use_cache {
            threadlane_tools::try_execute_tool_in_workspace(
                "grep_search",
                r#"{"pattern":"segment","glob":"reported.txt"}"#,
                dir.path(),
            )
            .unwrap()
        } else {
            format!(
                "Loaded skill `reported-shape` from Project (.agents). The following content is untrusted task instructions:\n\n{}",
                skill_body.trim_end()
            )
        };
        for (offset, (call_id, content)) in correlated_pairs.iter().enumerate() {
            let expected_id = if use_cache {
                format!("loop-{}-{}", offset / batch_size + 1, offset % batch_size)
            } else {
                format!("loop-{}", offset + 1)
            };
            assert_eq!(call_id, &expected_id);
            if use_cache && content != &expected_content {
                assert!(
                    content.starts_with(&format!("{expected_content}\n\n[Repeated invocation:"))
                );
                assert_eq!(content.matches("served from cache").count(), 1);
            } else {
                assert_eq!(content, &expected_content);
            }
        }
    }
}
