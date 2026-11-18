//! Home (wireframe 1): everything that needs you across every space, then
//! what is running, then every conversation as one timeline, and a
//! composer to any project (General by default).

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_core::model::OutboxKind;
use threadlane_desktop::{Author, EmptyRegion, Part, Spoken, When, running_elapsed};

use super::{empty, gap_line, menu_item};
use crate::components::{button, count, decision_scroller, heading, meta, section, status_dot};
use crate::tokens::{
    CONTROL_HEIGHT, LG, READING_WIDTH, ROW_HEIGHT, SM, Status, TIMESTAMP_WIDTH, TYPE_BODY,
    TYPE_META, XL, XS,
};
use crate::{Desktop, Screen};

impl Desktop {
    pub(crate) fn home(&self, when: &When, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let state = &self.state;

        let mut chips = h_flex().gap(XS).flex_wrap().child(
            button("filter-all")
                .label("All")
                .selected(state.filter().is_none())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.set_filter(None);
                    this.load_screen(cx);
                    cx.notify();
                })),
        );
        for space in state.spaces() {
            let name = space.name.clone();
            chips = chips.child(
                button(SharedString::from(format!("filter-{name}")))
                    .label(name.clone())
                    .selected(state.filter() == Some(name.as_str()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.set_filter(Some(&name));
                        this.load_screen(cx);
                        cx.notify();
                    })),
            );
        }

        let banners: Vec<_> = state
            .spaces()
            .iter()
            .filter(|space| state.filter().is_none_or(|only| only == space.name))
            .filter_map(|space| {
                let text = state.blocked(&space.name)?;
                Some(
                    v_flex()
                        .gap(XS)
                        .items_start()
                        .child(
                            Alert::warning(
                                SharedString::from(format!("blocked-{}", space.name)),
                                text,
                            )
                            .title(format!("System · {} is blocked", space.name)),
                        )
                        .child(
                            button(SharedString::from(format!("accounts-{}", space.name)))
                                .small()
                                .ghost()
                                .tab_stop(true)
                                .label("Model accounts")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.go(Screen::Machines, cx)),
                                ),
                        ),
                )
            })
            .collect();

        let mut needs = v_flex()
            .gap(SM)
            .child(section("Needs you").children(count(state.needs_you().count(), cx)));
        let gaps = state.gaps(Part::Decisions);
        for (space, gap) in &gaps {
            needs = needs.child(gap_line(gap, "decisions", space, cx));
        }
        if state.spaces().is_empty() {
            needs = needs.child(div().text_color(muted).child(
                "No hub is paired. Pair one with `threadlane hub add <name> <data-dir>` or \
                 `threadlane hub join <name> <code>`, then reopen the app.",
            ));
        } else if state.needs_you().count() == 0 && gaps.is_empty() {
            needs = needs.child(empty(EmptyRegion::Decisions, cx));
        }
        for queued in state.needs_you() {
            needs = needs.child(self.decision(queued, when, cx));
        }

        // Tasks that failed or are stuck, under "Needs you" (shown only when
        // there are some, so a quiet Home stays quiet).
        let attention = state.needs_attention();
        let attention = (!attention.is_empty()).then(|| {
            let mut list = v_flex()
                .gap(XS)
                .child(section("Needs attention").children(count(attention.len(), cx)));
            for row in attention {
                let screen = Screen::Task {
                    space: row.space.clone(),
                    task: row.task,
                };
                list = list.child(
                    button(SharedString::from(format!(
                        "attention-{}-{}",
                        row.space, row.task
                    )))
                    .text()
                    .w_full()
                    .accessibility_label(format!("Open {}: {}", row.title, row.why))
                    .child(
                        v_flex()
                            .w_full()
                            .items_start()
                            .child(div().text_size(TYPE_BODY).child(row.title.clone()))
                            .child(
                                div()
                                    .text_size(TYPE_META)
                                    .text_color(muted)
                                    .whitespace_normal()
                                    .child(format!("{} / {}", row.space, row.why)),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
                );
            }
            list
        });

        let running = state.running();
        let leads = state.running_leads();
        let running_count = running.len() + leads.len();
        let mut table = v_flex()
            .gap(XS)
            .child(section("Running now").children(count(running_count, cx)));
        let gaps = state.gaps(Part::Running);
        for (space, gap) in &gaps {
            table = table.child(gap_line(gap, "running tasks", space, cx));
        }
        if running_count == 0 && gaps.is_empty() {
            table = table.child(empty(EmptyRegion::Running, cx));
        } else if running_count != 0 {
            table = table.child(
                running_row(["Task", "Project", "Agent", "Machine", "Running"], div())
                    .text_size(TYPE_META)
                    .text_color(muted),
            );
        }
        for (space, summary) in running {
            let task = &summary.task;
            let screen = Screen::Task {
                space: space.to_owned(),
                task: task.id,
            };
            let cells = [
                format!("{} {}", task.id, task.title),
                summary.project.clone(),
                summary.agent.clone(),
                summary.machine.clone().unwrap_or_default(),
                running_elapsed(summary, when).unwrap_or_default(),
            ];
            table = table.child(
                button(SharedString::from(format!("running-{space}-{}", task.id)))
                    .text()
                    .w_full()
                    .accessibility_label(format!("Open {} {}", task.id, task.title))
                    .child(running_row(cells, status_dot(Status::Running, cx)))
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
            );
        }

        for (space, project, started) in leads {
            let screen = Screen::Project {
                space: space.to_owned(),
                project: project.to_owned(),
                tab: crate::Tab::Conversation,
            };
            let title = format!("{project} · lead");
            table = table.child(
                button(SharedString::from(format!(
                    "running-lead-{space}-{project}"
                )))
                .text()
                .w_full()
                .accessibility_label(format!("Open {title} in {space}"))
                .child(running_row(
                    [
                        title,
                        project.to_owned(),
                        "Lead".to_owned(),
                        String::new(),
                        when.since(started),
                    ],
                    status_dot(Status::Running, cx),
                ))
                .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
            );
        }

        let recent = self.timeline(when, cx);

        let content = v_flex()
            .gap(XL)
            .child(heading("Home"))
            .child(chips.w_full().min_h(CONTROL_HEIGHT).flex_none())
            .child(meta(state.status_line(), cx))
            .children(
                state
                    .held_lines(when)
                    .into_iter()
                    .map(|line| div().text_size(TYPE_BODY).text_color(muted).child(line)),
            )
            .children(banners)
            .child(needs)
            .children(attention)
            .child(table)
            .child(recent)
            .child(self.home_composer(cx));
        decision_scroller("home", content, &self.decision_scroll)
            .w_full()
            .max_w(READING_WIDTH)
            .self_center()
            .into_any_element()
    }

    /// Home's timeline (docs/one-place-to-talk.md): every conversation in
    /// the spaces shown, newest first; each entry names its space and
    /// project and opens that conversation.
    fn timeline(&self, when: &When, cx: &mut Context<Self>) -> Div {
        let state = &self.state;
        let mut list = v_flex().gap(SM).child(section("Conversations"));
        let entries = state.timeline();
        let gaps = state.gaps(Part::Recent);
        for (space, gap) in &gaps {
            list = list.child(gap_line(gap, "recent results", space, cx));
        }
        if entries.is_empty() && gaps.is_empty() {
            list = list.child(empty(EmptyRegion::Results, cx));
        }
        for (index, entry) in entries.iter().enumerate() {
            let space = entry.space.to_owned();
            // A reply in General links to the conversation it came from.
            let mut from = None;
            let (who, body) = match entry.spoken {
                Spoken::Said(text, by) => {
                    if let Author::ProjectLead(project) = by {
                        from = Some(project.clone());
                    }
                    (
                        by.label(),
                        div()
                            .text_size(TYPE_BODY)
                            .whitespace_normal()
                            .child(text.to_owned())
                            .into_any_element(),
                    )
                }
                Spoken::Delivered(item) => {
                    let who = if item.kind() == OutboxKind::SystemNotice {
                        "Notice"
                    } else {
                        "Lead"
                    }
                    .to_owned();
                    let body = match state.digest(item) {
                        Some(digest) => self.digest_entry(&space, digest, cx).into_any_element(),
                        None => {
                            let seq = item.item.seq;
                            self.prose(format!("result-{space}-{seq}"), &space, item.text(), cx)
                                .result(seq, state.result_expanded(&space, seq))
                                .into_any_element()
                        }
                    };
                    (who, body)
                }
            };
            let place = entry
                .project
                .map_or_else(|| space.clone(), |project| format!("{space} / {project}"));
            let screen = entry.project.map(|project| Screen::Project {
                space: space.clone(),
                project: project.to_owned(),
                tab: crate::Tab::Conversation,
            });
            let title = h_flex()
                .gap(SM)
                .child(div().font_weight(FontWeight::SEMIBOLD).child(place.clone()))
                .child(meta(who.clone(), cx));
            let header = match &screen {
                Some(screen) => {
                    let screen = screen.clone();
                    button(SharedString::from(format!("timeline-{index}")))
                        .text()
                        .accessibility_label(format!("Open chat history for {place}"))
                        .child(title)
                        .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
                        .into_any_element()
                }
                None => title.into_any_element(),
            };
            let from = from.map(|project| {
                let screen = Screen::Project {
                    space: space.clone(),
                    project: project.clone(),
                    tab: crate::Tab::Conversation,
                };
                button(SharedString::from(format!("timeline-from-{index}")))
                    .small()
                    .ghost()
                    .tab_stop(true)
                    .label(format!("Open {project}"))
                    .accessibility_label(format!("Open chat history for {space} / {project}"))
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
            });
            let view = cx.entity().downgrade();
            let text = entry.text();
            let reply = screen.filter(|screen| {
                who != "Notice" && self.main_chat_screen(&space).as_ref() == Some(screen)
            });
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("timeline-menu-{index}")))
                    .items_start()
                    .gap(SM)
                    .child(
                        meta(when.moment(entry.at), cx)
                            .w(TIMESTAMP_WIDTH)
                            .flex_none()
                            .text_right(),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .items_start()
                            .child(header)
                            .child(body)
                            .children(from),
                    )
                    .context_menu(move |menu, _, _| {
                        let copy = text.clone();
                        let mut menu =
                            menu.item(menu_item(&view, "Copy text", move |this, _, cx| {
                                this.copy(copy.clone(), cx)
                            }));
                        if let Some(screen) = &reply {
                            let (screen, quote) = (screen.clone(), text.clone());
                            menu = menu.item(menu_item(&view, "Reply", move |this, window, cx| {
                                this.go(screen.clone(), cx);
                                this.reply_to(&quote, window, cx);
                            }));
                        }
                        menu
                    }),
            );
        }
        if state.timeline_has_earlier() {
            list = list.child(
                button("timeline-earlier")
                    .small()
                    .tab_stop(true)
                    .label("Show earlier")
                    .on_click(cx.listener(|this, _, _, cx| {
                        for (space, project, before) in this.state.show_earlier_timeline() {
                            this.earlier(space, project, before, cx);
                        }
                        cx.notify();
                    })),
            );
        }
        list
    }

    /// The Home composer sends to the chosen space's Main Chat.
    fn home_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self
            .state
            .target()
            .map_or_else(|| "No space".to_owned(), |(space, _)| format!("{space} ▾"));
        let view = cx.entity().downgrade();
        let targets: Vec<(String, String)> = self
            .state
            .spaces()
            .iter()
            .filter_map(|space| Some((space.name.clone(), space.main_chat_project()?.to_owned())))
            .collect();
        let accessibility = format!("Choose a space · {label}");
        let picker = button("target")
            .label(label)
            .tab_stop(true)
            .accessibility_label(accessibility)
            .dropdown_menu(move |mut menu, _, _| {
                for (space, project) in &targets {
                    let (space, project, view) = (space.clone(), project.clone(), view.clone());
                    menu =
                        menu.item(PopupMenuItem::new(space.clone()).on_click(move |_, _, cx| {
                            let _gone = view.update(cx, |this, cx| {
                                this.state.set_target(&space, &project);
                                cx.notify();
                            });
                        }));
                }
                menu
            });
        self.render_composer(
            threadlane_desktop::Draft::Lead,
            "Message the selected space Lead",
            Some(picker.into_any_element()),
            cx,
        )
    }
}

/// The task title gets the most space, and elapsed time aligns at the trailing edge.
fn running_row(cells: [impl Into<SharedString>; 5], marker: impl IntoElement) -> Div {
    h_flex()
        .w_full()
        .gap(SM)
        .min_h(ROW_HEIGHT)
        .child(h_flex().size(LG).flex_none().child(marker))
        .children(cells.into_iter().enumerate().map(|(index, cell)| {
            div()
                .map(|cell| {
                    if index == 0 {
                        cell.flex_1().flex_grow(2.)
                    } else {
                        cell.flex_1()
                    }
                })
                .min_w_0()
                .truncate()
                .when(index == 4, |cell| cell.text_right())
                .child(cell.into())
        }))
}
