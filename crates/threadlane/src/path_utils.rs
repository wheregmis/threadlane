//! Shared path formatting and string truncation utilities.

use std::path::Path;

/// Compact a filesystem path for clean UI display.
/// Replaces home directory with `~` and truncates intermediate path segments
/// if there are more than 3 path components (e.g. `~/first/…/penultimate/last`).
pub fn compact_workspace_path(path: &Path, home: Option<&Path>) -> String {
    let (prefix, display_path) = match (path.is_absolute(), home) {
        (true, Some(home)) => match path.strip_prefix(home).ok() {
            Some(relative) => ("~", relative),
            None => ("", path),
        },
        _ if path.is_absolute() => ("", path),
        _ => ("", path),
    };
    let components: Vec<_> = display_path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();

    if components.is_empty() {
        return if prefix == "~" {
            "~".to_string()
        } else {
            path.display().to_string()
        };
    }

    let compacted = if components.len() > 3 {
        format!(
            "{}/…/{}/{}",
            components[0],
            components[components.len() - 2],
            components[components.len() - 1]
        )
    } else {
        components.join("/")
    };

    match prefix {
        "~" => format!("~/{compacted}"),
        _ if path.is_absolute() => format!("/{compacted}"),
        _ => compacted,
    }
}

/// Truncate a string to at most `max_len` unicode characters, appending an ellipsis if truncated.
pub fn truncate_chars(s: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_len {
        s.to_string()
    } else {
        let mut truncated: String = chars[..max_len.saturating_sub(1)].iter().collect();
        truncated.push('…');
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_compact_workspace_path() {
        let home = PathBuf::from("/Users/test");
        let path = PathBuf::from("/Users/test/projects/a/b/c/file.rs");
        let result = compact_workspace_path(&path, Some(&home));
        assert_eq!(result, "~/projects/…/c/file.rs");

        let short_path = PathBuf::from("/Users/test/projects/file.rs");
        let short_result = compact_workspace_path(&short_path, Some(&home));
        assert_eq!(short_result, "~/projects/file.rs");
    }

    #[test]
    fn test_truncate_chars() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("hello world", 5), "hell…");
    }
}
