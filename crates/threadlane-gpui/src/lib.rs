//! Threadlane desktop application binary shell.
//!
//! All product code now lives in the focused `threadlane-ui-*` crates; this
//! crate contributes only the `main` entry point (window setup, logging,
//! `--dump-config`) that composes them. This module stays intentionally
//! empty so no new shared state accretes here.
