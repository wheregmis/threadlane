//! Routines and follow-ups in the space overview and each project.

use gpui_kit::component::button::{ButtonGroup, ButtonVariants as _};
use gpui_kit::component::group_box::GroupBox;
use gpui_kit::component::input::Textarea;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::*;
use threadlane_core::model::Weekday;
use threadlane_desktop::{Cadence, Consequence, EmptyRegion, RoutineRow, When};

use super::{empty, gap_line, menu_item};
use crate::components::{button, card, field, meta, page_header, section, status_badge};
use crate::icons::AppIcon;
use crate::tokens::{
    FIELD_RADIUS, FORM_WIDTH, LG, MD, READING_WIDTH, SHORT_FIELD_WIDTH, SM, Status, XL, XS,
    ZONE_WIDTH,
};
use crate::{Desktop, Screen, Tab, TimerChange};

impl Desktop {
    pub(crate) fn routines_tab(
        &self,
        space: &str,
        project: &str,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let archived = self.state.is_archived(space, project);
        let draft_open = !archived && self.state.routine_form().is_open_for(space, project);
        let (in_space, named) = (space.to_owned(), project.to_owned());
        let overview = Screen::Routines(Some(space.to_owned()));
        let mut list = v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .max_w(READING_WIDTH)
            .self_center()
            .overflow_y_scrollbar()
            .id("project-routines")
            .gap(MD)
            .child(
                h_flex()
                    .justify_between()
                    .flex_wrap()
                    .gap(SM)
                    .child(section("Routines and follow-ups"))
                    .child(
                        h_flex()
                            .gap(SM)
                            .flex_wrap()
                            .child(
                                button("project-space-routines")
                                    .small()
                                    .ghost()
                                    .label("Space routines")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.go(overview.clone(), cx)
                                    })),
                            )
                            .children((!archived && !draft_open).then(|| {
                                button("new-routine")
                                    .small()
                                    .icon(AppIcon::CalendarPlus)
                                    .label("New routine")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_routine_form(&in_space, &named, window, cx);
                                    }))
                            })),
                    ),
            );
        if draft_open {
            list = list.child(self.routine_form(space, project, cx));
        }
        let rows = self.state.routine_rows(space, project, when);
        match self.state.routines_gap(space, project) {
            Some(gap) => list = list.child(gap_line(&gap, "routines", space, cx)),
            None if rows.is_empty() && !draft_open => {
                list = list.child(empty(EmptyRegion::Routines, cx));
            }
            None => {}
        }
        list.child(
            v_flex().children(
                rows.into_iter()
                    .map(|row| self.routine(space, project, row, cx)),
            ),
        )
        .into_any_element()
    }

    /// Every space's routines, by project.
    pub(crate) fn routines_screen(
        &self,
        scope: Option<&str>,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let targets: Vec<_> = self
            .state
            .targets()
            .into_iter()
            .filter(|(space, _)| scope.is_none_or(|scope| *space == scope))
            .map(|(space, project)| (space.to_owned(), project.to_owned()))
            .collect();
        let view = cx.entity().downgrade();
        let scoped = scope.is_some();
        let new_routine = (!targets.is_empty()).then(|| {
            h_flex().child(
                button("space-new-routine")
                    .small()
                    .icon(AppIcon::CalendarPlus)
                    .label("New routine…")
                    .dropdown_menu(move |mut menu, _, _| {
                        for (space, project) in &targets {
                            let (space, project, view) =
                                (space.clone(), project.clone(), view.clone());
                            let label = if scoped {
                                project.clone()
                            } else {
                                format!("{space} / {project}")
                            };
                            menu = menu.item(PopupMenuItem::new(label).on_click(
                                move |_, window, cx| {
                                    let _ = view.update(cx, |this, cx| {
                                        this.go(
                                            Screen::Project {
                                                space: space.clone(),
                                                project: project.clone(),
                                                tab: Tab::Routines,
                                            },
                                            cx,
                                        );
                                        this.open_routine_form(&space, &project, window, cx);
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
        });
        let mut groups = self.state.routine_groups(when);
        groups.retain(|group| scope.is_none_or(|name| group.space == name));
        let mut gaps = self.state.routines_space_gaps();
        gaps.retain(|(space, _)| scope.is_none_or(|name| *space == name));
        let mut list = v_flex().gap(XL).max_w(READING_WIDTH);
        if let Some(space) = scope {
            list =
                list.child(self.work_link("agenda", "Agenda", space, crate::WorkPage::Agenda, cx));
        }
        if groups.is_empty() && gaps.is_empty() {
            list = list.child(empty(EmptyRegion::AllRoutines, cx));
        }
        for (space, gap) in gaps {
            list = list.child(gap_line(&gap, "projects with routines", space, cx));
        }
        for group in groups {
            let open = Screen::Project {
                space: group.space.clone(),
                project: group.project.clone(),
                tab: Tab::Routines,
            };
            let label = if scope.is_some() {
                group.project.clone()
            } else {
                format!("{} / {}", group.space, group.project)
            };
            let mut project = v_flex().gap(SM).child(
                h_flex()
                    .justify_between()
                    .gap(SM)
                    .child(section(label))
                    .child(
                        button(SharedString::from(format!(
                            "routines-of-{}-{}",
                            group.space, group.project
                        )))
                        .small()
                        .ghost()
                        .label("Open project")
                        .icon(AppIcon::ChevronRight)
                        .accessibility_label(format!("Open routines for {}", group.project))
                        .on_click(cx.listener(move |this, _, _, cx| this.go(open.clone(), cx))),
                    ),
            );
            match group.rows {
                Err(gap) => project = project.child(gap_line(&gap, "routines", &group.space, cx)),
                Ok(rows) => {
                    project = project.child(
                        v_flex().children(
                            rows.into_iter()
                                .map(|row| self.routine(&group.space, &group.project, row, cx)),
                        ),
                    );
                }
            }
            list = list.child(project);
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .gap(LG)
            .child(page_header(
                "Routines",
                scope.map(|space| meta(space.to_owned(), cx)),
                new_routine,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_y_scrollbar()
                    .id("routines-all")
                    .child(list),
            )
            .into_any_element()
    }

    fn routine(&self, space: &str, project: &str, row: RoutineRow, cx: &mut Context<Self>) -> Div {
        let id = row.id;
        let key = format!("{space}-{project}-{id}");
        let title = routine_title(&row.instruction);
        let schedule = row.zone.as_ref().map_or_else(
            || row.cadence.clone(),
            |zone| format!("{} · {zone}", row.cadence),
        );
        let details = {
            let heading = format!("Routine #{id}");
            let view = cx.entity().downgrade();
            let screen = Screen::Work {
                space: space.to_owned(),
                page: crate::WorkPage::History {
                    timer: id,
                    before: None,
                },
            };
            let target = format!("{space} / {project}");
            let schedule = schedule.clone();
            let (timing, history) = (row.next.clone(), format!("{} · {}", row.by, row.fired));
            let instruction = row.instruction.clone();
            button(SharedString::from(format!("routine-details-{key}")))
                .small()
                .ghost()
                .label("Details…")
                .accessibility_label(format!("Details for routine {id}"))
                .on_click(move |_, window, cx| {
                    let (heading, target, schedule, timing, history, instruction) = (
                        heading.clone(),
                        target.clone(),
                        schedule.clone(),
                        timing.clone(),
                        history.clone(),
                        instruction.clone(),
                    );
                    let (view, screen) = (view.clone(), screen.clone());
                    window.open_dialog(cx, move |dialog, window, cx| {
                        let (view, screen) = (view.clone(), screen.clone());
                        dialog.title(heading.clone()).width(FORM_WIDTH).child(
                            v_flex()
                                .gap(SM)
                                .child(meta(target.clone(), cx))
                                .child(meta(format!("{schedule} · {timing}"), cx))
                                .child(meta(history.clone(), cx))
                                .child(
                                    button("routine-detail-history")
                                        .small()
                                        .label("Run history")
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let _ = view
                                                .update(cx, |this, cx| this.go(screen.clone(), cx));
                                        }),
                                )
                                .child(section("Instructions"))
                                .child(
                                    div()
                                        .id("routine-instructions")
                                        .min_w_0()
                                        .max_h(window.viewport_size().height * 0.45)
                                        .overflow_y_scrollbar()
                                        .whitespace_normal()
                                        .child(Text::new(
                                            "full-routine-prompt".into(),
                                            instruction.clone().into(),
                                        )),
                                ),
                        )
                    });
                })
        };
        let (label, icon, change) = if row.paused {
            ("Resume", AppIcon::Play, TimerChange::Resume)
        } else {
            ("Pause", AppIcon::Pause, TimerChange::Pause)
        };
        let view = cx.entity().downgrade();
        let actions = (!self.state.is_archived(space, project)).then(|| {
            let (space, project) = (space.to_owned(), project.to_owned());
            let manual = row.can_run_once.then(|| {
                let (space, project) = (space.clone(), project.clone());
                button(SharedString::from(format!("run-once-{key}")))
                    .small()
                    .label("Run once")
                    .disabled(self.queuing_routine(&space, id))
                    .accessibility_label(format!("Run routine {id} once in {project}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.queue_routine(space.clone(), project.clone(), id, cx)
                    }))
            });
            let cancel = Consequence::CancelRoutine(space.clone(), project.clone(), id);
            h_flex()
                .gap(XS)
                .children(manual)
                .child(
                    button(SharedString::from(format!("timer-toggle-{key}")))
                        .small()
                        .icon(icon)
                        .label(label)
                        .accessibility_label(format!("{label} routine {id}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.change_timer(space.clone(), project.clone(), id, change, cx);
                        })),
                )
                .child(
                    button(SharedString::from(format!("timer-more-{key}")))
                        .small()
                        .ghost()
                        .label("More")
                        .icon(AppIcon::ChevronDown)
                        .accessibility_label(format!("More actions for routine {id}"))
                        .dropdown_menu(move |menu, _, cx| {
                            let cancel = cancel.clone();
                            menu.item(
                                menu_item(&view, "Cancel routine…", move |this, window, cx| {
                                    this.confirm_action(cancel.clone(), window, cx);
                                })
                                .icon(Icon::new(AppIcon::X).text_color(cx.theme().danger)),
                            )
                        }),
                )
        });
        h_flex()
            .gap(MD)
            .py(MD)
            .justify_between()
            .flex_wrap()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_48()
                    .gap(XS)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(Text::new(
                                SharedString::from(format!("routine-title-{key}")).into(),
                                title.to_owned().into(),
                            )),
                    )
                    .child(
                        h_flex()
                            .gap(SM)
                            .flex_wrap()
                            .child(status_badge(
                                row.status,
                                if row.paused {
                                    Status::Waiting
                                } else {
                                    Status::Done
                                },
                                cx,
                            ))
                            .child(meta(schedule, cx))
                            .children(row.missed_grace_minutes.map(|minutes| {
                                meta(format!("Skip if over {minutes} min late"), cx)
                            }))
                            .children((!row.paused).then(|| meta(row.next, cx))),
                    ),
            )
            .child(
                h_flex()
                    .gap(XS)
                    .child(details)
                    .child(self.work_link(
                        format!("history-{key}"),
                        "Run history",
                        space,
                        crate::WorkPage::History {
                            timer: id,
                            before: None,
                        },
                        cx,
                    ))
                    .children(actions),
            )
    }

    fn open_routine_form(
        &mut self,
        space: &str,
        project: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let form = self.state.routine_form_mut();
        if !form.is_open_for(space, project) {
            form.open_for = Some((space.to_owned(), project.to_owned()));
            form.missed_grace_minutes = None;
            self.routine_instruction
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.routine_instruction.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// The draft for the current project.
    fn routine_form(&self, space: &str, project: &str, cx: &mut Context<Self>) -> GroupBox {
        let form = self.state.routine_form();
        let label = |text| meta(text, cx).w_24().flex_none();
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
            .child(label("Time"))
            .child(div().w(SHORT_FIELD_WIDTH).child(field(&self.routine_at)));
        let detail = match form.cadence {
            Cadence::Every => h_flex()
                .gap(SM)
                .flex_wrap()
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
                    .child(label("Days"))
                    .children(days)
                    .child(at)
            }
            Cadence::Monthly => h_flex()
                .gap(SM)
                .flex_wrap()
                .child(label("Day of month"))
                .child(div().w(SHORT_FIELD_WIDTH).child(field(&self.routine_day)))
                .child(at),
        };
        let grace = form.missed_grace_minutes;
        let policy_label = |grace: Option<u16>| {
            grace.map_or_else(
                || "Catch up once".to_owned(),
                |minutes| format!("Skip if more than {minutes} min late"),
            )
        };
        let view = cx.entity().downgrade();
        let missed = button("routine-missed-policy")
            .small()
            .outline()
            .label(policy_label(grace))
            .accessibility_label(format!("Missed run policy: {}", policy_label(grace)))
            .dropdown_menu(move |mut menu, _, _| {
                for minutes in [None, Some(5), Some(15), Some(30), Some(60)] {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(policy_label(minutes))
                            .checked(grace == minutes)
                            .on_click(move |_, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.state.routine_form_mut().missed_grace_minutes = minutes;
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            });
        let (in_space, named) = (space.to_owned(), project.to_owned());
        card(cx)
            .max_w(FORM_WIDTH)
            .child(section("New routine"))
            .child(meta("Instructions", cx))
            .child(
                Textarea::new(&self.routine_instruction)
                    .rounded(FIELD_RADIUS)
                    .aria_label("Instructions"),
            )
            .child(meta("Schedule", cx))
            .child(cadence)
            .child(detail)
            .child(
                h_flex().gap(SM).child(label("Time zone")).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .max_w(ZONE_WIDTH)
                        .child(field(&self.routine_zone)),
                ),
            )
            .child(
                h_flex()
                    .gap(SM)
                    .flex_wrap()
                    .child(label("Missed run"))
                    .child(missed),
            )
            .child(meta(
                "After downtime, the routine never runs more than once to catch up.",
                cx,
            ))
            .child(meta(
                "Work started by this routine counts as work you requested.",
                cx,
            ))
            .child(
                h_flex()
                    .gap(SM)
                    .child(
                        button("routine-add")
                            .primary()
                            .icon(AppIcon::CalendarPlus)
                            .label(if self.routine_adding {
                                "Creating routine…"
                            } else {
                                "Create routine"
                            })
                            .disabled(self.routine_adding)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_routine(in_space.clone(), named.clone(), cx);
                            })),
                    )
                    .child(
                        button("routine-close")
                            .icon(AppIcon::X)
                            .label("Cancel")
                            .disabled(self.routine_adding)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.routine_form_mut().open_for = None;
                                cx.notify();
                            })),
                    ),
            )
    }
}

fn routine_title(instruction: &str) -> &str {
    let first_line = instruction.trim().lines().next().unwrap_or("Routine");
    [": ", ". "]
        .into_iter()
        .filter_map(|separator| first_line.split_once(separator))
        .min_by_key(|(heading, _)| heading.len())
        .filter(|(heading, _)| !heading.is_empty() && heading.chars().count() <= 64)
        .map_or(first_line, |(heading, _)| heading)
}

#[cfg(test)]
mod tests {
    use super::routine_title;

    #[test]
    fn routine_titles_keep_short_headings_and_leave_other_text_for_visual_truncation() {
        assert_eq!(
            routine_title("Issue triage: Read recent issues."),
            "Issue triage"
        );
        assert_eq!(routine_title("PR review. Look for open PRs."), "PR review");
        assert_eq!(routine_title("Check the backup"), "Check the backup");
        assert_eq!(routine_title("整理資料\n將本週的更新歸檔。"), "整理資料");
        assert_eq!(routine_title("  "), "Routine");
    }
}
