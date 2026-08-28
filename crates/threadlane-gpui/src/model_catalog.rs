#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelProvider {
    OpenAi,
    Antigravity,
    OpenCode,
    Acp,
}

impl ModelProvider {
    pub(crate) const fn icon_path(self) -> &'static str {
        match self {
            Self::OpenAi => "icons/providers/openai.svg",
            Self::Antigravity => "icons/providers/google.svg",
            Self::OpenCode => "icons/providers/opencode.svg",
            Self::Acp => "icons/providers/acp.svg",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOption {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) provider: ModelProvider,
}

pub(crate) fn label_for(models: &[ModelOption], model_id: &str) -> Option<String> {
    models
        .iter()
        .find(|model| model.id == model_id)
        .map(|model| model.label.clone())
}

pub const UNKNOWN_MODEL_CONTEXT_LIMIT: usize = 128_000;

#[cfg(test)]
pub(crate) fn model_context_limit(model: &str) -> Option<usize> {
    if model.contains("gemini-3.1-pro") || model.contains("gemini-1.5-pro") {
        Some(2_000_000)
    } else if model.contains("flash") {
        Some(1_000_000)
    } else if model.contains("gpt-4o") || model.contains("o1") || model.contains("o3") {
        Some(128_000)
    } else {
        None
    }
}

#[cfg(test)]
pub(crate) fn model_context_window(model: &str) -> u32 {
    model_context_limit(model)
        .unwrap_or(UNKNOWN_MODEL_CONTEXT_LIMIT)
        .min(u32::MAX as usize) as u32
}

pub(crate) fn format_tokens(tokens: u32) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_window_and_token_formatting() {
        assert_eq!(
            model_context_window("antigravity/gemini-3.7-flash"),
            1_000_000
        );
        assert_eq!(
            model_context_window("unknown/model"),
            UNKNOWN_MODEL_CONTEXT_LIMIT as u32,
        );
        assert_eq!(
            model_context_window("antigravity/gemini-3.1-pro"),
            2_000_000
        );
        assert_eq!(format_tokens(850), "850");
        assert_eq!(format_tokens(24_500), "24.5k");
        assert_eq!(format_tokens(1_000_000), "1.0M");
    }
}
