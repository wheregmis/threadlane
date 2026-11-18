//! Sample buffers; all presentation comes from the production UI kit.
use gpui::{prelude::*, *};
use gpui_component::{ActiveTheme, WindowExt};
use gpui_component::button::ButtonVariant;
use gpui_component::dialog::DialogButtonProps;
use gpui_component::input::{EditorState, InputEvent, TabSize};
use gpui_component::menu::ContextMenuExt;
use gpui_component::scroll::ScrollableElement;
use gpui_component::text::TextViewState;
use std::collections::HashMap;
use threadlane_ui_kit as kit;

actions!(ui_kit_editor_preview, [SaveSampleBuffer]);

pub(crate) const FILE: &str = "example.rs";
const DIFF: &str = "diff:example.rs";
const SAMPLE: &str = "// UI kit sample — edits stay in this preview.\nfn main() {\n    println!(\"Hello, Threadlane!\");\n}\n";

const SAMPLE_DIFF: &str = "--- a/example.rs\n+++ b/example.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    println!(\"Hello!\");\n+    println!(\"Hello, Threadlane!\");\n }";

pub struct EditorPreview {
    buffer: Entity<EditorState>,
    workbench: Entity<kit::EditorWorkbench>,
    diffs: HashMap<String, Entity<TextViewState>>,
    saved: String,
    tabs: Vec<String>,
    selected: Option<String>,
    status: Option<String>,
    closed_sample: bool,
    focus_handle: FocusHandle,
    /// Text appended by the sample **Add selection to chat** control.
    draft: String,
    _subscription: Subscription,
}

impl EditorPreview {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.bind_keys([
            KeyBinding::new("cmd-s", SaveSampleBuffer, Some("EditorPreview")),
            KeyBinding::new("ctrl-s", SaveSampleBuffer, Some("EditorPreview")),
        ]);
        let buffer = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("rust")
                .line_number(true)
                .folding(true)
                .show_whitespaces(false)
                .tab_size(TabSize {
                    tab_size: 4,
                    hard_tabs: false,
                })
                .default_value(SAMPLE)
        });
        let workbench = cx.new(|cx| kit::EditorWorkbench::new(buffer.clone(), "rust", window, cx));
        let subscription = cx.subscribe(&buffer, |_, _, _: &InputEvent, cx| cx.notify());
        let markdown = format!("```diff\n{SAMPLE_DIFF}\n```");
        let diff = cx.new(|cx| TextViewState::markdown(&markdown, cx));
        Self {
            buffer,
            workbench,
            diffs: HashMap::from([(DIFF.into(), diff)]),
            saved: SAMPLE.into(),
            tabs: vec![FILE.into(), DIFF.into()],
            selected: Some(FILE.into()),
            status: None,
            closed_sample: false,
            focus_handle: cx.focus_handle(),
            draft: String::new(),
            _subscription: subscription,
        }
    }

    fn dirty(&self, cx: &App) -> bool {
        self.buffer.read(cx).value().as_str() != self.saved
    }

    pub fn open_sample(&mut self, cx: &mut Context<Self>) {
        if !self.tabs.iter().any(|id| id == FILE) {
            self.tabs.push(FILE.into());
        }
        self.selected = Some(FILE.into());
        cx.notify();
    }

    pub fn open_diff(&mut self, title: String, content: String, cx: &mut Context<Self>) {
        let id = format!("diff:{title}");
        let markdown = format!("```diff\n{}\n```", content.replace("```", "` ` `"));
        let state = cx.new(|cx| TextViewState::markdown(&markdown, cx));
        self.diffs.insert(id.clone(), state);
        if !self.tabs.contains(&id) {
            self.tabs.push(id.clone());
        }
        self.selected = Some(id);
        self.status = None;
        cx.notify();
    }

    fn reopen_control(&self) -> kit::ReopenClosedFileControl {
        kit::ReopenClosedFileControl::default()
            .with_target(self.closed_sample.then(|| format!("Sample checkout / {FILE}")))
    }

    fn reopen_button(&self) -> gpui_component::button::Button {
        let focus = self.focus_handle.clone();
        kit::editor_reopen_button(&self.reopen_control())
            .on_click(move |_, window, cx| {
                window.focus(&focus, cx);
                window.dispatch_action(Box::new(kit::ReopenClosedFile), cx);
            })
    }

    fn reopen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.closed_sample || window.has_active_dialog(cx) || window.has_active_sheet(cx) {
            return;
        }
        if !self.tabs.iter().any(|id| id == FILE) {
            self.buffer.update(cx, |buffer, cx| buffer.set_value(self.saved.clone(), window, cx));
        }
        self.open_sample(cx);
        self.closed_sample = false;
        self.buffer.update(cx, |buffer, cx| buffer.focus(window, cx));
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.selected.as_deref() == Some(FILE) && self.dirty(cx) {
            self.saved = self.buffer.read(cx).value().to_string();
            self.status = Some("Saved in this preview".into());
            cx.notify();
        }
    }

    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer
            .update(cx, |buffer, cx| buffer.set_value(SAMPLE, window, cx));
        self.saved = SAMPLE.into();
        self.diffs.clear();
        self.open_diff(FILE.into(), SAMPLE_DIFF.into(), cx);
        self.tabs = vec![FILE.into(), DIFF.into()];
        self.selected = Some(FILE.into());
        self.status = None;
        self.closed_sample = false;
        cx.notify();
    }

    fn close(
        &mut self,
        tab: &str,
        action: kit::EditorTabAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets: Vec<_> = self
            .tabs
            .iter()
            .filter(|id| match action {
                kit::EditorTabAction::Close => id.as_str() == tab,
                kit::EditorTabAction::CloseOthers => id.as_str() != tab,
                kit::EditorTabAction::CloseAll => true,
            })
            .cloned()
            .collect();
        if targets.iter().any(|id| id == FILE) && self.dirty(cx) {
            let entity = cx.entity().downgrade();
            let buffer = self.buffer.read(cx).value();
            window.open_alert_dialog(cx, move |dialog, _, _| {
                let targets = targets.clone();
                let entity = entity.clone();
                let buffer = buffer.clone();
                dialog
                    .title("Discard sample edits?")
                    .description("Close example.rs and discard its unsaved edits in this preview.")
                    .button_props(
                        DialogButtonProps::default()
                            .ok_text("Discard")
                            .ok_variant(ButtonVariant::Danger)
                            .show_cancel(true),
                    )
                    .on_ok(move |_, window, cx| {
                        let _ = entity.update(cx, |host, cx| {
                            // Keep any edits made after this confirmation opened.
                            if host.buffer.read(cx).value() == buffer {
                                host.remove(&targets, window, cx);
                            }
                        });
                        true
                    })
            });
        } else {
            self.remove(&targets, window, cx);
        }
    }

    fn remove(&mut self, targets: &[String], window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.iter().any(|id| id == FILE && targets.contains(id)) {
            self.closed_sample = true;
        }
        self.tabs.retain(|id| !targets.contains(id));
        if self
            .selected
            .as_ref()
            .is_some_and(|id| targets.contains(id))
        {
            self.selected = self.tabs.last().cloned();
        }
        self.diffs.retain(|id, _| !targets.contains(id));
        self.status = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }
}

impl Render for EditorPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dirty = self.dirty(cx);
        let selected_diff = self
            .selected
            .as_ref()
            .is_some_and(|id| self.diffs.contains_key(id));
        kit::editor_surface(cx)
            .id("sample-central-editor")
            .role(Role::Group)
            .track_focus(&self.focus_handle)
            .key_context("CentralEditor EditorPreview")
            .on_action(cx.listener(|host, _: &kit::ReopenClosedFile, window, cx| host.reopen(window, cx)))
            .on_action(cx.listener(|host, _: &SaveSampleBuffer, _, cx| host.save(cx)))
            .children((!self.tabs.is_empty()).then(|| {
                kit::editor_tab_bar(cx)
                    .child(kit::editor_tabs().children(self.tabs.iter().map(|id| {
                        let select = cx.entity().downgrade();
                        let close = cx.entity().downgrade();
                        let menu = cx.entity().downgrade();
                        let selected_id = id.clone();
                        let closed_id = id.clone();
                        let menu_id = id.clone();
                        let diff = self.diffs.contains_key(id);
                        kit::editor_tab(
                            format!("sample-editor-{id}"),
                            kit::editor_tab_title(id, diff),
                            id.strip_prefix("diff:").unwrap_or(id).to_string(),
                            self.selected.as_ref() == Some(id),
                            dirty && id == FILE,
                            diff,
                            move |_, _, cx| {
                                let _ = select.update(cx, |host, cx| {
                                    host.selected = Some(selected_id.clone());
                                    cx.notify();
                                });
                            },
                            move |_, window, cx| {
                                let _ = close.update(cx, |host, cx| {
                                    host.close(&closed_id, kit::EditorTabAction::Close, window, cx)
                                });
                            },
                            cx,
                        )
                        .context_menu(move |popup, _, _| {
                            let menu = menu.clone();
                            let id = menu_id.clone();
                            kit::editor_tab_menu(popup, move |action, window, cx| {
                                let _ =
                                    menu.update(cx, |host, cx| host.close(&id, action, window, cx));
                            })
                        })
                    })))
                    .child(kit::editor_actions(
                        None,
                        self.status.clone().map(|message| (message, false)),
                        self.reopen_button(),
                        (self.selected.as_deref() == Some(FILE)).then(|| {
                            let reason = kit::editor_excerpt_block_reason(
                                kit::editor_selection_snapshot(self.buffer.read(cx)).as_ref(),
                            );
                            kit::editor_add_selection_button(
                                "preview-add-selection",
                                &kit::AddSelectionControl {
                                    enabled: reason.is_none(),
                                    reason: reason.map(SharedString::from),
                                },
                            )
                            .on_click(cx.listener(|host, _, window, cx| {
                                let Some(snapshot) =
                                    kit::editor_selection_snapshot(host.buffer.read(cx))
                                else {
                                    window.push_notification("Select code in the file first", cx);
                                    return;
                                };
                                let dirty = host.dirty(cx);
                                host.draft.push_str(&kit::format_editor_excerpt(
                                    FILE, &snapshot, dirty,
                                ));
                                host.draft.push('\n');
                                host.status = Some("Added to the sample draft below".into());
                                cx.notify();
                            }))
                        }),
                        kit::editor_save_button(
                            dirty && self.selected.as_deref() == Some(FILE),
                            selected_diff,
                        )
                        .on_click(cx.listener(|host, _, _, cx| host.save(cx))),
                        cx,
                    ))
            }))
            .child(if self.selected.as_deref() == Some(FILE) {
                self.workbench.clone().into_any_element()
            } else if let Some(diff) = self.selected.as_ref().and_then(|id| self.diffs.get(id)) {
                kit::editor_diff(diff, cx).into_any_element()
            } else {
                kit::editor_empty_state(
                    self.reopen_control().is_available().then(|| self.reopen_button()),
                    cx,
                ).into_any_element()
            })
            .when(!self.draft.is_empty(), |el| {
                el.child(
                    div()
                        .flex_none()
                        .max_h(rems(10.0))
                        .overflow_y_scrollbar()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .whitespace_normal()
                        .child(self.draft.clone()),
                )
            })
    }
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;
