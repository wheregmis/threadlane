use crate::{SkillMetadata, SkillRecord, SkillScope};
use std::path::PathBuf;

fn record(id: &str, description: &str, instructions: &'static str) -> SkillRecord {
    SkillRecord {
        metadata: SkillMetadata {
            id: id.into(),
            name: id.into(),
            description: description.into(),
            tags: vec!["workflow".into()],
            file_path: PathBuf::new(),
            scope: SkillScope::Builtin,
            enabled: true,
            is_valid: true,
            validation_error: None,
        },
        allowed_root: PathBuf::new(),
        embedded_instructions: Some(instructions),
    }
}

pub(super) fn records() -> Vec<SkillRecord> {
    vec![
        record(
            "threadlane-planning",
            "Before nontrivial implementation or assigning edits to a worker: settle design and write an implementation-ready task plan. Not needed for tiny direct edits or read-only research.",
            include_str!("workflows/planning.md"),
        ),
        record(
            "threadlane-debugging",
            "When investigating a bug, failing test, or unexpected behavior: reproduce, trace the cause, test a hypothesis, and fix with regression coverage.",
            include_str!("workflows/debugging.md"),
        ),
        record(
            "threadlane-executing-plans",
            "When implementing an agreed plan directly or as a worker: follow scoped steps, use regression tests, escalate missing context, and report evidence.",
            include_str!("workflows/executing-plans.md"),
        ),
        record(
            "threadlane-verification-review",
            "Before accepting worker output or claiming completion: review against requirements and inspect current verification evidence; triage review feedback before fixes.",
            include_str!("workflows/verification-review.md"),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::records;
    use crate::{
        LoadSkillToolExecutor, SkillDiscoveryOptions, SkillManager, SkillScope, SkillSettings,
    };
    use std::fs;

    #[test]
    fn builtin_workflows_load_without_files_and_catalog_contains_only_metadata() {
        let mut manager = SkillManager::new();
        manager.discover_skills_with_options(SkillDiscoveryOptions::new(None, None));
        let catalog = manager.render_model_catalog();
        for record in records() {
            let id = &record.metadata.id;
            assert!(catalog.contains(id));
            let body = manager.get_skill_instructions(id).unwrap();
            assert_eq!(body, record.embedded_instructions.unwrap());
            assert!(!catalog.contains(&body));
            let executor = LoadSkillToolExecutor::new(manager.snapshot());
            let loaded = executor
                .execute("load_skill", &format!("{{\"name\":\"{id}\"}}"))
                .unwrap()
                .unwrap();
            assert!(loaded.contains(&body));
        }
    }

    #[test]
    fn builtin_workflows_honor_project_disable_and_file_override() {
        let project = tempfile::tempdir().unwrap();
        let id = "threadlane-planning";
        let options = || SkillDiscoveryOptions::new(Some(project.path().to_path_buf()), None);
        let mut manager = SkillManager::new();
        SkillSettings::load(project.path())
            .set_enabled(project.path(), id, false)
            .unwrap();
        manager.discover_skills_with_options(options());
        assert!(!manager.render_model_catalog().contains(id));
        assert!(manager.get_skill_instructions(id).is_err());
        let executor = LoadSkillToolExecutor::new(manager.snapshot());
        assert!(executor
            .execute("load_skill", &format!("{{\"name\":\"{id}\"}}"))
            .unwrap()
            .is_err());
        assert!(manager
            .list_skills()
            .iter()
            .any(|s| s.id == id && !s.enabled && s.scope == SkillScope::Builtin));

        SkillSettings::load(project.path())
            .set_enabled(project.path(), id, true)
            .unwrap();
        let dir = project.path().join(".threadlane/skills/planning");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {id}\ndescription: Local planning\n---\nLocal rules"),
        )
        .unwrap();
        manager.discover_skills_with_options(options());
        assert_eq!(manager.get_skill_instructions(id).unwrap(), "Local rules");
        assert!(manager
            .list_skills()
            .iter()
            .any(|s| s.id == id && s.scope == SkillScope::ProjectThreadlane));
    }
}
