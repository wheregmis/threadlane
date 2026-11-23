//! A project's memory as the Memory tab and the "What the lead knows" card
//! show it (Plan.md §8.3, wireframe 3): the files its runs can see, grouped
//! as the plan lays them out, the one selected with its versions, and your
//! edits to it.

use std::collections::BTreeMap;

use threadlane_client::{ClientCore, ClientError};
use threadlane_core::client::{MemoryReply, MemoryRequest};
use threadlane_core::memory::INDEX_FILE;
use threadlane_core::model::MemoryFile;

use crate::{Answer, AppState, Gap, When, count_form, describe, gap_in, take};

/// The groups of the Memory tab, in the order Plan.md §8.3 lays them out:
/// a path prefix as agents name it, and the group's title.
const GROUPS: [(&str, &str); 6] = [
    ("space/instructions.md", "Space instructions"),
    ("space/notes/", "Space notes"),
    ("project/instructions.md", "Project instructions"),
    ("project/notes/", "Project notes"),
    ("project/repos/", "Repository notes"),
    ("project/tasks/", "Task progress files"),
];

/// The instructions files, which you may write while they do not exist yet.
const INSTRUCTIONS: [&str; 2] = ["space/instructions.md", "project/instructions.md"];

/// The title of an instructions file that does not exist yet.
const UNWRITTEN: &str = "Not written yet";

/// The title of the group for files no other group takes.
const OTHER: &str = "Other files";

/// One group of the Memory tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryGroup {
    /// `Space notes`, `Task progress files`, ...
    pub title: &'static str,
    /// Its files, by path.
    pub files: Vec<MemoryRow>,
}

/// One file in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRow {
    /// Its path as agents name it, `project/notes/build.md`.
    pub path: String,
    /// Its title line, else its file name; an index's file name.
    pub title: String,
    /// Whether it is the one shown on the right.
    pub selected: bool,
}

/// A read-only entry in the selected file's version history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryVersionRow {
    pub version: i64,
    pub label: String,
    pub selected: bool,
    pub current: bool,
}

/// What the "What the lead knows" card says (wireframe 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeadKnows {
    /// Whether the project has instructions.
    pub instructions: bool,
    /// Notes of the project and of the space, indexes left out.
    pub notes: usize,
    /// Task progress files.
    pub progress: usize,
}

impl LeadKnows {
    /// `Instructions for this project` or `No instructions for this
    /// project`, then `4 notes · 3 task progress files`.
    #[must_use]
    pub fn lines(&self) -> [String; 2] {
        let instructions = if self.instructions {
            "Instructions for this project"
        } else {
            "No instructions for this project"
        };
        let notes = count_form(self.notes, "note", "notes");
        let summary = if self.progress == 0 {
            format!("{} {notes}", self.notes)
        } else {
            let files = count_form(self.progress, "task progress file", "task progress files");
            format!("{} {notes} · {} {files}", self.notes, self.progress)
        };
        [instructions.to_owned(), summary]
    }
}

/// Whether you may change a file: every file but an index, which only the
/// hub writes.
#[must_use]
pub fn editable(path: &str) -> bool {
    path.rsplit('/').next() != Some(INDEX_FILE)
}

/// A file's title line: its first non-blank line without leading `#`, or
/// its file name when it has none (an index starts with a list item).
fn title_of(file: &MemoryFile) -> String {
    let name = || file.path.rsplit('/').next().unwrap_or_default().to_owned();
    if !editable(&file.path) {
        return name();
    }
    file.content
        .lines()
        .map(|line| line.trim().trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .map_or_else(name, str::to_owned)
}

/// The title of the group `path` belongs to.
fn group_of(path: &str) -> &'static str {
    GROUPS
        .iter()
        .find(|(prefix, _)| path == *prefix || (prefix.ends_with('/') && path.starts_with(prefix)))
        .map_or(OTHER, |(_, title)| title)
}

/// What the Memory tab knows of one project.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct MemoryView {
    files: Vec<MemoryFile>,
    answers: BTreeMap<crate::Part, Answer>,
    selected: Option<String>,
    /// The selected file's versions, oldest first.
    versions: Vec<MemoryFile>,
    inspected_version: Option<i64>,
    /// Whether the selected file is open for editing.
    editing: bool,
    /// Why your last change was refused.
    refused: Option<String>,
}

impl MemoryView {
    pub(crate) fn answers(&self) -> &BTreeMap<crate::Part, Answer> {
        &self.answers
    }
}

/// A project's memory listing, and the versions of the file selected. Made
/// by [`load_memory`], applied with [`AppState::apply_memory`].
#[derive(Debug)]
pub struct MemoryLoad {
    space: String,
    project: String,
    files: Result<Vec<MemoryFile>, ClientError>,
    versions: Option<(String, Result<Vec<MemoryFile>, ClientError>)>,
}

/// Asks the hub of `space` for the memory `project`'s lead can see, and
/// for the versions of `selected`.
pub async fn load_memory(
    core: &ClientCore,
    space: &str,
    project: &str,
    selected: Option<String>,
) -> MemoryLoad {
    let named = Some(project.to_owned());
    let files = match core
        .memory(
            Some(space),
            MemoryRequest::List {
                project: named.clone(),
            },
        )
        .await
    {
        Ok(MemoryReply::Files { files }) => Ok(files),
        Ok(other) => Err(unexpected(space, &other)),
        Err(error) => Err(error),
    };
    let mut versions = None;
    if let Some(path) = selected {
        let request = MemoryRequest::History {
            project: named,
            path: path.clone(),
        };
        let listed = match core.memory(Some(space), request).await {
            Ok(MemoryReply::History { versions }) => Ok(versions),
            Ok(other) => Err(unexpected(space, &other)),
            Err(error) => Err(error),
        };
        versions = Some((path, listed));
    }
    MemoryLoad {
        space: space.to_owned(),
        project: project.to_owned(),
        files,
        versions,
    }
}

/// A change you make to one file. Made by [`change_memory`], applied with
/// [`AppState::apply_change`].
#[derive(Debug)]
pub struct MemoryChange {
    space: String,
    project: String,
    path: String,
    changed: Result<MemoryFile, ClientError>,
}

/// Writes `content` at `path` as you, or deletes the file with `None`.
pub async fn change_memory(
    core: &ClientCore,
    space: &str,
    project: &str,
    path: &str,
    content: Option<String>,
) -> MemoryChange {
    let (project_name, path_name) = (Some(project.to_owned()), path.to_owned());
    let request = match content {
        Some(content) => MemoryRequest::Write {
            project: project_name,
            path: path_name,
            content,
        },
        None => MemoryRequest::Delete {
            project: project_name,
            path: path_name,
        },
    };
    let changed = match core.memory(Some(space), request).await {
        Ok(MemoryReply::File { file }) => Ok(file),
        Ok(other) => Err(unexpected(space, &other)),
        Err(error) => Err(error),
    };
    MemoryChange {
        space: space.to_owned(),
        project: project.to_owned(),
        path: path.to_owned(),
        changed,
    }
}

/// A reply of the wrong kind.
fn unexpected(space: &str, reply: &MemoryReply) -> ClientError {
    ClientError::Api {
        space: space.to_owned(),
        source: threadlane_core::client::ApiError::Internal(format!(
            "the hub answered with {reply:?}"
        )),
    }
}

impl AppState {
    fn memory_view(&self, space: &str, project: &str) -> Option<&MemoryView> {
        self.memory.get(&(space.to_owned(), project.to_owned()))
    }

    fn memory_view_mut(&mut self, space: &str, project: &str) -> &mut MemoryView {
        self.memory
            .entry((space.to_owned(), project.to_owned()))
            .or_default()
    }

    /// Takes a memory listing, and the versions of the file selected. A
    /// listing the hub refused shows its error, and nothing as current.
    pub fn apply_memory(&mut self, load: MemoryLoad) {
        let view = self.memory_view_mut(&load.space, &load.project);
        view.files = take(&mut view.answers, crate::Part::Memory, load.files);
        if let Some((path, listed)) = load.versions
            && view.selected.as_deref() == Some(path.as_str())
        {
            view.versions = take(&mut view.answers, crate::Part::MemoryVersions, listed);
            if !view
                .versions
                .iter()
                .any(|file| Some(file.version) == view.inspected_version)
            {
                view.inspected_version = None;
            }
        }
    }

    /// What to show in place of the memory of `project`, if anything.
    #[must_use]
    pub fn memory_gap(&self, space: &str, project: &str) -> Option<Gap> {
        self.memory_view(space, project)
            .map_or(Some(Gap::Loading), |view| {
                gap_in(&view.answers, crate::Part::Memory)
            })
    }

    /// The Memory tab's list: the files the lead can see, in their groups,
    /// empty groups left out. Once listed, both instructions files are
    /// there, `Not written yet` while they do not exist, so you can write
    /// them.
    #[must_use]
    pub fn memory_groups(&self, space: &str, project: &str) -> Vec<MemoryGroup> {
        let Some(view) = self.memory_view(space, project) else {
            return Vec::new();
        };
        let titles = GROUPS
            .iter()
            .map(|(_, title)| *title)
            .chain(std::iter::once(OTHER));
        titles
            .filter_map(|title| {
                let row = |path: &str, title: String| MemoryRow {
                    path: path.to_owned(),
                    title,
                    selected: view.selected.as_deref() == Some(path),
                };
                let mut files: Vec<MemoryRow> = view
                    .files
                    .iter()
                    .filter(|file| {
                        !threadlane_core::memory::is_skill(&file.path)
                            && group_of(&file.path) == title
                    })
                    .map(|file| row(&file.path, title_of(file)))
                    .collect();
                let unwritten = INSTRUCTIONS.iter().find(|path| {
                    group_of(path) == title && !view.files.iter().any(|file| file.path == **path)
                });
                if let Some(path) = unwritten
                    && gap_in(&view.answers, crate::Part::Memory).is_none()
                {
                    files.push(row(path, UNWRITTEN.to_owned()));
                }
                (!files.is_empty()).then_some(MemoryGroup { title, files })
            })
            .collect()
    }

    /// Shows `path` on the right, closing any edit of another file.
    pub fn select_memory(&mut self, space: &str, project: &str, path: &str) {
        let view = self.memory_view_mut(space, project);
        if view.selected.as_deref() != Some(path) {
            view.selected = Some(path.to_owned());
            view.versions.clear();
            view.inspected_version = None;
            view.answers.remove(&crate::Part::MemoryVersions);
            view.editing = false;
            view.refused = None;
        }
    }

    /// The path selected in the Memory tab of `project`.
    #[must_use]
    pub fn memory_selected(&self, space: &str, project: &str) -> Option<&str> {
        self.memory_view(space, project)?.selected.as_deref()
    }

    /// The current version of the selected file, if it is listed.
    #[must_use]
    pub fn memory_file(&self, space: &str, project: &str) -> Option<&MemoryFile> {
        let view = self.memory_view(space, project)?;
        let selected = view.selected.as_deref()?;
        view.files.iter().find(|file| file.path == selected)
    }

    /// Whether the hub has answered for the selected file's version history.
    #[must_use]
    pub fn memory_versions_gap(&self, space: &str, project: &str) -> Option<Gap> {
        let view = self.memory_view(space, project)?;
        gap_in(&view.answers, crate::Part::MemoryVersions)
    }

    /// Select the version named by a dream while its path's history loads.
    pub fn preview_dream_version(&mut self, space: &str, project: &str, version: i64) {
        self.memory_view_mut(space, project).inspected_version = Some(version);
    }

    /// Inspect only a version already loaded for this file. This never changes
    /// the current file, the editor, or the server.
    pub fn inspect_memory_version(&mut self, space: &str, project: &str, version: i64) {
        let view = self.memory_view_mut(space, project);
        view.inspected_version = view
            .versions
            .iter()
            .find(|file| {
                file.version == version && Some(file.path.as_str()) == view.selected.as_deref()
            })
            .map(|file| file.version);
    }

    /// Leave history inspection on navigation without discarding current edits.
    pub fn clear_memory_inspection(&mut self) {
        for view in self.memory.values_mut() {
            view.inspected_version = None;
        }
    }

    #[must_use]
    pub fn memory_inspected(&self, space: &str, project: &str) -> Option<&MemoryFile> {
        let view = self.memory_view(space, project)?;
        let version = view.inspected_version?;
        view.versions.iter().find(|file| {
            file.version == version && Some(file.path.as_str()) == view.selected.as_deref()
        })
    }

    #[must_use]
    pub fn memory_version_rows(
        &self,
        space: &str,
        project: &str,
        when: &When,
    ) -> Vec<MemoryVersionRow> {
        let Some(view) = self.memory_view(space, project) else {
            return Vec::new();
        };
        view.versions
            .iter()
            .rev()
            .zip(self.memory_versions(space, project, when))
            .map(|(file, label)| MemoryVersionRow {
                version: file.version,
                label,
                selected: view.inspected_version == Some(file.version),
                current: self
                    .memory_file(space, project)
                    .is_some_and(|current| current.version == file.version),
            })
            .collect()
    }

    /// The selected file's versions, newest first: `v3 · 09:20 · by you`,
    /// `v2 · Yesterday 18:02 · by run 41`, `v1 · ... · deleted by you`.
    #[must_use]
    pub fn memory_versions(&self, space: &str, project: &str, when: &When) -> Vec<String> {
        let Some(view) = self.memory_view(space, project) else {
            return Vec::new();
        };
        view.versions
            .iter()
            .rev()
            .map(|version| {
                let by = version.written_by_run.map_or_else(
                    || "by you".to_owned(),
                    |run| format!("by run {}", run.get()),
                );
                let what = if version.deleted { "deleted " } else { "" };
                format!(
                    "v{} · {} · {what}{by}",
                    version.version,
                    when.moment(version.written_at)
                )
            })
            .collect()
    }

    /// Whether you may edit the selected file: a current file that is not
    /// an index, or an instructions file not written yet.
    #[must_use]
    pub fn memory_can_edit(&self, space: &str, project: &str) -> bool {
        let Some(path) = self.memory_selected(space, project) else {
            return false;
        };
        editable(path)
            && (self.memory_file(space, project).is_some() || INSTRUCTIONS.contains(&path))
    }

    /// Whether the selected file is open for editing.
    #[must_use]
    pub fn memory_editing(&self, space: &str, project: &str) -> bool {
        self.memory_view(space, project)
            .is_some_and(|view| view.editing)
    }

    /// Opens the selected file for editing, or closes it, unless it is an
    /// index. Returns the text to edit when it opens.
    pub fn edit_memory(&mut self, space: &str, project: &str, open: bool) -> Option<String> {
        let content = self.memory_can_edit(space, project).then(|| {
            self.memory_file(space, project)
                .map(|file| file.content.clone())
                .unwrap_or_default()
        });
        let view = self.memory_view_mut(space, project);
        view.refused = None;
        view.editing = open && content.is_some();
        content.filter(|_| open)
    }

    /// Why your last change to the selected file was refused: the store's
    /// message, which says what to change.
    #[must_use]
    pub fn memory_refused(&self, space: &str, project: &str) -> Option<&str> {
        self.memory_view(space, project)?.refused.as_deref()
    }

    /// Takes the outcome of your change. A refusal keeps the file open for
    /// editing with the store's reason; a change shows its new version at
    /// once, and the error otherwise. Returns what the status area says.
    pub fn apply_change(&mut self, change: MemoryChange) -> Result<String, String> {
        let MemoryChange {
            space,
            project,
            path,
            changed,
        } = change;
        let view = self.memory_view_mut(&space, &project);
        match changed {
            Ok(file) => {
                view.editing = false;
                view.refused = None;
                view.files.retain(|known| known.path != file.path);
                let done = if file.deleted {
                    format!("Deleted {path}.")
                } else {
                    format!("Saved {path} as version {}.", file.version)
                };
                if !file.deleted {
                    let at = view.files.partition_point(|known| known.path < file.path);
                    view.files.insert(at, file.clone());
                }
                if view.selected.as_deref() == Some(path.as_str()) {
                    view.versions.push(file);
                }
                Ok(done)
            }
            Err(error) => {
                let why = describe(&error);
                view.refused = Some(why.clone());
                Err(why)
            }
        }
    }

    /// What the "What the lead knows" card says, once the memory is listed.
    #[must_use]
    pub fn lead_knows(&self, space: &str, project: &str) -> Option<LeadKnows> {
        if self.memory_gap(space, project).is_some() {
            return None;
        }
        let files = &self.memory_view(space, project)?.files;
        let count = |group: &str| {
            files
                .iter()
                .filter(|file| group_of(&file.path) == group && editable(&file.path))
                .count()
        };
        Some(LeadKnows {
            instructions: files
                .iter()
                .any(|file| file.path == "project/instructions.md"),
            notes: count("Space notes") + count("Project notes"),
            progress: count("Task progress files"),
        })
    }
}

#[cfg(test)]
mod history_inspection_tests {
    use super::*;
    use threadlane_core::{
        ids::{MemoryFileId, RunId},
        time::Timestamp,
    };

    const PATH: &str = "project/notes/example.md";

    fn file(version: i64, deleted: bool) -> MemoryFile {
        MemoryFile {
            id: MemoryFileId(version),
            path: PATH.to_owned(),
            content: format!("Synthetic version {version}"),
            version,
            written_by_run: (version == 1).then_some(RunId(7)),
            deleted,
            written_at: Timestamp::from_millis(version * 60_000),
        }
    }

    fn load(space: &str, project: &str, versions: Vec<MemoryFile>) -> MemoryLoad {
        MemoryLoad {
            space: space.to_owned(),
            project: project.to_owned(),
            files: Ok(versions
                .last()
                .filter(|file| !file.deleted)
                .cloned()
                .into_iter()
                .collect()),
            versions: Some((PATH.to_owned(), Ok(versions))),
        }
    }

    #[test]
    fn historical_text_and_provenance_do_not_change_current_editing() {
        let mut state = AppState::default();
        state.select_memory("work", "api", PATH);
        state.apply_memory(load("work", "api", vec![file(1, false), file(2, false)]));
        let draft = state.edit_memory("work", "api", true);
        state.inspect_memory_version("work", "api", 1);
        assert_eq!(
            state.memory_inspected("work", "api").unwrap(),
            &file(1, false)
        );
        assert_eq!(state.memory_file("work", "api").unwrap(), &file(2, false));
        assert!(state.memory_editing("work", "api"));
        assert_eq!(draft.as_deref(), Some("Synthetic version 2"));
        let when = When {
            now: Timestamp::from_millis(120_000),
            offset_minutes: 0,
        };
        let rows = state.memory_version_rows("work", "api", &when);
        assert_eq!(
            rows.iter().map(|row| row.version).collect::<Vec<_>>(),
            [2, 1]
        );
        assert!(rows[0].current && !rows[0].selected);
        assert!(rows[0].label.ends_with("by you"));
        assert!(rows[1].selected && !rows[1].current);
        assert!(rows[1].label.ends_with("by run 7"));
        state.clear_memory_inspection();
        assert!(state.memory_inspected("work", "api").is_none());
        assert!(state.memory_editing("work", "api"));
    }

    #[test]
    fn dream_change_opens_a_deleted_version_after_history_loads() {
        let mut state = AppState::default();
        state.select_memory("work", "api", PATH);
        state.preview_dream_version("work", "api", 2);
        state.apply_memory(load("work", "api", vec![file(1, false), file(2, true)]));
        assert!(state.memory_inspected("work", "api").unwrap().deleted);
        state.inspect_memory_version("work", "api", 1);
        assert_eq!(
            state.memory_inspected("work", "api").unwrap(),
            &file(1, false)
        );
    }

    #[test]
    fn history_selection_is_scoped_and_cleared_when_unavailable() {
        let mut state = AppState::default();
        for (space, project) in [("work", "api"), ("home", "api"), ("work", "web")] {
            state.select_memory(space, project, PATH);
            state.apply_memory(load(space, project, vec![file(1, false), file(2, true)]));
        }
        state.inspect_memory_version("work", "api", 2);
        assert!(state.memory_inspected("work", "api").unwrap().deleted);
        assert!(state.memory_file("work", "api").is_none());
        assert!(state.memory_inspected("home", "api").is_none());
        assert!(state.memory_inspected("work", "web").is_none());
        state.inspect_memory_version("work", "api", 99);
        assert!(state.memory_inspected("work", "api").is_none());
        state.inspect_memory_version("work", "api", 1);
        state.apply_memory(load("work", "api", vec![file(2, true)]));
        assert!(state.memory_inspected("work", "api").is_none());
        state.inspect_memory_version("work", "api", 2);
        state.select_memory("work", "api", "project/instructions.md");
        assert!(state.memory_inspected("work", "api").is_none());
        // A late response for the old path cannot restore its preview.
        state.apply_memory(load("work", "api", vec![file(1, false)]));
        assert!(
            state
                .memory_version_rows(
                    "work",
                    "api",
                    &When {
                        now: Timestamp::from_millis(0),
                        offset_minutes: 0,
                    }
                )
                .is_empty()
        );
    }
}
