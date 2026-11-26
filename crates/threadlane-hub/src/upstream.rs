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
    if local_path(&lower) {
        found.push("a local path".to_owned());
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
            if let Some((owner, name)) = slug.split_once('/') {
                names.push(owner.to_owned());
                names.push(name.to_owned());
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

/// Whether the lowercased text names a path on a local machine: a home or
/// macOS temporary directory, the Threadlane data directory, or a Windows drive
/// path with either slash.
fn local_path(lower: &str) -> bool {
    const MARKERS: [&str; 7] = [
        "/users/",
        "\\users\\",
        "/home/",
        "/root/",
        "/private/var/",
        "/var/folders/",
        ".threadlane",
    ];
    MARKERS.iter().any(|marker| lower.contains(marker))
        || lower.match_indices(':').any(|(at, _)| {
            // `c:/` or `c:\`, with no letter or digit before the drive letter,
            // so `https://` is not a drive.
            let mut before = lower[..at].chars().rev();
            let drive = before.next().is_some_and(|c| c.is_ascii_alphabetic());
            let alone = !before.next().is_some_and(char::is_alphanumeric);
            let slash = matches!(lower[at + 1..].chars().next(), Some('/' | '\\'));
            drive && alone && slash
        })
}

/// Whether the text carries something that looks like a credential: a key
/// block, a token with a known prefix, a secret assigned to a key such as
/// `password=` or `api_key:`, or a long mixed-case alphanumeric string.
pub(crate) fn credential(text: &str) -> bool {
    text.contains("PRIVATE KEY")
        || words(text).any(|word| prefixed_token(word) || opaque_token(word))
        || assigned_secret(text)
}

/// A token whose prefix names its issuer, with enough characters after it.
fn prefixed_token(word: &str) -> bool {
    [
        "ghp_",
        "gho_",
        "ghs_",
        "ghu_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "sk-",
        "sk_live_",
        "rk_live_",
        "pk_live_",
        "xox",
        "AKIA",
        "ASIA",
        "AIza",
        "npm_",
        "pypi-",
        "hf_",
        "eyJ",
    ]
    .iter()
    .any(|prefix| word.starts_with(prefix) && word.len() >= prefix.len() + 16)
}

/// A string of at least 32 characters with upper- and lowercase letters and
/// digits, as random keys are. Hex digests, which are one case, pass.
fn opaque_token(word: &str) -> bool {
    word.len() >= 32
        && word.chars().any(|c| c.is_ascii_uppercase())
        && word.chars().any(|c| c.is_ascii_lowercase())
        && word.chars().any(|c| c.is_ascii_digit())
}

/// A secret-named key followed by `=` or `:` and a value of at least 8
/// characters with letters and digits, such as `password=hunter2s3cret`.
/// Placeholders like `<token>` or `$TOKEN` have no digit and pass.
fn assigned_secret(text: &str) -> bool {
    const KEYS: [&str; 12] = [
        "password",
        "passwd",
        "pwd",
        "secret",
        "token",
        "api_key",
        "apikey",
        "api-key",
        "access_key",
        "auth",
        "credential",
        "key",
    ];
    let lower = text.to_ascii_lowercase();
    lower.match_indices(['=', ':']).any(|(at, _)| {
        let key = lower[..at]
            .trim_end_matches([' ', '"', '\''])
            .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .next()
            .unwrap_or("");
        let named = KEYS.iter().any(|name| {
            key == *name || key.ends_with(&format!("_{name}")) || key.ends_with(&format!("-{name}"))
        });
        let rest = text[at + 1..].trim_start_matches([' ', '"', '\'']);
        let (value, after) = rest.split_at(
            rest.find(|c: char| {
                !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '='))
            })
            .unwrap_or(rest.len()),
        );
        // A call, a field or a `::` after the value means code, not a secret.
        let ends = after
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';'));
        named
            && ends
            && value.len() >= 8
            && value.chars().any(|c| c.is_ascii_alphabetic())
            && value.chars().any(|c| c.is_ascii_digit())
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

/// Whether `needle` occurs in `haystack` with no letter or digit on either
/// side. Short names count too: a project called `ai` matches "the ai crate"
/// but not "maintain".
fn has_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
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
            "the issue may be public in {}, and its text names {}. Rewrite it without them \
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
            "Open issue “{}” in {}? It is outside this project, and its visibility is not \
             checked: treat it as public.",
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
    fn the_bare_repository_name_of_a_registered_slug_is_found() {
        // Registered under a name unlike its GitHub slug (#780 review).
        let registered = [repository(
            "client",
            "https://github.com/private-co/saffron-engine",
        )];
        assert_eq!(
            private_details("saffron-engine panics on start", None, &registered),
            ["the registered repository \"client\""]
        );
        assert!(private_details("saffron panics on start", None, &registered).is_empty());
    }

    #[test]
    fn short_names_are_matched_as_whole_words() {
        let registered = [repository("ui", "https://github.com/acme/ui")];
        let check = |text: &str| private_details(text, Some("AI"), &registered);
        assert_eq!(check("The AI build breaks"), ["the project name \"AI\""]);
        assert_eq!(check("the ui crate"), ["the registered repository \"ui\""]);
        // Inside another word, a short name is not the name.
        assert!(check("maintain the build, quietly").is_empty());
        assert!(has_word("x", "x"));
        assert!(!has_word("ab", ""));
    }

    #[test]
    fn local_paths_of_every_platform_are_found() {
        for text in [
            "repro: cat /root/app/.env",
            "at C:/Users/me/app/main.rs",
            "at C:\\Users\\me\\app",
            "built in D:\\work\\app",
            "in d:/src/app",
            "under /home/me",
            "in /private/var/folders/x",
            "~/.threadlane/hub",
        ] {
            assert_eq!(private_details(text, None, &[]), ["a local path"], "{text}");
        }
        for text in [
            "see https://example.com/a/b",
            "the error: /etc/hosts is unreadable",
            "use a::b/c",
        ] {
            assert!(private_details(text, None, &[]).is_empty(), "{text}");
        }
    }

    #[test]
    fn credentials_are_found() {
        assert!(credential("token ghp_abcdefghijklmnopqrstuvwxyz"));
        assert!(credential("-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(credential("glpat-abcdefghijklmnopqrst"));
        assert!(credential("key AIzaSyAbcdefghijklmnopqrstu"));
        assert!(credential(
            "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6"
        ));
        assert!(credential("export STRIPE=sk_live_abcdefghijklmnopqr"));
        assert!(credential("password=hunter2s3cret"));
        assert!(credential("DB_PASSWORD: \"hunter2s3cret\""));
        assert!(credential("api_key = 'a1b2c3d4e5f6'"));
        assert!(credential("fails with key: hunter2s3cret at start"));
        assert!(credential("x Q7vLm2Xp9RtZ4kWn8YbC3dFh6JsA1eGu"));
        assert!(!credential("the sk-learn crate and ghp_ prefixes"));
        // Placeholders, code and digests are not secrets.
        assert!(!credential("password=<your password>"));
        assert!(!credential("set token: $GITHUB_TOKEN"));
        assert!(!credential("let token = parse_token2(input);"));
        assert!(!credential("token: Token2Kind::Ident"));
        assert!(!credential(
            "at commit 0ca4304d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b"
        ));
        assert!(!credential("monkey: banana12345"));
    }
}
