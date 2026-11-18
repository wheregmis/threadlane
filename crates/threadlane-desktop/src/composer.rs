//! Drafts and the delivery lifecycle for the desktop composer.

use std::collections::HashMap;

use threadlane_client::{ClientCore, ClientError, Sent};
use threadlane_core::attachment::{ImageAttachment, check_image_budget};
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

/// Where a submitted draft will go, even if the owner navigates away.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Destination {
    /// A project lead in the named space.
    Lead(String, String),
    /// A task lane in the named space.
    Task(String, TaskId),
}

impl Destination {
    /// Which input can edit this destination.
    #[must_use]
    pub fn draft(&self) -> Draft {
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
    /// The images selected for this submission.
    pub images: Vec<ImageAttachment>,
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
                core.send_with_images(Some(space), Some(project), &self.text, self.images.clone())
                    .await
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
struct DraftState {
    text: String,
    images: Vec<ImageAttachment>,
    loading_images: usize,
    error: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Composer {
    drafts: HashMap<Destination, DraftState>,
    held: Option<(Outgoing, u8)>,
    sending: Vec<Outgoing>,
    next_id: u64,
}

impl Composer {
    fn start(&mut self, send: Outgoing) -> Outgoing {
        self.sending.push(send.clone());
        send
    }
}

impl AppState {
    /// The text currently in the composer.
    #[must_use]
    pub fn draft(&self, destination: &Destination) -> &str {
        self.composer
            .drafts
            .get(destination)
            .map_or("", |draft| draft.text.as_str())
    }

    /// The images in this destination's draft.
    #[must_use]
    pub fn draft_images(&self, destination: &Destination) -> &[ImageAttachment] {
        self.composer
            .drafts
            .get(destination)
            .map_or(&[], |draft| &draft.images)
    }

    /// Records an image selection in progress for this conversation.
    pub fn loading_images(&mut self, destination: &Destination) {
        self.composer
            .drafts
            .entry(destination.clone())
            .or_default()
            .loading_images += 1;
    }

    /// Whether images are being prepared for this conversation.
    #[must_use]
    pub fn is_loading_images(&self, destination: &Destination) -> bool {
        self.composer
            .drafts
            .get(destination)
            .is_some_and(|draft| draft.loading_images > 0)
    }

    /// Completes one image selection, including cancellation or validation errors.
    pub fn attach_images(
        &mut self,
        destination: &Destination,
        images: Result<Vec<ImageAttachment>, String>,
    ) {
        let draft = self.composer.drafts.entry(destination.clone()).or_default();
        draft.loading_images = draft.loading_images.saturating_sub(1);
        draft.error = match images.and_then(|images| {
            let mut combined = draft.images.clone();
            for image in images {
                if !combined.contains(&image) {
                    combined.push(image);
                }
            }
            check_image_budget(&combined)?;
            Ok(combined)
        }) {
            Ok(images) => {
                draft.images = images;
                None
            }
            Err(error) => Some(error),
        };
    }

    /// Removes a selected image from this destination's draft.
    pub fn remove_image(&mut self, destination: &Destination, image: &ImageAttachment) {
        if let Some(draft) = self.composer.drafts.get_mut(destination) {
            draft.images.retain(|selected| selected != image);
            draft.error = None;
        }
    }

    /// Records edits without altering the snapshot of a submitted message.
    pub fn edit_draft(&mut self, destination: &Destination, text: &str) {
        if self.draft(destination) != text {
            let draft = self.composer.drafts.entry(destination.clone()).or_default();
            text.clone_into(&mut draft.text);
            draft.error = None;
        }
    }

    /// Carries the text and images typed for `from` over to `to`, so
    /// choosing another recipient keeps what you were writing.
    pub fn move_draft(&mut self, from: &Destination, to: &Destination) {
        if from == to {
            return;
        }
        let Some(draft) = self.composer.drafts.get_mut(from) else {
            return;
        };
        let text = std::mem::take(&mut draft.text);
        let images = std::mem::take(&mut draft.images);
        if !text.trim().is_empty() {
            self.preserve_draft(to, &text);
        }
        if !images.is_empty() {
            self.restore_images(to, images);
        }
    }

    /// Adds a Markdown quotation of the chosen message to the draft.
    pub fn reply_to_message(&mut self, destination: &Destination, text: &str) {
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
            self.draft(destination)
        );
        self.edit_draft(destination, &draft);
    }

    /// The failure associated with the draft, if any.
    #[must_use]
    pub fn send_error(&self, destination: &Destination) -> Option<&str> {
        self.composer.drafts.get(destination)?.error.as_deref()
    }

    /// The hold countdown for this composer.
    #[must_use]
    pub fn held_seconds(&self, destination: &Destination) -> Option<u8> {
        self.composer
            .held
            .as_ref()
            .and_then(|(send, seconds)| (&send.destination == destination).then_some(*seconds))
    }

    /// Whether a delivery to this composer is in progress.
    #[must_use]
    pub fn is_sending(&self, destination: &Destination) -> bool {
        self.composer
            .sending
            .iter()
            .any(|send| &send.destination == destination)
    }

    /// Whether another submission is possible.
    #[must_use]
    pub fn is_send_enabled(&self, destination: &Destination) -> bool {
        (!self.draft(destination).trim().is_empty() || !self.draft_images(destination).is_empty())
            && !self.is_loading_images(destination)
            && (!self.is_sending(destination) || self.held_seconds(destination).is_some())
    }

    /// Takes a nonempty draft, flushing any previous hold first.
    pub fn request_send(&mut self, destination: Destination, at: Timestamp) -> SendRequest {
        let archived = match &destination {
            Destination::Lead(space, project) => self.is_archived(space, project),
            Destination::Task(space, task) => self.task_is_archived(space, *task),
        };
        if archived || !self.is_send_enabled(&destination) {
            return SendRequest::default();
        }
        let draft = self.composer.drafts.entry(destination.clone()).or_default();
        draft.error = None;
        let text = draft.text.clone();
        let images = draft.images.clone();
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
        if request.flushed.as_ref().is_some_and(|send| {
            send.destination == destination && send.text == text && send.images == images
        }) {
            return request;
        }
        self.composer.next_id += 1;
        let send = Outgoing {
            id: self.composer.next_id,
            destination,
            text,
            images,
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
    pub fn undo_send(&mut self, destination: &Destination) -> bool {
        if self.held_seconds(destination).is_none() {
            return false;
        }
        if let Some((send, _)) = self.composer.held.take() {
            self.preserve_draft(destination, &send.text);
            self.restore_images(destination, send.images);
        }
        true
    }

    fn restore_images(&mut self, destination: &Destination, images: Vec<ImageAttachment>) {
        let draft = self.composer.drafts.entry(destination.clone()).or_default();
        let mut restored = images;
        for image in std::mem::take(&mut draft.images) {
            if !restored.contains(&image) {
                restored.push(image);
            }
        }
        draft.images = restored;
    }

    fn preserve_draft(&mut self, destination: &Destination, text: &str) {
        let current = &mut self
            .composer
            .drafts
            .entry(destination.clone())
            .or_default()
            .text;
        if current.as_str() != text {
            *current = if current.trim().is_empty() {
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
        images: &[ImageAttachment],
        at: Timestamp,
    ) -> bool {
        self.composer.sending.iter().any(|send| {
            &send.destination == destination
                && send.text == text
                && send.images == images
                && at >= send.at
        })
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
    fn fail_send(&mut self, destination: &Destination, why: String) {
        self.composer
            .drafts
            .entry(destination.clone())
            .or_default()
            .error = Some(why.clone());
        self.report(Activity::Failed(why));
    }

    /// Reconciles the optimistic row with the accepted event or keeps the draft and error.
    pub fn finish_send(&mut self, send: Outgoing, result: Result<Sent, ClientError>) {
        self.composer
            .sending
            .retain(|pending| pending.id != send.id);
        let destination = &send.destination;
        let label = send.destination.label();
        match result {
            Ok(sent) => {
                let same_draft_has_newer_send = self
                    .outgoing()
                    .any(|other| other.destination == *destination && other.id > send.id);
                if !same_draft_has_newer_send
                    && let Some(draft) = self.composer.drafts.get_mut(destination)
                {
                    if draft.text == send.text {
                        draft.text.clear();
                    }
                    for image in &send.images {
                        if let Some(index) = draft
                            .images
                            .iter()
                            .position(|draft_image| draft_image == image)
                        {
                            draft.images.remove(index);
                        }
                    }
                }
                match send.destination {
                    Destination::Lead(space, project) => {
                        self.sent_with_images(
                            &space,
                            &project,
                            &send.text,
                            sent.accepted.event_id,
                            send.at,
                            send.images,
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
                self.preserve_draft(destination, &send.text);
                self.restore_images(destination, send.images);
                self.fail_send(destination, why);
            }
        }
    }
}
