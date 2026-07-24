//! Durable application-global registry of projects attached to the sessions panel.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const REGISTRY_DIR: &str = "gui";
const REGISTRY_FILE: &str = "projects.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachedProject {
    pub path: PathBuf,
    pub display_name: String,
    pub attached_at: u64,
    pub last_opened_at: u64,
    #[serde(default)]
    pub last_session_id: Option<String>,
}

#[derive(Debug)]
pub enum ProjectRegistryError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    MalformedJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    NotDirectory(PathBuf),
    ProjectNotAttached(PathBuf),
}

impl fmt::Display for ProjectRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "failed to {operation} '{}': {source}", path.display()),
            Self::MalformedJson { path, source } => write!(
                f,
                "project registry '{}' contains malformed JSON; fix or move the file and try again: {source}",
                path.display()
            ),
            Self::NotDirectory(path) => {
                write!(f, "cannot attach '{}': path is not a directory", path.display())
            }
            Self::ProjectNotAttached(path) => {
                write!(f, "project '{}' is not attached", path.display())
            }
        }
    }
}

impl std::error::Error for ProjectRegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::MalformedJson { source, .. } => Some(source),
            Self::NotDirectory(_) | Self::ProjectNotAttached(_) => None,
        }
    }
}

pub struct ProjectRegistry {
    registry_path: PathBuf,
    projects: Vec<AttachedProject>,
}

impl ProjectRegistry {
    pub fn load(global_threadlane_dir: &Path) -> Result<Self, ProjectRegistryError> {
        let registry_path = registry_path(global_threadlane_dir);
        let projects = match fs::read(&registry_path) {
            Ok(contents) => {
                let loaded: Vec<AttachedProject> =
                    serde_json::from_slice(&contents).map_err(|source| {
                        ProjectRegistryError::MalformedJson {
                            path: registry_path.clone(),
                            source,
                        }
                    })?;
                let mut seen = std::collections::HashSet::new();
                loaded
                    .into_iter()
                    .filter_map(|mut project| {
                        project.path = fs::canonicalize(&project.path)
                            .unwrap_or_else(|_| project.path.clone());
                        seen.insert(project.path.clone()).then_some(project)
                    })
                    .collect()
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(source) => {
                return Err(io_error("read project registry", &registry_path, source));
            }
        };

        Ok(Self {
            registry_path,
            projects,
        })
    }

    pub fn attach(&mut self, raw_path: &Path) -> Result<AttachedProject, ProjectRegistryError> {
        let canonical_path = fs::canonicalize(raw_path)
            .map_err(|source| io_error("canonicalize project directory", raw_path, source))?;
        let metadata = fs::metadata(&canonical_path)
            .map_err(|source| io_error("inspect project directory", &canonical_path, source))?;
        if !metadata.is_dir() {
            return Err(ProjectRegistryError::NotDirectory(canonical_path));
        }

        if let Some(project) = self
            .projects
            .iter()
            .find(|project| project.path == canonical_path)
        {
            return Ok(project.clone());
        }

        let now = unix_timestamp();
        let project = AttachedProject {
            display_name: display_name(&canonical_path),
            path: canonical_path,
            attached_at: now,
            last_opened_at: now,
            last_session_id: None,
        };
        self.projects.push(project.clone());
        if let Err(error) = self.persist() {
            self.projects.pop();
            return Err(error);
        }

        Ok(project)
    }

    pub fn detach(&mut self, canonical_path: &Path) -> Result<bool, ProjectRegistryError> {
        let Some(index) = self
            .projects
            .iter()
            .position(|project| project.path == canonical_path)
        else {
            return Ok(false);
        };

        let removed = self.projects.remove(index);
        if let Err(error) = self.persist() {
            self.projects.insert(index, removed);
            return Err(error);
        }
        Ok(true)
    }

    fn project_index(&self, canonical_path: &Path) -> Result<usize, ProjectRegistryError> {
        self.projects
            .iter()
            .position(|project| project.path == canonical_path)
            .ok_or_else(|| ProjectRegistryError::ProjectNotAttached(canonical_path.to_path_buf()))
    }

    pub fn touch(&mut self, canonical_path: &Path) -> Result<(), ProjectRegistryError> {
        let index = self.project_index(canonical_path)?;
        let previous = self.projects[index].last_opened_at;
        self.projects[index].last_opened_at = unix_timestamp().max(previous.saturating_add(1));
        if let Err(error) = self.persist() {
            self.projects[index].last_opened_at = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn remember_selection(
        &mut self,
        canonical_path: &Path,
        session_id: Option<&str>,
    ) -> Result<(), ProjectRegistryError> {
        let index = self.project_index(canonical_path)?;
        let previous_time = self.projects[index].last_opened_at;
        let previous_session = self.projects[index].last_session_id.clone();
        self.projects[index].last_opened_at = unix_timestamp().max(previous_time.saturating_add(1));
        self.projects[index].last_session_id = session_id.map(str::to_owned);
        if let Err(error) = self.persist() {
            self.projects[index].last_opened_at = previous_time;
            self.projects[index].last_session_id = previous_session;
            return Err(error);
        }
        Ok(())
    }

    pub fn projects(&self) -> &[AttachedProject] {
        &self.projects
    }

    fn persist(&self) -> Result<(), ProjectRegistryError> {
        let parent = self
            .registry_path
            .parent()
            .expect("registry path has a parent");
        fs::create_dir_all(parent)
            .map_err(|source| io_error("create project registry directory", parent, source))?;

        let contents = serde_json::to_vec_pretty(&self.projects).map_err(|source| {
            ProjectRegistryError::MalformedJson {
                path: self.registry_path.clone(),
                source,
            }
        })?;
        let (temp_path, mut temp_file) = create_temp_file(&self.registry_path)?;

        let write_result = (|| {
            temp_file.write_all(&contents).map_err(|source| {
                io_error("write temporary project registry", &temp_path, source)
            })?;
            temp_file.write_all(b"\n").map_err(|source| {
                io_error("write temporary project registry", &temp_path, source)
            })?;
            temp_file.sync_all().map_err(|source| {
                io_error("sync temporary project registry", &temp_path, source)
            })?;
            drop(temp_file);
            fs::rename(&temp_path, &self.registry_path).map_err(|source| {
                io_error(
                    "atomically replace project registry",
                    &self.registry_path,
                    source,
                )
            })?;
            sync_parent_directory(parent)?;
            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        write_result
    }
}

fn registry_path(global_threadlane_dir: &Path) -> PathBuf {
    global_threadlane_dir.join(REGISTRY_DIR).join(REGISTRY_FILE)
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn create_temp_file(registry_path: &Path) -> Result<(PathBuf, File), ProjectRegistryError> {
    let parent = registry_path.parent().expect("registry path has a parent");
    let process_id = std::process::id();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    for attempt in 0..100_u32 {
        let temp_path = parent.join(format!(
            ".{REGISTRY_FILE}.{process_id}.{nonce}.{attempt}.tmp"
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(io_error(
                    "create temporary project registry",
                    &temp_path,
                    source,
                ));
            }
        }
    }

    Err(io_error(
        "create temporary project registry",
        registry_path,
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique temporary file",
        ),
    ))
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), ProjectRegistryError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("sync project registry directory", parent, source))
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), ProjectRegistryError> {
    Ok(())
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> ProjectRegistryError {
    ProjectRegistryError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(test_name: &str) -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "threadlane-project-registry-{test_name}-{}-{id}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn attach_canonicalizes_and_persists_a_directory() {
        let temp = TempDir::new("canonical");
        let global = temp.path().join("global");
        let project = temp.path().join("parent").join("project");
        fs::create_dir_all(&project).unwrap();
        let aliased_path = project.join("..").join("project");

        let mut registry = ProjectRegistry::load(&global).unwrap();
        let attached = registry.attach(&aliased_path).unwrap();

        assert_eq!(attached.path, fs::canonicalize(&project).unwrap());
        assert_eq!(attached.display_name, "project");
        assert_eq!(registry.projects(), &[attached.clone()]);
        assert!(attached.attached_at > 0);
        assert_eq!(attached.attached_at, attached.last_opened_at);
        assert!(attached.last_session_id.is_none());

        let loaded = ProjectRegistry::load(&global).unwrap();
        assert_eq!(loaded.projects(), &[attached]);
    }

    #[cfg(unix)]
    #[test]
    fn attach_deduplicates_symlink_aliases() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new("symlink");
        let global = temp.path().join("global");
        let project = temp.path().join("real-project");
        let alias = temp.path().join("project-alias");
        fs::create_dir(&project).unwrap();
        symlink(&project, &alias).unwrap();

        let mut registry = ProjectRegistry::load(&global).unwrap();
        let first = registry.attach(&project).unwrap();
        let duplicate = registry.attach(&alias).unwrap();

        assert_eq!(duplicate, first);
        assert_eq!(duplicate.display_name, "real-project");
        assert_eq!(registry.projects().len(), 1);
    }

    #[test]
    fn attach_rejects_missing_paths_and_files() {
        let temp = TempDir::new("invalid-paths");
        let global = temp.path().join("global");
        let file = temp.path().join("not-a-directory");
        fs::write(&file, b"content").unwrap();
        let mut registry = ProjectRegistry::load(&global).unwrap();

        let missing_error = registry.attach(&temp.path().join("missing")).unwrap_err();
        assert!(missing_error.to_string().contains("canonicalize"));

        let file_error = registry.attach(&file).unwrap_err();
        assert!(matches!(file_error, ProjectRegistryError::NotDirectory(_)));
        assert!(registry.projects().is_empty());
    }

    #[test]
    fn detach_and_touch_are_persisted() {
        let temp = TempDir::new("mutations");
        let global = temp.path().join("global");
        let first_path = temp.path().join("first");
        let second_path = temp.path().join("second");
        fs::create_dir(&first_path).unwrap();
        fs::create_dir(&second_path).unwrap();

        let mut registry = ProjectRegistry::load(&global).unwrap();
        let first = registry.attach(&first_path).unwrap();
        let second = registry.attach(&second_path).unwrap();
        registry
            .remember_selection(&first.path, Some("session-1"))
            .unwrap();
        assert!(registry.projects()[0].last_opened_at > first.last_opened_at);
        assert_eq!(
            registry.projects()[0].last_session_id.as_deref(),
            Some("session-1")
        );
        assert!(registry.detach(&second.path).unwrap());
        assert!(!registry.detach(&second.path).unwrap());

        let loaded = ProjectRegistry::load(&global).unwrap();
        assert_eq!(loaded.projects(), registry.projects());
        assert_eq!(loaded.projects().len(), 1);
        assert!(matches!(
            registry.touch(&second.path),
            Err(ProjectRegistryError::ProjectNotAttached(_))
        ));
    }

    #[test]
    fn older_registry_records_load_without_session_metadata() {
        let temp = TempDir::new("backward-compatible");
        let global = temp.path().join("global");
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();
        let path = registry_path(&global);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                r#"[{{"path":"{}","display_name":"project","attached_at":1,"last_opened_at":2}}]"#,
                project.display()
            ),
        )
        .unwrap();

        let registry = ProjectRegistry::load(&global).unwrap();
        assert_eq!(registry.projects().len(), 1);
        assert!(registry.projects()[0].last_session_id.is_none());
    }

    #[test]
    fn malformed_json_is_actionable_and_preserved() {
        let temp = TempDir::new("malformed");
        let global = temp.path().join("global");
        let path = registry_path(&global);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let malformed = b"{ definitely not valid JSON";
        fs::write(&path, malformed).unwrap();

        let error = ProjectRegistry::load(&global).err().unwrap();

        assert!(matches!(error, ProjectRegistryError::MalformedJson { .. }));
        assert!(error.to_string().contains("fix or move the file"));
        assert_eq!(fs::read(path).unwrap(), malformed);
    }

    #[test]
    fn persistence_uses_and_cleans_up_a_same_directory_temp_file() {
        let temp = TempDir::new("atomic-save");
        let global = temp.path().join("global");
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();

        let mut registry = ProjectRegistry::load(&global).unwrap();
        registry.attach(&project).unwrap();

        let gui_dir = global.join(REGISTRY_DIR);
        let entries: Vec<_> = fs::read_dir(&gui_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from(REGISTRY_FILE)]);
        let stored: Vec<AttachedProject> =
            serde_json::from_slice(&fs::read(gui_dir.join(REGISTRY_FILE)).unwrap()).unwrap();
        assert_eq!(stored, registry.projects());
    }
}
