use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

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

#[derive(Debug, Default)]
struct ParsedFrontmatter {
    name: String,
    description: String,
    tools: Option<Vec<String>>,
    model: Option<String>,
}

fn parse_agent_frontmatter(content: &str) -> (ParsedFrontmatter, Option<String>, String) {
    let mut meta = ParsedFrontmatter::default();
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (
            meta,
            Some("Missing frontmatter delimiter '---'".into()),
            content.to_string(),
        );
    }

    let rest = &trimmed[3..];
    let end_idx = match rest.find("---") {
        Some(idx) => idx,
        None => {
            return (
                meta,
                Some("Unclosed frontmatter delimiter '---'".into()),
                content.to_string(),
            )
        }
    };

    let yaml_block = &rest[..end_idx];
    let body = rest[end_idx + 3..].trim().to_string();

    for line in yaml_block.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((key, value)) = line.split_once(':') {
            let k = key.trim();
            let v = value.trim().trim_matches('"').trim_matches('\'');
            match k {
                "name" => meta.name = v.to_string(),
                "description" => meta.description = v.to_string(),
                "model" => {
                    meta.model = if v.is_empty() {
                        None
                    } else {
                        Some(v.to_string())
                    }
                }
                "tools" => {
                    let tools: Vec<String> = v
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    meta.tools = if tools.is_empty() { None } else { Some(tools) };
                }
                _ => {}
            }
        }
    }

    let mut err = None;
    if meta.name.is_empty() {
        err = Some("Missing 'name' field in frontmatter".into());
    } else if meta.description.is_empty() {
        err = Some("Missing 'description' field in frontmatter".into());
    }

    (meta, err, body)
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

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() && !path.is_symlink() {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
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

fn find_nearest_project_agents_dir(cwd: &Path) -> Option<PathBuf> {
    let mut current = cwd.to_path_buf();
    loop {
        let candidate_mypi = current.join(".mypi").join("agents");
        if candidate_mypi.is_dir() {
            return Some(candidate_mypi);
        }
        let candidate_agents = current.join(".agents").join("agents");
        if candidate_agents.is_dir() {
            return Some(candidate_agents);
        }

        if !current.pop() {
            return None;
        }
    }
}

pub fn discover_agents(cwd: &Path, scope: AgentScope) -> AgentDiscoveryResult {
    let home = dirs_home();
    let user_dirs = home
        .map(|h| {
            vec![
                h.join(".mypi").join("agents"),
                h.join(".agents").join("agents"),
            ]
        })
        .unwrap_or_default();
    let project_agents_dir = find_nearest_project_agents_dir(cwd);

    let mut user_agents = Vec::new();
    if scope != AgentScope::Project {
        for udir in user_dirs {
            user_agents.extend(load_agents_from_dir(&udir, AgentSource::User));
        }
    }

    let mut project_agents = Vec::new();
    if scope != AgentScope::User {
        if let Some(ref pdir) = project_agents_dir {
            project_agents.extend(load_agents_from_dir(pdir, AgentSource::Project));
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
    std::env::var_os("HOME").map(PathBuf::from)
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
}
