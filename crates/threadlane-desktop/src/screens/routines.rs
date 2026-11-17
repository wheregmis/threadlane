//! Routines (wireframe sidebar "Routines" and a project's "Routines" tab):
//! each routine and follow-up with what it does, when, who set it, and
//! Pause, Resume and Cancel; the tab also has "+ New routine".

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::ids::TimerId;
use threadlane_core::model::Weekday;
use threadlane_desktop::{Cadence, RoutineRow, When};

use super::{gap_line, heading, link, section};
use crate::{Desktop, Screen, Tab, TimerChange};

impl Desktop {
    /// A project's routines, then "+ New routine".
    pub(crate) fn routines_tab(
        &self,
        space: &str,
        project: &str,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut list = v_flex()
            .id("routines")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_3()
            .child(section("Routines and follow-ups"));
        let rows = self.state.routine_rows(space, project, when);
        match self.state.routines_gap(space, project) {
            Some(gap) => list = list.child(gap_line(&gap, "the routines", cx)),
            None if rows.is_empty() => {
                list = list.child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child("No routines yet. Add one, or an agent may set a follow-up."),
                );
            }
            None => {}
        }
        for row in rows {
            list = list.child(self.routine(space, project, row, cx));
        }
        list.child(self.routine_form(space, project, cx))
            .into_any_element()
    }

    /// Every space's routines, by space and project.
    pub(crate) fn routines_screen(&self, when: &When, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut screen = v_flex()
            .id("routines-all")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_3()
            .child(heading("Routines"));
        let groups = self.state.routine_groups(when);
        if groups.is_empty() {
            screen = screen.child(div().text_color(muted).child(
                "No routines or follow-ups in any project. Add one from a project's Routines tab.",
            ));
        }
        let mut last_space = None;
        for group in groups {
            if last_space.as_deref() != Some(group.space.as_str()) {
                screen = screen.child(heading(group.space.clone()).pt_2());
                last_space = Some(group.space.clone());
            }
            let open = Screen::Project {
                space: group.space.clone(),
                project: group.project.clone(),
                tab: Tab::Routines,
            };
            screen = screen.child(
                div().child(
                    link(
                        format!("routines-of-{}-{}", group.space, group.project),
                        group.project.clone(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.go(open.clone(), cx))),
                ),
            );
            match group.rows {
                Err(gap) => screen = screen.child(gap_line(&gap, "the routines", cx)),
                Ok(rows) => {
                    for row in rows {
                        screen = screen.child(self.routine(&group.space, &group.project, row, cx));
                    }
                }
            }
        }
        screen.into_any_element()
    }

    /// One routine: what, when, who, and its actions.
    fn routine(&self, space: &str, project: &str, row: RoutineRow, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let key = format!("{space}-{project}-{}", row.id);
        let id = row.id;
        let mut actions = h_flex().gap_2();
        if row.confirming {
            actions = actions
                .child(div().text_sm().child(format!(
                    "Cancel routine {id} for good? It cannot be resumed."
                )))
                .child(
                    Button::new(SharedString::from(format!("timer-cancel-confirm-{key}")))
                        .small()
                        .danger()
                        .label(format!("Cancel routine {id}"))
                        .on_click(on_timer(space, project, id, TimerChange::Cancel, cx)),
                )
                .child(
                    Button::new(SharedString::from(format!("timer-keep-{key}")))
                        .small()
                        .label("Keep it")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.keep_timer();
                            cx.notify();
                        })),
                );
        } else {
            let (label, change) = if row.paused {
                ("Resume", TimerChange::Resume)
            } else {
                ("Pause", TimerChange::Pause)
            };
            let in_space = space.to_owned();
            actions = actions
                .child(
                    Button::new(SharedString::from(format!("timer-{label}-{key}")))
                        .small()
                        .label(label)
                        .accessibility_label(format!("{label} routine {id}"))
                        .on_click(on_timer(space, project, id, change, cx)),
                )
                .child(
                    Button::new(SharedString::from(format!("timer-cancel-{key}")))
                        .small()
                        .ghost()
                        .label("Cancel")
                        .accessibility_label(format!("Cancel routine {id}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.ask_cancel_timer(&in_space, id);
                            cx.notify();
                        })),
                );
        }
        let when_line = match &row.zone {
            Some(zone) => format!("{} · {zone} · {}", row.cadence, row.next),
            None => format!("{} · {}", row.cadence, row.next),
        };
        v_flex()
            .gap_1()
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_sm().text_color(muted).child(format!("#{id}")))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .min_w_0()
                            .child(row.instruction),
                    ),
            )
            .child(div().text_sm().child(when_line))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(format!("{} · {} · {}", row.by, row.fired, row.status)),
            )
            .child(actions)
    }

    /// "+ New routine", or the form it opens.
    fn routine_form(&self, space: &str, project: &str, cx: &mut Context<Self>) -> AnyElement {
        let form = self.state.routine_form();
        if !form.open {
            return Button::new("new-routine")
                .label("+ New routine")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.routine_form_mut().open = true;
                    cx.notify();
                }))
                .into_any_element();
        }
        let muted = cx.theme().muted_foreground;
        let label = |text: &'static str| div().w(px(60.)).text_sm().text_color(muted).child(text);
        let cadence = ButtonGroup::new("routine-cadence")
            .outline()
            .compact()
            .children(Cadence::ALL.iter().map(|(cadence, name)| {
                Button::new(SharedString::from(format!("cadence-{name}")))
                    .label(*name)
                    .selected(form.cadence == *cadence)
            }))
            .on_click(cx.listener(|this, clicked: &Vec<usize>, _, cx| {
                if let Some((cadence, _)) = clicked.first().and_then(|at| Cadence::ALL.get(*at)) {
                    this.state.routine_form_mut().cadence = *cadence;
                    cx.notify();
                }
            }));
        let at = h_flex()
            .gap_2()
            .child(label("At"))
            .child(div().w(px(90.)).child(Input::new(&self.routine_at)));
        let detail = match form.cadence {
            Cadence::Every => h_flex()
                .gap_2()
                .child(label("Every"))
                .child(div().w(px(90.)).child(Input::new(&self.routine_every)))
                .child(
                    ButtonGroup::new("routine-unit")
                        .outline()
                        .compact()
                        .child(
                            Button::new("unit-minutes")
                                .label("minutes")
                                .selected(!form.hours),
                        )
                        .child(
                            Button::new("unit-hours")
                                .label("hours")
                                .selected(form.hours),
                        )
                        .on_click(cx.listener(|this, clicked: &Vec<usize>, _, cx| {
                            this.state.routine_form_mut().hours = clicked.first() == Some(&1);
                            cx.notify();
                        })),
                ),
            Cadence::Daily => at,
            Cadence::Weekdays => {
                let days = Weekday::ALL.iter().map(|day| {
                    let day = *day;
                    let name = day.as_str();
                    let shown = format!("{}{}", name[..1].to_uppercase(), &name[1..]);
                    Button::new(SharedString::from(format!("day-{name}")))
                        .small()
                        .label(shown)
                        .selected(form.days.contains(&day))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.routine_form_mut().toggle_day(day);
                            cx.notify();
                        }))
                });
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(label("On"))
                    .children(days)
                    .child(at)
            }
            Cadence::Monthly => h_flex()
                .gap_2()
                .child(label("Day"))
                .child(div().w(px(60.)).child(Input::new(&self.routine_day)))
                .child(at),
        };
        let (in_space, named) = (space.to_owned(), project.to_owned());
        v_flex()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .child(section("New routine"))
            .child(Input::new(&self.routine_instruction))
            .child(cadence)
            .child(detail)
            .child(
                h_flex()
                    .gap_2()
                    .child(label("Zone"))
                    .child(div().w(px(260.)).child(Input::new(&self.routine_zone))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("It is yours: what it starts counts as work you requested."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("routine-add")
                            .primary()
                            .label("Add routine")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_routine(in_space.clone(), named.clone(), cx);
                            })),
                    )
                    .child(
                        Button::new("routine-close")
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.routine_form_mut().open = false;
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }
}

/// A click that pauses, resumes or cancels timer `id`.
fn on_timer(
    space: &str,
    project: &str,
    id: TimerId,
    change: TimerChange,
    cx: &mut Context<Desktop>,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let (space, project) = (space.to_owned(), project.to_owned());
    cx.listener(move |this, _: &ClickEvent, _, cx| {
        this.change_timer(space.clone(), project.clone(), id, change, cx);
    })
}
