//! Read, search and directory presentation shared by every GPUI host.
use super::tool_detail::{
    args_json, card_container, card_header, highlighted_code, preview_viewport,
};
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Sizable};
use threadlane_protocol::daemon::ToolActivityInfo;
/// Resolve the language used by the editor and inline file previews.
pub fn detect_language(path_str: &str) -> &'static str {
    let path = std::path::Path::new(path_str);
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("rs") => "rust",
        Some("py" | "pyi") => "python",
        Some("js" | "mjs" | "cjs" | "jsx") => "javascript",
        Some("ts" | "mts" | "cts") => "typescript",
        Some("tsx") => "tsx",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("html" | "htm") => "html",
        Some("css") => "css",
        Some("md" | "markdown") => "markdown",
        Some("sh" | "bash" | "zsh") => "bash",
        Some("go") => "go",
        Some("c" | "h") => "c",
        Some("cpp" | "hpp" | "cc" | "cxx" | "hh") => "cpp",
        Some("diff" | "patch") => "diff",
        Some("zig") => "zig",
        Some("java") => "java",
        Some("rb" | "gemspec") => "ruby",
        Some("sql") => "sql",
        Some("cmake") => "cmake",
        _ => match path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|s| s.to_lowercase())
            .as_deref()
        {
            Some("dockerfile") => "bash",
            Some("cargo.lock") => "toml",
            Some("makefile" | "gnumakefile") => "make",
            Some("cmakelists.txt") => "cmake",
            Some("gemfile" | "rakefile") => "ruby",
            Some(".bashrc" | ".bash_profile" | ".zshrc" | ".profile") => "bash",
            _ => "text",
        },
    }
}

fn read_line(line: &str) -> Option<(usize, &str)> {
    let (anchor, text) = line.split_once('|')?;
    let (number, hash) = anchor.split_once(':')?;
    if hash.len() != 3 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let number = number.parse::<usize>().ok().filter(|n| *n > 0)?;
    Some((number, text))
}

fn search_line(line: &str) -> Option<(&str, usize, &str)> {
    // Find the numeric separator; file names and match text may contain colons.
    line.match_indices(':').find_map(|(ix, _)| {
        let (number, text) = line[ix + 1..].split_once(':')?;
        let number = number.parse::<usize>().ok().filter(|n| *n > 0)?;
        let path = &line[..ix];
        (!path.is_empty()).then_some((path, number, text))
    })
}

fn match_highlights(
    text: &str,
    pattern: &str,
    color: Hsla,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    if pattern.is_empty() {
        return Vec::new();
    }
    text.match_indices(pattern)
        .map(|(start, value)| {
            (
                start..start + value.len(),
                HighlightStyle {
                    background_color: Some(color.opacity(0.2)),
                    ..Default::default()
                },
            )
        })
        .collect()
}

/// Shared file action appearance. Hosts validate targets and supply capabilities.
pub fn open_button(
    id: String,
    path: &str,
    line: Option<usize>,
    folder: bool,
    enabled: bool,
    on_open: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    let label = if folder {
        format!("Reveal folder {path}")
    } else if let Some(line) = line {
        format!("Open {path} at line {line}")
    } else {
        format!("Open {path} in editor")
    };
    Button::new(SharedString::from(id))
        .ghost()
        .xsmall()
        .accessibility_label(label.clone())
        .tooltip(label)
        .disabled(!enabled)
        .on_click(on_open)
}
pub fn render(
    activity: &ToolActivityInfo,
    path: String,
    entry_base: std::path::PathBuf,
    open: impl Fn(String, String, Option<usize>, bool) -> Button,
    cx: &mut App,
) -> Option<AnyElement> {
    let tool = activity.title.trim().to_lowercase().replace(' ', "_");
    if !matches!(tool.as_str(), "read_file" | "grep_search" | "list_dir") {
        return None;
    }
    let theme = cx.theme().colors;
    let args = args_json(&activity.arguments).unwrap_or_default();
    let pending = activity.category == "Working"
        && (activity.detail.trim().is_empty()
            || activity.detail.trim() == activity.arguments.trim());
    let mut copy_text = (!pending && !activity.detail.is_empty()).then(|| activity.detail.clone());
    let mut header = card_header(&theme)
        .debug_selector(|| "tool-preview-header".into())
        .child(
            Icon::new(match tool.as_str() {
                "grep_search" => IconName::Search,
                "list_dir" => IconName::Folder,
                _ => IconName::File,
            })
            .xsmall(),
        );
    let title = if tool == "grep_search" {
        format!(
            "Search · {}",
            args.get("pattern").and_then(|v| v.as_str()).unwrap_or("")
        )
    } else {
        path.clone()
    };
    header = header.child(
        div()
            .id(SharedString::from(format!("preview-title-{}", activity.id)))
            .min_w_0()
            .flex_1()
            .truncate()
            .text_sm()
            .tooltip({
                let title = title.clone();
                move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(title.clone()).build(window, cx)
                }
            })
            .child(title),
    );
    let mut rows = Vec::new();
    if pending {
        rows.push(
            div()
                .text_color(theme.muted_foreground)
                .child("Loading…")
                .into_any_element(),
        );
    } else if activity.category == "Error" {
        rows.push(
            div()
                .text_color(theme.danger)
                .child(activity.detail.clone())
                .into_any_element(),
        );
    } else if tool == "read_file" {
        let source = activity
            .detail
            .lines()
            .filter_map(read_line)
            .collect::<Vec<_>>();
        if source.is_empty() {
            rows.push(div().child(activity.detail.clone()).into_any_element());
        } else {
            let first = source[0].0;
            let last = source.last().unwrap().0;
            header = header
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{first}–{last}")),
                )
                .child(
                    open(
                        format!("read-open-{}", activity.id),
                        path.clone(),
                        Some(first),
                        false,
                    )
                    .debug_selector(|| "tool-preview-open".into())
                    .icon(IconName::ExternalLink)
                    .label("Open"),
                );
            let numbers = source
                .iter()
                .map(|(no, _)| no.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            let code = source
                .iter()
                .map(|(_, text)| *text)
                .collect::<Vec<_>>()
                .join("\n");
            copy_text = Some(code.clone());
            rows.push(
                div()
                    .flex()
                    .items_start()
                    .gap_3()
                    .child(
                        div()
                            .flex_none()
                            .text_right()
                            .text_color(theme.muted_foreground)
                            .child(numbers),
                    )
                    .child(div().child(highlighted_code(code, detect_language(&path), cx)))
                    .into_any_element(),
            );
            // Keep continuation and recovery notices visible, but omit snapshot metadata.
            rows.extend(
                activity
                    .detail
                    .lines()
                    .filter(|line| {
                        read_line(line).is_none() && !line.starts_with("[Threadlane read_file ")
                    })
                    .map(|line| {
                        div()
                            .text_color(theme.muted_foreground)
                            .child(line.to_string())
                            .into_any_element()
                    }),
            );
        }
    } else if tool == "grep_search" {
        let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        let mut previous_path = "";
        for line in activity.detail.lines() {
            let Some((path, number, text)) = search_line(line) else {
                rows.push(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(line.to_string())
                        .into_any_element(),
                );
                continue;
            };
            if path != previous_path {
                rows.push(
                    div()
                        .mt_1()
                        .text_color(theme.muted_foreground)
                        .child(path.to_string())
                        .into_any_element(),
                );
                previous_path = path;
            }
            let selector = format!("tool-preview-search-match-{path}-{number}");
            rows.push(
                open(
                    format!("search-{}-{path}-{number}", activity.id),
                    path.into(),
                    Some(number),
                    false,
                )
                .debug_selector(move || selector.clone())
                .justify_start()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.muted_foreground)
                        .child(number.to_string()),
                )
                .child(
                    StyledText::new(text.to_string()).with_highlights(match_highlights(
                        text,
                        pattern,
                        theme.primary,
                    )),
                )
                .into_any_element(),
            );
        }
    } else {
        for line in activity.detail.lines() {
            let entry = line
                .strip_prefix("[DIR]  ")
                .map(|name| (name, true))
                .or_else(|| line.strip_prefix("[FILE] ").map(|name| (name, false)));
            let Some((name, folder)) = entry else {
                rows.push(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(line.to_string())
                        .into_any_element(),
                );
                continue;
            };
            let entry_path = entry_base.join(name).to_string_lossy().into_owned();
            let selector = format!("tool-preview-directory-entry-{name}");
            rows.push(
                open(
                    format!("directory-{}-{entry_path}", activity.id),
                    entry_path,
                    None,
                    folder,
                )
                .debug_selector(move || selector.clone())
                .icon(if folder {
                    IconName::Folder
                } else {
                    IconName::File
                })
                .label(name.to_string())
                .justify_start()
                .into_any_element(),
            );
        }
        if rows.is_empty() {
            rows.push(
                div()
                    .text_color(theme.muted_foreground)
                    .child("Empty directory")
                    .into_any_element(),
            );
        }
    }
    header = header
        .children(copy_text.map(|text| crate::surfaces::result_copy_button(&activity.id, text)));
    Some(
        card_container(&theme)
            .debug_selector(|| "tool-preview-card".into())
            .child(header)
            .child(
                preview_viewport(format!("tool-preview-{}", activity.id))
                    .debug_selector(|| "tool-preview-viewport".into())
                    .child(crate::result_scroll_body(
                        format!("tool-preview-scroll-{}", activity.id),
                        div()
                            .text_color(theme.foreground)
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_xs()
                            .child(
                                div()
                                    .debug_selector(|| "tool-preview-content".into())
                                    .p_2()
                                    .flex()
                                    .flex_col()
                                    .items_start()
                                    .gap_1()
                                    .children(rows),
                            ),
                    )),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::{match_highlights, read_line, search_line};
    #[test]
    fn native_tool_rows_preserve_source_and_match_boundaries() {
        assert_eq!(
            read_line("12:a3f|  let x = \"é\";"),
            Some((12, "  let x = \"é\";"))
        );
        assert_eq!(read_line("[Continue reading at start_line: 13]"), None);
        assert_eq!(read_line("0:a3f|invalid"), None);
        assert_eq!(
            search_line("src/a:b.rs:12:é: needle"),
            Some(("src/a:b.rs", 12, "é: needle"))
        );
        assert_eq!(search_line("No matches found."), None);
        let ranges = match_highlights("é needle needle", "needle", gpui::Hsla::default());
        assert_eq!(
            ranges
                .iter()
                .map(|(range, _)| range.clone())
                .collect::<Vec<_>>(),
            vec![3..9, 10..16]
        );
        assert!(match_highlights("text", "", gpui::Hsla::default()).is_empty());
    }
}
