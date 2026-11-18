//! What the memory layout means (Plan.md §8.3, docs/milestone-5.md §2):
//! which stored files are loaded into runs, their size budgets, and the
//! indexes the hub writes from the title and description of each note.
//!
//! Paths here are stored paths: `space/…` and `projects/<project id>/…`.

use crate::ids::{ProjectId, TaskId};

/// The file name of every index. Only the hub writes one.
pub const INDEX_FILE: &str = "INDEX.md";

/// The largest an instructions file may be, in bytes. It is loaded whole
/// into every run of its space or project, so it is paid for on every run.
pub const INSTRUCTIONS_BUDGET: usize = 8 * 1024;

/// The largest a task's progress file may be, in bytes. Its agent is given
/// it whole at the start of every fresh session.
pub const PROGRESS_BUDGET: usize = 8 * 1024;

/// The largest the title and description of an indexed file may be together,
/// in bytes: they are its line in an index loaded into every run.
pub const INDEX_LINE_BUDGET: usize = 300;

/// The root (`space` or `projects/<id>`) of a stored path and the rest.
fn split_root(path: &str) -> Option<(&str, &str)> {
    if let Some(rest) = path.strip_prefix("space/") {
        return Some(("space", rest));
    }
    let (id, rest) = path.strip_prefix("projects/")?.split_once('/')?;
    let numeric = !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit());
    numeric.then_some((&path[.."projects/".len() + id.len()], rest))
}

/// The stored path of `task`'s progress file in `project`:
/// `projects/<project>/tasks/<task id>.md` (Plan.md §8.3).
#[must_use]
pub fn progress_path(project: ProjectId, task: TaskId) -> String {
    format!("projects/{}/tasks/{task}.md", project.get())
}

/// Whether `path` is the space's or a project's `instructions.md`.
#[must_use]
pub fn is_instructions(path: &str) -> bool {
    split_root(path).is_some_and(|(_, rest)| rest == "instructions.md")
}

/// Whether `path` is a task progress file, `projects/<id>/tasks/…`.
fn is_progress(path: &str) -> bool {
    split_root(path).is_some_and(|(root, rest)| {
        root != "space"
            && rest
                .strip_prefix("tasks/")
                .is_some_and(|name| !name.is_empty())
    })
}

/// The index that lists `path`: `notes/INDEX.md` of its root for a note,
/// `tasks/INDEX.md` of its project for a progress file. `None` for any other
/// file, and for an index itself.
#[must_use]
pub fn index_for(path: &str) -> Option<String> {
    if path.rsplit('/').next() == Some(INDEX_FILE) {
        return None;
    }
    let (root, rest) = split_root(path)?;
    if rest
        .strip_prefix("notes/")
        .is_some_and(|name| !name.is_empty())
    {
        return Some(format!("{root}/notes/{INDEX_FILE}"));
    }
    is_progress(path).then(|| format!("{root}/tasks/{INDEX_FILE}"))
}

/// The title and description of an indexed file: its first non-blank line,
/// without leading `#`, and the non-blank line after it.
#[must_use]
pub fn summary(content: &str) -> Option<(&str, &str)> {
    let mut lines = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let title = lines.next()?.trim_start_matches('#').trim();
    let description = lines.next()?;
    (!title.is_empty()).then_some((title, description))
}

/// Refuses `path` if it is an index, which only the hub writes.
pub fn not_an_index(path: &str) -> Result<(), String> {
    if path.rsplit('/').next() == Some(INDEX_FILE) {
        return Err(format!(
            "{INDEX_FILE} files are written by the system from the title and description of \
             each file next to them; write those files instead"
        ));
    }
    Ok(())
}

/// Checks that `content` may be written at `path`: no index, budgets kept,
/// and a title and description where an index lists the file. The error is
/// written for the agent, saying what to change.
pub fn check(path: &str, content: &str) -> Result<(), String> {
    not_an_index(path)?;
    let size = content.len();
    if is_instructions(path) && size > INSTRUCTIONS_BUDGET {
        return Err(format!(
            "{path} would be {size} bytes, over its budget of {INSTRUCTIONS_BUDGET}. It is \
             loaded into every run: trim it, or move detail into notes, which are read on demand"
        ));
    }
    if is_progress(path) && size > PROGRESS_BUDGET {
        return Err(format!(
            "{path} would be {size} bytes, over its budget of {PROGRESS_BUDGET}. Trim it to \
             what is done, what is left, and what the next run must know, or split detail into \
             notes"
        ));
    }
    if index_for(path).is_some() {
        let (title, description) = summary(content).ok_or_else(|| {
            format!(
                "{path} must start with a title line and then a one-line description: they are \
                 its line in the index"
            )
        })?;
        let line = title.len() + description.len();
        if line > INDEX_LINE_BUDGET {
            return Err(format!(
                "the title and description of {path} are {line} bytes, over the budget of \
                 {INDEX_LINE_BUDGET} for a line of the index, which is loaded into every run: \
                 shorten them and keep the detail in the body"
            ));
        }
    }
    Ok(())
}

/// The bytes of the index at `index`, from the `(path, content)` of every
/// file it lists: one line per file, sorted by path, so the same files
/// always give the same bytes.
#[must_use]
pub fn render_index(index: &str, files: &[(String, String)]) -> String {
    let dir = index.strip_suffix(INDEX_FILE).unwrap_or(index);
    let mut sorted: Vec<&(String, String)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    for (path, content) in sorted {
        let name = path.strip_prefix(dir).unwrap_or(path);
        match summary(content) {
            Some((title, description)) => {
                out.push_str(&format!("- [{title}]({name}): {description}\n"));
            }
            None => out.push_str(&format!("- {name}\n")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_and_progress_files_have_indexes_and_nothing_else_does() {
        for (path, index) in [
            ("space/notes/style.md", Some("space/notes/INDEX.md")),
            ("space/notes/a/b.md", Some("space/notes/INDEX.md")),
            (
                "projects/3/notes/build.md",
                Some("projects/3/notes/INDEX.md"),
            ),
            (
                "projects/3/tasks/T-0001.md",
                Some("projects/3/tasks/INDEX.md"),
            ),
            ("projects/3/notes/INDEX.md", None),
            ("projects/3/instructions.md", None),
            ("projects/3/repos/app.md", None),
            ("space/tasks/x.md", None),
            ("projects/x/notes/a.md", None),
            ("elsewhere/notes/a.md", None),
        ] {
            assert_eq!(index_for(path).as_deref(), index, "{path}");
        }
        assert!(is_instructions("space/instructions.md"));
        assert!(is_instructions("projects/12/instructions.md"));
        assert!(!is_instructions("projects/12/notes/instructions.md"));
    }

    #[test]
    fn a_summary_is_a_title_line_and_the_next_line() {
        assert_eq!(
            summary("# Build\n\nHow to build.\nMore."),
            Some(("Build", "How to build."))
        );
        assert_eq!(summary("# Build\n"), None);
        assert_eq!(summary("#\nHow to build."), None);
    }

    #[test]
    fn budgets_and_titles_are_checked() {
        let over = "x".repeat(INSTRUCTIONS_BUDGET + 1);
        assert!(
            check("space/instructions.md", &over)
                .unwrap_err()
                .contains("trim")
        );
        assert!(check("space/instructions.md", &"x".repeat(INSTRUCTIONS_BUDGET)).is_ok());
        assert!(
            check("projects/1/repos/app.md", &over).is_ok(),
            "not preloaded"
        );
        let progress = format!("# T-0001\nWhat is left.\n{over}");
        assert!(check("projects/1/tasks/T-0001.md", &progress).is_err());
        assert!(check("projects/1/notes/a.md", "no description").is_err());
        let long = format!("# {}\nshort", "t".repeat(INDEX_LINE_BUDGET));
        assert!(check("projects/1/notes/a.md", &long).is_err());
        assert!(check("projects/1/notes/a.md", "# A\nAbout a.").is_ok());
        assert!(check("projects/1/notes/INDEX.md", "# A\nAbout a.").is_err());
    }

    #[test]
    fn an_index_is_the_same_bytes_whatever_order_its_files_come_in() {
        let files = vec![
            ("space/notes/b.md".to_owned(), "# B\nAbout b.".to_owned()),
            (
                "space/notes/a.md".to_owned(),
                "# A\nAbout a.\nBody.".to_owned(),
            ),
        ];
        let mut reversed = files.clone();
        reversed.reverse();
        let index = render_index("space/notes/INDEX.md", &files);
        assert_eq!(index, "- [A](a.md): About a.\n- [B](b.md): About b.\n");
        assert_eq!(render_index("space/notes/INDEX.md", &reversed), index);
    }
}
