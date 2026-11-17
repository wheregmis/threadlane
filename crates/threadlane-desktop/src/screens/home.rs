//! Home (wireframe 1): everything that needs you across every space, then
//! what is running, then what finished, and a composer to any project.

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Selectable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_desktop::When;

use super::{heading, section};
use crate::{Desktop, Screen};

impl Desktop {
    pub(crate) fn home(&self, when: &When, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let state = &self.state;

        let mut chips = h_flex().gap_2().child(
            Button::new("filter-all")
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
                Button::new(SharedString::from(format!("filter-{name}")))
                    .label(name.clone())
                    .selected(state.filter() == Some(name.as_str()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.set_filter(Some(&name));
                        cx.notify();
                    })),
            );
        }

        let banners = state
            .spaces()
            .iter()
            .filter(|space| state.filter().is_none_or(|only| only == space.name))
            .filter_map(|space| {
                let text = state.blocked(&space.name)?;
                Some(
                    Alert::warning(SharedString::from(format!("blocked-{}", space.name)), text)
                        .title(format!("System · {} is blocked", space.name)),
                )
            });

        let mut needs = v_flex().gap_2().child(section(format!(
            "Needs you ({})",
            state.needs_you().count()
        )));
        if state.spaces().is_empty() {
            needs = needs.child(div().text_color(muted).child(
                "No hub is paired. Pair one with `threadlane hub add <name> <data-dir>` or \
                 `threadlane hub join <name> <code>`, then reopen the app.",
            ));
        } else if state.needs_you().count() == 0 {
            needs = needs.child(div().text_color(muted).child("Nothing waits on you."));
        } else {
            needs = needs.child(self.note_field());
        }
        for queued in state.needs_you() {
            needs = needs.child(self.decision(queued, cx));
        }

        let running = state.running();
        let mut table = v_flex()
            .gap_1()
            .child(section(format!("Running now ({})", running.len())));
        if running.is_empty() {
            table = table.child(div().text_color(muted).child("Nothing is running."));
        } else {
            table = table.child(
                row(["Task", "Project", "Agent", "Machine", "Running"])
                    .text_xs()
                    .text_color(muted),
            );
        }
        for (space, summary) in running {
            let task = &summary.task;
            let screen = Screen::Task {
                space: space.to_owned(),
                task: task.id,
            };
            table = table.child(
                row([
                    format!("{} {}", task.id, task.title),
                    summary.project.clone(),
                    summary.agent.clone(),
                    summary.machine.clone().unwrap_or_default(),
                    summary
                        .running_since
                        .map(|since| when.since(since))
                        .unwrap_or_default(),
                ])
                .id(SharedString::from(format!("running-{space}-{}", task.id)))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
            );
        }

        let mut recent = v_flex().gap_1().child(section("Recent results"));
        let results = state.recent();
        if results.is_empty() {
            recent = recent.child(div().text_color(muted).child("Nothing yet."));
        }
        for item in results {
            recent = recent.child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .w(px(110.))
                            .flex_none()
                            .text_sm()
                            .text_color(muted)
                            .child(when.moment(item.item.delivered_at)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(item.label().replace('/', " / ")),
                    )
                    .child(div().text_sm().min_w_0().child(item.text())),
            );
        }

        v_flex()
            .id("home")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_5()
            .child(heading("Home"))
            .child(chips)
            .child(div().text_sm().text_color(muted).child(state.status_line()))
            .children(banners)
            .child(needs)
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
        let picker = Button::new("target")
            .label(label)
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
        h_flex()
            .gap_2()
            .pt_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(picker)
            .child(div().flex_1().child(Input::new(&self.composer)))
            .child(
                Button::new("send")
                    .primary()
                    .label("Send")
                    .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
            )
    }
}

/// One row of the running table, its cells at fixed widths.
fn row(cells: [impl Into<SharedString>; 5]) -> Div {
    let widths = [320., 140., 110., 110., 110.];
    h_flex()
        .gap_2()
        .py_1()
        .children(cells.into_iter().zip(widths).map(|(cell, width)| {
            div()
                .w(px(width))
                .flex_none()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_sm()
                .child(cell.into())
        }))
}
