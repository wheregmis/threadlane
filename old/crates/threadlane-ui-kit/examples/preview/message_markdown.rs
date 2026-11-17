//! Captured Markdown uses the production code chrome; host actions remain local.
use super::SessionPreview;
use gpui::{prelude::*, *};
use gpui_component::{notification::Notification, WindowExt};
use threadlane_protocol::daemon::ChatMessageInfo;
use gpui_component::text::TextViewState;
use threadlane_ui_kit::{
    self as kit,
    markdown::{ChatLinkTarget, MarkdownSegment},
};

impl SessionPreview {
    pub(super) fn render_message_markdown(
        &mut self,
        message: &ChatMessageInfo,
        cx: &mut Context<Self>,
    ) -> (AnyElement, Vec<Entity<TextViewState>>) {
        let segments = kit::markdown::markdown_segments(
            &mut self.segment_cache,
            &message.id,
            &message.content,
        );
        let mut content_states = Vec::new();
        let mut body = kit::message_content_column();
        for (index, segment) in segments.into_iter().enumerate() {
            let key = format!("{}-{index}", message.id);
            let element = match segment {
                MarkdownSegment::Markdown(text) => {
                    let state = kit::markdown::markdown_state(
                        &mut self.markdown,
                        "saved-session-preview".into(),
                        format!("segment-{key}"),
                        &text,
                        cx,
                    );
                    content_states.push(state.clone());
                    kit::markdown::markdown_view(&state, |_, _| {}).into_any_element()
                }
                MarkdownSegment::CodeBlock {
                    language,
                    header_path,
                    code,
                } => {
                    let path = header_path.as_deref().and_then(|path| {
                        match kit::markdown::classify_chat_link(path) {
                            ChatLinkTarget::ProjectFile(path) => Some(path),
                            _ => None,
                        }
                    });
                    let wrapped = self
                        .code_wrap_blocks
                        .get(&message.id)
                        .is_some_and(|blocks| blocks.contains(&index));
                    // Shell availability is captured from the desktop host;
                    // browser environment variables cannot represent its shell.
                    let runnable = !message.streaming
                        && kit::markdown::is_terminal_runnable_language(&language)
                        && (self
                            .fixture
                            .runnable_code_languages
                            .iter()
                            .any(|supported| supported.eq_ignore_ascii_case(&language))
                            || matches!(language.as_str(), "shell" | "terminal" | "console"));
                    let actions = kit::code_block_actions()
                        .child({
                            let message_id = message.id.clone();
                            kit::code_block_wrap_button(&key, wrapped).on_click(cx.listener(
                                move |host, _, _, cx| {
                                    host.toggle_code_block_wrap(&message_id, index, cx);
                                },
                            ))
                        })
                        .children(runnable.then(|| {
                            kit::code_block_run_button(&key).on_click(|_, window, cx| {
                                Self::preview_notice("Run in Terminal", window, cx);
                            })
                        }))
                        .children(path.as_ref().filter(|_| !message.streaming).map(|_| {
                            kit::code_block_open_button(&key).on_click(|_, window, cx| {
                                Self::preview_notice("Open in Editor", window, cx);
                            })
                        }))
                        .children((!message.streaming).then(|| {
                            let copy_key = format!("copy-code-{key}");
                            let copied = self.copied_message.as_deref() == Some(copy_key.as_str());
                            let code = code.clone();
                            kit::code_block_copy_button(&key, copied, cx).on_click(cx.listener(
                                move |host, _, window, cx| {
                                    host.copy_text(copy_key.clone(), code.clone(), cx);
                                    window.push_notification(
                                        Notification::info("Code copied to clipboard"),
                                        cx,
                                    );
                                },
                            ))
                        }));
                    let formatted = format!("```{language}\n{}\n```", code.trim_end());
                    let state = kit::markdown::markdown_state(
                        &mut self.markdown,
                        "saved-session-preview".into(),
                        format!("code-{key}"),
                        &formatted,
                        cx,
                    );
                    content_states.push(state.clone());
                    kit::code_block_surface(&key, cx)
                        .child(kit::code_block_header(
                            &key,
                            &language,
                            path.as_deref(),
                            actions,
                            cx,
                        ))
                        .child(kit::code_block_body(
                            &key,
                            cx,
                            wrapped,
                            kit::markdown::markdown_view(&state, |_, _| {}),
                        ))
                        .into_any_element()
                }
            };
            body = body.child(element);
        }
        (body.into_any_element(), content_states)
    }

    /// Same per-block wrap toggle as the chat surface, including the
    /// targeted transcript-row remeasure so heights update in place.
    fn toggle_code_block_wrap(
        &mut self,
        message_id: &str,
        block_index: usize,
        cx: &mut Context<Self>,
    ) {
        let blocks = self
            .code_wrap_blocks
            .entry(message_id.to_string())
            .or_default();
        if !blocks.remove(&block_index) {
            blocks.insert(block_index);
        }
        if blocks.is_empty() {
            self.code_wrap_blocks.remove(message_id);
        }
        let row = self.transcript.rows.iter().position(|row| {
            matches!(row, kit::transcript::TranscriptRow::Message(index)
                if self.messages[*index].id == message_id)
        });
        if let Some(row) = row {
            self.transcript.list.remeasure_items(row..row + 1);
        }
        cx.notify();
    }
}
