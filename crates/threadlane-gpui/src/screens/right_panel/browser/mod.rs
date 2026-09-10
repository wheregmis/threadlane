mod address;
pub(crate) use address::{resolve_address, search_url, AddressTarget};
mod scripts;
pub(crate) use scripts::{
    act_script, drain_console_logs_js, evaluate_script_wrap, snapshot_js, unwrap_callback_payload,
    wait_check_js,
};

#[cfg(target_os = "macos")]
mod view;
#[cfg(target_os = "macos")]
pub(crate) use view::BrowserView;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
pub(crate) use stub::BrowserView;
