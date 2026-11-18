//! "Share with mobile" surface for the Threadlane desktop app.
//!
//! [`open_pairing_dialog`] shows the pairing QR code: it starts the LAN
//! listener from [`threadlane_daemon::pairing`] on demand, renders the
//! `threadlane://pair` deep link the mobile app scans, and stops sharing on
//! request. The desktop sidebar footer button is the entry point.

mod dialog;

pub use dialog::open_pairing_dialog;
