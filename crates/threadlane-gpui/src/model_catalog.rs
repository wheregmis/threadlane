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
    fn token_formatting() {
        assert_eq!(format_tokens(850), "850");
        assert_eq!(format_tokens(24_500), "24.5k");
        assert_eq!(format_tokens(1_000_000), "1.0M");
    }
}
