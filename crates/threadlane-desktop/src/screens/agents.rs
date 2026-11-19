//! Space agent profiles, project assignments, and team topology.

use gpui_kit::component::button::{ButtonGroup, ButtonVariants as _};
use gpui_kit::component::group_box::GroupBox;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_core::model::BackendKind;
use threadlane_desktop::{EmptyRegion, Part, Space, TeamViewMode, When, count_form};

use super::agent_edit::backend_picker;
use super::{empty, gap_line};
use crate::components::{button, card, count, field, meta, page_header, section};
use crate::icons::AppIcon;
use crate::tokens::{FORM_WIDTH, LG, MD, READING_WIDTH, SM, TYPE_META, XL, XS};
use crate::{Desktop, Screen, Tab};

impl Desktop {
    pub(crate) fn agents(
        &self,
        scope: Option<&str>,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(edit) = self
            .state
            .agent_edit()
            .filter(|edit| scope.is_none_or(|space| space == edit.space))
        {
            return self.agent_edit_view(edit, cx).into_any_element();
        }
        let team_mode = self.state.team_view_mode();
        let mode_toggle = ButtonGroup::new("team-view-mode").children(
            [
                (TeamViewMode::Overview, "Agents"),
                (TeamViewMode::Topology, "Topology"),
            ]
            .into_iter()
            .map(|(mode, label)| {
                button(label)
                    .small()
                    .outline()
                    .label(label)
                    .selected(team_mode == mode)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.set_team_view_mode(mode);
                        cx.notify();
                    }))
            }),
        );

        let topology = team_mode == TeamViewMode::Topology;
        let mut spaces = v_flex()
            .gap(XL)
            .when(topology, |body| body.flex_1())
            .when(!topology, |body| body.max_w(READING_WIDTH));
        for space in self
            .state
            .spaces()
            .iter()
            .filter(|space| scope.is_none_or(|name| space.name == name))
        {
            let mut part = v_flex().gap(MD).when(topology, |part| part.flex_1());
            if scope.is_none() {
                part = part.child(section(space.name.clone()));
            }
            if let Some(gap) = space.gap(Part::Agents) {
                part = part.child(gap_line(&gap, "agents", &space.name, cx));
            }
            if topology {
                let topo = self.state.agent_topology(
                    &space.name,
                    self.state.topology_project(&space.name),
                    when,
                );
                if let Some(gap) = space.gap(Part::Projects) {
                    part = part.child(gap_line(&gap, "projects", &space.name, cx));
                }
                part = part.child(self.agent_topology_view(&space.name, topo, cx));
            } else {
                part = part
                    .child(self.space_agent_roster(space, cx))
                    .child(self.space_project_assignments(space, cx));
            }
            spaces = spaces.child(part);
        }

        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .gap(LG)
            .child(page_header(
                "Team",
                scope.map(|space| meta(space.to_owned(), cx)),
                Some(h_flex().child(mode_toggle)),
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_y_scrollbar()
                    .id("agents")
                    .child(spaces),
            )
            .into_any_element()
    }

    fn space_agent_roster(&self, space: &Space, cx: &mut Context<Self>) -> Div {
        let open = self.state.agent_form().space.as_deref() == Some(&space.name);
        let view = cx.entity().downgrade();
        let mut actions = h_flex().gap(SM).flex_wrap();
        for (to, agents) in self.state.copy_targets(&space.name) {
            let label = format!("Copy an agent to {to}…");
            let view = view.clone();
            actions = actions.child(
                button(SharedString::from(format!(
                    "copy-agents-{}-{to}",
                    space.name
                )))
                .small()
                .ghost()
                .icon(AppIcon::Copy)
                .label(label)
                .disabled(agents.is_empty())
                .dropdown_menu(move |mut menu, _, _| {
                    for agent in &agents {
                        let (agent, to, view) = (agent.clone(), to.clone(), view.clone());
                        menu = menu.item(PopupMenuItem::new(agent.name.clone()).on_click(
                            move |_, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.copy_agent(agent.clone(), to.clone(), cx);
                                });
                            },
                        ));
                    }
                    menu
                }),
            );
        }
        let name = space.name.clone();
        let mut roster = v_flex().gap(SM).child(
            h_flex()
                .justify_between()
                .flex_wrap()
                .gap(SM)
                .child(section("Agent profiles").children(count(space.agents.len(), cx)))
                .child(actions.children((!open).then(|| {
                    button(SharedString::from(format!("new-agent-{}", space.name)))
                        .small()
                        .icon(AppIcon::UserPlus)
                        .label("New agent")
                        .accessibility_label(format!("New agent in {}", space.name))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_agent_form(name.clone(), cx);
                            this.agent_name.focus_handle(cx).focus(window, cx);
                        }))
                }))),
        );
        if open {
            roster = roster.child(self.agent_form(&space.name, cx));
        }
        if space.agents.is_empty() && space.gap(Part::Agents).is_none() && !open {
            roster = roster.child(empty(EmptyRegion::Agents, cx));
        }

        let assignments_loaded = space.gap(Part::Projects).is_none()
            && space.active_projects().all(|project| {
                self.state
                    .project_gap(&space.name, &project.name, Part::Bindings)
                    .is_none()
            });
        let mut agents = v_flex();
        for agent in &space.agents {
            let backends = if agent.backends.is_empty() {
                "Any backend".to_owned()
            } else {
                agent
                    .backends
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" → ")
            };
            let projects = assignments_loaded.then(|| {
                // General is the space lead's own project and is not listed.
                let general = space.active_projects().find(|project| project.is_general);
                let count = self
                    .state
                    .agent_bindings(&space.name, &agent.name)
                    .iter()
                    .filter(|bound| general.is_none_or(|general| general.name != bound.project))
                    .count();
                meta(
                    format!("{count} {}", count_form(count, "project", "projects")),
                    cx,
                )
            });
            let skills = agent.skills.len();
            let execution = self
                .state
                .agent_runs_on(&space.name, agent)
                .filter(|line| line != "Runs on hub");
            let (space_name, agent_name) = (space.name.clone(), agent.name.clone());
            agents = agents.child(
                h_flex()
                    .gap(MD)
                    .py(SM)
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(XS)
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(agent.name.clone()),
                            )
                            .child(
                                h_flex()
                                    .gap(SM)
                                    .flex_wrap()
                                    .children(
                                        (!agent.role.eq_ignore_ascii_case(&agent.name))
                                            .then(|| meta(agent.role.clone(), cx)),
                                    )
                                    .child(meta(backends, cx))
                                    .children(projects)
                                    .children((skills > 0).then(|| {
                                        meta(
                                            format!(
                                                "{skills} {}",
                                                count_form(skills, "skill", "skills")
                                            ),
                                            cx,
                                        )
                                    }))
                                    .children(execution.map(|line| meta(line, cx))),
                            ),
                    )
                    .child(
                        button(SharedString::from(format!(
                            "edit-{}-{}",
                            space.name, agent.name
                        )))
                        .small()
                        .ghost()
                        .icon(AppIcon::Pencil)
                        .label("Edit")
                        .accessibility_label(format!("Edit {}", agent.name))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.open_agent_edit(&space_name, &agent_name, window, cx);
                            },
                        )),
                    ),
            );
        }
        roster.child(agents)
    }

    fn space_project_assignments(&self, space: &Space, cx: &mut Context<Self>) -> Div {
        let projects = v_flex()
            .gap(SM)
            .pt(SM)
            .child(section("Project assignments"));
        if let Some(gap) = space.gap(Part::Projects) {
            return projects.child(gap_line(&gap, "projects", &space.name, cx));
        }
        let mut rows = v_flex();
        for project in space
            .active_projects()
            .filter(|project| !project.is_general)
        {
            let open = Screen::Project {
                space: space.name.clone(),
                project: project.name.clone(),
                tab: Tab::Team,
            };
            let gap = self
                .state
                .project_gap(&space.name, &project.name, Part::Bindings);
            rows = rows.child(
                h_flex()
                    .gap(MD)
                    .py(SM)
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(Icon::new(AppIcon::Folder).text_color(cx.theme().muted_foreground))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(XS)
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(project.name.clone()),
                            )
                            .child(if let Some(gap) = gap {
                                gap_line(&gap, "assignments", &space.name, cx)
                            } else {
                                let assigned = self
                                    .state
                                    .project(&space.name, &project.name)
                                    .map_or(0, |view| view.bindings.len());
                                meta(
                                    format!(
                                        "{assigned} {} assigned",
                                        count_form(assigned, "agent", "agents")
                                    ),
                                    cx,
                                )
                            }),
                    )
                    .child(
                        button(SharedString::from(format!(
                            "team-{}-{}",
                            space.name, project.name
                        )))
                        .small()
                        .ghost()
                        .label("Open team")
                        .icon(AppIcon::ChevronRight)
                        .accessibility_label(format!("Open team for {}", project.name))
                        .on_click(cx.listener(move |this, _, _, cx| this.go(open.clone(), cx))),
                    ),
            );
        }
        if !space.active_projects().any(|project| !project.is_general) {
            return projects.child(empty(EmptyRegion::Projects, cx));
        }
        projects.child(rows)
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
            .child(meta("Name", cx))
            .child(field(&self.agent_name))
            .child(meta("Role", cx))
            .child(field(&self.agent_role))
            .child(meta("Instructions", cx))
            .child(field(&self.agent_instructions))
            .child(backends)
            .child(self.skill_picker(false, cx))
            .child(
                div()
                    .text_size(TYPE_META)
                    .text_color(cx.theme().muted_foreground)
                    .child("After creating the agent, add it from a project’s Team tab."),
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
