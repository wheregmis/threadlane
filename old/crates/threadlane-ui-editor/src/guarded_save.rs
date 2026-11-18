use super::{ClientSnapshot, EditorTab, EditorView};
use gpui::{App, ClipboardItem, Context, Entity, Window};
use gpui_component::input::EditorState;
use std::sync::Arc;
use threadlane_ui_kit::{EditorRecoveryAction, EditorSaveStatus};
use threadlane_ui_state::project_io::{self, ProjectFileError};

fn failure(error: ProjectFileError) -> EditorSaveStatus {
    match error {
        ProjectFileError::Changed => EditorSaveStatus::Conflict,
        ProjectFileError::Deleted => EditorSaveStatus::Deleted,
        ProjectFileError::Unsupported => EditorSaveStatus::Unsupported,
        error => EditorSaveStatus::Failed {
            message: format!("Couldn't save: {error}. Your edits are still here. Retry after checking the file and connection."),
            save_blocked: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::EditorView;
    use gpui::{AppContext, VisualTestContext};
    use gpui_component::WindowExt;
    use threadlane_ui_kit::EditorSaveStatus;

    fn settle(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        for _ in 0..4 {
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn guarded_save_conflict_copy_and_confirmed_reload(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::write(project.join("sample.rs"), "original\n").unwrap();
        let model = cx.new(|_| {
            let mut model = threadlane_ui_state::AppState::for_tests();
            model.active_work_dir = Some(project.clone());
            model
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_file_internal(&project, "sample.rs", window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| root.view().clone().downcast::<EditorView>().unwrap());
        settle(cx);
        let editor = view.read_with(cx, |view, _| view.tabs[0].editor_state.clone().unwrap());
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_value("my edits", window, cx);
                cx.emit(gpui_component::input::InputEvent::Change);
            });
        });
        settle(cx);
        std::fs::write(project.join("sample.rs"), "agent edits\n").unwrap();
        view.update(cx, |view, cx| view.save_tab_at(0, cx));
        settle(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.tabs[0].save_status, EditorSaveStatus::Conflict);
            assert!(view.tabs[0].is_dirty);
            assert_eq!(editor.read(cx).value().as_str(), "my edits");
        });
        assert_eq!(std::fs::read_to_string(project.join("sample.rs")).unwrap(), "agent edits\n");
        let copy = cx.debug_bounds("editor-copy-edits").unwrap();
        cx.simulate_click(copy.center(), Default::default());
        cx.update(|_, cx| assert_eq!(cx.read_from_clipboard().unwrap().text().as_deref(), Some("my edits")));
        let reload = cx.debug_bounds("editor-reload-file").unwrap();
        cx.simulate_click(reload.center(), Default::default());
        settle(cx);
        cx.update(|window, cx| assert!(window.has_active_dialog(cx)));
        cx.simulate_keystrokes("escape");
        settle(cx);
        view.read_with(cx, |_, cx| assert_eq!(editor.read(cx).value().as_str(), "my edits"));
        cx.simulate_click(reload.center(), Default::default());
        settle(cx);
        // Deletion after confirmation must retain the buffer, not turn into a create.
        std::fs::remove_file(project.join("sample.rs")).unwrap();
        let confirm = cx.debug_bounds("editor-confirm-reload").unwrap();
        cx.simulate_click(confirm.center(), Default::default());
        settle(cx);
        view.read_with(cx, |view, cx| {
            assert!(matches!(view.tabs[0].save_status, EditorSaveStatus::Failed { save_blocked: true, .. }));
            assert_eq!(editor.read(cx).value().as_str(), "my edits");
            assert_eq!(view.tabs[0].editor_state.as_ref(), Some(&editor));
        });
        view.update(cx, |view, cx| view.save_tab_at(0, cx));
        settle(cx);
        assert!(!project.join("sample.rs").exists());
        std::fs::write(project.join("sample.rs"), "restored\n").unwrap();
        let reload = cx.debug_bounds("editor-reload-file").unwrap();
        cx.simulate_click(reload.center(), Default::default());
        settle(cx);
        let confirm = cx.debug_bounds("editor-confirm-reload").unwrap();
        cx.simulate_click(confirm.center(), Default::default());
        settle(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.tabs[0].save_status, EditorSaveStatus::Ready);
            assert!(!view.tabs[0].is_dirty);
            assert_eq!(editor.read(cx).value().as_str(), "restored\n");
        });
    }

    #[gpui::test]
    fn guarded_save_acknowledges_only_submitted_snapshot(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::write(project.join("sample.rs"), "original").unwrap();
        let model = cx.new(|_| {
            let mut model = threadlane_ui_state::AppState::for_tests();
            model.active_work_dir = Some(project.clone());
            model
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_file_internal(&project, "sample.rs", window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| root.view().clone().downcast::<EditorView>().unwrap());
        settle(cx);
        cx.update(|window, cx| view.update(cx, |view, cx| {
            let editor = view.tabs[0].editor_state.clone().unwrap();
            editor.update(cx, |editor, cx| editor.set_value("submitted", window, cx));
            view.tabs[0].is_dirty = true;
            view.save_tab_at(0, cx);
            assert_eq!(view.tabs[0].save_status, EditorSaveStatus::Saving);
            editor.update(cx, |editor, cx| editor.set_value("newer edits", window, cx));
            view.save_tab_at(0, cx);
        }));
        settle(cx);
        assert_eq!(std::fs::read_to_string(project.join("sample.rs")).unwrap(), "submitted");
        view.read_with(cx, |view, cx| {
            assert_eq!(view.tabs[0].saved_content, "submitted");
            assert!(view.tabs[0].is_dirty);
            assert_eq!(view.tabs[0].save_status, EditorSaveStatus::Ready);
            assert_eq!(view.tabs[0].editor_state.as_ref().unwrap().read(cx).value().as_str(), "newer edits");
        });
    }

    #[gpui::test]
    fn pending_reload_preserves_typing_and_drops_closed_tab_completion(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::write(project.join("sample.rs"), "original\n").unwrap();
        let model = cx.new(|_| {
            let mut model = threadlane_ui_state::AppState::for_tests();
            model.active_work_dir = Some(project.clone());
            model
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_file_internal(&project, "sample.rs", window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });
        settle(cx);
        let editor = view.read_with(cx, |view, _| view.tabs[0].editor_state.clone().unwrap());
        view.update(cx, |view, _| view.tabs[0].save_status = EditorSaveStatus::Conflict);
        cx.update(|window, cx| {
        view.update(cx, |view, cx| view.confirm_tab_reload(0, window, cx));
        });
        settle(cx);
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_value("typed while reloading", window, cx);
                cx.emit(gpui_component::input::InputEvent::Change);
            });
            window.draw(cx).clear(cx);
        });
        let confirm = cx.debug_bounds("editor-confirm-reload").unwrap();
        cx.simulate_click(confirm.center(), Default::default());
        settle(cx);
        view.read_with(cx, |view, cx| {
            assert!(matches!(
                view.tabs[0].save_status,
                EditorSaveStatus::Failed {
                    save_blocked: true,
                    ..
                }
            ));
            assert_eq!(editor.read(cx).value().as_str(), "typed while reloading");
            assert!(view.tabs[0].is_dirty);
        });

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_value("original\n", window, cx);
                cx.emit(gpui_component::input::InputEvent::Change);
            });
        });
        view.update(cx, |view, _| view.tabs[0].save_status = EditorSaveStatus::Conflict);
        cx.update(|window, cx| {
        view.update(cx, |view, cx| view.confirm_tab_reload(0, window, cx));
        });
        settle(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.close_tab(0, window, cx));
            window.draw(cx).clear(cx);
        });
        let confirm = cx.debug_bounds("editor-confirm-reload").unwrap();
        cx.simulate_click(confirm.center(), Default::default());
        settle(cx);
        view.read_with(cx, |view, _| assert_eq!(view.tabs.len(), 0));
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "original\n"
        );
    }

    #[gpui::test]
    fn in_flight_reload_rejects_typing_and_closed_tab_completion(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::write(project.join("sample.rs"), "original\n").unwrap();
        std::fs::write(project.join("other.rs"), "different document\n").unwrap();
        let model = cx.new(|_| {
            let mut model = threadlane_ui_state::AppState::for_tests();
            model.active_work_dir = Some(project.clone());
            model
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_file_internal(&project, "sample.rs", window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });
        settle(cx);
        let editor = view.read_with(cx, |view, _| view.tabs[0].editor_state.clone().unwrap());
        let baseline_version =
            view.read_with(cx, |view, _| view.tabs[0].saved_version.clone().unwrap());
        std::fs::write(project.join("sample.rs"), "external update\n").unwrap();

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let revision = view.tabs[0].buffer_revision;
                let content = editor.read(cx).value().to_string();
                let snapshot = view.current_client_snapshot(cx);
                view.reload_tab(&editor, revision, &content, snapshot, window, cx);
                assert_eq!(view.tabs[0].save_status, EditorSaveStatus::Reloading);
                editor.update(cx, |editor, cx| {
                    editor.set_value("typed after reload started", window, cx);
                    cx.emit(gpui_component::input::InputEvent::Change);
                });
            });
        });
        settle(cx);
        view.read_with(cx, |view, cx| {
            let tab = &view.tabs[0];
            assert!(matches!(
                tab.save_status,
                EditorSaveStatus::Failed {
                    save_blocked: true,
                    ..
                }
            ));
            assert!(tab.is_dirty);
            assert_eq!(tab.saved_content, "original\n");
            assert_eq!(tab.saved_version.as_deref(), Some(baseline_version.as_str()));
            assert_eq!(
                tab.editor_state.as_ref().unwrap().read(cx).value().as_str(),
                "typed after reload started"
            );
        });
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "external update\n"
        );

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_value("original\n", window, cx);
                cx.emit(gpui_component::input::InputEvent::Change);
            });
        });
        settle(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let revision = view.tabs[0].buffer_revision;
                let content = editor.read(cx).value().to_string();
                let snapshot = view.current_client_snapshot(cx);
                view.reload_tab(&editor, revision, &content, snapshot, window, cx);
                assert_eq!(view.tabs[0].save_status, EditorSaveStatus::Reloading);
                view.close_tab(0, window, cx);
                view.open_file_internal(&project, "other.rs", window, cx);
            });
        });
        settle(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.tabs.len(), 1);
            assert_eq!(view.tabs[0].relative_path, "other.rs");
            assert_eq!(
                view.tabs[0]
                    .editor_state
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .value()
                    .as_str(),
                "different document\n"
            );
            assert_eq!(view.tabs[0].saved_content, "different document\n");
            assert_eq!(view.tabs[0].save_status, EditorSaveStatus::Ready);
        });
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "external update\n"
        );
    }

    #[gpui::test]
    fn reload_refresh_typing_before_read_completion_keeps_original_version(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::write(project.join("sample.rs"), "original\n").unwrap();
        let model = cx.new(|_| {
            let mut model = threadlane_ui_state::AppState::for_tests();
            model.active_work_dir = Some(project.clone());
            model
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_file_internal(&project, "sample.rs", window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });
        settle(cx);
        let (editor, original_version) = view.read_with(cx, |view, _| {
            (
                view.tabs[0].editor_state.clone().unwrap(),
                view.tabs[0].saved_version.clone().unwrap(),
            )
        });
        std::fs::write(project.join("sample.rs"), "external update\n").unwrap();

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.start_file_read(0, cx);
                assert!(view.tabs[0].loading);
                editor.update(cx, |editor, cx| {
                    editor.set_value("typed before read completed", window, cx);
                    cx.emit(gpui_component::input::InputEvent::Change);
                });
            });
        });
        settle(cx);
        view.read_with(cx, |view, cx| {
            let tab = &view.tabs[0];
            assert!(tab.is_dirty);
            assert_eq!(tab.saved_content, "original\n");
            assert_eq!(tab.saved_version.as_deref(), Some(original_version.as_str()));
            assert!(tab.pending_content.is_none());
            assert_eq!(
                tab.editor_state.as_ref().unwrap().read(cx).value().as_str(),
                "typed before read completed"
            );
        });

        view.update(cx, |view, cx| view.save_tab_at(0, cx));
        settle(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.tabs[0].save_status, EditorSaveStatus::Conflict);
            assert!(view.tabs[0].is_dirty);
            assert_eq!(view.tabs[0].saved_content, "original\n");
            assert_eq!(view.tabs[0].saved_version.as_deref(), Some(original_version.as_str()));
        });
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "external update\n"
        );
    }

    #[gpui::test]
    fn reload_refresh_typing_after_read_completion_keeps_original_version(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::write(project.join("sample.rs"), "original\n").unwrap();
        let model = cx.new(|_| {
            let mut model = threadlane_ui_state::AppState::for_tests();
            model.active_work_dir = Some(project.clone());
            model
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_file_internal(&project, "sample.rs", window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });
        settle(cx);
        let original_version =
            view.read_with(cx, |view, _| view.tabs[0].saved_version.clone().unwrap());
        std::fs::write(project.join("sample.rs"), "external update\n").unwrap();

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let editor = view.tabs[0].editor_state.clone().unwrap();
                let generation = view.tabs[0].request_generation;
                let buffer_revision = view.tabs[0].buffer_revision;
                let snapshot = view.current_client_snapshot(cx);
                view.tabs[0].loading = true;
                view.finish_file_open_for_request(
                    &project,
                    "sample.rs",
                    &editor,
                    generation,
                    buffer_revision,
                    snapshot,
                    Ok((
                        "external update\n".into(),
                        Some("sha256:external-update".into()),
                    )),
                    cx,
                );
                assert!(!view.tabs[0].loading);
                assert_eq!(
                    view.tabs[0].pending_content.as_deref(),
                    Some("external update\n")
                );
                assert!(view.tabs[0].pending_content_version.is_some());
                assert_eq!(view.tabs[0].saved_content, "original\n");
                assert_eq!(
                    view.tabs[0].saved_version.as_deref(),
                    Some(original_version.as_str())
                );
                editor.update(cx, |editor, cx| {
                    editor.set_value("typed after read completed", window, cx);
                    cx.emit(gpui_component::input::InputEvent::Change);
                });
            });
        });
        view.read_with(cx, |view, cx| {
            let tab = &view.tabs[0];
            assert!(tab.pending_content.is_none());
            assert!(tab.pending_content_version.is_none());
            assert!(tab.is_dirty);
            assert_eq!(tab.saved_content, "original\n");
            assert_eq!(tab.saved_version.as_deref(), Some(original_version.as_str()));
            assert_eq!(
                tab.editor_state.as_ref().unwrap().read(cx).value().as_str(),
                "typed after read completed"
            );
        });

        view.update(cx, |view, cx| view.save_tab_at(0, cx));
        settle(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.tabs[0].save_status, EditorSaveStatus::Conflict);
            assert!(view.tabs[0].is_dirty);
            assert_eq!(view.tabs[0].saved_content, "original\n");
            assert_eq!(view.tabs[0].saved_version.as_deref(), Some(original_version.as_str()));
        });
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "external update\n"
        );
    }

    #[gpui::test]
    fn reload_initial_read_with_loading_edits_does_not_grant_a_baseline(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::write(project.join("sample.rs"), "external update\n").unwrap();
        let model = cx.new(|_| {
            let mut model = threadlane_ui_state::AppState::for_tests();
            model.active_work_dir = Some(project.clone());
            model
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_file_internal(&project, "sample.rs", window, cx);
                view
            });
            view.update(cx, |view, cx| {
                let editor = view.tabs[0].editor_state.clone().unwrap();
                editor.update(cx, |editor, cx| {
                    editor.set_value("typed while loading", window, cx);
                    cx.emit(gpui_component::input::InputEvent::Change);
                });
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });
        settle(cx);
        let editor = view.read_with(cx, |view, _| view.tabs[0].editor_state.clone().unwrap());
        view.read_with(cx, |view, cx| {
            let tab = &view.tabs[0];
            assert!(!tab.baseline_loaded);
            assert!(tab.saved_version.is_none());
            assert!(tab.is_dirty);
            assert!(matches!(
                tab.save_status,
                EditorSaveStatus::Failed {
                    save_blocked: true,
                    ..
                }
            ));
            assert_eq!(
                tab.editor_state.as_ref().unwrap().read(cx).value().as_str(),
                "typed while loading"
            );
        });

        view.update(cx, |view, cx| view.save_tab_at(0, cx));
        settle(cx);
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "external update\n"
        );
        view.read_with(cx, |view, _| {
            assert!(!view.tabs[0].baseline_loaded);
            assert!(view.tabs[0].saved_version.is_none());
            assert!(matches!(
                view.tabs[0].save_status,
                EditorSaveStatus::Failed {
                    save_blocked: true,
                    ..
                }
            ));
        });

        let copy = cx.debug_bounds("editor-copy-edits").unwrap();
        assert!(cx.debug_bounds("editor-reload-file").is_some());
        cx.simulate_click(copy.center(), Default::default());
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("typed while loading")
            )
        });
        let reload = cx.debug_bounds("editor-reload-file").unwrap();
        cx.simulate_click(reload.center(), Default::default());
        settle(cx);
        cx.update(|window, cx| assert!(window.has_active_dialog(cx)));
        let confirm = cx.debug_bounds("editor-confirm-reload").unwrap();
        cx.simulate_click(confirm.center(), Default::default());
        settle(cx);
        view.read_with(cx, |view, cx| {
            let tab = &view.tabs[0];
            assert!(tab.baseline_loaded);
            assert!(tab.saved_version.is_some());
            assert_eq!(tab.saved_content, "external update\n");
            assert_eq!(tab.save_status, EditorSaveStatus::Ready);
            assert!(!tab.is_dirty);
            assert!(!tab.loading);
            assert!(tab.open_error.is_none());
            assert!(tab.pending_content.is_none());
            assert!(tab.pending_content_version.is_none());
            assert_eq!(editor.read(cx).value().as_str(), "external update\n");
        });

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_value("edited after reload", window, cx);
                cx.emit(gpui_component::input::InputEvent::Change);
            });
        });
        settle(cx);
        view.update(cx, |view, cx| view.save_tab_at(0, cx));
        settle(cx);
        assert_eq!(
            std::fs::read_to_string(project.join("sample.rs")).unwrap(),
            "edited after reload"
        );
        view.read_with(cx, |view, cx| {
            let tab = &view.tabs[0];
            assert!(tab.baseline_loaded);
            assert!(tab.saved_version.is_some());
            assert_eq!(tab.saved_content, "edited after reload");
            assert_eq!(tab.save_status, EditorSaveStatus::Ready);
            assert!(!tab.is_dirty);
            assert_eq!(editor.read(cx).value().as_str(), "edited after reload");
        });
    }
}

fn same_client(snapshot: &ClientSnapshot, current: &ClientSnapshot) -> bool {
    Arc::ptr_eq(&snapshot.client, &current.client)
        && snapshot.epoch == current.epoch && snapshot.connected == current.connected
}

impl EditorView {
    fn tab_save_status(&self, tab: &EditorTab, cx: &App) -> EditorSaveStatus {
        let current = self.current_client_snapshot(cx);
        if !current.connected || tab.client_invalidated
            || tab.client_origin.as_ref().is_none_or(|client| !Arc::ptr_eq(client, &current.client))
            || tab.client_epoch != current.epoch
        {
            return EditorSaveStatus::Failed {
                message: "The daemon connection changed or is unavailable. Your edits are still here. Reconnect and reload before saving; for a different daemon, copy your edits and reopen the file.".into(),
                save_blocked: true,
            };
        }
        if !current.client.supports_guarded_saves() { return EditorSaveStatus::Unsupported; }
        tab.save_status.clone()
    }

    pub(super) fn tab_can_save(&self, index: usize, cx: &App) -> bool {
        self.tabs.get(index).is_some_and(|tab| {
            !tab.is_diff && tab.baseline_loaded && !tab.loading && tab.pending_content.is_none()
                && tab.is_dirty && tab.saved_version.is_some()
                && self.tab_save_status(tab, cx).allows_save()
        })
    }

    pub(super) fn save_guarded_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        self.sync_client_context(cx);
        if !self.tab_can_save(index, cx) {
            let current = self.current_client_snapshot(cx);
            let failure = self.tabs.get(index).and_then(|tab| {
                if !tab.baseline_loaded {
                    Some(("The saved file has not loaded successfully. Retry reading it before saving.", false))
                } else if tab.client_invalidated
                    || !current.connected
                    || tab.client_epoch != current.epoch
                    || tab.client_origin.as_ref().is_none_or(|origin| !Arc::ptr_eq(origin, &current.client))
                {
                    Some(("This file belongs to a previous daemon connection or the daemon is unavailable. Your edits are still here. Reconnect and reload before saving; for a different daemon, copy your edits and reopen the file.", true))
                } else {
                    None
                }
            });
            if let Some((message, persistent)) = failure {
                if persistent {
                    if let Some(tab) = self.tabs.get_mut(index) {
                        tab.save_status = EditorSaveStatus::Failed {
                            message: message.into(),
                            save_blocked: true,
                        };
                    }
                }
                self.set_status(message.into(), true);
            }
            cx.notify(); return;
        }
        let snapshot = self.current_client_snapshot(cx);
        let tab = &mut self.tabs[index];
        let Some(editor) = tab.editor_state.clone() else { return; };
        let content = editor.read(cx).value().to_string();
        if content == tab.saved_content { tab.is_dirty = false; cx.notify(); return; }
        let version = tab.saved_version.clone().unwrap();
        let project = tab.project_dir.clone();
        let path = tab.relative_path.clone();
        tab.save_generation = tab.save_generation.wrapping_add(1);
        let generation = tab.save_generation;
        tab.save_status = EditorSaveStatus::Saving;
        let client = snapshot.client.clone();
        let write_content = content.clone();
        let write_project = project.clone();
        let write_path = path.clone();
        let task = cx.background_executor().spawn(async move {
            if client.file_search_connection_epoch() != snapshot.epoch {
                return Err(ProjectFileError::Disconnected);
            }
            project_io::write_file_guarded(&client, &write_project, write_path, write_content, version).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let current = this.current_client_snapshot(cx);
                let Some(tab) = this.tabs.iter_mut().find(|tab| tab.editor_state.as_ref() == Some(&editor)
                    && tab.project_dir == project && tab.relative_path == path && tab.save_generation == generation) else { return; };
                if !same_client(&snapshot, &current) {
                    tab.save_status = EditorSaveStatus::Failed {
                        message: "The daemon connection changed while saving. Your edits are still here. Reload to verify the saved file before saving again.".into(),
                        save_blocked: true,
                    };
                } else {
                    match result {
                        Ok(version) => {
                            tab.saved_version = Some(version);
                            tab.is_dirty = editor.read(cx).value().as_str() != content;
                            tab.saved_content = content;
                            tab.save_status = EditorSaveStatus::Ready;
                        }
                        Err(error) => tab.save_status = failure(error),
                    }
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    pub(super) fn render_save_recovery(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let tab = self.active_tab_index.and_then(|index| self.tabs.get(index))?;
        if tab.is_diff { return None; }
        let editor = tab.editor_state.clone()?;
        threadlane_ui_kit::editor_save_recovery(
            format!("{} / {}", tab.project_dir.display(), tab.relative_path),
            &self.tab_save_status(tab, cx),
            cx.listener(move |this, action: &EditorRecoveryAction, window, cx| {
                let Some(index) = this.tabs.iter().position(|tab| tab.editor_state.as_ref() == Some(&editor)) else { return; };
                match action {
                    EditorRecoveryAction::Copy => cx.write_to_clipboard(ClipboardItem::new_string(editor.read(cx).value().to_string())),
                    EditorRecoveryAction::RetrySave => this.save_tab_at(index, cx),
                    EditorRecoveryAction::Reload => this.confirm_tab_reload(index, window, cx),
                }
            }), cx,
        )
    }

    fn confirm_tab_reload(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else { return; };
        if tab.save_status.busy() || tab.is_diff || tab.client_invalidated { return; }
        let Some(editor) = tab.editor_state.clone() else { return; };
        let revision = tab.buffer_revision;
        let content = editor.read(cx).value().to_string();
        let snapshot = self.current_client_snapshot(cx);
        let target = format!("{} / {}", tab.project_dir.display(), tab.relative_path);
        let view = cx.entity().downgrade();
        threadlane_ui_kit::confirm_editor_reload(target, window, cx, move |window, cx| {
            let _ = view.update(cx, |view, cx| view.reload_tab(&editor, revision, &content, snapshot.clone(), window, cx));
        });
    }

    fn reload_tab(&mut self, editor: &Entity<EditorState>, revision: u64, content: &str,
        snapshot: ClientSnapshot, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_client_snapshot(cx);
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.editor_state.as_ref() == Some(editor)) else { return; };
        if tab.save_status.busy() || tab.client_invalidated { return; }
        if !same_client(&snapshot, &current) || tab.buffer_revision != revision || editor.read(cx).value().as_str() != content {
            tab.save_status = EditorSaveStatus::Failed { message: "The document or connection changed after confirmation. Your edits are still here. Choose Reload from disk again.".into(), save_blocked: true };
            cx.notify(); return;
        }
        tab.save_generation = tab.save_generation.wrapping_add(1);
        let generation = tab.save_generation;
        tab.save_status = EditorSaveStatus::Reloading;
        let project = tab.project_dir.clone();
        let path = tab.relative_path.clone();
        let client = snapshot.client.clone();
        let epoch = snapshot.epoch;
        let task = cx.background_executor().spawn(async move {
            if client.file_search_connection_epoch() != epoch { return Err(ProjectFileError::Disconnected); }
            project_io::read_file_versioned(&client, &project, path).await
        });
        let editor = editor.clone();
        let content = content.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let current = this.current_client_snapshot(cx);
                let Some(tab) = this.tabs.iter_mut().find(|tab| tab.editor_state.as_ref() == Some(&editor)
                    && tab.save_generation == generation) else { return; };
                if !same_client(&snapshot, &current) || tab.buffer_revision != revision || editor.read(cx).value().as_str() != content {
                    tab.save_status = EditorSaveStatus::Failed { message: "The document or connection changed while reloading. Your edits are still here. Choose Reload from disk again.".into(), save_blocked: true };
                } else {
                    match result {
                        Ok(file) => {
                            editor.update(cx, |editor, cx| editor.set_value(file.content.clone(), window, cx));
                            tab.markdown_preview.refresh(file.content.clone().into(), cx);
                            tab.saved_content = file.content;
                            tab.saved_version = Some(file.version);
                            tab.baseline_loaded = true;
                            tab.client_epoch = snapshot.epoch;
                            tab.client_connected = snapshot.connected;
                            tab.open_error = None;
                            tab.pending_content = None;
                            tab.pending_content_version = None;
                            tab.loading = false;
                            tab.is_dirty = false;
                            tab.buffer_revision = tab.buffer_revision.wrapping_add(1);
                            tab.save_status = EditorSaveStatus::Ready;
                        }
                        Err(error) => tab.save_status = EditorSaveStatus::Failed { message: format!("Couldn't reload: {error}. Your edits are still here. Check the file and connection, then choose Reload from disk again."), save_blocked: true },
                    }
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
}
