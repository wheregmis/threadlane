//! Sessions panel state: projects, session discovery, file operations, and active selection.

use crate::path_utils::{canonicalize_path, truncate_chars};
use threadlane_agent::{AgentMessage, SessionTree};

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct SessionEntry {
    pub id: String,
    pub title: String,
    pub work_dir: PathBuf,
    pub session_file: PathBuf,
    pub updated_at: u64,
}

#[derive(Clone, Debug)]
pub struct ProjectGroup {
    pub name: String,
    pub work_dir: PathBuf,
    pub sessions: Vec<SessionEntry>,
    pub available: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum SessionListRow {
    ProjectHeader {
        project_idx: usize,
    },
    Session {
        project_idx: usize,
        session_idx: usize,
    },
    EmptyProject,
}

pub struct SessionsData {
    pub projects: Vec<ProjectGroup>,
    /// O(1) lookup for spinner visibility per row in draw_walk.
    pub working_sessions: HashSet<(PathBuf, String)>,
    pub active_session_id: Option<String>,
    pub active_work_dir: PathBuf,
    pub context_session_id: Option<String>,
    pub context_work_dir: PathBuf,
    pub rows: Vec<SessionListRow>,
}

impl SessionsData {
    pub fn is_active(&self, work_dir: &Path, session_id: &str) -> bool {
        self.active_session_id.as_deref() == Some(session_id) && self.active_work_dir == work_dir
    }

    pub fn is_context_target(&self, work_dir: &Path, session_id: &str) -> bool {
        self.context_session_id.as_deref() == Some(session_id) && self.context_work_dir == work_dir
    }
}

pub static TITLE_ATTEMPTED: LazyLock<RwLock<std::collections::HashSet<(PathBuf, String)>>> =
    LazyLock::new(|| RwLock::new(std::collections::HashSet::new()));

pub fn normalize_session_title(value: &str) -> String {
    let mut title = value.trim().to_string();
    // Providers vary in whether they put the quote pair around the whole
    // response or put a `Title:` prefix inside it. Peel either wrapper in
    // either order until neither is present.
    loop {
        let before = title.clone();
        if title.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("title:")) {
            title = title[6..].trim().to_string();
        }
        let is_double_quoted = title.starts_with('"') && title.ends_with('"') && title.len() >= 2;
        let is_single_quoted =
            title.starts_with('\'') && title.ends_with('\'') && title.len() >= 2;
        if is_double_quoted || is_single_quoted {
            // Safe: both delimiters are ASCII (1 byte each).
            title = title[1..title.len() - 1].trim().to_string();
        }
        if title == before {
            break;
        }
    }
    // Collapse runs of whitespace without allocating a Vec.
    let mut collapsed = String::with_capacity(title.len());
    let mut prev_space = true;
    for ch in title.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                collapsed.push(' ');
                prev_space = true;
            }
        } else {
            collapsed.push(ch);
            prev_space = false;
        }
    }
    if collapsed.ends_with(' ') {
        collapsed.pop();
    }
    collapsed.chars().take(42).collect()
}

pub fn session_title_eligible(tree: &SessionTree, submitted_prompt: Option<&str>) -> bool {
    !tree.has_name()
        && (first_existing_user_prompt(tree).is_some()
            || submitted_prompt.is_some_and(|prompt| !prompt.trim().is_empty()))
}

/// Select the first persisted prompt for legacy sessions, or the prompt being
/// submitted when this is the first turn of a fresh session.
pub fn title_prompt_for_submission(
    tree: &SessionTree,
    submitted_prompt: Option<&str>,
) -> Option<String> {
    first_existing_user_prompt(tree).or_else(|| {
        submitted_prompt
            .filter(|prompt| !prompt.trim().is_empty())
            .map(str::to_owned)
    })
}

pub fn first_existing_user_prompt(tree: &SessionTree) -> Option<String> {
    // Metadata-bearing sessions are current sessions and retain active-branch
    // semantics. Legacy node-only files have no durable branch marker, so use
    // their deterministic persisted insertion order across every branch.
    let messages = if tree.metadata_present || tree.file_path.is_none() {
        tree.get_active_branch_messages()
    } else {
        tree.get_persisted_messages()
    };
    messages.into_iter().find_map(|message| {
        if let AgentMessage::User { content } = message {
            (!content.trim().is_empty()).then_some(content)
        } else if let AgentMessage::UserWithImages { content, .. } = message {
            (!content.trim().is_empty()).then_some(content)
        } else {
            None
        }
    })
}

pub fn begin_title_generation(work_dir: &Path, session_id: &str) -> bool {
    let key = (
        canonicalize_path(work_dir),
        session_id.to_string(),
    );
    // This is deliberately a lifetime attempt marker, not an in-flight guard.
    // Failed requests must not become eligible again later in this process.
    TITLE_ATTEMPTED.write().unwrap().insert(key)
}
pub fn end_title_generation(_work_dir: &Path, _session_id: &str) {
    // Kept as a no-op for callers that finish the detached task. Never clear
    // the marker: one attempt is allowed per session per application lifetime.
}

pub static SESSIONS_DATA: LazyLock<RwLock<SessionsData>> = LazyLock::new(|| {
    RwLock::new(SessionsData {
        projects: Vec::new(),
        working_sessions: HashSet::new(),
        active_session_id: None,
        active_work_dir: PathBuf::new(),
        context_session_id: None,
        context_work_dir: PathBuf::new(),
        rows: Vec::new(),
    })
});

fn project_display_name(work_dir: &Path) -> String {
    work_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| work_dir.display().to_string())
}

fn session_title_from_tree(tree: &SessionTree, fallback_id: &str) -> String {
    if let Some(name) = tree.name.as_ref().filter(|n| !n.trim().is_empty()) {
        return name.clone();
    }
    for msg in tree.get_active_branch_messages() {
        match msg {
            AgentMessage::User { content } => {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    return truncate_chars(trimmed, 42);
                }
            }
            AgentMessage::UserWithImages { content, images } => {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    return truncate_chars(trimmed, 42);
                }
                if let Some(image) = images.first() {
                    return truncate_chars(&format!("Image: {}", image.display_name), 42);
                }
            }
            _ => {}
        }
    }
    if fallback_id.starts_with("session_") {
        "Untitled session".to_string()
    } else {
        fallback_id.to_string()
    }
}

fn session_updated_at(tree: &SessionTree, path: &Path) -> u64 {
    let from_nodes = tree.nodes.values().map(|n| n.timestamp).max().unwrap_or(0);
    if from_nodes > 0 {
        return from_nodes;
    }
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn discover_sessions_in_project(work_dir: &Path) -> Vec<SessionEntry> {
    let sessions_dir = work_dir.join(".threadlane/sessions");
    let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
        return Vec::new();
    };

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".into());
        let tree = SessionTree::load_from_file(&path).unwrap_or_else(|_| {
            let mut t = SessionTree::new(id.clone());
            t.file_path = Some(path.clone());
            t
        });
        sessions.push(SessionEntry {
            id: id.clone(),
            title: session_title_from_tree(&tree, &id),
            // Store the canonical path once so draw_walk never needs a syscall.
            work_dir: canonicalize_path(work_dir),
            session_file: path.clone(),
            updated_at: session_updated_at(&tree, &path),
        });
    }
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.title.cmp(&b.title)));
    sessions
}

fn rebuild_session_rows(projects: &[ProjectGroup]) -> Vec<SessionListRow> {
    let mut rows = Vec::new();
    for (project_idx, project) in projects.iter().enumerate() {
        rows.push(SessionListRow::ProjectHeader { project_idx });
        if project.sessions.is_empty() {
            rows.push(SessionListRow::EmptyProject);
        } else {
            for session_idx in 0..project.sessions.len() {
                rows.push(SessionListRow::Session {
                    project_idx,
                    session_idx,
                });
            }
        }
    }
    rows
}

pub fn refresh_sessions(project_dirs: &[PathBuf]) -> Vec<SessionListRow> {
    let mut projects = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for raw_dir in project_dirs {
        let dir = canonicalize_path(raw_dir);
        if !seen.insert(dir.clone()) {
            continue;
        }
        projects.push(ProjectGroup {
            name: project_display_name(&dir),
            sessions: discover_sessions_in_project(&dir),
            available: dir.is_dir(),
            work_dir: dir,
        });
    }

    let rows = rebuild_session_rows(&projects);
    let mut data = SESSIONS_DATA.write().unwrap();
    let prev_id = data.active_session_id.clone();
    let prev_dir = data.active_work_dir.clone();
    let project_still_attached = projects.iter().any(|p| p.work_dir == prev_dir);
    let session_still_exists = prev_id.as_ref().is_some_and(|id| {
        projects
            .iter()
            .find(|p| p.work_dir == prev_dir)
            .is_some_and(|p| p.sessions.iter().any(|session| &session.id == id))
    });

    if !project_still_attached {
        data.active_work_dir = projects
            .first()
            .map(|project| project.work_dir.clone())
            .unwrap_or_default();
        data.active_session_id = None;
    } else if prev_id.is_some() && !session_still_exists {
        data.active_session_id = None;
    }

    data.projects = projects;
    data.rows = rows;
    data.rows.clone()
}

pub fn set_session_working(work_dir: &Path, session_id: &str, is_working: bool) {
    let mut data = SESSIONS_DATA.write().unwrap();
    let normalized_dir = canonicalize_path(work_dir);
    let key = (normalized_dir, session_id.to_string());
    if is_working {
        data.working_sessions.insert(key);
    } else {
        data.working_sessions.remove(&key);
    }
}

pub fn is_session_working(work_dir: &Path, session_id: &str) -> bool {
    let normalized_dir = canonicalize_path(work_dir);
    SESSIONS_DATA
        .read()
        .unwrap()
        .working_sessions
        .contains(&(normalized_dir, session_id.to_string()))
}

pub fn set_session_context_target(entry: Option<&SessionEntry>) {
    let mut data = SESSIONS_DATA.write().unwrap();
    if let Some(entry) = entry {
        data.context_session_id = Some(entry.id.clone());
        data.context_work_dir = entry.work_dir.clone();
    } else {
        data.context_session_id = None;
        data.context_work_dir.clear();
    }
}

pub fn set_active_session(work_dir: &Path, session_id: &str) {
    let mut data = SESSIONS_DATA.write().unwrap();
    data.active_work_dir = canonicalize_path(work_dir);
    data.active_session_id = Some(session_id.to_string());
}

pub fn set_active_project(work_dir: &Path) {
    let mut data = SESSIONS_DATA.write().unwrap();
    data.active_work_dir = canonicalize_path(work_dir);
    data.active_session_id = None;
}

pub fn is_project_working(work_dir: &Path) -> bool {
    let normalized_dir = canonicalize_path(work_dir);
    SESSIONS_DATA
        .read()
        .unwrap()
        .working_sessions
        .iter()
        .any(|(dir, _)| dir == &normalized_dir)
}

pub fn active_session_entry() -> Option<SessionEntry> {
    let data = SESSIONS_DATA.read().unwrap();
    let id = data.active_session_id.as_ref()?;
    for project in &data.projects {
        if project.work_dir != data.active_work_dir {
            continue;
        }
        if let Some(session) = project.sessions.iter().find(|s| &s.id == id) {
            return Some(session.clone());
        }
    }
    None
}

pub fn project_work_dir_at_row(row_idx: usize) -> Option<PathBuf> {
    let data = SESSIONS_DATA.read().unwrap();
    let SessionListRow::ProjectHeader { project_idx } = data.rows.get(row_idx)? else {
        return None;
    };
    data.projects
        .get(*project_idx)
        .map(|project| project.work_dir.clone())
}

pub fn session_entry_at_row(row_idx: usize) -> Option<SessionEntry> {
    let data = SESSIONS_DATA.read().unwrap();
    match data.rows.get(row_idx)? {
        SessionListRow::Session {
            project_idx,
            session_idx,
        } => data
            .projects
            .get(*project_idx)
            .and_then(|p| p.sessions.get(*session_idx))
            .cloned(),
        _ => None,
    }
}

pub fn create_new_session(work_dir: &Path) -> Option<SessionEntry> {
    let sessions_dir = work_dir.join(".threadlane/sessions");
    std::fs::create_dir_all(&sessions_dir).ok()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mut nonce = now.as_nanos();
    let (id, path) = loop {
        let id = format!("session_{nonce}");
        let path = sessions_dir.join(format!("{id}.jsonl"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => break (id, path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => nonce += 1,
            Err(_) => return None,
        }
    };
    let entry = SessionEntry {
        id,
        title: "New session".to_string(),
        work_dir: work_dir.to_path_buf(),
        session_file: path,
        updated_at: now.as_secs(),
    };
    Some(entry)
}

pub fn archive_session(entry: &SessionEntry) -> bool {
    let archive_dir = entry.work_dir.join(".threadlane/sessions/archive");
    if std::fs::create_dir_all(&archive_dir).is_err() {
        return false;
    }
    let Some(file_name) = entry.session_file.file_name() else {
        return false;
    };
    std::fs::rename(&entry.session_file, archive_dir.join(file_name)).is_ok()
}

pub fn delete_session(entry: &SessionEntry) -> bool {
    std::fs::remove_file(&entry.session_file).is_ok()
}

pub fn relative_time_label(updated_at: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(updated_at);
    let secs = now.saturating_sub(updated_at);
    if secs < 60 {
        return "now".to_string();
    }
    if secs < 3600 {
        return format!("{}m", secs / 60);
    }
    if secs < 86400 {
        return format!("{}h", secs / 3600);
    }
    format!("{}d", secs / 86400)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_test_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("threadlane-{name}-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn working_state_is_tracked_per_session() {
        let work_dir = unique_test_dir("working-sessions");
        set_session_working(&work_dir, "first", true);
        set_session_working(&work_dir, "second", true);

        assert!(is_session_working(&work_dir, "first"));
        assert!(is_session_working(&work_dir, "second"));

        set_session_working(&work_dir, "first", false);
        assert!(!is_session_working(&work_dir, "first"));
        assert!(is_session_working(&work_dir, "second"));
        set_session_working(&work_dir, "second", false);
    }

    #[test]
    fn rapid_session_creation_never_reuses_a_file() {
        let work_dir = unique_test_dir("session-creation");
        std::fs::create_dir_all(&work_dir).unwrap();

        let first = create_new_session(&work_dir).unwrap();
        let second = create_new_session(&work_dir).unwrap();

        assert_ne!(first.id, second.id);
        assert_ne!(first.session_file, second.session_file);
        assert!(first.session_file.exists());
        assert!(second.session_file.exists());

        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn session_title_normalization() {
        assert_eq!(
            normalize_session_title(" Title: \"Fix the login flow\" "),
            "Fix the login flow"
        );
        assert_eq!(
            normalize_session_title("\"Title: Fix the login flow\""),
            "Fix the login flow"
        );
        assert_eq!(
            normalize_session_title("  title:   \"Fix   the   login flow\"  "),
            "Fix the login flow"
        );
        assert!(normalize_session_title(&"x".repeat(100)).chars().count() <= 42);
        assert_eq!(normalize_session_title("   "), "");
        assert_eq!(
            normalize_session_title("✨éclair: Fix the login flow"),
            "✨éclair: Fix the login flow"
        );
    }

    #[test]
    fn named_sessions_are_not_title_eligible() {
        let mut named = SessionTree::new("named");
        named.name = Some("Already named".into());
        assert!(!session_title_eligible(&named, None));
        let mut unnamed = SessionTree::new("unnamed");
        unnamed.add_message(AgentMessage::User {
            content: "first turn".into(),
        });
        assert!(session_title_eligible(&unnamed, None));
    }

    #[test]
    fn in_flight_title_generation_rejects_duplicates() {
        let work_dir = unique_test_dir("title-in-flight");
        assert!(begin_title_generation(&work_dir, "session-1"));
        assert!(!begin_title_generation(&work_dir, "session-1"));
        assert!(begin_title_generation(&work_dir, "session-2"));
        end_title_generation(&work_dir, "session-1");
        end_title_generation(&work_dir, "session-2");
        assert!(!begin_title_generation(&work_dir, "session-1"));
    }

    #[test]
    fn legacy_title_uses_first_existing_user_message() {
        let mut tree = SessionTree::new("legacy");
        tree.add_message(AgentMessage::User {
            content: "First request".into(),
        });
        tree.add_message(AgentMessage::Assistant {
            content: Some("reply".into()),
            tool_calls: None,
        });
        tree.add_message(AgentMessage::User {
            content: "Later request".into(),
        });
        assert_eq!(
            first_existing_user_prompt(&tree).as_deref(),
            Some("First request")
        );
    }

    #[test]
    fn legacy_title_uses_first_persisted_user_across_inactive_branch() {
        let dir = unique_test_dir("legacy-title");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("legacy.jsonl");
        let records = [
            threadlane_agent::SessionNode {
                id: "root".into(),
                parent_id: None,
                timestamp: 1,
                message: AgentMessage::User {
                    content: "First persisted request".into(),
                },
            },
            threadlane_agent::SessionNode {
                id: "branch_a".into(),
                parent_id: Some("root".into()),
                timestamp: 2,
                message: AgentMessage::User {
                    content: "Inactive branch request".into(),
                },
            },
            threadlane_agent::SessionNode {
                id: "branch_b".into(),
                parent_id: Some("root".into()),
                timestamp: 3,
                message: AgentMessage::User {
                    content: "Active later branch request".into(),
                },
            },
        ];
        std::fs::write(
            &path,
            records
                .iter()
                .map(|record| serde_json::to_string(record).unwrap() + "\n")
                .collect::<String>(),
        )
        .unwrap();
        let mut tree = SessionTree::load_from_file(&path).unwrap();
        assert!(tree.switch_active_node("branch_b"));
        assert_eq!(
            first_existing_user_prompt(&tree).as_deref(),
            Some("First persisted request")
        );
    }

    #[test]
    fn fresh_unnamed_session_uses_submitted_prompt_for_title_trigger() {
        let tree = SessionTree::new("fresh");
        let submitted = "  Explain the authentication flow  ";

        assert!(session_title_eligible(&tree, Some(submitted)));
        assert_eq!(
            title_prompt_for_submission(&tree, Some(submitted)).as_deref(),
            Some(submitted)
        );
    }

    #[test]
    fn attachment_only_submission_is_not_a_title_trigger() {
        let tree = SessionTree::new("fresh-attachment");

        assert!(!session_title_eligible(&tree, Some(" \n\t ")));
        assert_eq!(title_prompt_for_submission(&tree, Some(" \n\t ")), None);
    }

    #[test]
    fn attachment_only_message_has_no_title_prompt() {
        let mut tree = SessionTree::new("image");
        tree.add_message(AgentMessage::UserWithImages {
            content: String::new(),
            images: Vec::new(),
        });
        assert!(first_existing_user_prompt(&tree).is_none());
        assert!(!session_title_eligible(&tree, None));
    }

    #[test]
    fn refresh_preserves_a_project_draft_and_canonicalizes_duplicates() {
        let work_dir = unique_test_dir("refresh-draft");
        std::fs::create_dir_all(&work_dir).unwrap();
        set_active_project(&work_dir);

        let alias = work_dir.join("..").join(work_dir.file_name().unwrap());
        refresh_sessions(&[work_dir.clone(), alias]);

        let data = SESSIONS_DATA.read().unwrap();
        assert_eq!(data.projects.len(), 1);
        assert_eq!(
            data.active_work_dir,
            std::fs::canonicalize(&work_dir).unwrap()
        );
        assert!(data.active_session_id.is_none());
        drop(data);
        let _ = std::fs::remove_dir_all(work_dir);
    }
}
