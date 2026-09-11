use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    pub(crate) name: String,
    description: String,
    argument_hint: Option<String>,
    content: String,
    file_path: PathBuf,
    scope: String,
}

/// Parse command arguments respecting bash-style quotes (single and double quotes).
fn parse_command_args(args_string: &str) -> Vec<String> {
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
fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");
    let mut result = String::new();
    let mut i = 0;
    let len = content.len();

    while i < len {
        if content[i..].starts_with('$') {
            let rest = &content[i + 1..];

            // Check for ${...}
            if let Some(braced) = rest.strip_prefix('{') {
                if let Some(close_offset) = braced.find('}') {
                    let close_idx = i + 1 + 1 + close_offset;
                    let expr = &content[i + 2..close_idx];
                    let substituted = eval_braced_expr(expr, args, &all_args);
                    result.push_str(&substituted);
                    i = close_idx + 1;
                    continue;
                }
            }

            // Check simple replacements: $ARGUMENTS, $@, $1, $2, etc.
            if rest.starts_with("ARGUMENTS") {
                result.push_str(&all_args);
                i += 1 + "ARGUMENTS".len();
                continue;
            } else if rest.starts_with('@') {
                result.push_str(&all_args);
                i += 2;
                continue;
            } else {
                let digit_len = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
                if digit_len > 0 {
                    let num_str = &rest[..digit_len];
                    if let Ok(idx) = num_str.parse::<usize>() {
                        if idx > 0 && idx <= args.len() {
                            result.push_str(&args[idx - 1]);
                        }
                    }
                    i += 1 + digit_len;
                    continue;
                }
            }
        }

        let ch = content[i..].chars().next().unwrap();
        result.push(ch);
        i += ch.len_utf8();
    }

    result
}

fn eval_braced_expr(expr: &str, args: &[String], all_args: &str) -> String {
    // 1. Check for defaults: TARGET:-DEFAULT. This must run before the
    //    `@:` slicing branch, otherwise `@:-none` is misread as a slice
    //    (`@:` + `-none` → empty string) instead of the `@` target with a
    //    `none` default.
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

    // 2. Check for slicing: @:N or @:N:L
    if let Some(slice_spec) = expr.strip_prefix("@:") {
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
fn parse_frontmatter(content: &str) -> (Option<String>, Option<String>, String) {
    let parsed = threadlane_skills::frontmatter::parse_frontmatter(content);
    let description = parsed.get_str("description").map(ToString::to_string);
    let argument_hint = parsed.get_str("argument-hint").map(ToString::to_string);
    (description, argument_hint, parsed.body)
}

/// Load prompt templates from a directory (non-recursive).
fn load_prompt_templates_from_dir(dir: &Path, scope: &str) -> Vec<PromptTemplate> {
    let mut templates = Vec::new();

    if !dir.exists() || !dir.is_dir() {
        return templates;
    }

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
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
pub(crate) fn load_prompt_templates(project_dir: &Path, global_dir: &Path) -> Vec<PromptTemplate> {
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
pub(crate) fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_substitution() {
        let content = "Hello, world!";
        let args = vec!["arg1".to_string(), "arg2".to_string()];
        let result = substitute_args(content, &args);
        assert_eq!(result, "Hello, world!");
    }

    #[test]
    fn test_positional_args() {
        let content = "Hello $1, you are $2 years old.";
        let args = vec!["Alice".to_string(), "30".to_string()];
        let result = substitute_args(content, &args);
        assert_eq!(result, "Hello Alice, you are 30 years old.");

        let content_with_braces = "Hello ${1}, you are ${2} years old.";
        let result_with_braces = substitute_args(content_with_braces, &args);
        assert_eq!(result_with_braces, "Hello Alice, you are 30 years old.");
    }

    #[test]
    fn test_all_args() {
        let content_at = "Args: $@";
        let content_arguments = "Args: $ARGUMENTS";
        let args = vec!["a".to_string(), "b".to_string(), "c".to_string()];

        let result_at = substitute_args(content_at, &args);
        assert_eq!(result_at, "Args: a b c");

        let result_arguments = substitute_args(content_arguments, &args);
        assert_eq!(result_arguments, "Args: a b c");

        let content_at_braces = "Args: ${@}";
        let content_arguments_braces = "Args: ${ARGUMENTS}";

        let result_at_braces = substitute_args(content_at_braces, &args);
        assert_eq!(result_at_braces, "Args: a b c");

        let result_arguments_braces = substitute_args(content_arguments_braces, &args);
        assert_eq!(result_arguments_braces, "Args: a b c");
    }

    #[test]
    fn test_defaults() {
        let args = vec!["arg1".to_string()];

        let content_pos_default = "Arg2 is ${2:-missing}";
        let result_pos_default = substitute_args(content_pos_default, &args);
        assert_eq!(result_pos_default, "Arg2 is missing");

        let content_pos_present = "Arg1 is ${1:-missing}";
        let result_pos_present = substitute_args(content_pos_present, &args);
        assert_eq!(result_pos_present, "Arg1 is arg1");

        let empty_args: Vec<String> = vec![];
        let content_arguments_default = "Args: ${ARGUMENTS:-nothing}";
        let result_arguments_default = substitute_args(content_arguments_default, &empty_args);
        assert_eq!(result_arguments_default, "Args: nothing");
    }

    #[test]
    fn test_defaults_at_symbol_bug() {
        let empty_args: Vec<String> = vec![];
        let content_all_default = "Args: ${@:-none}";
        let result_all_default = substitute_args(content_all_default, &empty_args);
        assert_eq!(result_all_default, "Args: none");
    }

    #[test]
    fn test_slicing() {
        let args = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];

        let content_slice_from = "Slice: ${@:2}";
        let result_slice_from = substitute_args(content_slice_from, &args);
        assert_eq!(result_slice_from, "Slice: b c d");

        let content_slice_len = "Slice: ${@:2:2}";
        let result_slice_len = substitute_args(content_slice_len, &args);
        assert_eq!(result_slice_len, "Slice: b c");

        let content_slice_out_of_bounds = "Slice: ${@:10}";
        let result_slice_out_of_bounds = substitute_args(content_slice_out_of_bounds, &args);
        assert_eq!(result_slice_out_of_bounds, "Slice: ");
    }

    #[test]
    fn test_edge_cases() {
        let args = vec!["a".to_string(), "b".to_string()];

        // Out of bounds positional without default
        let result = substitute_args("Missing $3", &args);
        assert_eq!(result, "Missing ");

        let result_braced = substitute_args("Missing ${3}", &args);
        assert_eq!(result_braced, "Missing ");

        // Malformed expression - missing closing brace
        let result_malformed = substitute_args("Malformed ${1", &args);
        assert_eq!(result_malformed, "Malformed ${1");

        // Literal $ followed by non-variable character
        let result_no_match = substitute_args("Cost is $X", &args);
        assert_eq!(result_no_match, "Cost is $X");
    }
}
