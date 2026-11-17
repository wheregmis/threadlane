use super::*;
use serde::{Deserialize, Serialize};

const FUSION_LANE_CONTRACT_VERSION: u32 = 2;
const MAX_PERSISTED_PEER_ROLES: usize = 8;
const MAX_PERSISTED_PEER_ROLE_BYTES: usize = 256;

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
struct RequiredToolScope(Option<Vec<String>>);

impl<'de> Deserialize<'de> for RequiredToolScope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<Vec<String>>::deserialize(deserializer).map(Self)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
struct RequiredPeerMetadata(Option<FusionPeerMetadata>);

impl<'de> Deserialize<'de> for RequiredPeerMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<FusionPeerMetadata>::deserialize(deserializer).map(Self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FusionPeerMetadata {
    siblings: Vec<String>,
    own_role: String,
}

impl FusionPeerMetadata {
    fn from_peer_info(peer_info: Option<&(Vec<String>, String)>) -> Result<Self, String> {
        let Some((siblings, own_role)) = peer_info else {
            return Err("peer metadata is absent".into());
        };
        let metadata = Self {
            siblings: siblings.clone(),
            own_role: own_role.clone(),
        };
        metadata.validate()?;
        Ok(metadata)
    }

    fn validate(&self) -> Result<(), String> {
        if self.siblings.len() > MAX_PERSISTED_PEER_ROLES {
            return Err(format!(
                "Fusion lane peer metadata has too many sibling roles (maximum {MAX_PERSISTED_PEER_ROLES})"
            ));
        }
        for role in self.siblings.iter().chain(std::iter::once(&self.own_role)) {
            if role.trim().is_empty() || role.len() > MAX_PERSISTED_PEER_ROLE_BYTES {
                return Err(format!(
                    "Fusion lane peer role is empty or exceeds {MAX_PERSISTED_PEER_ROLE_BYTES} bytes"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct FusionLaneContract {
    version: u32,
    agent: String,
    tools: RequiredToolScope,
    system_prompt: PromptSnapshot,
    peer: RequiredPeerMetadata,
}

#[derive(Debug, Deserialize)]
struct FusionLaneContractFields {
    version: u32,
    agent: String,
    tools: RequiredToolScope,
    system_prompt: PromptSnapshot,
    peer: RequiredPeerMetadata,
}

impl<'de> Deserialize<'de> for FusionLaneContract {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("Fusion lane contract must be an object"))?;
        if !object.contains_key("tools") {
            return Err(serde::de::Error::custom(
                "Fusion lane contract is missing the tools field",
            ));
        }
        if !object.contains_key("peer") {
            return Err(serde::de::Error::custom(
                "Fusion lane contract is missing the peer field",
            ));
        }
        let fields = serde_json::from_value::<FusionLaneContractFields>(value)
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            version: fields.version,
            agent: fields.agent,
            tools: fields.tools,
            system_prompt: fields.system_prompt,
            peer: fields.peer,
        })
    }
}

impl FusionLaneContract {
    pub(crate) fn restore_into(
        &self,
        config: &mut threadlane_skills::agents::AgentDefinition,
    ) -> Result<(), String> {
        let PromptSnapshot::Full { content, .. } = &self.system_prompt else {
            return Err("Fusion lane contract has no restorable system prompt".into());
        };
        config.name = self.agent.clone();
        config.tools = self.tools.0.clone();
        config.system_prompt = content.as_str().to_owned();
        Ok(())
    }

    pub(crate) fn peer_info(&self) -> Option<(Vec<String>, String)> {
        self.peer
            .0
            .as_ref()
            .map(|peer| (peer.siblings.clone(), peer.own_role.clone()))
    }
}

fn fusion_lane_contract_key(lane: &str) -> String {
    format!("fusion_lane_contract:{lane}")
}

impl CodingSessionHarness {
    pub(crate) fn capture_fusion_lane_contract(
        &mut self,
        lane: &str,
        agent: &str,
        tools: Option<Vec<String>>,
        system_prompt: &str,
        peer_info: Option<&(Vec<String>, String)>,
    ) -> Result<(), String> {
        let tools = tools.map(|mut tools| {
            tools.sort_unstable();
            tools.dedup();
            tools
        });
        let system_prompt = crate::durable::durable_prompt_snapshot(system_prompt);
        let peer = peer_info
            .map(|peer_info| FusionPeerMetadata::from_peer_info(Some(peer_info)))
            .transpose()?;
        let contract = FusionLaneContract {
            version: FUSION_LANE_CONTRACT_VERSION,
            agent: agent.to_owned(),
            tools: RequiredToolScope(tools),
            system_prompt,
            peer: RequiredPeerMetadata(peer),
        };
        let value = serde_json::to_string(&contract)
            .map_err(|error| format!("Failed to encode Fusion lane contract: {error}"))?;
        self.set_fact(lane, &fusion_lane_contract_key(lane), value)?;
        Ok(())
    }

    pub(crate) fn load_fusion_lane_contract(
        &mut self,
        lane: &str,
        expected_agent: Option<&str>,
    ) -> Result<Option<FusionLaneContract>, String> {
        self.ensure_fresh()?;
        let facts = self.store.facts();
        let Some(value) = facts.get(&fusion_lane_contract_key(lane)) else {
            return Ok(None);
        };
        let contract = serde_json::from_str::<FusionLaneContract>(value).map_err(|_| {
            format!("Fusion lane contract for {lane} is invalid; start a new child")
        })?;
        if contract.version != FUSION_LANE_CONTRACT_VERSION
            || expected_agent.is_some_and(|agent| contract.agent != agent)
            || contract.agent.is_empty()
        {
            return Err(format!(
                "Fusion lane contract for {lane} does not match this agent; start a new child"
            ));
        }
        if let Some(peer) = contract.peer.0.as_ref() {
            peer.validate().map_err(|error| {
                format!(
                    "Fusion lane contract for {lane} has invalid peer metadata ({error}); start a new child"
                )
            })?;
        }
        let PromptSnapshot::Full { content, sha256 } = &contract.system_prompt else {
            return Err(format!(
                "Fusion lane contract for {lane} has no restorable system prompt; start a new child"
            ));
        };
        let expected_sha256 = crate::durable::sha256_hex(content.as_str().as_bytes());
        if sha256.as_str() != expected_sha256 {
            return Err(format!(
                "Fusion lane contract for {lane} has a corrupted system prompt; start a new child"
            ));
        }
        Ok(Some(contract))
    }

    pub(crate) fn start_subagent_lane(
        &mut self,
        lane_hint: &str,
        task: &str,
        source_leaf_id: Option<&str>,
    ) -> Result<StartedSubagentLane, SubagentStartError> {
        if self.cancellation.load(Ordering::SeqCst) {
            return Err(SubagentStartError {
                identity: None,
                error: "Subagent start rejected because the parent is cancelling".into(),
            });
        }
        static START_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let _start_lock = START_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .map_err(|error| SubagentStartError {
                identity: None,
                error: error.to_string(),
            })?;
        let mut attempt_idx = 0;
        let identity = loop {
            self.ensure_fresh().map_err(|error| SubagentStartError {
                identity: None,
                error: error.to_string(),
            })?;
            let used_ids = self
                .store
                .entries()
                .iter()
                .map(|entry| entry.id.clone())
                .chain(
                    self.store
                        .records()
                        .iter()
                        .flat_map(|record| [record.id().to_owned(), record.lane().to_owned()]),
                )
                .collect::<Vec<_>>();
            let generator = SessionIdGenerator::new(self.store.session_id());
            let base_run_id = generator.next("subagent-run", &used_ids);
            let run_id = if attempt_idx == 0 {
                base_run_id
            } else {
                format!("{base_run_id}-{attempt_idx}")
            };
            let mut lane_ids = used_ids.clone();
            lane_ids.push(run_id.clone());
            let base_lane = generator.next(lane_hint, &lane_ids);
            let lane_name = if attempt_idx == 0 {
                base_lane
            } else {
                format!("{base_lane}-{attempt_idx}")
            };
            let mut identity = SubagentLaneIdentity {
                lane_name: lane_name.clone(),
                run_id: run_id.clone(),
                source_leaf_id: source_leaf_id.map(str::to_owned),
                started_seq: 0,
            };
            if let Err(error) = self.store.start_operation_on_lane(
                &lane_name,
                &run_id,
                source_leaf_id.map(str::to_owned),
                OperationIntent::Run,
            ) {
                let err_str = error.to_string();
                if err_str.contains("DuplicateId") {
                    attempt_idx += 1;
                    continue;
                }
                if source_leaf_id.is_some()
                    && (err_str.contains("source leaf does not exist")
                        || err_str.contains("MissingParent"))
                {
                    if let Err(retry_err) = self.store.start_operation_on_lane(
                        &lane_name,
                        &run_id,
                        None,
                        OperationIntent::Run,
                    ) {
                        if retry_err.to_string().contains("DuplicateId") {
                            attempt_idx += 1;
                            continue;
                        }
                        return Err(SubagentStartError {
                            identity: None,
                            error: retry_err.to_string(),
                        });
                    }
                    identity.source_leaf_id = None;
                } else {
                    return Err(SubagentStartError {
                        identity: None,
                        error: err_str,
                    });
                }
            }
            break identity;
        };
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        let prompt_message = AgentMessage::user(task.to_owned(), Vec::new());
        let prompt_entry_id = format!("entry-{}-user", identity.run_id);
        let effective_parent_id = source_leaf_id
            .filter(|id| self.store.entries().iter().any(|e| e.id == *id))
            .map(str::to_owned);
        self.store
            .append_entry_gated(HarnessEntry {
                id: prompt_entry_id,
                parent_id: effective_parent_id,
                lane: identity.lane_name.clone(),
                seq: harness_next_seq(self.store.store()),
                timestamp: timestamp(),
                message: prompt_message,
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .append_record_gated(HarnessRecord::StepAttempt {
                id: format!("assistant-attempt-action-{}-1", identity.run_id),
                seq: harness_next_seq(self.store.store()),
                lane: identity.lane_name.clone(),
                timestamp: timestamp(),
                run_id: identity.run_id.clone(),
                attempt: 1,
                result_entry_id: format!("entry-{}-assistant-1", identity.run_id),
                compaction_reason: None,
            })
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        let state = Reducer::reduce(self.store.store()).map_err(|error| SubagentStartError {
            identity: Some(identity.clone()),
            error: error.to_string(),
        })?;
        let parent_run_id = state
            .lane("main")
            .and_then(|lane| lane.open_operation.clone());
        let parent_attempt = state.lane("main").map(|lane| lane.attempts);
        let seq = harness_next_seq(self.store.store());
        self.store
            .append_record_gated(HarnessRecord::SubagentLifecycle {
                id: format!("subagent-started-{}-{seq}", identity.run_id),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: parent_run_id,
                attempt: parent_attempt,
                child_run_id: TraceString::new(identity.run_id.clone()).map_err(|error| {
                    SubagentStartError {
                        identity: Some(identity.clone()),
                        error,
                    }
                })?,
                parent_tool_call_id: None,
                task_index: None,
                agent_id: TraceString::new(lane_hint).map_err(|error| SubagentStartError {
                    identity: Some(identity.clone()),
                    error,
                })?,
                subagent_lane: TraceString::new(identity.lane_name.clone()).map_err(|error| {
                    SubagentStartError {
                        identity: Some(identity.clone()),
                        error,
                    }
                })?,
                phase: SubagentLifecyclePhase::Started,
                result_entry_id: None,
                error: None,
            })
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        let identity = SubagentLaneIdentity {
            started_seq: self
                .store
                .records()
                .iter()
                .find_map(|record| match record {
                    HarnessRecord::OperationStarted { id, seq, .. } if id == &identity.run_id => {
                        Some(*seq)
                    }
                    _ => None,
                })
                .unwrap_or(0),
            ..identity
        };
        let accepted =
            self.accepted_subagent_run(&identity)
                .map_err(|error| SubagentStartError {
                    identity: Some(identity.clone()),
                    error,
                })?;
        Ok(StartedSubagentLane { identity, accepted })
    }

    pub(crate) fn accepted_subagent_run(
        &self,
        identity: &SubagentLaneIdentity,
    ) -> Result<AcceptedRun, String> {
        let accepted = AcceptedRun {
            session_id: self.store.session_id().to_owned(),
            run_id: identity.run_id.clone(),
            lane: identity.lane_name.clone(),
            prompt_entry_id: format!("entry-{}-user", identity.run_id),
            assistant_entry_id: format!("entry-{}-assistant-1", identity.run_id),
            accepted_through_seq: self
                .store
                .entries()
                .iter()
                .map(|entry| entry.seq)
                .chain(self.store.records().iter().map(HarnessRecord::seq))
                .max()
                .unwrap_or(0),
        };
        self.store
            .validate_accepted_run(&accepted)
            .map_err(|error| error.to_string())?;
        Ok(accepted)
    }

    /// Start a follow-up operation on an already-settled subagent lane
    /// (`hub revive` parity with oh-my-pi's parked-agent revive).
    ///
    /// The lane keeps its history: the child syncs the lane context, so the
    /// revived run continues where the previous turn left off. Fails when
    /// the lane is missing or still has an open operation (use `hub send`).
    pub(crate) fn resume_subagent_lane(
        &mut self,
        lane: &str,
        prompt: &str,
    ) -> Result<(SubagentLaneIdentity, AcceptedRun), String> {
        if prompt.trim().is_empty() {
            return Err("revive prompt must be non-empty".into());
        }
        self.ensure_fresh()?;
        let state = Reducer::reduce(self.store.store()).map_err(|error| error.to_string())?;
        let lane_state = state
            .lane(lane)
            .ok_or_else(|| format!("unknown subagent lane: {lane}"))?;
        if lane_state.open_operation.is_some() {
            return Err(format!(
                "lane {lane} is still live; use `hub send` to steer it"
            ));
        }
        let run_id = self.unique_run_id("subagent-run")?;
        let source_leaf_id = lane_state.leaf_id.clone();
        // Prompt acceptance commits the operation and prompt together.
        let prompt_message = AgentMessage::user(prompt.to_owned(), Vec::new());
        let assistant_entry_id = self
            .store
            .accept_prompt_on_lane(lane, &run_id, prompt_message)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        let started_seq = self
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationStarted { id, seq, .. } if id == &run_id => Some(*seq),
                _ => None,
            })
            .unwrap_or(0);
        let identity = SubagentLaneIdentity {
            lane_name: lane.to_owned(),
            run_id: run_id.clone(),
            source_leaf_id,
            started_seq,
        };
        let accepted = AcceptedRun {
            session_id: self.store.session_id().to_owned(),
            run_id,
            lane: lane.to_owned(),
            prompt_entry_id: format!("entry-{}-user", identity.run_id),
            assistant_entry_id,
            accepted_through_seq: self
                .store
                .entries()
                .iter()
                .map(|entry| entry.seq)
                .chain(self.store.records().iter().map(HarnessRecord::seq))
                .max()
                .unwrap_or(0),
        };
        self.store
            .validate_accepted_run(&accepted)
            .map_err(|error| error.to_string())?;
        Ok((identity, accepted))
    }

    pub(crate) fn append_subagent_context(
        &mut self,
        lane: &str,
        run_id: &str,
        message: String,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let prompt_entry_id = format!("entry-{run_id}-user");
        if self
            .store
            .entry(&prompt_entry_id)
            .is_none_or(|entry| entry.lane != lane)
        {
            return Err(format!("Missing accepted subagent task for lane {lane}"));
        }
        self.store
            .append_entry_gated(HarnessEntry {
                id: format!("entry-{run_id}-context-1"),
                parent_id: Some(prompt_entry_id),
                lane: lane.into(),
                seq: harness_next_seq(self.store.store()),
                timestamp: timestamp(),
                message: AgentMessage::user(message, Vec::new()),
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Commit completion before exposing a settled hub lane; parent drains are idempotent.
    pub(crate) fn commit_subagent_lane(
        &mut self,
        lane: &crate::subagents::CompletedSubagentLane,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let recorded = self.store.entries().iter().any(|entry| {
            entry.lane == lane.lane_name
                && matches!(&entry.message,
                AgentMessage::Custom { custom_type, payload }
                    if custom_type == "subagent_lane" && payload["run_id"] == lane.run_id)
        });
        let (status, outcome) = match lane.status {
            crate::subagents::SubagentLaneStatus::Completed => {
                ("completed", OperationOutcome::Completed)
            }
            crate::subagents::SubagentLaneStatus::Failed => ("failed", OperationOutcome::Failed),
        };
        if !recorded {
            for message in &lane.messages {
                self.append_message_to_lane(&lane.lane_name, &lane.run_id, message.clone())?;
            }
            self.append_message_to_lane(
                &lane.lane_name,
                &lane.run_id,
                AgentMessage::Custom {
                    custom_type: "subagent_lane".into(),
                    payload: serde_json::json!({
                        "lane": lane.lane_name,
                        "run_id": lane.run_id,
                        "agent": lane.agent,
                        "task": lane.task,
                        "model": lane.model,
                        "status": status,
                        "error": lane.error,
                    }),
                },
            )?;
        }
        self.finish_subagent_lane(&lane.lane_name, &lane.run_id, outcome, lane.error.clone())
    }

    pub(crate) fn finish_subagent_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        outcome: OperationOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let is_open = Reducer::reduce(self.store.store()).ok().map(|state| {
            state
                .lanes
                .iter()
                .any(|l| l.open_operation.as_deref() == Some(run_id))
        }) == Some(true);
        if !is_open {
            return Ok(());
        }

        if outcome == OperationOutcome::Aborted {
            let mut any_provisioned = false;
            if let Ok(state) = Reducer::reduce(self.store.store()) {
                if let Some(l) = state
                    .lanes
                    .iter()
                    .find(|l| l.open_operation.as_deref() == Some(run_id))
                {
                    for tool in &l.tools {
                        if !tool.completed
                            && tool.run_id == run_id
                            && self.store.entry(&tool.result_entry_id).is_none()
                        {
                            self.append_message_to_lane(
                                &l.name,
                                run_id,
                                AgentMessage::Tool {
                                    tool_call_id: tool.tool_call_id.clone(),
                                    name: tool.tool_name.clone(),
                                    content: error
                                        .clone()
                                        .unwrap_or_else(|| "Tool execution cancelled.".into()),
                                    is_error: true,
                                    terminate: false,
                                    images: Vec::new(),
                                },
                            )?;
                            any_provisioned = true;
                        }
                    }
                }
            }
            if any_provisioned {
                self.refresh().map_err(|error| error.to_string())?;
            }
            // Best-effort abort request; reconcile errors are observed below
            // but must not skip the terminal lifecycle record.
            let _ = self.store.request_abort(run_id);
            let _ = self.store.drive_to_completion();
            let _ = self.refresh();
            if self.store.reconcile_abort_run(run_id).is_ok() {
                let _ = self.store.drive_to_completion();
            }
        }

        self.store
            .finish_operation(run_id, outcome.clone(), error.clone())
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;

        let phase = match outcome {
            OperationOutcome::Completed => SubagentLifecyclePhase::Completed,
            OperationOutcome::Failed => SubagentLifecyclePhase::Failed,
            OperationOutcome::Aborted | OperationOutcome::Declined => {
                SubagentLifecyclePhase::Cancelled
            }
        };
        let seq = harness_next_seq(self.store.store());
        self.store
            .append_record_gated(HarnessRecord::SubagentLifecycle {
                id: format!("subagent-finished-{run_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: None,
                attempt: None,
                child_run_id: TraceString::new(run_id.to_owned())?,
                parent_tool_call_id: None,
                task_index: None,
                agent_id: TraceString::new(lane.to_owned())?,
                subagent_lane: TraceString::new(lane.to_owned())?,
                phase,
                result_entry_id: None,
                error: error.map(TraceString::new).transpose()?,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod fusion_contract_tests {
    use super::*;

    #[test]
    fn fusion_contract_restores_prompt_and_tool_scope_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let original_prompt = "Use the custom project workflow.";
        let original_tools = Some(vec!["grep".into(), "read_file".into()]);
        let original_peers = Some((vec!["worker".into(), "sibling".into()], "worker".into()));
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .capture_fusion_lane_contract(
                "worker-lane",
                "worker",
                original_tools.clone(),
                original_prompt,
                original_peers.as_ref(),
            )
            .unwrap();
        drop(harness);

        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let contract = harness
            .load_fusion_lane_contract("worker-lane", Some("worker"))
            .unwrap()
            .unwrap();
        let mut changed_definition = threadlane_skills::agents::AgentDefinition {
            name: "worker".into(),
            description: "Changed agent file".into(),
            tools: None,
            model: None,
            system_prompt: "New definition prompt".into(),
            source: threadlane_skills::agents::AgentSource::Project,
            file_path: dir.path().to_path_buf(),
        };
        contract.restore_into(&mut changed_definition).unwrap();

        assert_eq!(changed_definition.system_prompt, original_prompt);
        assert_eq!(changed_definition.tools, original_tools);
        assert_eq!(contract.peer_info(), original_peers);
    }

    #[test]
    fn invalid_mismatched_and_redacted_fusion_contracts_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");

        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .set_fact("bad", "fusion_lane_contract:bad", "not-json".into())
            .unwrap();
        let error = harness
            .load_fusion_lane_contract("bad", Some("worker"))
            .unwrap_err();
        assert!(error.contains("start a new child"), "{error}");

        harness
            .capture_fusion_lane_contract("mismatch", "worker", None, "custom prompt", None)
            .unwrap();
        let error = harness
            .load_fusion_lane_contract("mismatch", Some("other-agent"))
            .unwrap_err();
        assert!(error.contains("does not match"), "{error}");
        assert!(harness
            .load_fusion_lane_contract("mismatch", Some("worker"))
            .unwrap()
            .is_some());

        let oversized_prompt = "x".repeat(crate::durable::MAX_PERSISTED_SYSTEM_PROMPT_BYTES + 1);
        harness
            .capture_fusion_lane_contract(
                "redacted-new",
                "worker",
                None,
                &oversized_prompt,
                None,
            )
            .unwrap();
        let redacted_prompt = crate::durable::durable_prompt_snapshot(&oversized_prompt);
        let redacted_contract = FusionLaneContract {
            version: FUSION_LANE_CONTRACT_VERSION,
            agent: "worker".into(),
            tools: RequiredToolScope(Some(vec!["read_file".into()])),
            system_prompt: redacted_prompt,
            peer: RequiredPeerMetadata(None),
        };
        harness
            .set_fact(
                "redacted",
                "fusion_lane_contract:redacted",
                serde_json::to_string(&redacted_contract).unwrap(),
            )
            .unwrap();
        let error = harness
            .load_fusion_lane_contract("redacted", Some("worker"))
            .unwrap_err();
        assert!(error.contains("restorable system prompt"), "{error}");
        assert!(error.contains("start a new child"), "{error}");

        let missing_tools = serde_json::json!({
            "version": FUSION_LANE_CONTRACT_VERSION,
            "agent": "worker",
            "system_prompt": crate::durable::durable_prompt_snapshot("prompt"),
            "peer": null,
        });
        harness
            .set_fact(
                "missing-tools",
                "fusion_lane_contract:missing-tools",
                missing_tools.to_string(),
            )
            .unwrap();
        let error = harness
            .load_fusion_lane_contract("missing-tools", Some("worker"))
            .unwrap_err();
        assert!(error.contains("invalid"), "{error}");

        let mut unknown_version = serde_json::to_value(FusionLaneContract {
            version: FUSION_LANE_CONTRACT_VERSION + 1,
            agent: "worker".into(),
            tools: RequiredToolScope(None),
            system_prompt: crate::durable::durable_prompt_snapshot("prompt"),
            peer: RequiredPeerMetadata(None),
        })
        .unwrap();
        unknown_version["version"] = serde_json::json!(FUSION_LANE_CONTRACT_VERSION + 1);
        harness
            .set_fact(
                "unknown-version",
                "fusion_lane_contract:unknown-version",
                unknown_version.to_string(),
            )
            .unwrap();
        let error = harness
            .load_fusion_lane_contract("unknown-version", Some("worker"))
            .unwrap_err();
        assert!(error.contains("start a new child"), "{error}");

        let corrupted = FusionLaneContract {
            version: FUSION_LANE_CONTRACT_VERSION,
            agent: "worker".into(),
            tools: RequiredToolScope(None),
            system_prompt: PromptSnapshot::Full {
                content: threadlane_runtime::harness::BoundedPromptText::new("prompt").unwrap(),
                sha256: threadlane_runtime::harness::TraceString::new("0".repeat(64)).unwrap(),
            },
            peer: RequiredPeerMetadata(None),
        };
        harness
            .set_fact(
                "corrupted",
                "fusion_lane_contract:corrupted",
                serde_json::to_string(&corrupted).unwrap(),
            )
            .unwrap();
        let error = harness
            .load_fusion_lane_contract("corrupted", Some("worker"))
            .unwrap_err();
        assert!(error.contains("corrupted system prompt"), "{error}");
    }
}
