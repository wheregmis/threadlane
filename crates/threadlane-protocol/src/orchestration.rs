//! Orchestration-mode contract: Normal execution or Fusion routing.
//!
//! `OrchestratorMode` is shared configuration between UI surfaces (composer
//! mode dropdown, settings, subagent defaults), session wiring, and the
//! fusion router. It lives here — alongside `ReasoningEffort` — so the
//! engine, the `threadlane-orchestrator` router, and UI crates share one
//! definition.

use serde::{Deserialize, Serialize};

/// Session orchestration mode: exactly two modes.
///
/// `Normal` runs every prompt directly on the selected model. `Fusion`
/// (Devin-Fusion parity) is the persistent dual-agent mode: the frontier
/// main agent plans, disambiguates, and reviews while a cheaper sidekick
/// agent (the configured subagent/fast model) owns mechanical implementation
/// and verification in parallel child lanes with its own cached context.
/// Model switches happen at compaction boundaries so they ride the
/// unavoidable cache miss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorMode {
    /// Direct execution on the selected model. Explicit `/fusion` still arms
    /// Fusion for one task.
    ///
    /// Accepts the pre-simplification spellings (`off`, `auto`, `always`) so
    /// stored project settings from before the two-mode collapse keep
    /// loading; they all mean direct execution now that prewalk is gone.
    #[default]
    #[serde(alias = "off", alias = "auto", alias = "always")]
    Normal,
    /// Persistent main + sidekick routing with compaction-boundary model
    /// switches. Explicit `/fusion` also arms it for one task when the
    /// stored mode is `Normal`.
    Fusion,
}

impl OrchestratorMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Fusion => "Fusion",
        }
    }

    /// Whether this mode enables the persistent Fusion main + sidekick router.
    pub fn is_fusion(&self) -> bool {
        matches!(self, Self::Fusion)
    }
}
