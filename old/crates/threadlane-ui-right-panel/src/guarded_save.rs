use super::RightPanelView;
use gpui::{App, ClipboardItem, Context, Window};
use gpui_component::input::EditorState;
use std::{path::PathBuf, sync::Arc};
use threadlane_client::DaemonClient;
use threadlane_ui_kit::{EditorRecoveryAction, EditorSaveStatus};
use threadlane_ui_state::project_io::{self, ProjectFileError};

#[derive(Clone)]
struct DocumentReloadSnapshot {
    editor: gpui::Entity<EditorState>,
    origin: DocumentOrigin,
    request: u64,
    revision: u64,
    content: String,
}

#[derive(Clone)]
pub(super) struct DocumentOrigin {
    pub(super) client: Arc<dyn DaemonClient>,
    epoch: u64,
    project: PathBuf,
    path: String,
}

impl DocumentOrigin {
    pub(super) fn new(client: Arc<dyn DaemonClient>, project: PathBuf, path: String) -> Self {
        Self { epoch: client.file_search_connection_epoch(), client, project, path }
    }

    fn same_checkout(&self, view: &RightPanelView, cx: &App) -> bool {
        let model = view.model.read(cx);
        Arc::ptr_eq(&self.client, &model.daemon_client)
            && view.project.as_ref() == Some(&self.project)
            && model.active_git_work_dir().as_ref() == Some(&self.project)
            && view.document_title.as_ref() == Some(&self.path)
    }

    pub(super) fn is_current(&self, view: &RightPanelView, cx: &App) -> bool {
        self.same_checkout(view, cx) && self.client.is_connected()
            && self.epoch == self.client.file_search_connection_epoch()
    }
}

impl RightPanelView {
    fn document_save_status(&self, cx: &App) -> EditorSaveStatus {
        if self.document_origin.as_ref().is_none_or(|origin| !origin.is_current(self, cx)) {
            return EditorSaveStatus::Failed { message: "The daemon connection changed or is unavailable. Your edits are still here. Reconnect and reload before saving; for a different daemon, copy your edits and reopen the file.".into(), save_blocked: true };
        }
        if !self.model.read(cx).daemon_client.supports_guarded_saves() { return EditorSaveStatus::Unsupported; }
        self.save_status.clone()
    }

    pub(super) fn document_can_save(&self, cx: &App) -> bool {
        self.editor_state.is_some() && self.is_dirty && !self.document_loading
            && self.pending_document.is_none() && self.saved_version.is_some()
            && self.document_save_status(cx).allows_save()
    }

    pub(super) fn save_guarded_document(&mut self, cx: &mut Context<Self>) {
        if !self.document_can_save(cx) { return; }
        let editor = self.editor_state.clone().unwrap();
        let origin = self.document_origin.clone().unwrap();
        let content = editor.read(cx).value().to_string();
        if content == self.saved_content { self.is_dirty = false; cx.notify(); return; }
        let version = self.saved_version.clone().unwrap();
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        let document_request = self.panel_document_request;
        self.save_status = EditorSaveStatus::Saving;
        let write_origin = origin.clone();
        let write_content = content.clone();
        let task = cx.background_executor().spawn(async move {
            if write_origin.client.file_search_connection_epoch() != write_origin.epoch { return Err(ProjectFileError::Disconnected); }
            project_io::write_file_guarded(&write_origin.client, &write_origin.project, write_origin.path, write_content, version).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.editor_state.as_ref() != Some(&editor) || this.save_generation != generation
                    || this.panel_document_request != document_request { return; }
                if !origin.is_current(this, cx) {
                    this.save_status = EditorSaveStatus::Failed { message: "The daemon connection changed while saving. Your edits are still here. Reload to verify the saved file before saving again.".into(), save_blocked: true };
                } else {
                    match result {
                        Ok(version) => {
                            this.saved_version = Some(version);
                            this.saved_content = content;
                            this.is_dirty = editor.read(cx).value().as_str() != this.saved_content;
                            this.save_status = EditorSaveStatus::Ready;
                        }
                        Err(ProjectFileError::Changed) => this.save_status = EditorSaveStatus::Conflict,
                        Err(ProjectFileError::Deleted) => this.save_status = EditorSaveStatus::Deleted,
                        Err(ProjectFileError::Unsupported) => this.save_status = EditorSaveStatus::Unsupported,
                        Err(error) => this.save_status = EditorSaveStatus::Failed { message: format!("Couldn't save: {error}. Your edits are still here. Retry after checking the file and connection."), save_blocked: false },
                    }
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    pub(super) fn render_save_recovery(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let editor = self.editor_state.clone()?;
        let request = self.panel_document_request;
        threadlane_ui_kit::editor_save_recovery(
            format!("{} / {}", self.project.as_ref()?.display(), self.document_title.as_ref()?),
            &self.document_save_status(cx),
            cx.listener(move |this, action: &EditorRecoveryAction, window, cx| {
                if this.editor_state.as_ref() != Some(&editor) || this.panel_document_request != request { return; }
                match action {
                    EditorRecoveryAction::Copy => cx.write_to_clipboard(ClipboardItem::new_string(editor.read(cx).value().to_string())),
                    EditorRecoveryAction::RetrySave => this.save_active_document(cx),
                    EditorRecoveryAction::Reload => this.confirm_document_reload(window, cx),
                }
            }), cx,
        )
    }

    fn confirm_document_reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.save_status.busy() { return; }
        let Some(editor) = self.editor_state.clone() else { return; };
        let Some(mut origin) = self.document_origin.clone() else { return; };
        if !origin.same_checkout(self, cx) { return; }
        origin.epoch = origin.client.file_search_connection_epoch();
        let request = self.panel_document_request;
        let revision = self.buffer_revision;
        let content = editor.read(cx).value().to_string();
        let snapshot = DocumentReloadSnapshot {
            editor,
            origin: origin.clone(),
            request,
            revision,
            content,
        };
        let view = cx.entity().downgrade();
        threadlane_ui_kit::confirm_editor_reload(format!("{} / {}", origin.project.display(), origin.path), window, cx, move |window, cx| {
            let _ = view.update(cx, |view, cx| {
                view.start_document_reload(snapshot.clone(), window, cx);
            });
        });
    }

    fn start_document_reload(
        &mut self,
        snapshot: DocumentReloadSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editor_state.as_ref() != Some(&snapshot.editor)
            || self.panel_document_request != snapshot.request
            || self.save_status.busy()
        {
            return;
        }
        if !snapshot.origin.is_current(self, cx)
            || self.buffer_revision != snapshot.revision
            || snapshot.editor.read(cx).value().as_str() != snapshot.content
        {
            self.save_status = EditorSaveStatus::Failed {
                message: "The document or connection changed after confirmation. Your edits are still here. Choose Reload from disk again.".into(),
                save_blocked: true,
            };
            cx.notify();
            return;
        }
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        self.save_status = EditorSaveStatus::Reloading;
        let read_origin = snapshot.origin.clone();
        let task = cx.background_executor().spawn(async move {
            if read_origin.client.file_search_connection_epoch() != read_origin.epoch {
                return Err(ProjectFileError::Disconnected);
            }
            project_io::read_file_versioned(
                &read_origin.client,
                &read_origin.project,
                read_origin.path,
            )
            .await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.editor_state.as_ref() != Some(&snapshot.editor)
                    || this.panel_document_request != snapshot.request
                    || this.save_generation != generation
                {
                    return;
                }
                if !snapshot.origin.is_current(this, cx)
                    || this.buffer_revision != snapshot.revision
                    || snapshot.editor.read(cx).value().as_str() != snapshot.content
                {
                    this.save_status = EditorSaveStatus::Failed {
                        message: "The document or connection changed while reloading. Your edits are still here. Choose Reload from disk again.".into(),
                        save_blocked: true,
                    };
                } else {
                    match result {
                        Ok(file) => {
                            snapshot.editor.update(cx, |editor, cx| {
                                editor.set_value(file.content.clone(), window, cx)
                            });
                            this.markdown_preview
                                .refresh(file.content.clone().into(), cx);
                            this.saved_content = file.content;
                            this.saved_version = Some(file.version);
                            this.document_origin = Some(snapshot.origin);
                            this.is_dirty = false;
                            this.buffer_revision = this.buffer_revision.wrapping_add(1);
                            this.save_status = EditorSaveStatus::Ready;
                        }
                        Err(error) => {
                            this.save_status = EditorSaveStatus::Failed {
                                message: format!("Couldn't reload: {error}. Your edits are still here. Check the file and connection, then choose Reload from disk again."),
                                save_blocked: true,
                            }
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{DocumentReloadSnapshot, RightPanelView};
    use gpui::{AppContext, VisualTestContext};
    use threadlane_ui_kit::EditorSaveStatus;
    use threadlane_ui_state::AppState;

    fn settle(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        for _ in 0..4 {
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
        }
    }

    fn set_editor_value(
        editor: &gpui::Entity<gpui_component::input::EditorState>,
        value: &str,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        editor.update(cx, |editor, cx| {
            editor.set_value(value, window, cx);
            cx.emit(gpui_component::input::InputEvent::Change);
        });
    }

    #[gpui::test]
    fn guarded_save_conflict_copy_and_reload_recovery(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::write(project.join("sample.rs"), "original\n").unwrap();
        let model = cx.new(|_| {
            let mut state = AppState::for_tests();
            state.active_session_id = None;
            state.active_work_dir = Some(project.clone());
            state
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let panel = cx.new(|cx| {
                let mut panel = RightPanelView::new(model, window, cx);
                panel.project = Some(project.clone());
                panel.active_surface = Some(super::super::Surface::Files);
                panel.start_panel_document_read(project.clone(), "sample.rs".into(), cx);
                panel
            });
            gpui_component::Root::new(panel, window, cx)
        });
        let panel = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<RightPanelView>().unwrap()
        });
        settle(cx);
        let editor = panel.read_with(cx, |panel, _| panel.editor_state.clone().unwrap());
        cx.update(|window, cx| set_editor_value(&editor, "my edits", window, cx));
        settle(cx);
        std::fs::write(project.join("sample.rs"), "agent edits\n").unwrap();
        panel.update(cx, |panel, cx| panel.save_active_document(cx));
        settle(cx);
        panel.read_with(cx, |panel, cx| {
            assert_eq!(panel.save_status, EditorSaveStatus::Conflict);
            assert!(panel.is_dirty);
            assert_eq!(editor.read(cx).value().as_str(), "my edits");
        });
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "agent edits\n"
        );

        let copy = cx.debug_bounds("editor-copy-edits").unwrap();
        cx.simulate_click(copy.center(), Default::default());
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("my edits")
            )
        });

        let reload = cx.debug_bounds("editor-reload-file").unwrap();
        cx.simulate_click(reload.center(), Default::default());
        settle(cx);
        cx.simulate_keystrokes("escape");
        settle(cx);
        panel.read_with(cx, |panel, cx| {
            assert_eq!(editor.read(cx).value().as_str(), "my edits");
            assert!(panel.is_dirty);
        });

        let reload = cx.debug_bounds("editor-reload-file").unwrap();
        cx.simulate_click(reload.center(), Default::default());
        settle(cx);
        std::fs::remove_file(project.join("sample.rs")).unwrap();
        let confirm = cx.debug_bounds("editor-confirm-reload").unwrap();
        cx.simulate_click(confirm.center(), Default::default());
        settle(cx);
        panel.read_with(cx, |panel, cx| {
            assert!(matches!(
                panel.save_status,
                EditorSaveStatus::Failed {
                    save_blocked: true,
                    ..
                }
            ));
            assert_eq!(editor.read(cx).value().as_str(), "my edits");
            assert!(panel.is_dirty);
        });
        panel.update(cx, |panel, cx| panel.save_active_document(cx));
        settle(cx);
        assert!(!project.join("sample.rs").exists());

        std::fs::write(project.join("sample.rs"), "restored\n").unwrap();
        let reload = cx.debug_bounds("editor-reload-file").unwrap();
        cx.simulate_click(reload.center(), Default::default());
        settle(cx);
        let confirm = cx.debug_bounds("editor-confirm-reload").unwrap();
        cx.simulate_click(confirm.center(), Default::default());
        settle(cx);
        panel.read_with(cx, |panel, cx| {
            assert_eq!(panel.save_status, EditorSaveStatus::Ready);
            assert!(!panel.is_dirty);
            assert_eq!(editor.read(cx).value().as_str(), "restored\n");
        });
    }

    #[gpui::test]
    fn pending_reload_preserves_typing_and_drops_replaced_document(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::write(project.join("sample.rs"), "original\n").unwrap();
        let model = cx.new(|_| {
            let mut state = AppState::for_tests();
            state.active_session_id = None;
            state.active_work_dir = Some(project.clone());
            state
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let panel = cx.new(|cx| {
                let mut panel = RightPanelView::new(model, window, cx);
                panel.project = Some(project.clone());
                panel.active_surface = Some(super::super::Surface::Files);
                panel.start_panel_document_read(project.clone(), "sample.rs".into(), cx);
                panel
            });
            gpui_component::Root::new(panel, window, cx)
        });
        let panel = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<RightPanelView>().unwrap()
        });
        settle(cx);
        let editor = panel.read_with(cx, |panel, _| panel.editor_state.clone().unwrap());
        panel.update(cx, |panel, _| panel.save_status = EditorSaveStatus::Conflict);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let reload = cx.debug_bounds("editor-reload-file").unwrap();
        cx.simulate_click(reload.center(), Default::default());
        settle(cx);
        cx.update(|window, cx| set_editor_value(&editor, "typed while reloading", window, cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let confirm = cx.debug_bounds("editor-confirm-reload").unwrap();
        cx.simulate_click(confirm.center(), Default::default());
        settle(cx);
        panel.read_with(cx, |panel, cx| {
            assert!(matches!(
                panel.save_status,
                EditorSaveStatus::Failed {
                    save_blocked: true,
                    ..
                }
            ));
            assert_eq!(editor.read(cx).value().as_str(), "typed while reloading");
            assert!(panel.is_dirty);
        });

        cx.update(|window, cx| set_editor_value(&editor, "original\n", window, cx));
        panel.update(cx, |panel, _| panel.save_status = EditorSaveStatus::Conflict);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let reload = cx.debug_bounds("editor-reload-file").unwrap();
        cx.simulate_click(reload.center(), Default::default());
        settle(cx);
        panel.update(cx, |panel, cx| panel.close_document(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let confirm = cx.debug_bounds("editor-confirm-reload").unwrap();
        cx.simulate_click(confirm.center(), Default::default());
        settle(cx);
        panel.read_with(cx, |panel, _| {
            assert!(panel.document_title.is_none());
            assert!(panel.editor_state.is_none());
            assert!(panel.saved_content.is_empty());
        });
    }

    #[gpui::test]
    fn in_flight_reload_rejects_typing_and_replaced_document(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::write(project.join("sample.rs"), "original\n").unwrap();
        std::fs::write(project.join("other.rs"), "different document\n").unwrap();
        let model = cx.new(|_| {
            let mut state = AppState::for_tests();
            state.active_session_id = None;
            state.active_work_dir = Some(project.clone());
            state
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let panel = cx.new(|cx| {
                let mut panel = RightPanelView::new(model, window, cx);
                panel.project = Some(project.clone());
                panel.active_surface = Some(super::super::Surface::Files);
                panel.start_panel_document_read(project.clone(), "sample.rs".into(), cx);
                panel
            });
            gpui_component::Root::new(panel, window, cx)
        });
        let panel = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<RightPanelView>().unwrap()
        });
        settle(cx);
        let editor = panel.read_with(cx, |panel, _| panel.editor_state.clone().unwrap());
        let baseline_version = panel.read_with(cx, |panel, _| panel.saved_version.clone().unwrap());
        std::fs::write(project.join("sample.rs"), "external update\n").unwrap();

        cx.update(|window, cx| {
            panel.update(cx, |panel, cx| {
                panel.save_status = EditorSaveStatus::Conflict;
                let snapshot = DocumentReloadSnapshot {
                    editor: editor.clone(),
                    origin: panel.document_origin.clone().unwrap(),
                    request: panel.panel_document_request,
                    revision: panel.buffer_revision,
                    content: editor.read(cx).value().to_string(),
                };
                panel.start_document_reload(snapshot, window, cx);
                assert_eq!(panel.save_status, EditorSaveStatus::Reloading);
                set_editor_value(&editor, "typed after reload started", window, cx);
            });
        });
        settle(cx);
        panel.read_with(cx, |panel, cx| {
            assert!(matches!(
                panel.save_status,
                EditorSaveStatus::Failed {
                    save_blocked: true,
                    ..
                }
            ));
            assert!(panel.is_dirty);
            assert_eq!(panel.saved_content, "original\n");
            assert_eq!(panel.saved_version.as_deref(), Some(baseline_version.as_str()));
            assert_eq!(
                editor.read(cx).value().as_str(),
                "typed after reload started"
            );
        });
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "external update\n"
        );

        cx.update(|window, cx| {
            set_editor_value(&editor, "original\n", window, cx);
        });
        settle(cx);
        cx.update(|window, cx| {
            panel.update(cx, |panel, cx| {
                panel.save_status = EditorSaveStatus::Conflict;
                let current_editor = panel.editor_state.clone().unwrap();
                let snapshot = DocumentReloadSnapshot {
                    editor: current_editor,
                    origin: panel.document_origin.clone().unwrap(),
                    request: panel.panel_document_request,
                    revision: panel.buffer_revision,
                    content: panel.editor_state.as_ref().unwrap().read(cx).value().to_string(),
                };
                panel.start_document_reload(snapshot, window, cx);
                assert_eq!(panel.save_status, EditorSaveStatus::Reloading);
                panel.start_panel_document_read(project.clone(), "other.rs".into(), cx);
            });
        });
        settle(cx);
        panel.read_with(cx, |panel, cx| {
            assert_eq!(panel.document_title.as_deref(), Some("other.rs"));
            let editor = panel.editor_state.as_ref().unwrap();
            assert_eq!(editor.read(cx).value().as_str(), "different document\n");
            assert_eq!(panel.saved_content, "different document\n");
            assert!(panel.saved_version.is_some());
            assert_eq!(panel.save_status, EditorSaveStatus::Ready);
            assert!(!panel.is_dirty);
        });
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "external update\n"
        );
    }

    #[gpui::test]
    fn save_acknowledges_only_submitted_snapshot(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::write(project.join("sample.rs"), "original\n").unwrap();
        let model = cx.new(|_| {
            let mut state = AppState::for_tests();
            state.active_session_id = None;
            state.active_work_dir = Some(project.clone());
            state
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let panel = cx.new(|cx| {
                let mut panel = RightPanelView::new(model, window, cx);
                panel.project = Some(project.clone());
                panel.active_surface = Some(super::super::Surface::Files);
                panel.start_panel_document_read(project.clone(), "sample.rs".into(), cx);
                panel
            });
            gpui_component::Root::new(panel, window, cx)
        });
        let panel = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<RightPanelView>().unwrap()
        });
        settle(cx);
        let editor = panel.read_with(cx, |panel, _| panel.editor_state.clone().unwrap());
        cx.update(|window, cx| set_editor_value(&editor, "submitted edits", window, cx));
        settle(cx);
        cx.update(|window, cx| {
            panel.update(cx, |panel, cx| panel.save_active_document(cx));
            set_editor_value(&editor, "newer edits", window, cx);
        });
        settle(cx);
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "submitted edits"
        );
        panel.read_with(cx, |panel, cx| {
            assert_eq!(panel.saved_content, "submitted edits");
            assert_eq!(panel.save_status, EditorSaveStatus::Ready);
            assert!(panel.is_dirty);
            assert_eq!(editor.read(cx).value().as_str(), "newer edits");
        });
    }

    #[gpui::test]
    fn replaced_client_rejects_save_completion(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::write(project.join("sample.rs"), "original\n").unwrap();
        let model = cx.new(|_| {
            let mut state = AppState::for_tests();
            state.active_session_id = None;
            state.active_work_dir = Some(project.clone());
            state
        });
        let replacement = AppState::for_tests().daemon_client.clone();
        let (root, cx) = cx.add_window_view(|window, cx| {
            let panel = cx.new(|cx| {
                let mut panel = RightPanelView::new(model.clone(), window, cx);
                panel.project = Some(project.clone());
                panel.active_surface = Some(super::super::Surface::Files);
                panel.start_panel_document_read(project.clone(), "sample.rs".into(), cx);
                panel
            });
            gpui_component::Root::new(panel, window, cx)
        });
        let panel = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<RightPanelView>().unwrap()
        });
        settle(cx);
        let editor = panel.read_with(cx, |panel, _| panel.editor_state.clone().unwrap());
        cx.update(|window, cx| set_editor_value(&editor, "submitted edits", window, cx));
        panel.update(cx, |panel, cx| panel.save_active_document(cx));
        model.update(cx, |state, _| state.daemon_client = replacement);
        settle(cx);
        panel.read_with(cx, |panel, cx| {
            assert!(matches!(
                panel.save_status,
                EditorSaveStatus::Failed {
                    save_blocked: true,
                    ..
                }
            ));
            assert!(panel.is_dirty);
            assert_eq!(editor.read(cx).value().as_str(), "submitted edits");
        });
    }
}
