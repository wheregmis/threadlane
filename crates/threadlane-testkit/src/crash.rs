//! Crash injection at named step boundaries (Plan.md §16).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use threadlane_core::runtime::{Killed, Step, StepHook};

#[derive(Debug, Default)]
struct State {
    /// Kill at the n-th time (1-based) each step is reached.
    kills: HashMap<Step, u32>,
    reached: Vec<Step>,
    counts: HashMap<Step, u32>,
    killed: Option<Step>,
}

/// A [`StepHook`] that records every boundary reached and kills the component
/// at the ones it was told to.
///
/// Once it has killed, every later step also reports [`Killed`], so a
/// component cannot accidentally keep going after its "death".
#[derive(Debug, Clone, Default)]
pub struct CrashPlan {
    state: Arc<Mutex<State>>,
}

impl CrashPlan {
    /// A plan that kills nothing and only records.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A plan that kills at the first time `step` is reached.
    #[must_use]
    pub fn kill_at(step: Step) -> Self {
        Self::kill_at_nth(step, 1)
    }

    /// A plan that kills the `nth` (1-based) time `step` is reached.
    #[must_use]
    pub fn kill_at_nth(step: Step, nth: u32) -> Self {
        let plan = Self::new();
        plan.state
            .lock()
            .expect("crash plan lock poisoned")
            .kills
            .insert(step, nth);
        plan
    }

    /// Every boundary reached so far, in order, including the fatal one.
    #[must_use]
    pub fn reached(&self) -> Vec<Step> {
        self.state
            .lock()
            .expect("crash plan lock poisoned")
            .reached
            .clone()
    }

    /// Where the component was killed, if it was.
    #[must_use]
    pub fn killed(&self) -> Option<Step> {
        self.state.lock().expect("crash plan lock poisoned").killed
    }
}

impl StepHook for CrashPlan {
    fn reach(&self, step: Step) -> Result<(), Killed> {
        let mut state = self.state.lock().expect("crash plan lock poisoned");
        if let Some(step) = state.killed {
            return Err(Killed { step });
        }
        state.reached.push(step);
        let count = {
            let count = state.counts.entry(step).or_insert(0);
            *count += 1;
            *count
        };
        if state.kills.get(&step) == Some(&count) {
            state.killed = Some(step);
            return Err(Killed { step });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Step = Step("test.a");
    const B: Step = Step("test.b");

    #[test]
    fn kills_at_the_named_occurrence_and_stays_dead() {
        let plan = CrashPlan::kill_at_nth(A, 2);
        assert!(plan.reach(A).is_ok());
        assert!(plan.reach(B).is_ok());
        assert_eq!(plan.reach(A), Err(Killed { step: A }));
        assert_eq!(plan.reach(B), Err(Killed { step: A }));
        assert_eq!(plan.reached(), vec![A, B, A]);
        assert_eq!(plan.killed(), Some(A));
    }
}
