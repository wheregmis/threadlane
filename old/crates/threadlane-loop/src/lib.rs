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

/// Default tripwire thresholds, shared by the live [`LoopDetector`], the
/// runtime config defaults, and the durable trajectory anomaly pass so the
/// three definitions of "loop" cannot drift apart.
pub const DEFAULT_IDENTICAL_LIMIT: usize = 5;
pub const DEFAULT_PINGPONG_ROUNDS: usize = 3;
pub const DEFAULT_ERROR_LIMIT: usize = 3;

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

/// Comparison material cap: full call+output strings verify hash matches
/// (ruling out u64 collisions) without retaining megabytes per entry.
const MATERIAL_CHARS: usize = 4_096;

/// Error normalization cap: normalized errors compare by shape, not by
/// embedded timestamps/pids/ports.
const NORMALIZED_ERROR_CHARS: usize = 8_192;

/// Builds the string half of an equality check: name, args, and a bounded
/// content prefix. The u64 hash covers the full content; both must match,
/// so a trip needs a real repeat, not a 2^-64 collision.
fn comparison_material(name: &str, args: &str, content: &str) -> String {
    let mut material = String::with_capacity(256);
    material.push_str(name);
    material.push('\0');
    material.push_str(args);
    material.push('\0');
    material.extend(content.chars().take(MATERIAL_CHARS));
    material
}

/// Normalizes an error body for loop comparison: timestamps, pids, ports,
/// durations, and other digit runs become `#`, whitespace collapses.
/// Timestamped-but-identical failures then compare equal (no false
/// negatives) while structurally different errors still differ.
fn normalize_error(content: &str) -> String {
    let mut out = String::with_capacity(content.len().min(NORMALIZED_ERROR_CHARS));
    let mut in_digits = false;
    let mut in_space = false;
    for ch in content.chars() {
        if out.chars().count() >= NORMALIZED_ERROR_CHARS {
            break;
        }
        if ch.is_ascii_digit() {
            if !in_digits {
                out.push('#');
                in_digits = true;
            }
            in_space = false;
        } else if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
            in_digits = false;
        } else {
            out.push(ch);
            in_digits = false;
            in_space = false;
        }
    }
    out
}

pub struct LoopDetector {
    enabled: bool,
    identical_limit: usize,
    pingpong_rounds: usize,
    error_limit: usize,
    /// Consecutive identical (call+output) run length, keyed by full hash
    /// plus comparison material: both must match, so a u64 collision alone
    /// can never trip.
    identical_key: Option<(u64, String)>,
    identical_count: usize,
    /// Consecutive identical error bodies (normalized) and their key.
    error_key: Option<(u64, String)>,
    error_count: usize,
    /// Recent calls for cycle detection (bounded). Hashes include
    /// name+args+output, so evolving outputs never match; material verifies
    /// hash-level cycle matches before tripping.
    history: VecDeque<(u64, String, String)>,
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
            history: VecDeque::with_capacity(32),
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
        let call_material = comparison_material(name, args, content);
        let normalized = if is_error {
            Some(normalize_error(content))
        } else {
            None
        };
        let error_hash = normalized
            .as_deref()
            .map(|shape| (hash_strs(&[name, shape]), shape.to_string()));

        // Identical call+output run (hash AND material must match).
        let call_key = (call_output, call_material.clone());
        if self.identical_key.as_ref() == Some(&call_key) {
            self.identical_count += 1;
        } else {
            self.identical_key = Some(call_key);
            self.identical_count = 1;
        }

        if self.identical_count >= self.identical_limit {
            return Some(LoopTrip::Identical {
                name: name.to_string(),
                count: self.identical_count,
            });
        }

        // Same-error run (arguments may vary; bodies compare normalized).
        match (&self.error_key, &error_hash) {
            (Some(previous), Some(current)) if previous == current => {
                self.error_count += 1;
            }
            (_, current) => {
                self.error_count = usize::from(current.is_some());
                self.error_key = current.clone();
            }
        }
        if self.error_count >= self.error_limit {
            return Some(LoopTrip::ErrorLoop {
                name: name.to_string(),
                count: self.error_count,
            });
        }

        // Ping-pong cycles (periods 2-4 over a 32-deep window, so longer
        // interleaved loops survive). Period 1 belongs to the identical rule
        // above; anything else needs full-block equality of hashes verified
        // against comparison material, so evolving outputs never match and a
        // bare hash collision cannot trip.
        self.history
            .push_back((call_output, name.to_string(), call_material));
        while self.history.len() > 32 {
            self.history.pop_front();
        }
        let hashes: Vec<u64> = self.history.iter().map(|(hash, _, _)| *hash).collect();
        for period in 2..=4 {
            let rounds = cycle_rounds(&hashes, period);
            if rounds >= self.pingpong_rounds
                && cycle_material_matches(&self.history, period, rounds)
            {
                let cycle: Vec<String> = self
                    .history
                    .iter()
                    .skip(self.history.len() - period)
                    .map(|(_, name, _)| name.clone())
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

/// Verifies a hash-level cycle against the stored comparison material: the
/// trailing `rounds` blocks must repeat byte-for-byte (bounded prefix), so a
/// u64 collision across differing calls cannot trip the cycle rule.
fn cycle_material_matches(
    history: &VecDeque<(u64, String, String)>,
    period: usize,
    rounds: usize,
) -> bool {
    let len = history.len();
    if period == 0 || len < period * rounds {
        return false;
    }
    let block: Vec<&str> = history
        .iter()
        .skip(len - period)
        .map(|(_, _, material)| material.as_str())
        .collect();
    for round in 1..rounds {
        let start = len - (round + 1) * period;
        let candidate: Vec<&str> = history
            .iter()
            .skip(start)
            .take(period)
            .map(|(_, _, material)| material.as_str())
            .collect();
        if candidate != block {
            return false;
        }
    }
    true
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
        LoopDetector::new(
            true,
            DEFAULT_IDENTICAL_LIMIT,
            DEFAULT_PINGPONG_ROUNDS,
            DEFAULT_ERROR_LIMIT,
        )
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
        let mut detector = LoopDetector::new(
            false,
            DEFAULT_IDENTICAL_LIMIT,
            DEFAULT_PINGPONG_ROUNDS,
            DEFAULT_ERROR_LIMIT,
        );
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

#[cfg(test)]
mod hardening_tests {
    use super::*;

    #[test]
    fn timestamped_errors_still_trip_as_one_loop() {
        let mut detector = LoopDetector::new(true, 10, 10, 3);
        assert_eq!(
            detector.observe("run", "{}", "failed after 120ms (pid 1001)", true),
            None
        );
        assert_eq!(
            detector.observe("run", "{}", "failed after 340ms (pid 1002)", true),
            None
        );
        assert!(matches!(
            detector.observe("run", "{}", "failed after 15ms (pid 1003)", true),
            Some(LoopTrip::ErrorLoop { count: 3, .. })
        ));
    }

    #[test]
    fn structurally_different_errors_do_not_merge() {
        let mut detector = LoopDetector::new(true, 10, 10, 3);
        assert_eq!(detector.observe("run", "{}", "connection refused", true), None);
        assert_eq!(detector.observe("run", "{}", "permission denied", true), None);
        assert_eq!(detector.observe("run", "{}", "connection refused", true), None);
    }

    #[test]
    fn period_four_cycles_trip() {
        let mut detector = LoopDetector::new(true, 100, 2, 100);
        let calls = ["a", "b", "c", "d", "a", "b", "c", "d"];
        for (index, name) in calls.iter().enumerate() {
            let last = index == calls.len() - 1;
            let trip = detector.observe(name, "{}", "same", false);
            if last {
                assert!(matches!(trip, Some(LoopTrip::PingPong { .. })), "got {trip:?}");
            } else {
                assert_eq!(trip, None);
            }
        }
    }
}
