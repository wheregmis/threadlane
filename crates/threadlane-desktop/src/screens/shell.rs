//! The shell's sidebar (wireframe "Home", left): Home with what needs you,
//! each paired space with its reachability and projects, Agents, Routines,
//! Machines, Settings.
//! Every row is a GPUI Kit button, so assistive technology names it and
//! can press it.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Selectable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_desktop::{Activity, Part, Reach};

use super::{gap_line, section};
use crate::{Desktop, Screen, Tab};

impl Desktop {
    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let needs_you = self.state.queue().len();
        let mut bar = v_flex()
            .id("sidebar")
            .w(px(220.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .gap_1()
            .p_2()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(section("Threadlane"))
            .child(row(
                "go-home",
                count("Home", needs_you),
                self.screen == Screen::Home,
                cx.listener(|this, _, _, cx| this.go(Screen::Home, cx)),
            ));

        for space in self.state.spaces() {
            let reach = match space.reach {
                Reach::Unknown => "◌ asking…",
                Reach::Reachable => "● reachable",
                Reach::Unreachable(_) => "○ out of reach",
            };
            bar = bar.child(section(format!("{} {reach}", space.name)).pt_3());
            if let Some(gap) = space.gap(Part::Projects) {
                bar = bar.child(gap_line(&gap, "projects", cx));
            }
            for project in &space.projects {
                let screen = Screen::Project {
                    space: space.name.clone(),
                    project: project.name.clone(),
                    tab: Tab::Tasks,
                };
                let active = matches!(
                    &self.screen,
                    Screen::Project { space: open, project: name, .. }
                        if *open == space.name && *name == project.name
                );
                let open = self.state.open_in(&space.name, project.id)
                    + self.state.attention_in(&space.name, &project.name);
                bar = bar.child(row(
                    format!("go-{}-{}", space.name, project.name),
                    count(&project.name, open),
                    active,
                    cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)),
                ));
            }
            let screen = Screen::NewProject {
                space: space.name.clone(),
            };
            bar = bar.child(row(
                format!("new-project-{}", space.name),
                "+ New project".to_owned(),
                self.screen == screen,
                cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)),
            ));
        }

        let machines = match self.state.machine_count() {
            Some((online, all)) => format!("Machines  {online} / {all}"),
            None => "Machines".to_owned(),
        };
        bar.child(section("Team").pt_3())
            .child(row(
                "go-agents",
                "Agents".to_owned(),
                self.screen == Screen::Agents,
                cx.listener(|this, _, _, cx| this.go(Screen::Agents, cx)),
            ))
            .child(row(
                "go-routines",
                "Routines".to_owned(),
                self.screen == Screen::Routines,
                cx.listener(|this, _, _, cx| this.go(Screen::Routines, cx)),
            ))
            .child(row(
                "go-machines",
                machines,
                self.screen == Screen::Machines,
                cx.listener(|this, _, _, cx| this.go(Screen::Machines, cx)),
            ))
            .child(row(
                "go-settings",
                "Settings".to_owned(),
                self.screen == Screen::Settings,
                cx.listener(|this, _, _, cx| this.go(Screen::Settings, cx)),
            ))
    }
}

impl Desktop {
    /// The status area under every screen: the last action and where it
    /// stands, until the next one.
    pub(crate) fn activity(&self, cx: &App) -> Option<Div> {
        let activity = self.state.activity()?;
        let (mark, color) = match activity {
            Activity::Working(_) => ("…", cx.theme().muted_foreground),
            Activity::Done(_) => ("✓", cx.theme().foreground),
            Activity::Failed(_) => ("!", cx.theme().danger),
        };
        Some(
            h_flex()
                .flex_none()
                .gap_2()
                .pt_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .text_sm()
                .text_color(color)
                .child(mark)
                .child(div().min_w_0().child(activity.text().to_owned())),
        )
    }
}

/// One sidebar row: a full-width ghost button named by its label.
fn row(
    id: impl Into<SharedString>,
    label: String,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    Button::new(id.into())
        .ghost()
        .w_full()
        .selected(active)
        .accessibility_label(label.clone())
        // A child rather than a label, which the button would center.
        .child(div().w_full().child(label))
        .on_click(on_click)
}

/// `Payments API  1`, or just the label when nothing waits.
fn count(label: &str, count: usize) -> String {
    if count == 0 {
        label.to_owned()
    } else {
        format!("{label}  {count}")
    }
}
