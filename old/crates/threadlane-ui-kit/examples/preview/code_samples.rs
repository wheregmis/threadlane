//! Labeled code samples rendered with the production conversation atoms.
use gpui::{prelude::*, *};
use gpui_component::{notification::Notification, WindowExt};
use std::collections::HashMap;
use threadlane_ui_kit as kit;

pub struct CodeSamples {
    markdown: HashMap<(SharedString, String), kit::markdown::MarkdownRenderState>,
    copied: Option<String>,
    copy_task: Option<Task<()>>,
    wrapped: std::collections::HashSet<String>,
}
impl CodeSamples {
    pub fn new() -> Self {
        Self {
            markdown: HashMap::new(),
            copied: None,
            copy_task: None,
            wrapped: std::collections::HashSet::new(),
        }
    }
}
impl Render for CodeSamples {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut samples = div().flex().flex_col().gap_3().min_w_0();
        for (key, language, path, code, streaming) in [
            (
                "sample-shell",
                "shell",
                "scripts/workspace/a-very-long-project-directory/check-components.sh",
                "cargo check -p threadlane-gpui\ngit diff --check",
                false,
            ),
            (
                "sample-rust",
                "rust",
                "crates/threadlane-ui-kit/src/conversation.rs",
                "pub fn render() {\n    println!(\"Shared on desktop and web\");\n}",
                false,
            ),
            (
                "sample-streaming",
                "rust",
                "crates/threadlane-ui-kit/src/conversation.rs",
                "pub fn render() {\n    // Still arriving…",
                true,
            ),
        ] {
            let wrapped = self.wrapped.contains(key);
            let actions = kit::code_block_actions()
                .child(
                    kit::code_block_wrap_button(key, wrapped).on_click(cx.listener(
                        move |host, _, _, cx| {
                            if !host.wrapped.remove(key) {
                                host.wrapped.insert(key.into());
                            }
                            cx.notify();
                        },
                    )),
                )
                .children((language == "shell" && !streaming).then(|| {
                    kit::code_block_run_button(key).on_click(|_, window, cx| {
                        window.push_notification(
                            Notification::info("Local preview: terminal commands are not executed"),
                            cx,
                        )
                    })
                }))
                .children((!streaming).then(|| {
                    kit::code_block_open_button(key).on_click(|_, window, cx| {
                        window.push_notification(
                            Notification::info("Local preview: project files are not opened"),
                            cx,
                        )
                    })
                }))
                .children((!streaming).then(|| {
                    kit::code_block_copy_button(key, self.copied.as_deref() == Some(key), cx)
                        .on_click(cx.listener(move |host, _, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(code.to_owned()));
                            host.copied = Some(key.into());
                            host.copy_task = Some(cx.spawn(async move |owner, cx| {
                                cx.background_executor()
                                    .timer(kit::MESSAGE_COPY_FEEDBACK_WINDOW)
                                    .await;
                                let _ = owner.update(cx, |host, cx| {
                                    host.copied = None;
                                    cx.notify();
                                });
                            }));
                            window.push_notification(
                                Notification::info("Code copied to clipboard"),
                                cx,
                            );
                            cx.notify();
                        }))
                }));
            let state = kit::markdown::markdown_state(
                &mut self.markdown,
                "code-samples".into(),
                key.into(),
                &format!("```{language}\n{code}\n```"),
                cx,
            );
            samples = samples
                .child(div().text_xs().child(if streaming {
                    "Streaming · actions unavailable until complete"
                } else {
                    "Completed · local sample"
                }))
                .child(
                    kit::code_block_surface(key, cx)
                        .child(kit::code_block_header(
                            key,
                            language,
                            Some(path),
                            actions,
                            cx,
                        ))
                        .child(kit::code_block_body(
                            key,
                            cx,
                            wrapped,
                            kit::markdown::markdown_view(&state, |_, _| {}),
                        )),
                );
        }
        samples
    }
}
