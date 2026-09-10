use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use std::time::Duration;

use base64::Engine as _;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::Root;
use gpui_component::button::{Button, ButtonVariants, Toggle, ToggleVariants};
use gpui_component::hover_card::HoverCard;
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::popover::Popover;
use gpui_component::progress::ProgressCircle;
use gpui_component::scroll::ScrollableElement;
use gpui_component::spinner::Spinner;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::text::{TextView, TextViewState};
use gpui_component::theme::ActiveTheme;
use gpui_component::{Disableable, Icon, IconName, Selectable, Sizable, WindowExt};

use crate::app::{actions::AppAction, controller};
use crate::screens::computer_mirror::MirrorView;
use crate::screens::editor::EditorView;
use crate::state::{
    AppState, ChatMessageInfo, ChatStreamEvent, MessageRole, SessionAttention,
    SubagentActivityInfo, SubagentActivityStatus, ToolActivityInfo, WorkMode,
};

use super::composer::*;
use super::context_meter::*;
use super::markdown::*;
use super::trajectory::*;
use super::transcript::*;

fn editor_target_matches_active_work_dir(target: &Path, active: Option<&Path>) -> bool {
    active == Some(target)
}

fn chat_error_summary(error: &str) -> (String, bool) {
    let expired = ["token_expired", "token has expired", "sign-in expired"]
        .iter()
        .any(|marker| contains_case_insensitive(error, marker));
    if expired {
        return (
            "Your provider sign-in has expired. Sign in again in Settings → Providers, then resend your message.".into(),
            true,
        );
    }
    let rejected = ["invalid_api_key", "http 401", "401 unauthorized"]
        .iter()
        .any(|marker| contains_case_insensitive(error, marker));
    if rejected {
        return (
            "The provider rejected your credentials. Check your sign-in or API key in Settings → Providers, then resend your message.".into(),
            true,
        );
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

/// Open the computer-use mirror popup: a small non-activating window in the
/// bottom-right corner showing the latest screenshot plus current action.
/// Guarded by `AppState::mirror_open` so repeated triggers reuse the window.
fn open_computer_mirror(model: &Entity<AppState>, cx: &mut AsyncApp) {
    let previews_dir = model.update(cx, |state, _cx| {
        if state.mirror_open {
            return None;
        }
        // The live mirror is global (one popup, many project sessions); the
        // session tools write latest.json/latest-frame.jpg here.
        threadlane_session::computer::global_previews_dir().or_else(|| {
            state
                .active_work_dir
                .clone()
                .map(|work_dir| work_dir.join(".threadlane").join("previews"))
        })
    });
    let Some(previews_dir) = previews_dir else {
        return;
    };
    let bounds = cx.update(|cx| {
        cx.primary_display().map(|display| {
            let area = display.visible_bounds();
            Bounds {
                origin: point(
                    area.origin.x + area.size.width - px(496.0),
                    area.origin.y + area.size.height - px(376.0),
                ),
                size: size(px(480.0), px(360.0)),
            }
        })
    });
    let Some(bounds) = bounds else {
        return;
    };
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: true,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        },
        {
            let model = model.clone();
            move |window, cx| {
                let view = MirrorView::build(model.clone(), previews_dir.clone(), window, cx);
                cx.new(|cx| Root::new(view, window, cx))
            }
        },
    );
    if opened.is_ok() {
        let _ = model.update(cx, |state, _cx| state.mirror_open = true);
    }
}

fn render_chat_error(id: &str, error: &str, model: &Entity<AppState>, cx: &App) -> Div {
    let theme = cx.theme().colors;
    let (summary, needs_provider_settings) = chat_error_summary(error);
    let details = error.to_owned();
    div().w_full().my_2().px_4().child(
        div()
            .w_full()
            .p_3()
            .rounded(cx.theme().radius)
            .bg(theme.danger.opacity(0.08))
            .border_1()
            .border_color(theme.danger.opacity(0.4))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.danger)
                    .child("Turn stopped"),
            )
            .child(
                div()
                    .mt_1()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(summary),
            )
            .child(
                div()
                    .mt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(needs_provider_settings.then(|| {
                        let model = model.clone();
                        Button::new(SharedString::from(format!("chat-error-settings-{id}")))
                            .label("Settings…")
                            .small()
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
                            .label("Copy details")
                            .ghost()
                            .small()
                            .tooltip("Copy full error to clipboard")
                            .debug_selector(|| "chat-error-copy".into())
                            .on_click(move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(details.clone()));
                                window.push_notification(
                                    Notification::info("Copied error details"),
                                    cx,
                                );
                            }),
                    ),
            ),
    )
}
use threadlane_session::commands::{SlashCommandInfo, available_slash_commands};
use threadlane_session::{ImageAttachment, PlanItemStatus, SessionPlan};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CentralTab {
    #[default]
    Chat,
    Trajectory,
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
}

pub struct ChatListView {
    model: Entity<AppState>,
    pub(crate) input_state: Entity<TextareaState>,
    pub(crate) header_left_padding: Pixels,
    transcript_list_state: ListState,
    transcript_messages: Arc<Vec<ChatMessageInfo>>,
    transcript_rows: Vec<TranscriptRow>,
    transcript_generating: bool,
    trajectory_list_state: ListState,
    expanded_activity_groups: HashSet<String>,
    progress_summary_expanded: bool,
    markdown_states: HashMap<(SharedString, String), MarkdownRenderState>,
    markdown_cache_namespace: SharedString,
    pasted_images: Vec<ImageAttachment>,
    composer_key: (Option<PathBuf>, Option<String>),
    composer_drafts: HashMap<(Option<PathBuf>, Option<String>), ComposerDraft>,
    last_session_key: Option<(std::path::PathBuf, String)>,
    initial_scroll_frames: u8,
    current_tab: CentralTab,
    editor: Entity<EditorView>,
    trajectory_mode: TrajectoryMode,
    trajectory_search: String,
    trajectory_search_input: Entity<InputState>,
    trajectory_category: Option<String>,
    trajectory_lane: Option<String>,
    selected_trajectory_index: Option<usize>,
    trajectory_inspector_tab: TrajectoryInspectorTab,
    trajectory_cache: Option<TrajectoryRenderCache>,
    trajectory_raw_json: Option<(u64, usize, String)>,
    slash_command_cache: Option<(
        Option<std::path::PathBuf>,
        std::time::Instant,
        Vec<SlashCommandInfo>,
    )>,
    slash_scroll_handle: ScrollHandle,
    selected_slash_index: usize,
    dismiss_slash_menu: bool,
    permission_details_open: bool,
    context_meter_open: bool,
    /// Selected options per question, keyed by `request_id\0question_id`.
    /// Options toggle multi-select; Submit sends one answer per question.
    question_selections: std::collections::HashMap<String, Vec<String>>,
    /// Custom-text inputs per question allowing free text, keyed the same way.
    /// Entities are created lazily when the card renders and dropped on
    /// submit/dismiss/session-switch.
    question_inputs: std::collections::HashMap<String, Entity<InputState>>,
    subagents_popover_open: bool,
    selected_subagent_run_id: Option<String>,
    copied_code_block: Option<(String, std::time::Instant)>,
    expanded_tool_aggregates: HashSet<String>,
    segment_cache: HashMap<String, (String, Vec<MarkdownSegment>)>,
    _subscriptions: Vec<Subscription>,
}
async fn next_chat_stream_batch(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<ChatStreamEvent>,
) -> Option<Vec<ChatStreamEvent>> {
    let mut events = vec![receiver.recv().await?];
    while events.len() < 128 {
        let Ok(event) = receiver.try_recv() else {
            break;
        };
        events.push(event);
    }
    Some(events)
}

impl ChatListView {
    pub(crate) fn new(
        model: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let transcript_list_state = ListState::new(0, ListAlignment::Bottom, px(600.0));
        transcript_list_state.set_follow_mode(FollowMode::Tail);
        let trajectory_list_state = ListState::new(0, ListAlignment::Top, px(400.0));
        let input_state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Ask a question, describe a task, or type / for commands...")
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .soft_wrap(true)
        });

        let trajectory_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search trajectory…"));
        let mut stream_rx = model
            .update(cx, |state, _cx| state.stream_rx.take())
            .expect("chat stream receiver was already taken");

        let editor = cx.new(|cx| EditorView::new(model.clone(), window, cx));

        let sub1 = cx.observe_in(&model, window, |this, model, window, cx| {
            this.sync_composer_draft(window, cx);
            if let Some(target) =
                model.update(cx, |state, _cx| state.requested_editor_target.take())
            {
                match target {
                    crate::state::RequestedEditorTarget::File { project, path } => {
                        let is_active = {
                            let state = model.read(cx);
                            editor_target_matches_active_work_dir(
                                &project,
                                state.active_git_work_dir().as_deref(),
                            )
                        };
                        if is_active {
                            this.current_tab = CentralTab::Editor;
                            this.editor.update(cx, |editor, cx| {
                                editor.open_file(project, &path, cx);
                            });
                        }
                    }
                    crate::state::RequestedEditorTarget::Diff {
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
                            this.current_tab = CentralTab::Editor;
                            this.editor.update(cx, |editor, cx| {
                                editor.open_diff(&path, &content, cx);
                            });
                        }
                    }
                }
            }
            cx.notify();
        });

        let sub_editor = cx.observe(&editor, |_this, _editor, cx| {
            cx.notify();
        });

        let model_clone = model.clone();
        let submit_list_state = transcript_list_state.clone();
        let sub2 = cx.subscribe_in(
            &input_state,
            window,
            move |this, input_state, event: &InputEvent, window, cx| {
                cx.notify();
                match event {
                    InputEvent::Change => {
                        this.dismiss_slash_menu = false;
                        this.selected_slash_index = 0;
                        this.slash_scroll_handle.scroll_to_item(0);
                    }
                    InputEvent::PressEnter {
                        secondary,
                        shift: false,
                    } => {
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
                                && threadlane_session::is_acp_model(&model_clone.read(cx).selected_model)
                            {
                                model_clone.update(cx, |state, cx| {
                                    state.session_status = Some("This agent does not support live steering. Use Queue to send your message after this turn.".into());
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
        cx.spawn(async move |this, cx| {
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
                    open_computer_mirror(&stream_model, cx);
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

        let sub3 = cx.observe(&trajectory_search_input, |this, input, cx| {
            this.trajectory_search = input.read(cx).value().to_lowercase();
            cx.notify();
        });

        let composer_key = {
            let state = model.read(cx);
            (
                state.active_work_dir.clone(),
                state.active_session_id.clone(),
            )
        };
        Self {
            model,
            input_state,
            header_left_padding: px(14.0),
            transcript_list_state,
            transcript_messages: Arc::new(Vec::new()),
            transcript_rows: Vec::new(),
            transcript_generating: false,
            trajectory_list_state,
            expanded_activity_groups: HashSet::new(),
            progress_summary_expanded: false,
            markdown_states: HashMap::new(),
            markdown_cache_namespace: SharedString::from(""),
            pasted_images: Vec::new(),
            composer_key,
            composer_drafts: HashMap::new(),
            last_session_key: None,
            initial_scroll_frames: 0,
            current_tab: CentralTab::Chat,
            editor,
            trajectory_mode: TrajectoryMode::Execution,
            trajectory_search: String::new(),
            trajectory_search_input,
            trajectory_category: None,
            trajectory_lane: None,
            selected_trajectory_index: None,
            trajectory_inspector_tab: TrajectoryInspectorTab::Overview,
            trajectory_cache: None,
            trajectory_raw_json: None,
            slash_command_cache: None,
            slash_scroll_handle: ScrollHandle::new(),
            selected_slash_index: 0,
            dismiss_slash_menu: false,
            permission_details_open: false,
            context_meter_open: false,
            question_selections: std::collections::HashMap::new(),
            question_inputs: std::collections::HashMap::new(),
            subagents_popover_open: false,
            selected_subagent_run_id: None,
            copied_code_block: None,
            expanded_tool_aggregates: HashSet::new(),
            segment_cache: HashMap::new(),
            _subscriptions: vec![sub1, sub2, sub3, sub_editor],
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

        // An explicit stash is separate from the unsent text and attachments in each task.
        let draft = ComposerDraft {
            text: self.input_state.read(cx).value(),
            images: std::mem::take(&mut self.pasted_images),
        };
        let previous = std::mem::replace(&mut self.composer_key, key);
        if !draft.text.is_empty() || !draft.images.is_empty() {
            self.composer_drafts.insert(previous, draft);
        }
        let draft = self
            .composer_drafts
            .remove(&self.composer_key)
            .unwrap_or_default();
        self.pasted_images = draft.images;
        self.input_state.update(cx, |input, cx| {
            input.set_value(draft.text, window, cx);
        });
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

    pub(crate) fn set_tab(&mut self, tab: CentralTab, cx: &mut Context<Self>) {
        self.current_tab = tab;
        cx.notify();
    }

    pub(crate) fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.current_tab = CentralTab::Chat;
        self.input_state.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (active_title, active_attention, linked_issue, active_work_dir) = {
            let state = self.model.read(cx);
            let active_session = state
                .projects
                .iter()
                .flat_map(|project| project.sessions.iter())
                .find(|session| state.active_session_id.as_deref() == Some(&session.id));
            let title = active_session
                .map(|session| session.title.clone())
                .unwrap_or_else(|| "New task".to_string());
            let attention = active_session
                .map(|session| state.session_attention(session))
                .unwrap_or(SessionAttention::Idle);
            let linked_issue = active_session.and_then(|session| session.github_issue.clone());
            let work_dir = active_session.map(|session| session.work_dir.clone());
            (title, attention, linked_issue, work_dir)
        };
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
                    .gap(px(4.0))
                    .px_2()
                    .py(px(1.5))
                    .rounded_full()
                    .bg(theme.warning.opacity(0.15))
                    .text_color(theme.warning)
                    .child(div().size(px(6.0)).rounded_full().bg(theme.warning))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Needs you"),
                    ),
            ),
            SessionAttention::Working => Some(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.0))
                    .px_2()
                    .py(px(1.5))
                    .rounded_full()
                    .bg(theme.accent.opacity(0.15))
                    .text_color(theme.accent)
                    .child(Spinner::new().xsmall())
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Working"),
                    ),
            ),
            SessionAttention::Ready => Some(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.0))
                    .px_2()
                    .py(px(1.5))
                    .rounded_full()
                    .bg(theme.success.opacity(0.15))
                    .text_color(theme.success)
                    .child(div().size(px(6.0)).rounded_full().bg(theme.success))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Ready"),
                    ),
            ),
            SessionAttention::Idle => None,
        };

        div()
            .h(px(52.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .pl(self.header_left_padding)
            // The workspace owns the rightmost 128px for command palette,
            // environment, and panel buttons rendered as absolute overlays.
            .pr(px(128.0))
            .border_b_1()
            .border_color(theme.title_bar_border)
            .bg(theme.title_bar)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .justify_start()
                    .min_w_0()
                    .child(
                        div()
                            .id("chat-header-title")
                            .truncate()
                            .text_sm()
                            .line_height(px(18.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .tooltip({
                                let title = active_title.clone();
                                move |window, cx| {
                                    gpui_component::tooltip::Tooltip::new(title.clone())
                                        .build(window, cx)
                                }
                            })
                            .child(active_title),
                    )
                    .children(status_badge)
                    .children(linked_issue.clone().map(|issue| {
                        let model = self.model.clone();
                        Button::new("chat-open-task-context")
                            .label(format!("#{}", issue.number))
                            .icon(IconName::Github)
                            .tooltip(format!("{} / {} · Open task context", issue.owner, issue.repo))
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                if let Some(work_dir) = active_work_dir.clone() {
                                    model.update(cx, |state, cx| {
                                        controller::dispatch(
                                            state,
                                            AppAction::OpenGitHubIssue {
                                                work_dir,
                                                number: issue.number,
                                            },
                                        );
                                        cx.notify();
                                    });
                                }
                            })
                    }))
            )
            .child(
                div()
                    .h(px(30.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .p(px(2.0))
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted.opacity(0.4))
                    .child(
                        Button::new("trajectory-tab-events")
                            .icon(Icon::default().path("icons/tabs/trajectory.svg"))
                            .label("Trajectory")
                            .tooltip("Trajectory (Execution & Diagnostics)")
                            .ghost()
                            .xsmall()
                            .selected(self.current_tab == CentralTab::Trajectory)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.current_tab = CentralTab::Trajectory;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("trajectory-tab-chat")
                            .icon(Icon::default().path("icons/tabs/chat.svg"))
                            .label("Chat")
                            .tooltip("Chat (Conversation & Turn History)")
                            .ghost()
                            .xsmall()
                            .selected(self.current_tab == CentralTab::Chat)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.current_tab = CentralTab::Chat;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("trajectory-tab-editor")
                            .icon(Icon::default().path("icons/tabs/editor.svg"))
                            .label(editor_label)
                            .tooltip("Editor (Code & Diff Review)")
                            .ghost()
                            .xsmall()
                            .selected(self.current_tab == CentralTab::Editor)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.current_tab = CentralTab::Editor;
                                cx.notify();
                            })),
                    ),
            )
            .child(div().flex_1())
    }

    /// Renders the 16px status circle used for a plan step: a bordered ✓ for
    /// completed, a spinner for in-progress (active generation), a static dot for in-progress (idle), and an empty ring for pending.
    fn plan_step_marker(
        status: PlanItemStatus,
        is_generating: bool,
        colors: gpui_component::ThemeColor,
    ) -> AnyElement {
        match status {
            PlanItemStatus::Completed => div()
                .size(px(16.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(colors.success)
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(colors.success)
                .child("✓")
                .into_any_element(),
            PlanItemStatus::InProgress => {
                if is_generating {
                    div()
                        .size(px(16.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(colors.primary)
                        .child(gpui_component::spinner::Spinner::new().xsmall())
                        .into_any_element()
                } else {
                    div()
                        .size(px(16.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_1()
                        .border_color(colors.primary)
                        .child(div().size(px(6.0)).rounded_full().bg(colors.primary))
                        .into_any_element()
                }
            }
            PlanItemStatus::Pending => div()
                .size(px(16.0))
                .flex_none()
                .rounded_full()
                .border_1()
                .border_color(colors.muted_foreground)
                .into_any_element(),
        }
    }

    fn render_plan_tracker(
        &self,
        plan: &SessionPlan,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if plan.items.is_empty() {
            return None;
        }

        let is_generating = self.model.read(cx).is_generating;
        let theme = cx.theme().colors;
        let completed = plan
            .items
            .iter()
            .filter(|item| item.status == PlanItemStatus::Completed)
            .count();
        let total = plan.items.len();
        let current_step = plan
            .items
            .iter()
            .position(|item| item.status == PlanItemStatus::InProgress)
            .or_else(|| {
                plan.items
                    .iter()
                    .position(|item| item.status == PlanItemStatus::Pending)
            })
            .map(|index| index + 1)
            .unwrap_or(total);
        let is_complete = completed == total;
        let content_plan = plan.clone();

        Some(
            HoverCard::new("session-plan-hover-card")
                .w_full()
                .flex_none()
                .anchor(Anchor::BottomCenter)
                .close_delay(Duration::from_millis(700))
                .trigger(
                    div().w_full().flex().justify_center().py_1().child(
                        Button::new("session-plan-tracker").ghost().child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(Self::plan_step_marker(
                                    if is_complete {
                                        PlanItemStatus::Completed
                                    } else {
                                        PlanItemStatus::InProgress
                                    },
                                    is_generating,
                                    theme,
                                ))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.muted_foreground)
                                        .child(format!("Step {current_step} / {total}")),
                                ),
                        ),
                    ),
                )
                .content(move |_state, _window, _cx| {
                    let colors = theme;
                    let rows = content_plan.items.iter().enumerate().map(|(index, item)| {
                        let marker = Self::plan_step_marker(item.status, is_generating, colors);
                        div().flex().items_start().gap_2().child(marker).child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .text_sm()
                                .text_color(colors.foreground)
                                .child(format!("{}. {}", index + 1, item.step)),
                        )
                    });
                    div()
                        .w(px(520.0))
                        .max_w(px(CHAT_CONTENT_MAX_WIDTH - 32.0))
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(content_plan.explanation.clone().map(|explanation| {
                            div()
                                .flex_none()
                                .pb_2()
                                .border_b_1()
                                .border_color(colors.border)
                                .text_sm()
                                .text_color(colors.muted_foreground)
                                .child(explanation)
                        }))
                        .child(
                            div()
                                .w_full()
                                .max_h(px(280.0))
                                .flex()
                                .flex_col()
                                .gap_2()
                                .overflow_y_scrollbar()
                                .children(rows),
                        )
                })
                .into_any_element(),
        )
    }

    fn render_tool_activity(
        &self,
        activity: &ToolActivityInfo,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let (marker, marker_color) = match activity.category.as_str() {
            "Error" => ("!", theme.danger),
            "Working" | "Thinking" => ("◌", theme.primary),
            "Completed" | "Edited" | "Created" | "Ran" | "Loaded" | "Explored" => {
                ("✓", theme.success)
            }
            _ => ("✓", theme.muted_foreground),
        };
        let model = self.model.clone();
        let tool_call_id = activity.id.clone();
        let has_detail = !activity.detail.trim().is_empty();
        let row_id = SharedString::from(activity.id.clone());
        let display_summary = activity.display_summary.clone();

        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .py_1()
            .child(
                div()
                    .id(row_id)
                    .tooltip({
                        let summary = display_summary.clone();
                        move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(summary.clone()).build(window, cx)
                        }
                    })
                    .h(px(28.0))
                    .px_1()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(has_detail, |row| {
                        row.cursor_pointer()
                            .hover(|row| row.bg(theme.muted))
                            .on_click(move |_event, _window, cx| {
                                model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::ToggleToolActivity(tool_call_id.clone()),
                                    );
                                    cx.notify();
                                });
                            })
                    })
                    .child({
                        let marker_el = div()
                            .w(px(18.0))
                            .flex_none()
                            .text_center()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(marker_color)
                            .child(marker);
                        marker_el.into_any_element()
                    })
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(display_summary.clone()),
                    )
                    .children(has_detail.then(|| {
                        Icon::new(if activity.is_expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall()
                        .text_color(theme.muted_foreground)
                    })),
            )
            .children(activity.is_expanded.then(|| {
                div()
                    .ml(px(26.0))
                    .mt_1()
                    .p_2()
                    .max_h(px(240.0))
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .overflow_y_scrollbar()
                    .child(activity.detail.clone())
            }))
    }

    fn render_activity_group(
        &mut self,
        messages: &[ChatMessageInfo],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const RECENT_ACTIVITY_LIMIT: usize = 4;

        let theme = cx.theme().colors;
        let activities = grouped_tool_activities(messages);
        let group_id = activities
            .clone()
            .next()
            .map(|activity| activity.id.clone())
            .unwrap_or_else(|| "empty".into());
        let is_expanded = self.expanded_activity_groups.contains(&group_id);
        let hidden_count = activities
            .clone()
            .count()
            .saturating_sub(RECENT_ACTIVITY_LIMIT);
        let visible_start = if is_expanded { 0 } else { hidden_count };
        let activity_rows = activities
            .skip(visible_start)
            .map(|activity| self.render_tool_activity(activity, cx))
            .collect::<Vec<_>>();
        let button_group_id = group_id.clone();

        div()
            .w_full()
            .min_w_0()
            .flex_none()
            .my_1()
            .px_4()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .children((hidden_count > 0).then(|| {
                        Button::new(SharedString::from(format!("activity-group-{group_id}")))
                            .xsmall()
                            .ghost()
                            .justify_start()
                            .text_color(theme.muted_foreground)
                            .label(if is_expanded {
                                "Collapse earlier activities".to_string()
                            } else {
                                format!("{hidden_count} earlier activities")
                            })
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                if !this.expanded_activity_groups.remove(&button_group_id) {
                                    this.expanded_activity_groups
                                        .insert(button_group_id.clone());
                                }
                                cx.notify();
                            }))
                    }))
                    .children(activity_rows),
            )
            .into_any_element()
    }

    fn render_working_indicator(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .my_1()
            .px_4()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .py_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.0))
                            .child(div().size(px(4.5)).rounded_full().bg(theme.primary))
                            .child(div().size(px(4.5)).rounded_full().bg(theme.primary))
                            .child(div().size(px(4.5)).rounded_full().bg(theme.primary)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child("Working…"),
                    ),
            )
            .into_any_element()
    }

    fn render_trajectory_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self
            .trajectory_cache
            .as_ref()
            .and_then(|cache| cache.rows.get(index))
            .cloned()
        else {
            return Empty.into_any_element();
        };
        let theme = cx.theme().colors;
        match row {
            TrajectoryRow::RequestHeader(request) => div()
                .h(px(28.0))
                .px_3()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(theme.border.opacity(0.5))
                .bg(theme.muted.opacity(0.35))
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.accent)
                .child(format!("Request #{request}"))
                .into_any_element(),
            TrajectoryRow::Setup => div()
                .h(px(20.0))
                .px_3()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(theme.border.opacity(0.5))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child("Setup")
                .into_any_element(),
            TrajectoryRow::TurnHeader(turn) => div()
                .h(px(22.0))
                .px_3()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(theme.border.opacity(0.5))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("Turn {turn}"))
                .into_any_element(),
            TrajectoryRow::Entry(all_index) => {
                let entry = &self
                    .trajectory_cache
                    .as_ref()
                    .expect("trajectory cache")
                    .all_entries[all_index];
                let selected = Some(all_index) == self.selected_trajectory_index;
                let preview = self
                    .trajectory_cache
                    .as_ref()
                    .expect("trajectory cache")
                    .previews[all_index]
                    .clone();
                let (badge_bg, badge_fg, badge_label): (Hsla, Hsla, SharedString) =
                    match entry.category.as_str() {
                        "Tool" | "Tool runtime" => {
                            (theme.warning.opacity(0.18), theme.warning, "TOOL".into())
                        }
                        "Provider" => (
                            theme.primary.opacity(0.18),
                            theme.primary,
                            "PROVIDER".into(),
                        ),
                        "Context Manifest" | "Manifest" => (
                            theme.accent.opacity(0.14),
                            theme.muted_foreground,
                            "MANIFEST".into(),
                        ),
                        "Request" => (theme.primary.opacity(0.16), theme.accent, "REQUEST".into()),
                        "Anomaly" => (theme.warning.opacity(0.20), theme.warning, "ANOMALY".into()),
                        "Error" => (theme.danger.opacity(0.20), theme.danger, "ERROR".into()),
                        "Input" => (theme.muted.opacity(0.8), theme.foreground, "INPUT".into()),
                        "Assistant" => (
                            theme.muted.opacity(0.8),
                            theme.foreground,
                            "ASSISTANT".into(),
                        ),
                        "Permission" => (
                            theme.warning.opacity(0.18),
                            theme.warning,
                            "PERMISSION".into(),
                        ),
                        "Subagent" => (
                            theme.primary.opacity(0.16),
                            theme.primary,
                            "SUBAGENT".into(),
                        ),
                        _ => (
                            theme.muted.opacity(0.5),
                            theme.muted_foreground,
                            entry.category.clone().into(),
                        ),
                    };
                let dot_color = if entry.diagnostics.is_anomaly || entry.category == "Anomaly" {
                    theme.warning
                } else if entry.category == "Error"
                    || entry.detail.contains("Failed")
                    || entry.detail.contains("Error")
                    || matches!(
                        entry.diagnostics.status.as_deref(),
                        Some("Failed" | "failed")
                    )
                {
                    theme.danger
                } else if entry.category == "Tool" || entry.category == "Tool runtime" {
                    theme.warning
                } else if entry.category == "Request" {
                    theme.primary
                } else {
                    theme.muted_foreground
                };
                let seq = entry.seq;
                let exit_code = entry.diagnostics.exit_code;
                let duration_ms = entry.diagnostics.duration_ms;
                let lane = entry.lane.clone();
                let view = cx.entity().clone();
                div()
                    .id(("trajectory", all_index))
                    .tooltip({
                        let tip = match lane.clone() {
                            Some(lane) => format!("{lane} · {preview}"),
                            None => preview.to_string(),
                        };
                        move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                        }
                    })
                    .h(px(34.0))
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(theme.border.opacity(0.45))
                    .cursor_pointer()
                    .border_l_2()
                    .border_color(if selected {
                        theme.accent
                    } else {
                        theme.border.opacity(0.0)
                    })
                    .when(selected, |this| this.bg(theme.accent.opacity(0.16)))
                    .hover(|style| style.bg(theme.muted.opacity(0.65)))
                    .child(div().size(px(6.0)).flex_none().rounded_full().bg(dot_color))
                    .child(
                        div()
                            .w(px(84.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .px_1p5()
                            .py_0p5()
                            .rounded_md()
                            .bg(badge_bg)
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(badge_fg)
                            .child(badge_label),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_sm()
                            .truncate()
                            .child(preview.clone()),
                    )
                    .children(exit_code.map(|code| {
                        let is_ok = code == 0;
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .bg(if is_ok {
                                theme.success.opacity(0.15)
                            } else {
                                theme.danger.opacity(0.15)
                            })
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if is_ok { theme.success } else { theme.danger })
                            .child(format!("exit {code}"))
                    }))
                    .children(duration_ms.map(|duration| {
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .bg(theme.muted.opacity(0.8))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if duration < 1000 {
                                format!("{duration}ms")
                            } else {
                                format!("{:.1}s", duration as f64 / 1000.0)
                            })
                    }))
                    .children(lane.map(|lane| {
                        div()
                            .max_w(px(110.0))
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(lane)
                    }))
                    .children(seq.map(|seq| {
                        div()
                            .w(px(52.0))
                            .text_right()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("#{seq}"))
                    }))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.selected_trajectory_index = Some(all_index);
                            this.trajectory_inspector_tab = TrajectoryInspectorTab::Overview;
                            cx.notify();
                        })
                    })
                    .into_any_element()
            }
        }
    }

    fn render_trajectory(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (revision, epoch) = match self.trajectory_mode {
            TrajectoryMode::Execution | TrajectoryMode::Requests => {
                let state = self.model.read(cx);
                (state.trajectory_revision(), state.trajectory_epoch())
            }
            TrajectoryMode::ModelContext
            | TrajectoryMode::DurableEvents
            | TrajectoryMode::Recovery => {
                let revision = self.model.read(cx).diagnostics_revision();
                (revision, revision)
            }
        };
        let key = TrajectoryCacheKey {
            revision,
            epoch,
            mode: self.trajectory_mode,
            query: self.trajectory_search.clone(),
            category: self.trajectory_category.clone(),
            lane: self.trajectory_lane.clone(),
        };
        if self
            .trajectory_cache
            .as_ref()
            .is_none_or(|cache| cache.key != key)
        {
            let cached_len = self
                .trajectory_cache
                .as_ref()
                .map_or(0, |cache| cache.all_entries.len());
            let cached_filtered_len = self
                .trajectory_cache
                .as_ref()
                .map_or(0, |cache| cache.filtered_indices.len());
            let projection_matches = self.trajectory_cache.as_ref().is_some_and(|cache| {
                cache.key.epoch == key.epoch
                    && cache.key.mode == key.mode
                    && cache.key.query == key.query
                    && cache.key.category == key.category
                    && cache.key.lane == key.lane
            });
            let mut cached_summary = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.summary))
                .unwrap_or_default();
            let mut cached_categories = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.categories))
                .unwrap_or_default();
            let mut cached_lane_latest = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.lane_latest))
                .unwrap_or_default();
            let mut cached_filtered_indices = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.filtered_indices))
                .unwrap_or_default();
            let mut cached_previews = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.previews))
                .unwrap_or_default();
            let cached_entries = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.all_entries))
                .unwrap_or_default();
            let cached_epoch = self
                .trajectory_cache
                .as_ref()
                .map_or(epoch, |cache| cache.key.epoch);
            let (all_entries, appended) = match self.trajectory_mode {
                TrajectoryMode::Execution | TrajectoryMode::Requests => {
                    let state = self.model.read(cx);
                    reconcile_trajectory_entries_by_epoch(
                        cached_entries,
                        state.active_trajectory(),
                        cached_epoch,
                        epoch,
                    )
                }
                TrajectoryMode::ModelContext => {
                    let source = self.model.read(cx).active_model_context_diagnostics();
                    reconcile_trajectory_entries_with_append(cached_entries, &source)
                }
                TrajectoryMode::DurableEvents => {
                    let source = self.model.read(cx).active_durable_event_diagnostics();
                    reconcile_trajectory_entries_with_append(cached_entries, &source)
                }
                TrajectoryMode::Recovery => {
                    let source = self.model.read(cx).active_recovery_diagnostics();
                    reconcile_trajectory_entries_with_append(cached_entries, &source)
                }
            };
            let (categories, lane_latest, filtered_indices) = if projection_matches && appended {
                extend_trajectory_facets(
                    Arc::make_mut(&mut cached_categories),
                    Arc::make_mut(&mut cached_lane_latest),
                    &mut cached_filtered_indices,
                    &all_entries,
                    cached_len,
                    &key,
                );
                (
                    cached_categories,
                    cached_lane_latest,
                    cached_filtered_indices,
                )
            } else {
                let mut categories = Vec::new();
                let mut lane_latest = std::collections::BTreeMap::new();
                let mut filtered_indices = Vec::new();
                extend_trajectory_facets(
                    &mut categories,
                    &mut lane_latest,
                    &mut filtered_indices,
                    &all_entries,
                    0,
                    &key,
                );
                (
                    Arc::new(categories),
                    Arc::new(lane_latest),
                    filtered_indices,
                )
            };
            let lanes = Arc::new(lane_latest.keys().cloned().collect());
            let previews = if projection_matches && appended {
                extend_trajectory_previews(&mut cached_previews, &all_entries, cached_len);
                cached_previews
            } else {
                let mut previews = Vec::with_capacity(all_entries.len());
                extend_trajectory_previews(&mut previews, &all_entries, 0);
                previews
            };
            let (rows, extends_previous) = if projection_matches && appended {
                let mut rows = self
                    .trajectory_cache
                    .as_mut()
                    .map(|cache| std::mem::take(&mut cache.rows))
                    .unwrap_or_default();
                extend_trajectory_rows(
                    &mut rows,
                    &all_entries,
                    &filtered_indices,
                    cached_filtered_len,
                    self.trajectory_mode,
                );
                (rows, true)
            } else {
                let rows =
                    build_trajectory_rows(&all_entries, &filtered_indices, self.trajectory_mode);
                let extends_previous = self
                    .trajectory_cache
                    .as_ref()
                    .is_some_and(|cache| rows.starts_with(&cache.rows));
                (rows, extends_previous)
            };
            let summary = if projection_matches && appended {
                extend_trajectory_summary(&mut cached_summary, &all_entries[cached_len..]);
                cached_summary
            } else {
                summarize_trajectory(&all_entries)
            };
            let previous_row_count = self
                .trajectory_cache
                .as_ref()
                .map_or(0, |cache| cache.rows.len());
            if extends_previous {
                self.trajectory_list_state.splice(
                    previous_row_count..previous_row_count,
                    rows.len() - previous_row_count,
                );
            } else {
                self.trajectory_list_state.reset(rows.len());
            }
            self.trajectory_raw_json = None;
            self.trajectory_cache = Some(TrajectoryRenderCache {
                key,
                all_entries,
                categories,
                lanes,
                lane_latest,
                filtered_indices,
                previews,
                rows,
                summary,
            });
        }
        let inspector_tab = self.trajectory_inspector_tab;
        let selected_index = self.selected_trajectory_index;
        if let Some(index) = (inspector_tab == TrajectoryInspectorTab::Raw)
            .then_some(selected_index)
            .flatten()
        {
            let needs_raw = self.trajectory_raw_json.as_ref().is_none_or(
                |(cached_revision, cached_index, _)| {
                    *cached_revision != revision || *cached_index != index
                },
            );
            if needs_raw {
                self.trajectory_raw_json = self
                    .trajectory_cache
                    .as_ref()
                    .and_then(|cache| cache.all_entries.get(index))
                    .map(|entry| (revision, index, format_trajectory_raw_json(entry)));
            }
        }
        let cache = self.trajectory_cache.as_ref().expect("trajectory cache");
        let all_entries = &cache.all_entries;
        let categories = Arc::clone(&cache.categories);
        let lanes = Arc::clone(&cache.lanes);
        let lane_latest = Arc::clone(&cache.lane_latest);
        let entries = &cache.filtered_indices;
        let theme = cx.theme().colors;
        if entries.is_empty() {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No canonical trajectory events have been observed in this session yet.")
                .into_any_element();
        }
        let selected_entry = selected_index
            .and_then(|index| all_entries.get(index))
            .cloned();
        let selected_raw_json = (inspector_tab == TrajectoryInspectorTab::Raw)
            .then(|| {
                self.trajectory_raw_json
                    .as_ref()
                    .map(|(_, _, raw)| raw.clone())
            })
            .flatten();
        let inspector = selected_entry.map(|entry| {
            let close_view = cx.entity().clone();
            let inspector_view = cx.entity().clone();
            let model_visible = entry.diagnostics.model_visible || matches!(
                entry.category.as_str(),
                "Input" | "Assistant" | "Context" | "Context Manifest" | "Tool"
            );
            let provenance = match entry.category.as_str() {
                "Input" => "User transcript · model-visible",
                "Assistant" => "Assistant transcript · model-visible",
                "Context" | "Context Manifest" => "Runtime context package · model-visible",
                "Tool" | "Tool runtime" => "Tool transcript · model-visible",
                "Anomaly" => "Automated diagnostic anomaly · durable",
                "Error" => "Runtime diagnostic · durable",
                _ => "Runtime lifecycle record · durable",
            };
            let mut metadata_items = vec![
                entry.seq.map(|value| ("Sequence", format!("#{value}"))),
                entry.request.map(|value| ("Request", format!("#{value}"))),
                entry.turn.map(|value| ("Turn", value.to_string())),
                entry.run_id.clone().map(|value| ("Run", value)),
                entry.lane.clone().map(|value| ("Lane", value)),
                entry.correlation_id.clone().map(|value| ("Call / Correlation", value)),
                entry.diagnostics.status.clone().map(|value| ("Status", value)),
                entry.diagnostics.duration_ms.map(|value| {
                    (
                        "Duration",
                        if value < 1000 {
                            format!("{value} ms")
                        } else {
                            format!("{:.2} s", value as f64 / 1000.0)
                        },
                    )
                }),
                entry.diagnostics.exit_code.map(|value| ("Exit Code", value.to_string())),
                entry.diagnostics.output_bytes.map(|value| ("Output Size", format!("{value} bytes"))),
                entry.diagnostics.token_estimate.map(|value| ("Est. Tokens", format!("~{value}"))),
                entry.diagnostics.items_count.map(|value| ("Item Count", value.to_string())),
            ];
            if !entry.diagnostics.files_mutated.is_empty() {
                metadata_items.push(Some(("Files Mutated", entry.diagnostics.files_mutated.join(", "))));
            }
            if !entry.diagnostics.commands_executed.is_empty() {
                metadata_items.push(Some(("Commands Executed", entry.diagnostics.commands_executed.join(", "))));
            }
            let metadata = metadata_items.into_iter().flatten();
            let (header_bg, header_fg, header_tag): (Hsla, Hsla, SharedString) = match entry.category.as_str() {
                "Tool" | "Tool runtime" => (theme.warning.opacity(0.18), theme.warning, "TOOL".into()),
                "Provider" => (theme.primary.opacity(0.18), theme.primary, "PROVIDER".into()),
                "Context Manifest" | "Manifest" => (theme.accent.opacity(0.14), theme.muted_foreground, "MANIFEST".into()),
                "Request" => (theme.primary.opacity(0.16), theme.accent, "REQUEST".into()),
                "Anomaly" => (theme.warning.opacity(0.20), theme.warning, "ANOMALY".into()),
                "Error" => (theme.danger.opacity(0.20), theme.danger, "ERROR".into()),
                _ => (theme.muted.opacity(0.5), theme.muted_foreground, entry.category.clone().into()),
            };
            div()
                .w(px(410.0))
                .min_w(px(320.0))
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .child(
                    div()
                        .h(px(48.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .bg(header_bg)
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(header_fg)
                                .child(header_tag),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(entry.summary.clone()),
                        )
                        .children(entry.diagnostics.duration_ms.map(|dur| {
                            let dur_str = if dur < 1000 { format!("{dur}ms") } else { format!("{:.1}s", dur as f64 / 1000.0) };
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(theme.muted.opacity(0.7))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(dur_str)
                        }))
                        .child(
                            Button::new("copy-trajectory-row")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Copy)
                                .tooltip("Copy trajectory entry")
                                .on_click({
                                    let text = format!(
                                        "seq:{:?} turn:{:?} category:{} summary:{} detail:{} lane:{:?} run:{:?} call:{:?}",
                                        entry.seq, entry.turn, entry.category, entry.summary,
                                        entry.detail, entry.lane, entry.run_id, entry.correlation_id,
                                    );
                                    move |_, window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                        window.push_notification(
                                            Notification::info("Copied trajectory entry"),
                                            cx,
                                        );
                                    }
                                }),
                        )
                        .child(
                            Button::new("close-trajectory-inspector")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip("Close inspector")
                                .on_click(move |_, _, cx| {
                                    close_view.update(cx, |this, cx| {
                                        this.selected_trajectory_index = None;
                                        cx.notify();
                                    })
                                }),
                        ),
                )
                .child(
                    div()
                        .h(px(38.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_1()
                        .border_b_1()
                        .border_color(theme.border)
                        .children([
                            ("Overview", TrajectoryInspectorTab::Overview),
                            ("Preview", TrajectoryInspectorTab::Preview),
                            ("Raw", TrajectoryInspectorTab::Raw),
                            ("Source", TrajectoryInspectorTab::Source),
                        ]
                        .into_iter()
                        .map(|(label, tab)| {
                            let view = inspector_view.clone();
                            Button::new(SharedString::from(format!("trajectory-inspector-{label}")))
                                .ghost()
                                .small()
                                .selected(inspector_tab == tab)
                                .label(label)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.trajectory_inspector_tab = tab;
                                        cx.notify();
                                    })
                                })
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(match inspector_tab {
                            TrajectoryInspectorTab::Overview => div()
                                .flex()
                                .flex_col()
                                .gap_4()
                                .child(
                                    div()
                                        .p_3()
                                        .rounded_lg()
                                        .bg(theme.muted.opacity(0.3))
                                        .border_1()
                                        .border_color(theme.border.opacity(0.5))
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .children(metadata.map(|(label, value)| {
                                            div()
                                                .flex()
                                                .gap_2()
                                                .text_sm()
                                                .child(
                                                    div()
                                                        .w(px(110.0))
                                                        .flex_none()
                                                        .text_xs()
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_color(theme.muted_foreground)
                                                        .child(label),
                                                )
                                                .child(div().min_w_0().flex_1().text_xs().child(value.clone()))
                                        })),
                                )
                                .children(entry.diagnostics.raw.as_ref().map(|raw_args| {
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("INPUT ARGUMENTS"))
                                        .child(TextView::markdown(
                                            format!("trajectory-args-{}", entry.seq.unwrap_or(0)),
                                            format!("```json\n{}\n```", raw_args),
                                        ).selectable(true))
                                }))
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("VISIBILITY"))
                                .child(div().text_sm().child(if model_visible { "Model-visible transcript/context" } else { "Runtime-only durable diagnostic" }))
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("SUMMARY"))
                                .child(div().text_sm().child(entry.summary.clone()))
                                .into_any_element(),
                            TrajectoryInspectorTab::Preview => div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("OUTPUT PREVIEW"))
                                .child(
                                    if entry.detail.is_empty() {
                                        div().text_sm().child("No preview content is available for this event.").into_any_element()
                                    } else {
                                        TextView::markdown(
                                            format!("trajectory-preview-{}", entry.seq.unwrap_or(0)),
                                            entry.detail.clone(),
                                        )
                                        .selectable(true)
                                        .into_any_element()
                                    },
                                )
                                .into_any_element(),
                            TrajectoryInspectorTab::Raw => {
                                let raw_json = selected_raw_json.clone().unwrap_or_default();
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("CANONICAL PROJECTION (JSON)"))
                                    .child(TextView::markdown(
                                        format!("trajectory-raw-{}", entry.seq.unwrap_or(0)),
                                        format!("```json\n{raw_json}\n```"),
                                    ).selectable(true))
                                    .into_any_element()
                            }
                            TrajectoryInspectorTab::Source => div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("PROVENANCE"))
                                .child(div().text_sm().child(entry.diagnostics.source.clone().unwrap_or_else(|| provenance.to_string())))
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("LINEAGE"))
                                .child(div().text_sm().child(format!(
                                    "Request {} · Turn {} · Lane {}",
                                    entry.request.map_or("—".to_string(), |request| format!("#{request}")),
                                    entry.turn.map_or("—".to_string(), |turn| turn.to_string()),
                                    entry.lane.as_deref().unwrap_or("—"),
                                )))
                                .children(entry.diagnostics.parent_id.as_ref().map(|p| {
                                    div().flex().flex_col().gap_1()
                                        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("PARENT ENTRY"))
                                        .child(div().text_sm().font_family("monospace").child(p.clone()))
                                }))
                                .children(entry.diagnostics.result_id.as_ref().map(|r| {
                                    div().flex().flex_col().gap_1()
                                        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("RESULT ENTRY"))
                                        .child(div().text_sm().font_family("monospace").child(r.clone()))
                                }))
                                .children(entry.correlation_id.clone().map(|id| div().text_sm().child(format!("Correlation: {id}"))))
                                .into_any_element(),
                        }),
                )
        });
        let overview_lane = |label: &'static str, markers: &HashSet<usize>, color: Hsla| {
            div()
                .h(px(18.0))
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(48.0))
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .h(px(12.0))
                        .flex()
                        .items_end()
                        .gap(px(2.0))
                        .children((0..48).map(|index| {
                            div()
                                .flex_1()
                                .h(if markers.contains(&index) {
                                    px(10.0)
                                } else {
                                    px(2.0)
                                })
                                .rounded_sm()
                                .bg(if markers.contains(&index) {
                                    color
                                } else {
                                    theme.border.opacity(0.35)
                                })
                        })),
                )
        };
        let overview = div()
            .h(px(58.0))
            .flex_none()
            .flex()
            .flex_col()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(overview_lane(
                "Input",
                &cache.summary.overview_positions[0],
                theme.success,
            ))
            .child(overview_lane(
                "Model",
                &cache.summary.overview_positions[1],
                theme.primary,
            ))
            .child(overview_lane(
                "Tools",
                &cache.summary.overview_positions[2],
                theme.warning,
            ));
        let category_label = self
            .trajectory_category
            .clone()
            .unwrap_or_else(|| "All events".into());
        let lane_label = self
            .trajectory_lane
            .clone()
            .unwrap_or_else(|| format!("{} lanes", lanes.len()));
        let category_view = cx.entity().clone();
        let lane_view = cx.entity().clone();
        let mode_view = cx.entity().clone();
        let mode_label = match self.trajectory_mode {
            TrajectoryMode::Execution => "Execution",
            TrajectoryMode::Requests => "Requests",
            TrajectoryMode::ModelContext => "Model Context",
            TrajectoryMode::DurableEvents => "Durable Events",
            TrajectoryMode::Recovery => "Recovery",
        };
        let toolbar = div()
            .h(px(38.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_3()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                Button::new("trajectory-mode-filter")
                    .ghost()
                    .small()
                    .label(mode_label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu;
                        for (label, mode) in [
                            ("Execution", TrajectoryMode::Execution),
                            ("Requests", TrajectoryMode::Requests),
                            ("Model Context", TrajectoryMode::ModelContext),
                            ("Durable Events", TrajectoryMode::DurableEvents),
                            ("Recovery", TrajectoryMode::Recovery),
                        ] {
                            let view = mode_view.clone();
                            menu =
                                menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.trajectory_mode = mode;
                                        this.trajectory_category = None;
                                        this.trajectory_lane = None;
                                        this.selected_trajectory_index = None;
                                        cx.notify();
                                    });
                                }));
                        }
                        menu
                    }),
            )
            .child(
                Button::new("trajectory-category-filter")
                    .ghost()
                    .small()
                    .label(category_label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let all_view = category_view.clone();
                        let mut menu = menu.item(PopupMenuItem::new("All events").on_click(
                            move |_, _, cx| {
                                all_view.update(cx, |this, cx| {
                                    this.trajectory_category = None;
                                    cx.notify();
                                });
                            },
                        ));
                        for category in categories.iter().cloned() {
                            let selected = category.clone();
                            let view = category_view.clone();
                            menu = menu.item(PopupMenuItem::new(category).on_click(
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.trajectory_category = Some(selected.clone());
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
            .children((lanes.len() > 1).then(|| {
                Button::new("trajectory-lane-filter")
                    .ghost()
                    .small()
                    .label(lane_label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let all_view = lane_view.clone();
                        let mut menu =
                            menu.item(PopupMenuItem::new("All lanes").on_click(move |_, _, cx| {
                                all_view.update(cx, |this, cx| {
                                    this.trajectory_lane = None;
                                    cx.notify();
                                });
                            }));
                        for lane in lanes.iter().cloned() {
                            let selected = lane.clone();
                            let view = lane_view.clone();
                            let latest = lane_latest.get(&lane).cloned().unwrap_or_default();
                            menu = menu.item(
                                PopupMenuItem::new(format!("{lane} — {latest}")).on_click(
                                    move |_, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.trajectory_lane = Some(selected.clone());
                                            cx.notify();
                                        });
                                    },
                                ),
                            );
                        }
                        menu
                    })
            }))
            .child(div().flex_1())
            .child(
                div()
                    .w(px(280.0))
                    .h(px(32.0))
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(Input::new(&self.trajectory_search_input).appearance(false)),
            );
        let tool_count = cache.summary.tool_count;
        let total_dur_ms = cache.summary.total_duration_ms;
        let dur_label = if total_dur_ms < 1000 {
            format!("{total_dur_ms}ms total")
        } else {
            format!("{:.2}s total", total_dur_ms as f64 / 1000.0)
        };
        let anomaly_count = cache.summary.anomaly_count;
        let max_turn = cache.summary.max_turn;

        let stats_bar = div()
            .h(px(26.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_4()
            .px_3()
            .border_b_1()
            .border_color(theme.border.opacity(0.4))
            .bg(theme.muted.opacity(0.15))
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(format!("{max_turn}")),
                    )
                    .child("turns"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(format!("{tool_count}")),
                    )
                    .child("tool calls"),
            )
            .child(
                div().flex().items_center().gap_1().child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(dur_label),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().size(px(6.0)).rounded_full().bg(if anomaly_count > 0 {
                        theme.warning
                    } else {
                        theme.success
                    }))
                    .child(format!("{anomaly_count} anomalies")),
            );

        div()
            .id("session-trajectory")
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(overview)
            .child(toolbar)
            .child(stats_bar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .id("trajectory-events-container")
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(
                                list(
                                    self.trajectory_list_state.clone(),
                                    cx.processor(Self::render_trajectory_row),
                                )
                                .size_full()
                                .with_sizing_behavior(ListSizingBehavior::Auto),
                            )
                            .child(div().absolute().inset_0().child(
                                gpui_component::scroll::Scrollbar::vertical(
                                    &self.trajectory_list_state,
                                ),
                            )),
                    )
                    .children(inspector),
            )
            .into_any_element()
    }

    fn sync_transcript_rows(
        &mut self,
        messages: Arc<Vec<ChatMessageInfo>>,
        generating: bool,
        session_changed: bool,
    ) {
        if !session_changed
            && Arc::ptr_eq(&messages, &self.transcript_messages)
            && generating == self.transcript_generating
        {
            return;
        }

        let old_message_count = self.transcript_messages.len();
        let old_row_count = self.transcript_rows.len();
        let new_message_count = messages.len();

        if !session_changed
            && new_message_count == old_message_count
            && generating == self.transcript_generating
        {
            let last_changed = messages
                .last()
                .zip(self.transcript_messages.last())
                .is_some_and(|(new, old)| {
                    new.id != old.id
                        || new.content.len() != old.content.len()
                        || new.reasoning_content.as_ref().map(String::len)
                            != old.reasoning_content.as_ref().map(String::len)
                        || new.tool_activities.len() != old.tool_activities.len()
                        || new.streaming != old.streaming
                });
            self.transcript_messages = messages;
            if last_changed {
                self.transcript_list_state
                    .remeasure_items(old_row_count.saturating_sub(1)..old_row_count);
            } else {
                self.transcript_list_state.remeasure();
            }
            return;
        }

        let new_rows = build_transcript_rows(&messages, generating);
        let new_row_count = new_rows.len();
        let working_changed = !session_changed
            && new_message_count == old_message_count
            && generating != self.transcript_generating;
        let prepended = !session_changed
            && new_message_count > old_message_count
            && self
                .transcript_messages
                .first()
                .zip(messages.get(new_message_count - old_message_count))
                .is_some_and(|(old, new)| old.id == new.id)
            && self
                .transcript_messages
                .last()
                .zip(messages.last())
                .is_some_and(|(old, new)| old.id == new.id)
            && new_row_count >= old_row_count;
        let appended = !session_changed
            && new_message_count > old_message_count
            && self
                .transcript_messages
                .first()
                .zip(messages.first())
                .is_some_and(|(old, new)| old.id == new.id)
            && self
                .transcript_messages
                .last()
                .zip(messages.get(old_message_count.saturating_sub(1)))
                .is_some_and(|(old, new)| old.id == new.id)
            && new_row_count >= old_row_count;

        self.transcript_messages = messages;
        self.transcript_rows = new_rows;
        self.transcript_generating = generating;
        if working_changed && generating {
            self.transcript_list_state
                .splice(old_row_count..old_row_count, 1);
        } else if working_changed {
            self.transcript_list_state
                .splice(new_row_count..old_row_count, 0);
        } else if prepended {
            self.transcript_list_state
                .splice(0..0, new_row_count - old_row_count);
        } else if appended {
            self.transcript_list_state
                .splice(old_row_count..old_row_count, new_row_count - old_row_count);
        } else {
            self.transcript_list_state.reset(new_row_count);
        }
        if session_changed {
            self.transcript_list_state.set_follow_mode(FollowMode::Tail);
        }
    }

    fn render_transcript_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let messages = Arc::clone(&self.transcript_messages);
        let content = match self.transcript_rows.get(index).cloned() {
            Some(TranscriptRow::Message(message_index)) => messages
                .get(message_index)
                .map(|message| self.render_message(message, cx)),
            Some(TranscriptRow::Activities(range)) => messages
                .get(range)
                .map(|messages| self.render_activity_group(messages, cx)),
            Some(TranscriptRow::Working) => Some(self.render_working_indicator(cx)),
            None => None,
        };

        div()
            .w_full()
            .max_w(px(CHAT_CONTENT_MAX_WIDTH))
            .mx_auto()
            .children(content)
            .into_any_element()
    }

    fn markdown_state(
        &mut self,
        key: String,
        source: &str,
        cx: &mut Context<Self>,
    ) -> Entity<TextViewState> {
        let key = (self.markdown_cache_namespace.clone(), key);
        let entry = self
            .markdown_states
            .entry(key)
            .or_insert_with(|| MarkdownRenderState {
                source: source.to_owned(),
                state: cx.new(|cx| TextViewState::markdown(source, cx)),
            });

        match classify_markdown_update(&entry.source, source) {
            MarkdownUpdate::Unchanged => {}
            MarkdownUpdate::Append(suffix) => {
                entry.source.push_str(suffix);
                entry
                    .state
                    .update(cx, |state, cx| state.push_str(suffix, cx));
            }
            MarkdownUpdate::Replace => {
                entry.source.clear();
                entry.source.push_str(source);
                entry
                    .state
                    .update(cx, |state, cx| state.set_text(source, cx));
            }
        }

        entry.state.clone()
    }

    fn cached_segments(&mut self, message_id: &str, content: &str) -> Vec<MarkdownSegment> {
        if let Some((cached_content, segments)) = self.segment_cache.get(message_id) {
            if cached_content == content {
                return segments.clone();
            }
        }

        let segments = extract_markdown_segments(content);
        self.segment_cache.insert(
            message_id.to_owned(),
            (content.to_owned(), segments.clone()),
        );
        segments
    }

    fn chat_markdown_view(&self, state: &Entity<TextViewState>) -> TextView {
        let model = self.model.clone();
        TextView::new(state)
            .selectable(true)
            .on_link_click(move |url, event, _window, cx| {
                let activate = match event {
                    ClickEvent::Mouse(click) => {
                        matches!(click.up.button, MouseButton::Left | MouseButton::Middle)
                    }
                    ClickEvent::Keyboard(_) => true,
                    ClickEvent::Touch(click) => !click.long_press,
                };
                if !activate {
                    return;
                }

                match classify_chat_link(url) {
                    ChatLinkTarget::Web => cx.open_url(url),
                    ChatLinkTarget::ProjectFile(path) => {
                        model.update(cx, |state, cx| {
                            state.request_open_file(path);
                            cx.notify();
                        });
                    }
                    ChatLinkTarget::Rejected => {}
                }
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
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
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

        let display_lang = if language.trim().is_empty() {
            "code"
        } else {
            language.trim()
        };

        div()
            .w_full()
            .my_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_1p5()
                    .bg(theme.secondary)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                Tag::new()
                                    .child(display_lang.to_string())
                                    .with_variant(TagVariant::Secondary)
                                    .small(),
                            )
                            .children(path_opt.map(|path| {
                                div()
                                    .id(SharedString::from(format!(
                                        "code-path-{msg_id}-{block_index}"
                                    )))
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .tooltip({
                                        let tip = path.clone();
                                        move |window, cx| {
                                            gpui_component::tooltip::Tooltip::new(tip.clone())
                                                .build(window, cx)
                                        }
                                    })
                                    .child(path)
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .children(is_runnable.then(|| {
                                let cmd = code_str.clone();
                                let model = model.clone();
                                Button::new(SharedString::from(format!(
                                    "run-term-{msg_id}-{block_index}"
                                )))
                                .icon(IconName::SquareTerminal)
                                .label("Run in Terminal")
                                .xsmall()
                                .secondary()
                                .tooltip("Run in active project terminal")
                                .on_click(move |_event, _window, cx| {
                                    let cmd = normalize_terminal_command(&cmd);
                                    if cmd.lines().filter(|line| !line.trim().is_empty()).count() > 1 {
                                        let model = model.clone();
                                        cx.spawn(async move |cx| {
                                            let result = rfd::AsyncMessageDialog::new()
                                                .set_title("Run multiple terminal commands?")
                                                .set_description("This code block contains multiple commands. Run them in the active terminal?")
                                                .set_buttons(rfd::MessageButtons::YesNo)
                                                .show()
                                                .await;
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
                                Button::new(SharedString::from(format!(
                                    "open-edit-{msg_id}-{block_index}"
                                )))
                                .icon(IconName::File)
                                .label("Open in Editor")
                                .xsmall()
                                .ghost()
                                .tooltip("Open file in central editor")
                                .on_click(move |_event, _window, cx| {
                                    let path = path.clone();
                                    model.update(cx, |state, cx| {
                                        controller::dispatch(
                                            state,
                                            AppAction::OpenFileInEditor(path),
                                        );
                                        cx.notify();
                                    });
                                })
                            }))
                            .when(!streaming, |actions| {
                                let block_key = format!("copy-code-{msg_id}-{block_index}");
                                let is_copied = self.copied_code_block.as_ref().is_some_and(|(id, time)| {
                                    id == &block_key && time.elapsed() < std::time::Duration::from_secs(2)
                                });
                                let copy_code_key = block_key.clone();
                                actions.child(
                                    Button::new(SharedString::from(block_key))
                                        .icon(if is_copied {
                                            IconName::Check
                                        } else {
                                            IconName::Copy
                                        })
                                        .accessibility_label(if is_copied {
                                            "Code copied"
                                        } else {
                                            "Copy code"
                                        })
                                        .xsmall()
                                        .ghost()
                                        .tooltip(if is_copied {
                                            "Copied!"
                                        } else {
                                            "Copy code to clipboard"
                                        })
                                        .when(is_copied, |btn| btn.text_color(theme.success))
                                        .on_click(cx.listener(move |this, _event, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copy_code.clone(),
                                            ));
                                            this.copied_code_block = Some((copy_code_key.clone(), std::time::Instant::now()));
                                            window.push_notification(
                                                Notification::info("Code copied to clipboard"),
                                                cx,
                                            );
                                            cx.notify();
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .overflow_x_scrollbar()
                    .p_3()
                    .font_family("monospace")
                    .text_xs()
                    .text_color(theme.foreground)
                    .child({
                        let formatted_code =
                            format!("```{language}\n{}\n```", code.trim_end());
                        let code_state = self.markdown_state(
                            format!("code-{msg_id}-{block_index}"),
                            &formatted_code,
                            cx,
                        );
                        self.chat_markdown_view(&code_state)
                    }),
            )
    }

    fn render_reasoning_block(
        &mut self,
        msg: &ChatMessageInfo,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let reasoning = msg.reasoning_content.as_deref()?;
        if reasoning.trim().is_empty() {
            return None;
        }
        let theme = cx.theme().colors;
        let is_streaming = msg.streaming;
        let is_expanded = msg.reasoning_expanded;
        let model = self.model.clone();
        let msg_id = msg.id.clone();

        let approx_tokens = (reasoning.len() + 3) / 4;
        let token_badge = Tag::new()
            .child(if is_streaming {
                "thinking…".to_string()
            } else {
                format!("~{approx_tokens} tokens")
            })
            .small()
            .with_variant(TagVariant::Secondary);

        let header = div()
            .id(SharedString::from(format!("reasoning-toggle-{}", msg.id)))
            .h(px(32.0))
            .px_2()
            .rounded_lg()
            .flex()
            .items_center()
            .justify_between()
            .cursor_pointer()
            .hover(|s| s.bg(theme.muted.opacity(0.5)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .text_color(if is_streaming {
                                theme.primary
                            } else {
                                theme.muted_foreground
                            })
                            .child("✦"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child(if is_streaming {
                                "Thinking…"
                            } else {
                                "Thought process"
                            }),
                    )
                    .child(token_badge),
            )
            .child(
                Icon::new(if is_expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(theme.muted_foreground),
            )
            .on_click(move |_event, _window, cx| {
                model.update(cx, |state, cx| {
                    controller::dispatch(state, AppAction::ToggleReasoningExpanded(msg_id.clone()));
                    cx.notify();
                });
            });

        let detail = is_expanded.then(|| {
            let container = div()
                .mx_2()
                .mb_2()
                .p_2p5()
                .max_h(px(340.0))
                .rounded_md()
                .border_t_1()
                .border_color(theme.border.opacity(0.4))
                .bg(theme.secondary.opacity(0.3))
                .text_xs()
                .text_color(theme.muted_foreground)
                .overflow_y_scrollbar();
            if is_streaming {
                container.child(reasoning.to_owned()).into_any_element()
            } else {
                let markdown_state =
                    self.markdown_state(format!("reasoning-{}", msg.id), reasoning, cx);
                container
                    .child(self.chat_markdown_view(&markdown_state))
                    .into_any_element()
            }
        });

        Some(
            div()
                .w_full()
                .min_w_0()
                .rounded_lg()
                .border_1()
                .border_color(theme.border.opacity(0.6))
                .bg(theme.title_bar)
                .child(header)
                .children(detail)
                .into_any_element(),
        )
    }

    fn render_tool_activities_block(
        &mut self,
        msg_id: &str,
        tools: &[ToolActivityInfo],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if tools.is_empty() {
            return None;
        }
        if tools.len() == 1 {
            return Some(self.render_tool_activity(&tools[0], cx).into_any_element());
        }

        let mut reads = 0;
        let mut edits = 0;
        let mut runs = 0;
        let mut others = 0;
        let mut has_error = false;
        let mut has_running = false;
        for t in tools {
            if t.category == "Error" {
                has_error = true;
            } else if t.category == "Working" || t.category == "Thinking" {
                has_running = true;
            }
            match t.title.as_str() {
                "read_file" | "view_file" | "grep_search" | "find_by_name" | "list_dir" => {
                    reads += 1
                }
                "write_to_file" | "replace_file_content" | "apply_diff" | "edit_file" => edits += 1,
                "run_command" | "execute" => runs += 1,
                _ => others += 1,
            }
        }

        let mut parts = Vec::new();
        if reads > 0 {
            parts.push(format!(
                "inspected {reads} file{}",
                if reads == 1 { "" } else { "s" }
            ));
        }
        if edits > 0 {
            parts.push(format!(
                "edited {edits} file{}",
                if edits == 1 { "" } else { "s" }
            ));
        }
        if runs > 0 {
            parts.push(format!(
                "ran {runs} command{}",
                if runs == 1 { "" } else { "s" }
            ));
        }
        if others > 0 {
            parts.push(format!(
                "{others} other tool{}",
                if others == 1 { "" } else { "s" }
            ));
        }
        let summary = if parts.is_empty() {
            format!("Ran {} tools", tools.len())
        } else {
            let mut s = parts.join(", ");
            if let Some(c) = s.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            s
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
        let status_icon = if has_running {
            Spinner::new()
                .xsmall()
                .color(theme.primary)
                .into_any_element()
        } else if has_error {
            Icon::new(IconName::CircleX)
                .xsmall()
                .text_color(theme.danger)
                .into_any_element()
        } else {
            Icon::new(IconName::CircleCheck)
                .xsmall()
                .text_color(theme.success)
                .into_any_element()
        };

        let toggle_key = group_key.clone();
        let toggle_has_running = has_running;
        let header = div()
            .id(SharedString::from(format!("tool-toggle-{msg_id}")))
            .h(px(28.0))
            .px_2()
            .rounded_md()
            .bg(theme.muted.opacity(0.25))
            .hover(|s| s.bg(theme.muted.opacity(0.5)))
            .flex()
            .items_center()
            .justify_between()
            .cursor_pointer()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(status_icon)
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(summary),
                    )
                    .child(
                        Tag::new()
                            .child(format!("{} tools", tools.len()))
                            .small()
                            .with_variant(TagVariant::Secondary),
                    ),
            )
            .child(
                Icon::new(if is_expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(theme.muted_foreground),
            )
            .on_click(cx.listener(move |this, _event, _window, cx| {
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

        let tool_rows = tools
            .iter()
            .map(|t| self.render_tool_activity(t, cx))
            .collect::<Vec<_>>();
        let detail_rows = is_expanded.then(|| {
            div()
                .flex()
                .flex_col()
                .pl_2()
                .mt_1()
                .border_l_2()
                .border_color(theme.border.opacity(0.4))
                .children(tool_rows)
        });

        Some(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .child(header)
                .children(detail_rows)
                .into_any_element(),
        )
    }

    fn render_message(&mut self, msg: &ChatMessageInfo, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        match msg.role {
            MessageRole::User => {
                let is_queued =
                    msg.id.starts_with("queued-user-") && self.model.read(cx).is_generating;
                let is_steered =
                    msg.id.starts_with("steered-user-") && self.model.read(cx).is_generating;
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_end()
                    .my_2()
                    .px_4()
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
                        div()
                            .min_w_0()
                            .max_w(px(USER_BUBBLE_MAX_WIDTH))
                            .p_3()
                            .rounded_lg()
                            .bg(theme.secondary)
                            .text_sm()
                            .text_color(theme.secondary_foreground)
                            .child({
                                let markdown_state =
                                    self.markdown_state(msg.id.clone(), &msg.content, cx);
                                self.chat_markdown_view(&markdown_state)
                            })
                            .context_menu({
                                let content = msg.content.clone();
                                move |menu, _window, _cx| {
                                    let text = content.clone();
                                    menu.item(PopupMenuItem::new("Copy Message").on_click(
                                        move |_event, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                text.clone(),
                                            ));
                                            window.push_notification(
                                                Notification::info("Copied to clipboard"),
                                                cx,
                                            );
                                        },
                                    ))
                                }
                            }),
                    )
            }
            MessageRole::Assistant => {
                let reasoning_element = self.render_reasoning_block(msg, cx);
                let filtered_tools: Vec<ToolActivityInfo> = msg
                    .tool_activities
                    .iter()
                    .filter(|tool| tool.title != "update_plan")
                    .cloned()
                    .collect();
                let tools_element = self.render_tool_activities_block(&msg.id, &filtered_tools, cx);

                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .my_2()
                    .px_4()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(reasoning_element)
                            .children(if !msg.content.is_empty() {
                                let segments = self.cached_segments(&msg.id, &msg.content);
                                let rendered_segments: Vec<AnyElement> = segments
                                    .into_iter()
                                    .enumerate()
                                    .map(|(idx, seg)| match seg {
                                        MarkdownSegment::Markdown(text) => {
                                            if msg.streaming {
                                                div()
                                                    .w_full()
                                                    .text_sm()
                                                    .text_color(theme.foreground)
                                                    .child(text)
                                                    .into_any_element()
                                            } else {
                                                let markdown_state = self.markdown_state(
                                                    format!("{}-seg-{}", msg.id, idx),
                                                    &text,
                                                    cx,
                                                );
                                                self.chat_markdown_view(&markdown_state)
                                                    .into_any_element()
                                            }
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
                                                cx,
                                            )
                                            .into_any_element(),
                                    })
                                    .collect();

                                Some(
                                    div()
                                        .w_full()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .children(rendered_segments),
                                )
                            } else {
                                None
                            })
                            .children(tools_element)
                            .context_menu({
                                let content = msg.content.clone();
                                move |menu, _window, _cx| {
                                    let text = content.clone();
                                    menu.item(PopupMenuItem::new("Copy Message").on_click(
                                        move |_event, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                text.clone(),
                                            ));
                                            window.push_notification(
                                                Notification::info("Copied to clipboard"),
                                                cx,
                                            );
                                        },
                                    ))
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
                        move |menu, _window, _cx| {
                            let text = content.clone();
                            menu.item(PopupMenuItem::new("Copy Message").on_click(
                                move |_event, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                    window.push_notification(
                                        Notification::info("Copied to clipboard"),
                                        cx,
                                    );
                                },
                            ))
                        }
                    }),
            ),
            MessageRole::Error => render_chat_error(&msg.id, &msg.content, &self.model, cx),
        }
        .into_any_element()
    }

    fn render_new_task(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let (projects, active_work_dir) = {
            let state = self.model.read(cx);
            (
                state
                    .projects
                    .iter()
                    .map(|project| (project.name.clone(), project.work_dir.clone()))
                    .collect::<Vec<_>>(),
                state.active_work_dir.clone(),
            )
        };
        let selected_project = projects
            .iter()
            .find(|(_, work_dir)| active_work_dir.as_ref() == Some(work_dir))
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "Choose a project".to_string());
        let model = self.model.clone();

        let project_picker = Button::new("new-task-project-picker")
            .icon(IconName::Folder)
            .label(selected_project)
            .dropdown_caret(true)
            .ghost()
            .small()
            .dropdown_menu(move |menu, _window, _cx| {
                let mut menu = menu;
                for (name, work_dir) in projects.clone() {
                    let model = model.clone();
                    menu = menu.item(PopupMenuItem::new(name).on_click(
                        move |_event, _window, cx| {
                            model.update(cx, |state, cx| {
                                controller::dispatch(
                                    state,
                                    AppAction::SelectDraftProject(work_dir.clone()),
                                );
                                cx.notify();
                            });
                        },
                    ));
                }

                let model = model.clone();
                menu.separator()
                    .item(PopupMenuItem::new("New project...").on_click(
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
                    ))
            });

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .pb(px(64.0))
            .child(
                div()
                    .text_2xl()
                    .text_color(theme.primary)
                    .child(IconName::Asterisk),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_lg()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("What should we build in")
                    .child(project_picker)
                    .child("?"),
            )
            .into_any_element()
    }

    fn resolve_pending_permission(
        &mut self,
        request_id: &str,
        decision: threadlane_session::PermissionDecision,
        cx: &mut Context<Self>,
    ) {
        self.permission_details_open = false;
        self.model.update(cx, |state, cx| {
            state.resolve_active_permission(request_id, decision);
            cx.notify();
        });
        cx.notify();
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
                threadlane_session::QuestionItemAnswer {
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
        let answer = threadlane_session::QuestionAnswer {
            request_id: request.id.clone(),
            answers,
            dismissed: false,
        };
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
        let key = event.keystroke.key.as_str();

        if self.permission_details_open {
            if key == "escape" {
                self.permission_details_open = false;
                cx.stop_propagation();
                cx.notify();
                return;
            }

            let request = {
                let state = self.model.read(cx);
                state
                    .active_session_id
                    .as_ref()
                    .and_then(|session_id| state.pending_permissions.get(session_id))
                    .cloned()
            };
            if let Some(request) = request {
                let decision = match key {
                    "y" | "Y" | "enter" => Some(threadlane_session::PermissionDecision::AllowOnce),
                    "a" | "A" => Some(threadlane_session::PermissionDecision::AllowAlways),
                    "n" | "N" => Some(threadlane_session::PermissionDecision::Deny),
                    _ => None,
                };
                if let Some(decision) = decision {
                    self.resolve_pending_permission(&request.id, decision, cx);
                    cx.stop_propagation();
                    return;
                }
            } else {
                self.permission_details_open = false;
                cx.notify();
            }
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

    fn render_permission_details_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.model.read(cx);
        let session_id = state.active_session_id.as_ref()?;
        let request = state.pending_permissions.get(session_id)?.clone();
        let theme = cx.theme().colors;

        let action_button = |id: &'static str,
                             label: &'static str,
                             decision: threadlane_session::PermissionDecision,
                             primary: bool,
                             danger: bool| {
            let request_id = request.id.clone();
            Button::new(id)
                .label(label)
                .small()
                .when(primary, |button| button.primary())
                .when(danger, |button| button.danger())
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.resolve_pending_permission(&request_id, decision, cx);
                }))
        };

        Some(
            div()
                .id("permission-details-backdrop")
                .absolute()
                .inset_0()
                .bg(crate::theme::overlay_scrim())
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _event, _window, cx| {
                        this.permission_details_open = false;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .id("permission-details-modal")
                        .w(px(640.0))
                        .max_w(px(CHAT_CONTENT_MAX_WIDTH))
                        .p_5()
                        .rounded_xl()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.title_bar)
                        .shadow_xl()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                            cx.stop_propagation();
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_base()
                                                .font_weight(FontWeight::BOLD)
                                                .text_color(theme.foreground)
                                                .child(format!("Permission Request: {}", request.title)),
                                        )
                                        .child(
                                            Tag::new()
                                                .child(request.capability.clone())
                                                .with_variant(TagVariant::Secondary)
                                                .small(),
                                        ),
                                )
                                .child(
                                    Button::new("close-permission-details-dialog-btn")
                                        .icon(IconName::Close)
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.permission_details_open = false;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .w_full()
                                .max_h(px(320.0))
                                .p_3()
                                .rounded_lg()
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.background)
                                .text_xs()
                                .text_color(theme.foreground)
                                .overflow_y_scrollbar()
                                .child(request.detail.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("Shortcuts: [Y] Allow once · [A] Always · [N] Deny · [Esc] Close"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(action_button(
                                            "details-deny",
                                            "Deny [N]",
                                            threadlane_session::PermissionDecision::Deny,
                                            false,
                                            true,
                                        ))
                                        .child(action_button(
                                            "details-allow-once",
                                            "Allow once [Y]",
                                            threadlane_session::PermissionDecision::AllowOnce,
                                            true,
                                            false,
                                        ))
                                        .child(action_button(
                                            "details-allow-always",
                                            "Always [A]",
                                            threadlane_session::PermissionDecision::AllowAlways,
                                            false,
                                            false,
                                        )),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_permission_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.model.read(cx);
        let session_id = state.active_session_id.as_ref()?;
        let request = state.pending_permissions.get(session_id)?.clone();
        let theme = cx.theme().colors;

        let action_button = |id: &'static str,
                             label: &'static str,
                             decision: threadlane_session::PermissionDecision,
                             primary: bool,
                             danger: bool| {
            let request_id = request.id.clone();
            Button::new(id)
                .label(label)
                .xsmall()
                .when(primary, |button| button.primary())
                .when(danger, |button| button.danger())
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.resolve_pending_permission(&request_id, decision, cx);
                }))
        };

        Some(
            div()
                .w_full()
                .flex_none()
                .px_4()
                .pt_1()
                .bg(theme.background)
                .child(
                    div()
                        .w_full()
                        .max_w(px(1000.0))
                        .mx_auto()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.title_bar)
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .child(
                                    div()
                                        .flex_none()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.foreground)
                                        .child(request.title),
                                )
                                .child(
                                    div()
                                        .id("permission-prompt-detail-text")
                                        .min_w_0()
                                        .text_color(theme.muted_foreground)
                                        .truncate()
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.permission_details_open = true;
                                            cx.notify();
                                        }))
                                        .child(request.detail),
                                ),
                        )
                        .child(
                            Button::new("permission-details-btn")
                                .icon(IconName::Maximize)
                                .label("Details")
                                .ghost()
                                .xsmall()
                                .tooltip("View full command & arguments")
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.permission_details_open = true;
                                    cx.notify();
                                })),
                        )
                        .child(action_button(
                            "permission-deny",
                            "Deny [N]",
                            threadlane_session::PermissionDecision::Deny,
                            false,
                            true,
                        ))
                        .child(action_button(
                            "permission-allow-once",
                            "Allow once [Y]",
                            threadlane_session::PermissionDecision::AllowOnce,
                            true,
                            false,
                        ))
                        .child(action_button(
                            "permission-allow-always",
                            "Always [A]",
                            threadlane_session::PermissionDecision::AllowAlways,
                            false,
                            false,
                        )),
                )
                .into_any_element(),
        )
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
                let header = item.header.clone();
                let body = item.question.clone();
                let options = item
                    .options
                    .iter()
                    .map(|option| {
                        let option_label = option.clone();
                        let is_selected = selected.iter().any(|item| item == option);
                        let request_id = request.id.clone();
                        let question_id = item.id.clone();
                        let option_value = option.clone();
                        Button::new(SharedString::from(format!(
                            "question-{request_id}-{}-{}",
                            item.id, option_label
                        )))
                        .label(option_label.clone())
                        .small()
                        .when(is_selected, |button| button.primary())
                        .when(!is_selected, |button| button.ghost())
                        .tooltip("Toggle this answer")
                        .on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                this.toggle_question_option(
                                    &request_id,
                                    &question_id,
                                    &option_value,
                                    cx,
                                );
                            },
                        ))
                    })
                    .collect::<Vec<_>>();
                let custom_input = item
                    .allow_custom
                    .then(|| {
                        self.question_inputs
                            .get(&key)
                            .map(|input| div().w_full().child(Input::new(input).small()))
                    })
                    .flatten();
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(header),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(body),
                    )
                    .child(div().flex().flex_wrap().gap_1().children(options))
                    .children(custom_input)
            })
            .collect::<Vec<_>>();

        Some(
            div()
                .w_full()
                .flex_none()
                .px_4()
                .pt_1()
                .bg(theme.background)
                .child(
                    div()
                        .w_full()
                        .max_w(px(1000.0))
                        .mx_auto()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.title_bar)
                        .flex()
                        .flex_col()
                        .gap_2()
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
                                        .label("Dismiss")
                                        .ghost()
                                        .xsmall()
                                        .tooltip("Dismiss without answering")
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.dismiss_active_question(cx);
                                        })),
                                )
                                .child(
                                    Button::new("question-submit")
                                        .label("Send answers")
                                        .small()
                                        .primary()
                                        .tooltip("Send the selected answers")
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
        let open = self.subagents_popover_open;
        let toggle_entity = cx.entity();
        let sync_entity = cx.entity();
        let content_entity = cx.entity();
        Some(
            Popover::new("subagents-popover")
                .anchor(Anchor::BottomRight)
                .appearance(false)
                .open(open)
                .on_open_change(move |open, _window, cx| {
                    sync_entity.update(cx, |this, cx| {
                        this.subagents_popover_open = *open;
                        cx.notify();
                    });
                })
                .trigger(SubagentPopoverTrigger {
                    selected: open,
                    toggle: Toggle::new("subagents-popover-trigger")
                        .ghost()
                        .rounded_full()
                        .tooltip("View subagent activity")
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(Icon::new(IconName::Bot).small())
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(count.to_string()),
                                ),
                        )
                        .on_click(move |open, _window, cx| {
                            toggle_entity.update(cx, |this, cx| {
                                this.subagents_popover_open = *open;
                                cx.notify();
                            });
                        }),
                })
                .content(move |_state, _window, cx| {
                    content_entity.update(cx, |this, cx| {
                        this.render_subagent_popover_content(active_count, cx)
                    })
                })
                .into_any_element(),
        )
    }

    fn render_subagent_popover_content(
        &mut self,
        active_count: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let subagents = self.model.read(cx).active_subagents().to_vec();
        let theme = cx.theme().colors;
        let selected_run_id = self
            .selected_subagent_run_id
            .clone()
            .filter(|run_id| {
                subagents
                    .iter()
                    .any(|item| item.journal_run_id.as_deref() == Some(run_id.as_str()))
            })
            .or_else(|| {
                subagents
                    .iter()
                    .find(|item| item.status == SubagentActivityStatus::Running)
                    .or_else(|| subagents.last())
                    .and_then(|item| item.journal_run_id.clone())
            });
        let selected = selected_run_id.as_ref().and_then(|run_id| {
            subagents
                .iter()
                .find(|item| item.journal_run_id.as_deref() == Some(run_id.as_str()))
        });
        let mut rows = Vec::new();
        for item in &subagents {
            let run_id = item
                .journal_run_id
                .clone()
                .unwrap_or_else(|| format!("queued-{}-{}", item.batch_run_id, item.task_index));
            let row_id = run_id.clone();
            let is_selected = selected_run_id.as_deref() == Some(run_id.as_str());
            let (marker, color, status) = match item.status {
                SubagentActivityStatus::Queued => ("○", theme.muted_foreground, "Queued"),
                SubagentActivityStatus::Running => ("◌", theme.primary, "Working"),
                SubagentActivityStatus::Completed => ("✓", theme.success, "Completed"),
                SubagentActivityStatus::Failed => ("!", theme.danger, "Failed"),
                SubagentActivityStatus::Cancelled => ("×", theme.warning, "Cancelled"),
            };
            let entity = cx.entity();
            rows.push(
                Button::new(SharedString::from(format!("subagent-popup-row-{row_id}")))
                    .accessibility_label(format!("{} · {status}", item.agent))
                    .ghost()
                    .h_auto()
                    .w_full()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(is_selected, |row| row.bg(theme.muted))
                    .flex()
                    .items_start()
                    .text_left()
                    .gap_2()
                    .on_click(move |_event, _window, cx| {
                        entity.update(cx, |this, cx| {
                            this.selected_subagent_run_id = Some(run_id.clone());
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .w(px(18.0))
                            .flex_none()
                            .text_center()
                            .text_color(color)
                            .font_weight(FontWeight::BOLD)
                            .child(marker),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child(item.agent.clone()),
                                    )
                                    .child(
                                        div().flex_none().text_xs().text_color(color).child(status),
                                    ),
                            )
                            .children(item.model.as_ref().map(|model| {
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        crate::model_catalog::label_for(model)
                                            .unwrap_or_else(|| model.clone()),
                                    )
                            }))
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(if item.task.is_empty() {
                                        item.lane.clone().unwrap_or_default()
                                    } else {
                                        item.task.clone()
                                    }),
                            ),
                    ),
            );
        }
        let detail = selected.map(|item| self.render_subagent_detail(item, cx));
        let count_label = if active_count > 0 {
            format!("{active_count} active")
        } else {
            format!("{} total", subagents.len())
        };
        div()
            .w(px(520.0))
            .max_w(px(CHAT_CONTENT_MAX_WIDTH - 32.0))
            .max_h(px(520.0))
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .shadow_lg()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("Subagents"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(count_label),
                    ),
            )
            .child(
                div()
                    .flex()
                    .min_h(px(240.0))
                    .child(
                        div()
                            .w(px(210.0))
                            .flex_none()
                            .p_2()
                            .border_r_1()
                            .border_color(theme.border)
                            .overflow_y_scrollbar()
                            .children(rows),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .p_3()
                            .overflow_y_scrollbar()
                            .children(detail),
                    ),
            )
            .into_any_element()
    }

    fn render_subagent_detail(
        &mut self,
        item: &SubagentActivityInfo,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let status = match item.status {
            SubagentActivityStatus::Queued => "Queued",
            SubagentActivityStatus::Running => "Working",
            SubagentActivityStatus::Completed => "Completed",
            SubagentActivityStatus::Failed => "Failed",
            SubagentActivityStatus::Cancelled => "Cancelled",
        };
        let messages = item
            .messages
            .iter()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|message| self.render_message(message, cx))
            .collect::<Vec<_>>();
        let workspace = item
            .isolation
            .as_ref()
            .map(|isolation| (isolation.workspace.clone(), isolation.branch.clone()));
        let branch_controls = workspace.map(|(worktree, branch)| {
            let inspect_model = self.model.clone();
            let inspect_root = self.model.read(cx).active_git_work_dir().unwrap();
            let inspect_branch = branch.clone();
            let apply_model = self.model.clone();
            let apply_root = inspect_root.clone();
            let apply_branch = branch.clone();
            let apply_worktree = worktree.clone();
            let discard_model = self.model.clone();
            let discard_root = inspect_root.clone();
            let discard_branch = branch.clone();
            let discard_worktree = worktree.clone();
            let terminal_model = self.model.clone();
            let terminal_worktree = worktree.clone();
            let worktree_available = worktree.is_dir();
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(Icon::default().path("icons/git/branch.svg").xsmall())
                        .child(branch.clone()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new(SharedString::from(format!("inspect-{branch}")))
                                .label("Inspect diff")
                                .outline()
                                .xsmall()
                                .on_click(move |_, _, cx| {
                                    let branch = inspect_branch.clone();
                                    let root = inspect_root.clone();
                                    let task = cx.background_executor().spawn(async move {
                                        threadlane_git::diff_branch(&root, &branch)
                                            .map(|diff| (root, branch, diff))
                                            .map_err(|error| error.to_string())
                                    });
                                    let model = inspect_model.clone();
                                    cx.spawn(async move |cx| {
                                        let result = task.await;
                                        let _ = model.update(cx, |state, cx| {
                                            match result {
                                                Ok((root, branch, diff)) => state.request_open_diff(
                                                    root,
                                                    format!("{branch}.diff"),
                                                    if diff.is_empty() {
                                                        "No committed changes on this branch."
                                                            .into()
                                                    } else {
                                                        diff
                                                    },
                                                ),
                                                Err(error) => state.session_status = Some(error),
                                            }
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("terminal-{branch}")))
                                .label("Terminal")
                                .ghost()
                                .xsmall()
                                .disabled(!worktree_available)
                                .tooltip(if worktree_available {
                                    format!("Open terminal in {}", worktree.display())
                                } else {
                                    "This worktree was cleaned up; the branch is still available."
                                        .into()
                                })
                                .on_click(move |_, _, cx| {
                                    terminal_model.update(cx, |state, cx| {
                                        controller::dispatch(
                                            state,
                                            AppAction::OpenTerminalAt(terminal_worktree.clone()),
                                        );
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("apply-{branch}")))
                                .label("Apply")
                                .xsmall()
                                .disabled(item.status != SubagentActivityStatus::Completed)
                                .on_click(move |_, _, cx| {
                                    let root = apply_root.clone();
                                    let branch = apply_branch.clone();
                                    let worktree = apply_worktree.clone();
                                    let task = cx.background_executor().spawn(async move {
                                        let parent = threadlane_git::inspect(&root)
                                            .map_err(|error| error.to_string())?;
                                        if parent.has_changes {
                                            return Err("Commit or stash parent changes before applying a subagent branch.".into());
                                        }
                                        if worktree.is_dir()
                                            && threadlane_git::inspect(&worktree)
                                                .map_err(|error| error.to_string())?
                                                .has_changes
                                        {
                                            return Err("The subagent worktree has uncommitted changes; commit them before applying.".into());
                                        }
                                        threadlane_git::merge(&root, &branch)
                                            .map_err(|error| error.to_string())?;
                                        if worktree.is_dir() {
                                            threadlane_git::remove_worktree(&root, &worktree, false)
                                                .map_err(|error| error.to_string())?;
                                        }
                                        threadlane_git::delete_branch(&root, &branch, false)
                                            .map_err(|error| error.to_string())?;
                                        Ok(format!("Applied {branch}"))
                                    });
                                    let model = apply_model.clone();
                                    cx.spawn(async move |cx| {
                                        let result = task.await;
                                        let _ = model.update(cx, |state, cx| {
                                            state.session_status = Some(result.unwrap_or_else(|error| error));
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("discard-{branch}")))
                                .label("Discard…")
                                .ghost()
                                .xsmall()
                                .on_click(move |_, _, cx| {
                                    let root = discard_root.clone();
                                    let branch = discard_branch.clone();
                                    let worktree = discard_worktree.clone();
                                    let model = discard_model.clone();
                                    cx.spawn(async move |cx| {
                                        let confirmed = rfd::AsyncMessageDialog::new()
                                            .set_title("Discard subagent branch?")
                                            .set_description(format!(
                                                "Delete {branch} and its worktree? This cannot be undone."
                                            ))
                                            .set_buttons(rfd::MessageButtons::YesNo)
                                            .show()
                                            .await;
                                        if !matches!(confirmed, rfd::MessageDialogResult::Yes) {
                                            return;
                                        }
                                        let task = cx.background_executor().spawn(async move {
                                            if worktree.is_dir() {
                                                threadlane_git::remove_worktree(&root, &worktree, true)
                                                    .map_err(|error| error.to_string())?;
                                            }
                                            threadlane_git::delete_branch(&root, &branch, true)
                                                .map_err(|error| error.to_string())?;
                                            let _ = threadlane_git::prune_worktrees(&root);
                                            Ok::<_, String>(format!("Discarded {branch}"))
                                        });
                                        let result = task.await;
                                        let _ = model.update(cx, |state, cx| {
                                            state.session_status = Some(result.unwrap_or_else(|error| error));
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                }),
                        ),
                )
        });
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(item.agent.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(status),
                            ),
                    )
                    .when_some(item.model.as_ref(), |header, model| {
                        header.child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.muted_foreground)
                                .child(
                                    crate::model_catalog::label_for(model)
                                        .unwrap_or_else(|| model.clone()),
                                ),
                        )
                    })
                    .when(!item.task.is_empty(), |header| {
                        header.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(item.task.clone()),
                        )
                    }),
            )
            .children(branch_controls)
            .children(item.error.as_ref().map(|error| {
                div()
                    .p_2()
                    .rounded_md()
                    .bg(theme.danger.opacity(0.08))
                    .text_xs()
                    .text_color(theme.danger)
                    .child(error.clone())
            }))
            .children(messages.is_empty().then(|| {
                div().w_full().flex().justify_center().py_4().child(
                    div()
                        .px_4()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.title_bar)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("No messages yet — the subagent hasn't responded."),
                )
            }))
            .children(messages)
            .into_any_element()
    }

    fn render_composer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let (
            selected_model,
            reasoning_effort,
            is_generating,
            pending_message,
            active_session_id,
            session_status,
        ) = {
            let state = self.model.read(cx);
            (
                state.selected_model.clone(),
                state.reasoning_effort,
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
                .map(|context| ContextMeterContext {
                    current_tokens: context.current_tokens,
                    context_limit: context.context_limit,
                    context_limit_is_estimate: context.context_limit_is_estimate,
                    effective_model: context.effective_model.clone(),
                    last_compaction_seq: context.last_compaction_seq,
                    provisional: context.provisional,
                    estimating: context.estimating,
                });
            (state.active_session_metrics(), context_window)
        };
        let subagent_count = self.model.read(cx).active_subagents().len();
        let supports_live_steering = !threadlane_session::is_acp_model(&selected_model);
        let steer_tooltip = if supports_live_steering {
            "Steer current turn immediately (Cmd+Enter)"
        } else {
            "This agent does not support live steering. Use Queue for the next turn."
        };
        let has_composer_text = !self.input_state.read(cx).value().trim().is_empty();
        let has_prompt =
            !self.input_state.read(cx).value().trim().is_empty() || !self.pasted_images.is_empty();
        let (model_options, selected_option, project_root) = {
            let state = self.model.read(cx);
            let options = state.available_models().to_vec();
            let opt = options.iter().find(|o| o.id == selected_model).cloned();
            let project = state.active_work_dir.clone();
            (options, opt, project)
        };
        let has_models = !model_options.is_empty();
        let needs_provider = !has_models;
        let model_label = selected_option
            .as_ref()
            .map(|option| option.label.clone())
            .unwrap_or_else(|| "Connect a provider".to_string());
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
        let acp_model_sections: HashMap<String, Vec<threadlane_session::AcpConfigOption>> = {
            let state = self.model.read(cx);
            let mut sections = HashMap::new();
            for option in &model_options {
                if option.provider != crate::model_catalog::ModelProvider::Acp {
                    continue;
                }
                let Some(agent_id) = threadlane_session::acp_agent_id(&option.id) else {
                    continue;
                };
                let options = if option.id == selected_model {
                    state.active_acp_config_options()
                } else {
                    crate::model_catalog::cached_acp_config_options(agent_id)
                };
                sections.insert(agent_id.to_string(), options);
            }
            sections
        };
        let model_for_picker = self.model.clone();
        let queue_model = self.model.clone();
        let steer_model = self.model.clone();
        let dismiss_model = self.model.clone();
        let dismiss_input = self.input_state.clone();
        let cancel_model = self.model.clone();
        let queue_prompt_model = self.model.clone();
        let queue_prompt_input = self.input_state.clone();
        let steer_prompt_model = self.model.clone();
        let steer_prompt_input = self.input_state.clone();
        let send_model = self.model.clone();
        let send_input = self.input_state.clone();

        let image_chips = self
            .pasted_images
            .iter()
            .enumerate()
            .map(|(index, image)| {
                let name = image.display_name.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_2p5()
                    .py_1()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .text_xs()
                    .text_color(theme.foreground)
                    .child(Icon::new(IconName::File).xsmall().text_color(theme.primary))
                    .child(
                        div()
                            .id(SharedString::from(format!("pasted-image-{index}")))
                            .max_w(px(160.0))
                            .truncate()
                            .tooltip({
                                let tip = name.clone();
                                move |window, cx| {
                                    gpui_component::tooltip::Tooltip::new(tip.clone())
                                        .build(window, cx)
                                }
                            })
                            .child(name),
                    )
                    .child(
                        Button::new(("remove-pasted-image", index))
                            .icon(IconName::Close)
                            .accessibility_label("Remove image")
                            .xsmall()
                            .ghost()
                            .tooltip("Remove image")
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                if index < this.pasted_images.len() {
                                    this.pasted_images.remove(index);
                                    cx.notify();
                                }
                            })),
                    )
            })
            .collect::<Vec<_>>();

        let provider_setup_model = self.model.clone();
        let provider_setup_banner = needs_provider.then(|| {
            div()
                .w_full()
                .max_w(px(1000.0))
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
                        .icon(IconName::Settings)
                        .label("Open Settings")
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
        let project_chip = Button::new("composer-project-chip")
            .icon(IconName::Folder)
            .label(selected_project_name)
            .dropdown_caret(true)
            .outline()
            .xsmall()
            .dropdown_menu(move |menu, _window, _cx| {
                let mut menu = menu;
                for (name, work_dir) in projects_list.clone() {
                    let model = project_chip_model.clone();
                    menu = menu.item(PopupMenuItem::new(name).on_click(
                        move |_event, _window, cx| {
                            model.update(cx, |state, cx| {
                                controller::dispatch(
                                    state,
                                    AppAction::SelectDraftProject(work_dir.clone()),
                                );
                                cx.notify();
                            });
                        },
                    ));
                }

                let model = project_chip_model.clone();
                menu.separator()
                    .item(PopupMenuItem::new("New project...").on_click(
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
                    ))
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

        let work_mode_chip = Button::new("composer-workmode-chip")
            .icon(if effective_work_mode == WorkMode::Worktree {
                Icon::default().path("icons/git/branch.svg")
            } else {
                Icon::new(IconName::SquareTerminal)
            })
            .label(work_mode_label)
            .dropdown_caret(true)
            .outline()
            .xsmall()
            .dropdown_menu(move |menu, _window, _cx| {
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

        // Keep the context row focused on the two choices that affect where
        // work happens. The branch is already represented by the worktree
        // choice and is not an additional control the user needs to parse.
        let composer_context_bar = div()
            .w_full()
            .max_w(px(1000.0))
            .mx_auto()
            .mb_2()
            .flex()
            .items_center()
            .gap_2()
            .child(project_chip)
            .child(work_mode_chip);

        let pending_preview = pending_message.map(|text| {
            div()
                .w_full()
                .max_w(px(1000.0))
                .mx_auto()
                .mb_2()
                .h(px(52.0))
                .px_3()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.title_bar)
                .flex()
                .items_center()
                .gap_3()
                .child(
                    Tag::new()
                        .child("Pending")
                        .with_variant(TagVariant::Secondary)
                        .small(),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(theme.foreground)
                        .child(text),
                )
                .child(
                    Button::new("queue-pending-message")
                        .icon(IconName::Plus)
                        .accessibility_label("Queue pending message")
                        .xsmall()
                        .secondary()
                        .tooltip("Queue after the current response")
                        .on_click(move |_event, _window, cx| {
                            queue_model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::QueuePendingMessage);
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new("steer-pending-message")
                        .icon(IconName::ArrowRight)
                        .accessibility_label("Steer with pending message")
                        .xsmall()
                        .primary()
                        .disabled(!supports_live_steering)
                        .tooltip(steer_tooltip)
                        .on_click(move |_event, _window, cx| {
                            steer_model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::SteerPendingMessage);
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new("dismiss-pending-message")
                        .icon(IconName::Undo2)
                        .accessibility_label("Edit pending message")
                        .xsmall()
                        .ghost()
                        .tooltip("Edit message in the composer")
                        .on_click(move |_event, window, cx| {
                            let restored = dismiss_model.update(cx, |state, cx| {
                                let restored =
                                    state.active_pending_composer_message().map(str::to_owned);
                                controller::dispatch(state, AppAction::DismissPendingMessage);
                                cx.notify();
                                restored
                            });
                            if let Some(restored) = restored {
                                dismiss_input.update(cx, |input, cx| {
                                    input.set_value(restored, window, cx);
                                });
                            }
                        }),
                )
        });

        let model_picker = Button::new("composer-model-picker")
            .label(model_label.clone())
            .dropdown_caret(true)
            .ghost()
            .disabled(!has_models)
            .tooltip(if has_models {
                format!("Model: {model_label}")
            } else {
                "No models available — connect a provider in Settings".to_string()
            });

        let model_picker = if let Some(option) = selected_option.as_ref() {
            model_picker.icon(Icon::default().path(option.provider.icon_path()))
        } else {
            model_picker
        };
        let selected_model_for_picker = selected_model.clone();
        let submenu_click_model = self.model.clone();
        let model_picker = model_picker.dropdown_menu(move |menu, _window, _cx| {
            let menu = menu.check_side(gpui_component::Side::Right);
            let mut previous_provider = None;
            let menu = model_options.iter().cloned().fold(
                menu.max_h(px(320.0)).scrollable(true),
                |menu, option| {
                    let menu = if previous_provider == Some(option.provider) {
                        menu
                    } else {
                        previous_provider = Some(option.provider);
                        menu.item(PopupMenuItem::label(option.provider.label()))
                    };
                    if option.provider != crate::model_catalog::ModelProvider::Acp {
                        let model = model_for_picker.clone();
                        let is_current = option.id == selected_model_for_picker;
                        let label = if is_current {
                            format!("{} · Current", option.label)
                        } else {
                            option.label
                        };
                        return menu.item(
                            PopupMenuItem::new(label)
                                .icon(Icon::default().path(option.provider.icon_path()))
                                .checked(is_current)
                                .on_click(move |_event, _window, cx| {
                                    model.update(cx, |state, cx| {
                                        controller::dispatch(
                                            state,
                                            AppAction::SelectModel(option.id.to_string()),
                                        );
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                    // External agents list their own models inline, fed by the
                    // shared launch-time cache until this session's engine
                    // connects. Picking one selects the agent and applies the
                    // model in a single gesture — no hover, no pre-select.
                    let agent_key = threadlane_session::acp_agent_id(&option.id)
                        .unwrap_or_default()
                        .to_string();
                    let agent_options = acp_model_sections
                        .get(&agent_key)
                        .cloned()
                        .unwrap_or_default();
                    let agent_setting = threadlane_session::config_option_for(
                        &agent_options,
                        threadlane_session::ACP_CONFIG_CATEGORY_MODEL,
                    )
                    .cloned();
                    let is_current = option.id == selected_model_for_picker;
                    let agent_label = if is_current {
                        format!("{} · Current", option.label)
                    } else {
                        option.label.clone()
                    };
                    let select_model = submenu_click_model.clone();
                    let select_id = option.id.clone();
                    let menu = menu.item(
                        PopupMenuItem::new(agent_label)
                            .icon(Icon::default().path(option.provider.icon_path()))
                            .checked(is_current)
                            .on_click(move |_event, _window, cx| {
                                select_model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::SelectModel(select_id.clone()),
                                    );
                                    cx.notify();
                                });
                            }),
                    );
                    match agent_setting {
                        Some(setting) => {
                            let current = setting.current_value().map(str::to_string);
                            let config_id = setting.id.clone();
                            setting.options.into_iter().fold(menu, |menu, choice| {
                                let click_model = submenu_click_model.clone();
                                let select_id = option.id.clone();
                                let config_id = config_id.clone();
                                let value = choice.value.clone();
                                // Only the selected agent's live state can
                                // mark a current model; cached currents may
                                // be stale, so other agents show none.
                                let checked =
                                    is_current && current.as_deref() == Some(choice.value.as_str());
                                let label = if checked {
                                    format!("{} · Current", choice.name)
                                } else {
                                    choice.name.clone()
                                };
                                menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                                    move |_event, _window, cx| {
                                        click_model.update(cx, |state, cx| {
                                            controller::dispatch(
                                                state,
                                                AppAction::SelectModel(select_id.clone()),
                                            );
                                            controller::dispatch(
                                                state,
                                                AppAction::SetAcpConfigOption {
                                                    config_id: config_id.clone(),
                                                    value: value.clone(),
                                                },
                                            );
                                            cx.notify();
                                        });
                                    },
                                ))
                            })
                        }
                        None => {
                            let reason = crate::model_catalog::cached_acp_error(&agent_key)
                                .map(|error| {
                                    let short: String = error.chars().take(120).collect();
                                    if error.chars().count() > 120 {
                                        format!("{short}…")
                                    } else {
                                        short
                                    }
                                })
                                .unwrap_or_else(|| format!("Connecting to {}…", option.label));
                            let settings_model = submenu_click_model.clone();
                            menu.item(PopupMenuItem::new(reason).disabled(true)).item(
                                PopupMenuItem::new("Check Settings → ACP Agents").on_click(
                                    move |_event, _window, cx| {
                                        settings_model.update(cx, |state, cx| {
                                            controller::dispatch(state, AppAction::OpenSettings);
                                            cx.notify();
                                        });
                                    },
                                ),
                            )
                        }
                    }
                },
            );
            menu
        });

        let effort_model = self.model.clone();
        let effort_options =
            crate::model_catalog::efforts_for_model(&selected_model, project_root.as_deref());
        // Models without thinking (ACP agents, off-only registry entries)
        // offer no effort control instead of dead options.
        let show_effort_picker = crate::model_catalog::supports_reasoning(
            &selected_model,
            project_root.as_deref(),
        );
        let effort_picker = Button::new("composer-reasoning-effort-picker")
            .icon(Icon::default().path("icons/effort.svg"))
            .label(reasoning_effort.label())
            .tooltip(format!("Reasoning effort: {}", reasoning_effort.label()))
            .dropdown_caret(true)
            .ghost()
            .dropdown_menu(move |menu, _window, _cx| {
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

        let input_value = self.input_state.read(cx).value().to_string();
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
                    .left(px(0.0))
                    .mb(px(8.0))
                    .w_full()
                    .max_w(px(640.0))
                    .max_h(px(320.0))
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
                            .h(px(28.0))
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
                                        div().font_weight(FontWeight::SEMIBOLD).child("COMMANDS"),
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
                            .max_h(px(260.0))
                            .when(!has_commands, |list| {
                                list.child(
                                    div()
                                        .h(px(36.0))
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
                                    .h(px(30.0))
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
                                    .cursor_pointer()
                                    .child(
                                        div()
                                            .w(px(112.0))
                                            .flex_none()
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

        let meter = context_meter_view_model(
            context_window.as_ref(),
            &ContextMeterMetrics {
                billed_input_tokens: metrics.billed_input_tokens(),
                output_tokens: metrics.output_tokens,
                cache_hit_percent: metrics.cache_hit_percent(),
            },
            !threadlane_session::is_acp_model(&selected_model),
        );
        let displayed_percent = meter.percent.unwrap_or_default();
        let meter_color = if meter.percent.is_none() || displayed_percent == 0.0 {
            theme.muted_foreground
        } else if displayed_percent >= CONTEXT_METER_DANGER_PCT {
            theme.danger
        } else if displayed_percent >= CONTEXT_METER_WARN_PCT {
            theme.warning
        } else {
            theme.accent
        };
        let subagent_popover = self.render_subagent_popover(cx);
        let context_meter_open = self.context_meter_open;
        let toggle_context_meter = cx.entity();
        let sync_context_meter = cx.entity();
        let context_meter = Popover::new("context-window-popover")
            .anchor(Anchor::BottomRight)
            .appearance(false)
            .open(context_meter_open)
            .on_open_change(move |open, _window, cx| {
                sync_context_meter.update(cx, |this, cx| {
                    if this.context_meter_open != *open {
                        this.context_meter_open = *open;
                        cx.notify();
                    }
                });
            })
            .trigger(ContextMeterTrigger {
                selected: context_meter_open,
                toggle: Toggle::new("context-meter-badge")
                    .ghost()
                    .rounded_full()
                    .size(px(32.0))
                    .tooltip(meter.detail_label.clone())
                    .child(
                        ProgressCircle::new("context-meter-circle")
                            .value(meter.bar_percent as f32)
                            .color(meter_color)
                            .size(px(24.0)),
                    )
                    .on_click(move |open, _window, cx| {
                        toggle_context_meter.update(cx, |this, cx| {
                            this.context_meter_open = *open;
                            cx.notify();
                        });
                    }),
            })
            .content(move |_state, _window, _cx| {
                let bar_width = meter.bar_percent / 100.0 * 308.0;
                let current_summary = match meter.percent {
                    Some(percent) => format!(
                        "{percent:.0}% · {}{}",
                        meter.current_label,
                        if meter.provisional {
                            " · provisional"
                        } else {
                            ""
                        }
                    ),
                    None => meter.current_label.clone(),
                };
                div()
                    .w(px(340.0))
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child("Current context"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(current_summary),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(5.0))
                            .rounded_full()
                            .bg(theme.muted.opacity(0.8))
                            .child(
                                div()
                                    .h_full()
                                    .w(px(bar_width as f32))
                                    .rounded_full()
                                    .bg(meter_color),
                            ),
                    )
                    .when_some(meter.effective_model.clone(), |card, effective_model| {
                        card.child(
                            div()
                                .flex()
                                .justify_between()
                                .items_center()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("Model")
                                .child(effective_model),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Total processed")
                            .child(meter.total_processed_label.clone()),
                    )
                    .when_some(meter.cache_hit_label.clone(), |card, cache_hit| {
                        card.child(
                            div()
                                .flex()
                                .justify_between()
                                .items_center()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("Cache hit")
                                .child(cache_hit),
                        )
                    })
                    .when_some(meter.last_compaction_seq, |card, sequence| {
                        card.child(
                            div()
                                .flex()
                                .justify_between()
                                .items_center()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("Last compacted")
                                .child(format!("Record #{sequence}")),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Context is compacted automatically when needed. Percent reflects the current model request."),
                    )
            });

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
            let preview_text = if draft.chars().count() > 60 {
                format!("{}…", draft.chars().take(60).collect::<String>())
            } else {
                draft.clone()
            };
            div()
                .w_full()
                .mb_2()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(theme.accent.opacity(0.3))
                .bg(theme.accent.opacity(0.1))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_1()
                        .min_w_0()
                        .child(IconName::File)
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.foreground)
                                .truncate()
                                .child(format!("Stashed draft: \"{preview_text}\"")),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            Button::new("restore-stashed-draft")
                                .label("Restore Draft")
                                .small()
                                .primary()
                                .on_click(move |_event, window, cx| {
                                    if let Some(session_id) = &restore_session_id {
                                        if let Some(text) = restore_model.update(cx, |state, cx| {
                                            let popped = state.pop_stashed_prompt(session_id);
                                            cx.notify();
                                            popped
                                        }) {
                                            restore_input.update(cx, |input, cx| {
                                                input.set_value(text, window, cx);
                                            });
                                        }
                                    }
                                }),
                        )
                        .child(
                            Button::new("dismiss-stashed-draft")
                                .icon(IconName::Close)
                                .accessibility_label("Discard stashed draft")
                                .ghost()
                                .xsmall()
                                .tooltip("Discard stash")
                                .on_click(move |_event, _window, cx| {
                                    if let Some(session_id) = &dismiss_session_id {
                                        dismiss_model.update(cx, |state, cx| {
                                            state.clear_stashed_prompt(session_id);
                                            cx.notify();
                                        });
                                    }
                                }),
                        ),
                )
        });

        let stash_button = {
            let do_stash_input = self.input_state.clone();
            let do_stash_model = self.model.clone();
            let do_stash_session_id = active_session_id.clone();
            Button::new("stash-prompt-btn")
                .icon(IconName::Folder)
                .accessibility_label("Stash draft")
                .tooltip("Stash draft")
                .ghost()
                .small()
                .disabled(is_generating || !has_prompt)
                .on_click(move |_event, window, cx| {
                    if let Some(session_id) = &do_stash_session_id {
                        let text = do_stash_input.read(cx).value().to_string();
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
                })
        };

        let billed_input_tokens = metrics.billed_input_tokens();
        let cache_hit = metrics
            .cache_hit_percent()
            .map(|percent| format!(" · Cache hit {percent}%"))
            .unwrap_or_default();

        div()
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .px_4()
            .pt_3()
            .pb_2()
            .bg(theme.background)
            .children(provider_setup_banner)
            .children(session_status.map(|status| {
                let (summary, needs_provider_settings) = chat_error_summary(&status);
                let is_error = status.starts_with("Could not")
                    || status.starts_with("Failed")
                    || status.starts_with("Error")
                    || needs_provider_settings;
                if is_error {
                    return render_chat_error("session-status", &status, &self.model, cx)
                        .into_any_element();
                }
                div()
                    .w_full()
                    .max_w(px(1000.0))
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
            .child(composer_context_bar)
            .child(
                div()
                    .w_full()
                    .max_w(px(1000.0))
                    .mx_auto()
                    .relative()
                    .min_h(px(136.0))
                    .flex()
                    .flex_col()
                    .justify_between()
                    .p_4()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .shadow_md()
                    .hover(|style| style.border_color(theme.border.opacity(0.85)))
                    .on_action(cx.listener(Self::paste_composer_clipboard))
                    .when(slash_completion_active, |composer| {
                        composer
                            .key_context(SLASH_COMMAND_KEY_CONTEXT)
                            .on_action(cx.listener(Self::complete_slash_command_action))
                            .on_action(cx.listener(Self::select_previous_slash_command_action))
                            .on_action(cx.listener(Self::select_next_slash_command_action))
                            .on_action(cx.listener(Self::dismiss_slash_command_action))
                    })
                    .children(stash_banner)
                    .children(
                        (!image_chips.is_empty())
                            .then(|| div().flex().flex_wrap().gap_2().children(image_chips)),
                    )
                    .child(command_menu)
                    .child(
                        div()
                            .w_full()
                            .flex_1()
                            .min_h(px(48.0))
                            .child(
                                Textarea::new(&self.input_state)
                                    .appearance(false)
                                    .bordered(false),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(model_picker)
                            .children(show_effort_picker.then_some(effort_picker))
                            .child(div().flex_1())
                            .child(stash_button)
                            .children(subagent_popover)
                            .child(context_meter)
                            .children(if is_generating {
                                    vec![
                                        Button::new("composer-queue-btn")
                                            .icon(IconName::Plus)
                                            .label("Queue")
                                            .small()
                                            .secondary()
                                            .disabled(!has_composer_text)
                                            .tooltip("Queue for next turn (Enter)")
                                            .on_click(cx.listener(move |this, _event, window, cx| {
                                                let text = queue_prompt_input.read(cx).value().to_string();
                                                if !text.trim().is_empty() {
                                                    queue_prompt_model.update(cx, |state, cx| {
                                                        let images = std::mem::take(&mut this.pasted_images);
                                                        controller::dispatch(state, AppAction::StageBusyMessage { text, images });
                                                        controller::dispatch(state, AppAction::QueuePendingMessage);
                                                        cx.notify();
                                                    });
                                                    queue_prompt_input.update(cx, |state, cx| {
                                                        state.set_value("", window, cx);
                                                    });
                                                    this.transcript_list_state.scroll_to_end();
                                                    cx.notify();
                                                }
                                            }))
                                            .into_any_element(),
                                        Button::new("composer-steer-btn")
                                            .icon(IconName::ArrowRight)
                                            .label("Steer")
                                            .small()
                                            .primary()
                                            .disabled(!has_composer_text || !supports_live_steering)
                                            .tooltip(steer_tooltip)
                                            .on_click(cx.listener(move |this, _event, window, cx| {
                                                let text = steer_prompt_input.read(cx).value().to_string();
                                                if !text.trim().is_empty() {
                                                    steer_prompt_model.update(cx, |state, cx| {
                                                        let images = std::mem::take(&mut this.pasted_images);
                                                        controller::dispatch(state, AppAction::StageBusyMessage { text, images });
                                                        controller::dispatch(state, AppAction::SteerPendingMessage);
                                                        cx.notify();
                                                    });
                                                    steer_prompt_input.update(cx, |state, cx| {
                                                        state.set_value("", window, cx);
                                                    });
                                                    this.transcript_list_state.scroll_to_end();
                                                    cx.notify();
                                                }
                                            }))
                                            .into_any_element(),
                                        Button::new("composer-stop-btn")
                                            .icon(IconName::CircleX)
                                            .accessibility_label("Stop generation")
                                            .small()
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
                                            .into_any_element(),
                                    ]
                            } else {
                                vec![
                                    Button::new("send-btn")
                                        .w(px(34.0))
                                        .h(px(34.0))
                                        .icon(IconName::ArrowUp)
                                        .accessibility_label("Send message")
                                        .tooltip(if needs_provider {
                                            "Connect a model provider in Settings before sending"
                                        } else if has_prompt {
                                            "Send message (Enter)"
                                        } else {
                                            "Type a message to send"
                                        })
                                        .when(has_prompt && !needs_provider, |b| b.primary())
                                        .when(!has_prompt || needs_provider, |b| b.ghost().disabled(true))
                                        .on_click(cx.listener(move |this, _event, window, cx| {
                                            let text = send_input.read(cx).value().to_string();
                                            if !text.trim().is_empty() || !this.pasted_images.is_empty() {
                                                let images = std::mem::take(&mut this.pasted_images);
                                                send_model.update(cx, |state, cx| {
                                                    controller::dispatch(
                                                        state,
                                                        AppAction::SendPromptWithImages {
                                                            text,
                                                            images,
                                                        },
                                                    );
                                                    cx.notify();
                                                });
                                                send_input.update(cx, |state, cx| {
                                                    state.set_value("", window, cx);
                                                });
                                                this.transcript_list_state.scroll_to_end();
                                                cx.notify();
                                            }
                                        }))
                                        .into_any_element(),
                                ]
                            }),
                    ),
            )
            // Keep this summary mounted even before the first response metrics arrive.
            // Otherwise the composer gains a new row mid-turn, shifting the transcript and
            // input as soon as the first tool call or token count is reported.
            .child(
                div()
                    .w_full()
                    .max_w(px(1000.0))
                    .mx_auto()
                    .flex()
                    .justify_center()
                    .pt_1()
                    .pb_2()
                    .px_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "{} turns · {} tool calls{cache_hit} · {} input / {} output tokens · {} subagents",
                        metrics.turns,
                        metrics.tool_calls,
                        crate::model_catalog::format_tokens(
                            billed_input_tokens.min(u64::from(u32::MAX)) as u32
                        ),
                        crate::model_catalog::format_tokens(
                            metrics.output_tokens.min(u64::from(u32::MAX)) as u32
                        ),
                        subagent_count,
                    )),
            )
    }
}

impl ChatListView {
    fn render_progress_summary(&self, cx: &mut Context<Self>) -> AnyElement {
        let (summary, category, tool_detail, active_subagent_tasks) = {
            let state = self.model.read(cx);
            let latest_tool = state
                .messages
                .iter()
                .rev()
                .flat_map(|message| message.tool_activities.iter().rev())
                .next();
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
                .unwrap_or_else(|| "Preparing the next step".to_string());
            let category = latest_tool
                .map(|tool| tool.category.clone())
                .unwrap_or_else(|| "Agent activity".to_string());
            let tool_detail = latest_tool
                .map(|tool| tool.detail.trim().to_string())
                .filter(|detail| !detail.is_empty());
            (summary, category, tool_detail, active_subagents)
        };
        let theme = cx.theme().colors;
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
            .on_click(cx.listener(|this, _, _, cx| this.toggle_progress_summary(cx)))
            .flex_none()
            .mx_4()
            .mt_2()
            .mb_1()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1p5()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.35))
            .cursor_pointer();

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
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child("Working on:"),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(summary),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(subagent_label.unwrap_or(category)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme.muted_foreground)
                    .child(
                        Icon::new(if self.progress_summary_expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall(),
                    ),
            );

        container = container.child(header_row);

        if self.progress_summary_expanded {
            let mut expanded_content = div()
                .flex()
                .flex_col()
                .gap_1p5()
                .pt_1p5()
                .border_t_1()
                .border_color(theme.border.opacity(0.5));

            if let Some(detail) = tool_detail {
                expanded_content = expanded_content.child(
                    div()
                        .max_h(px(120.0))
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
                let subagents_view = div().flex().flex_col().gap_1().children(
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
                                        .py(px(0.5))
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
                );
                expanded_content = expanded_content.child(subagents_view);
            }

            expanded_content = expanded_content.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Click to collapse")
                    .child(
                        Button::new("progress-open-trajectory")
                            .label("Open Trajectory")
                            .icon(IconName::ChevronRight)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.progress_summary_expanded = false;
                                this.current_tab = CentralTab::Trajectory;
                                cx.notify();
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

impl Render for ChatListView {

    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (messages, is_new_task, active_plan, session_key, is_generating, has_active_permission) = {
            let state = self.model.read(cx);
            let has_active_permission = state
                .active_session_id
                .as_ref()
                .is_some_and(|session_id| state.pending_permissions.contains_key(session_id));
            (
                state.messages.clone(),
                state.is_new_task,
                state.active_plan.clone(),
                state
                    .active_work_dir
                    .clone()
                    .zip(state.active_session_id.clone()),
                state.is_generating,
                has_active_permission,
            )
        };
        let session_changed = session_key != self.last_session_key;
        if session_changed {
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
            self.trajectory_category = None;
            self.trajectory_lane = None;
            self.selected_trajectory_index = None;
            self.trajectory_search.clear();
            self.trajectory_cache = None;
            self.trajectory_raw_json = None;
            self.subagents_popover_open = false;
            self.selected_subagent_run_id = None;
            self.selected_slash_index = 0;
            self.dismiss_slash_menu = false;
            self.permission_details_open = false;
            self.question_selections.clear();
            self.question_inputs.clear();
            self.trajectory_search_input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        }
        if self.permission_details_open && !has_active_permission {
            self.permission_details_open = false;
        }
        self.sync_transcript_rows(messages.clone(), is_generating, session_changed);
        if let Some(prompt) = self
            .model
            .update(cx, |state, _cx| state.requested_composer_prompt.take())
        {
            self.current_tab = CentralTab::Chat;
            self.input_state.update(cx, |input, cx| {
                input.set_value(&prompt, window, cx);
            });
        }
        if self.initial_scroll_frames > 0 {
            self.transcript_list_state.scroll_to_end();
            self.initial_scroll_frames = self.initial_scroll_frames.saturating_sub(1);
        }
        let theme = cx.theme().colors;

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .bg(theme.background)
            .on_key_down(cx.listener(Self::handle_key_down))
            .child(self.render_header(cx))
            .children(
                (self.current_tab == CentralTab::Chat && is_generating)
                    .then(|| self.render_progress_summary(cx)),
            )
            .child(match self.current_tab {
                CentralTab::Editor => self.editor.clone().into_any_element(),
                CentralTab::Trajectory => self.render_trajectory(cx),
                CentralTab::Chat => {
                    if is_new_task {
                        self.render_new_task(cx)
                    } else if messages.is_empty() {
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .px_4()
                            .child(
                                div()
                                    .w_full()
                                    .max_w(px(440.0))
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .gap_3()
                                    .px_6()
                                    .py_8()
                                    .rounded_xl()
                                    .border_1()
                                    .border_color(theme.border)
                                    .bg(theme.title_bar)
                                    .child(
                                        div()
                                            .size(px(40.0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded_full()
                                            .bg(theme.primary.opacity(0.12))
                                            .text_color(theme.primary)
                                            .child(IconName::Bot),
                                    )
                                    .child(
                                        div()
                                            .text_base()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.foreground)
                                            .child("Ready when you are"),
                                    )
                                    .child(
                                        div()
                                            .max_w(px(320.0))
                                            .text_center()
                                            .text_sm()
                                            .text_color(theme.muted_foreground)
                                            .child(
                                                "Describe what you want to build, investigate, or fix. Threadlane can use your project context and tools to help.",
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("Press Enter to send · Shift+Enter for a new line"),
                                    ),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .id("chat-transcript-container")
                            .relative()
                            .w_full()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(
                                list(
                                    self.transcript_list_state.clone(),
                                    cx.processor(Self::render_transcript_row),
                                )
                                .w_full()
                                .max_w(px(CHAT_CONTENT_MAX_WIDTH))
                                .h_full()
                                .mx_auto()
                                .pt_3()
                                .pb_6()
                                .with_sizing_behavior(ListSizingBehavior::Auto),
                            )
                            .child(div().absolute().inset_0().child(
                                gpui_component::scroll::Scrollbar::vertical(
                                    &self.transcript_list_state,
                                ),
                            ))
                            .into_any_element()
                    }
                }
            })
            .children(
                (self.current_tab == CentralTab::Chat)
                    .then(|| self.render_plan_tracker(&active_plan, cx))
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
            .children((self.current_tab == CentralTab::Chat).then(|| self.render_composer(cx)))
            .children(
                (self.current_tab == CentralTab::Chat && self.permission_details_open)
                    .then(|| self.render_permission_details_dialog(cx))
                    .flatten(),
            )
    }
}

#[path = "tests.rs"]
#[cfg(test)]
mod hot_path_tests;
