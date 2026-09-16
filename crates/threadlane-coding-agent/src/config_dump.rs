use std::path::PathBuf;

use crate::{CodingAgentOptions, HarnessCompositionSnapshot};
use threadlane_skills::SkillManager;
use threadlane_wasi::WasiExtensionManager;

pub fn dump_config(args: &[String]) -> Result<(), String> {
    let project_index = args
        .iter()
        .position(|arg| arg == "--project")
        .ok_or_else(|| "--dump-config requires --project <path>".to_string())?;
    let project = args
        .get(project_index + 1)
        .ok_or_else(|| "--project requires a path".to_string())?;
    let project = PathBuf::from(project)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let session_file = args
        .iter()
        .position(|arg| arg == "--session")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    let model = args
        .iter()
        .position(|arg| arg == "--model")
        .and_then(|index| args.get(index + 1))
        .cloned()
        .unwrap_or_else(|| "gpt-4o".into());
    let options = CodingAgentOptions {
        api_key: String::new(),
        account_id: None,
        model,
        work_dir: project.clone(),
        session_file,
        system_prompt: Default::default(),
        agent_config: None,
        coding_config: None,
        browser: threadlane_protocol::browser::BrowserBridge::unavailable(),
    };
    let mut manager = SkillManager::new();
    manager.discover_skills(Some(&project));
    let skills = manager.snapshot();
    let extensions = WasiExtensionManager::for_project_session(&project, "dump-config");
    extensions
        .reload_from_roots(
            threadlane_project::default_global_threadlane_dir().as_deref(),
            Some(&project),
        )
        .map_err(|error| error.to_string())?;
    let snapshot = HarnessCompositionSnapshot::resolved(&options, &skills, &extensions);
    println!("active_lane={}", snapshot.active_lane);
    println!(
        "session_file={}",
        snapshot.session_file.unwrap_or_else(|| "<none>".into())
    );
    println!("model={}", snapshot.model);
    println!("provider={}", snapshot.provider);
    println!("skills={}", snapshot.skills.join(","));
    println!("extensions={}", snapshot.extensions.join(","));
    println!("sandbox={}", snapshot.sandbox_policy);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::dump_config;

    #[test]
    fn requires_project_path() {
        let error = dump_config(&["app".into(), "--dump-config".into()]).unwrap_err();
        assert_eq!(error, "--dump-config requires --project <path>");
    }
}
