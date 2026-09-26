//! `cross/win/funcs.go`: [`OsFuncs`] over the registry and Win32.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Foundation::{DuplicateHandle, ERROR_MORE_DATA, HANDLE};
use windows_sys::Win32::System::SystemInformation::{
    ComputerNamePhysicalDnsHostname, GetComputerNameExW,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_READ_CONTROL,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
};

use crate::tray::all::{OsFuncs, WpConfig};
use crate::tray::win::registry::{
    NON_INITED_ERROR, Registry, WIN_RUN_AT_STARTUP_KEY_PATH, WIN_SOFTWARE_KEY_PATH,
    delete_registry_value, get_registry_value_int, get_registry_value_string, init_reg,
    update_registry_value_int, update_registry_value_string,
};
use crate::tray::win::syscallstuff::is_process_running;

/// Go's `Windows`. It also holds what `init` builds, which Go keeps in the
/// `registry.go` globals.
pub struct Windows {
    /// The paths `init` gives `SoftwareKey` and `StartupRunKey`: Go's, or
    /// scratch keys in tests.
    software_key_path: String,
    startup_run_key_path: String,
    registry: Option<Registry>,
}

impl Windows {
    /// Go's `NewWindows`.
    pub fn new() -> Self {
        Self {
            software_key_path: WIN_SOFTWARE_KEY_PATH.to_owned(),
            startup_run_key_path: WIN_RUN_AT_STARTUP_KEY_PATH.to_owned(),
            registry: None,
        }
    }

    /// Points `SoftwareKey` and `StartupRunKey` at scratch paths under `HKCU`,
    /// so tests never touch the keys the installed tray reads.
    #[cfg(test)]
    pub(crate) fn with_key_paths(software_key_path: &str, startup_run_key_path: &str) -> Self {
        Self {
            software_key_path: software_key_path.to_owned(),
            startup_run_key_path: startup_run_key_path.to_owned(),
            registry: None,
        }
    }

    fn registry(&self) -> io::Result<&Registry> {
        self.registry
            .as_ref()
            .ok_or_else(|| io::Error::other(NON_INITED_ERROR))
    }
}

impl Default for Windows {
    fn default() -> Self {
        Self::new()
    }
}

impl OsFuncs for Windows {
    fn init(&mut self) -> io::Result<()> {
        let mut registry = init_reg()?;
        registry
            .software_key
            .key_path
            .clone_from(&self.software_key_path);
        registry
            .startup_run_key
            .key_path
            .clone_from(&self.startup_run_key_path);
        self.registry = Some(registry);
        Ok(())
    }

    fn read_config(&self) -> io::Result<WpConfig> {
        let software_key = &self.registry()?.software_key;
        let mut wp = WpConfig::default();
        let port = get_registry_value_string(software_key, "WebPort")?;
        let ver = get_registry_value_string(software_key, "PodVersion").unwrap_or_default();
        let path = get_registry_value_string(software_key, "InstallPath").unwrap_or_default();
        let runatstartup =
            get_registry_value_string(software_key, "RunAtStartup").unwrap_or_default();
        let needsr = get_registry_value_string(software_key, "NeedsRestart").unwrap_or_default();
        let pid = get_registry_value_int(software_key, "LastRunningPID").unwrap_or_default();
        wp.ws_port = port;
        wp.version = ver;
        wp.install_path = path;
        wp.last_running_pid = pid as u32;
        if runatstartup == "true" {
            wp.run_at_startup = true;
        }
        if needsr == "true" {
            wp.needs_restart = true;
        }
        Ok(wp)
    }

    fn write_config(&self, wp: &WpConfig) -> io::Result<()> {
        let software_key = &self.registry()?.software_key;
        update_registry_value_string(software_key, "InstallPath", &wp.install_path)?;
        let _ = update_registry_value_string(software_key, "PodVersion", &wp.version);
        let _ = update_registry_value_string(software_key, "WebPort", &wp.ws_port);
        let _ = update_registry_value_string(
            software_key,
            "RunAtStartup",
            &wp.run_at_startup.to_string(),
        );
        let _ = update_registry_value_string(
            software_key,
            "NeedsRestart",
            &wp.needs_restart.to_string(),
        );
        let _ = update_registry_value_int(
            software_key,
            "LastRunningPID",
            i64::from(wp.last_running_pid),
        );
        Ok(())
    }

    fn run_pod_at_startup(&self, run: bool) -> io::Result<()> {
        let mut conf = self.read_config()?;
        let startup_run_key = &self.registry()?.startup_run_key;
        if run {
            // Go's `filepath.Join` also cleans the path; `Path::join` does not.
            let exe = Path::new(&conf.install_path).join(r"chipper\chipper.exe");
            let cmd = format!(r#"cmd.exe /C start "" "{}" -d"#, exe.display());
            let _ = update_registry_value_string(startup_run_key, "wire-pod", &cmd);
            conf.run_at_startup = true;
        } else {
            let _ = delete_registry_value(startup_run_key, "wire-pod");
            conf.run_at_startup = false;
        }
        self.write_config(&conf)
    }

    fn is_pod_already_running(&self) -> bool {
        let Ok(conf) = self.read_config() else {
            return false;
        };
        if conf.last_running_pid == 0 {
            return false;
        }
        is_process_running(conf.last_running_pid).unwrap_or(false)
    }

    fn is_pid_process_running(&self, pid: u32) -> io::Result<bool> {
        is_process_running(pid)
    }

    fn kill_existing_pod(&self) -> io::Result<()> {
        let conf = self.read_config()?;
        if conf.last_running_pid == 0 {
            return Err(io::Error::other("no pod running (pid: 0)"));
        }
        let proc = find_process(conf.last_running_pid)?;
        let _ = kill(&proc);
        Ok(())
    }

    fn resources_path(&self) -> PathBuf {
        PathBuf::from("./")
    }

    fn hostname(&self) -> String {
        os_hostname().unwrap_or_default()
    }

    fn on_exit(&self) {
        let Ok(mut conf) = self.read_config() else {
            return;
        };
        conf.last_running_pid = 0;
        let _ = self.write_config(&conf);
    }
}

// What follows stands in for Go's `os.FindProcess`, `Process.Kill` and
// `os.Hostname` on Windows.

// Reached only from `kill_existing_pod`, which the tray never calls.
#[allow(dead_code)]
fn find_process(pid: u32) -> io::Result<OwnedHandle> {
    // SAFETY: plain call; a null handle is checked below.
    let h = unsafe {
        OpenProcess(
            PROCESS_READ_CONTROL | PROCESS_QUERY_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if h.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `h` is an open process handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(h) })
}

#[allow(dead_code)]
fn kill(process: &OwnedHandle) -> io::Result<()> {
    let mut termination: HANDLE = ptr::null_mut();
    // SAFETY: `process` is open, and the current-process pseudo-handle needs no
    // closing.
    let ok = unsafe {
        let current = GetCurrentProcess();
        DuplicateHandle(
            current,
            process.as_raw_handle(),
            current,
            &mut termination,
            PROCESS_TERMINATE,
            0,
            0,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `termination` is a new handle that nothing else owns.
    let termination = unsafe { OwnedHandle::from_raw_handle(termination) };
    // SAFETY: `termination` stays open for the call.
    if unsafe { TerminateProcess(termination.as_raw_handle(), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn os_hostname() -> io::Result<String> {
    let mut n = 64u32;
    loop {
        let mut b = vec![0u16; n as usize];
        // SAFETY: `b` holds `n` writable u16s.
        let ok =
            unsafe { GetComputerNameExW(ComputerNamePhysicalDnsHostname, b.as_mut_ptr(), &mut n) };
        if ok != 0 {
            let name: Vec<u16> = b[..n as usize]
                .iter()
                .copied()
                .take_while(|&c| c != 0)
                .collect();
            return Ok(String::from_utf16_lossy(&name));
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
            return Err(err);
        }
        if n <= b.len() as u32 {
            return Err(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{SystemTime, UNIX_EPOCH};

    use windows_sys::Win32::System::Registry::{KEY_READ, KEY_WRITE};
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    use super::*;
    use crate::tray::win::registry::{KeyInfo, RootKey, delete_registry_key};

    /// Two sibling keys under `HKCU\Software\wire-pod-rs-test`, standing in
    /// for `Software\wire-pod` and the `Run` key, deleted on drop.
    struct Scratch {
        software: KeyInfo,
        run: KeyInfo,
    }

    impl Scratch {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let base = format!(r"Software\wire-pod-rs-test\{}-{nanos}", std::process::id());
            let key = |suffix: &str| KeyInfo {
                key: RootKey::CurrentUser,
                perms: KEY_READ | KEY_WRITE,
                key_path: format!("{base}-{suffix}"),
            };
            Self {
                software: key("software"),
                run: key("run"),
            }
        }

        fn windows(&self) -> Windows {
            let mut w = Windows::with_key_paths(&self.software.key_path, &self.run.key_path);
            w.init().unwrap();
            w
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = delete_registry_key(&self.software);
            let _ = delete_registry_key(&self.run);
        }
    }

    #[test]
    fn config_pod_checks_and_hostname_follow_go() {
        let scratch = Scratch::new();
        let uninited = Windows::with_key_paths(&scratch.software.key_path, &scratch.run.key_path);
        assert_eq!(
            uninited.read_config().unwrap_err().to_string(),
            NON_INITED_ERROR
        );
        let w = scratch.windows();
        assert_eq!(w.read_config().unwrap_err().kind(), io::ErrorKind::NotFound);

        // A child that waits on its stdin until it is killed.
        let mut child = Command::new("cmd.exe")
            .args(["/C", "pause"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        let conf = WpConfig {
            ws_port: "8080".to_owned(),
            run_at_startup: true,
            install_path: r"C:\Program Files\wire-pod".to_owned(),
            version: "v1.2.18".to_owned(),
            needs_restart: false,
            last_running_pid: child.id(),
            ..WpConfig::default()
        };
        w.write_config(&conf).unwrap();
        assert_eq!(w.read_config().unwrap(), conf);
        assert_eq!(
            get_registry_value_string(&scratch.software, "RunAtStartup").unwrap(),
            "true"
        );

        assert!(w.is_pod_already_running());
        w.kill_existing_pod().unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(1));
        assert!(!w.is_pod_already_running());

        w.on_exit();
        assert_eq!(w.read_config().unwrap().last_running_pid, 0);
        assert!(w.kill_existing_pod().is_err());

        // `hostname.exe` prints the DNS host name in its own case, which
        // `%COMPUTERNAME%` upper-cases.
        let out = Command::new("hostname.exe")
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .unwrap();
        assert_eq!(w.hostname(), String::from_utf8_lossy(&out.stdout).trim());
    }

    #[test]
    fn run_pod_at_startup_writes_go_command_to_the_run_key() {
        let scratch = Scratch::new();
        let w = scratch.windows();
        let conf = WpConfig {
            ws_port: "8080".to_owned(),
            install_path: r"C:\Program Files\wire-pod".to_owned(),
            ..WpConfig::default()
        };
        w.write_config(&conf).unwrap();

        w.run_pod_at_startup(true).unwrap();
        assert_eq!(
            get_registry_value_string(&scratch.run, "wire-pod").unwrap(),
            r#"cmd.exe /C start "" "C:\Program Files\wire-pod\chipper\chipper.exe" -d"#
        );
        assert!(w.read_config().unwrap().run_at_startup);

        w.run_pod_at_startup(false).unwrap();
        assert_eq!(
            get_registry_value_string(&scratch.run, "wire-pod")
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(!w.read_config().unwrap().run_at_startup);
    }
}
