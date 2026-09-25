//! Fusion routing (Devin-Fusion parity).
//!
//! The fusion router and its prompt directives sit next to the
//! `OrchestratorMode` turn-driving config they interpret (both contract
//! types live in `threadlane-protocol`). Import `threadlane_orchestrator`
//! directly.
//!
//! Only two session modes exist: `Normal` (direct execution) and `Fusion`.
//! Fusion is persistent, not one-shot: the frontier main agent plans,
//! disambiguates, and reviews while a cheaper sidekick agent owns mechanical
//! implementation and verification in parallel child lanes. Both keep their
//! own persistent cached contexts (main lane vs. subagent child lanes);
//! model switches on the main lane happen at compaction boundaries so they
//! ride the unavoidable cache miss.
//!
//! Routing is keyword-heuristic, never an LLM classifier: classification must
//! add no latency, cost, or nondeterminism to the hot path.

pub mod fusion;
pub use fusion::{
    FUSION_MAIN_FOOTER, FUSION_MAIN_HEADER, FUSION_SIDEKICK_HEADER, FusionComplexity, FusionDecision,
    FusionState, FUSION_ESCALATION_THRESHOLD, build_fusion_main_directive,
    build_fusion_sidekick_directive, classify_fusion_task, evaluate_fusion_prompt,
    fusion_would_be_noop, resolve_sidekick_model, select_model_at_compaction,
};
