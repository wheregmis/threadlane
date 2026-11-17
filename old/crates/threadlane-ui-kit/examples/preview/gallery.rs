//! Shared native and web catalogue using the same components consumed by Threadlane.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{InputEvent, InputState, TextareaState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{
    ActiveTheme, Disableable, Sizable, StyledExt, WindowExt,
};
use threadlane_protocol::daemon::{
    ChatMessageInfo, MessageRole, PermissionDecision, ToolActivityInfo,
};
use threadlane_protocol::{PermissionRequest, PermissionScope, QuestionItem};
use threadlane_ui_kit as kit;

pub struct Gallery {
    fixture: std::sync::Arc<super::session::Snapshot>,
    code_samples: Entity<crate::code_samples::CodeSamples>,
    file_picker_samples: Entity<crate::file_picker_samples::FilePickerSamples>,
    terminal: Entity<crate::terminal::TerminalPreview>,
    terminal_visible: bool,
    _terminal_subscription: Subscription,
    input: Entity<TextareaState>,
    answer: Entity<InputState>,
    selected: Vec<String>,
    tools: Vec<ToolActivityInfo>,
    completed_expanded: bool,
    reasoning: ChatMessageInfo,
    decision: Option<PermissionDecision>,
    queued_messages: Vec<(String, String)>,
    removing_messages: std::collections::HashSet<String>,
    queue_feedback: Option<String>,
    pending_message: Option<String>,
    attachments: Vec<String>,
    sample_image: std::sync::Arc<RenderImage>,
    image_actual_size: bool,
    saved_draft: Option<String>,
    recalled_prompt: Option<usize>,
    recall_samples: Vec<String>,
    _input_subscription: Subscription,
    document_dirty: bool,
    markdown_preview: kit::MarkdownPreview,
    recovery_sample: usize,
    ignore_whitespace: bool,
    selected_trajectory: Option<usize>,
    review_pr_sample: usize,
    review_pr_expanded: bool,
    draft_pr: Entity<crate::draft_pr::DraftPrPreview>,
    review_excerpt: Entity<crate::review::ReviewExcerptPreview>,
}

impl Gallery {
    pub fn new(
        fixture: std::sync::Arc<super::session::Snapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Describe the next task…")
                .auto_grow(1, 6)
        });
        let input_subscription = cx.subscribe(&input, |host, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) { host.recalled_prompt = None; }
            cx.notify();
        });
        let recall_samples = fixture.messages.iter()
            .filter(|message| message.role == MessageRole::User && !message.content.trim().is_empty())
            .map(|message| message.content.clone()).collect();
        let terminal = cx.new(|cx| crate::terminal::TerminalPreview::new("Sample project".into(), cx));
        let terminal_subscription = cx.subscribe_in(
            &terminal,
            window,
            |host, _, event: &crate::terminal::TerminalPreviewEvent, window, cx| {
                match event {
                    crate::terminal::TerminalPreviewEvent::Hide => host.terminal_visible = false,
                    crate::terminal::TerminalPreviewEvent::AddSelection(text) => {
                        let draft = host.input.read(cx).value();
                        let draft = if draft.is_empty() {
                            text.clone()
                        } else {
                            format!("{draft}\n\n{text}")
                        };
                        host.input
                            .update(cx, |input, cx| input.set_value(draft, window, cx));
                    }
                }
                cx.notify();
            },
        );
        let draft_pr = cx.new(|cx| crate::draft_pr::DraftPrPreview::new(fixture.git_status.as_ref(), window, cx));
        Self {
            recovery_sample: 0,
            review_excerpt: cx.new(|cx| crate::review::ReviewExcerptPreview::new(window, cx)),
            code_samples: cx.new(|_| crate::code_samples::CodeSamples::new()),
            file_picker_samples: cx.new(|_| crate::file_picker_samples::FilePickerSamples::new()),
            fixture, terminal, terminal_visible: true, _terminal_subscription: terminal_subscription,
            input,
            _input_subscription: input_subscription,
            answer: cx.new(|cx| InputState::new(window, cx).placeholder("Your preference")),
            selected: vec!["Compact".into()],
            completed_expanded: false,
            tools: [
                ("read", "read_file", "Read · src/app.rs", "Result", "40:a3f|pub fn render() {\n41:b4e|    // Shared controls preserve keyboard navigation, theme typography, scroll ownership, and source details on native and web. End of source line.\n42:c5d|}"),
                ("search", "grep_search", "Search · composer", "Working", "src/app.rs:42: Composer::new(input)"),
                ("command", "run_command", "Run · cargo check", "Result", "Exit Status: 0\n--- STDOUT ---\nChecking crates/threadlane-ui-kit/examples/preview/components/a-long-component-path-for-shared-native-and-web-rendering.rs — End of output line\nFinished dev profile\n0 errors\n--- STDERR ---\n"),
                ("running", "run_command", "Run · cargo metadata", "Working", ""),
                ("failed", "run_command", "Run · cargo nextest", "Error", "Exit Status: 1\n--- STDOUT ---\nOne test failed.\n--- STDERR ---\nInspect the assertion before rerunning."),
            ].into_iter().map(|(id, title, summary, category, detail)| ToolActivityInfo {
                id: id.into(), title: title.into(), display_summary: summary.into(),
                category: category.into(), detail: detail.into(),
                arguments: match id {
                    "read" => serde_json::json!({"path": "src/app.rs"}),
                    "search" => serde_json::json!({"path": "src", "pattern": "Composer"}),
                    "command" => serde_json::json!({"command": "cargo check -p threadlane-gpui", "cwd": "/sample/project"}),
                    "running" => serde_json::json!({"command": "cargo metadata --no-deps", "cwd": "/sample/project"}),
                    _ => serde_json::json!({"command": "cargo nextest run", "cwd": "/sample/project"}),
                }.to_string(),
                is_expanded: false,
            }).collect(),
            reasoning: ChatMessageInfo {
                id: "gallery-reasoning".into(), role: MessageRole::Assistant,
                content: String::new(), tool_activities: Vec::new(), streaming: false,
                reasoning_content: Some("Inspect existing components, preserve controlled state, and verify keyboard access.".into()),
                reasoning_expanded: false,
                retry_prompt: None,
            },
            decision: None,
            queued_messages: sample_queue(),
            removing_messages: std::collections::HashSet::new(),
            queue_feedback: None,
            pending_message: Some("Check keyboard navigation before finishing this turn.".into()),
            attachments: sample_attachments(),
            sample_image: std::sync::Arc::new(RenderImage::new(vec![image::Frame::new(
                // Grayscale checkerboard is image test data, independent of UI theme colors.
                image::RgbaImage::from_fn(800, 600, |x, y| {
                    let shade = if (x / 80 + y / 80) % 2 == 0 { 220 } else { 80 };
                    image::Rgba([shade, shade, shade, 255])
                }),
            )])),
            image_actual_size: false,
            saved_draft: Some("Check the composer at a narrow width, then verify keyboard focus.".into()),
            recalled_prompt: None,
            recall_samples,
            document_dirty: true,
            markdown_preview: kit::MarkdownPreview::new(cx),
            ignore_whitespace: false,
            selected_trajectory: None,
            review_pr_sample: 0,
            review_pr_expanded: true,
            draft_pr,
        }
    }

    fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        self.input.read(cx).focus_handle(cx).focus(window, cx);
    }

    fn step_recall(&mut self, older: bool, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.recall_samples.len();
        if count == 0 || !self.attachments.is_empty() {
            return;
        }
        if self.recalled_prompt.is_none() && !self.input.read(cx).value().is_empty() {
            return;
        }
        self.recalled_prompt = match (self.recalled_prompt, older) {
            (None, true) => Some(count - 1),
            (Some(index), true) => Some(index.saturating_sub(1)),
            (Some(index), false) if index + 1 < count => Some(index + 1),
            _ => None,
        };
        let text = self
            .recalled_prompt
            .map(|index| self.recall_samples[index].clone())
            .unwrap_or_default();
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self.focus_composer(window, cx);
        cx.notify();
    }

    fn render_saved_draft(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        self.saved_draft.as_ref().map(|draft| {
            let restore =
                kit::restore_saved_draft_button().on_click(cx.listener(|host, _, window, cx| {
                    if !host.input.read(cx).value().is_empty() || !host.attachments.is_empty() {
                        window.push_notification(
                            gpui_component::notification::Notification::info(
                                "Send or clear your draft before restoring the saved draft",
                            ),
                            cx,
                        );
                    } else if let Some(text) = host.saved_draft.take() {
                        host.recalled_prompt = None;
                        host.input
                            .update(cx, |input, cx| input.set_value(text, window, cx));
                    }
                    host.focus_composer(window, cx);
                    cx.notify();
                }));
            let discard =
                kit::discard_saved_draft_button().on_click(cx.listener(|host, _, window, cx| {
                    let Some(draft) = host.saved_draft.clone() else {
                        return;
                    };
                    let entity = cx.entity().downgrade();
                    window.open_alert_dialog(cx, move |alert, _, _| {
                        let entity = entity.clone();
                        let draft = draft.clone();
                        kit::discard_saved_draft_dialog(alert, draft.clone()).on_ok(
                            move |_, _, cx| {
                                let _ = entity.update(cx, |host, cx| {
                                    if host.saved_draft.as_ref() == Some(&draft) {
                                        host.saved_draft = None;
                                    }
                                    cx.notify();
                                });
                                true
                            },
                        )
                    });
                }));
            kit::saved_draft_banner(draft, restore, discard, cx)
        })
    }

    fn render_recall(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        self.recalled_prompt.map(|position| {
            kit::prompt_recall_strip(
                position,
                self.recall_samples.len(),
                kit::recall_older_button(position == 0).on_click(
                    cx.listener(|host, _, window, cx| host.step_recall(true, window, cx)),
                ),
                kit::recall_newer_button().on_click(
                    cx.listener(|host, _, window, cx| host.step_recall(false, window, cx)),
                ),
                cx,
            )
        })
    }

    fn render_draft_actions(&self, cx: &mut Context<Self>) -> Div {
        let text_empty = self.input.read(cx).value().trim().is_empty();
        let save_reason = if self.saved_draft.is_some() {
            Some("Restore or discard the saved draft before saving another")
        } else if !self.attachments.is_empty() {
            Some("Saved drafts support text only; remove attached images first")
        } else if text_empty {
            Some("Type a message to save")
        } else {
            None
        };
        let recall_reason = if !self.attachments.is_empty() {
            Some("Remove attached images before recalling a prompt")
        } else if self.recalled_prompt.is_none() && !self.input.read(cx).value().is_empty() {
            Some("Send or clear your draft before recalling a prompt")
        } else if self.recall_samples.is_empty() {
            Some("No earlier prompts to recall")
        } else {
            None
        };
        kit::composer_actions_group()
            .child(kit::save_draft_button(save_reason).on_click(cx.listener(
                |host, _, window, cx| {
                    if host.saved_draft.is_none() && host.attachments.is_empty() {
                        let text = host.input.read(cx).value().to_string();
                        if !text.trim().is_empty() {
                            host.saved_draft = Some(text);
                            host.recalled_prompt = None;
                            host.input
                                .update(cx, |input, cx| input.set_value("", window, cx));
                        }
                    }
                    cx.notify();
                },
            )))
            .child(
                kit::recall_prompt_button(recall_reason).on_click(
                    cx.listener(|host, _, window, cx| host.step_recall(true, window, cx)),
                ),
            )
    }
    fn render_attachments(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let count = self.attachments.len();
        kit::composer_attachment_group().children(self.attachments.iter().enumerate().map(
            |(ix, name)| {
                let preview_id = SharedString::from(format!("gallery-preview-{name}"));
                let focus = window
                    .use_keyed_state(preview_id.clone(), cx, |_, cx| cx.focus_handle())
                    .read(cx)
                    .clone();
                let preview_name = name.clone();
                let remove_name = name.clone();
                let preview = kit::staged_image_preview_button(preview_id, name, ix + 1, count)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_image_preview(preview_name.clone(), focus.clone(), window, cx);
                    }));
                let remove = kit::staged_image_remove_button(
                    SharedString::from(format!("gallery-remove-{name}")),
                    name,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.attachments.retain(|name| name != &remove_name);
                    cx.notify();
                }));
                kit::staged_image_chip(name.clone(), preview, remove, cx)
            },
        ))
    }

    fn open_image_preview(
        &mut self,
        name: String,
        focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        self.image_actual_size = false;
        let entity = cx.entity().downgrade();
        let close = entity.clone();
        let close_name = name.clone();
        window.open_dialog(cx, move |dialog, window, _| {
            let entity = entity.clone();
            let close = close.clone();
            let name = name.clone();
            let close_name = close_name.clone();
            let focus = focus.clone();
            kit::image_preview_dialog(dialog, window)
                .content(move |content, window, cx| {
                    let image = entity
                        .update(cx, |host, cx| {
                            let fit_owner = entity.clone();
                            let size_owner = entity.clone();
                            let controls = [
                                kit::image_preview_fit_button(
                                    "gallery-image-fit",
                                    !host.image_actual_size,
                                )
                                .on_click(move |_, _, cx| {
                                    let _ = fit_owner.update(cx, |host, cx| {
                                        host.image_actual_size = false;
                                        cx.notify();
                                    });
                                })
                                .into_any_element(),
                                kit::image_preview_actual_size_button(
                                    "gallery-image-size",
                                    host.image_actual_size,
                                    800,
                                    600,
                                )
                                .on_click(move |_, _, cx| {
                                    let _ = size_owner.update(cx, |host, cx| {
                                        host.image_actual_size = true;
                                        cx.notify();
                                    });
                                })
                                .into_any_element(),
                            ];
                            kit::image_preview_content(
                                name.clone(),
                                Some(Ok(host.sample_image.clone())),
                                host.image_actual_size,
                                controls,
                                window,
                                cx,
                            )
                        })
                        .ok();
                    content.children(image)
                })
                .on_close(move |_, window, cx| {
                    let _ = close.update(cx, |host, cx| {
                        if host.attachments.contains(&close_name) {
                            focus.focus(window, cx);
                        } else {
                            host.input.read(cx).focus_handle(cx).focus(window, cx);
                        }
                    });
                })
        });
        cx.notify();
    }

    fn render_pending(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let text = self.pending_message.as_ref()?;
        Some(kit::pending_message_row(
            text.clone(),
            [
                kit::pending_queue_button()
                    .on_click(cx.listener(|host, _, _, cx| {
                        if let Some(text) = host.pending_message.take() {
                            host.queued_messages
                                .push(("gallery-pending-message".into(), text));
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
                kit::pending_steer_button(true, "Steer the current sample turn")
                    .on_click(cx.listener(|host, _, _, cx| {
                        host.pending_message = None;
                        host.queue_feedback = Some("Pending sample used to steer the turn.".into());
                        cx.notify();
                    }))
                    .into_any_element(),
                kit::pending_edit_button()
                    .on_click(cx.listener(|host, _, window, cx| {
                        if let Some(text) = host.pending_message.take() {
                            host.input
                                .update(cx, |input, cx| input.set_value(text, window, cx));
                            host.input.read(cx).focus_handle(cx).focus(window, cx);
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            ],
            cx,
        ))
    }

    fn render_queue(&self, cx: &mut Context<Self>) -> Div {
        let rows = self
            .queued_messages
            .iter()
            .map(|(id, text)| {
                let steer_id = id.clone();
                let edit_id = id.clone();
                let remove_id = id.clone();
                let actions = [
                    kit::queued_steer_button(
                        id,
                        id != "gallery-queue-external",
                        if id == "gallery-queue-external" {
                            "This sample agent does not support live steering"
                        } else {
                            "Steer the current sample turn"
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.queued_messages.retain(|(id, _)| id != &steer_id);
                        this.queue_feedback = Some("Sample message used to steer the turn.".into());
                        cx.notify();
                    }))
                    .into_any_element(),
                    kit::queued_edit_button(id)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if !this.input.read(cx).value().is_empty() {
                                this.queue_feedback = Some(
                                    "Clear the sample draft before editing a queued message."
                                        .into(),
                                );
                            } else if let Some((_, text)) =
                                this.queued_messages.iter().find(|(id, _)| id == &edit_id)
                            {
                                let text = text.clone();
                                this.input
                                    .update(cx, |input, cx| input.set_value(text, window, cx));
                                this.queued_messages.retain(|(id, _)| id != &edit_id);
                                this.queue_feedback =
                                    Some("Sample message restored to the composer.".into());
                                this.input.read(cx).focus_handle(cx).focus(window, cx);
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                    kit::queued_remove_button(id)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.removing_messages.insert(remove_id.clone());
                            this.queue_feedback =
                                Some("Waiting for sample removal acknowledgement.".into());
                            cx.notify();
                        }))
                        .into_any_element(),
                ];
                kit::queued_message_row(
                    id,
                    text.clone(),
                    self.removing_messages.contains(id),
                    actions,
                    cx,
                )
                .into_any_element()
            })
            .collect::<Vec<_>>();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(
                (!rows.is_empty())
                    .then(|| kit::queued_message_panel("gallery-queue", rows.len(), rows, cx)),
            )
            .children(self.queue_feedback.as_ref().map(|feedback| {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(feedback.clone())
            }))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children((!self.removing_messages.is_empty()).then(|| {
                        Button::new("gallery-queue-acknowledge")
                            .small()
                            .ghost()
                            .label("Confirm sample removal")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.queued_messages
                                    .retain(|(id, _)| !this.removing_messages.contains(id));
                                this.removing_messages.clear();
                                this.queue_feedback = Some("Sample removal acknowledged.".into());
                                cx.notify();
                            }))
                    }))
                    .child(
                        Button::new("gallery-queue-reset")
                            .small()
                            .ghost()
                            .label("Reset sample queue")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.queued_messages = sample_queue();
                                this.removing_messages.clear();
                                this.queue_feedback = None;
                                this.pending_message = Some(
                                    "Check keyboard navigation before finishing this turn.".into(),
                                );
                                cx.notify();
                            })),
                    ),
            )
    }

    fn render_tool(
        &self,
        tool: &ToolActivityInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let motion = kit::DisclosureMotion::new(
            SharedString::from(format!("gallery-tool-{}", tool.id)),
            tool.is_expanded,
            window,
            cx,
        );
        let detail = motion
            .is_visible()
            .then(|| {
                let args = kit::tool_detail::args_json(&tool.arguments).unwrap_or_default();
                let path = kit::tool_detail::args_path(&args).unwrap_or_else(|| ".".into());
                kit::tool_preview::render(
                    tool,
                    path.clone(),
                    std::path::PathBuf::from(path),
                    |id, path, line, folder| {
                        kit::tool_preview::open_button(
                            id,
                            &path,
                            line,
                            folder,
                            true,
                            |_, window, cx| {
                                window.push_notification(
                                    gpui_component::notification::Notification::info(
                                        "Sample file action · no file was opened",
                                    ),
                                    cx,
                                );
                            },
                        )
                    },
                    cx,
                )
                .or_else(|| {
                    kit::tool_detail::render_activity_detail_card(
                        tool,
                        None::<fn(String, &mut App)>,
                        cx,
                    )
                })
                .expect("gallery samples use supported tool renderers")
            })
            .map(|body| motion.content(body));
        kit::tool_activity(
            tool,
            true,
            detail,
            false,
            {
                let entity = cx.entity().downgrade();
                let tool_id = tool.id.clone();
                move |_, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        if let Some(tool) = this.tools.iter_mut().find(|tool| tool.id == tool_id) {
                            tool.is_expanded = !tool.is_expanded;
                        }
                        cx.notify();
                    });
                }
            },
            cx,
        )
    }
}

impl Gallery {
    fn render_sidebar_samples(&self, cx: &App) -> Div {
        let mut samples = div().flex().flex_wrap().gap_4();
        for (index, label, attention, snooze, unavailable) in [
            (
                0,
                "Ready",
                threadlane_protocol::daemon::SessionAttention::Ready,
                None,
                false,
            ),
            (
                1,
                "Working",
                threadlane_protocol::daemon::SessionAttention::Working,
                None,
                false,
            ),
            (
                2,
                "Snooze save failed",
                threadlane_protocol::daemon::SessionAttention::Idle,
                Some(kit::SidebarSnoozeStatus::SaveFailed),
                false,
            ),
            (
                3,
                "Unavailable worktree",
                threadlane_protocol::daemon::SessionAttention::NeedsYou,
                None,
                true,
            ),
        ] {
            let mut session = self.fixture.session.clone().unwrap_or_default();
            session.id = format!("sidebar-gallery-{index}");
            if unavailable {
                session.is_worktree = true;
                session.worktree_available = false;
            }
            let project = session
                .work_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Project")
                .to_owned();
            let quick_snooze = snooze.clone();
            let full_snooze = snooze.clone();
            let request = |action: kit::SidebarSessionAction, window: &mut Window, cx: &mut App| {
                window.push_notification(format!("Sample request: {action:?}"), cx);
            };
            let card = kit::sidebar_session_card(
                &session,
                kit::SidebarSessionCardState {
                    show_project: true,
                    project,
                    attention,
                    selected: index == 0,
                    pinned: false,
                    unseen_result: index == 0,
                    snooze,
                    git_status: self.fixture.git_status.clone(),
                    pr: None,
                    now: self.fixture.captured_at,
                },
                request,
                move |menu, window, cx| {
                    kit::sidebar_session_menu(
                        menu,
                        kit::SidebarSessionMenuState::new(
                            quick_snooze
                                .clone()
                                .map(kit::SidebarSnoozeMenu::Status)
                                .unwrap_or_else(|| {
                                    kit::SidebarSnoozeMenu::Unavailable("Static sample".into())
                                }),
                        )
                        .terminal_available(!unavailable),
                        kit::SidebarSessionMenuScope::Quick,
                        request,
                        window,
                        cx,
                    )
                },
                move |menu, window, cx| {
                    kit::sidebar_session_menu(
                        menu,
                        kit::SidebarSessionMenuState::new(
                            full_snooze
                                .clone()
                                .map(kit::SidebarSnoozeMenu::Status)
                                .unwrap_or_else(|| {
                                    kit::SidebarSnoozeMenu::Unavailable("Static sample".into())
                                }),
                        )
                        .terminal_available(!unavailable),
                        kit::SidebarSessionMenuScope::Full,
                        request,
                        window,
                        cx,
                    )
                },
                cx,
            );
            samples = samples.child(
                div()
                    .w(rems(16.5))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    )
                    .child(card),
            );
        }
        samples
    }
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let entity = cx.entity().downgrade();
        let mut content = div()
            .flex()
            .flex_col()
            .gap_6()
            .w_full()
            .max_w(rems(46.0))
            .mx_auto()
            .p_6();
        let summary = kit::TrajectorySummary::from_entries(&self.fixture.trajectory);
        content = content
            .child(heading("Composer files · local samples", cx))
            .child(self.file_picker_samples.clone())
            .child(heading("Conversation code · local samples", cx))
            .child(self.code_samples.clone())
            .child(heading("Trajectory · saved session", cx))
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "Canonical events from the imported journal. Select a row to inspect its output."))
            .child(kit::trajectory_overview(&summary, cx))
            .child(kit::trajectory_stats(&summary, cx))
            .child(div().w_full().min_w_0().flex().flex_col().children(
                self.fixture.trajectory.iter().take(8).enumerate().map(|(index, entry)| {
                    let owner = entity.clone();
                    kit::trajectory_event_row(kit::trajectory_event_id("gallery-trajectory", entry, index),
                        entry, entry.summary.clone().into(), self.selected_trajectory == Some(index),
                        move |_, cx| { let _ = owner.update(cx, |host, cx| {
                            host.selected_trajectory = Some(index); cx.notify();
                        }); }, cx)
                })))
            .children(self.selected_trajectory.and_then(|index| self.fixture.trajectory.get(index))
                .map(|entry| kit::trajectory_entry_preview(entry)))
            .children(self.fixture.trajectory.is_empty().then(|| div().text_sm().text_color(theme.muted_foreground)
                .child("Import a saved session to preview its trajectory events.")))
            .child(heading("Typography", cx))
            .child(
                div().flex().flex_col().gap_3().children(
                    [
                        ("Regular · 400", FontWeight::NORMAL),
                        ("Medium · 500", FontWeight::MEDIUM),
                        ("Semibold · 600", FontWeight::SEMIBOLD),
                        ("Bold · 700", FontWeight::BOLD),
                    ]
                    .into_iter()
                    .map(|(label, weight)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_xs().text_color(theme.muted_foreground).child(label))
                            .child(
                                div()
                                    .text_base()
                                    .font_weight(weight)
                                    .child("The quick brown fox · UI 0123456789"),
                            )
                            .child(
                                div()
                                    .text_base()
                                    .font_weight(weight)
                                    .italic()
                                    .child("The quick brown fox · UI 0123456789"),
                            )
                    }),
                ),
            )
            .child(heading("Review pull request · local sample", cx))
            .child(div().flex().flex_wrap().gap_2().children(["Failing", "Pending", "Passed", "No checks", "Busy"].into_iter().enumerate().map(|(index, label)| {
                Button::new(SharedString::from(format!("gallery-review-pr-state-{index}"))).label(label).ghost().small()
                    .on_click(cx.listener(move |host, _, _, cx| { host.review_pr_sample = index; cx.notify(); }))
            })))
            .child(div().w(rems(24.0)).max_w_full().child(kit::review_pr_card(&crate::review::sample_review_pr(self.review_pr_sample), &kit::ReviewPrState {
                expanded: self.review_pr_expanded, feedback_count: Some(2), can_address: true, busy: self.review_pr_sample == 4,
            }, cx.listener(|host, action: &kit::ReviewPrAction, window, cx| {
                if *action == kit::ReviewPrAction::Toggle { host.review_pr_expanded = !host.review_pr_expanded; cx.notify(); }
                else { window.push_notification(gpui_component::notification::Notification::info("Sample PR action · no GitHub request or task was sent"), cx); }
            }), cx)))
            .child(heading("Draft pull request · local samples", cx))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("No GitHub or model requests are sent. Fields remain local."))
            .child(div().flex().flex_wrap().gap_2().children([
                (crate::draft_pr::DraftPrSample::Ready, "Ready"), (crate::draft_pr::DraftPrSample::Failure, "Failure"),
                (crate::draft_pr::DraftPrSample::Uncertain, "Uncertain"), (crate::draft_pr::DraftPrSample::ChangedCheckout, "Changed checkout"),
                (crate::draft_pr::DraftPrSample::Created, "Newer edits"),
            ].into_iter().enumerate().map(|(index, (sample, label))| Button::new(SharedString::from(format!("gallery-draft-pr-{index}")))
                .debug_selector(move || format!("gallery-draft-pr-{index}")).label(label).small().ghost()
                .on_click(cx.listener(move |host, _, window, cx| { host.draft_pr.update(cx, |this, cx| this.set_sample(sample, cx)); crate::draft_pr::open(host.draft_pr.clone(), window, cx); })))))
            .child(heading("Reopen closed file · saved-file recovery states", cx))
            .child(div().flex().flex_wrap().gap_2().children(
                ["Available", "Empty", "Loading", "Failed"].into_iter().enumerate().map(|(index, label)| {
                    Button::new(SharedString::from(format!("gallery-recovery-{index}")))
                        .label(label).small().ghost()
                        .on_click(cx.listener(move |host, _, _, cx| { host.recovery_sample = index; cx.notify(); }))
                })
            ))
            .child(div().max_w(rems(32.)).flex().flex_col()
                .child(kit::editor_reopen_button(&kit::ReopenClosedFileControl::default()
                    .with_target((self.recovery_sample != 1).then(|| "Sample worktree / src/例 example.rs".into()))
                    .with_loading(self.recovery_sample == 2)
                ).on_click(cx.listener(|host, _, _, cx| {
                    host.recovery_sample = 3;
                    cx.notify();
                })))
                .children((self.recovery_sample >= 2).then(|| kit::editor_file_status(
                    "Sample worktree / src/例 example.rs".into(),
                    (self.recovery_sample == 3).then(|| "Couldn't read the saved sample. Retry uses captured data only.".into()),
                    kit::editor_retry_button("Sample worktree / src/例 example.rs", false)
                        .on_click(cx.listener(|host, _, _, cx| { host.recovery_sample = 0; cx.notify(); })),
                    cx,
                )))
            )
            .child(heading("Markdown document · current buffer sample", cx))
            .child(
                div()
                    .h(rems(24.))
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .on_action(
                        cx.listener(|host, _: &kit::ToggleMarkdownPreview, window, cx| {
                            host.markdown_preview
                                .toggle(MARKDOWN_DOCUMENT_SAMPLE.into(), cx);
                            host.markdown_preview.focus_control(window, cx);
                            cx.notify();
                        }),
                    )
                    .child(kit::panel_document_header(
                        Some(self.markdown_preview.control(false, cx)),
                        "README.md",
                        true,
                        false,
                        Some("Markdown"),
                        false,
                        Some(kit::AddSelectionControl {
                            enabled: false,
                            reason: Some(
                                if self.markdown_preview.is_active() {
                                    kit::PREVIEW_SELECTION_REASON
                                } else {
                                    "Read-only gallery sample"
                                }
                                .into(),
                            ),
                        }),
                        |_, window, cx| {
                            window.push_notification("Read-only sample · no file was changed", cx);
                        },
                    ))
                    .children(
                        self.markdown_preview
                            .notice(true)
                            .map(|notice| kit::markdown_preview_notice(notice, cx)),
                    )
                    .child(if self.markdown_preview.is_active() {
                        self.markdown_preview.body(cx)
                    } else {
                        div()
                            .overflow_y_scrollbar()
                            .child(MARKDOWN_DOCUMENT_SAMPLE)
                            .into_any_element()
                    }),
            )
            .child(heading("Panel document header · local sample", cx))
            .children([
                kit::EditorSaveStatus::Saving,
                kit::EditorSaveStatus::Conflict,
                kit::EditorSaveStatus::Deleted,
                kit::EditorSaveStatus::Failed { message: "Couldn't reload: permission denied. Your edits are still here. Check the file and retry Reload from disk.".into(), save_blocked: true },
                kit::EditorSaveStatus::Unsupported,
            ].into_iter().enumerate().map(|(index, status)| div().id(("gallery-save-status", index)).children(
                kit::editor_save_recovery("sample / src/example.rs".into(), &status,
                    |action, window, cx| {
                        if *action == kit::EditorRecoveryAction::Reload {
                            kit::confirm_editor_reload("sample / src/example.rs".into(), window, cx, |window, cx| window.push_notification("Preview only · no buffer was changed", cx));
                        } else { window.push_notification("Preview only · no file was changed", cx); }
                    }, cx))))
            .child(kit::panel_document_header(None, "example.rs", self.document_dirty, self.document_dirty, Some("Rust"), true,
                Some(kit::AddSelectionControl { enabled: false, reason: Some("Select code in the file first".into()) }),
                cx.listener(|host, action: &kit::PanelDocumentAction, window, cx| {
                    if *action == kit::PanelDocumentAction::Save { host.document_dirty = false; }
                    else { window.push_notification(gpui_component::notification::Notification::info("Sample header action · no file was closed"), cx); }
                    cx.notify();
                })))
            .child(kit::review_whitespace_control(self.ignore_whitespace,
                cx.listener(|host, value: &bool, _, cx| { host.ignore_whitespace = *value; cx.notify(); }), cx))
            .child(Button::new("gallery-document-modify").label("Mark sample as modified").small().ghost()
                .on_click(cx.listener(|host, _, _, cx| { host.document_dirty = true; cx.notify(); })))
            .child(heading("Review diff excerpts · local samples", cx))
            .child(self.review_excerpt.clone())
            .child(heading("Environment · saved session", cx))
            .child(
                div()
                    .h(rems(38.0))
                    .flex()
                    .child(super::session::saved_environment(&self.fixture, {
                        let owner = cx.entity().downgrade();
                        move |action, window, cx| {
                            if action == kit::EnvironmentAction::Terminal {
                                let _ = owner.update(cx, |host, cx| { host.terminal_visible = true; cx.notify(); });
                            } else { super::session::SessionPreview::environment_action(action, window, cx); }
                        }
                    }, cx)),
            )
            .child(heading("Terminal chrome · local sample", cx))
            .child(if self.terminal_visible {
                div().h(rems(20.0)).child(self.terminal.clone()).into_any_element()
            } else {
                Button::new("gallery-terminal-show").label("Show terminal sample").small().ghost()
                    .on_click(cx.listener(|host, _, _, cx| { host.terminal_visible = true; cx.notify(); })).into_any_element()
            })
            .child(heading("Task plan · sample", cx))
            .children(kit::plan_tracker(
                "gallery-plan",
                &threadlane_protocol::SessionPlan {
                    explanation: Some("Refine the same components on desktop and web.".into()),
                    items: vec![
                        threadlane_protocol::PlanItem {
                            step: "Extract existing presentation into the kit".into(),
                            status: threadlane_protocol::PlanItemStatus::Completed,
                        },
                        threadlane_protocol::PlanItem {
                            step: "Inspect shared components with real conversation content".into(),
                            status: threadlane_protocol::PlanItemStatus::InProgress,
                        },
                        threadlane_protocol::PlanItem {
                            step: "Verify keyboard controls and responsive layout".into(),
                            status: threadlane_protocol::PlanItemStatus::Pending,
                        },
                    ],
                },
                false,
                cx,
            ))
            .child(heading("Conversation", cx))
            .child(
                kit::message_row(MessageRole::User).child(
                    kit::user_message_bubble(cx)
                        .child("Make tool previews cleaner and keep the details available."),
                ),
            )
            .child(kit::message_row(MessageRole::Assistant).child(
                "Tool results now start as a compact row. Expand a result to inspect its output.",
            ));
        let motion = kit::DisclosureMotion::new(
            "gallery-reasoning-motion",
            self.reasoning.reasoning_expanded,
            window,
            cx,
        );
        let detail = motion
            .is_visible()
            .then(|| {
                kit::reasoning_detail(cx)
                    .child(self.reasoning.reasoning_content.clone().unwrap_or_default())
                    .into_any_element()
            })
            .map(|body| motion.content(body));
        content = content.child(kit::reasoning_card(
            &self.reasoning,
            detail,
            false,
            {
                let entity = entity.clone();
                move |_, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.reasoning.reasoning_expanded = !this.reasoning.reasoning_expanded;
                        cx.notify();
                    });
                }
            },
            cx,
        ));

        let motion = kit::DisclosureMotion::new(
            "gallery-completed-tools",
            self.completed_expanded,
            window,
            cx,
        );
        let owner = entity.clone();
        let tools = kit::completed_activity_group(
            "gallery-tools",
            self.completed_expanded,
            self.tools.iter(),
            &motion,
            |tool| self.render_tool(tool, window, cx),
            move |_, cx| {
                let _ = owner.update(cx, |this, cx| {
                    this.completed_expanded = !this.completed_expanded;
                    cx.notify();
                });
            },
            &theme,
        );
        content = content
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(heading("Tool results", cx))
                    .child(
                        Button::new("gallery-finish-search")
                            .small()
                            .ghost()
                            .label("Finish search")
                            .disabled(
                                !self
                                    .tools
                                    .iter()
                                    .any(|tool| tool.id == "search" && tool.category == "Working"),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(search) =
                                    this.tools.iter_mut().find(|tool| tool.id == "search")
                                {
                                    search.category = "Result".into();
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(tools);

        let permission = PermissionRequest {
            id: "gallery-permission".into(),
            capability: "run_command".into(),
            title: "Run the project checks".into(),
            detail: "cargo check -p threadlane-gpui".into(),
            scopes: vec![PermissionScope::Once, PermissionScope::Session],
        };
        content = content
            .child(heading("Decisions", cx))
            .child(kit::permission_card(
                &permission,
                false,
                self.decision.is_none(),
                None,
                {
                    let entity = entity.clone();
                    move |_, decision, _, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.decision = Some(decision);
                            cx.notify();
                        });
                    }
                },
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(match self.decision {
                                None => "Choose how to handle this sample request.",
                                Some(PermissionDecision::Deny) => "Sample request denied.",
                                Some(PermissionDecision::AllowOnce) => {
                                    "Sample request allowed once."
                                }
                                Some(PermissionDecision::AllowSession) => {
                                    "Sample capability allowed for this session."
                                }
                                Some(PermissionDecision::AllowAlways) => {
                                    "Sample capability allowed for this project."
                                }
                            }),
                    )
                    .children(self.decision.is_some().then(|| {
                        Button::new("gallery-reset-decision")
                            .small()
                            .ghost()
                            .label("Reset decision")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.decision = None;
                                cx.notify();
                            }))
                    })),
            )
            .child(kit::question_surface(cx).child(kit::question_item(
                "gallery-question",
                &QuestionItem {
                    id: "gallery-density".into(),
                    header: "Density".into(),
                    question: "How should the conversation read?".into(),
                    options: vec!["Compact".into(), "Comfortable".into()],
                    allow_custom: true,
                },
                &self.selected,
                Some(&self.answer),
                false,
                {
                    let entity = entity.clone();
                    move |value, _, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            if this.selected.iter().any(|selected| selected == value) {
                                this.selected.clear();
                            } else {
                                this.selected = vec![value.to_owned()];
                            }
                            cx.notify();
                        });
                    }
                },
                cx,
            )));

        content = content
            .child(heading("Composer", cx))
            .children(self.render_pending(cx))
            .child(
                kit::composer_surface(self.input.read(cx).focus_handle(cx).is_focused(window), cx)
                    .children(self.render_saved_draft(cx))
                    .children(self.render_recall(cx))
                    .child(self.render_attachments(window, cx))
                    .child(kit::composer_input(&self.input))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .justify_end()
                            .mt_2()
                            .child(self.render_draft_actions(cx))
                            .child(
                                Button::new("gallery-reset-attachments")
                                    .small()
                                    .ghost()
                                    .label("Reset sample attachments")
                                    .on_click(cx.listener(|host, _, _, cx| {
                                        host.attachments = sample_attachments();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("gallery-clear")
                                    .small()
                                    .ghost()
                                    .label("Clear draft")
                                    .disabled(self.input.read(cx).value().is_empty())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.input.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                        this.recalled_prompt = None;
                                        this.focus_composer(window, cx);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(heading("Queued messages · sample", cx))
            .child(self.render_queue(cx))
            .child(heading("Session navigation · static state samples", cx))
            .child(self.render_sidebar_samples(cx));

        div()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .flex()
            .flex_col()
            .child(
                div()
                    .px_6()
                    .pt(threadlane_ui_theme::WINDOW_CONTROLS_CLEARANCE)
                    .pb_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .flex_wrap()
                            .gap_3()
                            .child(div().text_lg().font_semibold().child("Threadlane UI Kit"))
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        Button::new("gallery-motion")
                                            .small()
                                            .ghost()
                                            .label(if cx.reduce_motion() {
                                                "Enable motion"
                                            } else {
                                                "Reduce motion"
                                            })
                                            .on_click(|_, window, cx| {
                                                cx.set_reduce_motion(!cx.reduce_motion());
                                                window.refresh();
                                            }),
                                    )
                                    .child(
                                        Button::new("gallery-theme")
                                            .small()
                                            .ghost()
                                            .label(if cx.theme().is_dark() {
                                                "Light theme"
                                            } else {
                                                "Dark theme"
                                            })
                                            .on_click(|_, _, cx| {
                                                let name = if cx.theme().is_dark() {
                                                    "Threadlane Light"
                                                } else {
                                                    "Threadlane Dark"
                                                };
                                                threadlane_ui_theme::preview_theme(name, cx);
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Saved session and sample content · Local interactions"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(content),
            )
    }
}

fn heading(label: &'static str, cx: &App) -> Stateful<Div> {
    div()
        .id(label)
        .role(Role::Heading)
        .aria_label(label)
        .text_sm()
        .font_semibold()
        .text_color(cx.theme().foreground)
        .child(label)
}

fn sample_queue() -> Vec<(String, String)> {
    vec![
        ("gallery-queue-follow-up".into(), "Verify the updated chat layout at a narrow window width.\nKeep the full follow-up visible while the agent is working.".into()),
        ("gallery-queue-external".into(), "Summarize the changes after the current turn finishes.".into()),
    ]
}

fn sample_attachments() -> Vec<String> {
    vec![
        "layout-reference-with-a-long-filename.png".into(),
        "density-sample.png".into(),
    ]
}

const MARKDOWN_DOCUMENT_SAMPLE: &str = r#"# Workspace notes

Current buffer preview includes **unsaved changes**.

> Read-only: task boxes and code never run.

- [x] Review the plan
- [ ] Save explicitly

| File | Purpose |
| --- | --- |
| README.md | Project documentation |

```rust
fn example() { println!("display only"); }
```

[External link](https://example.com) · [Blocked local link](../README.md)

![HTTP image](https://example.com/image.png)
![Local image](file:///tmp/image.png)
![Data image](data:image/png;base64,AA)
[![Linked image](https://example.com/linked.png)](https://example.com)
<img src="file:///tmp/raw.png">
"#;
