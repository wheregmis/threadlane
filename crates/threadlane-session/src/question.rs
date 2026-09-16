//! Model-initiated clarifying questions: compatibility re-exports.
//!
//! The question manager, handle, and `ask_question` tool executor are
//! canonical in `threadlane_question` (re-exported through
//! `threadlane_runtime::question` for compatibility) and re-exported below
//! for compatibility. New code should import from `threadlane_question`
//! (manager/executor) or `threadlane_protocol::interaction` (wire types)
//! directly.

pub use threadlane_question::{
    AskQuestionToolExecutor, QuestionHandle, QuestionManager, ASK_QUESTION_TOOL_NAME,
};
