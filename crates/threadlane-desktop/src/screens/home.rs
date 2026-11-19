//! The Inbox (wireframe 1, `Screen::Home`): everything that needs you across
//! every space, then recent failures, then every conversation as one
//! timeline, above a composer to any space's Lead.

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_core::model::OutboxKind;
use threadlane_desktop::{Author, Part, Spoken, When, count_form, is_quiet_update};

use super::{gap_line, menu_item};
use crate::components::{button, count, decision_scroller, heading, meta, section, status_dot};
use crate::tokens::{
    CONTROL_HEIGHT, LG, MD, READING_WIDTH, ROW_HEIGHT, ROW_RADIUS, SM, Status, TABULAR, TYPE_BODY,
    TYPE_META, XL, XS,
};
use crate::{Desktop, Screen};

/// How many failed tasks Inbox shows before folding behind "Show all".
const INBOX_FAILURES_SHOWN: usize = 3;

/// How many recent messages Inbox shows before folding behind "Show more".
const INBOX_RECENT_SHOWN: usize = 3;

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

        let multi_space = state.spaces().len() > 1;
        let failures = state.recent_failures(when);
        let show_all_failures = state.inbox_show_all_failures();
        let visible_failures = if show_all_failures {
            failures.len()
        } else {
            failures.len().min(INBOX_FAILURES_SHOWN)
        };
        let mut failed_header = h_flex()
            .gap(SM)
            .items_center()
            .children(count(failures.len(), cx));
        if failures.len() > INBOX_FAILURES_SHOWN {
            failed_header = failed_header.child(
                button("inbox-toggle-failures")
                    .small()
                    .ghost()
                    .label(if show_all_failures {
                        "Show fewer".to_owned()
                    } else {
                        format!("Show all {}", failures.len())
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.toggle_inbox_all_failures();
                        cx.notify();
                    })),
            );
        }
        let mut failed = v_flex()
            .gap(XS)
            .child(section("Failed").child(failed_header));
        // A space whose task list failed to load could be hiding failures.
        let task_gaps = state.gaps(Part::Tasks);
        let show_failed = !failures.is_empty() || !task_gaps.is_empty();
        for (space, gap) in &task_gaps {
            failed = failed.child(gap_line(gap, "tasks", space, cx));
        }
        let mono = cx.theme().mono_font_family.clone();
        for &(space, summary) in &failures[..visible_failures] {
            let task = &summary.task;
            let screen = Screen::Task {
                space: space.to_owned(),
                task: task.id,
            };
            let place = if multi_space {
                format!("{space} / {} · {}", summary.project, summary.agent)
            } else {
                format!("{} · {}", summary.project, summary.agent)
            };
            failed = failed.child(
                button(SharedString::from(format!("failed-{space}-{}", task.id)))
                    .ghost()
                    .w_full()
                    .rounded(ROW_RADIUS)
                    .px(SM)
                    .accessibility_label(format!("Open {} {}", task.id, task.title))
                    .child(
                        h_flex()
                            .w_full()
                            .gap(SM)
                            .min_h(ROW_HEIGHT)
                            .items_center()
                            .child(
                                h_flex()
                                    .size(LG)
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .child(status_dot(Status::Failed, cx)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(TYPE_META)
                                    .font_family(mono.clone())
                                    .font_features(TABULAR.clone())
                                    .text_color(muted)
                                    .child(task.id.to_string()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(task.title.clone()),
                            )
                            .child(meta(place, cx).flex_none()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
            );
        }

        let recent_empty = state.timeline().is_empty()
            && state.gaps(Part::Recent).is_empty()
            && !state.timeline_has_earlier();
        let quiet = !show_needs && !show_failed && recent_empty && banners.is_empty();

        let content = v_flex()
            .gap(LG)
            .child(heading("Inbox"))
            .when(multi_space, |view| {
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
            .when(show_needs && show_failed, |view| {
                view.child(Separator::horizontal())
            })
            .when(show_failed, |view| view.child(failed))
            .when((show_needs || show_failed) && !recent_empty, |view| {
                view.child(Separator::horizontal())
            })
            .when(!recent_empty, |view| view.child(self.timeline(when, cx)))
            .when(quiet, |view| {
                view.child(meta(
                    "Nothing needs you. Tell your team what you need below.",
                    cx,
                ))
            });
        let _ = XL;
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
        let multi_space = state.spaces().len() > 1;
        let all_entries = state.timeline();
        let show_quiet = state.inbox_show_quiet_updates();
        let quiet_count = all_entries
            .iter()
            .filter(|entry| {
                matches!(entry.spoken, Spoken::Delivered(_)) && is_quiet_update(&entry.text())
            })
            .count();
        let filtered: Vec<_> = all_entries
            .iter()
            .filter(|entry| {
                show_quiet
                    || !(matches!(entry.spoken, Spoken::Delivered(_))
                        && is_quiet_update(&entry.text()))
            })
            .collect();
        let show_all_recent = state.inbox_show_all_recent();
        let visible_len = if show_all_recent {
            filtered.len()
        } else {
            filtered.len().min(INBOX_RECENT_SHOWN)
        };

        let mut header_actions = h_flex().gap(XS).items_center();
        if quiet_count > 0 {
            header_actions = header_actions.child(
                button("inbox-toggle-quiet")
                    .small()
                    .ghost()
                    .label(if show_quiet {
                        "Hide quiet updates".to_owned()
                    } else {
                        format!(
                            "{quiet_count} quiet {} hidden",
                            count_form(quiet_count, "update", "updates")
                        )
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.toggle_inbox_quiet_updates();
                        cx.notify();
                    })),
            );
        }
        if filtered.len() > INBOX_RECENT_SHOWN {
            header_actions = header_actions.child(
                button("inbox-toggle-recent")
                    .small()
                    .ghost()
                    .label(if show_all_recent {
                        "Show fewer".to_owned()
                    } else {
                        format!("Show {} more", filtered.len() - INBOX_RECENT_SHOWN)
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.toggle_inbox_all_recent();
                        cx.notify();
                    })),
            );
        }

        let mut list = v_flex()
            .gap(MD)
            .child(section("Recent").child(header_actions));
        let gaps = state.gaps(Part::Recent);
        for (space, gap) in &gaps {
            list = list.child(gap_line(gap, "recent results", space, cx));
        }
        if filtered.is_empty() && quiet_count > 0 {
            list = list.child(meta(
                "Only quiet routine updates recently. Click above to view them.",
                cx,
            ));
        }
        for (index, entry) in filtered[..visible_len].iter().enumerate() {
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
            let place = match (multi_space, entry.project) {
                (true, Some(project)) => format!("{space} / {project}"),
                (false, Some(project)) => project.to_owned(),
                (_, None) => space.clone(),
            };
            let screen = entry.project.map(|project| Screen::Project {
                space: space.clone(),
                project: project.to_owned(),
                tab: crate::Tab::Conversation,
            });
            let title = h_flex()
                .gap(SM)
                .items_center()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(place.clone()))
                .child(meta(who.clone(), cx));
            let header_left = match &screen {
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
            let header = h_flex()
                .w_full()
                .justify_between()
                .items_center()
                .gap(SM)
                .child(header_left)
                .child(
                    meta(when.moment(entry.at), cx)
                        .flex_none()
                        .font_features(TABULAR.clone()),
                );
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
                v_flex()
                    .id(SharedString::from(format!("timeline-menu-{index}")))
                    .w_full()
                    .gap(XS)
                    .items_start()
                    .child(header)
                    .child(div().w_full().child(body))
                    .children(crate::components::chat_images::previews(entry.images, cx))
                    .children(from)
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
        if show_all_recent && state.timeline_has_earlier() {
            list = list.child(
                button("timeline-earlier")
                    .small()
                    .ghost()
                    .self_start()
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

    /// The Inbox composer sends to the lead chosen in its "To:" picker.
    fn home_composer(&self, cx: &mut Context<Self>) -> Div {
        let picker = self.state.target().map(|current| {
            self.to_picker(
                current,
                |state, space, project| state.set_target(space, project),
                cx,
            )
        });
        self.render_composer(
            threadlane_desktop::Draft::Lead,
            "Message the lead",
            picker,
            cx,
        )
    }
}
