//! The sanctioned forge tools and the proposals the gate reviews or shows the owner.
//!
//! The hub never performs them. The gate checks a call and opens a decision
//! on it; the runner performs the stored call once you approve (§9.4).

use serde_json::{Value as Json, json};
use threadlane_core::forge::{
    ASSIGN_ISSUE, CLOSE_PULL_REQUEST, COMMENT, CREATE_ISSUE, EDIT_PULL_REQUEST, EXPECTED_HEAD,
    FAST_FORWARD_PULL_REQUEST, GITHUB_CLI, LABEL_ISSUE, MARK_READY, MAX_LABELS, MERGE, ONTO_TASK,
    OPEN_PULL_REQUEST, PUBLISH_BRANCH, valid_label_name,
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
        tier: if matches!(name, MERGE | CLOSE_PULL_REQUEST | GITHUB_CLI) {
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
            LABEL_ISSUE,
            "Add or remove labels on an issue (not a pull request) in a registered project \
             repository. Labels the repository does not have are reported back, not created.",
            json!({
                "type":"object","properties":{
                    "repository":repository,
                    "number":{"type":"integer","minimum":1},
                    "add":{"type":"array","items":{"type":"string"},"description":"Existing label names to add."},
                    "remove":{"type":"array","items":{"type":"string"},"description":"Label names to remove."}
                },
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
        define(
            GITHUB_CLI,
            "Run one gh command against a registered project repository, for a GitHub change no \
             other tool makes, such as closing an issue. The user always decides it and sees the \
             exact command. Give the arguments after gh, without --repo, which is set from \
             repository. End your turn when it waits on the user.",
            json!({
                "type":"object","properties":{
                    "repository":repository,
                    "args":{"type":"array","items":{"type":"string"},"minItems":2,
                        "description":"The gh arguments, as [\"issue\",\"close\",\"629\"]. The first is one of issue, pr, label, release, run, workflow."},
                    "missing_tool":{"type":"string","description":"One line: the dedicated tool Threadlane lacks, as \"Close or reopen an issue\"."}
                },
                "required":["repository","args","missing_tool"],"additionalProperties":false
            }),
        ),
        define(
            ASSIGN_ISSUE,
            "Assign an issue in a registered project repository to you, so others can see it is taken.",
            json!({
                "type":"object","properties":{"repository":repository,"number":{"type":"integer","minimum":1}},
                "required":["repository","number"],"additionalProperties":false
            }),
        ),
        define(
            EDIT_PULL_REQUEST,
            "Replace the title and/or body of an open pull request in a registered project \
             repository, such as a description that names an old head commit. Give at least one \
             of title and body; the one you leave out stays as it is. End your turn if the call \
             waits for approval.",
            json!({
                "type":"object","properties":{
                    "repository":repository,
                    "number":{"type":"integer","minimum":1},
                    "title":{"type":"string","description":"The new title, one line."},
                    "body":{"type":"string","description":"The new body, replacing the whole description."}
                },
                "required":["repository","number"],"additionalProperties":false
            }),
        ),
    ]
}

/// The `gh` commands `github_cli` may run: each acts on one repository and
/// takes `--repo`.
const GH_COMMANDS: &[&str] = &["issue", "pr", "label", "release", "run", "workflow"];

/// The `gh` arguments of a `github_cli` call, checked: a command that takes
/// `--repo`, no repository of the agent's choosing, nothing interactive, and
/// not an action a dedicated tool already makes.
fn gh_args(a: &Json) -> Result<Vec<String>, ToolError> {
    let args = a["args"]
        .as_array()
        .filter(|args| (2..=40).contains(&args.len()))
        .and_then(|args| {
            args.iter()
                .map(|arg| {
                    arg.as_str()
                        .filter(|arg| plain_text(arg))
                        .map(str::to_owned)
                })
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| invalid("args must be 2 to 40 strings of plain text"))?;
    if !GH_COMMANDS.contains(&args[0].as_str()) {
        return Err(invalid(&format!(
            "args must start with one of {}",
            GH_COMMANDS.join(", ")
        )));
    }
    let dedicated = match (args[0].as_str(), args[1].as_str()) {
        ("pr", "create") => Some(OPEN_PULL_REQUEST),
        ("pr", "merge") => Some(MERGE),
        ("pr", "ready") => Some(MARK_READY),
        ("pr", "edit")
            if args.iter().any(|arg| {
                ["--title", "--body", "-t", "-b"]
                    .iter()
                    .any(|flag| arg.starts_with(flag))
            }) =>
        {
            Some(EDIT_PULL_REQUEST)
        }
        ("pr", "close") => Some(CLOSE_PULL_REQUEST),
        ("pr" | "issue", "comment") => Some(COMMENT),
        ("issue", "create" | "new") => Some(CREATE_ISSUE),
        ("issue", "edit")
            if args
                .iter()
                .any(|arg| arg.starts_with("--add-label") || arg.starts_with("--remove-label")) =>
        {
            Some(LABEL_ISSUE)
        }
        // assign_issue checks who holds the issue first.
        ("issue", "edit") if args.iter().any(|arg| arg.starts_with("--add-assignee")) => {
            Some(ASSIGN_ISSUE)
        }
        _ => None,
    };
    if let Some(tool) = dedicated {
        return Err(invalid(&format!("use the {tool} tool for this")));
    }
    if let Some(flag) = args.iter().find(|arg| {
        let flag = arg.split('=').next().unwrap_or_default();
        matches!(flag, "--repo" | "--web" | "--editor" | "--body-file" | "-F")
            || (arg.starts_with("-R") && !arg.starts_with("--"))
            || (flag == "-w")
    }) {
        return Err(invalid(&format!(
            "{flag} is not allowed: the repository comes from repository, and nothing may open \
             a browser, an editor or a file"
        )));
    }
    Ok(args)
}

/// `owner/name` of a remote on github.com, the host the GitHub source reads.
fn github_repository(remote: &str) -> Option<String> {
    threadlane_core::github::normalize_remote(remote)?
        .strip_prefix("github.com/")
        .map(str::to_owned)
}

/// Who holds the issue an `assign_issue` call names.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Claim {
    /// Nobody else: the gate decides as for any call.
    Free,
    /// Already assigned to you alone; nothing to do.
    Yours(String),
    /// Someone else holds it, or GitHub could not say: you decide, always,
    /// on this question.
    Held(String),
}

/// Reads who holds the issue of a validated `assign_issue` call. A pull
/// request or a closed issue is refused; a failed read is [`Claim::Held`],
/// never free.
pub(crate) async fn issue_claim(
    db: &HubDb,
    github: Option<&dyn threadlane_core::github::GithubSource>,
    lane: &Lane,
    arguments: &Json,
) -> Result<Claim, ToolError> {
    let name = arguments["repository"].as_str().unwrap_or_default();
    let number = arguments["number"].as_u64().unwrap_or_default();
    let full_name = db
        .repositories(lane.project_id)
        .await
        .map_err(tool_error)?
        .into_iter()
        .find(|repository| repository.name == name)
        .and_then(|repository| github_repository(&repository.remote_url))
        .ok_or_else(|| invalid("the registered remote is not a GitHub repository"))?;
    let read = match github {
        Some(github) => github.issue_claim(&full_name, number).await,
        None => Err(threadlane_core::github::GithubError(
            "this hub does not read GitHub".to_owned(),
        )),
    };
    let claim = match read {
        Ok(claim) => claim,
        Err(error) => {
            return Ok(Claim::Held(format!(
                "Could not check who holds issue #{number} in {name} ({error}). \
                 Assign it to you anyway?"
            )));
        }
    };
    if claim.is_pull_request {
        return Err(invalid(&format!(
            "#{number} is a pull request; assign_issue assigns issues only"
        )));
    }
    if !claim.open {
        return Err(invalid(&format!("issue #{number} in {name} is closed")));
    }
    let other = |login: &String| !login.eq_ignore_ascii_case(&claim.viewer);
    let assigned: Vec<&str> = claim
        .assignees
        .iter()
        .filter(|login| other(login))
        .map(String::as_str)
        .collect();
    let pulls: Vec<String> = claim
        .open_pull_requests
        .iter()
        .filter(|(_, author)| other(author))
        .map(|(pull, author)| format!("#{pull} by {author}"))
        .collect();
    let mut held = Vec::new();
    if !assigned.is_empty() {
        held.push(format!("is assigned to {}", assigned.join(", ")));
    }
    if !pulls.is_empty() {
        let noun = if pulls.len() == 1 {
            "pull request"
        } else {
            "pull requests"
        };
        held.push(format!("has open {noun} {}", pulls.join(", ")));
    }
    Ok(if !held.is_empty() {
        Claim::Held(format!(
            "Issue #{number} in {name} {}. Take it anyway?",
            held.join(" and ")
        ))
    } else if claim.assignees.is_empty() {
        Claim::Free
    } else {
        Claim::Yours(format!("issue #{number} is already assigned to you"))
    })
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
        LABEL_ISSUE => {
            fields(a, &["repository", "number", "add", "remove"])?;
            let (add, remove) = (labels(a, "add")?, labels(a, "remove")?);
            if add.is_empty() && remove.is_empty() {
                return Err(invalid("name at least one label to add or remove"));
            }
            if add.len() + remove.len() > MAX_LABELS {
                return Err(invalid("at most 20 labels in one call"));
            }
            if add
                .iter()
                .any(|label| remove.iter().any(|r| r.eq_ignore_ascii_case(label)))
            {
                return Err(invalid("a label cannot be both added and removed"));
            }
            Ok(RepositoryAction::LabelIssue {
                number: number()?,
                add,
                remove,
            })
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
        GITHUB_CLI => {
            fields(a, &["repository", "args", "missing_tool"])?;
            missing_tool(a)?;
            Ok(RepositoryAction::GhCli { args: gh_args(a)? })
        }
        ASSIGN_ISSUE => {
            fields(a, &["repository", "number"])?;
            Ok(RepositoryAction::AssignIssue { number: number()? })
        }
        EDIT_PULL_REQUEST => {
            fields(a, &["repository", "number", "title", "body"])?;
            let title = match a.get("title") {
                None | Some(Json::Null) => None,
                Some(_) => Some(issue_title(a)?),
            };
            let body = match a.get("body") {
                None | Some(Json::Null) => None,
                Some(_) => Some(body()?),
            };
            if title.is_none() && body.is_none() {
                return Err(invalid("give a new title, a new body, or both"));
            }
            Ok(RepositoryAction::EditPullRequest {
                number: number()?,
                title,
                body,
            })
        }
        _ => Err(invalid("unknown repository action")),
    }
}

/// The label names under `key`, which may be absent.
fn labels(a: &Json, key: &str) -> Result<Vec<String>, ToolError> {
    let bad = || {
        invalid(&format!(
            "{key} must be a list of label names, each at most 50 characters without control \
             characters or surrounding spaces"
        ))
    };
    match a.get(key) {
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .filter(|name| valid_label_name(name))
                    .map(str::to_owned)
                    .ok_or_else(bad)
            })
            .collect(),
        Some(_) => Err(bad()),
    }
}

/// The blocker of a task that tried to publish with no repository attached,
/// with how to clear it.
fn missing_repository(registered: &[threadlane_core::model::Repository]) -> String {
    if registered.is_empty() {
        return "Cannot publish: no repository is attached, and the project has none \
                registered. Register one, then attach it to the task."
            .to_owned();
    }
    let names: Vec<_> = registered.iter().map(|r| r.name.as_str()).collect();
    format!(
        "Cannot publish: no repository is attached. Attach {} from the task screen, \
         or the lead can attach it with update_task.",
        names.join(" or ")
    )
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
            | LABEL_ISSUE
            | CLOSE_PULL_REQUEST
            | GITHUB_CLI
            | ASSIGN_ISSUE
            | EDIT_PULL_REQUEST
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
    if task.repositories.is_empty()
        && matches!(
            tool,
            PUBLISH_BRANCH | OPEN_PULL_REQUEST | FAST_FORWARD_PULL_REQUEST
        )
    {
        let blocker = missing_repository(&registered);
        db.block_publication(task.id, &blocker)
            .await
            .map_err(tool_error)?;
        return Err(ToolError::Blocked(format!(
            "{blocker} Do not retry; the blocker stays on the task until a repository is attached."
        )));
    }
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
    if tool == GITHUB_CLI {
        repository_action(tool, arguments)?;
        let args = gh_args(arguments)?;
        return Ok(Proposal {
            question: format!("Run gh {} in {name}?", shell_words(&args)),
            context: format!(
                "Task {}: {}.\nThreadlane has no tool for this yet: {}. The agent asks to use the \
                 GitHub CLI instead, with --repo set to {}.",
                task.id,
                task.title,
                missing_tool(arguments)?,
                repository.remote_url
            ),
        });
    }
    if matches!(
        tool,
        COMMENT
            | CREATE_ISSUE
            | MERGE
            | MARK_READY
            | LABEL_ISSUE
            | CLOSE_PULL_REQUEST
            | ASSIGN_ISSUE
            | EDIT_PULL_REQUEST
    ) {
        let action = repository_action(tool, arguments)?;
        if tool == ASSIGN_ISSUE && github_repository(&repository.remote_url).is_none() {
            return Err(invalid("the registered remote is not a GitHub repository"));
        }
        // `owner/repo#N` links to the right repository in a multi-repository project.
        let slug = threadlane_core::github::normalize_remote(&repository.remote_url)
            .and_then(|remote| remote.strip_prefix("github.com/").map(str::to_owned))
            .unwrap_or_default();
        let question = match &action {
            threadlane_core::forge::RepositoryAction::ClosePullRequest {
                number,
                expected_head,
                delete_branch,
            } => format!(
                "Close pull request {slug}#{number} in {name}{} if its head is still {expected_head}?",
                if *delete_branch {
                    " and delete its head branch"
                } else {
                    ""
                }
            ),
            threadlane_core::forge::RepositoryAction::EditPullRequest {
                number,
                title,
                body,
                ..
            } => format!(
                "Edit pull request {slug}#{number} in {name}: {}?",
                match (title, body) {
                    (Some(title), Some(_)) => format!("new title {title:?} and new body"),
                    (Some(title), None) => format!("new title {title:?}"),
                    _ => "new body".to_owned(),
                }
            ),
            threadlane_core::forge::RepositoryAction::LabelIssue {
                number,
                add,
                remove,
            } => {
                let mut change = Vec::new();
                if !add.is_empty() {
                    change.push(format!("add {}", add.join(", ")));
                }
                if !remove.is_empty() {
                    change.push(format!("remove {}", remove.join(", ")));
                }
                format!(
                    "Label issue {slug}#{number} in {name}: {}?",
                    change.join("; ")
                )
            }
            _ => match tool {
                COMMENT => format!(
                    "Comment on {} {slug}#{} in {name}?",
                    arguments["target"].as_str().unwrap_or_default(),
                    arguments["number"]
                ),
                MERGE => format!(
                    "Merge pull request {slug}#{} in {name}?",
                    arguments["number"]
                ),
                MARK_READY => format!(
                    "Mark draft pull request {slug}#{} in {name} ready for review?",
                    arguments["number"]
                ),
                ASSIGN_ISSUE => {
                    format!("Assign issue #{} in {name} to you?", arguments["number"])
                }
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
        let Some(remote) = threadlane_core::github::normalize_remote(&repository.remote_url) else {
            return Err(invalid("the registered remote is not a GitHub repository"));
        };
        let slug = remote.strip_prefix("github.com/").unwrap_or_default();
        return Ok(Proposal {
            question: format!("Fast-forward pull request {slug}#{number} in {name}?"),
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

fn missing_tool(a: &Json) -> Result<&str, ToolError> {
    a["missing_tool"]
        .as_str()
        .map(str::trim)
        .filter(|s| plain_line(s))
        .ok_or_else(|| {
            invalid("missing_tool must be one plain line naming the tool Threadlane lacks")
        })
}

/// The arguments as you would type them, each quoted when it needs to be.
fn shell_words(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if !arg.is_empty()
                && arg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_=.,:/@#+".contains(c))
            {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
    fn edit_pull_request_replaces_a_title_or_body_and_needs_one() {
        use threadlane_core::forge::RepositoryAction;
        let edit = |extra: Json| {
            let mut a = json!({"repository":"app","number":12});
            a.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            repository_action(EDIT_PULL_REQUEST, &a)
        };
        let number = std::num::NonZeroU64::new(12).unwrap();
        assert_eq!(
            edit(json!({"body":"Head is now abc.\n\nCloses #1"})),
            Ok(RepositoryAction::EditPullRequest {
                number,
                title: None,
                body: Some("Head is now abc.\n\nCloses #1".into()),
            })
        );
        assert_eq!(
            edit(json!({"title":"  Better title "})),
            Ok(RepositoryAction::EditPullRequest {
                number,
                title: Some("Better title".into()),
                body: None,
            })
        );
        for bad in [
            json!({}),
            json!({"title":"--help"}),
            json!({"title":"two\nlines"}),
            json!({"body":"bell\u{7}"}),
            json!({"body":7}),
            json!({"head":"x","body":"ok"}),
        ] {
            assert!(edit(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn label_issue_needs_valid_distinct_labels() {
        let parse = |a: Json| repository_action(LABEL_ISSUE, &a);
        assert_eq!(
            parse(json!({"repository":"app","number":7,"add":["bug"],"remove":["triage"]})),
            Ok(threadlane_core::forge::RepositoryAction::LabelIssue {
                number: std::num::NonZeroU64::new(7).unwrap(),
                add: vec!["bug".into()],
                remove: vec!["triage".into()],
            })
        );
        for bad in [
            json!({"repository":"app","number":7}),
            json!({"repository":"app","number":7,"add":[]}),
            json!({"repository":"app","number":0,"add":["bug"]}),
            json!({"repository":"app","number":7,"add":"bug"}),
            json!({"repository":"app","number":7,"add":[" bug"]}),
            json!({"repository":"app","number":7,"add":["a\nb"]}),
            json!({"repository":"app","number":7,"add":["Bug"],"remove":["bug"]}),
            json!({"repository":"app","number":7,"add":["bug"],"labels":["x"]}),
            json!({"repository":"app","number":7,"add":vec!["x"; 21]}),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn github_cli_runs_only_repository_scoped_gh_commands() {
        let call = |args: Json| {
            repository_action(
                GITHUB_CLI,
                &json!({"repository":"app","args":args,"missing_tool":"Label issues"}),
            )
        };
        assert_eq!(
            call(json!(["issue", "close", "629", "--reason=not planned"])),
            Ok(threadlane_core::forge::RepositoryAction::GhCli {
                args: ["issue", "close", "629", "--reason=not planned"]
                    .map(str::to_owned)
                    .to_vec()
            })
        );
        for bad in [
            json!(["api", "repos/x/y", "-X", "DELETE"]),
            json!(["auth", "token"]),
            json!(["repo", "delete"]),
            json!(["issue", "edit", "1", "--repo=other/repo"]),
            json!(["issue", "edit", "1", "-Rother/repo"]),
            json!(["issue", "edit", "1", "--body-file", "/etc/passwd"]),
            json!(["pr", "view", "1", "--web"]),
            json!(["pr", "merge", "1"]),
            json!(["pr", "close", "1"]),
            json!(["pr", "edit", "1", "--body=stale"]),
            json!(["pr", "edit", "1", "-t", "New"]),
            json!(["issue", "comment", "1"]),
            json!(["issue", "edit", "1", "--add-label=bug"]),
            json!(["issue", "edit", "1", "--add-assignee=@me"]),
            json!(["issue"]),
            json!(["issue", "edit", 1]),
        ] {
            assert!(call(bad.clone()).is_err(), "{bad}");
        }
        assert!(
            repository_action(
                GITHUB_CLI,
                &json!({"repository":"app","args":["issue","edit","1"],"missing_tool":"--force"})
            )
            .is_err()
        );
        assert_eq!(
            shell_words(
                &["issue", "edit", "1", "--add-label=good first issue", "it's"].map(str::to_owned)
            ),
            "issue edit 1 '--add-label=good first issue' 'it'\\''s'"
        );
    }

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
