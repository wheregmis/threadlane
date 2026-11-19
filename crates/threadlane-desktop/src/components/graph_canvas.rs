//! GPU-accelerated vector edge canvas and interactive node layers for the
//! Task Dependency DAG and Live Agent Topology Canvas.

use std::time::Duration;

use gpui_kit::component::button::{ButtonGroup, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::{ScrollableElement as _, ScrollbarAxis};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_desktop::{
    EdgeState, GraphEdge, MODES, TaskDag, TopologyGraph, TopologyNodeKind, count_form, mode_label,
};

use super::{button, count_badge, meta, status_badge, status_dot};
use crate::Desktop;
use crate::Screen;
use crate::icons::AppIcon;
use crate::tokens::{
    BADGE_RADIUS, MD, ROW_RADIUS, SM, Status, TABULAR, TYPE_BODY, TYPE_META, XS, XXS,
};

/// Evaluates a cubic Bézier curve at `t` in `[0.0, 1.0]`.
fn cubic_point(
    p0: (f32, f32),
    c1: (f32, f32),
    c2: (f32, f32),
    p3: (f32, f32),
    t: f32,
) -> (f32, f32) {
    let u = 1.0 - t;
    let tt = t * t;
    let uu = u * u;
    let uuu = uu * u;
    let ttt = tt * t;
    let x = uuu * p0.0 + 3.0 * uu * t * c1.0 + 3.0 * u * tt * c2.0 + ttt * p3.0;
    let y = uuu * p0.1 + 3.0 * uu * t * c1.1 + 3.0 * u * tt * c2.1 + ttt * p3.1;
    (x, y)
}

/// Paints the subtle architectural dot grid, cubic Bézier edges, directional
/// arrowheads, and (when `pulse` is `Some(phase)`) traveling beads along active edges.
fn edge_layer(
    id: impl Into<ElementId>,
    edges: Vec<GraphEdge>,
    width: f32,
    height: f32,
    pulse: Option<f32>,
) -> AnyElement {
    let layer = canvas(
        |bounds, _, _| bounds,
        move |bounds, _, window, cx| {
            let theme = cx.theme();
            let ox = bounds.origin.x;
            let oy = bounds.origin.y;

            // Subtle architectural dot grid.
            let dot_color = theme.border.opacity(0.32);
            let step = 28.0_f32;
            let mut gx = step * 0.5;
            while gx < width {
                let mut gy = step * 0.5;
                while gy < height {
                    window.paint_quad(
                        fill(
                            Bounds::new(point(ox + px(gx), oy + px(gy)), size(px(2.), px(2.))),
                            dot_color,
                        )
                        .corner_radii(px(1.)),
                    );
                    gy += step;
                }
                gx += step;
            }

            let (run_ink, run_tint) = Status::Running.colors(theme);
            let (fail_ink, _) = Status::Failed.colors(theme);
            let (done_ink, _) = Status::Done.colors(theme);

            for edge in &edges {
                let (stroke_color, stroke_w) = match edge.state {
                    EdgeState::Active => (run_ink, 2.2),
                    EdgeState::Blocked => (fail_ink.opacity(0.9), 2.0),
                    EdgeState::Satisfied => (done_ink.opacity(0.55), 1.5),
                    EdgeState::Pending => (theme.border.opacity(0.95), 1.4),
                };

                let p0 = (edge.x1, edge.y1);
                let p3 = (edge.x2 - 5.0, edge.y2);
                let dx = ((p3.0 - p0.0).abs() * 0.48).max(36.0);
                let c1 = (p0.0 + dx, p0.1);
                let c2 = (p3.0 - dx, p3.1);

                // Glow halo behind active edges.
                if edge.state == EdgeState::Active {
                    let mut glow = PathBuilder::stroke(px(6.0));
                    glow.move_to(point(ox + px(p0.0), oy + px(p0.1)));
                    glow.cubic_bezier_to(
                        point(ox + px(p3.0), oy + px(p3.1)),
                        point(ox + px(c1.0), oy + px(c1.1)),
                        point(ox + px(c2.0), oy + px(c2.1)),
                    );
                    if let Ok(path) = glow.build() {
                        window.paint_path(path, run_tint);
                    }
                }

                // Main cubic Bézier stroke.
                let mut builder = PathBuilder::stroke(px(stroke_w));
                builder.move_to(point(ox + px(p0.0), oy + px(p0.1)));
                builder.cubic_bezier_to(
                    point(ox + px(p3.0), oy + px(p3.1)),
                    point(ox + px(c1.0), oy + px(c1.1)),
                    point(ox + px(c2.0), oy + px(c2.1)),
                );
                if let Ok(path) = builder.build() {
                    window.paint_path(path, stroke_color);
                }

                // Source port dot.
                window.paint_quad(
                    fill(
                        Bounds::new(
                            point(ox + px(p0.0 - 2.5), oy + px(p0.1 - 2.5)),
                            size(px(5.), px(5.)),
                        ),
                        stroke_color,
                    )
                    .corner_radii(px(2.5)),
                );

                // Directional arrowhead at destination port.
                let tip = point(ox + px(edge.x2 - 1.0), oy + px(edge.y2));
                let mut arrow = PathBuilder::fill();
                arrow.move_to(tip);
                arrow.line_to(point(ox + px(edge.x2 - 8.0), oy + px(edge.y2 - 4.0)));
                arrow.line_to(point(ox + px(edge.x2 - 8.0), oy + px(edge.y2 + 4.0)));
                arrow.close();
                if let Ok(path) = arrow.build() {
                    window.paint_path(path, stroke_color);
                }

                // Animated traveling pulse beads along active edges.
                if edge.state == EdgeState::Active
                    && let Some(phase) = pulse
                {
                    for offset in [0.0_f32, 0.5_f32] {
                        let t = (phase + offset) % 1.0;
                        let (bx, by) = cubic_point(p0, c1, c2, p3, t);
                        window.paint_quad(
                            fill(
                                Bounds::new(
                                    point(ox + px(bx - 5.5), oy + px(by - 5.5)),
                                    size(px(11.), px(11.)),
                                ),
                                run_tint,
                            )
                            .corner_radii(px(5.5)),
                        );
                        window.paint_quad(
                            fill(
                                Bounds::new(
                                    point(ox + px(bx - 3.0), oy + px(by - 3.0)),
                                    size(px(6.), px(6.)),
                                ),
                                run_ink,
                            )
                            .corner_radii(px(3.0)),
                        );
                    }
                }
            }
        },
    )
    .w(px(width))
    .h(px(height));

    div()
        .id(id)
        .absolute()
        .top_0()
        .left_0()
        .w(px(width))
        .h(px(height))
        .child(layer)
        .into_any_element()
}

/// Wraps [`edge_layer`] in a repeating GPUI animation when there is at least
/// one active edge, or renders statically (0% idle CPU) otherwise.
fn animated_edges(
    id: &'static str,
    edges: Vec<GraphEdge>,
    width: f32,
    height: f32,
    has_active: bool,
) -> AnyElement {
    if !has_active {
        return edge_layer(id, edges, width, height, None);
    }
    div()
        .absolute()
        .top_0()
        .left_0()
        .w(px(width))
        .h(px(height))
        .with_animation(
            id,
            Animation::new(Duration::from_millis(1600)).repeat(),
            move |container, delta| {
                container.child(edge_layer(
                    SharedString::from(format!("{id}-frame")),
                    edges.clone(),
                    width,
                    height,
                    Some(delta),
                ))
            },
        )
        .into_any_element()
}

impl Desktop {
    /// Interactive Dependency DAG canvas for a project or space task list.
    pub(crate) fn task_dag_view(
        &self,
        id: &'static str,
        space: &str,
        dag: TaskDag,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if dag.nodes.is_empty() {
            return meta("No tasks to graph yet.", cx).into_any_element();
        }

        let mono = cx.theme().mono_font_family.clone();
        let (fail_ink, fail_tint) = Status::Failed.colors(cx.theme());
        let (wait_ink, wait_tint) = Status::Waiting.colors(cx.theme());
        let (run_ink, _) = Status::Running.colors(cx.theme());

        // Legend / summary strip
        let mut legend = h_flex().gap(MD).flex_wrap().items_center().child(meta(
            format!(
                "{} {} · {} {}",
                dag.nodes.len(),
                count_form(dag.nodes.len(), "task", "tasks"),
                dag.edges.len(),
                count_form(dag.edges.len(), "dependency", "dependencies")
            ),
            cx,
        ));
        if dag.active_edges > 0 {
            legend = legend.child(
                h_flex()
                    .gap(XS)
                    .items_center()
                    .child(status_dot(Status::Running, cx))
                    .child(
                        div()
                            .text_size(TYPE_META)
                            .text_color(run_ink)
                            .child(format!("{} active", dag.active_edges)),
                    ),
            );
        }
        if dag.bottlenecks > 0 {
            legend = legend.child(
                h_flex()
                    .gap(XS)
                    .items_center()
                    .child(status_dot(Status::Failed, cx))
                    .child(
                        div()
                            .text_size(TYPE_META)
                            .text_color(fail_ink)
                            .child(format!(
                                "{} {}",
                                dag.bottlenecks,
                                count_form(dag.bottlenecks, "bottleneck", "bottlenecks")
                            )),
                    ),
            );
        }
        if !dag.has_dependencies {
            legend = legend.child(meta(
                "Tasks with depends_on links connect automatically.",
                cx,
            ));
        }

        let canvas_w = dag.width.max(540.0);
        let canvas_h = dag.height.max(240.0);
        let has_active = dag.active_edges > 0;

        let mut surface = div()
            .relative()
            .w(px(canvas_w))
            .h(px(canvas_h))
            .child(animated_edges(
                id, dag.edges, canvas_w, canvas_h, has_active,
            ));

        for node in dag.nodes {
            let screen = Screen::Task {
                space: space.to_owned(),
                task: node.id,
            };
            let status_kind = Status::task(node.status);
            let is_failed = node.task_status == threadlane_core::model::TaskStatus::Failed;
            let border_color = if node.is_bottleneck && is_failed {
                fail_ink
            } else if node.is_bottleneck {
                wait_ink
            } else if status_kind == Status::Running {
                run_ink.opacity(0.7)
            } else {
                cx.theme().border
            };
            let bg = if node.is_bottleneck && is_failed {
                fail_tint
            } else if node.is_bottleneck {
                wait_tint
            } else {
                cx.theme().group_box
            };
            let title_color = if node.quiet {
                cx.theme().muted_foreground
            } else {
                cx.theme().foreground
            };

            let card = div()
                .id(SharedString::from(format!("{id}-node-{}", node.id)))
                .absolute()
                .left(px(node.x))
                .top(px(node.y))
                .w(px(node.width))
                .h(px(node.height))
                .p(SM)
                .rounded(ROW_RADIUS)
                .border_1()
                .border_color(border_color)
                .bg(bg)
                .hover(|s| s.border_color(cx.theme().primary))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
                .child(
                    v_flex()
                        .size_full()
                        .justify_between()
                        .gap(XXS)
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .items_center()
                                .gap(XS)
                                .child(
                                    h_flex()
                                        .gap(XS)
                                        .items_center()
                                        .child(status_dot(status_kind, cx))
                                        .child(
                                            div()
                                                .text_size(TYPE_META)
                                                .font_family(mono.clone())
                                                .font_features(TABULAR.clone())
                                                .text_color(cx.theme().muted_foreground)
                                                .child(node.id.to_string()),
                                        ),
                                )
                                .children(if node.is_bottleneck {
                                    Some(status_badge(
                                        format!("Blocks {}", node.downstream_blocked),
                                        if is_failed {
                                            Status::Failed
                                        } else {
                                            Status::Waiting
                                        },
                                        cx,
                                    ))
                                } else if !node.quiet {
                                    Some(status_badge(node.status, status_kind, cx))
                                } else {
                                    None
                                }),
                        )
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .truncate()
                                .text_size(TYPE_BODY)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(title_color)
                                .child(node.title),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .gap(XS)
                                .child(meta(node.agent, cx).truncate())
                                .child(
                                    meta(node.time, cx)
                                        .flex_none()
                                        .font_features(TABULAR.clone()),
                                ),
                        ),
                );
            surface = surface.child(card);
        }

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap(SM)
            .child(legend)
            .child(
                div()
                    .id(SharedString::from(format!("{id}-viewport")))
                    .flex_1()
                    .min_h(px(300.))
                    .w_full()
                    .rounded(ROW_RADIUS)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .overflow_scrollbar()
                    .child(surface),
            )
            .into_any_element()
    }

    /// Live Agent Org & Handoff Topology canvas for `space` on the `Team` screen.
    pub(crate) fn agent_topology_view(
        &self,
        space: &str,
        topo: TopologyGraph,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (run_ink, _) = Status::Running.colors(cx.theme());
        let (wait_ink, wait_tint) = Status::Waiting.colors(cx.theme());
        let (fail_ink, fail_tint) = Status::Failed.colors(cx.theme());
        let selected_agent = self.state.topology_selected_agent();

        // Scope filter bar + live delegation count
        let space_all = space.to_owned();
        let mut bar = h_flex().gap(XS).flex_wrap().items_center().child(
            button(SharedString::from(format!("topo-scope-all-{space}")))
                .small()
                .outline()
                .label("All projects")
                .selected(topo.selected_project.is_none())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state.set_topology_project(&space_all, None);
                    cx.notify();
                })),
        );
        for project in &topo.projects {
            let space_p = space.to_owned();
            let proj_p = project.clone();
            let selected = topo.selected_project.as_deref() == Some(project.as_str());
            bar = bar.child(
                button(SharedString::from(format!("topo-scope-{space}-{project}")))
                    .small()
                    .outline()
                    .label(project.clone())
                    .selected(selected)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let next = if selected { None } else { Some(proj_p.clone()) };
                        this.state.set_topology_project(&space_p, next);
                        cx.notify();
                    })),
            );
        }
        if topo.active_delegations > 0 {
            bar = bar.child(
                h_flex()
                    .pl(SM)
                    .gap(XS)
                    .items_center()
                    .child(status_dot(Status::Running, cx))
                    .child(
                        div()
                            .text_size(TYPE_META)
                            .text_color(run_ink)
                            .child(format!(
                                "{} active {}",
                                topo.active_delegations,
                                count_form(topo.active_delegations, "delegation", "delegations")
                            )),
                    ),
            );
        } else {
            bar = bar.child(
                div()
                    .pl(SM)
                    .child(meta("Click any agent to dial its autonomy mode.", cx)),
            );
        }

        let canvas_w = topo.width.max(760.0);
        let canvas_h = topo.height.max(260.0);
        let has_active = topo.active_delegations > 0;

        // Collect edge midpoint labels for active tasks before moving edges into the painter.
        let edge_labels: Vec<(String, f32, f32, String)> = topo
            .edges
            .iter()
            .filter_map(|e| {
                let label = e.label.as_ref()?;
                Some((
                    e.id.clone(),
                    (e.x1 + e.x2) * 0.5,
                    (e.y1 + e.y2) * 0.5,
                    label.clone(),
                ))
            })
            .collect();

        let mut surface = div()
            .relative()
            .w(px(canvas_w))
            .h(px(canvas_h))
            .child(animated_edges(
                "agent-topology-edges",
                topo.edges,
                canvas_w,
                canvas_h,
                has_active,
            ));

        // Edge midpoint badges (e.g. live task IDs on active delegations)
        for (edge_id, mx, my, label) in edge_labels {
            surface = surface.child(
                div()
                    .id(SharedString::from(format!("label-{edge_id}")))
                    .absolute()
                    .left(px(mx - 28.0))
                    .top(px(my - 10.0))
                    .child(status_badge(label, Status::Running, cx)),
            );
        }

        for node in topo.nodes {
            let is_selected = matches!(
                &node.kind,
                TopologyNodeKind::Agent { space: s, agent: a, .. }
                    if selected_agent == Some((s.as_str(), a.as_str()))
            );
            let status_kind = if node.running {
                Status::Running
            } else if node.failed {
                Status::Failed
            } else if node.waiting {
                Status::Waiting
            } else {
                Status::Idle
            };
            let border_color = if is_selected {
                cx.theme().primary
            } else if node.running {
                run_ink
            } else if node.failed {
                fail_ink
            } else if node.waiting {
                wait_ink
            } else {
                cx.theme().border
            };
            let bg = if node.failed {
                fail_tint
            } else if node.waiting {
                wait_tint
            } else {
                cx.theme().group_box
            };
            let icon = match &node.kind {
                TopologyNodeKind::SpaceLead { .. } => AppIcon::Inbox,
                TopologyNodeKind::Project { .. } => AppIcon::Folder,
                TopologyNodeKind::Agent { .. } => AppIcon::Bot,
            };

            let mut card = div()
                .id(SharedString::from(node.id.clone()))
                .absolute()
                .left(px(node.x))
                .top(px(node.y))
                .w(px(node.width))
                .h(px(node.height))
                .p(SM)
                .rounded(ROW_RADIUS)
                .border_1()
                .border_color(border_color)
                .bg(bg)
                .hover(|s| s.border_color(cx.theme().primary))
                .cursor_pointer()
                .child(
                    v_flex()
                        .size_full()
                        .justify_between()
                        .gap(XXS)
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .items_center()
                                .gap(XS)
                                .child(
                                    h_flex()
                                        .gap(XS)
                                        .items_center()
                                        .min_w_0()
                                        .child(
                                            Icon::new(icon)
                                                .size(TYPE_META)
                                                .text_color(cx.theme().muted_foreground),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(TYPE_BODY)
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(node.title.clone()),
                                        ),
                                )
                                .child(status_dot(status_kind, cx)),
                        )
                        .child(meta(node.subtitle.clone(), cx).truncate())
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .items_center()
                                .gap(XS)
                                .children(node.badge.map(|b| count_badge(b, cx)))
                                .children(node.active_label.map(|active| {
                                    div()
                                        .text_size(TYPE_META)
                                        .text_color(if node.running {
                                            run_ink
                                        } else {
                                            cx.theme().muted_foreground
                                        })
                                        .font_features(TABULAR.clone())
                                        .child(active)
                                })),
                        ),
                );

            card = match node.kind {
                TopologyNodeKind::SpaceLead { space } => {
                    card.on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(screen) = this.main_chat_screen(&space) {
                            this.go(screen, cx);
                        }
                    }))
                }
                TopologyNodeKind::Project { space, project, .. } => {
                    let already = topo.selected_project.as_deref() == Some(project.as_str());
                    card.on_click(cx.listener(move |this, _, _, cx| {
                        let next = if already { None } else { Some(project.clone()) };
                        this.state.set_topology_project(&space, next);
                        cx.notify();
                    }))
                }
                TopologyNodeKind::Agent { space, agent, .. } => {
                    card.on_click(cx.listener(move |this, _, _, cx| {
                        this.state.toggle_topology_agent(&space, &agent);
                        cx.notify();
                    }))
                }
            };

            surface = surface.child(card);
        }

        // Inline Autonomy Dial bar when an agent node is selected on this space.
        let dial = selected_agent
            .filter(|(s, _)| *s == space)
            .map(|(_, agent)| {
                let target_project = topo
                    .selected_project
                    .clone()
                    .or_else(|| topo.projects.first().cloned());
                let current_mode = target_project.as_deref().and_then(|proj| {
                    let grid = self.state.modes(space);
                    let agent_idx = grid.agents.iter().position(|a| a == agent)?;
                    let row = grid.rows.iter().find(|r| r.project == proj)?;
                    row.modes.as_ref().ok()?.get(agent_idx).copied().flatten()
                });
                let ceiling = target_project
                    .as_deref()
                    .map(|proj| self.state.project_ceiling(space, proj));

                let mut dial_bar = h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .gap(SM)
                    .flex_wrap()
                    .p(SM)
                    .rounded(ROW_RADIUS)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().group_box)
                    .child(
                        h_flex()
                            .gap(SM)
                            .items_center()
                            .child(Icon::new(AppIcon::Bot).size(TYPE_BODY))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(TYPE_BODY)
                                    .child(agent.to_owned()),
                            )
                            .children(target_project.as_ref().map(|proj| {
                                if topo.projects.len() > 1 {
                                    let projects = topo.projects.clone();
                                    let space_p = space.to_owned();
                                    let view = cx.entity().downgrade();
                                    button("topo-dial-project")
                                        .small()
                                        .outline()
                                        .icon(AppIcon::ChevronDown)
                                        .label(format!("in {proj}"))
                                        .dropdown_menu(move |mut menu, _, _| {
                                            for p in &projects {
                                                let (s, p, v) =
                                                    (space_p.clone(), p.clone(), view.clone());
                                                menu = menu.item(
                                                    PopupMenuItem::new(p.clone()).on_click(
                                                        move |_, _, cx| {
                                                            let _ = v.update(cx, |this, cx| {
                                                                this.state.set_topology_project(
                                                                    &s,
                                                                    Some(p.clone()),
                                                                );
                                                                cx.notify();
                                                            });
                                                        },
                                                    ),
                                                );
                                            }
                                            menu
                                        })
                                        .into_any_element()
                                } else {
                                    meta(format!("in {proj}"), cx).into_any_element()
                                }
                            })),
                    );

                if let Some(proj) = target_project {
                    let mut group = ButtonGroup::new("topo-autonomy-dial");
                    for (choice, label) in MODES {
                        let (s, p, a) = (space.to_owned(), proj.clone(), agent.to_owned());
                        let capped = ceiling.is_some_and(|cap| choice > cap);
                        group = group.child(
                            button(SharedString::from(format!("topo-dial-{label}")))
                                .small()
                                .outline()
                                .selected(current_mode == Some(choice))
                                .disabled(capped)
                                .label(label)
                                .when(capped, |btn| {
                                    btn.tooltip(format!(
                                        "Capped by project ceiling ({})",
                                        ceiling.map(mode_label).unwrap_or_default()
                                    ))
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_mode(s.clone(), p.clone(), a.clone(), choice, cx);
                                })),
                        );
                    }
                    dial_bar = dial_bar.child(group);
                }

                let (edit_space, edit_agent) = (space.to_owned(), agent.to_owned());
                dial_bar.child(
                    button("topo-dial-edit")
                        .small()
                        .ghost()
                        .icon(AppIcon::Pencil)
                        .label("Edit agent…")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_agent_edit(&edit_space, &edit_agent, window, cx);
                        })),
                )
            });

        let _ = (BADGE_RADIUS, ScrollbarAxis::Both);
        v_flex()
            .w_full()
            .gap(SM)
            .child(bar)
            .child(
                div()
                    .id(SharedString::from(format!("topo-viewport-{space}")))
                    .w_full()
                    .min_h(px(280.))
                    .max_h(px(520.))
                    .rounded(ROW_RADIUS)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .overflow_scrollbar()
                    .child(surface),
            )
            .children(dial)
            .into_any_element()
    }
}
