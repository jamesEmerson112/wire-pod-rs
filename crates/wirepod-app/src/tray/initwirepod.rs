//! `cross/podapp/initwirepod.go`, which is `pkg/initwirepod/startserver.go`
//! with the tray's dialogs and tooltips added. Only those additions are
//! translated here. The rest is `serve`'s boot and `startserver.rs`, which
//! reports to [`on_chipper`] through its hook.

use std::fmt::Display;

use tokio_util::sync::CancellationToken;
use wirepod_core::Env;
use wirepod_server::startserver::{self, Event};

use super::all::OsFuncs;
use super::podapp::{
    M_BOX_SUCCESS, M_BOX_TITLE, exit_program, get_needs_setup_msg, m_box_icon, open_browser,
    set_web_port, web_url,
};
use super::systray;
use super::zenity::{self, Icon};
use crate::args::{self, ServeArgs};
use crate::serve::{self, Boot};

/// Go's `NeedsSetupMsg`.
pub fn needs_setup_msg(cross: &dyn OsFuncs) {
    let icon = m_box_icon(cross);
    std::thread::spawn(move || {
        if zenity::info_with_extra_button(
            &get_needs_setup_msg(),
            M_BOX_TITLE,
            Icon::File(&icon),
            "Open browser",
            "OK",
        ) {
            open_browser(&web_url());
        }
    });
}

/// Go's `ErrMsg`.
pub fn err_msg(err: &dyn Display) -> ! {
    zenity::error(
        &format!("wire-pod has run into an issue. The program will now exit. Error details: {err}"),
        M_BOX_TITLE,
        Icon::Error,
    );
    exit_program(1)
}

/// Go's `StartFromProgramInit`: `serve`'s boot, packaged, with the tray's hook
/// installed before the chipper starts.
pub async fn start_from_program_init(cross: &'static dyn OsFuncs, webserver_port: Option<String>) {
    let mut env = Env::from_process();
    env.stt_service = "vosk".to_owned();
    let boot = Boot {
        args: ServeArgs {
            packaged: true,
            ..ServeArgs::default()
        },
        env,
        webserver_port,
        packaged: true,
    };
    let booted = match serve::init(&boot).await {
        Ok(booted) => booted,
        // Go's `vars.Init` has no failure; here APPDATA or the home directory
        // can be missing.
        Err(err) => err_msg(&err),
    };
    set_web_port(booted.web_port.clone());
    startserver::set_hook(Box::new(move |event| on_chipper(cross, event)));

    // A bind failure on the web port has been logged and, packaged, shown in
    // Go's box. Go then exits without `ExitProgram`.
    if serve::start(&boot.args, booted, CancellationToken::new())
        .await
        .is_err()
    {
        std::process::exit(1);
    }
}

/// What the tray's `StartFromProgramInit` and `StartChipper` add to
/// `startserver.go`'s.
fn on_chipper(cross: &dyn OsFuncs, event: Event<'_>) {
    match event {
        Event::NotSetUp => {
            needs_setup_msg(cross);
            systray::set_tooltip(&needs_setup_tooltip(&web_url()));
        }
        Event::Started { from_init } => {
            systray::set_tooltip(&running_tooltip(&web_url()));
            let discrete = args::discrete(std::env::args().nth(1).as_deref());
            if from_init && !discrete {
                let icon = m_box_icon(cross);
                std::thread::spawn(move || {
                    zenity::info(M_BOX_SUCCESS, M_BOX_TITLE, Icon::File(&icon))
                });
            }
        }
        Event::Failed(err) => err_msg(err),
    }
}

fn needs_setup_tooltip(url: &str) -> String {
    format!("wire-pod must be set up at {url}")
}

fn running_tooltip(url: &str) -> String {
    format!("wire-pod is running.\n{url}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tooltips_name_the_web_interface() {
        let url = "http://192.0.2.7:8081";
        assert_eq!(
            needs_setup_tooltip(url),
            "wire-pod must be set up at http://192.0.2.7:8081"
        );
        assert_eq!(
            running_tooltip(url),
            "wire-pod is running.\nhttp://192.0.2.7:8081"
        );
    }
}
