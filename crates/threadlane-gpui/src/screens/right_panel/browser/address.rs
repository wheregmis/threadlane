//! Safari-style address resolution for the embedded browser.
//!
//! Pure logic, no platform dependencies: safe to unit test on any host.

/// What the address input resolves to when the user submits it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AddressTarget {
    Url(String),
    Search(String),
}

/// Explicit schemes pass through, host-like text gets a scheme guessed for
/// it, anything else becomes a web search.
pub(crate) fn resolve_address(raw: &str) -> Option<AddressTarget> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let has_scheme = trimmed.split_once(':').is_some_and(|(scheme, rest)| {
        !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && scheme
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
            && (rest.starts_with("//") || matches!(scheme, "about" | "data" | "mailto" | "file"))
    });
    if has_scheme {
        return Some(AddressTarget::Url(trimmed.to_owned()));
    }

    if trimmed.contains(char::is_whitespace) {
        return Some(AddressTarget::Search(trimmed.to_owned()));
    }

    let authority = trimmed.split(['/', '?', '#']).next().unwrap_or(trimmed);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            (host, true)
        }
        Some(_) => return Some(AddressTarget::Search(trimmed.to_owned())),
        None => (authority, false),
    };
    let is_ip = !host.is_empty()
        && host.chars().all(|c| c.is_ascii_digit() || c == '.')
        && host.split('.').count() == 4;
    let is_local = host.eq_ignore_ascii_case("localhost") || is_ip;
    let host_like = is_local
        || (host.contains('.')
            && !host.starts_with('.')
            && !host.ends_with('.')
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')));

    if !host_like {
        return Some(AddressTarget::Search(trimmed.to_owned()));
    }
    // Dev servers rarely speak TLS; the public web rarely speaks anything else.
    let scheme = if is_local || (port && host.eq_ignore_ascii_case("localhost")) {
        "http"
    } else {
        "https"
    };
    Some(AddressTarget::Url(format!("{scheme}://{trimmed}")))
}

pub(crate) fn search_url(query: &str) -> String {
    let mut encoded = String::with_capacity(query.len() * 3);
    for byte in query.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    format!("https://www.google.com/search?q={encoded}")
}

/// The address bar hides `https://` the way Safari does; everything else —
/// including `http://` — stays visible because it is information.
///
/// Kept for the upcoming address-echo + agent-tool wiring; not yet called.
#[allow(dead_code)]
pub(crate) fn display_url(url: &str) -> &str {
    url.strip_prefix("https://").unwrap_or(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_resolves_to_none() {
        assert_eq!(resolve_address(""), None);
        assert_eq!(resolve_address("   "), None);
    }

    #[test]
    fn explicit_scheme_passes_through() {
        assert_eq!(
            resolve_address("https://example.com"),
            Some(AddressTarget::Url("https://example.com".to_owned()))
        );
        assert_eq!(
            resolve_address("http://localhost:3000/app"),
            Some(AddressTarget::Url("http://localhost:3000/app".to_owned()))
        );
    }

    #[test]
    fn host_like_gets_https() {
        assert_eq!(
            resolve_address("example.com"),
            Some(AddressTarget::Url("https://example.com".to_owned()))
        );
    }

    #[test]
    fn localhost_gets_http() {
        assert_eq!(
            resolve_address("localhost:3000"),
            Some(AddressTarget::Url("http://localhost:3000".to_owned()))
        );
        assert_eq!(
            resolve_address("localhost"),
            Some(AddressTarget::Url("http://localhost".to_owned()))
        );
    }

    #[test]
    fn phrases_become_search() {
        assert_eq!(
            resolve_address("how to center a div"),
            Some(AddressTarget::Search("how to center a div".to_owned()))
        );
    }

    #[test]
    fn display_hides_https_only() {
        assert_eq!(display_url("https://example.com"), "example.com");
        assert_eq!(
            display_url("http://localhost:3000"),
            "http://localhost:3000"
        );
    }
}
