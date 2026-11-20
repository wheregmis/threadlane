//! The Inbox triages decisions across spaces and keeps task and conversation activity nearby.

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_core::client::TaskSummary;
use threadlane_core::model::OutboxKind;
use threadlane_desktop::{
    Author, DecisionKey, DecisionScope, Draft, Part, Spoken, TimelineEntry, When, count_form,
    is_quiet_update, question_parts,
};

use super::{gap_line, menu_item};
use crate::components::{
    button, count, decision_scroller, icon_button, meta, page_header, section,
};
use crate::icons::AppIcon;
use crate::keyboard::DecisionScroll;
use crate::menus::{FocusComposer, shortcut_hint};
use crate::tokens::{LG, MD, READING_WIDTH, ROW_HEIGHT, SCROLLBAR_GUTTER, SM, TYPE_BODY, XS};
use crate::{Desktop, Screen};

/// How many failed tasks Inbox shows before folding behind "Show all".
const INBOX_FAILURES_SHOWN: usize = 3;

/// How many recent messages Inbox shows before folding behind "Show more".
const INBOX_RECENT_SHOWN: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum InboxTab {
    #[default]
    NeedsYou,
    Activity,
}

impl InboxTab {
    fn decision_scope(self) -> DecisionScope {
        match self {
            Self::NeedsYou => DecisionScope::Home,
            Self::Activity => DecisionScope::None,
        }
    }
}

#[derive(Default)]
pub(crate) struct Inbox {
    tab: InboxTab,
    composer_return_focus: Option<FocusHandle>,
    composing: bool,
    open_update: Option<String>,
}

impl Inbox {
    pub(crate) fn composing(&self) -> bool {
        self.composing
    }
}

impl Desktop {
    pub(crate) fn home(&self, when: &When, cx: &mut Context<Self>) -> AnyElement {
        let message = button("message-lead")
            .icon(AppIcon::MessagesSquare)
            .label("Message a lead")
            .selected(self.inbox.composing)
            .tab_stop(true)
            .disabled(self.state.target().is_none())
            .when_some(shortcut_hint(cx, &FocusComposer), |button, key| {
                button.tooltip(format!("Message a lead  {key}"))
            })
            .on_click(cx.listener(|this, _, window, cx| this.focus_composer(window, cx)));
        let pending = self.state.needs_you().count();
        let tabs = TabBar::new("inbox-tabs")
            .underline()
            .selected_index(usize::from(self.inbox.tab == InboxTab::Activity))
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                this.set_inbox_tab(
                    if *index == 0 {
                        InboxTab::NeedsYou
                    } else {
                        InboxTab::Activity
                    },
                    cx,
                );
            }))
            .child(
                Tab::new()
                    .aria_label(format!("Needs you, {pending} pending"))
                    .child(
                        h_flex()
                            .gap(SM)
                            .child("Needs you")
                            .children(count(pending, cx)),
                    ),
            )
            .child(Tab::new().aria_label("Activity").child("Activity"));
        let content = match self.inbox.tab {
            InboxTab::NeedsYou => self.inbox_needs_you(when, cx),
            InboxTab::Activity => self.inbox_activity(when, cx),
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .max_w(READING_WIDTH)
            .self_center()
            .gap(MD)
            .child(
                v_flex()
                    .flex_none()
                    .gap(SM)
                    .mr(SCROLLBAR_GUTTER)
                    .child(page_header("Inbox", None, Some(h_flex().child(message))))
                    .child(tabs)
                    .children((self.state.spaces().len() > 1).then(|| self.inbox_space_filter(cx))),
            )
            .child(decision_scroller("home", content, &self.decision_scroll))
            .children(
                self.inbox
                    .composing
                    .then(|| self.home_composer(cx).mr(SCROLLBAR_GUTTER)),
            )
            .into_any_element()
    }

    fn set_inbox_tab(&mut self, tab: InboxTab, cx: &mut Context<Self>) {
        if self.inbox.tab == tab {
            return;
        }
        self.inbox.tab = tab;
        self.state.set_decision_scope(tab.decision_scope());
        self.state.clear_current_decision();
        self.decision_scroll = ScrollHandle::default();
        self.scroll_to_decision.set(DecisionScroll::None);
        cx.notify();
    }

    fn inbox_space_filter(&self, cx: &mut Context<Self>) -> Div {
        let state = &self.state;
        let mut chips = h_flex().gap(XS).flex_wrap().child(
            button("filter-all")
                .small()
                .ghost()
                .tab_stop(true)
                .label("All spaces")
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
                    .small()
                    .ghost()
                    .tab_stop(true)
                    .label(name.clone())
                    .selected(state.filter() == Some(name.as_str()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.set_filter(Some(&name));
                        this.load_screen(cx);
                        cx.notify();
                    })),
            );
        }
        chips
    }

    fn inbox_needs_you(&self, when: &When, cx: &mut Context<Self>) -> Div {
        let state = &self.state;
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
                            .title(format!("{} is blocked", space.name)),
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

        let mut needs = v_flex().gap(SM).pb(SM);
        let gaps = state.gaps(Part::Decisions);
        if banners.is_empty() && gaps.is_empty() && state.needs_you().next().is_none() {
            return needs
                .pt(LG)
                .items_start()
                .child(section("You’re caught up"))
                .child(meta("No decisions or blockers in the spaces shown.", cx))
                .child(
                    button("inbox-view-activity")
                        .outline()
                        .small()
                        .tab_stop(true)
                        .label("View activity")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_inbox_tab(InboxTab::Activity, cx);
                            this.keyboard_focus.focus(window, cx);
                        })),
                );
        }
        needs = needs.children(banners);
        for (space, gap) in &gaps {
            needs = needs.child(gap_line(gap, "decisions", space, cx));
        }
        let current = state.current_decision().map(DecisionKey::from);
        for queued in state.needs_you() {
            let key = DecisionKey::from(queued);
            let open = current.as_ref() == Some(&key);
            let (question, _) = question_parts(&queued.decision.question);
            let project = state.project_name(&key.space, queued.decision.project_id);
            let place = inbox_place(&key.space, project);
            let when_label = when.moment(queued.decision.created_at);
            let label = disclosure_label(open, &question, &place);
            needs = needs.child(
                v_flex()
                    .gap(XS)
                    .child(
                        inbox_row(
                            format!("inbox-decision-{}-{}", key.space, key.id),
                            question,
                            place,
                            disclosure(open, when_label, cx),
                            cx,
                        )
                        .selected(open)
                        .accessibility_label(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this
                                .state
                                .current_decision()
                                .map(DecisionKey::from)
                                .as_ref()
                                == Some(&key)
                            {
                                this.state.clear_current_decision();
                                this.scroll_to_decision.set(DecisionScroll::None);
                            } else {
                                this.state.select_decision(key.clone());
                                this.scroll_to_decision.set(DecisionScroll::Card);
                            }
                            cx.notify();
                        })),
                    )
                    .children(open.then(|| self.decision(queued, when, cx))),
            );
        }
        needs
    }

    fn inbox_activity(&self, when: &When, cx: &mut Context<Self>) -> Div {
        let state = &self.state;
        let running = self.inbox_tasks(
            "Running tasks",
            &state.running(),
            Part::Running,
            None,
            when,
            cx,
        );
        let failed = self.inbox_tasks(
            "Recent failures",
            &state.recent_failures(when),
            Part::Tasks,
            Some((INBOX_FAILURES_SHOWN, state.inbox_show_all_failures())),
            when,
            cx,
        );

        let held = state.held_lines(when);
        let recent = self.timeline(when, cx);
        let empty = running.is_none() && failed.is_none() && recent.is_none() && held.is_empty();
        v_flex()
            .gap(LG)
            .pb(SM)
            .children(held.into_iter().map(|line| meta(line, cx)))
            .children(running)
            .children(failed)
            .children(recent)
            .children(empty.then(|| meta("No activity in the spaces shown.", cx).pt(LG)))
    }

    fn inbox_tasks(
        &self,
        title: &'static str,
        tasks: &[(&str, &TaskSummary)],
        part: Part,
        fold: Option<(usize, bool)>,
        when: &When,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let gaps = self.state.gaps(part);
        if tasks.is_empty() && gaps.is_empty() {
            return None;
        }
        let visible = match fold {
            Some((limit, false)) => tasks.len().min(limit),
            _ => tasks.len(),
        };
        let mut header = h_flex()
            .gap(SM)
            .items_center()
            .children(count(tasks.len(), cx));
        if let Some((limit, show_all)) = fold
            && tasks.len() > limit
        {
            header = header.child(
                button("inbox-toggle-failures")
                    .small()
                    .ghost()
                    .tab_stop(true)
                    .label(if show_all {
                        "Show fewer".to_owned()
                    } else {
                        format!("Show all {}", tasks.len())
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.toggle_inbox_all_failures();
                        cx.notify();
                    })),
            );
        }
        let mut section = v_flex().gap(XS).child(section(title).child(header));
        for (space, gap) in &gaps {
            section = section.child(gap_line(
                gap,
                if part == Part::Running {
                    "running tasks"
                } else {
                    "tasks"
                },
                space,
                cx,
            ));
        }
        for &(space, summary) in &tasks[..visible] {
            let task = &summary.task;
            let screen = Screen::Task {
                space: space.to_owned(),
                task: task.id,
            };
            let mut place = format!(
                "{} · {space} / {} · {}",
                task.id, summary.project, summary.agent
            );
            if part == Part::Running
                && let Some(machine) = &summary.machine
            {
                place.push_str(" · ");
                place.push_str(machine);
            }
            let time = if part == Part::Running {
                summary.running_since.map(|at| when.since(at))
            } else {
                Some(summary.completed_at.map_or_else(
                    || format!("Created {}", when.moment(task.created_at)),
                    |at| when.moment(at),
                ))
            };
            section = section.child(
                inbox_row(
                    format!("{part:?}-{space}-{}", task.id),
                    task.title.clone(),
                    place,
                    h_flex()
                        .gap(XS)
                        .children(time.map(|time| meta(time, cx)))
                        .child(
                            Icon::new(AppIcon::ChevronRight)
                                .small()
                                .text_color(cx.theme().muted_foreground),
                        ),
                    cx,
                )
                .accessibility_label(format!(
                    "Open {} {} in {space} / {}",
                    task.id, task.title, summary.project
                ))
                .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
            );
        }

        Some(section)
    }

    fn timeline(&self, when: &When, cx: &mut Context<Self>) -> Option<Div> {
        let state = &self.state;
        let all_entries = state.timeline();
        let gaps = state.gaps(Part::Recent);
        let has_earlier = state.timeline_has_earlier();
        if all_entries.is_empty() && gaps.is_empty() && !has_earlier {
            return None;
        }
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

        let mut header_actions = h_flex().gap(XS).flex_wrap();
        if quiet_count > 0 {
            header_actions = header_actions.child(
                button("inbox-toggle-quiet")
                    .small()
                    .ghost()
                    .tab_stop(true)
                    .label(if show_quiet {
                        "Hide quiet updates".to_owned()
                    } else {
                        format!(
                            "Show {quiet_count} quiet {}",
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
                    .tab_stop(true)
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
            .gap(XS)
            .child(section("Recent updates").flex_wrap().child(header_actions));
        for (space, gap) in &gaps {
            list = list.child(gap_line(gap, "recent results", space, cx));
        }
        if filtered.is_empty() && quiet_count > 0 {
            list = list.child(meta(
                "Only quiet routine updates recently. Click above to view them.",
                cx,
            ));
        }
        list = list.children(
            filtered[..visible_len]
                .iter()
                .map(|entry| self.timeline_entry(entry, when, cx)),
        );
        if has_earlier && (show_all_recent || filtered.len() <= INBOX_RECENT_SHOWN) {
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
        Some(list)
    }

    fn timeline_entry(
        &self,
        entry: &TimelineEntry<'_>,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let space = entry.space;
        let id = entry.prose_id();
        let text = entry.text();
        let title = update_title(&text, !entry.images.is_empty()).to_owned();
        let place = inbox_place(space, entry.project);
        let open = self.inbox.open_update.as_ref() == Some(&id);
        let (who, source, notice) = match entry.spoken {
            Spoken::Said(_, _, author) => (
                author.label(),
                match author {
                    Author::ProjectLead(project) => Some(project.as_str()),
                    _ => None,
                },
                false,
            ),
            Spoken::Delivered(item) if item.kind() == OutboxKind::SystemNotice => {
                ("Notice".to_owned(), None, true)
            }
            Spoken::Delivered(_) => ("Lead".to_owned(), None, false),
        };
        let conversation = |project: &str| Screen::Project {
            space: space.to_owned(),
            project: project.to_owned(),
            tab: crate::Tab::Conversation,
        };
        let screen = entry.project.map(conversation);
        let label = disclosure_label(open, &title, &place);
        let toggle_id = id.clone();
        let row = inbox_row(
            format!("timeline-toggle-{id}"),
            title,
            format!("{place} · {who}"),
            disclosure(open, when.moment(entry.at), cx),
            cx,
        )
        .selected(open)
        .accessibility_label(label)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.inbox.open_update =
                (this.inbox.open_update.as_ref() != Some(&toggle_id)).then(|| toggle_id.clone());
            cx.notify();
        }));
        let details = open.then(|| {
            let digest = match entry.spoken {
                Spoken::Delivered(item) => self.state.digest(item),
                Spoken::Said(..) => None,
            };
            let body = if let Some(digest) = digest {
                Some(self.digest_entry(space, digest, cx).into_any_element())
            } else {
                (!text.trim().is_empty()).then(|| {
                    self.prose(
                        id.clone(),
                        space,
                        entry.repository_project(),
                        text.clone(),
                        cx,
                    )
                    .into_any_element()
                })
            };
            let open_conversation = |key, label, screen: Screen| {
                button(key)
                    .small()
                    .ghost()
                    .tab_stop(true)
                    .label(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
            };
            let actions = h_flex()
                .flex_wrap()
                .gap(XS)
                .children(screen.clone().map(|screen| {
                    open_conversation(
                        format!("timeline-{id}"),
                        "Open conversation".to_owned(),
                        screen,
                    )
                }))
                .children(source.map(|project| {
                    open_conversation(
                        format!("timeline-from-{id}"),
                        format!("Open {project}"),
                        conversation(project),
                    )
                }));
            v_flex()
                .gap(SM)
                .p(MD)
                .bg(cx.theme().group_box)
                .rounded(cx.theme().radius)
                .children(body)
                .children(crate::components::chat_images::previews(entry.images, cx))
                .children((screen.is_some() || source.is_some()).then_some(actions))
        });
        let reply = screen
            .filter(|screen| !notice && self.main_chat_screen(space).as_ref() == Some(screen));
        let view = cx.entity().downgrade();
        v_flex()
            .id(SharedString::from(format!("timeline-menu-{id}")))
            .gap(XS)
            .child(row)
            .children(details)
            .context_menu(move |menu, _, _| {
                let copy = text.clone();
                let mut menu = menu.item(menu_item(&view, "Copy text", move |this, _, cx| {
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
            })
            .into_any_element()
    }

    pub(crate) fn open_inbox_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.target().is_none() {
            return;
        }
        if !self.inbox.composing {
            self.inbox.composer_return_focus = window.focused(cx);
            self.inbox.composing = true;
        }
        self.scroll_to_decision.set(DecisionScroll::None);
        let focus = self.composer.focus_handle(cx);
        window.on_next_frame(move |window, cx| focus.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn close_inbox_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.inbox.composing = false;
        self.inbox
            .composer_return_focus
            .take()
            .filter(|focus| self.keyboard_focus.contains(focus, window))
            .unwrap_or_else(|| self.keyboard_focus.clone())
            .focus(window, cx);
        cx.notify();
    }

    fn home_composer(&self, cx: &mut Context<Self>) -> Div {
        let recipient = self.state.target().map(|current| {
            self.to_picker(
                current,
                |state, space, project| state.set_target(space, project),
                cx,
            )
        });
        v_flex()
            .flex_none()
            .gap(XS)
            .child(
                h_flex()
                    .justify_between()
                    .gap(SM)
                    .child(
                        div().max_w_full().min_w_0().flex_1().child(
                            recipient.unwrap_or_else(|| {
                                meta("No lead available", cx).into_any_element()
                            }),
                        ),
                    )
                    .child(
                        icon_button(
                            "close-inbox-composer",
                            AppIcon::X,
                            "Close composer".to_owned(),
                        )
                        .tab_stop(true)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_inbox_composer(window, cx);
                        })),
                    ),
            )
            .child(self.render_composer(Draft::Lead, "Message the lead", None, cx))
    }
}

fn inbox_place(space: &str, project: Option<&str>) -> String {
    project.map_or_else(
        || space.to_owned(),
        |project| format!("{space} / {project}"),
    )
}

fn update_title(text: &str, has_images: bool) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| {
            let heading = line.trim_start_matches('#');
            heading.strip_prefix(' ').map_or(line, str::trim_start)
        })
        .unwrap_or(if has_images {
            "Image attachment"
        } else {
            "Update"
        })
}

fn disclosure_label(open: bool, title: &str, place: &str) -> String {
    let action = if open { "Hide" } else { "Show" };
    format!("{action} details for {title} ({place})")
}

fn disclosure(open: bool, time: String, cx: &App) -> Div {
    h_flex().gap(XS).child(meta(time, cx)).child(
        Icon::new(if open {
            AppIcon::ChevronDown
        } else {
            AppIcon::ChevronRight
        })
        .small()
        .text_color(cx.theme().muted_foreground),
    )
}

fn inbox_row(
    id: impl Into<ElementId>,
    title: String,
    context: String,
    trailing: Div,
    cx: &App,
) -> Button {
    button(id)
        .ghost()
        .w_full()
        .h_auto()
        .min_h(ROW_HEIGHT)
        .py(XS)
        .px(SM)
        .tab_stop(true)
        .tooltip(format!("{title} — {context}"))
        .child(
            h_flex()
                .w_full()
                .gap(MD)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_size(TYPE_BODY)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(meta(context, cx).truncate()),
                )
                .child(trailing.flex_none()),
        )
}

#[cfg(test)]
mod tests {
    use super::update_title;

    #[test]
    fn compact_updates_use_the_first_nonblank_line() {
        assert_eq!(update_title("\n  ## Result \nMore detail", false), "Result");
        assert_eq!(update_title("#578 was merged", false), "#578 was merged");
        assert_eq!(update_title("   ", true), "Image attachment");
    }
}
