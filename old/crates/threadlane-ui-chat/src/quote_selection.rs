//! Quote-selection helpers: which assistant segment may be quoted, and how
//! the quote is formatted into ordinary draft text. Kept pure where possible
//! so eligibility and formatting are testable without a window.
use gpui::{App, Entity, SharedString};
use gpui_component::text::TextViewState;

/// Maximum quote length in Unicode scalar values. Over-limit selections are
/// rejected wholesale, never truncated mid-selection.
pub const MAX_QUOTE_SCALARS: usize = 4_000;

pub const QUOTE_INTRO: &str = "Quoted from assistant response:";

pub const NO_SELECTION_MESSAGE: &str =
    "Select text within one response paragraph or code block";
pub const OVER_LIMIT_MESSAGE: &str = "Select 4,000 characters or fewer";
pub const STALE_SELECTION_MESSAGE: &str = "Selection changed. Select the text again.";

/// Why a selection cannot be quoted right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuoteRejection {
    /// Nothing (or only whitespace) is selected in an eligible segment.
    NoSelection,
    /// The selection crosses segments, messages, or non-content views.
    MixedSelection,
    /// The selection exceeds `MAX_QUOTE_SCALARS`.
    OverLimit,
}

impl QuoteRejection {
    pub fn message(self) -> &'static str {
        match self {
            QuoteRejection::NoSelection => NO_SELECTION_MESSAGE,
            QuoteRejection::MixedSelection => NO_SELECTION_MESSAGE,
            QuoteRejection::OverLimit => OVER_LIMIT_MESSAGE,
        }
    }
}

/// Whether the message's action controls can quote right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuoteEligibility {
    /// `entity` is the single content view holding the selection.
    Eligible(Entity<TextViewState>),
    Rejected(QuoteRejection),
}

/// Finds the one assistant content view that owns the current selection.
///
/// `segment_states` are the prose/code `TextViewState` entities of a single
/// assistant message. `window_selection` is the whole window's selected text
/// (`TextSelection::selected_text`): when it differs from the owner view's
/// own selection, the selection extends outside this message's content and
/// is rejected rather than silently quoting only part of it.
pub fn quote_owner(
    segment_states: &[Entity<TextViewState>],
    window_selection: &str,
    cx: &App,
) -> QuoteEligibility {
    let selected: Vec<(Entity<TextViewState>, String)> = segment_states
        .iter()
        .filter_map(|state| {
            let text = state.read(cx).selected_text();
            (!text.trim().is_empty()).then(|| (state.clone(), text))
        })
        .collect();
    let Some((owner, text)) = selected.first() else {
        return QuoteEligibility::Rejected(QuoteRejection::NoSelection);
    };
    if selected.len() != 1 || *text != window_selection {
        return QuoteEligibility::Rejected(QuoteRejection::MixedSelection);
    }
    if quoted_text(text).chars().count() > MAX_QUOTE_SCALARS {
        return QuoteEligibility::Rejected(QuoteRejection::OverLimit);
    }
    QuoteEligibility::Eligible(owner.clone())
}

/// The part of a selection that is quoted and counted against the cap.
/// Leading and trailing blank lines are dropped — a fully selected segment
/// reports its rendered trailing newline — while interior blank lines,
/// indentation and every other character are kept.
pub fn quoted_text(selected: &str) -> &str {
    let start = selected
        .split_inclusive('\n')
        .take_while(|line| line.trim().is_empty())
        .map(str::len)
        .sum::<usize>();
    let rest = &selected[start..];
    let content_end = rest.trim_end().len();
    let line_end = rest[content_end..]
        .find(['\r', '\n'])
        .map_or(rest.len(), |offset| content_end + offset);
    &rest[..line_end]
}

/// Formats selected display text as a labeled Markdown blockquote followed by
/// a blank line for the user's follow-up. Every line of [`quoted_text`] is
/// prefixed with `> `; all other characters — indentation, interior blank
/// lines, `>` and backticks, tabs, Unicode — are preserved verbatim.
pub fn format_quote_block(selected: &str) -> String {
    let selected = quoted_text(selected);
    let mut quote = String::with_capacity(selected.len() + QUOTE_INTRO.len() + 4);
    quote.push_str(QUOTE_INTRO);
    quote.push('\n');
    for line in selected.split('\n') {
        quote.push_str("> ");
        quote.push_str(line);
        quote.push('\n');
    }
    quote.push('\n');
    quote
}

/// A selection captured before focus transferred to the control that will
/// consume it. `state` is weak so a replaced/unmounted source drops away.
#[derive(Clone)]
pub struct QuoteSnapshot {
    pub state: gpui::WeakEntity<TextViewState>,
    pub text: String,
}

/// Render-time state for a message's Quote controls.
#[derive(Clone)]
pub struct QuoteControl {
    pub enabled: bool,
    /// Readable reason when disabled ("" when enabled).
    pub reason: SharedString,
    pub snapshot: Option<QuoteSnapshot>,
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use gpui_component::text::TextViewState;

    use super::{
        format_quote_block, quote_owner, quoted_text, QuoteEligibility, QuoteRejection,
        MAX_QUOTE_SCALARS, NO_SELECTION_MESSAGE, OVER_LIMIT_MESSAGE,
    };

    #[test]
    fn quoted_text_drops_only_outer_blank_lines() {
        assert_eq!(quoted_text("hello\n"), "hello");
        assert_eq!(
            quoted_text("\n \n  indented\n\nnext\n\t\n"),
            "  indented\n\nnext"
        );
        assert_eq!(quoted_text("crlf\r\n\r\n"), "crlf");
        assert_eq!(
            quoted_text("keeps trailing spaces  \n"),
            "keeps trailing spaces  "
        );
        assert_eq!(quoted_text(" \n\t"), "");
    }

    #[test]
    fn format_wraps_a_single_line() {
        assert_eq!(
            format_quote_block("Completed response"),
            "Quoted from assistant response:\n> Completed response\n\n"
        );
    }

    #[test]
    fn format_quotes_every_line_and_keeps_blank_lines() {
        assert_eq!(
            format_quote_block("first\n\nsecond"),
            "Quoted from assistant response:\n> first\n> \n> second\n\n"
        );
    }

    #[test]
    fn format_preserves_markdown_and_whitespace_verbatim() {
        let selected = "> already quoted\n`code` and ```fence\n\tindented\ttab";
        assert_eq!(
            format_quote_block(selected),
            "Quoted from assistant response:\n> > already quoted\n> `code` and ```fence\n> \tindented\ttab\n\n"
        );
    }

    #[test]
    fn format_keeps_crlf_carriage_returns_and_unicode() {
        let selected = "emoji \u{1f680} \u{4e2d}\u{6587}\r\nnext";
        assert_eq!(
            format_quote_block(selected),
            "Quoted from assistant response:\n> emoji \u{1f680} \u{4e2d}\u{6587}\r\n> next\n\n"
        );
    }

    #[test]
    fn rejection_messages_match_the_spec() {
        assert_eq!(
            QuoteRejection::NoSelection.message(),
            NO_SELECTION_MESSAGE
        );
        assert_eq!(
            QuoteRejection::MixedSelection.message(),
            "Select text within one response paragraph or code block"
        );
        assert_eq!(
            QuoteRejection::OverLimit.message(),
            OVER_LIMIT_MESSAGE
        );
    }

    fn selected_state(
        text: &str,
        cx: &mut gpui::TestAppContext,
    ) -> gpui::Entity<TextViewState> {
        let state = cx.new(|cx| TextViewState::markdown(text, cx));
        state.update(cx, |state, cx| state.select_all(cx));
        state
    }


    #[gpui::test]
    fn owner_rejects_when_nothing_is_selected(cx: &mut gpui::TestAppContext) {
        let state = cx.new(|cx| TextViewState::markdown("hello", cx));
        let eligibility = cx.update(|cx| quote_owner(&[state], "", cx));
        assert_eq!(eligibility, QuoteEligibility::Rejected(QuoteRejection::NoSelection));
    }

    #[gpui::test]
    fn owner_returns_the_single_selected_segment(cx: &mut gpui::TestAppContext) {
        let unselected = cx.new(|cx| TextViewState::markdown("other", cx));
        let selected = selected_state("hello", cx);
        match cx.update(|cx| quote_owner(&[unselected, selected.clone()], "hello\n", cx)) {
            QuoteEligibility::Eligible(owner) => assert_eq!(owner, selected),
            other => panic!("expected eligibility, got {other:?}"),
        }
    }

    #[gpui::test]
    fn owner_rejects_selections_spanning_segments(cx: &mut gpui::TestAppContext) {
        let first = selected_state("hello", cx);
        let second = selected_state("world", cx);
        let eligibility =
            cx.update(|cx| quote_owner(&[first, second], "hello\nworld\n", cx));
        assert_eq!(eligibility, QuoteEligibility::Rejected(QuoteRejection::MixedSelection));
    }

    #[gpui::test]
    fn owner_rejects_when_selection_extends_beyond_the_segment(
        cx: &mut gpui::TestAppContext,
    ) {
        let selected = selected_state("hello", cx);
        let eligibility =
            cx.update(|cx| quote_owner(&[selected], "hello\n plus chrome text", cx));
        assert_eq!(eligibility, QuoteEligibility::Rejected(QuoteRejection::MixedSelection));
    }

    #[gpui::test]
    fn owner_rejects_over_limit_without_truncating(cx: &mut gpui::TestAppContext) {
        // `selected_text` carries the rendered document's trailing newline,
        // which is not part of the counted selection.
        let long = "x".repeat(MAX_QUOTE_SCALARS + 1);
        let selected = selected_state(&long, cx);
        let eligibility = cx.update(|cx| quote_owner(&[selected], &format!("{long}\n"), cx));
        assert_eq!(eligibility, QuoteEligibility::Rejected(QuoteRejection::OverLimit));
    }

    #[gpui::test]
    fn owner_accepts_exactly_at_the_limit(cx: &mut gpui::TestAppContext) {
        let exact = "x".repeat(MAX_QUOTE_SCALARS - 1);
        let selected = selected_state(&exact, cx);
        match cx.update(|cx| quote_owner(&[selected], &format!("{exact}\n"), cx)) {
            QuoteEligibility::Eligible(_) => {}
            other => panic!("expected eligibility at the cap, got {other:?}"),
        }
    }

    #[gpui::test]
    fn format_drops_the_trailing_newline_of_a_full_segment_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        let state = selected_state("Completed response", cx);
        let selected = cx.update(|cx| state.read(cx).selected_text());
        assert_eq!(
            format_quote_block(&selected),
            "Quoted from assistant response:\n> Completed response\n\n"
        );
    }

    #[gpui::test]
    fn owner_accepts_a_full_segment_of_exactly_the_limit(cx: &mut gpui::TestAppContext) {
        let exact = "x".repeat(MAX_QUOTE_SCALARS);
        let state = selected_state(&exact, cx);
        let selected = cx.update(|cx| state.read(cx).selected_text());
        match cx.update(|cx| quote_owner(&[state.clone()], &selected, cx)) {
            QuoteEligibility::Eligible(_) => {}
            other => panic!("the rendered trailing newline is not selected text, got {other:?}"),
        }
    }
}
