//! The memory tools (Plan.md §8, docs/milestone-5.md §2): `memory_view`,
//! `memory_create`, `memory_edit`, `memory_delete`, `memory_rename`.
//!
//! An agent names paths under two roots, `space/…` and `project/…`, and
//! [`scope`] maps them to stored paths for its run, so no tool can name
//! another project (invariant 12). A change to an instructions file in a run
//! you did not trigger is asked as a decision; the hub applies it once you
//! approve ([`crate::gateway`]).

use serde_json::{Map, Value as Json, json};
use threadlane_core::ids::{ProjectId, TaskId};
use threadlane_core::link::PROGRESS_WRITTEN;
use threadlane_core::memory::{SKILLS_PREFIX, progress_path};
use threadlane_core::model::{LaneKind, MemoryAuthor, MemoryFile};
use threadlane_core::store::{MemoryStore, StoreError};
use threadlane_core::tools::{Tier, ToolDefinition, ToolError};
use threadlane_store::HubDb;

use crate::error::tool_error;
use crate::page;
use crate::publish::Proposal;
use crate::tools::{fields, invalid};

/// Shows a file, or lists a directory.
pub const MEMORY_VIEW: &str = "memory_view";
/// Creates a file, or replaces its content.
pub const MEMORY_CREATE: &str = "memory_create";
/// Replaces one exact string in a file.
pub const MEMORY_EDIT: &str = "memory_edit";
/// Deletes a file.
pub const MEMORY_DELETE: &str = "memory_delete";
/// Moves a file.
pub const MEMORY_RENAME: &str = "memory_rename";

/// The most characters one `memory_view` page of a file may ask for.
const MAX_VIEW: usize = 50_000;
/// The most files one page of a listing holds.
const LISTING: usize = 500;
/// A file larger than this is shown in pages, so a write warns of it.
const LARGE_FILE: usize = page::PAGE_CHARS;

/// The `limit` characters of `text` from character `offset`, ending at a
/// line break when one falls in the page, and the offset after them if any
/// text remains.
fn window(text: &str, offset: usize, limit: usize) -> (&str, Option<usize>) {
    let byte = |chars: usize| text.char_indices().nth(chars).map(|(at, _)| at);
    let Some(start) = byte(offset) else {
        return ("", None);
    };
    let rest = &text[start..];
    let Some(end) = rest.char_indices().nth(limit).map(|(at, _)| at) else {
        return (rest, None);
    };
    let end = rest[..end].rfind('\n').map_or(end, |line| line + 1);
    let shown = &rest[..end];
    (shown, Some(offset + shown.chars().count()))
}

/// The two instructions files as an agent names them.
const PROTECTED: [&str; 2] = ["space/instructions.md", "project/instructions.md"];

/// Whether `tool` changes memory.
pub(crate) fn is_change(tool: &str) -> bool {
    matches!(
        tool,
        MEMORY_CREATE | MEMORY_EDIT | MEMORY_DELETE | MEMORY_RENAME
    )
}

/// Whether `tool` is a memory tool.
pub(crate) fn is_memory(tool: &str) -> bool {
    tool == MEMORY_VIEW || is_change(tool)
}

/// Maps a path as an agent wrote it, in a run in a lane of `kind` in
/// `project`, to the stored path, or refuses it (docs/milestone-5.md §1
/// decisions 2 and 3, invariant 12):
///
/// - `project` and `project/…` are `projects/<project>/…`, the run's own
///   project; no path names another.
/// - `space` and `space/notes/…` are the space's, for every run;
///   `space/instructions.md` only for a lead. Nothing else in `space/` is.
/// - Any other root, an absolute path, an empty part, `.` or `..` is refused.
pub(crate) fn scope(project: ProjectId, kind: LaneKind, path: &str) -> Result<String, ToolError> {
    let refuse = |why: &str| Err(ToolError::InvalidArguments(format!("{path:?}: {why}")));
    let parts: Vec<&str> = path.split('/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
        || path.contains('\\')
        || path.chars().any(char::is_control)
    {
        return refuse(
            "memory paths are relative, start with space/ or project/, and have no empty, \
             . or .. parts",
        );
    }
    match parts[..] {
        ["project", ..] => Ok(format!(
            "projects/{}{}",
            project.get(),
            &path["project".len()..]
        )),
        ["space"] | ["space", "notes", ..] | ["space", "skills", ..] => Ok(path.to_owned()),
        ["space", "instructions.md"] if kind == LaneKind::Lead => Ok(path.to_owned()),
        ["space", "instructions.md"] => {
            refuse("the space's instructions are not available in a task lane")
        }
        ["space", ..] => refuse("in space/ there are only instructions.md, notes/ and skills/"),
        _ => refuse("memory paths start with space/ or project/"),
    }
}

/// A stored path as the run sees it, if [`scope`] lets the run see it.
pub(crate) fn visible(project: ProjectId, kind: LaneKind, stored: &str) -> Option<String> {
    let own = format!("projects/{}/", project.get());
    let named = match stored.strip_prefix(&own) {
        Some(rest) => format!("project/{rest}"),
        None => stored.to_owned(),
    };
    (scope(project, kind, &named).ok().as_deref() == Some(stored)).then_some(named)
}

/// Whether a call changes an instructions file or space skill playbook,
/// which only a run you triggered may do without asking
/// (docs/milestone-5.md §1 decision 6).
pub(crate) fn protected(tool: &str, arguments: &Json) -> bool {
    is_change(tool)
        && ["path", "from", "to"].iter().any(|field| {
            arguments
                .get(field)
                .and_then(Json::as_str)
                .is_some_and(|path| {
                    PROTECTED.contains(&path)
                        || path == "space/skills"
                        || path.starts_with(SKILLS_PREFIX)
                })
        })
}

/// The lane kind of a run that works `task`, if any: a task lane is the one
/// lane kind that works a task.
fn kind_of(task: Option<TaskId>) -> LaneKind {
    if task.is_some() {
        LaneKind::Task
    } else {
        LaneKind::Lead
    }
}

/// Refuses a change by `task`'s agent to any progress file but its own
/// (docs/milestone-5.md §4). The lead may change any of them.
fn own_progress(project: ProjectId, task: Option<TaskId>, stored: &str) -> Result<(), ToolError> {
    let Some(task) = task else {
        return Ok(());
    };
    let own = format!("projects/{}/", project.get());
    if stored.starts_with(&format!("{own}tasks/")) && stored != progress_path(project, task) {
        let named = stored.replacen(&own, "project/", 1);
        return Err(ToolError::InvalidArguments(format!(
            "{named:?}: in project/tasks/ you may change only your own task's progress file, \
             project/tasks/{task}.md"
        )));
    }
    Ok(())
}

/// A memory call, checked, with stored paths.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    View {
        path: String,
        offset: usize,
        limit: usize,
    },
    Create {
        path: String,
        content: String,
    },
    Edit {
        path: String,
        old: String,
        new: String,
    },
    Delete {
        path: String,
    },
    Rename {
        from: String,
        to: String,
    },
}

fn text<'a>(object: &'a Map<String, Json>, name: &str) -> Result<&'a str, ToolError> {
    object
        .get(name)
        .and_then(Json::as_str)
        .ok_or_else(|| ToolError::InvalidArguments(format!("{name} must be text")))
}

/// Checks a call's arguments and maps its paths, for a run in `project`
/// working `task` (a lead's run works none).
fn parse(
    project: ProjectId,
    task: Option<TaskId>,
    tool: &str,
    arguments: &Json,
) -> Result<Call, ToolError> {
    let kind = kind_of(task);
    let path = |object: &Map<String, Json>, name: &str| -> Result<String, ToolError> {
        scope(project, kind, text(object, name)?)
    };
    // A path the call changes.
    let changed = |object: &Map<String, Json>, name: &str| -> Result<String, ToolError> {
        let stored = path(object, name)?;
        own_progress(project, task, &stored)?;
        Ok(stored)
    };
    Ok(match tool {
        MEMORY_VIEW => {
            let object = fields(arguments, &["path", "offset", "limit"])?;
            let (offset, limit) = page::offset_limit(object, page::PAGE_CHARS, MAX_VIEW)?;
            Call::View {
                path: path(object, "path")?,
                offset,
                limit,
            }
        }
        MEMORY_CREATE => {
            let object = fields(arguments, &["path", "content"])?;
            Call::Create {
                path: changed(object, "path")?,
                content: text(object, "content")?.to_owned(),
            }
        }
        MEMORY_EDIT => {
            let object = fields(arguments, &["path", "old_text", "new_text"])?;
            let old = text(object, "old_text")?;
            if old.is_empty() {
                return Err(invalid("old_text must not be empty"));
            }
            Call::Edit {
                path: changed(object, "path")?,
                old: old.to_owned(),
                new: text(object, "new_text")?.to_owned(),
            }
        }
        MEMORY_DELETE => Call::Delete {
            path: changed(fields(arguments, &["path"])?, "path")?,
        },
        MEMORY_RENAME => {
            let object = fields(arguments, &["from", "to"])?;
            Call::Rename {
                from: changed(object, "from")?,
                to: changed(object, "to")?,
            }
        }
        other => return Err(ToolError::UnknownTool(other.to_owned())),
    })
}

/// A refusal from the store as a run in `project` sees it, with its paths
/// as the run names them.
pub(crate) fn refused(project: ProjectId, error: StoreError) -> ToolError {
    let named =
        |message: &str| message.replace(&format!("projects/{}/", project.get()), "project/");
    match error {
        StoreError::Invalid(message) | StoreError::Conflict(message) => {
            ToolError::InvalidArguments(named(&message))
        }
        StoreError::NotFound { id, .. } => {
            ToolError::InvalidArguments(named(&format!("there is no file at {id}")))
        }
        other => tool_error(other),
    }
}

/// Performs a memory call of a run in `project` working `task` (a lead's
/// run works none), written as `by`. Paths in the result are as the run
/// names them. A write of the task's own progress file says so in
/// [`PROGRESS_WRITTEN`], for the runner.
pub(crate) async fn execute(
    db: &HubDb,
    by: &MemoryAuthor,
    project: ProjectId,
    task: Option<TaskId>,
    tool: &str,
    arguments: &Json,
) -> Result<Json, ToolError> {
    let kind = kind_of(task);
    let refused = |error| refused(project, error);
    let shown = |file: &MemoryFile| visible(project, kind, &file.path).unwrap_or_default();
    let progress =
        |file: &MemoryFile| task.is_some_and(|task| file.path == progress_path(project, task));
    let written = |file: MemoryFile| {
        let mut result = json!({ "path": shown(&file), "version": file.version });
        if progress(&file) {
            result[PROGRESS_WRITTEN] = json!(true);
        }
        let size = file.content.chars().count();
        if size > LARGE_FILE {
            result["warning"] = json!(format!(
                "This file is now {size} characters, more than {LARGE_FILE}, so memory_view \
                 shows it in pages. Condense it or split it into smaller files."
            ));
        }
        result
    };
    match parse(project, task, tool, arguments)? {
        Call::View {
            path,
            offset,
            limit,
        } => {
            if let Some(file) = db.read(&path).await.map_err(tool_error)? {
                let (content, next) = window(&file.content, offset, limit);
                let mut result = json!({
                    "path": shown(&file),
                    "version": file.version,
                    "content": content,
                });
                if offset > 0 || next.is_some() {
                    result["offset"] = json!(offset);
                    page::mark(&mut result, MEMORY_VIEW, file.content.chars().count(), next);
                }
                return Ok(result);
            }
            let files: Vec<Json> = db
                .list(&format!("{path}/"))
                .await
                .map_err(tool_error)?
                .iter()
                .filter_map(|file| visible(project, kind, &file.path).map(Json::String))
                .collect();
            // The roots, their fixed directories and the instructions files
            // always exist, though they may hold nothing yet.
            let named = arguments["path"].as_str().unwrap_or_default();
            if files.is_empty() && named.ends_with("/instructions.md") {
                return Ok(json!({
                    "path": named,
                    "content": "",
                    "note": "This file has not been written yet.",
                }));
            }
            let root = matches!(
                named,
                "space"
                    | "project"
                    | "space/notes"
                    | "space/skills"
                    | "project/notes"
                    | "project/tasks"
                    | "project/repos"
            );
            if files.is_empty() && !root {
                return Err(ToolError::InvalidArguments(format!(
                    "there is no file or directory at {}",
                    arguments["path"]
                )));
            }
            // A listing pages by file, in pages of at most LISTING.
            let (listed, next) = page::take(&files, offset, limit.min(LISTING));
            let mut result = json!({ "files": listed });
            if offset > 0 || next.is_some() {
                page::mark(&mut result, MEMORY_VIEW, files.len(), next);
            }
            Ok(result)
        }
        Call::Create { path, content } => Ok(written(
            db.write(by, &path, &content).await.map_err(refused)?,
        )),
        Call::Edit { path, old, new } => Ok(written(
            db.edit(by, &path, &old, &new).await.map_err(refused)?,
        )),
        Call::Delete { path } => {
            let file = db.delete(by, &path).await.map_err(refused)?;
            Ok(json!({ "path": shown(&file), "deleted": true }))
        }
        Call::Rename { from, to } => {
            let file = db.rename(by, &from, &to).await.map_err(refused)?;
            let mut result = json!({
                "from": visible(project, kind, &from),
                "to": shown(&file),
                "version": file.version,
            });
            if progress(&file) {
                result[PROGRESS_WRITTEN] = json!(true);
            }
            Ok(result)
        }
    }
}

/// What a decision on a protected memory change asks you. The call is
/// checked first, so a call its run may not make is refused, not asked.
pub(crate) fn proposal(
    project: ProjectId,
    task: Option<TaskId>,
    tool: &str,
    arguments: &Json,
) -> Result<Proposal, ToolError> {
    let about = "Instructions are loaded into every run. This change was asked in a run you \
                 did not start, so it waits for you.";
    let named = |field: &str| {
        arguments
            .get(field)
            .and_then(Json::as_str)
            .unwrap_or_default()
    };
    let (question, detail) = match parse(project, task, tool, arguments)? {
        Call::Create { path, content } => {
            // Asking about a write the store would refuse helps no one.
            threadlane_core::memory::check(&path, &content).map_err(ToolError::InvalidArguments)?;
            (
                format!("Replace {} with new content?", named("path")),
                format!("New content:\n\n{content}"),
            )
        }
        Call::Edit { old, new, .. } => (
            format!("Edit {}?", named("path")),
            format!("Replace:\n\n{old}\n\nwith:\n\n{new}"),
        ),
        Call::Delete { .. } => (format!("Delete {}?", named("path")), String::new()),
        Call::Rename { .. } => (
            format!("Move {} to {}?", named("from"), named("to")),
            String::new(),
        ),
        Call::View { .. } => return Err(invalid("viewing memory is never asked")),
    };
    Ok(Proposal {
        question,
        context: format!("{about}\n\n{detail}").trim_end().to_owned(),
    })
}

/// What the session is told a stored memory call did, once performed.
pub(crate) fn performed(tool: &str, arguments: &Json) -> String {
    let named = |field: &str| {
        arguments
            .get(field)
            .and_then(Json::as_str)
            .unwrap_or_default()
    };
    match tool {
        MEMORY_DELETE => format!("{} is deleted.", named("path")),
        MEMORY_RENAME => format!("{} is now {}.", named("from"), named("to")),
        _ => format!("{} is written.", named("path")),
    }
}

/// The definitions of the memory tools, in the order they are offered.
pub(crate) fn definitions() -> Vec<ToolDefinition> {
    let path = json!({
        "type": "string",
        "description": "A path under space/ (the space's notes/, skills/, and for the lead its \
                        instructions.md) or project/ (this project's instructions.md, notes/, \
                        repos/, tasks/)."
    });
    let define = |name: &str, description: &str, tier: Tier, schema: Json| ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: schema,
        tier,
        // Every change is keyed on its call, so repeating one writes nothing.
        safe_to_repeat: true,
    };
    let protected = " A change to an instructions file in a run the user did not start asks \
                     them first: the call returns \"waiting on you\", and you must then end your \
                     turn.";
    vec![
        define(
            MEMORY_VIEW,
            "Show a memory file, or list the files under a directory such as project or \
             space/notes. A long file comes in pages of 20000 characters; a cut page gives \
             the offset of the next one.",
            Tier::Read,
            json!({
                "type": "object",
                "properties": {
                    "path": path.clone(),
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "The character (or, for a directory, the file) to start at."
                    },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_VIEW }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        ),
        define(
            MEMORY_CREATE,
            &format!(
                "Create a memory file, or replace its content. A file in notes/ or tasks/ \
                 starts with a title line and a one-line description, which become its line \
                 in that directory's INDEX.md; the system writes the index.{protected}"
            ),
            Tier::Local,
            json!({
                "type": "object",
                "properties": { "path": path.clone(), "content": { "type": "string" } },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        ),
        define(
            MEMORY_EDIT,
            &format!(
                "Replace one exact piece of text in a memory file. It must occur exactly once.\
                 {protected}"
            ),
            Tier::Local,
            json!({
                "type": "object",
                "properties": {
                    "path": path.clone(),
                    "old_text": { "type": "string" },
                    "new_text": { "type": "string" }
                },
                "required": ["path", "old_text", "new_text"],
                "additionalProperties": false
            }),
        ),
        define(
            MEMORY_DELETE,
            &format!("Delete a memory file. Its earlier versions are kept.{protected}"),
            Tier::Local,
            json!({
                "type": "object",
                "properties": { "path": path.clone() },
                "required": ["path"],
                "additionalProperties": false
            }),
        ),
        define(
            MEMORY_RENAME,
            &format!("Move a memory file to a path where there is none.{protected}"),
            Tier::Local,
            json!({
                "type": "object",
                "properties": { "from": path.clone(), "to": path },
                "required": ["from", "to"],
                "additionalProperties": false
            }),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: ProjectId = ProjectId(1);

    #[test]
    fn paths_map_into_the_runs_own_project_and_the_spaces_notes() {
        for kind in [LaneKind::Lead, LaneKind::Task] {
            for (named, stored) in [
                ("project", "projects/1"),
                ("project/instructions.md", "projects/1/instructions.md"),
                ("project/notes/build.md", "projects/1/notes/build.md"),
                ("project/tasks/T-0001.md", "projects/1/tasks/T-0001.md"),
                ("space", "space"),
                ("space/notes", "space/notes"),
                ("space/notes/style.md", "space/notes/style.md"),
                ("space/skills", "space/skills"),
                (
                    "space/skills/rust-pr-review.md",
                    "space/skills/rust-pr-review.md",
                ),
            ] {
                assert_eq!(scope(A, kind, named).as_deref(), Ok(stored), "{named}");
            }
            for refused in [
                "projects/2/notes/a.md",
                "projects/1/notes/a.md",
                "project/../projects/2/notes/a.md",
                "project/./a.md",
                "project//a.md",
                "project/",
                "/space/notes/a.md",
                "",
                "space/other.md",
                "space/tasks/a.md",
                "notes/a.md",
                "project\\a.md",
            ] {
                assert!(scope(A, kind, refused).is_err(), "{refused:?} in a {kind}");
            }
        }
        assert_eq!(
            scope(A, LaneKind::Lead, "space/instructions.md").as_deref(),
            Ok("space/instructions.md")
        );
        assert!(scope(A, LaneKind::Task, "space/instructions.md").is_err());
        assert_eq!(visible(A, LaneKind::Task, "space/instructions.md"), None);
        assert_eq!(visible(A, LaneKind::Task, "projects/2/notes/a.md"), None);
        assert_eq!(
            visible(A, LaneKind::Task, "projects/1/notes/a.md").as_deref(),
            Some("project/notes/a.md")
        );
    }

    #[test]
    fn a_file_is_shown_in_windows_that_end_at_a_line_break() {
        assert_eq!(window("abc", 0, 10), ("abc", None));
        assert_eq!(window("abc", 0, 3), ("abc", None));
        assert_eq!(window("abcdef", 0, 4), ("abcd", Some(4)));
        assert_eq!(window("abcdef", 4, 4), ("ef", None));
        assert_eq!(window("abc", 3, 4), ("", None));
        assert_eq!(window("abc", 9, 4), ("", None));
        assert_eq!(window("ab\ncd\nef", 0, 7), ("ab\ncd\n", Some(6)));
        assert_eq!(window("ab\ncd\nef", 6, 7), ("ef", None));
        assert_eq!(window("é\nüx", 0, 3), ("é\n", Some(2)));
        assert_eq!(window("é\nüx", 2, 3), ("üx", None));
    }

    #[test]
    fn changes_to_either_instructions_file_are_protected() {
        assert!(protected(
            MEMORY_EDIT,
            &json!({ "path": "project/instructions.md" })
        ));
        assert!(protected(
            MEMORY_RENAME,
            &json!({ "from": "project/notes/a.md", "to": "space/instructions.md" })
        ));
        assert!(!protected(
            MEMORY_VIEW,
            &json!({ "path": "project/instructions.md" })
        ));
        assert!(protected(
            MEMORY_CREATE,
            &json!({ "path": "space/skills/rust-pr-review.md" })
        ));
        assert!(protected(
            MEMORY_RENAME,
            &json!({ "from": "project/notes/a.md", "to": "space/skills/rust-pr-review.md" })
        ));
        assert!(!protected(
            MEMORY_CREATE,
            &json!({ "path": "project/notes/instructions.md" })
        ));
    }
}
