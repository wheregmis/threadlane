//! Offline word suggestions using GPUI Kit's completion menu, not a language server.
use std::collections::BTreeSet;

use gpui::{App, Task, Window};
use gpui_component::input::{CompletionProvider, Rope, RopeExt};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Range, TextEdit,
};

const MAX_BUFFER_BYTES: usize = 1024 * 1024;
const MAX_SUGGESTIONS: usize = 50;

pub(crate) struct BufferWords;

pub(crate) struct LanguageCompletion {
    pub primary: std::rc::Rc<dyn CompletionProvider>,
    pub words: bool,
    pub editor: gpui::WeakEntity<gpui_component::input::EditorState>,
}

impl CompletionProvider for LanguageCompletion {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: CompletionContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let primary = self.primary.completions(text, offset, trigger, window, cx);
        let text = text.clone();
        let words = self.words;
        let editor = self.editor.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |cx| match primary.await {
            Ok(result) => Ok(result),
            Err(error) if !words => Err(error),
            Err(_) => {
                let current = cx.update(|cx| {
                    editor
                        .upgrade()
                        .is_some_and(|editor| editor.read(cx).text() == &text)
                });
                if !current {
                    return Ok(CompletionResponse::Array(vec![]));
                }
                executor
                    .spawn(async move { Ok(CompletionResponse::Array(suggestions(&text, offset))) })
                    .await
            }
        })
    }

    fn is_completion_trigger(&self, offset: usize, new_text: &str, cx: &mut App) -> bool {
        self.primary.is_completion_trigger(offset, new_text, cx)
            || self.words && BufferWords.is_completion_trigger(offset, new_text, cx)
    }
}

fn is_word(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

fn suggestions(rope: &Rope, offset: usize) -> Vec<CompletionItem> {
    if rope.len() > MAX_BUFFER_BYTES {
        return Vec::new();
    }
    let text = rope.to_string();
    let Some(before) = text.get(..offset) else {
        return Vec::new();
    };
    let start = before
        .char_indices()
        .rev()
        .find(|(_, ch)| !is_word(*ch))
        .map_or(0, |(ix, ch)| ix + ch.len_utf8());
    let prefix = &text[start..offset];
    if prefix.chars().count() < 2 || prefix.starts_with(|ch: char| ch.is_numeric()) {
        return Vec::new();
    }
    let end = text[offset..]
        .char_indices()
        .find(|(_, ch)| !is_word(*ch))
        .map_or(text.len(), |(ix, _)| offset + ix);
    let current = &text[start..end];
    // GPUI Kit's positions are Unicode scalar columns, not LSP wire UTF-16 columns.
    let range = Range::new(rope.offset_to_position(start), rope.offset_to_position(end));
    text.split(|ch| !is_word(ch))
        .filter(|word| word.starts_with(prefix) && *word != current && word.len() <= 256)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|word| CompletionItem {
            label: word.to_owned(),
            kind: Some(CompletionItemKind::TEXT),
            detail: Some("Word in this file".into()),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                range,
                new_text: word.to_owned(),
            })),
            ..Default::default()
        })
        .collect()
}

impl CompletionProvider for BufferWords {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let text = text.clone();
        cx.background_executor()
            .spawn(async move { Ok(CompletionResponse::Array(suggestions(&text, offset))) })
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        let mut chars = new_text.chars();
        chars.next().is_some_and(is_word) && chars.next().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_BUFFER_BYTES, MAX_SUGGESTIONS, suggestions};
    use gpui_component::input::Rope;
    use lsp_types::{CompletionTextEdit, Position, Range};

    #[test]
    fn editor_completion_replaces_whole_identifier_and_deduplicates() {
        let text = "alpha_value alpha_value alphabet\nalp_suffix";
        let items = suggestions(&Rope::from(text), text.find("alp_suffix").unwrap() + 3);
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["alpha_value", "alphabet"]
        );
        let Some(CompletionTextEdit::Edit(edit)) = &items[0].text_edit else {
            panic!("missing edit")
        };
        assert_eq!(
            edit.range,
            Range::new(Position::new(1, 0), Position::new(1, 10))
        );
    }

    #[test]
    fn editor_completion_uses_kit_unicode_columns() {
        let text = "café_value\n😀 café";
        let items = suggestions(&Rope::from(text), text.len());
        let Some(CompletionTextEdit::Edit(edit)) = &items[0].text_edit else {
            panic!("missing edit")
        };
        assert_eq!(
            edit.range,
            Range::new(Position::new(1, 2), Position::new(1, 6))
        );
        assert_eq!(edit.new_text, "café_value");
        assert!(suggestions(&Rope::from(text), text.len() - 1).is_empty());
    }

    #[test]
    fn editor_completion_is_bounded_and_ignores_empty_numeric_and_short_prefixes() {
        for text in ["", "alpha a", "12 123 12", "alpha "] {
            assert!(suggestions(&Rope::from(text), text.len()).is_empty());
        }
        let text = "a".repeat(MAX_BUFFER_BYTES + 1);
        assert!(suggestions(&Rope::from(text.as_str()), text.len()).is_empty());
        let mut text = (0..100).map(|i| format!("word_{i} ")).collect::<String>();
        text.push_str("wo");
        assert_eq!(
            suggestions(&Rope::from(text.as_str()), text.len()).len(),
            MAX_SUGGESTIONS
        );
    }
}
