//! `cross/podapp/main.go`: the tray's entry point, menu and dialogs.

use std::backtrace::Backtrace;
use std::io;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use wirepod_core::paths::POD_NAME;

use super::all::{OsFuncs, WpConfig};
use super::initwirepod::{self, err_msg};
use super::systray::{self, MenuItem};
use super::zenity::{self, Icon};

/// Go's `cross`.
static CROSS: OnceLock<Box<dyn OsFuncs>> = OnceLock::new();

/// Go's `vars.WebPort`, which the server sets once it has read it.
static WEB_PORT: OnceLock<String> = OnceLock::new();

pub(super) const M_BOX_TITLE: &str = "WirePod";
pub(super) const M_BOX_SUCCESS: &str = "WirePod has started successfully! It is now running in the background and can be managed in the system tray.";

pub(super) fn m_box_icon(cross: &dyn OsFuncs) -> PathBuf {
    cross.resources_path().join("icons/png/podfull.png")
}

pub(super) fn get_needs_setup_msg() -> String {
    format!(
        "WirePod is now running in the background. You must set it up by heading to {} in a browser.",
        web_url()
    )
}

/// Go's `os.Hostname()` is `cross.Hostname()` here, so a test can fake it.
fn check_if_restart_needed(cross: &dyn OsFuncs) -> bool {
    let host = cross.hostname();
    let Ok(mut conf) = cross.read_config() else {
        return false;
    };
    if conf.needs_restart && host != "escapepod" {
        true
    } else if conf.needs_restart && host == "escapepod" {
        conf.needs_restart = false;
        let _ = cross.write_config(&conf);
        false
    } else {
        false
    }
}

/// Go's `StartWirePod`, the entry point.
pub fn start_wire_pod(mut cross_os: impl OsFuncs + 'static) {
    // Go recovers a panic on its main goroutine only; a hook sees every thread.
    std::panic::set_hook(Box::new(crash_dump));

    let init = cross_os.init();
    let cross: &'static dyn OsFuncs = CROSS.get_or_init(|| Box::new(cross_os)).as_ref();
    if let Err(err) = init {
        err_msg(&err);
    }
    if check_if_restart_needed(cross) {
        zenity::error(
            "You must restart your computer before starting WirePod.",
            M_BOX_TITLE,
            Icon::Error,
        );
        std::process::exit(1);
    }
    // Go sets `vars.Packaged` here; `on_ready` boots a packaged state.
    let conf_dir = match user_config_dir() {
        Ok(dir) => dir,
        Err(err) => err_msg(&err),
    };
    let pid_file = std::fs::read(conf_dir.join("runningPID")).ok();
    if is_already_running(cross, pid_file.as_deref()) {
        zenity::error("WirePod is already running.", M_BOX_TITLE, Icon::Error);
        std::process::exit(1);
    }

    let conf = match record_running_pid(cross, std::process::id()) {
        Ok(conf) => conf,
        Err(err) => err_msg(&err),
    };

    let chdir = std::env::set_current_dir(Path::new(&conf.install_path).join("chipper"));
    println!("Working directory: {}/chipper", conf.install_path);
    if chdir.is_err() {
        err_msg(&format!(
            "error setting runtime directory to {}/chipper",
            conf.install_path
        ));
    }

    let webserver_port = webserver_port(&conf, std::env::var("WEBSERVER_PORT").ok());

    systray::run(move || on_ready(cross, webserver_port), on_exit);
}

/// Go's two checks, the `runningPID` file and then `IsPodAlreadyRunning`,
/// which show the same box.
fn is_already_running(cross: &dyn OsFuncs, pid_file: Option<&[u8]>) -> bool {
    if let Some(pid_file) = pid_file {
        let pid = String::from_utf8_lossy(pid_file).parse().unwrap_or(0);
        if cross.is_pid_process_running(pid).unwrap_or(false) {
            return true;
        }
    }
    cross.is_pod_already_running()
}

fn record_running_pid(cross: &dyn OsFuncs, pid: u32) -> io::Result<WpConfig> {
    let mut conf = cross.read_config()?;
    conf.last_running_pid = pid;
    cross.write_config(&conf)?;
    Ok(conf)
}

/// Go sets `WEBSERVER_PORT` from the registry unless the port is the default,
/// and otherwise leaves whatever the process started with.
fn webserver_port(conf: &WpConfig, from_env: Option<String>) -> Option<String> {
    if conf.ws_port != "8080" && conf.ws_port != "0" {
        Some(conf.ws_port.clone())
    } else {
        from_env
    }
}

/// Go's `ExitProgram`.
pub fn exit_program(code: i32) -> ! {
    if let Some(cross) = CROSS.get() {
        cross.on_exit();
    }
    systray::quit();
    std::process::exit(code)
}

fn on_exit() {
    std::process::exit(0)
}

struct Menu {
    quit: MenuItem,
    browse: MenuItem,
    config: MenuItem,
    startup: MenuItem,
    about: MenuItem,
}

fn on_ready(cross: &'static dyn OsFuncs, webserver_port: Option<String>) {
    // Go sets STT_SERVICE and DEBUG_LOGGING here. `initwirepod` carries the
    // first in the `Env` it builds, and the second has no counterpart.

    let systray_icon = match std::fs::read(
        cross
            .resources_path()
            .join("icons/ico")
            .join("pod24x24.ico"),
    ) {
        Ok(icon) => icon,
        Err(_) => {
            zenity::error(
                "Error, could not load systray icon. Something is wrong with the program directory. Exiting.",
                M_BOX_TITLE,
                Icon::Error,
            );
            std::process::exit(1);
        }
    };

    systray::set_icon(&systray_icon);
    systray::set_title("WirePod");
    systray::set_tooltip("WirePod is starting...");
    let menu = Menu {
        quit: systray::add_menu_item("Quit", "Quit WirePod"),
        browse: systray::add_menu_item("Web Interface", "Open web UI"),
        config: systray::add_menu_item(
            "Config Folder",
            "Open config folder in case you need to. The web UI should have everything you need.",
        ),
        startup: systray::add_menu_item("Run On Startup", ""),
        about: systray::add_menu_item("About", "About WirePod"),
    };

    let conf = cross.read_config().unwrap_or_default();
    if conf.run_at_startup {
        menu.startup.check();
    } else {
        menu.startup.uncheck();
    }

    // The server's runtime is built on this thread, because the message loop
    // owns the main one. The menu gets a thread of its own, as Go's goroutine,
    // so a box it shows blocks nothing else.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => err_msg(&err),
    };
    let handle = runtime.handle().clone();
    std::thread::spawn(move || handle.block_on(menu_loop(cross, menu, conf)));

    runtime.block_on(initwirepod::start_from_program_init(cross, webserver_port));
}

async fn menu_loop(cross: &'static dyn OsFuncs, mut menu: Menu, conf: WpConfig) {
    loop {
        tokio::select! {
            Some(()) = menu.quit.clicked_ch.recv() => {
                zenity::info(
                    "WirePod will now exit.",
                    M_BOX_TITLE,
                    Icon::File(&m_box_icon(cross)),
                );
                exit_program(0);
            }
            Some(()) = menu.browse.clicked_ch.recv() => {
                let url = web_url();
                std::thread::spawn(move || open_browser(&url));
            }
            Some(()) = menu.config.clicked_ch.recv() => {
                let path = user_config_dir().unwrap_or_default().join(POD_NAME);
                std::thread::spawn(move || open_file_explorer(&path));
            }
            Some(()) = menu.about.clicked_ch.recv() => {
                zenity::info(
                    &format!(
                        "WirePod is an Escape Pod alternative which is able to get any Anki/DDL Vector robot setup and working with voice commands.\n\nVersion: {}",
                        conf.version
                    ),
                    "WirePod",
                    Icon::File(&m_box_icon(cross)),
                );
            }
            Some(()) = menu.startup.clicked_ch.recv() => {
                if menu.startup.checked() {
                    menu.startup.uncheck();
                    let _ = cross.run_pod_at_startup(false);
                } else {
                    menu.startup.check();
                    let _ = cross.run_pod_at_startup(true);
                }
            }
            // Go's channels are never closed; these close when the tray is gone.
            else => return,
        }
    }
}

/// Go's `openBrowser`, on its Windows branch.
pub(super) fn open_browser(url: &str) {
    if let Err(err) = Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn()
    {
        let text = format!("Error opening browser: {err}");
        std::thread::spawn(move || zenity::warning(&text, M_BOX_TITLE, Icon::Warning));
        tracing::info!("{err}");
    }
}

/// Go's `openFileExplorer`, on its Windows branch.
fn open_file_explorer(path: &Path) -> io::Result<()> {
    Command::new("explorer").arg(path).spawn().map(|_| ())
}

/// The deferred `recover` in Go's `StartWirePod`.
fn crash_dump(info: &PanicHookInfo<'_>) {
    let payload = info.payload();
    let r = if let Some(r) = payload.downcast_ref::<&str>() {
        (*r).to_owned()
    } else if let Some(r) = payload.downcast_ref::<String>() {
        r.clone()
    } else {
        String::new()
    };
    let conf = user_config_dir().unwrap_or_default();
    let dump_file = conf.join("wire-pod").join("dump.txt");
    let _ = std::fs::create_dir_all(conf.join("wire-pod"));
    let _ = std::fs::write(
        &dump_file,
        format!("{r}\n\n\n{}", Backtrace::force_capture()),
    );
    println!("panic!: {r}");
    zenity::error(
        &format!(
            "wire-pod has crashed. dump located in {}. exiting",
            dump_file.display()
        ),
        "wire-pod crash :(",
        Icon::Error,
    );
    exit_program(1);
}

/// Go's `os.UserConfigDir` on Windows.
fn user_config_dir() -> io::Result<PathBuf> {
    match std::env::var_os("APPDATA") {
        Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir)),
        _ => Err(io::Error::other("%AppData% is not defined")),
    }
}

/// Go's `vars.WebPort`, which is `8080` until the server has read it.
pub(super) fn web_port() -> &'static str {
    WEB_PORT.get().map_or("8080", String::as_str)
}

pub(super) fn set_web_port(port: String) {
    let _ = WEB_PORT.set(port);
}

/// Go's `"http://" + vars.GetOutboundIP().String() + ":" + vars.WebPort`.
pub(super) fn web_url() -> String {
    format!("http://{}:{}", get_outbound_ip(), web_port())
}

/// Go's `vars.GetOutboundIP`. `wirepod-server`'s mDNS module and
/// `wirepod-setup`'s certificates module each keep a private copy, which
/// answer loopback where Go answers `0.0.0.0`.
fn get_outbound_ip() -> IpAddr {
    let local = UdpSocket::bind("0.0.0.0:0").and_then(|socket| {
        socket.connect("8.8.8.8:80")?;
        socket.local_addr()
    });
    match local {
        Ok(addr) => addr.ip(),
        Err(err) => {
            tracing::info!("not connected to a network: {err}");
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Mutex;

    use super::*;

    /// Go's `OSFuncs` over a config held in memory.
    #[derive(Default)]
    struct Fake {
        config: Mutex<WpConfig>,
        hostname: String,
        running: HashSet<u32>,
        pod_running: bool,
    }

    impl Fake {
        fn config(&self) -> WpConfig {
            self.config
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .clone()
        }
    }

    impl OsFuncs for Fake {
        fn init(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn read_config(&self) -> io::Result<WpConfig> {
            Ok(self.config())
        }
        fn run_pod_at_startup(&self, _run: bool) -> io::Result<()> {
            Ok(())
        }
        fn write_config(&self, config: &WpConfig) -> io::Result<()> {
            *self.config.lock().unwrap_or_else(|err| err.into_inner()) = config.clone();
            Ok(())
        }
        fn is_pod_already_running(&self) -> bool {
            self.pod_running
        }
        fn is_pid_process_running(&self, pid: u32) -> io::Result<bool> {
            Ok(self.running.contains(&pid))
        }
        fn kill_existing_pod(&self) -> io::Result<()> {
            Ok(())
        }
        fn resources_path(&self) -> PathBuf {
            PathBuf::from("./")
        }
        fn hostname(&self) -> String {
            self.hostname.clone()
        }
        fn on_exit(&self) {}
    }

    #[test]
    fn a_live_pid_or_a_running_pod_refuses_a_second_instance() {
        let fake = Fake {
            running: HashSet::from([4242]),
            ..Fake::default()
        };
        assert!(is_already_running(&fake, Some(b"4242")));
        assert!(!is_already_running(&fake, Some(b"4243")));
        assert!(!is_already_running(&fake, Some(b"not a pid")));
        assert!(!is_already_running(&fake, None));

        let fake = Fake {
            pod_running: true,
            ..Fake::default()
        };
        assert!(is_already_running(&fake, None));
    }

    #[test]
    fn needs_restart_is_cleared_on_escapepod_and_refuses_anywhere_else() {
        let pending = WpConfig {
            needs_restart: true,
            ..WpConfig::default()
        };

        let fake = Fake {
            config: Mutex::new(pending.clone()),
            hostname: "escapepod".to_owned(),
            ..Fake::default()
        };
        assert!(!check_if_restart_needed(&fake));
        assert!(!fake.config().needs_restart);

        let fake = Fake {
            config: Mutex::new(pending),
            hostname: "desktop".to_owned(),
            ..Fake::default()
        };
        assert!(check_if_restart_needed(&fake));
        assert!(fake.config().needs_restart);
    }

    #[test]
    fn the_running_pid_is_written_and_the_registry_port_becomes_the_web_port() {
        let fake = Fake {
            config: Mutex::new(WpConfig {
                ws_port: "8081".to_owned(),
                install_path: "C:/Program Files/wire-pod".to_owned(),
                ..WpConfig::default()
            }),
            ..Fake::default()
        };
        let conf = record_running_pid(&fake, 31337).expect("the config is written");
        assert_eq!(conf.last_running_pid, 31337);
        assert_eq!(fake.config().last_running_pid, 31337);
        assert_eq!(fake.config().install_path, "C:/Program Files/wire-pod");

        let port = webserver_port(&conf, None);
        assert_eq!(port.as_deref(), Some("8081"));
        assert_eq!(crate::serve::web_port(None, port.as_deref()), "8081");

        for default in ["8080", "0"] {
            let conf = WpConfig {
                ws_port: default.to_owned(),
                ..WpConfig::default()
            };
            assert_eq!(webserver_port(&conf, None), None);
            assert_eq!(
                webserver_port(&conf, Some("9090".to_owned())).as_deref(),
                Some("9090")
            );
        }
    }
}
