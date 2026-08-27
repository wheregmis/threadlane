use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub path: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_opened_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_session_id: Option<String>,
}

impl ProjectRecord {
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string());
        Self {
            path: path.to_string_lossy().to_string(),
            name,
            last_opened_at: None,
            last_session_id: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRegistry {
    pub projects: Vec<ProjectRecord>,
}

pub fn default_global_threadlane_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".threadlane"))
}

pub fn load_project_registry() -> Vec<ProjectRecord> {
    let Some(global_dir) = default_global_threadlane_dir() else {
        return Vec::new();
    };
    let path = global_dir.join("projects.json");
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(registry) = serde_json::from_slice::<ProjectRegistry>(&bytes) {
            return registry.projects;
        }
    }
    Vec::new()
}

pub fn save_project_registry(projects: &[ProjectRecord]) -> Result<(), String> {
    let global_dir = default_global_threadlane_dir()
        .ok_or_else(|| "Global Threadlane dir unavailable".to_string())?;
    std::fs::create_dir_all(&global_dir).map_err(|e| e.to_string())?;
    let registry_path = global_dir.join("projects.json");
    let data = serde_json::to_vec_pretty(&ProjectRegistry {
        projects: projects.to_vec(),
    })
    .map_err(|e| e.to_string())?;
    std::fs::write(&registry_path, data).map_err(|e| e.to_string())
}

pub fn register_project(project_path: &Path) -> Result<ProjectRecord, String> {
    let canonical = project_path
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize project path: {e}"))?;
    let path_str = canonical.to_string_lossy().to_string();
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());

    let record = ProjectRecord {
        path: path_str.clone(),
        name,
        last_opened_at: Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_default(),
        ),
        last_session_id: None,
    };

    let mut projects = load_project_registry();
    projects.retain(|p| p.path != path_str);
    projects.insert(0, record.clone());
    save_project_registry(&projects)?;

    Ok(record)
}

pub fn validate_path_in_workspace(path: &str, root: &Path) -> Result<PathBuf, String> {
    let target = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };

    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("Cannot canonicalize root {}: {e}", root.display()))?;

    if target.exists() {
        let canonical_target = target
            .canonicalize()
            .map_err(|e| format!("Cannot canonicalize target {}: {e}", target.display()))?;
        if canonical_target.starts_with(&canonical_root) {
            Ok(canonical_target)
        } else {
            Err(format!(
                "Path {} is outside workspace {}",
                target.display(),
                canonical_root.display()
            ))
        }
    } else {
        // If not yet existing, verify parent
        let mut ancestor = target.clone();
        while let Some(parent) = ancestor.parent() {
            if parent.exists() {
                let canonical_parent = parent.canonicalize().map_err(|e| e.to_string())?;
                if canonical_parent.starts_with(&canonical_root) {
                    return Ok(target);
                } else {
                    return Err(format!(
                        "Path {} is outside workspace {}",
                        target.display(),
                        canonical_root.display()
                    ));
                }
            }
            ancestor = parent.to_path_buf();
        }
        Ok(target)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListProjectsResponse {
    pub projects: Vec<ProjectRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterProjectRequest {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnregisterProjectRequest {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectProjectRequest {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowseDirectoriesRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDirectoryInfo {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_git: bool,
    pub is_project: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowseDirectoriesResponse {
    pub current_path: String,
    pub display_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_path: Option<String>,
    pub entries: Vec<HostDirectoryInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryEntryKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
    pub kind: DirectoryEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDirectoryRequest {
    pub project_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDirectoryResponse {
    pub entries: Vec<DirectoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFileRequest {
    pub project_path: String,
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFileResponse {
    pub content: String,
    pub line_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteFileRequest {
    pub project_path: String,
    pub relative_path: String,
    pub content: String,
    pub overwrite: bool,
}
