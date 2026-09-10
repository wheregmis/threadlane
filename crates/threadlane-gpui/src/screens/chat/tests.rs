#[gpui::test]
fn unsupported_acp_steer_keeps_the_composer_text_and_images(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let model = cx.new(|_| {
        let mut state = crate::state::AppState::default();
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
        let mut state = crate::state::AppState::default();
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
    MarkdownSegment, MarkdownUpdate, TrajectoryCacheKey, TrajectoryMode, TrajectoryRow,
    TranscriptRow, INPUT_KEY_CONTEXT, MARKDOWN_CACHE_ENTRY_LIMIT, SLASH_COMMAND_BINDING_CONTEXT,
    SLASH_COMMAND_KEY_CONTEXT,
};

#[gpui::test]
fn chat_errors_are_bounded_deduplicated_and_keep_recovery_details(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    struct ErrorHarness {
        model: gpui::Entity<crate::state::AppState>,
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

    let mut message = crate::state::ChatMessageInfo {
        id: "error".into(),
        role: crate::state::MessageRole::Error,
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
    message.role = crate::state::MessageRole::Assistant;
    assert_eq!(
        super::visible_session_status(Some(&error), Some(&message)),
        Some(error.as_str())
    );

    cx.update(gpui_component::init);
    let model = cx.new(|_| crate::state::AppState::default());
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
        crate::state::WorkspacePage::Settings
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
use crate::state::{
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
    assert_eq!(estimating.current_label, "Estimating…");
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
    assert_eq!(view.current_label, "Estimating…");
    assert_eq!(view.bar_percent, 0.0);
    assert_eq!(view.detail_label, "Context usage details, estimating usage");
}

#[test]
fn meter_treats_zero_context_limit_as_unknown_even_when_not_estimating() {
    let mut context = estimating_context();
    context.current_tokens = 42;
    context.estimating = false;

    let view = context_meter_view_model(Some(&context), &ContextMeterMetrics::default(), true);

    assert_eq!(view.percent, None);
    assert_eq!(view.current_label, "Estimating…");
    assert_eq!(view.bar_percent, 0.0);
    assert_eq!(view.detail_label, "Context usage details, estimating usage");
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
