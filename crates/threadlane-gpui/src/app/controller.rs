use super::actions::AppAction;
use crate::state::{AppState, SessionHydrationRequest};

/// Application intent boundary used by screens.
///
/// The controller is intentionally small for now. Keeping actions in one place
/// gives us a stable seam for moving backend work out of `AppState` incrementally.
pub(crate) fn dispatch(state: &mut AppState, action: AppAction) -> Option<SessionHydrationRequest> {
    match action {
        AppAction::AttachProject(path) => {
            if let Err(error) = state.attach_project(path) {
                state.session_status = Some(error);
            }
        }
        AppAction::SelectSession {
            work_dir,
            session_id,
        } => return Some(state.select_session(work_dir, session_id)),
        AppAction::SettleSession {
            work_dir,
            session_id,
        } => {
            if let Err(error) = state.settle_session(work_dir, session_id) {
                state.session_status = Some(error);
            }
        }
        AppAction::RemoveSession {
            work_dir,
            session_id,
        } => {
            if let Err(error) = state.remove_session(work_dir, session_id) {
                state.session_status = Some(error);
            }
        }
        AppAction::ToggleProject(path) => state.toggle_project_expanded(&path),
        AppAction::SetSidebarProjectFilter(work_dir) => state.set_sidebar_project_filter(work_dir),
        AppAction::BeginNewTask => state.begin_new_task(),
        AppAction::SelectDraftProject(path) => state.select_draft_project(path),
        AppAction::SelectWorkMode(mode) => state.set_work_mode(mode),
        AppAction::StartIssueWork {
            work_dir,
            issue,
            title,
        } => {
            if let Err(error) = state.start_issue_work(work_dir, issue, title) {
                state.session_status = Some(error);
            }
        }
        AppAction::SendPrompt(text) => {
            if let Err(error) = state.send_prompt(text) {
                state.session_status = Some(error);
            }
        }
        AppAction::SendPromptWithImages { text, images } => {
            if let Err(error) = state.send_prompt_with_images(text, images) {
                state.session_status = Some(error);
            }
        }
        AppAction::StageBusyMessage { text, images } => {
            if let Err(error) = state.stage_busy_message(text, images) {
                state.session_status = Some(error);
            }
        }
        AppAction::QueuePendingMessage => {
            let _ = state.queue_pending_message();
        }
        AppAction::SteerPendingMessage => {
            let _ = state.steer_pending_message();
        }
        AppAction::DismissPendingMessage => state.dismiss_pending_message(),
        AppAction::ToggleToolActivity(tool_call_id) => state.toggle_tool_activity(&tool_call_id),
        AppAction::CancelGeneration => {
            if let Err(error) = state.cancel_generation() {
                state.session_status = Some(error);
            }
        }
        AppAction::SelectModel(model) => state.set_selected_model(model),
        AppAction::SelectReasoningEffort(effort) => state.set_reasoning_effort(effort),
        AppAction::SetAcpConfigOption { config_id, value } => {
            state.set_acp_config_option(config_id, value)
        }
        AppAction::OpenGitHub => state.open_github(),
        AppAction::CloseGitHub => state.close_github(),
        AppAction::OpenSettings => state.open_settings(),
        AppAction::CloseSettings => state.close_settings(),
        AppAction::SaveOpenAiKey(key) => {
            if let Err(error) = state.save_openai_key(key) {
                state.session_status = Some(format!("Failed to save OpenAI key: {error}"));
            }
        }
        AppAction::SaveOpenCodeKey(key) => {
            if let Err(error) = state.save_opencode_key(key) {
                state.session_status = Some(format!("Failed to save OpenCode key: {error}"));
            }
        }
        AppAction::SetActiveCodexAccount(id) => {
            let _ = threadlane_auth::openai_auth::set_active_codex_account(&id);
            state.reconcile_selected_model();
        }
        AppAction::RemoveCodexAccount(id) => {
            let _ = threadlane_auth::openai_auth::remove_codex_account(&id);
            state.reconcile_selected_model();
        }
        AppAction::ToggleReasoningExpanded(msg_id) => {
            if let Some(message) = state.messages_mut().iter_mut().find(|m| m.id == msg_id) {
                message.reasoning_expanded = !message.reasoning_expanded;
            }
        }
        AppAction::OpenFileInEditor(path) => state.request_open_file(path),
        AppAction::RunTerminalCommand(cmd) => state.request_run_terminal_command(cmd),
    }
    None
}
