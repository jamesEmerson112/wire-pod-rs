//! Stands in for `github.com/ncruces/zenity`: the message boxes the tray
//! shows, over Win32. Each call blocks until the box is closed, as Go's does.
//!
//! zenity shows every one of these as a `MessageBoxW`, and hooks the box to
//! relabel its buttons and swap in a custom icon. Here a custom icon or an
//! extra button uses a task dialog instead, which needs the Common Controls 6
//! manifest that `build.rs` embeds.

use std::io;
use std::path::Path;
use std::ptr;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, S_OK, WPARAM};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOG_NOTIFICATIONS, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0,
    TD_ERROR_ICON, TD_INFORMATION_ICON, TD_WARNING_ICON, TDCBF_OK_BUTTON,
    TDF_ALLOW_DIALOG_CANCELLATION, TDF_USE_HICON_MAIN, TDN_CREATED,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, HICON, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND,
    MESSAGEBOX_STYLE, MessageBoxW, SetForegroundWindow,
};
use windows_sys::core::{HRESULT, PCWSTR};

use super::systray::{icon_from_bytes, to_wide};

/// The task dialog's id for the OK button of [`info_with_extra_button`].
const BUTTON_OK: i32 = 100;
/// The task dialog's id for the extra button of [`info_with_extra_button`].
const BUTTON_EXTRA: i32 = 101;

/// The icon options Go passes: `zenity.ErrorIcon`, `zenity.WarningIcon`,
/// `zenity.InfoIcon` and `zenity.Icon(path)`.
#[derive(Clone, Copy, Debug)]
pub enum Icon<'a> {
    Error,
    Warning,
    // zenity's `InfoIcon`; the tray passes its own icon file instead.
    #[allow(dead_code)]
    Info,
    /// A `.ico` file, or a PNG, which is what Go passes.
    File(&'a Path),
}

/// zenity's `messageKind`, which picks the icon when the options give none
/// that Win32 has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Error,
    Warning,
    Info,
}

/// `zenity.Error(text, icon, zenity.Title(title))`.
pub fn error(text: &str, title: &str, icon: Icon<'_>) {
    message(Kind::Error, text, title, icon);
}

/// `zenity.Warning(text, icon, zenity.Title(title))`.
pub fn warning(text: &str, title: &str, icon: Icon<'_>) {
    message(Kind::Warning, text, title, icon);
}

/// `zenity.Info(text, icon, zenity.Title(title))`.
pub fn info(text: &str, title: &str, icon: Icon<'_>) {
    message(Kind::Info, text, title, icon);
}

/// `zenity.Info` with `zenity.ExtraButton(extra)` and `zenity.OKLabel(ok)`.
/// True when the extra button was pressed, which Go reports as
/// `zenity.ErrExtraButton`.
///
/// Without a task dialog this shows a plain information box and returns
/// false.
pub fn info_with_extra_button(
    text: &str,
    title: &str,
    icon: Icon<'_>,
    extra: &str,
    ok: &str,
) -> bool {
    // zenity shows no box at all when a custom icon will not load. Here the
    // dialog keeps its buttons and shows the information icon instead.
    let loaded = match icon {
        Icon::File(path) => load_icon(path).ok(),
        _ => None,
    };
    let main_icon = match (loaded, icon) {
        (Some(handle), _) => MainIcon::Handle(handle),
        (None, icon) => MainIcon::Standard(standard_icon(Kind::Info, icon)),
    };
    let pressed = task_dialog(
        text,
        title,
        main_icon,
        &[(BUTTON_OK, ok), (BUTTON_EXTRA, extra)],
    );
    if let Some(handle) = loaded {
        // SAFETY: the icon was made by `load_icon` and the dialog has closed.
        unsafe { DestroyIcon(handle) };
    }
    match pressed {
        Some(button) => extra_pressed(button),
        None => {
            message_box(text, title, message_box_style(Kind::Info, icon));
            false
        }
    }
}

/// zenity's `message`.
fn message(kind: Kind, text: &str, title: &str, icon: Icon<'_>) {
    if let Icon::File(path) = icon {
        // zenity shows no box at all when the icon will not load. Here that,
        // or a missing task dialog, falls back to the plain box.
        if let Ok(handle) = load_icon(path) {
            let shown = task_dialog(text, title, MainIcon::Handle(handle), &[]).is_some();
            // SAFETY: the icon was made by `load_icon` and the dialog has closed.
            unsafe { DestroyIcon(handle) };
            if shown {
                return;
            }
        }
    }
    message_box(text, title, message_box_style(kind, icon));
}

fn message_box(text: &str, title: &str, style: MESSAGEBOX_STYLE) {
    let text = to_wide(text);
    let title = to_wide(title);
    // SAFETY: both strings are terminated and outlive the call; the box has
    // no owner, as zenity's has none here.
    unsafe { MessageBoxW(ptr::null_mut(), text.as_ptr(), title.as_ptr(), style) };
}

/// zenity's flags for a box with only an OK button: the icon asked for, or
/// the kind's own when that is a custom one.
fn message_box_style(kind: Kind, icon: Icon<'_>) -> MESSAGEBOX_STYLE {
    let icon = match (icon, kind) {
        (Icon::Error, _) | (Icon::File(_), Kind::Error) => MB_ICONERROR,
        (Icon::Warning, _) | (Icon::File(_), Kind::Warning) => MB_ICONWARNING,
        (Icon::Info, _) | (Icon::File(_), Kind::Info) => MB_ICONINFORMATION,
    };
    MB_SETFOREGROUND | MB_OK | icon
}

/// The task dialog's version of [`message_box_style`]'s icon.
fn standard_icon(kind: Kind, icon: Icon<'_>) -> PCWSTR {
    match (icon, kind) {
        (Icon::Error, _) | (Icon::File(_), Kind::Error) => TD_ERROR_ICON,
        (Icon::Warning, _) | (Icon::File(_), Kind::Warning) => TD_WARNING_ICON,
        (Icon::Info, _) | (Icon::File(_), Kind::Info) => TD_INFORMATION_ICON,
    }
}

/// zenity's `getIcon` for a path: a PNG or an `.ico` file, at the default
/// icon size.
fn load_icon(path: &Path) -> io::Result<HICON> {
    icon_from_bytes(&std::fs::read(path)?)
}

enum MainIcon {
    Handle(HICON),
    Standard(PCWSTR),
}

/// Shows a task dialog and returns the id of the button that closed it.
/// `None` when comctl32 has no task dialog, which is the case without the
/// Common Controls 6 manifest, or when the dialog fails.
///
/// With no `buttons` the dialog has one OK button, which Escape and the
/// close box also answer, as a box with `MB_OK` does. With buttons it can
/// only be closed through them, as zenity's `MB_YESNO` box can.
fn task_dialog(
    text: &str,
    title: &str,
    main_icon: MainIcon,
    buttons: &[(i32, &str)],
) -> Option<i32> {
    let task_dialog_indirect = task_dialog_indirect()?;
    let text = to_wide(text);
    let title = to_wide(title);
    let labels: Vec<Vec<u16>> = buttons
        .iter()
        .map(|(_, label)| to_wide(&quote_accelerators(label)))
        .collect();
    let buttons: Vec<TASKDIALOG_BUTTON> = buttons
        .iter()
        .zip(&labels)
        .map(|((id, _), label)| TASKDIALOG_BUTTON {
            nButtonID: *id,
            pszButtonText: label.as_ptr(),
        })
        .collect();

    // SAFETY: `TASKDIALOGCONFIG` is a plain C struct. It is packed, so its
    // fields are only ever assigned, never borrowed.
    let mut config: TASKDIALOGCONFIG = unsafe { std::mem::zeroed() };
    config.cbSize = size_of::<TASKDIALOGCONFIG>() as u32;
    config.pszWindowTitle = title.as_ptr();
    config.pszContent = text.as_ptr();
    config.pfCallback = Some(on_notification);
    match main_icon {
        MainIcon::Handle(handle) => {
            config.dwFlags |= TDF_USE_HICON_MAIN;
            config.Anonymous1 = TASKDIALOGCONFIG_0 { hMainIcon: handle };
        }
        MainIcon::Standard(resource) => {
            config.Anonymous1 = TASKDIALOGCONFIG_0 {
                pszMainIcon: resource,
            };
        }
    }
    match buttons.first() {
        None => {
            config.dwCommonButtons = TDCBF_OK_BUTTON;
            config.dwFlags |= TDF_ALLOW_DIALOG_CANCELLATION;
        }
        Some(first) => {
            config.cButtons = buttons.len() as u32;
            config.pButtons = buttons.as_ptr();
            config.nDefaultButton = first.nButtonID;
        }
    }

    let mut pressed = 0;
    // SAFETY: every pointer in `config` points at a local that outlives the
    // call, and `pressed` is a valid out-pointer.
    let result =
        unsafe { task_dialog_indirect(&config, &mut pressed, ptr::null_mut(), ptr::null_mut()) };
    (result >= 0).then_some(pressed)
}

/// Brings the dialog to the front, as `MB_SETFOREGROUND` does for the box.
unsafe extern "system" fn on_notification(
    window: HWND,
    notification: TASKDIALOG_NOTIFICATIONS,
    _wparam: WPARAM,
    _lparam: LPARAM,
    _data: isize,
) -> HRESULT {
    if notification == TDN_CREATED {
        // SAFETY: the window is the dialog that was just created.
        unsafe { SetForegroundWindow(window) };
    }
    S_OK
}

type TaskDialogIndirect =
    unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut BOOL) -> HRESULT;

/// `TaskDialogIndirect`, looked up at run time. Linking it would stop the
/// program from starting at all where comctl32 is older than version 6.
fn task_dialog_indirect() -> Option<TaskDialogIndirect> {
    static FUNCTION: OnceLock<Option<TaskDialogIndirect>> = OnceLock::new();
    *FUNCTION.get_or_init(|| {
        let name = to_wide("comctl32.dll");
        // SAFETY: the name is a terminated UTF-16 string. The library stays
        // loaded for the life of the process.
        let module = unsafe { LoadLibraryW(name.as_ptr()) };
        if module.is_null() {
            return None;
        }
        // SAFETY: the name is a terminated byte string.
        let function = unsafe { GetProcAddress(module, c"TaskDialogIndirect".as_ptr().cast()) }?;
        // SAFETY: comctl32 exports `TaskDialogIndirect` with this signature.
        Some(unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, TaskDialogIndirect>(
                function,
            )
        })
    })
}

/// True when the button that closed [`info_with_extra_button`]'s dialog is
/// the extra one.
fn extra_pressed(button: i32) -> bool {
    button == BUTTON_EXTRA
}

/// zenity's `quoteAccelerators`: a button label shows `&` as itself.
fn quote_accelerators(text: &str) -> String {
    text.replace('&', "&&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_extra_button_counts_as_extra() {
        assert!(extra_pressed(BUTTON_EXTRA));
        assert!(!extra_pressed(BUTTON_OK));
        // IDCANCEL, and the zero a failed dialog leaves.
        assert!(!extra_pressed(2));
        assert!(!extra_pressed(0));
        assert_eq!(quote_accelerators("Open & go"), "Open && go");
    }

    #[test]
    fn a_custom_icon_falls_back_to_the_kinds_own() {
        let path = Path::new("icons/png/podfull.png");
        assert_eq!(
            message_box_style(Kind::Info, Icon::File(path)),
            MB_SETFOREGROUND | MB_OK | MB_ICONINFORMATION
        );
        assert_eq!(
            message_box_style(Kind::Error, Icon::File(path)),
            MB_SETFOREGROUND | MB_OK | MB_ICONERROR
        );
        assert_eq!(
            message_box_style(Kind::Info, Icon::Warning),
            MB_SETFOREGROUND | MB_OK | MB_ICONWARNING
        );
        assert_eq!(
            standard_icon(Kind::Info, Icon::File(path)),
            TD_INFORMATION_ICON
        );
    }
}
