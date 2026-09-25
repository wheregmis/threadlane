//! Fusion mode: persistent frontier-main + cheap-sidekick routing.
//!
//! Devin-Fusion parity (see `OrchestratorMode::Fusion`): the frontier main
//! agent plans, disambiguates, and reviews while a cheaper sidekick agent
//! owns mechanical implementation and verification in parallel child lanes.
//! Main and child lanes have durable histories. Provider-side prompt cache
//! behavior is separate and must be confirmed by reported usage.

use serde::{Deserialize, Serialize};
use threadlane_protocol::ReasoningEffort;

pub const FUSION_STATE_VERSION: u32 = 2;
pub const FUSION_CLASSIFIER_VERSION: u32 = 1;

/// Marker for the hidden Fusion main-agent directive injected into the
/// system prompt while Fusion is armed.
pub const FUSION_MAIN_HEADER: &str = "[FUSION PROTOCOL: Frontier Main + Sidekick]";
/// Footer closing the Fusion main directive block.
pub const FUSION_MAIN_FOOTER: &str = "[END FUSION PROTOCOL]";
/// Marker for the sidekick lane directive handed to delegated children.
pub const FUSION_SIDEKICK_HEADER: &str = "[FUSION SIDEKICK: Mechanical Implementation]";

/// Cheap heuristic task complexity used for initial routing. This is
/// deliberately keyword-based, not an LLM classifier: classification must add
/// no latency, cost, or nondeterminism to the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusionComplexity {
    /// Mechanical work (rewrites, removals, test runs) that hands off cleanly.
    Mechanical,
    /// Mixed work: delegate with supervision, escalate on ambiguity.
    Mixed,
    /// Judgment is the deliverable: keep on the frontier main agent.
    Judgment,
}

/// Deterministic baseline classifier result. `abstain` keeps uncertain work
/// with the main agent; no external classifier is required for offline use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FusionClassification {
    pub version: u32,
    pub complexity: FusionComplexity,
    pub confidence_percent: u8,
    pub reason_codes: Vec<&'static str>,
    pub abstain: bool,
}

/// Classify a prompt into mechanical / mixed / judgment.
///
/// Judgment markers win over mechanical ones: when the deliverable is intent
/// or taste (hard multi-file features, ambiguous scope, UX wording), delegating
/// the coding loses the subtle intent even when the work looks mechanical.
pub fn classify_fusion_task(prompt: &str) -> FusionComplexity {
    classify_fusion_prompt(prompt).complexity
}

pub fn classify_fusion_prompt(prompt: &str) -> FusionClassification {
    let lower = prompt.to_lowercase();
    let has_any = |markers: &[&str]| markers.iter().any(|m| lower.contains(m));
    // Judgment signals: ambiguity, taste, cross-cutting intent.
    let judgment = has_any(&[
        "judg",
        "ambigu",
        "which approach",
        "trade-off",
        "tradeoff",
        "taste",
        "ux wording",
        "copy writing",
        "product intent",
        "decide between",
        "cross-team",
        "hard",
        "ambiguous",
        "subtle intent",
        "redux",
        "selector",
        "gated on",
        "design",
        "feature",
        "user experience",
        "product behavior",
    ]);
    // Mechanical signals: bulk, verification-heavy, or deprecation work.
    let mechanical = has_any(&[
        "mechanical",
        "bulk",
        "rename",
        "removal",
        "remove",
        "rip out",
        "cleanly",
        "deprecat",
        "modernize",
        "migrate",
        "test suite",
        "playwright",
        "e2e",
        "verify with",
        "run the tests",
        "full test",
        "make ",
        "repeating",
        "across many files",
        "across the server",
        "integration",
        "reuse what's upstream",
        "reuse upstream",
    ]);
    let (complexity, confidence_percent, reason_codes, abstain) = match (judgment, mechanical) {
        (true, false) => (FusionComplexity::Judgment, 90, vec!["judgment_marker"], false),
        (false, true) => (FusionComplexity::Mechanical, 80, vec!["mechanical_marker"], false),
        (true, true) => (FusionComplexity::Mixed, 40, vec!["conflicting_markers"], true),
        (false, false) => (FusionComplexity::Mixed, 0, vec!["no_markers"], true),
    };
    FusionClassification {
        version: FUSION_CLASSIFIER_VERSION,
        complexity,
        confidence_percent,
        reason_codes,
        abstain,
    }
}

/// Resolve the Fusion child model from its single configured choice.
pub fn resolve_sidekick_model(active_model: &str, fast_model: Option<&str>) -> String {
    if let Some(model) = fast_model.filter(|m| !m.trim().is_empty()) {
        return model.to_string();
    }
    active_model.to_string()
}

/// Returns true when the resolved sidekick is a real handoff target.
pub fn fusion_would_be_noop(active_model: &str, sidekick_model: &str) -> bool {
    active_model == sidekick_model
}

/// Routing decision for an incoming Fusion prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FusionDecision {
    /// Delegate implementation/verification to the sidekick lane now; the
    /// main agent plans, monitors, and reviews.
    DelegateToSidekick { reason: String },
    /// Keep the work on the frontier main agent.
    KeepOnMain { reason: String },
}

impl FusionDecision {
    /// Whether the prompt should start work on a sidekick child lane.
    pub fn delegates(&self) -> bool {
        matches!(self, Self::DelegateToSidekick { .. })
    }

    /// Model-visible triage note appended to the Fusion main directive for
    /// this prompt. This is what makes the keyword router behavioral rather
    /// than advisory: the main agent reads the initial route in its own
    /// context and dispatches the turn accordingly.
    pub fn directive_suffix(&self) -> String {
        match self {
            Self::DelegateToSidekick { reason } => format!(
                "\nInitial triage for this task: DELEGATE ({reason}). Open with the plan, hand implementation and verification to the sidekick, then review."
            ),
            Self::KeepOnMain { reason } => format!(
                "\nInitial triage for this task: KEEP ON MAIN ({reason}). Do the implementation yourself; use the sidekick only for isolated mechanical subtasks."
            ),
        }
    }
}

/// Initial routing for an incoming prompt under Fusion.
pub fn evaluate_fusion_prompt(prompt: &str, sidekick_model: &str) -> FusionDecision {
    if prompt.trim().is_empty() {
        return FusionDecision::KeepOnMain {
            reason: "empty prompt runs directly".into(),
        };
    }
    let classification = classify_fusion_prompt(prompt);
    match classification.complexity {
        FusionComplexity::Mechanical => FusionDecision::DelegateToSidekick {
            reason: format!(
                "mechanical work hands off cleanly to sidekick `{sidekick_model}`; main monitors and reviews"
            ),
        },
        FusionComplexity::Mixed => FusionDecision::KeepOnMain {
            reason: "classifier abstained on mixed or unclear intent; main keeps ownership".into(),
        },
        FusionComplexity::Judgment => FusionDecision::KeepOnMain {
            reason: "judgment is the deliverable; delegating loses subtle intent".into(),
        },
    }
}

/// Consecutive sidekick failures that force escalation to main. The streak
/// must survive lane commits until the compaction router consumes it with
/// `record_escalation`; clearing it earlier makes the upgrade branch dead.
pub const FUSION_ESCALATION_THRESHOLD: u32 = 2;

/// Persistent per-session Fusion router state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusionState {
    pub version: u32,
    pub classifier_version: u32,
    pub compaction_generation: u64,
    /// Frontier model owning the main lane at arm time.
    pub main_model: String,
    /// Cheap model owning sidekick child lanes.
    pub sidekick_model: String,
    /// Reasoning effort for sidekick lanes, if pinned.
    pub sidekick_effort: Option<ReasoningEffort>,
    /// Successful sidekick delegations this session.
    pub delegated: u64,
    /// Escalations back to the main agent.
    pub escalated: u64,
    /// Consecutive sidekick tool errors (resets on success).
    pub consecutive_sidekick_errors: u32,
    /// Structured child signal awaiting a safe main-lane boundary.
    #[serde(default)]
    pub pending_escalation: Option<String>,
}

impl FusionState {
    /// Create armed Fusion state. Callers must resolve the sidekick model
    /// first via [`resolve_sidekick_model`].
    pub fn new(
        main_model: String,
        sidekick_model: String,
        sidekick_effort: Option<ReasoningEffort>,
    ) -> Self {
        Self {
            version: FUSION_STATE_VERSION,
            classifier_version: FUSION_CLASSIFIER_VERSION,
            compaction_generation: 0,
            main_model,
            sidekick_model,
            sidekick_effort,
            delegated: 0,
            escalated: 0,
            consecutive_sidekick_errors: 0,
            pending_escalation: None,
        }
    }

    /// Record a delegation to the sidekick lane.
    pub fn record_delegation(&mut self) {
        self.delegated = self.delegated.saturating_add(1);
    }

    /// Record a completed escalation after the main agent takes ownership
    /// at a compaction boundary. Never clear the streak per failed lane.
    pub fn record_escalation(&mut self) {
        self.escalated = self.escalated.saturating_add(1);
        self.consecutive_sidekick_errors = 0;
        self.pending_escalation = None;
    }

    /// Record one sidekick tool result. Errors accumulate toward the
    /// escalation threshold; success resets the streak.
    pub fn record_sidekick_result(&mut self, is_error: bool) {
        if is_error {
            self.consecutive_sidekick_errors = self.consecutive_sidekick_errors.saturating_add(1);
        } else {
            self.consecutive_sidekick_errors = 0;
        }
    }

    /// Whether the sidekick error streak forces escalation to main.
    pub fn escalation_needed(&self) -> bool {
        self.consecutive_sidekick_errors >= FUSION_ESCALATION_THRESHOLD
            || self.pending_escalation.is_some()
    }

    pub fn request_escalation(&mut self, reason_code: &str) {
        self.pending_escalation = Some(reason_code.to_owned());
    }

    /// A persisted router is reusable only with the same configured model
    /// roles. Invalid or old snapshots fall back to a fresh router.
    pub fn compatible_with(
        &self,
        active_model: &str,
        configured_sidekick: &str,
        configured_effort: Option<ReasoningEffort>,
    ) -> bool {
        self.version == FUSION_STATE_VERSION
            && self.classifier_version == FUSION_CLASSIFIER_VERSION
            && !self.main_model.trim().is_empty()
            && self.sidekick_model == configured_sidekick
            && self.sidekick_effort == configured_effort
            && (active_model == self.main_model || active_model == self.sidekick_model)
    }
}

/// Keep the main lane on its selected model. Previously downgraded sessions
/// return to that model at the next compaction boundary.
pub fn select_model_at_compaction(active_model: &str, state: &FusionState) -> Option<String> {
    if active_model != state.main_model {
        return Some(state.main_model.clone());
    }
    None
}

/// Frontier-main system directive: take minimal actions, only read what is
/// absolutely necessary, delegate implementation/verification to the sidekick
/// via `subagent`, and own the plan, ambiguity, and final review.
pub fn build_fusion_main_directive(sidekick_model: &str) -> String {
    format!(
        "\n\n{FUSION_MAIN_HEADER}\n\
         Sidekick model: {sidekick_model}\n\
         You are the frontier main agent. Take MINIMAL direct actions and only read what is absolutely necessary.\n\
         For delegated tasks, first ask the sidekick to explore the code and return only relevant file snippets. Use those findings to make the plan. Then hand mechanical implementation and verification (edits, tests, lint) to the sidekick `{sidekick_model}` via `subagent` with `wait=false` when work can proceed independently; supervise it with `hub read` and `hub wait`.\n\
         Review the resulting diff yourself. If it needs substantial edits, send precise feedback with `hub revive` on the same lane, then review again.\n\
         Make tiny corrections found during review yourself; do not launch a fresh child for a one-line fix.\n\
         Own the significant decisions yourself: the plan (`update_plan`), interpretation of ambiguity (`ask_question` — never let the sidekick guess intent), and the final review before delivery.\n\
         Keep your own context lean; let the sidekick gather its own context in its lane.\n\
         If sidekick work errors twice in a row or the subtle intent is at risk, escalate back to yourself and finish directly.\n\
         {FUSION_MAIN_FOOTER}"
    )
}

/// Sidekick lane directive: mechanical implementation with verification,
/// escalating ambiguity instead of guessing.
pub fn build_fusion_sidekick_directive() -> String {
    format!(
        "\n\n{FUSION_SIDEKICK_HEADER}\n\
         You are the cost-effective sidekick agent. Own mechanical implementation and verification: read, edit, run tests, and report back.\n\
         Do not re-plan the task or guess at ambiguous intent — surface questions through your lane output so the frontier main agent decides.\n\
         Verify every change with the relevant checks before reporting; keep the summary concrete (files changed, commands run, outputs)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mechanical_prompts_delegate() {
        let decision = evaluate_fusion_prompt(
            "Modernize search.js to ES6 and verify with the full make suite",
            "flash",
        );
        assert!(decision.delegates());
        assert_eq!(
            classify_fusion_task("Rip out the OpenTracing integration across the server, cleanly"),
            FusionComplexity::Mechanical
        );
    }

    #[test]
    fn judgment_prompts_stay_on_main() {
        let decision = evaluate_fusion_prompt(
            "Add a team selector to the search bar (cross-team search), gated on a flag",
            "flash",
        );
        assert!(matches!(decision, FusionDecision::KeepOnMain { .. }));
    }

    #[test]
    fn mixed_prompts_abstain_to_main() {
        let decision = evaluate_fusion_prompt(
            "Design a WebSocket feature in Quarkus and reuse upstream code",
            "flash",
        );
        assert!(!decision.delegates());
        let classification = classify_fusion_prompt(
            "Design a WebSocket feature in Quarkus and reuse upstream code",
        );
        assert!(classification.abstain);
        assert_eq!(classification.version, FUSION_CLASSIFIER_VERSION);
    }

    #[test]
    fn triage_suffix_is_behavioral_not_advisory() {
        let delegate = evaluate_fusion_prompt(
            "Remove the deprecated auth module and run the full suite",
            "flash",
        );
        let suffix = delegate.directive_suffix();
        assert!(suffix.contains("DELEGATE"));
        assert!(suffix.contains("flash"));
        let keep = evaluate_fusion_prompt(
            "Add a team selector to the search bar (cross-team search), gated on a flag",
            "flash",
        );
        let suffix = keep.directive_suffix();
        assert!(suffix.contains("KEEP ON MAIN"));
    }

    #[test]
    fn empty_prompt_stays_on_main() {
        assert!(matches!(
            evaluate_fusion_prompt("   ", "flash"),
            FusionDecision::KeepOnMain { .. }
        ));
    }

    #[test]
    fn sidekick_resolution_uses_fusion_model() {
        assert_eq!(resolve_sidekick_model("main", Some("luna")), "luna");
        assert_eq!(resolve_sidekick_model("main", None), "main");
        assert!(fusion_would_be_noop("m", "m"));
        assert!(!fusion_would_be_noop("main", "sidekick"));
    }

    #[test]
    fn error_streak_drives_escalation_and_compaction_upgrade() {
        let mut state = FusionState::new("main".into(), "side".into(), None);
        assert!(!state.escalation_needed());
        state.record_sidekick_result(true);
        assert!(!state.escalation_needed());
        state.record_sidekick_result(true);
        assert!(state.escalation_needed());
        // A legacy session running main on the sidekick returns to main.
        assert_eq!(
            select_model_at_compaction("side", &state),
            Some("main".into())
        );
        state.record_escalation();
        assert!(!state.escalation_needed());
    }

    #[test]
    fn clean_mechanical_stretch_keeps_main_on_selected_model() {
        let mut state = FusionState::new("main".into(), "side".into(), None);
        for _ in 0..4 {
            state.record_delegation();
            state.record_sidekick_result(false);
        }
        assert_eq!(select_model_at_compaction("main", &state), None);
        assert_eq!(select_model_at_compaction("side", &state), Some("main".into()));
        let noop = FusionState::new("same".into(), "same".into(), None);
        assert_eq!(select_model_at_compaction("same", &noop), None);
    }

    #[test]
    fn directives_name_sidekick_and_ownership() {
        let main = build_fusion_main_directive("flash");
        assert!(main.contains(FUSION_MAIN_HEADER));
        assert!(main.contains("flash"));
        assert!(main.contains("MINIMAL"));
        assert!(main.contains("update_plan"));
        assert!(main.contains("wait=false"));
        assert!(main.contains("hub revive"));
        let side = build_fusion_sidekick_directive();
        assert!(side.contains(FUSION_SIDEKICK_HEADER));
    }

    #[test]
    fn restored_state_rejects_stale_model_roles_and_versions() {
        let mut state = FusionState::new("main".into(), "side".into(), None);
        state.record_delegation();
        state.record_sidekick_result(true);
        let restored: FusionState = serde_json::from_str(&serde_json::to_string(&state).unwrap())
            .unwrap();
        assert!(restored.compatible_with("main", "side", None));
        assert!(restored.compatible_with("side", "side", None));
        assert_eq!(restored.delegated, 1);
        assert_eq!(restored.consecutive_sidekick_errors, 1);
        assert!(!restored.compatible_with("unknown", "side", None));
        assert!(!restored.compatible_with("main", "changed", None));
        state.version += 1;
        assert!(!state.compatible_with("main", "side", None));
    }

    #[test]
    fn feature_wording_prevents_false_mechanical_handoff() {
        let prompt = "Remove the old search selector and redesign the cross-team feature";
        let result = classify_fusion_prompt(prompt);
        assert!(result.abstain);
        assert_eq!(result.reason_codes, vec!["conflicting_markers"]);
        assert!(!evaluate_fusion_prompt(prompt, "side").delegates());
    }

    #[test]
    fn quality_signal_escalates_at_compaction_without_two_failures() {
        let mut state = FusionState::new("main".into(), "side".into(), None);
        state.request_escalation("unresolved_question");
        assert!(state.escalation_needed());
        assert_eq!(select_model_at_compaction("side", &state), Some("main".into()));
        state.record_escalation();
        assert_eq!(state.pending_escalation, None);
    }
}
