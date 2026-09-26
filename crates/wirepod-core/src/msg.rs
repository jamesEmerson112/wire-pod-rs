//! Go's `pkg/logger/msg-winmac.go` and `msg-and.go`: the message boxes a
//! packaged build shows when it cannot bind a port.
//!
//! On Windows zenity's box is a `MessageBoxW`, called here directly. Go also
//! runs zenity on Linux and macOS; there this prints, as `msg-and.go` does.

/// Go's `WarnMsg`.
pub fn warn_msg(msg: &str) {
    #[cfg(windows)]
    win::message_box(msg, win::MB_ICONWARNING);
    #[cfg(not(windows))]
    println!("{msg}");
}

/// Go's `ErrMsg`.
pub fn err_msg(msg: &str) {
    #[cfg(windows)]
    win::message_box(msg, win::MB_ICONERROR);
    #[cfg(not(windows))]
    println!("{msg}");
}

#[cfg(windows)]
mod win {
    pub(super) use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_ICONWARNING};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_SETFOREGROUND, MESSAGEBOX_STYLE, MessageBoxW,
    };

    /// zenity's `message` for a box with only an OK button, which blocks until
    /// it is dismissed.
    pub(super) fn message_box(text: &str, icon: MESSAGEBOX_STYLE) {
        let text = wide(text);
        let title = wide("WirePod");
        // SAFETY: both buffers are NUL-terminated and outlive the call, and a
        // null owner window is allowed.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_SETFOREGROUND | icon,
            );
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }
}
