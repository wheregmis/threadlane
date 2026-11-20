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

/// Paints the viewport-clipped architectural dot grid, cubic Bézier edges,
/// port dots, and directional arrowheads (static; does not repaint on pulse frames).
pub(crate) fn static_edge_layer(
    id: impl Into<ElementId>,
    edges: Vec<GraphEdge>,
    width: f32,
    height: f32,
) -> AnyElement {
    let layer = canvas(
        |bounds, _, _| bounds,
        move |bounds, _, window, cx| {
            let theme = cx.theme();
            let ox = bounds.origin.x;
            let oy = bounds.origin.y;
            let viewport = window.viewport_size();

            // Subtle architectural dot grid clipped strictly to the visible viewport.
            let dot_color = theme.border.opacity(0.32);
            let step = 28.0_f32;
            let min_x = (-f32::from(ox)).max(0.0);
            let max_x = (f32::from(viewport.width) - f32::from(ox)).min(width);
            let min_y = (-f32::from(oy)).max(0.0);
            let max_y = (f32::from(viewport.height) - f32::from(oy)).min(height);
            let start_col = ((min_x - step * 0.5).max(0.0) / step).floor() as i32;
            let start_row = ((min_y - step * 0.5).max(0.0) / step).floor() as i32;
            let mut gx = step * 0.5 + start_col as f32 * step;
            while gx <= max_x {
                let mut gy = step * 0.5 + start_row as f32 * step;
                while gy <= max_y {
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
                    EdgeState::Pending => (theme.muted_foreground.opacity(0.7), 1.4),
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

/// Paints ONLY the traveling pulse beads along active edges (independent of
/// the static dot grid and curves).
fn pulse_overlay_layer(
    id: impl Into<ElementId>,
    active_edges: Vec<GraphEdge>,
    width: f32,
    height: f32,
    phase: f32,
) -> AnyElement {
    let layer = canvas(
        |bounds, _, _| bounds,
        move |bounds, _, window, cx| {
            let (run_ink, run_tint) = Status::Running.colors(cx.theme());
            let ox = bounds.origin.x;
            let oy = bounds.origin.y;
            for edge in &active_edges {
                let p0 = (edge.x1, edge.y1);
                let p3 = (edge.x2 - 5.0, edge.y2);
                let dx = ((p3.0 - p0.0).abs() * 0.48).max(36.0);
                let c1 = (p0.0 + dx, p0.1);
                let c2 = (p3.0 - dx, p3.1);
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

/// Composes the static dot-grid + edge layer with an optional lightweight
/// animated pulse overlay when there is at least one active edge.
fn animated_edges(
    id: &'static str,
    edges: Vec<GraphEdge>,
    width: f32,
    height: f32,
    has_active: bool,
) -> AnyElement {
    let active_only: Vec<GraphEdge> = if has_active {
        edges
            .iter()
            .filter(|e| e.state == EdgeState::Active)
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let static_layer = static_edge_layer(
        SharedString::from(format!("{id}-static")),
        edges,
        width,
        height,
    );
    if active_only.is_empty() {
        return static_layer;
    }
    div()
        .absolute()
        .top_0()
        .left_0()
        .w(px(width))
        .h(px(height))
        .child(static_layer)
        .child(
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
                        container.child(pulse_overlay_layer(
                            SharedString::from(format!("{id}-pulse")),
                            active_only.clone(),
                            width,
                            height,
                            delta,
                        ))
                    },
                ),
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
                .absolute()
                .left(px(node.x))
                .top(px(node.y))
                .w(px(node.width))
                .h(px(node.height))
                .child(
                    button(SharedString::from(format!("{id}-node-{}", node.id)))
                        .ghost()
                        .tab_stop(true)
                        .size_full()
                        .p(SM)
                        .rounded(ROW_RADIUS)
                        .border_1()
                        .border_color(border_color)
                        .bg(bg)
                        .accessibility_label(format!(
                            "Open {} {}, {}",
                            node.id, node.title, node.status
                        ))
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
                        ),
                );
            surface = surface.child(card);
        }

        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .h_full()
            .gap(SM)
            .child(legend)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .rounded(ROW_RADIUS)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(
                        div()
                            .id(SharedString::from(format!("{id}-viewport")))
                            .size_full()
                            .overflow_scroll()
                            .track_scroll(&self.dag_scroll)
                            .child(surface),
                    )
                    .scrollbar(&self.dag_scroll, ScrollbarAxis::Both),
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

        // Snapshot current specialist names so the dial only shows for an agent
        // still present in the graph after refreshes/renames.
        let current_agents: Vec<String> = topo
            .nodes
            .iter()
            .filter_map(|n| match &n.kind {
                TopologyNodeKind::Agent {
                    space: s, agent, ..
                } if s == space => Some(agent.clone()),
                _ => None,
            })
            .collect();

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
            let aria = match &node.kind {
                TopologyNodeKind::SpaceLead { .. } => format!("Open {} space chat", node.title),
                TopologyNodeKind::Project { project, .. } => {
                    format!("Filter topology to project {project}")
                }
                TopologyNodeKind::Agent { agent, .. } => {
                    let models = node.tooltip.as_deref().unwrap_or_default();
                    format!("Select {agent} ({models}) to adjust autonomy mode")
                }
            };

            let card_btn = button(SharedString::from(node.id.clone()))
                .ghost()
                .tab_stop(true)
                .size_full()
                .p(SM)
                .rounded(ROW_RADIUS)
                .border_1()
                .border_color(border_color)
                .bg(bg)
                .accessibility_label(aria)
                .when_some(node.tooltip, |card, tip| card.tooltip(tip))
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

            let card_btn = match node.kind {
                TopologyNodeKind::SpaceLead { space } => {
                    card_btn.on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(screen) = this.main_chat_screen(&space) {
                            this.go(screen, cx);
                        }
                    }))
                }
                TopologyNodeKind::Project { space, project, .. } => {
                    let already = topo.selected_project.as_deref() == Some(project.as_str());
                    card_btn.on_click(cx.listener(move |this, _, _, cx| {
                        let next = if already { None } else { Some(project.clone()) };
                        this.state.set_topology_project(&space, next);
                        cx.notify();
                    }))
                }
                TopologyNodeKind::Agent { space, agent, .. } => {
                    card_btn.on_click(cx.listener(move |this, _, _, cx| {
                        this.state.toggle_topology_agent(&space, &agent);
                        cx.notify();
                    }))
                }
            };

            surface = surface.child(
                div()
                    .absolute()
                    .left(px(node.x))
                    .top(px(node.y))
                    .w(px(node.width))
                    .h(px(node.height))
                    .child(card_btn),
            );
        }

        // Inline Autonomy Dial bar when an agent node still present in the graph is selected.
        let dial = selected_agent
            .filter(|(s, a)| *s == space && current_agents.iter().any(|name| name == a))
            .map(|(_, agent)| {
                let grid = self.state.modes(space);
                let agent_idx = grid.agents.iter().position(|a| a == agent);
                // Only auto-pick a project when scoped, when there is 1 project,
                // or when the agent is bound to exactly 1 project.
                let bound_projects: Vec<&str> = agent_idx
                    .map(|idx| {
                        grid.rows
                            .iter()
                            .filter(|r| {
                                r.project != "General"
                                    && r.modes
                                        .as_ref()
                                        .ok()
                                        .and_then(|m| m.get(idx).copied().flatten())
                                        .is_some()
                            })
                            .map(|r| r.project.as_str())
                            .collect()
                    })
                    .unwrap_or_default();
                let target_project = topo
                    .selected_project
                    .clone()
                    .or_else(|| (topo.projects.len() == 1).then(|| topo.projects[0].clone()))
                    .or_else(|| (bound_projects.len() == 1).then(|| bound_projects[0].to_owned()));
                let current_mode = target_project.as_deref().and_then(|proj| {
                    let idx = agent_idx?;
                    let row = grid.rows.iter().find(|r| r.project == proj)?;
                    row.modes.as_ref().ok()?.get(idx).copied().flatten()
                });
                let ceiling = target_project
                    .as_deref()
                    .map(|proj| self.state.project_ceiling(space, proj));

                let project_control: Option<AnyElement> = if topo.projects.len() > 1 {
                    let projects = topo.projects.clone();
                    let space_p = space.to_owned();
                    let view = cx.entity().downgrade();
                    let label = target_project
                        .as_deref()
                        .map_or_else(|| "Pick a project…".to_owned(), |p| format!("in {p}"));
                    Some(
                        button("topo-dial-project")
                            .small()
                            .outline()
                            .icon(AppIcon::ChevronDown)
                            .label(label)
                            .dropdown_menu(move |mut menu, _, _| {
                                for p in &projects {
                                    let (s, p, v) = (space_p.clone(), p.clone(), view.clone());
                                    menu = menu.item(PopupMenuItem::new(p.clone()).on_click(
                                        move |_, _, cx| {
                                            let _ = v.update(cx, |this, cx| {
                                                this.state
                                                    .set_topology_project(&s, Some(p.clone()));
                                                cx.notify();
                                            });
                                        },
                                    ));
                                }
                                menu
                            })
                            .into_any_element(),
                    )
                } else {
                    target_project
                        .as_ref()
                        .map(|proj| meta(format!("in {proj}"), cx).into_any_element())
                };

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
                            .children(project_control),
                    );

                if let Some(proj) = target_project {
                    let buttons = MODES.into_iter().map(|(choice, label)| {
                        let (s, p, a) = (space.to_owned(), proj.clone(), agent.to_owned());
                        let capped = ceiling.is_some_and(|cap| choice > cap);
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
                                let cap = this.state.project_ceiling(&s, &p);
                                if choice <= cap {
                                    this.set_mode(s.clone(), p.clone(), a.clone(), choice, cx);
                                }
                            }))
                    });
                    dial_bar =
                        dial_bar.child(ButtonGroup::new("topo-autonomy-dial").children(buttons));
                } else {
                    dial_bar = dial_bar.child(meta("Pick a project to adjust autonomy mode.", cx));
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

        let _ = BADGE_RADIUS;
        v_flex()
            .flex_1()
            .w_full()
            .gap(SM)
            .child(bar)
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex_1()
                    // Floor keeps each canvas usable when several spaces stack.
                    .min_h(px(280.0))
                    .rounded(ROW_RADIUS)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(
                        div()
                            .id(SharedString::from(format!("topo-viewport-{space}")))
                            .size_full()
                            .overflow_scroll()
                            .track_scroll(&self.topo_scroll)
                            .child(surface),
                    )
                    .scrollbar(&self.topo_scroll, ScrollbarAxis::Both),
            )
            .children(dial)
            .into_any_element()
    }
}
