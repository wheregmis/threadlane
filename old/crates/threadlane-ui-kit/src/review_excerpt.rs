//! Literal displayed-patch excerpts. Hosts own checkout and composer identity.
use std::{ops::Range, path::PathBuf};

use gpui::{Entity, WeakEntity};
use gpui_component::text::TextViewState;

use crate::{ReviewDiffDocument, EDITOR_EXCERPT_LIMIT};

pub const STALE_REVIEW_EXCERPT: &str = "The diff or chat changed. Select the text again.";

/// Explicit destination and loaded request identity supplied by the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewExcerptTarget {
    pub(crate) checkout: PathBuf,
    pub(crate) relative_path: String,
    pub(crate) destination: (Option<PathBuf>, Option<String>),
    pub(crate) revision: u64,
    pub(crate) ignore_whitespace: bool,
}

impl ReviewExcerptTarget {
    pub fn new(
        checkout: PathBuf,
        relative_path: String,
        destination: (Option<PathBuf>, Option<String>),
        revision: u64,
        ignore_whitespace: bool,
    ) -> Self {
        Self {
            checkout,
            relative_path,
            destination,
            revision,
            ignore_whitespace,
        }
    }

    pub fn destination(&self) -> &(Option<PathBuf>, Option<String>) {
        &self.destination
    }
}

/// Bounded capture made before command focus clears window text selection.
#[derive(Clone)]
pub struct ReviewExcerptRequest {
    pub(crate) document: Entity<ReviewDiffDocument>,
    pub(crate) snapshot: ReviewExcerptSnapshot,
}

#[derive(Clone)]
pub(crate) struct ReviewExcerptSnapshot {
    pub(crate) target: ReviewExcerptTarget,
    pub(crate) text: String,
    pub(crate) source: WeakEntity<TextViewState>,
    pub(crate) source_range: Option<Range<usize>>,
    pub(crate) content_revision: u64,
}

impl ReviewExcerptRequest {
    pub fn document(&self) -> &Entity<ReviewDiffDocument> {
        &self.document
    }
    pub fn target(&self) -> &ReviewExcerptTarget {
        &self.snapshot.target
    }
    pub fn text(&self) -> &str {
        &self.snapshot.text
    }
}

pub fn review_excerpt_block_reason(selected: &str) -> Option<&'static str> {
    if selected.len() > EDITOR_EXCERPT_LIMIT {
        Some("Select less diff text (maximum 32 KiB)")
    } else if selected.trim().is_empty() {
        Some("Select text in the diff first")
    } else {
        None
    }
}

/// Safe fences preserve exact selected display text, including partial lines.
pub fn format_review_excerpt(
    target: &ReviewExcerptTarget,
    selected: &str,
) -> Result<String, &'static str> {
    if let Some(reason) = review_excerpt_block_reason(selected) {
        return Err(reason);
    }
    let provenance = if target.ignore_whitespace {
        "Whitespace-filtered patch snapshot"
    } else {
        "Displayed patch snapshot"
    };
    let fence = crate::safe_fence(selected);
    Ok(format!(
        "Review diff excerpt: {}\n{provenance}\n{fence}diff\n{selected}\n{fence}",
        target.relative_path,
    ))
}

#[cfg(test)]
mod tests {
    use super::{format_review_excerpt, review_excerpt_block_reason, ReviewExcerptTarget};
    use crate::EDITOR_EXCERPT_LIMIT;

    #[test]
    fn review_excerpt_preserves_literal_text_and_provenance() {
        let mut target = ReviewExcerptTarget {
            checkout: "/checkout".into(),
            relative_path: "src/🦀.rs".into(),
            destination: (Some("/project".into()), Some("session".into())),
            revision: 1,
            ignore_whitespace: false,
        };
        let text = "-old\r\n+  café ```literal```\n context\n\npartial  ";
        assert_eq!(
            format_review_excerpt(&target, text).unwrap(),
            format!(
                "Review diff excerpt: src/🦀.rs\nDisplayed patch snapshot\n````diff\n{text}\n````"
            )
        );
        target.ignore_whitespace = true;
        assert!(format_review_excerpt(&target, text)
            .unwrap()
            .contains("Whitespace-filtered patch snapshot"));
        assert!(format_review_excerpt(&target, "+line\n\n")
            .unwrap()
            .ends_with("+line\n\n\n```"));
        assert!(format_review_excerpt(&target, "").is_err());
        assert!(format_review_excerpt(&target, &"x".repeat(EDITOR_EXCERPT_LIMIT + 1)).is_err());
    }

    #[test]
    fn review_excerpt_utf8_limit_rejects_without_truncation() {
        assert!(review_excerpt_block_reason("").is_some());
        assert!(review_excerpt_block_reason(" \r\n\t").is_some());
        assert!(review_excerpt_block_reason(&"🦀".repeat(EDITOR_EXCERPT_LIMIT / 4)).is_none());
        assert!(review_excerpt_block_reason(&format!(
            "{}x",
            "🦀".repeat(EDITOR_EXCERPT_LIMIT / 4)
        ))
        .is_some());
    }
}
