//! The agents of each space (wireframe 6): the definitions as cards, with
//! "+ New agent" and "Copy an agent to <space>…", and the modes grid, one
//! row per project and one cell per agent. A cell changes that agent's mode
//! in that project, binding it there from "—".

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::group_box::GroupBox;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::model::BackendKind;
use threadlane_desktop::{EmptyRegion, Part};

use super::agent_edit::backend_picker;
use super::project::{MODES, mode_name};
use super::{empty, gap_line};
use crate::Desktop;
use crate::components::{button, card, field, heading, section};
use crate::icons::AppIcon;
use crate::tokens::{
    AGENT_CARD_WIDTH, FORM_WIDTH, GRID_CELL_WIDTH, LG, SM, TYPE_BODY, TYPE_META, XS,
};

impl Desktop {
    pub(crate) fn agents(&self, scope: Option<&str>, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let view = cx.entity().downgrade();
        let mut screen = v_flex()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .id("agents")
            .gap(LG)
            .child(heading("Team"));
        for space in self
            .state
            .spaces()
            .iter()
            .filter(|space| scope.is_none_or(|name| space.name == name))
        {
            let mut cards = h_flex().gap(SM).flex_wrap().items_start();
            for agent in &space.agents {
                let backends = if agent.backends.is_empty() {
                    "Any backend".to_owned()
                } else {
                    agent
                        .backends
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" then ")
                };
                let open = {
                    let (space, agent) = (space.name.clone(), agent.name.clone());
                    move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                        this.open_agent_edit(&space, &agent, window, cx);
                    }
                };
                let edit = open.clone();
                let id = format!("agent-card-{}-{}", space.name, agent.name);
                cards = cards.child(
                    div()
                        .id(SharedString::from(id.clone()))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| open(this, window, cx)))
                        .child(
                            card(cx)
                                .w(AGENT_CARD_WIDTH)
                                .child(
                                    h_flex()
                                        .justify_between()
                                        .items_center()
                                        .gap(SM)
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(agent.name.clone()),
                                        )
                                        .child(
                                            button(SharedString::from(format!("{id}-edit")))
                                                .small()
                                                .ghost()
                                                .label("Edit")
                                                .accessibility_label(format!("Edit {}", agent.name))
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        cx.stop_propagation();
                                                        edit(this, window, cx);
                                                    },
                                                )),
                                        ),
                                )
                                .child(div().text_size(TYPE_BODY).child(agent.role.clone()))
                                .child(
                                    div()
                                        .text_size(TYPE_BODY)
                                        .text_color(muted)
                                        .child(backends)
                                        .child(self.skill_picker(false, cx)),
                                )
                                .children(self.state.agent_runs_on(&space.name, agent).map(
                                    |line| div().text_size(TYPE_BODY).text_color(muted).child(line),
                                )),
                        ),
                );
            }

            let grid = self.state.modes(&space.name);
            let mut table =
                v_flex().gap(XS).child(
                    h_flex()
                        .gap(SM)
                        .text_size(TYPE_META)
                        .text_color(muted)
                        .child(div().w(GRID_CELL_WIDTH).flex_none().child("Project"))
                        .children(grid.agents.iter().map(|agent| {
                            div().w(GRID_CELL_WIDTH).flex_none().child(agent.clone())
                        })),
                );
            for row in grid.rows {
                let mut line = h_flex().gap(SM).child(
                    div()
                        .w(GRID_CELL_WIDTH)
                        .flex_none()
                        .text_size(TYPE_BODY)
                        .child(row.project.clone()),
                );
                match row.modes {
                    Err(gap) => line = line.child(gap_line(&gap, "agent modes", &space.name, cx)),
                    Ok(modes) => {
                        for (agent, mode) in grid.agents.iter().zip(modes) {
                            let (space, project, agent) =
                                (space.name.clone(), row.project.clone(), agent.clone());
                            let view = view.clone();
                            let id = format!("mode-{space}-{project}-{agent}");
                            let label = mode.map_or("—", mode_name);
                            let cell = button(SharedString::from(id))
                                .small()
                                .w(GRID_CELL_WIDTH)
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

            let mut actions = h_flex().gap(SM).flex_wrap().child(
                button(SharedString::from(format!("new-agent-{}", space.name)))
                    .small()
                    .primary()
                    .icon(AppIcon::UserPlus)
                    .label("New agent")
                    .accessibility_label(format!("New agent in {}", space.name))
                    .on_click({
                        let space = space.name.clone();
                        cx.listener(move |this, _, _, cx| {
                            this.open_agent_form(space.clone(), cx);
                        })
                    }),
            );
            for (to, agents) in self.state.copy_targets(&space.name) {
                let label = format!("Copy an agent to {to}…");
                let view = view.clone();
                let button = button(SharedString::from(format!(
                    "copy-agents-{}-{to}",
                    space.name
                )))
                .small()
                .icon(AppIcon::Copy)
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
            let mut part = v_flex().gap(SM).child(
                h_flex()
                    .justify_between()
                    .gap(SM)
                    .child(section(format!("Definitions in {}", space.name)))
                    .child(actions),
            );
            if self.state.agent_form().space.as_deref() == Some(space.name.as_str()) {
                part = part.child(self.agent_form(&space.name, cx));
            }
            if self.state.agent_edit().map(|edit| edit.space.as_str()) == Some(space.name.as_str())
                && let Some(view) = self.agent_edit_view(cx)
            {
                part = part.child(view);
            }
            if let Some(gap) = space.gap(Part::Agents) {
                part = part.child(gap_line(&gap, "agents", &space.name, cx));
            } else if space.agents.is_empty() {
                part = part.child(empty(EmptyRegion::Agents, cx));
            }
            part = part
                .child(cards)
                .child(section(format!("Modes by project in {}", space.name)).pt(SM));
            if let Some(gap) = space.gap(Part::Projects) {
                part = part.child(gap_line(&gap, "projects", &space.name, cx));
            }
            screen = screen.child(
                part.child(
                    table
                        .overflow_x_scrollbar()
                        .id(SharedString::from(format!("mode-grid-{}", space.name))),
                ),
            );
        }
        screen.into_any_element()
    }
}

impl Desktop {
    /// The "+ New agent" form: name, role, instructions, and the backends
    /// in order of preference.
    fn agent_form(&self, space: &str, cx: &mut Context<Self>) -> GroupBox {
        let backends = backend_picker(
            "agent-backends",
            self.state.agent_form().backends,
            None,
            BackendKind::ALL,
            &self.agent_models,
            |this, index| this.state.agent_form_mut().backends = index,
            cx,
        );
        let view = cx.entity().downgrade();
        let (offered, note) = match &self.state.agent_form().templates {
            None => (Vec::new(), Some("Listing templates…".to_owned())),
            Some(Ok(templates)) if templates.is_empty() => {
                (Vec::new(), Some("This hub offers no templates.".to_owned()))
            }
            Some(Ok(templates)) => (templates.clone(), None),
            Some(Err(error)) => (Vec::new(), Some(format!("No templates: {error}"))),
        };
        let mut start = h_flex().gap(SM).items_center().child(
            button("agent-template")
                .small()
                .outline()
                .icon(AppIcon::FileText)
                .label("Start from a template")
                .disabled(offered.is_empty())
                .dropdown_menu(move |mut menu, _, _| {
                    for template in &offered {
                        let (id, view) = (template.id.clone(), view.clone());
                        let label = format!("{}: {}", template.name, template.role);
                        menu =
                            menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                let _gone = view.update(cx, |this, cx| {
                                    let Some(draft) =
                                        this.state.agent_form_mut().pick_template(&id)
                                    else {
                                        return;
                                    };
                                    for (input, value) in [
                                        (&this.agent_name, draft.name),
                                        (&this.agent_role, draft.role),
                                        (&this.agent_instructions, draft.instructions),
                                    ] {
                                        input.update(cx, |input, cx| {
                                            input.set_value(value, window, cx);
                                        });
                                    }
                                    cx.notify();
                                });
                            }));
                    }
                    menu
                }),
        );
        if let Some(note) = note {
            start = start.child(
                div()
                    .text_size(TYPE_META)
                    .text_color(cx.theme().muted_foreground)
                    .child(note),
            );
        }
        let space = space.to_owned();
        card(cx)
            .max_w(FORM_WIDTH)
            .child(section(format!("New agent in {space}")))
            .child(start)
            .child(field(&self.agent_name))
            .child(field(&self.agent_role))
            .child(field(&self.agent_instructions))
            .child(backends)
            .child(self.skill_picker(false, cx))
            .child(
                div()
                    .text_size(TYPE_META)
                    .text_color(cx.theme().muted_foreground)
                    .child("It is bound to no project yet: pick its mode in a — cell below."),
            )
            .child(
                h_flex()
                    .gap(SM)
                    .child(
                        button("agent-create")
                            .primary()
                            .icon(AppIcon::UserPlus)
                            .label("Create agent")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.create_agent(space.clone(), cx);
                            })),
                    )
                    .child(
                        button("agent-close")
                            .icon(AppIcon::X)
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.agent_form_mut().space = None;
                                cx.notify();
                            })),
                    ),
            )
    }
}
