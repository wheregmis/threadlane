pub const INPUT_KEY_CONTEXT: &str = "Input";
pub const SLASH_COMMAND_KEY_CONTEXT: &str = "SlashCommandMenu";
pub const SLASH_COMMAND_BINDING_CONTEXT: &str = "SlashCommandMenu > Input";
pub use threadlane_ui_kit::file_completion::{
    active_file_query, filter_file_matches, format_path_insertion, is_safe_relative_path,
    markdown_code_span, FileQueryTrigger, FILE_COMPLETION_BINDING_CONTEXT,
    FILE_COMPLETION_KEY_CONTEXT, FILE_COMPLETION_RESULT_LIMIT,
};
pub use threadlane_ui_kit::{PROMPT_RECALL_BINDING_CONTEXT, PROMPT_RECALL_KEY_CONTEXT};

// Content widths are rem-based so the reading column follows interface zoom.
pub use threadlane_ui_theme::CHAT_CONTENT_MAX_WIDTH;
// Questions should read as a compact inline card, not fill the composer column.
pub use threadlane_ui_theme::QUESTION_CARD_MAX_WIDTH;

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_ui_theme::USER_BUBBLE_MAX_WIDTH;

    #[test]
    fn question_card_is_narrower_than_chat_content() {
        assert!(QUESTION_CARD_MAX_WIDTH < USER_BUBBLE_MAX_WIDTH);
    }
}

pub use threadlane_client::ComposerDraft;

pub fn active_slash_command_query(text: &str) -> Option<&str> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('/') {
        return None;
    }
    let rest = &trimmed[1..];
    if rest.contains(char::is_whitespace) {
        return None;
    }
    Some(rest)
}
