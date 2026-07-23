use mypi_agent::ToolExecutor;
use mypi_coding_agent::{
    load_skill_tool_definition, LoadSkillToolExecutor, SkillDiscoveryOptions,
    SkillDiscoveryWarningKind, SkillManager, SkillScope, LOAD_SKILL_TOOL_NAME,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn write_skill(directory: &Path, id: &str, description: &str, body: &str) {
    fs::create_dir_all(directory).unwrap();
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {id}\ndescription: {description}\ntags: [test]\n---\n{body}"),
    )
    .unwrap();
}

fn write_native_package(package: &Path, id: &str, body: &str) {
    fs::create_dir_all(package).unwrap();
    fs::write(
        package.join("mypi-package.json"),
        r#"{"enabled":true,"skills":["skills"]}"#,
    )
    .unwrap();
    write_skill(&package.join("skills").join(id), id, "native package", body);
}

fn write_pi_package(package: &Path, id: &str) {
    fs::create_dir_all(package).unwrap();
    fs::write(
        package.join("package.json"),
        r#"{"pi":{"skills":["skills"]}}"#,
    )
    .unwrap();
    write_skill(
        &package.join("skills").join(id),
        id,
        "Pi package skill",
        &format!("BODY_{id}"),
    );
}

#[test]
fn native_sources_are_preserved_with_deterministic_precedence() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();

    write_skill(
        &home.path().join(".agents/skills/global-agents"),
        "global-agents",
        "global agents",
        "global agents body",
    );
    write_skill(
        &home.path().join(".mypi/skills/global-mypi"),
        "global-mypi",
        "global mypi",
        "global mypi body",
    );
    write_native_package(
        &home.path().join(".mypi/packages/global-package"),
        "global-package",
        "global package body",
    );
    write_skill(
        &project.path().join(".agents/skills/project-agents"),
        "project-agents",
        "project agents",
        "project agents body",
    );
    write_skill(
        &project.path().join(".mypi/skills/project-mypi"),
        "project-mypi",
        "project mypi",
        "project mypi body",
    );
    write_native_package(
        &project.path().join(".mypi/packages/project-package"),
        "project-package",
        "project package body",
    );

    for root in [
        home.path().join(".agents/skills/duplicate"),
        home.path().join(".mypi/skills/duplicate"),
        project.path().join(".agents/skills/duplicate"),
        project.path().join(".mypi/skills/duplicate"),
    ] {
        write_skill(&root, "duplicate", "duplicate", &root.to_string_lossy());
    }
    write_native_package(
        &home.path().join(".mypi/packages/duplicate-global"),
        "duplicate",
        "global package wins globally",
    );
    write_native_package(
        &project.path().join(".mypi/packages/duplicate-project"),
        "duplicate",
        "project package wins",
    );

    let mut manager = SkillManager::new();
    let report = manager.discover_skills_with_options(SkillDiscoveryOptions::new(
        Some(project.path().to_path_buf()),
        Some(home.path().to_path_buf()),
    ));
    let scopes: BTreeMap<_, _> = manager
        .list_skills()
        .into_iter()
        .map(|skill| (skill.id, skill.scope))
        .collect();

    assert_eq!(scopes["global-agents"], SkillScope::GlobalAgents);
    assert_eq!(scopes["global-mypi"], SkillScope::GlobalMypi);
    assert_eq!(scopes["global-package"], SkillScope::GlobalPackage);
    assert_eq!(scopes["project-agents"], SkillScope::ProjectAgents);
    assert_eq!(scopes["project-mypi"], SkillScope::ProjectMypi);
    assert_eq!(scopes["project-package"], SkillScope::ProjectPackage);
    assert_eq!(scopes["duplicate"], SkillScope::ProjectPackage);
    assert_eq!(
        manager.get_skill_instructions("duplicate").unwrap(),
        "project package wins"
    );
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.kind == SkillDiscoveryWarningKind::DuplicateSkill));

    let ids: Vec<_> = report
        .skills
        .iter()
        .map(|skill| skill.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "duplicate",
            "global-agents",
            "global-mypi",
            "global-package",
            "project-agents",
            "project-mypi",
            "project-package",
        ]
    );
}

#[tokio::test]
async fn yaml_catalog_and_load_tool_are_lazy_and_strict() {
    let project = tempdir().unwrap();
    let skill_directory = project.path().join(".mypi/skills/yaml-skill");
    fs::create_dir_all(&skill_directory).unwrap();
    fs::write(
        skill_directory.join("SKILL.md"),
        r#"---
name: yaml-skill
description: >
  Diagnose quoted: "values"
  across multiple lines.
tags:
  - debugging
unknown-field: accepted
---
BODY_SENTINEL_ONLY_AFTER_LOAD
"#,
    )
    .unwrap();

    let mut manager = SkillManager::new();
    let report = manager.discover_skills_with_options(SkillDiscoveryOptions::new(
        Some(project.path().to_path_buf()),
        None,
    ));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(
        report.skills[0].description,
        "Diagnose quoted: \"values\" across multiple lines."
    );

    let catalog = manager.render_model_catalog();
    assert!(catalog.contains("`yaml-skill`"));
    assert!(catalog.contains("Diagnose quoted: \"values\" across multiple lines."));
    assert!(!catalog.contains("BODY_SENTINEL_ONLY_AFTER_LOAD"));
    assert!(!catalog.contains(&project.path().to_string_lossy().to_string()));

    let definition = load_skill_tool_definition();
    assert_eq!(definition.name, LOAD_SKILL_TOOL_NAME);
    assert_eq!(definition.strict, Some(true));
    assert_eq!(definition.parameters["additionalProperties"], false);

    let executor = LoadSkillToolExecutor::new(manager.snapshot());
    let result = executor
        .execute_tool(LOAD_SKILL_TOOL_NAME, r#"{"name":"yaml-skill"}"#)
        .await
        .unwrap()
        .unwrap();
    assert!(result.contains("BODY_SENTINEL_ONLY_AFTER_LOAD"));
    assert!(result.contains("untrusted task instructions"));
    assert!(!result.contains(&project.path().to_string_lossy().to_string()));

    assert!(executor
        .execute_tool(
            LOAD_SKILL_TOOL_NAME,
            r#"{"name":"yaml-skill","path":"/tmp/escape"}"#,
        )
        .await
        .unwrap()
        .is_err());
    assert!(executor
        .execute_tool(LOAD_SKILL_TOOL_NAME, r#"{"name":"YAML-SKILL"}"#)
        .await
        .unwrap()
        .is_err());
    assert!(executor.execute_tool("other_tool", "{}").await.is_none());
}

#[test]
fn discovers_only_direct_and_enabled_declared_pi_skills() {
    let home = tempdir().unwrap();
    let project = tempdir().unwrap();

    write_skill(
        &home.path().join(".pi/agent/skills/pi-global"),
        "pi-global",
        "global Pi",
        "global Pi body",
    );
    write_skill(
        &project.path().join(".pi/skills/pi-project"),
        "pi-project",
        "project Pi",
        "project Pi body",
    );

    let npm_root = home.path().join(".pi/agent/npm/node_modules");
    write_pi_package(&npm_root.join("plain"), "pi-npm");
    write_pi_package(&npm_root.join("@scope/scoped"), "pi-scoped");
    write_pi_package(&npm_root.join("disabled"), "pi-disabled");
    write_pi_package(&npm_root.join("undeclared"), "pi-undeclared");
    write_pi_package(
        &home
            .path()
            .join(".pi/agent/git/github.com/owner/repository"),
        "pi-git",
    );

    let settings_directory = home.path().join(".pi/agent");
    fs::create_dir_all(&settings_directory).unwrap();
    fs::write(
        settings_directory.join("settings.json"),
        r#"{
  "packages": [
    "npm:plain@1.0.0",
    "npm:@scope/scoped@2.0.0",
    "git:https://github.com/owner/repository.git#main",
    {"source":"npm:disabled","enabled":false}
  ]
}"#,
    )
    .unwrap();

    let mut manager = SkillManager::new();
    let report = manager.discover_skills_with_options(SkillDiscoveryOptions::new(
        Some(project.path().to_path_buf()),
        Some(home.path().to_path_buf()),
    ));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let scopes: BTreeMap<_, _> = report
        .skills
        .into_iter()
        .map(|skill| (skill.id, skill.scope))
        .collect();
    assert_eq!(scopes["pi-global"], SkillScope::GlobalPi);
    assert_eq!(scopes["pi-project"], SkillScope::ProjectPi);
    assert_eq!(scopes["pi-npm"], SkillScope::GlobalPiPackage);
    assert_eq!(scopes["pi-scoped"], SkillScope::GlobalPiPackage);
    assert_eq!(scopes["pi-git"], SkillScope::GlobalPiPackage);
    assert!(!scopes.contains_key("pi-disabled"));
    assert!(!scopes.contains_key("pi-undeclared"));
}

#[test]
fn rejects_declared_path_escapes_and_oversized_skills() {
    let home = tempdir().unwrap();
    let package = home
        .path()
        .join(".pi/agent/npm/node_modules/escape-package");
    let outside = home.path().join("outside");
    write_skill(&outside, "escaped", "escaped", "must not load");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("package.json"),
        r#"{"pi":{"skills":["../outside","skills"]}}"#,
    )
    .unwrap();
    write_skill(
        &package.join("skills/oversized"),
        "oversized",
        "too large",
        &"x".repeat(256),
    );
    fs::create_dir_all(home.path().join(".pi/agent")).unwrap();
    fs::write(
        home.path().join(".pi/agent/settings.json"),
        r#"{"packages":["npm:escape-package"]}"#,
    )
    .unwrap();

    let mut options = SkillDiscoveryOptions::new(None, Some(home.path().to_path_buf()));
    options.max_skill_bytes = 128;
    let mut manager = SkillManager::new();
    let report = manager.discover_skills_with_options(options);

    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.kind == SkillDiscoveryWarningKind::PathEscape));
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.kind == SkillDiscoveryWarningKind::InvalidSkill));
    let oversized = report
        .skills
        .iter()
        .find(|skill| skill.id == "oversized")
        .unwrap();
    assert!(!oversized.enabled);
    assert!(manager.get_skill_instructions("oversized").is_err());
    assert!(manager.get_skill_instructions("escaped").is_err());
}

#[test]
fn load_revalidates_size_id_and_containment() {
    let project = tempdir().unwrap();
    let directory = project.path().join(".agents/skills/mutable");
    write_skill(&directory, "mutable", "mutable", "initial body");

    let mut options = SkillDiscoveryOptions::new(Some(project.path().to_path_buf()), None);
    options.max_skill_bytes = 160;
    let mut manager = SkillManager::new();
    manager.discover_skills_with_options(&options);
    assert_eq!(
        manager.get_skill_instructions("mutable").unwrap(),
        "initial body"
    );

    fs::write(
        directory.join("SKILL.md"),
        format!(
            "---\nname: mutable\ndescription: changed\n---\n{}",
            "z".repeat(200)
        ),
    )
    .unwrap();
    assert!(manager
        .get_skill_instructions("mutable")
        .unwrap_err()
        .contains("limit"));

    write_skill(&directory, "renamed", "changed ID", "small again");
    assert!(manager
        .get_skill_instructions("mutable")
        .unwrap_err()
        .contains("declared ID"));

    fs::remove_file(directory.join("SKILL.md")).unwrap();
    assert!(manager.get_skill_instructions("mutable").is_err());
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_native_packages_root_outside_project() {
    use std::os::unix::fs::symlink;

    let project = tempdir().unwrap();
    let outside = tempdir().unwrap();
    write_native_package(
        &outside.path().join("escaped-package"),
        "escaped-native-package",
        "outside body",
    );
    fs::create_dir_all(project.path().join(".mypi")).unwrap();
    symlink(outside.path(), project.path().join(".mypi/packages")).unwrap();

    let mut manager = SkillManager::new();
    let report = manager.discover_skills_with_options(SkillDiscoveryOptions::new(
        Some(project.path().to_path_buf()),
        None,
    ));

    assert!(manager.list_skills().is_empty());
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.kind == SkillDiscoveryWarningKind::PathEscape));
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_enabled_pi_package_outside_installation_root() {
    use std::os::unix::fs::symlink;

    let home = tempdir().unwrap();
    let outside = home.path().join("private-package");
    write_pi_package(&outside, "escaped-pi-package");

    let package_parent = home.path().join(".pi/agent/npm/node_modules");
    fs::create_dir_all(&package_parent).unwrap();
    symlink(&outside, package_parent.join("linked-package")).unwrap();
    fs::write(
        home.path().join(".pi/agent/settings.json"),
        r#"{"packages":["npm:linked-package"]}"#,
    )
    .unwrap();

    let mut manager = SkillManager::new();
    let report = manager.discover_skills_with_options(SkillDiscoveryOptions::new(
        None,
        Some(home.path().to_path_buf()),
    ));

    assert!(manager.list_skills().is_empty());
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.kind == SkillDiscoveryWarningKind::PathEscape));
}

#[cfg(unix)]
#[test]
fn rejects_symlink_escape_from_direct_skill_root() {
    use std::os::unix::fs::symlink;

    let project = tempdir().unwrap();
    let outside = tempdir().unwrap();
    write_skill(outside.path(), "outside-skill", "outside", "outside body");
    let skills_root = project.path().join(".agents/skills");
    fs::create_dir_all(&skills_root).unwrap();
    symlink(outside.path(), skills_root.join("linked-skill")).unwrap();

    let mut manager = SkillManager::new();
    let report = manager.discover_skills_with_options(SkillDiscoveryOptions::new(
        Some(project.path().to_path_buf()),
        None,
    ));
    assert!(manager.list_skills().is_empty());
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.kind == SkillDiscoveryWarningKind::PathEscape));
}
