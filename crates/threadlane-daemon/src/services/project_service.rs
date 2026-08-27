use std::fs;
use std::path::{Path, PathBuf};
use threadlane_protocol::project::*;
use threadlane_session::project_registry::{
    load_project_registry, register_project, select_project, unregister_project,
};

#[derive(Clone, Default)]
pub struct ProjectService;

impl ProjectService {
    pub fn new() -> Self {
        Self
    }

    pub fn list_projects(&self) -> Result<ListProjectsResponse, String> {
        let registry = load_project_registry();
        let projects = registry
            .into_iter()
            .map(|p| ProjectRecord {
                path: p.path.to_string_lossy().to_string(),
                name: p.name,
                last_opened_at: Some(p.last_opened_at.to_string()),
                last_session_id: p.last_session_id,
            })
            .collect();
        Ok(ListProjectsResponse { projects })
    }

    pub fn register_project(&self, req: RegisterProjectRequest) -> Result<ProjectRecord, String> {
        let path = PathBuf::from(&req.path);
        if !path.exists() || !path.is_dir() {
            return Err(format!("Path '{}' does not exist or is not a directory", req.path));
        }

        let record = register_project(&path).map_err(|e| e.to_string())?;

        Ok(ProjectRecord {
            path: record.path.to_string_lossy().to_string(),
            name: record.name,
            last_opened_at: Some(record.last_opened_at.to_string()),
            last_session_id: record.last_session_id,
        })
    }

    pub fn unregister_project(&self, req: UnregisterProjectRequest) -> Result<(), String> {
        let path = PathBuf::from(&req.path);
        unregister_project(&path)
    }

    pub fn select_project(&self, req: SelectProjectRequest) -> Result<ProjectRecord, String> {
        let path = PathBuf::from(&req.path);
        let record = select_project(&path, req.session_id.as_deref())?;

        Ok(ProjectRecord {
            path: record.path.to_string_lossy().to_string(),
            name: record.name,
            last_opened_at: Some(record.last_opened_at.to_string()),
            last_session_id: record.last_session_id,
        })
    }

    pub fn browse_directories(
        &self,
        req: BrowseDirectoriesRequest,
    ) -> Result<BrowseDirectoriesResponse, String> {
        let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let target = match req.path {
            Some(ref p) if !p.is_empty() => {
                if p == "~" || p.starts_with("~/") {
                    if p == "~" {
                        home_dir.clone()
                    } else {
                        home_dir.join(&p[2..])
                    }
                } else {
                    PathBuf::from(p)
                }
            }
            _ => home_dir.clone(),
        };

        let canonical = target.canonicalize().unwrap_or(target);
        if !canonical.is_dir() {
            return Err(format!("'{}' is not a directory", canonical.display()));
        }

        let current_path = canonical.to_string_lossy().to_string();
        let display_path = if let Ok(rel) = canonical.strip_prefix(&home_dir) {
            if rel.as_os_str().is_empty() {
                "~/".to_string()
            } else {
                format!("~/{}/", rel.to_string_lossy())
            }
        } else {
            format!("{}/", current_path)
        };

        let parent_path = canonical.parent().map(|p| p.to_string_lossy().to_string());

        let mut entries = Vec::new();
        if let Ok(read_dir) = fs::read_dir(&canonical) {
            for item in read_dir.flatten() {
                let Ok(file_type) = item.file_type() else {
                    continue;
                };
                let is_dir =
                    file_type.is_dir() || (file_type.is_symlink() && item.path().is_dir());
                if !is_dir {
                    continue;
                }
                let name = item.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }

                let full_path = item.path();
                let is_git = full_path.join(".git").exists();
                let is_project = full_path.join(".threadlane").exists();

                entries.push(HostDirectoryInfo {
                    name,
                    path: full_path.to_string_lossy().to_string(),
                    is_dir: true,
                    is_git,
                    is_project,
                });
            }
        }

        entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        Ok(BrowseDirectoriesResponse {
            current_path,
            display_path,
            parent_path,
            entries,
        })
    }

    pub fn list_directory(&self, req: ListDirectoryRequest) -> Result<ListDirectoryResponse, String> {
        let base = Path::new(&req.project_path);
        if !base.exists() {
            return Err(format!("Project path '{}' does not exist", req.project_path));
        }

        let target = match req.relative_path {
            Some(ref rel) if !rel.is_empty() => base.join(rel),
            _ => base.to_path_buf(),
        };

        if !target.exists() {
            return Err(format!("Directory '{}' does not exist", target.display()));
        }

        let read_dir = fs::read_dir(&target).map_err(|e| e.to_string())?;
        let mut entries = Vec::new();

        for item in read_dir.flatten() {
            let file_type = item.file_type().map_err(|e| e.to_string())?;
            let name = item.file_name().to_string_lossy().to_string();
            
            // Skip hidden dotfiles like .git by default in top level
            if name.starts_with('.') && name != ".threadlane" {
                continue;
            }

            let kind = if file_type.is_dir() {
                DirectoryEntryKind::Directory
            } else if file_type.is_symlink() {
                DirectoryEntryKind::Symlink
            } else {
                DirectoryEntryKind::File
            };

            let size_bytes = if kind == DirectoryEntryKind::File {
                item.metadata().ok().map(|m| m.len())
            } else {
                None
            };

            let item_path = item.path();
            let relative_item_path = item_path
                .strip_prefix(base)
                .unwrap_or(&item_path)
                .to_string_lossy()
                .to_string();

            entries.push(DirectoryEntry {
                name,
                path: relative_item_path,
                kind,
                size_bytes,
            });
        }

        entries.sort_by(|a, b| {
            match (a.kind, b.kind) {
                (DirectoryEntryKind::Directory, DirectoryEntryKind::Directory) => a.name.cmp(&b.name),
                (DirectoryEntryKind::Directory, _) => std::cmp::Ordering::Less,
                (_, DirectoryEntryKind::Directory) => std::cmp::Ordering::Greater,
                _ => a.name.cmp(&b.name),
            }
        });

        Ok(ListDirectoryResponse { entries })
    }

    pub fn read_file(&self, req: ReadFileRequest) -> Result<ReadFileResponse, String> {
        let base = Path::new(&req.project_path);
        let path = base.join(&req.relative_path);

        if !path.exists() {
            return Err(format!("File '{}' does not exist", path.display()));
        }

        let content = fs::read_to_string(&path).map_err(|e| format!("Failed to read file: {e}"))?;
        let line_count = content.lines().count();

        Ok(ReadFileResponse {
            content,
            line_count,
        })
    }

    pub fn write_file(&self, req: WriteFileRequest) -> Result<(), String> {
        let base = Path::new(&req.project_path);
        let path = base.join(&req.relative_path);

        if path.exists() && !req.overwrite {
            return Err(format!("File '{}' already exists and overwrite is false", path.display()));
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create parent directories: {e}"))?;
        }

        fs::write(&path, req.content.as_bytes()).map_err(|e| format!("Failed to write file: {e}"))?;
        Ok(())
    }
}
