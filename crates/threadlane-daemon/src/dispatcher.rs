use crate::services::*;
use serde_json::Value;
use threadlane_protocol::rpc::*;

#[derive(Clone, Default)]
pub struct RpcDispatcher {
    pub session_service: SessionService,
    pub terminal_service: TerminalService,
    pub project_service: ProjectService,
    pub git_service: GitService,
    pub task_service: TaskService,
    pub capabilities_service: CapabilitiesService,
}

impl RpcDispatcher {
    pub fn new() -> Self {
        Self {
            session_service: SessionService::new(),
            terminal_service: TerminalService::new(),
            project_service: ProjectService::new(),
            git_service: GitService::new(),
            task_service: TaskService::new(),
            capabilities_service: CapabilitiesService::new(),
        }
    }

    pub async fn dispatch(&self, request: RpcRequest) -> RpcResponse {
        let id = request.id.clone();
        let method = request.method.as_str();
        let params = request.params.unwrap_or(Value::Null);

        let result = match method {
            // ── Session Methods ──────────────────────────────────────────
            "session/create" => match serde_json::from_value(params) {
                Ok(req) => self
                    .session_service
                    .create_session(req)
                    .await
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "session/list" => match serde_json::from_value(params) {
                Ok(req) => self
                    .session_service
                    .list_sessions(req)
                    .await
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "session/get" => match serde_json::from_value(params) {
                Ok(req) => self
                    .session_service
                    .get_session(req)
                    .await
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "session/delete" => match serde_json::from_value(params) {
                Ok(req) => self
                    .session_service
                    .delete_session(req)
                    .await
                    .map(|_| Value::Null),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "session/send_prompt" => match serde_json::from_value(params) {
                Ok(req) => self
                    .session_service
                    .send_prompt(req)
                    .await
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "session/cancel" => match serde_json::from_value(params) {
                Ok(req) => self
                    .session_service
                    .cancel_run(req)
                    .await
                    .map(|_| Value::Null),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "session/set_model" => match serde_json::from_value(params) {
                Ok(req) => self
                    .session_service
                    .set_model(req)
                    .await
                    .map(|_| Value::Null),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "session/submit_permission" => match serde_json::from_value(params) {
                Ok(req) => self
                    .session_service
                    .submit_permission_decision(req)
                    .await
                    .map(|_| Value::Null),
                Err(e) => Err(format!("Invalid params: {e}")),
            },

            // ── Terminal Methods ─────────────────────────────────────────
            "terminal/spawn" => match serde_json::from_value(params) {
                Ok(req) => self
                    .terminal_service
                    .spawn_terminal(req)
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "terminal/input" => match serde_json::from_value(params) {
                Ok(req) => self
                    .terminal_service
                    .write_input(req)
                    .map(|_| Value::Null),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "terminal/resize" => match serde_json::from_value(params) {
                Ok(req) => self
                    .terminal_service
                    .resize_terminal(req)
                    .map(|_| Value::Null),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "terminal/close" => match serde_json::from_value(params) {
                Ok(req) => self
                    .terminal_service
                    .close_terminal(req)
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },

            // ── Project & Workspace Methods ──────────────────────────────
            "project/list" => self
                .project_service
                .list_projects()
                .map(|res| serde_json::to_value(res).unwrap()),
            "project/register" => match serde_json::from_value(params) {
                Ok(req) => self
                    .project_service
                    .register_project(req)
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "project/list_dir" => match serde_json::from_value(params) {
                Ok(req) => self
                    .project_service
                    .list_directory(req)
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "project/read_file" => match serde_json::from_value(params) {
                Ok(req) => self
                    .project_service
                    .read_file(req)
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "project/write_file" => match serde_json::from_value(params) {
                Ok(req) => self
                    .project_service
                    .write_file(req)
                    .map(|_| Value::Null),
                Err(e) => Err(format!("Invalid params: {e}")),
            },

            // ── Git Methods ──────────────────────────────────────────────
            "git/status" => match serde_json::from_value(params) {
                Ok(req) => self
                    .git_service
                    .status(req)
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "git/diff" => match serde_json::from_value(params) {
                Ok(req) => self
                    .git_service
                    .diff(req)
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "git/branches" => match serde_json::from_value(params) {
                Ok(req) => self
                    .git_service
                    .list_branches(req)
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "git/checkout" => match serde_json::from_value(params) {
                Ok(req) => self
                    .git_service
                    .checkout(req)
                    .map(|_| Value::Null),
                Err(e) => Err(format!("Invalid params: {e}")),
            },

            // ── Tasks Methods ────────────────────────────────────────────
            "tasks/list" => match serde_json::from_value(params) {
                Ok(req) => self
                    .task_service
                    .list_tasks(req)
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "tasks/start" => match serde_json::from_value(params) {
                Ok(req) => self
                    .task_service
                    .start_task(req)
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "tasks/cancel" => match serde_json::from_value(params) {
                Ok(req) => self
                    .task_service
                    .cancel_task(req)
                    .map(|_| Value::Null),
                Err(e) => Err(format!("Invalid params: {e}")),
            },

            // ── Capabilities Methods ─────────────────────────────────────
            "capabilities/models" => self
                .capabilities_service
                .list_models()
                .map(|res| serde_json::to_value(res).unwrap()),
            "capabilities/skills" => match serde_json::from_value(params) {
                Ok(req) => self
                    .capabilities_service
                    .list_skills(req)
                    .map(|res| serde_json::to_value(res).unwrap()),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "capabilities/toggle_skill" => match serde_json::from_value(params) {
                Ok(req) => self
                    .capabilities_service
                    .toggle_skill(req)
                    .map(|_| Value::Null),
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "daemon/info" => self
                .capabilities_service
                .get_daemon_info()
                .map(|res| serde_json::to_value(res).unwrap()),

            _ => Err(format!("Method '{method}' not found")),
        };

        match result {
            Ok(value) => RpcResponse::success(id, value),
            Err(err) => RpcResponse::error(id, RpcError::new(ERROR_INTERNAL_ERROR, err)),
        }
    }
}
