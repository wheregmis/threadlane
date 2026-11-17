//! Milestone 1 exit tests: the real hub and the real runner together
//! (Plan.md §15 milestone 1, docs/milestone-1.md §10).
//!
//! Everything but `live` runs on an in-memory database, a fake clock, and a
//! scripted backend. `live` starts the hub binary with the real `claude` and
//! is ignored by default.

mod harness;
mod invariants;

mod crash;
mod cycle;
mod failures;
mod fencing;
mod live;
mod loops;
