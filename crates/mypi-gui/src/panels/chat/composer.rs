#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationEvent {
    AgentEnd,
    CommandOutput,
    AgentError,
}

/// Accept only events belonging to the current generation. An AgentEnd is a
/// terminal marker, but its command output is still valid for that generation.
pub fn accepts_generation_event(
    active_generation: Option<u64>,
    terminal_generation: Option<u64>,
    generation: u64,
    event: GenerationEvent,
) -> bool {
    match event {
        GenerationEvent::CommandOutput => {
            active_generation == Some(generation) || terminal_generation == Some(generation)
        }
        GenerationEvent::AgentEnd | GenerationEvent::AgentError => {
            active_generation == Some(generation)
        }
    }
}

pub fn concise_status(error: &str) -> String {
    let first_line = error.lines().next().unwrap_or_default().trim();
    let mut text: String = first_line.chars().take(160).collect();
    if text.chars().count() < first_line.chars().count() {
        text.push('…');
    }
    text
}

/// Return the draft only when cancellation belongs to the currently active
/// generation. This keeps abort paths correlated just like agent events.
pub fn draft_for_cancellation(
    active_generation: Option<u64>,
    submitted_draft: Option<&(u64, String)>,
    cancelled_generation: u64,
) -> Option<String> {
    (active_generation == Some(cancelled_generation))
        .then(|| submitted_draft.filter(|(id, _)| *id == cancelled_generation))
        .flatten()
        .map(|(_, draft)| draft.clone())
}
pub fn submitted_draft(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| (raw.to_string(), trimmed.to_string()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerStatus {
    Ready,
    Working,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerPresentation {
    pub show_model: bool,
    pub working: bool,
}

#[derive(Clone, Debug)]
pub struct ComposerState {
    status: ComposerStatus,
}

impl ComposerState {
    pub fn new() -> Self {
        Self {
            status: ComposerStatus::Ready,
        }
    }

    pub fn set_status(&mut self, status: ComposerStatus, _message: impl Into<String>) {
        self.status = status;
    }

    pub fn presentation(&self) -> ComposerPresentation {
        ComposerPresentation {
            show_model: true,
            working: self.status == ComposerStatus::Working,
        }
    }
}

impl Default for ComposerState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_correlation_accepts_end_then_output() {
        assert!(accepts_generation_event(
            Some(7),
            None,
            7,
            GenerationEvent::AgentEnd
        ));
        assert!(accepts_generation_event(
            None,
            Some(7),
            7,
            GenerationEvent::CommandOutput
        ));
    }

    #[test]
    fn stale_generation_is_ignored() {
        assert!(!accepts_generation_event(
            Some(8),
            None,
            7,
            GenerationEvent::AgentError
        ));
        assert!(!accepts_generation_event(
            None,
            Some(8),
            7,
            GenerationEvent::CommandOutput
        ));
        assert!(!accepts_generation_event(
            Some(8),
            Some(8),
            7,
            GenerationEvent::AgentEnd
        ));
        assert!(!accepts_generation_event(
            None,
            Some(7),
            7,
            GenerationEvent::AgentError
        ));
    }

    #[test]
    fn invalidation_prevents_old_output() {
        assert!(!accepts_generation_event(
            None,
            None,
            7,
            GenerationEvent::CommandOutput
        ));
    }

    #[test]
    fn cancellation_restores_only_matching_submitted_draft() {
        let draft = (7, "keep this draft".to_string());
        assert_eq!(
            draft_for_cancellation(Some(7), Some(&draft), 7),
            Some("keep this draft".to_string())
        );
        assert_eq!(draft_for_cancellation(Some(8), Some(&draft), 7), None);
        assert_eq!(draft_for_cancellation(Some(7), None, 7), None);
    }

    #[test]
    fn error_status_is_single_line_and_bounded() {
        let error = format!("first line\n{}", "x".repeat(200));
        assert_eq!(concise_status(&error), "first line");
        assert!(concise_status(&"x".repeat(200)).chars().count() <= 161);
    }
    #[test]
    fn submitted_draft_preserves_raw_whitespace_and_multiline_text() {
        let raw = "  first line\nsecond line  ";
        assert_eq!(
            submitted_draft(raw),
            Some((raw.to_string(), "first line\nsecond line".to_string()))
        );
        assert_eq!(submitted_draft(" \n\t "), None);
    }

    #[test]
    fn idle_keeps_composer_controls_available() {
        let state = ComposerState::new();
        assert_eq!(
            state.presentation(),
            ComposerPresentation {
                show_model: true,
                working: false,
            }
        );
    }

    #[test]
    fn working_keeps_all_normal_controls_and_uses_working_action() {
        let mut state = ComposerState::new();
        state.set_status(ComposerStatus::Working, "Working...");
        let presentation = state.presentation();
        assert!(presentation.working);
        assert!(presentation.show_model);
    }

    #[test]
    fn error_keeps_input_and_model_controls_clear() {
        let mut state = ComposerState::new();
        state.set_status(ComposerStatus::Error, "Provider unavailable");
        let presentation = state.presentation();
        assert!(!presentation.working);
        assert!(presentation.show_model);
    }
}
