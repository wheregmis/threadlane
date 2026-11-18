//! Drafts and the delivery lifecycle for the desktop composer.

use threadlane_client::{ClientCore, ClientError, Sent};
use threadlane_core::{ids::TaskId, time::Timestamp};

use crate::{Activity, AppState, describe};

/// The undo window, in seconds.
pub const SEND_HOLD_SECONDS: u8 = 3;

/// The two existing draft fields used by the four composer placements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Draft {
    /// Home, project board and conversation.
    Lead,
    /// Task lane.
    Task,
}

impl Draft {
    fn index(self) -> usize {
        match self {
            Self::Lead => 0,
            Self::Task => 1,
        }
    }
}

/// Where a submitted draft will go, even if the owner navigates away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// A project lead in the named space.
    Lead(String, String),
    /// A task lane in the named space.
    Task(String, TaskId),
}

impl Destination {
    fn draft(&self) -> Draft {
        match self {
            Self::Lead(..) => Draft::Lead,
            Self::Task(..) => Draft::Task,
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Lead(space, project) => format!("the lead of {space} / {project}"),
            Self::Task(space, task) => format!("{space} / {task}"),
        }
    }
}

/// Whether an Enter gesture submits the draft. The field inserts Shift+Enter.
#[must_use]
pub fn sends_on_enter(command: bool, shift: bool) -> bool {
    command || !shift
}

/// One optimistic message, held or being delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    /// Local identity until the hub supplies an event ID.
    pub id: u64,
    /// The selected recipient.
    pub destination: Destination,
    /// The draft as typed.
    pub text: String,
    /// When the owner submitted it.
    pub at: Timestamp,
}

impl Outgoing {
    /// Delivers the snapshot to its original recipient.
    ///
    /// # Errors
    ///
    /// Returns any failure reported by the client or hub.
    pub async fn deliver(&self, core: &ClientCore) -> Result<Sent, ClientError> {
        match &self.destination {
            Destination::Lead(space, project) => {
                core.send(Some(space), Some(project), &self.text).await
            }
            Destination::Task(space, task) => {
                core.send_to_task(Some(space), &task.to_string(), &self.text)
                    .await
            }
        }
    }
}

/// Work requested by one press of Send.
#[derive(Debug, Default)]
pub struct SendRequest {
    /// A previous held message flushed by this press.
    pub flushed: Option<Outgoing>,
    /// The new message to deliver now, if the hold is disabled.
    pub immediate: Option<Outgoing>,
    /// The new message whose countdown should start.
    pub held: Option<u64>,
}

#[derive(Debug, Default)]
pub(crate) struct Composer {
    drafts: [String; 2],
    errors: [Option<String>; 2],
    held: Option<(Outgoing, u8)>,
    sending: Vec<Outgoing>,
    next_id: u64,
}

impl Composer {
    fn is_sending(&self, draft: Draft) -> bool {
        self.sending
            .iter()
            .any(|send| send.destination.draft() == draft)
    }

    fn start(&mut self, send: Outgoing) -> Outgoing {
        self.sending.push(send.clone());
        send
    }
}

impl AppState {
    /// The text currently in the composer.
    #[must_use]
    pub fn draft(&self, draft: Draft) -> &str {
        &self.composer.drafts[draft.index()]
    }

    /// Records edits without altering the snapshot of a submitted message.
    pub fn edit_draft(&mut self, draft: Draft, text: &str) {
        if self.draft(draft) != text {
            text.clone_into(&mut self.composer.drafts[draft.index()]);
            self.composer.errors[draft.index()] = None;
        }
    }

    /// Adds a Markdown quotation of the chosen message to the draft.
    pub fn reply_to_message(&mut self, composer: Draft, text: &str) {
        let quote = text
            .lines()
            .map(|line| {
                format!(
                    "> {line}
"
                )
            })
            .collect::<String>();
        let draft = format!(
            "{quote}
{}",
            self.draft(composer)
        );
        self.edit_draft(composer, &draft);
    }

    /// The failure associated with the draft, if any.
    #[must_use]
    pub fn send_error(&self, draft: Draft) -> Option<&str> {
        self.composer.errors[draft.index()].as_deref()
    }

    /// The hold countdown for this composer.
    #[must_use]
    pub fn held_seconds(&self, draft: Draft) -> Option<u8> {
        self.composer
            .held
            .as_ref()
            .and_then(|(send, seconds)| (send.destination.draft() == draft).then_some(*seconds))
    }

    /// Whether a delivery to this composer is in progress.
    #[must_use]
    pub fn is_sending(&self, draft: Draft) -> bool {
        self.composer.is_sending(draft)
    }

    /// Whether another submission is possible.
    #[must_use]
    pub fn is_send_enabled(&self, draft: Draft) -> bool {
        !self.draft(draft).trim().is_empty()
            && (!self.is_sending(draft) || self.held_seconds(draft).is_some())
    }

    /// Takes a nonempty draft, flushing any previous hold first.
    pub fn request_send(&mut self, destination: Destination, at: Timestamp) -> SendRequest {
        let draft = destination.draft();
        let archived = match &destination {
            Destination::Lead(space, project) => self.is_archived(space, project),
            Destination::Task(space, task) => self.task_is_archived(space, *task),
        };
        if archived || !self.is_send_enabled(draft) {
            return SendRequest::default();
        }
        self.composer.errors[draft.index()] = None;
        let text = self.draft(draft).to_owned();
        let flushed = self
            .composer
            .held
            .take()
            .map(|(send, _)| self.composer.start(send));
        let mut request = SendRequest {
            flushed,
            ..SendRequest::default()
        };
        // A second Enter on the same draft expedites it. Edited text starts a new hold.
        if request
            .flushed
            .as_ref()
            .is_some_and(|send| send.destination == destination && send.text == text)
        {
            return request;
        }
        self.composer.next_id += 1;
        let send = Outgoing {
            id: self.composer.next_id,
            destination,
            text,
            at,
        };
        if self.hold_messages() {
            request.held = Some(send.id);
            self.composer.held = Some((send, SEND_HOLD_SECONDS));
        } else {
            request.immediate = Some(self.composer.start(send));
        }
        request
    }

    /// Advances the matching hold by one second and releases it on expiry.
    /// A timer for a flushed or undone send has no effect.
    pub fn tick_send(&mut self, id: u64) -> Option<Outgoing> {
        let (send, seconds) = self.composer.held.as_mut()?;
        if send.id != id {
            return None;
        }
        *seconds -= 1;
        if *seconds > 0 {
            return None;
        }
        self.composer
            .held
            .take()
            .map(|(send, _)| self.composer.start(send))
    }

    /// Cancels the visible held message and restores its text without losing later edits.
    pub fn undo_send(&mut self, draft: Draft) -> bool {
        if self.held_seconds(draft).is_none() {
            return false;
        }
        if let Some((send, _)) = self.composer.held.take() {
            self.preserve_draft(draft, &send.text);
        }
        true
    }

    fn preserve_draft(&mut self, draft: Draft, text: &str) {
        let current = self.draft(draft);
        if current != text {
            self.composer.drafts[draft.index()] = if current.trim().is_empty() {
                text.to_owned()
            } else {
                format!(
                    "{text}

{current}"
                )
            };
        }
    }

    /// Whether a newly received owner event may be the copy of a send still in flight.
    ///
    /// History has no client message ID. Keep the optimistic row during this short overlap,
    /// then reconcile by the event ID from the acknowledgement.
    #[must_use]
    pub fn has_optimistic_copy(
        &self,
        destination: &Destination,
        text: &str,
        at: Timestamp,
    ) -> bool {
        self.composer
            .sending
            .iter()
            .any(|send| &send.destination == destination && send.text == text && at >= send.at)
    }

    /// Reports when the requested message starts going to its recipient.
    pub fn sending(&mut self, send: &Outgoing) {
        self.report(Activity::Working(format!(
            "Sending to {}…",
            send.destination.label()
        )));
    }

    /// The local messages that have yet to be confirmed by the hub.
    pub fn outgoing(&self) -> impl Iterator<Item = &Outgoing> {
        self.composer
            .sending
            .iter()
            .chain(self.composer.held.iter().map(|(send, _)| send))
    }

    /// Shows a submission failure beside the draft and in the status line.
    pub fn fail_send(&mut self, draft: Draft, why: String) {
        self.composer.errors[draft.index()] = Some(why.clone());
        self.report(Activity::Failed(why));
    }

    /// Reconciles the optimistic row with the accepted event or keeps the draft and error.
    pub fn finish_send(&mut self, send: Outgoing, result: Result<Sent, ClientError>) {
        self.composer
            .sending
            .retain(|pending| pending.id != send.id);
        let draft = send.destination.draft();
        let label = send.destination.label();
        match result {
            Ok(sent) => {
                let same_draft_has_newer_send = self
                    .outgoing()
                    .any(|other| other.destination.draft() == draft && other.id > send.id);
                if self.draft(draft) == send.text && !same_draft_has_newer_send {
                    self.composer.drafts[draft.index()].clear();
                }
                match send.destination {
                    Destination::Lead(space, project) => {
                        self.sent(
                            &space,
                            &project,
                            &send.text,
                            sent.accepted.event_id,
                            send.at,
                        );
                    }
                    Destination::Task(space, task) => {
                        self.sent_to_task(
                            &space,
                            task,
                            &send.text,
                            sent.accepted.event_id,
                            send.at,
                        );
                    }
                }
                self.report(Activity::Done(format!("Sent to {label}.")));
            }
            Err(error) => {
                let why = describe(&error);
                self.preserve_draft(draft, &send.text);
                self.fail_send(draft, why);
            }
        }
    }
}
