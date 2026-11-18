//! The starter agents compiled into the hub (docs/agent-templates.md).

use threadlane_core::client::AgentTemplate;

/// Each template's id and its file under `templates/agents/`.
const FILES: [(&str, &str); 5] = [
    (
        "chief-of-staff",
        include_str!("../../../templates/agents/chief-of-staff.md"),
    ),
    (
        "engineer",
        include_str!("../../../templates/agents/engineer.md"),
    ),
    (
        "reviewer",
        include_str!("../../../templates/agents/reviewer.md"),
    ),
    (
        "reporter",
        include_str!("../../../templates/agents/reporter.md"),
    ),
    (
        "architect",
        include_str!("../../../templates/agents/architect.md"),
    ),
];

/// Every template, in the order of [`FILES`].
pub(crate) fn all() -> Result<Vec<AgentTemplate>, String> {
    FILES.iter().map(|(id, text)| parse(id, text)).collect()
}

/// A template file: `# <Name>: <role>`, a `Backends:` line, then the
/// instructions.
fn parse(id: &str, text: &str) -> Result<AgentTemplate, String> {
    let bad = |what: &str| format!("template {id}: {what}");
    let mut lines = text.lines();
    let (name, role) = lines
        .next()
        .and_then(|line| line.strip_prefix("# "))
        .and_then(|line| line.split_once(':'))
        .map(|(name, role)| (name.trim(), role.trim()))
        .filter(|(name, role)| !name.is_empty() && !role.is_empty())
        .ok_or_else(|| bad("the first line must be `# <Name>: <role>`"))?;
    let backends = lines
        .next()
        .and_then(|line| line.strip_prefix("Backends:"))
        .ok_or_else(|| bad("the second line must be `Backends: ...`"))?
        .split(',')
        .map(|backend| {
            backend
                .trim()
                .parse()
                .map_err(|error| bad(&format!("{error}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let instructions = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    if instructions.is_empty() {
        return Err(bad("the instructions are empty"));
    }
    Ok(AgentTemplate {
        id: id.to_owned(),
        name: name.to_owned(),
        role: role.to_owned(),
        backends,
        instructions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_parses_with_a_name_role_backends_and_instructions() {
        let templates = all().expect("every template parses");
        assert_eq!(templates.len(), FILES.len());
        for template in templates {
            assert!(!template.name.is_empty(), "{}", template.id);
            assert!(!template.role.is_empty(), "{}", template.id);
            assert!(!template.backends.is_empty(), "{}", template.id);
            assert!(!template.instructions.is_empty(), "{}", template.id);
        }
    }

    #[test]
    fn a_malformed_template_is_refused() {
        let body = "Backends: claude\n\nDo it.";
        assert!(parse("x", &format!("# X: does x\n{body}")).is_ok());
        assert!(parse("x", &format!("X: does x\n{body}")).is_err());
        assert!(parse("x", &format!("# X\n{body}")).is_err());
        assert!(parse("x", "# X: does x\nBackends: gpt\n\nDo it.").is_err());
        assert!(parse("x", "# X: does x\nBackends: claude\n\n").is_err());
    }
}
