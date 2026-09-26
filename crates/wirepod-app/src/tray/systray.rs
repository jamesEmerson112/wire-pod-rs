//! Stands in for `github.com/getlantern/systray`: the notification-area icon,
//! its tooltip and its menu, over Win32.
//!
//! getlantern makes its Win32 calls on whichever thread calls it. Here every
//! call only updates one process-wide state and posts [`WM_SYNC`] to the
//! hidden window, and the window's thread makes the Win32 changes. That is
//! what lets the setters run before the window exists: [`run`] applies
//! whatever they left.

use std::ffi::c_void;
use std::io;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, TRUE, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateIconFromResourceEx, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW,
    GetCursorPos, GetMessageW, GetSystemMetrics, HICON, InsertMenuItemW, LR_DEFAULTSIZE,
    MENUITEMINFOW, MFS_CHECKED, MFS_UNCHECKED, MFT_STRING, MIIM_FTYPE, MIIM_ID, MIIM_STATE,
    MIIM_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW,
    SM_CXICON, SW_HIDE, SetForegroundWindow, SetMenuItemInfoW, ShowWindow, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, TrackPopupMenu, TranslateMessage, UnregisterClassW, WM_APP, WM_CLOSE,
    WM_COMMAND, WM_DESTROY, WM_ENDSESSION, WM_LBUTTONUP, WM_RBUTTONUP, WM_USER, WNDCLASSEXW,
    WS_OVERLAPPEDWINDOW,
};

/// getlantern's `nid.ID`.
const NOTIFY_ID: u32 = 100;
/// getlantern's `wmSystrayMessage`, the icon's callback message.
const WM_TRAY: u32 = WM_USER + 1;
/// Asks the window's thread to apply the state. No getlantern counterpart.
const WM_SYNC: u32 = WM_APP + 1;
const CLASS_NAME: &str = "SystrayClass";
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// getlantern's `wmTaskbarCreated`, which Explorer broadcasts when it restarts.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

static STATE: Mutex<State> = Mutex::new(State::new());

/// A window, menu or icon handle, kept in [`STATE`].
#[derive(Clone, Copy)]
struct Handle(*mut c_void);

// SAFETY: these handles are identifiers the system resolves in any thread of
// the process. The state only stores them, and only the window's thread uses
// them, apart from `PostMessageW`, which any thread may call.
unsafe impl Send for Handle {}

impl Handle {
    const NULL: Handle = Handle(ptr::null_mut());

    fn is_null(self) -> bool {
        self.0.is_null()
    }
}

struct Item {
    id: u32,
    title: String,
    checked: bool,
    clicked: UnboundedSender<()>,
}

struct State {
    // Written by any thread.
    tooltip: String,
    icon_bytes: Vec<u8>,
    icon_generation: u64,
    items: Vec<Item>,
    next_id: u32,
    quit_requested: bool,
    // Owned by the window's thread.
    window: Handle,
    menu: Handle,
    icon: Handle,
    applied_icon_generation: u64,
    inserted_items: usize,
    icon_added: bool,
}

impl State {
    const fn new() -> Self {
        Self {
            tooltip: String::new(),
            icon_bytes: Vec::new(),
            icon_generation: 0,
            items: Vec::new(),
            next_id: 0,
            quit_requested: false,
            window: Handle::NULL,
            menu: Handle::NULL,
            icon: Handle::NULL,
            applied_icon_generation: 0,
            inserted_items: 0,
            icon_added: false,
        }
    }

    /// Wakes the window's thread, if there is a window yet.
    fn post_sync(&self) {
        if !self.window.is_null() {
            // SAFETY: posting to a window handle that may since have been
            // destroyed fails harmlessly.
            unsafe { PostMessageW(self.window.0, WM_SYNC, 0, 0) };
        }
    }
}

fn state() -> MutexGuard<'static, State> {
    // The window procedure must not panic, so a poisoned lock is used as it is.
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `systray.Run`: shows the icon, calls `on_ready` on a thread of its own, and
/// runs the message loop on the calling thread until [`quit`]. `on_exit` runs
/// once the loop has ended.
///
/// getlantern calls `onExit` from the window procedure, on `WM_DESTROY` and on
/// `WM_ENDSESSION`. Here both end the loop, and `on_exit` runs after it.
pub fn run(on_ready: impl FnOnce() + Send + 'static, on_exit: impl FnOnce()) {
    // getlantern logs a failure here and never calls `onReady`, then waits in
    // its message loop forever. This returns and calls `on_exit` instead.
    match register() {
        Ok(()) => {
            let spawned = std::thread::Builder::new()
                .name("systray-ready".to_owned())
                .spawn(on_ready);
            if let Err(err) = spawned {
                tracing::error!("systray: unable to start the ready callback: {err}");
            }
            native_loop();
        }
        Err(err) => tracing::error!("systray: unable to init instance: {err}"),
    }
    unregister();
    on_exit();
}

/// `systray.Quit`: ends the message loop [`run`] is running.
pub fn quit() {
    let mut st = state();
    // Go's `quitOnce`.
    if st.quit_requested {
        return;
    }
    st.quit_requested = true;
    // Before the window exists, `run` posts this once it has created it. Go
    // posts to window 0 and the request is lost.
    if !st.window.is_null() {
        // SAFETY: as in `State::post_sync`.
        unsafe { PostMessageW(st.window.0, WM_CLOSE, 0, 0) };
    }
}

/// `systray.SetIcon`, from the bytes of an `.ico` file.
///
/// getlantern writes the bytes to a file in the temporary directory and loads
/// that. Here they are read in memory, at the same default icon size.
pub fn set_icon(icon: &[u8]) {
    let mut st = state();
    st.icon_bytes = icon.to_vec();
    st.icon_generation += 1;
    st.post_sync();
}

/// `systray.SetTitle`, which getlantern does not implement on Windows.
pub fn set_title(_title: &str) {}

/// `systray.SetTooltip`.
pub fn set_tooltip(tooltip: &str) {
    let mut st = state();
    st.tooltip = tooltip.to_owned();
    st.post_sync();
}

/// `systray.AddMenuItem`. As in getlantern on Windows, the tooltip is not
/// shown.
pub fn add_menu_item(title: &str, _tooltip: &str) -> MenuItem {
    let (clicked, clicked_ch) = unbounded_channel();
    let mut st = state();
    st.next_id += 1;
    let id = st.next_id;
    st.items.push(Item {
        id,
        title: title.to_owned(),
        checked: false,
        clicked,
    });
    st.post_sync();
    MenuItem { clicked_ch, id }
}

/// `systray.MenuItem`.
pub struct MenuItem {
    /// Go's `ClickedCh`: one message per click.
    ///
    /// Go's channel is unbuffered and a click nobody is waiting for is
    /// dropped. This one queues it.
    pub clicked_ch: UnboundedReceiver<()>,
    id: u32,
}

impl MenuItem {
    /// `MenuItem.Check`.
    pub fn check(&self) {
        set_checked(self.id, true);
    }

    /// `MenuItem.Uncheck`.
    pub fn uncheck(&self) {
        set_checked(self.id, false);
    }

    /// `MenuItem.Checked`.
    pub fn checked(&self) -> bool {
        state()
            .items
            .iter()
            .find(|item| item.id == self.id)
            .is_some_and(|item| item.checked)
    }
}

fn set_checked(id: u32, checked: bool) {
    let mut st = state();
    if let Some(item) = st.items.iter_mut().find(|item| item.id == id) {
        item.checked = checked;
    }
    st.post_sync();
}

/// getlantern's `initInstance` and `createMenu`: the hidden window, the
/// notification icon and the empty popup menu.
fn register() -> io::Result<()> {
    // SAFETY: a null name asks for the executable's own module.
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    if instance.is_null() {
        return Err(io::Error::last_os_error());
    }

    let taskbar_created = to_wide("TaskbarCreated");
    // SAFETY: the name is a terminated UTF-16 string.
    let message = unsafe { RegisterWindowMessageW(taskbar_created.as_ptr()) };
    TASKBAR_CREATED.store(message, Ordering::Relaxed);

    let class_name = to_wide(CLASS_NAME);
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        lpszClassName: class_name.as_ptr(),
        // SAFETY: every other field of this plain C struct may be zero.
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: the class points at a terminated name and a valid procedure.
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(io::Error::last_os_error());
    }

    // The menu comes first so that a failure leaves no window behind. From
    // here on, `run` cleans up through `unregister`.
    // SAFETY: no arguments.
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return Err(io::Error::last_os_error());
    }
    state().menu = Handle(menu);

    let window_name = to_wide("");
    // SAFETY: the class is registered above and the names are terminated.
    let window = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            window_name.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        )
    };
    if window.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the window was created above, on this thread.
    unsafe { ShowWindow(window, SW_HIDE) };

    let quit_requested = {
        let mut st = state();
        st.window = Handle(window);
        st.quit_requested
    };
    sync(window);
    if quit_requested {
        // SAFETY: as for `ShowWindow`.
        unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
    }
    Ok(())
}

/// What getlantern leaves to the process exit, and its class unregistration.
fn unregister() {
    let (menu, icon) = {
        let mut st = state();
        st.window = Handle::NULL;
        st.icon_added = false;
        (
            std::mem::replace(&mut st.menu, Handle::NULL),
            std::mem::replace(&mut st.icon, Handle::NULL),
        )
    };
    // SAFETY: the handles were created by this module and are no longer in
    // the state, so nothing else uses them.
    unsafe {
        if !menu.is_null() {
            DestroyMenu(menu.0);
        }
        if !icon.is_null() {
            DestroyIcon(icon.0);
        }
        let class_name = to_wide(CLASS_NAME);
        UnregisterClassW(class_name.as_ptr(), GetModuleHandleW(ptr::null()));
    }
}

/// getlantern's `nativeLoop`.
fn native_loop() {
    // SAFETY: `MSG` is a plain C struct.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `message` is a valid out-pointer for the whole loop.
        match unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } {
            -1 => {
                let err = io::Error::last_os_error();
                tracing::error!("systray: error at message loop: {err}");
                return;
            }
            0 => return,
            _ => {
                // SAFETY: `message` was filled in by `GetMessageW`.
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }
}

/// getlantern's `wndProc`. Nothing here may hold the state's lock across a
/// call that can re-enter this procedure, such as `TrackPopupMenu` or
/// `DestroyWindow`.
unsafe extern "system" fn wndproc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND => {
            if wparam as i32 != -1 {
                menu_item_selected(wparam as u32);
            }
        }
        // getlantern calls `onExit` on `WM_ENDSESSION` without ending the
        // loop. Destroying the window ends it, and `run` calls `on_exit`.
        WM_CLOSE | WM_ENDSESSION => {
            // SAFETY: the window is this procedure's own, on its own thread.
            unsafe { DestroyWindow(window) };
        }
        WM_DESTROY => {
            remove_icon(window);
            // SAFETY: no pointers.
            unsafe { PostQuitMessage(0) };
        }
        WM_TRAY => {
            if matches!(lparam as u32, WM_LBUTTONUP | WM_RBUTTONUP) {
                show_menu(window);
            }
        }
        WM_SYNC => sync(window),
        _ if message != 0 && message == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            state().icon_added = false;
            sync(window);
        }
        // SAFETY: the arguments are the ones this procedure was called with.
        _ => return unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
    0
}

/// getlantern's `systrayMenuItemSelected`.
fn menu_item_selected(id: u32) {
    let st = state();
    match st.items.iter().find(|item| item.id == id) {
        // A receiver that has been dropped is not an error.
        Some(item) => {
            let _ = item.clicked.send(());
        }
        None => tracing::error!("systray: no menu item with ID {id}"),
    }
}

/// getlantern's `showMenu`.
fn show_menu(window: HWND) {
    let menu = state().menu;
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `point` is a valid out-pointer; the window and the menu belong
    // to this thread's tray.
    unsafe {
        if GetCursorPos(&mut point) == 0 {
            return;
        }
        SetForegroundWindow(window);
        TrackPopupMenu(
            menu.0,
            TPM_BOTTOMALIGN | TPM_LEFTALIGN,
            point.x,
            point.y,
            0,
            window,
            ptr::null(),
        );
    }
}

/// Applies the state to the icon and the menu, on the window's thread.
fn sync(window: HWND) {
    let (icon_bytes, new_items, checks, menu) = {
        let mut st = state();
        let icon_bytes = (st.icon_generation != st.applied_icon_generation).then(|| {
            st.applied_icon_generation = st.icon_generation;
            st.icon_bytes.clone()
        });
        let new_items: Vec<(u32, Vec<u16>, bool)> = st.items[st.inserted_items..]
            .iter()
            .map(|item| (item.id, to_wide(&item.title), item.checked))
            .collect();
        let checks: Vec<(u32, bool)> = st.items[..st.inserted_items]
            .iter()
            .map(|item| (item.id, item.checked))
            .collect();
        st.inserted_items = st.items.len();
        (icon_bytes, new_items, checks, st.menu)
    };

    let mut retired = Handle::NULL;
    if let Some(bytes) = icon_bytes {
        match icon_from_bytes(&bytes) {
            Ok(icon) => retired = std::mem::replace(&mut state().icon, Handle(icon)),
            Err(err) => tracing::error!("systray: unable to set icon: {err}"),
        }
    }

    let (data, added) = {
        let st = state();
        (notify_icon_data(window, &st), st.icon_added)
    };
    let command = if added { NIM_MODIFY } else { NIM_ADD };
    // SAFETY: `data` is a filled-in `NOTIFYICONDATAW` for this window.
    if unsafe { Shell_NotifyIconW(command, &data) } != 0 {
        state().icon_added = true;
    } else if added {
        tracing::warn!("systray: unable to update the icon");
    } else {
        // Explorer may not be up yet. `TaskbarCreated` adds the icon then,
        // where getlantern gives up and never calls `onReady`.
        tracing::warn!("systray: unable to add the icon; waiting for the taskbar");
    }
    if !retired.is_null() {
        // SAFETY: the old icon is out of the state and the shell has the new one.
        unsafe { DestroyIcon(retired.0) };
    }

    let position = checks.len();
    for (offset, (id, title, checked)) in new_items.into_iter().enumerate() {
        let mut info = menu_item_info(id, checked);
        info.fMask |= MIIM_FTYPE | MIIM_STRING | MIIM_ID;
        info.fType = MFT_STRING;
        info.dwTypeData = title.as_ptr().cast_mut();
        info.cch = (title.len() - 1) as u32;
        // SAFETY: `info` points at `title`, which outlives the call.
        if unsafe { InsertMenuItemW(menu.0, (position + offset) as u32, TRUE, &info) } == 0 {
            let err = io::Error::last_os_error();
            tracing::error!("systray: unable to addOrUpdateMenuItem: {err}");
        }
    }
    for (id, checked) in checks {
        let info = menu_item_info(id, checked);
        // SAFETY: `info` is a filled-in `MENUITEMINFOW` without pointers.
        unsafe { SetMenuItemInfoW(menu.0, id, 0, &info) };
    }
}

/// getlantern's `nid.delete`, on `WM_DESTROY`.
fn remove_icon(window: HWND) {
    let data = {
        let mut st = state();
        st.window = Handle::NULL;
        st.icon_added = false;
        notify_icon_data(window, &st)
    };
    // SAFETY: `data` is a filled-in `NOTIFYICONDATAW` for this window.
    unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
}

fn notify_icon_data(window: HWND, st: &State) -> NOTIFYICONDATAW {
    // SAFETY: `NOTIFYICONDATAW` is a plain C struct.
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = window;
    data.uID = NOTIFY_ID;
    data.uFlags = NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    if !st.icon.is_null() {
        data.uFlags |= NIF_ICON;
        data.hIcon = st.icon.0;
    }
    // Filled apart, as the struct is packed on 32-bit Windows.
    let mut tip = [0; 128];
    copy_wide(&mut tip, &st.tooltip);
    data.szTip = tip;
    data
}

fn menu_item_info(id: u32, checked: bool) -> MENUITEMINFOW {
    // SAFETY: `MENUITEMINFOW` is a plain C struct.
    let mut info: MENUITEMINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<MENUITEMINFOW>() as u32;
    info.fMask = MIIM_STATE;
    info.fState = if checked { MFS_CHECKED } else { MFS_UNCHECKED };
    info.wID = id;
    info
}

/// An icon from the bytes of an `.ico` file, or of a PNG, which zenity's
/// custom icons accept. It is made at the default icon size, as getlantern's
/// `LoadImage` and zenity's `CreateIconFromResourceEx` make theirs.
pub(crate) fn icon_from_bytes(bytes: &[u8]) -> io::Result<HICON> {
    let image = if bytes.starts_with(PNG_SIGNATURE) {
        bytes
    } else {
        // SAFETY: no pointers.
        let want = unsafe { GetSystemMetrics(SM_CXICON) }.max(1) as u32;
        ico_image(bytes, want)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not an .ico file"))?
    };
    let len = u32::try_from(image.len()).map_err(|_| io::Error::other("icon too large"))?;
    // SAFETY: `image` is `len` readable bytes.
    let icon = unsafe {
        CreateIconFromResourceEx(image.as_ptr(), len, TRUE, 0x0003_0000, 0, 0, LR_DEFAULTSIZE)
    };
    if icon.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(icon)
    }
}

/// The image in an `.ico` file that best fits `want` pixels: the smallest at
/// least that wide, else the widest, and then the one with the most colour
/// bits. `None` when the bytes are not an icon file.
fn ico_image(ico: &[u8], want: u32) -> Option<&[u8]> {
    let word = |at: usize| Some(u16::from_le_bytes([*ico.get(at)?, *ico.get(at + 1)?]));
    let dword = |at: usize| Some(u32::from_le_bytes(ico.get(at..at + 4)?.try_into().ok()?));
    if word(0)? != 0 || word(2)? != 1 {
        return None;
    }
    let images = (0..usize::from(word(4)?))
        .map(|index| {
            let entry = 6 + 16 * index;
            let width = match *ico.get(entry)? {
                0 => 256,
                width => u32::from(width),
            };
            let len = dword(entry + 8)? as usize;
            let offset = dword(entry + 12)? as usize;
            let image = ico.get(offset..offset.checked_add(len)?)?;
            Some((width, word(entry + 6)?, image))
        })
        .collect::<Option<Vec<_>>>()?;
    images
        .into_iter()
        .max_by_key(|&(width, bits, _)| {
            let big_enough = width >= want;
            (
                big_enough,
                if big_enough { u32::MAX - width } else { width },
                bits,
            )
        })
        .map(|(_, _, image)| image)
}

/// `s` as a terminated UTF-16 string.
pub(crate) fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies `s` into a fixed UTF-16 buffer, cut short to leave room for the
/// terminator. getlantern copies without one when the text fills the buffer.
fn copy_wide(buffer: &mut [u16], s: &str) {
    let room = buffer.len().saturating_sub(1);
    let mut len = 0;
    for (slot, unit) in buffer.iter_mut().zip(s.encode_utf16().take(room)) {
        *slot = unit;
        len += 1;
    }
    if let Some(terminator) = buffer.get_mut(len) {
        *terminator = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_are_terminated_and_tooltips_cut_short() {
        assert_eq!(to_wide("OK"), [u16::from(b'O'), u16::from(b'K'), 0]);
        assert_eq!(to_wide(""), [0]);
        assert_eq!(to_wide("é"), [0xE9, 0]);

        let mut tip = [0xFFFF_u16; 4];
        copy_wide(&mut tip, "ab");
        assert_eq!(tip, [u16::from(b'a'), u16::from(b'b'), 0, 0xFFFF]);
        copy_wide(&mut tip, "wire-pod");
        assert_eq!(tip, [u16::from(b'w'), u16::from(b'i'), u16::from(b'r'), 0]);
    }

    #[test]
    fn ico_image_picks_the_best_fit() {
        let tray = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/pod24x24.ico"
        ))
        .expect("the tray icon is in resources");
        let image = ico_image(&tray, 32).expect("one 24 px image");
        assert_eq!(image.len(), 2440);
        assert_eq!(&image[..4], &40u32.to_le_bytes(), "a BITMAPINFOHEADER");

        let large = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/pod256x256.ico"
        ))
        .expect("the 256 px icon is in resources");
        assert!(
            ico_image(&large, 32)
                .expect("one image")
                .starts_with(PNG_SIGNATURE)
        );

        // Two images, 16 px at byte 38 and 48 px at byte 40, two bytes each.
        let mut ico = vec![0, 0, 1, 0, 2, 0];
        for (width, offset) in [(16u8, 38u32), (48, 40)] {
            ico.extend([width, width, 0, 0, 1, 0, 32, 0]);
            ico.extend(2u32.to_le_bytes());
            ico.extend(offset.to_le_bytes());
        }
        ico.extend([0x16, 0x16, 0x48, 0x48]);
        assert_eq!(ico_image(&ico, 16), Some(&[0x16, 0x16][..]));
        assert_eq!(ico_image(&ico, 32), Some(&[0x48, 0x48][..]));
        assert_eq!(ico_image(&ico, 64), Some(&[0x48, 0x48][..]));
        assert_eq!(ico_image(&ico[..41], 32), None, "an image cut short");
        assert_eq!(ico_image(PNG_SIGNATURE, 32), None);
    }

    #[test]
    fn menu_items_keep_their_check_state_without_a_window() {
        let item = add_menu_item("Run On Startup", "");
        assert!(!item.checked());
        item.check();
        assert!(item.checked());
        item.uncheck();
        assert!(!item.checked());

        let other = add_menu_item("About", "About WirePod");
        other.check();
        assert!(!item.checked(), "each item has its own state");
    }
}
