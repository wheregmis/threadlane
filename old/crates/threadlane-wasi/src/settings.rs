//! Extension inventory settings for host applications.
//!
//! Thin wrappers over [`ExtensionManager`] scoping every operation to an
//! optional project root: global `~/.threadlane` state plus the project's
//! own `.threadlane` overlay.

use std::path::{Path, PathBuf};

use crate::packages::{ExtensionManager, ExtensionRecord, ExtensionScope};
use threadlane_project::default_global_threadlane_dir;

fn extension_manager(project_root: Option<PathBuf>) -> ExtensionManager {
    ExtensionManager::new(default_global_threadlane_dir(), project_root)
}

pub fn discover_extensions(project_root: Option<PathBuf>) -> Vec<ExtensionRecord> {
    extension_manager(project_root).discover()
}

pub fn install_extension(
    project_root: Option<PathBuf>,
    source: &Path,
    scope: ExtensionScope,
) -> Result<String, String> {
    let record = extension_manager(project_root).install_from_wasm(source, scope)?;
    Ok(format!(
        "Installed {} v{}.",
        record.name(),
        record.version()
    ))
}

pub fn set_extension_enabled(
    project_root: Option<PathBuf>,
    target: &ExtensionRecord,
    enabled: bool,
) -> Result<(), String> {
    let manager = extension_manager(project_root);
    let current = manager
        .discover()
        .into_iter()
        .find(|record| {
            record.id() == target.id()
                && record.scope() == target.scope()
                && record.module_path() == target.module_path()
        })
        .ok_or_else(|| "Extension inventory changed. Please try again.".to_string())?;
    manager.set_enabled(&current, enabled)
}

pub fn remove_extension(
    project_root: Option<PathBuf>,
    target: &ExtensionRecord,
) -> Result<(), String> {
    let manager = extension_manager(project_root);
    let current = manager
        .discover()
        .into_iter()
        .find(|record| {
            record.id() == target.id()
                && record.scope() == target.scope()
                && record.module_path() == target.module_path()
        })
        .ok_or_else(|| "Extension inventory changed. Please try again.".to_string())?;
    manager.remove(&current)
}
