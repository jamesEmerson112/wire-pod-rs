//! `cross/win/registry.go`: the tray's registry keys and the calls that read
//! and write them.

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::ptr;

use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, KEY_QUERY_VALUE, KEY_READ,
    KEY_WRITE, REG_DWORD, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE, REG_QWORD, REG_SZ, RegCloseKey,
    RegDeleteKeyW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows_sys::core::PCWSTR;

// `Software\Microsoft\Windows\CurrentVersion\Uninstall\wire-pod`

// DisplayIcon string (path)
// DisplayVersion string (v1.0.0)
// Publisher string (github.com/kercre123)
// UninstallString (is.Where + uninstall.exe)
// InstallLocation (is.Where)
pub const WIN_UNINSTALL_KEY_KEY: RootKey = RootKey::LocalMachine;
pub const WIN_UNINSTALL_KEY_PATH: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\WirePod";
pub const WIN_UNINSTALL_INSTALLER_PERMS: u32 = KEY_ALL_ACCESS;

// shouldn't ever need to access uninstall path in pod software, but go off i guess
pub const WIN_UNINSTALL_POD_PERMS: u32 = KEY_QUERY_VALUE;

// InstallPath string (is.Where)
// PodVersion string (v1.0.0)
// WebPort string (8080)
// LastRunningPID int (for wire-pod runtime, installer shouldn't touch this)
pub const WIN_SOFTWARE_KEY_KEY: RootKey = RootKey::CurrentUser;
pub const WIN_SOFTWARE_POD_PERMS: u32 = KEY_READ | KEY_WRITE;
pub const WIN_SOFTWARE_INSTALLER_PERMS: u32 = KEY_ALL_ACCESS;
pub const WIN_SOFTWARE_KEY_PATH: &str = r"Software\wire-pod";

pub const WIN_RUN_AT_STARTUP_KEY_KEY: RootKey = RootKey::CurrentUser;
pub const WIN_RUN_AT_STARTUP_PERMS: u32 = KEY_READ | KEY_WRITE;
pub const WIN_RUN_AT_STARTUP_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

pub const NON_INITED_ERROR: &str = "you must run podonwin.Init()";

/// Go's `registry.Key` for the two predefined keys Go names. windows-sys makes
/// `HKEY` a pointer, which would keep `Windows` from being `Send` and `Sync`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootKey {
    CurrentUser,
    LocalMachine,
}

impl RootKey {
    fn hkey(self) -> HKEY {
        match self {
            Self::CurrentUser => HKEY_CURRENT_USER,
            Self::LocalMachine => HKEY_LOCAL_MACHINE,
        }
    }
}

/// Go's `KeyInfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    pub key: RootKey,
    pub perms: u32,
    pub key_path: String,
}

/// Go's package globals `SoftwareKey`, `UninstallKey`, `StartupRunKey` and
/// `IsInstaller`. Go's `Inited` is whether one exists, so the functions below
/// leave that check to whoever holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registry {
    pub software_key: KeyInfo,
    pub uninstall_key: KeyInfo,
    pub startup_run_key: KeyInfo,
    pub is_installer: bool,
}

/// Go's `InitReg`.
pub fn init_reg() -> io::Result<Registry> {
    let is_installer = check_if_elevated();
    let mut software_key = KeyInfo {
        key: WIN_SOFTWARE_KEY_KEY,
        perms: 0,
        key_path: WIN_SOFTWARE_KEY_PATH.to_owned(),
    };
    let mut uninstall_key = KeyInfo {
        key: WIN_UNINSTALL_KEY_KEY,
        perms: 0,
        key_path: WIN_UNINSTALL_KEY_PATH.to_owned(),
    };
    let startup_run_key = KeyInfo {
        key: WIN_RUN_AT_STARTUP_KEY_KEY,
        key_path: WIN_RUN_AT_STARTUP_KEY_PATH.to_owned(),
        perms: WIN_RUN_AT_STARTUP_PERMS,
    };
    if is_installer {
        software_key.perms = WIN_SOFTWARE_INSTALLER_PERMS;
        uninstall_key.perms = WIN_UNINSTALL_INSTALLER_PERMS;
    } else {
        software_key.perms = WIN_SOFTWARE_POD_PERMS;
        uninstall_key.perms = WIN_UNINSTALL_POD_PERMS;
    }
    Ok(Registry {
        software_key,
        uninstall_key,
        startup_run_key,
        is_installer,
    })
}

pub fn delete_everything_from_registry(registry: &Registry) -> io::Result<()> {
    if !registry.is_installer {
        return Err(io::Error::other("must be run from installer"));
    }
    let _ = delete_registry_key(&registry.software_key);
    let _ = delete_registry_key(&registry.uninstall_key);
    let _ = delete_registry_value(&registry.startup_run_key, "wire-pod");
    Ok(())
}

pub fn delete_registry_key(key_info: &KeyInfo) -> io::Result<()> {
    delete_key(key_info.key, &key_info.key_path)
}

pub fn delete_registry_value(key_info: &KeyInfo, key: &str) -> io::Result<()> {
    let k = open_key(key_info.key, &key_info.key_path, key_info.perms)?;
    k.delete_value(key)
}

pub fn update_registry_value_string(key_info: &KeyInfo, key: &str, value: &str) -> io::Result<()> {
    let k = create_key(key_info.key, &key_info.key_path, key_info.perms)?;
    k.set_string_value(key, value)
}

pub fn get_registry_value_string(key_info: &KeyInfo, key: &str) -> io::Result<String> {
    let k = open_key(key_info.key, &key_info.key_path, key_info.perms)?;
    k.get_string_value(key)
}

pub fn update_registry_value_int(key_info: &KeyInfo, key: &str, value: i64) -> io::Result<()> {
    let k = create_key(key_info.key, &key_info.key_path, key_info.perms)?;
    k.set_qword_value(key, value as u64)
}

pub fn get_registry_value_int(key_info: &KeyInfo, key: &str) -> io::Result<i64> {
    let k = open_key(key_info.key, &key_info.key_path, key_info.perms)?;
    let val = k.get_integer_value(key)?;
    Ok(val as i64)
}

pub fn check_if_elevated() -> bool {
    File::open(r"\\.\PHYSICALDRIVE0").is_ok()
}

// What follows stands in for `golang.org/x/sys/windows/registry`.

// windows-sys gates its `RegCreateKeyExW` behind `Win32_Security`, which the
// workspace does not enable, so the call is declared here.
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegCreateKeyExW(
        hkey: HKEY,
        lpsubkey: PCWSTR,
        reserved: u32,
        lpclass: PCWSTR,
        dwoptions: u32,
        samdesired: u32,
        lpsecurityattributes: *const c_void,
        phkresult: *mut HKEY,
        lpdwdisposition: *mut u32,
    ) -> WIN32_ERROR;
}

const ERR_UNEXPECTED_TYPE: &str = "unexpected key value type";

/// An open key, closed on drop.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful open or create.
        unsafe { RegCloseKey(self.0) };
    }
}

fn status(code: WIN32_ERROR) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

/// Go's `syscall.UTF16FromString`, which refuses a NUL inside the string.
fn utf16(s: &str) -> io::Result<Vec<u16>> {
    if s.contains('\0') {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    Ok(s.encode_utf16().chain(Some(0)).collect())
}

fn open_key(root: RootKey, path: &str, access: u32) -> io::Result<Key> {
    let path = utf16(path)?;
    let mut h: HKEY = ptr::null_mut();
    // SAFETY: `path` is NUL-terminated and outlives the call.
    status(unsafe { RegOpenKeyExW(root.hkey(), path.as_ptr(), 0, access, &mut h) })?;
    Ok(Key(h))
}

fn create_key(root: RootKey, path: &str, access: u32) -> io::Result<Key> {
    let path = utf16(path)?;
    let mut h: HKEY = ptr::null_mut();
    let mut disposition = 0u32;
    // SAFETY: `path` is NUL-terminated and outlives the call, and the class
    // and security attributes may be null.
    status(unsafe {
        RegCreateKeyExW(
            root.hkey(),
            path.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE,
            access,
            ptr::null(),
            &mut h,
            &mut disposition,
        )
    })?;
    Ok(Key(h))
}

fn delete_key(root: RootKey, path: &str) -> io::Result<()> {
    let path = utf16(path)?;
    // SAFETY: `path` is NUL-terminated and outlives the call.
    status(unsafe { RegDeleteKeyW(root.hkey(), path.as_ptr()) })
}

impl Key {
    fn delete_value(&self, name: &str) -> io::Result<()> {
        let name = utf16(name)?;
        // SAFETY: `name` is NUL-terminated and outlives the call.
        status(unsafe { RegDeleteValueW(self.0, name.as_ptr()) })
    }

    fn get_value(&self, name: &str, mut buf: Vec<u8>) -> io::Result<(Vec<u8>, u32)> {
        let name = utf16(name)?;
        let mut t = 0u32;
        let mut n = buf.len() as u32;
        loop {
            // SAFETY: `buf` holds `n` writable bytes and `name` is NUL-terminated.
            let code = unsafe {
                RegQueryValueExW(
                    self.0,
                    name.as_ptr(),
                    ptr::null(),
                    &mut t,
                    buf.as_mut_ptr(),
                    &mut n,
                )
            };
            if code == ERROR_SUCCESS {
                buf.truncate(n as usize);
                return Ok((buf, t));
            }
            if code != ERROR_MORE_DATA {
                return Err(io::Error::from_raw_os_error(code as i32));
            }
            if n <= buf.len() as u32 {
                return Err(io::Error::from_raw_os_error(code as i32));
            }
            buf = vec![0; n as usize];
        }
    }

    fn get_string_value(&self, name: &str) -> io::Result<String> {
        let (data, typ) = self.get_value(name, vec![0; 64])?;
        if typ != REG_SZ && typ != REG_EXPAND_SZ {
            return Err(io::Error::other(ERR_UNEXPECTED_TYPE));
        }
        let u: Vec<u16> = data
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|&c| c != 0)
            .collect();
        Ok(String::from_utf16_lossy(&u))
    }

    fn get_integer_value(&self, name: &str) -> io::Result<u64> {
        let (data, typ) = self.get_value(name, vec![0; 8])?;
        match typ {
            REG_DWORD => match <[u8; 4]>::try_from(data.as_slice()) {
                Ok(b) => Ok(u64::from(u32::from_le_bytes(b))),
                Err(_) => Err(io::Error::other("DWORD value is not 4 bytes long")),
            },
            REG_QWORD => match <[u8; 8]>::try_from(data.as_slice()) {
                Ok(b) => Ok(u64::from_le_bytes(b)),
                Err(_) => Err(io::Error::other("QWORD value is not 8 bytes long")),
            },
            _ => Err(io::Error::other(ERR_UNEXPECTED_TYPE)),
        }
    }

    fn set_value(&self, name: &str, valtype: u32, data: &[u8]) -> io::Result<()> {
        let name = utf16(name)?;
        let data_ptr = if data.is_empty() {
            ptr::null()
        } else {
            data.as_ptr()
        };
        // SAFETY: `data_ptr` covers `data.len()` bytes and `name` is
        // NUL-terminated; both outlive the call.
        status(unsafe {
            RegSetValueExW(
                self.0,
                name.as_ptr(),
                0,
                valtype,
                data_ptr,
                data.len() as u32,
            )
        })
    }

    fn set_string_value(&self, name: &str, value: &str) -> io::Result<()> {
        let v = utf16(value)?;
        let buf: Vec<u8> = v.iter().flat_map(|c| c.to_le_bytes()).collect();
        self.set_value(name, REG_SZ, &buf)
    }

    fn set_qword_value(&self, name: &str, value: u64) -> io::Result<()> {
        self.set_value(name, REG_QWORD, &value.to_le_bytes())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    /// A key under `HKCU\Software\wire-pod-rs-test`, deleted on drop.
    struct ScratchKey(KeyInfo);

    impl ScratchKey {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(KeyInfo {
                key: RootKey::CurrentUser,
                perms: WIN_SOFTWARE_POD_PERMS,
                key_path: format!(r"Software\wire-pod-rs-test\{}-{nanos}", std::process::id()),
            })
        }
    }

    impl Drop for ScratchKey {
        fn drop(&mut self) {
            let _ = delete_registry_key(&self.0);
        }
    }

    #[test]
    fn strings_and_ints_round_trip_through_a_scratch_key() {
        let scratch = ScratchKey::new();
        let key = &scratch.0;

        let path = r"C:\Program Files\wire-pod";
        update_registry_value_string(key, "InstallPath", path).unwrap();
        assert_eq!(get_registry_value_string(key, "InstallPath").unwrap(), path);
        let long = "x".repeat(100);
        update_registry_value_string(key, "Long", &long).unwrap();
        assert_eq!(get_registry_value_string(key, "Long").unwrap(), long);

        update_registry_value_int(key, "LastRunningPID", 4242).unwrap();
        assert_eq!(get_registry_value_int(key, "LastRunningPID").unwrap(), 4242);
        let k = open_key(key.key, &key.key_path, key.perms).unwrap();
        assert_eq!(
            k.get_value("LastRunningPID", vec![0; 8]).unwrap().1,
            REG_QWORD
        );

        k.set_value("Dword", REG_DWORD, &7u32.to_le_bytes())
            .unwrap();
        assert_eq!(get_registry_value_int(key, "Dword").unwrap(), 7);
        drop(k);

        let err = get_registry_value_string(key, "LastRunningPID").unwrap_err();
        assert_eq!(err.to_string(), ERR_UNEXPECTED_TYPE);

        delete_registry_value(key, "InstallPath").unwrap();
        let err = get_registry_value_string(key, "InstallPath").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);

        delete_registry_key(key).unwrap();
        let err = get_registry_value_int(key, "LastRunningPID").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn init_reg_names_go_keys_and_permissions() {
        let registry = init_reg().unwrap();
        assert_eq!(registry.software_key.key, RootKey::CurrentUser);
        assert_eq!(registry.software_key.key_path, r"Software\wire-pod");
        assert_eq!(registry.uninstall_key.key, RootKey::LocalMachine);
        assert_eq!(
            registry.startup_run_key.key_path,
            r"Software\Microsoft\Windows\CurrentVersion\Run"
        );
        assert_eq!(registry.startup_run_key.perms, KEY_READ | KEY_WRITE);
        let (software, uninstall) = if registry.is_installer {
            (KEY_ALL_ACCESS, KEY_ALL_ACCESS)
        } else {
            (KEY_READ | KEY_WRITE, KEY_QUERY_VALUE)
        };
        assert_eq!(registry.software_key.perms, software);
        assert_eq!(registry.uninstall_key.perms, uninstall);
    }
}
