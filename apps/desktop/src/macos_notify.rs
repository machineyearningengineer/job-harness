//! Native macOS UserNotifications (`UNUserNotificationCenter`) bridge.
//! Delivers notifications with Job Harness's own app icon, prompts official macOS
//! notification permission cleanly, and activates Job Harness (opening the linked role)
//! on click — never spawning Script Editor.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// Only the macOS bridge reports `Denied` or `Authorized`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum NotificationAuthStatus {
    NotDetermined,
    Denied,
    Authorized,
    Unbundled,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::NotificationAuthStatus;
    use std::ffi::CString;

    unsafe extern "C" {
        fn jh_notify_init();
        fn jh_notify_request_auth();
        fn jh_notify_auth_status() -> i32;
        fn jh_notify_send(
            req_id: *const std::ffi::c_char,
            title: *const std::ffi::c_char,
            subtitle: *const std::ffi::c_char,
            body: *const std::ffi::c_char,
            record_path: *const std::ffi::c_char,
        );
        fn jh_notify_take_clicked_record(buf: *mut std::ffi::c_char, cap: i32) -> i32;
    }

    pub fn init() {
        unsafe { jh_notify_init() };
    }

    pub fn request_authorization() {
        unsafe { jh_notify_request_auth() };
    }

    pub fn auth_status() -> NotificationAuthStatus {
        match unsafe { jh_notify_auth_status() } {
            0 => NotificationAuthStatus::NotDetermined,
            1 => NotificationAuthStatus::Denied,
            2 => NotificationAuthStatus::Authorized,
            _ => NotificationAuthStatus::Unbundled,
        }
    }

    pub fn send(id: &str, title: &str, subtitle: &str, body: &str, record_path: Option<&str>) {
        let to_c = |s: &str| CString::new(s.replace('\0', " ")).unwrap_or_default();
        let c_id = to_c(id);
        let c_title = to_c(title);
        let c_sub = to_c(subtitle);
        let c_body = to_c(body);
        let c_rec = to_c(record_path.unwrap_or(""));
        unsafe {
            jh_notify_send(
                c_id.as_ptr(),
                c_title.as_ptr(),
                c_sub.as_ptr(),
                c_body.as_ptr(),
                c_rec.as_ptr(),
            );
        }
    }

    pub fn take_clicked_record() -> Option<String> {
        let mut buf = vec![0u8; 512];
        let n =
            unsafe { jh_notify_take_clicked_record(buf.as_mut_ptr() as *mut _, buf.len() as i32) };
        if n <= 0 {
            return None;
        }
        String::from_utf8(buf[..n as usize].to_vec()).ok()
    }

    pub fn open_system_settings() {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.Notifications-Settings.extension")
            .spawn();
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::NotificationAuthStatus;
    pub fn init() {}
    pub fn request_authorization() {}
    pub fn auth_status() -> NotificationAuthStatus {
        NotificationAuthStatus::Unbundled
    }
    pub fn send(_id: &str, _title: &str, _subtitle: &str, _body: &str, _record_path: Option<&str>) {
    }
    pub fn take_clicked_record() -> Option<String> {
        None
    }
    pub fn open_system_settings() {}
}

pub use imp::{
    auth_status, init, open_system_settings, request_authorization, send, take_clicked_record,
};
