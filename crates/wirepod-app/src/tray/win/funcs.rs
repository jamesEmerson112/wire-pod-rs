//! `cross/win/funcs.go`.
//!
//! Scaffolding for M6 stage 3: [`Windows`] exists so `podapp` can be written
//! against it, and every call fails until the translation replaces this file.

use std::io;
use std::path::PathBuf;

use crate::tray::all::{OsFuncs, WpConfig};

const NOT_YET: &str = "cross/win is not translated yet";

/// Go's `Windows`.
pub struct Windows {}

impl Windows {
    /// Go's `NewWindows`.
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for Windows {
    fn default() -> Self {
        Self::new()
    }
}

impl OsFuncs for Windows {
    fn init(&mut self) -> io::Result<()> {
        Err(io::Error::other(NOT_YET))
    }

    fn read_config(&self) -> io::Result<WpConfig> {
        Err(io::Error::other(NOT_YET))
    }

    fn run_pod_at_startup(&self, _run: bool) -> io::Result<()> {
        Err(io::Error::other(NOT_YET))
    }

    fn write_config(&self, _config: &WpConfig) -> io::Result<()> {
        Err(io::Error::other(NOT_YET))
    }

    fn is_pod_already_running(&self) -> bool {
        false
    }

    fn is_pid_process_running(&self, _pid: u32) -> io::Result<bool> {
        Err(io::Error::other(NOT_YET))
    }

    fn kill_existing_pod(&self) -> io::Result<()> {
        Err(io::Error::other(NOT_YET))
    }

    fn resources_path(&self) -> PathBuf {
        PathBuf::from("./")
    }

    fn hostname(&self) -> String {
        String::new()
    }

    fn on_exit(&self) {}
}
