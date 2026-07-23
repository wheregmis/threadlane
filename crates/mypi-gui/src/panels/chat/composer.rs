//! Pure presentation state for the chat composer.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerStatus {
    Ready,
    Working,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerPresentation {
    pub expanded: bool,
    pub show_model: bool,
    pub show_plan: bool,
    pub working: bool,
    pub show_error: bool,
    pub status_text: String,
}

#[derive(Clone, Debug)]
pub struct ComposerState {
    status: ComposerStatus,
    status_text: String,
    focused: bool,
    has_text: bool,
    plan_relevant: bool,
}

impl ComposerState {
    pub fn new() -> Self {
        Self {
            status: ComposerStatus::Ready,
            status_text: String::new(),
            focused: false,
            has_text: false,
            plan_relevant: false,
        }
    }

    pub fn set_status(&mut self, status: ComposerStatus, message: impl Into<String>) {
        self.status = status;
        self.status_text = match status {
            ComposerStatus::Ready => String::new(),
            ComposerStatus::Working | ComposerStatus::Error => message.into(),
        };
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn set_has_text(&mut self, has_text: bool) {
        self.has_text = has_text;
    }

    pub fn set_plan_relevant(&mut self, plan_relevant: bool) {
        self.plan_relevant = plan_relevant;
    }

    pub fn presentation(&self) -> ComposerPresentation {
        let working = self.status == ComposerStatus::Working;
        let show_error = self.status == ComposerStatus::Error;
        let expanded = self.focused || self.has_text || working || show_error;

        ComposerPresentation {
            expanded,
            show_model: expanded && !working,
            show_plan: expanded && self.plan_relevant && !working,
            working,
            show_error,
            status_text: self.status_text.clone(),
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
    fn idle_is_compact_and_hides_adaptive_controls() {
        let state = ComposerState::new();
        assert_eq!(
            state.presentation(),
            ComposerPresentation {
                expanded: false,
                show_model: false,
                show_plan: false,
                working: false,
                show_error: false,
                status_text: String::new(),
            }
        );
    }

    #[test]
    fn focus_expands_and_reveals_model_without_forcing_plan() {
        let mut state = ComposerState::new();
        state.set_focused(true);
        let presentation = state.presentation();
        assert!(presentation.expanded);
        assert!(presentation.show_model);
        assert!(!presentation.show_plan);
    }

    #[test]
    fn typing_expands_composer() {
        let mut state = ComposerState::new();
        state.set_has_text(true);
        assert!(state.presentation().expanded);
        assert!(state.presentation().show_model);
    }

    #[test]
    fn working_replaces_send_state_and_clears_old_error() {
        let mut state = ComposerState::new();
        state.set_status(ComposerStatus::Error, "Provider unavailable");
        state.set_status(ComposerStatus::Working, "Working...");
        let presentation = state.presentation();
        assert!(presentation.working);
        assert!(!presentation.show_error);
        assert_eq!(presentation.status_text, "Working...");
    }

    #[test]
    fn error_keeps_input_available_and_exposes_message() {
        let mut state = ComposerState::new();
        state.set_status(ComposerStatus::Error, "Provider unavailable");
        let presentation = state.presentation();
        assert!(!presentation.working);
        assert!(presentation.show_error);
        assert_eq!(presentation.status_text, "Provider unavailable");
    }

    #[test]
    fn plan_visibility_is_independent_and_requires_relevant_plan() {
        let mut state = ComposerState::new();
        state.set_focused(true);
        state.set_plan_relevant(true);
        assert!(state.presentation().show_plan);
        state.set_focused(false);
        state.set_has_text(false);
        assert!(!state.presentation().show_plan);
    }
}
