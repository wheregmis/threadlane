//! Filing an issue in a repository outside the project, such as a
//! third-party upstream whose regression a task found (#779).
//!
//! The repository is named by its GitHub `owner/name` and is never one the
//! project registered: those take `create_issue`. Each filing is shown to
//! the user, who approves it; no standing rule or default covers it. The
//! text goes to a public tracker, so by default a call whose title or body
//! names the project, its repositories, local paths or Threadlane ids is
//! refused and the agent rewrites it. Credentials are refused always.

use serde_json::{Value as Json, json};
use threadlane_core::forge::FILE_UPSTREAM_ISSUE;
use threadlane_core::model::{Lane, Repository};
use threadlane_core::store::{OrgStore, TaskStore};
use threadlane_core::tools::{Tier, ToolDefinition, ToolError};
use threadlane_store::HubDb;

use crate::error::tool_error;
use crate::publish::{Proposal, github_repository, issue_title, plain_text};
use crate::tools::{fields, invalid};

/// The argument that lets the text keep project details the check found.
pub(crate) const INCLUDE_PROJECT_DETAILS: &str = "include_project_details";

/// The tool as task agents are offered it.
pub(crate) fn definition() -> ToolDefinition {
    ToolDefinition {
        name: FILE_UPSTREAM_ISSUE.into(),
        description: "Open an issue in a public GitHub repository that is not registered with \
             this project, such as a third-party dependency where you found a bug. For a \
             registered repository use create_issue. The user approves each filing. Write it \
             for a public tracker: describe the behavior and a minimal reproduction, and leave \
             out the project's and user's names, repository names, local paths, task ids and \
             secrets; a call that includes them is refused unless include_project_details is \
             set, which the user then sees. If the call answers 'waiting on you', end your turn."
            .into(),
        input_schema: json!({
            "type":"object","properties":{
                "upstream":{"type":"string","description":"The GitHub repository as owner/name, e.g. tokio-rs/tokio."},
                "title":{"type":"string"},
                "body":{"type":"string"},
                INCLUDE_PROJECT_DETAILS:{"type":"boolean","description":"Keep project details the check found in the text. Only when the user asked to share them."}
            },
            "required":["upstream","title","body"],"additionalProperties":false
        }),
        tier: Tier::SharedHardToUndo,
        safe_to_repeat: false,
    }
}

/// A checked call: where it files, and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Filing {
    /// GitHub `owner/name`, as given.
    pub upstream: String,
    pub title: String,
    pub body: String,
    pub include_project_details: bool,
}

impl Filing {
    /// The remote the runner files against.
    pub(crate) fn remote(&self) -> String {
        format!("https://github.com/{}", self.upstream)
    }
}

/// Checks a call's shape, without the project.
pub(crate) fn parse(args: &Json) -> Result<Filing, ToolError> {
    fields(
        args,
        &["upstream", "title", "body", INCLUDE_PROJECT_DETAILS],
    )?;
    let upstream = args["upstream"]
        .as_str()
        .map(str::trim)
        .filter(|slug| valid_slug(slug))
        .ok_or_else(|| {
            invalid("upstream must be a GitHub repository as owner/name, such as tokio-rs/tokio")
        })?
        .to_owned();
    let body = args["body"]
        .as_str()
        .filter(|body| !body.trim().is_empty() && plain_text(body))
        .ok_or_else(|| invalid("body must be plain text of 1 to 20000 characters"))?
        .to_owned();
    let include_project_details = match args.get(INCLUDE_PROJECT_DETAILS) {
        None => false,
        Some(Json::Bool(include)) => *include,
        Some(_) => return Err(invalid("include_project_details must be a boolean")),
    };
    Ok(Filing {
        upstream,
        title: issue_title(args)?,
        body,
        include_project_details,
    })
}

/// A GitHub `owner/name`: an owner of up to 39 letters, digits and inner
/// hyphens, and a name of up to 100 letters, digits, `.`, `_` and `-`.
fn valid_slug(slug: &str) -> bool {
    let Some((owner, name)) = slug.split_once('/') else {
        return false;
    };
    let owner_ok = (1..=39).contains(&owner.len())
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let name_ok = (1..=100).contains(&name.len())
        && name != "."
        && name != ".."
        && !name.ends_with(".git")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    owner_ok && name_ok
}

/// What the text names that should not reach a public tracker by default.
pub(crate) fn private_details(
    text: &str,
    project: Option<&str>,
    registered: &[Repository],
) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut found = Vec::new();
    for marker in [
        "/users/",
        "/home/",
        ":\\users\\",
        "/private/var/",
        "/var/folders/",
        ".threadlane",
    ] {
        if lower.contains(marker) {
            found.push("a local path".to_owned());
            break;
        }
    }
    if words(text).any(threadlane_id) {
        found.push("a Threadlane task or decision id".to_owned());
    }
    if let Some(project) = project
        && has_word(&lower, &project.to_ascii_lowercase())
    {
        found.push(format!("the project name {project:?}"));
    }
    for repository in registered {
        let slug = github_repository(&repository.remote_url);
        let mut names = vec![repository.name.clone()];
        if let Some(slug) = &slug {
            names.push(slug.clone());
            if let Some((owner, _)) = slug.split_once('/') {
                names.push(owner.to_owned());
            }
        }
        if lower.contains(&repository.remote_url.to_ascii_lowercase())
            || names
                .iter()
                .any(|name| has_word(&lower, &name.to_ascii_lowercase()))
        {
            found.push(format!("the registered repository {:?}", repository.name));
        }
    }
    found
}

/// Whether the text carries something that looks like a credential.
pub(crate) fn credential(text: &str) -> bool {
    text.contains("PRIVATE KEY")
        || words(text).any(|word| {
            [
                "ghp_",
                "gho_",
                "ghs_",
                "ghu_",
                "github_pat_",
                "sk-",
                "xox",
                "AKIA",
            ]
            .iter()
            .any(|prefix| word.starts_with(prefix) && word.len() >= prefix.len() + 16)
        })
}

/// Words of letters, digits, `_` and `-`.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .filter(|word| !word.is_empty())
}

/// `T-0123` or `D-0123`.
fn threadlane_id(word: &str) -> bool {
    word.split_once('-').is_some_and(|(kind, digits)| {
        matches!(kind, "T" | "D") && digits.len() >= 4 && digits.chars().all(|c| c.is_ascii_digit())
    })
}

/// Whether `needle`, of at least 3 characters, occurs in `haystack` with no
/// letter or digit on either side.
fn has_word(haystack: &str, needle: &str) -> bool {
    if needle.chars().count() < 3 {
        return false;
    }
    haystack.match_indices(needle).any(|(at, _)| {
        let before = haystack[..at].chars().next_back();
        let after = haystack[at + needle.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// Checks a call against the project and words the card the user decides.
pub(crate) async fn proposal(
    db: &HubDb,
    lane: &Lane,
    arguments: &Json,
) -> Result<Proposal, ToolError> {
    let filing = parse(arguments)?;
    let task_id = lane.task_id.ok_or_else(|| {
        ToolError::Blocked(format!(
            "{FILE_UPSTREAM_ISSUE} is available only in a task lane"
        ))
    })?;
    let task = db
        .task(task_id)
        .await
        .map_err(tool_error)?
        .ok_or_else(|| ToolError::Failed(format!("task {task_id} not found")))?;
    let registered = db.repositories(lane.project_id).await.map_err(tool_error)?;
    if let Some(repository) = registered.iter().find(|repository| {
        github_repository(&repository.remote_url)
            .is_some_and(|slug| slug.eq_ignore_ascii_case(&filing.upstream))
    }) {
        return Err(ToolError::InvalidArguments(format!(
            "{} is registered in this project as {:?}: use create_issue with that repository",
            filing.upstream, repository.name
        )));
    }
    let text = format!("{}\n{}", filing.title, filing.body);
    if credential(&text) {
        return Err(ToolError::Blocked(
            "the issue text contains what looks like a credential; remove it and call again".into(),
        ));
    }
    let project = db
        .project(lane.project_id)
        .await
        .map_err(tool_error)?
        .filter(|project| !project.is_general);
    let found = private_details(
        &text,
        project.as_ref().map(|project| project.name.as_str()),
        &registered,
    );
    if !found.is_empty() && !filing.include_project_details {
        return Err(ToolError::Blocked(format!(
            "the issue would be public in {}, and its text names {}. Rewrite it without them \
             and call again; set include_project_details only if the user asked to share them.",
            filing.upstream,
            found.join(", ")
        )));
    }
    let shares = if found.is_empty() {
        String::new()
    } else {
        format!(
            "\nThe agent asks to include project details: {}.",
            found.join(", ")
        )
    };
    Ok(Proposal {
        question: format!(
            "Open issue “{}” in {}? It is public, outside this project.",
            filing.title, filing.upstream
        ),
        context: format!(
            "Task {}: {}.\n{} is not registered with this project; you approve each filing \
             there.{shares}\n\n{}",
            task.id, task.title, filing.upstream, filing.body
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_core::ids::{ProjectId, RepositoryId};

    fn repository(name: &str, remote: &str) -> Repository {
        Repository {
            id: RepositoryId::from(1),
            project_id: ProjectId::from(1),
            name: name.into(),
            remote_url: remote.into(),
            default_branch: "main".into(),
            protected_branches: Vec::new(),
        }
    }

    #[test]
    fn a_filing_names_a_github_repository_a_title_and_a_body() {
        let filing = parse(&json!({
            "upstream": " tokio-rs/tokio ", "title": " Panic on empty read ", "body": "Steps"
        }))
        .unwrap();
        assert_eq!(filing.upstream, "tokio-rs/tokio");
        assert_eq!(filing.title, "Panic on empty read");
        assert!(!filing.include_project_details);
        assert_eq!(filing.remote(), "https://github.com/tokio-rs/tokio");
        for upstream in [
            "tokio",
            "https://github.com/tokio-rs/tokio",
            "a/b/c",
            "-x/y",
            "x/..",
            "x/y.git",
            "x y/z",
        ] {
            assert!(
                parse(&json!({"upstream": upstream, "title": "t", "body": "b"})).is_err(),
                "{upstream}"
            );
        }
        assert!(parse(&json!({"upstream": "a/b", "title": "t", "body": "  "})).is_err());
        assert!(parse(&json!({"upstream": "a/b", "title": "t"})).is_err());
        assert!(parse(&json!({"upstream": "a/b", "title": "-t", "body": "b"})).is_err());
        assert!(
            parse(&json!({"upstream": "a/b", "title": "t", "body": "b", "repository": "x"}))
                .is_err()
        );
        assert!(
            parse(&json!({"upstream": "a/b", "title": "t", "body": "b", INCLUDE_PROJECT_DETAILS: "yes"}))
                .is_err()
        );
    }

    #[test]
    fn project_details_are_found_and_generic_text_passes() {
        let registered = [repository(
            "orchard-app",
            "git@github.com:acme-private/orchard-app.git",
        )];
        let check = |text: &str| private_details(text, Some("Orchard"), &registered);
        assert!(
            check("Reading an empty buffer panics in 1.2.3.\n\nSteps: call read(&mut []).")
                .is_empty()
        );
        assert_eq!(check("Seen at /Users/me/src/x.rs"), ["a local path"]);
        assert_eq!(
            check("Found while working on T-0650"),
            ["a Threadlane task or decision id"]
        );
        assert_eq!(
            check("Our orchard build breaks"),
            ["the project name \"Orchard\""]
        );
        // Part of another word is not the name.
        assert!(check("orchards and orchardist").is_empty());
        assert_eq!(
            check("see acme-private/orchard-app#4"),
            [
                "the project name \"Orchard\"",
                "the registered repository \"orchard-app\""
            ]
        );
        assert_eq!(
            check("ACME-private owns it"),
            ["the registered repository \"orchard-app\""]
        );
    }

    #[test]
    fn credentials_are_found() {
        assert!(credential("token ghp_abcdefghijklmnopqrstuvwxyz"));
        assert!(credential("-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(!credential("the sk-learn crate and ghp_ prefixes"));
    }
}
