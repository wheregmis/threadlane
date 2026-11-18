use crate::{
    editor_excerpt_block_reason, editor_selection_snapshot, format_editor_excerpt, safe_fence,
    EditorSelectionSnapshot, EDITOR_EXCERPT_LIMIT,
};
use gpui::{AppContext, TestAppContext};

#[test]
fn editor_language_detection_matches_enabled_native_grammars() {
    use gpui_component::highlighter::Language;
    for (file, language) in [
        ("src/Component.TSX", "tsx"),
        ("src/main.go", "go"),
        ("source.c", "c"),
        ("include/header.hpp", "cpp"),
        ("build.zig", "zig"),
        ("Main.java", "java"),
        ("Gemfile", "ruby"),
        ("db/query.sql", "sql"),
        ("Makefile", "make"),
        ("CMakeLists.txt", "cmake"),
        ("types.pyi", "python"),
        (".zshrc", "bash"),
    ] {
        assert_eq!(crate::tool_preview::detect_language(file), language);
        assert_eq!(Language::from_str(language).name(), language);
    }
    assert_eq!(crate::tool_preview::detect_language("unknown.filetype"), "text");
}

#[test]
fn reopen_control_describes_identity_availability_and_saved_file_policy() {
    let mut control = crate::ReopenClosedFileControl::default();
    assert_eq!(control.description(), "No closed files in this window.");
    control = control.with_target(Some("/worktrees/第二 / src/a b.rs".into()));
    assert_eq!(control.description(), "Reopen /worktrees/第二 / src/a b.rs. Reopens the saved file; discarded edits are not restored.");
    control = control.with_loading(true);
    assert!(control.description().starts_with("Reopening /worktrees/第二 / src/a b.rs"));
}

fn snapshot(text: &str, range: std::ops::Range<usize>, start: usize, end: usize) -> EditorSelectionSnapshot {
    EditorSelectionSnapshot {
        text: text.to_string(),
        byte_range: range,
        start_line: start,
        end_line: end,
    }
}

#[test]
fn safe_fence_clears_the_longest_backtick_run() {
    assert_eq!(safe_fence("plain"), "```");
    assert_eq!(safe_fence("has ``` inside"), "````");
    assert_eq!(safe_fence("``x`` ````y````"), "`````");
}

#[test]
fn format_editor_excerpt_labels_saved_and_unsaved_buffers() {
    let saved = format_editor_excerpt(
        "src/config.rs",
        &snapshot("let a = 1;\nlet b = 2;", 12..25, 12, 18),
        false,
    );
    assert_eq!(
        saved,
        "File excerpt: src/config.rs · buffer lines 12–18 · Buffer snapshot\n```\nlet a = 1;\nlet b = 2;\n```"
    );
    let dirty = format_editor_excerpt(
        "src/config.rs",
        &snapshot("let a = 1;", 12..22, 12, 12),
        true,
    );
    assert_eq!(
        dirty,
        "File excerpt: src/config.rs · buffer line 12 · Unsaved buffer\n```\nlet a = 1;\n```"
    );
}

#[test]
fn format_editor_excerpt_grows_fence_past_interior_backticks() {
    let excerpt = format_editor_excerpt(
        "doc.md",
        &snapshot("```rust\nfn f() {}\n```", 0..19, 1, 3),
        false,
    );
    assert!(excerpt.contains("\n````\n```rust"));
    assert!(excerpt.ends_with("\n````"));
}

#[test]
fn editor_excerpt_block_reason_reports_empty_and_oversized() {
    assert_eq!(
        editor_excerpt_block_reason(None),
        Some("Select code in the file first")
    );
    let too_big = snapshot(&"x".repeat(EDITOR_EXCERPT_LIMIT + 1), 0..EDITOR_EXCERPT_LIMIT + 1, 1, 400);
    assert_eq!(
        editor_excerpt_block_reason(Some(&too_big)),
        Some("Select less code (maximum 32 KiB)")
    );
    let ok = snapshot("small", 0..5, 1, 1);
    assert_eq!(editor_excerpt_block_reason(Some(&ok)), None);
}

#[gpui::test]
fn editor_selection_snapshot_tracks_one_based_buffer_lines(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let text = "one\ntwo\nthree\nfour";
    let holder = std::rc::Rc::new(std::cell::RefCell::new(None));
    let capture = holder.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let editor = cx.new(|cx| {
            gpui_component::input::EditorState::new(window, cx).default_value(text)
        });
        *capture.borrow_mut() = Some(editor);
        gpui_component::Root::new(cx.new(|_| TestHost), window, cx)
    });
    let editor = holder.borrow_mut().take().unwrap();

    let snapshot_for = |range, cx: &mut gpui::VisualTestContext| {
        editor.update(cx, |editor, cx| editor.set_selected_range(range, cx));
        editor.read_with(cx, |editor, _| editor_selection_snapshot(editor))
    };

    // "two\nthree" spans buffer lines 2–3.
    let snapped = snapshot_for(4..13, cx).expect("selection");
    assert_eq!(snapped.text, "two\nthree");
    assert_eq!((snapped.start_line, snapped.end_line), (2, 3));

    // An exclusive end at the next line's first byte still names the
    // previous line.
    let snapped = snapshot_for(4..8, cx).expect("selection");
    assert_eq!(snapped.text, "two\n");
    assert_eq!((snapped.start_line, snapped.end_line), (2, 2));

    // A partial first line keeps its column offset, not the line start.
    let snapped = snapshot_for(5..9, cx).expect("selection");
    assert_eq!(snapped.text, "wo\nt");
    assert_eq!((snapped.start_line, snapped.end_line), (2, 3));

    // A caret alone is not a selection.
    assert!(snapshot_for(4..4, cx).is_none());
}

struct TestHost;
impl gpui::Render for TestHost {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div()
    }
}

#[test]
fn markdown_preview_eligibility_and_utf8_limit() {
    for path in ["README.md", "notes.MARKDOWN", "docs/Plan.Md"] {
        assert!(crate::markdown_preview_eligible(path));
    }
    for path in ["note.mdx", "main.rs", "README", "diff:README.md"] {
        assert!(!crate::markdown_preview_eligible(path));
    }
    assert!(crate::markdown_preview_size_allowed(
        &"a".repeat(512 * 1024)
    ));
    assert!(!crate::markdown_preview_size_allowed(
        &"a".repeat(512 * 1024 + 1)
    ));
    assert!(!crate::markdown_preview_size_allowed(
        &"é".repeat(256 * 1024 + 1)
    ));
}

#[test]
fn markdown_preview_external_links_are_explicit_http_only() {
    for url in [
        "https://example.com/doc#part",
        "http://localhost:3000",
        "HTTPS://example.com",
    ] {
        assert!(crate::markdown_preview_external_url(url));
    }
    for url in [
        "file:///etc/passwd",
        "data:image/png;base64,AA",
        "javascript:alert(1)",
        "mailto:a@b.com",
        "../README.md",
        "#part",
        "//example.com",
        "https:",
        "https:///etc/passwd",
        "https://example.com\n",
    ] {
        assert!(!crate::markdown_preview_external_url(url), "{url}");
    }
}
