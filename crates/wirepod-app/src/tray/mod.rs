//! The Windows tray shell, translated from the WirePod repo
//! (`E:/GitHub/WirePod`), which builds the installed `chipper.exe` around the
//! `chipper` module.
//!
//! `systray` and `zenity` stand in for the two Go libraries the shell calls,
//! `github.com/getlantern/systray` and `github.com/ncruces/zenity`, over Win32.

// Without the `tray` feature nothing calls into the tray, but it still
// compiles, so the default build and CI run its tests.
#![cfg_attr(not(feature = "tray"), allow(dead_code))]

pub mod all;
pub mod initwirepod;
pub mod podapp;
pub mod systray;
pub mod win;
pub mod zenity;
