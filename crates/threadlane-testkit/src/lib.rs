//! Test harness for Threadlane (Plan.md §15 milestone 0, §16).
//!
//! - [`FakeClock`]: time moves only when the test moves it.
//! - [`ScriptedBackend`]: a backend that plays [`Script`]s instead of calling a
//!   model, and keeps native sessions the way a real backend does.
//! - [`CrashPlan`]: kills a component at a named step boundary.
//! - [`fakes`]: in-memory implementations of the remaining interfaces.
//! - [`git`]: local bare repositories for workspace tests.
//! - [`conformance`]: the backend conformance suite every driver runs.
//!
//! Stores are faked by opening the real store on an in-memory database
//! (`threadlane_store::HubDb::open_in_memory`).

mod backend;
mod clock;
pub mod conformance;
mod crash;
pub mod fakes;
pub mod git;

pub use crate::backend::{ReceivedRun, Script, ScriptStep, ScriptedBackend, SessionOutcome};
pub use crate::clock::FakeClock;
pub use crate::crash::CrashPlan;
