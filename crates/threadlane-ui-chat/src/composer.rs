use gpui::SharedString;
use threadlane_protocol::ImageAttachment;

pub const INPUT_KEY_CONTEXT: &str = "Input";
pub const SLASH_COMMAND_KEY_CONTEXT: &str = "SlashCommandMenu";
pub const SLASH_COMMAND_BINDING_CONTEXT: &str = "SlashCommandMenu > Input";

// Content widths are rem-based so the reading column follows interface zoom.
pub const CHAT_CONTENT_MAX_WIDTH: f32 = 48.0;
pub const USER_BUBBLE_MAX_WIDTH: f32 = 40.0;

#[derive(Default, Clone)]
pub struct ComposerDraft {
    pub text: SharedString,
    pub images: Vec<ImageAttachment>,
}

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
