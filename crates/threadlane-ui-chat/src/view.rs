use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use std::time::Duration;

use base64::Engine as _;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{
    Button, ButtonVariants,
};
use gpui_component::combobox::{ComboboxEvent, ComboboxState};
use gpui_component::input::{
    Input, InputEvent, InputState, MoveDown, MoveUp, TextareaState,
};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::popover::Popover;
use gpui_component::scroll::ScrollableElement;
use gpui_component::spinner::Spinner;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::text::{TextView, TextViewState};
use gpui_component::theme::ActiveTheme;
use gpui_component::{Disableable, Icon, IconName, Sizable, WindowExt};
use crate::image_preview::decode_staged_image;

use threadlane_ui_editor::EditorView;
use threadlane_ui_mirror::MirrorView;
use threadlane_ui_state::{actions::AppAction, controller};
use threadlane_ui_state::{
    AppState, ChatMessageInfo, SessionEvent, MessageRole, SessionAttention, SessionInfo,
    SubagentActivityStatus, ToolActivityInfo, WorkMode, WorkspacePage,
};

use super::composer::*;
use super::context_meter::*;
use super::tool_detail;
use super::markdown::*;
use super::model_picker::{self, ModelPickerDelegate, ModelPickerValue, PickerOwner};
use super::transcript::*;

#[cfg(test)]
use threadlane_ui_kit::environment_changes_label;
fn editor_target_matches_active_work_dir(target: &Path, active: Option<&Path>) -> bool {
    active == Some(target)
}

fn chat_error_summary(error: &str) -> (String, bool) {
    let normalized = error.to_lowercase();
    let expired = ["token_expired", "token has expired", "sign-in expired"]
        .iter()
        .any(|marker| normalized.contains(marker));
    if expired {
        return (
            "Your provider sign-in has expired. Sign in again in Settings → Providers, then resend your message.".into(),
            true,
        );
    }
    let rejected = ["invalid_api_key", "http 401", "401 unauthorized"]
        .iter()
        .any(|marker| normalized.contains(marker));
    if rejected {
        return (
            "The provider rejected your credentials. Check your sign-in or API key in Settings → Providers, then resend your message.".into(),
            true,
        );
    }
    if normalized.contains("generation ended without a durable agentend event") {
        return ("The response ended unexpectedly.".into(), false);
    }
    let first_line = error
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let mut chars = first_line.trim().chars();
    let mut summary: String = chars.by_ref().take(240).collect();
    if chars.next().is_some() {
        summary.push('…');
    }
    if summary.is_empty() {
        summary = "The turn stopped without an error description.".into();
    }
    (summary, false)
}

fn visible_session_status<'a>(
    status: Option<&'a str>,
    last_message: Option<&ChatMessageInfo>,
) -> Option<&'a str> {
    status.filter(|status| {
        !status.trim().is_empty()
            && !matches!(*status, "Working…" | "Reconciling session…")
            && !last_message.is_some_and(|message| {
                message.role == MessageRole::Error && message.content == *status
            })
    })
}

/// Compact but unambiguous checkout path: worktrees inside the project root
/// render relative to it (`.threadlane/worktrees/<name>`), anything else
/// shrinks `$HOME`/`%USERPROFILE%` to `~`.
fn session_checkout_display(session: &SessionInfo) -> String {
    if let Ok(relative) = session.runtime_work_dir.strip_prefix(&session.work_dir) {
        if relative.components().next().is_some() {
            return relative.display().to_string();
        }
    }
    let display = session.runtime_work_dir.to_string_lossy().into_owned();
    for variable in ["HOME", "USERPROFILE"] {
        let Some(home) = std::env::var_os(variable) else {
            continue;
        };
        let home = home.to_string_lossy();
        if !home.is_empty() && display.starts_with(home.as_ref()) {
            let rest = &display[home.len()..];
            if rest.is_empty() {
                return "~".to_string();
            }
            if rest.starts_with(['/', '\\']) {
                return format!("~{rest}");
            }
        }
    }
    display
}

use threadlane_ui_kit::format_run_elapsed;

fn has_sendable_prompt(text: &str, image_count: usize) -> bool {
    !text.trim().is_empty() || image_count > 0
}

/// Queue entry id embedded in an optimistic `queued-user-{session}-{entry}`
/// echo; `None` for echoes that carry no durable entry to act on.
fn queued_entry_id(message_id: &str, session_id: Option<&str>) -> Option<String> {
    let prefix = format!("queued-user-{}-", session_id?);
    message_id
        .strip_prefix(&prefix)
        .filter(|entry_id| !entry_id.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
use threadlane_ui_kit::truncate_preview_text as truncate_plan_step;

use threadlane_ui_kit::tool_group_summary;

#[cfg(test)]
use threadlane_ui_kit::plan_tracker_texts;

#[cfg(test)]
use threadlane_ui_kit::{reasoning_token_badge, tool_activity_glyph};

fn progress_header_prefix(is_error: bool) -> Option<&'static str> {
    is_error.then_some("Needs attention:")
}

#[cfg(test)]
use threadlane_ui_kit::skills_chip_label;

fn is_current_project(active: Option<&PathBuf>, candidate: &Path) -> bool {
    active.is_some_and(|dir| dir.as_path() == candidate)
}

fn retry_failed_prompt(
    state: &mut AppState,
    session_id: &str,
    work_dir: &std::path::Path,
    error_id: &str,
    payload: &threadlane_protocol::RetryPrompt,
) {
    if state.active_session_id.as_deref() != Some(session_id)
        || state.active_work_dir.as_deref() != Some(work_dir)
        || state.is_generating
        || !payload.is_sendable()
        || !state.messages.iter().any(|message| {
            message.id == error_id
                && message.role == MessageRole::Error
                && message.retry_prompt.as_ref() == Some(payload)
        })
    {
        return;
    }
    controller::dispatch(
        state,
        AppAction::SendPromptWithImages {
            text: payload.text.clone(),
            images: payload.images.clone(),
        },
    );
}

fn render_chat_error(
    id: &str,
    error: &str,
    retry_prompt: Option<threadlane_protocol::RetryPrompt>,
    model: &Entity<AppState>,
    cx: &App,
) -> Div {
    let (summary, needs_provider_settings) = chat_error_summary(error);
    let details = error.to_owned();
    let retry_prompt = retry_prompt.filter(threadlane_protocol::RetryPrompt::is_sendable);
    let identity = model
        .read(cx)
        .active_session_id
        .clone()
        .zip(model.read(cx).active_work_dir.clone());
    let can_retry = retry_prompt.is_some() && identity.is_some() && !model.read(cx).is_generating;
    div().w_full().my_2().px_4().child(
        threadlane_ui_kit::chat_error_card(summary, cx).child(
            div()
                .mt_2()
                .flex()
                .items_center()
                .gap_2()
                .children(can_retry.then(|| {
                    let payload = retry_prompt
                        .clone()
                        .expect("retry gated on an exact payload");
                    let error_id = id.to_owned();
                    let identity = identity.clone().expect("retry gated on session identity");
                    let model = model.clone();
                    Button::new(SharedString::from(format!("chat-error-retry-{id}")))
                        .label("Resend")
                        .small()
                        .rounded_md()
                        .tooltip("Resend the failed message")
                        .accessibility_label("Resend the failed message")
                        .debug_selector(|| "chat-error-retry".into())
                        .on_click(move |_, _, cx| {
                            model.update(cx, |state, cx| {
                                retry_failed_prompt(
                                    state,
                                    &identity.0,
                                    &identity.1,
                                    &error_id,
                                    &payload,
                                );
                                cx.notify();
                            });
                        })
                }))
                .children(needs_provider_settings.then(|| {
                    let model = model.clone();
                    Button::new(SharedString::from(format!("chat-error-settings-{id}")))
                        .label("Settings…")
                        .accessibility_label("Open provider settings")
                        .small()
                        .rounded_md()
                        .tooltip("Open provider settings")
                        .debug_selector(|| "chat-error-settings".into())
                        .on_click(move |_, _, cx| {
                            model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::OpenSettings);
                                cx.notify();
                            });
                        })
                }))
                .child(
                    Button::new(SharedString::from(format!("chat-error-copy-{id}")))
                        .icon(IconName::Copy)
                        .accessibility_label("Copy full error to clipboard")
                        .ghost()
                        .small()
                        .rounded_md()
                        .tooltip("Copy full error to clipboard")
                        .debug_selector(|| "chat-error-copy".into())
                        .on_click(move |_, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(details.clone()));
                            window
                                .push_notification(Notification::info("Copied error details"), cx);
                        }),
                ),
        ),
    )
}
use threadlane_coding_agent::commands::{available_slash_commands, SlashCommandInfo};
use threadlane_protocol::{ImageAttachment, SessionPlan};

actions!(
    threadlane_composer,
    [
        PasteClipboard,
        CompleteSlashCommand,
        SelectPreviousSlashCommand,
        SelectNextSlashCommand,
        DismissSlashCommand,
    ]
);

actions!(threadlane_chat, [QuoteSelection]);

#[path = "quote_selection.rs"]
mod quote_selection;
use quote_selection::*;

#[path = "conversation_find.rs"]
mod conversation_find;
#[path = "file_completion.rs"]
mod file_completion;
#[path = "prompt_navigation.rs"]
mod prompt_navigation;
pub use conversation_find::ConversationFindHandoff;
use conversation_find::*;
pub use threadlane_ui_kit::{RecallOlderPrompt, RecallNewerPrompt};
use file_completion::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CentralTab {
    #[default]
    Chat,
    Editor,
}

pub fn init(cx: &mut App) {
    // gpui-component's Textarea owns the focused `Input` context. Register
    // after gpui-component initialization so this action can inspect image
    // clipboard entries while preserving text paste behavior.
    cx.bind_keys([
        KeyBinding::new("cmd-v", PasteClipboard, Some(INPUT_KEY_CONTEXT)),
        KeyBinding::new("ctrl-v", PasteClipboard, Some(INPUT_KEY_CONTEXT)),
        KeyBinding::new(
            "tab",
            CompleteSlashCommand,
            Some(SLASH_COMMAND_BINDING_CONTEXT),
        ),
        KeyBinding::new(
            "up",
            SelectPreviousSlashCommand,
            Some(SLASH_COMMAND_BINDING_CONTEXT),
        ),
        KeyBinding::new(
            "down",
            SelectNextSlashCommand,
            Some(SLASH_COMMAND_BINDING_CONTEXT),
        ),
        KeyBinding::new(
            "escape",
            DismissSlashCommand,
            Some(SLASH_COMMAND_BINDING_CONTEXT),
        ),
    ]);
    threadlane_ui_kit::init_prompt_recall(cx);
    init_conversation_find(cx);
    init_file_completion(cx);
}

type ComposerKey = (Option<PathBuf>, Option<String>);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImagePreviewMode {
    Fit,
    ActualSize,
}

struct ImagePreviewState {
    generation: u64,
    composer_key: ComposerKey,
    image_index: usize,
    display_name: String,
    data_url: String,
    decoded: Option<Result<Arc<RenderImage>, String>>,
    mode: ImagePreviewMode,
    initiating_focus: FocusHandle,
}

#[derive(Clone)]
struct PromptRecallState {
    /// Transcript message whose text is currently loaded in the composer.
    landmark_id: String,
    /// The loaded text; any edit to the composer ends browsing.
    applied_text: String,
}

pub struct ChatListView {
    model: Entity<AppState>,
    /// Searchable model picker state + the project/session its snapshot was
    /// captured for; recreated when that owner changes so navigation can
    /// never commit a stale row into a different context.
    pub(crate) model_picker: Option<Entity<ComboboxState<ModelPickerDelegate>>>,
    model_picker_owner: Option<PickerOwner>,
    /// Written by the picker's trigger renderer each frame: its snapshot is
    /// refreshed from live state only while the popup is closed.
    model_picker_open: std::rc::Rc<std::cell::Cell<bool>>,
    model_picker_subscription: Option<Subscription>,
    #[cfg(test)]
    reasoning_menu_open: std::rc::Rc<std::cell::Cell<bool>>,
    pub input_state: Entity<TextareaState>,
    pub header_left_padding: Pixels,
    environment_available: bool,
    transcript: threadlane_ui_kit::transcript::TranscriptState,
    find_open: bool,
    find_input: Entity<InputState>,
    find_query: String,
    find_results: Vec<ConversationMatch>,
    find_selected: Option<String>,
    find_previous_focus: Option<FocusHandle>,
    chat_focus: FocusHandle,
    find_generation: u64,
    find_pending: bool,
    find_source: Option<(Arc<Vec<ChatMessageInfo>>, bool, bool)>,
    find_session: (Option<PathBuf>, Option<String>),
    find_task: Option<Task<()>>,
    /// Conversation-search result waiting for its session to hydrate before
    /// seeding the find strip.
    pending_find_handoff: Option<ConversationFindHandoff>,

    prompt_recall: Option<PromptRecallState>,
    outline_open: bool,
    outline_focus: FocusHandle,
    outline_list_state: ListState,
    prompt_rail_list_state: ListState,
    prompt_rail_active_id: Option<String>,
    outline_landmarks: Vec<PromptLandmark>,
    /// Prompt landmarks memoized against the messages `Arc` + generation
    /// flag; holding the `Arc` itself makes pointer-equality invalidation
    /// sound (any `Arc::make_mut` on the model reallocates).
    prompt_landmarks_cache: Option<(Arc<Vec<ChatMessageInfo>>, bool, Arc<Vec<PromptLandmark>>)>,
    outline_focus_id: Option<String>,
    outline_selected_id: Option<String>,

    expanded_activity_groups: HashSet<String>,
    progress_summary_expanded: bool,
    markdown_states: HashMap<(SharedString, String), MarkdownRenderState>,
    /// Selection snapshots armed per assistant message id for Quote controls.
    /// Pressing the Quote control clears the window selection in the capture
    /// phase, so the render-time snapshot must survive until the press's
    /// bubble-phase mouse-down consumes it; a later render that sees no (or
    /// a different) selection drops it so the action cannot replay a stale
    /// selection. `Rc` because the context menu closure re-evaluates
    /// eligibility without access to `self`.
    armed_quotes: std::rc::Rc<std::cell::RefCell<HashMap<String, QuoteSnapshot>>>,
    markdown_cache_namespace: SharedString,
    pasted_images: Vec<ImageAttachment>,
    image_preview: Option<ImagePreviewState>,
    image_preview_generation: u64,
    composer_key: ComposerKey,
    last_session_key: Option<(std::path::PathBuf, String)>,
    initial_scroll_frames: u8,
    current_tab: CentralTab,
    editor: Entity<EditorView>,

    /// The computer-use mirror floating over the chat while
    /// `AppState::mirror_open`, plus the observation that redraws this view
    /// when the mirror closes itself.
    mirror: Option<(Entity<MirrorView>, Subscription)>,
    slash_command_cache: Option<(
        Option<std::path::PathBuf>,
        std::time::Instant,
        Vec<SlashCommandInfo>,
    )>,
    slash_scroll_handle: ScrollHandle,
    selected_slash_index: usize,
    dismiss_slash_menu: bool,
    /// `@` file completion picker: transient results, the resolved Git root,
    /// dismissal flag, and a generation guard rejecting late task results.
    file_completion: Option<FileCompletionState>,
    file_completion_generation: u64,
    file_completion_task: Option<Task<()>>,
    file_scroll_handle: ScrollHandle,
    selected_file_index: usize,
    dismiss_file_menu: bool,
    permission_details_request: Option<String>,
    context_meter_open: bool,
    /// Selected options per question, keyed by `request_id\0question_id`.
    /// Options toggle multi-select; Submit sends one answer per question.
    question_selections: std::collections::HashMap<String, Vec<String>>,
    /// Custom-text inputs per question allowing free text, keyed the same way.
    /// Entities are created lazily when the card renders and dropped on
    /// submit/dismiss/session-switch.
    question_inputs: std::collections::HashMap<String, Entity<InputState>>,
    copied_message: Option<(String, std::time::Instant)>,
    copy_feedback_task: Option<Task<()>>,
    expanded_tool_aggregates: HashSet<String>,
    segment_cache: HashMap<String, (String, Vec<MarkdownSegment>)>,
    /// Per-code-block soft-wrap choices for the active conversation, keyed
    /// by message id then fenced-block index. View-only: wrapped text never
    /// feeds Copy/Run. Reset on session change, pruned with removed
    /// messages, and dropped when a message's content is replaced (rather
    /// than appended to) so a reused index cannot wrap unrelated code.
    code_wrap_blocks: HashMap<String, HashSet<usize>>,
    _subscriptions: Vec<Subscription>,
}
/// Maximum chat-stream events per pump tick: bounds one redraw's work so a
/// hot turn cannot starve the UI; leftovers stay queued for the next tick.
const CHAT_STREAM_BATCH_LIMIT: usize = 128;
/// How long copy confirmations stay visible on code blocks and message
/// footers; shared so both affordances clear together.
const COPIED_FEEDBACK_WINDOW: std::time::Duration = threadlane_ui_kit::MESSAGE_COPY_FEEDBACK_WINDOW;

async fn next_chat_stream_batch(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
) -> Option<Vec<SessionEvent>> {
    threadlane_ui_state::next_event_batch_capped(receiver, CHAT_STREAM_BATCH_LIMIT).await
}

impl ChatListView {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let transcript = threadlane_ui_kit::transcript::TranscriptState::new(window);
        let transcript_list_state = transcript.list.clone();
        let chat = cx.entity().downgrade();
        transcript_list_state.set_scroll_handler(move |_, _, cx| {
            let _ = chat.update(cx, |_, cx| cx.notify());
        });
        let input_state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Ask a question, or type / for commands, @ for files...")
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .soft_wrap(true)
        });

        let find_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Find in conversation…"));
        let find_subscription = cx.subscribe_in(
            &find_input,
            window,
            |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = input.read(cx).value().to_string();
                    if query != this.find_query {
                        this.find_query = query;
                        this.find_selected = None;
                        this.refresh_conversation_find(true, cx);
                    }
                }
            },
        );
        let mut stream_rx = model
            .update(cx, |state, _cx| state.stream_rx.take())
            .unwrap_or_else(|| {
                // A second view construction must not panic the UI: fall
                // back to a detached channel (no stream events arrive).
                tracing::warn!("chat stream receiver was already taken; using a detached channel");
                tokio::sync::mpsc::unbounded_channel().1
            });

        let editor = cx.new(|cx| EditorView::new(model.clone(), window, cx));

        let sub1 = cx.observe_in(&model, window, |this, model, window, cx| {
            this.sync_composer_draft(window, cx);
            // Cross-surface composer inserts (browser annotations): append
            // without disturbing already-typed input or staged attachments.
            // Session-scoped inserts wait for their session to be active.
            let inserts = model.update(cx, |state, _cx| {
                let active_session = state.active_session_id.clone();
                let active_work_dir = state.active_work_dir.clone();
                let (ready, waiting): (Vec<_>, Vec<_>) =
                    std::mem::take(&mut state.requested_composer_inserts)
                        .into_iter()
                        .partition(|insert| {
                            insert.session_id.is_none()
                                || (insert.session_id == active_session
                                    && insert.work_dir == active_work_dir)
                        });
                state.requested_composer_inserts = waiting;
                ready
            });
            for insert in inserts {
                if !insert.text.is_empty() || !insert.images.is_empty() {
                    this.prompt_recall = None;
                }
                if !insert.text.is_empty() {
                    this.input_state.update(cx, |input, cx| {
                        let existing = input.value().to_string();
                        let separator = if existing.is_empty() || existing.ends_with('\n') {
                            ""
                        } else {
                            "\n"
                        };
                        input.set_value(
                            format!("{existing}{separator}{}", insert.text),
                            window,
                            cx,
                        );
                    });
                }
                this.pasted_images.extend(insert.images);
            }
            this.retain_prompt_recall(cx);
            if let Some(target) =
                model.update(cx, |state, _cx| state.requested_editor_target.take())
            {
                match target {
                    threadlane_ui_state::RequestedEditorTarget::SearchFile {
                        project, path, line, owner_project, owner_session, daemon_identity, connection_epoch,
                    } => {
                        let state = model.read(cx);
                        let active = state.daemon_client.supports_file_search()
                            && state.daemon_client.file_search_connection_epoch() == connection_epoch
                            && state.client.active_work_dir == owner_project
                            && state.client.active_session_id == owner_session
                            && std::sync::Arc::as_ptr(&state.daemon_client) as *const () as usize == daemon_identity
                            && state.active_git_work_dir().as_ref() == Some(&project);
                        if active {
                            this.set_tab(CentralTab::Editor, cx);
                            this.editor.update(cx, |editor, cx| editor.open_file_at_line(project, &path, Some(line), cx));
                        }
                    }
                    threadlane_ui_state::RequestedEditorTarget::File { project, path, line } => {
                        let is_active = {
                            let state = model.read(cx);
                            editor_target_matches_active_work_dir(
                                &project,
                                state.active_git_work_dir().as_deref(),
                            )
                        };
                        if is_active {
                            this.set_tab(CentralTab::Editor, cx);
                            this.editor.update(cx, |editor, cx| {
                                editor.open_file_at_line(project, &path, line, cx);
                            });
                        }
                    }
                    threadlane_ui_state::RequestedEditorTarget::Diff {
                        project,
                        path,
                        content,
                    } => {
                        let is_active = {
                            let state = model.read(cx);
                            editor_target_matches_active_work_dir(
                                &project,
                                state.active_git_work_dir().as_deref(),
                            )
                        };
                        if is_active {
                            this.set_tab(CentralTab::Editor, cx);
                            this.editor.update(cx, |editor, cx| {
                                editor.open_diff(&path, &content, cx);
                            });
                        }
                    }
                }
            }
            cx.notify();
            this.progress_find_handoff(window, cx);
            this.refresh_conversation_find(false, cx);
        });

        let sub_editor = cx.observe(&editor, |_this, _editor, cx| {
            cx.notify();
        });

        let sub_editor_selection = cx.subscribe_in(
            &editor,
            window,
            |this, _editor, request: &threadlane_ui_kit::EditorSelectionRequest, window, cx| {
                this.insert_editor_selection(request.clone(), window, cx);
            },
        );

        let model_clone = model.clone();
        let submit_list_state = transcript_list_state.clone();
        let sub2 = cx.subscribe_in(
            &input_state,
            window,
            move |this, input_state, event: &InputEvent, window, cx| {
                cx.notify();
                match event {
                    InputEvent::Change => {
                        this.prompt_recall = None;
                        this.dismiss_slash_menu = false;
                        this.selected_slash_index = 0;
                        this.slash_scroll_handle.scroll_to_item(0);
                        this.dismiss_file_menu = false;
                        this.sync_file_completion(cx);
                    }
                    InputEvent::PressEnter {
                        secondary,
                        shift: false,
                    } => {
                        if model_clone.read(cx).active_worktree_setup().is_some() { return; }
                        // An open `@` picker owns Enter in every state — apply
                        // a valid result or swallow; never Send/Queue/Steer.
                        if this.file_menu_open(cx) {
                            this.apply_selected_file_completion(window, cx);
                            return;
                        }
                        let text = input_state.read(cx).value().to_string();
                        let is_generating = model_clone.read(cx).is_generating;
                        let project_root = model_clone.read(cx).active_work_dir.clone();

                        if let Some(query) = active_slash_command_query(&text) {
                            if !this.dismiss_slash_menu {
                                let matching = this
                                    .cached_slash_commands(project_root.as_deref())
                                    .into_iter()
                                    .filter(|cmd| query.is_empty() || cmd.name.starts_with(query))
                                    .collect::<Vec<_>>();
                                if !matching.is_empty() {
                                    let selected = this
                                        .selected_slash_index
                                        .min(matching.len().saturating_sub(1));
                                    let command_name = matching[selected].name.clone();
                                    this.complete_slash_command(&command_name, window, cx);
                                    return;
                                }
                            }
                        }

                        if !text.trim().is_empty()
                            || (!is_generating && !this.pasted_images.is_empty())
                        {
                            if is_generating && *secondary
                                && threadlane_acp_engine::is_acp_model(&model_clone.read(cx).selected_model)
                            {
                                model_clone.update(cx, |state, cx| {
                                    state.session_status = Some("This agent does not support live steering. Send your message to queue it after this turn.".into());
                                    cx.notify();
                                });
                                return;
                            }
                            let images = std::mem::take(&mut this.pasted_images);
                            let is_steer = *secondary;
                            model_clone.update(cx, |state, cx| {
                                if is_generating {
                                    controller::dispatch(
                                        state,
                                        AppAction::StageBusyMessage { text, images },
                                    );
                                    if is_steer {
                                        controller::dispatch(state, AppAction::SteerPendingMessage);
                                    } else {
                                        controller::dispatch(state, AppAction::QueuePendingMessage);
                                    }
                                } else {
                                    controller::dispatch(
                                        state,
                                        AppAction::SendPromptWithImages { text, images },
                                    );
                                }
                                cx.notify();
                            });
                            this.prompt_recall = None;
                            input_state.update(cx, |state, cx| {
                                state.set_value("", window, cx);
                            });
                            submit_list_state.scroll_to_end();
                            cx.notify();
                        }
                    }
                    _ => {}
                }
            },
        );

        let stream_model = model.clone();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            if this
                .update(cx, |view, cx| {
                    // The chat pump only wakes on stream events, so session_seen
                    // write results are also observed here — a failed save
                    // otherwise sits unobserved until the next chat event. The
                    // same holds for session_snooze writes: without this a
                    // settled session's snooze stays "Saving snooze…" until an
                    // unrelated event drains the acknowledgment.
                    let writes_changed = view.model.update(cx, |state, _| {
                        state.drain_session_seen_write_results()
                            | state.drain_session_snooze_write_results()
                    });
                    if view.model.read(cx).is_generating || writes_changed {
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            // Dev hook: `THREADLANE_MIRROR_DEBUG=1` opens the mirror at
            // launch, so the popup can be observed without a model turn or
            // an approval prompt. It shows the last screenshot sidecar until
            // the first computer call lands. Nothing reaches a model through
            // this path.
            if std::env::var_os("THREADLANE_MIRROR_DEBUG").is_some() {
                let _ = this.update(cx, |view, cx| view.open_mirror(cx));
            }
            while let Some(events) = next_chat_stream_batch(&mut stream_rx).await {
                let changed = stream_model.update(cx, |state, cx| {
                    let changed = state.drain_chat_stream(events);
                    if changed {
                        cx.notify();
                    }
                    changed
                });
                let mirror_trigger =
                    stream_model.update(cx, |state, _cx| state.take_computer_mirror_trigger());
                if mirror_trigger {
                    let _ = this.update(cx, |view, cx| view.open_mirror(cx));
                }
                cx.background_executor()
                    .timer(Duration::from_millis(30))
                    .await;
                if changed && stream_rx.is_empty() {
                    let _ = this.update(cx, |_this, cx| cx.notify());
                }
            }
        })
        .detach();

        let composer_key = {
            let state = model.read(cx);
            (
                state.active_work_dir.clone(),
                state.active_session_id.clone(),
            )
        };
        Self {
            model,
            model_picker: None,
            model_picker_owner: None,
            model_picker_open: Default::default(),
            model_picker_subscription: None,
            #[cfg(test)]
            reasoning_menu_open: Default::default(),
            input_state,
            header_left_padding: px(14.0),
            environment_available: false,
            transcript,
            find_open: false,
            find_input,
            find_query: String::new(),
            find_results: Vec::new(),
            find_selected: None,
            find_previous_focus: None,
            chat_focus: cx.focus_handle(),
            find_generation: 0,
            find_pending: false,
            find_source: None,
            find_session: (None, None),
            find_task: None,
            pending_find_handoff: None,
            prompt_recall: None,
            outline_open: false,
            outline_focus: cx.focus_handle(),
            outline_list_state: ListState::new(0, ListAlignment::Top, window.rem_size() * 20.0),
            prompt_rail_list_state: ListState::new(0, ListAlignment::Top, window.rem_size()),
            prompt_rail_active_id: None,
            outline_landmarks: Vec::new(),
            prompt_landmarks_cache: None,
            outline_focus_id: None,
            outline_selected_id: None,
            expanded_activity_groups: HashSet::new(),
            progress_summary_expanded: false,
            markdown_states: HashMap::new(),
            armed_quotes: Default::default(),
            markdown_cache_namespace: SharedString::from(""),
            pasted_images: Vec::new(),
            image_preview: None,
            image_preview_generation: 0,
            composer_key,
            last_session_key: None,
            initial_scroll_frames: 0,
            current_tab: CentralTab::Chat,
            editor,
            mirror: None,
            slash_command_cache: None,
            slash_scroll_handle: ScrollHandle::new(),
            selected_slash_index: 0,
            dismiss_slash_menu: false,
            file_completion: None,
            file_completion_generation: 0,
            file_completion_task: None,
            file_scroll_handle: ScrollHandle::new(),
            selected_file_index: 0,
            dismiss_file_menu: false,
            permission_details_request: None,
            context_meter_open: false,
            question_selections: std::collections::HashMap::new(),
            question_inputs: std::collections::HashMap::new(),
            copied_message: None,
            copy_feedback_task: None,
            expanded_tool_aggregates: HashSet::new(),
            segment_cache: HashMap::new(),
            code_wrap_blocks: HashMap::new(),
            _subscriptions: vec![sub1, sub2, sub_editor, sub_editor_selection, find_subscription],
        }
    }

    fn sync_composer_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = {
            let state = self.model.read(cx);
            (
                state.active_work_dir.clone(),
                state.active_session_id.clone(),
            )
        };
        if key == self.composer_key {
            return;
        }
        self.prompt_recall = None;
        self.clear_file_completion();
        self.invalidate_image_preview(window, cx);

        // An explicit stash is separate from the unsent text and attachments in each task.
        let draft = ComposerDraft {
            text: self.input_state.read(cx).value().to_string(),
            images: std::mem::take(&mut self.pasted_images),
        };
        let previous = std::mem::replace(&mut self.composer_key, key);
        if !draft.text.is_empty() || !draft.images.is_empty() {
            self.model.update(cx, |state, _| { state.client.composer_drafts.insert(previous, draft); });
        }
        let draft = self.model.update(cx, |state, _| state.client.composer_drafts.remove(&self.composer_key).unwrap_or_default());
        self.pasted_images = draft.images;
        self.input_state.update(cx, |input, cx| {
            input.set_value(draft.text, window, cx);
        });
    }

    /// Whether the central Editor surface is showing (command-surface
    /// targeting for **Add selection to chat**).
    pub fn editor_is_current_tab(&self) -> bool {
        self.current_tab == CentralTab::Editor
    }

    /// Reason the embedded editor cannot currently hand a selection to the
    /// draft — `None` when ready.
    pub fn editor_selection_block_reason(&self, cx: &App) -> Option<SharedString> {
        self.editor.read(cx).selection_block_reason(cx)
    }

    /// Palette/command-surface entry for the embedded editor's **Add
    /// selection to chat**: runs the same capture+emit path the header
    /// button uses, so every activation flows through
    /// `insert_editor_selection`.
    pub fn add_editor_selection_to_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.request_add_selection_to_chat(window, cx)
        });
    }

    /// Appends a captured editor selection to the draft it names. The
    /// source buffer, selection range, checkout, and destination key were
    /// captured together at activation and are revalidated here: a stale
    /// tab, moved selection, or raced session/project switch leaves the
    /// draft and the editor untouched.
    fn insert_editor_selection(
        &mut self,
        request: threadlane_ui_kit::EditorSelectionRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let still_current = self
            .editor
            .update(cx, |editor, cx| editor.selection_request_is_current(&request, cx));
        let checkout_active = self.model.read(cx).active_git_work_dir().as_ref()
            == Some(&request.checkout);
        if !still_current || !checkout_active {
            window.push_notification(
                Notification::info("The editor selection changed — it was not added"),
                cx,
            );
            return;
        }
        let excerpt = threadlane_ui_kit::format_editor_excerpt(
            &request.relative_path,
            &request.snapshot,
            request.dirty,
        );
        if !self.append_draft_text_for(request.destination.clone(), &excerpt, window, cx) {
            window.push_notification(
                Notification::info("The chat draft changed — the selection was not added"),
                cx,
            );
        }
    }

    fn preview_attachment_is_current(&self, preview: &ImagePreviewState) -> bool {
        self.current_tab == CentralTab::Chat
            && self.composer_key == preview.composer_key
            && self
                .pasted_images
                .get(preview.image_index)
                .is_some_and(|image| {
                    image.display_name == preview.display_name && image.data_url == preview.data_url
                })
    }

    fn invalidate_image_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.image_preview.take().is_some() {
            self.image_preview_generation = self.image_preview_generation.wrapping_add(1);
            window.close_dialog(cx);
        }
    }

    fn open_image_preview(
        &mut self,
        image_index: usize,
        initiating_focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.current_tab != CentralTab::Chat
            || self.image_preview.is_some()
            || window.has_active_dialog(cx)
        {
            return;
        }
        let Some(attachment) = self.pasted_images.get(image_index).cloned() else {
            return;
        };

        self.image_preview_generation = self.image_preview_generation.wrapping_add(1);
        let generation = self.image_preview_generation;
        let composer_key = self.composer_key.clone();
        self.image_preview = Some(ImagePreviewState {
            generation,
            composer_key: composer_key.clone(),
            image_index,
            display_name: attachment.display_name.clone(),
            data_url: attachment.data_url.clone(),
            decoded: None,
            mode: ImagePreviewMode::Fit,
            initiating_focus: initiating_focus.clone(),
        });

        let decode_attachment = attachment.clone();
        let decode_task = cx.background_spawn(async move { decode_staged_image(&decode_attachment) });
        let completion_chat = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let decoded = decode_task.await;
            let _ = completion_chat.update(cx, |this, cx| {
                let current = this.image_preview.as_ref().is_some_and(|preview| {
                    preview.generation == generation && this.preview_attachment_is_current(preview)
                });
                if current {
                    if let Some(preview) = this.image_preview.as_mut() {
                        preview.decoded = Some(decoded);
                    }
                    cx.notify();
                }
            });
        })
        .detach();

        let content_chat = cx.entity().downgrade();
        let close_chat = content_chat.clone();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let content_chat = content_chat.clone();
            let close_chat = close_chat.clone();
            threadlane_ui_kit::image_preview_dialog(dialog, window)
                .content(move |content, window, cx| {
                    let preview = content_chat
                        .update(cx, |this, cx| {
                            this.render_image_preview_content(generation, window, cx)
                        })
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| {
                            div()
                                .child("This image preview is no longer available.")
                                .into_any_element()
                        });
                    content.child(preview)
                })
                .on_close(move |_, window, cx| {
                    let _ = close_chat.update(cx, |this, cx| {
                        this.finish_image_preview(generation, window, cx);
                    });
                })
        });
        cx.notify();
    }

    fn finish_image_preview(
        &mut self,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = self
            .image_preview
            .as_ref()
            .filter(|preview| preview.generation == generation)
        else {
            return;
        };
        let attachment_is_current = self.preview_attachment_is_current(preview);
        let initiating_focus = preview.initiating_focus.clone();
        self.image_preview = None;
        self.image_preview_generation = self.image_preview_generation.wrapping_add(1);
        if attachment_is_current {
            window.focus(&initiating_focus, cx);
        } else if self.current_tab == CentralTab::Chat {
            self.input_state.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    fn render_image_preview_content(
        &mut self,
        generation: u64,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let preview = self.image_preview.as_ref().filter(|preview| {
            preview.generation == generation && self.preview_attachment_is_current(preview)
        })?;
        let display_name = preview.display_name.clone();
        let decoded = preview.decoded.as_ref().map(|result| match result {
            Ok(image) => Ok(image.clone()),
            Err(error) => Err(error.clone()),
        });
        let mode = preview.mode;
        let dimensions = decoded
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|image| {
                let size = image.size(0);
                (size.width.0 as u32, size.height.0 as u32)
            });
        let mode_controls = dimensions.map(|(width, height)| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    threadlane_ui_kit::image_preview_fit_button(
                        ("image-preview-fit", generation),
                        mode == ImagePreviewMode::Fit,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(preview) = this
                            .image_preview
                            .as_mut()
                            .filter(|preview| preview.generation == generation)
                        {
                            preview.mode = ImagePreviewMode::Fit;
                            cx.notify();
                        }
                    })),
                )
                .child(
                    threadlane_ui_kit::image_preview_actual_size_button(
                        ("image-preview-actual-size", generation),
                        mode == ImagePreviewMode::ActualSize,
                        width,
                        height,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(preview) = this
                            .image_preview
                            .as_mut()
                            .filter(|preview| preview.generation == generation)
                        {
                            preview.mode = ImagePreviewMode::ActualSize;
                            cx.notify();
                        }
                    })),
                )
        });
        Some(
            threadlane_ui_kit::image_preview_content(
                display_name,
                decoded,
                mode == ImagePreviewMode::ActualSize,
                mode_controls.map(|controls| controls.into_any_element()),
                window,
                cx,
            )
            .into_any_element(),
        )
    }

    fn paste_composer_clipboard(
        &mut self,
        _action: &PasteClipboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(clipboard) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(text) = clipboard.text().filter(|text| !text.is_empty()) {
            self.input_state.update(cx, |input, cx| {
                input.insert(text, window, cx);
            });
        }

        let mut pasted = 0;
        for entry in clipboard.entries {
            let ClipboardEntry::Image(image) = entry else {
                continue;
            };
            if image.bytes.is_empty() {
                continue;
            }

            let mime_type = image.format.mime_type();
            let extension = mime_type.strip_prefix("image/").unwrap_or("png");
            self.pasted_images.push(ImageAttachment {
                display_name: format!("Pasted image {}.{extension}", self.pasted_images.len() + 1),
                data_url: format!(
                    "data:{mime_type};base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(image.bytes)
                ),
            });
            pasted += 1;
        }

        cx.stop_propagation();
        if pasted > 0 {
            cx.notify();
        }
    }

    pub fn set_tab(&mut self, tab: CentralTab, cx: &mut Context<Self>) {
        if tab != self.current_tab && self.image_preview.is_some() {
            self.image_preview = None;
            self.image_preview_generation = self.image_preview_generation.wrapping_add(1);
        }
        if tab != CentralTab::Chat {
            self.clear_conversation_find();
            self.prompt_recall = None;
            self.clear_file_completion();
            self.outline_open = false;
            self.outline_selected_id = None;
        }
        self.current_tab = tab;
        cx.notify();
    }

    pub fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.current_tab = CentralTab::Chat;
        self.input_state.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Applies the row the model picker committed.
    ///
    /// The row was captured when the popup opened, so it is revalidated
    /// against live state first: a project/session change makes the whole
    /// snapshot invalid (the picker is also recreated on the next render,
    /// this just guards a commit that already landed), and a choice whose
    /// exact identity disappeared surfaces an error instead of silently
    /// retargeting a different model.
    fn commit_model_picker_choice(
        &mut self,
        value: ModelPickerValue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let ModelPickerValue::OpenAgentSettings = value {
            self.model.update(cx, |state, cx| {
                controller::dispatch(state, AppAction::OpenSettings);
                cx.notify();
            });
            return;
        }
        if let ModelPickerValue::RefreshModels = value {
            // Clear every cached provider inventory and re-fetch, so a newly
            // released model reaches this picker without a restart or TTL wait.
            const REFRESH_PENDING: &str = "Refreshing model lists…";
            const REFRESH_DONE: &str = "Model lists refreshed.";
            let model = self.model.clone();
            let project = self.model.read(cx).active_work_dir.clone();
            self.model.update(cx, |state, cx| {
                state.session_status = Some(REFRESH_PENDING.into());
                cx.notify();
            });
            cx.spawn(async move |_, cx| {
                threadlane_daemon::catalog::refresh_all_models(project).await;
                let _ = cx.update(|cx| {
                    model.update(cx, |state, cx| {
                        state.refresh_available_models();
                        // A session switch or new turn may have replaced the
                        // marker: only the still-showing refresh message gets
                        // the completion, never whatever status owns the slot now.
                        if state.session_status.as_deref() == Some(REFRESH_PENDING) {
                            state.session_status = Some(REFRESH_DONE.into());
                        }
                        cx.notify();
                    })
                });
            })
            .detach();
            cx.defer_in(window, |this, window, cx| this.focus_composer(window, cx));
            return;
        }
        let owner_current = self
            .model_picker_owner
            .as_ref()
            .is_some_and(|owner| owner.is_current(self.model.read(cx)));
        if !owner_current || model_picker::choice_is_stale(&value, self.model.read(cx)) {
            self.model.update(cx, |state, cx| {
                state.session_status = Some(model_picker::STALE_CHOICE_MESSAGE.to_string());
                cx.notify();
            });
            return;
        }
        self.model.update(cx, |state, cx| {
            match &value {
                ModelPickerValue::Model(id) => {
                    controller::dispatch(state, AppAction::SelectModel(id.clone()));
                }
                ModelPickerValue::AgentChoice {
                    model_id,
                    config_id,
                    value: choice,
                } => {
                    controller::dispatch(state, AppAction::SelectModel(model_id.clone()));
                    // `SetAcpConfigOption` targets `selected_model`'s agent,
                    // so it may only fire when the agent switch actually
                    // took effect — otherwise the config would land on the
                    // agent that was selected before the commit.
                    if state.selected_model == *model_id {
                        controller::dispatch(
                            state,
                            AppAction::SetAcpConfigOption {
                                config_id: config_id.clone(),
                                value: choice.clone(),
                            },
                        );
                    }
                }
                ModelPickerValue::OpenAgentSettings | ModelPickerValue::RefreshModels => {}
            }
            cx.notify();
        });
        // The kit refocuses the trigger when the popup closes; hand focus to
        // the composer right after so typing continues where the user left it.
        cx.defer_in(window, |this, window, cx| this.focus_composer(window, cx));
    }

    /// Appends `text` to the draft `destination` names, then reveals Chat
    /// and focuses the composer. `destination` is re-validated against the
    /// live composer key after draft synchronization, so a session or
    /// project switch that raced the caller leaves the draft — and the
    /// payload — untouched and this returns `false`.
    ///
    /// The append is ordinary undoable draft text (`select_all` +
    /// `replace`, not `set_value`, which would silently clear the draft's
    /// undo history): it preserves staged images and the clipboard,
    /// clears prompt-recall and file completion like any external edit,
    /// and never sends, queues, or stages anything.
    pub fn append_draft_text_for(
        &mut self,
        destination: ComposerKey,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.sync_composer_draft(window, cx);
        if text.is_empty() || self.composer_key != destination {
            return false;
        }
        self.prompt_recall = None;
        self.clear_file_completion();
        self.input_state.update(cx, |input, cx| {
            let existing = input.value().to_string();
            let separator = if existing.is_empty() || existing.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            input.select_all(window, cx);
            input.replace(format!("{existing}{separator}{text}"), window, cx);
        });
        self.set_tab(CentralTab::Chat, cx);
        self.focus_composer(window, cx);
        true
    }

    /// Session titles generated from a linked issue start with "#N"; the
    /// sidebar card and environment panel already surface that number, so the
    /// header drops the duplicated prefix.
    fn header_title_without_issue_prefix(title: &str, issue_number: Option<u64>) -> String {
        issue_number
            .and_then(|number| {
                title
                    .strip_prefix(&format!("#{number}"))
                    .map(|rest| rest.trim_start_matches([' ', '·', '-', '–', ':']).to_string())
            })
            .filter(|rest| !rest.is_empty())
            .unwrap_or_else(|| title.to_string())
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (active_title, active_attention, linked_issue) = {
            let state = self.model.read(cx);
            let active_session = state
                .projects
                .iter()
                .flat_map(|project| project.sessions.iter())
                .find(|session| state.active_session_id.as_deref() == Some(&session.id));
            let title = active_session
                .map(threadlane_ui_kit::session_display_title)
                .unwrap_or_else(|| threadlane_ui_kit::UNTITLED_SESSION_TITLE.to_string());
            let attention = active_session
                .map(|session| state.session_attention(session))
                .unwrap_or(SessionAttention::Idle);
            let linked_issue = active_session.and_then(|session| session.github_issue.clone());
            (title, attention, linked_issue)
        };
        let display_title = Self::header_title_without_issue_prefix(
            &active_title,
            linked_issue.as_ref().map(|issue| issue.number),
        );
        let theme = cx.theme().colors;
        let editor_tab_count = self.editor.read(cx).tab_count();
        let editor_label = if editor_tab_count > 0 {
            format!("Editor ({editor_tab_count})")
        } else {
            "Editor".to_string()
        };


        let status_badge = match active_attention {
            SessionAttention::NeedsYou => Some(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py(rems(0.1875))
                    .rounded_full()
                    .bg(theme.warning.opacity(0.12))
                    .text_color(theme.warning)
                    .child(div().size_1p5().rounded_full().bg(theme.warning))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Needs you"),
                    ),
            ),
            // Live progress is shown in the transcript; reserve header badges for attention.
            SessionAttention::Working => None,
            SessionAttention::Ready => Some(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py(rems(0.1875))
                    .rounded_full()
                    .bg(theme.muted.opacity(0.4))
                    .text_color(theme.muted_foreground)
                    .child(div().size_1p5().rounded_full().bg(theme.success))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Ready"),
                    ),
            ),
            SessionAttention::Idle => None,
        };

        threadlane_ui_kit::chat_header_surface(self.header_left_padding, cx)
            .child(
                threadlane_ui_kit::chat_header_identity(
                    display_title, active_title, status_badge.map(IntoElement::into_any_element), cx,
                ),
            )
            .when(self.current_tab == CentralTab::Chat, |el| {
                el.child(
                    threadlane_ui_kit::conversation_find_button()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_conversation_find(&FindInConversation, window, cx)
                        })),
                )
            })
            // Segmented control (t3code/synara pattern): muted segment chips
            // with the active surface raised; kept as direct children so the
            // header's flex_wrap can still stack them on narrow widths.
            .child(
                threadlane_ui_kit::chat_tab_button("central-tab-chat", "Chat", self.current_tab == CentralTab::Chat, cx)
                    .tooltip("Chat (⌘1)")
                    .accessibility_label("Chat (⌘1)")
                    .on_click(cx.listener(|this, _, _, cx| this.set_tab(CentralTab::Chat, cx))),
            )
            .child(
                threadlane_ui_kit::chat_tab_button("central-tab-editor", editor_label.clone(), self.current_tab == CentralTab::Editor, cx)
                    .tooltip("Editor (⌘3)")
                    .accessibility_label(format!(
                        "Editor (⌘3){}",
                        if editor_tab_count > 0 {
                            format!(", {} tabs", editor_tab_count)
                        } else {
                            String::new()
                        }
                    ))
                    .on_click(cx.listener(|this, _, _, cx| this.set_tab(CentralTab::Editor, cx))),
            )
    }

    /// The workspace owns the available width and hides this summary when a tool panel opens.
    pub fn set_environment_width(&mut self, width: Pixels, rem: Pixels, cx: &mut Context<Self>) {
        let available = threadlane_ui_kit::environment_fits(width, rem);
        if self.environment_available != available {
            self.environment_available = available;
            cx.notify();
        }
    }

    fn render_environment(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.model.read(cx);
        let project = state.active_work_dir.as_ref();
        let checkout = state.active_git_work_dir();
        let status = checkout
            .as_ref()
            .and_then(|dir| state.git_statuses.get(dir));
        let name = project
            .and_then(|dir| state.projects.iter().find(|p| p.work_dir == *dir))
            .map(|p| p.name.clone())
            .or_else(|| {
                project
                    .and_then(|dir| dir.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "No project".into());
        let location = match checkout.as_ref() {
            None => "Checkout unavailable",
            Some(dir) if Some(dir) != project => "Worktree",
            Some(_) => "Local",
        };
        let checkout_path = checkout
            .as_ref()
            .map(|dir| threadlane_ui_kit::display_path(dir));
        let menu_model = self.model.clone();
        let model = self.model.clone();
        threadlane_ui_kit::environment_panel(
            name,
            location,
            checkout_path,
            status,
            checkout.is_some(),
            move |menu, _, cx| {
                let state = menu_model.read(cx);
                let status = state
                    .active_git_work_dir()
                    .and_then(|dir| state.git_statuses.get(&dir));
                threadlane_ui_kit::environment_git_menu(
                    menu,
                    status,
                    threadlane_git::can_create_pull_request(
                        state.active_git_work_dir().is_some(),
                        status,
                    ),
                    |label, action| {
                        PopupMenuItem::new(label)
                            .when_some(Self::environment_command(action), |item, action| {
                                item.action(action)
                            })
                    },
                )
            },
            self.render_token_efficiency(cx),
            move |action, window, cx| {
                use threadlane_ui_kit::EnvironmentAction;
                match action {
                    EnvironmentAction::Repository => model.update(cx, |state, cx| {
                        controller::dispatch(state, AppAction::OpenGitHub);
                        cx.notify();
                    }),
                    EnvironmentAction::Terminal => model.update(cx, |state, cx| {
                        if let Some(dir) = state.active_git_work_dir() {
                            controller::dispatch(state, AppAction::OpenTerminalAt(dir));
                            cx.notify();
                        }
                    }),
                    action => Self::dispatch_environment_action(action, window, cx),
                }
            },
            cx,
        )
    }
    fn environment_command(
        action: threadlane_ui_kit::EnvironmentAction,
    ) -> Option<Box<dyn Action>> {
        use threadlane_ui_kit::EnvironmentAction;
        let action: Box<dyn Action> = match action {
            EnvironmentAction::Branches => Box::new(crate::OpenWorkspaceBranches),
            EnvironmentAction::Review => Box::new(crate::OpenWorkspaceReview),
            EnvironmentAction::Commit => Box::new(crate::OpenWorkspaceCommit),
            EnvironmentAction::Pull => Box::new(crate::PullWorkspaceBranch),
            EnvironmentAction::Push => Box::new(crate::PushWorkspaceBranch),
            EnvironmentAction::CreatePullRequest => Box::new(crate::CreateWorkspacePullRequest),
            EnvironmentAction::CreateBranch => Box::new(crate::CreateWorkspaceBranch),
            EnvironmentAction::Files => Box::new(crate::OpenWorkspaceFiles),
            EnvironmentAction::Repository | EnvironmentAction::Terminal => return None,
        };
        Some(action)
    }

    fn dispatch_environment_action(
        action: threadlane_ui_kit::EnvironmentAction,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(action) = Self::environment_command(action) {
            window.dispatch_action(action, cx);
        }
    }

    fn render_token_efficiency(&self, cx: &App) -> AnyElement {
        let state = self.model.read(cx);
        threadlane_ui_kit::token_efficiency(
            state.active_token_efficiency(),
            state.is_generating,
            cx,
        )
    }

    /// Pinned execution context for worktree sessions: a resumed — or
    /// compaction-restored — chat looks identical to a local one while its
    /// agent runs in an isolated checkout, so the path and branch stay
    /// visible above the transcript instead of hiding in tooltips or the
    /// width-gated environment panel.
    fn render_session_context(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.model.read(cx);
        let session = state
            .projects
            .iter()
            .flat_map(|project| project.sessions.iter())
            .find(|session| state.active_session_id.as_deref() == Some(&session.id))?;
        if !session.is_worktree {
            return None;
        }
        let theme = cx.theme().colors;
        let checkout = session.runtime_work_dir.clone();
        let checkout_display = session_checkout_display(session);
        // Live status wins over the discovery snapshot: branch switches in
        // the review panel update `git_statuses` immediately, while
        // `SessionInfo::git_branch` only refreshes on the next scan.
        let branch = state
            .git_statuses
            .get(&session.runtime_work_dir)
            .and_then(|status| status.branch.clone())
            .or_else(|| session.git_branch.clone());
        let available = session.worktree_available;
        let detail = if available {
            format!(
                "This session runs in an isolated worktree{}\nAgent, terminal, and tools use it as the working directory\n{}",
                branch
                    .as_ref()
                    .map(|branch| format!(" on branch {branch}"))
                    .unwrap_or_default(),
                checkout.display()
            )
        } else {
            format!(
                "This session's worktree is not checked out; its recorded path is shown\nSession history remains available\n{}",
                checkout.display()
            )
        };
        let accent = if available {
            theme.muted_foreground
        } else {
            theme.warning
        };
        let aria = format!(
            "Session runs in a worktree{} at {}",
            if available { "" } else { " not checked out" },
            checkout.display(),
        );
        let aria = match branch.as_deref() {
            Some(branch) => format!("{aria}, branch {branch}"),
            None => aria,
        };
        Some(
            div()
                .id("session-worktree-context")
                .debug_selector(|| "session-worktree-context".into())
                .role(Role::Group)
                .aria_label(aria)
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(detail.clone()).build(window, cx)
                })
                .w_full()
                .px_4()
                .py_1p5()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .border_b_1()
                .border_color(theme.border.opacity(0.4))
                .child(
                    Icon::new(IconName::Folder)
                        .xsmall()
                        .flex_none()
                        .text_color(accent),
                )
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(accent)
                        .child(if available {
                            "Worktree"
                        } else {
                            "Not checked out"
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(checkout_display),
                )
                .children(branch.map(|branch| {
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_1p5()
                        .py(rems(0.125))
                        .rounded_full()
                        .bg(theme.muted.opacity(0.3))
                        .child(
                            Icon::default()
                                .path("icons/git/branch.svg")
                                .size(rems(0.6875))
                                .text_color(theme.muted_foreground.opacity(0.85)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.muted_foreground)
                                .max_w(rems(9.0))
                                .truncate()
                                .child(branch),
                        )
                }))
                .children(available.then(|| {
                    Button::new("session-worktree-terminal")
                        .debug_selector(|| "session-worktree-terminal".into())
                        .icon(IconName::SquareTerminal)
                        .label("Terminal")
                        .ghost()
                        .xsmall()
                        .accessibility_label(format!(
                            "Open a terminal in {}",
                            checkout.display()
                        ))
                        .tooltip("Open a terminal in this worktree")
                        .on_click({
                            let model = self.model.clone();
                            move |_, _, cx| {
                                model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::OpenTerminalAt(checkout.clone()),
                                    );
                                    cx.notify();
                                });
                            }
                        })
                }))
                .into_any_element(),
        )
    }

    fn render_workspace_changes(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.model.read(cx);
        let status = state.git_statuses.get(&state.active_git_work_dir()?)?;
        let count = status.files.len();
        if count == 0 {
            return None;
        }
        Some(
            div()
                .debug_selector(|| "workspace-changes-row".into())
                .w_full()
                .max_w(rems(CHAT_CONTENT_MAX_WIDTH))
                .mx_auto()
                .px_4()
                .py_1p5()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(
                    Icon::default()
                        .path("icons/git/compare.svg")
                        .xsmall()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(format!(
                    "Workspace changes · {count} {}",
                    if count == 1 { "file" } else { "files" }
                ))
                // Beside the count it reviews, and outlined so it reads as a control.
                .child(
                    Button::new("review-workspace-changes")
                        .debug_selector(|| "workspace-changes-review".into())
                        .label("Review")
                        .outline()
                        .xsmall()
                        .rounded_full()
                        .accessibility_label(format!(
                            "Review {count} uncommitted {}",
                            if count == 1 { "change" } else { "changes" }
                        ))
                        .tooltip("Review all uncommitted changes in this workspace")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(crate::OpenWorkspaceReview), cx)
                        }),
                )
                .into_any_element(),
        )
    }

    fn render_plan_tracker(&self, plan: &SessionPlan, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.model.read(cx);
        threadlane_ui_kit::plan_tracker(state.active_session_id.as_deref().unwrap_or("draft"), plan, state.is_generating, cx)
    }

    fn render_tool_activity(
        &self,
        activity: &ToolActivityInfo,
        window: &mut Window,
        row_index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let model = self.model.clone();
        let transcript = self.transcript.list.clone();
        let tool_call_id = activity.id.clone();
        let has_detail = !activity.detail.trim().is_empty() || tool_detail::expandable(activity);
        let motion = threadlane_ui_kit::DisclosureMotion::new(
            SharedString::from(format!("tool-body-{}", activity.id)),
            activity.is_expanded,
            window,
            cx,
        );
        motion.remeasure_list_row(&self.transcript.list, row_index, window);
        let detail = motion.is_visible().then(|| {
            tool_detail::render_activity_detail_card(activity, &self.model, cx)
                .unwrap_or_else(|| threadlane_ui_kit::tool_detail(cx)
                    .child(activity.detail.clone()).into_any_element())
        }).map(|body| motion.content(body));
        threadlane_ui_kit::tool_activity(activity, has_detail, detail, false, move |_, cx| {
            transcript.pause_following_tail();
            transcript.remeasure();
            model.update(cx, |state, cx| {
                controller::dispatch(state, AppAction::ToggleToolActivity(tool_call_id.clone()));
                cx.notify();
            });
        }, cx)
    }

    fn render_activity_group(
        &mut self,
        messages: &[ChatMessageInfo],
        window: &mut Window,
        row_index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let activities = grouped_tool_activities(messages);
        let group_id = activities
            .clone()
            .next()
            .map(|activity| activity.id.clone())
            .unwrap_or_else(|| "empty".into());
        let expanded = self.expanded_activity_groups.contains(&group_id);
        let motion = threadlane_ui_kit::DisclosureMotion::new(
            SharedString::from(format!("activity-group-body-{group_id}")),
            expanded,
            window,
            cx,
        );
        motion.remeasure_list_row(&self.transcript.list, row_index, window);
        let owner = cx.entity().downgrade();
        let toggle_id = group_id.clone();
        let group = threadlane_ui_kit::completed_activity_group(
            &group_id,
            expanded,
            activities,
            &motion,
            |activity| self.render_tool_activity(activity, window, row_index, cx),
            move |_, cx| {
                let _ = owner.update(cx, |this, cx| {
                    if !this.expanded_activity_groups.remove(&toggle_id) {
                        this.expanded_activity_groups.insert(toggle_id.clone());
                    }
                    this.transcript.list.pause_following_tail();
                    this.transcript.list.remeasure();
                    cx.notify();
                });
            },
            &theme,
        );
        group
    }

    fn render_working_indicator(&self, _cx: &mut Context<Self>) -> AnyElement {
        // Running state is shown by the progress summary below the transcript.
        Empty.into_any_element()
    }


    fn sync_transcript_rows(&mut self, messages: Arc<Vec<ChatMessageInfo>>, generating: bool, session_changed: bool) {
        self.transcript.sync(messages, generating, session_changed, self.find_open || self.outline_selected_id.is_some());
        if !self.code_wrap_blocks.is_empty() {
            let messages = &self.transcript.messages;
            self.code_wrap_blocks
                .retain(|id, blocks| !blocks.is_empty() && messages.iter().any(|m| &m.id == id));
        }
    }

    fn render_transcript_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let messages = Arc::clone(&self.transcript.messages);
        let selected_match = self.find_open
            && matches!(self.transcript.rows.get(index),
            Some(TranscriptRow::Message(i)) if self.find_selected.as_ref() == Some(&messages[*i].id));
        let selected_prompt = self.outline_selected_id.as_ref().is_some_and(|id| {
            matches!(self.transcript.rows.get(index),
                Some(TranscriptRow::Message(i)) if messages[*i].id == *id)
        });
        let selected = selected_match || selected_prompt;
        let selected_label = if selected_prompt {
            "Selected prompt"
        } else {
            "Selected matching message"
        };
        let content = match self.transcript.rows.get(index).cloned() {
            Some(TranscriptRow::Message(message_index)) => messages
                .get(message_index)
                .map(|message| self.render_message(message, window, index, cx)),
            Some(TranscriptRow::Activities(range)) => messages
                .get(range)
                .map(|messages| self.render_activity_group(messages, window, index, cx)),
            Some(TranscriptRow::Working) => Some(self.render_working_indicator(cx)),
            None => None,
        };

        threadlane_ui_kit::conversation_transcript_row(selected.then_some(selected_label), cx)
            .children(content)
            .into_any_element()
    }

    fn markdown_state(
        &mut self,
        key: String,
        source: &str,
        cx: &mut Context<Self>,
    ) -> Entity<TextViewState> {
        threadlane_ui_kit::markdown::markdown_state(&mut self.markdown_states, self.markdown_cache_namespace.clone(), key, source, cx)
    }

    fn cached_segments(&mut self, message_id: &str, content: &str) -> Vec<MarkdownSegment> {
        if let Some((source, _)) = self.segment_cache.get(message_id) {
            if source != content
                && classify_markdown_update(source, content) == MarkdownUpdate::Replace
            {
                self.code_wrap_blocks.remove(message_id);
            }
        }
        threadlane_ui_kit::markdown::markdown_segments(&mut self.segment_cache, message_id, content)
    }

    fn chat_markdown_view(&self, state: &Entity<TextViewState>) -> TextView {
        let model = self.model.clone();
        threadlane_ui_kit::markdown::markdown_view(state, move |path, cx| {
            model.update(cx, |state, cx| { state.request_open_file(path); cx.notify(); });
        })
    }

    fn render_interactive_code_block(
        &mut self,
        msg_id: &str,
        block_index: usize,
        language: &str,
        header_path: Option<&str>,
        code: &str,
        streaming: bool,
        content_states: &mut Vec<Entity<TextViewState>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let model = self.model.clone();
        let code_str = code.to_string();
        let copy_code = code.to_string();
        let is_runnable = !streaming
            && is_terminal_runnable_language(language)
            && active_shell_supports_language(language);
        let path_opt = header_path.and_then(|path| match classify_chat_link(path) {
            ChatLinkTarget::ProjectFile(path) => Some(path),
            _ => None,
        });
        let path_for_open = (!streaming).then(|| path_opt.clone()).flatten();

        let key = format!("{msg_id}-{block_index}");
        let wrapped = self
            .code_wrap_blocks
            .get(msg_id)
            .is_some_and(|blocks| blocks.contains(&block_index));
        let actions = threadlane_ui_kit::code_block_actions()
            .child({
                let msg_id = msg_id.to_string();
                threadlane_ui_kit::code_block_wrap_button(&key, wrapped)
                    .on_click(cx.listener(move |host, _, _, cx| {
                        host.toggle_code_block_wrap(&msg_id, block_index, cx);
                    }))
            })
            .children(is_runnable.then(|| {
                let cmd = code_str.clone();
                let model = model.clone();
                threadlane_ui_kit::code_block_run_button(&key)
                    .on_click(move |_, _, cx| {
                        let cmd = normalize_terminal_command(&cmd);
                        if cmd.lines().filter(|line| !line.trim().is_empty()).count() > 1 {
                            let model = model.clone();
                            cx.spawn(async move |cx| {
                                let result = rfd::AsyncMessageDialog::new()
                                    .set_title("Run multiple terminal commands?")
                                    .set_description("This code block contains multiple commands. Run them in the active terminal?")
                                    .set_buttons(rfd::MessageButtons::YesNo)
                                    .show().await;
                                if matches!(result, rfd::MessageDialogResult::Yes) {
                                    let _ = model.update(cx, |state, cx| {
                                        controller::dispatch(state, AppAction::RunTerminalCommand(cmd));
                                        cx.notify();
                                    });
                                }
                            }).detach();
                        } else {
                            model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::RunTerminalCommand(cmd));
                                cx.notify();
                            });
                        }
                    })
            }))
            .children(path_for_open.map(|path| {
                let model = model.clone();
                threadlane_ui_kit::code_block_open_button(&key)
                    .on_click(move |_, _, cx| {
                        model.update(cx, |state, cx| {
                            controller::dispatch(state, AppAction::OpenFileInEditor(path.clone()));
                            cx.notify();
                        });
                    })
            }))
            .children((!streaming).then(|| {
                let copy_key = format!("copy-code-{key}");
                let copied = self.copied_message.as_ref().is_some_and(|(id, time)| {
                    id == &copy_key && time.elapsed() < COPIED_FEEDBACK_WINDOW
                });
                threadlane_ui_kit::code_block_copy_button(&key, copied, cx)
                    .on_click(cx.listener(move |host, _, window, cx| {
                        host.copy_text(copy_key.clone(), copy_code.clone(), cx);
                        window.push_notification(Notification::info("Code copied to clipboard"), cx);
                    }))
            }));
        let formatted_code = format!("```{language}\n{}\n```", code.trim_end());
        let code_state = self.markdown_state(format!("code-{key}"), &formatted_code, cx);
        content_states.push(code_state.clone());
        threadlane_ui_kit::code_block_surface(&key, cx)
            .child(threadlane_ui_kit::code_block_header(&key, language, path_opt.as_deref(), actions, cx))
            .child(threadlane_ui_kit::code_block_body(
                &key,
                cx,
                wrapped,
                self.chat_markdown_view(&code_state),
            ))
    }

    /// Flip one fenced block between horizontal-scroll and soft-wrap. The
    /// row's measured height changes, so invalidate just that transcript row
    /// and leave the list's reading anchor alone.
    fn toggle_code_block_wrap(&mut self, msg_id: &str, block_index: usize, cx: &mut Context<Self>) {
        let blocks = self.code_wrap_blocks.entry(msg_id.to_string()).or_default();
        if !blocks.remove(&block_index) {
            blocks.insert(block_index);
        }
        if blocks.is_empty() {
            self.code_wrap_blocks.remove(msg_id);
        }
        let row = self.transcript.rows.iter().position(|row| {
            matches!(row, TranscriptRow::Message(index)
                if self.transcript.messages[*index].id == msg_id)
        });
        if let Some(row) = row {
            self.transcript.list.remeasure_items(row..row + 1);
        }
        cx.notify();
    }

    fn render_reasoning_block(
        &mut self,
        msg: &ChatMessageInfo,
        window: &mut Window,
        row_index: usize,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let reasoning = msg.reasoning_content.as_deref()?;
        if reasoning.trim().is_empty() {
            return None;
        }
        let is_streaming = msg.streaming;
        let is_expanded = msg.reasoning_expanded;
        let model = self.model.clone();
        let msg_id = msg.id.clone();

        let motion = threadlane_ui_kit::DisclosureMotion::new(
            SharedString::from(format!("reasoning-body-{}", msg.id)),
            is_expanded,
            window,
            cx,
        );
        motion.remeasure_list_row(&self.transcript.list, row_index, window);
        let detail = motion.is_visible().then(|| {
            let container = threadlane_ui_kit::reasoning_detail(cx);
            if is_streaming {
                container.child(reasoning.to_owned()).into_any_element()
            } else {
                let markdown_state =
                    self.markdown_state(format!("reasoning-{}", msg.id), reasoning, cx);
                container
                    .child(self.chat_markdown_view(&markdown_state))
                    .into_any_element()
            }
        }).map(|body| motion.content(body));

        let transcript = self.transcript.list.clone();
        Some(threadlane_ui_kit::reasoning_card(msg, detail, false, move |_, cx| {
            transcript.pause_following_tail();
            transcript.remeasure();
            model.update(cx, |state, cx| {
                controller::dispatch(state, AppAction::ToggleReasoningExpanded(msg_id.clone()));
                cx.notify();
            });
        }, cx))
    }

    fn render_tool_activities_block(
        &mut self,
        msg_id: &str,
        tools: &[ToolActivityInfo],
        window: &mut Window,
        row_index: usize,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if tools.is_empty() {
            return None;
        }
        if tools.len() == 1 {
            return Some(
                self.render_tool_activity(&tools[0], window, row_index, cx)
                    .into_any_element(),
            );
        }

        let summary = tool_group_summary(tools);
        let has_error = tools.iter().any(|tool| tool.category == "Error");
        let has_running = tools
            .iter()
            .any(|tool| matches!(tool.category.as_str(), "Working" | "Thinking"));
        let status = match (has_running, has_error) {
            (true, true) => "Running · failed",
            (true, false) => "Running",
            (false, true) => "Failed",
            (false, false) => "Completed",
        };

        let group_key = format!("tool-group-{msg_id}");
        let is_expanded = if has_running {
            !self
                .expanded_tool_aggregates
                .contains(&format!("collapsed-{group_key}"))
        } else {
            self.expanded_tool_aggregates
                .contains(&format!("expanded-{group_key}"))
        };

        let theme = cx.theme().colors;
        let status_icon = if has_error {
            Icon::new(IconName::CircleX)
                .xsmall()
                .text_color(theme.danger)
                .into_any_element()
        } else if has_running {
            Spinner::new().xsmall().color(theme.info).into_any_element()
        } else {
            Icon::new(IconName::CircleCheck)
                .xsmall()
                .text_color(theme.success)
                .into_any_element()
        };

        let toggle_key = group_key.clone();
        let toggle_has_running = has_running;
        let description = format!(
            "{}: {summary}, {} tools, {status}",
            if is_expanded {
                "Collapse tools"
            } else {
                "Expand tools"
            },
            tools.len()
        );
        let header = threadlane_ui_kit::disclosure_button(
            SharedString::from(format!("tool-toggle-{msg_id}")),
            is_expanded,
            description,
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .flex_1()
                .child(status_icon)
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(summary),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(match status {
                            "Failed" | "Running · failed" => theme.danger,
                            "Running" => theme.info,
                            _ => theme.muted_foreground,
                        })
                        .child(status),
                ),
        )
        .debug_selector(|| "tool-group-disclosure".into())
        .px_3()
        .rounded_none()
        .bg(theme.muted.opacity(0.1))
        .on_click(cx.listener(move |this, _event, _window, cx| {
            this.transcript.list.pause_following_tail();
            this.transcript.list.remeasure();
            if toggle_has_running {
                let key = format!("collapsed-{toggle_key}");
                if !this.expanded_tool_aggregates.remove(&key) {
                    this.expanded_tool_aggregates.insert(key);
                }
            } else {
                let key = format!("expanded-{toggle_key}");
                if !this.expanded_tool_aggregates.remove(&key) {
                    this.expanded_tool_aggregates.insert(key);
                }
            }
            cx.notify();
        }));

        let motion = threadlane_ui_kit::DisclosureMotion::new(
            SharedString::from(format!("tool-group-body-{msg_id}")),
            is_expanded,
            window,
            cx,
        );
        motion.remeasure_list_row(&self.transcript.list, row_index, window);
        let detail_rows = motion
            .is_visible()
            .then(|| {
                div()
                    .flex()
                    .flex_col()
                    .px_2p5()
                    .py_1()
                    .border_t_1()
                    .border_color(theme.border.opacity(0.3))
                    .bg(theme.background.opacity(0.3))
                    .children(
                        tools
                            .iter()
                            .map(|tool| self.render_tool_activity(tool, window, row_index, cx)),
                    )
            })
            .map(|body| motion.content(body));

        Some(
            threadlane_ui_kit::result_surface(&theme)
                .flex()
                .flex_col()
                .child(header)
                .children(detail_rows)
                .into_any_element(),
        )
    }

    fn copy_text(&mut self, key: String, content: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(content));
        self.copied_message = Some((key, std::time::Instant::now()));
        // One retained timer, matching the preview: the latest copy owns feedback.
        self.copy_feedback_task = Some(cx.spawn(async move |owner, cx| {
            cx.background_executor().timer(COPIED_FEEDBACK_WINDOW).await;
            let _ = owner.update(cx, |host, cx| {
                host.copied_message = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Message action row: Copy for every role, plus Quote selection on
    /// assistant responses. `quote` carries the render-time eligibility —
    /// enabled/disabled, the readable reason when disabled, and the
    /// selection snapshot taken before focus moved to the control.
    fn render_message_actions(
        &self,
        msg: &ChatMessageInfo,
        align_end: bool,
        quote: Option<QuoteControl>,
        cx: &mut Context<Self>,
    ) -> Div {
        let copy_key = format!("message-copy-{}", msg.id);
        let is_copied = self
            .copied_message
            .as_ref()
            .is_some_and(|(id, time)| id == &copy_key && time.elapsed() < COPIED_FEEDBACK_WINDOW);
        let content = msg.content.clone();
        let copy_key_click = copy_key.clone();
        threadlane_ui_kit::message_actions(align_end)
            .when_some(quote, |el, quote| {
                let msg_id = msg.id.clone();
                let snapshot = quote.snapshot.clone();
                el.child(
                    div()
                        // Activate on mouse-down, not click: the press clears
                        // the window selection during the capture phase, and
                        // the render it triggers shows the control disabled
                        // again — a `click` bound to the button would never
                        // dispatch against that disabled frame. The armed
                        // snapshot still exists when this bubble-phase
                        // handler runs.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _event, window, cx| {
                                this.activate_quote_selection(
                                    &msg_id,
                                    snapshot.clone(),
                                    window,
                                    cx,
                                );
                            }),
                        )
                        .child(threadlane_ui_kit::message_quote_button(
                            &msg.id,
                            quote.enabled,
                            quote.reason.clone(),
                        )),
                )
            })
            .child(
                threadlane_ui_kit::message_copy_button(&msg.id, is_copied, cx)
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.copy_text(copy_key_click.clone(), content.clone(), cx);
                    })),
            )
    }

    /// Inserts the snapshot selection as a labeled blockquote into the
    /// current draft, then focuses the composer. Revalidates that the
    /// assistant message is still present and settled, that the snapshot's
    /// view is alive, and that any still-active selection still matches the
    /// snapshot — a click that only cleared the selection to reach this
    /// control keeps the snapshot; a genuinely different selection fails
    /// with `STALE_SELECTION_MESSAGE` and touches nothing. The quote lands
    /// through `append_draft_text_for`, so a session/project switch that
    /// raced the activation cannot write into the wrong draft.
    fn activate_quote_selection(
        &mut self,
        message_id: &str,
        snapshot: Option<QuoteSnapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = snapshot.or_else(|| {
            self.armed_quotes.borrow_mut().remove(message_id)
        });
        let Some(snapshot) = snapshot else {
            return;
        };
        // A live snapshot is consumed once: success or a stale report both
        // leave the control disabled until the user selects again.
        self.armed_quotes.borrow_mut().remove(message_id);
        let still_present = self.transcript.messages.iter().any(|msg| {
            msg.id == message_id && msg.role == MessageRole::Assistant && !msg.streaming
        });
        let text = match (still_present, snapshot.state.upgrade()) {
            (true, Some(state)) => {
                let current = state.read(cx).selected_text();
                if current.trim().is_empty() || current == snapshot.text {
                    snapshot.text.clone()
                } else {
                    window.push_notification(
                        Notification::info(STALE_SELECTION_MESSAGE),
                        cx,
                    );
                    return;
                }
            }
            _ => {
                window.push_notification(
                    Notification::info(STALE_SELECTION_MESSAGE),
                    cx,
                );
                return;
            }
        };
        if quoted_text(&text).chars().count() > MAX_QUOTE_SCALARS {
            window.push_notification(Notification::info(OVER_LIMIT_MESSAGE), cx);
            return;
        }
        let destination = self.composer_key.clone();
        let quote = format_quote_block(&text);
        if !self.append_draft_text_for(destination, &quote, window, cx) {
            window.push_notification(
                Notification::info(STALE_SELECTION_MESSAGE),
                cx,
            );
        }
    }

    /// Dispatched `QuoteSelection` from a message's context menu: activates
    /// that message's render-time snapshot.
    fn quote_selection_action(
        &mut self,
        message_id: &str,
        snapshot: Option<QuoteSnapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_quote_selection(message_id, snapshot, window, cx);
    }

    /// Live quote eligibility for one assistant message: `content_states`
    /// are the entities its rendered prose/code segments draw from. Streaming
    /// and empty responses are never quotable.
    ///
    /// Pressing the Quote control clears the window selection in the
    /// capture phase, so the render-time `armed_quotes` entry bridges to the
    /// press's bubble-phase mouse-down. Any render that sees no matching
    /// selection — cleared by an unrelated press, or replaced by a different
    /// one — drops the arm, so a Quote press after the selection moved can
    /// never replay stale text.
    fn quote_control(
        armed_quotes: &std::rc::Rc<std::cell::RefCell<HashMap<String, QuoteSnapshot>>>,
        message_id: &str,
        streaming: bool,
        content_states: &[Entity<TextViewState>],
        window: &mut Window,
        cx: &mut App,
    ) -> QuoteControl {
        let disabled = |reason: &'static str| QuoteControl {
            enabled: false,
            reason: SharedString::from(reason),
            snapshot: None,
        };
        if streaming || content_states.is_empty() {
            return disabled(NO_SELECTION_MESSAGE);
        }
        let window_selection = gpui_kit::base::TextSelection::selected_text(window, cx);
        match quote_owner(content_states, &window_selection, cx) {
            QuoteEligibility::Eligible(state) => {
                let snapshot = QuoteSnapshot {
                    state: state.downgrade(),
                    text: state.read(cx).selected_text(),
                };
                armed_quotes
                    .borrow_mut()
                    .insert(message_id.to_string(), snapshot.clone());
                QuoteControl {
                    enabled: true,
                    reason: SharedString::from(""),
                    snapshot: Some(snapshot),
                }
            }
            QuoteEligibility::Rejected(reason) => {
                // No live matching selection: the arm — if one exists —
                // belongs to a selection that was cleared or replaced.
                armed_quotes.borrow_mut().remove(message_id);
                disabled(reason.message())
            }
        }
    }

    fn render_message(
        &mut self,
        msg: &ChatMessageInfo,
        window: &mut Window,
        row_index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        match msg.role {
            MessageRole::User => {
                let is_queued =
                    msg.id.starts_with("queued-user-") && self.model.read(cx).is_generating;
                let is_steered =
                    msg.id.starts_with("steered-user-") && self.model.read(cx).is_generating;
                threadlane_ui_kit::message_row(MessageRole::User)
                    .when(is_queued, |el| {
                        el.child(
                            div().flex().items_center().gap_1().mb_1().child(
                                Tag::new()
                                    .child("Queued for next turn")
                                    .with_variant(TagVariant::Secondary)
                                    .small(),
                            ),
                        )
                    })
                    .when(is_steered, |el| {
                        el.child(
                            div().child(
                                Tag::new()
                                    .child("Steering current turn")
                                    .with_variant(TagVariant::Primary)
                                    .small(),
                            ),
                        )
                    })
                    .child(
                        threadlane_ui_kit::user_message_bubble(cx)
                            .child({
                                let markdown_state =
                                    self.markdown_state(msg.id.clone(), &msg.content, cx);
                                self.chat_markdown_view(&markdown_state)
                            })
                            .context_menu({
                                let content = msg.content.clone();
                                move |menu, window, _cx| {
                                    let text = content.clone();
                                    threadlane_ui_kit::message_context_menu(menu,
                                        move |_event, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                text.clone(),
                                            ));
                                            window.push_notification(
                                                Notification::info("Copied to clipboard"),
                                                cx,
                                            );
                                        },
                                    )
                                }
                            }),
                    )
                    .children((!msg.content.is_empty()).then(|| {
                        let mut footer = self.render_message_actions(msg, true, None, cx);
                        if !self.model.read(cx).is_generating {
                            let content = msg.content.clone();
                            footer = footer.child(
                                threadlane_ui_kit::message_edit_button(&msg.id)
                                .on_click(cx.listener(
                                    move |this, _event, window, cx| {
                                        if !this.input_state.read(cx).value().is_empty()
                                            || !this.pasted_images.is_empty()
                                        {
                                            window.push_notification(
                                                Notification::info(
                                                    "Send or clear your draft before editing a message",
                                                ),
                                                cx,
                                            );
                                            this.focus_composer(window, cx);
                                            return;
                                        }
                                        this.prompt_recall = None;
                                        this.input_state.update(cx, |input, cx| {
                                            input.set_value(content.clone(), window, cx);
                                        });
                                        this.focus_composer(window, cx);
                                    },
                                )),
                            );
                        }
                        footer
                    }))
            }
            MessageRole::Assistant => {
                let reasoning_element = self.render_reasoning_block(msg, window, row_index, cx);
                let filtered_tools: Vec<ToolActivityInfo> = msg
                    .tool_activities
                    .iter()
                    .filter(|tool| tool.title != "update_plan")
                    .cloned()
                    .collect();
                let tools_element = self.render_tool_activities_block(&msg.id, &filtered_tools, window, row_index, cx);

                // Content `TextViewState` entities in render order: the only
                // views whose selections are quotable for this message.
                let mut content_states: Vec<Entity<TextViewState>> = Vec::new();
                let content_column = if !msg.content.is_empty() {
                    let segments = self.cached_segments(&msg.id, &msg.content);
                    let rendered_segments: Vec<AnyElement> = segments
                        .into_iter()
                        .enumerate()
                        .map(|(idx, seg)| match seg {
                            MarkdownSegment::Markdown(text) => {
                                let markdown_state = self.markdown_state(
                                    format!("{}-seg-{}", msg.id, idx),
                                    &text,
                                    cx,
                                );
                                content_states.push(markdown_state.clone());
                                self.chat_markdown_view(&markdown_state)
                                    .into_any_element()
                            }
                            MarkdownSegment::CodeBlock {
                                language,
                                header_path,
                                code,
                            } => self
                                .render_interactive_code_block(
                                    &msg.id,
                                    idx,
                                    &language,
                                    header_path.as_deref(),
                                    &code,
                                    msg.streaming,
                                    &mut content_states,
                                    cx,
                                )
                                .into_any_element(),
                        })
                        .collect();

                    Some(
                        threadlane_ui_kit::message_content_column()
                            .children(rendered_segments),
                    )
                } else {
                    None
                };
                let quote = Self::quote_control(
                    &self.armed_quotes,
                    &msg.id,
                    msg.streaming,
                    &content_states,
                    window,
                    cx,
                );
                let quote_snapshot = quote.snapshot.clone();

                threadlane_ui_kit::message_row(MessageRole::Assistant)
                    // The context menu's focus path bubbles through this row,
                    // so a dispatched `QuoteSelection` lands here.
                    .on_action({
                        let msg_id = msg.id.clone();
                        cx.listener(move |this, _: &QuoteSelection, window, cx| {
                            this.quote_selection_action(
                                &msg_id,
                                quote_snapshot.clone(),
                                window,
                                cx,
                            );
                        })
                    })
                    .child(
                        threadlane_ui_kit::assistant_message_content()
                            .children(reasoning_element)
                            .children(content_column)
                            .children(tools_element)
                            .children((!msg.streaming && !msg.content.is_empty()).then(|| {
                                self.render_message_actions(
                                    msg,
                                    false,
                                    Some(quote.clone()),
                                    cx,
                                )
                            }))
                            .context_menu({
                                let content = msg.content.clone();
                                let states = content_states.clone();
                                let streaming = msg.streaming;
                                let armed_quotes = self.armed_quotes.clone();
                                let menu_message_id = msg.id.clone();
                                let chat = cx.entity().downgrade();
                                move |menu, window, cx| {
                                    let text = content.clone();
                                    // Re-evaluate live: the selection may have moved
                                    // since the frame that rendered this message.
                                    let QuoteControl { enabled, reason, snapshot } =
                                        Self::quote_control(
                                            &armed_quotes,
                                            &menu_message_id,
                                            streaming,
                                            &states,
                                            window,
                                            cx,
                                        );
                                    let chat = chat.clone();
                                    let message_id = menu_message_id.clone();
                                    let quote_item = threadlane_ui_kit::message_quote_menu_item(
                                        enabled,
                                        (!enabled).then_some(reason),
                                        // Apply the snapshot taken while the menu was
                                        // built: a left press on this item clears the
                                        // window selection, and the render it causes
                                        // drops the row's armed snapshot, before the
                                        // click is delivered.
                                        move |_event, window, cx| {
                                            chat.update(cx, |chat, cx| {
                                                chat.activate_quote_selection(
                                                    &message_id,
                                                    snapshot.clone(),
                                                    window,
                                                    cx,
                                                );
                                            })
                                            .ok();
                                        },
                                    );
                                    threadlane_ui_kit::message_context_menu_with_quote(
                                        menu,
                                        move |_event, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                text.clone(),
                                            ));
                                            window.push_notification(
                                                Notification::info("Copied to clipboard"),
                                                cx,
                                            );
                                        },
                                        Some(quote_item),
                                    )
                                }
                            }),
                    )
            }
            MessageRole::ContextMarker => {
                div().w_full().flex().justify_center().my_2().px_4().child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(msg.content.clone()),
                )
            }
            MessageRole::System => div().flex().justify_center().my_2().child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(msg.content.clone())
                    .context_menu({
                        let content = msg.content.clone();
                        move |menu, window, _cx| {
                            let text = content.clone();
                            threadlane_ui_kit::message_context_menu(menu,
                                move |_event, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                    window.push_notification(
                                        Notification::info("Copied to clipboard"),
                                        cx,
                                    );
                                },
                            )
                        }
                    }),
            ),
            MessageRole::Error => render_chat_error(
                &msg.id,
                &msg.content,
                msg.retry_prompt.clone(),
                &self.model,
                cx,
            ),
        }
        .into_any_element()
    }

    fn render_new_task(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let (projects, active_work_dir, needs_provider) = {
            let state = self.model.read(cx);
            (
                state
                    .projects
                    .iter()
                    .map(|project| (project.name.clone(), project.work_dir.clone()))
                    .collect::<Vec<_>>(),
                state.active_work_dir.clone(),
                state.available_models().is_empty(),
            )
        };
        let selected_project = projects
            .iter()
            .find(|(_, work_dir)| active_work_dir.as_ref() == Some(work_dir))
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "Choose a project".to_string());
        let selected_project_path = projects
            .iter()
            .find(|(_, work_dir)| active_work_dir.as_ref() == Some(work_dir))
            .map(|(_, work_dir)| threadlane_ui_kit::display_path(work_dir));
        let model = self.model.clone();
        let hero_active_dir = active_work_dir.clone();

        let project_picker =
            threadlane_ui_kit::new_task_project_button(selected_project.clone(), selected_project_path)
                .dropdown_menu(move |menu, window, _cx| {
                    let items = projects.clone().into_iter().map(|(name, work_dir)| {
                        let model = model.clone();
                        let is_current = is_current_project(hero_active_dir.as_ref(), &work_dir);
                        threadlane_ui_kit::project_picker_item(name, is_current).on_click(
                            move |_event, _window, cx| {
                                model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::SelectDraftProject(work_dir.clone()),
                                    );
                                    cx.notify();
                                });
                            },
                        )
                    });

                    let model = model.clone();
                    let attach = threadlane_ui_kit::new_project_picker_item().on_click(
                        move |_event, _window, cx| {
                            let model = model.clone();
                            cx.spawn(async move |cx| {
                                let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await
                                else {
                                    return;
                                };
                                let path = folder.path().to_path_buf();
                                let _ = model.update(cx, |state, cx| {
                                    controller::dispatch(state, AppAction::AttachProject(path));
                                    cx.notify();
                                });
                            })
                            .detach();
                        },
                    );
                    threadlane_ui_kit::project_picker_menu(menu, items, attach)
                });

        let suggestions: [(SharedString, &str, Icon); 4] = [
            (
                "Explore repository".into(),
                "Explore repository architecture and key workflows",
                Icon::from(IconName::Search),
            ),
            (
                "Plan feature".into(),
                "Plan an end-to-end feature implementation",
                Icon::default().path("icons/square-pen.svg"),
            ),
            (
                "Run tests".into(),
                "Run the test suite and diagnose any issues",
                Icon::from(IconName::Play),
            ),
            (
                "Review changes".into(),
                "Review uncommitted git status and diffs",
                Icon::default().path("icons/git/compare.svg"),
            ),
        ];
        let input_state = self.input_state.clone();

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .px_4()
            .pb_16()
            .child(
                div()
                    .id("new-task-mark")
                    .aria_label("Threadlane")
                    .size(rems(4.))
                    .rounded_2xl()
                    .bg(theme.accent.opacity(0.10))
                    .border_1()
                    .border_color(theme.accent.opacity(0.22))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme.primary)
                    .child(
                        Icon::default()
                            .path("icons/threadlane.svg")
                            .size_8(),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child("What should we build in")
                    .child(project_picker),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if needs_provider {
                        "Connect a model provider first, then describe a goal or ask a question."
                    } else {
                        "Describe a goal, plan changes, or ask questions about your project."
                    }),
            )
            .children(needs_provider.then(|| {
                let model = self.model.clone();
                Button::new("hero-open-provider-settings")
                    .icon(IconName::Settings)
                    .label("Connect a provider")
                    .primary()
                    .on_click(move |_event, _window, cx| {
                        model.update(cx, |state, cx| {
                            controller::dispatch(state, AppAction::OpenSettings);
                            cx.notify();
                        });
                    })
            }))
            // Starter prompts appear once a provider is connected; before that the
            // hero's one next step is connecting, and disabled chips read as broken.
            .children((!needs_provider).then(|| {
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap_2()
                    .max_w(rems(40.0))
                    .mt_4()
                    .children(suggestions.into_iter().map(|(label, prompt, icon)| {
                        let input = input_state.clone();
                        let prompt_str = prompt.to_string();
                        Button::new(SharedString::from(format!("suggestion-{label}")))
                            .icon(icon)
                            .label(label)
                            .tooltip(prompt)
                            .outline()
                            .small()
                            .rounded_full()
                            .on_click(move |_event, window, cx| {
                                input.update(cx, |input, cx| {
                                    input.set_value(&prompt_str, window, cx);
                                    input.focus(window, cx);
                                });
                            })
                    }))
            }))
            .into_any_element()
    }

    fn resolve_pending_permission(
        &mut self,
        request_id: &str,
        decision: threadlane_permission::PermissionDecision,
        cx: &mut Context<Self>,
    ) -> bool {
        let resolved = self.model.update(cx, |state, cx| {
            let resolved = state.resolve_active_permission(request_id, decision);
            cx.notify();
            resolved
        });
        if resolved {
            self.permission_details_request = None;
        }
        cx.notify();
        resolved
    }

    fn question_selection_key(request_id: &str, question_id: &str) -> String {
        format!("{request_id}\0{question_id}")
    }

    fn toggle_question_option(
        &mut self,
        request_id: &str,
        question_id: &str,
        option: &str,
        cx: &mut Context<Self>,
    ) {
        let key = Self::question_selection_key(request_id, question_id);
        let entry = self.question_selections.entry(key).or_default();
        if let Some(pos) = entry.iter().position(|item| item == option) {
            entry.remove(pos);
        } else {
            entry.push(option.to_string());
        }
        cx.notify();
    }

    fn submit_active_question(&mut self, cx: &mut Context<Self>) {
        let request = {
            let state = self.model.read(cx);
            state
                .active_session_id
                .as_ref()
                .and_then(|session_id| state.pending_questions.get(session_id))
                .cloned()
        };
        let Some(request) = request else {
            return;
        };
        let answers = request
            .questions
            .iter()
            .map(|item| {
                let key = Self::question_selection_key(&request.id, &item.id);
                let custom_text = self
                    .question_inputs
                    .get(&key)
                    .map(|input| input.read(cx).value().to_string())
                    .map(|text| text.trim().to_string())
                    .filter(|text| !text.is_empty());
                threadlane_protocol::QuestionItemAnswer {
                    question_id: item.id.clone(),
                    selected: self
                        .question_selections
                        .get(&key)
                        .cloned()
                        .unwrap_or_default(),
                    custom_text,
                }
            })
            .collect::<Vec<_>>();
        let answer = threadlane_protocol::QuestionAnswer {
            request_id: request.id.clone(),
            answers,
            dismissed: false,
        };
        // The Send button is disabled while empty, but never resolve a
        // totally unanswered card through any other path either: an empty
        // answer is indistinguishable from a real one downstream.
        if answer.answers.iter().all(|item| {
            item.selected.is_empty()
                && item
                    .custom_text
                    .as_deref()
                    .is_none_or(|text| text.is_empty())
        }) {
            return;
        }
        let request_id = request.id.clone();
        self.question_selections
            .retain(|key, _| !key.starts_with(&format!("{request_id}\0")));
        self.question_inputs
            .retain(|key, _| !key.starts_with(&format!("{request_id}\0")));
        self.model.update(cx, |state, cx| {
            state.resolve_active_question_answer(&request_id, answer);
            cx.notify();
        });
        cx.notify();
    }

    fn dismiss_active_question(&mut self, cx: &mut Context<Self>) {
        let request_id = {
            let state = self.model.read(cx);
            state
                .active_session_id
                .as_ref()
                .and_then(|session_id| state.pending_questions.get(session_id))
                .map(|request| request.id.clone())
        };
        let Some(request_id) = request_id else {
            return;
        };
        self.question_selections
            .retain(|key, _| !key.starts_with(&format!("{request_id}\0")));
        self.question_inputs
            .retain(|key, _| !key.starts_with(&format!("{request_id}\0")));
        self.model.update(cx, |state, cx| {
            state.resolve_active_question(&request_id);
            cx.notify();
        });
        cx.notify();
    }

    fn complete_slash_command(
        &mut self,
        command_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = format!("/{command_name} ");
        self.input_state.update(cx, |state, cx| {
            state.set_value(&value, window, cx);
            let cursor = value.len();
            state.set_selected_range(cursor..cursor, cx);
            state.focus(window, cx);
        });
        self.selected_slash_index = 0;
        self.slash_scroll_handle.scroll_to_item(0);
        cx.notify();
    }

    fn matching_slash_command_count(&mut self, cx: &mut Context<Self>) -> usize {
        if self.dismiss_slash_menu {
            return 0;
        }
        let text = self.input_state.read(cx).value().to_string();
        let project_root = self.model.read(cx).active_work_dir.clone();
        let Some(query) = active_slash_command_query(&text) else {
            return 0;
        };
        self.cached_slash_commands(project_root.as_deref())
            .into_iter()
            .filter(|command| query.is_empty() || command.name.starts_with(query))
            .count()
    }

    fn select_previous_slash_command_action(
        &mut self,
        _: &SelectPreviousSlashCommand,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.matching_slash_command_count(cx);
        if count == 0 {
            return;
        }
        self.selected_slash_index = if self.selected_slash_index == 0 {
            count - 1
        } else {
            self.selected_slash_index - 1
        };
        self.slash_scroll_handle
            .scroll_to_item(self.selected_slash_index);
        cx.notify();
    }

    fn select_next_slash_command_action(
        &mut self,
        _: &SelectNextSlashCommand,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.matching_slash_command_count(cx);
        if count == 0 {
            return;
        }
        self.selected_slash_index = (self.selected_slash_index + 1) % count;
        self.slash_scroll_handle
            .scroll_to_item(self.selected_slash_index);
        cx.notify();
    }

    fn complete_slash_command_action(
        &mut self,
        _: &CompleteSlashCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dismiss_slash_menu {
            return;
        }
        let text = self.input_state.read(cx).value().to_string();
        let project_root = self.model.read(cx).active_work_dir.clone();
        let Some(query) = active_slash_command_query(&text) else {
            return;
        };
        let matching = self
            .cached_slash_commands(project_root.as_deref())
            .into_iter()
            .filter(|command| query.is_empty() || command.name.starts_with(query))
            .collect::<Vec<_>>();
        if matching.is_empty() {
            return;
        }
        let selected = self
            .selected_slash_index
            .min(matching.len().saturating_sub(1));
        self.complete_slash_command(&matching[selected].name, window, cx);
    }

    fn dismiss_slash_command_action(
        &mut self,
        _: &DismissSlashCommand,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_slash_menu = true;
        cx.notify();
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.find_open {
            return;
        }
        if self.outline_open {
            self.handle_outline_key_down(event, window, cx);
            return;
        }
        let key = event.keystroke.key.as_str();

        if self.handle_file_completion_key_down(key, window, cx) {
            return;
        }

        let text = self.input_state.read(cx).value().to_string();
        let project_root = self.model.read(cx).active_work_dir.clone();
        if let Some(query) = active_slash_command_query(&text) {
            if !self.dismiss_slash_menu {
                if key == "escape" {
                    self.dismiss_slash_menu = true;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                let matching = self
                    .cached_slash_commands(project_root.as_deref())
                    .into_iter()
                    .filter(|cmd| query.is_empty() || cmd.name.starts_with(query))
                    .collect::<Vec<_>>();
                if !matching.is_empty() {
                    let total = matching.len();
                    match key {
                        "down" => {
                            self.selected_slash_index = (self.selected_slash_index + 1) % total;
                            self.slash_scroll_handle
                                .scroll_to_item(self.selected_slash_index);
                            cx.stop_propagation();
                            cx.notify();
                            return;
                        }
                        "up" => {
                            self.selected_slash_index = if self.selected_slash_index == 0 {
                                total.saturating_sub(1)
                            } else {
                                self.selected_slash_index - 1
                            };
                            self.slash_scroll_handle
                                .scroll_to_item(self.selected_slash_index);
                            cx.stop_propagation();
                            cx.notify();
                            return;
                        }
                        "tab" => {
                            let selected = self
                                .selected_slash_index
                                .min(matching.len().saturating_sub(1));
                            let command_name = matching[selected].name.clone();
                            self.complete_slash_command(&command_name, window, cx);
                            cx.stop_propagation();
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn open_permission_details(
        &mut self,
        request_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self
            .model
            .read(cx)
            .active_session_id
            .as_ref()
            .and_then(|id| self.model.read(cx).pending_permissions.get(id));
        if active.is_none_or(|request| request.id != request_id)
            || self.permission_details_request.is_some()
        {
            return;
        }
        self.permission_details_request = Some(request_id.to_string());
        let chat = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let close_chat = chat.clone();
            let content = chat
                .update(cx, |chat, cx| chat.render_permission_details_dialog(cx))
                .ok()
                .flatten();
            dialog
                .title("Permission request")
                .w(window.rem_size() * 40.0)
                .children(content)
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    let _ = close_chat.update(cx, |chat, cx| {
                        chat.permission_details_request = None;
                        cx.notify();
                    });
                })
        });
        cx.notify();
    }

    fn render_permission_details_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.model.read(cx);
        let request = state
            .pending_permissions
            .get(state.active_session_id.as_ref()?)?
            .clone();
        if self.permission_details_request.as_ref() != Some(&request.id) {
            return None;
        }
        let theme = cx.theme().colors;
        let allows_always = request
            .scopes
            .contains(&threadlane_protocol::PermissionScope::Always);
        let allows_session = request
            .scopes
            .contains(&threadlane_protocol::PermissionScope::Session);
        let action_button = |id: &'static str, label: &'static str, decision, primary: bool| {
            let request_id = request.id.clone();
            Button::new(id)
                .label(label)
                .small()
                .when(primary, |button| button.primary())
                .on_click(cx.listener(move |this, _, window, cx| {
                    if this.resolve_pending_permission(&request_id, decision, cx) {
                        window.close_dialog(cx);
                    }
                }))
        };
        Some(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(request.title.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(request.capability.clone()),
                )
                .child(
                    div()
                        .w_full()
                        .max_h(rems(20.0))
                        .p_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.background)
                        .text_sm()
                        .overflow_y_scrollbar()
                        .child(request.detail.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_end()
                        .gap_2()
                        .child(action_button(
                            "details-deny",
                            "Deny",
                            threadlane_permission::PermissionDecision::Deny,
                            false,
                        ))
                        .child(action_button(
                            "details-allow-once",
                            "Allow once",
                            threadlane_permission::PermissionDecision::AllowOnce,
                            true,
                        ))
                        .when(allows_session, |row| {
                            row.child(
                                action_button(
                                    "details-allow-session",
                                    "Allow session",
                                    threadlane_permission::PermissionDecision::AllowSession,
                                    false,
                                )
                                .debug_selector(|| "permission-details-session".into()),
                            )
                        })
                        .when(allows_always, |row| {
                            row.child(
                                action_button(
                                    "details-allow-always",
                                    "Always allow",
                                    threadlane_permission::PermissionDecision::AllowAlways,
                                    false,
                                )
                                .debug_selector(|| "permission-details-always".into()),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    fn render_permission_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.model.read(cx);
        let session_id = state.active_session_id.as_ref()?;
        let request = state.pending_permissions.get(session_id)?.clone();
        let owner = cx.entity().downgrade();
        let request_id = request.id.clone();
        let details = Button::new("permission-details-btn").icon(IconName::Maximize).label("Details")
            .accessibility_label("View full command & arguments").ghost().xsmall().rounded_md().tooltip("View full command & arguments")
            .on_click(cx.listener(move |this, _, window, cx| this.open_permission_details(&request_id, window, cx)));
        Some(div().w_full().max_w(rems(CHAT_CONTENT_MAX_WIDTH)).mx_auto().flex_none().px_4().pt_1().bg(cx.theme().background)
            .child(threadlane_ui_kit::permission_card(&request, false, true, Some(details.into_any_element()),
                move |request_id, decision, _, cx| { let _ = owner.update(cx, |this, cx| this.resolve_pending_permission(request_id, decision, cx)); }, cx))
            .into_any_element())
    }

    fn render_question_prompt(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.model.read(cx);
        let session_id = state.active_session_id.as_ref()?;
        let request = state.pending_questions.get(session_id)?.clone();
        let theme = cx.theme().colors;

        // Lazily create one custom-text input per allow_custom question so
        // the entity (and focus) survives re-renders.
        for item in request.questions.iter().filter(|item| item.allow_custom) {
            let key = Self::question_selection_key(&request.id, &item.id);
            if !self.question_inputs.contains_key(&key) {
                let input = cx
                    .new(|cx| InputState::new(window, cx).placeholder("Custom answer (optional)…"));
                self.question_inputs.insert(key, input);
            }
        }
        // Drop state for superseded requests so a new question starts clean.
        let prefix = format!("{}\0", request.id);
        self.question_inputs
            .retain(|key, _| key.starts_with(&prefix));
        self.question_selections
            .retain(|key, _| key.starts_with(&prefix));

        let questions = request
            .questions
            .iter()
            .map(|item| {
                let key = Self::question_selection_key(&request.id, &item.id);
                let selected = self
                    .question_selections
                    .get(&key)
                    .cloned()
                    .unwrap_or_default();
                let owner = cx.entity().downgrade();
                let request_id = request.id.clone(); let question_id = item.id.clone();
                threadlane_ui_kit::question_item(&request.id, item, &selected,
                    self.question_inputs.get(&key), false,
                    move |value, _, cx| { let _ = owner.update(cx, |this, cx| {
                        this.toggle_question_option(&request_id, &question_id, value, cx);
                    }); }, cx)

            })
            .collect::<Vec<_>>();

        // Sending with zero selections and zero custom text resolves an
        // empty answer (indistinguishable from a real one downstream), so
        // the Send button stays disabled until something is answered.
        // Toggling options calls cx.notify, and inputs notify on edit, so
        // this recomputes as the user answers.
        let has_answer = request.questions.iter().any(|item| {
            let key = Self::question_selection_key(&request.id, &item.id);
            let selected = self
                .question_selections
                .get(&key)
                .is_some_and(|selected| !selected.is_empty());
            let custom = self
                .question_inputs
                .get(&key)
                .is_some_and(|input| !input.read(cx).value().trim().is_empty());
            selected || custom
        });

        Some(
            div()
                .debug_selector(|| "question-card".into())
                .w_full()
                .max_w(rems(QUESTION_CARD_MAX_WIDTH))
                .mx_auto()
                .flex_none()
                .px_4()
                .pt_1()
                .bg(theme.background)
                .child(
                    threadlane_ui_kit::question_surface(cx)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_none()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_xs()
                                        .text_color(theme.foreground)
                                        .child("Model question"),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .truncate()
                                        .child("The run waits for your answer."),
                                ),
                        )
                        .children(questions)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("question-dismiss")
                                        .debug_selector(|| "question-dismiss".into())
                                        .label("Dismiss")
                                        .accessibility_label("Dismiss without answering")
                                        .ghost()
                                        .small()
                                        .rounded_lg()
                                        .tooltip("Dismiss without answering")
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.dismiss_active_question(cx);
                                        })),
                                )
                                .child(
                                    Button::new("question-submit")
                                        .debug_selector(|| "question-submit".into())
                                        .label("Send answers")
                                        .accessibility_label(if has_answer {
                                            "Send the selected answers"
                                        } else {
                                            "Select an option or type a custom answer first"
                                        })
                                        .small()
                                        .primary()
                                        .rounded_lg()
                                        .disabled(!has_answer)
                                        .tooltip(if has_answer {
                                            "Send the selected answers"
                                        } else {
                                            "Select an option or type a custom answer first"
                                        })
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.submit_active_question(cx);
                                        })),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Cached slash-command discovery. `available_slash_commands` scans
    /// extension directories and compiles each installed WASM module just to
    /// read its manifest, so it must not run per keystroke in render. The
    /// cache is keyed by project root and refreshed at most once per TTL
    /// while the command menu is open.
    fn cached_slash_commands(
        &mut self,
        project_root: Option<&std::path::Path>,
    ) -> Vec<SlashCommandInfo> {
        const SLASH_COMMAND_CACHE_TTL: Duration = Duration::from_secs(10);
        let project_root = project_root.map(std::path::Path::to_path_buf);
        if let Some((root, loaded_at, commands)) = &self.slash_command_cache {
            if *root == project_root && loaded_at.elapsed() < SLASH_COMMAND_CACHE_TTL {
                return commands.clone();
            }
        }
        let commands = available_slash_commands(project_root.as_deref());
        self.slash_command_cache =
            Some((project_root, std::time::Instant::now(), commands.clone()));
        commands
    }

    fn render_subagent_popover(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (count, active_count) = subagent_popover_counts(
            self.model
                .read(cx)
                .active_subagents()
                .iter()
                .map(|item| item.status),
        )?;
        let failed_count = self
            .model
            .read(cx)
            .active_subagents()
            .iter()
            .filter(|item| item.status == SubagentActivityStatus::Failed)
            .count();
        let label = if failed_count > 0 {
            format!("{failed_count} need attention · {active_count} active · {count} total")
        } else {
            format!("{active_count} active · {count} total")
        };
        Some(
            Button::new("open-agents-panel")
                .accessibility_label(format!("Open Agents panel, {label}"))
                .tooltip(format!("Open Agents panel · {label}"))
                .ghost()
                .rounded_full()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .when(failed_count > 0, |indicator| {
                            indicator.text_color(cx.theme().colors.danger)
                        })
                        .child(Icon::new(IconName::Bot).small())
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(count.to_string()),
                        ),
                )
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(crate::OpenWorkspaceAgents), cx);
                })
                .into_any_element(),
        )
    }

    fn render_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let (
            selected_model,
            reasoning_effort,
            orchestrator_mode,
            is_generating,
            pending_message,
            active_session_id,
            session_status,
        ) = {
            let state = self.model.read(cx);
            (
                state.selected_model.clone(),
                state.reasoning_effort,
                state.orchestrator_mode,
                state.is_generating,
                state.active_pending_composer_message().map(str::to_owned),
                state.active_session_id.clone(),
                visible_session_status(state.session_status.as_deref(), state.messages.last())
                    .map(str::to_owned),
            )
        };
        let (metrics, context_window) = {
            let state = self.model.read(cx);
            let context_window = state
                .active_context_window()
                .map(ContextMeterContext::from);
            (state.active_session_metrics(), context_window)
        };
        let supports_live_steering = !threadlane_acp_engine::is_acp_model(&selected_model);
        let steer_tooltip = if supports_live_steering {
            "Steer current turn immediately (Cmd+Enter)"
        } else {
            "This agent does not support live steering. Send your message to queue it after this turn."
        };
        let has_composer_text = !self.input_state.read(cx).value().trim().is_empty();
        // Queued sends are text-only: an images-only draft cannot stage for
        // the next turn, so images only count toward sendability when idle.
        let has_prompt = has_sendable_prompt(
            &self.input_state.read(cx).value(),
            if is_generating {
                0
            } else {
                self.pasted_images.len()
            },
        );
        let (model_options, selected_option, project_root) = {
            let state = self.model.read(cx);
            let options = state.available_models().to_vec();
            let opt = options.iter().find(|o| o.id == selected_model).cloned();
            let project = state.active_work_dir.clone();
            (options, opt, project)
        };
        let has_models = !model_options.is_empty();
        let needs_provider = !has_models;
        let preparing_worktree = self.model.read(cx).active_worktree_setup().is_some();
        // Shortcuts are only worth advertising once a message can be sent.
        let composer_shortcut_hint = if needs_provider {
            "Connect a provider to send messages"
        } else {
            "Enter to send · Shift+Enter for a new line"
        };
        let model_label = selected_option
            .as_ref()
            .map(|option| option.label.clone())
            .unwrap_or_else(|| {
                if needs_provider {
                    "Connect a provider".to_string()
                } else {
                    threadlane_daemon::catalog::selection_label(&selected_model, &model_options)
                }
            });
        // Selecting an ACP agent picks the *agent*; the agent then picks its own
        // model. Naming it here rather than only in the settings menu is what
        // makes choosing a model visibly take effect, since this is the control
        // a user reads to answer "which model am I on".
        let model_label = match self.model.read(cx).active_acp_model_label() {
            Some(agent_model) => format!("{model_label} · {agent_model}"),
            None => model_label,
        };
        // Selecting an ACP agent picks the *agent*; the agent then runs one of
        // its own models. Both are "which model am I on", so both belong in
        // this one control rather than split across two.
        // Per-agent model settings behind each "External agents" row: live
        // options for the selected agent, launch-time cache for the rest, so
        // every agent's models are visible before it is picked.
        let acp_model_sections: HashMap<String, Vec<threadlane_acp::AcpConfigOption>> = {
            let state = self.model.read(cx);
            let mut sections = HashMap::new();
            for option in &model_options {
                if option.provider != threadlane_daemon::catalog::ModelProvider::Acp {
                    continue;
                }
                let Some(agent_id) = threadlane_acp_engine::acp_agent_id(&option.id) else {
                    continue;
                };
                let options = if option.id == selected_model {
                    state.active_acp_config_options()
                } else {
                    threadlane_daemon::catalog::cached_acp_config_options(agent_id)
                };
                sections.insert(agent_id.to_string(), options);
            }
            sections
        };
        let queue_model = self.model.clone();
        let steer_model = self.model.clone();
        let dismiss_model = self.model.clone();
        let dismiss_input = self.input_state.clone();
        let cancel_model = self.model.clone();
        let send_model = self.model.clone();
        let send_input = self.input_state.clone();

        let image_count = self.pasted_images.len();
        let image_chips = self
            .pasted_images
            .iter()
            .enumerate()
            .map(|(index, image)| {
                // The existing upload model has no attachment ID. Retain its index-keyed
                // controls and generation guard; the kit receives host-owned identity.
                let preview_button_id = ("preview-pasted-image", index);
                let preview_focus = window
                    .use_keyed_state(preview_button_id.clone(), cx, |_, cx| cx.focus_handle())
                    .read(cx)
                    .clone();
                let preview = threadlane_ui_kit::staged_image_preview_button(
                    preview_button_id,
                    &image.display_name,
                    index + 1,
                    image_count,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_image_preview(index, preview_focus.clone(), window, cx);
                }));
                let remove = threadlane_ui_kit::staged_image_remove_button(
                    ("remove-pasted-image", index),
                    &image.display_name,
                )
                .on_click(cx.listener(move |this, _event, window, cx| {
                    if index < this.pasted_images.len() {
                        this.pasted_images.remove(index);
                        this.invalidate_image_preview(window, cx);
                        cx.notify();
                    }
                }));
                threadlane_ui_kit::staged_image_chip(image.display_name.clone(), preview, remove, cx)
            })
            .collect::<Vec<_>>();

        // The new-task hero carries its own "Connect a provider" action; only
        // show the banner where that hero is not on screen.
        let hero_visible = self.current_tab == CentralTab::Chat && {
            let state = self.model.read(cx);
            state.is_new_task
                || (state.messages.is_empty() && !state.active_session_is_loading())
        };
        let provider_setup_model = self.model.clone();
        let provider_setup_banner = (needs_provider && !hero_visible).then(|| {
            div()
                .debug_selector(|| "provider-setup-banner".into())
                .w_full()
                .max_w(rems(CHAT_CONTENT_MAX_WIDTH))
                .mx_auto()
                .mb_2()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(theme.warning.opacity(0.45))
                .bg(theme.warning.opacity(0.08))
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child("Connect a model provider to start"),
                        )
                        .child(div().text_xs().text_color(theme.muted_foreground).child(
                            "Add an account or API key in Settings, then choose a model here.",
                        )),
                )
                .child(
                    Button::new("composer-open-provider-settings")
                        .debug_selector(|| "provider-setup-open-settings".into())
                        .icon(IconName::Settings)
                        .label("Open settings")
                        .small()
                        .primary()
                        .on_click(move |_event, _window, cx| {
                            provider_setup_model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::OpenSettings);
                                cx.notify();
                            });
                        }),
                )
        });

        let (
            projects_list,
            active_work_dir,
            is_new_task,
            draft_work_mode,
            active_session_is_worktree,
            active_session_worktree_available,
            active_session_project_name,
        ) = {
            let state = self.model.read(cx);
            let active_dir = state.active_work_dir.clone();
            let active_session = state.active_session_id.as_ref().and_then(|sid| {
                state.projects.iter().find_map(|project| {
                    project
                        .sessions
                        .iter()
                        .find(|session| &session.id == sid)
                        .map(|session| {
                            (
                                project.name.clone(),
                                session.is_worktree,
                                session.worktree_available,
                            )
                        })
                })
            });
            (
                state
                    .projects
                    .iter()
                    .map(|p| (p.name.clone(), p.work_dir.clone()))
                    .collect::<Vec<_>>(),
                active_dir,
                state.is_new_task,
                state.draft_work_mode,
                active_session
                    .as_ref()
                    .is_some_and(|(_, is_worktree, _)| *is_worktree),
                active_session
                    .as_ref()
                    .is_none_or(|(_, _, available)| *available),
                active_session.map(|(project_name, _, _)| project_name),
            )
        };

        let effective_work_mode = if is_new_task {
            draft_work_mode
        } else if active_session_is_worktree {
            WorkMode::Worktree
        } else {
            WorkMode::Local
        };

        let selected_project_name = if is_new_task {
            None
        } else {
            active_session_project_name
        }
        .or_else(|| {
            projects_list
                .iter()
                .find(|(_, work_dir)| active_work_dir.as_ref() == Some(work_dir))
                .map(|(name, _)| name.clone())
        })
        .unwrap_or_else(|| "Select project".to_string());

        let project_chip_model = self.model.clone();
        let project_chip_active = active_work_dir.clone();
        let project_chip_tooltip = active_work_dir
            .as_ref()
            .map(|dir| threadlane_ui_kit::display_path(dir))
            .unwrap_or_else(|| selected_project_name.clone());
        let project_chip = threadlane_ui_kit::composer_project_button(
            selected_project_name.clone(),
            project_chip_tooltip,
        )
        .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, window, _cx| {
            let items = projects_list.clone().into_iter().map(|(name, work_dir)| {
                let model = project_chip_model.clone();
                let is_current = is_current_project(project_chip_active.as_ref(), &work_dir);
                threadlane_ui_kit::project_picker_item(name, is_current).on_click(
                    move |_event, _window, cx| {
                        model.update(cx, |state, cx| {
                            controller::dispatch(
                                state,
                                AppAction::SelectDraftProject(work_dir.clone()),
                            );
                            cx.notify();
                        });
                    },
                )
            });

            let model = project_chip_model.clone();
            let attach =
                threadlane_ui_kit::new_project_picker_item().on_click(move |_event, _window, cx| {
                    let model = model.clone();
                    cx.spawn(async move |cx| {
                        let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await else {
                            return;
                        };
                        let path = folder.path().to_path_buf();
                        let _ = model.update(cx, |state, cx| {
                            controller::dispatch(state, AppAction::AttachProject(path));
                            cx.notify();
                        });
                    })
                    .detach();
                });
            threadlane_ui_kit::project_picker_menu(menu, items, attach)
        });

        let work_mode_model = self.model.clone();
        let work_mode_label = match effective_work_mode {
            WorkMode::Local => "Local",
            WorkMode::Worktree => {
                if is_new_task {
                    "New local worktree"
                } else if active_session_worktree_available {
                    "Worktree"
                } else {
                    "Worktree unavailable"
                }
            }
        };

        let work_mode_chip = threadlane_ui_kit::composer_work_mode_button(work_mode_label, effective_work_mode == WorkMode::Worktree)
            .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, window, _cx| {
                let menu = menu.check_side(gpui_component::Side::Right);
                let local_model = work_mode_model.clone();
                let wt_model = work_mode_model.clone();
                menu.item(PopupMenuItem::label("Work in"))
                    .item(
                        PopupMenuItem::new("Local")
                            .icon(IconName::SquareTerminal)
                            .checked(effective_work_mode == WorkMode::Local)
                            .on_click(move |_event, _window, cx| {
                                local_model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::SelectWorkMode(WorkMode::Local),
                                    );
                                    cx.notify();
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new("New local worktree")
                            .icon(Icon::default().path("icons/git/branch.svg"))
                            .checked(effective_work_mode == WorkMode::Worktree)
                            .on_click(move |_event, _window, cx| {
                                wt_model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::SelectWorkMode(WorkMode::Worktree),
                                    );
                                    cx.notify();
                                });
                            }),
                    )
            });

        let base_chip = (is_new_task && effective_work_mode == WorkMode::Worktree).then(|| {
            let state = self.model.read(cx);
            let selected = state.draft_worktree_base.clone();
            let branches = state.draft_worktree_bases.clone();
            let model = self.model.clone();
            let label = selected
                .clone()
                .unwrap_or_else(|| "Loading branches…".into());
            threadlane_ui_kit::composer_worktree_base_button(label, !branches.is_empty())
                .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _, _| {
                    let mut menu = menu;
                    for branch in &branches {
                        let model = model.clone();
                        let value = branch.clone();
                        menu = menu.item(
                            PopupMenuItem::new(branch.clone())
                                .checked(selected.as_ref() == Some(branch))
                                .on_click(move |_, _, cx| {
                                    model.update(cx, |state, cx| {
                                        state.draft_worktree_base = Some(value.clone());
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                    menu
                })
        });

        let branch = {
            let state = self.model.read(cx);
            state
                .active_git_work_dir()
                .and_then(|dir| state.git_statuses.get(&dir))
                .and_then(|status| status.branch.clone())
        };
        let skills_chip = {
            let active_skills_count = active_work_dir
                .as_ref()
                .map(|dir| {
                    threadlane_skills::settings::discover_skills(Some(dir))
                        .into_iter()
                        .filter(|s| s.enabled)
                        .count()
                })
                .unwrap_or(0);

            threadlane_ui_kit::composer_skills_button(active_skills_count)
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.model.update(cx, |state, cx| {
                        controller::dispatch(state, AppAction::OpenSettings);
                        cx.notify();
                    });
                }))
        };

        let composer_context_bar = threadlane_ui_kit::composer_context_bar()
            .child(project_chip).child(work_mode_chip).children(base_chip).child(skills_chip)
            .children(branch.map(|branch| threadlane_ui_kit::composer_branch_label(branch, cx)));

        let queued_messages: Vec<(String, String)> = self
            .model
            .read(cx)
            .messages
            .iter()
            .filter(|message| crate::transcript::is_queued_message(message, is_generating))
            .map(|message| (message.id.clone(), message.content.clone()))
            .collect();
        let queued_session_id = active_session_id.clone();
        // Rows whose removal the daemon has not confirmed yet stay listed
        // but show "Removing…" instead of their actions — the entry may
        // still be queued on the peer, so it must not look actionable.
        let queued_pending_removals: std::collections::HashSet<String> = match queued_session_id
            .as_deref()
        {
            Some(session_id) => {
                let state = self.model.read(cx);
                queued_messages
                    .iter()
                    .filter_map(|(message_id, _)| {
                        queued_entry_id(message_id, Some(session_id)).filter(|entry_id| {
                            state.queued_removal_pending(session_id, entry_id)
                        })
                    })
                    .collect()
            }
            None => std::collections::HashSet::new(),
        };
        let queued_preview = (!queued_messages.is_empty()).then(|| {
            let count = queued_messages.len();
            let rows = queued_messages
                .into_iter()
                .map(|(message_id, text)| {
                    let entry_id = queued_entry_id(&message_id, queued_session_id.as_deref());
                    let removing = entry_id
                        .as_ref()
                        .is_some_and(|entry_id| queued_pending_removals.contains(entry_id));
                    let mut actions = Vec::new();
                    if let Some(entry_id) = entry_id.filter(|_| !removing) {
                        let queued_steer_model = self.model.clone();
                        let queued_edit_model = self.model.clone();
                        let queued_remove_model = self.model.clone();
                        let queued_edit_input = self.input_state.clone();
                        let steer_entry_id = entry_id.clone();
                        let remove_entry_id = entry_id.clone();
                        actions.push(
                            threadlane_ui_kit::queued_steer_button(
                                &message_id,
                                supports_live_steering,
                                steer_tooltip,
                            )
                            .on_click(move |_event, _window, cx| {
                                queued_steer_model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::SteerQueuedMessage {
                                            entry_id: steer_entry_id.clone(),
                                        },
                                    );
                                    cx.notify();
                                });
                            })
                            .into_any_element(),
                        );
                        actions.push(
                            threadlane_ui_kit::queued_edit_button(&message_id)
                                .on_click(cx.listener(move |this, _event, window, cx| {
                                    if !this.input_state.read(cx).value().is_empty()
                                        || !this.pasted_images.is_empty()
                                    {
                                        window.push_notification(
                                            Notification::info(
                                                "Send or clear your draft before editing a message",
                                            ),
                                            cx,
                                        );
                                        this.focus_composer(window, cx);
                                        return;
                                    }
                                    let restored = queued_edit_model.update(cx, |state, cx| {
                                        let restored = state
                                            .edit_queued_message(&entry_id)
                                            .map_err(|error| {
                                                state.session_status = Some(error);
                                            })
                                            .ok();
                                        cx.notify();
                                        restored
                                    });
                                    if let Some((text, images)) = restored {
                                        this.prompt_recall = None;
                                        this.pasted_images.extend(images);
                                        queued_edit_input.update(cx, |input, cx| {
                                            input.set_value(text, window, cx);
                                        });
                                    }
                                }))
                                .into_any_element(),
                        );
                        actions.push(
                            threadlane_ui_kit::queued_remove_button(&message_id)
                                .on_click(move |_event, _window, cx| {
                                    queued_remove_model.update(cx, |state, cx| {
                                        controller::dispatch(
                                            state,
                                            AppAction::RemoveQueuedMessage {
                                                entry_id: remove_entry_id.clone(),
                                            },
                                        );
                                        cx.notify();
                                    });
                                })
                                .into_any_element(),
                        );
                    }
                    threadlane_ui_kit::queued_message_row(&message_id, text, removing, actions, cx)
                        .into_any_element()
                })
                .collect::<Vec<_>>();
            threadlane_ui_kit::queued_message_panel(
                format!(
                    "queued-messages-list-{}",
                    queued_session_id.as_deref().unwrap_or("new-task")
                ),
                count,
                rows,
                cx,
            )
        });
        let pending_preview = pending_message.map(|text| {
            threadlane_ui_kit::pending_message_row(
                text,
                [
                    threadlane_ui_kit::pending_queue_button()
                        .on_click(move |_event, _window, cx| {
                            queue_model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::QueuePendingMessage);
                                cx.notify();
                            });
                        })
                        .into_any_element(),
                    threadlane_ui_kit::pending_steer_button(supports_live_steering, steer_tooltip)
                        .on_click(move |_event, _window, cx| {
                            steer_model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::SteerPendingMessage);
                                cx.notify();
                            });
                        })
                        .into_any_element(),
                    threadlane_ui_kit::pending_edit_button()
                        .on_click(cx.listener(move |this, _event, window, cx| {
                            let restored = dismiss_model.update(cx, |state, cx| {
                                let restored =
                                    state.active_pending_composer_message().map(str::to_owned);
                                controller::dispatch(state, AppAction::DismissPendingMessage);
                                cx.notify();
                                restored
                            });
                            if let Some(restored) = restored {
                                this.prompt_recall = None;
                                dismiss_input.update(cx, |input, cx| {
                                    input.set_value(restored, window, cx);
                                });
                            }
                        }))
                        .into_any_element(),
                ],
                cx,
            )
        });

        // The picker owns a per-open snapshot (model_picker.rs): one state
        // per project/session owner, refreshed only while the popup is
        // closed, so a catalog update can never reorder rows under the
        // keyboard cursor mid-open.
        let picker_owner = PickerOwner::capture(self.model.read(cx));
        if self.model_picker.is_none() || self.model_picker_owner.as_ref() != Some(&picker_owner) {
            let delegate = ModelPickerDelegate::new(model_picker::picker_sections(
                &model_options,
                &acp_model_sections,
                &selected_model,
            ));
            let picker_state =
                cx.new(|cx| ComboboxState::new(delegate, Vec::new(), window, cx).searchable(true));
            self.model_picker_subscription = Some(cx.subscribe_in(
                &picker_state,
                window,
                |this, picker, event, window, cx| match event {
                    ComboboxEvent::Change(values) => {
                        if let Some(value) = values.first().cloned() {
                            this.commit_model_picker_choice(value, window, cx);
                        }
                        // The kit detects a selection change by comparing
                        // IndexPaths, not values: a kept selection at
                        // {section, row} would suppress the next commit at
                        // the same filtered index. Rows are actions, so the
                        // selection resets after every commit.
                        picker.update(cx, |picker, cx| {
                            picker.set_selected_indices(Vec::new(), window, cx);
                        });
                    }
                    ComboboxEvent::Confirm(_) => {}
                },
            ));
            self.model_picker = Some(picker_state);
            self.model_picker_owner = Some(picker_owner);
        } else if !self.model_picker_open.get() {
            // Closed: track the live catalog. Swapping the delegate wholesale
            // keeps the snapshot consistent, and clearing the query makes
            // the next open land on the full list with the cursor on row one.
            let delegate = ModelPickerDelegate::new(model_picker::picker_sections(
                &model_options,
                &acp_model_sections,
                &selected_model,
            ));
            if let Some(picker_state) = self.model_picker.as_ref() {
                picker_state.update(cx, |picker, cx| {
                    picker.set_items(delegate, window, cx);
                    picker.set_selected_indices(Vec::new(), window, cx);
                    if !picker.query(cx).is_empty() {
                        picker.set_query("", window, cx);
                    }
                });
            }
        }
        let Some(picker_state) = self.model_picker.clone() else {
            unreachable!("picker is created or already present above");
        };
        let picker_open_flag = self.model_picker_open.clone();
        let model_label_for_trigger = model_label.clone();
        let provider_icon = selected_option
            .as_ref()
            .map(|option| SharedString::from(option.provider.icon_path()));
        let model_picker = threadlane_ui_kit::composer_model_picker(
            &picker_state,
            model_label_for_trigger,
            has_models,
            provider_icon,
            move |open| picker_open_flag.set(open),
            cx,
        );

        let effort_model = self.model.clone();
        let effort_options =
            threadlane_daemon::catalog::efforts_for_model(&selected_model, project_root.as_deref());
        // Models without thinking (ACP agents, off-only registry entries)
        // offer no effort control instead of dead options.
        let show_effort_picker =
            threadlane_daemon::catalog::supports_reasoning(&selected_model, project_root.as_deref());
        let effort_picker = threadlane_ui_kit::composer_effort_button(reasoning_effort.label())
            .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, window, _cx| {
                let menu = menu.check_side(gpui_component::Side::Right);
                effort_options
                    .clone()
                    .into_iter()
                    .fold(menu, |menu, effort| {
                        let model = effort_model.clone();
                        menu.item(
                            PopupMenuItem::new(effort.label())
                                .icon(Icon::default().path("icons/effort.svg"))
                                .checked(effort == reasoning_effort)
                                .on_click(move |_event, _window, cx| {
                                    model.update(cx, |state, cx| {
                                        controller::dispatch(
                                            state,
                                            AppAction::SelectReasoningEffort(effort),
                                        );
                                        cx.notify();
                                    });
                                }),
                        )
                    })
            });
        #[cfg(test)]
        let effort_picker = effort_picker.on_open_change({
            let open = self.reasoning_menu_open.clone();
            move |is_open, _, _| open.set(*is_open)
        });

        // Session mode dropdown, mirroring the model picker: Agent runs
        // every prompt directly on the selected model, Fusion routes through
        // frontier-main + sidekick lanes.
        let mode_model = self.model.clone();
        let has_mode_project = self.model.read(cx).active_work_dir.is_some();
        let mode_picker = threadlane_ui_kit::composer_mode_button(orchestrator_mode.label(), has_mode_project)
            .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _window, _cx| {
                let menu = menu.check_side(gpui_component::Side::Right);
                [
                    threadlane_protocol::OrchestratorMode::Normal,
                    threadlane_protocol::OrchestratorMode::Fusion,
                ]
                .into_iter()
                .fold(menu, |menu, mode| {
                    let model = mode_model.clone();
                    menu.item(
                        PopupMenuItem::new(mode.label())
                            .checked(mode == orchestrator_mode)
                            .on_click(move |_event, _window, cx| {
                                model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::SelectOrchestratorMode(mode),
                                    );
                                    cx.notify();
                                });
                            }),
                    )
                })
            });

        let input_value = self.input_state.read(cx).value().to_string();
        // Arm Up/Down recall interception while browsing or while an empty
        // composer could enter browsing; everything else keeps native arrows.
        let prompt_recall_keys_active =
            self.prompt_recall.is_some() || (input_value.is_empty() && self.pasted_images.is_empty());
        let mut slash_completion_active = false;
        let command_menu = if let Some(query) = active_slash_command_query(&input_value) {
            if self.dismiss_slash_menu {
                div().into_any_element()
            } else {
                let commands = self
                    .cached_slash_commands(project_root.as_deref())
                    .into_iter()
                    .filter(|command| query.is_empty() || command.name.starts_with(query))
                    .collect::<Vec<_>>();
                let command_count = commands.len();
                let has_commands = command_count > 0;
                slash_completion_active = has_commands;
                let selected_idx = self
                    .selected_slash_index
                    .min(command_count.saturating_sub(1));
                div()
                    .absolute()
                    .bottom_full()
                    .left(rems(0.0))
                    .mb_2()
                    .w_full()
                    .max_w(rems(40.0))
                    .max_h(rems(20.0))
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .shadow_xl()
                    .p_1p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .h_7()
                            .px_2()
                            .border_b_1()
                            .border_color(theme.border.opacity(0.4))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div().font_weight(FontWeight::SEMIBOLD).child("Commands"),
                                    )
                                    .child(
                                        div()
                                            .text_color(theme.muted_foreground)
                                            .child("↑↓ navigate · Tab/Enter select · Esc dismiss"),
                                    ),
                            )
                            .child(if has_commands {
                                format!("{}/{}", selected_idx + 1, command_count)
                            } else {
                                "0/0".to_string()
                            }),
                    )
                    .child(
                        div()
                            .id("slash-command-list")
                            .role(Role::List)
                            .relative()
                            .mt_1()
                            .track_scroll(&self.slash_scroll_handle)
                            .overflow_y_scroll()
                            .vertical_scrollbar(&self.slash_scroll_handle)
                            .max_h(rems(16.25))
                            .when(!has_commands, |list| {
                                list.child(
                                    div()
                                        // Same row height as command rows so
                                        // filtering to empty doesn't jump.
                                        .h(rems(1.875))
                                        .flex()
                                        .items_center()
                                        .px_2()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("No matching commands"),
                                )
                            })
                            .children(commands.into_iter().enumerate().map(|(idx, command)| {
                                let is_active = idx == selected_idx;
                                let command_label =
                                    format!("/{}: {}", &command.name, &command.description);
                                let command_name = command.name.clone();
                                div()
                                    .id(SharedString::from(format!(
                                        "composer-command-{}",
                                        command.name
                                    )))
                                    .role(Role::Button)
                                    .aria_label(command_label)
                                    .h(rems(1.875))
                                    .flex()
                                    .items_center()
                                    .rounded_md()
                                    .px_2()
                                    .text_sm()
                                    .bg(if is_active {
                                        theme.accent.opacity(0.16)
                                    } else {
                                        gpui::transparent_black()
                                    })
                                    .hover(|style| style.bg(theme.list_hover))
                                    .child(
                                        div()
                                            .w(rems(10.0))
                                            .flex_none()
                                            .truncate()
                                            .font_weight(if is_active {
                                                FontWeight::BOLD
                                            } else {
                                                FontWeight::SEMIBOLD
                                            })
                                            .text_color(if is_active {
                                                theme.primary
                                            } else {
                                                theme.foreground
                                            })
                                            .child(format!("/{}", command.name)),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_color(if is_active {
                                                theme.foreground
                                            } else {
                                                theme.muted_foreground
                                            })
                                            .child(command.description),
                                    )
                                    .on_click(cx.listener(move |this, _event, window, cx| {
                                        this.complete_slash_command(&command_name, window, cx);
                                    }))
                            })),
                    )
                    .into_any_element()
            }
        } else {
            div().into_any_element()
        };

        // `@` file completion: the trigger is recomputed from the live caret
        // each frame (caret moves emit no events); state is refreshed on
        // Change and cleared when the trigger disappears or the draft,
        // session, or tab changes.
        let file_trigger = if self.dismiss_file_menu {
            None
        } else {
            self.current_file_trigger(cx)
        };
        // A trigger can exist without picker state (e.g. a restored draft
        // ending in `@`, whose set_value emits no Change), and stored state
        // can go stale without a Change event — e.g. an `Unsupported` state
        // recorded while the session worktree was still preparing. Resync
        // whenever the resolved root no longer matches the retained one.
        let git_root = self.model.read(cx).active_git_work_dir();
        let file_state_stale = self
            .file_completion
            .as_ref()
            .map(|state| state.root.as_deref() != git_root.as_deref())
            .unwrap_or(true);
        if file_trigger.is_some() && file_state_stale {
            cx.defer_in(window, |this, _window, cx| {
                this.sync_file_completion(cx);
            });
        }
        let file_completion_active = file_trigger.is_some();
        let file_menu = file_trigger
            .as_ref()
            .map(|trigger| self.render_file_menu(trigger, cx))
            .unwrap_or_else(|| div().into_any_element());
        let meter = context_meter_view_model(
            context_window.as_ref(),
            &ContextMeterMetrics {
                billed_input_tokens: metrics.billed_input_tokens(),
                output_tokens: metrics.output_tokens,
                cache_hit_percent: metrics.cache_hit_percent(),
            },
            !threadlane_acp_engine::is_acp_model(&selected_model),
        );
        let context_percent_label = meter.percent.map(|percent| format!("{percent:.0}%"));
        let subagent_popover = self.render_subagent_popover(cx);
        let sync_context_meter = cx.entity().downgrade();
        let context_meter = threadlane_ui_kit::context_meter::context_meter_popover(
            meter, self.context_meter_open,
            move |open, _, cx| {
                let _ = sync_context_meter.update(cx, |this, cx| {
                    if this.context_meter_open != *open { this.context_meter_open = *open; cx.notify(); }
                });
            }, cx,
        );

        let stashed_draft = active_session_id
            .as_ref()
            .and_then(|id| self.model.read(cx).get_stashed_prompt(id).cloned());
        let stash_model = self.model.clone();
        let stash_input = self.input_state.clone();
        let stash_session_id = active_session_id.clone();
        let stash_banner = stashed_draft.map(|draft| {
            let restore_input = stash_input.clone();
            let restore_model = stash_model.clone();
            let restore_session_id = stash_session_id.clone();
            let dismiss_model = stash_model.clone();
            let dismiss_session_id = stash_session_id.clone();
            threadlane_ui_kit::saved_draft_banner(
                &draft,
                threadlane_ui_kit::restore_saved_draft_button().on_click(cx.listener(
                    move |this, _event, window, cx| {
                        if !restore_input.read(cx).value().is_empty() || !this.pasted_images.is_empty()
                        {
                            window.push_notification(
                                Notification::info(
                                    "Send or clear your draft before restoring the saved draft",
                                ),
                                cx,
                            );
                            this.focus_composer(window, cx);
                            return;
                        }
                        if let Some(session_id) = &restore_session_id {
                            if let Some(text) = restore_model.update(cx, |state, cx| {
                                let popped = state.pop_stashed_prompt(session_id);
                                cx.notify();
                                popped
                            }) {
                                this.prompt_recall = None;
                                restore_input.update(cx, |input, cx| {
                                    input.set_value(text, window, cx);
                                });
                            }
                            this.focus_composer(window, cx);
                        }
                    },
                )),
                threadlane_ui_kit::discard_saved_draft_button().on_click(move |_event, window, cx| {
                    let Some(session_id) = dismiss_session_id.clone() else {
                        return;
                    };
                    let Some(draft) = dismiss_model
                        .read(cx)
                        .get_stashed_prompt(&session_id)
                        .cloned()
                    else {
                        return;
                    };
                    let model = dismiss_model.clone();
                    window.open_alert_dialog(cx, move |alert, _, _| {
                        let model = model.clone();
                        let session_id = session_id.clone();
                        let draft = draft.clone();
                        threadlane_ui_kit::discard_saved_draft_dialog(alert, draft.clone()).on_ok(
                            move |_, _, cx| {
                                model.update(cx, |state, cx| {
                                    if state.get_stashed_prompt(&session_id) == Some(&draft) {
                                        state.clear_stashed_prompt(&session_id);
                                        cx.notify();
                                    }
                                });
                                true
                            },
                        )
                    });
                }),
                cx,
            )
        });

        let stash_button = {
            let do_stash_input = self.input_state.clone();
            let do_stash_model = self.model.clone();
            let do_stash_session_id = active_session_id.clone();
            let stash_unavailable_reason = if is_generating {
                Some("Wait for the current turn to finish before saving a draft")
            } else if active_session_id.is_none() {
                Some("Start a task before saving a draft")
            } else if active_session_id
                .as_ref()
                .is_some_and(|id| self.model.read(cx).get_stashed_prompt(id).is_some())
            {
                Some("Restore or discard the saved draft before saving another")
            } else if !self.pasted_images.is_empty() {
                Some("Saved drafts support text only; remove attached images first")
            } else if !has_composer_text {
                Some("Type a message to save")
            } else {
                None
            };
            // Offered only once there is a task and something to save; the
            // remaining reasons are transient and keep it visible but disabled.
            let stash_offered = active_session_id.is_some() && has_composer_text;
            stash_offered.then(|| threadlane_ui_kit::save_draft_button(stash_unavailable_reason)
                .on_click(move |_event, window, cx| {
                    if let Some(session_id) = &do_stash_session_id {
                        let text = do_stash_input.read(cx).value().to_string();
                        if do_stash_model
                            .read(cx)
                            .get_stashed_prompt(session_id)
                            .is_some()
                        {
                            return;
                        }
                        if !text.trim().is_empty() {
                            do_stash_model.update(cx, |state, cx| {
                                state.stash_prompt(session_id, text);
                                cx.notify();
                            });
                            do_stash_input.update(cx, |input, cx| {
                                input.set_value("", window, cx);
                            });
                        }
                    }
                }))
        };

        // The composer-level "Recall previous prompt" command: same gates as
        // the Up-arrow path, disabled with a reason when ineligible.
        let prompt_recall_button = {
            let recall_unavailable_reason = if self.prompt_recall.is_some() {
                None
            } else {
                self.prompt_recall_block_reason(has_composer_text, cx)
            };
            // Hidden while there is nothing to recall at all; transient
            // blocks (typing, a running turn) keep it visible but disabled.
            let recall_offered = self.prompt_recall.is_some() || {
                let state = self.model.read(cx);
                !state.is_new_task && state.active_session_id.is_some()
            } && !self.recallable_prompts(cx).is_empty();
            recall_offered.then(|| threadlane_ui_kit::recall_prompt_button(recall_unavailable_reason)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.step_prompt_recall(true, window, cx);
                    this.focus_composer(window, cx);
                })))
        };

        let setup_card = self
            .model
            .read(cx)
            .active_worktree_setup()
            .cloned()
            .map(|setup| {
                use threadlane_ui_state::worktree_setup::SetupStage;
                let retry = self.model.clone();
                let cancel = self.model.clone();
                let failed = setup.error.is_some();
                div()
                    .flex()
                    .flex_col()
                    .id("worktree-setup")
                    .mb_2()
                    .p_3()
                    .gap_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(if failed {
                                "Worktree setup failed"
                            } else {
                                "Preparing environment…"
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("Base: {}", setup.base)),
                    )
                    .children(
                        [
                            SetupStage::Naming,
                            SetupStage::Creating,
                            SetupStage::Starting,
                        ]
                        .into_iter()
                        .map(|stage| {
                            let status = if stage < setup.stage {
                                "Done"
                            } else if stage == setup.stage {
                                if failed {
                                    "Failed"
                                } else {
                                    "In progress"
                                }
                            } else {
                                "Pending"
                            };
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .text_color(if stage == setup.stage && failed {
                                    theme.danger
                                } else {
                                    theme.muted_foreground
                                })
                                .when(stage == setup.stage && !failed, |row| {
                                    row.child(gpui_component::spinner::Spinner::new().xsmall())
                                })
                                .when(stage < setup.stage, |row| {
                                    row.child(Icon::new(IconName::Check).xsmall())
                                })
                                .child(format!("{} · {status}", stage.label()))
                        }),
                    )
                    .children(
                        setup
                            .branch
                            .as_ref()
                            .map(|branch| div().text_xs().child(branch.clone())),
                    )
                    .children(
                        setup
                            .error
                            .map(|error| div().text_xs().text_color(theme.danger).child(error)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(failed, |row| {
                                row.child(
                                    Button::new("worktree-setup-retry")
                                        .label("Retry setup")
                                        .small()
                                        .outline()
                                        .on_click(move |_, _, cx| {
                                            retry.update(cx, |state, cx| {
                                                state.retry_worktree_setup();
                                                cx.notify();
                                            });
                                        }),
                                )
                            })
                            .child(
                                Button::new("worktree-setup-cancel")
                                    .label("Cancel setup")
                                    .small()
                                    .ghost()
                                    .on_click(move |_, _, cx| {
                                        cancel.update(cx, |state, cx| {
                                            controller::dispatch(
                                                state,
                                                AppAction::CancelGeneration,
                                            );
                                            cx.notify();
                                        });
                                    }),
                            ),
                    )
            });

        threadlane_ui_kit::composer_container(cx)
            .children(provider_setup_banner)
            .children(setup_card)
            .children(session_status.map(|status| {
                let (summary, needs_provider_settings) = chat_error_summary(&status);
                let is_error = status.starts_with("Could not")
                    || status.starts_with("Failed")
                    || status.starts_with("Error")
                    || needs_provider_settings;
                if is_error {
                    return render_chat_error("session-status", &status, None, &self.model, cx)
                        .into_any_element();
                }
                div()
                    .w_full()
                    .max_w(rems(CHAT_CONTENT_MAX_WIDTH))
                    .mx_auto()
                    .mb_2()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(IconName::Asterisk)
                    .child(div().min_w_0().child(summary))
                    .into_any_element()
            }))
            .children(pending_preview)
            .children(queued_preview)
            .child(composer_context_bar)
            .child(
                threadlane_ui_kit::composer_surface(
                    self.input_state.read(cx).focus_handle(cx).is_focused(window), cx,
                )
                    .on_action(cx.listener(Self::paste_composer_clipboard))
                    .map(|composer| {
                        let mut contexts = String::new();
                        if slash_completion_active {
                            contexts.push_str(SLASH_COMMAND_KEY_CONTEXT);
                        }
                        if file_completion_active {
                            if !contexts.is_empty() {
                                contexts.push(' ');
                            }
                            contexts.push_str(FILE_COMPLETION_KEY_CONTEXT);
                        }
                        if prompt_recall_keys_active {
                            if !contexts.is_empty() {
                                contexts.push(' ');
                            }
                            contexts.push_str(PROMPT_RECALL_KEY_CONTEXT);
                        }
                        if contexts.is_empty() {
                            composer
                        } else {
                            composer
                                .key_context(contexts.as_str())
                                .on_action(cx.listener(Self::complete_slash_command_action))
                                .on_action(cx.listener(Self::select_previous_slash_command_action))
                                .on_action(cx.listener(Self::select_next_slash_command_action))
                                .on_action(cx.listener(Self::dismiss_slash_command_action))
                                .on_action(cx.listener(Self::complete_file_completion_action))
                                .on_action(cx.listener(Self::select_previous_file_completion_action))
                                .on_action(cx.listener(Self::select_next_file_completion_action))
                                .on_action(cx.listener(Self::dismiss_file_completion_action))
                                .on_action(cx.listener(Self::recall_older_prompt_action))
                                .on_action(cx.listener(Self::recall_newer_prompt_action))
                        }
                    })
                    .children(stash_banner)
                    .children(self.prompt_recall.is_some().then(|| {
                        self.render_prompt_recall_strip(cx)
                    }))
                    .children(
                        (!image_chips.is_empty())
                            .then(|| threadlane_ui_kit::composer_attachment_group().children(image_chips)),
                    )
                    .child(command_menu)
                    .child(file_menu)
                    .child(
                        div()
                            .w_full()
                            .flex_1()
                            .min_h_6()
                            .child(
                                threadlane_ui_kit::composer_input(&self.input_state),
                            ),
                    )
                    .child(
                        threadlane_ui_kit::composer_toolbar(cx)
                            .child(
                                threadlane_ui_kit::composer_picker_group()
                                    .child(model_picker)
                                    .child(mode_picker)
                                    .children(show_effort_picker.then_some(effort_picker)),
                            )
                            .child(div().flex_1().min_w_2())
                            .child(
                                threadlane_ui_kit::composer_actions_group()
                                    .children(stash_button)
                                    .children(prompt_recall_button)
                                    .children(subagent_popover)
                                    .children(context_percent_label.map(|label| {
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(label)
                                    }))
                                    // Nothing to measure before the first exchange.
                                    .children((!hero_visible).then_some(context_meter))
                                    .child({
                                        let send_hint = if preparing_worktree {
                                            "Wait for worktree setup to finish before sending"
                                        } else if needs_provider {
                                            "Connect a model provider in Settings before sending"
                                        } else if !has_prompt {
                                            "Type a message to send"
                                        } else if is_generating {
                                            "Send message (Enter); queues after this turn"
                                        } else {
                                            "Send message (Enter)"
                                        };
                                        threadlane_ui_kit::composer_send_button(has_prompt && !needs_provider && !preparing_worktree, send_hint)
                                            .on_click(cx.listener(move |this, _event, window, cx| {
                                                let text = send_input.read(cx).value().to_string();
                                                if !text.trim().is_empty()
                                                    || (!is_generating && !this.pasted_images.is_empty())
                                                {
                                                    this.prompt_recall = None;
                                                    let images = std::mem::take(&mut this.pasted_images);
                                                    send_model.update(cx, |state, cx| {
                                                        if is_generating {
                                                            // Sending while a turn runs queues by
                                                            // default; steer lives on the queued
                                                            // message rows above the composer.
                                                            controller::dispatch(state, AppAction::StageBusyMessage { text, images });
                                                            controller::dispatch(state, AppAction::QueuePendingMessage);
                                                        } else {
                                                            controller::dispatch(state, AppAction::SendPromptWithImages { text, images });
                                                        }
                                                        cx.notify();
                                                    });
                                                    send_input.update(cx, |state, cx| {
                                                        state.set_value("", window, cx);
                                                    });
                                                    this.transcript.list.scroll_to_end();
                                                    cx.notify();
                                                }
                                            }))
                                    })
                                    .children(is_generating.then(|| {
                                        Button::new("composer-stop-btn")
                                            .debug_selector(|| "composer-stop-btn".into())
                                            .icon(IconName::CircleX)
                                            .accessibility_label("Stop generation")
                                            .small()
                                            .rounded_full()
                                            .danger()
                                            .tooltip("Stop generation (Esc)")
                                            .on_click(cx.listener(move |_this, _event, _window, cx| {
                                                cancel_model.update(cx, |state, cx| {
                                                    controller::dispatch(
                                                        state,
                                                        AppAction::CancelGeneration,
                                                    );
                                                    cx.notify();
                                                });
                                            }))
                                    })),
                            ),
                    ),
            )
            .child(
                threadlane_ui_kit::composer_shortcuts(composer_shortcut_hint, cx),
            )
    }
}

impl ChatListView {
    fn render_progress_summary(&self, cx: &mut Context<Self>) -> AnyElement {
        let (summary, category, tool_detail, active_subagent_tasks) = {
            let state = self.model.read(cx);
            let latest_tool = current_turn_latest_tool(&state.messages);
            let active_subagents = state
                .active_subagents()
                .iter()
                .filter(|subagent| {
                    matches!(
                        subagent.status,
                        SubagentActivityStatus::Queued | SubagentActivityStatus::Running
                    )
                })
                .map(|s| (s.agent.clone(), s.task.clone()))
                .collect::<Vec<_>>();
            let summary = latest_tool
                .map(|tool| {
                    if tool.display_summary.trim().is_empty() {
                        tool.title.clone()
                    } else {
                        tool.display_summary.clone()
                    }
                })
                .unwrap_or_else(|| "Generating response".to_string());
            let category = latest_tool
                .map(|tool| tool.category.clone())
                .unwrap_or_else(|| "Agent activity".to_string());
            let tool_detail = latest_tool
                .map(|tool| tool.detail.trim().to_string())
                .filter(|detail| !detail.is_empty());
            (summary, category, tool_detail, active_subagents)
        };
        let theme = cx.theme().colors;
        let is_latest_error = category == "Error";
        let summary_color = if is_latest_error {
            theme.danger
        } else {
            theme.muted_foreground
        };
        let disclosure_label = format!(
            "{} activity details: {summary}",
            if self.progress_summary_expanded {
                "Collapse"
            } else {
                "Expand"
            }
        );
        let elapsed = self
            .model
            .read(cx)
            .active_run_elapsed_seconds()
            .map(format_run_elapsed);
        let disclosure_label = match &elapsed {
            Some(elapsed) => format!("{disclosure_label}; elapsed {elapsed}"),
            None => disclosure_label,
        };
        let subagent_count = active_subagent_tasks.len();
        let subagent_label = (subagent_count > 0).then(|| {
            format!(
                "{} subagent{}",
                subagent_count,
                if subagent_count == 1 { "" } else { "s" }
            )
        });

        let mut container = div()
            .id("chat-progress-summary")
            .debug_selector(|| "chat-progress-summary".into())
            .flex_none()
            .w_full()
            .max_w(rems(CHAT_CONTENT_MAX_WIDTH))
            .mx_auto()
            .px_4()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .border_t_1()
            .border_color(theme.border.opacity(0.4))
            .bg(theme.secondary.opacity(0.3));

        let header_row = div()
            .flex()
            .items_center()
            .gap_2()
            .child(Spinner::new().xsmall())
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(progress_header_prefix(is_latest_error).map(|prefix| {
                        div()
                            .flex_none()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.danger)
                            .child(prefix)
                    }))
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(summary_color)
                            .debug_selector(|| "progress-summary-text".into())
                            .min_w_0()
                            .flex_1()
                            .child(summary),
                    ),
            )
            .children(subagent_label.or_else(|| (!is_latest_error).then_some(category)).map(|label| {
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(label)
            }))
            .children(elapsed.map(|elapsed| {
                div()
                    .debug_selector(|| "progress-summary-elapsed".into())
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(elapsed)
            }))
            .child(
                div().debug_selector(|| "progress-summary-chevron".into()).flex_none().text_color(theme.muted_foreground).child(
                    Icon::new(if self.progress_summary_expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .xsmall(),
                ),
            );

        container = container.child(
            Button::new("progress-summary-disclosure")
                .debug_selector(|| "progress-summary-disclosure".into())
                .tooltip(disclosure_label.clone())
                .accessibility_label(disclosure_label)
                .ghost()
                .h_auto()
                .w_full()
                .p_0()
                .on_click(cx.listener(|this, _, _, cx| this.toggle_progress_summary(cx)))
                .child(header_row.w_full()),
        );

        if self.progress_summary_expanded {
            let mut expanded_content = div()
                .debug_selector(|| "progress-summary-details".into())
                .flex()
                .flex_col()
                .gap_1p5()
                .pt_1p5()
                .border_t_1()
                .border_color(theme.border.opacity(0.5));

            if let Some(detail) = tool_detail {
                expanded_content = expanded_content.child(
                    div()
                        .max_h(rems(7.5))
                        .overflow_y_scrollbar()
                        .px_2()
                        .py_1()
                        .rounded_lg()
                        .bg(theme.background)
                        .border_1()
                        .border_color(theme.border.opacity(0.4))
                        .text_xs()
                        .text_color(theme.foreground)
                        .child(detail),
                );
            }

            if !active_subagent_tasks.is_empty() {
                let overflow_count = active_subagent_tasks.len().saturating_sub(3);
                let subagents_view = div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(
                        active_subagent_tasks
                            .into_iter()
                            .take(3)
                            .map(|(agent, task)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .text_xs()
                                    .child(
                                        div()
                                            .flex_none()
                                            .px_1p5()
                                            .py(rems(0.03125))
                                            .rounded_lg()
                                            .bg(theme.accent.opacity(0.15))
                                            .text_color(theme.accent)
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(agent),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_color(theme.muted_foreground)
                                            .child(task),
                                    )
                            }),
                    )
                    .children((overflow_count > 0).then(|| {
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("+{overflow_count} more"))
                    }));
                expanded_content = expanded_content.child(subagents_view);
            }

            expanded_content = expanded_content.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Activity details")
                    .child(
                        Button::new("progress-open-trajectory")
                            .debug_selector(|| "progress-open-trajectory".into())
                            .label("Open trajectory")
                            .icon(IconName::ChevronRight)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.progress_summary_expanded = false;
                                this.clear_conversation_find();
                                cx.notify();
                                window.dispatch_action(Box::new(crate::OpenWorkspaceTrajectory), cx);
                            })),
                    ),
            );

            container = container.child(expanded_content);
        }

        container.into_any_element()
    }

    fn toggle_progress_summary(&mut self, cx: &mut Context<Self>) {
        self.progress_summary_expanded = !self.progress_summary_expanded;
        cx.notify();
    }
}

impl ChatListView {
    /// Show the computer-use mirror as a floating panel over the chat. The
    /// entity is created on first open; repeated triggers only raise the
    /// flag. The mirror is global (one panel, many project sessions) and the
    /// session tools write `latest.json` into the global previews dir.
    pub(crate) fn open_mirror(&mut self, cx: &mut Context<Self>) {
        let previews_dir = self.model.update(cx, |state, cx| {
            if !state.mirror_open {
                state.mirror_open = true;
                cx.notify();
            }
            threadlane_computer::resolve_previews_dir(state.active_work_dir.as_deref())
        });
        if self.mirror.is_none() {
            let Some(previews_dir) = previews_dir else {
                return;
            };
            let model = self.model.clone();
            let mirror = cx.new(|cx| MirrorView::new(model, previews_dir, cx));
            let subscription = cx.observe(&mirror, |_this, _mirror, cx| cx.notify());
            self.mirror = Some((mirror, subscription));
        }
        cx.notify();
    }
}

impl Render for ChatListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (messages, is_new_task, active_plan, session_key, is_generating, active_permission_id, on_chat_page) = {
            let state = self.model.read(cx);
            let active_permission_id = state
                .active_session_id
                .as_ref()
                .and_then(|session_id| state.pending_permissions.get(session_id))
                .map(|request| request.id.clone());
            if !state.mirror_open {
                // Dropping the entity ends its frame subscription, which is
                // what lets the poller leave its video tier.
                self.mirror = None;
            }
            (
                state.messages.clone(),
                state.is_new_task,
                state.active_plan.clone(),
                state
                    .active_work_dir
                    .clone()
                    .zip(state.active_session_id.clone()),
                state.is_generating,
                active_permission_id,
                state.workspace_page == WorkspacePage::Chat,
            )
        };
        let session_changed = session_key != self.last_session_key;
        if session_changed {
            self.clear_conversation_find();
            self.prompt_recall = None;
            self.outline_open = false;
            self.outline_landmarks.clear();
            self.prompt_landmarks_cache = None;
            self.prompt_rail_active_id = None;
            self.outline_focus_id = None;
            self.outline_selected_id = None;
            self.markdown_cache_namespace = session_key
                .as_ref()
                .map(|(work_dir, session_id)| {
                    SharedString::from(format!("{}\0{session_id}", work_dir.display()))
                })
                .unwrap_or_else(|| SharedString::from(""));
            if markdown_cache_exceeded(self.markdown_states.len()) {
                self.markdown_states.clear();
                self.segment_cache.clear();
            }
            self.last_session_key = session_key;
            self.initial_scroll_frames = 6;
            self.selected_slash_index = 0;
            self.dismiss_slash_menu = false;
            self.clear_file_completion();
            self.question_selections.clear();
            self.question_inputs.clear();
            self.code_wrap_blocks.clear();
        }
        if self.permission_details_request.is_some()
            && (session_changed || self.permission_details_request != active_permission_id)
        {
            self.permission_details_request = None;
            window.defer(cx, |window, cx| window.close_dialog(cx));
        }
        self.sync_transcript_rows(messages.clone(), is_generating, session_changed);
        // Acknowledge a presented run completion only once the completed
        // transcript is actually visible at the tail in a foreground window
        // on the Chat page/tab. Loading states, Editor/GitHub/Settings, and
        // a background or unfocused window never clear a New result marker.
        if on_chat_page
            && self.current_tab == CentralTab::Chat
            && !is_new_task
            && !messages.is_empty()
            && self.transcript.list.is_following_tail()
            && window.is_window_active()
        {
            self.model.update(cx, |state, cx| {
                if state.acknowledge_presented_completion() {
                    cx.notify();
                }
            });
        }
        self.refresh_conversation_outline(cx);
        self.retain_prompt_recall(cx);
        if let Some(prompt) = self
            .model
            .update(cx, |state, _cx| state.requested_composer_prompt.take())
        {
            self.current_tab = CentralTab::Chat;
            self.prompt_recall = None;
            self.input_state.update(cx, |input, cx| {
                input.set_value(&prompt, window, cx);
            });
        }
        if self.initial_scroll_frames > 0 {
            self.transcript.list.scroll_to_end();
            self.initial_scroll_frames = self.initial_scroll_frames.saturating_sub(1);
        }
        let theme = cx.theme().colors;
        let show_environment = self.environment_available
            && self.current_tab == CentralTab::Chat
            && !is_new_task
            && self.model.read(cx).active_work_dir.is_some();

        threadlane_ui_kit::conversation_surface(cx)
            .when(self.current_tab == CentralTab::Chat, |el| el
                .role(Role::Group).track_focus(&self.chat_focus)
                .key_context(if self.find_open { "Conversation ConversationFindActive" } else { "Conversation" })
                .on_action(cx.listener(Self::open_conversation_find))
                .on_action(cx.listener(Self::close_conversation_find))
                .on_action(cx.listener(Self::next_conversation_match))
                .on_action(cx.listener(Self::previous_conversation_match)))
            .on_key_down(cx.listener(Self::handle_key_down))
            .child(self.render_header(cx))
            .children(self.find_open.then(|| self.render_conversation_find(cx)))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .justify_center()
                    .child(
                        threadlane_ui_kit::conversation_column(show_environment)
                            .children(self.render_session_context(cx))
                            .children(
                                (self.current_tab == CentralTab::Chat && !is_generating)
                                    .then(|| self.model.read(cx).active_run_elapsed_seconds())
                                    .flatten()
                                    .map(|seconds| threadlane_ui_kit::last_run_duration(seconds, cx)),
                            )
                            .child(match self.current_tab {
                                CentralTab::Editor => self.editor.clone().into_any_element(),
                                CentralTab::Chat => {
                                    if is_new_task {
                                        self.render_new_task(cx)
                                    } else if messages.is_empty()
                                        && self.model.read(cx).active_session_is_loading()
                                    {
                                        div()
                                            .flex_1()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .gap_2()
                                            .text_sm()
                                            .text_color(theme.muted_foreground)
                                            .child(Spinner::new().small())
                                            .child("Loading conversation…")
                                            .into_any_element()
                                    } else if messages.is_empty() {
                                        // One empty state: an empty transcript renders the
                                        // same new-task hero wherever it appears.
                                        self.render_new_task(cx)
                                    } else {
                                        let rail = self.render_prompt_rail(cx);
                                        let transcript = threadlane_ui_kit::transcript_list(&self.transcript, cx.processor(Self::render_transcript_row));
                                        threadlane_ui_kit::conversation_transcript_viewport(
                                            rail, transcript, &self.transcript.list, show_environment,
                                            cx.listener(|this, _, _, cx| {
                                                this.outline_selected_id = None;
                                                this.transcript.list.scroll_to_end();
                                                cx.notify();
                                            }),
                                        ).into_any_element()
                                    }
                                }
                            })
                            .children(
                                (self.current_tab == CentralTab::Chat && is_generating)
                                    .then(|| self.render_progress_summary(cx)),
                            )
                            .children(
                                (self.current_tab == CentralTab::Chat)
                                    .then(|| self.render_plan_tracker(&active_plan, cx))
                                    .flatten(),
                            )
                            .children(
                                (self.current_tab == CentralTab::Chat && !show_environment)
                                    .then(|| self.render_workspace_changes(cx))
                                    .flatten(),
                            )
                            .children(
                                (self.current_tab == CentralTab::Chat)
                                    .then(|| self.render_permission_prompt(cx))
                                    .flatten(),
                            )
                            .children(
                                (self.current_tab == CentralTab::Chat)
                                    .then(|| self.render_question_prompt(window, cx))
                                    .flatten(),
                            )
                            .children(
                                (self.current_tab == CentralTab::Chat)
                                    .then(|| self.render_composer(window, cx)),
                            ),
                    )
                    .children(show_environment.then(|| self.render_environment(cx))),
            )
            // The computer-use mirror floats over everything above.
            .children(self.mirror.as_ref().map(|(mirror, _)| mirror.clone()))
    }
}

#[path = "tests.rs"]
#[cfg(test)]
mod hot_path_tests;
