//! Fusion mode: persistent frontier-main + cheap-sidekick routing.
//!
//! Devin-Fusion parity (see `OrchestratorMode::Fusion`): the frontier main
//! agent plans, disambiguates, and reviews while a cheaper sidekick agent
//! owns mechanical implementation and verification in parallel child lanes.
//! Both keep their own persistent cached contexts (main lane vs. subagent
//! child lanes); model switches on the main lane happen at compaction
//! boundaries so they ride the unavoidable cache miss.

use threadlane_protocol::ReasoningEffort;

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

/// Classify a prompt into mechanical / mixed / judgment.
///
/// Judgment markers win over mechanical ones: when the deliverable is intent
/// or taste (hard multi-file features, ambiguous scope, UX wording), delegating
/// the coding loses the subtle intent even when the work looks mechanical.
pub fn classify_fusion_task(prompt: &str) -> FusionComplexity {
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
    match (judgment, mechanical) {
        (true, false) => FusionComplexity::Judgment,
        (false, true) => FusionComplexity::Mechanical,
        (true, true) => FusionComplexity::Mixed,
        (false, false) => FusionComplexity::Mixed,
    }
}

/// Tools the sidekick lane is allowed to own. The main agent keeps plan
/// authorship (`update_plan`), user ambiguity (`ask_question`), and delivery
/// (`create_draft_pull_request`, `manage_subagent_branch`); everything else
/// mechanical (reads, edits, test runs) hands off. Unknown tools fail closed
/// to the main agent.
pub fn is_sidekick_eligible_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "read_file"
            | "grep_search"
            | "list_files"
            | "file_search"
            | "edit_file_hashline"
            | "edit_files_hashline"
            | "write_file"
            | "apply_workspace_edit_plan"
            | "run_command"
            | "context_snapshot"
            | "subagent_context"
    )
}

/// Frontier-only tools that must stay on the main agent even in Fusion mode.
pub fn is_frontier_only_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "update_plan" | "ask_question" | "create_draft_pull_request" | "manage_subagent_branch"
    )
}

/// Resolve which model the sidekick lanes run. Preference order mirrors the
/// existing session wiring: explicit subagent model, then the sidekick
/// (fast) model, then the active model (which makes Fusion a noop that
/// callers detect and report instead of a pointless switch).
pub fn resolve_sidekick_model(
    active_model: &str,
    fast_model: Option<&str>,
    subagent_model: Option<&str>,
) -> String {
    if let Some(model) = subagent_model.filter(|m| !m.trim().is_empty()) {
        return model.to_string();
    }
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
}

/// Initial routing for an incoming prompt under Fusion.
pub fn evaluate_fusion_prompt(prompt: &str, sidekick_model: &str) -> FusionDecision {
    if prompt.trim().is_empty() {
        return FusionDecision::KeepOnMain {
            reason: "empty prompt runs directly".into(),
        };
    }
    match classify_fusion_task(prompt) {
        FusionComplexity::Mechanical => FusionDecision::DelegateToSidekick {
            reason: format!(
                "mechanical work hands off cleanly to sidekick `{sidekick_model}`; main monitors and reviews"
            ),
        },
        FusionComplexity::Mixed => FusionDecision::DelegateToSidekick {
            reason: format!(
                "mixed work delegates to sidekick `{sidekick_model}` with supervision; escalate on ambiguity"
            ),
        },
        FusionComplexity::Judgment => FusionDecision::KeepOnMain {
            reason: "judgment is the deliverable; delegating loses subtle intent".into(),
        },
    }
}

/// Persistent per-session Fusion router state.
#[derive(Debug)]
pub struct FusionState {
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
    /// Whether this arming came from explicit `/fusion` (true) or stored
    /// `OrchestratorMode::Fusion` (false).
    pub explicit: bool,
    /// When the mode was armed (for observability logging).
    pub started_at: std::time::Instant,
}

impl FusionState {
    /// Create armed Fusion state. Callers must resolve the sidekick model
    /// first via [`resolve_sidekick_model`].
    pub fn new(
        main_model: String,
        sidekick_model: String,
        sidekick_effort: Option<ReasoningEffort>,
        explicit: bool,
    ) -> Self {
        Self {
            main_model,
            sidekick_model,
            sidekick_effort,
            delegated: 0,
            escalated: 0,
            consecutive_sidekick_errors: 0,
            explicit,
            started_at: std::time::Instant::now(),
        }
    }

    /// Record a delegation to the sidekick lane.
    pub fn record_delegation(&mut self) {
        self.delegated = self.delegated.saturating_add(1);
    }

    /// Record an escalation back to the main agent (clears the error streak).
    pub fn record_escalation(&mut self) {
        self.escalated = self.escalated.saturating_add(1);
        self.consecutive_sidekick_errors = 0;
    }

    /// Record one sidekick tool result. Errors accumulate toward the
    /// escalation threshold; success resets the streak.
    pub fn record_sidekick_result(&mut self, is_error: bool) {
        if is_error {
            self.consecutive_sidekick_errors =
                self.consecutive_sidekick_errors.saturating_add(1);
        } else {
            self.consecutive_sidekick_errors = 0;
        }
    }

    /// Whether the sidekick error streak forces escalation to main.
    pub fn escalation_needed(&self) -> bool {
        self.consecutive_sidekick_errors >= 2
    }
}

/// Model switch opportunity evaluated at compaction boundaries, where a cache
/// miss happens anyway so the switch is effectively free.
///
/// - Repeated sidekick failures upgrade the main lane back to the frontier
///   model so judgment recovers the run.
/// - Long clean mechanical stretches downgrade the main lane to the sidekick
///   model to save cost while implementation continues.
/// - Otherwise no switch: churn without signal just burns cache.
pub fn select_model_at_compaction(
    active_model: &str,
    state: &FusionState,
) -> Option<String> {
    if state.escalation_needed() && active_model != state.main_model {
        return Some(state.main_model.clone());
    }
    let clean_mechanical = state.delegated > state.escalated.saturating_mul(2).max(2)
        && state.consecutive_sidekick_errors == 0;
    if clean_mechanical
        && active_model == state.main_model
        && !fusion_would_be_noop(&state.main_model, &state.sidekick_model)
    {
        return Some(state.sidekick_model.clone());
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
         By default DELEGATE and MONITOR: hand mechanical implementation and slow verification (edits, test suites, bulk refactors, deprecation removals) to the sidekick `{sidekick_model}` via `subagent`, then review the diff.\n\
         Own the significant decisions yourself: the plan (`update_plan`), interpretation of ambiguity (`ask_question` — never let the sidekick guess intent), and the final review before delivery.\n\
         Keep your own context lean so both lanes stay cache-friendly; let the sidekick gather its own context in its lane.\n\
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
        let decision =
            evaluate_fusion_prompt("Modernize search.js to ES6 and verify with the full make suite", "flash");
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
    fn mixed_prompts_delegate_with_supervision() {
        let decision = evaluate_fusion_prompt(
            "Integrate the WebSocket MCP transport into Quarkus, reusing upstream",
            "flash",
        );
        assert!(decision.delegates());
    }

    #[test]
    fn empty_prompt_stays_on_main() {
        assert!(matches!(
            evaluate_fusion_prompt("   ", "flash"),
            FusionDecision::KeepOnMain { .. }
        ));
    }

    #[test]
    fn sidekick_tool_gates_match_contract() {
        assert!(is_sidekick_eligible_tool("edit_file_hashline"));
        assert!(is_sidekick_eligible_tool("run_command"));
        assert!(!is_sidekick_eligible_tool("update_plan"));
        assert!(!is_sidekick_eligible_tool("ask_question"));
        assert!(is_frontier_only_tool("update_plan"));
        assert!(!is_frontier_only_tool("read_file"));
        // Unknown tools fail closed to main.
        assert!(!is_sidekick_eligible_tool("computer_act"));
    }

    #[test]
    fn sidekick_resolution_prefers_subagent_then_fast() {
        assert_eq!(
            resolve_sidekick_model("main", Some("fast"), Some("child")),
            "child"
        );
        assert_eq!(resolve_sidekick_model("main", Some("fast"), None), "fast");
        assert_eq!(resolve_sidekick_model("main", None, None), "main");
        assert!(fusion_would_be_noop("m", "m"));
        assert!(!fusion_would_be_noop("main", "sidekick"));
    }

    #[test]
    fn error_streak_drives_escalation_and_compaction_upgrade() {
        let mut state = FusionState::new("main".into(), "side".into(), None, true);
        assert!(!state.escalation_needed());
        state.record_sidekick_result(true);
        assert!(!state.escalation_needed());
        state.record_sidekick_result(true);
        assert!(state.escalation_needed());
        // Active on sidekick with a failing streak upgrades to main for free.
        assert_eq!(
            select_model_at_compaction("side", &state),
            Some("main".into())
        );
        state.record_escalation();
        assert!(!state.escalation_needed());
    }

    #[test]
    fn clean_mechanical_stretch_downgrades_at_compaction() {
        let mut state = FusionState::new("main".into(), "side".into(), None, false);
        for _ in 0..4 {
            state.record_delegation();
            state.record_sidekick_result(false);
        }
        assert_eq!(
            select_model_at_compaction("main", &state),
            Some("side".into())
        );
        // No churn when already on the cheap model or when sidekick is a noop.
        assert_eq!(select_model_at_compaction("side", &state), None);
        let noop = FusionState::new("same".into(), "same".into(), None, false);
        assert_eq!(select_model_at_compaction("same", &noop), None);
    }

    #[test]
    fn directives_name_sidekick_and_ownership() {
        let main = build_fusion_main_directive("flash");
        assert!(main.contains(FUSION_MAIN_HEADER));
        assert!(main.contains("flash"));
        assert!(main.contains("MINIMAL"));
        assert!(main.contains("update_plan"));
        let side = build_fusion_sidekick_directive();
        assert!(side.contains(FUSION_SIDEKICK_HEADER));
    }
}
