//! Measures encoding and decoding the hub's assignment to a runner.

use std::hint::black_box;
use std::time::Duration;

use serde_json::json;
use threadlane_core::backend::NativeSession;
use threadlane_core::ids::{EventId, ProjectId, RunId, SessionId, TaskId};
use threadlane_core::link::{Begin, RunAssignment};
use threadlane_core::model::{BackendKind, LaneKind};
use threadlane_core::tools::{Tier, ToolDefinition};

const SAMPLES: usize = 10;
const CONVERSIONS: usize = 20;

#[hotpath::measure]
fn encode(assignment: &Begin) {
    for _ in 0..CONVERSIONS {
        black_box(serde_json::to_vec(black_box(assignment)).expect("encode run assignment"));
    }
}

#[hotpath::measure]
fn decode(wire: &[u8]) {
    for _ in 0..CONVERSIONS {
        black_box(serde_json::from_slice::<Begin>(black_box(wire)).expect("decode run assignment"));
    }
}

#[hotpath::main(percentiles = [50, 95])]
fn main() {
    let assignment = Begin::Run(Box::new(RunAssignment {
        images: Vec::new(),
        dreaming: false,
        space_lead_ask: None,
        run_id: RunId(1),
        session_id: SessionId(1),
        project_id: ProjectId(1),
        lane_kind: LaneKind::Task,
        task_id: Some(TaskId(1)),
        agent_name: "Developer".into(),
        repositories: vec![],
        review: None,
        pr_review: None,
        start_patches: Vec::new(),
        events: (1..=32).map(EventId).collect(),
        perform: vec![],
        backend: BackendKind::Codex,
        model: None,
        native_session: NativeSession::Resume {
            id: "session-1".into(),
        },
        skills: Vec::new(),
        appended_instructions: "Project instructions and conventions.\n".repeat(128),
        prompt: "Recent updates to the task.\n".repeat(256),
        tools: (1..=16)
            .map(|index| ToolDefinition {
                name: format!("fixture_tool_{index}"),
                description: "Read a project record by its identifier.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"id": {"type": "string"}},
                    "required": ["id"]
                }),
                tier: Tier::Read,
                safe_to_repeat: true,
            })
            .collect(),
        remove_builtin_tools: false,
        deny_rules: vec![],
        wall_time_limit: Duration::from_secs(600),
        max_turns: Some(40),
        trace_id: "trace-1".into(),
    }));
    let wire = serde_json::to_vec(&assignment).expect("encode fixture");
    let Begin::Run(decoded) = serde_json::from_slice(&wire).expect("decode fixture") else {
        panic!("fixture must contain a run assignment");
    };
    assert_eq!(
        decoded.prompt.len(),
        "Recent updates to the task.\n".len() * 256
    );
    assert_eq!(decoded.tools.len(), 16);

    for _ in 0..SAMPLES {
        encode(&assignment);
        decode(&wire);
    }
}
