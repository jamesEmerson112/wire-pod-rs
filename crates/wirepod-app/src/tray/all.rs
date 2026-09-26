//! `cross/all/all.go`: the settings the tray keeps between runs, and the
//! operating-system interface it is written against.

use std::io;
use std::path::PathBuf;

/// Go's `WPConfig`. On Windows it lives in the registry, so Go's JSON tags
/// have no counterpart.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WpConfig {
    pub ws_port: String,
    pub run_at_startup: bool,
    pub install_path: String,
    pub version: String,
    // if NeedsRestart && hostname != escapepod; then error
    pub needs_restart: bool,
    /// Go's `int`; a Windows process id is a `u32`.
    pub last_running_pid: u32,
    pub first_startup: bool,
    pub no_pod_warn: bool,
}

/// Go's `OSFuncs`.
pub trait OsFuncs: Send + Sync {
    fn init(&mut self) -> io::Result<()>;
    fn read_config(&self) -> io::Result<WpConfig>;
    fn run_pod_at_startup(&self, run: bool) -> io::Result<()>;
    fn write_config(&self, config: &WpConfig) -> io::Result<()>;
    fn is_pod_already_running(&self) -> bool;
    fn is_pid_process_running(&self, pid: u32) -> io::Result<bool>;
    fn kill_existing_pod(&self) -> io::Result<()>;
    fn resources_path(&self) -> PathBuf;
    fn hostname(&self) -> String;
    fn on_exit(&self);
}
