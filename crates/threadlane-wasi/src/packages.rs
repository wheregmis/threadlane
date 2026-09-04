use crate::WasiExtension;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtensionScope {
    Global,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtensionLayout {
    Loose,
    Managed,
}

#[derive(Debug, Clone)]
pub struct ExtensionRecord {
    id: String,
    name: String,
    version: String,
    module_path: PathBuf,
    scope: ExtensionScope,
    enabled: bool,
    effective: bool,
    layout: ExtensionLayout,
    extensions_root: PathBuf,
}

impl ExtensionRecord {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn module_path(&self) -> &Path {
        &self.module_path
    }

    pub fn scope(&self) -> ExtensionScope {
        self.scope
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn is_effective(&self) -> bool {
        self.effective
    }
}

pub fn default_global_threadlane_dir() -> Option<PathBuf> {
    threadlane_runtime::utils::dirs_home().map(|home| home.join(".threadlane"))
}

pub struct ExtensionManager {
    global_threadlane_dir: Option<PathBuf>,
    project_root: Option<PathBuf>,
}

impl ExtensionManager {
    pub fn new(global_threadlane_dir: Option<PathBuf>, project_root: Option<PathBuf>) -> Self {
        Self {
            global_threadlane_dir,
            project_root,
        }
    }

    pub fn install_from_wasm(
        &self,
        source: &Path,
        scope: ExtensionScope,
    ) -> Result<ExtensionRecord, String> {
        let source = source
            .canonicalize()
            .map_err(|error| format!("Failed to resolve extension module: {error}"))?;
        if !source.is_file() || source.extension() != Some(OsStr::new("wasm")) {
            return Err("Extension source must be a compiled .wasm file".into());
        }

        let extension = WasiExtension::load_from_file_requiring_manifest(&source)?;
        validate_extension_id(&extension.manifest.name)?;
        let extensions_root = self.resolve_extensions_root(scope, true)?.ok_or_else(|| {
            format!(
                "{} extension root is unavailable",
                match scope {
                    ExtensionScope::Global => "Global",
                    ExtensionScope::Project => "Project",
                }
            )
        })?;
        let id = extension.manifest.name.clone();
        let target = extensions_root.join(format!("{id}.wasm"));
        let existing_records = self.discover();
        let managed_replacements = existing_records
            .iter()
            .filter(|record| {
                record.scope == scope
                    && record.name == extension.manifest.name
                    && record.layout == ExtensionLayout::Managed
            })
            .cloned()
            .collect::<Vec<_>>();
        for record in &managed_replacements {
            self.validate_record(record)?;
        }
        match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("Extension destination must not be a symbolic link".into())
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err("Extension destination must be a file".into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Failed to inspect extension destination: {error}")),
        }
        let marker = disabled_marker(&target);
        let (target_marker_exists, mut enabled) = match fs::symlink_metadata(&marker) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("Extension disable marker must not be a symbolic link".into())
            }
            Ok(metadata) if metadata.is_file() => (true, false),
            Ok(_) => return Err("Extension disable marker must be a file".into()),
            Err(error) if error.kind() == ErrorKind::NotFound => (false, true),
            Err(error) => {
                return Err(format!(
                    "Failed to inspect extension disable marker: {error}"
                ))
            }
        };
        if managed_replacements.iter().any(|record| !record.enabled) {
            enabled = false;
        }
        let effective = enabled
            && (scope == ExtensionScope::Project
                || !existing_records.iter().any(|record| {
                    record.scope == ExtensionScope::Project
                        && record.name == extension.manifest.name
                        && record.enabled
                }));
        let installed_record = ExtensionRecord {
            id: id.clone(),
            name: extension.manifest.name.clone(),
            version: extension.manifest.version.clone(),
            module_path: target.clone(),
            scope,
            enabled,
            effective,
            layout: ExtensionLayout::Loose,
            extensions_root: extensions_root.clone(),
        };

        let mut source_file = fs::File::open(&source)
            .map_err(|error| format!("Failed to open extension module: {error}"))?;
        let (staged, mut staged_file) = create_staging_file(&extensions_root, &id)?;
        if let Err(error) = io::copy(&mut source_file, &mut staged_file) {
            drop(staged_file);
            let _ = fs::remove_file(&staged);
            return Err(format!("Failed to stage extension module: {error}"));
        }
        drop(staged_file);
        let created_marker = if !enabled && !target_marker_exists {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
            {
                Ok(_) => true,
                Err(error) => {
                    let _ = fs::remove_file(&staged);
                    return Err(format!(
                        "Failed to preserve disabled extension state: {error}"
                    ));
                }
            }
        } else {
            false
        };
        let result = (|| {
            WasiExtension::load_from_file_requiring_manifest(&staged)?;
            let mut managed_backups = Vec::new();
            for record in &managed_replacements {
                let original = record
                    .module_path
                    .parent()
                    .ok_or_else(|| "Managed extension has no package directory".to_string())?
                    .to_path_buf();
                let backup = available_path(&extensions_root, &record.id, "backup");
                if let Err(error) = fs::rename(&original, &backup) {
                    restore_directories(&managed_backups)?;
                    return Err(format!("Failed to back up managed extension: {error}"));
                }
                managed_backups.push((original, backup));
            }
            if let Err(error) = replace_file(&staged, &target, &extensions_root, &id) {
                restore_directories(&managed_backups)?;
                return Err(error);
            }
            for (_, backup) in managed_backups {
                let _ = fs::remove_dir_all(backup);
            }
            Ok(installed_record)
        })();
        if staged.exists() {
            let _ = fs::remove_file(staged);
        }
        if result.is_err() && created_marker {
            let _ = fs::remove_file(marker);
        }
        result
    }

    pub fn discover(&self) -> Vec<ExtensionRecord> {
        let mut records = Vec::new();
        for scope in [ExtensionScope::Global, ExtensionScope::Project] {
            let Ok(Some(extensions_root)) = self.resolve_extensions_root(scope, false) else {
                continue;
            };
            let Ok(entries) = fs::read_dir(&extensions_root) else {
                continue;
            };
            let mut entries = entries.flatten().collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let file_name = entry.file_name();
                if file_name.to_string_lossy().starts_with('.') {
                    continue;
                }
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let (id, module_path, layout) = if file_type.is_file()
                    && entry.path().extension() == Some(OsStr::new("wasm"))
                {
                    (
                        entry
                            .path()
                            .file_stem()
                            .map(|stem| stem.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        entry.path(),
                        ExtensionLayout::Loose,
                    )
                } else if file_type.is_dir() {
                    (
                        file_name.to_string_lossy().into_owned(),
                        entry.path().join("extension.wasm"),
                        ExtensionLayout::Managed,
                    )
                } else {
                    continue;
                };
                let Ok(metadata) = fs::symlink_metadata(&module_path) else {
                    continue;
                };
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    continue;
                }
                let Ok(module_path) = module_path.canonicalize() else {
                    continue;
                };
                let Ok(extension) = WasiExtension::load_from_file(&module_path) else {
                    continue;
                };
                if validate_extension_id(&extension.manifest.name).is_err() {
                    continue;
                }
                records.push(ExtensionRecord {
                    id,
                    name: extension.manifest.name,
                    version: extension.manifest.version,
                    enabled: extension_enabled(&module_path),
                    effective: false,
                    module_path,
                    scope,
                    layout,
                    extensions_root: extensions_root.clone(),
                });
            }
        }

        let mut selected = HashMap::<String, usize>::new();
        let mut selected_project = HashSet::<String>::new();
        for (index, record) in records.iter().enumerate() {
            if !record.enabled {
                continue;
            }
            match record.scope {
                ExtensionScope::Global => {
                    selected.entry(record.name.clone()).or_insert(index);
                }
                ExtensionScope::Project if selected_project.insert(record.name.clone()) => {
                    selected.insert(record.name.clone(), index);
                }
                ExtensionScope::Project => {}
            }
        }
        for index in selected.into_values() {
            records[index].effective = true;
        }
        records
    }

    pub(crate) fn discover_checked(&self) -> Result<Vec<ExtensionRecord>, String> {
        for scope in [ExtensionScope::Global, ExtensionScope::Project] {
            let Some(extensions_root) = self.resolve_extensions_root(scope, false)? else {
                continue;
            };
            let entries = fs::read_dir(&extensions_root)
                .map_err(|error| format!("Failed to read extension root: {error}"))?;
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let file_type = entry
                    .file_type()
                    .map_err(|error| format!("Failed to inspect extension entry: {error}"))?;
                let module_path = if file_type.is_file()
                    && entry.path().extension() == Some(OsStr::new("wasm"))
                {
                    entry.path()
                } else if file_type.is_dir() {
                    entry.path().join("extension.wasm")
                } else {
                    continue;
                };
                let Ok(metadata) = fs::symlink_metadata(&module_path) else {
                    continue;
                };
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    continue;
                }
                if fs::symlink_metadata(disabled_marker(&module_path)).is_ok() {
                    continue;
                }
                let extension = WasiExtension::load_from_file(&module_path).map_err(|error| {
                    format!(
                        "Failed to load extension '{}': {error}",
                        module_path.display()
                    )
                })?;
                validate_extension_id(&extension.manifest.name).map_err(|error| {
                    format!(
                        "Invalid extension manifest name in '{}': {error}",
                        module_path.display()
                    )
                })?;
            }
        }
        Ok(self.discover())
    }

    pub fn set_enabled(&self, record: &ExtensionRecord, enabled: bool) -> Result<(), String> {
        self.validate_record(record)?;
        let marker = disabled_marker(&record.module_path);
        match fs::symlink_metadata(&marker) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("Extension disable marker must not be a symbolic link".into())
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err("Extension disable marker must be a file".into())
            }
            Ok(_) if enabled => fs::remove_file(marker)
                .map_err(|error| format!("Failed to enable extension: {error}"))?,
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound && !enabled => {
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(marker)
                    .map_err(|error| format!("Failed to disable extension: {error}"))?;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Failed to inspect extension disable marker: {error}"
                ))
            }
        }
        Ok(())
    }

    pub fn remove(&self, record: &ExtensionRecord) -> Result<(), String> {
        self.validate_record(record)?;
        match record.layout {
            ExtensionLayout::Loose => {
                let marker = disabled_marker(&record.module_path);
                let marker_contents = match fs::symlink_metadata(&marker) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err("Extension disable marker must not be a symbolic link".into())
                    }
                    Ok(metadata) if !metadata.is_file() => {
                        return Err("Extension disable marker must be a file".into())
                    }
                    Ok(_) => Some(fs::read(&marker).map_err(|error| {
                        format!("Failed to read extension disable marker: {error}")
                    })?),
                    Err(error) if error.kind() == ErrorKind::NotFound => None,
                    Err(error) => {
                        return Err(format!(
                            "Failed to inspect extension disable marker: {error}"
                        ))
                    }
                };
                if marker_contents.is_some() {
                    fs::remove_file(&marker)
                        .map_err(|error| format!("Failed to remove extension marker: {error}"))?;
                }
                if let Err(error) = fs::remove_file(&record.module_path) {
                    if let Some(contents) = marker_contents {
                        fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&marker)
                            .and_then(|mut marker| marker.write_all(&contents))
                            .map_err(|restore_error| {
                            format!(
                                "Failed to remove extension: {error}; failed to restore disable marker: {restore_error}"
                            )
                        })?;
                    }
                    return Err(format!("Failed to remove extension: {error}"));
                }
            }
            ExtensionLayout::Managed => {
                let package_dir = record
                    .module_path
                    .parent()
                    .ok_or_else(|| "Managed extension has no package directory".to_string())?;
                let metadata = fs::symlink_metadata(package_dir)
                    .map_err(|error| format!("Failed to inspect managed extension: {error}"))?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err("Managed extension destination must be a directory".into());
                }
                let package_dir = package_dir
                    .canonicalize()
                    .map_err(|error| format!("Failed to resolve managed extension: {error}"))?;
                if package_dir.parent() != Some(record.extensions_root.as_path()) {
                    return Err(
                        "Managed extension must remain directly within its extension root".into(),
                    );
                }
                fs::remove_dir_all(package_dir)
                    .map_err(|error| format!("Failed to remove managed extension: {error}"))?;
            }
        }
        Ok(())
    }

    fn validate_record(&self, record: &ExtensionRecord) -> Result<(), String> {
        let Some(extensions_root) = self.resolve_extensions_root(record.scope, false)? else {
            return Err("Extension root no longer exists".into());
        };
        if extensions_root != record.extensions_root {
            return Err("Extension root changed after discovery".into());
        }
        let metadata = fs::symlink_metadata(&record.module_path)
            .map_err(|error| format!("Failed to inspect extension module: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("Extension module must be a regular file".into());
        }
        let module_path = record
            .module_path
            .canonicalize()
            .map_err(|error| format!("Failed to resolve extension module: {error}"))?;
        if module_path != record.module_path {
            return Err("Extension module changed after discovery".into());
        }
        let contained = match record.layout {
            ExtensionLayout::Loose => module_path.parent() == Some(extensions_root.as_path()),
            ExtensionLayout::Managed => {
                module_path.file_name() == Some(OsStr::new("extension.wasm"))
                    && module_path.parent().and_then(Path::parent)
                        == Some(extensions_root.as_path())
            }
        };
        if !contained {
            return Err("Extension module must remain directly within its extension root".into());
        }
        Ok(())
    }

    fn resolve_extensions_root(
        &self,
        scope: ExtensionScope,
        create: bool,
    ) -> Result<Option<PathBuf>, String> {
        match scope {
            ExtensionScope::Global => {
                let Some(threadlane_dir) = self.global_threadlane_dir.as_deref() else {
                    return Ok(None);
                };
                resolve_threadlane_extensions_dir(threadlane_dir, None, create)
            }
            ExtensionScope::Project => {
                let Some(project_root) = self.project_root.as_deref() else {
                    return Ok(None);
                };
                let project_root = project_root
                    .canonicalize()
                    .map_err(|error| format!("Failed to resolve project root: {error}"))?;
                if !project_root.is_dir() {
                    return Err("Project root must be a directory".into());
                }
                resolve_threadlane_extensions_dir(
                    &project_root.join(".threadlane"),
                    Some(&project_root),
                    create,
                )
            }
        }
    }
}

fn create_staging_file(parent: &Path, id: &str) -> Result<(PathBuf, fs::File), String> {
    for suffix in 0..1000 {
        let path = parent.join(format!(".{id}.staged-{}-{suffix}.wasm", std::process::id()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Failed to create extension staging file: {error}")),
        }
    }
    Err("Failed to allocate extension staging file".into())
}

fn available_path(parent: &Path, id: &str, kind: &str) -> PathBuf {
    for suffix in 0..1000 {
        let path = parent.join(format!(".{id}.{kind}-{}-{suffix}", std::process::id()));
        if !path.exists() {
            return path;
        }
    }
    parent.join(format!(".{id}.{kind}-{}", std::process::id()))
}

fn available_file_path(parent: &Path, id: &str, kind: &str) -> PathBuf {
    for suffix in 0..1000 {
        let path = parent.join(format!(".{id}.{kind}-{}-{suffix}.wasm", std::process::id()));
        if !path.exists() {
            return path;
        }
    }
    parent.join(format!(".{id}.{kind}-{}.wasm", std::process::id()))
}

fn disabled_marker(module_path: &Path) -> PathBuf {
    let mut marker = module_path.as_os_str().to_os_string();
    marker.push(".disabled");
    PathBuf::from(marker)
}

fn extension_enabled(module_path: &Path) -> bool {
    fs::symlink_metadata(disabled_marker(module_path))
        .is_err_and(|error| error.kind() == ErrorKind::NotFound)
}

fn restore_directories(backups: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    for (original, backup) in backups.iter().rev() {
        fs::rename(backup, original)
            .map_err(|error| format!("Failed to restore managed extension: {error}"))?;
    }
    Ok(())
}

fn replace_file(
    staged: &Path,
    target: &Path,
    extensions_root: &Path,
    id: &str,
) -> Result<(), String> {
    if target.exists() {
        let backup = available_file_path(extensions_root, id, "backup");
        fs::rename(target, &backup)
            .map_err(|error| format!("Failed to back up existing extension: {error}"))?;
        if let Err(error) = fs::rename(staged, target) {
            let restore = fs::rename(&backup, target);
            return Err(match restore {
                Ok(()) => format!("Failed to install extension replacement: {error}"),
                Err(restore_error) => format!(
                    "Failed to install extension replacement: {error}; failed to restore previous extension: {restore_error}"
                ),
            });
        }
        let _ = fs::remove_file(backup);
    } else {
        fs::rename(staged, target)
            .map_err(|error| format!("Failed to install extension: {error}"))?;
    }
    Ok(())
}

fn resolve_threadlane_extensions_dir(
    threadlane_dir: &Path,
    containing_root: Option<&Path>,
    create: bool,
) -> Result<Option<PathBuf>, String> {
    if let Some(root) = containing_root {
        if threadlane_dir.parent() != Some(root) {
            return Err("Project extension root must remain within the project".into());
        }
    }

    let extensions_dir = threadlane_dir.join("extensions");
    for directory in [threadlane_dir, extensions_dir.as_path()] {
        match fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Extension destination component '{}' must not be a symbolic link",
                    directory.display()
                ))
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "Extension destination component '{}' must be a directory",
                    directory.display()
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound && create => {
                fs::create_dir(directory).map_err(|error| {
                    format!(
                        "Failed to create extension destination '{}': {error}",
                        directory.display()
                    )
                })?;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "Failed to inspect extension destination '{}': {error}",
                    directory.display()
                ))
            }
        }
    }

    let resolved_threadlane = threadlane_dir
        .canonicalize()
        .map_err(|error| format!("Failed to resolve Threadlane directory: {error}"))?;
    if let Some(root) = containing_root {
        if resolved_threadlane.parent() != Some(root) {
            return Err("Project extension root must remain within the project".into());
        }
    }

    let resolved_extensions = extensions_dir
        .canonicalize()
        .map_err(|error| format!("Failed to resolve extension root: {error}"))?;
    if resolved_extensions.parent() != Some(resolved_threadlane.as_path()) {
        return Err("Extension root must remain within its Threadlane directory".into());
    }
    Ok(Some(resolved_extensions))
}

const MAX_EXTENSION_ID_LEN: usize = 128;

pub(crate) fn validate_extension_id(id: &str) -> Result<(), String> {
    if id.len() > MAX_EXTENSION_ID_LEN {
        return Err(format!(
            "Extension name must not exceed {MAX_EXTENSION_ID_LEN} ASCII characters"
        ));
    }
    let mut chars = id.bytes();
    let Some(first) = chars.next() else {
        return Err("Extension name must not be empty".into());
    };
    if !first.is_ascii_alphanumeric()
        || !chars.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("Extension name must begin with an ASCII letter or digit and contain only ASCII letters, digits, '-', '_', or '.'".into());
    }
    Ok(())
}
