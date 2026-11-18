//! Portable diff highlighting for shared selectable text views.
use gpui::{App, Entity, HighlightStyle};
use gpui_component::{ActiveTheme, highlighter::HighlightTheme, text::TextViewState};
use gpui_kit::base::{TextView, text::CodeBlock};
use std::{cell::RefCell, ops::Range, sync::Arc};

type DiffHighlighter = dyn Fn(&CodeBlock) -> Vec<(Range<usize>, HighlightStyle)> + Send + Sync;

/// GPUI Kit's tree-sitter highlighter is a stub on WASM. Diff syntax needs only
/// line prefixes and Git object IDs, so both hosts use this same bounded pass
/// with the existing syntax-theme scopes and text-selection renderer.
pub fn diff_text_view(text: &Entity<TextViewState>, cx: &App) -> TextView {
    thread_local! {
        static HIGHLIGHTER: RefCell<Option<(Arc<HighlightTheme>, Arc<DiffHighlighter>)>> = RefCell::new(None);
    }
    let theme = cx.theme().highlight_theme.clone();
    let highlighter = HIGHLIGHTER.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((cached_theme, highlighter)) = cache.as_ref() {
            if Arc::ptr_eq(cached_theme, &theme) {
                return highlighter.clone();
            }
        }
        let colors = theme.clone();
        let highlighter: Arc<DiffHighlighter> = Arc::new(move |block| {
            if block.lang().as_deref() == Some("diff") {
                diff_highlights(&block.code(), &colors)
            } else {
                Vec::new()
            }
        });
        *cache = Some((theme, highlighter.clone()));
        highlighter
    });
    TextView::new(text)
        .selectable(true)
        .shared_code_block_highlighter(highlighter)
}

fn diff_highlights(text: &str, theme: &HighlightTheme) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut highlights = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let line = line.trim_end_matches(['\r', '\n']);
        let scope = if line.starts_with('+') {
            Some("string")
        } else if line.starts_with('-') {
            Some("keyword")
        } else if line.starts_with("@@") {
            Some("attribute")
        } else if line.starts_with("diff ") {
            Some("variable.builtin")
        } else {
            None
        };
        if let Some(style) = scope.and_then(|scope| theme.style(scope)) {
            highlights.push((start..start + line.len(), style));
        } else if let Some(hashes) = line
            .strip_prefix("index ")
            .or_else(|| line.strip_prefix("commit "))
        {
            let base = start + line.len() - hashes.len();
            let token = hashes.split_whitespace().next().unwrap_or("");
            let mut position = base;
            for hash in token.split("..") {
                if (7..=64).contains(&hash.len())
                    && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    if let Some(style) = theme.style("constant") {
                        highlights.push((position..position + hash.len(), style));
                    }
                }
                position += hash.len() + 2;
            }
        }
    }
    highlights
}

#[cfg(test)]
mod tests {
    use super::diff_highlights;
    use gpui_component::highlighter::HighlightTheme;

    #[test]
    fn portable_diff_colors_preserve_unicode_and_uncolored_context() {
        let theme = HighlightTheme::default_dark();
        let diff = "diff --git a/你好.rs b/你好.rs\r\nindex 8da44dfa..400778c1 100644\n--- a/你好.rs\n+++ b/你好.rs\n@@ -1 +1 @@\n-old café\n+new 🦀\n unchanged\n";
        let highlights = diff_highlights(diff, &theme);
        for (range, _) in &highlights {
            assert!(diff.is_char_boundary(range.start) && diff.is_char_boundary(range.end));
            assert!(!diff[range.clone()].contains('\n'));
            assert!(!diff[range.clone()].contains("unchanged"));
        }
        for (token, scope) in [
            ("8da44dfa", "constant"),
            ("400778c1", "constant"),
            ("-old café", "keyword"),
            ("+new 🦀", "string"),
        ] {
            let start = diff.find(token).unwrap();
            assert!(
                highlights
                    .iter()
                    .any(|(range, style)| range == &(start..start + token.len())
                        && Some(*style) == theme.style(scope))
            );
        }
        assert!(
            highlights
                .windows(2)
                .all(|pair| pair[0].0.end <= pair[1].0.start)
        );
    }
}
