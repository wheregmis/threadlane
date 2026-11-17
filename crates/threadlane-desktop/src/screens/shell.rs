//! The shell's sidebar (wireframe "Home", left): Home with what needs you,
//! each paired space with its reachability and projects, Agents, Machines.

use gpui_kit::component::sidebar::{
    Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::*;

use crate::{Desktop, Screen, Tab};

impl Desktop {
    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let needs_you = self.state.queue().len();
        let home = SidebarMenuItem::new(count("Home", needs_you))
            .active(self.screen == Screen::Home)
            .on_click(cx.listener(|this, _, _, cx| this.go(Screen::Home, cx)));
        let mut groups =
            vec![SidebarGroup::new("Threadlane").child(SidebarMenu::new().child(home))];

        for space in self.state.spaces() {
            let reach = if space.unreachable.is_none() {
                "● reachable"
            } else {
                "○ out of reach"
            };
            let items = space.projects.iter().map(|project| {
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
                let open = self.state.open_in(&space.name, project.id);
                SidebarMenuItem::new(count(&project.name, open))
                    .active(active)
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
            });
            groups.push(
                SidebarGroup::new(format!("{} {reach}", space.name))
                    .child(SidebarMenu::new().children(items)),
            );
        }

        let (online, machines) = self.state.spaces().iter().fold((0, 0), |(on, all), space| {
            (on + space.online(), all + space.machines.len())
        });
        groups.push(
            SidebarGroup::new("Team").child(
                SidebarMenu::new()
                    .child(
                        SidebarMenuItem::new("Agents")
                            .active(self.screen == Screen::Agents)
                            .on_click(cx.listener(|this, _, _, cx| this.go(Screen::Agents, cx))),
                    )
                    .child(
                        SidebarMenuItem::new(format!("Machines  {online} / {machines}"))
                            .active(self.screen == Screen::Machines)
                            .on_click(cx.listener(|this, _, _, cx| this.go(Screen::Machines, cx))),
                    ),
            ),
        );

        Sidebar::new("sidebar")
            .w(px(220.))
            .header(SidebarHeader::new().child("Threadlane"))
            .children(groups)
    }
}

/// `Payments API  1`, or just the label when nothing waits.
fn count(label: &str, count: usize) -> String {
    if count == 0 {
        label.to_owned()
    } else {
        format!("{label}  {count}")
    }
}
