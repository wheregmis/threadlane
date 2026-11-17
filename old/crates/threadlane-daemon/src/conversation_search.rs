//! Bounded, read-only scan of saved user/assistant message text across one
//! project's discovered sessions. No journal writes, no persistent index —
//! every search re-reads transcripts through the same durable paging the
//! hydration path uses (`compute_session_messages`), and matching reuses the
//! find strip's exact semantics via `threadlane_protocol::transcript`.
//!
//! Queries, snippets, and session text never enter the journal or logs.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use threadlane_protocol::transcript::first_conversation_match;

use crate::projection::compute_session_messages;

/// A single transcript is fully projected before matching, so very large
/// session files are skipped instead of allocating unbounded text.
const SESSION_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// Total transcript bytes one search will read across all sessions.
const TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// One row per matching session; more matches only mean a wider net.
const RESULT_CAP: usize = 100;
/// Serial I/O budget for the whole scan.
const WORK_BUDGET: Duration = Duration::from_secs(5);

/// One discovered session to scan: the owning attached project, its session
/// id, the journal path, the recency stamp used for result ordering, and the
/// display context a result row needs without a second lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationSearchTarget {
    pub work_dir: PathBuf,
    pub session_id: String,
    pub session_file: PathBuf,
    /// Session title shown as the result row's headline.
    pub title: String,
    /// Recorded git branch, shown as the row's project/branch context.
    pub git_branch: Option<String>,
    pub updated_at: u64,
}

/// One result row: the session's first chronologically matching message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationSearchMatch {
    pub work_dir: PathBuf,
    pub session_id: String,
    pub title: String,
    pub git_branch: Option<String>,
    pub message_id: String,
    /// Plain-text, whitespace-normalized excerpt around the match.
    pub excerpt: String,
    /// The session's `updated_at`, carried so ordering stays stable.
    pub updated_at: u64,
}

/// How the scan finished relative to its inputs — the UI turns these into
/// the honest coverage states the feature requires.
#[derive(Clone, Debug, Default)]
pub struct ConversationSearchReport {
    /// One row per matching session, ordered by session recency with a
    /// stable identity tie-break.
    pub matches: Vec<ConversationSearchMatch>,
    /// Sessions fully scanned.
    pub scanned: usize,
    /// Sessions the scan attempted (the input target count).
    pub total: usize,
    /// Human-readable coverage limits, in encounter order; empty means the
    /// scan reached every discovered session without skipping or capping.
    pub partial: Vec<String>,
    /// A work or result budget fired — a narrower query could cover more.
    pub limited: bool,
}

/// Progress events emitted while [`search_conversations`] runs. `Scanned`
/// carries the count of sessions fully processed so far; `Done` is the
/// terminal event, sent even when the scan is cancelled or budget-limited,
/// so the receiver's loop always terminates.
pub enum ConversationSearchProgress {
    Scanned(usize),
    Done(ConversationSearchReport),
}

/// Serially scan `targets` for a case-insensitive literal `query` match in
/// saved user/assistant message content. Sessions whose transcript is
/// missing, unreadable, corrupt, or over the per-file budget never count as
/// zero matches — each contributes an explicit `partial` reason instead.
///
/// `cancelled` is polled between sessions (per-session work is bounded by
/// the file-size gate); progress events report the scanned-session count.
pub fn search_conversations(
    targets: Vec<ConversationSearchTarget>,
    query: &str,
    cancelled: &AtomicBool,
    progress: Option<tokio::sync::mpsc::UnboundedSender<ConversationSearchProgress>>,
) -> ConversationSearchReport {
    let deadline = Instant::now() + WORK_BUDGET;
    let total = targets.len();
    let mut report = ConversationSearchReport {
        total,
        ..ConversationSearchReport::default()
    };
    let mut read_bytes = 0u64;
    let mut unreadable = 0usize;
    let mut oversized = 0usize;
    'sessions: for target in targets {
        if cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline {
            if !cancelled.load(Ordering::Relaxed) {
                report.partial.push("5-second work budget reached".into());
                report.limited = true;
            }
            break 'sessions;
        }
        // `read_transcript_page` answers a missing file with an empty page;
        // an explicit existence check keeps "vanished" distinct from "empty".
        let Ok(metadata) = std::fs::symlink_metadata(&target.session_file) else {
            unreadable += 1;
            continue;
        };
        let file_bytes = metadata.len();
        if file_bytes > SESSION_FILE_BYTES {
            oversized += 1;
            continue;
        }
        if read_bytes.saturating_add(file_bytes) > TOTAL_BYTES {
            report.partial.push("256 MiB total-read budget reached".into());
            report.limited = true;
            break 'sessions;
        }
        match compute_session_messages(&target.session_file) {
            Ok(messages) => {
                read_bytes += file_bytes;
                report.scanned += 1;
                if let Some(progress) = &progress {
                    let _ = progress.send(ConversationSearchProgress::Scanned(report.scanned));
                }
                // First chronologically matching message only: one row per
                // session. History files are not generating, so no pending
                // queue or in-flight rows participate.
                if let Some(hit) = first_conversation_match(&messages, false, query) {
                    report.matches.push(ConversationSearchMatch {
                        work_dir: target.work_dir,
                        session_id: target.session_id,
                        title: target.title,
                        git_branch: target.git_branch,
                        message_id: hit.message_id,
                        excerpt: hit.excerpt,
                        updated_at: target.updated_at,
                    });
                    if report.matches.len() >= RESULT_CAP {
                        report
                            .partial
                            .push("100 conversation results reached".into());
                        report.limited = true;
                        break 'sessions;
                    }
                }
            }
            Err(_) => {
                read_bytes += file_bytes;
                unreadable += 1;
            }
        }
    }
    if unreadable > 0 {
        report.partial.push(format!(
            "{unreadable} saved conversation(s) could not be read"
        ));
    }
    if oversized > 0 {
        report.partial.push(format!(
            "{oversized} session(s) over 32 MiB skipped"
        ));
    }
    // Session recency first; a stable identity tie-break keeps equal
    // timestamps deterministic across scans.
    report.matches.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
            .then_with(|| a.work_dir.cmp(&b.work_dir))
    });
    if let Some(progress) = progress {
        let _ = progress.send(ConversationSearchProgress::Done(report.clone()));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use threadlane_protocol::AgentMessage;
    use threadlane_runtime::harness::Entry;

    fn write_session(path: &std::path::Path, messages: Vec<AgentMessage>) {
        let lines = messages
            .into_iter()
            .enumerate()
            .map(|(index, message)| {
                serde_json::to_string(&Entry {
                    id: format!("entry-{index}"),
                    parent_id: None,
                    lane: "main".into(),
                    seq: index as u64 + 1,
                    timestamp: index as u64 + 1,
                    message,
                    surface_op: Default::default(),
                    terminate: false,
                })
                .unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(path, format!("{lines}\n")).unwrap();
    }

    fn assistant(content: &str) -> AgentMessage {
        AgentMessage::Assistant {
            content: Some(content.into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        }
    }

    fn target(dir: &std::path::Path, session_id: &str, updated_at: u64) -> ConversationSearchTarget {
        ConversationSearchTarget {
            work_dir: dir.to_path_buf(),
            session_id: session_id.into(),
            session_file: dir.join(format!("{session_id}.jsonl")),
            title: session_id.into(),
            git_branch: None,
            updated_at,
        }
    }

    fn cancelled() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn search_excludes_pending_queue_inputs() {
        use threadlane_runtime::harness::{AgentHarness, JsonlStore, ProvisionedEntry, QueueKind};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        write_session(&path, vec![AgentMessage::user("saved history", Vec::new())]);
        let mut harness = AgentHarness::new(JsonlStore::open(&path).unwrap());
        for (id, queue) in [
            ("follow-up", QueueKind::FollowUp),
            ("next-run", QueueKind::NextRun),
        ] {
            harness
                .enqueue_unbound(
                    queue,
                    ProvisionedEntry::new(
                        id,
                        None,
                        AgentMessage::user("pending needle", Vec::new()),
                    ),
                )
                .unwrap();
            harness.drive_to_completion().unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let report = search_conversations(
            vec![target(dir.path(), "session", 1)],
            "needle",
            &cancelled(),
            None,
        );
        assert_eq!(report.scanned, 1);
        assert!(
            report.matches.is_empty(),
            "pending inputs are not saved conversation history"
        );
        let report = search_conversations(
            vec![target(dir.path(), "session", 1)],
            "saved history",
            &cancelled(),
            None,
        );
        assert_eq!(report.matches.len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn search_reports_one_row_per_matching_session_in_recency_order() {
        let dir = tempfile::tempdir().unwrap();
        write_session(
            &dir.path().join("old.jsonl"),
            vec![
                AgentMessage::user("find the flaky needle test", Vec::new()),
                assistant("Looking into it"),
            ],
        );
        write_session(
            &dir.path().join("new.jsonl"),
            vec![assistant("The needle moved to haystack-2")],
        );
        write_session(
            &dir.path().join("silent.jsonl"),
            vec![AgentMessage::user("unrelated", Vec::new())],
        );

        let report = search_conversations(
            vec![
                target(dir.path(), "old", 10),
                target(dir.path(), "new", 20),
                target(dir.path(), "silent", 30),
            ],
            "NEEDLE",
            &cancelled(),
            None,
        );

        assert_eq!(
            report
                .matches
                .iter()
                .map(|hit| hit.session_id.as_str())
                .collect::<Vec<_>>(),
            ["new", "old"]
        );
        assert_eq!(report.matches[0].excerpt, "The needle moved to haystack-2");
        assert_eq!(report.scanned, 3);
        assert_eq!(report.total, 3);
        assert!(report.partial.is_empty());
        assert!(!report.limited);
    }

    #[test]
    fn search_marks_missing_and_corrupt_sessions_incomplete_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("corrupt.jsonl"), b"!!! not a session line\n").unwrap();
        let missing = target(dir.path(), "missing", 1);
        let corrupt = target(dir.path(), "corrupt", 2);

        let report =
            search_conversations(vec![missing, corrupt], "needle", &cancelled(), None);

        assert!(report.matches.is_empty());
        assert_eq!(report.scanned, 0);
        assert!(report
            .partial
            .iter()
            .any(|reason| reason.contains("could not be read")));
        assert!(!report.limited);
    }

    #[test]
    fn search_skips_oversized_sessions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("huge.jsonl"),
            vec![b'x'; SESSION_FILE_BYTES as usize + 1],
        )
        .unwrap();
        write_session(
            &dir.path().join("small.jsonl"),
            vec![assistant("needle here")],
        );

        let report = search_conversations(
            vec![
                target(dir.path(), "huge", 2),
                target(dir.path(), "small", 1),
            ],
            "needle",
            &cancelled(),
            None,
        );

        assert_eq!(report.matches.len(), 1);
        assert_eq!(report.matches[0].session_id, "small");
        assert!(report
            .partial
            .iter()
            .any(|reason| reason.contains("32 MiB")));
    }

    #[test]
    fn search_honours_cancellation_between_sessions() {
        let dir = tempfile::tempdir().unwrap();
        write_session(
            &dir.path().join("a.jsonl"),
            vec![assistant("needle a")],
        );
        write_session(
            &dir.path().join("b.jsonl"),
            vec![assistant("needle b")],
        );
        let cancel_flag = AtomicBool::new(true);

        let report = search_conversations(
            vec![target(dir.path(), "a", 1), target(dir.path(), "b", 2)],
            "needle",
            &cancel_flag,
            None,
        );

        assert!(report.matches.is_empty());
        assert_eq!(report.scanned, 0);
    }

    #[test]
    fn search_ignores_tool_and_reasoning_content() {
        let dir = tempfile::tempdir().unwrap();
        write_session(
            &dir.path().join("tools.jsonl"),
            vec![
                assistant("visible answer"),
                AgentMessage::Custom {
                    custom_type: "thinking".into(),
                    payload: serde_json::json!({"text": "needle in reasoning"}),
                },
            ],
        );

        let report = search_conversations(
            vec![target(dir.path(), "tools", 1)],
            "needle in reasoning",
            &cancelled(),
            None,
        );

        assert!(report.matches.is_empty(), "tool/reasoning text must not match");
        assert_eq!(report.scanned, 1);
    }
}
