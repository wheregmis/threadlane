//! The agents of each space (wireframe 6): the definitions as cards, with
//! "+ New agent" and "Copy an agent to <space>…", and the modes grid, one
//! row per project and one cell per agent. A cell changes that agent's mode
//! in that project, binding it there from "—".

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::*;
use threadlane_desktop::{BACKENDS, Part};

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
                        .child(div().text_sm().text_color(muted).child(backends))
                        .children(
                            self.state
                                .agent_runs_on(&space.name, agent)
                                .map(|line| div().text_sm().text_color(muted).child(line)),
                        ),
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

            let mut actions = h_flex().gap_2().flex_wrap().child(
                Button::new(SharedString::from(format!("new-agent-{}", space.name)))
                    .small()
                    .primary()
                    .label("+ New agent")
                    .accessibility_label(format!("New agent in {}", space.name))
                    .on_click({
                        let space = space.name.clone();
                        cx.listener(move |this, _, _, cx| {
                            this.state.agent_form_mut().space = Some(space.clone());
                            cx.notify();
                        })
                    }),
            );
            for (to, agents) in self.state.copy_targets(&space.name) {
                let label = format!("Copy an agent to {to}…");
                let view = view.clone();
                let button = Button::new(SharedString::from(format!(
                    "copy-agents-{}-{to}",
                    space.name
                )))
                .small()
                .label(label.clone())
                .accessibility_label(label)
                .disabled(agents.is_empty())
                .dropdown_menu(move |mut menu, _, _| {
                    for agent in &agents {
                        let (agent, to, view) = (agent.clone(), to.clone(), view.clone());
                        menu = menu.item(PopupMenuItem::new(agent.name.clone()).on_click(
                            move |_, _, cx| {
                                let _gone = view.update(cx, |this, cx| {
                                    this.copy_agent(agent.clone(), to.clone(), cx);
                                });
                            },
                        ));
                    }
                    menu
                });
                actions = actions.child(button);
            }
            let mut part = v_flex().gap_2().child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(section(format!("Definitions in {}", space.name)))
                    .child(actions),
            );
            if self.state.agent_form().space.as_deref() == Some(space.name.as_str()) {
                part = part.child(self.agent_form(&space.name, cx));
            }
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

impl Desktop {
    /// The "+ New agent" form: name, role, instructions, and the backends
    /// in order of preference.
    fn agent_form(&self, space: &str, cx: &mut Context<Self>) -> Div {
        let picked = self.state.agent_form().backends;
        let backends = ButtonGroup::new("agent-backends")
            .outline()
            .compact()
            .children(BACKENDS.iter().enumerate().map(|(index, (label, _))| {
                Button::new(SharedString::from(format!("agent-backends-{index}")))
                    .label(*label)
                    .selected(picked == index)
            }))
            .on_click(cx.listener(|this, clicked: &Vec<usize>, _, cx| {
                if let Some(index) = clicked.first() {
                    this.state.agent_form_mut().backends = *index;
                    cx.notify();
                }
            }));
        let space = space.to_owned();
        v_flex()
            .gap_2()
            .p_3()
            .max_w(px(560.))
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .child(section(format!("New agent in {space}")))
            .child(Input::new(&self.agent_name))
            .child(Input::new(&self.agent_role))
            .child(Input::new(&self.agent_instructions))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Backends, in order of preference"),
            )
            .child(backends)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("It is bound to no project yet: pick its mode in a — cell below."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("agent-create")
                            .primary()
                            .label("Create agent")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.create_agent(space.clone(), cx);
                            })),
                    )
                    .child(
                        Button::new("agent-close")
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.agent_form_mut().space = None;
                                cx.notify();
                            })),
                    ),
            )
    }
}
