use gpui::{
    div, px, size, AppContext, Context, InteractiveElement, Modifiers, ParentElement, Render,
    Styled, TestAppContext, Window,
};
use threadlane_protocol::{
    PermissionRequest, PermissionScope, PlanItem, PlanItemStatus, SessionPlan,
};

#[gpui::test]
fn standalone_kit_has_the_shared_source_grammars(cx: &mut TestAppContext) {
    use gpui_component::ActiveTheme;
    cx.update(gpui_component::init);
    cx.update(|cx| {
        for (language, source) in [
            ("rust", "pub fn render() { let label = \"日本語\"; }"),
            ("python", "def render():\n    return True\n"),
            ("javascript", "const label = '日本語';"),
            ("typescript", "const label: string = '日本語';"),
            ("html", "<div title=\"日本語\">Hello</div>"),
            ("css", "div { color: red; }"),
            ("bash", "echo '日本語'"),
            ("toml", "label = \"日本語\"\n"),
            ("yaml", "label: 日本語\n"),
            ("markdown", "# 日本語\n\n**Shared** components.\n"),
            ("diff", "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n"),
            ("json", "{\"label\":\"日本語\"}"),
        ] {
            let mut parser = gpui_component::highlighter::SyntaxHighlighter::new(language);
            parser.update(None, &gpui_component::Rope::from_str(source), None);
            assert!(
                parser.tree().is_some(),
                "{language} grammar missing from kit"
            );
            assert!(
                !parser.tree().unwrap().root_node().has_error(),
                "{language} source parsed with syntax errors"
            );
            let spans = parser.styles(&(0..source.len()), cx.theme().highlight_theme.as_ref());
            assert!(
                spans.iter().any(|(_, style)| style.color.is_some()),
                "{language} source has no syntax colors"
            );
            for (range, _) in spans {
                assert!(range.start <= range.end && range.end <= source.len());
                assert!(source.is_char_boundary(range.start) && source.is_char_boundary(range.end));
            }
        }
    });
}

struct RefinementPreview;

impl Render for RefinementPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let request = PermissionRequest {
            id: "refinement-permission".into(),
            capability: "run_command".into(),
            title: "Run the project checks before applying the proposed changes".into(),
            detail: "Review the command and choose how long permission should last.".into(),
            scopes: vec![
                PermissionScope::Once,
                PermissionScope::Session,
                PermissionScope::Always,
            ],
        };
        let plan = SessionPlan {
            explanation: Some("Check the shared components at the current window size.".into()),
            items: vec![PlanItem {
                step: "Inspect long task descriptions and keep every action reachable. ".repeat(8),
                status: PlanItemStatus::InProgress,
            }],
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(super::permission_card(
                &request,
                false,
                true,
                None,
                |_, _, _, _| {},
                cx,
            ))
            .children(super::plan_tracker("refinement-plan", &plan, false, cx))
            .child(
                super::composer_toolbar(cx)
                    .child(super::composer_model_button("Balanced", true, false, false))
                    .child(super::composer_mode_button("Agent", true))
                    .child(super::composer_effort_button(threadlane_protocol::ReasoningEffort::Medium)),
            )
    }
}

struct CodeHeaderPreview;

struct ToolOutputPreview {
    read: bool,
    rows: usize,
    long_lines: bool,
}

impl Render for ToolOutputPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let activity = threadlane_protocol::daemon::ToolActivityInfo {
            id: "output-refinement".into(),
            title: if self.read { "read_file" } else { "run_command" }.into(),
            category: "Result".into(),
            display_summary: "Inspect shared component output".into(),
            arguments: serde_json::json!({"command": "cargo check", "path": "src/a-very-long-file-name.rs"}).to_string(),
            detail: if self.long_lines {
                let line = format!("{} end_of_line", "長い_output_".repeat(30));
                if self.read { format!("1:a3f|{line}\n") } else { line }
            } else if self.read {
                (0..self.rows).map(|ix| format!("{}:a3f|fn component() {{}}\n", 999998 + ix)).collect()
            } else {
                "Checked shared component\n".repeat(self.rows)
            },
            is_expanded: true,
        };
        let body = if self.read {
            super::tool_preview::render(
                &activity,
                "src/a-very-long-file-name.rs".into(),
                "/sample".into(),
                |id, path, line, folder| {
                    super::tool_preview::open_button(id, &path, line, folder, true, |_, _, _| {})
                },
                cx,
            )
            .unwrap()
        } else {
            super::tool_detail::render_command_card(&activity, cx)
        };
        div().w_full().child(body)
    }
}

#[gpui::test]
fn short_tool_output_shrinks_and_long_output_scrolls_at_zoom(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for read in [false, true] {
        for rows in [1, 200] {
            let (_, cx) = cx.add_window_view(|window, cx| {
                gpui_component::Root::new(
                    cx.new(|_| ToolOutputPreview {
                        read,
                        rows,
                        long_lines: false,
                    }),
                    window,
                    cx,
                )
            });
            cx.simulate_resize(size(px(320.), px(900.)));
            for font in [16., 20.] {
                cx.update(|window, cx| {
                    gpui_component::Theme::global_mut(cx).font_size = px(font);
                    gpui_component::Theme::sync_base(cx);
                    window.refresh();
                    window.draw(cx).clear(cx);
                });
                let viewport = cx
                    .debug_bounds(if read {
                        "tool-preview-viewport"
                    } else {
                        "command-output"
                    })
                    .unwrap();
                assert!(
                    viewport.size.height > px(0.) && viewport.size.height <= px(font * 6.),
                    "read={read}, rows={rows}, font={font}, viewport={viewport:?}"
                );
                if rows == 1 {
                    assert!(
                        viewport.size.height < px(font * 4.),
                        "short output must not reserve six rows of blank space: {viewport:?}"
                    );
                } else {
                    let selector = if read {
                        "tool-preview-content"
                    } else {
                        "command-output-content"
                    };
                    let before = cx.debug_bounds(selector).unwrap();
                    cx.simulate_event(gpui::ScrollWheelEvent {
                        position: viewport.center(),
                        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-40.))),
                        ..Default::default()
                    });
                    cx.run_until_parked();
                    cx.update(|window, cx| window.draw(cx).clear(cx));
                    assert!(
                        cx.debug_bounds(selector).unwrap().top() < before.top(),
                        "long output must remain scrollable"
                    );
                }
                if read {
                    let header = cx.debug_bounds("tool-preview-header").unwrap();
                    let action = cx.debug_bounds("tool-preview-open").unwrap();
                    assert_eq!(
                        action.intersect(&header),
                        action,
                        "file action remains inside its header at font={font}"
                    );
                }
            }
        }
    }
}

#[gpui::test]
fn tool_output_copy_preserves_content_and_tracks_new_output(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for read in [false, true] {
        let mut preview = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| ToolOutputPreview {
                read,
                rows: 1,
                long_lines: false,
            });
            preview = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let preview = preview.unwrap();
        cx.simulate_resize(size(px(320.), px(900.)));
        cx.update(|window, cx| {
            gpui_component::Theme::global_mut(cx).font_size = px(20.);
            gpui_component::Theme::sync_base(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let selector = "tool-output-copy-output-refinement";
        let copied_selector = "tool-output-copy-output-refinement-copied";
        let copy = cx.debug_bounds(selector).expect("output has a copy action");
        assert!(copy.left() >= px(0.) && copy.right() <= px(320.));
        let text = if read {
            "fn component() {}"
        } else {
            "Checked shared component\n"
        };
        cx.update(|window, cx| {
            window.focus_next(cx);
            window.draw(cx).clear(cx);
        });
        if read {
            cx.simulate_keystrokes("tab");
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        let keystroke = gpui::Keystroke::parse("enter").unwrap();
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(text.into())
        );
        assert!(cx.debug_bounds(copied_selector).is_some());

        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.executor().tick();
        cx.run_until_parked();
        preview.update(cx, |preview, cx| {
            preview.rows = 2;
            cx.notify();
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(
            cx.debug_bounds(copied_selector).is_none(),
            "new output is not already copied"
        );
        let copy = cx.debug_bounds(selector).unwrap();
        cx.simulate_click(copy.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(if read {
                format!("{text}\n{text}")
            } else {
                text.repeat(2)
            })
        );
        assert!(cx.debug_bounds(copied_selector).is_some());
        // The previous copy's timer must not clear this newer copy's feedback.
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.executor().tick();
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds(copied_selector).is_some());
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.executor().tick();
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds(copied_selector).is_none());
        assert!(cx.debug_bounds(selector).is_some());
    }
}

#[gpui::test]
fn long_tool_lines_scroll_horizontally_without_moving_the_header(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for read in [false, true] {
        let (_, cx) = cx.add_window_view(|window, cx| {
            gpui_component::Root::new(
                cx.new(|_| ToolOutputPreview {
                    read,
                    rows: 1,
                    long_lines: true,
                }),
                window,
                cx,
            )
        });
        cx.simulate_resize(size(px(320.), px(900.)));
        cx.update(|window, cx| {
            gpui_component::Theme::global_mut(cx).font_size = px(20.);
            gpui_component::Theme::sync_base(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let viewport = cx
            .debug_bounds(if read {
                "tool-preview-viewport"
            } else {
                "command-output"
            })
            .unwrap();
        let selector = if read {
            "tool-preview-content"
        } else {
            "command-output-content"
        };
        let before = cx.debug_bounds(selector).unwrap();
        let header = cx
            .debug_bounds(if read {
                "tool-preview-header"
            } else {
                "command-text"
            })
            .unwrap();
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(-80.), px(0.))),
            ..Default::default()
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds(selector).unwrap().left() < before.left(),
            "read={read}: long lines must remain horizontally reachable"
        );
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(-10000.), px(0.))),
            ..Default::default()
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds(selector).unwrap().right() <= viewport.right() + px(1.),
            "the end of a long line must be reachable"
        );
        assert_eq!(
            cx.debug_bounds(if read {
                "tool-preview-header"
            } else {
                "command-text"
            })
            .unwrap(),
            header
        );
        assert!(
            viewport.right() <= px(320.),
            "long lines must not widen the card"
        );
    }
}

impl Render for CodeHeaderPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        div().size_full().child(
            super::code_block_surface("refinement", cx).child(
                super::code_block_header(
                    "refinement",
                    "powershell",
                    Some("scripts/a-very-long-project-name/verify-all-components.ps1"),
                    super::code_block_actions()
                        .child(super::code_block_run_button("refinement").on_click(|_, _, _| {}))
                        .child(super::code_block_open_button("refinement").on_click(|_, _, _| {}))
                        .child(
                            super::code_block_copy_button("refinement", false, cx)
                                .on_click(|_, _, _| {}),
                        ),
                    cx,
                )
                .debug_selector(|| "refinement-code-header".into()),
            ),
        )
    }
}

#[gpui::test]
fn code_actions_stay_reachable_at_narrow_width_and_zoom(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|_| CodeHeaderPreview);
        gpui_component::Root::new(view, window, cx)
    });
    for font in [13., 20.] {
        for width in [320., 480., 800.] {
            cx.simulate_resize(size(px(width), px(900.)));
            cx.update(|window, cx| {
                gpui_component::Theme::global_mut(cx).font_size = px(font);
                gpui_component::Theme::sync_base(cx);
                window.refresh();
                window.draw(cx).clear(cx);
            });
            let header = cx.debug_bounds("refinement-code-header").unwrap();
            for action in [
                "run-term-refinement",
                "open-edit-refinement",
                "copy-code-refinement",
            ] {
                let bounds = cx.debug_bounds(action).unwrap();
                assert_eq!(
                    bounds.intersect(&header),
                    bounds,
                    "{action} clipped at font={font}, width={width}: {bounds:?}, header={header:?}"
                );
                assert!(bounds.right() <= px(width));
            }
        }
    }
}

#[gpui::test]
fn decisions_and_composer_remain_aligned_at_narrow_width_and_zoom(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(|cx| cx.set_reduce_motion(true));
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|_| RefinementPreview);
        gpui_component::Root::new(view, window, cx)
    });
    cx.simulate_resize(size(px(480.), px(900.)));
    for font in [13., 20.] {
        cx.update(|window, cx| {
            gpui_component::Theme::global_mut(cx).font_size = px(font);
            gpui_component::Theme::sync_base(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let title = cx.debug_bounds("permission-title").unwrap();
        let actions = cx.debug_bounds("permission-actions").unwrap();
        assert!((title.left() - actions.left()).abs() <= px(1.));
        assert!(
            (title.size.width - actions.size.width).abs() <= px(1.),
            "actions must not squeeze the title"
        );
        assert!(actions.top() >= title.bottom());
        for selector in ["permission-inline-session", "permission-inline-always"] {
            let bounds = cx.debug_bounds(selector).unwrap();
            assert!(bounds.left() >= px(0.) && bounds.right() <= px(480.));
        }
        let model = cx.debug_bounds("composer-model-picker").unwrap();
        let mode = cx.debug_bounds("composer-mode-picker").unwrap();
        let effort = cx.debug_bounds("composer-reasoning-effort-picker").unwrap();
        assert_eq!(model.size.height, mode.size.height);
        assert_eq!(model.size.height, effort.size.height);
        let tracker = cx.debug_bounds("session-plan-tracker").unwrap();
        cx.simulate_click(tracker.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let details = cx.debug_bounds("session-plan-details").unwrap();
        assert!(
            details.left() >= px(0.) && details.right() <= px(480.),
            "plan popup stays inside the viewport: {details:?}"
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds("session-plan-details").is_none());
    }
}
