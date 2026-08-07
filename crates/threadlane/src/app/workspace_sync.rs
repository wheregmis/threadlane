use super::*;
use std::path::PathBuf;

impl App {
    pub(super) fn select_workspace(&mut self, work_dir: PathBuf, session_id: impl Into<String>) {
        self.workspace_state
            .select(SessionKey::new(work_dir, session_id));
    }

    pub(super) fn select_workspace_ui(
        &mut self,
        cx: &mut Cx,
        work_dir: PathBuf,
        session_id: String,
    ) {
        self.save_active_draft(cx);
        self.git_operation_pending = false;
        self.git_pr_pending = false;
        self.git_pr_created = false;
        self.git_status_pending = false;
        self.git_new_branch_open = false;
        if let Some(abort) = self.git_commit_message_abort.take() {
            abort.abort();
        }
        self.git_commit_message_pending = false;
        self.git_commit_message_request_id = self.git_commit_message_request_id.wrapping_add(1);
        self.git_diff_open = false;
        self.git_diff_request_id = self.git_diff_request_id.wrapping_add(1);
        self.git_diff_pending = false;
        self.git_feedback = None;
        self.ui
            .text_input(cx, ids!(git_commit_message))
            .set_text(cx, "");
        if let Some(mut changes) = self
            .ui
            .widget(cx, ids!(git_changes))
            .borrow_mut::<GitChanges>()
        {
            changes.clear_selection(cx);
        }
        self.select_workspace(work_dir, session_id);
        if let Some(key) = self.workspace_state.active_key() {
            let home_dir = std::env::var_os("HOME").map(PathBuf::from);
            self.ui
                .label(cx, ids!(project_name_label))
                .set_text(cx, &project_name(&key.work_dir));
            self.ui.label(cx, ids!(workspace_label)).set_text(
                cx,
                &compact_workspace_path(&key.work_dir, home_dir.as_deref()),
            );
        }
        let draft = self
            .workspace_state
            .active_workspace()
            .map(|workspace| workspace.ui.draft.clone())
            .unwrap_or_default();
        self.ui
            .threadlane_command_text_input(cx, ids!(prompt_input))
            .text_input_ref(cx)
            .set_text(cx, &draft);
        self.refresh_attachment_ui(cx);
        self.sync_terminal_project(cx);
        self.sync_git_branch_picker(cx);
        self.sync_right_sidebar(cx);
        self.sync_left_sidebar(cx);
        self.request_git_status();
        self.sync_task_sidebar(cx);
    }

    pub(super) fn select_project_draft(&mut self, cx: &mut Cx, work_dir: PathBuf) {
        if !work_dir.is_dir() {
            self.push_chat(
                MsgRole::System,
                format!("Project folder `{}` is missing.", work_dir.display()),
            );
            return;
        }
        set_active_project(&work_dir);
        if let Some(registry) = self.project_registry.as_mut() {
            if let Err(error) = registry.remember_selection(&work_dir, None) {
                self.push_chat(
                    MsgRole::System,
                    format!("Could not update recent-project state: {error}"),
                );
            }
        }
        self.select_workspace_ui(cx, work_dir.clone(), "draft".to_string());
        let key = SessionKey::project_draft(work_dir.clone());
        if !self.session_runtimes.contains_key(&key) {
            let (api_key, account_id) = self.current_credentials(cx);
            let model = self
                .ui
                .icon_drop_down(cx, ids!(model_drop))
                .selected_label();
            let model = if model.is_empty() {
                default_model_name().to_string()
            } else {
                model
            };
            let effort = ReasoningEffort::from_label(
                &self
                    .ui
                    .icon_drop_down(cx, ids!(effort_drop))
                    .selected_label(),
            )
            .unwrap_or_default();
            let agent = CodingAgent::new(CodingAgentOptions {
                api_key,
                account_id,
                model: model.clone(),
                work_dir: work_dir.clone(),
                session_file: None,
                system_prompt: Default::default(),
            });
            self.session_runtimes
                .insert(key.clone(), SessionRuntime::new(agent, model, effort));
        }
        if let Some((model, effort)) = self
            .session_runtimes
            .get(&key)
            .map(|runtime| (runtime.model.clone(), runtime.reasoning_effort))
        {
            self.set_model_dropup_options(cx, self.available_models.clone(), &model);
            self.set_reasoning_effort_picker(cx, effort);
        }
        self.refresh_project_capabilities(cx, &work_dir);
        self.restore_active_status(cx);
        cx.redraw_all();
    }

    pub(super) fn sync_sidebar_action_visibility(&mut self, cx: &mut Cx, event: &Event) {
        match event {
            Event::MouseMove(event) => self.sidebar_pointer = Some(event.abs),
            Event::MouseLeave(_) => self.sidebar_pointer = None,
            _ => {}
        }
        let pointer = self.sidebar_pointer;

        let context_menu_open = crate::panels::sessions::state::SESSIONS_DATA
            .read()
            .unwrap()
            .context_session_id
            .is_some();

        let projects_header = self.ui.view(cx, ids!(projects_header));
        let add_project_visible = !context_menu_open
            && pointer.is_some_and(|position| projects_header.area().rect(cx).contains(position));
        let add_project_btn = self.ui.button(cx, ids!(add_project_btn));
        if add_project_btn.visible() != add_project_visible {
            add_project_btn.set_visible(cx, add_project_visible);
            projects_header.redraw(cx);
        }
    }
}
