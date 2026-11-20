//! Owner-facing routine activity and project outputs.
use crate::ids::{EventId, RunId, TaskId, TimerId};
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    pub event: EventId,
    pub timer: TimerId,
    pub project: String,
    pub instruction: String,
    pub due: Timestamp,
    pub started: Option<Timestamp>,
    pub manual: bool,
    pub input_count: u32,
    pub status: String,
    pub result: Option<String>,
    pub tasks: Vec<TaskId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scheduled {
    pub project: String,
    pub timer: TimerId,
    pub instruction: String,
    pub due: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageGroup {
    pub project: String,
    pub agent: String,
    pub backend: String,
    pub runs: u64,
    pub duration_ms: u64,
    pub tokens: u64,
    pub token_reports: u64,
    pub cost: f64,
    pub cost_reports: u64,
    pub tasks: Vec<TaskId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputSource {
    PullRequest(String),
    Report(String),
    Attachment { event: EventId, index: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectOutput {
    pub name: String,
    pub at: Timestamp,
    pub source: OutputSource,
    pub task: Option<TaskId>,
    pub run: Option<RunId>,
}
