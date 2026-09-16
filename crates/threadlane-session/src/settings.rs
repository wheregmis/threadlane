use crate::{
    default_global_threadlane_dir, ExtensionManager, ExtensionRecord, ExtensionScope, SkillManager,
    SkillMetadata, SkillSettings,
};
use std::path::{Path, PathBuf};

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
pub fn discover_skills(project_root: Option<&Path>) -> Vec<SkillMetadata> {
    let mut manager = SkillManager::new();
    manager.discover_skills(project_root);
    manager.list_skills()
}
pub fn set_skill_enabled(project_root: &Path, skill_id: &str, enabled: bool) -> Result<(), String> {
    SkillSettings::load(project_root).set_enabled(project_root, skill_id, enabled)
}
pub fn disable_all_skills(
    project_root: &Path,
    skill_ids: impl IntoIterator<Item = String>,
) -> Result<(), String> {
    let mut settings = SkillSettings::load(project_root);
    settings.disable_all(project_root, skill_ids)
}
