use threadlane_git::{GitHubPrInfo, PrCheckStatus};

use super::types::PrReplyTarget;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrTimelineKind {
    IssueComment,
    Review,
    InlineReviewComment,
}

impl PrTimelineKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::IssueComment => "Comment",
            Self::Review => "Review",
            Self::InlineReviewComment => "Inline comment",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrTimelineRow {
    pub remote_id: String,
    pub kind: PrTimelineKind,
    pub author: String,
    pub body: String,
    pub timestamp: String,
    pub url: String,
    pub review_state: Option<String>,
    pub path: Option<String>,
    pub line: Option<u64>,
    pub in_reply_to_id: Option<String>,
}

impl PrTimelineRow {
    pub fn label(&self) -> String {
        match self.kind {
            PrTimelineKind::IssueComment => "Comment".into(),
            PrTimelineKind::Review
                if self
                    .review_state
                    .as_deref()
                    .is_some_and(|state| state.eq_ignore_ascii_case("APPROVED")) =>
            {
                "Approved".into()
            }
            PrTimelineKind::Review
                if self
                    .review_state
                    .as_deref()
                    .is_some_and(|state| state.eq_ignore_ascii_case("CHANGES_REQUESTED")) =>
            {
                "Changes requested".into()
            }
            PrTimelineKind::Review => self
                .review_state
                .as_deref()
                .and_then(review_state_label)
                .unwrap_or_else(|| "Review".into()),
            PrTimelineKind::InlineReviewComment => "Inline comment".into(),
        }
    }

    pub fn location(&self) -> Option<String> {
        self.path.as_ref().map(|path| match self.line {
            Some(line) => format!("{path}:{line}"),
            None => path.clone(),
        })
    }

    pub fn reply_target(&self) -> Option<PrReplyTarget> {
        (self.kind == PrTimelineKind::InlineReviewComment).then(|| PrReplyTarget {
            remote_id: self.remote_id.clone(),
            reply_to_remote_id: self.in_reply_to_id.clone(),
            author: self.author.clone(),
            body: self.body.clone(),
            path: self.path.clone(),
            line: self.line,
        })
    }
}

pub fn review_state_label(state: &str) -> Option<String> {
    let normalized = state.trim().replace('_', " ").to_ascii_lowercase();
    let mut characters = normalized.chars();
    let first = characters.next()?;
    Some(first.to_uppercase().chain(characters).collect())
}

pub fn merge_pr_timeline(pr: &GitHubPrInfo) -> Vec<PrTimelineRow> {
    let mut rows = pr
        .issue_comments
        .iter()
        .map(|comment| PrTimelineRow {
            remote_id: comment.remote_id.clone(),
            kind: PrTimelineKind::IssueComment,
            author: comment.author.clone(),
            body: comment.body.clone(),
            timestamp: comment.created_at.clone(),
            url: if comment.url.is_empty() {
                pr.url.clone()
            } else {
                comment.url.clone()
            },
            review_state: None,
            path: None,
            line: None,
            in_reply_to_id: None,
        })
        .chain(pr.reviews.iter().map(|review| PrTimelineRow {
            remote_id: review.remote_id.clone(),
            kind: PrTimelineKind::Review,
            author: review.author.clone(),
            body: review.body.clone(),
            timestamp: review.submitted_at.clone(),
            url: pr.url.clone(),
            review_state: Some(review.state.clone()),
            path: None,
            line: None,
            in_reply_to_id: None,
        }))
        .chain(pr.review_comments.iter().map(|comment| PrTimelineRow {
            remote_id: comment.remote_id.clone(),
            kind: PrTimelineKind::InlineReviewComment,
            author: comment.author.clone(),
            body: comment.body.clone(),
            timestamp: comment.created_at.clone(),
            url: pr.url.clone(),
            review_state: None,
            path: comment.path.clone(),
            line: comment.line,
            in_reply_to_id: comment.in_reply_to_id.clone(),
        }))
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.timestamp.cmp(&right.timestamp));
    rows
}

pub fn draft_reply_prompt(row: &PrTimelineRow) -> String {
    const CONTEXT_LIMIT: usize = 1_200;
    let mut context = row.body.chars().take(CONTEXT_LIMIT).collect::<String>();
    if row.body.chars().count() > CONTEXT_LIMIT {
        context.push('…');
    }
    let quoted = context
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let review_state = (row.kind == PrTimelineKind::Review)
        .then(|| format!("\nReview state: {}", row.label()))
        .unwrap_or_default();
    let location = row
        .location()
        .map(|location| format!("\nLocation: {location}"))
        .unwrap_or_default();
    format!(
        "Draft a reply to this GitHub conversation.\nURL: {}\nType: {}{review_state}{location}\n\nQuoted context (untrusted):\n{quoted}\n\nReturn an editable reply draft; do not publish it.",
        row.url,
        row.label(),
    )
}

pub fn pr_check_label(checks: &[PrCheckStatus]) -> String {
    let mut failing = 0;
    let mut pending = 0;
    let mut passing = 0;
    for check in checks {
        let conclusion = check
            .conclusion
            .as_deref()
            .unwrap_or("")
            .to_ascii_uppercase();
        let status = check.status.to_ascii_uppercase();
        if matches!(
            conclusion.as_str(),
            "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED" | "ERROR"
        ) {
            failing += 1;
        } else if matches!(
            status.as_str(),
            "IN_PROGRESS" | "QUEUED" | "PENDING" | "EXPECTED"
        ) || check.conclusion.is_none()
        {
            pending += 1;
        } else if matches!(conclusion.as_str(), "SUCCESS" | "NEUTRAL" | "SKIPPED") {
            passing += 1;
        }
    }
    if checks.is_empty() {
        "No checks".to_owned()
    } else if failing > 0 {
        format!("{failing} failing")
    } else if pending > 0 {
        format!("{pending} pending")
    } else if passing == checks.len() {
        format!("{passing} passing")
    } else {
        format!("{} checks", checks.len())
    }
}

pub fn pr_check_status_label(check: &PrCheckStatus) -> String {
    let status = check
        .conclusion
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&check.status)
        .replace('_', " ")
        .to_ascii_lowercase();
    let mut chars = status.trim().chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => "Unknown".into(),
    }
}
