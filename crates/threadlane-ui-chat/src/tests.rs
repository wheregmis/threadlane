#[gpui::test]
fn composer_model_controls_fit_narrow_and_wide_panels(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.selected_model = "some-future/model-with-a-long-display-name".into();
        state.is_new_task = false;
        state
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        gpui_component::Root::new(chat, window, cx)
    });
    for width in [320.0, 800.0] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(800.0)));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        for selector in ["composer-model-picker", "composer-reasoning-effort-picker"] {
            let bounds = cx
                .debug_bounds(selector)
                .expect("composer control is visible");
            assert!(bounds.size.width > gpui::px(0.0));
            assert!(
                bounds.left() >= gpui::px(0.0),
                "{selector} overflows left at {width}px"
            );
            assert!(
                bounds.right() <= gpui::px(width),
                "{selector} overflows right at {width}px: {bounds:?}"
            );
        }
    }
}

#[gpui::test]
fn reasoning_menu_reports_open_and_escape_dismissal(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.selected_model = "some-future/model-with-a-long-display-name".into();
        state.is_new_task = false;
        state
    });
    let (chat, cx) = cx.add_window_view(move |window, cx| {
        super::ChatListView::new(model, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let trigger = cx.debug_bounds("composer-reasoning-effort-picker").unwrap();
    cx.simulate_click(trigger.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    chat.read_with(cx, |chat, _| {
        assert!(chat.reasoning_menu_open.get(), "click must open menu");
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| {
        assert!(!chat.reasoning_menu_open.get(), "Escape must close menu");
    });
}

#[gpui::test]
fn composer_shift_enter_adds_a_line_without_sending(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let model = cx.new(|_| threadlane_ui_state::AppState::default());
    let retained_model = model.clone();
    let (chat, cx) =
        cx.add_window_view(move |window, cx| super::ChatListView::new(model, window, cx));
    chat.update_in(cx, |chat, window, cx| {
        chat.input_state.update(cx, |input, cx| {
            input.set_value("First line", window, cx);
        });
        chat.focus_composer(window, cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.simulate_keystrokes("end shift-enter");
    cx.run_until_parked();
    chat.read_with(cx, |chat, cx| {
        assert_eq!(chat.input_state.read(cx).value().as_ref(), "First line\n");
    });
    retained_model.read_with(cx, |state, _| {
        assert!(state.messages.is_empty(), "newline must not submit the draft");
        assert!(!state.is_generating);
    });
}

#[gpui::test]
fn stashing_requires_a_task_and_preserves_existing_stashes_and_images(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let model = cx.new(|_| threadlane_ui_state::AppState::default());
    let retained_model = model.clone();
    let (chat, cx) =
        cx.add_window_view(move |window, cx| super::ChatListView::new(model, window, cx));
    for (has_task, has_stash, has_image, generating) in [
        (false, false, false, false),
        (true, true, false, false),
        (true, false, true, false),
        (true, false, false, true),
        (true, false, false, false),
    ] {
        retained_model.update(cx, |state, cx| {
            state.active_session_id = has_task.then(|| "stash-task".into());
            state.is_generating = generating;
            state.clear_stashed_prompt("stash-task");
            if has_stash {
                state.stash_prompt("stash-task", "Previous stash".into());
            }
            cx.notify();
        });
        cx.run_until_parked();
        chat.update_in(cx, |chat, window, cx| {
            chat.input_state.update(cx, |input, cx| {
                input.set_value("Current draft", window, cx);
            });
            chat.pasted_images.clear();
            if has_image {
                chat.pasted_images.push(super::ImageAttachment {
                    display_name: "draft.png".into(),
                    data_url: "data:image/png;base64,test".into(),
                });
            }
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let stash = cx.debug_bounds("stash-prompt-btn").unwrap();
        cx.simulate_click(stash.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let allowed = has_task && !has_stash && !has_image && !generating;
        chat.read_with(cx, |chat, cx| {
            assert_eq!(
                chat.input_state.read(cx).value().as_ref(),
                if allowed { "" } else { "Current draft" }
            );
            assert_eq!(chat.pasted_images.len(), usize::from(has_image));
        });
        retained_model.read_with(cx, |state, _| {
            let expected = if has_stash {
                Some("Previous stash")
            } else if allowed {
                Some("Current draft")
            } else {
                None
            };
            assert_eq!(
                state.get_stashed_prompt("stash-task").map(String::as_str),
                expected
            );
        });
    }
}

#[gpui::test]
fn restoring_a_stash_preserves_unsent_text_and_images(cx: &mut gpui::TestAppContext) {
    use gpui::{AppContext as _, Focusable as _};

    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.active_session_id = Some("draft-session".into());
        state.is_new_task = false;
        state.stash_prompt("draft-session", "Saved request".into());
        state
    });
    let retained_model = model.clone();
    let holder = std::rc::Rc::new(std::cell::RefCell::new(None));
    let holder_clone = holder.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        holder_clone.borrow_mut().replace(chat.clone());
        struct DialogHost(gpui::Entity<super::ChatListView>);
        impl gpui::Render for DialogHost {
            fn render(&mut self, _window: &mut gpui::Window, _cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
                use gpui::{ParentElement as _, Styled as _};
                gpui::div().size_full().child(self.0.clone())
            }
        }
        let host = cx.new(|_| DialogHost(chat));
        gpui_component::Root::new(host, window, cx)
    });
    let chat = holder.borrow().as_ref().unwrap().clone();
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let discard = cx.debug_bounds("discard-stashed-draft").unwrap();
    cx.simulate_click(discard.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("dialog-0").is_some());
    retained_model.read_with(cx, |state, _| {
        assert_eq!(
            state.get_stashed_prompt("draft-session").map(String::as_str),
            Some("Saved request")
        );
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("dialog-0").is_none());
    for (draft, image) in [("Newer request", false), ("", true), ("", false)] {
        chat.update_in(cx, |chat, window, cx| {
            chat.input_state.update(cx, |input, cx| {
                input.set_value(draft, window, cx);
            });
            chat.pasted_images.clear();
            if image {
                chat.pasted_images.push(super::ImageAttachment {
                    display_name: "new.png".into(),
                    data_url: "data:image/png;base64,test".into(),
                });
            }
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let restore = cx.debug_bounds("restore-stashed-draft").unwrap();
        cx.simulate_click(restore.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let blocked = !draft.is_empty() || image;
        chat.read_with(cx, |chat, cx| {
            assert_eq!(
                chat.input_state.read(cx).value().as_ref(),
                if blocked { draft } else { "Saved request" }
            );
            assert_eq!(chat.pasted_images.len(), usize::from(image));
        });
        retained_model.read_with(cx, |state, _| {
            assert_eq!(state.get_stashed_prompt("draft-session").is_some(), blocked);
        });
        cx.update(|window, cx| {
            let focus = chat.read(cx).input_state.read(cx).focus_handle(cx);
            assert_eq!(window.focused(cx), Some(focus));
        });
    }
    chat.update_in(cx, |chat, window, cx| {
        chat.input_state.update(cx, |input, cx| {
            input.set_value("Keep this draft", window, cx);
        });
        chat.pasted_images.push(super::ImageAttachment {
            display_name: "keep.png".into(),
            data_url: "data:image/png;base64,keep".into(),
        });
        cx.notify();
    });
    for replaced in [false, true] {
        retained_model.update(cx, |state, cx| {
            state.stash_prompt("draft-session", "Discard this stash".into());
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let discard = cx.debug_bounds("discard-stashed-draft").unwrap();
        cx.simulate_click(discard.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("dialog-0").is_some());
        if replaced {
            retained_model.update(cx, |state, cx| {
                state.stash_prompt("draft-session", "New saved draft".into());
                cx.notify();
            });
        }
        cx.update(|window, cx| {
            window.dispatch_action(
                Box::new(gpui_component::dialog::Confirm { secondary: false }),
                cx,
            );
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("dialog-0").is_none());
        retained_model.read_with(cx, |state, _| {
            assert_eq!(
                state.get_stashed_prompt("draft-session").map(String::as_str),
                replaced.then_some("New saved draft")
            );
        });
        chat.read_with(cx, |chat, cx| {
            assert_eq!(chat.input_state.read(cx).value().as_ref(), "Keep this draft");
            assert_eq!(chat.pasted_images.len(), 1);
            assert_eq!(chat.pasted_images[0].data_url, "data:image/png;base64,keep");
        });
    }
}

#[gpui::test]
fn unsupported_acp_steer_keeps_the_composer_text_and_images(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.selected_model = "acp/test".into();
        state.is_generating = true;
        state
    });
    let retained_model = model.clone();
    let (chat, cx) =
        cx.add_window_view(move |window, cx| super::ChatListView::new(model, window, cx));
    chat.update_in(cx, |chat, window, cx| {
        chat.pasted_images.push(super::ImageAttachment {
            display_name: "draft.png".into(),
            data_url: "data:image/png;base64,test".into(),
        });
        chat.input_state.update(cx, |input, cx| {
            input.set_value("Keep this draft", window, cx);
            cx.emit(super::InputEvent::PressEnter {
                secondary: true,
                shift: false,
            });
        });
    });
    cx.run_until_parked();
    chat.update(cx, |chat, cx| {
        assert_eq!(
            chat.input_state.read(cx).value().as_ref(),
            "Keep this draft"
        );
        assert_eq!(chat.pasted_images.len(), 1);
    });
    retained_model.read_with(cx, |state, _| {
        assert!(state
            .session_status
            .as_deref()
            .unwrap()
            .contains("Use Queue"));
        assert!(state.active_pending_composer_message().is_none());
    });
}

#[gpui::test]
fn unsent_composer_drafts_and_images_follow_their_task(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.active_work_dir = Some("/projects/one".into());
        state.active_session_id = Some("first".into());
        state
    });
    let retained_model = model.clone();
    let (chat, cx) =
        cx.add_window_view(move |window, cx| super::ChatListView::new(model, window, cx));
    chat.update_in(cx, |chat, window, cx| {
        chat.input_state.update(cx, |input, cx| {
            input.set_value("First task draft", window, cx);
        });
        chat.pasted_images.push(super::ImageAttachment {
            display_name: "first.png".into(),
            data_url: "data:image/png;base64,test".into(),
        });
    });
    retained_model.update(cx, |state, cx| {
        state.active_session_id = Some("second".into());
        cx.notify();
    });
    cx.run_until_parked();
    chat.update_in(cx, |chat, window, cx| {
        assert!(chat.input_state.read(cx).value().is_empty());
        assert!(chat.pasted_images.is_empty());
        chat.input_state.update(cx, |input, cx| {
            input.set_value("Second task draft", window, cx);
        });
    });
    retained_model.update(cx, |state, cx| {
        state.active_session_id = Some("first".into());
        cx.notify();
    });
    cx.run_until_parked();
    chat.update(cx, |chat, cx| {
        assert_eq!(
            chat.input_state.read(cx).value().as_ref(),
            "First task draft"
        );
        assert_eq!(chat.pasted_images[0].display_name, "first.png");
    });
    retained_model.update(cx, |state, cx| {
        state.active_session_id = None;
        cx.notify();
    });
    cx.run_until_parked();
    chat.update_in(cx, |chat, window, cx| {
        assert!(chat.input_state.read(cx).value().is_empty());
        assert!(chat.pasted_images.is_empty());
        chat.input_state.update(cx, |input, cx| {
            input.set_value("New task draft", window, cx);
        });
    });
    retained_model.update(cx, |state, cx| {
        state.active_work_dir = Some("/projects/two".into());
        cx.notify();
    });
    cx.run_until_parked();
    chat.update(cx, |chat, cx| {
        assert!(chat.input_state.read(cx).value().is_empty())
    });
    retained_model.update(cx, |state, cx| {
        state.active_work_dir = Some("/projects/one".into());
        cx.notify();
    });
    cx.run_until_parked();
    chat.update(cx, |chat, cx| {
        assert_eq!(chat.input_state.read(cx).value().as_ref(), "New task draft");
    });
}

use super::{
    active_slash_command_query, build_trajectory_rows, build_transcript_rows, classify_chat_link,
    classify_markdown_update, contains_case_insensitive, context_meter_view_model,
    editor_target_matches_active_work_dir, extend_trajectory_facets, extend_trajectory_previews,
    extend_trajectory_rows, extend_trajectory_summary, extract_markdown_segments,
    format_trajectory_raw_json, grouped_tool_activities, is_terminal_runnable_language,
    markdown_cache_exceeded, next_chat_stream_batch, normalize_terminal_command,
    reconcile_trajectory_entries, reconcile_trajectory_entries_by_epoch, subagent_popover_counts,
    summarize_trajectory, ChatLinkTarget, ContextMeterContext, ContextMeterMetrics,
    MarkdownSegment, MarkdownUpdate, TrajectoryCacheKey, TrajectoryInspectorTab, TrajectoryMode,
    TrajectoryRenderCache, TrajectoryRow, TrajectorySummary,
    TranscriptRow, INPUT_KEY_CONTEXT, MARKDOWN_CACHE_ENTRY_LIMIT, SLASH_COMMAND_BINDING_CONTEXT,
    SLASH_COMMAND_KEY_CONTEXT,
};

#[gpui::test]
fn chat_errors_are_bounded_deduplicated_and_keep_recovery_details(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    struct ErrorHarness {
        model: gpui::Entity<threadlane_ui_state::AppState>,
        error: String,
    }

    impl gpui::Render for ErrorHarness {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            super::render_chat_error("test", &self.error, &self.model, cx)
        }
    }

    let error = format!(
        "Provider HTTP 401: {{\"error\":{{\"code\":\"token_expired\",\"detail\":\"{}\"}}}}",
        "diagnostic ".repeat(1_000)
    );
    let (summary, needs_settings) = super::chat_error_summary(&error);
    assert!(needs_settings);
    assert!(summary.contains("Sign in again"));
    assert!(!summary.contains("token_expired"));
    assert!(summary.chars().count() < 240);
    assert_eq!(
        super::chat_error_summary(&"界".repeat(500))
            .0
            .chars()
            .count(),
        241
    );
    assert_eq!(
        super::chat_error_summary("\nNetwork failed\nraw detail").0,
        "Network failed"
    );

    let mut message = threadlane_ui_state::ChatMessageInfo {
        id: "error".into(),
        role: threadlane_ui_state::MessageRole::Error,
        content: error.clone(),
        tool_activities: Vec::new(),
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
    };
    assert!(super::visible_session_status(Some(&error), Some(&message)).is_none());
    assert_eq!(
        super::visible_session_status(Some("Message queued…"), Some(&message)),
        Some("Message queued…")
    );
    message.role = threadlane_ui_state::MessageRole::Assistant;
    assert_eq!(
        super::visible_session_status(Some(&error), Some(&message)),
        Some(error.as_str())
    );

    cx.update(gpui_component::init);
    let model = cx.new(|_| threadlane_ui_state::AppState::default());
    let expected_error = error.clone();
    let retained_model = model.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|_| ErrorHarness { model, error });
        gpui_component::Root::new(view, window, cx)
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let copy = cx.debug_bounds("chat-error-copy").unwrap();
    cx.simulate_click(copy.center(), gpui::Modifiers::default());
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(expected_error)
    );

    let settings = cx.debug_bounds("chat-error-settings").unwrap();
    cx.simulate_click(settings.center(), gpui::Modifiers::default());
    assert_eq!(
        retained_model.read_with(cx, |model, _| model.workspace_page),
        threadlane_ui_state::WorkspacePage::Settings
    );
}

#[test]
fn last_retryable_prompt_prefers_the_latest_user_message() {
    let user = |id: &str, content: &str| ChatMessageInfo {
        id: id.into(),
        role: MessageRole::User,
        content: content.into(),
        tool_activities: Vec::new(),
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
    };
    let assistant = |id: &str| ChatMessageInfo {
        id: id.into(),
        role: MessageRole::Assistant,
        content: "done".into(),
        tool_activities: Vec::new(),
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
    };
    assert_eq!(super::last_retryable_prompt(&[]), None);
    assert_eq!(super::last_retryable_prompt(&[assistant("a")]), None);
    assert_eq!(
        super::last_retryable_prompt(&[user("u", "   ")]),
        None,
        "whitespace-only prompts cannot be resent"
    );
    assert_eq!(
        super::last_retryable_prompt(&[user("u1", "first"), assistant("a"), user("u2", "second")]),
        Some("second".to_string()),
        "retry resends the latest user prompt, not an earlier one"
    );
}

#[gpui::test]
fn error_retry_appears_only_with_a_user_prompt_and_no_active_run(
    cx: &mut gpui::TestAppContext,
) {
    use gpui::AppContext as _;

    struct ErrorHarness {
        model: gpui::Entity<threadlane_ui_state::AppState>,
        error: String,
    }

    impl gpui::Render for ErrorHarness {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            super::render_chat_error("retry-test", &self.error, &self.model, cx)
        }
    }

    let message = |role: MessageRole, content: &str| ChatMessageInfo {
        id: format!("{role:?}-retry"),
        role,
        content: content.into(),
        tool_activities: Vec::new(),
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
    };
    let show_error = |cx: &mut gpui::TestAppContext,
                        model: gpui::Entity<threadlane_ui_state::AppState>| {
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|_| ErrorHarness {
                model,
                error: "boom".into(),
            });
            gpui_component::Root::new(view, window, cx)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.debug_bounds("chat-error-retry").is_some()
    };

    cx.update(gpui_component::init);
    let without_prompt = cx.new(|_| threadlane_ui_state::AppState::default());
    assert!(
        !show_error(cx, without_prompt),
        "no user prompt means nothing to resend"
    );

    let with_prompt = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.messages = vec![
            message(MessageRole::User, "try again"),
            message(MessageRole::Error, "boom"),
        ]
        .into();
        state
    });
    assert!(
        show_error(cx, with_prompt),
        "a failed turn with a prior user prompt offers Retry"
    );

    let generating = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.messages = vec![message(MessageRole::User, "try again")].into();
        state.is_generating = true;
        state
    });
    assert!(
        !show_error(cx, generating),
        "retry stays hidden while a generation is running"
    );
}

#[test]
fn editor_targets_only_open_for_the_active_git_checkout() {
    let worktree = std::path::Path::new("/projects/app/.threadlane/worktrees/session");
    let canonical = std::path::Path::new("/projects/app");

    assert!(editor_target_matches_active_work_dir(
        worktree,
        Some(worktree)
    ));
    assert!(!editor_target_matches_active_work_dir(
        worktree,
        Some(canonical)
    ));
    assert!(!editor_target_matches_active_work_dir(worktree, None));
}
use threadlane_ui_state::{
    reported_session_shape_state, ChatMessageInfo, ChatStreamEvent, MessageRole,
    SubagentActivityStatus, ToolActivityInfo, TrajectoryDiagnostics, TrajectoryEntry,
};

#[test]
fn markdown_cache_resets_only_after_its_limit() {
    assert!(!markdown_cache_exceeded(MARKDOWN_CACHE_ENTRY_LIMIT));
    assert!(markdown_cache_exceeded(MARKDOWN_CACHE_ENTRY_LIMIT + 1));
}

#[tokio::test]
async fn chat_stream_batch_waits_then_caps_ready_events() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(tokio::time::timeout(
        std::time::Duration::from_millis(10),
        next_chat_stream_batch(&mut rx),
    )
    .await
    .is_err());
    for index in 0..130 {
        tx.send(ChatStreamEvent::Finished {
            session_id: index.to_string(),
            session_file: std::path::PathBuf::new(),
        })
        .unwrap();
    }
    assert_eq!(next_chat_stream_batch(&mut rx).await.unwrap().len(), 128);
    assert_eq!(next_chat_stream_batch(&mut rx).await.unwrap().len(), 2);
}

#[test]
fn trajectory_search_matches_ascii_without_case_sensitivity() {
    assert!(contains_case_insensitive("Read File", "read"));
    assert!(contains_case_insensitive("TOOL-CALL-42", "call-42"));
    assert!(!contains_case_insensitive("Write File", "read"));
}

#[test]
fn subagent_popover_counts_items_without_owning_them() {
    assert_eq!(subagent_popover_counts([]), None);
    assert_eq!(
        subagent_popover_counts([
            SubagentActivityStatus::Queued,
            SubagentActivityStatus::Running,
            SubagentActivityStatus::Completed,
        ]),
        Some((3, 2))
    );
}

#[test]
fn trajectory_search_preserves_unicode_lowercase_matching() {
    assert!(contains_case_insensitive("CAFÉ output", "café"));
    assert!(contains_case_insensitive("Kelvin", "kelvin"));
    assert!(!contains_case_insensitive("CAFÉ output", "résumé"));
}

#[test]
fn chat_link_classifies_web_urls_as_external() {
    assert_eq!(
        classify_chat_link("https://example.com/spec"),
        ChatLinkTarget::Web
    );
    assert_eq!(
        classify_chat_link("http://example.com/spec"),
        ChatLinkTarget::Web
    );
}

#[test]
fn chat_link_normalizes_safe_project_relative_paths() {
    assert_eq!(
        classify_chat_link("docs/spec.md"),
        ChatLinkTarget::ProjectFile("docs/spec.md".into())
    );
    assert_eq!(
        classify_chat_link("docs/design/../spec.md"),
        ChatLinkTarget::ProjectFile("docs/spec.md".into())
    );
}

#[test]
fn chat_link_rejects_absolute_and_escaping_paths() {
    assert_eq!(classify_chat_link("/tmp/spec.md"), ChatLinkTarget::Rejected);
    assert_eq!(
        classify_chat_link("../../outside.md"),
        ChatLinkTarget::Rejected
    );
}

#[test]
fn chat_link_does_not_parse_line_or_fragment_suffixes() {
    assert_eq!(
        classify_chat_link("src/main.rs:42"),
        ChatLinkTarget::ProjectFile("src/main.rs:42".into())
    );
    assert_eq!(
        classify_chat_link("src/main.rs#L42"),
        ChatLinkTarget::ProjectFile("src/main.rs#L42".into())
    );
}
fn metrics_with_usage(
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
) -> ContextMeterMetrics {
    let billed_input_tokens = input_tokens
        .saturating_add(cache_read_tokens)
        .saturating_add(cache_write_tokens);
    ContextMeterMetrics {
        billed_input_tokens,
        output_tokens,
        cache_hit_percent: (billed_input_tokens > 0).then(|| {
            (((cache_read_tokens as u128) * 100 + (billed_input_tokens as u128) / 2)
                / billed_input_tokens as u128) as u64
        }),
    }
}

fn estimating_context() -> ContextMeterContext {
    ContextMeterContext {
        current_tokens: 0,
        context_limit: 0,
        context_limit_is_estimate: false,
        effective_model: "new-model".into(),
        last_compaction_seq: None,
        provisional: false,
        estimating: true,
    }
}

#[test]
fn a_model_that_never_reports_usage_is_not_shown_as_estimating() {
    // An ACP agent runs its own loop and sends no token accounting, so the
    // meter has nothing to project. Rendering that as "Estimating…" both
    // promises a number that never arrives and leaves the badge animating
    // for the whole turn.
    let view = context_meter_view_model(None, &ContextMeterMetrics::default(), false);
    assert_eq!(view.current_label, "Not reported");
    assert_eq!(view.percent, None);
    assert_eq!(view.bar_percent, 0.0);

    // A model that does report usage keeps the pending label until a
    // context figure arrives.
    let estimating = context_meter_view_model(None, &ContextMeterMetrics::default(), true);
    assert_eq!(estimating.current_label, "Unavailable");
}

#[test]
fn an_unreported_context_still_shows_what_was_processed() {
    // Turn counts and any usage Threadlane did observe stay meaningful
    // even when the context window itself is unmeasurable.
    let view = context_meter_view_model(
        None,
        &ContextMeterMetrics {
            billed_input_tokens: 1_200,
            output_tokens: 800,
            cache_hit_percent: Some(40),
        },
        false,
    );
    assert_eq!(view.total_processed_label, "2.0k");
    assert_eq!(view.cache_hit_label.as_deref(), Some("40%"));
}

#[tokio::test]
async fn meter_separates_current_context_from_total_processed() {
    let (_path, state) = reported_session_shape_state().await;
    let _projected_context = state.active_context_window().unwrap();
    let projected_metrics = state.active_session_metrics();
    let view = context_meter_view_model(
        Some(&ContextMeterContext {
            current_tokens: 38_278,
            context_limit: 128_000,
            context_limit_is_estimate: false,
            effective_model: "gpt-4o".into(),
            last_compaction_seq: None,
            provisional: false,
            estimating: false,
        }),
        &ContextMeterMetrics {
            billed_input_tokens: projected_metrics.billed_input_tokens(),
            output_tokens: projected_metrics.output_tokens,
            cache_hit_percent: projected_metrics.cache_hit_percent(),
        },
        true,
    );
    let percent = view.percent.expect("known context percentage");
    assert!((percent - 29.904_687_5).abs() < 1e-12);
    assert!((view.bar_percent - 29.904_687_5).abs() < 1e-12);
    assert_eq!(view.current_label, "38.3k / 128.0k");
    assert!(view.total_processed_label.ends_with('M'));
    assert_ne!(view.total_processed_label, view.current_label);
    assert!(view.cache_hit_label.is_some());
    assert_eq!(view.detail_label, "Context usage details, 30% used");
}

#[test]
fn meter_estimating_context_has_no_false_percentage() {
    let view = context_meter_view_model(
        Some(&estimating_context()),
        &ContextMeterMetrics::default(),
        true,
    );
    assert_eq!(view.percent, None);
    assert_eq!(view.current_label, "Unavailable");
    assert_eq!(view.bar_percent, 0.0);
    assert_eq!(view.detail_label, "Context usage details, current usage unavailable");
}

#[test]
fn meter_treats_zero_context_limit_as_unknown_even_when_not_estimating() {
    let mut context = estimating_context();
    context.current_tokens = 42;
    context.estimating = false;

    let view = context_meter_view_model(Some(&context), &ContextMeterMetrics::default(), true);

    assert_eq!(view.percent, None);
    assert_eq!(view.current_label, "Unavailable");
    assert_eq!(view.bar_percent, 0.0);
    assert_eq!(view.detail_label, "Context usage details, current usage unavailable");
}

#[test]
fn meter_cache_hit_rounding_uses_wide_intermediates_at_u64_max() {
    let metrics = metrics_with_usage(0, 0, u64::MAX, 0);

    assert_eq!(metrics.billed_input_tokens, u64::MAX);
    assert_eq!(metrics.cache_hit_percent, Some(100));
}

#[test]
fn meter_labels_estimated_limit_and_clamps_only_bar() {
    let view = context_meter_view_model(
        Some(&ContextMeterContext {
            current_tokens: 120_000,
            context_limit: 100_000,
            context_limit_is_estimate: true,
            effective_model: "model".into(),
            last_compaction_seq: Some(42),
            provisional: true,
            estimating: false,
        }),
        &ContextMeterMetrics::default(),
        true,
    );
    assert_eq!(view.percent, Some(120.0));
    assert_eq!(view.bar_percent, 100.0);
    assert_eq!(view.current_label, "120.0k / ~100.0k");
    assert_eq!(view.last_compaction_seq, Some(42));
    assert!(view.provisional);
}

#[test]
fn markdown_update_appends_only_the_new_suffix() {
    assert_eq!(
        classify_markdown_update("Hello", "Hello **world**"),
        MarkdownUpdate::Append(" **world**")
    );
}

#[test]
fn markdown_update_skips_identical_content() {
    assert_eq!(
        classify_markdown_update("Hello", "Hello"),
        MarkdownUpdate::Unchanged
    );
}

#[test]
fn markdown_update_replaces_non_append_changes() {
    assert_eq!(
        classify_markdown_update("Hello", "Jello"),
        MarkdownUpdate::Replace
    );
    assert_eq!(
        classify_markdown_update("Hello", "Hello there"),
        MarkdownUpdate::Append(" there")
    );
    assert_eq!(
        classify_markdown_update("Hello there", "Hello"),
        MarkdownUpdate::Replace
    );
    assert_eq!(
        classify_markdown_update("Hello", "Hello!"),
        MarkdownUpdate::Append("!")
    );
    assert_eq!(
        classify_markdown_update("Hello", "Jello there"),
        MarkdownUpdate::Replace
    );
}

#[test]
fn grouped_tool_activities_borrows_in_order_and_hides_plan_updates() {
    let activity_message = |activities: &[(&str, &str)]| ChatMessageInfo {
        id: activities[0].0.into(),
        role: MessageRole::Assistant,
        content: String::new(),
        tool_activities: activities
            .iter()
            .map(|(id, title)| ToolActivityInfo {
                id: (*id).into(),
                category: "tool".into(),
                title: (*title).into(),
                display_summary: String::new(),
                detail: String::new(),
                is_expanded: false,
            })
            .collect(),
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
    };
    let messages = vec![
        activity_message(&[("tool-1", "read_file"), ("plan", "update_plan")]),
        activity_message(&[("tool-2", "write_file")]),
    ];

    let ids = grouped_tool_activities(&messages)
        .map(|activity| activity.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(ids, vec!["tool-1", "tool-2"]);
}

#[test]
fn progress_summary_never_reuses_a_previous_turns_tool() {
    let user = ChatMessageInfo {
        id: "prompt".into(),
        role: MessageRole::User,
        content: "Follow up".into(),
        tool_activities: vec![],
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
    };
    let mut activity = user.clone();
    activity.role = MessageRole::Assistant;
    activity.content.clear();
    activity.tool_activities.push(ToolActivityInfo {
        id: "read".into(),
        category: "Completed".into(),
        title: "read_file".into(),
        display_summary: "Read README.md".into(),
        detail: "Old turn output".into(),
        is_expanded: false,
    });
    let mut messages = vec![activity.clone(), user];
    assert!(super::current_turn_latest_tool(&messages).is_none());
    activity.tool_activities[0].id = "current".into();
    messages.push(activity);
    assert_eq!(
        super::current_turn_latest_tool(&messages).unwrap().id,
        "current"
    );
    for prefix in ["queued-user", "steered-user"] {
        let mut pending = messages[1].clone();
        pending.id = format!("{prefix}-session-3");
        messages.push(pending);
        assert_eq!(
            super::current_turn_latest_tool(&messages).unwrap().id,
            "current"
        );
    }
    assert!(super::current_turn_latest_tool(&[]).is_none());
}

#[test]
fn transcript_rows_group_consecutive_tool_only_messages() {
    let message = |id: &str, activity: bool| ChatMessageInfo {
        id: id.into(),
        role: if activity {
            MessageRole::Assistant
        } else {
            MessageRole::User
        },
        content: if activity { "" } else { id }.into(),
        tool_activities: activity
            .then(|| ToolActivityInfo {
                id: format!("tool-{id}"),
                category: "tool".into(),
                title: "read_file".into(),
                display_summary: String::new(),
                detail: String::new(),
                is_expanded: false,
            })
            .into_iter()
            .collect(),
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
    };
    let messages = vec![
        message("user", false),
        message("tool-1", true),
        message("tool-2", true),
        message("answer", false),
    ];

    assert_eq!(
        build_transcript_rows(&messages, true),
        vec![
            TranscriptRow::Message(0),
            TranscriptRow::Activities(1..3),
            TranscriptRow::Message(3),
            TranscriptRow::Working,
        ]
    );
}

#[test]
fn queued_messages_leave_transcript_only_while_generating() {
    let messages: Vec<_> = [
        "user",
        "queued-user-session-1",
        "steered-user-session-2",
        "queued-user-session-3",
    ]
    .into_iter()
    .map(|id| ChatMessageInfo {
        id: id.into(),
        role: MessageRole::User,
        content: id.into(),
        tool_activities: Vec::new(),
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
    })
    .collect();
    assert_eq!(
        build_transcript_rows(&messages, true),
        vec![
            TranscriptRow::Message(0),
            TranscriptRow::Message(2),
            TranscriptRow::Working
        ]
    );
    assert_eq!(
        build_transcript_rows(&messages, false),
        (0..4).map(TranscriptRow::Message).collect::<Vec<_>>()
    );
}

#[gpui::test]
fn queue_filter_transitions_keep_retained_list_in_sync(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let model = cx.new(|_| threadlane_ui_state::AppState::default());
    let (chat, cx) = cx.add_window_view(move |window, cx| {
        super::ChatListView::new(model, window, cx)
    });
    chat.update(cx, |chat, _| {
        for queued_count in 0..=3 {
            let messages = std::sync::Arc::new(
                (0..=queued_count)
                    .map(|index| ChatMessageInfo {
                        id: if index == 0 {
                            "user".into()
                        } else {
                            format!("queued-user-session-{index}")
                        },
                        role: MessageRole::User,
                        content: format!("Message {index}"),
                        tool_activities: Vec::new(),
                        streaming: false,
                        reasoning_content: None,
                        reasoning_expanded: false,
                    })
                    .collect::<Vec<_>>(),
            );
            chat.sync_transcript_rows(messages.clone(), true, true);
            for generating in [false, true, false] {
                chat.sync_transcript_rows(messages.clone(), generating, false);
                let expected = build_transcript_rows(&messages, generating);
                assert_eq!(chat.transcript_rows, expected);
                assert_eq!(
                    chat.transcript_list_state.item_count(),
                    expected.len(),
                    "queue size {queued_count}, generating {generating}"
                );
            }
        }
    });
}

#[gpui::test]
fn queued_panel_tracks_active_messages_and_generation(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state.is_generating = true;
        threadlane_ui_state::activate_test_session(
            &mut state,
            "session-1",
            std::path::Path::new("/test-project/.threadlane/sessions/session-1.jsonl"),
        );
        state.messages = (0..2)
            .map(|index| ChatMessageInfo {
                id: format!("queued-user-session-1-{index}"),
                role: MessageRole::User,
                content: format!("Follow-up {index}\nKeep the full multiline message visible"),
                tool_activities: Vec::new(),
                streaming: false,
                reasoning_content: None,
                reasoning_expanded: false,
            })
            .collect::<Vec<_>>()
            .into();
        state
    });
    let retained_model = model.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        gpui_component::Root::new(chat, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("queued-messages-panel").is_some());
    assert!(cx.debug_bounds("queued-message-row").is_some());
    retained_model.update(cx, |state, cx| {
        state.is_generating = false;
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("queued-messages-panel").is_none());
    retained_model.update(cx, |state, cx| {
        state.is_generating = true;
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("queued-messages-panel").is_some());
    retained_model.update(cx, |state, cx| {
        state.messages = Vec::new().into();
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("queued-messages-panel").is_none());
}

#[test]
fn selected_trajectory_entry_formats_as_raw_json() {
    let entry = TrajectoryEntry {
        seq: Some(1),
        run_id: None,
        turn: None,
        request: None,
        category: "Tool".into(),
        summary: "Read file".into(),
        detail: "src/main.rs".into(),
        lane: None,
        correlation_id: None,
        diagnostics: TrajectoryDiagnostics::default(),
    };

    let raw = format_trajectory_raw_json(&entry);

    assert!(raw.contains("\"category\": \"Tool\""));
    assert!(raw.contains("\"summary\": \"Read file\""));
}

fn trajectory_entry(category: &str, request: Option<u32>, turn: Option<u32>) -> TrajectoryEntry {
    TrajectoryEntry {
        seq: None,
        run_id: None,
        turn,
        request,
        category: category.into(),
        summary: category.into(),
        detail: String::new(),
        lane: None,
        correlation_id: None,
        diagnostics: TrajectoryDiagnostics::default(),
    }
}

#[test]
fn trajectory_cache_reuses_entries_for_append_only_updates() {
    let cached = vec![trajectory_entry("Input", Some(1), Some(1))];
    let source = vec![
        cached[0].clone(),
        trajectory_entry("Tool", Some(1), Some(1)),
    ];

    assert_eq!(reconcile_trajectory_entries(cached, &source), source);
}

#[test]
fn trajectory_cache_replaces_entries_when_existing_data_changes() {
    let cached = vec![trajectory_entry("Input", Some(1), Some(1))];
    let source = vec![trajectory_entry("Assistant", Some(1), Some(1))];

    assert_eq!(reconcile_trajectory_entries(cached, &source), source);
}

#[test]
fn trajectory_epoch_distinguishes_append_from_replacement() {
    let cached = vec![trajectory_entry("Input", Some(1), Some(1))];
    let appended_source = vec![
        cached[0].clone(),
        trajectory_entry("Tool", Some(1), Some(1)),
    ];
    let (entries, appended) =
        reconcile_trajectory_entries_by_epoch(cached.clone(), &appended_source, 7, 7);
    assert!(appended);
    assert_eq!(entries, appended_source);

    let replacement = vec![trajectory_entry("Assistant", Some(1), Some(1))];
    let (entries, appended) = reconcile_trajectory_entries_by_epoch(cached, &replacement, 7, 8);
    assert!(!appended);
    assert_eq!(entries, replacement);
}

#[test]
fn trajectory_incremental_facets_match_full_rebuild() {
    let mut input = trajectory_entry("Input", Some(1), Some(1));
    input.lane = Some("main".into());
    let mut tool = trajectory_entry("Tool", Some(1), Some(1));
    tool.lane = Some("main".into());
    let mut anomaly = trajectory_entry("Anomaly", Some(1), Some(2));
    anomaly.lane = Some("child".into());
    let appended_tool = trajectory_entry("Tool", Some(1), Some(2));
    let entries = vec![input, tool, anomaly, appended_tool];
    let key = TrajectoryCacheKey {
        revision: 4,
        epoch: 1,
        mode: TrajectoryMode::Execution,
        query: "tool".into(),
        category: None,
        lane: None,
    };

    let mut incremental = (Vec::new(), std::collections::BTreeMap::new(), Vec::new());
    extend_trajectory_facets(
        &mut incremental.0,
        &mut incremental.1,
        &mut incremental.2,
        &entries[..2],
        0,
        &key,
    );
    extend_trajectory_facets(
        &mut incremental.0,
        &mut incremental.1,
        &mut incremental.2,
        &entries,
        2,
        &key,
    );

    let mut rebuilt = (Vec::new(), std::collections::BTreeMap::new(), Vec::new());
    extend_trajectory_facets(
        &mut rebuilt.0,
        &mut rebuilt.1,
        &mut rebuilt.2,
        &entries,
        0,
        &key,
    );
    assert_eq!(incremental, rebuilt);
    assert_eq!(incremental.0, vec!["Anomaly", "Input", "Tool"]);
    assert_eq!(incremental.2, vec![1, 3]);
}

#[test]
fn trajectory_previews_extend_without_reformatting_existing_entries() {
    let mut input = trajectory_entry("Input", Some(1), Some(1));
    input.summary = "Prompt".into();
    input.detail = "first\nsecond".into();
    let mut tool = trajectory_entry("Tool", Some(1), Some(1));
    tool.summary = "read_file".into();
    tool.detail.clear();
    let entries = vec![input, tool];
    let mut previews = Vec::new();

    extend_trajectory_previews(&mut previews, &entries[..1], 0);
    extend_trajectory_previews(&mut previews, &entries, 1);

    assert_eq!(previews, ["Prompt  first second", "read_file"]);
}

#[test]
fn trajectory_rows_preserve_request_headers_and_setup_boundaries() {
    let entries = vec![
        trajectory_entry("Provider", Some(1), Some(1)),
        trajectory_entry("Input", Some(1), Some(1)),
        trajectory_entry("Input", Some(2), Some(2)),
    ];

    assert_eq!(
        build_trajectory_rows(&entries, &[0, 1, 2], TrajectoryMode::Requests),
        vec![
            TrajectoryRow::RequestHeader(1),
            TrajectoryRow::Setup,
            TrajectoryRow::Entry(0),
            TrajectoryRow::Entry(1),
            TrajectoryRow::RequestHeader(2),
            TrajectoryRow::Entry(2),
        ]
    );
}

#[test]
fn trajectory_incremental_rows_match_full_rebuild() {
    let entries = vec![
        trajectory_entry("Provider", Some(1), Some(1)),
        trajectory_entry("Input", Some(1), Some(1)),
        trajectory_entry("Input", Some(2), Some(2)),
    ];
    let indices = [0, 1, 2];
    let mut incremental = Vec::new();
    extend_trajectory_rows(
        &mut incremental,
        &entries,
        &indices[..2],
        0,
        TrajectoryMode::Requests,
    );
    extend_trajectory_rows(
        &mut incremental,
        &entries,
        &indices,
        2,
        TrajectoryMode::Requests,
    );

    assert_eq!(
        incremental,
        build_trajectory_rows(&entries, &indices, TrajectoryMode::Requests)
    );
}

#[test]
fn trajectory_summary_is_computed_once_from_canonical_entries() {
    let mut tool = trajectory_entry("Tool", Some(1), Some(3));
    tool.diagnostics.duration_ms = Some(25);
    let mut anomaly = trajectory_entry("Anomaly", Some(1), Some(4));
    anomaly.diagnostics.duration_ms = Some(75);
    anomaly.diagnostics.is_anomaly = true;

    let summary = summarize_trajectory(&[tool, anomaly]);

    assert_eq!(summary.tool_count, 1);
    assert_eq!(summary.total_duration_ms, 100);
    assert_eq!(summary.anomaly_count, 1);
    assert_eq!(summary.max_turn, 4);
}

#[test]
fn trajectory_summary_append_matches_full_rebuild() {
    let initial = vec![trajectory_entry("Input", Some(1), Some(1))];
    let mut tool = trajectory_entry("Tool", Some(1), Some(1));
    tool.diagnostics.duration_ms = Some(25);
    let mut anomaly = trajectory_entry("Anomaly", Some(1), Some(2));
    anomaly.diagnostics.duration_ms = Some(75);
    let appended = vec![tool, anomaly];
    let mut incremental = summarize_trajectory(&initial);
    extend_trajectory_summary(&mut incremental, &appended);

    let rebuilt = summarize_trajectory(&initial.into_iter().chain(appended).collect::<Vec<_>>());
    assert_eq!(incremental.overview_positions, rebuilt.overview_positions);
    assert_eq!(incremental.tool_count, rebuilt.tool_count);
    assert_eq!(incremental.total_duration_ms, 100);
    assert_eq!(incremental.anomaly_count, rebuilt.anomaly_count);
    assert_eq!(incremental.max_turn, rebuilt.max_turn);
}
#[test]
fn trajectory_cache_key_changes_with_data_or_filter() {
    let base = TrajectoryCacheKey {
        revision: 7,
        epoch: 2,
        mode: TrajectoryMode::Execution,
        query: "tool".into(),
        category: None,
        lane: None,
    };
    let mut changed = base.clone();
    changed.revision += 1;
    assert_ne!(base, changed);

    let mut changed = base.clone();
    changed.query = "provider".into();
    assert_ne!(base, changed);
}

#[test]
fn extract_markdown_segments_parses_mixed_content_with_paths() {
    let input = "Here is the command:\n```bash\ncargo build --workspace\n```\nAnd code in file:\n```rust src/main.rs\n// src/main.rs\nfn main() {}\n```\nFinished!";
    let segments = extract_markdown_segments(input);
    assert_eq!(segments.len(), 5);
    assert_eq!(
        segments[0],
        MarkdownSegment::Markdown("Here is the command:\n".into())
    );
    assert_eq!(
        segments[1],
        MarkdownSegment::CodeBlock {
            language: "bash".into(),
            header_path: None,
            code: "cargo build --workspace\n".into(),
        }
    );
    assert_eq!(
        segments[2],
        MarkdownSegment::Markdown("And code in file:\n".into())
    );
    assert_eq!(
        segments[3],
        MarkdownSegment::CodeBlock {
            language: "rust".into(),
            header_path: Some("src/main.rs".into()),
            code: "// src/main.rs\nfn main() {}\n".into(),
        }
    );
    assert_eq!(segments[4], MarkdownSegment::Markdown("Finished!".into()));
}

#[test]
fn extract_markdown_segments_handles_comment_path_heuristic() {
    let input = "```typescript\n// app/routes/index.tsx\nexport default function Home() {}\n```";
    let segments = extract_markdown_segments(input);
    assert_eq!(segments.len(), 1);
    assert_eq!(
        segments[0],
        MarkdownSegment::CodeBlock {
            language: "typescript".into(),
            header_path: Some("app/routes/index.tsx".into()),
            code: "// app/routes/index.tsx\nexport default function Home() {}\n".into(),
        }
    );
}

#[test]
fn extract_markdown_segments_rejects_version_and_shebang_as_paths() {
    for input in [
        "```sh\n#!/bin/sh\necho hi\n```",
        "```rust\n// v1.2\nfn main() {}\n```",
    ] {
        let segments = extract_markdown_segments(input);
        assert!(matches!(
            segments.as_slice(),
            [MarkdownSegment::CodeBlock {
                header_path: None,
                ..
            }]
        ));
    }
}

#[test]
fn extract_markdown_segments_accepts_indented_closing_fence() {
    let segments = extract_markdown_segments("```rust\nlet x = 1;\n  ```\nAfter");
    assert!(
        matches!(segments.as_slice(), [MarkdownSegment::CodeBlock { .. }, MarkdownSegment::Markdown(text)] if text == "After")
    );
}

#[test]
fn extract_markdown_segments_keeps_text_before_mid_line_backticks() {
    let segments = extract_markdown_segments("Prefix ```inline\n```rust\ncode\n```");
    assert!(
        matches!(segments.as_slice(), [MarkdownSegment::Markdown(text), MarkdownSegment::CodeBlock { language, .. }] if text == "Prefix ```inline\n" && language == "rust")
    );
}

#[test]
fn normalize_terminal_command_removes_prompt_markers() {
    assert_eq!(
        normalize_terminal_command("$ echo one\n>>> echo two\n> echo three"),
        "echo one\necho two\necho three"
    );
}

#[test]
fn terminal_runnable_language_identifies_shell_flavors() {
    assert!(is_terminal_runnable_language("bash"));
    assert!(is_terminal_runnable_language("sh"));
    assert!(is_terminal_runnable_language("zsh"));
    assert!(is_terminal_runnable_language("shell"));
    assert!(!is_terminal_runnable_language("rust"));
    assert!(!is_terminal_runnable_language("python"));
    assert!(!is_terminal_runnable_language("json"));
}

#[test]
fn active_slash_command_query_identifies_autocomplete_prefixes() {
    assert_eq!(active_slash_command_query("/"), Some(""));
    assert_eq!(active_slash_command_query("/com"), Some("com"));
    assert_eq!(active_slash_command_query("  /help"), Some("help"));
    assert_eq!(active_slash_command_query("/commit message"), None);
    assert_eq!(active_slash_command_query("/commit "), None);
    assert_eq!(active_slash_command_query("hello /help"), None);
    assert_eq!(active_slash_command_query("plain text"), None);
}

#[test]
fn slash_command_binding_context_matches_nested_input() {
    use gpui::{KeyBindingContextPredicate, KeyContext};

    let predicate = KeyBindingContextPredicate::parse(SLASH_COMMAND_BINDING_CONTEXT).unwrap();
    let menu_ctx = KeyContext::try_from(SLASH_COMMAND_KEY_CONTEXT).unwrap();
    let input_ctx = KeyContext::try_from(INPUT_KEY_CONTEXT).unwrap();

    let active_contexts = vec![menu_ctx, input_ctx.clone()];
    assert_eq!(predicate.depth_of(&active_contexts), Some(2));
    assert!(predicate.eval(&active_contexts));

    let normal_contexts = vec![input_ctx];
    assert_eq!(predicate.depth_of(&normal_contexts), None);
    assert!(!predicate.eval(&normal_contexts));
}

#[gpui::test]
fn reading_history_survives_new_activity_and_jump_resumes_following(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state.messages = (0..80)
            .map(|index| ChatMessageInfo {
                id: format!("message-{index}"),
                role: threadlane_ui_state::MessageRole::User,
                content: format!(
                    "Message {index}: {}",
                    "Long conversation content. ".repeat(10)
                ),
                tool_activities: Vec::new(),
                streaming: false,
                reasoning_content: None,
                reasoning_expanded: false,
            })
            .collect::<Vec<_>>()
            .into();
        state
    });
    let retained_model = model.clone();
    let holder: std::rc::Rc<
        std::cell::RefCell<Option<gpui::Entity<super::ChatListView>>>,
    > = Default::default();
    let holder_clone = holder.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        holder_clone.borrow_mut().replace(chat.clone());
        gpui_component::Root::new(chat, window, cx)
    });
    let chat = holder
        .borrow()
        .as_ref()
        .expect("chat view mounted")
        .clone();
    cx.run_until_parked();
    chat.update(cx, |chat, cx| {
        chat.initial_scroll_frames = 0;
        chat.transcript_list_state.scroll_to(gpui::ListOffset {
            item_ix: 10,
            offset_in_item: gpui::px(0.),
        });
        cx.notify();
    });
    cx.run_until_parked();
    let before = chat.read_with(cx, |chat, _| {
        chat.transcript_list_state.logical_scroll_top()
    });
    retained_model.update(cx, |state, cx| {
        let mut next = state.messages.last().unwrap().clone();
        next.id = "new-activity".into();
        std::sync::Arc::make_mut(&mut state.messages).push(next);
        cx.notify();
    });
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| {
        assert!(!chat.transcript_list_state.is_following_tail());
        let after = chat.transcript_list_state.logical_scroll_top();
        assert_eq!(after.item_ix, before.item_ix);
        assert_eq!(after.offset_in_item, before.offset_in_item);
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let jump = cx
        .debug_bounds("jump-to-latest")
        .expect("reader can return to latest");
    cx.simulate_click(jump.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| {
        assert!(chat.transcript_list_state.is_following_tail())
    });
}

#[gpui::test]
fn plan_disclosure_opens_on_click_and_does_not_leak_to_another_task(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    use threadlane_protocol::messages::{PlanItem, PlanItemStatus, SessionPlan};
    struct PlanColumn(gpui::Entity<super::ChatListView>);
    impl gpui::Render for PlanColumn {
        fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
            use gpui::{ParentElement as _, Styled as _};
            gpui::div().w(gpui::px(448.)).h_full().child(self.0.clone())
        }
    }
    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.active_session_id = Some("planned-task".into());
        state.active_plan = SessionPlan {
            explanation: None,
            items: vec![PlanItem {
                step: "Verify the layout with a long task description. ".repeat(30),
                status: PlanItemStatus::InProgress,
            }],
        };
        state
    });
    let retained_model = model.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        gpui_component::Root::new(cx.new(|_| PlanColumn(chat)), window, cx)
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let tracker = cx.debug_bounds("session-plan-tracker").unwrap();
    assert!(tracker.right() <= gpui::px(448.), "long plan stays inside the chat pane: {tracker:?}");
    cx.simulate_click(tracker.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("session-plan-details").is_some());
    retained_model.update(cx, |state, cx| {
        state.active_session_id = Some("other-task".into());
        state.active_plan = SessionPlan::default();
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("session-plan-details").is_none());
}

#[gpui::test]
fn permission_details_are_bound_to_the_request_that_opened_them(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    use gpui_component::WindowExt as _;
    struct DialogHost(gpui::Entity<super::ChatListView>);
    impl gpui::Render for DialogHost {
        fn render(&mut self, _window: &mut gpui::Window, _cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
            use gpui::{ParentElement as _, Styled as _};
            gpui::div().size_full().child(self.0.clone())
        }
    }
    cx.update(gpui_component::init);
    let request = |id: &str| threadlane_protocol::PermissionRequest {
        id: id.into(), capability: "network".into(), title: "Connect to test host".into(),
        detail: "https://example.test".into(), scopes: vec![threadlane_protocol::PermissionScope::Once],
    };
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.active_session_id = Some("permission-task".into());
        state.pending_permissions.insert("permission-task".into(), request("first"));
        state
    });
    let retained_model = model.clone();
    let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
    let capture = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        *capture.borrow_mut() = Some(chat.clone());
        gpui_component::Root::new(cx.new(|_| DialogHost(chat)), window, cx)
    });
    let chat = captured.borrow_mut().take().unwrap();
    cx.run_until_parked();
    cx.update(|window, cx| chat.update(cx, |chat, cx| chat.open_permission_details("first", window, cx)));
    cx.run_until_parked();
    cx.update(|window, cx| { window.draw(cx).clear(cx); assert!(window.has_active_dialog(cx)); });
    assert!(cx.debug_bounds("permission-details-always").is_none());
    assert!(cx.debug_bounds("permission-inline-always").is_none());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|window, cx| assert!(!window.has_active_dialog(cx), "Escape closes details"));
    retained_model.read_with(cx, |state, _| assert_eq!(state.pending_permissions["permission-task"].id, "first"));
    cx.update(|window, cx| chat.update(cx, |chat, cx| chat.open_permission_details("first", window, cx)));
    cx.run_until_parked();
    retained_model.update(cx, |state, _| {
        state.pending_permissions.insert("permission-task".into(), request("replacement"));
    });
    // Even before a render synchronizes the modal, stale details cannot expose
    // the replacement request's actions.
    chat.update(cx, |chat, cx| assert!(chat.render_permission_details_dialog(cx).is_none()));
    retained_model.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| assert!(chat.permission_details_request.is_none()));
    cx.update(|window, cx| assert!(!window.has_active_dialog(cx)));
    retained_model.read_with(cx, |state, _| {
        assert_eq!(state.pending_permissions["permission-task"].id, "replacement");
    });
}

#[gpui::test]
fn completed_activity_disclosure_renders_interactive_tool_rows(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        let tool_message = ChatMessageInfo {
            id: "tool-result".into(),
            role: MessageRole::Assistant,
            content: String::new(),
            tool_activities: vec![ToolActivityInfo {
                id: "read-file".into(),
                category: "Completed".into(),
                title: "read_file".into(),
                display_summary: "Read source file".into(),
                detail: "Large source file line\n".repeat(250),
                is_expanded: false,
            }],
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
        };
        let mut messages = (0..60).map(|index| ChatMessageInfo {
            id: format!("history-{index}"), role: MessageRole::User,
            content: format!("Historical message {index}. {}", "Keep this reading position. ".repeat(5)),
            tool_activities: Vec::new(), streaming: false, reasoning_content: None, reasoning_expanded: false,
        }).collect::<Vec<_>>();
        messages.insert(20, tool_message);
        state.messages = messages.into();
        state
    });
    let holder: std::rc::Rc<
        std::cell::RefCell<Option<gpui::Entity<super::ChatListView>>>,
    > = Default::default();
    let holder_clone = holder.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        holder_clone.borrow_mut().replace(chat.clone());
        gpui_component::Root::new(chat, window, cx)
    });
    let chat = holder
        .borrow()
        .as_ref()
        .expect("chat view mounted")
        .clone();
    cx.run_until_parked();
    chat.update(cx, |chat, cx| {
        chat.initial_scroll_frames = 0;
        chat.transcript_list_state.scroll_to(gpui::ListOffset { item_ix: 20, offset_in_item: gpui::px(0.) });
        cx.notify();
    });
    cx.run_until_parked();
    let before = chat.read_with(cx, |chat, _| chat.transcript_list_state.logical_scroll_top());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let disclosure = cx.debug_bounds("activity-group-disclosure").expect("completed group has a disclosure");
    cx.simulate_click(disclosure.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    chat.read_with(cx, |chat, _| {
        assert_eq!(chat.expanded_activity_groups.len(), 1);
        let after = chat.transcript_list_state.logical_scroll_top();
        assert_eq!((after.item_ix, after.offset_in_item), (before.item_ix, before.offset_in_item));
    });
    let tool = cx.debug_bounds("tool-activity-disclosure").expect("expanded group exposes tool");
    cx.simulate_click(tool.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    chat.read_with(cx, |chat, cx| {
        assert!(chat.model.read(cx).messages.iter().flat_map(|message| &message.tool_activities).any(|activity| activity.id == "read-file" && activity.is_expanded));
        assert!(!chat.transcript_list_state.is_following_tail());
        let after = chat.transcript_list_state.logical_scroll_top();
        assert_eq!((after.item_ix, after.offset_in_item), (before.item_ix, before.offset_in_item));
    });

}

#[gpui::test]
fn reasoning_disclosure_supports_keyboard_and_pauses_following(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state.messages = vec![ChatMessageInfo {
            id: "reasoning-test".into(), role: MessageRole::Assistant,
            content: "Long response paragraph.\n\n".repeat(100), tool_activities: Vec::new(), streaming: false,
            reasoning_content: Some("Reasoning detail.\n".repeat(100)), reasoning_expanded: false,
        }].into();
        state
    });
    struct Harness(gpui::Entity<super::ChatListView>);
    impl gpui::Render for Harness {
        fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
            use gpui::{InteractiveElement as _, ParentElement as _, Styled as _};
            gpui::div().id("workspace").tab_group().size_full().child(self.0.clone())
        }
    }
    let holder: std::rc::Rc<
        std::cell::RefCell<Option<gpui::Entity<super::ChatListView>>>,
    > = Default::default();
    let holder_clone = holder.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        holder_clone.borrow_mut().replace(chat.clone());
        let host = cx.new(|_| Harness(chat));
        gpui_component::Root::new(host, window, cx)
    });
    let chat = holder
        .borrow()
        .as_ref()
        .expect("chat view mounted")
        .clone();
    cx.run_until_parked();
    chat.update(cx, |chat, cx| {
        chat.initial_scroll_frames = 0;
        chat.transcript_list_state.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: gpui::px(0.) });
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let reasoning = cx.debug_bounds("reasoning-disclosure").expect("reasoning has a native disclosure");
    cx.simulate_click(reasoning.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    chat.read_with(cx, |chat, cx| {
        assert!(chat.model.read(cx).messages[0].reasoning_expanded);
        assert!(!chat.transcript_list_state.is_following_tail());
    });
    cx.update(|window, cx| {
        window.blur(cx);
        window.focus_next(cx); // Chat
        window.focus_next(cx); // Trajectory
        window.focus_next(cx); // Editor
        window.focus_next(cx); // Reasoning
        window.draw(cx).clear(cx);
    });
    let keystroke = gpui::Keystroke::parse("space").unwrap();
    cx.simulate_event(gpui::KeyDownEvent { keystroke: keystroke.clone(), is_held: false, prefer_character_input: false });
    cx.simulate_event(gpui::KeyUpEvent { keystroke });
    cx.run_until_parked();
    chat.read_with(cx, |chat, cx| {
        assert!(!chat.model.read(cx).messages[0].reasoning_expanded, "focused disclosure supports Space");
    });
}

#[gpui::test]
fn environment_tracks_checkout_and_yields_space_to_chat(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.projects.clear();
        threadlane_ui_state::activate_test_session(&mut state, "first", std::path::Path::new("/projects/one/first.jsonl"));
        state.is_new_task = false;
        state
    });
    let retained_model = model.clone();
    let (chat, cx) = cx.add_window_view(move |window, cx| super::ChatListView::new(model, window, cx));
    chat.update(cx, |chat, cx| chat.set_environment_width(gpui::px(1200.), gpui::px(16.), cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("chat-environment").is_some());
    let terminal = cx.debug_bounds("environment-terminal").unwrap();
    cx.simulate_click(terminal.center(), gpui::Modifiers::default());
    retained_model.read_with(cx, |state, _| {
        assert_eq!(state.requested_terminal_work_dir, state.active_git_work_dir());
        assert!(state.requested_terminal_work_dir.is_some());
    });
    retained_model.update(cx, |state, cx| {
        threadlane_ui_state::activate_test_session(state, "second", std::path::Path::new("/projects/two/second.jsonl"));
        state.requested_terminal_work_dir = None;
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let terminal = cx.debug_bounds("environment-terminal").unwrap();
    cx.simulate_click(terminal.center(), gpui::Modifiers::default());
    retained_model.read_with(cx, |state, _| {
        assert_eq!(state.requested_terminal_work_dir, Some("/projects/two".into()));
    });
    chat.update(cx, |chat, cx| chat.set_environment_width(gpui::px(900.), gpui::px(16.), cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("chat-environment").is_none());
}

#[gpui::test]
fn user_message_edit_loads_composer_for_resend(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    use gpui::Focusable as _;
    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state.messages = vec![ChatMessageInfo {
            id: "user-1".into(),
            role: MessageRole::User,
            content: "User request".into(),
            tool_activities: Vec::new(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
        }]
        .into();
        state
    });
    let holder: std::rc::Rc<
        std::cell::RefCell<Option<gpui::Entity<super::ChatListView>>>,
    > = Default::default();
    let holder_clone = holder.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        holder_clone.borrow_mut().replace(chat.clone());
        gpui_component::Root::new(chat, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let chat = holder
        .borrow()
        .as_ref()
        .expect("chat view mounted")
        .clone();
    let edit = cx
        .debug_bounds("message-edit")
        .expect("settled user message exposes an edit action");
    cx.simulate_click(edit.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let composer_focus = chat.read_with(cx, |chat, cx| {
        chat.input_state
            .read(cx)
            .focus_handle(cx)
    });
    cx.update(|window, cx| {
        assert_eq!(
            window.focused(cx),
            Some(composer_focus),
            "edit hands focus to the composer"
        );
    });
    chat.update(cx, |chat, cx| {
        assert_eq!(
            chat.input_state.read(cx).value().as_ref(),
            "User request",
            "edit loads the message text into the composer"
        );
        assert_eq!(chat.current_tab, super::CentralTab::Chat);
    });

    // Editing history must never silently replace an unsent draft, including
    // image-only drafts whose text field is empty.
    for (draft, with_image) in [("Unsent follow-up", false), ("", true)] {
        cx.update(|window, cx| {
            chat.update(cx, |chat, cx| {
                chat.input_state.update(cx, |input, cx| {
                    input.set_value(draft, window, cx);
                });
                chat.pasted_images = vec![super::ImageAttachment {
                    display_name: "draft.png".into(),
                    data_url: "data:image/png;base64,test".into(),
                }];
                if !with_image {
                    chat.pasted_images.clear();
                }
                cx.notify();
            });
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let edit = cx
            .debug_bounds("message-edit")
            .expect("edit action remains available");
        cx.simulate_click(edit.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        chat.read_with(cx, |chat, cx| {
            assert_eq!(chat.input_state.read(cx).value().as_ref(), draft);
            assert_eq!(chat.pasted_images.len(), usize::from(with_image));
            if with_image {
                assert_eq!(chat.pasted_images[0].display_name, "draft.png");
            }
        });
    }
}

#[gpui::test]
fn user_message_edit_hidden_while_generating(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state.messages = vec![ChatMessageInfo {
            id: "user-1".into(),
            role: MessageRole::User,
            content: "User request".into(),
            tool_activities: Vec::new(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
        }]
        .into();
        state.is_generating = true;
        state
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        gpui_component::Root::new(chat, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("message-edit").is_none(),
        "edit stays hidden while a generation is running"
    );
    assert!(
        cx.debug_bounds("message-copy").is_some(),
        "copy remains available while generating"
    );
}

#[gpui::test]
fn user_message_exposes_copy_action(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state.messages = vec![ChatMessageInfo {
            id: "user-1".into(),
            role: MessageRole::User,
            content: "User request".into(),
            tool_activities: Vec::new(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
        }]
        .into();
        state
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        gpui_component::Root::new(chat, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let copy = cx
        .debug_bounds("message-copy")
        .expect("user message exposes a copy action");
    cx.simulate_click(copy.center(), gpui::Modifiers::default());
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("User request".to_string())
    );
}

#[gpui::test]
fn completed_assistant_message_exposes_copy_action(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state.messages = vec![ChatMessageInfo {
            id: "assistant-1".into(),
            role: MessageRole::Assistant,
            content: "Completed response".into(),
            tool_activities: Vec::new(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
        }]
        .into();
        state
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        gpui_component::Root::new(chat, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let copy = cx
        .debug_bounds("message-copy")
        .expect("completed response exposes a copy action");
    cx.simulate_click(copy.center(), gpui::Modifiers::default());
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("Completed response".to_string())
    );
}

#[gpui::test]
fn message_copy_shows_copied_feedback(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state.messages = vec![ChatMessageInfo {
            id: "assistant-1".into(),
            role: MessageRole::Assistant,
            content: "Completed response".into(),
            tool_activities: Vec::new(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
        }]
        .into();
        state
    });
    let holder: std::rc::Rc<
        std::cell::RefCell<Option<gpui::Entity<super::ChatListView>>>,
    > = Default::default();
    let holder_clone = holder.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        holder_clone.borrow_mut().replace(chat.clone());
        gpui_component::Root::new(chat, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let chat = holder
        .borrow()
        .as_ref()
        .expect("chat view mounted")
        .clone();
    chat.read_with(cx, |chat, _| {
        assert!(chat.copied_message.is_none());
    });
    let copy = cx
        .debug_bounds("message-copy")
        .expect("copy action present");
    cx.simulate_click(copy.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| {
        let (key, _) = chat
            .copied_message
            .as_ref()
            .expect("copy records per-message feedback");
        assert_eq!(key, "message-copy-assistant-1");
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("message-copy").is_some());
}

#[gpui::test]
fn streaming_assistant_message_hides_copy_action(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state.messages = vec![ChatMessageInfo {
            id: "assistant-streaming".into(),
            role: MessageRole::Assistant,
            content: "Partial response".into(),
            tool_activities: Vec::new(),
            streaming: true,
            reasoning_content: None,
            reasoning_expanded: false,
        }]
        .into();
        state
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        gpui_component::Root::new(chat, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("message-copy").is_none(),
        "streaming response defers its actions until generation completes"
    );
}

#[test]
fn run_elapsed_formats_seconds_minutes_and_hours() {
    assert_eq!(super::format_run_elapsed(7), "7s");
    assert_eq!(super::format_run_elapsed(59), "59s");
    assert_eq!(super::format_run_elapsed(65), "1m 05s");
    assert_eq!(super::format_run_elapsed(600), "10m 00s");
    assert_eq!(super::format_run_elapsed(3725), "1h 02m");
}

#[test]
fn sendable_prompt_accepts_text_or_images() {
    assert!(!super::has_sendable_prompt("", 0));
    assert!(!super::has_sendable_prompt("   ", 0));
    assert!(super::has_sendable_prompt("hello", 0));
    assert!(super::has_sendable_prompt("", 1));
    assert!(super::has_sendable_prompt("   ", 2));
}

#[test]
fn plan_step_truncates_long_labels_with_ellipsis() {
    assert_eq!(super::truncate_plan_step("Short step", 42), "Short step");
    assert_eq!(
        super::truncate_plan_step(&"x".repeat(42), 42),
        "x".repeat(42)
    );
    let long = "Implement the multi-turn durable queue handoff with tests";
    let truncated = super::truncate_plan_step(long, 20);
    assert!(truncated.ends_with('…'));
    assert!(truncated.chars().count() <= 20);
    assert!(long.starts_with(truncated.trim_end_matches('…')));
}

#[test]
fn preview_truncation_matches_stash_banner_bounds() {
    let short = "Fix the typo";
    assert_eq!(super::truncate_preview_text(short, 60), short);
    assert_eq!(
        super::truncate_preview_text(&"y".repeat(60), 60),
        "y".repeat(60)
    );
    let long = "z".repeat(61);
    let truncated = super::truncate_preview_text(&long, 60);
    assert!(truncated.ends_with('…'));
    assert_eq!(truncated.chars().count(), 60);
    assert_eq!(super::truncate_preview_text("  padded  ", 60), "padded");
}

#[test]
fn completed_activity_labels_use_singular_for_one() {
    assert_eq!(super::completed_activities_text(1), "1 completed activity");
    assert_eq!(super::completed_activities_text(2), "2 completed activities");
    assert_eq!(
        super::expand_activities_a11y(1),
        "Expand 1 completed tool activity"
    );
    assert_eq!(
        super::expand_activities_a11y(3),
        "Expand 3 completed tool activities"
    );
}

#[test]
fn plan_tracker_labels_avoid_repeating_plan_when_complete() {
    let (display, a11y, tooltip) = super::plan_tracker_texts(1, 3, Some("Run tests"));
    assert_eq!(display, "Run tests");
    assert_eq!(a11y, "Task plan, 1 of 3 complete, current step: Run tests");
    assert_eq!(tooltip, "Show task plan · Run tests");
    let (display, a11y, tooltip) = super::plan_tracker_texts(3, 3, None);
    assert_eq!(display, "Complete");
    assert_eq!(a11y, "Task plan, 3 of 3 complete");
    assert_eq!(tooltip, "Show task plan · all steps complete");
}

#[test]
fn reasoning_token_badge_uses_singular_for_one_token() {
    assert_eq!(super::reasoning_token_badge(true, 100), "thinking…");
    assert_eq!(super::reasoning_token_badge(false, 1), "~1 token");
    assert_eq!(super::reasoning_token_badge(false, 0), "~0 tokens");
    assert_eq!(super::reasoning_token_badge(false, 100), "~25 tokens");
}

#[test]
fn tool_activity_glyph_marks_unknown_categories_neutral() {
    assert_eq!(super::tool_activity_glyph("Error"), "!");
    assert_eq!(super::tool_activity_glyph("Working"), "◌");
    assert_eq!(super::tool_activity_glyph("Thinking"), "◌");
    assert_eq!(super::tool_activity_glyph("Completed"), "✓");
    assert_eq!(super::tool_activity_glyph("Edited"), "✓");
    assert_eq!(super::tool_activity_glyph("tool"), "•");
    assert_eq!(super::tool_activity_glyph(""), "•");
}

#[test]
fn progress_header_prefix_names_errors_explicitly() {
    assert_eq!(super::progress_header_prefix(false), "Latest activity:");
    assert_eq!(super::progress_header_prefix(true), "Needs attention:");
}

#[test]
fn skills_chip_label_uses_singular_for_one_skill() {
    assert_eq!(super::skills_chip_label(0), "Skills");
    assert_eq!(super::skills_chip_label(1), "1 Skill");
    assert_eq!(super::skills_chip_label(4), "4 Skills");
}

#[test]
fn stats_nouns_use_singular_for_one() {
    assert_eq!(super::plural_noun(0, "turn", "turns"), "turns");
    assert_eq!(super::plural_noun(1, "turn", "turns"), "turn");
    assert_eq!(super::plural_noun(2, "tool call", "tool calls"), "tool calls");
    assert_eq!(super::plural_noun(1, "anomaly", "anomalies"), "anomaly");
}

#[test]
fn project_menus_mark_the_current_project() {
    use std::path::{Path, PathBuf};

    let active = PathBuf::from("/work/mypi");
    assert!(super::is_current_project(Some(&active), Path::new("/work/mypi")));
    assert!(!super::is_current_project(
        Some(&active),
        Path::new("/work/other")
    ));
    assert!(!super::is_current_project(None, Path::new("/work/mypi")));
}

#[gpui::test]
fn header_issue_link_appears_only_with_a_linked_issue(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let session_file = std::path::Path::new("/test-project/.threadlane/sessions/session-1.jsonl");
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        threadlane_ui_state::activate_test_session(&mut state, "session-1", session_file);
        state
    });
    let retained_model = model.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        gpui_component::Root::new(chat, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("chat-header-issue-link").is_none(),
        "no issue link without a linked issue"
    );
    retained_model.update(cx, |state, cx| {
        for project in &mut state.projects {
            for session in &mut project.sessions {
                session.github_issue = Some(threadlane_git::GitHubIssueRef {
                    host: "github.com".into(),
                    owner: "octo".into(),
                    repo: "demo".into(),
                    number: 42,
                    url: "https://github.com/octo/demo/issues/42".into(),
                });
            }
        }
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("chat-header-issue-link").is_some(),
        "issue link appears once the session links an issue"
    );
}

#[gpui::test]
fn new_task_hero_offers_project_picker(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = true;
        state
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        gpui_component::Root::new(chat, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("new-task-project-picker").is_some(),
        "new-task hero names its project picker"
    );
}

#[gpui::test]
fn question_card_toggles_option_selection(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let session_file = std::path::Path::new("/test-project/.threadlane/sessions/session-1.jsonl");
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        threadlane_ui_state::activate_test_session(&mut state, "session-1", session_file);
        state.pending_questions.insert(
            "session-1".into(),
            threadlane_protocol::QuestionRequest {
                id: "q-req-1".into(),
                questions: vec![threadlane_protocol::QuestionItem {
                    id: "q1".into(),
                    header: "Scope".into(),
                    question: "Which scope should apply?".into(),
                    options: vec!["Yes".into(), "No".into()],
                    allow_custom: false,
                }],
            },
        );
        state
    });
    // No Root layer: option toggles only notify, and submit/dismiss need a
    // live session runtime to resolve, so the card's UI-owned toggle state is
    // what's pinned here.
    let (chat, cx) =
        cx.add_window_view(move |window, cx| super::ChatListView::new(model, window, cx));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    for selector in [
        "question-card",
        "question-option-q1-0",
        "question-option-q1-1",
        "question-submit",
        "question-dismiss",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} is visible while a question is pending"
        );
    }
    let selection_key = "q-req-1\0q1".to_string();
    assert!(
        chat.read_with(cx, |chat, _| {
            chat.question_selections
                .get(&selection_key)
                .cloned()
                .unwrap_or_default()
        })
        .is_empty()
    );
    let option = cx.debug_bounds("question-option-q1-0").unwrap();
    cx.simulate_click(option.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        chat.read_with(cx, |chat, _| {
            chat.question_selections
                .get(&selection_key)
                .cloned()
                .unwrap_or_default()
        }),
        vec!["Yes".to_string()]
    );
    // Toggling again removes the answer.
    let option = cx.debug_bounds("question-option-q1-0").unwrap();
    cx.simulate_click(option.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        chat.read_with(cx, |chat, _| {
            chat.question_selections
                .get(&selection_key)
                .cloned()
                .unwrap_or_default()
        })
        .is_empty()
    );
}

#[gpui::test]
fn pending_preview_offers_queue_steer_and_edit_while_generating(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state.is_generating = true;
        threadlane_ui_state::activate_test_session(
            &mut state,
            "session-1",
            std::path::Path::new("/test-project/.threadlane/sessions/session-1.jsonl"),
        );
        threadlane_ui_state::controller::dispatch(
            &mut state,
            threadlane_ui_state::actions::AppAction::StageBusyMessage {
                text: "Follow up after this turn".into(),
                images: Vec::new(),
            },
        );
        state
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        gpui_component::Root::new(chat, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    for selector in [
        "pending-preview-row",
        "pending-queue",
        "pending-steer",
        "pending-dismiss",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} is visible while a message is staged for the next turn"
        );
    }
}

#[gpui::test]
fn workspace_changes_review_entry_tracks_uncommitted_files(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let session_file = std::path::Path::new("/test-project/.threadlane/sessions/session-1.jsonl");
    let work_dir = session_file.parent().unwrap().to_path_buf();
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        threadlane_ui_state::activate_test_session(&mut state, "session-1", session_file);
        state.git_statuses.insert(
            work_dir.clone(),
            threadlane_git::GitStatus {
                files: vec![
                    threadlane_git::GitFile::default(),
                    threadlane_git::GitFile::default(),
                ],
                ..Default::default()
            },
        );
        state
    });
    let retained_model = model.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        gpui_component::Root::new(chat, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("workspace-changes-review").is_some(),
        "Review entry appears with uncommitted files"
    );
    retained_model.update(cx, |state, cx| {
        if let Some(status) = state.git_statuses.get_mut(&work_dir) {
            status.files.clear();
        }
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("workspace-changes-review").is_none(),
        "Review entry hides with a clean tree"
    );
}

#[gpui::test]
fn provider_setup_banner_hides_when_models_exist(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| super::ChatListView::new(model, window, cx));
        gpui_component::Root::new(chat, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    // The static registry seeds always provide models in-process, so the
    // setup banner must stay hidden; its visible state needs an empty
    // catalog, which has no test seam yet.
    assert!(
        cx.debug_bounds("provider-setup-banner").is_none(),
        "setup banner hides when models are available"
    );
    assert!(
        cx.debug_bounds("composer-model-picker").is_some(),
        "model picker stays visible instead"
    );
}

#[gpui::test]
fn trajectory_inspector_tabs_switch_content(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state
    });
    let (chat, cx) =
        cx.add_window_view(move |window, cx| super::ChatListView::new(model, window, cx));
    chat.update(cx, |chat, cx| {
        chat.trajectory_cache = Some(TrajectoryRenderCache {
            key: TrajectoryCacheKey {
                revision: 0,
                epoch: 0,
                mode: TrajectoryMode::Execution,
                query: String::new(),
                category: None,
                lane: None,
            },
            all_entries: vec![trajectory_entry("Tool", Some(1), Some(1))],
            categories: Arc::new(vec!["Tool".to_string()]),
            lanes: Arc::new(Vec::new()),
            lane_latest: Arc::new(BTreeMap::new()),
            filtered_indices: vec![0],
            previews: vec!["Tool".into()],
            rows: vec![TrajectoryRow::Entry(0)],
            summary: TrajectorySummary::default(),
        });
        chat.selected_trajectory_index = Some(0);
        chat.current_tab = super::CentralTab::Trajectory;
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    for selector in [
        "trajectory-inspector-Overview",
        "trajectory-inspector-Preview",
        "trajectory-inspector-Raw",
        "trajectory-inspector-Source",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} inspector tab is reachable"
        );
    }
    chat.read_with(cx, |chat, _| {
        assert_eq!(
            chat.trajectory_inspector_tab,
            TrajectoryInspectorTab::Overview
        );
    });
    let raw = cx.debug_bounds("trajectory-inspector-Raw").unwrap();
    cx.simulate_click(raw.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| {
        assert_eq!(chat.trajectory_inspector_tab, TrajectoryInspectorTab::Raw);
    });
}

#[gpui::test]
fn environment_section_renders_without_git_data(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state.active_work_dir = Some(std::path::PathBuf::from("/test-project"));
        state
    });
    let (chat, cx) =
        cx.add_window_view(move |window, cx| super::ChatListView::new(model, window, cx));
    chat.update(cx, |chat, cx| {
        chat.environment_available = true;
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    for selector in [
        "chat-environment",
        "environment-changes",
        "environment-files",
        "environment-terminal",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} renders with no session or git status"
        );
    }
}

#[gpui::test]
fn trajectory_toolbar_filters_are_reachable(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = threadlane_ui_state::AppState::default();
        state.is_new_task = false;
        state
    });
    let (chat, cx) =
        cx.add_window_view(move |window, cx| super::ChatListView::new(model, window, cx));
    chat.update(cx, |chat, cx| {
        chat.trajectory_cache = Some(TrajectoryRenderCache {
            key: TrajectoryCacheKey {
                revision: 0,
                epoch: 0,
                mode: TrajectoryMode::Execution,
                query: String::new(),
                category: None,
                lane: None,
            },
            all_entries: vec![trajectory_entry("Tool", Some(1), Some(1))],
            categories: Arc::new(vec!["Tool".to_string()]),
            lanes: Arc::new(vec!["main".to_string(), "worker".to_string()]),
            lane_latest: Arc::new(BTreeMap::from([
                ("main".to_string(), "Read file".to_string()),
                ("worker".to_string(), "Write file".to_string()),
            ])),
            filtered_indices: vec![0],
            previews: vec!["Tool".into()],
            rows: vec![TrajectoryRow::Entry(0)],
            summary: TrajectorySummary::default(),
        });
        chat.current_tab = super::CentralTab::Trajectory;
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    for selector in [
        "trajectory-mode-filter",
        "trajectory-category-filter",
        "trajectory-lane-filter",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} is reachable in the trajectory toolbar"
        );
    }
}
