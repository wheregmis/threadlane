//! `threadlane-mobile`: a thin GPUI client that watches sessions on a
//! Threadlane desktop over the daemon's WebSocket contract.
//!
//! The Swift container (`mobile/ios`) owns UIKit and links this crate's
//! static library. Rust owns the whole UI inside the GPUI surface; the
//! container's only jobs are driving `gpui_ios_request_frame` off a
//! `CADisplayLink` and forwarding `threadlane://` pairing URLs.

// Link gpui-mobile so its symbols (ios platform, ffi exports) are available.
extern crate gpui_mobile;

pub mod app;
pub mod client;
pub mod preferences;
pub mod saved_devices;

#[cfg(target_os = "ios")]
use gpui::{prelude::*, App, WindowOptions};

/// Minimal logger that routes Rust `log` messages through NSLog.
#[cfg(target_os = "ios")]
struct NsLogLogger;

#[cfg(target_os = "ios")]
impl log::Log for NsLogLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            nslog(&format!(
                "[{}] {}: {}",
                record.level(),
                record.target(),
                record.args()
            ));
        }
    }
    fn flush(&self) {}
}

#[cfg(target_os = "ios")]
fn nslog(msg: &str) {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    unsafe {
        extern "C" {
            fn NSLog(fmt: *mut AnyObject, ...);
        }
        let c_msg = std::ffi::CString::new(msg).unwrap_or_default();
        let ns_msg: *mut AnyObject = msg_send![class!(NSString), alloc];
        let ns_msg: *mut AnyObject = msg_send![ns_msg, initWithUTF8String: c_msg.as_ptr()];
        let c_fmt = std::ffi::CString::new("%@").unwrap_or_default();
        let ns_fmt: *mut AnyObject = msg_send![class!(NSString), alloc];
        let ns_fmt: *mut AnyObject = msg_send![ns_fmt, initWithUTF8String: c_fmt.as_ptr()];
        NSLog(ns_fmt, ns_msg);
    }
}

/// Register the app's root view with the GPUI iOS platform.
///
/// Called from the container (before `gpui_ios_run_demo`) so the run loop
/// knows which view to build. Deep links are stashed for the view to
/// drain — the handler runs on a UIKit thread with no GPUI context.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn gpui_ios_register_app() {
    let _ = log::set_logger(&NsLogLogger).map(|()| log::set_max_level(log::LevelFilter::Info));

    std::panic::set_hook(Box::new(|info| {
        nslog(&format!("GPUI PANIC: {info}"));
    }));

    gpui_mobile::packages::deeplink::set_deep_link_handler(|url| {
        app::push_deeplink(url.to_string());
    });

    gpui_mobile::ios::ffi::set_app_callback(Box::new(|cx: &mut App| {
        gpui_kit::init(cx);
        // Register the bundled Threadlane themes and apply the saved/default
        // selection — `init`'s themes-dir watch is desktop-only.
        threadlane_ui_theme::init_bundled(cx);
        cx.open_window(WindowOptions::default(), |window, cx| {
            let content = cx.new(|cx| app::MobileApp::new(window, cx));
            cx.new(|cx| gpui_kit::component::Root::new(content, window, cx))
        })
        .expect("open Threadlane mobile view");
    }));
}

/// Forward scene foreground activation to the active reconnect driver.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn threadlane_mobile_did_become_active() {
    app::request_foreground_reconnect();
}
