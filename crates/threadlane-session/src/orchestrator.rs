//! One-shot Prewalk handoff (oh-my-pi parity).
//!
//! Prewalk is off by default. When armed (explicit `/prewalk` or
//! `OrchestratorMode::Always`), the starting model inspects the repository,
//! writes a complete execution plan, captures it with the `update_plan` todo
//! tool, and begins the change. After the first workspace-mutating edit/write
//! lands behind an opened todo gate, the session switches one-shot to the
//! fast/cheap target model, which verifies consistency, scope, and tests
//! before finishing.
//!
//! Differences from the previous Threadlane implementation, all deliberate:
//! - No LLM intent classifier (`Auto` mode removed). Classification added
//!   latency, cost, and nondeterminism; oh-my-pi arms explicitly.
//! - No `complete_prewalk` tool. The handoff is automatic at the first
//!   qualifying edit/write, so a model that never calls the tool can no
//!   longer strand the session on the frontier model.
//! - Todo-gated: when `update_plan` is available, a successful call (even
//!   read-only `view`) must open the gate before edits count. Edits before
//!   the gate do not trigger.
//! - Turn-boundary semantics with hidden plan/continue/checklist prompts,
//!   scrubbed after the handoff, plus a bounded continuation safety net so a
//!   text-only reply to the plan nudge cannot silently end the run.
//! - One-shot with noop detection: identical target model + effort disarms
//!   with a notice instead of a pointless switch.

use threadlane_runtime::{OrchestratorMode, ReasoningEffort};

#[derive(Debug)]
pub(crate) struct PrewalkState {
    pub(crate) target_model: String,
    pub(crate) target_reasoning: Option<ReasoningEffort>,
    pub(crate) started_at: std::time::Instant,
    /// A successful `update_plan` call opened the handoff gate. Includes
    /// read-only `view`, matching oh-my-pi's `todo` gate.
    pub(crate) todo_seen: bool,
    /// Whether `update_plan` is in the active toolset. When false the gate
    /// is considered open from the start.
    pub(crate) requires_todo: bool,
    /// Continuation safety net armed (fires at most once).
    pub(crate) continue_pending: bool,
}

impl PrewalkState {
    pub(crate) fn new(
        target_model: String,
        target_reasoning: Option<ReasoningEffort>,
        requires_todo: bool,
    ) -> Self {
        Self {
            target_model,
            target_reasoning,
            started_at: std::time::Instant::now(),
            todo_seen: false,
            requires_todo,
            continue_pending: true,
        }
    }

    pub(crate) fn todo_gate_open(&self) -> bool {
        self.todo_seen || !self.requires_todo
    }
}

pub(crate) const ARCHITECT_PROTOCOL_HEADER: &str =
    "[ARCHITECT PROTOCOL: Frontier Architect -> Fast Model Handoff]";
const ARCHITECT_PROTOCOL_FOOTER: &str = "[END ARCHITECT PROTOCOL]";

/// Marker for the hidden post-handoff verification checklist appended after
/// the plan nudge is scrubbed.
pub(crate) const PREWALK_CHECKLIST_HEADER: &str = "[PREWALK CHECKLIST: Fast Model Verification]";

/// Tool that opens the todo gate (oh-my-pi `todo` === Threadlane `update_plan`).
pub(crate) const PREWALK_TODO_TOOL: &str = "update_plan";

/// First workspace-mutating actions that trigger the handoff once the todo
/// gate is open. Read-only tools (`read_file`, `grep_search`, `run_command`
/// diagnostics, etc.) never trigger, matching oh-my-pi where only
/// `edit`/`write` count.
pub(crate) fn is_prewalk_implementation_action(tool_name: &str, is_error: bool) -> bool {
    if is_error {
        return false;
    }
    matches!(
        tool_name,
        "edit_file_hashline" | "edit_files_hashline" | "write_file" | "apply_workspace_edit_plan"
    )
}

/// Successful todo calls open the gate, including read-only views.
pub(crate) fn is_prewalk_todo_gate_opener(tool_name: &str, is_error: bool) -> bool {
    !is_error && tool_name == PREWALK_TODO_TOOL
}

pub(crate) fn prewalk_would_be_noop(
    active_model: &str,
    active_effort: Option<ReasoningEffort>,
    target_model: &str,
    target_effort: Option<ReasoningEffort>,
) -> bool {
    active_model == target_model && active_effort == target_effort
}

/// Orchestrator decision for an incoming prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrchestratorDecision {
    /// Execute directly with active model without Prewalk.
    DirectExecution,
    /// Engage Prewalk: frontier architect plans + lands first edit via system
    /// directive, then hands off to fast model automatically.
    EngagePrewalk {
        fast_model: String,
        fast_reasoning: Option<ReasoningEffort>,
        architect_system_directive: String,
    },
}

pub(crate) struct Orchestrator;

impl Orchestrator {
    pub(crate) fn evaluate(
        prompt: &str,
        mode: OrchestratorMode,
        active_model: &str,
        fast_model: &str,
        fast_reasoning: Option<ReasoningEffort>,
        requires_todo: bool,
    ) -> OrchestratorDecision {
        // Noop guard: same target is not a handoff.
        if active_model == fast_model {
            return OrchestratorDecision::DirectExecution;
        }
        // `Auto` is deprecated and inert; only `Always` arms automatically.
        // Explicit `/prewalk` bypasses this function entirely.
        if !mode.arms_automatically() {
            return OrchestratorDecision::DirectExecution;
        }
        if prompt.trim().is_empty() {
            return OrchestratorDecision::DirectExecution;
        }
        let architect_system_directive = build_architect_directive(fast_model, requires_todo);
        OrchestratorDecision::EngagePrewalk {
            fast_model: fast_model.to_string(),
            fast_reasoning,
            architect_system_directive,
        }
    }
}

/// Rich frontier-architect directive: inspect, write a complete execution
/// plan, capture it with `update_plan` (5-9 meaningful steps), then land the
/// first workspace edit. Adapted from oh-my-pi's `prewalk-plan.md`.
///
/// `requires_todo` must mirror the session's `PrewalkState`: when the todo
/// tool is absent from the active toolset (core schema mode), the gate is
/// open from the start, so the directive must not demand an unfulfillable
/// `update_plan` call — the written plan itself is the checkpoint.
pub(crate) fn build_architect_directive(fast_model: &str, requires_todo: bool) -> String {
    let todo_step = if requires_todo {
        "Then, in the SAME reply and only after the complete plan, use `update_plan` to capture 5-9 items: one per MEANINGFUL step, each with a concrete target + verification. Only code-changing or code-verifying steps; exclude reporting, bookkeeping, and cleanup ceremony.\n\
         Checkpoint, not final answer: after the todo list, continue the task; do not stop on the plan alone.\n"
    } else {
        "The `update_plan` todo tool is not available in this session, so the written plan above IS the checkpoint: continue the task after it; do not stop on the plan alone.\n"
    };
    let gate = if requires_todo {
        "behind an opened todo gate "
    } else {
        ""
    };
    format!(
        "\n\n{ARCHITECT_PROTOCOL_HEADER}\n\
         Target Fast Model: {fast_model}\n\
         You are the frontier architect. STOP: in your NEXT reply, before further exploration, write a complete plan. Enough is known; do not defer.\n\
         Plan first — explicit and comprehensive, your reference for the remainder:\n\
         - Remaining execution-order steps: exact files, symbols, commands, checks.\n\
         - Risks and edge cases; how each landed change will be verified (specific commands, expected outputs). NEVER modify tests or verification assets to pass checks.\n\
         - What is already done, briefly, to prevent repetition.\n\
         Be thorough and concrete. Tools may verify details only after the plan.\n\
         {todo_step}\
         Inspect the relevant code, land ONE foundational working change with `edit_file_hashline`/`write_file`, and verify it. The handoff to {fast_model} is AUTOMATIC after your first edit/write {gate}— no handoff tool call is needed or accepted.\n\
         {ARCHITECT_PROTOCOL_FOOTER}"
    )
}

/// Hidden post-handoff verification checklist for the fast model. Adapted
/// from oh-my-pi's `prewalk-checklist.md`; injected by replacing the plan
/// nudge at handoff time.
pub(crate) fn build_checklist_directive() -> String {
    format!(
        "\n\n{PREWALK_CHECKLIST_HEADER}\n\
         Before claiming task complete, verify:\n\
         - Consistency: if a pattern, signature, or check changed in one place, grep every other call site or duplicate copy needing the identical change. A fix at only some matching sites fails.\n\
         - Scope: if the diff exceeds the minimal issue-resolving change, confirm behavior is unchanged outside the reported issue. Prefer the smallest correct diff over a broader rewrite.\n\
         - Verification: run the issue's full test module or file, not only the expected-to-flip test. A sibling-test-breaking change fails.\n\
         Do not claim task complete until all three checks are done."
    )
}

/// Bounded continuation safety net for a text-only reply to the plan nudge.
/// Adapted from oh-my-pi's `prewalk-continue.md`. Fires at most once per
/// armed prewalk; without it the turn loop treats zero tool calls as a
/// natural stop and production runs die before any code is written.
pub(crate) const PREWALK_CONTINUE_PROMPT: &str = "Continue task now; do not end turn here.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implementation_action_gate_matches_only_mutating_writes() {
        assert!(is_prewalk_implementation_action("edit_file_hashline", false));
        assert!(is_prewalk_implementation_action(
            "edit_files_hashline",
            false
        ));
        assert!(is_prewalk_implementation_action("write_file", false));
        assert!(is_prewalk_implementation_action(
            "apply_workspace_edit_plan",
            false
        ));
        assert!(!is_prewalk_implementation_action("write_file", true));
        assert!(!is_prewalk_implementation_action("read_file", false));
        assert!(!is_prewalk_implementation_action("grep_search", false));
        assert!(!is_prewalk_implementation_action("run_command", false));
        assert!(!is_prewalk_implementation_action("update_plan", false));
        assert!(!is_prewalk_implementation_action("complete_prewalk", false));
        assert!(!is_prewalk_implementation_action("todo", false));
    }

    #[test]
    fn todo_gate_opens_on_successful_update_plan_including_view() {
        assert!(is_prewalk_todo_gate_opener("update_plan", false));
        assert!(!is_prewalk_todo_gate_opener("update_plan", true));
        assert!(!is_prewalk_todo_gate_opener("read_file", false));
    }

    #[test]
    fn noop_detection_covers_model_and_effort() {
        assert!(prewalk_would_be_noop("m", Some(ReasoningEffort::Low), "m", Some(ReasoningEffort::Low)));
        assert!(prewalk_would_be_noop("m", None, "m", None));
        assert!(!prewalk_would_be_noop("a", None, "b", None));
        assert!(!prewalk_would_be_noop(
            "m",
            Some(ReasoningEffort::Low),
            "m",
            Some(ReasoningEffort::High)
        ));
        assert!(!prewalk_would_be_noop("m", None, "m", Some(ReasoningEffort::Low)));
    }

    #[test]
    fn architect_directive_is_automatic_no_handoff_tool() {
        let directive = build_architect_directive("fast-model", true);
        assert!(directive.contains(ARCHITECT_PROTOCOL_HEADER));
        assert!(directive.contains("Target Fast Model: fast-model"));
        assert!(directive.contains("update_plan"));
        assert!(directive.contains("AUTOMATIC"));
        assert!(!directive.contains("complete_prewalk"));
    }

    #[test]
    fn architect_directive_without_todo_tool_demands_no_unfulfillable_call() {
        let directive = build_architect_directive("fast-model", false);
        assert!(directive.contains(ARCHITECT_PROTOCOL_HEADER));
        assert!(directive.contains("AUTOMATIC"));
        assert!(!directive.contains("complete_prewalk"));
        // The written plan itself is the checkpoint when the tool is absent.
        assert!(!directive.contains("use `update_plan`"));
        assert!(directive.contains("IS the checkpoint"));
    }

    #[test]
    fn checklist_covers_consistency_scope_verification() {
        let checklist = build_checklist_directive();
        assert!(checklist.contains("Consistency"));
        assert!(checklist.contains("Scope"));
        assert!(checklist.contains("Verification"));
    }

    #[test]
    fn auto_mode_is_inert_off_by_default() {
        assert_eq!(OrchestratorMode::default(), OrchestratorMode::Off);
        // Deprecated Auto never arms, even for actionable tasks.
        assert_eq!(
            Orchestrator::evaluate("fix the bug in lib.rs", OrchestratorMode::Auto, "pro", "flash", None, true),
            OrchestratorDecision::DirectExecution
        );
        assert_eq!(
            Orchestrator::evaluate("fix the bug", OrchestratorMode::Off, "pro", "flash", None, true),
            OrchestratorDecision::DirectExecution
        );
        assert_eq!(
            Orchestrator::evaluate("fix the bug", OrchestratorMode::Always, "same", "same", None, true),
            OrchestratorDecision::DirectExecution
        );
        assert_eq!(
            Orchestrator::evaluate("   ", OrchestratorMode::Always, "pro", "flash", None, true),
            OrchestratorDecision::DirectExecution
        );
    }

    #[test]
    fn always_mode_engages_with_plan_directive() {
        match Orchestrator::evaluate(
            "Fix the concurrency bug in session store",
            OrchestratorMode::Always,
            "gemini-pro",
            "gemini-flash",
            Some(ReasoningEffort::Low),
            true,
        ) {
            OrchestratorDecision::EngagePrewalk {
                fast_model,
                fast_reasoning,
                architect_system_directive,
            } => {
                assert_eq!(fast_model, "gemini-flash");
                assert_eq!(fast_reasoning, Some(ReasoningEffort::Low));
                assert!(architect_system_directive.contains(ARCHITECT_PROTOCOL_HEADER));
                assert!(architect_system_directive.contains("Target Fast Model: gemini-flash"));
            }
            _ => panic!("expected EngagePrewalk"),
        }
    }
}
