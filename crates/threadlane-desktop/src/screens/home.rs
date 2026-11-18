//! The Inbox (wireframe 1, `Screen::Home`): everything that needs you across
//! every space, then recent failures, then every conversation as one
//! timeline, above a composer to any space's Lead.

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_core::model::OutboxKind;
use threadlane_desktop::{Author, Part, Spoken, When};

use super::{gap_line, menu_item};
use crate::components::{button, count, decision_scroller, heading, meta, section, status_dot};
use crate::tokens::{
    CONTROL_HEIGHT, LG, READING_WIDTH, ROW_HEIGHT, SM, Status, TIMESTAMP_WIDTH, TYPE_BODY, XL, XS,
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
        let show_needs =
            state.spaces().is_empty() || state.needs_you().next().is_some() || !gaps.is_empty();
        for (space, gap) in &gaps {
            needs = needs.child(gap_line(gap, "decisions", space, cx));
        }
        if state.spaces().is_empty() {
            needs = needs.child(div().text_color(muted).child(
                "No hub is paired. Pair one with `threadlane hub add <name> <data-dir>` or \
                 `threadlane hub join <name> <code>`, then reopen the app.",
            ));
        }
        for queued in state.needs_you() {
            needs = needs.child(self.decision(queued, when, cx));
        }

        let failures = state.recent_failures(when);
        let mut failed = v_flex()
            .gap(XS)
            .child(section("Failed").children(count(failures.len(), cx)));
        for &(space, summary) in &failures {
            let task = &summary.task;
            let screen = Screen::Task {
                space: space.to_owned(),
                task: task.id,
            };
            failed = failed.child(
                button(SharedString::from(format!("failed-{space}-{}", task.id)))
                    .text()
                    .w_full()
                    .accessibility_label(format!("Open {} {}", task.id, task.title))
                    .child(
                        h_flex()
                            .w_full()
                            .gap(SM)
                            .min_h(ROW_HEIGHT)
                            .child(
                                h_flex()
                                    .size(LG)
                                    .flex_none()
                                    .child(status_dot(Status::Failed, cx)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(format!("{} {}", task.id, task.title)),
                            )
                            .child(meta(
                                format!("{space} / {} · {}", summary.project, summary.agent),
                                cx,
                            )),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
            );
        }

        let recent_empty = state.timeline().is_empty()
            && state.gaps(Part::Recent).is_empty()
            && !state.timeline_has_earlier();
        let quiet = !show_needs && failures.is_empty() && recent_empty && banners.is_empty();

        let content = v_flex()
            .gap(XL)
            .child(heading("Inbox"))
            .when(state.spaces().len() > 1, |view| {
                view.child(chips.w_full().min_h(CONTROL_HEIGHT).flex_none())
            })
            .children(
                state
                    .held_lines(when)
                    .into_iter()
                    .map(|line| div().text_size(TYPE_BODY).text_color(muted).child(line)),
            )
            .children(banners)
            .when(show_needs, |view| view.child(needs))
            .when(!failures.is_empty(), |view| view.child(failed))
            .when(!recent_empty, |view| view.child(self.timeline(when, cx)))
            .when(quiet, |view| {
                view.child(meta(
                    "Nothing needs you. Tell your team what you need below.",
                    cx,
                ))
            });
        // The page scrolls above a composer pinned at the bottom, as in a project's Chat tab.
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .max_w(READING_WIDTH)
            .self_center()
            .gap(SM)
            .child(decision_scroller("home", content, &self.decision_scroll))
            .when(!state.spaces().is_empty(), |view| {
                view.child(self.home_composer(cx).flex_none())
            })
            .into_any_element()
    }

    /// The Inbox timeline (docs/one-place-to-talk.md): every conversation in
    /// the spaces shown, newest first; each entry names its space and
    /// project and opens that conversation.
    fn timeline(&self, when: &When, cx: &mut Context<Self>) -> Div {
        let state = &self.state;
        let mut list = v_flex().gap(SM).child(section("Recent"));
        let entries = state.timeline();
        let gaps = state.gaps(Part::Recent);
        for (space, gap) in &gaps {
            list = list.child(gap_line(gap, "recent results", space, cx));
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
                            .children(crate::components::chat_images::previews(entry.images, cx))
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

    /// The Inbox composer sends to the chosen space's Main Chat.
    fn home_composer(&self, cx: &mut Context<Self>) -> Div {
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
            "Message the space Lead",
            (self.state.spaces().len() > 1).then(|| picker.into_any_element()),
            cx,
        )
    }
}
