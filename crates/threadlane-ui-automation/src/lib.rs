//! Automation navigation, editor, and paged run history. Execution stays in the owning daemon.
mod editor;
use gpui::*;
use std::path::PathBuf;
use threadlane_automation::Definition;
use threadlane_protocol::automation::AutomationCommand as Command;
use threadlane_ui_state::{automation_io, AppState};

pub use threadlane_ui_kit::automation_form::SaveAutomation;
pub fn init(cx: &mut App) { threadlane_ui_kit::automation_form::init(cx); }

pub struct AutomationsView {
    model: Entity<AppState>,
    selected: Option<String>,
    expanded_prompt: Option<String>,
    scope: Option<PathBuf>,
    history: bool,
    attention_only: bool,
    page: usize,
    error: Option<String>,
    busy: bool,
    header_inset: Option<Pixels>,
    _subscription: Subscription,
}

/// A completed delete may reset navigation only while its definition remains selected.
fn deleted_selection_is_current(deleted_id: Option<&str>, selected: Option<&str>) -> bool {
    deleted_id.is_some() && deleted_id == selected
}

impl AutomationsView {
    pub fn new(model: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&model, |_, _, cx| cx.notify());
        Self {
            model,
            selected: None,
            expanded_prompt: None,
            scope: None,
            history: false,
            attention_only: false,
            page: 0,
            error: None,
            busy: false,
            header_inset: None,
            _subscription: subscription,
        }
    }

    /// Window-controls inset for the header while the sidebar is hidden.
    pub fn set_header_inset(&mut self, inset: Option<Pixels>, cx: &mut Context<Self>) {
        if self.header_inset != inset {
            self.header_inset = inset;
            cx.notify();
        }
    }
    /// Dispatch one daemon mutation, retaining selection on failure and showing its error.
    /// Store projections arrive through the model's authoritative watch or event stream.
    fn command(&mut self, command: Command, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let client = self.model.read(cx).daemon_client.clone();
        self.busy = true;
        self.error = None;
        let deleted_id = match &command {
            Command::Delete { id } => Some(id.clone()),
            _ => None,
        };
        let task = threadlane_provider::exec::get_runtime()
            .spawn(async move { automation_io::mutate(&client, command).await });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("Automation request failed: {error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                if result.is_ok()
                    && deleted_selection_is_current(
                        deleted_id.as_deref(),
                        this.selected.as_deref(),
                    )
                {
                    this.selected = None;
                    this.history = true;
                    this.page = 0;
                }
                this.error = result.err();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn edit(
        &mut self,
        definition: Option<Definition>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        editor::open(self.model.clone(), definition, window, cx);
    }
}
impl Render for AutomationsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use threadlane_ui_kit::automation::{
            automation_screen, AutomationAction, AutomationScreen,
        };
        let state = self.model.read(cx);
        let snapshot = state.automations.snapshot.clone();
        let screen = AutomationScreen {
            projects: state
                .projects
                .iter()
                .map(|p| (p.work_dir.to_string_lossy().into_owned(), p.name.clone()))
                .collect(),
            selected: self.selected.clone(),
            expanded_prompt: self.expanded_prompt.clone(),
            scope: self.scope.clone(),
            history: self.history,
            attention_only: self.attention_only,
            page: self.page,
            busy: self.busy,
            header_inset: self.header_inset,
            error: self
                .error
                .clone()
                .or_else(|| state.automations.error.clone()),
        };
        let owner = cx.entity().downgrade();
        automation_screen(
            &snapshot,
            &screen,
            move |action, window, cx| {
                let _ = owner.update(cx, |this, cx| {
                    match action {
                        AutomationAction::Scope(scope) => {
                            this.scope = scope;
                            this.selected = None;
                            this.page = 0;
                        }
                        AutomationAction::Select(id) => {
                            this.selected = Some(id);
                            this.page = 0;
                        }
                        AutomationAction::Back => {
                            this.selected = None;
                            this.page = 0;
                        }
                        AutomationAction::History(history) => {
                            this.history = history;
                            this.selected = None;
                            this.page = 0;
                        }
                        AutomationAction::AttentionOnly(attention) => {
                            this.attention_only = attention;
                            this.page = 0;
                        }
                        AutomationAction::ExpandPrompt(id) => this.expanded_prompt = id,
                        AutomationAction::Page(page) => this.page = page,
                        AutomationAction::Edit(id) => {
                            let editing_existing = id.is_some();
                            let definition = id.and_then(|id| {
                                this.model
                                    .read(cx)
                                    .automations
                                    .snapshot
                                    .definitions
                                    .iter()
                                    .find(|d| d.id == id)
                                    .cloned()
                            });
                            if editing_existing && definition.is_none() { return; }
                            this.edit(definition, window, cx);
                        }
                        AutomationAction::RunNow(id) => this.command(Command::RunNow { id }, cx),
                        AutomationAction::SetEnabled(id, enabled) => {
                            this.command(Command::SetEnabled { id, enabled }, cx)
                        }
                        AutomationAction::Delete(id) => this.command(Command::Delete { id }, cx),
                        AutomationAction::CancelRun(id) => this.command(Command::Cancel { id }, cx),
                        AutomationAction::ReviewRun(id) => this.command(Command::Review { id }, cx),
                        AutomationAction::DeleteRun(id) => this.command(Command::DeleteRun { id }, cx),
                        AutomationAction::OpenChat(id) => {
                            let result = this.model.update(cx, |state, cx| {
                                let result = state.open_automation_run(&id);
                                cx.notify();
                                result
                            });
                            match result {
                                Ok(()) => this.command(Command::Review { id }, cx),
                                Err(error) => this.error = Some(error),
                            }
                        }
                    }
                    cx.notify();
                });
            },
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, Modifiers, TestAppContext};
    use gpui_component::WindowExt;
    use threadlane_ui_state::{activate_test_session, AppState};

    /// Remote deletion must not undo a newer selection or a return to the list.
    #[test]
    fn delayed_delete_preserves_newer_navigation() {
        assert!(super::deleted_selection_is_current(Some("a"), Some("a")));
        assert!(!super::deleted_selection_is_current(Some("a"), Some("b")));
        assert!(!super::deleted_selection_is_current(Some("a"), None));
        assert!(!super::deleted_selection_is_current(None, Some("a")));
        assert!(!super::deleted_selection_is_current(None, None));
    }

    #[gpui::test]
    fn history_filters_attention_keeps_prompts_collapsed_and_retains_chat(cx: &mut TestAppContext) {
        use threadlane_automation::{Definition, Run, RunStatus, Schedule};
        cx.update(gpui_component::init);
        let temp = tempfile::tempdir().unwrap();
        let model = cx.new(|_| {
            let mut state = AppState::for_tests();
            activate_test_session(&mut state, "original", &temp.path().join("original.jsonl"));
            let definition = Definition {
                id: "research".into(),
                revision: 1,
                name: "Research across Threadlane projects and compare native desktop workflows"
                    .into(),
                prompt: "Research without edits. ".repeat(200),
                project: temp.path().into(),
                model: "model".into(),
                effort: "medium".into(),
                worktree: false,
                schedule: Schedule::Manual,
                enabled: false,
                notify_all: false,
                anchor: 0,
                next_at: None,
                failures: 0,
                paused_reason: None,
            };
            state.automations.snapshot.definitions = vec![definition.clone()];
            state.automations.snapshot.runs = [
                RunStatus::Failed,
                RunStatus::Running,
                RunStatus::Succeeded,
                RunStatus::Succeeded,
                RunStatus::WaitingAnswer,
                RunStatus::Cancelled,
                RunStatus::Failed,
            ]
            .into_iter()
            .enumerate()
            .map(|(i, status)| Run {
                id: i.to_string(),
                definition: definition.clone(),
                scheduled_for: None,
                created_at: 0,
                finished_at: None,
                status,
                session_id: format!("automation_{i}"),
                session_file: None,
                error: None,
                reviewed: i >= 3,
            })
            .collect();
            state
        });
        let shared = model.clone();
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                let mut view = super::AutomationsView::new(shared, cx);
                view.selected = Some("research".into());
                view.page = 9; // A removed last page must fall back to existing rows.
                view
            });
            *capture.borrow_mut() = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let view = captured.borrow_mut().take().expect("mounted automations");
        let scroll_history = |cx: &mut gpui::VisualTestContext, delta| {
            let area = cx
                .debug_bounds("automation-content")
                .expect("scrollable history");
            let viewport = cx.update(|window, _| window.viewport_size());
            cx.simulate_event(gpui::ScrollWheelEvent {
                // The selector covers the whole content, including the rows below the window.
                position: gpui::point(area.center().x, viewport.height / 2.0),
                delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(delta))),
                ..Default::default()
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
        };
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("automation-prompt-body").is_none());
        assert!(cx.debug_bounds("remove-run-0").is_some());
        assert!(cx.debug_bounds("remove-run-1").is_none());
        assert!(
            cx.debug_bounds("automation-prev").is_none(),
            "a single page needs no pagination"
        );
        let toggle = cx.debug_bounds("automation-prompt-toggle").unwrap();
        cx.simulate_click(toggle.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("automation-prompt-body").is_some());
        let filter = cx.debug_bounds("automation-attention").unwrap();
        cx.simulate_click(filter.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        for (selector, visible) in [
            ("automation-run-0", true),
            ("automation-run-1", false),
            ("automation-run-2", true),
            ("automation-run-3", false),
            ("automation-run-4", true),
            ("automation-run-5", false),
            ("automation-run-6", false),
        ] {
            assert_eq!(
                cx.debug_bounds(selector).is_some(),
                visible,
                "attention filtering must use the same contract as the sidebar badge: {selector}"
            );
        }
        let title = cx.debug_bounds("automation-run-title-0").unwrap();
        let context = cx.debug_bounds("automation-run-context-0").unwrap();
        assert!(
            context.top() >= title.bottom(),
            "run metadata must stay below long titles after filtering"
        );
        model.update(cx, |state, cx| {
            for run in &mut state.automations.snapshot.runs {
                run.reviewed = true;
                run.status = RunStatus::Succeeded;
            }
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds("automation-run-0").is_none());
        let all = cx
            .debug_bounds("automation-all-runs")
            .expect("empty attention view has a way out");
        cx.simulate_click(all.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("automation-run-0").is_some());
        model.update(cx, |state, cx| {
            let template = state.automations.snapshot.runs[0].clone();
            state.automations.snapshot.runs = (0..31)
                .map(|i| {
                    let mut run = template.clone();
                    run.id = i.to_string();
                    run
                })
                .collect();
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        scroll_history(cx, -10000.0);
        let next = cx
            .debug_bounds("automation-next")
            .expect("multiple pages expose navigation");
        let viewport_height = cx.update(|window, _| window.viewport_size().height);
        assert!(
            next.bottom() <= viewport_height,
            "pagination must be scrolled into view: {next:?}, height {viewport_height:?}"
        );
        cx.simulate_click(next.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(cx, |view, _| assert_eq!(view.page, 1));
        assert!(
            cx.debug_bounds("automation-run-0").is_some(),
            "last page remains reachable"
        );
        scroll_history(cx, 10000.0);
        let filter = cx.debug_bounds("automation-attention").unwrap();
        cx.simulate_click(filter.center(), Modifiers::default());
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.page, 0, "filters reset pagination")
        });
        view.update(cx, |view, cx| {
            view.selected = None;
            view.history = false;
            cx.notify();
        });
        for (width, rem_size) in [(480.0, 16.0), (800.0, 16.0), (480.0, 20.0)] {
            cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(900.0)));
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.set_rem_size(gpui::px(rem_size));
                window.draw(cx).clear(cx);
            });
            for selector in [
                "automation-history",
                "new-automation",
                "automation-row-research",
            ] {
                let bounds = cx
                    .debug_bounds(selector)
                    .expect("automation control is visible");
                assert!(
                    bounds.left() >= gpui::px(0.0) && bounds.right() <= gpui::px(width),
                    "{selector} overflows width {width}, rem {rem_size}: {bounds:?}"
                );
            }
            view.update(cx, |view, cx| {
                view.history = true;
                view.attention_only = false;
                view.page = 0;
                cx.notify();
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            let title = cx.debug_bounds("automation-run-title-30").unwrap();
            let context = cx.debug_bounds("automation-run-context-30").unwrap();
            assert!(title.right() <= gpui::px(width) && context.top() >= title.bottom(),
                "history must wrap long titles above their metadata at width {width}, rem {rem_size}");
            view.update(cx, |view, cx| {
                view.history = false;
                cx.notify();
            });
        }
        model.read_with(cx, |state, _| {
            assert_eq!(state.active_session_id.as_deref(), Some("original"))
        });
    }

    #[gpui::test]
    fn automation_sheet_supports_pointer_and_keyboard_without_switching_chat(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.update(super::init);
        cx.update(|cx| cx.set_reduce_motion(true));
        let temp = tempfile::tempdir().unwrap();
        let model = cx.new(|_| {
            let mut state = AppState::for_tests();
            activate_test_session(&mut state, "original", &temp.path().join("original.jsonl"));
            state
        });
        let original = model.read_with(cx, |state, _| {
            (
                state.active_session_id.clone(),
                state.active_work_dir.clone(),
            )
        });
        let shared = model.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| super::AutomationsView::new(shared, cx));
            gpui_component::Root::new(view, window, cx)
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let button = cx.debug_bounds("new-automation").unwrap();
        cx.simulate_click(button.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| {
            assert!(window.has_active_sheet(cx));
            window.draw(cx).clear(cx);
        });
        let scroll_form = |cx: &mut gpui::VisualTestContext| {
            let viewport = cx.update(|window, _| window.viewport_size());
            cx.simulate_event(gpui::ScrollWheelEvent {
                position: gpui::point(viewport.width - gpui::px(40.0), viewport.height / 2.0),
                delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(-10000.0))),
                ..Default::default()
            });
            cx.run_until_parked();
            cx.update(|window, cx| { window.refresh(); window.draw(cx).clear(cx); });
        };
        scroll_form(cx);
        let save = cx.debug_bounds("automation-editor-save").unwrap();
        let viewport = cx.update(|window, _| window.viewport_size());
        assert!(save.right() <= viewport.width && save.bottom() <= viewport.height, "Save must be visible before it is clicked");
        cx.simulate_click(save.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| {
            assert!(
                window.has_active_sheet(cx),
                "invalid fields must keep the editor open"
            )
        });
        scroll_form(cx);
        assert!(cx.debug_bounds("automation-editor-error").is_some(), "invalid fields must show validation feedback");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.update(|window, cx| assert!(!window.has_active_sheet(cx)));
        model.read_with(cx, |state, _| {
            assert_eq!(
                (
                    state.active_session_id.clone(),
                    state.active_work_dir.clone()
                ),
                original
            );
            assert!(state.automations.snapshot.definitions.is_empty());
        });
    }
}
