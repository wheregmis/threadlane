use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_AGENT_DIRECTORY_ENTRIES: usize = 256;
const MAX_AGENT_DEFINITION_SIZE: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentScope {
    User,
    Project,
    Both,
}

impl Default for AgentScope {
    fn default() -> Self {
        AgentScope::User
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentSource {
    User,
    Project,
}

impl AgentSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentSource::User => "user",
            AgentSource::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub description: String,
    pub tools: Option<Vec<String>>,
    pub model: Option<String>,
    pub system_prompt: String,
    pub source: AgentSource,
    pub file_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDiscoveryResult {
    pub agents: Vec<AgentConfig>,
    pub project_agents_dir: Option<PathBuf>,
}

#[derive(Debug, Default, Clone)]
struct AgentFrontmatterMeta {
    name: String,
    description: String,
    tools: Option<Vec<String>>,
    model: Option<String>,
}

fn parse_agent_frontmatter(content: &str) -> (AgentFrontmatterMeta, Option<String>, String) {
    let parsed = crate::frontmatter::parse_frontmatter(content);
    let mut meta = AgentFrontmatterMeta::default();

    if let Some(err) = parsed.parse_error {
        return (meta, Some(err), parsed.body);
    }
    if parsed.metadata.is_empty() {
        return (
            meta,
            Some("Missing frontmatter delimiter '---'".into()),
            parsed.body,
        );
    }

    if let Some(name) = parsed.get("name") {
        meta.name = name.to_string();
    }
    if let Some(desc) = parsed.get("description") {
        meta.description = desc.to_string();
    }
    if let Some(model) = parsed.get("model") {
        meta.model = if model.is_empty() {
            None
        } else {
            Some(model.to_string())
        };
    }
    if let Some(tools_str) = parsed.get("tools") {
        let tools: Vec<String> = tools_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        meta.tools = if tools.is_empty() { None } else { Some(tools) };
    }

    let mut err = None;
    if meta.name.is_empty() {
        err = Some("Missing 'name' field in frontmatter".into());
    } else if meta.description.is_empty() {
        err = Some("Missing 'description' field in frontmatter".into());
    }

    (meta, err, parsed.body)
}

fn load_agents_from_dir(dir: &Path, source: AgentSource) -> Vec<AgentConfig> {
    let mut agents = Vec::new();
    if !dir.is_dir() {
        return agents;
    }

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return agents,
    };

    let mut paths = Vec::with_capacity(MAX_AGENT_DIRECTORY_ENTRIES + 1);
    for entry in entries.take(MAX_AGENT_DIRECTORY_ENTRIES + 1) {
        if let Ok(entry) = entry {
            paths.push(entry.path());
        }
    }
    if paths.len() > MAX_AGENT_DIRECTORY_ENTRIES {
        return agents;
    }
    paths.sort();

    for path in paths {
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }

        let metadata = match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_AGENT_DEFINITION_SIZE => {
                metadata
            }
            _ => continue,
        };
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(_) => continue,
        };
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        if file
            .take(MAX_AGENT_DEFINITION_SIZE + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > MAX_AGENT_DEFINITION_SIZE
        {
            continue;
        }
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => continue,
        };

        let (frontmatter, err, body) = parse_agent_frontmatter(&content);
        if err.is_some() || frontmatter.name.is_empty() {
            continue;
        }

        agents.push(AgentConfig {
            name: frontmatter.name,
            description: frontmatter.description,
            tools: frontmatter.tools,
            model: frontmatter.model,
            system_prompt: body,
            source,
            file_path: path,
        });
    }

    agents
}

fn find_nearest_project_agent_dirs(cwd: &Path) -> (Option<PathBuf>, Vec<PathBuf>) {
    let mut current = cwd.to_path_buf();
    loop {
        let candidate_agents = current.join(".agents").join("agents");
        let candidate_threadlane = current.join(".threadlane").join("agents");
        let mut directories = Vec::new();
        if candidate_agents.is_dir() {
            directories.push(candidate_agents.clone());
        }
        if candidate_threadlane.is_dir() {
            directories.push(candidate_threadlane.clone());
        }
        if !directories.is_empty() {
            return (
                candidate_threadlane
                    .is_dir()
                    .then_some(candidate_threadlane)
                    .or_else(|| candidate_agents.is_dir().then_some(candidate_agents)),
                directories,
            );
        }

        if !current.pop() {
            return (None, Vec::new());
        }
    }
}

pub fn discover_agents(cwd: &Path, scope: AgentScope) -> AgentDiscoveryResult {
    let home = dirs_home();
    let user_dirs = home
        .map(|h| {
            vec![
                h.join(".agents").join("agents"),
                h.join(".threadlane").join("agents"),
            ]
        })
        .unwrap_or_default();
    let (project_agents_dir, project_agent_dirs) = find_nearest_project_agent_dirs(cwd);

    let mut user_agents = Vec::new();
    if scope != AgentScope::Project {
        for udir in user_dirs {
            user_agents.extend(load_agents_from_dir(&udir, AgentSource::User));
        }
    }

    let mut project_agents = Vec::new();
    if scope != AgentScope::User {
        for directory in project_agent_dirs {
            project_agents.extend(load_agents_from_dir(&directory, AgentSource::Project));
        }
    }

    let mut agent_map: HashMap<String, AgentConfig> = HashMap::new();

    if scope == AgentScope::Both {
        for a in user_agents {
            agent_map.insert(a.name.clone(), a);
        }
        for a in project_agents {
            agent_map.insert(a.name.clone(), a);
        }
    } else if scope == AgentScope::User {
        for a in user_agents {
            agent_map.insert(a.name.clone(), a);
        }
    } else {
        for a in project_agents {
            agent_map.insert(a.name.clone(), a);
        }
    }

    let mut result_agents: Vec<AgentConfig> = agent_map.into_values().collect();
    result_agents.sort_by(|a, b| a.name.cmp(&b.name));

    AgentDiscoveryResult {
        agents: result_agents,
        project_agents_dir,
    }
}

fn dirs_home() -> Option<PathBuf> {
    directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_agent_frontmatter() {
        let content = r#"---
name: scout
description: Fast recon agent
tools: read_file, list_dir, grep_search
model: gpt-4o-mini
---

You are a scout agent. Explore the codebase and report back.
"#;

        let (meta, err, body) = parse_agent_frontmatter(content);
        assert!(err.is_none());
        assert_eq!(meta.name, "scout");
        assert_eq!(meta.description, "Fast recon agent");
        assert_eq!(
            meta.tools,
            Some(vec![
                "read_file".to_string(),
                "list_dir".to_string(),
                "grep_search".to_string()
            ])
        );
        assert_eq!(meta.model, Some("gpt-4o-mini".to_string()));
        assert_eq!(
            body,
            "You are a scout agent. Explore the codebase and report back."
        );
    }

    #[test]
    fn oversized_agent_definition_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("valid.md"),
            "---\nname: valid\ndescription: valid agent\n---\nValid.",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("oversized.md"),
            vec![b'a'; MAX_AGENT_DEFINITION_SIZE as usize + 1],
        )
        .unwrap();

        let agents = load_agents_from_dir(dir.path(), AgentSource::Project);

        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "valid");
    }

    #[test]
    fn directory_over_entry_limit_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..=MAX_AGENT_DIRECTORY_ENTRIES {
            let name = format!("agent-{index:03}");
            std::fs::write(
                dir.path().join(format!("{name}.md")),
                format!("---\nname: {name}\ndescription: test agent\n---\nTest."),
            )
            .unwrap();
        }

        let agents = load_agents_from_dir(dir.path(), AgentSource::Project);

        assert!(agents.is_empty());
    }

    #[test]
    fn agent_definitions_are_loaded_in_filename_order() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["charlie", "alpha", "bravo"] {
            std::fs::write(
                dir.path().join(format!("{name}.md")),
                format!("---\nname: {name}\ndescription: test agent\n---\nTest."),
            )
            .unwrap();
        }

        let agents = load_agents_from_dir(dir.path(), AgentSource::Project);

        assert_eq!(
            agents
                .iter()
                .map(|agent| agent.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "bravo", "charlie"]
        );
    }

    #[test]
    fn project_agent_locations_are_merged_with_threadlane_precedence() {
        let project = tempfile::tempdir().unwrap();
        let agents_dir = project.path().join(".agents/agents");
        let threadlane_dir = project.path().join(".threadlane/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::create_dir_all(&threadlane_dir).unwrap();
        std::fs::write(
            agents_dir.join("scout.md"),
            "---\nname: scout\ndescription: agents scout\n---\nAgents scout.",
        )
        .unwrap();
        std::fs::write(
            agents_dir.join("reviewer.md"),
            "---\nname: reviewer\ndescription: reviewer\n---\nReviewer.",
        )
        .unwrap();
        std::fs::write(
            threadlane_dir.join("scout.md"),
            "---\nname: scout\ndescription: threadlane scout\n---\nThreadlane scout.",
        )
        .unwrap();
        std::fs::write(
            threadlane_dir.join("worker.md"),
            "---\nname: worker\ndescription: worker\n---\nWorker.",
        )
        .unwrap();

        let result = discover_agents(project.path(), AgentScope::Project);
        assert_eq!(
            result
                .agents
                .iter()
                .map(|agent| agent.name.as_str())
                .collect::<Vec<_>>(),
            vec!["reviewer", "scout", "worker"]
        );
        assert_eq!(
            result
                .agents
                .iter()
                .find(|agent| agent.name == "scout")
                .unwrap()
                .description,
            "threadlane scout"
        );
    }
}
