use crate::context::ProjectContext;
use std::collections::HashSet;
use std::path::Path;
use threadlane_runtime::AgentToolDefinition;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemPromptConfig {
    /// Replaces threadlane's default identity, tool list, and default guidelines.
    pub(crate) custom_prompt: Option<String>,
    /// Text appended after the base prompt and before project resources.
    pub(crate) append_prompt: Option<String>,
    /// Additional guideline bullets for the default prompt.
    pub(crate) guidelines: Vec<String>,
}

pub(crate) struct SystemPromptBuildOptions<'a> {
    pub(crate) config: &'a SystemPromptConfig,
    pub(crate) work_dir: &'a Path,
    pub(crate) tools: &'a [AgentToolDefinition],
    pub(crate) project_context: &'a ProjectContext,
    pub(crate) skill_catalog: Option<&'a str>,
    pub(crate) agent_catalog: Option<&'a str>,
    pub(crate) loaded_extension_count: usize,
}

fn normalize_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn escaped_attribute(value: &Path) -> String {
    value
        .to_string_lossy()
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn visible_tool_names(tools: &[AgentToolDefinition]) -> HashSet<&str> {
    tools
        .iter()
        .map(|tool| tool.name.trim())
        .filter(|name| !name.is_empty())
        .collect()
}

fn append_project_context(prompt: &mut String, context: &ProjectContext) {
    if !context.context_files.is_empty() {
        prompt.push_str("\n\n<project_context>\n");
        prompt.push_str(
            "Project instruction files are available in the workspace. Read the relevant file before changing code in its scope:\n",
        );
        for path in &context.context_files {
            prompt.push_str("- ");
            prompt.push_str(&escaped_attribute(path));
            prompt.push('\n');
        }
        prompt.push_str("</project_context>");
    }

    if let Some(memory) = &context.memory_content {
        prompt.push_str("\n\n<project_memory>\n");
        prompt.push_str("Persistent project memory from .threadlane/memory.md:\n\n");
        prompt.push_str(memory);
        prompt.push_str("\n</project_memory>");
    }
}

fn append_catalog(prompt: &mut String, catalog: Option<&str>) {
    if let Some(catalog) = catalog.map(str::trim).filter(|catalog| !catalog.is_empty()) {
        prompt.push_str("\n\n");
        prompt.push_str(catalog);
    }
}

pub(crate) fn build_system_prompt(options: SystemPromptBuildOptions<'_>) -> String {
    let available_tool_names = visible_tool_names(options.tools);

    let mut prompt = if let Some(custom_prompt) = options
        .config
        .custom_prompt
        .as_deref()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
    {
        custom_prompt.to_string()
    } else {
        let mut tool_guidelines = Vec::new();
        let mut seen = HashSet::new();
        let mut add_tool_guideline = |guideline: &str| {
            let guideline = normalize_line(guideline);
            if !guideline.is_empty() && seen.insert(guideline.clone()) {
                tool_guidelines.push(guideline);
            }
        };

        if available_tool_names.contains("read_file") {
            add_tool_guideline(
                "Inspect relevant files before making changes; do not guess about code, variable names, or schemas you have not read.",
            );
        }
        if available_tool_names.contains("get_repo_map") {
            add_tool_guideline(
                "Use `get_repo_map` to get a compact skeleton of the workspace files and top-level exported symbols without pulling full file bodies into context.",
            );
        }
        if available_tool_names.contains("manage_memory")
            || available_tool_names.contains("read_memory")
            || available_tool_names.contains("save_memory")
            || available_tool_names.contains("consolidate_memory")
        {
            add_tool_guideline(
                "Use `manage_memory` (with action 'save' or 'consolidate') to store persistent project facts, architectural decisions, gotchas, or verification commands into `.threadlane/memory.md` so future sessions benefit.",
            );
        }
        if available_tool_names.contains("write_file")
            || available_tool_names.contains("edit_file_hashline")
            || available_tool_names.contains("edit_files_hashline")
        {
            add_tool_guideline(
                "Keep edits focused, preserve existing user work, and follow the project's established style.",
            );
        }
        if available_tool_names.contains("edit_file_hashline") {
            add_tool_guideline(
                "Prefer `edit_file_hashline` for high-precision edits using line:hash anchors (e.g. '12:a3f') returned from `read_file` or prior `edit_file_hashline` outputs.",
            );
            add_tool_guideline(
                "For multi-line code blocks or deletions, use range edits (start_anchor and end_anchor) rather than per-line edits.",
            );
            add_tool_guideline(
                "Batch all edits for a file into a single `edit_file_hashline` tool call's edits array.",
            );
            add_tool_guideline(
                "Successful `edit_file_hashline` calls return the unified diff and updated surrounding line:hash anchors. Do not run redundant `read_file` or `git diff` commands simply to check what changed or to obtain new hashes for adjacent edits.",
            );
            add_tool_guideline(
                "If a hashline mismatch occurs, re-read the relevant file range with `read_file` to obtain updated line hashes before retrying.",
            );
        }
        if available_tool_names.contains("edit_files_hashline") {
            add_tool_guideline(
                "Use `edit_files_hashline` when changes across multiple files must commit together; every file and anchor is preflighted before the transaction writes any target.",
            );
        }
        if available_tool_names.contains("apply_workspace_edit_plan") {
            add_tool_guideline(
                "LSP rename and format tools return non-mutating workspace-edit plans. Apply an accepted plan with `apply_workspace_edit_plan`, which validates every workspace path and UTF-16 range before committing files.",
            );
        }
        if available_tool_names.contains("run_command") {
            add_tool_guideline(
                "Auxiliary capabilities can be inspected or executed in-process via `run_command` using `dyn <tool_name> [json_args]` or `dyn --help` without tool schema overhead.",
            );
        }
        if available_tool_names.contains("subagent") {
            add_tool_guideline(
                "SUBAGENT DELEGATION RULES: Use `subagent` judiciously and only when necessary.",
            );
            add_tool_guideline(
                "Do NOT spawn subagents for simple requests, single-file edits, or direct questions—handle them directly.",
            );
            add_tool_guideline(
                "Phase-Ordered Execution: Subagents MUST follow a sequential lifecycle (Research -> Implementation -> Review). NEVER spawn a `reviewer` or `tester` subagent concurrently with or before code changes exist.",
            );
            add_tool_guideline(
                "Parallel subagents are reserved ONLY for independent read-only exploration across multiple files.",
            );
            add_tool_guideline(
                "When invoking `subagent`, specify clear custom `instructions` and the minimum required `tools` for each subagent.",
            );
        }
        if available_tool_names.contains("update_plan") {
            add_tool_guideline(
                "For multi-step work, maintain a concise plan with `update_plan`; keep at most one item in progress and skip plans for simple requests.",
            );
            add_tool_guideline(
                "Update the plan throughout the work, not only at the end: mark a step in_progress when you start it, mark it completed immediately after it succeeds, and update the next step before continuing. Keep the plan statuses accurate after every meaningful milestone.",
            );
            add_tool_guideline(
                "Plan like a lazy senior developer: identify existing codebase helpers/types and find the root cause first; keep plans strictly to 2–5 minimal milestones aimed at the shortest working diff; never plan speculative scaffolding or unrequested abstractions.",
            );
        }

        for guideline in &options.config.guidelines {
            add_tool_guideline(guideline);
        }

        let formatted_tool_guidelines = if tool_guidelines.is_empty() {
            String::new()
        } else {
            format!(
                "\n{}",
                tool_guidelines
                    .into_iter()
                    .map(|g| format!("- {g}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };

        let extension_note = if options.loaded_extension_count == 0 {
            String::new()
        } else {
            format!(
                "\n\n{} WASI extension(s) are loaded in the sandbox. Their tools are included above when available.",
                options.loaded_extension_count
            )
        };

        let validation_rule = if available_tool_names.contains("run_command") {
            "\n- Run focused validation after changes when practical, and never claim a command passed unless you ran it successfully and verified the output."
        } else {
            ""
        };

        format!(
            "You are an expert coding assistant operating inside threadlane. Use the tools exposed by the runtime when relevant.\n\n\
            ## Execution Guidelines\n\
            - Match effort to the request and complete the requested scope. Make reasonable assumptions unless proceeding would be unsafe or useless.\n\
            - Inspect before editing, fix root causes, preserve surrounding idioms, and keep changes minimal. Do not add speculative abstractions or unrelated cleanup.\n\
            - Do not claim completion or successful validation without evidence. If blocked, finish unblocked work and state what remains.{validation_rule}\n\
            - Use concise plans only for substantial multi-step work. Avoid redundant reads and tool calls.\n\
            - If a tool fails, adapt to its error rather than retrying verbatim. Run independent calls in parallel when useful.\n\
            - Be concise and direct. Cite code as `file_path:line_number` when relevant.\n\n\
            ## Tool-Specific Guidance\
            {formatted_tool_guidelines}{extension_note}"
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
    use serde_json::json;
    use std::path::PathBuf;

    fn tool(name: &str, description: &str) -> AgentToolDefinition {
        AgentToolDefinition::new(name, description, json!({"type": "object"}))
    }

    #[test]
    fn default_prompt_uses_runtime_schemas_instead_of_repeating_descriptions() {
        let tools = vec![
            tool("read_file", "Read a file."),
            tool("custom_search", "Search data."),
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

        assert!(prompt.contains("## Execution Guidelines"));
        assert!(prompt.contains("Inspect relevant files before making changes"));
        assert!(!prompt.contains("Read a file."));
        assert!(!prompt.contains("Search data."));
        assert!(prompt.len() < 4_000);
    }

    #[test]
    fn project_instructions_are_referenced_not_embedded() {
        let context = ProjectContext {
            context_files: vec![PathBuf::from("/workspace/AGENTS.md")],
            memory_content: Some("Remember this.".into()),
        };
        let prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &SystemPromptConfig::default(),
            work_dir: Path::new("/workspace"),
            tools: &[tool("read_file", "Read a file.")],
            project_context: &context,
            skill_catalog: None,
            agent_catalog: None,
            loaded_extension_count: 0,
        });

        assert!(prompt.contains("/workspace/AGENTS.md"));
        assert!(prompt.contains("Read the relevant file"));
        assert!(!prompt.contains("<project_instructions"));
        assert!(prompt.contains("Remember this."));
    }

    #[test]
    fn catalogs_require_corresponding_tools() {
        let config = SystemPromptConfig::default();
        let context = ProjectContext::default();
        let build = |tools: &[AgentToolDefinition]| {
            build_system_prompt(SystemPromptBuildOptions {
                config: &config,
                work_dir: Path::new("/workspace"),
                tools,
                project_context: &context,
                skill_catalog: Some("SKILLS"),
                agent_catalog: Some("AGENTS"),
                loaded_extension_count: 0,
            })
        };

        assert!(!build(&[]).contains("SKILLS"));
        assert!(!build(&[]).contains("AGENTS"));
        assert!(build(&[tool("read_file", "read")]).contains("SKILLS"));
        assert!(build(&[tool("subagent", "delegate")]).contains("AGENTS"));
    }

    #[test]
    fn custom_prompt_keeps_resources_and_append_text() {
        let config = SystemPromptConfig {
            custom_prompt: Some("Custom base".into()),
            append_prompt: Some("Extra rule".into()),
            guidelines: vec![],
        };
        let context = ProjectContext {
            context_files: vec![PathBuf::from("/workspace/AGENTS.md")],
            memory_content: None,
        };
        let prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &config,
            work_dir: Path::new("/workspace"),
            tools: &[],
            project_context: &context,
            skill_catalog: None,
            agent_catalog: None,
            loaded_extension_count: 0,
        });

        assert!(prompt.starts_with("Custom base\n\nExtra rule"));
        assert!(prompt.contains("/workspace/AGENTS.md"));
        assert!(prompt.ends_with("Current working directory: /workspace"));
    }
}
