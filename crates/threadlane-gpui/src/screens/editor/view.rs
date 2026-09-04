use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Editor, EditorState, InputEvent, TabSize};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::text::{TextView, TextViewState};
use gpui_component::{ActiveTheme, Disableable, IconName, Sizable};

use crate::state::AppState;

actions!(editor, [SaveFile]);

/// How long a save/open status message stays visible before auto-expiring.
const STATUS_MSG_TTL: std::time::Duration = std::time::Duration::from_secs(3);

fn detect_language(path_str: &str) -> &'static str {
    let path = Path::new(path_str);
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("ts" | "mts" | "cts" | "jsx" | "tsx") => "typescript",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("html" | "htm") => "html",
        Some("css") => "css",
        Some("md" | "markdown") => "markdown",
        Some("sh" | "bash" | "zsh") => "bash",
        Some("go") => "go",
        Some("c" | "h") => "c",
        Some("cpp" | "hpp" | "cc" | "cxx" | "hh") => "cpp",
        Some("diff" | "patch") => "diff",
        Some("zig") => "zig",
        _ => match path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|s| s.to_lowercase())
            .as_deref()
        {
            Some("dockerfile") => "bash",
            Some("cargo.lock") => "toml",
            _ => "text",
        },
    }
}

fn smart_tab_title(path_str: &str, is_diff: bool) -> String {
    let clean_path = path_str.strip_prefix("diff:").unwrap_or(path_str);
    let path = Path::new(clean_path);
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

pub struct EditorTab {
    project_dir: PathBuf,
    relative_path: String,
    file_name: String,
    _language: &'static str,
    saved_content: String,
    is_dirty: bool,
    is_diff: bool,
    editor_state: Option<Entity<EditorState>>,
    text_view_state: Option<Entity<TextViewState>>,
    _subscription: Option<Subscription>,
}

#[derive(Clone, Debug)]
enum PendingOpen {
    File { project: PathBuf, path: String },
    Diff { path: String, content: String },
}

pub struct EditorView {
    model: Entity<AppState>,
    tabs: Vec<EditorTab>,
    active_tab_index: Option<usize>,
    pending_open: Option<PendingOpen>,
    status_msg: Option<(String, bool, std::time::Instant)>,
    _subscriptions: Vec<Subscription>,
}

impl EditorView {
    pub(crate) fn new(model: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model_clone = model.clone();
        let sub = cx.observe(&model_clone, |_this, _model, cx| {
            cx.notify();
        });

        Self {
            model,
            tabs: Vec::new(),
            active_tab_index: None,
            pending_open: None,
            status_msg: None,
            _subscriptions: vec![sub],
        }
    }

    fn has_tabs(&self) -> bool {
        !self.tabs.is_empty()
    }

    pub(crate) fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    fn is_active_dirty(&self) -> bool {
        self.active_tab_index
            .and_then(|idx| self.tabs.get(idx))
            .map(|tab| tab.is_dirty && !tab.is_diff)
            .unwrap_or(false)
    }

    fn is_active_diff(&self) -> bool {
        self.active_tab_index
            .and_then(|idx| self.tabs.get(idx))
            .map(|tab| tab.is_diff)
            .unwrap_or(false)
    }

    fn sync_pending_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_open.take() else {
            return;
        };
        match pending {
            PendingOpen::File { project, path } => {
                self.open_file_internal(&project, &path, window, cx)
            }
            PendingOpen::Diff { path, content } => {
                self.open_diff_internal(&path, &content, window, cx)
            }
        }
    }

    pub(crate) fn open_file(&mut self, project: PathBuf, relative_path: &str, cx: &mut Context<Self>) {
        self.pending_open = Some(PendingOpen::File {
            project,
            path: relative_path.to_string(),
        });
        cx.notify();
    }

    pub(crate) fn open_diff(&mut self, relative_path: &str, content: &str, cx: &mut Context<Self>) {
        self.pending_open = Some(PendingOpen::Diff {
            path: relative_path.to_string(),
            content: content.to_string(),
        });
        cx.notify();
    }

    fn open_diff_internal(
        &mut self,
        relative_path: &str,
        content: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab_key = format!("diff:{relative_path}");
        let markdown = format!("```diff\n{}\n```", content.replace("```", "` ` `"));

        if let Some(existing_idx) = self.tabs.iter().position(|t| t.relative_path == tab_key) {
            if let Some(tab) = self.tabs.get_mut(existing_idx) {
                tab.saved_content = content.to_string();
                if let Some(ref text_view) = tab.text_view_state {
                    text_view.update(cx, |state, cx| {
                        state.set_text(&markdown, cx);
                    });
                }
            }
            self.active_tab_index = Some(existing_idx);
            cx.notify();
            return;
        }

        let markdown_state = cx.new(|cx| TextViewState::markdown(&markdown, cx));
        let tab_title = smart_tab_title(relative_path, true);

        self.tabs.push(EditorTab {
            project_dir: self
                .model
                .read(cx)
                .active_work_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from(".")),
            relative_path: tab_key,
            file_name: tab_title,
            _language: "diff",
            saved_content: content.to_string(),
            is_dirty: false,
            is_diff: true,
            editor_state: None,
            text_view_state: Some(markdown_state),
            _subscription: None,
        });

        self.active_tab_index = Some(self.tabs.len() - 1);
        self.status_msg = None;
        cx.notify();
    }

    fn open_file_internal(
        &mut self,
        project_dir: &Path,
        relative_path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // If already open, just select the tab
        if let Some(existing_idx) = self
            .tabs
            .iter()
            .position(|t| t.project_dir == project_dir && t.relative_path == relative_path)
        {
            self.active_tab_index = Some(existing_idx);
            cx.notify();
            return;
        }

        let full_path = project_dir.join(relative_path);
        let content = match std::fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(err) => {
                tracing::error!("Failed to open file {}: {}", full_path.display(), err);
                self.set_status(format!("Unable to open {}: {err}", relative_path), true);
                cx.notify();
                return;
            }
        };

        let lang = detect_language(relative_path);
        let content_for_sub = content.clone();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(lang)
                .line_number(true)
                .folding(true)
                .show_whitespaces(false)
                .tab_size(TabSize {
                    tab_size: 4,
                    hard_tabs: false,
                })
                .default_value(&content)
        });

        let target_path = relative_path.to_string();
        let target_project = project_dir.to_path_buf();
        let subscription = cx.subscribe(&editor, move |this, editor, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let current = editor.read(cx).value();
                if let Some(tab) = this
                    .tabs
                    .iter_mut()
                    .find(|t| t.project_dir == target_project && t.relative_path == target_path)
                {
                    let dirty = current.as_str() != tab.saved_content.as_str();
                    if tab.is_dirty != dirty {
                        tab.is_dirty = dirty;
                        cx.notify();
                    }
                }
            }
        });

        let tab_title = smart_tab_title(relative_path, false);

        self.tabs.push(EditorTab {
            project_dir: project_dir.to_path_buf(),
            relative_path: relative_path.to_string(),
            file_name: tab_title,
            _language: lang,
            saved_content: content_for_sub,
            is_dirty: false,
            is_diff: false,
            editor_state: Some(editor),
            text_view_state: None,
            _subscription: Some(subscription),
        });

        self.active_tab_index = Some(self.tabs.len() - 1);
        self.status_msg = None;
        cx.notify();
    }

    fn set_status(&mut self, msg: String, is_error: bool) {
        self.status_msg = Some((msg, is_error, std::time::Instant::now()));
    }

    fn visible_status(&self) -> Option<(String, bool)> {
        match &self.status_msg {
            Some((msg, is_error, at)) if at.elapsed() < STATUS_MSG_TTL => {
                Some((msg.clone(), *is_error))
            }
            _ => None,
        }
    }

    fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.active_tab_index = Some(index);
            self.status_msg = None;
            cx.notify();
        }
    }

    fn remove_tab_at(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.tabs.remove(index);
            if self.tabs.is_empty() {
                self.active_tab_index = None;
            } else if let Some(active) = self.active_tab_index {
                if active >= self.tabs.len() {
                    self.active_tab_index = Some(self.tabs.len() - 1);
                } else if active > index {
                    self.active_tab_index = Some(active - 1);
                }
            }
            self.status_msg = None;
            cx.notify();
        }
    }

    fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }

        let tab = &self.tabs[index];
        if tab.is_dirty && !tab.is_diff {
            let file_name = tab.file_name.clone();
            let target_path = tab.relative_path.clone();
            let target_project = tab.project_dir.clone();

            cx.spawn(async move |this, cx| {
                let result = rfd::AsyncMessageDialog::new()
                    .set_title("Discard unsaved changes?")
                    .set_description(format!(
                        "Do you want to discard unsaved changes to \"{file_name}\"?"
                    ))
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    .await;
                if matches!(result, rfd::MessageDialogResult::Yes) {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(pos) = this.tabs.iter().position(|t| {
                            t.project_dir == target_project && t.relative_path == target_path
                        }) {
                            this.remove_tab_at(pos, cx);
                        }
                    });
                }
            })
            .detach();
        } else {
            self.remove_tab_at(index, cx);
        }
    }

    fn close_other_tabs(&mut self, keep_index: usize, cx: &mut Context<Self>) {
        if keep_index >= self.tabs.len() {
            return;
        }

        let dirty_names: Vec<String> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(i, t)| *i != keep_index && t.is_dirty && !t.is_diff)
            .map(|(_, t)| t.file_name.clone())
            .collect();

        if dirty_names.is_empty() {
            let kept = self.tabs.remove(keep_index);
            self.tabs = vec![kept];
            self.active_tab_index = Some(0);
            self.status_msg = None;
            cx.notify();
        } else {
            let keep_path = self.tabs[keep_index].relative_path.clone();
            let keep_project = self.tabs[keep_index].project_dir.clone();
            let description = if dirty_names.len() == 1 {
                format!(
                    "Do you want to discard unsaved changes to \"{}\"?",
                    dirty_names[0]
                )
            } else {
                format!(
                    "Do you want to discard unsaved changes to {} files ({})?",
                    dirty_names.len(),
                    dirty_names.join(", ")
                )
            };

            cx.spawn(async move |this, cx| {
                let result = rfd::AsyncMessageDialog::new()
                    .set_title("Discard unsaved changes?")
                    .set_description(description)
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    .await;
                if matches!(result, rfd::MessageDialogResult::Yes) {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(pos) = this.tabs.iter().position(|t| {
                            t.project_dir == keep_project && t.relative_path == keep_path
                        }) {
                            let kept = this.tabs.remove(pos);
                            this.tabs = vec![kept];
                            this.active_tab_index = Some(0);
                            this.status_msg = None;
                            cx.notify();
                        }
                    });
                }
            })
            .detach();
        }
    }

    fn close_all_tabs(&mut self, cx: &mut Context<Self>) {
        let dirty_names: Vec<String> = self
            .tabs
            .iter()
            .filter(|t| t.is_dirty && !t.is_diff)
            .map(|t| t.file_name.clone())
            .collect();

        if dirty_names.is_empty() {
            self.tabs.clear();
            self.active_tab_index = None;
            self.status_msg = None;
            cx.notify();
        } else {
            let description = if dirty_names.len() == 1 {
                format!(
                    "Do you want to discard unsaved changes to \"{}\"?",
                    dirty_names[0]
                )
            } else {
                format!(
                    "Do you want to discard unsaved changes to {} files ({})?",
                    dirty_names.len(),
                    dirty_names.join(", ")
                )
            };

            cx.spawn(async move |this, cx| {
                let result = rfd::AsyncMessageDialog::new()
                    .set_title("Discard unsaved changes?")
                    .set_description(description)
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    .await;
                if matches!(result, rfd::MessageDialogResult::Yes) {
                    let _ = this.update(cx, |this, cx| {
                        this.tabs.clear();
                        this.active_tab_index = None;
                        this.status_msg = None;
                        cx.notify();
                    });
                }
            })
            .detach();
        }
    }

    fn save_active_file(&mut self, cx: &mut Context<Self>) {
        let Some(idx) = self.active_tab_index else {
            return;
        };
        self.save_tab_at(idx, cx);
    }

    fn save_file_action(&mut self, _: &SaveFile, _window: &mut Window, cx: &mut Context<Self>) {
        self.save_active_file(cx);
    }

    fn save_tab_at(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };

        if tab.is_diff {
            return;
        }

        let Some(ref editor) = tab.editor_state else {
            return;
        };

        let file_path = tab.project_dir.join(&tab.relative_path);
        let content = editor.read(cx).value().to_string();
        let file_name = tab.file_name.clone();

        match std::fs::write(&file_path, &content) {
            Ok(_) => {
                tab.saved_content = content;
                tab.is_dirty = false;
                self.set_status(format!("Saved {file_name}"), false);
                cx.notify();
            }
            Err(err) => {
                tracing::error!("Failed to save file {}: {}", file_path.display(), err);
                self.set_status(format!("Error saving {file_name}: {err}"), true);
                cx.notify();
            }
        }
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let is_active_dirty = self.is_active_dirty();
        let is_active_diff = self.is_active_diff();
        let view_entity = cx.entity().clone();

        div()
            .h(px(34.0))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .bg(theme.muted.opacity(0.3))
            .border_b_1()
            .border_color(theme.border)
            .px_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .overflow_x_scrollbar()
                    .children(self.tabs.iter().enumerate().map(|(idx, tab)| {
                        let is_selected = Some(idx) == self.active_tab_index;
                        let tab_bg = if is_selected {
                            theme.background
                        } else {
                            theme.background.opacity(0.0)
                        };

                        let text_color = if is_selected {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        };

                        let select_view = view_entity.clone();
                        let menu_view = view_entity.clone();
                        let close_view = view_entity.clone();

                        let raw_path = tab
                            .relative_path
                            .strip_prefix("diff:")
                            .unwrap_or(&tab.relative_path);
                        let tooltip_text = if tab.is_diff {
                            format!("Git Diff: {raw_path}")
                        } else {
                            raw_path.to_string()
                        };

                        div()
                            .id(SharedString::from(format!("editor-tab-{}", idx)))
                            .h(px(26.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .px_2()
                            .rounded_t_sm()
                            .bg(tab_bg)
                            .border_1()
                            .border_color(if is_selected {
                                theme.border
                            } else {
                                theme.border.opacity(0.0)
                            })
                            .when(!is_selected, |this| {
                                this.hover(|s| s.bg(theme.muted.opacity(0.5)))
                            })
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(tooltip_text.clone())
                                    .build(window, cx)
                            })
                            .on_click(move |_event, _window, cx| {
                                select_view.update(cx, |this, cx| this.select_tab(idx, cx));
                            })
                            .context_menu({
                                let keep_idx = idx;
                                move |menu, _window, _cx| {
                                    let v1 = menu_view.clone();
                                    let v2 = menu_view.clone();
                                    let v3 = menu_view.clone();
                                    menu.item(PopupMenuItem::new("Close Tab").on_click(
                                        move |_event, _window, cx| {
                                            v1.update(cx, |this, cx| this.close_tab(keep_idx, cx));
                                        },
                                    ))
                                    .item(PopupMenuItem::new("Close Other Tabs").on_click(
                                        move |_event, _window, cx| {
                                            v2.update(cx, |this, cx| {
                                                this.close_other_tabs(keep_idx, cx)
                                            });
                                        },
                                    ))
                                    .item(
                                        PopupMenuItem::new("Close All Tabs").on_click(
                                            move |_event, _window, cx| {
                                                v3.update(cx, |this, cx| this.close_all_tabs(cx));
                                            },
                                        ),
                                    )
                                }
                            })
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(if tab.is_diff {
                                        theme.warning
                                    } else {
                                        text_color
                                    })
                                    .child(IconName::File),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(if is_selected {
                                        FontWeight::MEDIUM
                                    } else {
                                        FontWeight::NORMAL
                                    })
                                    .text_color(text_color)
                                    .child(tab.file_name.clone()),
                            )
                            .child(if tab.is_dirty && !tab.is_diff {
                                div()
                                    .size(px(6.0))
                                    .rounded_full()
                                    .bg(theme.accent)
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            })
                            .child(
                                Button::new(SharedString::from(format!("tab-close-{}", idx)))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .on_click(move |_event, _window, cx| {
                                        close_view.update(cx, |this, cx| this.close_tab(idx, cx));
                                    }),
                            )
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .child(if let Some((msg, is_error)) = self.visible_status() {
                        div()
                            .text_size(px(11.0))
                            .text_color(if is_error {
                                theme.danger
                            } else {
                                theme.muted_foreground
                            })
                            .px_2()
                            .child(msg)
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    .child(
                        Button::new("editor-save-btn")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Check)
                            .label(if is_active_diff { "Diff" } else { "Save" })
                            .disabled(!is_active_dirty || is_active_diff)
                            .tooltip(if is_active_diff {
                                "Diff view (read-only)"
                            } else {
                                "Save file (Cmd+S)"
                            })
                            .on_click({
                                let save_view = view_entity.clone();
                                move |_event, _window, cx| {
                                    save_view.update(cx, |this, cx| this.save_active_file(cx));
                                }
                            }),
                    ),
            )
    }

    fn render_empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(
                div()
                    .size(px(48.0))
                    .rounded_full()
                    .bg(theme.muted.opacity(0.5))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(24.0))
                    .text_color(theme.muted_foreground)
                    .child(IconName::File),
            )
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("No files open in Editor"),
            )
            .child(
                div()
                    .max_w(px(380.0))
                    .text_center()
                    .text_size(px(12.0))
                    .text_color(theme.muted_foreground)
                    .child("Click a file in the Files panel or a changed file in Review to open and view here."),
            )
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_pending_file(window, cx);
        let theme = cx.theme().colors;

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .w_full()
            .min_w_0()
            .min_h_0()
            .bg(theme.background)
            .on_action(cx.listener(Self::save_file_action))
            .children(self.has_tabs().then(|| self.render_tab_bar(cx)))
            .child(if let Some(idx) = self.active_tab_index {
                if let Some(active_tab) = self.tabs.get(idx) {
                    if active_tab.is_diff {
                        if let Some(ref text_view) = active_tab.text_view_state {
                            div()
                                .flex_1()
                                .min_h_0()
                                .w_full()
                                .h_full()
                                .p_4()
                                .overflow_y_scrollbar()
                                .child(TextView::new(text_view).selectable(true))
                                .into_any_element()
                        } else {
                            self.render_empty_state(cx).into_any_element()
                        }
                    } else if let Some(ref editor) = active_tab.editor_state {
                        div()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .h_full()
                            .child(Editor::new(editor).bordered(false).size_full())
                            .into_any_element()
                    } else {
                        self.render_empty_state(cx).into_any_element()
                    }
                } else {
                    self.render_empty_state(cx).into_any_element()
                }
            } else {
                self.render_empty_state(cx).into_any_element()
            })
    }
}
