use super::agent_worktree_target_matches;
use threadlane_protocol::{
    daemon::{SubagentActivityInfo, SubagentActivityStatus},
    events::SubagentIsolation,
};
use threadlane_ui_kit::AgentWorktreeAction;

fn completed() -> SubagentActivityInfo {
    SubagentActivityInfo {
        batch_run_id: 42,
        task_index: 2,
        journal_run_id: Some("run-original".into()),
        lane: Some("agent-lane".into()),
        agent: "Reviewer".into(),
        task: "Review".into(),
        model: None,
        status: SubagentActivityStatus::Completed,
        messages: Vec::new(),
        isolation: Some(SubagentIsolation {
            branch: "agents/review".into(),
            workspace: "/project/.threadlane/worktrees/review".into(),
        }),
        error: None,
    }
}

#[test]
fn agent_worktree_confirmation_rejects_revived_or_replaced_targets() {
    let expected = completed();
    let mut current = expected.clone();
    assert!(agent_worktree_target_matches(
        &expected,
        &current,
        AgentWorktreeAction::Discard
    ));
    current.status = SubagentActivityStatus::Running;
    assert!(!agent_worktree_target_matches(
        &expected,
        &current,
        AgentWorktreeAction::Discard
    ));
    assert!(!agent_worktree_target_matches(
        &expected,
        &current,
        AgentWorktreeAction::Apply
    ));
    current = expected.clone();
    current.journal_run_id = Some("run-followup".into());
    assert!(!agent_worktree_target_matches(
        &expected,
        &current,
        AgentWorktreeAction::Discard
    ));
    current = expected.clone();
    current.isolation.as_mut().unwrap().workspace = "/project/another-worktree".into();
    assert!(!agent_worktree_target_matches(
        &expected,
        &current,
        AgentWorktreeAction::Discard
    ));
    current = expected.clone();
    current.batch_run_id += 1;
    assert!(!agent_worktree_target_matches(
        &expected,
        &current,
        AgentWorktreeAction::Inspect
    ));
}
