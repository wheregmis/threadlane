//! Owner-facing routine activity, project outputs, and reusable review workflows.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRecipe {
    pub implementation: String,
    pub review: String,
    pub version: i64,
}

/// Shared by the installation preview and the routine saved on the hub.
pub fn review_routine(engineer: &str, reviewer: &str) -> String {
    format!(
        "Review open pull requests in this project's registered repositories. Ask {reviewer} to review one PR that still needs review. If changes are needed, propose a task for {engineer}. Send the owner the PR, findings and next decision."
    )
}

impl Default for ReviewRecipe {
    fn default() -> Self {
        Self {
            implementation: "Implement the requested change and record the branch, checks and results for review.".into(),
            review: "Review the implementation and its checks. Summarize your findings for the owner.".into(),
            version: 0,
        }
    }
}
