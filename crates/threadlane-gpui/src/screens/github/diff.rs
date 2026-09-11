use super::types::PrWorkspaceKey;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrDiffRequest {
    pub key: PrWorkspaceKey,
    pub path: String,
    pub revision: u64,
}

impl PrDiffRequest {
    pub fn new(key: PrWorkspaceKey, path: String, revision: u64) -> Self {
        Self {
            key,
            path,
            revision,
        }
    }
}

pub fn pr_diff_result_matches_request(
    result: &PrDiffRequest,
    active: Option<&PrDiffRequest>,
    selected_key: Option<&PrWorkspaceKey>,
    selected_path: Option<&str>,
) -> bool {
    active == Some(result)
        && selected_key == Some(&result.key)
        && selected_path == Some(result.path.as_str())
}

pub fn decode_git_quoted_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix] != b'\\' {
            decoded.push(bytes[ix]);
            ix += 1;
            continue;
        }
        ix += 1;
        let escaped = *bytes.get(ix)?;
        if matches!(escaped, b'0'..=b'7') {
            let mut value = 0u16;
            let mut digits = 0;
            while digits < 3 && ix < bytes.len() && matches!(bytes[ix], b'0'..=b'7') {
                value = value * 8 + u16::from(bytes[ix] - b'0');
                ix += 1;
                digits += 1;
            }
            decoded.push(u8::try_from(value).ok()?);
            continue;
        }
        decoded.push(match escaped {
            b'a' => 7,
            b'b' => 8,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 11,
            b'f' => 12,
            b'r' => b'\r',
            other => other,
        });
        ix += 1;
    }
    String::from_utf8(decoded).ok()
}

pub fn diff_header_matches_path(header: &str, path: &str) -> bool {
    let Some(rest) = header.strip_prefix("diff --git ") else {
        return false;
    };
    let expected = format!("b/{path}");
    if rest.ends_with(&format!(" {expected}")) {
        return true;
    }
    rest.rsplit_once(" \"")
        .and_then(|(_, quoted)| quoted.strip_suffix('"'))
        .and_then(decode_git_quoted_path)
        .is_some_and(|decoded| decoded == expected)
}

pub fn selected_file_diff(raw: &str, path: &str) -> Option<String> {
    let mut matched_start = None;
    let mut offset = 0;
    for line in raw.split_inclusive('\n') {
        let header = line.strip_suffix('\n').unwrap_or(line);
        let header = header.strip_suffix('\r').unwrap_or(header);
        if header.starts_with("diff --git ") {
            if let Some(start) = matched_start {
                return Some(raw[start..offset].to_owned());
            }
            if diff_header_matches_path(header, path) {
                matched_start = Some(offset);
            }
        }
        offset += line.len();
    }
    matched_start.map(|start| raw[start..].to_owned())
}

pub fn prepare_selected_diff(raw: &str, path: &str) -> Option<String> {
    let diff = selected_file_diff(raw, path)?;
    let longest_backticks = diff
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest_backticks.saturating_add(1).max(3));
    let before_fence = if diff.ends_with('\n') { "" } else { "\n" };
    Some(format!("{fence}diff\n{diff}{before_fence}{fence}"))
}
