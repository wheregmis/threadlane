//! Threadlane's domain vocabulary and the component interfaces of Plan.md §3.4.
//!
//! Nothing here touches a database, a network, or a process. Concrete
//! implementations live in their own crates; fakes for tests live in
//! `threadlane-testkit`.

pub mod adapters;
pub mod backend;
pub mod client;
pub mod ids;
pub mod link;
pub mod model;
pub mod runtime;
pub mod store;
pub mod time;
pub mod tools;
