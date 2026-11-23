//! Space-wide composable skill playbooks (`space/skills/<slug>.md`):
//! listing, starter templates, versioned editing/rollback, and composer
//! `/skill:<slug>` mention helpers.

use std::collections::BTreeMap;

use threadlane_client::{ClientCore, ClientError};
use threadlane_core::client::{
    ApiError, MemoryReply, MemoryRequest, OperationsReply, OperationsRequest,
};
use threadlane_core::memory::{is_valid_skill_slug, parse_skill_mentions, skill_path, skill_slug};
use threadlane_core::model::{Agent, MemoryFile};
use threadlane_core::time::Timestamp;

use crate::{Answer, AppState, Gap, MemoryVersionRow, Part, TeamViewMode, When, describe, gap_in};

/// One space-wide composable skill playbook loaded from `space/skills/<slug>.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceSkill {
    pub slug: String,
    pub path: String,
    pub title: String,
    pub content: String,
    pub version: i64,
    pub written_at: Timestamp,
}

/// A starter template offered when a space has no skill playbooks yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillStarter {
    pub slug: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub content: &'static str,
}

/// Three ready-to-use playbooks for bootstrapping a space's skills library.
pub const SKILL_STARTERS: [SkillStarter; 3] = [
    SkillStarter {
        slug: "rust-pr-review",
        title: "Rust PR Review",
        summary: "Correctness, error paths, concurrency, tests, and clippy hygiene.",
        content: "# Rust PR Review\n\n\
Check every changed Rust module before approving:\n\
- Verify error paths return explicit context instead of panicking (`unwrap` / `expect` only in tests).\n\
- Confirm lock scopes and async `.await` points cannot deadlock or hold locks across I/O.\n\
- Ensure shared helpers replace duplicated logic and new public items have concise docstrings.\n\
- Run `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --check`.\n",
    },
    SkillStarter {
        slug: "accessibility-audit",
        title: "Accessibility Audit",
        summary: "Labels, keyboard navigation, contrast, and semantic states.",
        content: "# Accessibility Audit\n\n\
Verify UI changes against desktop accessibility standards:\n\
- Every icon-only or interactive control has a clear `accessibility_label` and tooltip.\n\
- Keyboard navigation works end-to-end with visible focus rings and Escape dismissal on overlays.\n\
- Color never acts as the sole status indicator; pair dots or tints with labels or icons.\n\
- Confirm text and interactive targets keep readable contrast in both light and dark themes.\n",
    },
    SkillStarter {
        slug: "release-notes",
        title: "Release Notes",
        summary: "Concise user-facing changelog grouped by features and fixes.",
        content: "# Release Notes\n\n\
Summarize merged changes for users:\n\
- Group items under **Highlights**, **Improvements**, and **Fixes**.\n\
- Lead each bullet with the user-visible outcome, omitting internal refactors unless behavioral.\n\
- Link relevant PR numbers and call out any migration or configuration steps.\n",
    },
];

/// Extract a human-readable skill title from the first `# Heading` line,
/// falling back to the slug.
#[must_use]
pub fn skill_title(slug: &str, content: &str) -> String {
    content
        .lines()
        .map(|line| line.trim().trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .filter(|line| !line.is_empty())
        .map_or_else(|| slug.to_owned(), str::to_owned)
}

fn to_space_skill(file: MemoryFile) -> Option<SpaceSkill> {
    if file.deleted {
        return None;
    }
    let slug = skill_slug(&file.path)?.to_owned();
    let title = skill_title(&slug, &file.content);
    Some(SpaceSkill {
        slug,
        path: file.path,
        title,
        content: file.content,
        version: file.version,
        written_at: file.written_at,
    })
}

/// Loaded skills list and optional version history for one space.
#[derive(Debug)]
pub struct SkillsLoad {
    pub space: String,
    pub files: Result<Vec<MemoryFile>, ClientError>,
    pub versions: Option<(String, Result<Vec<MemoryFile>, ClientError>)>,
}

/// Result of saving, deleting, or rolling back one skill playbook.
#[derive(Debug)]
pub struct SkillChange {
    pub space: String,
    pub slug: String,
    pub result: Result<MemoryFile, ClientError>,
}

/// Loads all space skills for `space` and, when given, the version history of `selected_slug`.
pub async fn load_skills(
    core: &ClientCore,
    space: &str,
    selected_slug: Option<String>,
) -> SkillsLoad {
    let files = match core
        .memory(Some(space), MemoryRequest::List { project: None })
        .await
    {
        Ok(MemoryReply::Files { files }) => Ok(files),
        Ok(other) => Err(unexpected(space, &other)),
        Err(error) => Err(error),
    };
    let versions = if let Some(slug) = selected_slug.filter(|s| is_valid_skill_slug(s)) {
        let path = skill_path(&slug);
        let history = match core
            .memory(
                Some(space),
                MemoryRequest::History {
                    project: None,
                    path,
                },
            )
            .await
        {
            Ok(MemoryReply::History { versions }) => Ok(versions),
            Ok(other) => Err(unexpected(space, &other)),
            Err(error) => Err(error),
        };
        Some((slug, history))
    } else {
        None
    };
    SkillsLoad {
        space: space.to_owned(),
        files,
        versions,
    }
}

/// Saves `content` to `space/skills/<slug>.md` as owner-authored memory.
pub async fn save_skill(
    core: &ClientCore,
    space: &str,
    slug: &str,
    content: String,
) -> SkillChange {
    let result = match core
        .memory(
            Some(space),
            MemoryRequest::Write {
                project: None,
                path: skill_path(slug),
                content,
            },
        )
        .await
    {
        Ok(MemoryReply::File { file }) => Ok(file),
        Ok(other) => Err(unexpected(space, &other)),
        Err(error) => Err(error),
    };
    SkillChange {
        space: space.to_owned(),
        slug: slug.to_owned(),
        result,
    }
}

/// Deletes `space/skills/<slug>.md` as owner-authored memory (preserving history).
pub async fn delete_skill(core: &ClientCore, space: &str, slug: &str) -> SkillChange {
    let result = match core
        .memory(
            Some(space),
            MemoryRequest::Delete {
                project: None,
                path: skill_path(slug),
            },
        )
        .await
    {
        Ok(MemoryReply::File { file }) => Ok(file),
        Ok(other) => Err(unexpected(space, &other)),
        Err(error) => Err(error),
    };
    SkillChange {
        space: space.to_owned(),
        slug: slug.to_owned(),
        result,
    }
}

/// Restores `version` of `space/skills/<slug>.md` as a new owner-authored version.
pub async fn restore_skill_version(
    core: &ClientCore,
    space: &str,
    slug: &str,
    version: i64,
) -> SkillChange {
    let result = match core
        .operations(
            Some(space),
            OperationsRequest::Rollback {
                project: None,
                path: skill_path(slug),
                version,
            },
        )
        .await
    {
        Ok(OperationsReply::RolledBack { file }) => Ok(file),
        Ok(other) => Err(ClientError::Api {
            space: space.to_owned(),
            source: ApiError::Invalid(format!("unexpected reply from {space}: {other:?}")),
        }),
        Err(error) => Err(error),
    };
    SkillChange {
        space: space.to_owned(),
        slug: slug.to_owned(),
        result,
    }
}

fn unexpected(space: &str, reply: &MemoryReply) -> ClientError {
    ClientError::Api {
        space: space.to_owned(),
        source: ApiError::Invalid(format!("unexpected memory reply from {space}: {reply:?}")),
    }
}

/// Adds `/skill:<slug>` to `text` if not already mentioned.
#[must_use]
pub fn append_skill_mention(text: &str, slug: &str) -> String {
    if parse_skill_mentions(text).iter().any(|known| known == slug) {
        return text.to_owned();
    }
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        format!("/skill:{slug} ")
    } else {
        format!("{trimmed} /skill:{slug}")
    }
}

/// Removes `/skill:<slug>` mentions from `text` while preserving surrounding prose and newlines.
#[must_use]
pub fn remove_skill_mention(text: &str, slug: &str) -> String {
    let needle = format!("/skill:{slug}");
    let mut lines = Vec::new();
    let is_mention = |token: &str| {
        token
            .trim_start_matches(['(', '[', '{', '"', '\''])
            .trim_end_matches([')', ']', '}', '"', '\'', ',', ';', '.', ':', '!', '?'])
            .eq_ignore_ascii_case(&needle)
    };
    for line in text.lines() {
        // Lines without the mention keep their spacing, so code blocks stay indented.
        if !line.split_whitespace().any(is_mention) {
            lines.push(line.to_owned());
            continue;
        }
        let kept: Vec<&str> = line.split_whitespace().filter(|t| !is_mention(t)).collect();
        lines.push(kept.join(" "));
    }
    let mut joined = lines.join("\n");
    if text.ends_with('\n') && !joined.is_empty() {
        joined.push('\n');
    }
    joined.trim().to_owned()
}

/// Toggles `/skill:<slug>` in `text`.
#[must_use]
pub fn toggle_skill_mention(text: &str, slug: &str) -> String {
    if parse_skill_mentions(text).iter().any(|known| known == slug) {
        remove_skill_mention(text, slug)
    } else {
        append_skill_mention(text, slug)
    }
}

/// Per-space Skills UI state.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SkillsView {
    skills: Vec<SpaceSkill>,
    answers: BTreeMap<Part, Answer>,
    selected: Option<String>,
    creating: bool,
    preview: bool,
    saving: bool,
    error: Option<String>,
    versions: Vec<MemoryFile>,
    inspected_version: Option<i64>,
}

impl AppState {
    fn skills_view(&self, space: &str) -> Option<&SkillsView> {
        self.skills.get(space)
    }

    fn skills_view_mut(&mut self, space: &str) -> &mut SkillsView {
        self.skills.entry(space.to_owned()).or_default()
    }

    /// The space currently active on the unscoped `Team -> Skills` tab.
    #[must_use]
    pub fn skills_space(&self) -> Option<&str> {
        self.skills_space
            .as_deref()
            .filter(|name| self.space(name).is_some())
            .or_else(|| self.selected_space().map(|space| space.name.as_str()))
    }

    /// Selects which space `Team -> Skills` shows when unscoped.
    pub fn set_skills_space(&mut self, space: &str) {
        self.skills_space = Some(space.to_owned());
    }

    /// Cached space skill playbooks for `space`, sorted by slug.
    #[must_use]
    pub fn space_skills(&self, space: &str) -> &[SpaceSkill] {
        self.skills_view(space)
            .map_or(&[], |view| view.skills.as_slice())
    }

    /// Loading or fetch error state for `space`'s skills library.
    #[must_use]
    pub fn skills_gap(&self, space: &str) -> Option<Gap> {
        let answers = self.skills_view(space).map(|view| &view.answers)?;
        gap_in(answers, Part::Memory)
    }

    /// Whether `space`'s skills library is loading or has not yet been requested.
    #[must_use]
    pub fn skills_loading(&self, space: &str) -> bool {
        self.skills_view(space)
            .is_none_or(|view| gap_in(&view.answers, Part::Memory) == Some(Gap::Loading))
    }

    /// Currently selected skill in `space`'s Skills library, unless creating a new one.
    #[must_use]
    pub fn skill_selected(&self, space: &str) -> Option<&SpaceSkill> {
        let view = self.skills_view(space)?;
        if view.creating {
            return None;
        }
        let slug = view.selected.as_deref()?;
        view.skills.iter().find(|skill| skill.slug == slug)
    }

    /// Selected skill slug in `space`, if any.
    #[must_use]
    pub fn skill_selected_slug(&self, space: &str) -> Option<&str> {
        let view = self.skills_view(space)?;
        (!view.creating)
            .then_some(view.selected.as_deref())
            .flatten()
    }

    #[must_use]
    pub fn skill_creating(&self, space: &str) -> bool {
        self.skills_view(space).is_some_and(|view| view.creating)
    }

    #[must_use]
    pub fn skill_preview(&self, space: &str) -> bool {
        self.skills_view(space).is_some_and(|view| view.preview)
    }

    #[must_use]
    pub fn skill_saving(&self, space: &str) -> bool {
        self.skills_view(space).is_some_and(|view| view.saving)
    }

    #[must_use]
    pub fn skill_error(&self, space: &str) -> Option<&str> {
        self.skills_view(space)?.error.as_deref()
    }

    #[must_use]
    pub fn skill_versions_gap(&self, space: &str) -> Option<Gap> {
        let view = self.skills_view(space)?;
        view.selected.as_ref()?;
        gap_in(&view.answers, Part::MemoryVersions)
    }

    /// Version rows for the selected skill, newest first.
    #[must_use]
    pub fn skill_version_rows(&self, space: &str, when: &When) -> Vec<MemoryVersionRow> {
        let Some(view) = self.skills_view(space) else {
            return Vec::new();
        };
        let current_version = self.skill_selected(space).map(|skill| skill.version);
        view.versions
            .iter()
            .rev()
            .map(|version| {
                let by = version.written_by_run.map_or_else(
                    || "by you".to_owned(),
                    |run| format!("by run {}", run.get()),
                );
                let what = if version.deleted { "deleted " } else { "" };
                MemoryVersionRow {
                    version: version.version,
                    label: format!(
                        "v{} · {} · {what}{by}",
                        version.version,
                        when.moment(version.written_at)
                    ),
                    selected: view.inspected_version == Some(version.version),
                    current: current_version == Some(version.version),
                }
            })
            .collect()
    }

    #[must_use]
    pub fn skill_inspected(&self, space: &str) -> Option<&MemoryFile> {
        let view = self.skills_view(space)?;
        let version = view.inspected_version?;
        let slug = view.selected.as_deref()?;
        let path = skill_path(slug);
        view.versions
            .iter()
            .find(|file| file.version == version && file.path == path)
    }

    /// Agents in `space` that currently attach `slug`.
    #[must_use]
    pub fn skill_used_by(&self, space: &str, slug: &str) -> Vec<&Agent> {
        self.space(space)
            .map(|known| {
                known
                    .agents
                    .iter()
                    .filter(|agent| agent.playbooks.iter().any(|s| s == slug))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Begins creating a new skill in `space`.
    pub fn start_new_skill(&mut self, space: &str) {
        let view = self.skills_view_mut(space);
        view.creating = true;
        view.preview = false;
        view.error = None;
        view.versions.clear();
        view.inspected_version = None;
    }

    /// Cancels new-skill creation and falls back to the first existing skill if any.
    pub fn cancel_new_skill(&mut self, space: &str) {
        let view = self.skills_view_mut(space);
        view.creating = false;
        view.error = None;
        if view.selected.is_none() {
            view.selected = view.skills.first().map(|skill| skill.slug.clone());
        }
    }

    /// Selects `slug` in `space`'s Skills editor, returning its content if cached.
    pub fn select_skill(&mut self, space: &str, slug: &str) -> Option<String> {
        let view = self.skills_view_mut(space);
        let changed = view.creating || view.selected.as_deref() != Some(slug);
        view.creating = false;
        view.selected = Some(slug.to_owned());
        view.error = None;
        if changed {
            view.preview = false;
            view.versions.clear();
            view.inspected_version = None;
            view.answers.remove(&Part::MemoryVersions);
        }
        view.skills
            .iter()
            .find(|skill| skill.slug == slug)
            .map(|skill| skill.content.clone())
    }

    /// Switches to `Team -> Skills` focused on `space` and `slug`.
    pub fn open_skill_view(&mut self, space: &str, slug: &str) -> Option<String> {
        self.agent_edit = None;
        self.team_view_mode = TeamViewMode::Skills;
        self.skills_space = Some(space.to_owned());
        self.select_skill(space, slug)
    }

    pub fn set_skill_preview(&mut self, space: &str, preview: bool) {
        self.skills_view_mut(space).preview = preview;
    }

    pub fn inspect_skill_version(&mut self, space: &str, version: Option<i64>) {
        self.skills_view_mut(space).inspected_version = version;
    }

    pub fn begin_skill_save(&mut self, space: &str) -> bool {
        let view = self.skills_view_mut(space);
        if view.saving {
            return false;
        }
        view.saving = true;
        view.error = None;
        true
    }

    pub fn fail_skill_validation(&mut self, space: &str, error: String) {
        let view = self.skills_view_mut(space);
        view.saving = false;
        view.error = Some(error);
    }

    /// Applies the loaded skill list and optional version history for `space`.
    /// Returns the content of the newly auto-selected skill when selection changed.
    pub fn apply_skills(&mut self, load: SkillsLoad) -> Option<String> {
        let SkillsLoad {
            space,
            files,
            versions,
        } = load;
        let view = self.skills_view_mut(&space);
        let previous = view.selected.clone();
        match files {
            Ok(files) => {
                view.answers.insert(Part::Memory, Answer::Answered);
                let mut parsed: Vec<SpaceSkill> =
                    files.into_iter().filter_map(to_space_skill).collect();
                parsed.sort_by(|a, b| a.slug.cmp(&b.slug));
                view.skills = parsed;
                if let Some(selected) = &view.selected
                    && !view.skills.iter().any(|skill| &skill.slug == selected)
                {
                    view.selected = None;
                }
                if !view.creating && view.selected.is_none() {
                    view.selected = view.skills.first().map(|skill| skill.slug.clone());
                }
            }
            Err(error) => {
                view.answers
                    .insert(Part::Memory, Answer::Failed(describe(&error)));
            }
        }
        if let Some((slug, listed)) = versions
            && view.selected.as_deref() == Some(slug.as_str())
        {
            match listed {
                Ok(history) => {
                    view.answers.insert(Part::MemoryVersions, Answer::Answered);
                    view.versions = history;
                }
                Err(error) => {
                    view.answers
                        .insert(Part::MemoryVersions, Answer::Failed(describe(&error)));
                }
            }
        }
        if !view.creating && view.selected != previous {
            view.selected.as_ref().and_then(|slug| {
                view.skills
                    .iter()
                    .find(|skill| &skill.slug == slug)
                    .map(|skill| skill.content.clone())
            })
        } else {
            None
        }
    }

    /// Applies the outcome of saving, deleting, or rolling back a skill.
    pub fn apply_skill_change(
        &mut self,
        change: SkillChange,
    ) -> Result<(String, Option<String>), String> {
        let SkillChange {
            space,
            slug,
            result,
        } = change;
        let view = self.skills_view_mut(&space);
        view.saving = false;
        match result {
            Ok(file) => {
                view.error = None;
                view.creating = false;
                view.inspected_version = None;
                let note = if file.deleted {
                    view.skills.retain(|skill| skill.slug != slug);
                    if view.selected.as_deref() == Some(slug.as_str()) {
                        view.selected = view.skills.first().map(|skill| skill.slug.clone());
                        view.versions.clear();
                    }
                    format!("Deleted skill /skill:{slug}.")
                } else if let Some(skill) = to_space_skill(file.clone()) {
                    let version = skill.version;
                    if let Some(existing) = view.skills.iter_mut().find(|s| s.slug == slug) {
                        *existing = skill;
                    } else {
                        view.skills.push(skill);
                        view.skills.sort_by(|a, b| a.slug.cmp(&b.slug));
                    }
                    view.selected = Some(slug.clone());
                    if !view.versions.iter().any(|v| v.version == file.version) {
                        view.versions.push(file);
                    }
                    format!("Saved /skill:{slug} (v{version}).")
                } else {
                    format!("Updated /skill:{slug}.")
                };
                let next_content = view.selected.as_ref().and_then(|sel| {
                    view.skills
                        .iter()
                        .find(|s| &s.slug == sel)
                        .map(|s| s.content.clone())
                });
                Ok((note, next_content))
            }
            Err(error) => {
                let why = describe(&error);
                view.error = Some(why.clone());
                Err(why)
            }
        }
    }
}
