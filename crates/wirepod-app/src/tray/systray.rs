//! Stands in for `github.com/getlantern/systray`: the notification-area icon,
//! its tooltip and its menu, over Win32.
//!
//! Scaffolding for M6 stage 3. The signatures are the contract `podapp` is
//! written against; the bodies are placeholders that show nothing.

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

/// `systray.Run`: shows the icon, calls `on_ready` on a thread of its own, and
/// runs the message loop on the calling thread until [`quit`]. `on_exit` runs
/// once the loop has ended.
pub fn run(on_ready: impl FnOnce() + Send + 'static, on_exit: impl FnOnce()) {
    let _ = std::thread::spawn(on_ready).join();
    on_exit();
}

/// `systray.Quit`: ends the message loop [`run`] is running.
pub fn quit() {}

/// `systray.SetIcon`, from the bytes of an `.ico` file.
pub fn set_icon(_icon: &[u8]) {}

/// `systray.SetTitle`.
pub fn set_title(_title: &str) {}

/// `systray.SetTooltip`.
pub fn set_tooltip(_tooltip: &str) {}

/// `systray.AddMenuItem`.
pub fn add_menu_item(_title: &str, _tooltip: &str) -> MenuItem {
    let (_sender, clicked_ch) = unbounded_channel();
    MenuItem { clicked_ch }
}

/// `systray.MenuItem`.
pub struct MenuItem {
    /// Go's `ClickedCh`: one message per click.
    pub clicked_ch: UnboundedReceiver<()>,
}

impl MenuItem {
    /// `MenuItem.Check`.
    pub fn check(&self) {}

    /// `MenuItem.Uncheck`.
    pub fn uncheck(&self) {}

    /// `MenuItem.Checked`.
    pub fn checked(&self) -> bool {
        false
    }
}
