//! Orchestration-mode contract for explicit `/prewalk` engagement.
//!
//! `OrchestratorMode` is shared configuration between UI surfaces (settings,
//! subagent defaults), session wiring, and the prewalk state machine. It
//! lives here — alongside `ReasoningEffort` — so the engine, the
//! `threadlane-orchestrator` state machine, and UI crates share one
//! definition; `threadlane-runtime` re-exports it for backward
//! compatibility.

use serde::{Deserialize, Serialize};

/// Orchestration mode governing explicit /prewalk engagement and Fusion.
///
/// Prewalk is off by default (oh-my-pi parity): it is a one-shot handoff from
/// the active model to a faster/cheaper model after planning reaches
/// implementation. It is armed explicitly via `/prewalk` or `Always` mode;
/// there is no LLM intent classifier.
///
/// Fusion (Devin-Fusion parity) is the persistent dual-agent mode: the
/// frontier main agent plans, disambiguates, and reviews while a cheaper
/// sidekick agent (the configured fast/subagent model) owns mechanical
/// implementation and verification in parallel child lanes with its own
/// cached context. Model switches happen at compaction boundaries so they
/// ride the unavoidable cache miss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorMode {
    /// Deprecated: previously ran an LLM intent classifier. Now behaves as
    /// `Off` (direct execution) to preserve deserialization of old configs
    /// without paying classifier latency/cost.
    #[serde(alias = "auto")]
    Auto,
    /// Arm prewalk on all incoming prompts.
    Always,
    /// Fusion mode: persistent main + sidekick routing with
    /// compaction-boundary model switches. Explicit `/fusion` also arms it
    /// for one task when the stored mode is `Off`.
    Fusion,
    /// Direct execution only (explicit /prewalk or /fusion command required).
    #[default]
    Off,
}

impl OrchestratorMode {
    pub fn label(&self) -> &'static str {
        match self {
            // Auto is retained only for backward compat; it no longer engages.
            Self::Auto => "Off (Manual /prewalk)",
            Self::Always => "Always",
            Self::Fusion => "Fusion (Main + Sidekick)",
            Self::Off => "Off (Manual /prewalk)",
        }
    }

    /// Whether this mode arms prewalk automatically. `Auto` is intentionally
    /// inert (see variant docs). Fusion is handled by the fusion router, not
    /// the one-shot prewalk path.
    pub fn arms_automatically(&self) -> bool {
        matches!(self, Self::Always)
    }

    /// Whether this mode enables the persistent Fusion main + sidekick router.
    pub fn is_fusion(&self) -> bool {
        matches!(self, Self::Fusion)
    }
}
