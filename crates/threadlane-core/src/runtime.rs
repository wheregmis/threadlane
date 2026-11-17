//! Wake hints and crash-injection step boundaries (Plan.md §3.4, §6.5, §16).

use std::fmt;

use crate::ids::LaneId;

/// Who a wake hint is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WakeTarget {
    /// Runners: work may be available.
    Runners,
    /// A specific lane has new events.
    Lane(LaneId),
    /// The dispatcher: the outbox has new messages.
    Dispatcher,
}

/// Delivers wake hints. Hints are best effort; correctness never depends on
/// them, because every consumer also polls on a slow interval (Plan.md §6.5).
pub trait Waker: Send + Sync + fmt::Debug {
    /// Sends a hint. Never blocks and never fails.
    fn wake(&self, target: WakeTarget);
}

/// A waker that drops every hint.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopWaker;

impl Waker for NoopWaker {
    fn wake(&self, _target: WakeTarget) {}
}

/// A named step boundary inside a component, for example `runner.after_commit`.
///
/// Components call [`StepHook::reach`] at every boundary listed in
/// [`steps`]; a test hook can stop the component there as if its process had
/// been killed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Step(pub &'static str);

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// The component was stopped at a step boundary by an injected crash.
///
/// Treat it like process death: propagate it, drop in-memory state, and let
/// recovery start again from the stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("killed at step {step}")]
pub struct Killed {
    /// Where the component was stopped.
    pub step: Step,
}

/// Called at every step boundary.
pub trait StepHook: Send + Sync + fmt::Debug {
    /// Returns `Err(Killed)` if the component must stop here.
    ///
    /// # Errors
    ///
    /// [`Killed`] when an injected crash is due at this step.
    fn reach(&self, step: Step) -> Result<(), Killed>;
}

/// The production hook: never kills anything.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHooks;

impl StepHook for NoHooks {
    fn reach(&self, _step: Step) -> Result<(), Killed> {
        Ok(())
    }
}

/// Step boundaries named in Plan.md. Components reference these constants so
/// crash tests can enumerate every boundary.
pub mod steps {
    use super::Step;

    /// Runner cycle, Plan.md §4.3. The runner reaches these between its
    /// calls to the hub, so each one is a distinct durable state.
    pub mod runner {
        use super::Step;

        /// The lease was granted.
        pub const AFTER_CLAIM: Step = Step("runner.after_claim");
        /// The debounce window has passed (Plan.md §4.8).
        pub const AFTER_DEBOUNCE: Step = Step("runner.after_debounce");
        /// The hub opened the run and claimed its events.
        pub const AFTER_START_RUN: Step = Step("runner.after_start_run");
        /// Task lanes: the working directory is ready: its worktrees, or a
        /// plain directory for a task without repositories.
        pub const AFTER_PREPARE_WORKSPACE: Step = Step("runner.after_prepare_workspace");
        /// The backend process was started.
        pub const AFTER_BACKEND_START: Step = Step("runner.after_backend_start");
        /// The backend finished and its events were mirrored.
        pub const AFTER_BACKEND_RUN: Step = Step("runner.after_backend_run");
        /// Task lanes: uncommitted work was committed on the task branch.
        /// The bundle upload of Plan.md §5.5 is milestone 6.
        pub const AFTER_CHECKPOINT: Step = Step("runner.after_checkpoint");
        /// The hub committed the run.
        pub const AFTER_COMMIT: Step = Step("runner.after_commit");
        /// The hub reported nothing more to run in the lane.
        pub const AFTER_RECHECK: Step = Step("runner.after_recheck");
        /// The lease was released.
        pub const AFTER_RELEASE: Step = Step("runner.after_release");

        /// Every runner boundary, in cycle order.
        pub const ALL: &[Step] = &[
            AFTER_CLAIM,
            AFTER_DEBOUNCE,
            AFTER_START_RUN,
            AFTER_PREPARE_WORKSPACE,
            AFTER_BACKEND_START,
            AFTER_BACKEND_RUN,
            AFTER_CHECKPOINT,
            AFTER_COMMIT,
            AFTER_RECHECK,
            AFTER_RELEASE,
        ];

        /// The boundaries a lead lane reaches (it has no workspace).
        pub const LEAD: &[Step] = &[
            AFTER_CLAIM,
            AFTER_DEBOUNCE,
            AFTER_START_RUN,
            AFTER_BACKEND_START,
            AFTER_BACKEND_RUN,
            AFTER_COMMIT,
            AFTER_RECHECK,
            AFTER_RELEASE,
        ];

        /// The boundaries a task lane reaches: the lead's, plus preparing
        /// its worktrees and committing its work.
        pub const TASK: &[Step] = &[
            AFTER_CLAIM,
            AFTER_DEBOUNCE,
            AFTER_START_RUN,
            AFTER_PREPARE_WORKSPACE,
            AFTER_BACKEND_START,
            AFTER_BACKEND_RUN,
            AFTER_CHECKPOINT,
            AFTER_COMMIT,
            AFTER_RECHECK,
            AFTER_RELEASE,
        ];
    }

    /// Starting a run inside the hub, Plan.md §4.3 steps 2 to 5.3.
    pub mod begin_run {
        use super::Step;

        pub const AFTER_READ_PENDING: Step = Step("begin_run.after_read_pending");
        pub const AFTER_INITIATIVE_CHECK: Step = Step("begin_run.after_initiative_check");
        pub const AFTER_START_RUN: Step = Step("begin_run.after_start_run");
        pub const AFTER_ASSEMBLE_CONTEXT: Step = Step("begin_run.after_assemble_context");

        /// Every boundary, in order.
        pub const ALL: &[Step] = &[
            AFTER_READ_PENDING,
            AFTER_INITIATIVE_CHECK,
            AFTER_START_RUN,
            AFTER_ASSEMBLE_CONTEXT,
        ];
    }

    /// One call to one of the system's tools inside the hub, Plan.md §4.6.
    pub mod tool_call {
        use super::Step;

        /// The call was recorded as started.
        pub const AFTER_BEGIN: Step = Step("tool_call.after_begin");
        /// The tool took effect.
        pub const AFTER_EFFECT: Step = Step("tool_call.after_effect");
        /// The result was recorded.
        pub const AFTER_FINISH: Step = Step("tool_call.after_finish");

        /// Every boundary, in order.
        pub const ALL: &[Step] = &[AFTER_BEGIN, AFTER_EFFECT, AFTER_FINISH];
    }

    /// Performing a stored call you approved, Plan.md §9.4: the hub records
    /// the intent and the outcome, and the runner makes the effect in
    /// between, or the hub itself for a memory change.
    pub mod perform {
        use super::Step;

        /// The hub recorded that the call is about to be performed.
        pub const AFTER_BEGIN: Step = Step("perform.after_begin");
        /// The runner made the effect through the forge, or the hub wrote
        /// the memory change.
        pub const AFTER_EFFECT: Step = Step("perform.after_effect");
        /// The hub recorded the outcome.
        pub const AFTER_FINISH: Step = Step("perform.after_finish");

        /// Every boundary, in order.
        pub const ALL: &[Step] = &[AFTER_BEGIN, AFTER_EFFECT, AFTER_FINISH];
    }

    /// Scheduler pass, Plan.md §7.3.
    pub mod scheduler {
        use super::Step;

        pub const AFTER_SCAN: Step = Step("scheduler.after_scan");
        pub const AFTER_FIRE: Step = Step("scheduler.after_fire");
        pub const AFTER_WAKE: Step = Step("scheduler.after_wake");

        /// Every scheduler boundary, in pass order.
        pub const ALL: &[Step] = &[AFTER_SCAN, AFTER_FIRE, AFTER_WAKE];
    }

    /// Dispatcher pass, Plan.md §10.2.
    pub mod dispatcher {
        use super::Step;

        pub const AFTER_READ: Step = Step("dispatcher.after_read");
        pub const AFTER_DELIVER: Step = Step("dispatcher.after_deliver");
        pub const AFTER_MARK_SENT: Step = Step("dispatcher.after_mark_sent");

        /// Every dispatcher boundary, in pass order.
        pub const ALL: &[Step] = &[AFTER_READ, AFTER_DELIVER, AFTER_MARK_SENT];
    }

    /// A usage limit inside the hub, Plan.md §5.3.
    pub mod account {
        use super::Step;

        /// The account's cooldown was recorded.
        pub const AFTER_COOLDOWN: Step = Step("account.after_cooldown");
        /// The system notice that the space is blocked, or has resumed, was
        /// queued.
        pub const AFTER_NOTICE: Step = Step("account.after_notice");

        /// Every boundary, in order.
        pub const ALL: &[Step] = &[AFTER_COOLDOWN, AFTER_NOTICE];
    }

    /// Placement, Plan.md §5.4.
    pub mod placement {
        use super::Step;

        pub const AFTER_SELECT: Step = Step("placement.after_select");
        pub const AFTER_GRANT: Step = Step("placement.after_grant");

        /// Every placement boundary.
        pub const ALL: &[Step] = &[AFTER_SELECT, AFTER_GRANT];
    }
}
