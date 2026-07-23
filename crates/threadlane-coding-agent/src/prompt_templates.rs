use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub content: String,
    pub file_path: PathBuf,
    pub scope: String,
}

/// Parse command arguments respecting bash-style quotes (single and double quotes).
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for ch in args_string.chars() {
        if let Some(q) = in_quote {
            if ch == q {
                in_quote = None;
            } else {
                current.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                args.push(current);
                current = String::new();
            }
        } else {
            current.push(ch);
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

/// Substitute argument placeholders in template content.
/// Supports:
/// - $1, $2, ... for positional args
/// - $@ and $ARGUMENTS for all args joined by space
/// - ${N:-default} for positional arg N with default when missing/empty
/// - ${@:-default} and ${ARGUMENTS:-default} for all args with default when empty
/// - ${@:N} for args from Nth onwards (1-indexed)
/// - ${@:N:L} for L args starting from Nth
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");
    let mut result = String::new();
    let chars: Vec<char> = content.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == '$' {
            // Check for ${...}
            if i + 1 < len && chars[i + 1] == '{' {
                if let Some(close_idx) = find_closing_brace(&chars, i + 2) {
                    let expr: String = chars[i + 2..close_idx].iter().collect();
                    let substituted = eval_braced_expr(&expr, args, &all_args);
                    result.push_str(&substituted);
                    i = close_idx + 1;
                    continue;
                }
            }

            // Check simple replacements: $ARGUMENTS, $@, $1, $2, etc.
            let rest: String = chars[i + 1..].iter().collect();
            if rest.starts_with("ARGUMENTS") {
                result.push_str(&all_args);
                i += 1 + "ARGUMENTS".len();
                continue;
            } else if rest.starts_with('@') {
                result.push_str(&all_args);
                i += 2;
                continue;
            } else {
                let mut num_str = String::new();
                let mut j = i + 1;
                while j < len && chars[j].is_ascii_digit() {
                    num_str.push(chars[j]);
                    j += 1;
                }
                if !num_str.is_empty() {
                    if let Ok(idx) = num_str.parse::<usize>() {
                        if idx > 0 && idx <= args.len() {
                            result.push_str(&args[idx - 1]);
                        }
                    }
                    i = j;
                    continue;
                }
            }
        }

        result.push(chars[i]);
        i += 1;
    }

    result
}

fn find_closing_brace(chars: &[char], start: usize) -> Option<usize> {
    for idx in start..chars.len() {
        if chars[idx] == '}' {
            return Some(idx);
        }
    }
    None
}

fn eval_braced_expr(expr: &str, args: &[String], all_args: &str) -> String {
    // 1. Check for slicing: @:N or @:N:L
    if expr.starts_with("@:") {
        let slice_spec = &expr[2..];
        let parts: Vec<&str> = slice_spec.split(':').collect();
        let start_idx = parts
            .first()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1);
        let start_0 = if start_idx == 0 { 0 } else { start_idx - 1 };

        if start_0 >= args.len() {
            return String::new();
        }

        if parts.len() >= 2 {
            if let Ok(length) = parts[1].parse::<usize>() {
                let end = (start_0 + length).min(args.len());
                return args[start_0..end].join(" ");
            }
        }
        return args[start_0..].join(" ");
    }

    // 2. Check for defaults: TARGET:-DEFAULT
    if let Some((target, default_val)) = expr.split_once(":-") {
        let val = match target {
            "@" | "ARGUMENTS" => {
                if all_args.is_empty() {
                    None
                } else {
                    Some(all_args.to_string())
                }
            }
            num_str => num_str
                .parse::<usize>()
                .ok()
                .filter(|&n| n > 0 && n <= args.len())
                .map(|n| args[n - 1].clone()),
        };
        return val.unwrap_or_else(|| default_val.to_string());
    }

    // Fallback: evaluate basic expression inside braces
    match expr {
        "@" | "ARGUMENTS" => all_args.to_string(),
        num_str => num_str
            .parse::<usize>()
            .ok()
            .filter(|&n| n > 0 && n <= args.len())
            .map(|n| args[n - 1].clone())
            .unwrap_or_default(),
    }
}

/// Parse frontmatter metadata from markdown file content.
pub fn parse_frontmatter(content: &str) -> (Option<String>, Option<String>, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (None, None, content.to_string());
    }

    let rest = &trimmed[3..];
    if let Some(end_idx) = rest.find("\n---") {
        let frontmatter_block = &rest[..end_idx];
        let body = rest[end_idx + 4..].trim().to_string();

        let mut description = None;
        let mut argument_hint = None;

        for line in frontmatter_block.lines() {
            let line = line.trim();
            if let Some((key, val)) = line.split_once(':') {
                let key = key.trim();
                let val = val.trim().trim_matches('"').trim_matches('\'').to_string();
                match key {
                    "description" => description = Some(val),
                    "argument-hint" => argument_hint = Some(val),
                    _ => {}
                }
            }
        }

        (description, argument_hint, body)
    } else {
        (None, None, content.to_string())
    }
}

/// Load prompt templates from a directory (non-recursive).
pub fn load_prompt_templates_from_dir(dir: &Path, scope: &str) -> Vec<PromptTemplate> {
    let mut templates = Vec::new();

    if !dir.exists() || !dir.is_dir() {
        return templates;
    }

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().map_or(false, |ext| ext == "md") {
                if let Ok(content) = fs::read_to_string(&path) {
                    let stem = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();

                    let (desc_opt, argument_hint, body) = parse_frontmatter(&content);

                    let description = desc_opt.unwrap_or_else(|| {
                        body.lines()
                            .find(|l| !l.trim().is_empty())
                            .map(|l| {
                                let l = l.trim();
                                if l.len() > 60 {
                                    format!("{}...", &l[..60])
                                } else {
                                    l.to_string()
                                }
                            })
                            .unwrap_or_else(|| stem.clone())
                    });

                    templates.push(PromptTemplate {
                        name: stem,
                        description,
                        argument_hint,
                        content: body,
                        file_path: path,
                        scope: scope.to_string(),
                    });
                }
            }
        }
    }

    templates
}

/// Load all prompt templates from global, project, and package locations.
pub fn load_prompt_templates(project_dir: &Path, global_dir: &Path) -> Vec<PromptTemplate> {
    let mut templates = Vec::new();

    // 1. Global prompts: ~/.threadlane/prompts/
    let global_prompts = global_dir.join("prompts");
    templates.extend(load_prompt_templates_from_dir(&global_prompts, "global"));

    // 2. Project prompts: <project>/.threadlane/prompts/
    let project_prompts = project_dir.join(".threadlane/prompts");
    let project_templates = load_prompt_templates_from_dir(&project_prompts, "project");

    for pt in project_templates {
        // Project templates override global templates with the same name
        templates.retain(|t| t.name != pt.name);
        templates.push(pt);
    }

    // 3. Package prompts: <project>/.threadlane/packages/*/prompts/
    let project_packages_prompts = project_dir.join(".threadlane/packages");
    if project_packages_prompts.exists() && project_packages_prompts.is_dir() {
        if let Ok(pkgs) = fs::read_dir(&project_packages_prompts) {
            for pkg in pkgs.flatten() {
                let pkg_prompts = pkg.path().join("prompts");
                if pkg_prompts.exists() && pkg_prompts.is_dir() {
                    let pkg_templates = load_prompt_templates_from_dir(&pkg_prompts, "package");
                    for pt in pkg_templates {
                        templates.retain(|t| t.name != pt.name);
                        templates.push(pt);
                    }
                }
            }
        }
    }

    templates
}

/// Expand a prompt template if the input starts with `/name`.
/// Returns the expanded prompt string, or the original text if no template matched.
pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return text.to_string();
    }

    let mut parts = trimmed[1..].splitn(2, char::is_whitespace);
    let name = match parts.next() {
        Some(n) if !n.is_empty() => n,
        _ => return text.to_string(),
    };
    let args_str = parts.next().unwrap_or("").trim();

    if let Some(template) = templates.iter().find(|t| t.name == name) {
        let args = parse_command_args(args_str);
        substitute_args(&template.content, &args)
    } else {
        text.to_string()
    }
}
