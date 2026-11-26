//! Milestone 1, 3 and 5 exit tests: the real hub and the real runner together
//! (Plan.md §15 milestones 1 and 3, docs/milestone-1.md §10,
//! docs/milestone-3.md §2 and §3).
//!
//! Everything but `live` runs on an in-memory database, a fake clock, and a
//! scripted backend. `live` starts the hub binary with the real `claude` and
//! is ignored by default.

mod harness;
mod invariants;

mod accounts;
mod backends;
mod context;
mod crash;
mod cycle;
mod delivery;
mod evals;
mod failures;
mod fencing;
mod github;
mod initiative;
mod interruptions;
mod live;
mod load;
mod loops;
mod memory;
mod merge;
mod milestone;
mod operations;
mod permissions;
mod publish;
mod review;
mod space_lead;
mod tasks;
mod timers;
mod upstream;
