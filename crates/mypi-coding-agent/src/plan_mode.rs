use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub index: usize,
    pub description: String,
    pub completed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessPolicy {
    FullAccess,
    ReadOnly,
}

impl Default for HarnessPolicy {
    fn default() -> Self {
        Self::FullAccess
    }
}

impl HarnessPolicy {
    pub fn system_prompt_instructions(self) -> &'static str {
        match self {
            Self::FullAccess => "",
            Self::ReadOnly => {
                "\n\n=== PLAN MODE ACTIVE (READ-ONLY EXPLORATION) ===\n\
                You are currently in Plan Mode. File modifications are disabled.\n\
                1. Analyze code and workspace using read_file, list_dir, and read-only shell commands.\n\
                2. When proposing a plan, output it under a `Plan:` header with numbered items:\n\
                   Plan:\n\
                   1. First step description\n\
                   2. Second step description\n\
                3. During execution, mark completed steps using `[DONE:n]` markers."
            }
        }
    }

    pub fn evaluate_tool_call(self, tool_name: &str, arguments: &str) -> HarnessPolicyDecision {
        match self {
            Self::FullAccess => HarnessPolicyDecision::allow(),
            Self::ReadOnly => {
                if matches!(tool_name, "write_file" | "edit_file" | "write" | "edit") {
                    return HarnessPolicyDecision::block(format!(
                        "Tool `{}` is blocked because Plan Mode is ACTIVE (Read-only exploration). Toggle off using /plan.",
                        tool_name
                    ));
                }

                if tool_name == "run_command" {
                    let first_word = arguments.trim().split_whitespace().next().unwrap_or("");
                    if !matches!(
                        first_word,
                        "ls"
                            | "cat"
                            | "grep"
                            | "rg"
                            | "find"
                            | "pwd"
                            | "git"
                            | "cargo"
                            | "echo"
                            | "head"
                            | "tail"
                    ) {
                        return HarnessPolicyDecision::block(format!(
                            "Command `{}` is restricted in Plan Mode. Only read-only commands (ls, cat, grep, cargo check, git status) are permitted.",
                            arguments
                        ));
                    }
                }

                HarnessPolicyDecision::allow()
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HarnessPolicyDecision {
    pub block: bool,
    pub reason: Option<String>,
}

impl HarnessPolicyDecision {
    pub fn allow() -> Self {
        Self::default()
    }

    pub fn block(reason: impl Into<String>) -> Self {
        Self {
            block: true,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanModeState {
    pub enabled: bool,
    pub items: Vec<PlanItem>,
}

impl PlanModeState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn toggle(&mut self) -> bool {
        self.enabled = !self.enabled;
        self.enabled
    }

    pub fn harness_policy(&self) -> HarnessPolicy {
        if self.enabled {
            HarnessPolicy::ReadOnly
        } else {
            HarnessPolicy::FullAccess
        }
    }

    pub fn parse_and_update_plan(&mut self, text: &str) -> usize {
        let mut in_plan_block = false;
        let mut new_items = Vec::new();

        for line in text.lines() {
            let trimmed = line.trim();

            if let Some(start) = trimmed.find("[DONE:") {
                let rest = &trimmed[start + 6..];
                if let Some(end) = rest.find(']') {
                    if let Ok(idx) = rest[..end].trim().parse::<usize>() {
                        self.mark_done(idx);
                    }
                }
            }

            let heading = trimmed
                .trim_start_matches('#')
                .trim()
                .trim_matches('*')
                .trim()
                .trim_end_matches(':')
                .trim();
            if heading.eq_ignore_ascii_case("plan")
                || heading.eq_ignore_ascii_case("implementation plan")
                || heading.eq_ignore_ascii_case("proposed plan")
            {
                in_plan_block = true;
                continue;
            }

            if in_plan_block {
                if let Some((index, description)) = parse_ordered_item(trimmed) {
                    new_items.push(PlanItem {
                        index,
                        description: description.to_string(),
                        completed: false,
                    });
                    continue;
                }

                if !trimmed.is_empty() && (trimmed.starts_with('#') || !trimmed.starts_with('-')) {
                    in_plan_block = false;
                }
            }
        }

        if !new_items.is_empty() {
            self.items = new_items;
        }

        self.items.len()
    }

    pub fn mark_done(&mut self, index: usize) -> bool {
        for item in &mut self.items {
            if item.index == index {
                item.completed = true;
                return true;
            }
        }
        false
    }

    pub fn format_todos(&self) -> String {
        if self.items.is_empty() {
            return "📋 No active plan items.".to_string();
        }

        let mut output = String::from("📋 Current Plan Progress:\n");
        let mut completed_count = 0;

        for item in &self.items {
            let status_icon = if item.completed {
                completed_count += 1;
                "✅"
            } else {
                "⏳"
            };
            output.push_str(&format!(
                "  {} {}. {}\n",
                status_icon, item.index, item.description
            ));
        }

        output.push_str(&format!(
            "\nProgress: {}/{} steps completed.",
            completed_count,
            self.items.len()
        ));
        output
    }

    pub fn is_tool_allowed(&self, tool_name: &str) -> bool {
        !self
            .harness_policy()
            .evaluate_tool_call(tool_name, "")
            .block
    }

    pub fn is_command_allowed(&self, command_line: &str) -> bool {
        !self
            .harness_policy()
            .evaluate_tool_call("run_command", command_line)
            .block
    }

    pub fn system_prompt_instructions(&self) -> &'static str {
        self.harness_policy().system_prompt_instructions()
    }
}

fn parse_ordered_item(line: &str) -> Option<(usize, &str)> {
    let digits_end = line.find(|c: char| !c.is_ascii_digit())?;
    let index = line[..digits_end].parse::<usize>().ok()?;
    if !(1..=50).contains(&index) {
        return None;
    }

    let delimiter = line[digits_end..].chars().next()?;
    if delimiter != '.' && delimiter != ')' {
        return None;
    }

    let description = line[digits_end + delimiter.len_utf8()..].trim();
    (!description.is_empty()).then_some((index, description))
}
