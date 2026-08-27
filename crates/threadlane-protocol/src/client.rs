use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, oneshot, Mutex};

use crate::capabilities::*;
use crate::git::*;
use crate::permission::*;
use crate::project::*;
use crate::rpc::*;
use crate::session::*;
use crate::settings::*;
use crate::terminal::*;
use crate::update::*;

pub struct DaemonClient {
    next_id: AtomicU64,
    out_tx: tokio::sync::mpsc::Sender<String>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResponse>>>>,
    session_events: broadcast::Sender<SessionEvent>,
    terminal_events: broadcast::Sender<TerminalOutputEvent>,
}

impl DaemonClient {
    /// Connects to the daemon over a Unix Domain Socket.
    pub async fn connect_uds(socket_path: &Path) -> Result<Self, String> {
        let stream = UnixStream::connect(socket_path)
            .await
            .map_err(|e| format!("Failed to connect to daemon at {}: {e}", socket_path.display()))?;

        let (reader, mut writer) = stream.into_split();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(128);
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (session_events, _) = broadcast::channel(1024);
        let (terminal_events, _) = broadcast::channel(1024);

        // Background writer pump
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if writer.write_all(msg.as_bytes()).await.is_err() {
                    break;
                }
                if writer.write_all(b"\n").await.is_err() {
                    break;
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
        });

        // Background reader pump
        let pending_clone = pending.clone();
        let session_events_clone = session_events.clone();
        let terminal_events_clone = terminal_events.clone();

        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                // Check if notification or response
                if let Ok(notif) = serde_json::from_str::<RpcNotification>(line) {
                    if notif.method == "session/event" {
                        if let Some(params) = notif.params {
                            if let Ok(event) = serde_json::from_value::<SessionEvent>(params) {
                                let _ = session_events_clone.send(event);
                            }
                        }
                    } else if notif.method == "terminal/event" {
                        if let Some(params) = notif.params {
                            if let Ok(event) = serde_json::from_value::<TerminalOutputEvent>(params) {
                                let _ = terminal_events_clone.send(event);
                            }
                        }
                    }
                } else if let Ok(res) = serde_json::from_str::<RpcResponse>(line) {
                    if let RequestId::Number(id) = res.id {
                        let mut lock = pending_clone.lock().await;
                        if let Some(sender) = lock.remove(&id) {
                            let _ = sender.send(res);
                        }
                    }
                }
            }
        });

        Ok(Self {
            next_id: AtomicU64::new(1),
            out_tx,
            pending,
            session_events,
            terminal_events,
        })
    }

    /// Generic request/response dispatch.
    pub async fn request<Req: Serialize, Res: DeserializeOwned>(
        &self,
        method: &str,
        params: Req,
    ) -> Result<Res, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let params_val = serde_json::to_value(params).map_err(|e| e.to_string())?;
        let req = RpcRequest::new(id, method, Some(params_val));
        let json_str = serde_json::to_string(&req).map_err(|e| e.to_string())?;

        let (resp_tx, resp_rx) = oneshot::channel();
        {
            let mut lock = self.pending.lock().await;
            lock.insert(id, resp_tx);
        }

        self.out_tx
            .send(json_str)
            .await
            .map_err(|e| format!("Failed to send request: {e}"))?;

        let response = resp_rx
            .await
            .map_err(|_| "Daemon closed connection before responding".to_string())?;

        if let Some(error) = response.error {
            return Err(format!("RPC error [{}]: {}", error.code, error.message));
        }

        let result = response.result.unwrap_or(Value::Null);
        serde_json::from_value(result).map_err(|e| format!("Failed to deserialize response: {e}"))
    }

    // ── Typed API Helpers ──────────────────────────────────────────────────

    pub async fn get_daemon_info(&self) -> Result<DaemonInfoResponse, String> {
        self.request("daemon/info", Value::Null).await
    }

    pub async fn list_models(&self) -> Result<ListModelsResponse, String> {
        self.request("capabilities/models", Value::Null).await
    }

    pub async fn list_projects(&self) -> Result<ListProjectsResponse, String> {
        self.request("project/list", Value::Null).await
    }

    pub async fn register_project(&self, path: &str) -> Result<ProjectRecord, String> {
        self.request(
            "project/register",
            RegisterProjectRequest {
                path: path.to_string(),
            },
        )
        .await
    }

    pub async fn create_session(
        &self,
        req: CreateSessionRequest,
    ) -> Result<SessionSummary, String> {
        self.request("session/create", req).await
    }

    pub async fn list_sessions(
        &self,
        project_path: &str,
    ) -> Result<Vec<SessionSummary>, String> {
        self.request(
            "session/list",
            ListSessionsRequest {
                project_path: project_path.to_string(),
            },
        )
        .await
    }

    pub async fn list_session_infos(
        &self,
        project_path: &str,
    ) -> Result<Vec<SessionInfo>, String> {
        self.request(
            "session/list_infos",
            ListSessionsRequest {
                project_path: project_path.to_string(),
            },
        )
        .await
    }

    pub async fn hydrate_session(
        &self,
        req: HydrateSessionRequest,
    ) -> Result<HydrateSessionResponse, String> {
        self.request("session/hydrate", req).await
    }

    pub async fn archive_session(
        &self,
        req: ArchiveSessionRequest,
    ) -> Result<(), String> {
        self.request("session/archive", req).await
    }

    pub async fn delete_session(
        &self,
        req: DeleteSessionRequest,
    ) -> Result<(), String> {
        self.request("session/delete", req).await
    }

    pub async fn send_prompt(
        &self,
        req: SendPromptRequest,
    ) -> Result<PromptAcceptedResponse, String> {
        self.request("session/send_prompt", req).await
    }

    pub async fn cancel_run(&self, session_id: &str) -> Result<(), String> {
        self.request(
            "session/cancel",
            CancelRunRequest {
                session_id: session_id.to_string(),
            },
        )
        .await
    }

    pub async fn submit_permission(
        &self,
        req: SubmitPermissionRequest,
    ) -> Result<(), String> {
        self.request("session/submit_permission", req).await
    }

    pub async fn queue_follow_up(
        &self,
        req: crate::session::QueueFollowUpRequest,
    ) -> Result<(), String> {
        self.request("session/queue_follow_up", req).await
    }

    pub async fn queue_steer(
        &self,
        req: crate::session::QueueSteerRequest,
    ) -> Result<(), String> {
        self.request("session/queue_steer", req).await
    }

    pub async fn spawn_terminal(
        &self,
        req: SpawnTerminalRequest,
    ) -> Result<TerminalSpawnedResponse, String> {
        self.request("terminal/spawn", req).await
    }

    pub async fn write_terminal_input(
        &self,
        terminal_id: &str,
        data: &str,
    ) -> Result<(), String> {
        self.request(
            "terminal/input",
            TerminalInputRequest {
                terminal_id: terminal_id.to_string(),
                data: data.to_string(),
            },
        )
        .await
    }

    pub async fn git_status(&self, project_path: &str) -> Result<GitStatusResponse, String> {
        self.request(
            "git/status",
            GitStatusRequest {
                project_path: project_path.to_string(),
            },
        )
        .await
    }

    pub fn subscribe_session_events(&self) -> broadcast::Receiver<SessionEvent> {
        self.session_events.subscribe()
    }

    pub fn subscribe_terminal_events(&self) -> broadcast::Receiver<TerminalOutputEvent> {
        self.terminal_events.subscribe()
    }

    // ── Skills ─────────────────────────────────────────────────────────────

    pub async fn list_skills(
        &self,
        req: ListSkillsRequest,
    ) -> Result<ListSkillsResponse, String> {
        self.request("capabilities/skills", req).await
    }

    pub async fn toggle_skill(&self, req: ToggleSkillRequest) -> Result<(), String> {
        self.request("capabilities/toggle_skill", req).await
    }

    // ── ACP Agent CRUD ─────────────────────────────────────────────────────

    pub async fn list_acp_agents(
        &self,
        req: ListAcpAgentsRequest,
    ) -> Result<ListAcpAgentsResponse, String> {
        self.request("capabilities/acp/list", req).await
    }

    pub async fn add_acp_agent(&self, req: AddAcpAgentRequest) -> Result<(), String> {
        self.request("capabilities/acp/add", req).await
    }

    pub async fn set_acp_enabled(&self, req: SetAcpEnabledRequest) -> Result<(), String> {
        self.request("capabilities/acp/set_enabled", req).await
    }

    pub async fn remove_acp_agent(&self, req: RemoveAcpAgentRequest) -> Result<(), String> {
        self.request("capabilities/acp/remove", req).await
    }

    // ── Provider Auth ──────────────────────────────────────────────────────

    pub async fn get_provider_auth(
        &self,
        req: GetProviderAuthRequest,
    ) -> Result<ProviderAuthStatusResponse, String> {
        self.request("auth/status", req).await
    }

    pub async fn connect_provider(
        &self,
        req: ConnectProviderRequest,
    ) -> Result<ConnectProviderResponse, String> {
        self.request("auth/connect", req).await
    }

    pub async fn disconnect_provider(&self, req: DisconnectProviderRequest) -> Result<(), String> {
        self.request("auth/disconnect", req).await
    }

    // ── Settings ───────────────────────────────────────────────────────────

    pub async fn get_needle_enabled(&self) -> Result<GetNeedleEnabledResponse, String> {
        self.request("settings/needle_get", serde_json::Value::Null)
            .await
    }

    pub async fn set_needle_enabled(&self, req: SetNeedleEnabledRequest) -> Result<(), String> {
        self.request("settings/needle_set", req).await
    }

    pub async fn get_subagent_settings(
        &self,
        req: GetSubagentSettingsRequest,
    ) -> Result<SubagentSettingsData, String> {
        self.request("settings/subagent_get", req).await
    }

    pub async fn set_subagent_settings(
        &self,
        req: SetSubagentSettingsRequest,
    ) -> Result<(), String> {
        self.request("settings/subagent_set", req).await
    }

    // ── Updater ────────────────────────────────────────────────────────────

    pub async fn check_for_update(&self) -> Result<CheckForUpdateResponse, String> {
        self.request("update/check", serde_json::Value::Null).await
    }

    pub async fn download_update(&self, req: DownloadUpdateRequest) -> Result<(), String> {
        self.request("update/download", req).await
    }

    pub async fn install_update(&self, req: InstallUpdateRequest) -> Result<(), String> {
        self.request("update/install", req).await
    }

    // ── Session title generation ───────────────────────────────────────────

    pub async fn generate_title(
        &self,
        req: GenerateTitleRequest,
    ) -> Result<GenerateTitleResponse, String> {
        self.request("session/generate_title", req).await
    }

    // ── Git extended ops ───────────────────────────────────────────────────

    pub async fn git_stage_file(&self, req: GitStageFileRequest) -> Result<(), String> {
        self.request("git/stage_file", req).await
    }

    pub async fn git_commit(&self, req: GitCommitRequest) -> Result<GitCommitResponse, String> {
        self.request("git/commit", req).await
    }

    pub async fn git_push(&self, req: GitPushPullRequest) -> Result<(), String> {
        self.request("git/push", req).await
    }

    pub async fn git_pull(&self, req: GitPushPullRequest) -> Result<(), String> {
        self.request("git/pull", req).await
    }

    pub async fn git_discard_file(&self, req: GitDiscardFileRequest) -> Result<(), String> {
        self.request("git/discard_file", req).await
    }

    pub async fn git_ignore(&self, req: GitIgnoreRequest) -> Result<(), String> {
        self.request("git/ignore", req).await
    }

    pub async fn git_merge(&self, req: GitMergeRequest) -> Result<(), String> {
        self.request("git/merge", req).await
    }

    pub async fn git_pop_stash(&self, req: GitStashActionRequest) -> Result<(), String> {
        self.request("git/stash_pop", req).await
    }

    pub async fn git_drop_stash(&self, req: GitStashActionRequest) -> Result<(), String> {
        self.request("git/stash_drop", req).await
    }

    pub async fn git_commit_diff_message(
        &self,
        project_path: &str,
    ) -> Result<GitCommitDiffMessageResponse, String> {
        self.request(
            "git/commit_diff_message",
            serde_json::json!({ "project_path": project_path }),
        )
        .await
    }
}
