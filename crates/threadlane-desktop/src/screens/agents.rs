//! The agents of each space (wireframe 6): the definitions as cards, and
//! the modes grid, one row per project and one cell per agent. A cell
//! changes that agent's mode in that project.

use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_desktop::Part;

use super::project::{MODES, mode_name};
use super::{gap_line, heading, section};
use crate::Desktop;

/// The width of one grid column.
const CELL: f32 = 130.;

impl Desktop {
    pub(crate) fn agents(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let view = cx.entity().downgrade();
        let mut screen = v_flex()
            .id("agents")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_4()
            .child(heading("Agents"))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("Agents are defined per space."),
            );
        for space in self.state.spaces() {
            let mut cards = h_flex().gap_2().flex_wrap().items_start();
            for agent in &space.agents {
                let backends = if agent.backends.is_empty() {
                    "Either backend".to_owned()
                } else {
                    agent
                        .backends
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" then ")
                };
                cards = cards.child(
                    v_flex()
                        .w(px(220.))
                        .gap_1()
                        .p_3()
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(cx.theme().radius)
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(agent.name.clone()),
                        )
                        .child(div().text_sm().child(agent.role.clone()))
                        .child(div().text_sm().text_color(muted).child(backends)),
                );
            }

            let grid = self.state.modes(&space.name);
            let mut table = v_flex().gap_1().child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(muted)
                    .child(div().w(px(CELL)).flex_none().child("Project"))
                    .children(
                        grid.agents
                            .iter()
                            .map(|agent| div().w(px(CELL)).flex_none().child(agent.clone())),
                    ),
            );
            for row in grid.rows {
                let mut line = h_flex().gap_2().child(
                    div()
                        .w(px(CELL))
                        .flex_none()
                        .text_sm()
                        .child(row.project.clone()),
                );
                match row.modes {
                    Err(gap) => line = line.child(gap_line(&gap, "the modes", cx)),
                    Ok(modes) => {
                        for (agent, mode) in grid.agents.iter().zip(modes) {
                            let (space, project, agent) =
                                (space.name.clone(), row.project.clone(), agent.clone());
                            let view = view.clone();
                            let id = format!("mode-{space}-{project}-{agent}");
                            let label = mode.map_or("—", mode_name);
                            let cell = Button::new(SharedString::from(id))
                                .small()
                                .w(px(CELL))
                                .label(label)
                                .accessibility_label(format!("{agent} in {project}: {label}"))
                                .dropdown_menu(move |mut menu, _, _| {
                                    for (choice, name) in MODES {
                                        let (space, project, agent, view) = (
                                            space.clone(),
                                            project.clone(),
                                            agent.clone(),
                                            view.clone(),
                                        );
                                        menu = menu.item(PopupMenuItem::new(name).on_click(
                                            move |_, _, cx| {
                                                let _gone = view.update(cx, |this, cx| {
                                                    this.set_mode(
                                                        space.clone(),
                                                        project.clone(),
                                                        agent.clone(),
                                                        choice,
                                                        cx,
                                                    );
                                                });
                                            },
                                        ));
                                    }
                                    menu
                                });
                            line = line.child(cell);
                        }
                    }
                }
                table = table.child(line);
            }

            let mut part = v_flex()
                .gap_2()
                .child(section(format!("Definitions in {}", space.name)));
            if let Some(gap) = space.gap(Part::Agents) {
                part = part.child(gap_line(&gap, "the agents", cx));
            }
            part = part
                .child(cards)
                .child(section(format!("Modes by project in {}", space.name)).pt_2());
            if let Some(gap) = space.gap(Part::Projects) {
                part = part.child(gap_line(&gap, "the projects", cx));
            }
            screen = screen.child(part.child(table));
        }
        screen.into_any_element()
    }
}
