//! Local host for the production automation form; saves only to the sample snapshot.
use gpui::*;
use gpui_component::WindowExt;
use gpui_component::input::{InputEvent, InputState, TextareaState};
use std::rc::Rc;
use threadlane_automation::{Definition, Schedule};
use threadlane_ui_kit::automation_form::{
    AutomationForm, AutomationFormAction, automation_form, automation_schedule_fields,
    automation_schedule_preview, schedule_from_fields,
};

pub struct AutomationSampleEditor {
    definition: Definition,
    projects: Vec<(String, String)>,
    name: Entity<InputState>,
    prompt: Entity<TextareaState>,
    interval: Entity<InputState>,
    time: Entity<InputState>,
    timezone: Entity<InputState>,
    cadence: String,
    weekday: u32,
    error: Option<String>,
    on_save: Rc<dyn Fn(Definition, &mut App)>,
    _subscriptions: Vec<Subscription>,
}

impl AutomationSampleEditor {
    pub fn new(
        definition: Definition,
        projects: Vec<(String, String)>,
        on_save: impl Fn(Definition, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (cadence, interval, time, timezone, weekday) =
            automation_schedule_fields(&definition.schedule);
        let name = cx.new(|cx| InputState::new(window, cx).default_value(&definition.name));
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .default_value(&definition.prompt)
                .auto_grow(4, 10)
                .soft_wrap(true)
        });
        let interval = cx.new(|cx| InputState::new(window, cx).default_value(interval));
        let time = cx.new(|cx| InputState::new(window, cx).default_value(time));
        let timezone = cx.new(|cx| InputState::new(window, cx).default_value(timezone));
        let mut subscriptions = [&name, &interval, &time, &timezone]
            .iter()
            .map(|input| cx.observe(*input, |_: &mut Self, _, cx| cx.notify()))
            .collect::<Vec<_>>();
        subscriptions.push(cx.observe(&prompt, |_: &mut Self, _, cx| cx.notify()));
        subscriptions.push(cx.subscribe_in(
            &name,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save(window, cx);
                }
            },
        ));
        Self {
            definition,
            projects,
            name,
            prompt,
            interval,
            time,
            timezone,
            cadence: cadence.into(),
            weekday,
            error: None,
            on_save: Rc::new(on_save),
            _subscriptions: subscriptions,
        }
    }

    fn schedule(&self, cx: &App) -> Result<Schedule, String> {
        schedule_from_fields(
            &self.cadence,
            &self.interval.read(cx).value(),
            &self.time.read(cx).value(),
            &self.timezone.read(cx).value(),
            self.weekday,
            &self.definition.schedule,
        )
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut definition = self.definition.clone();
        definition.name = self.name.read(cx).value().trim().into();
        definition.prompt = self.prompt.read(cx).value().trim().into();
        let result = self.schedule(cx).and_then(|schedule| {
            definition.schedule = schedule;
            definition.validate()
        });
        if let Err(error) = result {
            self.error = Some(error);
            cx.notify();
            return;
        }
        (self.on_save)(definition, cx);
        window.close_sheet(cx);
    }

    fn apply(&mut self, action: AutomationFormAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            AutomationFormAction::Starter(starter) => {
                if threadlane_ui_kit::automation_form::can_apply_starter(
                    self.definition.revision,
                    &self.name.read(cx).value(),
                    &self.prompt.read(cx).value(),
                ) {
                    self.name
                        .update(cx, |input, cx| input.set_value(starter.title(), window, cx));
                    self.prompt.update(cx, |input, cx| {
                        input.set_value(starter.prompt(), window, cx)
                    });
                }
            }
            AutomationFormAction::Project(id) => self.definition.project = id.into(),
            AutomationFormAction::Model(id) => self.definition.model = id,
            AutomationFormAction::Effort(id) => self.definition.effort = id,
            AutomationFormAction::Cadence(id) => self.cadence = id,
            AutomationFormAction::Weekday(day) => self.weekday = day,
            AutomationFormAction::Worktree(worktree) => self.definition.worktree = worktree,
            AutomationFormAction::NotifyAll(notify) => self.definition.notify_all = notify,
            AutomationFormAction::Save => self.save(window, cx),
            AutomationFormAction::Cancel => window.close_sheet(cx),
        }
        cx.notify();
    }
}

impl Render for AutomationSampleEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form = AutomationForm {
            definition: &self.definition,
            name: &self.name,
            prompt: &self.prompt,
            interval: &self.interval,
            time: &self.time,
            timezone: &self.timezone,
            cadence: &self.cadence,
            weekday: self.weekday,
            projects: self.projects.clone(),
            models: vec![("Sample model".into(), "Sample model".into())],
            model_label: self.definition.model.clone(),
            efforts: ["low", "medium", "high"]
                .map(|label| (label.into(), label.into()))
                .into(),
            show_effort: true,
            is_git: true,
            busy: false,
            error: self.error.clone(),
            schedule_preview: automation_schedule_preview(self.schedule(cx), 1_791_000_000),
        };
        let owner = cx.entity().downgrade();
        automation_form(
            form,
            move |action, window, cx| {
                let _ = owner.update(cx, |this, cx| this.apply(action, window, cx));
            },
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, Focusable, Modifiers, TestAppContext};
    use gpui_component::WindowExt;
    use threadlane_automation::{Definition, Schedule};
    use threadlane_ui_kit::automation_form::automation_editor_sheet;

    #[gpui::test]
    fn shared_form_validates_saves_custom_schedule_and_scopes_keyboard(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(threadlane_ui_kit::automation_form::init);
        // This test isolates form behavior; entrance motion is observed in the real preview.
        cx.update(|cx| cx.set_reduce_motion(true));
        let definition = Definition {
            id: "sample-custom".into(),
            revision: 1,
            name: String::new(),
            prompt: "Sample prompt".into(),
            project: "/sample-project".into(),
            model: "Sample model".into(),
            effort: "medium".into(),
            worktree: false,
            schedule: Schedule::Calendar {
                hour: 9,
                minute: 0,
                days: vec![4, 2, 0],
                timezone: "UTC".into(),
            },
            enabled: true,
            notify_all: false,
            anchor: 1_791_000_000,
            next_at: None,
            failures: 0,
            paused_reason: None,
        };
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let saved = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let saves = saved.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let editor = cx.new(|cx| {
                super::AutomationSampleEditor::new(
                    definition,
                    vec![("/sample-project".into(), "Sample project".into())],
                    move |definition, _| saves.borrow_mut().push(definition),
                    window,
                    cx,
                )
            });
            *capture.borrow_mut() = Some(editor);
            let host = cx.new(|cx| {
                crate::automation::AutomationPreview::new("/sample-project".into(), window, cx)
            });
            gpui_component::Root::new(host, window, cx)
        });
        let editor = captured.borrow_mut().take().unwrap();
        let sheet_editor = editor.clone();
        cx.update(|window, cx| {
            window.open_sheet(cx, move |sheet, _, _| {
                automation_editor_sheet(sheet, true, sheet_editor.clone())
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let scroll_form = |cx: &mut gpui::VisualTestContext, delta| {
            let viewport = cx.update(|window, _| window.viewport_size());
            cx.simulate_event(gpui::ScrollWheelEvent {
                position: gpui::point(viewport.width - gpui::px(40.0), viewport.height / 2.0),
                delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(delta))),
                ..Default::default()
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
        };
        for width in [480.0, 800.0] {
            cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(900.0)));
            cx.run_until_parked();
            scroll_form(cx, -10000.0);
            let form = cx.debug_bounds("automation-editor-form").unwrap();
            let save = cx.debug_bounds("automation-editor-save").unwrap();
            assert!(
                form.left() >= gpui::px(0.0) && form.right() <= gpui::px(width),
                "form must fit the sheet at {width}px: {form:?}"
            );
            assert!(
                save.left() >= gpui::px(0.0)
                    && save.right() <= gpui::px(width)
                    && save.bottom() <= gpui::px(900.0),
                "Save must remain reachable at {width}px: {save:?}"
            );
        }
        let save = cx.debug_bounds("automation-editor-save").unwrap();
        let viewport = cx.update(|window, _| window.viewport_size());
        assert!(
            save.right() <= viewport.width && save.bottom() <= viewport.height,
            "save must be scrolled into the viewport: {save:?}, {viewport:?}"
        );
        cx.simulate_click(save.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| {
            assert!(window.has_active_sheet(cx));
            window.draw(cx).clear(cx);
        });
        assert!(saved.borrow().is_empty());
        scroll_form(cx, -10000.0);
        assert!(cx.debug_bounds("automation-editor-error").is_some());
        scroll_form(cx, 10000.0);
        cx.update(|window, cx| window.focus(&editor.read(cx).name.read(cx).focus_handle(cx), cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_input("Custom schedule sample");
        cx.update(|window, cx| window.focus(&editor.read(cx).time.read(cx).focus_handle(cx), cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("10:30");
        cx.update(|window, cx| window.focus(&editor.read(cx).prompt.read(cx).focus_handle(cx), cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        cx.update(|window, cx| assert!(!window.has_active_sheet(cx)));
        assert_eq!(saved.borrow().len(), 1);
        assert_eq!(saved.borrow()[0].name, "Custom schedule sample");
        assert_eq!(
            saved.borrow()[0].schedule,
            Schedule::Calendar {
                hour: 10,
                minute: 30,
                days: vec![4, 2, 0],
                timezone: "UTC".into()
            }
        );
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        assert_eq!(
            saved.borrow().len(),
            1,
            "save binding must stay scoped to the open editor"
        );
        let sheet_editor = editor.clone();
        cx.update(|window, cx| {
            window.open_sheet(cx, move |sheet, _, _| {
                automation_editor_sheet(sheet, true, sheet_editor.clone())
            })
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.update(|window, cx| assert!(!window.has_active_sheet(cx)));
        assert_eq!(saved.borrow().len(), 1, "cancel must not save");
    }
}
