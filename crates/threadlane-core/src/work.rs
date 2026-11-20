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
        "Check merge readiness for open pull requests in this project's registered repositories. Verify the current head, CI, conflicts and unresolved threads. Look up pr_review_status for that head. Reuse its verdict or wait for the active review. Only for an unreviewed head, assign {reviewer} with create_task.pr_review. Route requested changes to the original task or propose a fix for {engineer}. Send the owner the PR, findings and next decision."
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

/// A project-owned graph. Each incoming edge is a task that must finish first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowGraph {
    pub version: i64,
    pub steps: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub id: u32,
    pub name: String,
    pub agent: String,
    pub instructions: String,
    pub after: Vec<u32>,
    pub owner_review: bool,
    /// Canvas coordinates at the default 16px rem. Older graphs use automatic placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[u16; 2]>,
}

impl WorkflowGraph {
    pub const MAX_STEPS: usize = 32;

    pub fn from_review(recipe: &ReviewRecipe) -> Self {
        Self {
            version: 0,
            steps: vec![
                WorkflowStep {
                    id: 1,
                    name: "Implement".into(),
                    agent: "Engineer".into(),
                    instructions: recipe.implementation.clone(),
                    after: vec![],
                    owner_review: false,
                    position: None,
                },
                WorkflowStep {
                    id: 2,
                    name: "Review".into(),
                    agent: "Reviewer".into(),
                    instructions: recipe.review.clone(),
                    after: vec![1],
                    owner_review: true,
                    position: None,
                },
            ],
        }
    }

    /// Topological layers, shared by the canvas layout and the hub's task creation.
    pub fn layers(&self) -> Result<Vec<Vec<&WorkflowStep>>, &'static str> {
        use std::collections::HashSet;
        if self.steps.len() > Self::MAX_STEPS {
            return Err("A workflow can have up to 32 steps.");
        }
        let ids: HashSet<_> = self.steps.iter().map(|step| step.id).collect();
        if ids.len() != self.steps.len() {
            return Err("Step IDs must be unique.");
        }
        for step in &self.steps {
            let mut seen = HashSet::new();
            for id in &step.after {
                if !ids.contains(id) {
                    return Err("A connection points to a missing step.");
                }
                if !seen.insert(id) {
                    return Err("These steps are already connected.");
                }
            }
        }
        let mut done = HashSet::new();
        let mut layers = Vec::new();
        while done.len() < self.steps.len() {
            let ready: Vec<_> = self
                .steps
                .iter()
                .filter(|step| {
                    !done.contains(&step.id) && step.after.iter().all(|id| done.contains(id))
                })
                .collect();
            if ready.is_empty() {
                return Err("This connection would create a cycle.");
            }
            done.extend(ready.iter().map(|step| step.id));
            layers.push(ready);
        }
        Ok(layers)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.steps.is_empty() {
            return Err("Add a step before saving.");
        }
        self.layers()?;
        for step in &self.steps {
            if step.name.trim().is_empty()
                || step.name.len() > 128
                || step.name.contains(['\r', '\n'])
            {
                return Err("Give each step a single-line name of up to 128 bytes.");
            }
            if step.agent.trim().is_empty()
                || step.agent.len() > 128
                || step.agent.contains(['\r', '\n'])
            {
                return Err("Choose an agent for each step.");
            }
            if step.instructions.trim().is_empty() || step.instructions.len() > 8192 {
                return Err("Give each step instructions of up to 8 KiB.");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod graph_tests {
    use super::*;

    #[test]
    fn invalid_connections_cannot_be_saved() {
        let base = WorkflowGraph::from_review(&ReviewRecipe::default());
        for incoming in [vec![1], vec![2], vec![99]] {
            let mut graph = base.clone();
            graph.steps[0].after = incoming;
            assert!(graph.validate().is_err());
        }
        let mut graph = base;
        graph.steps[1].after.push(1);
        assert!(graph.validate().is_err());
        graph.steps[1].after.clear();
        graph.steps[1].id = 1;
        assert!(graph.validate().is_err());
        graph.steps.clear();
        assert!(graph.validate().is_err());
    }
}
