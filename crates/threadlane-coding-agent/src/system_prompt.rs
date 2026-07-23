use crate::context::ProjectContext;
use threadlane_agent::AgentToolDefinition;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

const MAX_TOOL_DESCRIPTION_CHARS: usize = 240;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemPromptConfig {
    /// Replaces threadlane's default identity, tool list, and default guidelines.
    pub custom_prompt: Option<String>,
    /// Text appended after the base prompt and before project resources.
    pub append_prompt: Option<String>,
    /// Additional guideline bullets for the default prompt.
    pub guidelines: Vec<String>,
}

pub struct SystemPromptBuildOptions<'a> {
    pub config: &'a SystemPromptConfig,
    pub work_dir: &'a Path,
    pub tools: &'a [AgentToolDefinition],
    pub project_context: &'a ProjectContext,
    pub skill_catalog: Option<&'a str>,
    pub agent_catalog: Option<&'a str>,
    pub loaded_extension_count: usize,
}

fn normalize_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn escaped_attribute(value: &Path) -> String {
    value
        .to_string_lossy()
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn visible_tools(tools: &[AgentToolDefinition]) -> Vec<(&str, String)> {
    let mut tools_by_name = BTreeMap::new();
    for tool in tools {
        let name = tool.name.trim();
        if name.is_empty() {
            continue;
        }
        let description = tool
            .description
            .as_deref()
            .map(normalize_line)
            .filter(|description| !description.is_empty())
            .unwrap_or_else(|| "No description provided.".to_string());
        tools_by_name
            .entry(name)
            .or_insert_with(|| truncate_chars(&description, MAX_TOOL_DESCRIPTION_CHARS));
    }
    tools_by_name.into_iter().collect()
}

fn append_project_context(prompt: &mut String, context: &ProjectContext) {
    if context.instructions.is_empty() {
        return;
    }

    prompt.push_str("\n\n<project_context>\n");
    prompt.push_str("Project-specific instructions and guidelines:\n\n");
    for instruction in &context.instructions {
        prompt.push_str(&format!(
            "<project_instructions path=\"{}\">\n{}\n</project_instructions>\n\n",
            escaped_attribute(&instruction.path),
            instruction.content
        ));
    }
    prompt.push_str("</project_context>");
}

fn append_catalog(prompt: &mut String, catalog: Option<&str>) {
    if let Some(catalog) = catalog.map(str::trim).filter(|catalog| !catalog.is_empty()) {
        prompt.push_str("\n\n");
        prompt.push_str(catalog);
    }
}

pub fn build_system_prompt(options: SystemPromptBuildOptions<'_>) -> String {
    let visible_tools = visible_tools(options.tools);
    let available_tool_names: HashSet<_> = visible_tools.iter().map(|(name, _)| *name).collect();

    let mut prompt = if let Some(custom_prompt) = options
        .config
        .custom_prompt
        .as_deref()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
    {
        custom_prompt.to_string()
    } else {
        let tools = if visible_tools.is_empty() {
            "(none)".to_string()
        } else {
            visible_tools
                .iter()
                .map(|(name, description)| format!("- {name}: {description}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut guidelines = Vec::new();
        let mut seen = HashSet::new();
        let mut add_guideline = |guideline: &str| {
            let guideline = normalize_line(guideline);
            if !guideline.is_empty() && seen.insert(guideline.clone()) {
                guidelines.push(guideline);
            }
        };

        if available_tool_names.contains("read_file") {
            add_guideline("Inspect relevant files before making changes; do not guess about code you have not read.");
        }
        if available_tool_names.contains("write_file") || available_tool_names.contains("edit_file")
        {
            add_guideline("Keep edits focused, preserve existing user work, and follow the project's established style.");
        }
        if available_tool_names.contains("run_command") {
            add_guideline("Run focused validation after changes when practical, and never claim a command passed unless you ran it successfully.");
        }
        for guideline in &options.config.guidelines {
            add_guideline(guideline);
        }
        add_guideline("Be concise and direct in your responses.");
        add_guideline("Show file paths clearly when working with files.");

        let guidelines = guidelines
            .into_iter()
            .map(|guideline| format!("- {guideline}"))
            .collect::<Vec<_>>()
            .join("\n");

        let extension_note = if options.loaded_extension_count == 0 {
            String::new()
        } else {
            format!(
                "\n\n{} WASI extension(s) are loaded in the sandbox. Their tools are included above when available.",
                options.loaded_extension_count
            )
        };

        format!(
            "You are an expert coding assistant operating inside threadlane, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.\n\nAvailable tools:\n{tools}\n\nAdditional custom tools may be available depending on the project.\n\nGuidelines:\n{guidelines}{extension_note}"
        )
    };

    if let Some(append_prompt) = options
        .config
        .append_prompt
        .as_deref()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
    {
        prompt.push_str("\n\n");
        prompt.push_str(append_prompt);
    }

    append_project_context(&mut prompt, options.project_context);
    if available_tool_names.contains("read_file") {
        append_catalog(&mut prompt, options.skill_catalog);
    }
    if available_tool_names.contains("subagent") {
        append_catalog(&mut prompt, options.agent_catalog);
    }

    let work_dir = options.work_dir.to_string_lossy().replace('\\', "/");
    prompt.push_str(&format!("\n\nCurrent working directory: {work_dir}"));
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ProjectInstruction;
    use serde_json::json;
    use std::path::PathBuf;

    fn tool(name: &str, description: &str) -> AgentToolDefinition {
        AgentToolDefinition::new(name, description, json!({"type": "object"}))
    }

    #[test]
    fn default_prompt_lists_only_available_tools_and_dynamic_guidelines() {
        let tools = vec![
            tool("read_file", "Read a file."),
            tool("custom_search", "Search custom data.\nSafely."),
        ];
        let prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &SystemPromptConfig::default(),
            work_dir: Path::new("/workspace"),
            tools: &tools,
            project_context: &ProjectContext::default(),
            skill_catalog: None,
            agent_catalog: None,
            loaded_extension_count: 0,
        });

        assert!(prompt.contains("- read_file: Read a file."));
        assert!(prompt.contains("- custom_search: Search custom data. Safely."));
        assert!(!prompt.contains("write_file:"));
        assert!(prompt.contains("Inspect relevant files before making changes"));
        assert!(!prompt.contains("Run focused validation after changes"));
        assert!(prompt.ends_with("Current working directory: /workspace"));
    }

    #[test]
    fn custom_prompt_replaces_defaults_but_keeps_resources_and_append_text() {
        let context = ProjectContext {
            context_files: vec![PathBuf::from("/workspace/AGENTS.md")],
            instructions: vec![ProjectInstruction {
                path: PathBuf::from("/workspace/AGENTS.md"),
                content: "Always test.".into(),
            }],
            combined_instructions: "Always test.".into(),
        };
        let config = SystemPromptConfig {
            custom_prompt: Some("Custom identity.".into()),
            append_prompt: Some("Additional rule.".into()),
            guidelines: vec!["not rendered for custom prompts".into()],
        };
        let prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &config,
            work_dir: Path::new("/workspace"),
            tools: &[tool("read_file", "Read")],
            project_context: &context,
            skill_catalog: Some("=== Available Skills ===\n- `review`: Review code"),
            agent_catalog: None,
            loaded_extension_count: 0,
        });

        assert!(prompt.starts_with("Custom identity.\n\nAdditional rule."));
        assert!(!prompt.contains("Available tools:"));
        assert!(prompt.contains("<project_instructions path=\"/workspace/AGENTS.md\">"));
        assert!(prompt.contains("=== Available Skills ==="));
    }

    #[test]
    fn catalogs_require_their_corresponding_tools() {
        let prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &SystemPromptConfig::default(),
            work_dir: Path::new("/workspace"),
            tools: &[],
            project_context: &ProjectContext::default(),
            skill_catalog: Some("SKILL_SENTINEL"),
            agent_catalog: Some("AGENT_SENTINEL"),
            loaded_extension_count: 0,
        });

        assert!(!prompt.contains("SKILL_SENTINEL"));
        assert!(!prompt.contains("AGENT_SENTINEL"));
    }
}
