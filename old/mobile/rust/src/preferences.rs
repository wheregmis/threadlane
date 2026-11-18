//! Thin `NSUserDefaults` wrapper for persisted pairing state.
//!
//! `gpui-mobile`'s `shared_preferences` package calls `-[NSUserDefaults
//! synchronize]` with a `void` return type, but the method returns `BOOL`;
//! objc2's debug signature check panics on the first write. `synchronize` is
//! deprecated — defaults persist without it — so these helpers talk to
//! `standardUserDefaults` directly.

#[cfg(target_os = "ios")]
mod imp {
    use std::ffi::{CStr, CString};

    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};

    fn ns_string(value: &str) -> *mut AnyObject {
        let c_value = CString::new(value).expect("preference values cannot contain NUL");
        unsafe { msg_send![class!(NSString), stringWithUTF8String: c_value.as_ptr()] }
    }

    fn user_defaults() -> *mut AnyObject {
        unsafe { msg_send![class!(NSUserDefaults), standardUserDefaults] }
    }

    pub fn get_string(key: &str) -> Option<String> {
        unsafe {
            let value: *mut AnyObject = msg_send![user_defaults(), stringForKey: ns_string(key)];
            if value.is_null() {
                return None;
            }
            let utf8: *const i8 = msg_send![value, UTF8String];
            if utf8.is_null() {
                Some(String::new())
            } else {
                Some(CStr::from_ptr(utf8).to_string_lossy().into_owned())
            }
        }
    }

    pub fn set_string(key: &str, value: &str) {
        unsafe {
            let _: () =
                msg_send![user_defaults(), setObject: ns_string(value), forKey: ns_string(key)];
        }
    }

    pub fn remove(key: &str) {
        unsafe {
            let _: () = msg_send![user_defaults(), removeObjectForKey: ns_string(key)];
        }
    }
}

#[cfg(not(target_os = "ios"))]
mod imp {
    pub fn get_string(_key: &str) -> Option<String> {
        None
    }

    pub fn set_string(_key: &str, _value: &str) {}

    pub fn remove(_key: &str) {}
}

pub use imp::{get_string, remove, set_string};
