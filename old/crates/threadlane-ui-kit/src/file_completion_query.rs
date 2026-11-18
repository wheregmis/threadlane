//! Caret-local file references shared by desktop and web composers.
use std::ops::Range;

/// Maximum rows offered by `@` file completion before the picker reports
/// further matches instead of listing everything.
pub const FILE_COMPLETION_RESULT_LIMIT: usize = 50;

/// A caret-local `@` trigger inside the composer text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileQueryTrigger {
    /// UTF-8 byte range of the `@query` token, `@` included. A selection
    /// replaces exactly this range and verifies the text still matches.
    pub range: Range<usize>,
    /// The text after `@` up to the caret.
    pub query: String,
}

fn is_file_trigger_boundary(character: char) -> bool {
    character.is_whitespace() || matches!(character, '(' | '[' | '{' | '"' | '\'')
}

/// Detect an `@`-prefixed file query ending at `caret`.
///
/// `@` only triggers at a word boundary — start of text, after whitespace, or
/// after an opening bracket/quote — so `user@example.com`, `https://x@y`, and
/// other interior `@` characters never open completion. The query runs from
/// `@` to the caret; a selection replaces exactly `range` after re-checking
/// the current text, never surrounding content.
pub fn active_file_query(text: &str, caret: usize) -> Option<FileQueryTrigger> {
    if caret > text.len() || !text.is_char_boundary(caret) {
        return None;
    }
    let at = text[..caret].rfind('@')?;
    let boundary_ok = at == 0
        || text[..at]
            .chars()
            .next_back()
            .is_some_and(is_file_trigger_boundary);
    if !boundary_ok {
        return None;
    }
    let query = &text[at + '@'.len_utf8()..caret];
    if query
        .chars()
        .any(|character| character.is_whitespace() || character == '@')
    {
        return None;
    }
    Some(FileQueryTrigger {
        range: at..caret,
        query: query.to_owned(),
    })
}

/// Rank a workspace-relative `path` against a `query`. Lower is better:
/// exact basename, basename prefix, basename substring, then path substring.
fn file_match_rank(query: &str, path: &str) -> Option<u8> {
    let path_lower = path.to_lowercase();
    let basename = path_lower.rsplit('/').next().unwrap_or(&path_lower);
    if basename == query {
        Some(0)
    } else if basename.starts_with(query) {
        Some(1)
    } else if basename.contains(query) {
        Some(2)
    } else if path_lower.contains(query) {
        Some(3)
    } else {
        None
    }
}

/// Match `query` against workspace-relative `paths`.
///
/// Matching is case-insensitive and the ordering deterministic: ranked by
/// match quality, then shorter paths, then lexicographically, so repeated
/// filenames are always disambiguated the same way. An empty `query` returns
/// the bounded initial list in lexical order. The `bool` reports that matches
/// beyond `limit` exist.
pub fn filter_file_matches<'a>(
    query: &str,
    paths: &'a [String],
    limit: usize,
) -> (Vec<&'a String>, bool) {
    let query_lower = query.to_lowercase();
    let mut ranked: Vec<(u8, &'a String)> = paths
        .iter()
        .filter_map(|path| file_match_rank(&query_lower, path).map(|rank| (rank, path)))
        .collect();
    ranked.sort_by(|(rank_a, path_a), (rank_b, path_b)| {
        rank_a
            .cmp(rank_b)
            .then_with(|| path_a.len().cmp(&path_b.len()))
            .then_with(|| path_a.cmp(path_b))
    });
    let has_more = ranked.len() > limit;
    (
        ranked
            .into_iter()
            .take(limit)
            .map(|(_, path)| path)
            .collect(),
        has_more,
    )
}

fn longest_backtick_run(text: &str) -> usize {
    text.chars()
        .fold((0usize, 0usize), |(longest, run), character| {
            if character == '`' {
                let run = run + 1;
                (longest.max(run), run)
            } else {
                (longest, 0)
            }
        })
        .0
}

/// Escape `text` as a Markdown code span that round-trips exactly, including
/// backticks and spaces inside the path.
pub fn markdown_code_span(text: &str) -> String {
    let ticks = "`".repeat(longest_backtick_run(text) + 1);
    if text.starts_with('`') || text.ends_with('`') {
        format!("{ticks} {text} {ticks}")
    } else {
        format!("{ticks}{text}{ticks}")
    }
}

/// The text inserted at the caret for a chosen workspace path: a Markdown
/// code span plus a trailing separator so typing continues outside the span.
pub fn format_path_insertion(path: &str) -> String {
    format!("{} ", markdown_code_span(path))
}

/// A Git-reported relative name must stay inside the workspace root: no
/// absolute paths, `..` climbs, or NUL bytes.
pub fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains('\0')
        && path.split('/').all(|component| component != "..")
}

#[cfg(test)]
mod tests {
    use super::{
        active_file_query, filter_file_matches, format_path_insertion, is_safe_relative_path,
    };

    #[test]
    fn file_query_triggers_at_word_boundaries() {
        for (text, expected_query) in [
            ("@", ""),
            ("@src/li", "src/li"),
            ("fix @mod", "mod"),
            ("fix @mod tail", "mod"),
            ("(@x", "x"),
            ("\"@x", "x"),
            ("uni @日本語", "日本語"),
        ] {
            let caret = text.find('@').unwrap() + 1 + expected_query.len();
            let trigger = active_file_query(text, caret).unwrap_or_else(|| {
                panic!("expected trigger for {text:?} at {caret}");
            });
            assert_eq!(trigger.query, expected_query, "{text:?}");
            assert_eq!(&text[trigger.range.clone()], format!("@{expected_query}"));
        }
    }

    #[test]
    fn file_query_rejects_interior_and_detached_ats() {
        for (text, caret) in [
            ("user@example.com", "user@ex".len()),
            ("https://a@b", "https://a@b".len()),
            ("a@", 2),
            ("mail me at x@y.z", "mail me at x@y".len()),
            ("@ spaced", "@ spaced".len()),
            ("after @gone tail", "after @gone".len() + 1),
        ] {
            assert_eq!(active_file_query(text, caret), None, "{text:?} at {caret}");
        }
        // Caret before the `@` and a caret beyond the token both miss.
        assert_eq!(active_file_query("@x", 0), None);
        assert_eq!(active_file_query("pre @x post", "pre @x".len() + 6), None);
    }

    #[test]
    fn file_query_replaces_only_through_the_caret() {
        let text = "see @hel.rs and more";
        let trigger = active_file_query(text, "see @hel".len()).unwrap();
        assert_eq!(trigger.query, "hel");
        assert_eq!(&text[trigger.range], "@hel");
    }

    #[test]
    fn file_matches_rank_basename_before_path_and_stay_deterministic() {
        let paths = vec![
            "src/deep/util.rs".to_string(),
            "src/util.rs".to_string(),
            "util.rs".to_string(),
            "docs/autils.md".to_string(),
            "unrelated.txt".to_string(),
        ];
        let (matches, has_more) = filter_file_matches("util", &paths, 50);
        assert!(!has_more);
        // Exact basename first, then prefix, then substring; shorter first.
        assert_eq!(
            matches.iter().map(|path| path.as_str()).collect::<Vec<_>>(),
            vec![
                "util.rs",
                "src/util.rs",
                "src/deep/util.rs",
                "docs/autils.md"
            ]
        );
    }

    #[test]
    fn file_matches_support_path_queries_caps_and_empty_queries() {
        let paths = vec![
            "src/app/main.rs".to_string(),
            "tests/main.rs".to_string(),
            "src/lib.rs".to_string(),
        ];
        let (matches, _) = filter_file_matches("src/", &paths, 50);
        assert_eq!(
            matches.iter().map(|path| path.as_str()).collect::<Vec<_>>(),
            vec!["src/lib.rs", "src/app/main.rs"]
        );
        let (capped, has_more) = filter_file_matches("main", &paths, 1);
        assert!(has_more);
        assert_eq!(capped.len(), 1);
        // Empty query offers the whole list (bounded) in lexical order.
        let (all, has_more) = filter_file_matches("", &paths, 50);
        assert!(!has_more);
        assert_eq!(
            all.iter().map(|path| path.as_str()).collect::<Vec<_>>(),
            vec!["src/lib.rs", "tests/main.rs", "src/app/main.rs"]
        );
    }

    #[test]
    fn code_spans_round_trip_special_paths() {
        assert_eq!(format_path_insertion("a/b.rs"), "`a/b.rs` ");
        assert_eq!(format_path_insertion("a file.rs"), "`a file.rs` ");
        assert_eq!(format_path_insertion("tick`file.rs"), "``tick`file.rs`` ");
        assert_eq!(format_path_insertion("`leading.rs"), "`` `leading.rs `` ");
        assert_eq!(format_path_insertion("日本 語.rs"), "`日本 語.rs` ");
    }

    #[test]
    fn unsafe_relative_paths_are_rejected() {
        for path in [
            "",
            "/abs.rs",
            "../escape.rs",
            "a/../../b.rs",
            "nul\0byte.rs",
        ] {
            assert!(!is_safe_relative_path(path), "{path:?}");
        }
        for path in ["a/b.rs", "deep/../lookalike", ".hidden", "a/./b.rs"] {
            // ".." inside a longer component name is fine; real climbs are not.
            let expected = !path.contains("/../") && !path.starts_with("../");
            assert_eq!(is_safe_relative_path(path), expected, "{path:?}");
        }
    }
}
