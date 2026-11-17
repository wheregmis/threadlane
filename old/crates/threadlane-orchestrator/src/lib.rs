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
//! implementation and verification in durable child lanes. Provider cache
//! reuse is observed only through provider-reported usage, not inferred from
//! the lane journal. The main lane stays on its selected model.
//!
//! Routing uses a versioned, deterministic keyword baseline and abstains on
//! unclear intent; it adds no provider call to the hot path.

pub mod fusion;
pub use fusion::{
    FUSION_CLASSIFIER_VERSION, FUSION_ESCALATION_THRESHOLD, FUSION_MAIN_FOOTER,
    FUSION_MAIN_HEADER, FUSION_SIDEKICK_HEADER, FUSION_STATE_VERSION, FusionClassification,
    FusionComplexity, FusionDecision, FusionState, build_fusion_main_directive,
    build_fusion_sidekick_directive, classify_fusion_prompt, classify_fusion_task, evaluate_fusion_prompt,
    fusion_would_be_noop, resolve_sidekick_model, select_model_at_compaction,
};
