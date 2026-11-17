//! Skill inventory settings for host applications.
//!
//! Thin wrappers over [`SkillManager`] and [`SkillSettings`] scoping every
//! operation to a project root.

use std::path::Path;

use crate::{SkillManager, SkillMetadata, SkillSettings};

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
