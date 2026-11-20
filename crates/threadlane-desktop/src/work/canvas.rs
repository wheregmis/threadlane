//! Project workflow editor. Connections on the canvas become task dependencies at launch.
use super::*;
use crate::components::graph_canvas::static_edge_layer;
use gpui_kit::component::{checkbox::Checkbox, scroll::ScrollbarAxis};
use std::collections::HashMap;
use threadlane_core::work::{WorkflowGraph, WorkflowStep};
use threadlane_desktop::{EdgeState, GraphEdge};

const NODE_WIDTH: u16 = 192;
const NODE_HEIGHT: u16 = 56;
const INSET: u16 = 16;
type Positions = HashMap<u32, [u16; 2]>;

/// Place older and newly added steps in the free slots around positioned nodes.
fn layout(graph: &WorkflowGraph) -> Result<Positions, &'static str> {
    let mut positions: Positions = graph
        .steps
        .iter()
        .filter_map(|s| Some((s.id, s.position?)))
        .collect();
    for (column, layer) in graph.layers()?.iter().enumerate() {
        let x = INSET + column as u16 * (NODE_WIDTH + 3 * INSET);
        let mut y = INSET;
        for step in layer {
            if positions.contains_key(&step.id) {
                continue;
            }
            while positions.values().any(|&[a, b]| {
                a.abs_diff(x) < NODE_WIDTH + INSET && b.abs_diff(y) < NODE_HEIGHT + INSET
            }) {
                y += NODE_HEIGHT + INSET;
            }
            positions.insert(step.id, [x, y]);
            y += NODE_HEIGHT + INSET;
        }
    }
    Ok(positions)
}

struct MoveStep {
    id: u32,
    generation: u64,
}
struct LinkStep {
    id: u32,
    generation: u64,
}

struct DragPreview;
impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

#[derive(Clone, Copy)]
enum Preview {
    Move(u32, [u16; 2]),
    Link(u32, Point<Pixels>),
}

fn board_point(pointer: Point<Pixels>, scroll: &ScrollHandle) -> Point<Pixels> {
    pointer - scroll.bounds().origin - scroll.offset()
}

fn moved_position(
    pointer: Point<Pixels>,
    scroll: &ScrollHandle,
    grab: Point<Pixels>,
    unit: Pixels,
) -> [u16; 2] {
    let at = board_point(pointer, scroll) - grab;
    let scale = 16. / f32::from(unit);
    [at.x, at.y].map(|v| (f32::from(v) * scale).round() as u16)
}

struct StepFields {
    name: Entity<InputState>,
    instructions: Entity<TextareaState>,
}

impl StepFields {
    fn new(step: &WorkflowStep, window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).default_value(step.name.clone()));
        let instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .default_value(step.instructions.clone())
        });
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        cx.observe(&instructions, |_, _, cx| cx.notify()).detach();
        Self { name, instructions }
    }
}

pub(super) struct CanvasDraft {
    graph: WorkflowGraph,
    fields: HashMap<u32, StepFields>,
    selected: Option<u32>,
    connecting: bool,
    preview: Option<Preview>,
    grab: Point<Pixels>,
    scroll: ScrollHandle,
}

impl CanvasDraft {
    fn new(graph: WorkflowGraph, window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let fields = graph
            .steps
            .iter()
            .map(|step| (step.id, StepFields::new(step, window, cx)))
            .collect();
        Self {
            selected: graph.steps.first().map(|step| step.id),
            graph,
            fields,
            connecting: false,
            preview: None,
            grab: Point::default(),
            scroll: ScrollHandle::new(),
        }
    }

    pub(super) fn saved(&mut self, version: i64) {
        self.graph.version = version;
    }
    pub(super) fn step_mut(&mut self, id: u32) -> Option<&mut WorkflowStep> {
        self.graph.steps.iter_mut().find(|step| step.id == id)
    }
    fn snapshot(&self, cx: &App) -> WorkflowGraph {
        let mut graph = self.graph.clone();
        for step in &mut graph.steps {
            let fields = &self.fields[&step.id];
            step.name = fields.name.read(cx).value().to_string();
            step.instructions = fields.instructions.read(cx).value().to_string();
        }
        graph
    }

    fn select(&mut self, id: u32) -> Result<(), &'static str> {
        if self.connecting
            && let Some(source) = self.selected
        {
            return self.connect(source, id);
        }
        self.selected = Some(id);
        Ok(())
    }

    fn connect(&mut self, source: u32, id: u32) -> Result<(), &'static str> {
        let positions = layout(&self.graph)?;
        self.step_mut(id)
            .ok_or("Select an existing step.")?
            .after
            .push(source);
        if let Err(error) = self.graph.layers() {
            self.step_mut(id).unwrap().after.pop();
            return Err(error);
        }
        for step in &mut self.graph.steps {
            step.position = Some(positions[&step.id]);
        }
        self.connecting = false;
        self.selected = Some(id);
        Ok(())
    }
}

impl Desktop {
    pub(super) fn workflow_canvas(
        &mut self,
        space: &str,
        project: &str,
        saved: Option<&WorkflowGraph>,
        recipe: &ReviewRecipe,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let baseline = saved.cloned().unwrap_or_else(|| {
            let mut graph = WorkflowGraph::from_review(recipe);
            graph.steps[0].agent.clone_from(&self.work.engineer);
            graph.steps[1].agent.clone_from(&self.work.reviewer);
            graph
        });
        if self.work.canvas.is_none() {
            self.work.canvas = Some(CanvasDraft::new(baseline.clone(), window, cx));
        }
        let draft = self.work.canvas.as_ref().unwrap();
        let graph = draft.snapshot(cx);
        let selected = draft.selected;
        let connecting = draft.connecting;
        let dirty = saved != Some(&graph);
        let validation = graph.validate().err();
        let saving = self.work.saving;
        let agents: Vec<String> = self
            .state
            .project(space, project)
            .into_iter()
            .flat_map(|p| &p.bindings)
            .filter(|b| !b.is_lead && b.mode != Mode::Off)
            .map(|b| b.agent.clone())
            .collect();
        let default_agent = agents.first().cloned().unwrap_or_default();
        let (s, p) = (space.to_owned(), project.to_owned());
        let toolbar = h_flex()
            .flex_wrap()
            .gap(SM)
            .child(
                button("canvas-add")
                    .small()
                    .icon(AppIcon::Plus)
                    .label("Add step")
                    .disabled(saving || graph.steps.len() >= WorkflowGraph::MAX_STEPS)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let Some(draft) = &mut this.work.canvas else {
                            return;
                        };
                        let id = (1..).find(|id| !draft.fields.contains_key(id)).unwrap();
                        let step = WorkflowStep {
                            id,
                            name: format!("Step {id}"),
                            instructions: String::new(),
                            agent: default_agent.clone(),
                            after: vec![],
                            owner_review: false,
                            position: None,
                        };
                        draft.fields.insert(id, StepFields::new(&step, window, cx));
                        draft.graph.steps.push(step);
                        draft.selected = Some(id);
                        draft.connecting = false;
                        cx.notify();
                    })),
            )
            .child(
                button("canvas-connect")
                    .small()
                    .selected(connecting)
                    .icon(AppIcon::GitFork)
                    .label(if connecting {
                        "Cancel connection"
                    } else {
                        "Connect from step"
                    })
                    .disabled(saving || selected.is_none() || graph.steps.len() < 2)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(draft) = &mut this.work.canvas {
                            draft.connecting = !draft.connecting;
                        }
                        cx.notify();
                    })),
            )
            .child(
                button("save-canvas")
                    .small()
                    .icon(AppIcon::Save)
                    .label("Save project workflow")
                    .disabled(saving || !dirty || validation.is_some())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let Some(draft) = &this.work.canvas else {
                            return;
                        };
                        let graph = draft.snapshot(cx);
                        this.workflow_action(
                            s.clone(),
                            p.clone(),
                            TaskRequest::SaveProjectWorkflow {
                                project: p.clone(),
                                graph,
                            },
                            cx,
                        );
                    })),
            );

        let board = self.canvas_board(
            &graph,
            (space, project),
            graph == baseline && validation.is_none(),
            window,
            cx,
        );
        let inspector = graph
            .steps
            .iter()
            .find(|step| Some(step.id) == selected)
            .map(|step| self.step_inspector(step, &graph, &agents, cx));
        let repositories = self.state.task_repositories(space, project);
        if self.work.repository.is_empty()
            && let [repo] = repositories.as_slice()
        {
            self.work.repository.clone_from(repo);
        }
        let blocker = if dirty {
            Some("Save the project workflow before starting.")
        } else if graph.steps.iter().any(|step| !agents.contains(&step.agent)) {
            Some("Choose an enabled project specialist for every step.")
        } else {
            workflow_start_blocker(
                false,
                self.state.project_ceiling(space, project) == Mode::Off,
                true,
                repositories.contains(&self.work.repository),
                self.work.goal.read(cx).value().as_str(),
            )
        };
        let (s, p) = (space.to_owned(), project.to_owned());
        let version = graph.version;
        let start = button("launch-canvas")
            .primary()
            .icon(AppIcon::Play)
            .label("Start workflow")
            .disabled(saving || blocker.is_some())
            .on_click(cx.listener(move |this, _, _, cx| {
                let mut request = TaskRequest::StartProjectWorkflow {
                    project: p.clone(),
                    repository: this.work.repository.clone(),
                    goal: this.work.goal.read(cx).value().to_string(),
                    version,
                    client_request_id: String::new(),
                };
                this.work.identify_run(&mut request);
                this.workflow_action(s.clone(), p.clone(), request, cx);
            }));
        let hint = if connecting {
            Some("Select the step that should run next.")
        } else {
            validation
        };
        v_flex()
            .gap(LG)
            .child(toolbar)
            .children(hint.map(|hint| meta(hint, cx)))
            .child(board)
            .children(inspector)
            .child(
                card(cx)
                    .id("canvas-launch")
                    .child(section("Run workflow"))
                    .child(meta("Goal", cx))
                    .child(
                        Textarea::new(&self.work.goal)
                            .aria_label("Workflow goal")
                            .disabled(saving),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap(SM)
                            .child(meta("Repository", cx))
                            .child(self.workflow_picker(
                                WorkflowChoice::Repository,
                                &self.work.repository,
                                repositories,
                                cx,
                            ))
                            .child(start),
                    )
                    .children(blocker.map(|reason| meta(reason, cx))),
            )
            .child(self.workflow_runs(space, project, cx))
    }

    fn canvas_board(
        &self,
        graph: &WorkflowGraph,
        project_key: (&str, &str),
        autosave: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let draft = self.work.canvas.as_ref().unwrap();
        let generation = self.work.generation;
        let scale = f32::from(window.rem_size()) / 16.;
        let (w, h) = (
            f32::from(NODE_WIDTH) * scale,
            f32::from(NODE_HEIGHT) * scale,
        );
        let preview = draft.preview.filter(|_| cx.has_active_drag());
        let mut positions = match layout(graph) {
            Ok(positions) => positions,
            Err(reason) => return meta(reason, cx),
        };
        if let Some(Preview::Move(id, position)) = preview {
            positions.insert(id, position);
        }
        let positions: HashMap<_, _> = positions
            .into_iter()
            .map(|(id, [x, y])| (id, (f32::from(x) * scale, f32::from(y) * scale)))
            .collect();
        let width = positions
            .values()
            .map(|p| p.0 + w + 48. * scale)
            .fold(0., f32::max)
            .max(f32::from(draft.scroll.bounds().size.width));
        let height = positions
            .values()
            .map(|p| p.1 + h + 48. * scale)
            .fold(0., f32::max)
            .max(224. * scale);
        let mut edges = Vec::new();
        for step in &graph.steps {
            let (x2, y2) = positions[&step.id];
            for source in &step.after {
                let (x1, y1) = positions[source];
                edges.push(GraphEdge {
                    id: format!("{source}-{}", step.id),
                    x1: x1 + w,
                    y1: y1 + h / 2.,
                    x2,
                    y2: y2 + h / 2.,
                    state: EdgeState::Pending,
                    label: None,
                });
            }
        }
        if let Some(Preview::Link(source, point)) = preview
            && let Some(&(x, y)) = positions.get(&source)
        {
            edges.push(GraphEdge {
                id: "new-connection".into(),
                x1: x + w,
                y1: y + h / 2.,
                x2: point.x.into(),
                y2: point.y.into(),
                state: EdgeState::Active,
                label: None,
            });
        }
        let (space, project) = (project_key.0.to_owned(), project_key.1.to_owned());
        let mut surface = div()
            .relative()
            .w(px(width))
            .min_w_full()
            .h(px(height))
            .child(static_edge_layer("workflow-edges", edges, width, height));
        for step in &graph.steps {
            let id = step.id;
            let (x, y) = positions[&id];
            let name = if step.name.is_empty() {
                "Untitled step"
            } else {
                &step.name
            };
            let agent = if step.agent.is_empty() {
                "Choose agent"
            } else {
                &step.agent
            };
            let details = format!("{name} · {agent}");
            let enabled = !self.work.saving;
            let port = div()
                .id(("workflow-output", id as usize))
                .absolute()
                .right(-rems(0.625))
                .top(rems(1.125))
                .size(rems(1.25))
                .flex()
                .items_center()
                .justify_center()
                .cursor_crosshair()
                .child(
                    div()
                        .size_2()
                        .rounded_full()
                        .bg(cx.theme().muted_foreground),
                )
                .when(enabled, |port| {
                    port.on_drag(LinkStep { id, generation }, |_, _, _, cx| {
                        cx.new(|_| DragPreview)
                    })
                });
            surface = surface.child(
                div()
                    .id(("workflow-movable", id as usize))
                    .drag_over::<LinkStep>(move |style, drag, _, cx| {
                        if drag.id == id {
                            return style;
                        }
                        style.rounded(cx.theme().radius).border_2().border_color(cx.theme().primary)
                    })
                    .when(enabled, |node| {
                        node.on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, _| {
                            if let Some(draft) = &mut this.work.canvas {
                                draft.grab = board_point(event.position, &draft.scroll) - point(px(x), px(y));
                                draft.preview = None;
                            }
                        }))
                        .on_drag(MoveStep { id, generation }, |_, _, _, cx| cx.new(|_| DragPreview))
                        .on_drop(cx.listener(move |this, drag: &LinkStep, _, cx| {
                            if this.work.generation == drag.generation && !this.work.saving
                                && let Some(draft) = &mut this.work.canvas
                            {
                                draft.preview = None;
                                if let Err(reason) = draft.connect(drag.id, id) {
                                    this.state.report(Activity::Failed(reason.into()));
                                }
                                cx.notify();
                            }
                        }))
                    })
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(w))
                    .h(px(h))
                    .child(
                        button(("canvas-node", id as usize))
                            .outline()
                            .size_full()
                            .selected(draft.selected == Some(id))
                            .disabled(self.work.saving)
                            .accessibility_label(details.clone())
                            .tooltip(format!("{details}. Drag to move. Drag the right handle to another step to connect."))
                            .cursor_grab()
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .items_start()
                                    .child(div().truncate().child(name.to_owned()))
                                    .child(meta(agent.to_owned(), cx).truncate()),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(draft) = &mut this.work.canvas
                                    && let Err(error) = draft.select(id)
                                {
                                    this.state.report(Activity::Failed(error.into()));
                                }
                                cx.notify();
                            })),
                    ).child(port),
            );
        }
        div()
            .relative()
            .w_full()
            .h(rems(16.))
            .border_1()
            .rounded(cx.theme().radius)
            .border_color(cx.theme().border)
            .child(
                div()
                    .id("workflow-viewport")
                    .size_full()
                    .overflow_scroll()
                    .on_drag_move::<MoveStep>(cx.listener(
                        |this, event: &DragMoveEvent<MoveStep>, window, cx| {
                            let drag = event.drag(cx);
                            if drag.generation == this.work.generation
                                && let Some(draft) = &mut this.work.canvas
                            {
                                draft.preview = Some(Preview::Move(
                                    drag.id,
                                    moved_position(
                                        event.event.position,
                                        &draft.scroll,
                                        draft.grab,
                                        window.rem_size(),
                                    ),
                                ));
                                cx.notify();
                            }
                        },
                    ))
                    .on_drag_move::<LinkStep>(cx.listener(
                        |this, event: &DragMoveEvent<LinkStep>, _, cx| {
                            let drag = event.drag(cx);
                            if drag.generation == this.work.generation
                                && let Some(draft) = &mut this.work.canvas
                            {
                                draft.preview = Some(Preview::Link(
                                    drag.id,
                                    board_point(event.event.position, &draft.scroll),
                                ));
                                cx.notify();
                            }
                        },
                    ))
                    .on_drop(cx.listener(move |this, drag: &MoveStep, window, cx| {
                        if drag.generation != this.work.generation || this.work.saving {
                            return;
                        }
                        let Some(draft) = &mut this.work.canvas else {
                            return;
                        };
                        draft.preview = None;
                        let pos = moved_position(
                            window.mouse_position(),
                            &draft.scroll,
                            draft.grab,
                            window.rem_size(),
                        );
                        if let Ok(mut positions) = layout(&draft.graph)
                            && positions.insert(drag.id, pos) != Some(pos)
                        {
                            for step in &mut draft.graph.steps {
                                step.position = Some(positions[&step.id]);
                            }
                            draft.selected = Some(drag.id);
                            draft.connecting = false;
                            if autosave {
                                let graph = draft.snapshot(cx);
                                this.workflow_action(
                                    space.clone(),
                                    project.clone(),
                                    TaskRequest::SaveProjectWorkflow {
                                        project: project.clone(),
                                        graph,
                                    },
                                    cx,
                                );
                            }
                        }
                        cx.notify();
                    }))
                    .track_scroll(&draft.scroll)
                    .child(surface),
            )
            .scrollbar(&draft.scroll, ScrollbarAxis::Both)
    }

    fn step_inspector(
        &self,
        step: &WorkflowStep,
        graph: &WorkflowGraph,
        agents: &[String],
        cx: &mut Context<Self>,
    ) -> Div {
        let id = step.id;
        let draft = self.work.canvas.as_ref().unwrap();
        let fields = &draft.fields[&id];
        let saving = self.work.saving;
        let remove = button("remove-canvas-step")
            .small()
            .ghost()
            .label("Remove step")
            .disabled(saving)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(draft) = &mut this.work.canvas {
                    draft.graph.steps.retain(|s| s.id != id);
                    for step in &mut draft.graph.steps {
                        step.after.retain(|source| *source != id);
                    }
                    draft.fields.remove(&id);
                    draft.selected = draft.graph.steps.first().map(|s| s.id);
                    draft.connecting = false;
                }
                cx.notify();
            }));
        let mut panel = v_flex()
            .gap(SM)
            .child(
                h_flex()
                    .justify_between()
                    .child(section("Selected step"))
                    .child(remove),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(MD)
                    .child(
                        v_flex()
                            .flex_1()
                            .flex_basis(rems(14.))
                            .gap_1()
                            .child(meta("Step name", cx))
                            .child(field(&fields.name).aria_label("Step name").disabled(saving)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(meta("Agent", cx))
                            .child(self.workflow_picker(
                                WorkflowChoice::Step(id),
                                &step.agent,
                                agents.to_vec(),
                                cx,
                            )),
                    ),
            )
            .child(meta("Instructions", cx))
            .child(
                Textarea::new(&fields.instructions)
                    .aria_label("Step instructions")
                    .disabled(saving),
            )
            .child(
                Checkbox::new("canvas-owner-review")
                    .checked(step.owner_review)
                    .disabled(saving)
                    .label("Require owner approval before completing this step")
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        if let Some(step) = this.work.canvas.as_mut().and_then(|d| d.step_mut(id)) {
                            step.owner_review = *checked;
                        }
                        cx.notify();
                    })),
            );
        if !step.after.is_empty() {
            panel = panel.child(
                h_flex()
                    .flex_wrap()
                    .gap(SM)
                    .child(meta("Runs after", cx))
                    .children(
                        graph
                            .steps
                            .iter()
                            .filter(|s| step.after.contains(&s.id))
                            .map(|source| {
                                let source_id = source.id;
                                let label = format!("Disconnect {}", source.name);
                                button(("unlink-canvas", source_id as usize))
                                    .small()
                                    .outline()
                                    .label(source.name.clone())
                                    .icon(AppIcon::Unlink)
                                    .disabled(saving)
                                    .accessibility_label(label.clone())
                                    .tooltip(label)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(step) =
                                            this.work.canvas.as_mut().and_then(|d| d.step_mut(id))
                                        {
                                            step.after.retain(|s| *s != source_id);
                                        }
                                        cx.notify();
                                    }))
                            }),
                    ),
            );
        }
        panel
    }
}

#[cfg(test)]
mod tests {
    use super::{NODE_HEIGHT, ReviewRecipe, WorkflowGraph, layout};

    #[test]
    fn legacy_layout_keeps_manual_positions_and_finds_room_for_unplaced_steps() {
        let graph = WorkflowGraph::from_review(&ReviewRecipe::default());
        let json = serde_json::to_string(&graph).unwrap();
        assert!(!json.contains("position"));
        let mut graph: WorkflowGraph = serde_json::from_str(&json).unwrap();
        let initial = layout(&graph).unwrap();
        graph.steps[1].position = Some(initial[&1]);
        let moved = layout(&graph).unwrap();
        assert_eq!(moved[&2], initial[&1]);
        assert!(moved[&1][1] >= moved[&2][1] + NODE_HEIGHT);
        assert_eq!(graph.steps[1].after, vec![1]);
    }
}
