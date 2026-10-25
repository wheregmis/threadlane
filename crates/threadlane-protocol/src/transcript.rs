//! GPUI-free transcript row projection and conversation matching over
//! [`ChatMessageInfo`]. Shared by the chat find strip, prompt recall,
//! and the daemon's saved-conversation scan; the interactive
//! [`TranscriptState`](gpui) wrapper stays in `threadlane-ui-kit`.
use std::ops::Range;

use crate::daemon::{ChatMessageInfo, MessageRole, ToolActivityInfo};

pub fn is_queued_message(message: &ChatMessageInfo, generating: bool) -> bool {
    generating && message.role == MessageRole::User && message.id.starts_with("queued-user-")
}

pub fn current_turn_latest_tool(messages: &[ChatMessageInfo]) -> Option<&ToolActivityInfo> {
    messages
        .iter()
        .rev()
        .take_while(|message| {
            // Optimistic queue/steer echoes do not start a new accepted turn.
            message.role != MessageRole::User
                || message.id.starts_with("queued-user-")
                || message.id.starts_with("steered-user-")
        })
        .flat_map(|message| message.tool_activities.iter().rev())
        .next()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptRow {
    Message(usize),
    Activities(Range<usize>),
    Working,
}

pub fn is_activity_only(message: &ChatMessageInfo) -> bool {
    message.role == MessageRole::Assistant
        && message.content.is_empty()
        && message.reasoning_content.is_none()
        && message
            .tool_activities
            .iter()
            .any(|activity| activity.title != "update_plan")
}

pub fn build_transcript_rows(messages: &[ChatMessageInfo], generating: bool) -> Vec<TranscriptRow> {
    let mut rows = Vec::with_capacity(messages.len().saturating_add(1));
    rows.extend(transcript_rows(messages, generating));
    rows
}

fn transcript_rows(
    messages: &[ChatMessageInfo],
    mut generating: bool,
) -> impl Iterator<Item = TranscriptRow> + '_ {
    let mut index = 0;
    std::iter::from_fn(move || {
        while index < messages.len() {
            // Pending follow-ups live above the composer, not in the accepted transcript.
            if is_queued_message(&messages[index], generating) {
                index += 1;
                continue;
            }
            if !is_activity_only(&messages[index]) {
                let row = TranscriptRow::Message(index);
                index += 1;
                return Some(row);
            }

            let start = index;
            while index < messages.len() && is_activity_only(&messages[index]) {
                index += 1;
            }
            return Some(TranscriptRow::Activities(start..index));
        }
        if generating {
            generating = false;
            Some(TranscriptRow::Working)
        } else {
            None
        }
    })
}

pub fn grouped_tool_activities(
    messages: &[ChatMessageInfo],
) -> impl Iterator<Item = &ToolActivityInfo> + Clone {
    messages
        .iter()
        .flat_map(|message| message.tool_activities.iter())
        .filter(|activity| activity.title != "update_plan")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationMatch {
    pub message_id: String,
    pub row_index: usize,
    pub excerpt: String,
}

/// Search the projected transcript, including unmounted rows, once per message.
pub fn find_conversation_messages(
    messages: &[ChatMessageInfo],
    generating: bool,
    query: &str,
) -> Vec<ConversationMatch> {
    conversation_matches(messages, generating, query).collect()
}

/// Return only the first hit without scanning or allocating excerpts for later messages.
pub fn first_conversation_match(
    messages: &[ChatMessageInfo],
    generating: bool,
    query: &str,
) -> Option<ConversationMatch> {
    conversation_matches(messages, generating, query).next()
}

fn conversation_matches<'a>(
    messages: &'a [ChatMessageInfo],
    generating: bool,
    query: &str,
) -> impl Iterator<Item = ConversationMatch> + 'a {
    let query = query.to_lowercase();
    let search_enabled = !query.trim().is_empty();
    transcript_rows(messages, generating)
        .take(if search_enabled { usize::MAX } else { 0 })
        .enumerate()
        .filter_map(move |(row_index, row)| {
            let TranscriptRow::Message(index) = row else {
                return None;
            };
            let message = &messages[index];
            if !matches!(message.role, MessageRole::User | MessageRole::Assistant)
                || message.content.is_empty()
            {
                return None;
            }
            let offset = message.content.to_lowercase().find(&query)?;
            Some(ConversationMatch {
                message_id: message.id.clone(),
                row_index,
                excerpt: matching_excerpt(&message.content, offset),
            })
        })
}

fn matching_excerpt(content: &str, offset: usize) -> String {
    // Lowercasing can expand a character (İ -> i + combining dot). Map the
    // folded byte offset back to source characters rather than slicing UTF-8.
    let mut folded_bytes = 0;
    let match_char = content
        .chars()
        .take_while(|ch| {
            if folded_bytes >= offset {
                return false;
            }
            folded_bytes += ch.to_lowercase().map(char::len_utf8).sum::<usize>();
            true
        })
        .count();
    let start = match_char.saturating_sub(40);
    let mut chars = content.chars().skip(start);
    let mut excerpt = String::new();
    if start > 0 {
        excerpt.push('…');
    }
    // Keep source excerpts compact even for code with many blank lines.
    excerpt.extend(
        chars
            .by_ref()
            .take(160)
            .map(|ch| if ch.is_whitespace() { ' ' } else { ch }),
    );
    if chars.next().is_some() {
        excerpt.push('…');
    }
    excerpt
}

pub fn next_find_match(selected: Option<usize>, count: usize, previous: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match (selected, previous) {
        (Some(index), true) => (index + count - 1) % count,
        (Some(index), false) => (index + 1) % count,
        (None, true) => count - 1,
        (None, false) => 0,
    })
}

#[cfg(test)]
mod search_tests {
    use super::{
        TranscriptRow, build_transcript_rows, find_conversation_messages, first_conversation_match,
        transcript_rows,
    };
    use crate::daemon::{ChatMessageInfo, MessageRole, ToolActivityInfo};

    fn message(id: &str, role: MessageRole, content: &str) -> ChatMessageInfo {
        ChatMessageInfo {
            id: id.into(),
            role,
            content: content.into(),
            tool_activities: Vec::new(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
            retry_prompt: None,
        }
    }

    #[test]
    fn first_conversation_hit_preserves_find_semantics() {
        let messages = vec![
            message("queued-user-1", MessageRole::User, "needle"),
            message("user-1", MessageRole::User, "no match"),
            message("assistant-1", MessageRole::Assistant, "İstanbul NEEDLE"),
            message("user-2", MessageRole::User, "another needle"),
        ];
        let all = find_conversation_messages(&messages, true, "NEEDLE");
        assert_eq!(all.len(), 2);
        let first = first_conversation_match(&messages, true, "NEEDLE").unwrap();
        assert_eq!(first, all[0]);
        assert_eq!(first.row_index, 1);
        assert_eq!(first.message_id, "assistant-1");
        assert_eq!(first.excerpt, "İstanbul NEEDLE");
        for query in ["", "  ", "missing"] {
            assert!(first_conversation_match(&messages, true, query).is_none());
            assert!(find_conversation_messages(&messages, true, query).is_empty());
        }
    }

    #[test]
    fn lazy_transcript_rows_preserve_groups_queue_and_working_row() {
        let mut activity = message("tool", MessageRole::Assistant, "");
        activity.tool_activities.push(ToolActivityInfo {
            title: "read_file".into(),
            id: "call".into(),
            category: "Loaded".into(),
            display_summary: String::new(),
            detail: String::new(),
            arguments: String::new(),
            is_expanded: false,
        });
        let messages = vec![
            message("queued-user-1", MessageRole::User, "needle"),
            activity.clone(),
            activity,
            message("user-1", MessageRole::User, "needle"),
        ];
        let expected = vec![
            TranscriptRow::Activities(1..3),
            TranscriptRow::Message(3),
            TranscriptRow::Working,
        ];
        assert_eq!(build_transcript_rows(&messages, true), expected);
        let mut rows = transcript_rows(&messages, true);
        for row in expected {
            assert_eq!(rows.next(), Some(row));
        }
        assert_eq!(rows.next(), None);
        assert_eq!(rows.next(), None);
        let hit = first_conversation_match(&messages, true, "needle").unwrap();
        assert_eq!(hit.row_index, 1);
        assert_eq!(hit.message_id, "user-1");
        assert_eq!(
            build_transcript_rows(&messages, false),
            vec![
                TranscriptRow::Message(0),
                TranscriptRow::Activities(1..3),
                TranscriptRow::Message(3)
            ]
        );
        assert_eq!(
            build_transcript_rows(&[], true),
            vec![TranscriptRow::Working]
        );
    }

    #[test]
    fn excerpts_map_folded_offsets_back_to_unicode_source() {
        let content = format!("{}İstanbul NEEDLE{}", "é".repeat(80), "λ".repeat(180));
        let messages = vec![message("user", MessageRole::User, &content)];
        let hit = first_conversation_match(&messages, false, "needle").unwrap();
        assert_eq!(
            hit.excerpt,
            format!("…{}İstanbul NEEDLE{}…", "é".repeat(31), "λ".repeat(114))
        );
    }
}

const PROMPT_EXCERPT_CHARS: usize = 96;

/// One user prompt in the transcript, in chronological order. Shared by the
/// conversation outline and composer prompt recall.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptLandmark {
    pub message_id: String,
    /// 1-based index among user prompts ("Prompt N").
    pub ordinal: usize,
    /// Transcript row index at derivation time; revalidate before scrolling.
    pub row_index: usize,
    /// Full message content; composer recall loads this verbatim.
    pub text: String,
    /// Bounded, whitespace-normalized excerpt for compact lists. Empty when
    /// the message has no display text.
    pub excerpt: String,
    /// Optimistic queue/steer echo not yet confirmed by the session stream.
    pub pending_echo: bool,
}

/// Chronological user prompts derived from the projected transcript rows, so
/// activity-only and excluded messages never appear as landmarks.
pub fn prompt_landmarks(messages: &[ChatMessageInfo], generating: bool) -> Vec<PromptLandmark> {
    let mut landmarks = Vec::new();
    for (row_index, row) in transcript_rows(messages, generating).enumerate() {
        let TranscriptRow::Message(index) = row else {
            continue;
        };
        let message = &messages[index];
        if message.role != MessageRole::User {
            continue;
        }
        landmarks.push(PromptLandmark {
            message_id: message.id.clone(),
            ordinal: landmarks.len() + 1,
            row_index,
            text: message.content.clone(),
            excerpt: prompt_excerpt(&message.content),
            pending_echo: message.id.starts_with("queued-user-")
                || message.id.starts_with("steered-user-"),
        });
    }
    landmarks
}

/// Whitespace-normalized, `PROMPT_EXCERPT_CHARS`-bounded excerpt of a prompt
/// for compact list display; appends `…` when truncated.
fn prompt_excerpt(content: &str) -> String {
    let mut excerpt = String::with_capacity(PROMPT_EXCERPT_CHARS + 1);
    let mut pending_space = false;
    let mut count = 0;
    let mut chars = content.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            pending_space = !excerpt.is_empty();
            continue;
        }
        if pending_space {
            excerpt.push(' ');
            count += 1;
            pending_space = false;
            if count > PROMPT_EXCERPT_CHARS {
                excerpt.push('…');
                return excerpt;
            }
        }
        excerpt.push(ch);
        count += 1;
        if count > PROMPT_EXCERPT_CHARS {
            excerpt.push('…');
            return excerpt;
        }
    }
    excerpt
}

/// Outcome of stepping the composer prompt-recall cursor with Up/Down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptRecallStep {
    /// The key falls through to the default caret behavior.
    PassThrough,
    /// Load the entry at this index of the eligible prompt list.
    Load(usize),
    /// The cursor moved past the newest entry; restore the empty composer.
    Clear,
}

/// Older/newer stepping over eligible prompt entries. `position` is the
/// cursor's index into the entry list (`None` when the composer is not
/// browsing). Older navigation saturates at the oldest entry; newer
/// navigation past the newest entry clears the composer.
pub fn prompt_recall_step(position: Option<usize>, count: usize, older: bool) -> PromptRecallStep {
    if count == 0 {
        return PromptRecallStep::PassThrough;
    }
    match (position, older) {
        (None, true) => PromptRecallStep::Load(count - 1),
        (None, false) => PromptRecallStep::PassThrough,
        (Some(index), true) => PromptRecallStep::Load(index.saturating_sub(1)),
        (Some(index), false) if index + 1 >= count => PromptRecallStep::Clear,
        (Some(index), false) => PromptRecallStep::Load(index + 1),
    }
}

/// Arrow/Home/End stepping over an outline list; moves focus only, no wrap.
pub fn step_prompt_focus(focused: Option<usize>, count: usize, key: &str) -> Option<usize> {
    if count == 0 {
        return None;
    }
    match key {
        "up" => Some(match focused {
            Some(index) if index > 0 => index - 1,
            _ => 0,
        }),
        "down" => Some(match focused {
            Some(index) => (index + 1).min(count - 1),
            None => 0,
        }),
        "home" => Some(0),
        "end" => Some(count - 1),
        _ => focused,
    }
}
