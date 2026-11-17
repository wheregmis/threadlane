use super::{SessionPreview, Snapshot};
use gpui::{size, AppContext, Modifiers, TestAppContext, VisualTestContext};
use std::{cell::RefCell, rc::Rc, sync::Arc};
use threadlane_protocol::daemon::{ChatMessageInfo, MessageRole, RunTiming};

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    draw(cx);
}
fn message(id: &str, role: MessageRole, content: &str) -> ChatMessageInfo {
    ChatMessageInfo {
        id: id.into(),
        role,
        content: content.into(),
        tool_activities: Vec::new(),
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
        retry_prompt: None,
    }
}

#[gpui::test]
fn shared_review_excerpt_reveals_chat_draft_from_editor(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let snapshot: Snapshot =
        serde_json::from_value(serde_json::json!({"session":null,"messages":[]})).unwrap();
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| SessionPreview::with_snapshot(Arc::new(snapshot), window, cx));
        let destination = {
            let host = view.read(cx);
            (
                Some(host.session.work_dir.clone()),
                Some(host.session.id.clone()),
            )
        };
        view.update(cx, |host, cx| {
            host.editor_open = true;
            host.input
                .update(cx, |input, cx| input.set_value("Question:", window, cx));
        });
        let document = cx.new(|cx| threadlane_ui_kit::ReviewDiffDocument::new(window, cx));
        document.update(cx, |document, cx| {
            document.set_excerpt_target(
                Ok(threadlane_ui_kit::ReviewExcerptTarget::new(
                    destination.0.clone().unwrap(),
                    "src/a.rs".into(),
                    destination,
                    1,
                    false,
                )),
                cx,
            );
            document.set_patch("@@ -1 +1 @@\n-old\n+new", cx);
            document.request_focus(cx);
        });
        let agents = view.read(cx).agents.clone();
        cx.subscribe(
            &document,
            move |_, _, request: &threadlane_ui_kit::ReviewExcerptRequest, cx| {
                agents.update(cx, |_, cx| cx.emit(request.clone()));
            },
        )
        .detach();
        *capture.borrow_mut() = Some(view);
        gpui_component::Root::new(document, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    draw(cx);
    #[cfg(target_os = "macos")]
    cx.simulate_keystrokes("cmd-a");
    #[cfg(not(target_os = "macos"))]
    cx.simulate_keystrokes("ctrl-a");
    draw(cx);
    #[cfg(target_os = "macos")]
    cx.simulate_keystrokes("cmd-shift-enter");
    #[cfg(not(target_os = "macos"))]
    cx.simulate_keystrokes("ctrl-shift-enter");
    draw(cx);
    view.read_with(cx, |host, cx| {
        assert!(!host.editor_open, "successful insertion must reveal Chat");
        assert!(host
            .input
            .read(cx)
            .value()
            .starts_with("Question:\nReview diff excerpt: src/a.rs\nDisplayed patch snapshot"));
        assert!(host.input.read(cx).value().contains("-old\n+new"));
    });
}

#[gpui::test]
fn shared_code_blocks_keep_preview_actions_local(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    for (streaming, path, language, supported) in [
        (false, "scripts/check.sh", "shell", false),
        (false, "../outside.sh", "bash", false),
        (false, "scripts/check.sh", "bash", true),
        (true, "scripts/check.sh", "shell", true),
    ] {
        let mut snapshot: Snapshot =
            serde_json::from_value(serde_json::json!({"session":null,"messages":[]})).unwrap();
        let mut entry = message(
            "code",
            MessageRole::Assistant,
            &format!("\x60\x60\x60{language} {path}\n$ printf 'hello'\n\x60\x60\x60"),
        );
        entry.streaming = streaming;
        snapshot.messages = vec![entry];
        if supported {
            snapshot.runnable_code_languages = vec![language.into()];
        }
        let original = snapshot.messages.clone();
        let saved = Rc::new(RefCell::new(None));
        let capture = saved.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| SessionPreview::with_snapshot(Arc::new(snapshot), window, cx));
            *capture.borrow_mut() = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let view = saved.borrow_mut().take().unwrap();
        cx.update(|window, cx| {
            view.update(cx, |host, cx| {
                host.input
                    .update(cx, |input, cx| input.set_value("Keep my draft", window, cx));
            })
        });
        for font in [13., 20.] {
            cx.update(|_, cx| {
                gpui_component::Theme::global_mut(cx).font_size = gpui::px(font);
                gpui_component::Theme::sync_base(cx);
            });
            let rem = cx.update(|window, _| window.rem_size());
            for width in [30., 100.] {
                cx.simulate_resize(size(rem * width, rem * 60.));
                draw(cx);
                assert!(cx.debug_bounds("code-block-code-0").is_some());
                for (selector, expected) in [
                    ("copy-code-code-0", !streaming),
                    ("open-edit-code-0", !streaming && path == "scripts/check.sh"),
                    (
                        "run-term-code-0",
                        !streaming && (supported || language == "shell"),
                    ),
                ] {
                    let bounds = cx.debug_bounds(selector);
                    assert_eq!(bounds.is_some(), expected, "{selector}");
                    if let Some(bounds) = bounds {
                        assert!(
                            bounds.left() >= gpui::px(0.) && bounds.right() <= rem * width,
                            "{selector} must fit"
                        );
                    }
                }
            }
        }
        if !streaming {
            click(cx, "copy-code-code-0");
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some("$ printf 'hello'\n".into())
            );
            assert!(view.read_with(cx, |host, _| host.copied_message.is_some()));
            cx.dispatcher
                .scheduler()
                .clock()
                .advance(threadlane_ui_kit::MESSAGE_COPY_FEEDBACK_WINDOW);
            cx.executor().tick();
            draw(cx);
            assert!(view.read_with(cx, |host, _| host.copied_message.is_none()));
            // Let the copy toast leave before exercising another header target.
            cx.dispatcher
                .scheduler()
                .clock()
                .advance(std::time::Duration::from_secs(10));
            cx.executor().tick();
            draw(cx);
            if cx.debug_bounds("run-term-code-0").is_some() {
                click(cx, "run-term-code-0");
            }
        }
        view.read_with(cx, |host, cx| {
            assert_eq!(*host.messages, original);
            assert_eq!(host.input.read(cx).value(), "Keep my draft");
        });
    }
}
#[gpui::test]
fn shared_prompt_navigation_copy_edit_and_recall_preserve_captured_session(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    cx.update(threadlane_ui_kit::init_prompt_recall);
    cx.update(threadlane_ui_kit::init_conversation_find);
    let mut snapshot: Snapshot =
        serde_json::from_value(serde_json::json!({"session":null,"messages":[]})).unwrap();
    snapshot.messages = vec![message(
        "first",
        MessageRole::User,
        "First prompt\nKeep this line",
    )];
    for i in 0..65 {
        snapshot.messages.push(message(
            &format!("answer-{i}"),
            MessageRole::Assistant,
            &format!("Long conversation answer {i}"),
        ));
    }
    snapshot.messages.extend([
        message("second", MessageRole::User, "Second prompt"),
        message("blank", MessageRole::User, " "),
        message("queued-user-pending", MessageRole::User, "Queued echo"),
        message("third", MessageRole::User, "Third prompt"),
    ]);
    let original = snapshot.messages.clone();
    snapshot.run_timing = Some(RunTiming {
        start_seq: 1,
        source_seq: 2,
        started_at_ms: Some(1000),
        finished_at_ms: Some(4000),
        finished: true,
        suppressed: false,
    });
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| SessionPreview::with_snapshot(Arc::new(snapshot), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    draw(cx);
    assert!(cx.debug_bounds("last-run-duration").is_some());
    click(cx, "prompt-rail-first");
    view.read_with(cx, |host, _| {
        assert_eq!(host.outline_selected_id.as_deref(), Some("first"));
        assert!(!host.transcript.list.is_following_tail());
        assert_eq!(host.transcript.list.logical_scroll_top().item_ix, 0);
    });
    // Copy and edit are separate actions. Editing never overwrites a draft.
    click(cx, "message-copy-first");
    assert!(view.read_with(cx, |host, _| host.copied_message.is_some()));
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("First prompt\nKeep this line".into())
    );
    cx.dispatcher
        .scheduler()
        .clock()
        .advance(threadlane_ui_kit::MESSAGE_COPY_FEEDBACK_WINDOW);
    cx.executor().tick();
    draw(cx);
    assert!(view.read_with(cx, |host, _| host.copied_message.is_none()));
    cx.update(|window, cx| {
        view.update(cx, |host, cx| {
            host.input
                .update(cx, |input, cx| input.set_value("Keep my draft", window, cx));
        })
    });
    click(cx, "message-edit-first");
    assert_eq!(
        view.read_with(cx, |host, cx| host.input.read(cx).value()),
        "Keep my draft"
    );
    cx.update(|window, cx| {
        view.update(cx, |host, cx| {
            host.input
                .update(cx, |input, cx| input.set_value("", window, cx));
        })
    });
    click(cx, "message-edit-first");
    assert_eq!(
        view.read_with(cx, |host, cx| host.input.read(cx).value()),
        "First prompt\nKeep this line"
    );
    click(cx, "jump-to-latest");
    assert!(view.read_with(cx, |host, _| host.outline_selected_id.is_none()));
    // Keyboard browsing changes list focus before jumping, and preserves the composer.
    click(cx, "conversation-outline-open");
    assert!(view.read_with(cx, |host, _| host.outline_open));
    cx.simulate_keystrokes("home");
    draw(cx);
    assert!(view.read_with(cx, |host, _| host.transcript.list.is_following_tail()));
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |host, _| host.outline_selected_id.clone())
            .as_deref(),
        Some("first")
    );
    assert_eq!(
        view.read_with(cx, |host, cx| host.input.read(cx).value()),
        "First prompt\nKeep this line"
    );
    click(cx, "conversation-outline-open");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!view.read_with(cx, |host, _| host.outline_open));
    // Find supersedes the outline selection. Use its keyboard command while
    // the earlier draft-protection notification still occupies the header.
    cx.simulate_keystrokes("cmd-f");
    draw(cx);
    assert!(view.read_with(cx, |host, _| host.find_open));
    assert!(view.read_with(cx, |host, _| host.outline_selected_id.is_none()));
    cx.simulate_input("First");
    draw(cx);
    click(cx, "prompt-rail-first");
    assert!(!view.read_with(cx, |host, _| host.find_open));
    // Recall ignores empty prompts and optimistic echoes; Down past newest clears.
    cx.update(|window, cx| {
        view.update(cx, |host, cx| {
            host.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.focus(window, cx);
            });
            cx.notify();
        })
    });
    draw(cx);
    cx.simulate_keystrokes("up");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |host, cx| host.input.read(cx).value()),
        "Third prompt"
    );
    assert!(cx.debug_bounds("prompt-recall-strip").is_some());
    cx.simulate_keystrokes("up");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |host, cx| host.input.read(cx).value()),
        "Second prompt"
    );
    cx.simulate_keystrokes("up");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |host, cx| host.input.read(cx).value()),
        "First prompt\nKeep this line"
    );
    click(cx, "prompt-recall-newer");
    click(cx, "prompt-recall-newer");
    click(cx, "prompt-recall-newer");
    assert_eq!(
        view.read_with(cx, |host, cx| host.input.read(cx).value()),
        ""
    );
    cx.simulate_keystrokes("up");
    cx.simulate_input("Edited ");
    draw(cx);
    assert!(view.read_with(cx, |host, _| host.prompt_recall.is_none()));
    let edited = view.read_with(cx, |host, cx| host.input.read(cx).value());
    cx.simulate_keystrokes("up");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |host, cx| host.input.read(cx).value()),
        edited
    );
    // Chrome stays inside a narrow, large-font conversation.
    cx.update(|_, cx| {
        gpui_component::Theme::global_mut(cx).font_size = gpui::px(20.);
        gpui_component::Theme::sync_base(cx);
    });
    let rem = cx.update(|window, _| window.rem_size());
    cx.simulate_resize(size(rem * 30., rem * 50.));
    draw(cx);
    let rail = cx.debug_bounds("prompt-navigation-rail").unwrap();
    assert!(rail.left() >= gpui::px(0.) && rail.right() <= rem * 30.);
    view.read_with(cx, |host, _| {
        assert_eq!(*host.messages, original);
        assert_eq!(host.fixture.messages, original);
    });
}
