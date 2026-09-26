//! Go's `pkg/logger/msg-winmac.go` and `msg-and.go`: the message boxes a
//! packaged build shows when it cannot bind a port.
//!
//! Scaffolding for M6 stage 3. Until the Windows message box lands, both
//! functions print, which is what `msg-and.go` does.

/// Go's `WarnMsg`.
pub fn warn_msg(msg: &str) {
    println!("{msg}");
}

/// Go's `ErrMsg`.
pub fn err_msg(msg: &str) {
    println!("{msg}");
}
