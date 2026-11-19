//! Bounded pages for agent-facing tool results (#563): a result an agent
//! reads must stay well under a backend's tool-result limit, and a cut one
//! says how to read on.

use serde_json::{Map, Value as Json, json};
use threadlane_core::tools::ToolError;

use crate::tools::invalid;

/// The most characters of items one page holds, well under the limits
/// backends put on a tool result.
pub(crate) const PAGE_CHARS: usize = 20_000;
/// The longest text field kept whole in a listed item.
pub(crate) const FIELD_CHARS: usize = 2_000;

/// The `offset` and `limit` arguments, `limit` defaulting to `default`
/// and at most `max`.
pub(crate) fn offset_limit(
    object: &Map<String, Json>,
    default: usize,
    max: usize,
) -> Result<(usize, usize), ToolError> {
    let number = |name: &str| -> Result<Option<usize>, ToolError> {
        object.get(name).map_or(Ok(None), |value| {
            value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .map(Some)
                .ok_or_else(|| invalid(&format!("{name} must be a non-negative integer")))
        })
    };
    let limit = number("limit")?.unwrap_or(default);
    if limit == 0 || limit > max {
        return Err(invalid(&format!("limit must be between 1 and {max}")));
    }
    Ok((number("offset")?.unwrap_or(0), limit))
}

/// The items from `offset`: at most `limit`, stopping before the page passes
/// [`PAGE_CHARS`] but always holding at least one, and the offset of the next
/// page if any remain. An item too large alone is cut to its text.
pub(crate) fn take(items: &[Json], offset: usize, limit: usize) -> (Vec<Json>, Option<usize>) {
    let mut page = Vec::new();
    let mut size = 0;
    for item in items.iter().skip(offset).take(limit) {
        let text = item.to_string();
        if !page.is_empty() && size + text.len() > PAGE_CHARS {
            break;
        }
        size += text.len();
        page.push(if text.len() > PAGE_CHARS {
            json!({ "clipped": clip(&text, PAGE_CHARS) })
        } else {
            item.clone()
        });
    }
    let next = offset + page.len();
    (page, (next < items.len()).then_some(next))
}

/// Adds to `result` what a cut page needs: the total, and how to get the
/// next page from `tool`.
pub(crate) fn mark(result: &mut Json, tool: &str, total: usize, next: Option<usize>) {
    result["total"] = json!(total);
    if let Some(next) = next {
        result["next_offset"] = json!(next);
        result["more"] = json!(format!(
            "This page is cut. Call {tool} again with offset {next} for the next page."
        ));
    }
}

/// `text` cut to at most `max` characters, saying how many were cut.
pub(crate) fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some((end, _)) => format!(
            "{}… [{} more characters]",
            &text[..end],
            text[end..].chars().count()
        ),
    }
}

/// `text` on one line of at most `max` characters.
pub(crate) fn one_line(text: &str, max: usize) -> String {
    clip(&text.split_whitespace().collect::<Vec<_>>().join(" "), max)
}

/// `value` with every text in it cut to [`FIELD_CHARS`].
pub(crate) fn clip_fields(value: &mut Json) {
    match value {
        Json::String(text) => *text = clip(text, FIELD_CHARS),
        Json::Array(items) => items.iter_mut().for_each(clip_fields),
        Json::Object(map) => map.values_mut().for_each(clip_fields),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_stop_at_the_limit_the_size_budget_and_the_end() {
        let items: Vec<Json> = (0..10).map(|n| json!(n)).collect();
        assert_eq!(take(&items, 0, 4), (items[..4].to_vec(), Some(4)));
        assert_eq!(take(&items, 8, 4), (items[8..].to_vec(), None));
        assert_eq!(take(&items, 6, 4), (items[6..].to_vec(), None));
        assert_eq!(take(&items, 10, 4), (vec![], None));
        assert_eq!(take(&items, 50, 4), (vec![], None));

        let big: Vec<Json> = (0..5).map(|_| json!("x".repeat(8_000))).collect();
        let (page, next) = take(&big, 0, 5);
        assert_eq!((page.len(), next), (2, Some(2)));
        let (page, next) = take(&big, 4, 5);
        assert_eq!((page.len(), next), (1, None));

        let huge = [json!("y".repeat(3 * PAGE_CHARS))];
        let (page, next) = take(&huge, 0, 1);
        assert!(page[0].to_string().len() < PAGE_CHARS + 100);
        assert_eq!(next, None);
    }

    #[test]
    fn limits_are_checked_and_defaulted() {
        let args = |value: Json| value.as_object().unwrap().clone();
        assert_eq!(offset_limit(&args(json!({})), 50, 200).unwrap(), (0, 50));
        assert_eq!(
            offset_limit(&args(json!({"offset": 3, "limit": 200})), 50, 200).unwrap(),
            (3, 200)
        );
        for bad in [
            json!({"limit": 0}),
            json!({"limit": 201}),
            json!({"offset": -1}),
            json!({"offset": "2"}),
        ] {
            assert!(offset_limit(&args(bad.clone()), 50, 200).is_err(), "{bad}");
        }
    }

    #[test]
    fn clipping_is_by_character_and_says_what_was_cut() {
        assert_eq!(clip("héllo", 5), "héllo");
        assert_eq!(clip("héllo", 2), "hé… [3 more characters]");
        assert_eq!(one_line("a\n  b\tc", 10), "a b c");
        let mut value = json!({"a": ["z".repeat(FIELD_CHARS + 5)], "b": 1});
        clip_fields(&mut value);
        assert!(
            value["a"][0]
                .as_str()
                .unwrap()
                .ends_with("[5 more characters]")
        );
        assert_eq!(value["b"], 1);
    }
}
