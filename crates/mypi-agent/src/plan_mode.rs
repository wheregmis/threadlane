use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub index: usize,
    pub description: String,
    pub completed: bool,
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

    pub fn parse_and_update_plan(&mut self, text: &str) -> usize {
        let mut in_plan_block = false;
        let mut new_items = Vec::new();

        for line in text.lines() {
            let trimmed = line.trim();

            // Check for [DONE:n] markers in assistant responses even outside a plan block.
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

                // A new Markdown heading or ordinary prose ends this plan block.
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
        if !self.enabled {
            return true;
        }

        // Write and edit tools are blocked in plan mode
        !matches!(tool_name, "write_file" | "edit_file" | "write" | "edit")
    }

    pub fn is_command_allowed(&self, command_line: &str) -> bool {
        if !self.enabled {
            return true;
        }

        let first_word = command_line.trim().split_whitespace().next().unwrap_or("");

        matches!(
            first_word,
            "ls" | "cat"
                | "grep"
                | "rg"
                | "find"
                | "pwd"
                | "git"
                | "cargo"
                | "echo"
                | "head"
                | "tail"
        )
    }

    pub fn system_prompt_instructions(&self) -> &'static str {
        if !self.enabled {
            ""
        } else {
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
