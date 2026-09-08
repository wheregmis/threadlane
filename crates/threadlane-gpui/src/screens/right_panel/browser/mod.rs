mod address;
pub(crate) use address::{AddressTarget, resolve_address, search_url};
mod scripts;
pub(crate) use scripts::{
    act_script, evaluate_script_wrap, snapshot_js, unwrap_callback_payload,
};

#[cfg(target_os = "macos")]
mod view;
#[cfg(target_os = "macos")]
pub(crate) use view::BrowserView;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
pub(crate) use stub::BrowserView;
