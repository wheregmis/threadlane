//! The shared-effect tools of milestone 3, `publish_branch` and
//! `open_pull_request` (Plan.md §9.2 layer 1, docs/milestone-3.md §4), and
//! what a decision on a gated call asks you.
//!
//! The hub never performs them. The gate checks a call and opens a decision
//! on it; the runner performs the stored call once you approve (§9.4).

use serde_json::{Value as Json, json};
use threadlane_core::forge::{OPEN_PULL_REQUEST, PUBLISH_BRANCH};
use threadlane_core::model::{DecisionOption, Lane};
use threadlane_core::store::{OrgStore, TaskStore};
use threadlane_core::tools::{Tier, ToolDefinition, ToolError};
use threadlane_core::workspace::{task_branch, valid_repository_name};
use threadlane_store::HubDb;

use crate::error::tool_error;
use crate::tools::{fields, invalid};

/// The longest pull request title accepted.
const MAX_TITLE: usize = 200;
/// The longest pull request body accepted.
const MAX_BODY: usize = 20_000;

/// What a decision on a gated call shows you.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Proposal {
    pub question: String,
    pub context: String,
}

/// The options of every approval: approve performs the stored call,
/// decline does not. The first is the one the agent would pick.
pub(crate) fn approval_options() -> Vec<DecisionOption> {
    vec![
        DecisionOption {
            label: "approve".to_owned(),
            effect: json!({ "approve": true }),
        },
        DecisionOption {
            label: "decline".to_owned(),
            effect: json!({ "approve": false }),
        },
    ]
}

/// The definitions of the two tools.
pub(crate) fn definitions() -> Vec<ToolDefinition> {
    let repository = json!({
        "type": "string",
        "description": "The registered name of one of the task's repositories."
    });
    let define = |name: &str, description: &str, schema: Json| ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: schema,
        tier: Tier::SharedHardToUndo,
        safe_to_repeat: false,
    };
    vec![
        define(
            PUBLISH_BRANCH,
            "Push your task's branch of one repository to its remote. The user is asked \
             first: the call returns at once with \"waiting on you\", and you must then end \
             your turn. Once they answer, you are resumed with the outcome.",
            json!({
                "type": "object",
                "properties": { "repository": repository.clone() },
                "required": ["repository"],
                "additionalProperties": false
            }),
        ),
        define(
            OPEN_PULL_REQUEST,
            "Open a draft pull request from your task's branch of one repository into its \
             default branch, pushing the branch first. The user is asked first: the call \
             returns at once with \"waiting on you\", and you must then end your turn. Once \
             they answer, you are resumed with the outcome.",
            json!({
                "type": "object",
                "properties": {
                    "repository": repository,
                    "title": { "type": "string", "description": "One line." },
                    "body": { "type": "string", "description": "What changed and why." }
                },
                "required": ["repository", "title"],
                "additionalProperties": false
            }),
        ),
    ]
}

/// Checks a gated call and says what to ask you about it. A call to one of
/// the two tools must come from a task lane and name one of the task's
/// repositories, registered in its project (invariant 19), with plain text
/// arguments; any other tool is shown as it was called.
pub(crate) async fn proposal(
    db: &HubDb,
    lane: &Lane,
    tool: &str,
    arguments: &Json,
) -> Result<Proposal, ToolError> {
    if crate::memory::is_change(tool) {
        return crate::memory::proposal(lane.project_id, lane.task_id, tool, arguments);
    }
    if crate::timers::asks(tool) {
        return crate::timers::proposal(db, lane, arguments).await;
    }
    if tool != PUBLISH_BRANCH && tool != OPEN_PULL_REQUEST {
        return Ok(Proposal {
            question: format!("Allow {tool}?"),
            context: format!("Arguments: {arguments}"),
        });
    }
    let allowed: &[&str] = if tool == PUBLISH_BRANCH {
        &["repository"]
    } else {
        &["repository", "title", "body"]
    };
    let object = fields(arguments, allowed)?;
    let task_id = lane
        .task_id
        .ok_or_else(|| ToolError::Blocked(format!("{tool} is available only in a task lane")))?;
    let task = db
        .task(task_id)
        .await
        .map_err(tool_error)?
        .ok_or_else(|| ToolError::Failed(format!("task {task_id} not found")))?;
    let name = match object.get("repository") {
        Some(Json::String(name)) if valid_repository_name(name) => name.as_str(),
        _ => return Err(invalid("repository must be a registered repository name")),
    };
    let registered = db.repositories(lane.project_id).await.map_err(tool_error)?;
    let repository = registered
        .iter()
        .find(|repository| repository.name == name)
        .ok_or_else(|| {
            ToolError::InvalidArguments(format!("no repository {name:?} in this project"))
        })?;
    if !task.repositories.contains(&repository.id) {
        return Err(ToolError::InvalidArguments(format!(
            "repository {name:?} is not one this task works in"
        )));
    }
    let branch = task_branch(task.id);
    let about = format!("Task {}: {}.", task.id, task.title);
    if tool == PUBLISH_BRANCH {
        return Ok(Proposal {
            question: format!("Push branch {branch} of {name}?"),
            context: format!("{about} Remote: {}.", repository.remote_url),
        });
    }
    let title = match object.get("title") {
        Some(Json::String(title)) if plain_line(title) => title.trim(),
        _ => {
            return Err(invalid(
                "title must be one line of text, not starting with '-', at most 200 characters",
            ));
        }
    };
    let body = match object.get("body") {
        None | Some(Json::Null) => "",
        Some(Json::String(body)) if plain_text(body) => body.as_str(),
        Some(_) => {
            return Err(invalid(
                "body must be text without control characters, at most 20000 characters",
            ));
        }
    };
    Ok(Proposal {
        question: format!(
            "Open a draft pull request from {branch} into {} of {name}, titled {title:?}?",
            repository.default_branch
        ),
        context: format!(
            "{about} The branch is pushed to {} first.\n\n{body}",
            repository.remote_url
        )
        .trim_end()
        .to_owned(),
    })
}

/// One line of text that a command line cannot read as an option.
fn plain_line(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text.chars().count() <= MAX_TITLE
        && !text.starts_with('-')
        && !text.chars().any(char::is_control)
}

/// Text with no control characters but line breaks and tabs.
fn plain_text(text: &str) -> bool {
    text.chars().count() <= MAX_BODY
        && !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_bodies_are_plain_text() {
        assert!(plain_line("Add the page"));
        for bad in [
            "",
            "  ",
            "-f",
            "--force",
            "two\nlines",
            "nul\0",
            &"x".repeat(201),
        ] {
            assert!(!plain_line(bad), "{bad:?}");
        }
        assert!(plain_text("Line one\nline two\tok\r\n"));
        assert!(plain_text(""));
        for bad in ["bell\x07", "escape\x1b[0m", &"x".repeat(20_001)] {
            assert!(!plain_text(bad), "{bad:?}");
        }
    }
}
