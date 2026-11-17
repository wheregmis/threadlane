//! Editor presentation. Hosts own buffers, file access, selection and save/close guards.
use std::path::PathBuf;

use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Editor, EditorState, RopeExt};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::scroll::{Scrollable, ScrollableElement};
use gpui_component::tag::Tag;
use gpui_component::text::TextViewState;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Sizable};
use gpui_component::WindowExt;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum EditorSaveStatus {
    #[default]
    Ready,
    Saving,
    Reloading,
    Conflict,
    Deleted,
    Unsupported,
    Failed { message: String, save_blocked: bool },
}

impl EditorSaveStatus {
    pub fn busy(&self) -> bool {
        matches!(self, Self::Saving | Self::Reloading)
    }

    pub fn allows_save(&self) -> bool {
        matches!(self, Self::Ready | Self::Failed { save_blocked: false, .. })
    }

    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Ready => None,
            Self::Saving => Some("Saving… You can keep editing."),
            Self::Reloading => Some("Reading saved file… Your edits are kept until the read succeeds."),
            Self::Conflict => Some("Couldn't save: this file changed on disk. Your edits are still here."),
            Self::Deleted => Some("Couldn't save: this file was deleted. Your edits are still here. Restore the file before reloading; Save will not recreate it."),
            Self::Unsupported => Some("Saving requires an updated daemon with guarded-save support. Your edits are still here."),
            Self::Failed { message, .. } => Some(message),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorRecoveryAction { Copy, Reload, RetrySave }

/// Persistent document-local recovery; hosts retain the buffer and own all I/O.
pub fn editor_save_recovery(
    target: String,
    status: &EditorSaveStatus,
    on_action: impl Fn(&EditorRecoveryAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Option<Div> {
    let message = status.message()?;
    let callback = std::rc::Rc::new(on_action);
    let request = move |action| {
        let callback = callback.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| callback(&action, window, cx)
    };
    Some(
        div()
            .flex_none()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .id("editor-save-status")
                    .role(if status.busy() {
                        Role::Status
                    } else {
                        Role::Alert
                    })
                    .text_xs()
                    .whitespace_normal()
                    .child(format!("{target}: {message}")),
            )
            .when(!status.busy(), |view| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .whitespace_normal()
                        .child("Copy your edits, reload the saved file, then manually reapply your changes."),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            Button::new("editor-copy-edits")
                                .debug_selector(|| "editor-copy-edits".into())
                                .outline()
                                .small()
                                .label("Copy my edits")
                                .accessibility_label(format!("Copy all my edits in {target}"))
                                .on_click(request(EditorRecoveryAction::Copy)),
                        )
                        .child(
                            Button::new("editor-reload-file")
                                .debug_selector(|| "editor-reload-file".into())
                                .outline()
                                .small()
                                .label("Reload from disk…")
                                .disabled(matches!(status, EditorSaveStatus::Unsupported))
                                .accessibility_label(format!("Reload {target} from disk…"))
                                .on_click(request(EditorRecoveryAction::Reload)),
                        )
                        .when(status.allows_save(), |view| {
                            view.child(
                                Button::new("editor-retry-save")
                                    .outline()
                                    .small()
                                    .label("Retry Save")
                                    .on_click(request(EditorRecoveryAction::RetrySave)),
                            )
                        }),
                )
            }),
    )
}

pub fn confirm_editor_reload(
    target: String,
    window: &mut Window,
    cx: &mut App,
    on_confirm: impl Fn(&mut Window, &mut App) + 'static,
) {
    let on_confirm = std::rc::Rc::new(on_confirm);
    window.open_dialog(cx, move |dialog, _, _| {
        let on_confirm = on_confirm.clone();
        dialog.title(format!("Reload “{target}” from disk?"))
            .child(div().whitespace_normal().child("Unsaved edits will be discarded only after a successful read. Copy your edits first if you want to keep them. If you keep typing, reload will require a new confirmation."))
            .on_ok(|_, _, _| true)
            .footer(div().flex().flex_wrap().gap_2()
                .child(Button::new("editor-cancel-reload").debug_selector(|| "editor-cancel-reload".into()).primary().label("Cancel")
                    .on_click(|_, window, cx| window.close_dialog(cx)))
                .child(Button::new("editor-confirm-reload").debug_selector(|| "editor-confirm-reload".into()).danger().label("Discard edits and reload")
                    .on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        on_confirm(window, cx);
                    })))
    });
}

actions!(editor, [AddSelectionToChat, ReopenClosedFile, SavePanelDocument]);

pub fn init_editor(cx: &mut App) {
    crate::editor_workbench::init(cx);
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-s", SavePanelDocument, Some("PanelDocument")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-s", SavePanelDocument, Some("PanelDocument")),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-t", ReopenClosedFile, Some("CentralEditor")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-t", ReopenClosedFile, Some("CentralEditor")),
        #[cfg(target_family = "wasm")]
        KeyBinding::new("cmd-shift-t", ReopenClosedFile, Some("CentralEditor")),
    ]);
}

#[derive(Clone, Debug, Default)]
pub struct ReopenClosedFileControl {
    target: Option<String>,
    loading: bool,
}

impl ReopenClosedFileControl {
    pub fn with_target(mut self, target: Option<String>) -> Self {
        self.target = target;
        self
    }

    pub fn with_loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// Whether a closed file can be (or is being) reopened.
    pub fn is_available(&self) -> bool {
        self.loading || self.target.is_some()
    }

    pub fn description(&self) -> String {
        match (&self.target, self.loading) {
            (Some(target), loading) => format!(
                "{} {target}. Reopens the saved file; discarded edits are not restored.",
                if loading { "Reopening" } else { "Reopen" },
            ),
            (None, true) => "Reopening saved file; discarded edits are not restored.".into(),
            (None, false) => "No closed files in this window.".into(),
        }
    }
}

pub fn editor_reopen_button(control: &ReopenClosedFileControl) -> Button {
    let description = control.description();
    Button::new("editor-reopen-closed-file")
        .debug_selector(|| "editor-reopen-closed-file".into())
        .ghost()
        .xsmall()
        .label(if control.loading { "Reopening…" } else { "Reopen closed file" })
        .disabled(control.loading || control.target.is_none())
        .accessibility_label(description.clone())
        .tooltip(description)
        .on_click(|_, window, cx| window.dispatch_action(Box::new(ReopenClosedFile), cx))
}

pub fn editor_file_status(
    target: String,
    error: Option<String>,
    retry: Button,
    cx: &App,
) -> Div {
    let failed = error.is_some();
    let message = error.unwrap_or_else(|| "Reading saved file…".into());
    div()
        .flex_none()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .p_3()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(div()
            .id("editor-file-status")
            .role(if failed { Role::Alert } else { Role::Status })
            .flex_1()
            .min_w_0()
            .text_xs()
            .whitespace_normal()
            .text_color(if failed { cx.theme().danger } else { cx.theme().muted_foreground })
            .child(format!("{target}: {message}")))
        .children(failed.then_some(retry))
}

pub fn editor_retry_button(target: &str, loading: bool) -> Button {
    let label = format!("Retry reading saved file {target}. Edits in this buffer are preserved.");
    Button::new("editor-retry-file")
        .debug_selector(|| "editor-retry-file".into())
        .outline()
        .xsmall()
        .label("Retry")
        .disabled(loading)
        .accessibility_label(label.clone())
        .tooltip(label)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelDocumentAction {
    Back,
    Save,
    AddSelectionToChat,
    Close,
}

/// Largest editor selection excerpt a chat handoff accepts, in UTF-8 bytes.
/// Matches the terminal excerpt limit and the "Select less code" notice.
pub const EDITOR_EXCERPT_LIMIT: usize = 32 * 1024;

/// A snapshot of a file editor's active selection captured for the chat
/// handoff. Line numbers are one-based inclusive positions in the buffer at
/// capture time — a snapshot, not a live reference into disk contents.
#[derive(Clone, Debug)]
pub struct EditorSelectionSnapshot {
    /// Exact selected buffer text. Never truncated: oversized selections are
    /// rejected by `editor_excerpt_block_reason` instead.
    pub text: String,
    /// UTF-8 byte range the snapshot was taken from; revalidation compares
    /// the buffer's live `selected_range` against it.
    pub byte_range: std::ops::Range<usize>,
    /// One-based buffer line the selection starts on.
    pub start_line: usize,
    /// One-based buffer line the selection ends on. An exclusive end at the
    /// next line's first byte still counts as the previous line.
    pub end_line: usize,
}

/// Snapshot the active selection of `editor`, `None` when it is empty.
/// Multiple cursors contribute only the active selection.
pub fn editor_selection_snapshot(editor: &EditorState) -> Option<EditorSelectionSnapshot> {
    let byte_range = editor.selected_range();
    if byte_range.start >= byte_range.end {
        return None;
    }
    let text = editor.text().slice(byte_range.clone()).to_string();
    let start = editor.text().offset_to_point(byte_range.start);
    let end = editor.text().offset_to_point(byte_range.end);
    // `end.column == 0` means the selection ends exactly at a line's first
    // byte; the one-based inclusive label must name the previous line.
    let end_line = if end.column == 0 {
        end.row.max(1)
    } else {
        end.row + 1
    };
    Some(EditorSelectionSnapshot {
        text,
        byte_range,
        start_line: start.row + 1,
        end_line,
    })
}

/// `None` when the selection can be handed to the chat draft; otherwise the
/// user-facing reason the command is disabled or a stale activation is
/// rejected. Reasons are textual, never color-only.
pub fn editor_excerpt_block_reason(
    snapshot: Option<&EditorSelectionSnapshot>,
) -> Option<&'static str> {
    match snapshot {
        None => Some("Select code in the file first"),
        Some(snapshot) if snapshot.byte_range.len() > EDITOR_EXCERPT_LIMIT => {
            Some("Select less code (maximum 32 KiB)")
        }
        Some(_) => None,
    }
}

/// A markdown code fence longer than any backtick run in `text`, so the
/// fenced block always parses as one unit.
pub fn safe_fence(text: &str) -> String {
    let longest_run = text
        .split(|ch| ch != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    "`".repeat((longest_run + 1).max(3))
}

/// The labeled, safely fenced plain-text block appended to the draft. The
/// line numbers are one-based inclusive positions in the captured buffer,
/// partial lines and interior whitespace are preserved verbatim, and the
/// provenance label distinguishes an unsaved buffer from a saved snapshot.
pub fn format_editor_excerpt(
    relative_path: &str,
    snapshot: &EditorSelectionSnapshot,
    dirty: bool,
) -> String {
    let lines = if snapshot.start_line == snapshot.end_line {
        format!("buffer line {}", snapshot.start_line)
    } else {
        format!("buffer lines {}–{}", snapshot.start_line, snapshot.end_line)
    };
    let provenance = if dirty {
        "Unsaved buffer"
    } else {
        "Buffer snapshot"
    };
    let fence = safe_fence(&snapshot.text);
    let body = snapshot.text.trim_end_matches('\n');
    format!("File excerpt: {relative_path} · {lines} · {provenance}\n{fence}\n{body}\n{fence}")
}

/// Hand-off emitted by a file-editor host when its **Add selection to chat**
/// command activates. Everything the destination needs is captured
/// synchronously at activation; receivers revalidate buffer identity,
/// selection range, checkout, and destination key before appending so a
/// stale activation never rewrites a changed draft.
#[derive(Clone)]
pub struct EditorSelectionRequest {
    /// The buffer entity the selection was read from.
    pub editor: Entity<EditorState>,
    /// Checkout the file belongs to (`active_git_work_dir` at capture).
    pub checkout: PathBuf,
    /// Checkout-relative file path, for the excerpt label and identity checks.
    pub relative_path: String,
    /// Whether the buffer held unsaved edits when captured.
    pub dirty: bool,
    pub snapshot: EditorSelectionSnapshot,
    /// Composer destination key `(active_work_dir, active_session_id)`
    /// captured at activation.
    pub destination: (Option<PathBuf>, Option<String>),
}

/// Render-time state for the shared **Add selection to chat** control: the
/// enabled flag plus the textual reason when disabled.
pub struct AddSelectionControl {
    pub enabled: bool,
    pub reason: Option<SharedString>,
}

/// The quiet labeled button both file-editor hosts show beside file actions.
/// Its accessible name and tooltip explain that multiple cursors contribute
/// only the active selection, and carry the disabled reason when blocked.
pub fn editor_add_selection_button(
    id: impl Into<ElementId>,
    control: &AddSelectionControl,
) -> Button {
    let hint: SharedString = control
        .reason
        .clone()
        .unwrap_or_else(|| {
            "Append the selected code to the chat draft. Multiple cursors add only the active selection."
                .into()
        });
    Button::new(id)
        .debug_selector(|| "editor-add-selection".into())
        .ghost()
        .xsmall()
        .label("Add selection to chat")
        .accessibility_label(hint.clone())
        .tooltip(hint)
        .disabled(!control.enabled)
}

/// Compact header used by the Files and Review panels. Save/close guards belong to the host.
pub fn panel_document_header(
    mode: Option<AnyElement>,
    title: &str,
    dirty: bool,
    save_enabled: bool,
    language: Option<&str>,
    reviewing: bool,
    add_selection: Option<AddSelectionControl>,
    on_action: impl Fn(&PanelDocumentAction, &mut Window, &mut App) + 'static,
) -> Div {
    let has_mode = mode.is_some();
    let callback = std::rc::Rc::new(on_action);
    let request = move |action| {
        let callback = callback.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| callback(&action, window, cx)
    };
    let back = if reviewing {
        "Back to changed files"
    } else {
        "Back to project files"
    };
    let save = if dirty {
        "Save the open document"
    } else {
        "No unsaved changes"
    };
    div()
        .debug_selector(|| "panel-document-header".into())
        .min_h(rems(2.375))
        .when(has_mode, |header| header.flex_wrap().gap_1().py_1())
        .flex_none()
        .min_w_0()
        .px_2()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .min_w_0()
                .flex_1()
                .child(
                    Button::new("right-panel-document-back")
                        .debug_selector(|| "right-panel-document-back".into())
                        .accessibility_label(back)
                        .tooltip(back)
                        .icon(IconName::ArrowLeft)
                        .ghost()
                        .xsmall()
                        .on_click(request(PanelDocumentAction::Back)),
                )
                .child(IconName::File)
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .child(title.to_owned()),
                )
                .children(dirty.then(|| Tag::warning().child("modified").xsmall()))
                .children(language.map(|language| {
                    Tag::secondary()
                        .child(language.to_owned())
                        .outline()
                        .xsmall()
                })),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .when(has_mode, |actions| actions.flex_wrap().max_w_full())
                .children(mode)
                .children(add_selection.map(|control| {
                    editor_add_selection_button("panel-add-selection", &control)
                        .on_click(request(PanelDocumentAction::AddSelectionToChat))
                }))
                .children(language.map(|_| {
                    Button::new("save-document")
                        .debug_selector(|| "save-document".into())
                        .small()
                        .label("Save")
                        .icon(IconName::Check)
                        .accessibility_label(save)
                        .tooltip(save)
                        .disabled(!save_enabled)
                        .on_click(request(PanelDocumentAction::Save))
                }))
                .child(
                    Button::new("close-document")
                        .debug_selector(|| "close-document".into())
                        .accessibility_label("Close document")
                        .small()
                        .ghost()
                        .icon(IconName::Close)
                        .tooltip("Close document")
                        .on_click(request(PanelDocumentAction::Close)),
                ),
        )
}

pub fn review_whitespace_control(
    ignore: bool,
    on_change: impl Fn(&bool, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    div().px_3().py_2().flex().flex_col().gap_2()
        .child(Checkbox::new("review-ignore-whitespace")
            .debug_selector(|| "review-ignore-whitespace".into()).w_full().small().label("Ignore whitespace").checked(ignore)
            .accessibility_label("Ignore whitespace. Ignores whitespace when comparing lines. Whitespace can affect program behavior. File counts and commit selection are unchanged.")
            .tooltip("Ignores whitespace when comparing lines. Whitespace can affect program behavior. File counts and commit selection are unchanged.")
            .on_click(on_change))
        .children(ignore.then(|| div().text_xs().text_color(cx.theme().muted_foreground).child("Whitespace ignored · Display only")))
}

pub fn editor_surface(cx: &App) -> Div {
    div()
        .tab_group()
        .flex()
        .flex_col()
        .flex_1()
        .size_full()
        .min_w_0()
        .min_h_0()
        .bg(cx.theme().background)
}

pub fn editor_tab_bar(cx: &App) -> Div {
    div()
        .min_h(rems(2.125))
        .flex_wrap()
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .bg(cx.theme().muted.opacity(0.3))
        .border_b_1()
        .border_color(cx.theme().border)
        .px_2()
}

pub fn editor_tabs() -> Scrollable<Div> {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_1()
        .overflow_x_scrollbar()
}

/// A tab's selection and close controls are siblings, so closing cannot select another tab.
pub fn editor_tab(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    path: impl Into<SharedString>,
    selected: bool,
    dirty: bool,
    diff: bool,
    on_select: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let id = id.into();
    let label = label.into();
    let path = path.into();
    let hint = if diff {
        format!("Git Diff: {path}")
    } else {
        path.to_string()
    };
    let accessible = format!(
        "{hint}{}{}",
        if dirty && !diff {
            ", unsaved changes"
        } else {
            ""
        },
        if selected { ", selected" } else { "" }
    );
    let theme = cx.theme().colors;
    let foreground = if selected {
        theme.foreground
    } else {
        theme.muted_foreground
    };
    div()
        .id(id.clone())
        .role(Role::Group)
        .h(rems(1.625))
        .flex_none()
        .flex()
        .items_center()
        .gap_1p5()
        .px_2()
        .rounded_t_sm()
        .bg(if selected {
            theme.background
        } else {
            theme.background.opacity(0.0)
        })
        .border_1()
        .border_color(if selected {
            theme.border
        } else {
            theme.border.opacity(0.0)
        })
        .when(!selected, |tab| {
            tab.hover(|style| style.bg(theme.muted.opacity(0.5)))
        })
        .child(
            Button::new(SharedString::from(format!("{id}-select")))
                .debug_selector({
                    let id = id.clone();
                    move || format!("{id}-select").into()
                })
                .accessibility_label(accessible)
                .tooltip(hint)
                .ghost()
                .xsmall()
                .compact()
                .h_full()
                .px_0()
                .on_click(on_select)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(Icon::new(IconName::File).size_3().text_color(if diff {
                            theme.warning
                        } else {
                            foreground
                        }))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(if selected {
                                    FontWeight::MEDIUM
                                } else {
                                    FontWeight::NORMAL
                                })
                                .text_color(foreground)
                                .child(label),
                        )
                        .children(
                            (dirty && !diff)
                                .then(|| div().size(rems(0.375)).rounded_full().bg(theme.accent)),
                        ),
                ),
        )
        .child(
            Button::new(SharedString::from(format!("{id}-close")))
                .debug_selector(move || format!("{id}-close").into())
                .accessibility_label(format!("Close {path}"))
                .ghost()
                .xsmall()
                .icon(IconName::Close)
                .tooltip("Close tab")
                .on_click(on_close),
        )
}

#[derive(Clone, Copy)]
pub enum EditorTabAction {
    Close,
    CloseOthers,
    CloseAll,
}

pub fn editor_tab_menu(
    menu: PopupMenu,
    on_request: impl Fn(EditorTabAction, &mut Window, &mut App) + 'static,
) -> PopupMenu {
    let callback = std::rc::Rc::new(on_request);
    [
        ("Close Tab", EditorTabAction::Close),
        ("Close Other Tabs", EditorTabAction::CloseOthers),
        ("Close All Tabs", EditorTabAction::CloseAll),
    ]
    .into_iter()
    .fold(menu, |menu, (label, action)| {
        let callback = callback.clone();
        menu.item(
            PopupMenuItem::new(label).on_click(move |_, window, cx| callback(action, window, cx)),
        )
    })
}

pub fn editor_save_button(dirty: bool, diff: bool) -> Button {
    let hint = if diff {
        "Diff view (read-only)"
    } else {
        "Save file (Cmd+S)"
    };
    Button::new("editor-save-btn")
        .debug_selector(|| "editor-save-btn".into())
        .ghost()
        .xsmall()
        .icon(IconName::Check)
        .label(if diff { "Diff" } else { "Save" })
        .disabled(!dirty || diff)
        .accessibility_label(hint)
        .tooltip(hint)
}

pub fn editor_actions(
    mode: Option<AnyElement>,
    status: Option<(String, bool)>,
    reopen: Button,
    add_selection: Option<Button>,
    save: Button,
    cx: &App,
) -> Div {
    div()
        .flex_none()
        .flex_wrap()
        .max_w_full()
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .children(status.map(|(message, error)| {
            div()
                .id("editor-status")
                .role(Role::Status)
                .text_xs()
                .text_color(if error {
                    cx.theme().danger
                } else {
                    cx.theme().muted_foreground
                })
                .px_2()
                .child(message)
        }))
        .children(mode)
        .child(reopen)
        .children(add_selection)
        .child(save)
}

pub fn editor_buffer(editor: &Entity<EditorState>) -> Div {
    div()
        .flex_1()
        .min_h_0()
        .size_full()
        .child(Editor::new(editor).bordered(false).size_full())
}

pub fn editor_tab_title(path_str: &str, is_diff: bool) -> String {
    let clean_path = path_str.strip_prefix("diff:").unwrap_or(path_str);
    let path = std::path::Path::new(clean_path);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(clean_path);
    let parent_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str());

    let label = if let Some(parent) = parent_name {
        if !parent.is_empty() && parent != "." {
            format!("{parent}/{file_name}")
        } else {
            file_name.to_string()
        }
    } else {
        file_name.to_string()
    };

    if is_diff {
        format!("Diff · {label}")
    } else {
        label
    }
}


pub fn editor_diff(text: &Entity<TextViewState>, cx: &App) -> Scrollable<Div> {
    div()
        .flex_1()
        .min_h_0()
        .size_full()
        .p_4()
        .overflow_y_scrollbar()
        .child(crate::diff_text_view(text, cx))
}

/// `reopen` is `None` when there is nothing to reopen, so the empty state never shows a dead control.
pub fn editor_empty_state(reopen: Option<Button>, cx: &App) -> Div {
    // Single-line, width-independent text (no wrap, no ellipsis): with the terminal split open,
    // GPUI paints the measurement from a near-zero-width layout probe into the full-width box,
    // so wrapping text came out one letter per line and truncated text as a bare "…".
    div().flex_1().min_h_0().overflow_hidden().flex().flex_col().items_center().justify_center().gap_3().p_6()
        .child(div().flex_none().size_12().rounded_full().bg(cx.theme().muted.opacity(0.5))
            .flex().items_center().justify_center().text_2xl().text_color(cx.theme().muted_foreground)
            .child(Icon::new(IconName::File)))
        .child(div().flex_none().w_full().text_center().whitespace_nowrap().overflow_hidden()
            .text_sm().font_weight(FontWeight::MEDIUM).text_color(cx.theme().foreground)
            .child("No files open"))
        .child(div().flex_none().w_full().text_center().whitespace_nowrap().overflow_hidden()
            .text_xs().text_color(cx.theme().muted_foreground)
            .child("Open a file from the Files panel or Review."))
        .children(reopen.map(|reopen| div().flex_none().child(reopen)))
}
