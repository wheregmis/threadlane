//! Web links from displayed cells only; no ANSI/OSC payloads or I/O.

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalLink {
    pub url: String,
    pub cells: Vec<(u16, u16)>,
}

/// Reject URL parser fixups (missing authority, backslashes, whitespace) and credentials.
/// Keep the displayed spelling rather than normalizing the destination.
pub fn is_web_url(text: &str) -> bool {
    let Some(authority) = text
        .strip_prefix("http://")
        .or_else(|| text.strip_prefix("https://"))
    else {
        return false;
    };
    if authority.is_empty()
        || authority.starts_with(['/', '?', '#'])
        || text
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '\\')
        || authority
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|host| host.contains('@'))
    {
        return false;
    }
    let bytes = text.as_bytes();
    if bytes.iter().enumerate().any(|(i, byte)| {
        *byte == b'%'
            && (i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit())
    }) {
        return false;
    }
    url::Url::parse(text).is_ok_and(|url| {
        url.host_str().is_some() && url.username().is_empty() && url.password().is_none()
    })
}

fn trimmed_token(token: &str) -> &str {
    let mut token = token.trim_start_matches(['(', '[', '{', '<', '"', '\'']);
    let mut balance = [0_i32; 3];
    for ch in token.chars() {
        match ch {
            '(' => balance[0] += 1,
            ')' => balance[0] -= 1,
            '[' => balance[1] += 1,
            ']' => balance[1] -= 1,
            '{' => balance[2] += 1,
            '}' => balance[2] -= 1,
            _ => {}
        }
    }
    while let Some(ch) = token.chars().next_back() {
        match ch {
            '.' | ',' | ';' | '!' | '"' | '\'' | '>' => {}
            ')' if balance[0] < 0 => balance[0] += 1,
            ']' if balance[1] < 0 => balance[1] += 1,
            '}' if balance[2] < 0 => balance[2] += 1,
            _ => break,
        }
        token = &token[..token.len() - ch.len_utf8()];
    }
    token
}

pub(crate) fn visible_links(screen: &vt100::Screen, first_continues: bool) -> Vec<TerminalLink> {
    if screen.alternate_screen() {
        return Vec::new();
    }
    let (rows, cols) = screen.size();
    let mut links = Vec::new();
    let mut token = String::new();
    // One byte-to-cell entry per displayed UTF-8 byte, not one cell per byte.
    let mut positions = Vec::new();
    let mut ambiguous = first_continues;
    let finish = |token: &mut String,
                  positions: &mut Vec<(u16, u16)>,
                  ambiguous: bool,
                  links: &mut Vec<TerminalLink>| {
        let trimmed = trimmed_token(token);
        if !ambiguous && is_web_url(trimmed) {
            let start = trimmed.as_ptr() as usize - token.as_ptr() as usize;
            let mut cells = positions[start..start + trimmed.len()].to_vec();
            cells.dedup();
            let wide: Vec<_> = cells
                .iter()
                .filter_map(|&(row, col)| {
                    screen
                        .cell(row, col)
                        .filter(|cell| cell.is_wide())
                        .map(|_| (row, col + 1))
                })
                .collect();
            cells.extend(wide);
            cells.sort_unstable();
            links.push(TerminalLink {
                url: trimmed.to_string(),
                cells,
            });
        }
        token.clear();
        positions.clear();
    };
    for row in 0..rows {
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let contents = cell.contents();
            let contents = if contents.is_empty() { " " } else { contents };
            for ch in contents.chars() {
                if ch.is_whitespace() || matches!(ch, '<' | '>' | '\"' | '\'') {
                    finish(&mut token, &mut positions, ambiguous, &mut links);
                    ambiguous = false;
                } else {
                    token.push(ch);
                    positions.extend(std::iter::repeat_n((row, col), ch.len_utf8()));
                }
            }
        }
        if !screen.row_wrapped(row) {
            // A token touching the right edge may still be an unfinished wrap.
            finish(
                &mut token,
                &mut positions,
                ambiguous || row + 1 == rows,
                &mut links,
            );
            ambiguous = false;
        }
    }
    // Never finish a token continuing beyond the captured viewport.
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(text: &str, cols: u16) -> Vec<TerminalLink> {
        let mut parser = vt100::Parser::new(8, cols, 10);
        parser.process(text.as_bytes());
        visible_links(parser.screen(), false)
    }

    #[test]
    fn recognizes_displayed_urls_and_balanced_punctuation() {
        let found = links(
            "\x1b[32m(http://localhost:3000/a(b)?x=1#frag),\x1b[0m https://[::1]:8080/.",
            100,
        );
        assert_eq!(
            found
                .iter()
                .map(|link| link.url.as_str())
                .collect::<Vec<_>>(),
            ["http://localhost:3000/a(b)?x=1#frag", "https://[::1]:8080/"]
        );
    }

    #[test]
    fn maps_wide_combining_cells_and_soft_wraps() {
        let found = links("界e\u{301} http://localhost:3000/long/path ", 20);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].url, "http://localhost:3000/long/path");
        assert_eq!(found[0].cells[0], (0, 4));
        assert!(found[0].cells.contains(&(1, 0)));
    }

    #[test]
    fn rejects_unsafe_malformed_hidden_and_clipped_links() {
        for text in [
            "file:///tmp/foo",
            "javascript:alert(1)",
            "http://user:pass@host/",
            "http:///host",
            "http://host:99999",
            "http://host\\evil",
            "xhttp://host",
            "\x1b]8;;http://hidden\x07label\x1b]8;;\x07",
        ] {
            assert!(links(text, 100).is_empty(), "{text:?}");
        }
        let mut parser = vt100::Parser::new(2, 16, 0);
        parser.process(b"\x1b[2;1Hhttp://host/abcd");
        assert!(visible_links(parser.screen(), false).is_empty());
        let mut parser = vt100::Parser::new(2, 30, 0);
        parser.process(b"http://host/path ");
        assert!(visible_links(parser.screen(), true).is_empty());
    }

    #[test]
    fn hard_newlines_do_not_join_and_alt_screen_is_unavailable() {
        assert_eq!(links("http://host/a\r\nb", 30)[0].url, "http://host/a");
        let mut parser = vt100::Parser::new(3, 30, 0);
        parser.process(b"\x1b[?1049hhttp://host/");
        assert!(visible_links(parser.screen(), false).is_empty());
    }
}
