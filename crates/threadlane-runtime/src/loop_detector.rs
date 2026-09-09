//! Turn-level loop circuit breaker: the 37×-identical-read failure mode.
//!
//! The repetition cache (dispatcher) stops re-execution waste, but the model
//! can still burn provider calls and context on loops the cache cannot see:
//! ping-ponging tools, or same-error retries with varying arguments. This
//! detector watches the executed tool sequence and trips with a terminal
//! message instead of letting the run continue to the context limit.
//!
//! All tripwires require *consecutive* matches with *identical outputs* —
//! any different call, or any changed output, resets the runs — so
//! legitimate polling (changing outputs) and varied exploration never trip.

use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

/// What tripped the breaker, with a human-readable terminal message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopTrip {
    Identical { name: String, count: usize },
    PingPong { cycle: Vec<String>, rounds: usize },
    ErrorLoop { name: String, count: usize },
}

impl LoopTrip {
    pub fn message(&self) -> String {
        match self {
            LoopTrip::Identical { name, count } => format!(
                "Stopped: `{name}` ran {count} times in a row with identical arguments and identical output. Nothing new can come from another identical call — change the arguments, use a different tool, or summarize what the repeated result tells you."
            ),
            LoopTrip::PingPong { cycle, rounds } => format!(
                "Stopped: tools cycled ({}) {rounds} times in a row with identical outputs. The loop is not converging — pick one different action or summarize.",
                cycle.join(" → "),
            ),
            LoopTrip::ErrorLoop { name, count } => format!(
                "Stopped: `{name}` failed {count} times in a row with the same error. Retrying verbatim will not help — diagnose the error cause, change the approach, or ask the user."
            ),
        }
    }

    pub fn rule_name(&self) -> &'static str {
        match self {
            LoopTrip::Identical { .. } => "repetition-guard/identical",
            LoopTrip::PingPong { .. } => "repetition-guard/ping-pong",
            LoopTrip::ErrorLoop { .. } => "repetition-guard/error-loop",
        }
    }
}

fn hash_strs(parts: &[&str]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for part in parts {
        part.hash(&mut hasher);
    }
    hasher.finish()
}

pub struct LoopDetector {
    enabled: bool,
    identical_limit: usize,
    pingpong_rounds: usize,
    error_limit: usize,
    /// Consecutive identical (call+output) run length, and its key.
    identical_key: Option<u64>,
    identical_count: usize,
    /// Consecutive identical error bodies and their key.
    error_key: Option<u64>,
    error_count: usize,
    /// Recent (output-hash, tool-name) pairs for cycle detection (bounded).
    /// Hashes include name+args+output, so evolving outputs never match.
    history: VecDeque<(u64, String)>,
}

impl LoopDetector {
    pub fn new(
        enabled: bool,
        identical_limit: usize,
        pingpong_rounds: usize,
        error_limit: usize,
    ) -> Self {
        Self {
            enabled,
            identical_limit: identical_limit.max(2),
            pingpong_rounds: pingpong_rounds.max(2),
            error_limit: error_limit.max(2),
            identical_key: None,
            identical_count: 0,
            error_key: None,
            error_count: 0,
            history: VecDeque::with_capacity(16),
        }
    }

    /// Observe one executed tool call. Returns the trip, if any.
    pub fn observe(
        &mut self,
        name: &str,
        args: &str,
        content: &str,
        is_error: bool,
    ) -> Option<LoopTrip> {
        if !self.enabled {
            return None;
        }
        let call_output = hash_strs(&[name, args, content]);
        let error_hash = is_error.then(|| hash_strs(&[name, content]));

        // Identical call+output run.
        if self.identical_key == Some(call_output) {
            self.identical_count += 1;
        } else {
            self.identical_key = Some(call_output);
            self.identical_count = 1;
        }
        if self.identical_count >= self.identical_limit {
            return Some(LoopTrip::Identical {
                name: name.to_string(),
                count: self.identical_count,
            });
        }

        // Same-error run (arguments may vary).
        match (self.error_key, error_hash) {
            (Some(previous), Some(current)) if previous == current => {
                self.error_count += 1;
            }
            (_, current) => {
                self.error_key = current;
                self.error_count = usize::from(current.is_some());
            }
        }
        if self.error_count >= self.error_limit {
            return Some(LoopTrip::ErrorLoop {
                name: name.to_string(),
                count: self.error_count,
            });
        }

        // Ping-pong cycles (period 2-3). Period 1 belongs to the identical
        // rule above; anything else needs full-block equality, so evolving
        // outputs never match.
        self.history.push_back((call_output, name.to_string()));
        while self.history.len() > 16 {
            self.history.pop_front();
        }
        let hashes: Vec<u64> = self.history.iter().map(|(hash, _)| *hash).collect();
        for period in 2..=3 {
            if cycle_rounds(&hashes, period) >= self.pingpong_rounds {
                let cycle: Vec<String> = self.history
                    .iter()
                    .skip(self.history.len() - period)
                    .map(|(_, name)| name.clone())
                    .collect();
                return Some(LoopTrip::PingPong {
                    cycle,
                    rounds: self.pingpong_rounds,
                });
            }
        }
        None
    }
}

/// How many consecutive trailing blocks of `period` repeat. Full blocks
/// only: a partial tail does not count.
fn cycle_rounds(history: &[u64], period: usize) -> usize {
    if period == 0 || history.len() < period {
        return 0;
    }
    let len = history.len();
    let block = &history[len - period..];
    let mut rounds = 0;
    let mut start = len;
    while start >= period {
        start -= period;
        if history[start..start + period] == *block {
            rounds += 1;
        } else {
            break;
        }
    }
    rounds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detector() -> LoopDetector {
        LoopDetector::new(true, 5, 3, 3)
    }

    #[test]
    fn identical_calls_trip_at_limit_with_same_output() {
        let mut detector = detector();
        for n in 1..5 {
            assert_eq!(
                detector.observe("read_file", "{}", "contents", false),
                None,
                "call {n} must not trip yet"
            );
        }
        assert_eq!(
            detector.observe("read_file", "{}", "contents", false),
            Some(LoopTrip::Identical {
                name: "read_file".into(),
                count: 5
            })
        );
    }

    #[test]
    fn changing_output_resets_the_identical_run() {
        let mut detector = detector();
        for _ in 0..4 {
            assert_eq!(detector.observe("read_file", "{}", "v1", false), None);
        }
        // Output changed: the run restarts even though name+args match.
        assert_eq!(detector.observe("read_file", "{}", "v2", false), None);
        for _ in 0..3 {
            assert_eq!(detector.observe("read_file", "{}", "v2", false), None);
        }
        assert!(matches!(
            detector.observe("read_file", "{}", "v2", false),
            Some(LoopTrip::Identical { .. })
        ));
    }

    #[test]
    fn different_calls_reset_all_runs() {
        let mut detector = detector();
        for _ in 0..4 {
            assert_eq!(detector.observe("read_file", "{}", "v1", false), None);
        }
        assert_eq!(detector.observe("list_dir", "{}", "v1", false), None);
        for _ in 0..4 {
            assert_eq!(detector.observe("read_file", "{}", "v1", false), None);
        }
        assert!(matches!(
            detector.observe("read_file", "{}", "v1", false),
            Some(LoopTrip::Identical { .. })
        ));
    }

    #[test]
    fn ping_pong_cycles_trip_with_names() {
        let mut detector = detector();
        for _ in 0..2 {
            assert_eq!(detector.observe("read_file", "{}", "a", false), None);
            assert_eq!(detector.observe("list_dir", "{}", "b", false), None);
        }
        // Fifth and sixth observations complete the 3rd round.
        assert_eq!(detector.observe("read_file", "{}", "a", false), None);
        assert_eq!(
            detector.observe("list_dir", "{}", "b", false),
            Some(LoopTrip::PingPong {
                cycle: vec!["read_file".into(), "list_dir".into()],
                rounds: 3
            })
        );
    }

    #[test]
    fn ping_pong_requires_stable_outputs() {
        let mut detector = detector();
        for round in 0..5 {
            assert_eq!(
                detector.observe("read_file", "{}", &format!("a{round}"), false),
                None
            );
            assert_eq!(
                detector.observe("list_dir", "{}", &format!("b{round}"), false),
                None
            );
        }
    }

    #[test]
    fn same_error_trips_regardless_of_arguments() {
        let mut detector = detector();
        assert_eq!(
            detector.observe("read_file", r#"{"path":"a"}"#, "denied", true),
            None
        );
        assert_eq!(
            detector.observe("read_file", r#"{"path":"b"}"#, "denied", true),
            None
        );
        assert_eq!(
            detector.observe("read_file", r#"{"path":"c"}"#, "denied", true),
            Some(LoopTrip::ErrorLoop {
                name: "read_file".into(),
                count: 3
            })
        );
    }

    #[test]
    fn error_run_breaks_on_success() {
        let mut detector = detector();
        assert_eq!(
            detector.observe("read_file", r#"{"path":"a"}"#, "denied", true),
            None
        );
        assert_eq!(
            detector.observe("read_file", r#"{"path":"b"}"#, "ok", false),
            None
        );
        assert_eq!(
            detector.observe("read_file", r#"{"path":"c"}"#, "denied", true),
            None,
            "error run must restart after a success"
        );
    }

    #[test]
    fn disabled_detector_never_trips() {
        let mut detector = LoopDetector::new(false, 5, 3, 3);
        for _ in 0..10 {
            assert_eq!(detector.observe("read_file", "{}", "same", false), None);
        }
    }

    #[test]
    fn cycle_rounds_counts_full_trailing_blocks() {
        assert_eq!(cycle_rounds(&[1, 2, 1, 2, 1, 2], 2), 3);
        assert_eq!(cycle_rounds(&[7, 7, 7, 7], 1), 4);
        assert_eq!(cycle_rounds(&[1, 2, 1, 2, 1], 2), 2);
        assert_eq!(cycle_rounds(&[1, 2, 3, 4], 2), 1);
        // Single trailing block always counts once; tripwires need more.
        assert_eq!(cycle_rounds(&[1, 2], 2), 1);
    }
}
