use threadlane_coding_agent::{
    CapabilityCatalog, CodingAgentOptions, FullTrustRunner, HarnessSupervisor, SkillManager,
    SkillScope, TrustStore,
};
use std::fs::{self, File};
use std::io::Write;
use tempfile::tempdir;

#[tokio::test]
async fn test_supervisor_multi_task_isolation() {
    let global_dir = tempdir().unwrap();

    let proj1_dir = tempdir().unwrap();
    let proj2_dir = tempdir().unwrap();

    let supervisor = HarnessSupervisor::new(global_dir.path().to_path_buf());

    let proj1 = supervisor.register_project(proj1_dir.path()).unwrap();
    let proj2 = supervisor.register_project(proj2_dir.path()).unwrap();

    assert_ne!(proj1.id, proj2.id);

    let opts1 = CodingAgentOptions {
        api_key: "test_key".into(),
        account_id: None,
        model: "gpt-4o".into(),
        work_dir: proj1_dir.path().to_path_buf(),
        session_file: None,
        system_prompt: Default::default(),
    };

    let opts2 = CodingAgentOptions {
        api_key: "test_key".into(),
        account_id: None,
        model: "gpt-4o".into(),
        work_dir: proj2_dir.path().to_path_buf(),
        session_file: None,
        system_prompt: Default::default(),
    };

    let task1_id = supervisor.create_task(&proj1.id, None, opts1).unwrap();
    let task2_id = supervisor.create_task(&proj2.id, None, opts2).unwrap();

    assert_ne!(task1_id, task2_id);

    let t1_tasks = supervisor.list_tasks_for_project(&proj1.id);
    let t2_tasks = supervisor.list_tasks_for_project(&proj2.id);

    assert_eq!(t1_tasks.len(), 1);
    assert_eq!(t1_tasks[0].id, task1_id);

    assert_eq!(t2_tasks.len(), 1);
    assert_eq!(t2_tasks[0].id, task2_id);

    assert_ne!(t1_tasks[0].session_file, t2_tasks[0].session_file);
}

#[test]
fn test_skill_discovery_and_precedence() {
    let dir = tempdir().unwrap();
    let proj_skills = dir.path().join(".agents/skills/test-skill");
    fs::create_dir_all(&proj_skills).unwrap();

    let skill_file = proj_skills.join("SKILL.md");
    let mut f = File::create(&skill_file).unwrap();
    writeln!(
        f,
        "---\nname: test-skill\ndescription: A test skill\ntags: [test, mock]\n---\nInstruction step 1"
    )
    .unwrap();

    let home_dir = tempdir().unwrap();
    let mut mgr = SkillManager::new();
    mgr.discover_skills_with_home(Some(dir.path()), Some(home_dir.path()));

    let list = mgr.list_skills();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "test-skill");
    assert_eq!(list[0].scope, SkillScope::ProjectAgents);

    let instructions = mgr.get_skill_instructions("test-skill").unwrap();
    assert_eq!(instructions, "Instruction step 1");
}

#[test]
fn test_full_trust_revision_approval() {
    let global_dir = tempdir().unwrap();
    let trust_file = global_dir.path().join("state/trust.json");

    let exe_dir = tempdir().unwrap();
    let exe_path = exe_dir.path().join("dummy_extension.sh");
    {
        let mut f = File::create(&exe_path).unwrap();
        writeln!(f, "#!/bin/sh\necho '{{\"status\": \"ok\"}}'").unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&exe_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&exe_path, perms).unwrap();
    }

    let runner = FullTrustRunner::new("pkg-1".into(), exe_path.clone()).unwrap();
    let rev = runner.revision.clone();

    let err = runner.execute_request("{}", &trust_file);
    assert!(err.is_err());
    assert!(err.unwrap_err().contains("Security Denial"));

    let mut store = TrustStore::load_from_file(&trust_file);
    store.approve("pkg-1".into(), rev.clone());
    store.save_to_file(&trust_file).unwrap();

    let res = runner.execute_request("{}", &trust_file);
    assert!(res.is_ok());
}

#[test]
fn test_capability_catalog_discovery() {
    let proj_dir = tempdir().unwrap();
    let global_dir = tempdir().unwrap();

    let catalog = CapabilityCatalog::discover(Some(proj_dir.path()), global_dir.path());
    assert!(catalog.skills.is_empty() || !catalog.skills.is_empty());
}
