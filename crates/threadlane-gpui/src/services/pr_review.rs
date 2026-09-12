//! PR review feedback: compatibility re-exports.
//!
//! The review-feedback domain logic (feedback extraction, tracking store,
//! auto-address prompts) is canonical in `threadlane_git` and re-exported
//! here for compatibility. New code should import `threadlane_git` directly.

pub use threadlane_git::{
    build_auto_address_prompt, check_and_record_fresh_feedback, collect_actionable_pr_feedback,
    load_auto_address_pr_reviews_enabled, load_pr_review_tracking, mark_feedback_seen,
    save_auto_address_pr_reviews_enabled, save_pr_review_tracking, FeedbackSyncResult,
    PrReviewTrackingStore,
};
