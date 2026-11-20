//! The sanctioned forge tools and the proposals the gate reviews or shows the owner.
//!
//! The hub never performs them. The gate checks a call and opens a decision
//! on it; the runner performs the stored call once you approve (§9.4).

use serde_json::{Value as Json, json};
use threadlane_core::forge::{
    CLOSE_PULL_REQUEST, COMMENT, CREATE_ISSUE, EXPECTED_HEAD, FAST_FORWARD_PULL_REQUEST, MARK_READY,
    MERGE, ONTO_TASK, OPEN_PULL_REQUEST, PUBLISH_BRANCH,
};
use threadlane_core::link::{DIFF_PREVIEW, PUBLISH_REVISION};
use threadlane_core::model::{DecisionOption, Lane};
use threadlane_core::store::{OrgStore, TaskStore};
use threadlane_core::tools::{Tier, ToolDefinition, ToolError};
use threadlane_core::workspace::{task_branch, valid_commit_id, valid_repository_name};
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

/// Task-lane forge tools and their permission tiers.
pub(crate) fn definitions() -> Vec<ToolDefinition> {
    let repository = json!({
        "type": "string",
        "description": "A repository registered in the task's project."
    });
    let define = |name: &str, description: &str, schema: Json| ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: schema,
        tier: if matches!(name, MERGE | CLOSE_PULL_REQUEST) {
            Tier::SharedHardToUndo
        } else {
            Tier::SharedReversible
        },
        safe_to_repeat: false,
    };
    vec![
        define(
            PUBLISH_BRANCH,
            "Push your task's branch of one repository to its remote. To change a pull request \
             an earlier task opened, set onto_task to that task: your commit is pushed onto its \
             branch and the pull request updates in place. End your turn if the call waits for approval.",
            json!({
                "type": "object",
                "properties": {
                    "repository": repository.clone(),
                    "onto_task": {
                        "type": "string",
                        "description": "An ended task of this project, as T-0142. Your branch must contain its branch's commits."
                    }
                },
                "required": ["repository"],
                "additionalProperties": false
            }),
        ),
        define(
            OPEN_PULL_REQUEST,
            "Open a draft pull request from your task's branch of one repository into its \
             default branch, pushing the branch first. End your turn if the call waits for approval.",
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
        define(
            FAST_FORWARD_PULL_REQUEST,
            "Fast-forward an existing open pull request in a task repository to your task's commit. \
             Give the PR number and its current full head SHA. Your commit must contain that head.",
            json!({
                "type":"object",
                "properties":{
                    "repository": repository,
                    "number":{"type":"integer","minimum":1},
                    "expected_head":{"type":"string","description":"The current full commit SHA of the PR head."}
                },
                "required":["repository","number","expected_head"],
                "additionalProperties":false
            }),
        ),
        define(
            COMMENT,
            "Comment on an issue or pull request in a registered project repository.",
            json!({
                "type":"object","properties":{"repository":repository,"target":{"enum":["issue","pull_request"]},"number":{"type":"integer","minimum":1},"body":{"type":"string"}},
                "required":["repository","target","number","body"],"additionalProperties":false
            }),
        ),
        define(
            CREATE_ISSUE,
            "Create an issue in a registered project repository.",
            json!({
                "type":"object","properties":{"repository":repository,"title":{"type":"string"},"body":{"type":"string"}},
                "required":["repository","title"],"additionalProperties":false
            }),
        ),
        define(
            MERGE,
            "Merge a pull request in a registered project repository.",
            json!({
                "type":"object","properties":{"repository":repository,"number":{"type":"integer","minimum":1}},
                "required":["repository","number"],"additionalProperties":false
            }),
        ),
        define(
            MARK_READY,
            "Mark a draft pull request in a registered project repository ready for review.",
            json!({
                "type":"object","properties":{"repository":repository,"number":{"type":"integer","minimum":1}},
                "required":["repository","number"],"additionalProperties":false
            }),
        ),
        define(
            CLOSE_PULL_REQUEST,
            "Close an open pull request in a registered project repository, such as a superseded \
             duplicate, and optionally delete its head branch. Give the PR number and its current \
             full head SHA: the close is refused if the head has moved.",
            json!({
                "type":"object",
                "properties":{
                    "repository":repository,
                    "number":{"type":"integer","minimum":1},
                    "expected_head":{"type":"string","description":"The current full commit SHA of the PR head."},
                    "delete_branch":{"type":"boolean","description":"Also delete the PR's head branch. Default false."}
                },
                "required":["repository","number","expected_head"],"additionalProperties":false
            }),
        ),
    ]
}

/// Parses the arguments again before handing the stored call to the runner.
pub(crate) fn repository_action(
    tool: &str,
    a: &Json,
) -> Result<threadlane_core::forge::RepositoryAction, ToolError> {
    use threadlane_core::forge::RepositoryAction;
    let number = || {
        a["number"]
            .as_u64()
            .and_then(std::num::NonZeroU64::new)
            .ok_or_else(|| invalid("number must be a positive integer"))
    };
    let body = || match a.get("body") {
        Some(Json::String(s)) if plain_text(s) => Ok(s.clone()),
        None if tool == CREATE_ISSUE => Ok(String::new()),
        _ => Err(invalid(
            "body must be plain text of at most 20000 characters",
        )),
    };
    match tool {
        COMMENT => {
            fields(a, &["repository", "target", "number", "body"])?;
            let target = serde_json::from_value(a["target"].clone())
                .map_err(|_| invalid("target must be issue or pull_request"))?;
            let body = body()?;
            if body.trim().is_empty() {
                return Err(invalid("a comment needs a body"));
            }
            Ok(RepositoryAction::Comment {
                target,
                number: number()?,
                body,
            })
        }
        CREATE_ISSUE => {
            fields(a, &["repository", "title", "body"])?;
            let title = issue_title(a)?;
            Ok(RepositoryAction::CreateIssue {
                title,
                body: body()?,
            })
        }
        MERGE => {
            fields(a, &["repository", "number"])?;
            Ok(RepositoryAction::Merge { number: number()? })
        }
        MARK_READY => {
            fields(a, &["repository", "number"])?;
            Ok(RepositoryAction::MarkReady { number: number()? })
        }
        CLOSE_PULL_REQUEST => {
            fields(a, &["repository", "number", EXPECTED_HEAD, "delete_branch"])?;
            let expected_head = a[EXPECTED_HEAD]
                .as_str()
                .filter(|head| valid_commit_id(head))
                .ok_or_else(|| invalid("expected_head must be a full commit SHA"))?
                .to_owned();
            let delete_branch = match a.get("delete_branch") {
                None | Some(Json::Null) => false,
                Some(Json::Bool(delete)) => *delete,
                Some(_) => return Err(invalid("delete_branch must be true or false")),
            };
            Ok(RepositoryAction::ClosePullRequest {
                number: number()?,
                expected_head,
                delete_branch,
            })
        }
        _ => Err(invalid("unknown repository action")),
    }
}

/// Validates the arguments and repository before any human or backend is asked.
/// Publishing also needs a worktree belonging to this task.
pub(crate) async fn proposal(
    db: &HubDb,
    lane: &Lane,
    tool: &str,
    arguments: &Json,
) -> Result<Proposal, ToolError> {
    if crate::memory::is_change(tool) {
        return crate::memory::proposal(lane.project_id, lane.task_id, tool, arguments);
    }
    if tool == crate::timers::CREATE_SCHEDULE {
        return crate::timers::proposal(db, lane, arguments).await;
    }
    if tool == crate::tools::CREATE_TASK {
        return crate::proposal::task(db, lane, arguments).await;
    }
    if tool == crate::tools::ASK_USER {
        let (question, _) = crate::tools::question(arguments)?;
        return Ok(Proposal {
            question,
            context: String::new(),
        });
    }
    if !matches!(
        tool,
        PUBLISH_BRANCH
            | OPEN_PULL_REQUEST
            | FAST_FORWARD_PULL_REQUEST
            | COMMENT
            | CREATE_ISSUE
            | MERGE
            | MARK_READY
            | CLOSE_PULL_REQUEST
    ) {
        return Ok(Proposal {
            question: format!("Allow {tool}?"),
            context: format!("Arguments: {arguments}"),
        });
    }
    let task_id = lane
        .task_id
        .ok_or_else(|| ToolError::Blocked(format!("{tool} is available only in a task lane")))?;
    let task = db
        .task(task_id)
        .await
        .map_err(tool_error)?
        .ok_or_else(|| ToolError::Failed(format!("task {task_id} not found")))?;
    let registered = db.repositories(lane.project_id).await.map_err(tool_error)?;
    // Says which names would have worked: agents often give the forge's
    // owner/name, or another repository of the project.
    let names = |of_task: bool| {
        registered
            .iter()
            .filter(|repository| !of_task || task.repositories.contains(&repository.id))
            .map(|repository| repository.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let name = match arguments.get("repository") {
        Some(Json::String(name)) if valid_repository_name(name) => name.as_str(),
        _ => {
            return Err(ToolError::InvalidArguments(format!(
                "repository must be the name of a repository registered in this project: {}",
                names(false)
            )));
        }
    };
    let repository = registered
        .iter()
        .find(|repository| repository.name == name)
        .ok_or_else(|| {
            ToolError::InvalidArguments(format!(
                "no repository {name:?} in this project; it has {}",
                names(false)
            ))
        })?;
    if matches!(
        tool,
        COMMENT | CREATE_ISSUE | MERGE | MARK_READY | CLOSE_PULL_REQUEST
    ) {
        let action = repository_action(tool, arguments)?;
        let question = match &action {
            threadlane_core::forge::RepositoryAction::ClosePullRequest {
                number,
                expected_head,
                delete_branch,
            } => format!(
                "Close pull request #{number} in {name}{} if its head is still {expected_head}?",
                if *delete_branch {
                    " and delete its head branch"
                } else {
                    ""
                }
            ),
            _ => match tool {
            COMMENT => format!(
                "Comment on {} #{} in {name}?",
                arguments["target"].as_str().unwrap_or_default(),
                arguments["number"]
            ),
            MERGE => format!("Merge pull request #{} in {name}?", arguments["number"]),
            MARK_READY => format!(
                "Mark draft pull request #{} in {name} ready for review?",
                arguments["number"]
            ),
            _ => format!("Create issue {} in {name}?", arguments["title"]),
            },
        };
        return Ok(Proposal {
            question,
            context: format!(
                "Task {}: {}.\n{}",
                task.id,
                task.title,
                arguments["body"].as_str().unwrap_or_default()
            ),
        });
    }
    let allowed: &[&str] = match tool {
        PUBLISH_BRANCH => &["repository", ONTO_TASK, DIFF_PREVIEW, PUBLISH_REVISION],
        FAST_FORWARD_PULL_REQUEST => &[
            "repository",
            "number",
            EXPECTED_HEAD,
            DIFF_PREVIEW,
            PUBLISH_REVISION,
        ],
        _ => &[
            "repository",
            "title",
            "body",
            DIFF_PREVIEW,
            PUBLISH_REVISION,
        ],
    };
    let object = fields(arguments, allowed)?;
    if !task.repositories.contains(&repository.id) {
        return Err(ToolError::InvalidArguments(format!(
            "repository {name:?} is not one this task works in; it works in {}",
            names(true)
        )));
    }
    let revision = object
        .get(PUBLISH_REVISION)
        .and_then(Json::as_str)
        .filter(|revision| valid_commit_id(revision))
        .ok_or_else(|| invalid("publish requires a full commit SHA from the runner"))?;
    let branch = task_branch(task.id);
    let about = format!("Task {}: {}. Commit: {revision}.", task.id, task.title);
    if tool == FAST_FORWARD_PULL_REQUEST {
        let number = object
            .get("number")
            .and_then(Json::as_u64)
            .and_then(std::num::NonZeroU64::new)
            .ok_or_else(|| invalid("number must be a positive integer"))?;
        let head = object
            .get(EXPECTED_HEAD)
            .and_then(Json::as_str)
            .filter(|head| valid_commit_id(head))
            .ok_or_else(|| invalid("expected_head must be a full commit SHA"))?;
        if threadlane_core::github::normalize_remote(&repository.remote_url).is_none() {
            return Err(invalid("the registered remote is not a GitHub repository"));
        }
        return Ok(Proposal {
            question: format!("Fast-forward pull request #{number} in {name}?"),
            context: format!(
                "{about} Expected PR head: {head}. Remote: {}.",
                repository.remote_url
            ),
        });
    }
    if tool == PUBLISH_BRANCH {
        let Some(onto) = object.get(ONTO_TASK).filter(|onto| !onto.is_null()) else {
            return Ok(Proposal {
                question: format!("Push branch {branch} of {name}?"),
                context: format!("{about} Remote: {}.", repository.remote_url),
            });
        };
        // Only a branch this system made, whose own task can no longer move it.
        let onto = crate::tools::task_ref(onto)?;
        let target = db
            .task(onto)
            .await
            .map_err(tool_error)?
            .filter(|target| target.project_id == lane.project_id && target.id != task.id)
            .ok_or_else(|| invalid("onto_task must be another task of this project"))?;
        if !target.status.is_terminal() || !target.repositories.contains(&repository.id) {
            return Err(invalid(
                "onto_task must be an ended task that worked in this repository",
            ));
        }
        let onto = task_branch(onto);
        return Ok(Proposal {
            question: format!("Push branch {branch} of {name} onto {onto}?"),
            context: format!(
                "{about} Updates {onto} and any pull request open from it. Remote: {}.",
                repository.remote_url
            ),
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

pub(crate) fn issue_title(args: &Json) -> Result<String, ToolError> {
    args["title"]
        .as_str()
        .filter(|s| plain_line(s))
        .map(|s| s.trim().to_owned())
        .ok_or_else(|| invalid("title must be one plain line of at most 200 characters"))
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
pub(crate) fn plain_text(text: &str) -> bool {
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
