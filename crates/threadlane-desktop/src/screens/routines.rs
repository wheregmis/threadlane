//! Routines (wireframe sidebar "Routines" and a project's "Routines" tab):
//! each routine and follow-up with what it does, when, who set it, and
//! Pause, Resume and Cancel; the tab also has "+ New routine".

use gpui_kit::component::button::{ButtonGroup, ButtonVariants as _};
use gpui_kit::component::group_box::GroupBox;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::ids::TimerId;
use threadlane_core::model::Weekday;
use threadlane_desktop::{Cadence, Consequence, EmptyRegion, RoutineRow, When};

use super::{empty, gap_line, link};
use crate::components::{button, card, field, heading, meta, section, status_badge};
use crate::icons::AppIcon;
use crate::tokens::{
    INLINE_LABEL_WIDTH, MD, SHORT_FIELD_WIDTH, SM, Status, TYPE_BODY, TYPE_META, XL, ZONE_WIDTH,
};
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
        let mut list = v_flex()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .id("routines")
            .gap(MD)
            .child(section("Routines and follow-ups"));
        let rows = self.state.routine_rows(space, project, when);
        match self.state.routines_gap(space, project) {
            Some(gap) => list = list.child(gap_line(&gap, "routines", space, cx)),
            None if rows.is_empty() => {
                list = list.child(empty(EmptyRegion::Routines, cx));
            }
            None => {}
        }
        for row in rows {
            list = list.child(self.routine(space, project, row, cx));
        }
        list.children(
            (!self.state.is_archived(space, project))
                .then(|| self.routine_form(space, project, cx)),
        )
        .into_any_element()
    }

    /// Every space's routines, by space and project.
    pub(crate) fn routines_screen(&self, when: &When, cx: &mut Context<Self>) -> AnyElement {
        let mut screen = v_flex()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .id("routines-all")
            .gap(XL)
            .child(heading("Routines"));
        let groups = self.state.routine_groups(when);
        let gaps = self.state.routines_space_gaps();
        if groups.is_empty() && gaps.is_empty() {
            screen = screen.child(empty(EmptyRegion::AllRoutines, cx));
        }
        for (space, gap) in gaps {
            screen = screen.child(gap_line(&gap, "projects with routines", space, cx));
        }
        let mut last_space = None;
        for group in groups {
            if last_space.as_deref() != Some(group.space.as_str()) {
                screen = screen.child(section(group.space.clone()).pt(SM));
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
                Err(gap) => screen = screen.child(gap_line(&gap, "routines", &group.space, cx)),
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
    fn routine(
        &self,
        space: &str,
        project: &str,
        row: RoutineRow,
        cx: &mut Context<Self>,
    ) -> GroupBox {
        let muted = cx.theme().muted_foreground;
        let key = format!("{space}-{project}-{}", row.id);
        let id = row.id;
        let (label, change) = if row.paused {
            ("Resume", TimerChange::Resume)
        } else {
            ("Pause", TimerChange::Pause)
        };
        let (in_space, named) = (space.to_owned(), project.to_owned());
        let actions = h_flex()
            .gap(SM)
            .child(
                button(SharedString::from(format!("timer-{label}-{key}")))
                    .small()
                    .icon(if row.paused {
                        AppIcon::Play
                    } else {
                        AppIcon::Pause
                    })
                    .label(label)
                    .accessibility_label(format!("{label} routine {id}"))
                    .on_click(on_timer(space, project, id, change, cx)),
            )
            .child(
                button(SharedString::from(format!("timer-cancel-{key}")))
                    .small()
                    .ghost()
                    .icon(AppIcon::X)
                    .label("Cancel…")
                    .accessibility_label(format!("Cancel routine {id}"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_action(
                            Consequence::CancelRoutine(in_space.clone(), named.clone(), id),
                            window,
                            cx,
                        );
                    })),
            );
        let when_line = match &row.zone {
            Some(zone) => format!("{} · {zone} · {}", row.cadence, row.next),
            None => format!("{} · {}", row.cadence, row.next),
        };
        card(cx)
            .child(
                h_flex()
                    .gap(SM)
                    .child(
                        div()
                            .text_size(TYPE_BODY)
                            .text_color(muted)
                            .child(format!("#{id}")),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .min_w_0()
                            .child(row.instruction),
                    ),
            )
            .child(
                h_flex()
                    .gap(SM)
                    .flex_wrap()
                    .justify_between()
                    .child(status_badge(
                        row.status,
                        if row.paused {
                            Status::Waiting
                        } else {
                            Status::Done
                        },
                        cx,
                    ))
                    .child(meta(when_line, cx).text_right()),
            )
            .child(meta(format!("{} · {}", row.by, row.fired), cx).text_right())
            .children((!self.state.is_archived(space, project)).then_some(actions))
    }

    /// "+ New routine", or the form it opens.
    fn routine_form(&self, space: &str, project: &str, cx: &mut Context<Self>) -> AnyElement {
        let form = self.state.routine_form();
        if !form.open {
            return button("new-routine")
                .icon(AppIcon::CalendarPlus)
                .label("New routine")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.routine_form_mut().open = true;
                    cx.notify();
                }))
                .into_any_element();
        }
        let muted = cx.theme().muted_foreground;
        let label = |text: &'static str| {
            div()
                .w(INLINE_LABEL_WIDTH)
                .text_size(TYPE_BODY)
                .text_color(muted)
                .child(text)
        };
        let cadence = ButtonGroup::new("routine-cadence")
            .outline()
            .compact()
            .children(Cadence::ALL.iter().map(|(cadence, name)| {
                button(SharedString::from(format!("cadence-{name}")))
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
            .gap(SM)
            .child(label("At"))
            .child(div().w(SHORT_FIELD_WIDTH).child(field(&self.routine_at)));
        let detail = match form.cadence {
            Cadence::Every => h_flex()
                .gap(SM)
                .child(label("Every"))
                .child(div().w(SHORT_FIELD_WIDTH).child(field(&self.routine_every)))
                .child(
                    ButtonGroup::new("routine-unit")
                        .outline()
                        .compact()
                        .child(
                            button("unit-minutes")
                                .label("minutes")
                                .selected(!form.hours),
                        )
                        .child(button("unit-hours").label("hours").selected(form.hours))
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
                    button(SharedString::from(format!("day-{name}")))
                        .small()
                        .label(shown)
                        .selected(form.days.contains(&day))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.routine_form_mut().toggle_day(day);
                            cx.notify();
                        }))
                });
                h_flex()
                    .gap(SM)
                    .flex_wrap()
                    .child(label("On"))
                    .children(days)
                    .child(at)
            }
            Cadence::Monthly => h_flex()
                .gap(SM)
                .child(label("Day"))
                .child(div().w(INLINE_LABEL_WIDTH).child(field(&self.routine_day)))
                .child(at),
        };
        let (in_space, named) = (space.to_owned(), project.to_owned());
        card(cx)
            .child(section("New routine"))
            .child(field(&self.routine_instruction))
            .child(cadence)
            .child(detail)
            .child(
                h_flex()
                    .gap(SM)
                    .child(label("Zone"))
                    .child(div().w(ZONE_WIDTH).child(field(&self.routine_zone))),
            )
            .child(
                div()
                    .text_size(TYPE_META)
                    .text_color(muted)
                    .child("It is yours: what it starts counts as work you requested."),
            )
            .child(
                h_flex()
                    .gap(SM)
                    .child(
                        button("routine-add")
                            .primary()
                            .icon(AppIcon::CalendarPlus)
                            .label("Add routine")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_routine(in_space.clone(), named.clone(), cx);
                            })),
                    )
                    .child(
                        button("routine-close")
                            .icon(AppIcon::X)
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
