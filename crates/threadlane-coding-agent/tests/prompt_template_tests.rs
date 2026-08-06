use tempfile::TempDir;
use threadlane_coding_agent::prompt_templates::{
    expand_prompt_template, load_prompt_templates, parse_command_args, parse_frontmatter,
    substitute_args,
};

#[test]
fn test_parse_command_args() {
    // Happy paths
    let args1 = parse_command_args("Button \"click handler\" 'disabled state'");
    assert_eq!(args1, vec!["Button", "click handler", "disabled state"]);

    let args2 = parse_command_args("  foo   bar   baz ");
    assert_eq!(args2, vec!["foo", "bar", "baz"]);

    // Empty and whitespace strings
    let args_empty = parse_command_args("");
    assert!(args_empty.is_empty(), "Empty string should return empty vec");

    let args_whitespace = parse_command_args("   \t\n  ");
    assert!(args_whitespace.is_empty(), "Whitespace string should return empty vec");

    // Unclosed quotes (should collect everything until EOF)
    let args_unclosed_double = parse_command_args("unclosed \"quote string");
    assert_eq!(args_unclosed_double, vec!["unclosed", "quote string"]);

    let args_unclosed_single = parse_command_args("'unclosed single");
    assert_eq!(args_unclosed_single, vec!["unclosed single"]);

    // Mixed quotes (quotes inside other quotes)
    let args_mixed1 = parse_command_args("\"'single inside double'\"");
    assert_eq!(args_mixed1, vec!["'single inside double'"]);

    let args_mixed2 = parse_command_args("'\"double inside single\"'");
    assert_eq!(args_mixed2, vec!["\"double inside single\""]);

    // Quotes concatenated without spaces
    let args_concat = parse_command_args("foo\"bar\"baz");
    assert_eq!(args_concat, vec!["foobarbaz"]);

    let args_adjacent = parse_command_args("\"foo\"'bar'");
    assert_eq!(args_adjacent, vec!["foobar"]);

    // Empty quotes - currently `parse_command_args` ignores completely empty quotes
    // because `current.is_empty()` returns true when only whitespace follows.
    // If empty arguments are desired, `parse_command_args` would need an `is_explicit` flag.
    // Let's assert its current actual behavior.
    let args_empty_quotes = parse_command_args("\"\" ''");
    assert!(args_empty_quotes.is_empty(), "Empty quotes are currently ignored by parse_command_args");

    // Non-ascii characters
    let args_unicode = parse_command_args("\"こんにちは\" '世界'");
    assert_eq!(args_unicode, vec!["こんにちは", "世界"]);
}

#[test]
fn test_substitute_args_positional_and_defaults() {
    let template = "Create React component $1 with features: $@ and default count ${2:-5}.";
    let args = vec!["Button".to_string(), "hover".to_string()];
    let result = substitute_args(template, &args);
    assert_eq!(
        result,
        "Create React component Button with features: Button hover and default count hover."
    );

    let template_with_default = "Count is ${1:-10} and name is ${2:-unnamed}.";
    let empty_args: Vec<String> = vec![];
    let result_default = substitute_args(template_with_default, &empty_args);
    assert_eq!(result_default, "Count is 10 and name is unnamed.");
}

#[test]
fn test_substitute_args_slices() {
    let template = "All after first: ${@:2}";
    let args = vec![
        "first".to_string(),
        "second".to_string(),
        "third".to_string(),
    ];
    let result = substitute_args(template, &args);
    assert_eq!(result, "All after first: second third");

    let template_length = "Two from second: ${@:2:2}";
    let result_length = substitute_args(template_length, &args);
    assert_eq!(result_length, "Two from second: second third");
}

#[test]
fn test_parse_frontmatter() {
    let raw = r#"---
description: Review staged git changes
argument-hint: "<BRANCH>"
---
Review the staged changes (`git diff --cached`).
"#;

    let (desc, hint, body) = parse_frontmatter(raw);
    assert_eq!(desc.as_deref(), Some("Review staged git changes"));
    assert_eq!(hint.as_deref(), Some("<BRANCH>"));
    assert!(body.contains("Review the staged changes"));
}

#[test]
fn test_load_and_expand_prompt_template() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path().join("proj");
    let global_dir = temp_dir.path().join("global");

    let proj_prompts = project_dir.join(".threadlane/prompts");
    std::fs::create_dir_all(&proj_prompts).unwrap();

    let review_template_path = proj_prompts.join("review.md");
    std::fs::write(
        &review_template_path,
        r#"---
description: Review code changes
---
Review code in branch ${1:-main} focusing on ${2:-bugs}.
"#,
    )
    .unwrap();

    let templates = load_prompt_templates(&project_dir, &global_dir);
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].name, "review");

    let expanded_default = expand_prompt_template("/review", &templates);
    assert_eq!(
        expanded_default,
        "Review code in branch main focusing on bugs."
    );

    let expanded_args = expand_prompt_template("/review dev security", &templates);
    assert_eq!(
        expanded_args,
        "Review code in branch dev focusing on security."
    );
}
