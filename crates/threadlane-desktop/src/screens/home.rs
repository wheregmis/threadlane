//! Home (wireframe 1): everything that needs you across every space, then
//! what is running, then what finished, and a composer to any project.

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_desktop::{Part, When, running_elapsed};

use super::gap_line;
use crate::components::{
    button, composer, count, empty_state, field, heading, meta, section, status_dot,
};
use crate::icons::AppIcon;
use crate::tokens::{
    CONTROL_HEIGHT, LG, READING_WIDTH, ROW_HEIGHT, SM, Status, TIMESTAMP_WIDTH, TYPE_BODY,
    TYPE_META, XL, XS,
};
use crate::{Desktop, Screen};

impl Desktop {
    pub(crate) fn home(
        &self,
        when: &When,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let state = &self.state;

        let mut chips = h_flex().gap(XS).flex_wrap().child(
            button("filter-all")
                .label("All")
                .selected(state.filter().is_none())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.set_filter(None);
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
            needs = needs.child(gap_line(gap, &format!("the decisions of {space}"), cx));
        }
        if state.spaces().is_empty() {
            needs = needs.child(div().text_color(muted).child(
                "No hub is paired. Pair one with `threadlane hub add <name> <data-dir>` or \
                 `threadlane hub join <name> <code>`, then reopen the app.",
            ));
        } else if state.needs_you().count() == 0 && gaps.is_empty() {
            needs = needs.child(empty_state("Nothing waits on you.", None, cx));
        }
        for queued in state.needs_you() {
            needs = needs.child(self.decision(queued, window, cx));
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
        let mut table = v_flex()
            .gap(XS)
            .child(section("Running now").children(count(running.len(), cx)));
        let gaps = state.gaps(Part::Running);
        for (space, gap) in &gaps {
            table = table.child(gap_line(gap, &format!("what runs in {space}"), cx));
        }
        if running.is_empty() && gaps.is_empty() {
            table = table.child(empty_state("Nothing is running.", None, cx));
        } else if !running.is_empty() {
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

        let mut recent = v_flex().gap(XS).child(section("Recent results"));
        let results = state.recent();
        let gaps = state.gaps(Part::Recent);
        for (space, gap) in &gaps {
            recent = recent.child(gap_line(gap, &format!("the results of {space}"), cx));
        }
        if results.is_empty() && gaps.is_empty() {
            recent = recent.child(empty_state("Nothing yet.", None, cx));
        }
        for item in results {
            let digest = state
                .digest(item)
                .map(|digest| self.digest_entry(&item.space, digest, cx));
            let excerpt = digest.is_none().then(|| state.excerpt(item));
            let (space, seq) = (item.space.clone(), item.item.seq);
            let toggle = excerpt
                .as_ref()
                .and_then(|excerpt| excerpt.toggle)
                .map(|label| {
                    button(SharedString::from(format!("result-{space}-{seq}")))
                        .link()
                        .small()
                        .label(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.toggle_result(&space, seq);
                            cx.notify();
                        }))
                });
            recent = recent.child(
                h_flex()
                    .items_start()
                    .gap(SM)
                    .child(
                        meta(when.moment(item.item.delivered_at), cx)
                            .w(TIMESTAMP_WIDTH)
                            .flex_none()
                            .text_right(),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .items_start()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(item.label().replace('/', " / ")),
                            )
                            .children(digest)
                            .children(
                                excerpt
                                    .map(|excerpt| div().text_size(TYPE_BODY).child(excerpt.text)),
                            )
                            .children(toggle),
                    ),
            );
        }

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .max_w(READING_WIDTH)
            .self_center()
            .overflow_y_scrollbar()
            .id("home")
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
            .child(self.home_composer(cx))
            .into_any_element()
    }

    /// The composer with its project picker (Plan.md §2.1: every message
    /// goes to one project's lead).
    fn home_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self.state.target().map_or_else(
            || "No project".to_owned(),
            |(space, project)| format!("{space} / {project} ▾"),
        );
        let view = cx.entity().downgrade();
        let targets: Vec<(String, String)> = self
            .state
            .targets()
            .into_iter()
            .map(|(space, project)| (space.to_owned(), project.to_owned()))
            .collect();
        let accessibility = format!("Choose a project · {label}");
        let picker = button("target")
            .label(label)
            .tab_stop(true)
            .accessibility_label(accessibility)
            .dropdown_menu(move |mut menu, _, _| {
                for (space, project) in &targets {
                    let (space, project, view) = (space.clone(), project.clone(), view.clone());
                    menu = menu.item(PopupMenuItem::new(format!("{space} / {project}")).on_click(
                        move |_, _, cx| {
                            let _gone = view.update(cx, |this, cx| {
                                this.state.set_target(&space, &project);
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            });
        composer(cx)
            .child(picker)
            .child(div().flex_1().min_w_0().child(field(&self.composer)))
            .child(
                button("send")
                    .primary()
                    .icon(AppIcon::Send)
                    .label("Send")
                    .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
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
