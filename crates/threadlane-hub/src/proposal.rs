//! Proposals (Plan.md §9.4 kind `proposal`, docs/milestone-9.md decision
//! 7): a paused `create_task` from work you did not request. Starting it
//! creates the task in the hub ([`crate::gateway`]); declining resumes the
//! lead with your note; "do not propose this kind again" also writes a line
//! to a project note for the lead, not a rule in the gate.

use serde_json::{Value as Json, json};
use threadlane_core::ids::DecisionId;
use threadlane_core::model::{Decision, DecisionKind, DecisionOption, Lane, Task, TaskOrigin};
use threadlane_core::store::{
    DecisionStore, MemoryStore, OrgStore, SessionStore, StoreError, TaskStore,
};
use threadlane_core::time::Timestamp;
use threadlane_core::tools::ToolError;
use threadlane_store::HubDb;

use crate::publish::Proposal;
use crate::tools::{TaskCall, bound_agent, named_repositories};

/// The option effect that also records the kind as not to propose again.
const NEVER_AGAIN: &str = "never_again";

/// The options of every proposal; the first, start, is recommended.
pub(crate) fn options() -> Vec<DecisionOption> {
    vec![
        DecisionOption {
            label: "start".to_owned(),
            effect: json!({ "approve": true }),
        },
        DecisionOption {
            label: "not now".to_owned(),
            effect: json!({ "approve": false }),
        },
        DecisionOption {
            label: "do not propose this kind again".to_owned(),
            effect: json!({ "approve": false, NEVER_AGAIN: true }),
        },
    ]
}

/// Checks a proposed `create_task` call as creating it would, so a call
/// that would be refused is refused, not asked (invariant 19), and says
/// what to ask you.
pub(crate) async fn task(db: &HubDb, lane: &Lane, arguments: &Json) -> Result<Proposal, ToolError> {
    let call = TaskCall::parse(arguments)?;
    let agent = match call.agent {
        Some(name) => {
            bound_agent(db, lane.project_id, name).await?;
            name.to_owned()
        }
        None => match lane.task_id {
            Some(task) => format!("task {task}'s agent"),
            None => "the lead".to_owned(),
        },
    };
    let repositories = named_repositories(db, lane.project_id, call.object).await?;
    let mut context = format!("{}\n\nFrom work you did not ask for.", call.goal);
    if let Some(Json::Array(names)) = call.object.get("repositories")
        && repositories.is_some_and(|ids| !ids.is_empty())
    {
        let names: Vec<&str> = names.iter().filter_map(Json::as_str).collect();
        context.push_str(&format!(" Repositories: {}.", names.join(", ")));
    }
    if let Some(args) = call.object.get("pr_review") {
        let target = crate::tools::review_head(db, lane.project_id, args).await?;
        if let Some(found) = db
            .pr_review_status(lane.project_id, &target)
            .await
            .map_err(crate::error::tool_error)?
        {
            return Err(ToolError::Blocked(format!(
                "Review of {} at {} is {} on {}.",
                target.url, target.head, found.state, found.task
            )));
        }
        context.push_str(&format!(" Review {} at {}.", target.url, target.head));
    }
    Ok(Proposal {
        question: format!("Start a task for {agent}: {}?", call.title),
        context,
    })
}

/// The project note the lead keeps the kinds of work you declined for good
/// in (stored path).
pub(crate) fn declined_note(decision: &Decision) -> String {
    format!(
        "projects/{}/notes/declined-proposals.md",
        decision.project_id.get()
    )
}

/// After you answered `decision` with "do not propose this kind again",
/// adds its line to [`declined_note`], written by you as the owner. Any
/// other answer writes nothing. Called once per answer: a second answer to
/// a decision is not recorded.
// shortcut: written after the answer's transaction, so a crash between the
// two loses the line; move it into that transaction if that matters.
pub(crate) async fn record_answer(
    db: &HubDb,
    decision: &Decision,
    at: Timestamp,
) -> Result<(), StoreError> {
    let never = decision
        .chosen()
        .is_some_and(|option| option.effect.get(NEVER_AGAIN) == Some(&Json::Bool(true)));
    if !never {
        return Ok(());
    }
    let path = declined_note(decision);
    let mut content = match db.read(&path).await? {
        Some(file) => file.content,
        None => "# Declined proposals\nKinds of work the user asked you not to propose again.\n\n"
            .to_owned(),
    };
    let title = decision.arguments["title"].as_str().unwrap_or("this kind");
    content.push_str(&format!(
        "- Do not propose work like \"{title}\" again ({}).\n",
        decision.id
    ));
    db.write_by_owner(&path, &content, at).await?;
    Ok(())
}

/// The agent that made this proposal, from its original run.
pub(crate) async fn author(db: &HubDb, decision: &Decision) -> Result<Option<String>, StoreError> {
    let Some(run_id) = decision.run_id else {
        return Ok(None);
    };
    let Some(agent_id) = db.run(run_id).await?.and_then(|run| run.agent_id) else {
        return Ok(None);
    };
    Ok(db.agent(agent_id).await?.map(|agent| agent.name))
}

/// Follows accepted work back to the proposal whose call created it.
pub(crate) async fn accepted_author(db: &HubDb, task: &Task) -> Result<Option<String>, StoreError> {
    let mut parent = None;
    loop {
        let task = parent.as_ref().unwrap_or(task);
        if task.origin != TaskOrigin::Accepted {
            break;
        }
        let decision_id = task
            .created_by_call
            .as_deref()
            .and_then(|key| key.split_once(":perform:"))
            .and_then(|(_, id)| DecisionId::parse(id));
        if let Some(id) = decision_id
            && let Some(decision) = db.decision(id).await?
            && decision.project_id == task.project_id
            && decision.kind == DecisionKind::Proposal
            && decision.approved()
        {
            return author(db, &decision).await;
        }
        let Some(parent_id) = task.parent_task_id else {
            break;
        };
        parent = db.task(parent_id).await?;
        if parent.is_none() {
            break;
        }
    }
    Ok(None)
}
