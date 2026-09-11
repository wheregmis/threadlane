use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use gpui::Entity;
use gpui_component::text::TextViewState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkdownUpdate<'a> {
    Unchanged,
    Append(&'a str),
    Replace,
}

pub fn classify_markdown_update<'a>(current: &str, next: &'a str) -> MarkdownUpdate<'a> {
    if current == next {
        MarkdownUpdate::Unchanged
    } else if let Some(suffix) = next.strip_prefix(current) {
        MarkdownUpdate::Append(suffix)
    } else {
        MarkdownUpdate::Replace
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatLinkTarget {
    Web,
    ProjectFile(String),
    Rejected,
}

pub fn classify_chat_link(link: &str) -> ChatLinkTarget {
    if link.starts_with("http://") || link.starts_with("https://") {
        return ChatLinkTarget::Web;
    }

    let mut normalized = PathBuf::new();
    for component in Path::new(link).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir if normalized.pop() => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return ChatLinkTarget::Rejected;
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        ChatLinkTarget::Rejected
    } else {
        ChatLinkTarget::ProjectFile(normalized.to_string_lossy().into_owned())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownSegment {
    Markdown(String),
    CodeBlock {
        language: String,
        header_path: Option<String>,
        code: String,
    },
}

pub fn is_terminal_runnable_language(lang: &str) -> bool {
    lang.eq_ignore_ascii_case("bash")
        || lang.eq_ignore_ascii_case("sh")
        || lang.eq_ignore_ascii_case("zsh")
        || lang.eq_ignore_ascii_case("shell")
        || lang.eq_ignore_ascii_case("terminal")
        || lang.eq_ignore_ascii_case("console")
        || lang.eq_ignore_ascii_case("cmd")
        || lang.eq_ignore_ascii_case("powershell")
}

/// Cached active shell name, resolved once from `$SHELL`.
fn cached_shell() -> &'static str {
    static SHELL: OnceLock<String> = OnceLock::new();
    SHELL.get_or_init(|| {
        std::env::var("SHELL")
            .ok()
            .and_then(|path| {
                Path::new(&path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_ascii_lowercase)
            })
            .unwrap_or_else(|| "sh".into())
    })
}

pub fn active_shell_supports_language(lang: &str) -> bool {
    let shell = cached_shell();
    let lang = lang.trim();
    if lang.eq_ignore_ascii_case("bash") {
        shell == "bash"
    } else if lang.eq_ignore_ascii_case("zsh") {
        shell == "zsh"
    } else if lang.eq_ignore_ascii_case("sh") {
        matches!(shell, "sh" | "bash" | "zsh")
    } else if lang.eq_ignore_ascii_case("cmd") {
        matches!(shell, "cmd" | "cmd.exe")
    } else if lang.eq_ignore_ascii_case("powershell") {
        matches!(shell, "pwsh" | "powershell")
    } else if lang.eq_ignore_ascii_case("shell")
        || lang.eq_ignore_ascii_case("terminal")
        || lang.eq_ignore_ascii_case("console")
    {
        true
    } else {
        false
    }
}

pub fn normalize_terminal_command(command: &str) -> String {
    command
        .lines()
        .map(|line| {
            line.strip_prefix("$ ")
                .or_else(|| line.strip_prefix(">>> "))
                .or_else(|| line.strip_prefix("> "))
                .unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn is_fence_line(raw: &str, idx: usize) -> bool {
    let line_start = raw[..idx].rfind('\n').map_or(0, |p| p + 1);
    raw[line_start..idx].len() <= 3 && raw[line_start..idx].bytes().all(|b| b == b' ')
}

pub fn parse_code_block_header(header: &str, code: &str) -> (String, Option<String>) {
    let header_trimmed = header.trim();
    let parts: Vec<&str> = header_trimmed.split_whitespace().collect();
    let language = parts.first().copied().unwrap_or("text").to_lowercase();

    let mut detected_path = if parts.len() > 1 {
        Some(parts[1].to_string())
    } else if let Some((_, path)) = language.split_once(':') {
        Some(path.to_string())
    } else {
        None
    };

    let clean_language = language.split(':').next().unwrap_or("text").to_string();

    if detected_path.is_none() {
        if let Some(first_line) = code.lines().next() {
            let trimmed = first_line.trim();
            let comment_content = trimmed
                .strip_prefix("//")
                .or_else(|| trimmed.strip_prefix('#'))
                .or_else(|| {
                    trimmed
                        .strip_prefix("/*")
                        .and_then(|s| s.strip_suffix("*/"))
                })
                .or_else(|| {
                    trimmed
                        .strip_prefix("<!--")
                        .and_then(|s| s.strip_suffix("-->"))
                });

            if let Some(candidate) = comment_content {
                let candidate = candidate.trim();
                if candidate.contains('/')
                    && !candidate.contains(' ')
                    && candidate.len() < 120
                    && !candidate.starts_with("http")
                    && !candidate.starts_with('!')
                {
                    detected_path = Some(candidate.to_string());
                }
            }
        }
    }

    (clean_language, detected_path)
}

pub fn extract_markdown_segments(raw: &str) -> Vec<MarkdownSegment> {
    let mut segments = Vec::new();
    let mut current_pos = 0;
    let mut search_from = 0;

    while let Some(start_fence) = raw[search_from..].find("```") {
        let fence_start_idx = search_from + start_fence;

        if !is_fence_line(raw, fence_start_idx) {
            search_from = fence_start_idx + 3;
            continue;
        }

        if fence_start_idx > current_pos {
            let text = &raw[current_pos..fence_start_idx];
            if !text.is_empty() {
                segments.push(MarkdownSegment::Markdown(text.to_string()));
            }
        }

        let header_start = fence_start_idx + 3;
        let Some(header_newline) = raw[header_start..].find('\n') else {
            segments.push(MarkdownSegment::Markdown(
                raw[fence_start_idx..].to_string(),
            ));
            return segments;
        };

        let header_line = raw[header_start..header_start + header_newline].trim();
        let code_start = header_start + header_newline + 1;

        let mut close_fence_idx = None;
        let mut search_pos = code_start;
        while let Some(close_pos) = raw[search_pos..].find("```") {
            let candidate_idx = search_pos + close_pos;
            if is_fence_line(raw, candidate_idx) {
                close_fence_idx = Some(candidate_idx);
                break;
            }
            search_pos = candidate_idx + 3;
        }

        if let Some(close_idx) = close_fence_idx {
            let code = &raw[code_start..close_idx];
            let (language, header_path) = parse_code_block_header(header_line, code);
            segments.push(MarkdownSegment::CodeBlock {
                language,
                header_path,
                code: code.to_string(),
            });
            let after_close = close_idx + 3;
            let next_pos = if after_close < raw.len() && raw.as_bytes()[after_close] == b'\n' {
                after_close + 1
            } else {
                after_close
            };
            current_pos = next_pos;
            search_from = current_pos;
        } else {
            let code = &raw[code_start..];
            let (language, header_path) = parse_code_block_header(header_line, code);
            segments.push(MarkdownSegment::CodeBlock {
                language,
                header_path,
                code: code.to_string(),
            });
            return segments;
        }
    }

    if current_pos < raw.len() {
        let remainder = &raw[current_pos..];
        if !remainder.is_empty() {
            segments.push(MarkdownSegment::Markdown(remainder.to_string()));
        }
    }

    if segments.is_empty() && !raw.is_empty() {
        segments.push(MarkdownSegment::Markdown(raw.to_string()));
    }

    segments
}

pub struct MarkdownRenderState {
    pub source: String,
    pub state: Entity<TextViewState>,
}

pub const MARKDOWN_CACHE_ENTRY_LIMIT: usize = 512;

pub fn markdown_cache_exceeded(entry_count: usize) -> bool {
    entry_count > MARKDOWN_CACHE_ENTRY_LIMIT
}
