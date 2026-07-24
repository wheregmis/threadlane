use std::collections::HashMap;

/// Parsed markdown document containing frontmatter key-value metadata and body content.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedFrontmatter {
    pub metadata: HashMap<String, String>,
    pub body: String,
    pub parse_error: Option<String>,
}

impl ParsedFrontmatter {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.metadata.get(key).map(|s| s.as_str())
    }
}

/// Parse frontmatter (delimited by `---`) and body from markdown content.
pub fn parse_frontmatter(content: &str) -> ParsedFrontmatter {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return ParsedFrontmatter {
            metadata: HashMap::new(),
            body: content.to_string(),
            parse_error: None,
        };
    }

    let rest = &trimmed[3..];
    let end_idx = match rest.find("---") {
        Some(idx) => idx,
        None => {
            return ParsedFrontmatter {
                metadata: HashMap::new(),
                body: content.to_string(),
                parse_error: Some("Unclosed frontmatter delimiter '---'".into()),
            };
        }
    };

    let frontmatter_block = &rest[..end_idx];
    let body = rest[end_idx + 3..].trim().to_string();
    let mut metadata = HashMap::new();

    for line in frontmatter_block.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once(':') {
            let key = key.trim().to_string();
            let val = val.trim().trim_matches('"').trim_matches('\'').to_string();
            metadata.insert(key, val);
        }
    }

    ParsedFrontmatter {
        metadata,
        body,
        parse_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter_valid() {
        let text = "---\nname: test_agent\ndescription: \"Test Description\"\n---\nHello world body";
        let parsed = parse_frontmatter(text);
        assert_eq!(parsed.get("name"), Some("test_agent"));
        assert_eq!(parsed.get("description"), Some("Test Description"));
        assert_eq!(parsed.body, "Hello world body");
        assert!(parsed.parse_error.is_none());
    }

    #[test]
    fn test_parse_frontmatter_no_delimiter() {
        let text = "Hello world body";
        let parsed = parse_frontmatter(text);
        assert!(parsed.metadata.is_empty());
        assert_eq!(parsed.body, "Hello world body");
        assert!(parsed.parse_error.is_none());
    }

    #[test]
    fn test_parse_frontmatter_unclosed() {
        let text = "---\nname: test\nbody text";
        let parsed = parse_frontmatter(text);
        assert_eq!(parsed.parse_error, Some("Unclosed frontmatter delimiter '---'".into()));
    }
}
