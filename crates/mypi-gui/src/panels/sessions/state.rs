//! Sessions panel state: projects, session discovery, file operations, and active selection.

use crate::panels::chat::truncate_chars;
use mypi_agent::{AgentMessage, SessionTree};

use std::path::{Path, PathBuf};
use std::sync::RwLock;
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
        if let AgentMessage::User { content } = msg {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                continue;
            }
            let cleaned = trimmed
                .strip_prefix("/plan ")
                .or_else(|| trimmed.strip_prefix("/plan"))
                .unwrap_or(trimmed)
                .trim();
            if !cleaned.is_empty() {
                return truncate_chars(cleaned, 42);
            }
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

fn load_extra_project_dirs(work_dir: &Path) -> Vec<PathBuf> {
    let path = work_dir.join(".mypi/gui/sidebar_projects.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(list) = serde_json::from_str::<Vec<String>>(&text) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for item in list {
        let p = PathBuf::from(item);
        let resolved = if p.is_absolute() { p } else { work_dir.join(p) };
        if resolved != work_dir && resolved.is_dir() {
            dirs.push(resolved);
        }
    }
    dirs
}

pub fn refresh_sessions(work_dir: &Path) -> Vec<SessionListRow> {
    let mut projects = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut push_project = |dir: PathBuf| {
        if !seen.insert(dir.clone()) {
            return;
        }
        let sessions = discover_sessions_in_project(&dir);
        projects.push(ProjectGroup {
            name: project_display_name(&dir),
            work_dir: dir,
            sessions,
        });
    };

    push_project(work_dir.to_path_buf());
    for extra in load_extra_project_dirs(work_dir) {
        push_project(extra);
    }

    let rows = rebuild_session_rows(&projects);

    let mut data = SESSIONS_DATA.write().unwrap();
    let prev_id = data.active_session_id.clone();
    let prev_dir = data.active_work_dir.clone();

    let still_active = prev_id.is_none()
        || projects.iter().any(|p| {
            p.work_dir == prev_dir && p.sessions.iter().any(|s| Some(&s.id) == prev_id.as_ref())
        });

    if !still_active {
        if let Some(session) = projects
            .iter()
            .find(|p| p.work_dir == work_dir)
            .and_then(|p| p.sessions.first())
        {
            data.active_session_id = Some(session.id.clone());
            data.active_work_dir = session.work_dir.clone();
        } else {
            data.active_session_id = None;
            data.active_work_dir = work_dir.to_path_buf();
        }
    }

    data.projects = projects;
    data.rows = rows.clone();
    if data.active_work_dir.as_os_str().is_empty() {
        data.active_work_dir = work_dir.to_path_buf();
    }
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
    data.active_work_dir = work_dir.to_path_buf();
    data.active_session_id = Some(session_id.to_string());
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
    refresh_sessions(work_dir);
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
}
