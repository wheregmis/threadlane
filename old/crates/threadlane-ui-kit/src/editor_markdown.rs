//! Read-only document preview. Hosts retain the editor and own all file effects.
use gpui::{prelude::*, *};
use gpui_component::{ActiveTheme, WindowExt};
use gpui_kit::base::text::{TextView, TextViewState, TextViewStyle};

pub const MARKDOWN_PREVIEW_LIMIT: usize = 512 * 1024;
pub const PREVIEW_SELECTION_REASON: &str = "Switch to Source to add a code selection";
const SIZE_REASON: &str = "Preview unavailable for files larger than 512 KiB.";

actions!(editor_document, [ToggleMarkdownPreview]);

pub fn markdown_preview_eligible(path: &str) -> bool {
    !path.starts_with("diff:")
        && std::path::Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
            })
}

pub fn markdown_preview_size_allowed(text: &str) -> bool {
    text.len() <= MARKDOWN_PREVIEW_LIMIT
}

/// Do not reuse chat's relative-path navigation policy for documents.
pub fn markdown_preview_external_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        && !rest.is_empty()
        && !rest.starts_with(['/', '?', '#'])
        && !url
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '\\')
}

/// One instance per open document, never shared with Review or another checkout.
/// TextViewState owns parsing revisions, selection and its independent scroll state.
pub struct MarkdownPreview {
    active: bool,
    source_focus: FocusHandle,
    preview_focus: FocusHandle,
    state: Option<Entity<TextViewState>>,
    text: SharedString,
    error: Option<&'static str>,
}

impl MarkdownPreview {
    pub fn new(cx: &mut App) -> Self {
        Self {
            active: false,
            state: None,
            text: "".into(),
            error: None,
            source_focus: cx.focus_handle(),
            preview_focus: cx.focus_handle(),
        }
    }

    pub fn focus_control(&self, window: &mut Window, cx: &mut App) {
        window.focus(
            if self.active {
                &self.preview_focus
            } else {
                &self.source_focus
            },
            cx,
        );
    }

    pub fn control(&self, loading: bool, cx: &App) -> AnyElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .children([false, true].map(|preview| {
                let selected = preview == self.active;
                gpui_kit::base::Button::new(if preview {
                    "markdown-preview-mode"
                } else {
                    "markdown-source-mode"
                })
                .debug_selector(move || {
                    if preview {
                        "markdown-preview-mode".into()
                    } else {
                        "markdown-source-mode".into()
                    }
                })
                .track_focus(if preview {
                    &self.preview_focus
                } else {
                    &self.source_focus
                })
                .accessibility_label(if preview {
                    "Preview Markdown"
                } else {
                    "Markdown source"
                })
                .aria_toggled(if selected {
                    gpui::accesskit::Toggled::True
                } else {
                    gpui::accesskit::Toggled::False
                })
                .disabled(loading && preview)
                .selected(selected)
                .px_2()
                .py_1()
                .text_xs()
                .rounded_sm()
                .border_1()
                .border_color(theme.border)
                .bg(if selected {
                    theme.secondary
                } else {
                    theme.background
                })
                .text_color(if loading && preview {
                    theme.muted_foreground
                } else {
                    theme.foreground
                })
                .focus_visible(|style| style.border_color(theme.ring))
                .hover(|style| style.bg(theme.muted))
                .child(if preview { "Preview" } else { "Source" })
                .on_click(move |_, window, cx| {
                    if !selected {
                        window.dispatch_action(Box::new(ToggleMarkdownPreview), cx);
                    }
                })
            }))
            .into_any_element()
    }
    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn show_source(&mut self) {
        self.active = false;
    }

    pub fn toggle(&mut self, text: SharedString, cx: &mut App) {
        if self.active {
            self.show_source();
        } else {
            self.active = true;
            self.refresh(text, cx);
        }
    }

    /// Called only when the retained source buffer changes, or on entry.
    pub fn refresh(&mut self, text: SharedString, cx: &mut App) {
        if !self.active {
            if markdown_preview_size_allowed(&text) {
                self.error = None;
            }
            return;
        }
        if !markdown_preview_size_allowed(&text) {
            self.active = false;
            self.error = Some(SIZE_REASON);
            return;
        }
        self.error = None;
        if self.state.is_some() && self.text == text {
            return;
        }
        self.text = text;
        if let Some(state) = &self.state {
            state.update(cx, |state, cx| state.set_text(&self.text, cx));
        } else {
            self.state = Some(cx.new(|cx| TextViewState::markdown(&self.text, cx)));
        }
    }

    pub fn notice(&self, dirty: bool) -> Option<&'static str> {
        self.error.or(if self.active {
            Some(if dirty {
                "Preview · Unsaved changes"
            } else {
                "Preview · Read-only"
            })
        } else {
            None
        })
    }

    pub fn body(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let style = TextViewStyle::default()
            .with_foreground(theme.foreground)
            .with_muted_foreground(theme.muted_foreground)
            .with_link(theme.link)
            .with_selection(theme.selection)
            .with_code_background(theme.muted)
            .with_border(theme.border)
            .with_dark(theme.is_dark())
            .with_table_head(
                StyleRefinement::default()
                    .bg(theme.table_head)
                    .text_color(theme.table_head_foreground),
            )
            .with_inline_code(HighlightStyle {
                background_color: Some(theme.accent),
                ..Default::default()
            })
            .with_heading(|level| {
                StyleRefinement::default()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(rems(match level {
                        1 => 1.5,
                        2 => 1.25,
                        _ => 1.0,
                    }))
            });
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Images aren't loaded in preview"),
            )
            .child(if self.text.is_empty() {
                div().p_3().child("This file is empty.").into_any_element()
            } else if let Some(state) = &self.state {
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(
                        TextView::new(state)
                            .style(style)
                            .selectable(true)
                            .scrollable(true)
                            .image_source(markdown_preview_image)
                            .on_link_click(|url, _, window, cx| {
                                activate_preview_link(url, window, cx)
                            }),
                    )
                    .into_any_element()
            } else {
                div().p_3().child("Loading preview…").into_any_element()
            })
            .into_any_element()
    }
}

/// An in-memory bundled image, not a URI. Base uses this authoritative resolver
/// for both measurement and painting, with no fallback even for data URLs.
pub fn markdown_preview_image(_: &SharedUri) -> ImageSource {
    static IMAGE: std::sync::OnceLock<std::sync::Arc<Image>> = std::sync::OnceLock::new();
    ImageSource::Image(
        IMAGE
            .get_or_init(|| {
                std::sync::Arc::new(Image::from_bytes(
                    ImageFormat::Png,
                    include_bytes!("markdown-placeholder.png").to_vec(),
                ))
            })
            .clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::{markdown_preview_image, MarkdownPreview, MARKDOWN_PREVIEW_LIMIT};
    use gpui::{ImageSource, TestAppContext};

    #[test]
    fn markdown_preview_images_never_return_document_uris() {
        for uri in [
            "https://example.com/image.png",
            "file:///etc/passwd",
            "data:image/svg+xml,<svg/>",
            "../image.png",
        ] {
            assert!(matches!(
                markdown_preview_image(&uri.into()),
                ImageSource::Image(_)
            ));
        }
    }

    #[gpui::test]
    fn markdown_preview_retains_state_and_repeated_edits(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let mut preview = MarkdownPreview::new(cx);
            assert!(!preview.is_active());
            preview.toggle("# First edit".into(), cx);
            let id = preview.state.as_ref().unwrap().entity_id();
            preview.refresh("# Second edit".into(), cx);
            assert_eq!(preview.text.as_ref(), "# Second edit");
            preview.show_source();
            preview.toggle("# Second edit".into(), cx);
            assert_eq!(preview.state.as_ref().unwrap().entity_id(), id);
            preview.refresh("x".repeat(MARKDOWN_PREVIEW_LIMIT + 1).into(), cx);
            assert!(!preview.is_active());
            assert!(preview.notice(false).unwrap().contains("512 KiB"));
            preview.refresh("x".repeat(MARKDOWN_PREVIEW_LIMIT + 1).into(), cx);
            assert!(preview.notice(false).is_some());
            preview.refresh("é".repeat(MARKDOWN_PREVIEW_LIMIT / 2).into(), cx);
            assert!(!preview.is_active());
            assert!(preview.notice(false).is_none());
            assert_eq!(preview.state.as_ref().unwrap().entity_id(), id);
            assert_eq!(preview.text.as_ref(), "# Second edit");
            preview.toggle("".into(), cx);
            assert!(preview.is_active());
            assert!(preview.text.is_empty());
        });
    }
}

/// Shared loading/read-error presentation; document chrome remains outside it.
pub fn markdown_document_message(message: &str, cx: &App) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .p_3()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(message.to_owned())
}

/// Persistent mode/availability feedback, independent of transient save status.
pub fn markdown_preview_notice(message: &str, cx: &App) -> Div {
    div()
        .flex_none()
        .min_w_0()
        .px_3()
        .py_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(message.to_owned())
}

#[cfg(test)]
mod resource_tests {
    use super::MarkdownPreview;
    use gpui::{AppContext, Context, IntoElement, Render, TestAppContext, Window};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct Document(MarkdownPreview);
    impl Render for Document {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.body(cx)
        }
    }

    #[gpui::test]
    fn markdown_preview_renders_untrusted_resources_without_fetching(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        cx.update(|cx| {
            cx.set_http_client(gpui::http_client::FakeHttpClient::create(move |_| {
                count.fetch_add(1, Ordering::SeqCst);
                async {
                    Ok(gpui::http_client::Response::builder()
                        .status(404)
                        .body(Default::default())
                        .unwrap())
                }
            }))
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let document = cx.new(|cx| {
                let mut preview = MarkdownPreview::new(cx);
                preview.toggle(
                    r#"# Untrusted document

![remote](https://example.invalid/image.png)
![local](file:///does-not-exist/image.png)
![data](data:image/svg+xml,<svg/>)
[![linked](https://example.invalid/linked.png)](https://example.invalid)
<img src="https://example.invalid/raw.png">
<script>alert('not executable')</script>

- [ ] Never write back

[blocked](javascript:alert(1)) [relative](../secret) [fragment](#section)

```sh
false # display only
```
"#
                    .into(),
                    cx,
                );
                Document(preview)
            });
            gpui_component::Root::new(document, window, cx)
        });
        for _ in 0..4 {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        let document = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Document>().unwrap()
        });
        cx.update(|_window, cx| {
            document.update(cx, |document, cx| {
                let state = document.0.state.as_ref().unwrap();
                state.update(cx, |state, cx| state.select_all(cx));
                assert!(state
                    .read(cx)
                    .selected_text()
                    .contains("Untrusted document"));
            })
        });
        assert!(cx.opened_url().is_none());
        cx.update(|window, cx| super::activate_preview_link("file:///etc/passwd", window, cx));
        assert!(cx.opened_url().is_none());
        cx.update(|window, cx| {
            super::activate_preview_link("https://example.com/explicit", window, cx)
        });
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("https://example.com/explicit")
        );
    }
}

fn activate_preview_link(url: &str, window: &mut Window, cx: &mut App) {
    if markdown_preview_external_url(url) {
        cx.open_url(url);
    } else {
        window.push_notification("Only HTTP(S) links can be opened from preview. Local, relative and fragment links are unavailable.", cx);
    }
}
