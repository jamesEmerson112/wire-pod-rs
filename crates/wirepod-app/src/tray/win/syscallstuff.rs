//! `cross/win/syscallstuff.go`: whether a process id names a live process.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows_sys::Win32::Foundation::{ERROR_INVALID_PARAMETER, STILL_ACTIVE};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

pub fn is_process_running(pid: u32) -> io::Result<bool> {
    // needs to be able to see admin processes as well
    // SAFETY: plain call; a null handle is checked below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        let err = io::Error::last_os_error();
        // if the error is "The parameter is incorrect.", it usually means the process does not exist
        // Go compares the English message; the status code is the same test in any language.
        if err.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(false);
        }
        return Err(err);
    }
    // SAFETY: `h` is an open process handle that nothing else owns.
    let h = unsafe { OwnedHandle::from_raw_handle(h) };

    let mut code = 0u32;
    // SAFETY: `h` stays open for the call and `code` is writable.
    if unsafe { GetExitCodeProcess(h.as_raw_handle(), &mut code) } == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(code == STILL_ACTIVE as u32)
}

#[cfg(test)]
mod tests {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    use super::*;

    #[test]
    fn a_live_process_is_running_and_an_exited_one_is_not() {
        assert!(is_process_running(std::process::id()).unwrap());

        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        // `child` still holds its handle, so the pid cannot be reused yet.
        assert!(!is_process_running(pid).unwrap());

        // Windows answers pid 0 with ERROR_INVALID_PARAMETER.
        assert!(!is_process_running(0).unwrap());
    }
}
