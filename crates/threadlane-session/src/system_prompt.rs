use crate::context::ProjectContext;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use threadlane_runtime::AgentToolDefinition;

const MAX_TOOL_DESCRIPTION_CHARS: usize = 240;

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
    if !context.instructions.is_empty() {
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
                "Auxiliary capabilities can be inspected or executed in-process via `run_command` using `dyn <tool_name> [args]` or `dyn --help` without tool schema overhead.",
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
            "You are an expert coding assistant operating inside threadlane, a native coding agent harness. You help users solve software engineering tasks by reading files, executing commands, editing code, and writing new files.\n\n\
            ## Available Tools\n\
            {tools}\n\n\
            Additional custom tools may be available depending on the project.\n\n\
            ## Delivering Work & Execution Rules\n\
            - Deliver complete, production-ready work. Act on the actual request without quietly narrowing, widening, or transforming the requested scope.\n\
            - Do not stop halfway, leave temporary placeholders or TODOs, or report completion until the task is fully finished and verified.\n\
            - If part of a task is blocked, finish every unblocked part first and state clearly what was left out and why.\n\
            - Reserve blocking questions for cases where proceeding under any assumption would be unsafe or render the work useless. Make reasonable, documented assumptions and proceed.\n\
            - Avoid unnecessary self-correction, apologies, or ruminating over past errors; state corrections plainly and continue.\n\
            - Do not re-derive facts already established in the conversation or re-litigate decisions already made.\n\n\
            ## Tool Strategy & Guidance\
            {formatted_tool_guidelines}\n\
            - If a tool call fails or is declined, adjust your strategy based on the error or feedback instead of retrying verbatim.\n\
            - Independent tool calls can run in parallel in a single response when appropriate.\n\n\
            ## Code Quality & Verification\n\
            - Match the surrounding code's idioms, naming, style, and comment density.{validation_rule}\n\
            - Report outcomes faithfully: state test failures directly with relevant output snippets, and report status without hedging.\n\
            - Reference code locations as `file_path:line_number` (e.g. `src/main.rs:42`) so they render as clickable links in the harness UI.\n\
            - Be concise and direct in your responses. Show file paths clearly when working with files.{extension_note}"
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
        assert!(prompt.contains("## Delivering Work & Execution Rules"));
        assert!(prompt.contains("## Tool Strategy & Guidance"));
        assert!(prompt.contains("## Code Quality & Verification"));
        assert!(prompt.ends_with("Current working directory: /workspace"));
    }

    #[test]
    fn test_hashline_system_prompt_guidelines() {
        let tools = vec![
            tool("read_file", "Read a file."),
            tool("edit_file_hashline", "Edit file with hashline."),
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

        assert!(prompt.contains("Prefer `edit_file_hashline` for high-precision edits using line:hash anchors (e.g. '12:a3f') returned from `read_file` or prior `edit_file_hashline` outputs."));
        assert!(prompt.contains("For multi-line code blocks or deletions, use range edits (start_anchor and end_anchor) rather than per-line edits."));
        assert!(prompt.contains(
            "Batch all edits for a file into a single `edit_file_hashline` tool call's edits array."
        ));
        assert!(prompt.contains("Successful `edit_file_hashline` calls return the unified diff and updated surrounding line:hash anchors."));
        assert!(prompt.contains("If a hashline mismatch occurs, re-read the relevant file range with `read_file` to obtain updated line hashes before retrying."));
    }

    #[test]
    fn test_run_command_guideline_presence() {
        let tools = vec![tool("run_command", "Run shell command.")];
        let prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &SystemPromptConfig::default(),
            work_dir: Path::new("/workspace"),
            tools: &tools,
            project_context: &ProjectContext::default(),
            skill_catalog: None,
            agent_catalog: None,
            loaded_extension_count: 0,
        });

        assert!(prompt.contains("Run focused validation after changes when practical, and never claim a command passed unless you ran it successfully and verified the output."));
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
            memory_content: None,
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

    #[test]
    fn subagent_guidelines_enforce_controlled_delegation() {
        let tools = vec![tool("subagent", "Invoke a subagent.")];
        let prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &SystemPromptConfig::default(),
            work_dir: Path::new("/workspace"),
            tools: &tools,
            project_context: &ProjectContext::default(),
            skill_catalog: None,
            agent_catalog: None,
            loaded_extension_count: 0,
        });

        assert!(prompt.contains(
            "SUBAGENT DELEGATION RULES: Use `subagent` judiciously and only when necessary."
        ));
        assert!(prompt.contains("NEVER spawn a `reviewer` or `tester` subagent concurrently with or before code changes exist."));
        assert!(!prompt.contains("ALWAYS use the `subagent` tool to fan out work"));
    }

    #[test]
    fn test_fast_path_and_plan_guidelines() {
        let tools = vec![tool("update_plan", "Update plan.")];
        let prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &SystemPromptConfig::default(),
            work_dir: Path::new("/workspace"),
            tools: &tools,
            project_context: &ProjectContext::default(),
            skill_catalog: None,
            agent_catalog: None,
            loaded_extension_count: 0,
        });

        assert!(prompt.contains("For multi-step work, maintain a concise plan with `update_plan`"));
    }
}
