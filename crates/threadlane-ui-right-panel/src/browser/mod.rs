mod address;
pub use address::{resolve_address, search_url, AddressTarget};
mod scripts;
pub use scripts::{
    act_script, drain_console_logs_js, evaluate_script_wrap, snapshot_js, unwrap_callback_payload,
    wait_check_js,
};

#[cfg(target_os = "macos")]
mod view;
#[cfg(target_os = "macos")]
pub use view::BrowserView;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
pub use stub::BrowserView;
