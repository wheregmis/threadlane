//! What a hub offers its clients (Plan.md §11.2).
//!
//! [`HubApi`] describes one space and has no notion of any other. The hub
//! implements it; a transport carries it to clients as [`ClientRequest`] and
//! [`ClientResponse`]. Later milestones add capabilities; so far: who am I
//! talking to, send a message, follow the feed, the organization, and pairing.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::admission::AdmittedKey;
use crate::ids::{EventId, ProjectId};
use crate::model::{Agent, Mode, OutboxMessage};
use crate::time::Timestamp;

/// What a hub says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubInfo {
    /// The space's name, for example `Work`.
    pub space: String,
    pub mode_ceiling: Mode,
    pub schema_version: i64,
}

/// A project as a client lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: ProjectId,
    pub name: String,
    pub is_general: bool,
    pub paused: bool,
}

/// A message from you to a project's lead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMessage {
    /// The project's name. `None` means the General project.
    pub project: Option<String>,
    pub text: String,
    /// Chosen by the client, unique per message. Sending the same id twice
    /// is a successful no-op, so a client may retry freely.
    pub client_message_id: String,
}

/// The hub's acknowledgement of a [`UserMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub event_id: EventId,
    pub project_id: ProjectId,
    /// `true` if this `client_message_id` had been accepted before.
    pub duplicate: bool,
}

/// One delivered message or decision card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedItem {
    /// Position in the feed. Pass the last one seen as `after`.
    pub seq: i64,
    pub message: OutboxMessage,
    pub project_name: Option<String>,
    pub delivered_at: Timestamp,
}

/// An agent's binding to a project as a client lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingSummary {
    /// The agent's name.
    pub agent: String,
    pub role: String,
    pub mode: Mode,
    pub is_lead: bool,
}

/// The Organization capability (Plan.md §11.2): projects, agents, bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OrgRequest {
    /// Creates a project with its lead lane and lead binding.
    CreateProject { name: String, description: String },
    /// Every agent in the space.
    Agents,
    CreateAgent {
        name: String,
        role: String,
        instructions: String,
    },
    /// The bindings of a project. `None` means the General project.
    Bindings { project: Option<String> },
    /// Sets the agent's mode in a project, binding it there if it is not yet.
    /// `None` means the General project.
    SetMode {
        project: Option<String>,
        agent: String,
        mode: Mode,
    },
}

/// The answer to an [`OrgRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OrgReply {
    Project { project: ProjectSummary },
    Agents { agents: Vec<Agent> },
    Agent { agent: Agent },
    Bindings { bindings: Vec<BindingSummary> },
    Binding { binding: BindingSummary },
}

/// The Pairing capability (Plan.md §11.2): join codes and admitted clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PairingRequest {
    /// A new one-time join code for a client, valid for a few minutes.
    IssueJoinCode,
    /// Every admitted key, revoked ones included.
    Clients,
    /// Revokes the active key of this name and closes its connections.
    Revoke { name: String },
}

/// The answer to a [`PairingRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PairingReply {
    /// The code to give the new client. It carries the hub's id, how to reach
    /// it, and a one-time secret.
    JoinCode {
        code: String,
        expires_at: Timestamp,
    },
    Clients {
        clients: Vec<AdmittedKey>,
    },
    Revoked {
        client: AdmittedKey,
    },
}

/// Why a client request failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApiError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid request: {0}")]
    Invalid(String),
    /// The hub cannot be reached; the client shows its last known state.
    #[error("hub unreachable: {0}")]
    Unreachable(String),
    #[error("hub error: {0}")]
    Internal(String),
}

/// The hub interface for clients.
#[async_trait]
pub trait HubApi: Send + Sync + fmt::Debug {
    async fn info(&self) -> Result<HubInfo, ApiError>;
    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError>;
    /// Accepts a message: route, insert with deduplication, wake, acknowledge
    /// (Plan.md §6.3). Returns as soon as the event is durable.
    async fn send_message(&self, message: &UserMessage) -> Result<Accepted, ApiError>;
    /// Feed items with `seq` greater than `after`, oldest first. If there are
    /// none, waits up to `wait` for the next one, then returns what there is,
    /// possibly nothing.
    async fn feed(&self, after: i64, wait: Duration) -> Result<Vec<FeedItem>, ApiError>;
    /// Reads or changes the space's projects, agents, and bindings.
    async fn org(&self, request: OrgRequest) -> Result<OrgReply, ApiError>;
    /// Issues join codes, lists admitted keys, revokes them.
    async fn pairing(&self, request: PairingRequest) -> Result<PairingReply, ApiError>;
}

/// [`HubApi`] as it crosses a transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClientRequest {
    Info,
    Projects,
    SendMessage { message: UserMessage },
    Feed { after: i64, wait_ms: u64 },
    Org { request: OrgRequest },
    Pairing { request: PairingRequest },
}

/// The answer to a [`ClientRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClientResponse {
    Info { info: HubInfo },
    Projects { projects: Vec<ProjectSummary> },
    Accepted { accepted: Accepted },
    Feed { items: Vec<FeedItem> },
    Org { reply: OrgReply },
    Pairing { reply: PairingReply },
    Error { error: ApiError },
}

/// Answers one request from an API. Shared by every transport's server side.
pub async fn dispatch(api: &dyn HubApi, request: ClientRequest) -> ClientResponse {
    let result = match request {
        ClientRequest::Info => api.info().await.map(|info| ClientResponse::Info { info }),
        ClientRequest::Projects => api
            .projects()
            .await
            .map(|projects| ClientResponse::Projects { projects }),
        ClientRequest::SendMessage { message } => api
            .send_message(&message)
            .await
            .map(|accepted| ClientResponse::Accepted { accepted }),
        ClientRequest::Feed { after, wait_ms } => api
            .feed(after, Duration::from_millis(wait_ms))
            .await
            .map(|items| ClientResponse::Feed { items }),
        ClientRequest::Org { request } => api
            .org(request)
            .await
            .map(|reply| ClientResponse::Org { reply }),
        ClientRequest::Pairing { request } => api
            .pairing(request)
            .await
            .map(|reply| ClientResponse::Pairing { reply }),
    };
    result.unwrap_or_else(|error| ClientResponse::Error { error })
}
