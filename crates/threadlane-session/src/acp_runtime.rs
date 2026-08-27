//! Drives a turn against an external ACP agent.
//!
//! [`crate::acp`] owns the protocol and [`crate::acp_bridge`] owns the mapping
//! onto `AgentEvent`; this module is the piece that joins them to the rest of
//! the session, so selecting an `acp/<id>` model actually runs.
//!
//! Three things make an ACP turn different from a provider turn and shape
//! everything here:
//!
//! * **The agent owns the conversation.** ACP has no notion of replaying a
//!   message list, so the connection is kept alive across turns and reused;
//!   dropping it would silently reset the agent's context.
//! * **Consent has to reach a human.** The agent asks the client for
//!   permission, so requests are rendered through the same
//!   `AgentEvent::PermissionRequested` prompt the native tools use rather than
//!   answered from a fixed policy.
//! * **Stopping is two-sided.** Aborting the local task only stops Threadlane
//!   from listening; the agent keeps working until it is told to stop, so the
//!   turn carries a drop guard that sends `session/cancel`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use log::warn;
use tokio::sync::{broadcast, mpsc};

use crate::acp::{
    ACP_CONFIG_CATEGORY_EFFORT, ACP_CONFIG_CATEGORY_MODEL, AcpClientHandler, AcpConfigOption,
    AcpContentBlock, AcpManager, AcpPermissionOptionKind, AcpPermissionOutcome,
    AcpPermissionRequest, AcpReadTextFileRequest, AcpSession, AcpSessionNotification,
    AcpSessionUpdate, AcpStopReason, AcpToolCall, AcpWorkspaceClient, AcpWriteTextFileRequest,
    config_option_for,
};
use crate::acp_bridge::agent_events_for;
use crate::permission::{PermissionDecision, PermissionHandle};
use threadlane_runtime::{
    AgentEvent, AgentToolResult, ImageAttachment, ReasoningEffort, TokenUsage,
};

/// Durable tool activity collected while an ACP turn streams.
pub(crate) struct AcpTurnToolActivity {
    pub(crate) tool_call_id: String,
    pub(crate) name: String,
    pub(crate) arguments: String,
    pub(crate) result: Option<AgentToolResult>,
}

pub(crate) struct AcpTurnOutcome {
    pub(crate) reply: String,
    pub(crate) tools: Vec<AcpTurnToolActivity>,
}

/// A live connection to one external ACP agent, reused across turns.
pub struct AcpEngine {
    global_dir: Option<PathBuf>,
    work_dir: PathBuf,
    active: Option<ActiveSession>,
}

struct ActiveSession {
    agent_id: String,
    session: Arc<AcpSession>,
    /// Buffered `session/update` notifications.
    ///
    /// The handler pushes onto this from the connection's read loop, which is
    /// what preserves the order the agent emitted them in; a turn drains it.
    updates: mpsc::UnboundedReceiver<AcpSessionNotification>,
}

/// Sends `session/cancel` if the turn is dropped before it finishes.
///
/// Cancellation aborts the task running the turn, which drops this future
/// rather than unwinding it, so this is the only place a cancelled turn can
/// still tell the agent to stop.
struct CancelOnDrop {
    session: Arc<AcpSession>,
    completed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let session = self.session.clone();
        // Drop is synchronous, so the notification has to outlive this frame.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if let Err(error) = session.cancel().await {
                        warn!("Failed to cancel ACP turn: {error}");
                    }
                });
            }
            Err(_) => warn!("Cancelled an ACP turn with no runtime to send session/cancel on"),
        }
    }
}

impl AcpEngine {
    pub fn new(global_dir: Option<PathBuf>, work_dir: PathBuf) -> Self {
        Self {
            global_dir,
            work_dir,
            active: None,
        }
    }

    /// Runs one prompt to completion, streaming the agent's output as
    /// `AgentEvent`s and returning the assistant text it produced.
    ///
    /// The text is returned rather than only broadcast because the caller has
    /// to journal it: reading it back off the broadcast channel would race
    /// with every other subscriber and drop output on lag.
    ///
    /// Emits `AgentEnd` on success and `AgentError` on failure so the UI
    /// leaves the generating state either way.
    pub async fn run_turn(
        &mut self,
        agent_id: &str,
        prompt: &str,
        images: &[ImageAttachment],
        effort: ReasoningEffort,
        event_tx: &broadcast::Sender<AgentEvent>,
        permissions: &PermissionHandle,
    ) -> Result<String, String> {
        self.run_turn_detailed(agent_id, prompt, images, effort, event_tx, permissions)
            .await
            .map(|outcome| outcome.reply)
    }

    pub(crate) async fn run_turn_detailed(
        &mut self,
        agent_id: &str,
        prompt: &str,
        images: &[ImageAttachment],
        effort: ReasoningEffort,
        event_tx: &broadcast::Sender<AgentEvent>,
        permissions: &PermissionHandle,
    ) -> Result<AcpTurnOutcome, String> {
        if let Err(error) = self.ensure_session(agent_id, event_tx, permissions).await {
            let _ = event_tx.send(AgentEvent::AgentError {
                error: error.clone(),
            });
            return Err(error);
        }
        let active = self
            .active
            .as_mut()
            .expect("ensure_session leaves a live session on success");
        let session = active.session.clone();
        let session_id = session.session_id().to_string();
        let blocks = prompt_blocks(
            prompt,
            images,
            session.agent().agent_capabilities.supports_image_prompts(),
        );

        // Applied per turn rather than at session start: the picker can change
        // between turns, and the agent keeps the setting on its own session.
        if let Some(value) = acp_effort_value(effort) {
            if let Err(error) = session
                .set_config_option(ACP_CONFIG_CATEGORY_EFFORT, value)
                .await
            {
                // An agent that does not expose effort is not a broken turn.
                warn!("Could not set ACP reasoning effort to '{value}': {error}");
            }
        }

        let _ = event_tx.send(AgentEvent::AgentStart);
        let mut guard = CancelOnDrop {
            session: session.clone(),
            completed: false,
        };

        let session_for_prompt = session.clone();
        let turn = async move { session_for_prompt.prompt(blocks).await };
        tokio::pin!(turn);

        // Updates and the prompt response arrive on the same connection, so
        // they must be awaited together: draining only after the prompt
        // resolves would withhold the whole turn's output until the end.
        let mut reply = String::new();
        let mut tools = Vec::new();
        let outcome = loop {
            tokio::select! {
                notification = active.updates.recv() => match notification {
                    Some(notification) => {
                        forward_update(notification, &session_id, event_tx, &mut reply, &mut tools);
                    }
                    // The handler holds the sender for the session's lifetime,
                    // so this only closes if the connection is gone.
                    None => continue,
                },
                result = &mut turn => break result,
            }
        };

        // The agent emits its closing chunks just before answering the prompt,
        // so anything already queued still belongs to this turn.
        while let Ok(notification) = active.updates.try_recv() {
            forward_update(notification, &session_id, event_tx, &mut reply, &mut tools);
        }
        guard.completed = true;

        match outcome {
            Ok(stop_reason) => {
                if let Some(note) = stop_reason_note(stop_reason) {
                    let _ = event_tx.send(AgentEvent::AgentError { error: note });
                }
                // ACP reports no token accounting, so the usage row stays at
                // zero rather than showing a number the agent never sent.
                let _ = event_tx.send(AgentEvent::AgentEnd {
                    usage: TokenUsage::default(),
                });
                Ok(AcpTurnOutcome { reply, tools })
            }
            Err(error) => {
                // A failed turn can leave the connection in an unknown state;
                // the next turn reconnects rather than reusing it.
                self.shutdown().await;
                let _ = event_tx.send(AgentEvent::AgentError {
                    error: error.clone(),
                });
                Err(error)
            }
        }
    }

    /// Opens a session against `agent_id`, reusing the existing one when it
    /// already points at that agent.
    async fn ensure_session(
        &mut self,
        agent_id: &str,
        event_tx: &broadcast::Sender<AgentEvent>,
        permissions: &PermissionHandle,
    ) -> Result<(), String> {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.agent_id == agent_id)
        {
            return Ok(());
        }
        // Switching agents ends the previous conversation; leaving the old
        // subprocess running would leak it for the life of the app.
        self.shutdown().await;

        let (updates_tx, updates_rx) = mpsc::unbounded_channel();
        let handler = AcpWorkspaceClient::new(self.work_dir.clone())
            .with_update_sender(updates_tx)
            .with_permission_responder(permission_responder(event_tx.clone(), permissions.clone()));

        let manager = AcpManager::new(self.global_dir.clone(), Some(self.work_dir.clone()));
        let session = manager
            .start_session(agent_id, &self.work_dir, Arc::new(handler))
            .await
            .map_err(|error| start_failure_message(agent_id, &self.work_dir, &error))?;

        self.active = Some(ActiveSession {
            agent_id: agent_id.to_string(),
            session: Arc::new(session),
            updates: updates_rx,
        });
        Ok(())
    }

    /// Settings the agent exposes for the user to change.
    ///
    /// Two are withheld. Effort, because the reasoning picker already owns it
    /// and re-applies it every turn — a second control would let the two
    /// disagree with the reasoning picker silently winning. And the agent
    /// persona, because routing to a sub-persona is the agent's own concern
    /// and listing every installed one crowds the picker without telling the
    /// user anything about this session.
    ///
    /// Empty until a session exists — the agent sends its settings on
    /// `session/new`, so there is nothing to offer before it connects.
    pub fn user_config_options(&self, agent_id: &str) -> Vec<AcpConfigOption> {
        self.active
            .as_ref()
            .filter(|active| active.agent_id == agent_id)
            .map(|active| active.session.config_options())
            .unwrap_or_default()
            .into_iter()
            .filter(AcpConfigOption::is_user_configurable)
            .collect()
    }

    /// Connects to `agent_id` if it is not already connected, so its settings
    /// can be listed before the first turn.
    ///
    /// Opening the picker is the user asking what this agent offers, and the
    /// only way to find out is to ask the agent, which means starting it.
    pub async fn ensure_connected(
        &mut self,
        agent_id: &str,
        event_tx: &broadcast::Sender<AgentEvent>,
        permissions: &PermissionHandle,
    ) -> Result<Vec<AcpConfigOption>, String> {
        self.ensure_session(agent_id, event_tx, permissions).await?;
        Ok(self.user_config_options(agent_id))
    }

    /// Applies one agent-defined setting, connecting first if needed.
    ///
    /// Returns the settings as the agent reports them afterwards: changing one
    /// can change another, since picking a different model changes which
    /// effort levels that model offers.
    pub async fn set_config_option(
        &mut self,
        agent_id: &str,
        config_id: &str,
        value: &str,
        event_tx: &broadcast::Sender<AgentEvent>,
        permissions: &PermissionHandle,
    ) -> Result<Vec<AcpConfigOption>, String> {
        self.ensure_session(agent_id, event_tx, permissions).await?;
        let session = self
            .active
            .as_ref()
            .map(|active| active.session.clone())
            .ok_or_else(|| format!("ACP agent '{agent_id}' is not connected"))?;
        session.set_config_option_by_id(config_id, value).await?;
        Ok(self.user_config_options(agent_id))
    }

    /// Label the live agent reports for one of its settings, such as the model
    /// it is running.
    ///
    /// Returns `None` until a session exists: the settings come from the agent
    /// on `session/new`, so there is nothing truthful to show before then.
    pub fn config_label(&self, agent_id: &str, category: &str) -> Option<String> {
        self.active
            .as_ref()
            .filter(|active| active.agent_id == agent_id)
            .and_then(|active| active.session.config_label(category))
    }

    /// Model the live agent is running, named as specifically as it reports.
    pub fn model_label(&self, agent_id: &str) -> Option<String> {
        self.active
            .as_ref()
            .filter(|active| active.agent_id == agent_id)
            .and_then(|active| {
                let options = active.session.config_options();
                config_option_for(&options, ACP_CONFIG_CATEGORY_MODEL)
                    .and_then(AcpConfigOption::current_detail_label)
            })
    }

    /// Ends the conversation and stops the agent subprocess.
    pub async fn shutdown(&mut self) {
        if let Some(active) = self.active.take() {
            active.session.shutdown().await;
        }
    }
}

/// How long to wait for an agent to name a session.
///
/// Matches the provider-side title timeout: a title is a convenience, and a
/// slow agent must not keep a subprocess alive indefinitely for one.
const TITLE_TIMEOUT: Duration = Duration::from_secs(30);

const TITLE_INSTRUCTION: &str = "Return only a concise session title, maximum 42 Unicode \
     characters, with no Markdown or explanation. Do not use any tools, read any files, or take \
     any action; answer from the text alone.";

/// Asks an ACP agent to name a session.
///
/// Opens its own short-lived session rather than borrowing the one running the
/// turn: that session is busy, and ACP has no way to interleave a second
/// prompt on one session. The handler refuses filesystem access and cancels
/// every permission request, so naming a session cannot become a side effect —
/// the instruction asks the agent not to act, and this makes it so.
pub async fn generate_title(
    global_dir: Option<PathBuf>,
    work_dir: PathBuf,
    agent_id: &str,
    prompt: &str,
) -> Result<String, String> {
    let (updates_tx, mut updates) = mpsc::unbounded_channel();
    let manager = AcpManager::new(global_dir, Some(work_dir.clone()));
    let session = manager
        .start_session(
            agent_id,
            &work_dir,
            Arc::new(AcpTitleClient {
                updates: updates_tx,
            }),
        )
        .await
        .map_err(|error| start_failure_message(agent_id, &work_dir, &error))?;

    let session_id = session.session_id().to_string();
    let blocks = vec![AcpContentBlock::text(format!(
        "{TITLE_INSTRUCTION}\n\n{prompt}"
    ))];
    let turn = session.prompt(blocks);
    tokio::pin!(turn);

    let mut title = String::new();
    let collected = tokio::time::timeout(TITLE_TIMEOUT, async {
        loop {
            tokio::select! {
                notification = updates.recv() => {
                    if let Some(notification) = notification {
                        append_title_text(&mut title, notification, &session_id);
                    }
                }
                result = &mut turn => break result,
            }
        }
    })
    .await;

    while let Ok(notification) = updates.try_recv() {
        append_title_text(&mut title, notification, &session_id);
    }
    session.shutdown().await;

    match collected {
        Ok(Ok(_)) => Ok(title),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(format!(
            "ACP agent '{agent_id}' did not return a title within {}s",
            TITLE_TIMEOUT.as_secs()
        )),
    }
}

/// Appends an agent message chunk belonging to `session_id` to the title.
///
/// Only message text counts: thoughts and tool output are not a session name.
/// The session filter matters for the same reason it does on a turn — one
/// connection can carry several sessions, and another one's text is not this
/// title.
fn append_title_text(title: &mut String, notification: AcpSessionNotification, session_id: &str) {
    if notification.session_id != session_id {
        return;
    }
    if let AcpSessionUpdate::AgentMessageChunk(block) = notification.update {
        if let Some(text) = block.as_text() {
            title.push_str(text);
        }
    }
}

/// Collects an agent's reply while granting it nothing.
struct AcpTitleClient {
    updates: mpsc::UnboundedSender<AcpSessionNotification>,
}

#[async_trait]
impl AcpClientHandler for AcpTitleClient {
    async fn on_session_update(&self, notification: AcpSessionNotification) {
        let _ = self.updates.send(notification);
    }

    async fn request_permission(&self, _request: AcpPermissionRequest) -> AcpPermissionOutcome {
        AcpPermissionOutcome::Cancelled
    }

    async fn read_text_file(&self, _request: AcpReadTextFileRequest) -> Result<String, String> {
        Err("Filesystem access is not available while naming a session".to_string())
    }

    async fn write_text_file(&self, _request: AcpWriteTextFileRequest) -> Result<(), String> {
        Err("Filesystem access is not available while naming a session".to_string())
    }
}

/// Maps Threadlane's reasoning picker onto the effort values agents use.
///
/// `Off` and `Minimal` have no agent equivalent and map to the lowest level
/// the agent offers rather than being silently ignored; an agent that exposes
/// no effort setting at all rejects the change, which the caller treats as
/// informational.
fn acp_effort_value(effort: ReasoningEffort) -> Option<&'static str> {
    match effort {
        ReasoningEffort::Off | ReasoningEffort::Minimal | ReasoningEffort::Low => Some("low"),
        ReasoningEffort::Medium => Some("medium"),
        ReasoningEffort::High => Some("high"),
        ReasoningEffort::XHigh => Some("xhigh"),
        ReasoningEffort::Max => Some("max"),
    }
}

/// Builds the prompt, attaching images only when the agent takes them.
///
/// An agent that does not advertise image support may reject a prompt
/// containing one, which would lose the user's text as well, so unsupported
/// attachments are named in the text instead of dropped silently.
fn prompt_blocks(
    prompt: &str,
    images: &[ImageAttachment],
    supports_images: bool,
) -> Vec<AcpContentBlock> {
    if images.is_empty() {
        return vec![AcpContentBlock::text(prompt)];
    }
    if !supports_images {
        let names = images
            .iter()
            .map(|image| image.display_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return vec![AcpContentBlock::text(format!(
            "{prompt}\n\n[Attached images could not be sent: this agent does not accept image \
             prompts. Attachments: {names}]"
        ))];
    }

    let mut blocks = vec![AcpContentBlock::text(prompt)];
    let mut unsent = Vec::new();
    for image in images {
        match decode_data_url(&image.data_url) {
            Some((mime_type, data)) => blocks.push(AcpContentBlock::Image { data, mime_type }),
            None => unsent.push(image.display_name.as_str()),
        }
    }
    if !unsent.is_empty() {
        blocks.push(AcpContentBlock::text(format!(
            "[Attached images could not be read: {}]",
            unsent.join(", ")
        )));
    }
    blocks
}

/// Splits a `data:<mime>;base64,<data>` URL into its parts.
///
/// ACP carries the payload and its type separately, and only base64 data URLs
/// can be forwarded without re-encoding.
fn decode_data_url(data_url: &str) -> Option<(String, String)> {
    let rest = data_url.strip_prefix("data:")?;
    let (metadata, data) = rest.split_once(',')?;
    let mime_type = metadata.strip_suffix(";base64")?;
    if mime_type.is_empty() || data.is_empty() {
        return None;
    }
    Some((mime_type.to_string(), data.to_string()))
}

/// Translates one notification into transcript events, accumulating the
/// assistant text into `reply`.
///
/// Notifications for another session are dropped: an agent may run several
/// sessions on one connection, and the other one's output is not this turn's.
fn forward_update(
    notification: AcpSessionNotification,
    session_id: &str,
    event_tx: &broadcast::Sender<AgentEvent>,
    reply: &mut String,
    tools: &mut Vec<AcpTurnToolActivity>,
) {
    if notification.session_id != session_id {
        return;
    }
    for event in agent_events_for(notification.update) {
        match &event {
            AgentEvent::MessageUpdate {
                text_delta: Some(text),
                ..
            } => reply.push_str(text),
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                name,
                arguments,
            } => tools.push(AcpTurnToolActivity {
                tool_call_id: tool_call_id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
                result: None,
            }),
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                ..
            } => {
                if let Some(tool) = tools
                    .iter_mut()
                    .rev()
                    .find(|tool| tool.tool_call_id == *tool_call_id)
                {
                    tool.result = Some(result.clone());
                }
            }
            _ => {}
        }
        let _ = event_tx.send(event);
    }
}

/// Explains a turn that ended for a reason other than finishing.
///
/// `Cancelled` is omitted: the user asked for it and the cancel path already
/// reports it.
fn stop_reason_note(stop_reason: AcpStopReason) -> Option<String> {
    match stop_reason {
        AcpStopReason::EndTurn | AcpStopReason::Cancelled | AcpStopReason::Unknown => None,
        AcpStopReason::MaxTokens => Some("The agent stopped: token limit reached.".into()),
        AcpStopReason::MaxTurnRequests => {
            Some("The agent stopped: it hit its own request limit for this turn.".into())
        }
        AcpStopReason::Refusal => Some("The agent declined to continue this turn.".into()),
    }
}

/// Adds the context a bare protocol error is missing.
///
/// The common failures here are environmental — the command is not installed,
/// or is not on the PATH of a GUI-launched app — and the raw error does not
/// say which agent or directory was involved.
fn start_failure_message(agent_id: &str, work_dir: &Path, error: &str) -> String {
    let mut message = format!(
        "Failed to start ACP agent '{agent_id}' in {}: {error}",
        work_dir.display()
    );
    if error.contains("No such file or directory") || error.contains("program not found") {
        message.push_str(
            "\n\nThe agent's command was not found. Check the command in Settings → ACP Agents; \
             an app launched from the desktop does not inherit a shell PATH, so a version-manager \
             binary such as npx may need an absolute path.",
        );
    }
    message
}

/// Routes an agent's permission request to the user.
fn permission_responder(
    event_tx: broadcast::Sender<AgentEvent>,
    permissions: PermissionHandle,
) -> crate::acp::AcpPermissionResponder {
    Arc::new(move |request: AcpPermissionRequest| {
        let event_tx = event_tx.clone();
        let permissions = permissions.clone();
        Box::pin(async move {
            let decision = permissions
                .request_external(
                    &event_tx,
                    "acp",
                    permission_title(request.tool_call()),
                    permission_detail(request.tool_call()),
                    request.offers_allow_always(),
                )
                .await;
            select_option(&request, decision)
        })
    })
}

/// Maps the user's answer onto one of the options the agent offered.
///
/// Option ids are the agent's own, so a decision can only be expressed with an
/// id it actually sent. When it offered nothing matching, the request is
/// cancelled rather than answered with a guess — silently substituting an
/// allow for a deny would be the worst possible failure here.
fn select_option(
    request: &AcpPermissionRequest,
    decision: PermissionDecision,
) -> AcpPermissionOutcome {
    let preferred: &[AcpPermissionOptionKind] = match decision {
        PermissionDecision::AllowAlways => &[
            AcpPermissionOptionKind::AllowAlways,
            AcpPermissionOptionKind::AllowOnce,
        ],
        PermissionDecision::AllowOnce => &[AcpPermissionOptionKind::AllowOnce],
        PermissionDecision::Deny => &[
            AcpPermissionOptionKind::RejectOnce,
            AcpPermissionOptionKind::RejectAlways,
        ],
    };
    for kind in preferred {
        if let Some(option) = request.option_for(*kind) {
            return AcpPermissionOutcome::Selected {
                option_id: option.option_id().to_string(),
            };
        }
    }
    AcpPermissionOutcome::Cancelled
}

fn permission_title(tool_call: Option<&AcpToolCall>) -> String {
    tool_call
        .and_then(|call| call.title.as_deref())
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(|title| title.to_string())
        .unwrap_or_else(|| "The agent is requesting permission".to_string())
}

/// Shows what is actually being approved.
///
/// The title is the agent's own summary; the raw input is the part a user
/// needs in order to judge a command or a path.
fn permission_detail(tool_call: Option<&AcpToolCall>) -> String {
    let Some(call) = tool_call else {
        return "The agent did not describe this request.".to_string();
    };
    match call.raw_input.as_ref() {
        Some(input) => serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string()),
        None => call
            .title
            .clone()
            .unwrap_or_else(|| "No further detail provided.".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(value: serde_json::Value) -> AcpPermissionRequest {
        serde_json::from_value(value).unwrap()
    }

    fn options() -> serde_json::Value {
        json!([
            { "optionId": "yes", "name": "Allow", "kind": "allow_once" },
            { "optionId": "yes-always", "name": "Always allow", "kind": "allow_always" },
            { "optionId": "no", "name": "Deny", "kind": "reject_once" },
        ])
    }

    #[test]
    fn a_decision_selects_the_agents_own_option_id() {
        let request = request(json!({ "sessionId": "s1", "options": options() }));

        assert_eq!(
            select_option(&request, PermissionDecision::AllowOnce),
            AcpPermissionOutcome::Selected {
                option_id: "yes".into()
            }
        );
        assert_eq!(
            select_option(&request, PermissionDecision::AllowAlways),
            AcpPermissionOutcome::Selected {
                option_id: "yes-always".into()
            }
        );
        assert_eq!(
            select_option(&request, PermissionDecision::Deny),
            AcpPermissionOutcome::Selected {
                option_id: "no".into()
            }
        );
    }

    #[test]
    fn allow_always_falls_back_to_allowing_once() {
        let request = request(json!({
            "sessionId": "s1",
            "options": [
                { "optionId": "yes", "name": "Allow", "kind": "allow_once" },
                { "optionId": "no", "name": "Deny", "kind": "reject_once" },
            ],
        }));
        assert_eq!(
            select_option(&request, PermissionDecision::AllowAlways),
            AcpPermissionOutcome::Selected {
                option_id: "yes".into()
            }
        );
    }

    #[test]
    fn a_denial_never_falls_back_to_an_allow() {
        // An agent that offers no way to say no must not have "yes" inferred.
        let request = request(json!({
            "sessionId": "s1",
            "options": [{ "optionId": "yes", "name": "Allow", "kind": "allow_once" }],
        }));
        assert_eq!(
            select_option(&request, PermissionDecision::Deny),
            AcpPermissionOutcome::Cancelled
        );
    }

    #[test]
    fn unknown_option_kinds_cancel_rather_than_guess() {
        let request = request(json!({
            "sessionId": "s1",
            "options": [{ "optionId": "maybe", "name": "Hmm", "kind": "something_new" }],
        }));
        assert_eq!(
            select_option(&request, PermissionDecision::AllowOnce),
            AcpPermissionOutcome::Cancelled
        );
    }

    #[test]
    fn a_prompt_describes_the_call_it_is_approving() {
        let call: AcpToolCall = serde_json::from_value(json!({
            "toolCallId": "call_1",
            "title": "Run `rm -rf build`",
            "kind": "execute",
            "rawInput": { "command": "rm -rf build" },
        }))
        .unwrap();

        assert_eq!(permission_title(Some(&call)), "Run `rm -rf build`");
        assert!(permission_detail(Some(&call)).contains("rm -rf build"));
    }

    #[test]
    fn a_request_without_a_tool_call_still_prompts() {
        assert_eq!(permission_title(None), "The agent is requesting permission");
        assert!(!permission_detail(None).is_empty());
    }

    fn image(name: &str, data_url: &str) -> ImageAttachment {
        ImageAttachment {
            display_name: name.into(),
            data_url: data_url.into(),
        }
    }

    #[test]
    fn a_text_only_prompt_is_a_single_block() {
        assert_eq!(
            prompt_blocks("hello", &[], true),
            vec![AcpContentBlock::text("hello")]
        );
    }

    #[test]
    fn images_are_split_into_data_and_mime_type() {
        let blocks = prompt_blocks(
            "what is this",
            &[image("shot.png", "data:image/png;base64,AAAA")],
            true,
        );
        assert_eq!(
            blocks,
            vec![
                AcpContentBlock::text("what is this"),
                AcpContentBlock::Image {
                    data: "AAAA".into(),
                    mime_type: "image/png".into(),
                },
            ]
        );
    }

    #[test]
    fn an_agent_without_image_support_still_gets_the_text() {
        let blocks = prompt_blocks(
            "what is this",
            &[image("shot.png", "data:image/png;base64,AAAA")],
            false,
        );
        let [AcpContentBlock::Text { text }] = blocks.as_slice() else {
            panic!("expected the prompt to degrade to one text block, got {blocks:?}");
        };
        // Losing the question along with the attachment would be the worst
        // outcome; the user must at least get an answer and an explanation.
        assert!(text.starts_with("what is this"));
        assert!(text.contains("shot.png"));
        assert!(text.contains("does not accept image"));
    }

    #[test]
    fn an_unreadable_attachment_is_named_rather_than_dropped() {
        let blocks = prompt_blocks(
            "look",
            &[
                image("good.png", "data:image/png;base64,AAAA"),
                image("bad.png", "https://example.com/bad.png"),
            ],
            true,
        );
        assert_eq!(blocks.len(), 3);
        assert!(matches!(blocks[1], AcpContentBlock::Image { .. }));
        let AcpContentBlock::Text { text } = &blocks[2] else {
            panic!("expected a trailing note, got {:?}", blocks[2]);
        };
        assert!(text.contains("bad.png"));
        assert!(!text.contains("good.png"));
    }

    #[test]
    fn only_base64_data_urls_decode() {
        assert_eq!(
            decode_data_url("data:image/jpeg;base64,ZZZ"),
            Some(("image/jpeg".into(), "ZZZ".into()))
        );
        // Percent-encoded and remote sources cannot be forwarded as-is.
        assert_eq!(decode_data_url("data:image/png,raw"), None);
        assert_eq!(decode_data_url("https://example.com/a.png"), None);
        assert_eq!(decode_data_url("data:image/png;base64,"), None);
        assert_eq!(decode_data_url("data:;base64,AAAA"), None);
    }

    #[test]
    fn only_abnormal_stops_are_reported() {
        assert!(stop_reason_note(AcpStopReason::EndTurn).is_none());
        assert!(stop_reason_note(AcpStopReason::Cancelled).is_none());
        assert!(stop_reason_note(AcpStopReason::Refusal).is_some());
        assert!(stop_reason_note(AcpStopReason::MaxTokens).is_some());
    }

    #[test]
    fn forwarded_tool_activity_is_retained_for_durable_journaling() {
        let (event_tx, _) = broadcast::channel(8);
        let mut reply = String::new();
        let mut tools = Vec::new();
        let start = serde_json::from_value::<AcpToolCall>(json!({
            "toolCallId": "call-1",
            "title": "Read main.rs",
            "kind": "read",
            "rawInput": { "path": "src/main.rs" }
        }))
        .unwrap();
        forward_update(
            AcpSessionNotification {
                session_id: "session-1".into(),
                update: AcpSessionUpdate::ToolCall(start),
            },
            "session-1",
            &event_tx,
            &mut reply,
            &mut tools,
        );
        let finish = serde_json::from_value::<AcpToolCall>(json!({
            "toolCallId": "call-1",
            "title": "Read main.rs",
            "status": "completed",
            "content": [{
                "type": "content",
                "content": { "type": "text", "text": "fn main() {}" }
            }]
        }))
        .unwrap();
        forward_update(
            AcpSessionNotification {
                session_id: "session-1".into(),
                update: AcpSessionUpdate::ToolCallUpdate(finish),
            },
            "session-1",
            &event_tx,
            &mut reply,
            &mut tools,
        );

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_call_id, "call-1");
        assert_eq!(tools[0].arguments, r#"{"path":"src/main.rs"}"#);
        let result = tools[0].result.as_ref().expect("terminal tool result");
        assert_eq!(result.content, "fn main() {}");
        assert!(!result.is_error);
    }

    #[test]
    fn a_missing_binary_explains_the_path_problem() {
        let message = start_failure_message(
            "claude_code",
            Path::new("/work"),
            "No such file or directory (os error 2)",
        );
        assert!(message.contains("claude_code"));
        assert!(message.contains("/work"));
        assert!(message.contains("absolute path"));

        // An unrelated failure must not get the PATH advice bolted onto it.
        let other = start_failure_message("claude_code", Path::new("/work"), "handshake timed out");
        assert!(!other.contains("absolute path"));
    }
}
