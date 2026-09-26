//! Stands in for `github.com/ncruces/zenity`: the message boxes the tray
//! shows, over Win32. Each call blocks until the box is closed, as Go's does.
//!
//! Scaffolding for M6 stage 3. The signatures are the contract `podapp` is
//! written against; the bodies are placeholders that print.

use std::path::Path;

/// The icon options Go passes: `zenity.ErrorIcon`, `zenity.WarningIcon`,
/// `zenity.InfoIcon` and `zenity.Icon(path)`.
#[derive(Clone, Copy, Debug)]
pub enum Icon<'a> {
    Error,
    Warning,
    Info,
    File(&'a Path),
}

/// `zenity.Error(text, icon, zenity.Title(title))`.
pub fn error(text: &str, title: &str, _icon: Icon<'_>) {
    eprintln!("{title}: {text}");
}

/// `zenity.Warning(text, icon, zenity.Title(title))`.
pub fn warning(text: &str, title: &str, _icon: Icon<'_>) {
    eprintln!("{title}: {text}");
}

/// `zenity.Info(text, icon, zenity.Title(title))`.
pub fn info(text: &str, title: &str, _icon: Icon<'_>) {
    println!("{title}: {text}");
}

/// `zenity.Info` with `zenity.ExtraButton(extra)` and `zenity.OKLabel(ok)`.
/// True when the extra button was pressed, which Go reports as
/// `zenity.ErrExtraButton`.
pub fn info_with_extra_button(
    text: &str,
    title: &str,
    _icon: Icon<'_>,
    _extra: &str,
    _ok: &str,
) -> bool {
    println!("{title}: {text}");
    false
}
