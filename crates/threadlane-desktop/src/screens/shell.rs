//! Navigation, space reachability, and the last action.

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_desktop::{Activity, Part, Reach};

use super::gap_line;
use crate::components::{meta, row, status_dot};
use crate::icons::AppIcon;
use crate::tokens::{BAR_HEIGHT, LG, MD, SIDEBAR_WIDTH, SM, Status, TYPE_EMPHASIS, XS};
use crate::{Desktop, Screen, Tab};

impl Desktop {
    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let needs_you = self.state.queue().len();
        let mut bar = v_flex()
            .w(SIDEBAR_WIDTH)
            .flex_none()
            .h_full()
            .gap(XS)
            .p(SM)
            .pt(LG)
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .overflow_y_scrollbar()
            .id("sidebar")
            .child(
                h_flex()
                    .h(BAR_HEIGHT)
                    .px(MD)
                    .text_size(TYPE_EMPHASIS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Threadlane"),
            )
            .child(
                row(
                    "go-home",
                    AppIcon::House,
                    "Home",
                    positive_count(needs_you),
                    self.screen == Screen::Home,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| this.go(Screen::Home, cx))),
            );

        for space in self.state.spaces() {
            let (status, label) = match space.reach {
                Reach::Unknown => (Status::Idle, "Asking…"),
                Reach::Reachable => (Status::Done, "Reachable"),
                Reach::Unreachable(_) => (Status::Failed, "Out of reach"),
            };
            bar = bar.child(
                h_flex()
                    .px(MD)
                    .pt(MD)
                    .gap(XS)
                    .child(meta(space.name.clone(), cx).truncate())
                    .child(status_dot(status, cx))
                    .child(meta(label, cx).flex_none()),
            );
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
                bar = bar.child(
                    row(
                        format!("go-{}-{}", space.name, project.name),
                        AppIcon::Folder,
                        project.name.clone(),
                        positive_count(open),
                        active,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
                );
            }
            let screen = Screen::NewProject {
                space: space.name.clone(),
            };
            bar = bar.child(
                row(
                    format!("new-project-{}", space.name),
                    AppIcon::FolderPlus,
                    "New project",
                    None,
                    self.screen == screen,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
            );
        }

        let machines = self
            .state
            .machine_count()
            .filter(|&(_, all)| all != 0)
            .map(|(online, all)| format!("{online} / {all}").into());
        bar.child(meta("TEAM", cx).px(MD).pt(MD)).children(
            [
                ("go-agents", AppIcon::Bot, "Agents", None, Screen::Agents),
                (
                    "go-routines",
                    AppIcon::CalendarClock,
                    "Routines",
                    None,
                    Screen::Routines,
                ),
                (
                    "go-machines",
                    AppIcon::Monitor,
                    "Machines",
                    machines,
                    Screen::Machines,
                ),
                (
                    "go-settings",
                    AppIcon::Settings,
                    "Settings",
                    None,
                    Screen::Settings,
                ),
            ]
            .into_iter()
            .map(|(id, icon, label, count, screen)| {
                row(id, icon, label, count, self.screen == screen, cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
            })
            .collect::<Vec<_>>(),
        )
    }

    /// The last action and where it stands, until the next one.
    pub(crate) fn activity(&self, cx: &App) -> Option<Div> {
        let activity = self.state.activity()?;
        let status = match activity {
            Activity::Working(_) => Status::Running,
            Activity::Done(_) => Status::Done,
            Activity::Failed(_) => Status::Failed,
        };
        Some(
            v_flex()
                .flex_none()
                .gap(SM)
                .child(Separator::horizontal())
                .child(
                    h_flex()
                        .gap(SM)
                        .child(status_dot(status, cx))
                        .child(meta(activity.text().to_owned(), cx)),
                ),
        )
    }
}

fn positive_count(count: usize) -> Option<SharedString> {
    (count > 0).then(|| count.to_string().into())
}
