//! Pure helpers for Find in terminal output.
//!
//! The parser worker scans retained scrollback through vt100's own scrollback
//! view: `set_scrollback` accepts deep offsets, and `rows()`/`row_wrapped()`
//! then expose that window of retained rows. This module walks the full
//! retained buffer in `rows`-sized windows, joins soft-wrapped rows into
//! logical lines, and reports matching logical lines as bounded descriptors
//! (absolute row + excerpt) — never raw buffer snapshots.

/// One matching logical line: `absolute_row` indexes the retained buffer as
/// `scrollback[0..len]` followed by live rows `len..len + rows`, and `excerpt`
/// is a bounded excerpt of the line text for display.
#[derive(Clone, Debug)]
pub(crate) struct TerminalSearchHit {
    pub absolute_row: usize,
    pub excerpt: String,
}

/// Bounded outcome of a full retained-buffer scan.
#[derive(Clone, Debug, Default)]
pub(crate) struct TerminalSearchOutcome {
    /// The NEWEST matching logical lines, capped at
    /// [`TERMINAL_SEARCH_MAX_HITS`]. Keeping the tail rather than the head
    /// means "reveal newest" and backward navigation always cover the most
    /// recent output; older matches beyond the cap are counted by `total`
    /// but not navigable.
    pub hits: Vec<TerminalSearchHit>,
    /// Total matching logical lines, including any beyond `hits` when the
    /// descriptor list was truncated.
    pub total: usize,
    /// Retained scrollback length at scan time. Hits keep this so callers can
    /// reinterpret `absolute_row` against a later buffer shape.
    pub scrollback_len: usize,
    pub truncated: bool,
}

/// Maximum retained match descriptors per scan (the newest ones).
/// Navigation happens inside this list; `total` still counts every matching
/// line.
pub(crate) const TERMINAL_SEARCH_MAX_HITS: usize = 512;
/// Excerpts are capped so a match inside a huge line stays displayable.
const TERMINAL_SEARCH_EXCERPT_CHARS: usize = 160;

/// Joined logical line: `row` is the absolute index of its first physical
/// row and `text` concatenates the row contents across soft wraps.
struct LogicalLine {
    row: usize,
    text: String,
}

/// Splits absolute row indexes `0..len` into logical lines: a physical row
/// that `wrapped()` continues into the next row belongs to the same line.
fn logical_lines(texts: &[String], wraps: &[bool]) -> Vec<LogicalLine> {
    let mut lines = Vec::new();
    let mut index = 0;
    while index < texts.len() {
        let start = index;
        let mut text = texts[start].clone();
        while wraps.get(index).copied().unwrap_or(false) && index + 1 < texts.len() {
            index += 1;
            text.push_str(&texts[index]);
        }
        index += 1;
        lines.push(LogicalLine { row: start, text });
    }
    lines
}

/// Case-sensitive literal line match. A line counts once no matter how many
/// times the query occurs inside it.
fn line_matches(text: &str, query: &str) -> bool {
    !query.is_empty() && text.contains(query)
}

/// Bounded excerpt around the first occurrence: keeps a little leading
/// context, then the match and what fits after it.
fn line_excerpt(text: &str, query: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= TERMINAL_SEARCH_EXCERPT_CHARS {
        return trimmed.to_string();
    }
    let match_start = trimmed.find(query).unwrap_or(0);
    let mut start = match_start.saturating_sub(TERMINAL_SEARCH_EXCERPT_CHARS / 4);
    while !trimmed.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + TERMINAL_SEARCH_EXCERPT_CHARS).min(trimmed.len());
    while end < trimmed.len() && !trimmed.is_char_boundary(end) {
        end += 1;
    }
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if end < trimmed.len() { "…" } else { "" };
    format!("{prefix}{}{suffix}", &trimmed[start..end])
}

/// Scans the whole retained buffer (scrollback + live rows) for `query`,
/// restoring the caller's scrollback offset when done. Matching is over
/// logical lines so a match inside a soft wrap still reveals the line start.
///
/// This walks the buffer through `set_scrollback` windows instead of
/// `contents()` so the scan covers off-screen retained output without
/// materializing ANSI-formatted state.
pub(crate) fn scan_retained_output(
    screen: &mut vt100::Screen,
    rows: u16,
    cols: u16,
    query: &str,
) -> TerminalSearchOutcome {
    let saved_offset = screen.scrollback();
    screen.set_scrollback(usize::MAX);
    let scrollback_len = screen.scrollback();

    let capacity = scrollback_len + usize::from(rows);
    let mut texts: Vec<String> = Vec::with_capacity(capacity);
    let mut wraps: Vec<bool> = Vec::with_capacity(capacity);

    // Scrollback chunks, oldest first: at offset `o` the view shows
    // scrollback[len - o .. len - o + min(o, rows)).
    let mut offset = scrollback_len;
    while offset > 0 {
        screen.set_scrollback(offset);
        let count = offset.min(usize::from(rows));
        let mut rows_iter = screen.rows(0, cols);
        for row in 0..count {
            texts.push(rows_iter.next().unwrap_or_default());
            wraps.push(screen.row_wrapped(row as u16));
        }
        offset -= count;
    }

    // Live rows.
    screen.set_scrollback(0);
    let mut rows_iter = screen.rows(0, cols);
    for row in 0..usize::from(rows) {
        texts.push(rows_iter.next().unwrap_or_default());
        wraps.push(screen.row_wrapped(row as u16));
    }
    drop(rows_iter);

    screen.set_scrollback(saved_offset);

    let mut hits = Vec::new();
    let mut total = 0;
    for line in logical_lines(&texts, &wraps) {
        if line_matches(&line.text, query) {
            total += 1;
            hits.push(TerminalSearchHit {
                absolute_row: line.row,
                excerpt: line_excerpt(&line.text, query),
            });
        }
    }
    // Keep the newest descriptors: a settled query reveals the newest match,
    // and backward navigation then covers the tail of the result set.
    let overflow = hits.len().saturating_sub(TERMINAL_SEARCH_MAX_HITS);
    if overflow > 0 {
        hits.drain(..overflow);
    }

    TerminalSearchOutcome {
        truncated: total > hits.len(),
        hits,
        total,
        scrollback_len,
    }
}

/// Scrollback offset that reveals `hit`'s first row at the top of the view:
/// retained rows reveal via a deep offset, live rows reveal at offset 0.
pub(crate) fn reveal_offset(hit: &TerminalSearchHit, scrollback_len: usize) -> usize {
    scrollback_len
        .saturating_sub(hit.absolute_row)
        .min(scrollback_len)
}

/// Visible row index a hit cue should paint on, given the current scrollback
/// length and view offset. Returns `None` when the hit is not on screen.
///
/// Scrollback and live rows share one linear buffer `0..len + rows`: at
/// offset `o` the view shows buffer rows `len - o .. len - o + rows`, so any
/// hit — retained or live at scan time — paints at
/// `absolute_row - len_now + offset`. Comparing against the CURRENT length
/// stays correct when appends push a previously-live hit into scrollback.
pub(crate) fn cue_row(
    hit: &TerminalSearchHit,
    scrollback_len: usize,
    offset: usize,
    rows: u16,
) -> Option<u16> {
    let visible = hit
        .absolute_row
        .checked_add(offset)?
        .checked_sub(scrollback_len)?;
    (visible < usize::from(rows)).then_some(visible as u16)
}

/// Index of the next/previous match with wraparound. A settled query with no
/// selection starts at the newest (last) matching line.
pub(crate) use threadlane_ui_kit::next_terminal_find_match as next_find_match;

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(input: &[u8], rows: u16, cols: u16) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, 10_000);
        parser.process(input);
        parser
    }

    #[test]
    fn logical_lines_join_soft_wraps_but_not_hard_newlines() {
        let texts: Vec<String> = ["hel", "lo world", "next"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let wraps = vec![true, false, false];
        let lines = logical_lines(&texts, &wraps);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].row, 0);
        assert_eq!(lines[0].text, "hello world");
        assert_eq!(lines[1].row, 2);
        assert_eq!(lines[1].text, "next");
    }

    #[test]
    fn matching_counts_once_per_line_and_wraps_navigation() {
        // Multiple occurrences in one line count once.
        let mut parser = feed(b"one two one\r\nplain", 4, 24);
        let outcome = scan_retained_output(parser.screen_mut(), 4, 24, "one");
        assert_eq!(outcome.total, 1);
        assert_eq!(outcome.hits.len(), 1);

        assert_eq!(next_find_match(None, 3, false), Some(2));
        assert_eq!(next_find_match(None, 3, true), Some(0));
        assert_eq!(next_find_match(Some(2), 3, false), Some(0));
        assert_eq!(next_find_match(Some(0), 3, true), Some(2));
        assert_eq!(next_find_match(Some(0), 0, false), None);
    }

    #[test]
    fn empty_query_matches_nothing() {
        let mut parser = feed(b"content", 4, 24);
        let outcome = scan_retained_output(parser.screen_mut(), 4, 24, "");
        assert_eq!(outcome.total, 0);
        assert!(outcome.hits.is_empty());
        assert!(!outcome.truncated);
    }

    #[test]
    fn scan_finds_matches_in_deep_scrollback() {
        // 42 logical lines on a 4-row screen push the first needle deep into
        // retained scrollback while the second stays live.
        let mut input = String::from("needle deep\r\n");
        for line in 0..40 {
            input.push_str(&format!("line {line}\r\n"));
        }
        input.push_str("needle live");
        let mut parser = feed(input.as_bytes(), 4, 32);

        let outcome = scan_retained_output(parser.screen_mut(), 4, 32, "needle");
        assert_eq!(outcome.total, 2);
        assert_eq!(outcome.scrollback_len, 38);
        assert_eq!(outcome.hits[0].absolute_row, 0);
        assert_eq!(outcome.hits[1].absolute_row, outcome.scrollback_len + 3);
    }

    #[test]
    fn scan_finds_match_inside_a_soft_wrapped_line() {
        let mut parser = feed(b"aaaaaaaa\r\nwrap needle", 2, 8);
        let outcome = scan_retained_output(parser.screen_mut(), 2, 8, "needle");
        assert_eq!(outcome.total, 1);
        // The match text lands on the wrap's continuation row, but the hit
        // anchors at the logical line's first row (live row 0 here).
        assert_eq!(outcome.hits[0].absolute_row, outcome.scrollback_len);
        assert_eq!(outcome.hits[0].excerpt, "wrap needle");
    }

    #[test]
    fn scan_ignores_ansi_styling_and_carriage_return_updates() {
        let mut parser = feed(
            b"\x1b[31mneedle\x1b[0m styled\r\r\nspin X\rspin needle",
            4,
            24,
        );
        let outcome = scan_retained_output(parser.screen_mut(), 4, 24, "needle");
        assert_eq!(outcome.total, 2);
        assert_eq!(outcome.hits[0].excerpt, "needle styled");
        assert_eq!(outcome.hits[1].excerpt, "spin needle");
    }

    #[test]
    fn scan_handles_unicode_and_wide_characters() {
        let mut parser = feed("héllo 世界 needle ✓\r\n".as_bytes(), 4, 24);
        let outcome = scan_retained_output(parser.screen_mut(), 4, 24, "needle");
        assert_eq!(outcome.total, 1);
        assert!(outcome.hits[0].excerpt.contains("needle"));
        let outcome = scan_retained_output(parser.screen_mut(), 4, 24, "世界");
        assert_eq!(outcome.total, 1);
    }

    #[test]
    fn scan_is_case_sensitive() {
        let mut parser = feed(b"Needle here", 4, 24);
        let outcome = scan_retained_output(parser.screen_mut(), 4, 24, "needle");
        assert_eq!(outcome.total, 0);
    }

    #[test]
    fn excerpt_stays_bounded_and_char_safe() {
        let long = format!("{}needle{}", "x".repeat(400), "y".repeat(400));
        let mut parser = feed(format!("{long}\r\n").as_bytes(), 4, 80);
        let outcome = scan_retained_output(parser.screen_mut(), 4, 80, "needle");
        let excerpt = &outcome.hits[0].excerpt;
        assert!(excerpt.chars().count() <= TERMINAL_SEARCH_EXCERPT_CHARS + 2);
        assert!(excerpt.contains("needle"));
        assert!(excerpt.starts_with('…') || !excerpt.starts_with('x'));
        assert!(excerpt.ends_with('…'));
    }

    #[test]
    fn repeated_identical_lines_all_report() {
        let mut parser = feed(b"same\r\nsame\r\nsame", 4, 24);
        let outcome = scan_retained_output(parser.screen_mut(), 4, 24, "same");
        assert_eq!(outcome.total, 3);
        assert_eq!(
            outcome
                .hits
                .iter()
                .map(|hit| hit.absolute_row)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn scan_preserves_the_callers_view_offset() {
        let mut parser = feed(b"top\r\nmid\r\nbot", 2, 8);
        parser.screen_mut().set_scrollback(1);
        let _ = scan_retained_output(parser.screen_mut(), 2, 8, "mid");
        assert_eq!(parser.screen().scrollback(), 1);
    }

    #[test]
    fn hit_descriptors_keep_the_newest_matches_past_the_cap() {
        let mut input = String::new();
        for line in 0..(TERMINAL_SEARCH_MAX_HITS + 10) {
            input.push_str(&format!("hit {line}\r\n"));
        }
        let mut parser = feed(input.as_bytes(), 4, 16);
        let outcome = scan_retained_output(parser.screen_mut(), 4, 16, "hit");
        assert_eq!(outcome.hits.len(), TERMINAL_SEARCH_MAX_HITS);
        assert!(outcome.truncated);
        assert_eq!(outcome.total, TERMINAL_SEARCH_MAX_HITS + 10);
        // The kept descriptors are the newest: the first kept hit is 10
        // matches in and the last hit is the final line.
        assert_eq!(outcome.hits.first().unwrap().absolute_row, 10);
        assert_eq!(
            outcome.hits.last().unwrap().absolute_row,
            TERMINAL_SEARCH_MAX_HITS + 10 - 1
        );
    }

    #[test]
    fn cue_row_maps_scrollback_and_live_hits() {
        let hit = TerminalSearchHit {
            absolute_row: 3,
            excerpt: String::new(),
        };
        // scrollback hit: painted at absolute_row - len + offset.
        assert_eq!(cue_row(&hit, 10, 8, 4), Some(1));
        assert_eq!(cue_row(&hit, 10, 5, 4), None);
        // live hit: painted at absolute_row - len + offset as well.
        let live = TerminalSearchHit {
            absolute_row: 11,
            excerpt: String::new(),
        };
        assert_eq!(cue_row(&live, 10, 0, 4), Some(1));
        assert_eq!(cue_row(&live, 10, 2, 4), Some(3));
        assert_eq!(cue_row(&live, 10, 3, 4), None);
        // When appends push a previously-live hit into scrollback, the same
        // absolute row maps against the grown length — not a stale scan-time
        // length — so it goes offscreen instead of cueing unrelated output.
        let hit_at_11 = TerminalSearchHit {
            absolute_row: 11,
            excerpt: String::new(),
        };
        assert_eq!(cue_row(&hit_at_11, 12, 0, 4), None);
        assert_eq!(cue_row(&hit_at_11, 12, 2, 4), Some(1));
    }

    #[test]
    fn reveal_offset_pins_scrollback_hits_and_leaves_live_at_tail() {
        let scrollback_hit = TerminalSearchHit {
            absolute_row: 4,
            excerpt: String::new(),
        };
        assert_eq!(reveal_offset(&scrollback_hit, 10), 6);
        let live_hit = TerminalSearchHit {
            absolute_row: 12,
            excerpt: String::new(),
        };
        assert_eq!(reveal_offset(&live_hit, 10), 0);
    }
}
