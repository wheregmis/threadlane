use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectRecord {
    #[serde(default)]
    pub id: String,
    pub path: PathBuf,
    #[serde(alias = "display_name")]
    pub name: String,
    #[serde(default)]
    pub last_selected_task_id: Option<String>,
    #[serde(default)]
    pub attached_at: u64,
    #[serde(default)]
    pub last_opened_at: u64,
    #[serde(default)]
    pub last_session_id: Option<String>,
}

impl ProjectRecord {
    pub fn from_path(path: PathBuf) -> Self {
        let now = now_millis();
        let name = path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".into());
        Self {
            id: project_id(&path),
            path,
            name,
            last_selected_task_id: None,
            attached_at: now,
            last_opened_at: now,
            last_session_id: None,
        }
    }
}

pub fn load_project_registry() -> Vec<ProjectRecord> {
    load_project_registry_from(&default_global_dir())
}

pub fn save_project_registry(projects: &[ProjectRecord]) -> Result<(), String> {
    let _guard = registry_lock().lock().map_err(|error| error.to_string())?;
    save_project_registry_to(&default_global_dir(), projects)
}

pub fn register_project(raw_path: &Path) -> Result<ProjectRecord, String> {
    let _guard = registry_lock().lock().map_err(|error| error.to_string())?;
    let canonical = raw_path.canonicalize().map_err(|error| {
        format!(
            "Failed to canonicalize project path '{}': {error}",
            raw_path.display()
        )
    })?;
    let global_dir = default_global_dir();
    let mut projects = load_project_registry_from(&global_dir);
    if let Some(project) = projects
        .iter_mut()
        .find(|project| same_path(&project.path, &canonical))
    {
        project.last_opened_at = now_millis();
        let result = project.clone();
        save_project_registry_to(&global_dir, &projects)?;
        return Ok(result);
    }
    let project = ProjectRecord::from_path(canonical);
    projects.push(project.clone());
    save_project_registry_to(&global_dir, &projects)?;
    Ok(project)
}

pub fn select_project(raw_path: &Path, session_id: Option<&str>) -> Result<ProjectRecord, String> {
    let _guard = registry_lock().lock().map_err(|error| error.to_string())?;
    let canonical = raw_path.canonicalize().map_err(|error| {
        format!(
            "Failed to canonicalize project path '{}': {error}",
            raw_path.display()
        )
    })?;
    let global_dir = default_global_dir();
    let mut projects = load_project_registry_from(&global_dir);
    let index = if let Some(index) = projects
        .iter()
        .position(|project| same_path(&project.path, &canonical))
    {
        index
    } else {
        projects.push(ProjectRecord::from_path(canonical));
        projects.len() - 1
    };
    projects[index].last_opened_at = now_millis();
    if let Some(session_id) = session_id {
        projects[index].last_session_id = Some(session_id.to_owned());
    }
    let result = projects[index].clone();
    save_project_registry_to(&global_dir, &projects)?;
    Ok(result)
}

pub fn unregister_project(raw_path: &Path) -> Result<(), String> {
    let _guard = registry_lock().lock().map_err(|error| error.to_string())?;
    let canonical = raw_path.canonicalize().unwrap_or_else(|_| raw_path.to_path_buf());
    let global_dir = default_global_dir();
    let mut projects = load_project_registry_from(&global_dir);
    projects.retain(|project| !same_path(&project.path, &canonical));
    save_project_registry_to(&global_dir, &projects)
}

pub(crate) fn merge_and_save_project_registry_to(
    global_dir: &Path,
    incoming: &[ProjectRecord],
) -> Result<(), String> {
    let _guard = registry_lock().lock().map_err(|error| error.to_string())?;
    let mut merged = load_project_registry_from(global_dir);
    for mut record in incoming.iter().cloned() {
        if let Some(index) = merged
            .iter()
            .position(|current| same_path(&current.path, &record.path))
        {
            let durable = &merged[index];
            record.attached_at = match (record.attached_at, durable.attached_at) {
                (0, value) | (value, 0) => value,
                (left, right) => left.min(right),
            };
            if durable.last_opened_at > record.last_opened_at {
                record.last_opened_at = durable.last_opened_at;
                record.last_session_id = durable.last_session_id.clone();
            } else if record.last_session_id.is_none() {
                record.last_session_id = durable.last_session_id.clone();
            }
            if record.last_selected_task_id.is_none() {
                record.last_selected_task_id = durable.last_selected_task_id.clone();
            }
            merged[index] = record;
        } else {
            merged.push(record);
        }
    }
    save_project_registry_to(global_dir, &merged)
}

pub(crate) fn load_project_registry_from(global_dir: &Path) -> Vec<ProjectRecord> {
    let canonical_file = global_dir.join("projects.json");
    let projects = fs::read(&canonical_file)
        .map(|contents| parse_project_records(&contents))
        .unwrap_or_default();

    normalize_projects(projects)
}

pub(crate) fn save_project_registry_to(
    global_dir: &Path,
    projects: &[ProjectRecord],
) -> Result<(), String> {
    fs::create_dir_all(global_dir).map_err(|error| error.to_string())?;
    let path = global_dir.join("projects.json");
    let temporary_path = global_dir.join("projects.json.tmp");
    let json = serde_json::to_vec_pretty(projects).map_err(|error| error.to_string())?;
    fs::write(&temporary_path, json).map_err(|error| error.to_string())?;
    fs::rename(temporary_path, path).map_err(|error| error.to_string())
}

fn parse_project_records(contents: &[u8]) -> Vec<ProjectRecord> {
    let Ok(entries) = serde_json::from_slice::<Vec<serde_json::Value>>(contents) else {
        return Vec::new();
    };
    normalize_projects(
        entries
            .into_iter()
            .filter_map(|entry| serde_json::from_value::<ProjectRecord>(entry).ok())
            .collect(),
    )
}

fn normalize_projects(projects: Vec<ProjectRecord>) -> Vec<ProjectRecord> {
    let mut seen = HashSet::new();
    projects
        .into_iter()
        .filter_map(|mut project| {
            project.path = fs::canonicalize(&project.path).unwrap_or(project.path);
            if is_ephemeral_temp_project(&project.path) {
                return None;
            }
            if project.id.is_empty() {
                project.id = project_id(&project.path);
            }
            seen.insert(project.path.clone()).then_some(project)
        })
        .collect()
}

fn is_ephemeral_temp_project(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let suffix = name.strip_prefix(".tmp").unwrap_or_default();
    !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_alphanumeric())
}

fn same_path(left: &Path, right: &Path) -> bool {
    fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf())
        == fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf())
}

fn registry_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn default_global_dir() -> PathBuf {
    threadlane_wasi::packages::default_global_threadlane_dir().unwrap_or_else(|| {
        directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().join(".threadlane"))
            .unwrap_or_else(|| PathBuf::from(".threadlane"))
    })
}

fn project_id(path: &Path) -> String {
    format!("{:x}", md5::compute(path.to_string_lossy().as_bytes()))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_supervisor_save_preserves_newer_project_selection() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let mut durable = ProjectRecord::from_path(project);
        durable.last_opened_at = 20;
        durable.last_session_id = Some("session-2".into());
        save_project_registry_to(dir.path(), &[durable.clone()]).unwrap();

        let mut stale_supervisor = durable;
        stale_supervisor.last_opened_at = 2;
        stale_supervisor.last_session_id = None;
        stale_supervisor.last_selected_task_id = Some("task-3".into());
        merge_and_save_project_registry_to(dir.path(), &[stale_supervisor]).unwrap();

        let projects = load_project_registry_from(dir.path());
        assert_eq!(projects[0].last_opened_at, 20);
        assert_eq!(projects[0].last_session_id.as_deref(), Some("session-2"));
        assert!(&projects[0].last_selected_task_id.as_deref() == &Some("task-3"));
    }
}
