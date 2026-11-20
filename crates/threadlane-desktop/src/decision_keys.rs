//! Keyboard selection and submission for the decisions a screen displays.

use threadlane_core::forge::{
    COMMENT, CREATE_ISSUE, MARK_READY, OPEN_PULL_REQUEST, PUBLISH_BRANCH,
};
use threadlane_core::ids::{DecisionId, TaskId};
use threadlane_core::model::{Decision, DecisionKind, DecisionStatus};

use crate::{AppState, Queued};

/// The decision cards available on the open screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DecisionScope {
    /// A screen without decision cards.
    #[default]
    None,
    /// Home, subject to its current space filter.
    Home,
    /// One project, identified by space and project name.
    Project(String, String),
    /// One task, identified by space and task ID.
    Task(String, TaskId),
}

/// A decision's identity, including its hub.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DecisionKey {
    /// The hub that owns the decision.
    pub space: String,
    /// The decision ID within the hub.
    pub id: DecisionId,
}

impl From<&Queued> for DecisionKey {
    fn from(queued: &Queued) -> Self {
        Self {
            space: queued.space.clone(),
            id: queued.decision.id,
        }
    }
}

/// An option picked on a particular card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionChoice {
    /// The card being answered.
    pub key: DecisionKey,
    /// The label sent to the hub.
    pub option: String,
}

#[derive(Debug, Default)]
enum Current {
    #[default]
    Oldest,
    Selected(DecisionKey),
    Cleared,
}

#[derive(Debug, Default)]
pub(crate) struct DecisionKeys {
    scope: DecisionScope,
    current: Current,
    confirmation: Option<DecisionChoice>,
}

/// The decision commands that are available outside fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionCommand {
    /// Select the next pending card.
    Next,
    /// Select the previous pending card.
    Previous,
    /// Reveal and focus the current card's note field.
    Reply,
    /// Pick an option by its zero-based index.
    Pick(usize),
}

/// The UI effect of a decision command.
#[derive(Debug, PartialEq, Eq)]
pub enum DecisionEffect {
    /// Reveal the current card within its scrolling region.
    Scroll,
    /// Focus the specified card's note field.
    FocusNote(DecisionKey),
    /// Send the selected option.
    Answer(DecisionChoice),
    /// Ask for confirmation of the selected option.
    Confirm(DecisionChoice),
}

impl AppState {
    /// Starts the selection on a newly opened screen at its oldest decision.
    pub fn set_decision_scope(&mut self, scope: DecisionScope) {
        self.decision_keys = DecisionKeys {
            scope,
            ..DecisionKeys::default()
        };
    }

    /// Pending cards in the chosen scope, oldest first.
    pub fn decisions_in<'a>(
        &'a self,
        scope: &'a DecisionScope,
    ) -> impl DoubleEndedIterator<Item = &'a Queued> {
        self.queue.iter().filter(move |queued| {
            let decision = &queued.decision;
            match scope {
                DecisionScope::None => false,
                DecisionScope::Home => self.shown(&queued.space),
                DecisionScope::Project(space, project) => {
                    queued.space == *space
                        && self.project_name(space, decision.project_id) == Some(project)
                }
                DecisionScope::Task(space, task) => {
                    queued.space == *space && decision.task_id == Some(*task)
                }
            }
        })
    }

    /// Pending cards on the current screen.
    pub fn visible_decisions(&self) -> impl DoubleEndedIterator<Item = &Queued> {
        self.decisions_in(&self.decision_keys.scope)
    }

    /// The decision that receives keyboard commands.
    #[must_use]
    pub fn current_decision(&self) -> Option<&Queued> {
        let mut visible = self.visible_decisions();
        match &self.decision_keys.current {
            Current::Oldest => visible.next(),
            Current::Selected(key) => visible
                .find(|queued| key.matches(queued))
                .or_else(|| self.visible_decisions().next()),
            Current::Cleared => None,
        }
    }

    /// Clears the ring and disables number shortcuts until stepping again.
    pub fn clear_current_decision(&mut self) {
        self.decision_keys.current = Current::Cleared;
    }

    /// Selects a pending decision row in the current scope.
    pub fn select_decision(&mut self, key: DecisionKey) {
        if self.visible_decisions().any(|queued| key.matches(queued)) {
            self.decision_keys.current = Current::Selected(key);
        }
    }

    /// Applies a shortcut if the user is outside a text field.
    pub fn decision_command(
        &mut self,
        command: DecisionCommand,
        field_focused: bool,
    ) -> Option<DecisionEffect> {
        if field_focused || self.decision_keys.confirmation.is_some() {
            return None;
        }
        let current = self.current_decision().map(DecisionKey::from);
        match command {
            DecisionCommand::Next | DecisionCommand::Previous => {
                let step = |mut visible: &mut dyn Iterator<Item = &Queued>| {
                    if let Some(key) = &current {
                        (&mut visible).find(|queued| key.matches(queued))?;
                    }
                    visible.next().map(DecisionKey::from)
                };
                let next = if command == DecisionCommand::Previous {
                    step(&mut self.visible_decisions().rev())
                } else {
                    step(&mut self.visible_decisions())
                };
                let key = next.or(current)?;
                self.decision_keys.current = Current::Selected(key);
                Some(DecisionEffect::Scroll)
            }
            DecisionCommand::Reply => {
                let key = current?;
                self.reveal_decision_note(key.clone());
                Some(DecisionEffect::FocusNote(key))
            }
            DecisionCommand::Pick(index) => {
                let decision = self.current_decision()?;
                let key = DecisionKey::from(decision);
                if self.answering.contains(&key) {
                    return None;
                }
                let choice = DecisionChoice {
                    key,
                    option: decision.decision.options.get(index)?.label.clone(),
                };
                if confirms_number(&decision.decision) {
                    self.decision_keys.confirmation = Some(choice.clone());
                    Some(DecisionEffect::Confirm(choice))
                } else {
                    Some(DecisionEffect::Answer(choice))
                }
            }
        }
    }

    /// Reveals a card's note field.
    pub fn reveal_decision_note(&mut self, key: DecisionKey) {
        self.decision_notes.insert(key.clone());
        self.decision_keys.current = Current::Selected(key);
    }

    /// Whether the pending card shows its note field.
    #[must_use]
    pub fn shows_decision_note(&self, queued: &Queued) -> bool {
        queued.decision.kind == DecisionKind::Input
            || self.decision_notes.contains(&DecisionKey::from(queued))
    }

    /// Enter in a note submits only a nonblank answer to an input question.
    #[must_use]
    pub fn decision_note_enter(&self, key: &DecisionKey, note: &str) -> Option<DecisionChoice> {
        let queued = self
            .visible_decisions()
            .find(|queued| key.matches(queued))?;
        let decision = &queued.decision;
        if decision.kind != DecisionKind::Input || note.trim().is_empty() || self.is_answering(key)
        {
            return None;
        }
        Some(DecisionChoice {
            key: key.clone(),
            option: decision
                .options
                .get(decision.recommended? as usize)?
                .label
                .clone(),
        })
    }

    /// Confirms the exact option presented by the dialog, if still open.
    pub fn confirm_decision(&mut self, shown: &DecisionChoice) -> Option<DecisionChoice> {
        let choice = self.decision_keys.confirmation.take()?;
        (&choice == shown
            && self
                .visible_decisions()
                .any(|queued| choice.key.matches(queued)))
        .then_some(choice)
    }

    /// Dismisses the confirmation without submitting.
    pub fn cancel_decision_confirmation(&mut self) {
        self.decision_keys.confirmation = None;
    }

    /// Reserves a decision while its answer is in flight.
    pub fn begin_decision_answer(&mut self, key: DecisionKey) -> bool {
        !self
            .decision_records
            .get(&key)
            .is_some_and(|record| record.status != DecisionStatus::Pending)
            && self.answering.insert(key)
    }

    /// Releases the reservation after a response or a failure.
    pub fn finish_decision_answer(&mut self, key: &DecisionKey) {
        self.answering.remove(key);
    }

    /// Whether a response is in flight for this card.
    #[must_use]
    pub fn is_answering(&self, key: &DecisionKey) -> bool {
        self.answering.contains(key)
    }

    pub(crate) fn advance_answered_decision(&mut self, key: &DecisionKey) {
        if !self
            .current_decision()
            .is_some_and(|queued| key.matches(queued))
        {
            return;
        }
        let mut visible = self.visible_decisions().peekable();
        let mut previous = None;
        while let Some(queued) = visible.next() {
            if key.matches(queued) {
                let next = visible.peek().copied().or(previous).map(DecisionKey::from);
                drop(visible);
                self.decision_keys.current = next.map_or(Current::Oldest, Current::Selected);
                return;
            }
            previous = Some(queued);
        }
    }
}

impl DecisionKey {
    pub(crate) fn matches(&self, queued: &Queued) -> bool {
        self.space == queued.space && self.id == queued.decision.id
    }
}

fn confirms_number(decision: &Decision) -> bool {
    // shortcut: decisions carry a tool name, so unknown tools require
    // confirmation until the hub supplies tiers in its decision replies.
    !matches!(
        decision.tool.as_str(),
        COMMENT
            | threadlane_core::forge::REPORT_LIMITATION
            | CREATE_ISSUE
            | MARK_READY
            | threadlane_core::forge::LABEL_ISSUE
            | OPEN_PULL_REQUEST
            | PUBLISH_BRANCH
            | "memory_view"
            | "memory_create"
            | "memory_edit"
            | "memory_delete"
            | "memory_rename"
            | "create_task"
            | "update_task"
            | "cancel_task"
            | "list_tasks"
            | "finish_task"
            | "set_follow_up"
            | "create_schedule"
            | "list_timers"
            | "cancel_timer"
            | "send_message"
            | "project_sessions"
            | "ask_user"
    )
}
