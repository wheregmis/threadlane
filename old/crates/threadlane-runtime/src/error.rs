//! Typed errors for the agent crate.
//!
//! Replaces ad-hoc `Result<_, String>` with discriminated error variants
//! that support programmatic handling and structured diagnostics.

use thiserror::Error;

/// Errors originating from the agent loop, tool dispatch, or persistence.
#[derive(Debug, Error)]
pub enum AgentError {
    /// Tool registration failed (duplicate name, empty id, schema conflict).
    #[error("tool registration failed: {0}")]
    ToolRegistration(String),

    /// Provider communication error (streaming, payload building, routing).
    #[error("provider error: {0}")]
    Provider(String),

    /// Session tree or journal persistence error.
    #[error("session error: {0}")]
    Session(String),

    /// Error during context compaction.
    #[error("compaction error: {0}")]
    Compaction(String),

    /// Hook execution failure.
    #[error("hook error: {0}")]
    Hook(String),

    /// A catch-all for errors that don't fit the above categories.
    #[error("{0}")]
    Other(String),
}
