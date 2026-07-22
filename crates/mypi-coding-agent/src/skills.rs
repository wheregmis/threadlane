use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillScope {
    GlobalAgents,
    GlobalMypi,
    GlobalPackage,
    ProjectAgents,
    ProjectMypi,
    ProjectPackage,
}

impl SkillScope {
    pub fn precedence(self) -> u8 {
        match self {
            SkillScope::GlobalAgents => 1,
            SkillScope::GlobalMypi => 2,
            SkillScope::GlobalPackage => 3,
            SkillScope::ProjectAgents => 4,
            SkillScope::ProjectMypi => 5,
            SkillScope::ProjectPackage => 6,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            SkillScope::GlobalAgents => "Global (~/.agents)",
            SkillScope::GlobalMypi => "Global (~/.mypi)",
            SkillScope::GlobalPackage => "Global Package",
            SkillScope::ProjectAgents => "Project (.agents)",
            SkillScope::ProjectMypi => "Project (.mypi)",
            SkillScope::ProjectPackage => "Project Package",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub file_path: PathBuf,
    pub scope: SkillScope,
    pub enabled: bool,
    pub is_valid: bool,
    pub validation_error: Option<String>,
}

pub struct SkillManager {
    skills: HashMap<String, SkillMetadata>,
}

impl SkillManager {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
        }
    }

    pub fn discover_skills(&mut self, project_root: Option<&Path>) {
        self.discover_skills_with_home(project_root, dirs_home().as_deref());
    }

    pub fn discover_skills_with_home(&mut self, project_root: Option<&Path>, home_dir: Option<&Path>) {
        self.skills.clear();

        if let Some(home) = home_dir {
            self.scan_directory(&home.join(".agents/skills"), SkillScope::GlobalAgents);
            self.scan_directory(&home.join(".mypi/skills"), SkillScope::GlobalMypi);
            self.scan_packages(&home.join(".mypi/packages"), SkillScope::GlobalPackage);
        }

        if let Some(proj) = project_root {
            self.scan_directory(&proj.join(".agents/skills"), SkillScope::ProjectAgents);
            self.scan_directory(&proj.join(".mypi/skills"), SkillScope::ProjectMypi);
            self.scan_packages(&proj.join(".mypi/packages"), SkillScope::ProjectPackage);
        }
    }

    fn scan_directory(&mut self, dir: &Path, scope: SkillScope) {
        if !dir.is_dir() {
            return;
        }
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let skill_file = path.join("SKILL.md");
                if skill_file.exists() {
                    self.process_skill_file(&skill_file, scope);
                }
            } else if path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
                self.process_skill_file(&path, scope);
            }
        }
    }

    fn scan_packages(&mut self, packages_dir: &Path, scope: SkillScope) {
        if !packages_dir.is_dir() {
            return;
        }
        let entries = match fs::read_dir(packages_dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let pkg_dir = entry.path();
            if pkg_dir.is_dir() {
                self.scan_directory(&pkg_dir.join("skills"), scope);
            }
        }
    }

    fn process_skill_file(&mut self, file_path: &Path, scope: SkillScope) {
        let content = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(e) => {
                let id = file_path
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".into());
                let meta = SkillMetadata {
                    id: id.clone(),
                    name: id,
                    description: "Unreadable file".into(),
                    tags: Vec::new(),
                    file_path: file_path.to_path_buf(),
                    scope,
                    enabled: false,
                    is_valid: false,
                    validation_error: Some(format!("Failed to read file: {e}")),
                };
                self.add_skill_with_precedence(meta);
                return;
            }
        };

        let (parsed_meta, err) = parse_skill_frontmatter(&content);
        let id = parsed_meta.name.clone();
        let is_valid = err.is_none() && !id.is_empty();

        let meta = SkillMetadata {
            id: if id.is_empty() { "unnamed".into() } else { id.clone() },
            name: if id.is_empty() { "Unnamed Skill".into() } else { id },
            description: parsed_meta.description,
            tags: parsed_meta.tags,
            file_path: file_path.to_path_buf(),
            scope,
            enabled: is_valid,
            is_valid,
            validation_error: err,
        };

        self.add_skill_with_precedence(meta);
    }

    fn add_skill_with_precedence(&mut self, meta: SkillMetadata) {
        if let Some(existing) = self.skills.get(&meta.id) {
            if meta.scope.precedence() >= existing.scope.precedence() {
                self.skills.insert(meta.id.clone(), meta);
            }
        } else {
            self.skills.insert(meta.id.clone(), meta);
        }
    }

    pub fn list_skills(&self) -> Vec<SkillMetadata> {
        let mut list: Vec<SkillMetadata> = self.skills.values().cloned().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    pub fn get_skill_instructions(&self, skill_id: &str) -> Result<String, String> {
        let meta = self
            .skills
            .get(skill_id)
            .ok_or_else(|| format!("Skill '{skill_id}' not found"))?;

        if !meta.enabled || !meta.is_valid {
            return Err(format!("Skill '{skill_id}' is disabled or invalid"));
        }

        let content = fs::read_to_string(&meta.file_path)
            .map_err(|e| format!("Failed to read instruction file: {e}"))?;

        let instructions = strip_frontmatter(&content);
        Ok(instructions.to_string())
    }
}

struct ParsedFrontmatter {
    name: String,
    description: String,
    tags: Vec<String>,
}

fn parse_skill_frontmatter(content: &str) -> (ParsedFrontmatter, Option<String>) {
    let mut name = String::new();
    let mut description = String::new();
    let mut tags = Vec::new();

    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (
            ParsedFrontmatter {
                name,
                description,
                tags,
            },
            Some("Missing YAML frontmatter delimiter '---'".into()),
        );
    }

    let rest = &trimmed[3..];
    let end_idx = match rest.find("---") {
        Some(idx) => idx,
        None => {
            return (
                ParsedFrontmatter {
                    name,
                    description,
                    tags,
                },
                Some("Unclosed YAML frontmatter delimiter '---'".into()),
            );
        }
    };

    let yaml_block = &rest[..end_idx];
    for line in yaml_block.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once(':') {
            let key = key.trim();
            let val = val.trim().trim_matches('"').trim_matches('\'');
            match key {
                "name" => name = val.to_string(),
                "description" => description = val.to_string(),
                "tags" => {
                    if val.starts_with('[') && val.ends_with(']') {
                        tags = val[1..val.len() - 1]
                            .split(',')
                            .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                    }
                }
                _ => {}
            }
        }
    }

    let err = if name.is_empty() {
        Some("Frontmatter missing required 'name' property".into())
    } else {
        None
    };

    (
        ParsedFrontmatter {
            name,
            description,
            tags,
        },
        err,
    )
}

fn strip_frontmatter(content: &str) -> &str {
    let trimmed = content.trim_start();
    if trimmed.starts_with("---") {
        let rest = &trimmed[3..];
        if let Some(idx) = rest.find("---") {
            return rest[idx + 3..].trim();
        }
    }
    content.trim()
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
