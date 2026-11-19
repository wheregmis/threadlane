//! Pure layout and state models for the Interactive Dependency DAG (Tasks)
//! and Live Agent Topology Canvas (Team).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use threadlane_core::client::TaskSummary;
use threadlane_core::ids::TaskId;
use threadlane_core::model::{Mode, TaskStatus};

use crate::{AppState, When, status_label, work_elapsed};

/// View mode for a project's `Tasks` tab.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProjectTasksViewMode {
    #[default]
    Board,
    List,
    Graph,
}

/// View mode for the space-wide `Tasks` screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpaceTasksViewMode {
    #[default]
    List,
    Graph,
}

/// View mode for the `Team` screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TeamViewMode {
    #[default]
    Overview,
    Topology,
}

/// Visual state of a directed edge on either the Task DAG or Agent Topology canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeState {
    /// Work is actively flowing along this edge (animated pulse).
    Active,
    /// A failure or waiting decision is blocking downstream work.
    Blocked,
    /// Upstream work is complete or an autonomous/assisted delegation is ready.
    Satisfied,
    /// Idle or future dependency.
    Pending,
}

/// Geometry constants for the Task Dependency DAG canvas.
pub const DAG_NODE_W: f32 = 236.0;
pub const DAG_NODE_H: f32 = 90.0;
const DAG_COL_GAP: f32 = 76.0;
const DAG_ROW_GAP: f32 = 18.0;
const CANVAS_PAD: f32 = 24.0;

/// One task card placed on the dependency DAG canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct DagNode {
    pub id: TaskId,
    pub title: String,
    pub project: String,
    pub agent: String,
    pub status: &'static str,
    pub task_status: TaskStatus,
    pub time: String,
    pub layer: usize,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Number of non-terminal downstream tasks directly or transitively waiting on this task.
    pub downstream_blocked: usize,
    /// True when this unfinished task is blocking at least one downstream task.
    pub is_bottleneck: bool,
    /// Dimmed styling for completed tasks.
    pub quiet: bool,
}

/// One directed dependency edge (`from` -> `to`) on the canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphEdge {
    pub id: String,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub state: EdgeState,
    pub label: Option<String>,
}

/// Positioned Task Dependency DAG ready for GPUI rendering.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskDag {
    pub nodes: Vec<DagNode>,
    pub edges: Vec<GraphEdge>,
    pub width: f32,
    pub height: f32,
    pub active_edges: usize,
    pub bottlenecks: usize,
    pub has_dependencies: bool,
}

/// Builds a layered left-to-right directed acyclic graph from `tasks`.
///
/// When tasks have `depends_on` links among the visible set, nodes are assigned
/// longest-path topological layers and ordered within each layer to keep
/// related work together. When no dependencies exist yet, tasks are arranged in
/// a compact 3-column grid so the view stays clean and scannable.
#[must_use]
pub fn build_task_dag(tasks: &[TaskSummary], when: &When) -> TaskDag {
    if tasks.is_empty() {
        return TaskDag::default();
    }

    let by_id: HashMap<TaskId, &TaskSummary> = tasks.iter().map(|s| (s.task.id, s)).collect();

    // Build visible adjacency lists: dep -> dependent (forward) and dependent -> deps (reverse).
    let mut parents: HashMap<TaskId, Vec<TaskId>> = HashMap::with_capacity(tasks.len());
    let mut children: HashMap<TaskId, Vec<TaskId>> = HashMap::with_capacity(tasks.len());
    let mut edge_pairs: Vec<(TaskId, TaskId)> = Vec::new();
    for summary in tasks {
        let id = summary.task.id;
        for &dep in &summary.task.depends_on {
            if dep != id && by_id.contains_key(&dep) {
                parents.entry(id).or_default().push(dep);
                children.entry(dep).or_default().push(id);
                edge_pairs.push((dep, id));
            }
        }
    }
    let has_dependencies = !edge_pairs.is_empty();

    // Compute transitive non-terminal downstream counts for bottleneck detection.
    let mut downstream_counts: HashMap<TaskId, usize> = HashMap::with_capacity(tasks.len());
    for summary in tasks {
        let root = summary.task.id;
        let mut visited = HashSet::new();
        let mut stack = children.get(&root).cloned().unwrap_or_default();
        let mut blocked = 0usize;
        while let Some(next) = stack.pop() {
            if !visited.insert(next) {
                continue;
            }
            if let Some(child) = by_id.get(&next)
                && !child.task.status.is_terminal()
            {
                blocked += 1;
            }
            if let Some(more) = children.get(&next) {
                stack.extend(more.iter().copied());
            }
        }
        downstream_counts.insert(root, blocked);
    }

    // Assign coordinates: either layered DAG or tidy 3-column grid when flat.
    let mut positions: HashMap<TaskId, (usize, f32, f32)> = HashMap::with_capacity(tasks.len());
    let (width, height) = if has_dependencies {
        let mut layers: HashMap<TaskId, usize> = HashMap::with_capacity(tasks.len());
        for summary in tasks {
            let mut visiting = HashSet::new();
            compute_layer(summary.task.id, &parents, &mut layers, &mut visiting);
        }

        let mut by_layer: BTreeMap<usize, Vec<TaskId>> = BTreeMap::new();
        for summary in tasks {
            let layer = layers.get(&summary.task.id).copied().unwrap_or(0);
            by_layer.entry(layer).or_default().push(summary.task.id);
        }

        // Order each layer: barycenter of parent row indices when available,
        // then urgency (active/failed/waiting first), then newest TaskId.
        let mut row_index: HashMap<TaskId, usize> = HashMap::with_capacity(tasks.len());
        let mut max_rows = 0usize;
        let mut max_layer = 0usize;
        for (layer, mut ids) in by_layer {
            max_layer = max_layer.max(layer);
            ids.sort_by(|a, b| {
                let bary = |id: TaskId| -> u64 {
                    let Some(ps) = parents.get(&id) else {
                        return u64::MAX;
                    };
                    let mut sum = 0u64;
                    let mut count = 0u64;
                    for p in ps {
                        if let Some(&idx) = row_index.get(p) {
                            sum += idx as u64 * 100;
                            count += 1;
                        }
                    }
                    sum.checked_div(count).unwrap_or(u64::MAX)
                };
                let sa = by_id[a];
                let sb = by_id[b];
                // Keep connected roots at the top of Layer 0 so dependency
                // chains stay compact and aligned with their children.
                let connected =
                    |id: TaskId| -> u8 { u8::from(layer != 0 || !children.contains_key(&id)) };
                bary(*a)
                    .cmp(&bary(*b))
                    .then_with(|| connected(*a).cmp(&connected(*b)))
                    .then_with(|| {
                        status_priority(sa.task.status).cmp(&status_priority(sb.task.status))
                    })
                    .then_with(|| b.cmp(a))
            });
            max_rows = max_rows.max(ids.len());
            for (row, id) in ids.into_iter().enumerate() {
                row_index.insert(id, row);
                let x = CANVAS_PAD + layer as f32 * (DAG_NODE_W + DAG_COL_GAP);
                let y = CANVAS_PAD + row as f32 * (DAG_NODE_H + DAG_ROW_GAP);
                positions.insert(id, (layer, x, y));
            }
        }
        let width =
            CANVAS_PAD * 2.0 + (max_layer + 1) as f32 * DAG_NODE_W + max_layer as f32 * DAG_COL_GAP;
        let height = CANVAS_PAD * 2.0
            + max_rows as f32 * DAG_NODE_H
            + max_rows.saturating_sub(1) as f32 * DAG_ROW_GAP;
        (width, height)
    } else {
        let mut ordered: Vec<&TaskSummary> = tasks.iter().collect();
        ordered.sort_by(|a, b| {
            status_priority(a.task.status)
                .cmp(&status_priority(b.task.status))
                .then_with(|| {
                    b.completed_at
                        .unwrap_or(b.task.created_at)
                        .cmp(&a.completed_at.unwrap_or(a.task.created_at))
                })
                .then_with(|| b.task.id.cmp(&a.task.id))
        });
        let cols = ordered.len().clamp(1, 3);
        let rows = ordered.len().div_ceil(cols);
        for (idx, summary) in ordered.into_iter().enumerate() {
            let col = idx % cols;
            let row = idx / cols;
            let x = CANVAS_PAD + col as f32 * (DAG_NODE_W + 24.0);
            let y = CANVAS_PAD + row as f32 * (DAG_NODE_H + DAG_ROW_GAP);
            positions.insert(summary.task.id, (0, x, y));
        }
        let width = CANVAS_PAD * 2.0 + cols as f32 * DAG_NODE_W + (cols - 1) as f32 * 24.0;
        let height = CANVAS_PAD * 2.0
            + rows as f32 * DAG_NODE_H
            + rows.saturating_sub(1) as f32 * DAG_ROW_GAP;
        (width, height)
    };

    let mut nodes = Vec::with_capacity(tasks.len());
    let mut bottlenecks = 0usize;
    for summary in tasks {
        let task = &summary.task;
        let (layer, x, y) = positions[&task.id];
        let downstream_blocked = downstream_counts.get(&task.id).copied().unwrap_or(0);
        let is_bottleneck = !matches!(task.status, TaskStatus::Done) && downstream_blocked > 0;
        if is_bottleneck {
            bottlenecks += 1;
        }
        let status = status_label(task.status, task.waiting_reason.as_deref());
        let time = work_elapsed(summary, when)
            .unwrap_or_else(|| when.moment(summary.completed_at.unwrap_or(task.created_at)));
        nodes.push(DagNode {
            id: task.id,
            title: task.title.clone(),
            project: summary.project.clone(),
            agent: summary.agent.clone(),
            status,
            task_status: task.status,
            time,
            layer,
            x,
            y,
            width: DAG_NODE_W,
            height: DAG_NODE_H,
            downstream_blocked,
            is_bottleneck,
            quiet: task.status == TaskStatus::Done,
        });
    }

    let mut edges = Vec::with_capacity(edge_pairs.len());
    let mut active_edges = 0usize;
    for (from, to) in edge_pairs {
        let (_, fx, fy) = positions[&from];
        let (_, tx, ty) = positions[&to];
        let from_status = by_id[&from].task.status;
        let to_status = by_id[&to].task.status;
        let state = classify_task_edge(from_status, to_status);
        if state == EdgeState::Active {
            active_edges += 1;
        }
        edges.push(GraphEdge {
            id: format!("edge-{from}-{to}"),
            x1: fx + DAG_NODE_W,
            y1: fy + DAG_NODE_H * 0.5,
            x2: tx,
            y2: ty + DAG_NODE_H * 0.5,
            state,
            label: None,
        });
    }

    TaskDag {
        nodes,
        edges,
        width,
        height,
        active_edges,
        bottlenecks,
        has_dependencies,
    }
}

fn compute_layer(
    id: TaskId,
    parents: &HashMap<TaskId, Vec<TaskId>>,
    memo: &mut HashMap<TaskId, usize>,
    visiting: &mut HashSet<TaskId>,
) -> usize {
    if let Some(&layer) = memo.get(&id) {
        return layer;
    }
    if !visiting.insert(id) {
        return 0;
    }
    let layer = parents
        .get(&id)
        .map(|deps| {
            deps.iter()
                .map(|&dep| compute_layer(dep, parents, memo, visiting) + 1)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    visiting.remove(&id);
    memo.insert(id, layer);
    layer
}

fn status_priority(status: TaskStatus) -> u8 {
    match status {
        TaskStatus::WaitingOnYou | TaskStatus::Proposed => 0,
        TaskStatus::Failed => 1,
        TaskStatus::Running | TaskStatus::InReview => 2,
        TaskStatus::Queued | TaskStatus::Sleeping => 3,
        TaskStatus::Done | TaskStatus::Cancelled => 4,
    }
}

fn classify_task_edge(from: TaskStatus, to: TaskStatus) -> EdgeState {
    if matches!(to, TaskStatus::Running | TaskStatus::InReview)
        || (matches!(from, TaskStatus::Running | TaskStatus::InReview)
            && matches!(to, TaskStatus::Queued | TaskStatus::Proposed))
    {
        EdgeState::Active
    } else if matches!(
        from,
        TaskStatus::Failed | TaskStatus::WaitingOnYou | TaskStatus::Cancelled
    ) && !to.is_terminal()
    {
        EdgeState::Blocked
    } else if from == TaskStatus::Done {
        EdgeState::Satisfied
    } else {
        EdgeState::Pending
    }
}

// ---------------------------------------------------------------------------
// Live Agent Topology ("Org & Handoff" Canvas on Team)
// ---------------------------------------------------------------------------

pub const TOPO_NODE_W: f32 = 216.0;
pub const TOPO_NODE_H: f32 = 86.0;
const TOPO_COL_GAP: f32 = 96.0;
const TOPO_ROW_GAP: f32 = 18.0;

/// What kind of entity a Topology Canvas node represents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopologyNodeKind {
    SpaceLead {
        space: String,
    },
    Project {
        space: String,
        project: String,
        ceiling: Option<Mode>,
        paused: bool,
    },
    Agent {
        space: String,
        agent: String,
        role: String,
        mode: Option<Mode>,
    },
}

/// One interactive node on the Team Topology Canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct TopologyNode {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub badge: Option<String>,
    pub active_label: Option<String>,
    pub running: bool,
    pub waiting: bool,
    pub failed: bool,
    pub kind: TopologyNodeKind,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Positioned Live Agent Topology graph ready for GPUI rendering.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TopologyGraph {
    pub nodes: Vec<TopologyNode>,
    pub edges: Vec<GraphEdge>,
    pub width: f32,
    pub height: f32,
    pub active_delegations: usize,
    pub projects: Vec<String>,
    pub selected_project: Option<String>,
}

impl AppState {
    /// Builds the 3-tier live Org / Handoff topology for `space`:
    /// `Space Lead` -> `Project Leads` -> `Specialists (Engineer / Reviewer / ...)`.
    #[must_use]
    pub fn agent_topology(
        &self,
        space_name: &str,
        project_filter: Option<&str>,
        when: &When,
    ) -> TopologyGraph {
        let Some(space) = self.space(space_name) else {
            return TopologyGraph::default();
        };

        let all_projects: Vec<String> = space
            .active_projects()
            .filter(|p| !p.is_general)
            .map(|p| p.name.clone())
            .collect();
        let selected_project = project_filter
            .filter(|name| all_projects.iter().any(|p| p == name))
            .map(str::to_owned);
        let visible_projects: Vec<String> = match selected_project.as_deref() {
            Some(only) => vec![only.to_owned()],
            None => all_projects.clone(),
        };

        // Specialists: all agents in the space (putting Lead in Tier 0 if there
        // are other agents; if Lead is the only agent, keep it visible in Tier 2 too).
        let has_specialists = space.agents.iter().any(|a| a.name != "Lead");
        let specialists: Vec<_> = space
            .agents
            .iter()
            .filter(|a| !has_specialists || a.name != "Lead")
            .collect();

        // Lookup matrix of agent modes per project.
        let grid = self.modes(space_name);
        let mut mode_lookup: HashMap<(&str, &str), Mode> = HashMap::new();
        for row in &grid.rows {
            if let Ok(modes) = &row.modes {
                for (agent, mode) in grid.agents.iter().zip(modes.iter()) {
                    if let Some(mode) = mode {
                        mode_lookup.insert((row.project.as_str(), agent.as_str()), *mode);
                    }
                }
            }
        }

        // Compute column vertical centering so the 3-tier org chart looks balanced.
        let project_rows = visible_projects.len().max(1);
        let specialist_rows = specialists.len().max(1);
        let max_rows = project_rows.max(specialist_rows);
        let content_h =
            max_rows as f32 * TOPO_NODE_H + max_rows.saturating_sub(1) as f32 * TOPO_ROW_GAP;
        let col_top = |count: usize| -> f32 {
            let h = count as f32 * TOPO_NODE_H + count.saturating_sub(1) as f32 * TOPO_ROW_GAP;
            CANVAS_PAD + (content_h - h).max(0.0) * 0.5
        };

        let col0_x = CANVAS_PAD;
        let col1_x = CANVAS_PAD + TOPO_NODE_W + TOPO_COL_GAP;
        let col2_x = CANVAS_PAD + 2.0 * (TOPO_NODE_W + TOPO_COL_GAP);

        let mut nodes = Vec::new();
        let mut node_coords: HashMap<String, (f32, f32)> = HashMap::new();

        // Tier 0: Space Lead
        let general_running = self.is_working(space_name, "General");
        let space_lead_id = format!("topo-lead-{space_name}");
        let lead_y = col_top(1);
        node_coords.insert(space_lead_id.clone(), (col0_x, lead_y));
        nodes.push(TopologyNode {
            id: space_lead_id.clone(),
            title: "Space Lead".to_owned(),
            subtitle: format!("{space_name} · General"),
            badge: Some("Orchestrator".to_owned()),
            active_label: general_running.then(|| "Working now".to_owned()),
            running: general_running,
            waiting: self.queue().iter().any(|q| q.space == space_name),
            failed: false,
            kind: TopologyNodeKind::SpaceLead {
                space: space_name.to_owned(),
            },
            x: col0_x,
            y: lead_y,
            width: TOPO_NODE_W,
            height: TOPO_NODE_H,
        });

        // Tier 1: Projects
        let proj_top = col_top(visible_projects.len().max(1));
        for (idx, project) in visible_projects.iter().enumerate() {
            let id = format!("topo-project-{space_name}-{project}");
            let y = proj_top + idx as f32 * (TOPO_NODE_H + TOPO_ROW_GAP);
            let running = self.is_working(space_name, project);
            let paused = self.is_paused(space_name, project);
            let ceiling = self.project_ceiling(space_name, project);
            let waiting = space
                .tasks
                .iter()
                .any(|t| t.project == *project && t.task.status == TaskStatus::WaitingOnYou);
            let failed = space
                .tasks
                .iter()
                .any(|t| t.project == *project && t.task.status == TaskStatus::Failed);
            let active_count = space
                .tasks
                .iter()
                .filter(|t| {
                    t.project == *project
                        && matches!(t.task.status, TaskStatus::Running | TaskStatus::InReview)
                })
                .count();
            let active_label = if paused {
                Some("Paused".to_owned())
            } else if active_count > 0 {
                Some(format!("{active_count} active"))
            } else if running {
                Some("Lead working".to_owned())
            } else {
                None
            };
            let badge = Some(format!("Ceiling: {}", crate::mode_label(ceiling)));
            node_coords.insert(id.clone(), (col1_x, y));
            nodes.push(TopologyNode {
                id,
                title: project.clone(),
                subtitle: "Project lead".to_owned(),
                badge,
                active_label,
                running,
                waiting,
                failed,
                kind: TopologyNodeKind::Project {
                    space: space_name.to_owned(),
                    project: project.clone(),
                    ceiling: Some(ceiling),
                    paused,
                },
                x: col1_x,
                y,
                width: TOPO_NODE_W,
                height: TOPO_NODE_H,
            });
        }

        // Tier 2: Specialists
        let spec_top = col_top(specialists.len().max(1));
        for (idx, agent) in specialists.iter().enumerate() {
            let id = format!("topo-agent-{space_name}-{}", agent.name);
            let y = spec_top + idx as f32 * (TOPO_NODE_H + TOPO_ROW_GAP);
            let active_task = space.tasks.iter().find(|t| {
                t.agent == agent.name
                    && visible_projects.iter().any(|p| p == &t.project)
                    && matches!(t.task.status, TaskStatus::Running | TaskStatus::InReview)
            });
            let waiting = space.tasks.iter().any(|t| {
                t.agent == agent.name
                    && visible_projects.iter().any(|p| p == &t.project)
                    && t.task.status == TaskStatus::WaitingOnYou
            });
            let failed = space.tasks.iter().any(|t| {
                t.agent == agent.name
                    && visible_projects.iter().any(|p| p == &t.project)
                    && t.task.status == TaskStatus::Failed
            });
            let mode = selected_project
                .as_deref()
                .and_then(|proj| mode_lookup.get(&(proj, agent.name.as_str())).copied())
                .or_else(|| {
                    (visible_projects.len() == 1)
                        .then(|| {
                            mode_lookup
                                .get(&(visible_projects[0].as_str(), agent.name.as_str()))
                                .copied()
                        })
                        .flatten()
                });
            let badge = mode.map(|m| crate::mode_label(m).to_owned()).or_else(|| {
                let bound_count = visible_projects
                    .iter()
                    .filter(|p| {
                        mode_lookup
                            .get(&(p.as_str(), agent.name.as_str()))
                            .is_some_and(|m| *m != Mode::Off)
                    })
                    .count();
                (bound_count > 0).then(|| format!("Active in {bound_count}"))
            });
            let active_label = active_task.map(|t| {
                work_elapsed(t, when).map_or_else(
                    || t.task.id.to_string(),
                    |el| format!("{} · {el}", t.task.id),
                )
            });
            let backends = if agent.backends.is_empty() {
                "any backend".to_owned()
            } else {
                agent
                    .backends
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" / ")
            };
            node_coords.insert(id.clone(), (col2_x, y));
            nodes.push(TopologyNode {
                id,
                title: agent.name.clone(),
                subtitle: format!("{} · {backends}", agent.role),
                badge,
                active_label,
                running: active_task.is_some(),
                waiting,
                failed,
                kind: TopologyNodeKind::Agent {
                    space: space_name.to_owned(),
                    agent: agent.name.clone(),
                    role: agent.role.clone(),
                    mode,
                },
                x: col2_x,
                y,
                width: TOPO_NODE_W,
                height: TOPO_NODE_H,
            });
        }

        // Build live delegation & handoff edges.
        let mut edges = Vec::new();
        let mut active_delegations = 0usize;

        // Space Lead -> Project edges
        for project in &visible_projects {
            let proj_id = format!("topo-project-{space_name}-{project}");
            let (lx, ly) = node_coords[&space_lead_id];
            let (px, py) = node_coords[&proj_id];
            let running = self.is_working(space_name, project);
            let waiting = space
                .tasks
                .iter()
                .any(|t| t.project == *project && t.task.status == TaskStatus::WaitingOnYou);
            let state = if running {
                active_delegations += 1;
                EdgeState::Active
            } else if waiting {
                EdgeState::Blocked
            } else {
                EdgeState::Pending
            };
            edges.push(GraphEdge {
                id: format!("topo-edge-lead-{project}"),
                x1: lx + TOPO_NODE_W,
                y1: ly + TOPO_NODE_H * 0.5,
                x2: px,
                y2: py + TOPO_NODE_H * 0.5,
                state,
                label: None,
            });
        }

        // Project -> Specialist edges
        let mut seen_edges: BTreeSet<(String, String)> = BTreeSet::new();
        for project in &visible_projects {
            let proj_id = format!("topo-project-{space_name}-{project}");
            let (px, py) = node_coords[&proj_id];
            for agent in &specialists {
                let mode = mode_lookup
                    .get(&(project.as_str(), agent.name.as_str()))
                    .copied();
                let active_task = space.tasks.iter().find(|t| {
                    t.project == *project
                        && t.agent == agent.name
                        && matches!(t.task.status, TaskStatus::Running | TaskStatus::InReview)
                });
                let waiting_or_failed = space.tasks.iter().any(|t| {
                    t.project == *project
                        && t.agent == agent.name
                        && matches!(t.task.status, TaskStatus::WaitingOnYou | TaskStatus::Failed)
                });
                // Connect if bound (mode != Off) OR currently has an active/blocked task.
                let connected = mode.is_some_and(|m| m != Mode::Off)
                    || active_task.is_some()
                    || waiting_or_failed;
                if !connected {
                    continue;
                }
                let agent_id = format!("topo-agent-{space_name}-{}", agent.name);
                if !seen_edges.insert((proj_id.clone(), agent_id.clone())) {
                    continue;
                }
                let (ax, ay) = node_coords[&agent_id];
                let (state, label) = if let Some(task) = active_task {
                    active_delegations += 1;
                    (EdgeState::Active, Some(task.task.id.to_string()))
                } else if waiting_or_failed {
                    (EdgeState::Blocked, None)
                } else if matches!(mode, Some(Mode::Autonomous | Mode::Assisted)) {
                    (EdgeState::Satisfied, None)
                } else {
                    (EdgeState::Pending, None)
                };
                edges.push(GraphEdge {
                    id: format!("topo-edge-{project}-{}", agent.name),
                    x1: px + TOPO_NODE_W,
                    y1: py + TOPO_NODE_H * 0.5,
                    x2: ax,
                    y2: ay + TOPO_NODE_H * 0.5,
                    state,
                    label,
                });
            }
        }

        let width = CANVAS_PAD * 2.0 + 3.0 * TOPO_NODE_W + 2.0 * TOPO_COL_GAP;
        let height = CANVAS_PAD * 2.0 + content_h;

        TopologyGraph {
            nodes,
            edges,
            width,
            height,
            active_delegations,
            projects: all_projects,
            selected_project,
        }
    }
}
