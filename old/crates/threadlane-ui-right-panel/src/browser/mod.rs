mod address;
pub use address::{resolve_address, search_url, AddressTarget};
mod scripts;
pub use scripts::{
    act_script, annotate_install_js, annotate_poll_js, annotate_uninstall_js,
    drain_console_logs_js, evaluate_script_wrap, snapshot_js, unwrap_callback_payload,
    wait_check_js,
};

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
mod view;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
mod webview;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub use view::BrowserView;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod stub;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub use stub::BrowserView;
