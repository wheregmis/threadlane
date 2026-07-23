//! Sessions panel state: projects, session discovery, file operations, and active selection.

use crate::panels::chat::truncate_chars;
use mypi_agent::{AgentMessage, SessionTree};

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
    pub working_sessions: Vec<(PathBuf, String)>,
    pub active_session_id: Option<String>,
    pub active_work_dir: PathBuf,
    pub context_session_id: Option<String>,
    pub context_work_dir: PathBuf,
    pub rows: Vec<SessionListRow>,
}

pub static TITLE_IN_FLIGHT: LazyLock<RwLock<std::collections::HashSet<(PathBuf, String)>>> =
    LazyLock::new(|| RwLock::new(std::collections::HashSet::new()));

pub fn normalize_session_title(value: &str) -> String {
    let mut title = value.trim().to_string();
    // Providers vary in whether they put the quote pair around the whole
    // response or put a `Title:` prefix inside it. Peel either wrapper in
    // either order until neither is present.
    loop {
        let before = title.clone();
        let prefix: String = title.chars().take(6).collect();
        if prefix.eq_ignore_ascii_case("title:") {
            title = title.chars().skip(6).collect::<String>().trim().to_string();
        }
        let chars: Vec<char> = title.chars().collect();
        if chars.len() >= 2
            && ((chars[0] == '"' && chars[chars.len() - 1] == '"')
                || (chars[0] == '\'' && chars[chars.len() - 1] == '\''))
        {
            title = chars[1..chars.len() - 1].iter().collect::<String>();
            title = title.trim().to_string();
        }
        if title == before {
            break;
        }
    }
    title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    title.chars().take(42).collect()
}

pub fn session_title_eligible(tree: &SessionTree) -> bool {
    !tree.has_name()
}
pub fn begin_title_generation(work_dir: &Path, session_id: &str) -> bool {
    let key = (
        std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf()),
        session_id.to_string(),
    );
    TITLE_IN_FLIGHT.write().unwrap().insert(key)
}
pub fn end_title_generation(work_dir: &Path, session_id: &str) {
    let key = (
        std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf()),
        session_id.to_string(),
    );
    TITLE_IN_FLIGHT.write().unwrap().remove(&key);
}

pub static SESSIONS_DATA: RwLock<SessionsData> = RwLock::new(SessionsData {
    projects: Vec::new(),
    working_sessions: Vec::new(),
    active_session_id: None,
    active_work_dir: PathBuf::new(),
    context_session_id: None,
    context_work_dir: PathBuf::new(),
    rows: Vec::new(),
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
    let sessions_dir = work_dir.join(".mypi/sessions");
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
            work_dir: work_dir.to_path_buf(),
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
        let dir = std::fs::canonicalize(raw_dir).unwrap_or_else(|_| raw_dir.clone());
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
    data.rows = rows.clone();
    rows
}

pub fn set_session_working(work_dir: &Path, session_id: &str, is_working: bool) {
    let mut data = SESSIONS_DATA.write().unwrap();
    let normalized_dir = std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
    let key = (normalized_dir, session_id.to_string());
    if is_working {
        if !data.working_sessions.contains(&key) {
            data.working_sessions.push(key);
        }
    } else {
        data.working_sessions.retain(|working| working != &key);
    }
}

pub fn is_session_working(work_dir: &Path, session_id: &str) -> bool {
    let normalized_dir = std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
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
    data.active_work_dir =
        std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
    data.active_session_id = Some(session_id.to_string());
}

pub fn set_active_project(work_dir: &Path) {
    let mut data = SESSIONS_DATA.write().unwrap();
    data.active_work_dir =
        std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
    data.active_session_id = None;
}

pub fn is_project_working(work_dir: &Path) -> bool {
    let normalized_dir = std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
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
    let sessions_dir = work_dir.join(".mypi/sessions");
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
    let archive_dir = entry.work_dir.join(".mypi/sessions/archive");
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
        std::env::temp_dir().join(format!("mypi-gui-{name}-{}-{nonce}", std::process::id()))
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
        assert!(!session_title_eligible(&named));
        assert!(session_title_eligible(&SessionTree::new("unnamed")));
    }

    #[test]
    fn in_flight_title_generation_rejects_duplicates() {
        let work_dir = unique_test_dir("title-in-flight");
        assert!(begin_title_generation(&work_dir, "session-1"));
        assert!(!begin_title_generation(&work_dir, "session-1"));
        assert!(begin_title_generation(&work_dir, "session-2"));
        end_title_generation(&work_dir, "session-1");
        end_title_generation(&work_dir, "session-2");
        assert!(begin_title_generation(&work_dir, "session-1"));
        end_title_generation(&work_dir, "session-1");
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
