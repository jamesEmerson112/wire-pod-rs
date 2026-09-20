//! Translation of `pkg/wirepod/setup/ssh.go`.
//!
//! Go's clients are `golang.org/x/crypto/ssh` and go-scp; here they sit behind
//! [`Transport`], so the command and file sequence can be driven without a
//! robot, and [`Russh`] is the one that dials. go-scp's `CopyFile` reads its
//! reader into memory before sending, so [`Transport::copy_file`] takes the
//! bytes rather than a reader. The `/api-ssh/` mux registration stays with
//! `wirepod-server`, which owns every route.

use std::borrow::Cow;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use russh::client::{self, Handle};
use russh::keys::{Algorithm, EcdsaCurve, PrivateKey, PrivateKeyWithHashAlg, decode_secret_key};
use russh::{ChannelMsg, Disconnect, Preferred};
use wirepod_core::Paths;
use wirepod_core::config::ServerConfig;

use crate::certs::create_server_config;

// this file will be copied to the bot
// Go's `SetupScriptPath` is a global it rewrites for android and for a packaged
// Linux build; here it comes out of the asset directory.
// TODO(M5): vars.AndroidPath, vars.IsPackagedLinux

// path to copy to
pub const BOT_SETUP_PATH: &str = "/data/pod-bot-install.sh";

/// `ssh.go:141`, which nothing in this repository vendors.
const VIC_CLOUD_PATH: &str = "../vector-cloud/build/vic-cloud";

/// `ssh.go:163`.
const VIC_CLOUD_URL: &str =
    "https://github.com/kercre123/wire-pod/raw/main/vector-cloud/build/vic-cloud";

/// Go's `vars.Packaged`, which is false in every build this port makes, and
/// its android branch, which the port does not have.
// TODO(M5): vars.Packaged
const PACKAGED: bool = false;

/// Go's `HostKeyAlgorithms` (`ssh.go:71`).
const HOST_KEY_ALGORITHMS: &[Algorithm] = &[
    Algorithm::Rsa { hash: None },
    Algorithm::Ecdsa {
        curve: EcdsaCurve::NistP256,
    },
];

static SETUP_SSH_STATUS: LazyLock<Mutex<String>> =
    LazyLock::new(|| Mutex::new("not running".to_string()));

static SSH_SETTING_UP: AtomicBool = AtomicBool::new(false);

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    /// The text `golang.org/x/crypto/ssh` gives an `ExitError`, which the two
    /// checks below compare against.
    #[error("Process exited with status {0}")]
    ExitStatus(u32),
    #[error("{0}")]
    Ssh(#[from] russh::Error),
    #[error("{0}")]
    Key(#[from] russh::keys::Error),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Other(String),
}

/// The status the web UI polls.
pub fn setup_ssh_status() -> String {
    SETUP_SSH_STATUS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn set_setup_ssh_status(status: impl Into<String>) {
    *SETUP_SSH_STATUS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = status.into();
}

fn do_err(err: SshError, msg: &str) -> SshError {
    SSH_SETTING_UP.store(false, Ordering::SeqCst);
    set_setup_ssh_status(format!("not running (last error: {err}, last step: {msg})"));
    err
}

/// The half of `golang.org/x/crypto/ssh` and go-scp this file uses.
#[async_trait]
pub trait Transport {
    async fn run_cmd(&mut self, cmd: &str) -> Result<String, SshError>;
    async fn copy_file(
        &mut self,
        contents: Vec<u8>,
        remote_path: &str,
        permissions: &str,
    ) -> Result<(), SshError>;
    async fn close(&mut self);
}

async fn set_cpu_ram_freq<T: Transport + ?Sized>(
    client: &mut T,
    cpufreq: &str,
    ramfreq: &str,
    gov: &str,
) {
    let _ = client
        .run_cmd(&("echo ".to_string() + cpufreq + " > /sys/devices/system/cpu/cpu0/cpufreq/scaling_max_freq && echo disabled > /sys/kernel/debug/msm_otg/bus_voting && echo 0 > /sys/kernel/debug/msm-bus-dbg/shell-client/update_request && echo 1 > /sys/kernel/debug/msm-bus-dbg/shell-client/mas && echo 512 > /sys/kernel/debug/msm-bus-dbg/shell-client/slv && echo 0 > /sys/kernel/debug/msm-bus-dbg/shell-client/ab && echo active clk2 0 1 max " + ramfreq + " > /sys/kernel/debug/rpm_send_msg/message && echo " + gov + " > /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor && echo 1 > /sys/kernel/debug/msm-bus-dbg/shell-client/update_request"))
        .await;
}

pub async fn setup_bot_via_ssh(
    paths: &Paths,
    server: &ServerConfig,
    ip: &str,
    key: &[u8],
) -> Result<(), SshError> {
    if !SSH_SETTING_UP.load(Ordering::SeqCst) {
        tracing::info!(comp = "", "Setting up {ip} via SSH");
        set_setup_ssh_status("Setting up SSH connection...");
        create_server_config(paths.data(), server).await;
        // Go records this error and then dials with a nil signer, which panics.
        let signer = parse_private_key(key).map_err(|err| do_err(err, "parsing priv key"))?;
        let mut client = Russh::dial(ip, signer)
            .await
            .map_err(|err| do_err(err, "ssh dial"))?;
        run_setup(&mut client, paths, server).await
    } else {
        Err(SshError::Other("a bot is already being setup".to_string()))
    }
}

fn parse_private_key(key: &[u8]) -> Result<PrivateKey, SshError> {
    let text = std::str::from_utf8(key).map_err(|err| SshError::Other(err.to_string()))?;
    Ok(decode_secret_key(text, None)?)
}

/// Everything [`setup_bot_via_ssh`] does once the connection is up.
pub async fn run_setup<T: Transport + ?Sized>(
    client: &mut T,
    paths: &Paths,
    server: &ServerConfig,
) -> Result<(), SshError> {
    set_setup_ssh_status("Checking if device is a Vector...");
    let output = client
        .run_cmd("uname -a")
        .await
        .map_err(|err| do_err(err, "checking if vector"))?;
    if !output.contains("Vector") {
        return Err(do_err(
            SshError::Other("the remote device is not a vector".to_string()),
            "checking if vector",
        ));
    }
    set_setup_ssh_status("Checking if Vector is running CFW...");
    // outputWired, _ := runCmd(client, "cat /etc/wired/webroot/index.html")
    let mut do_cloud = true;
    let mut init_command = "mount -o rw,remount / && mount -o rw,remount,exec /data && systemctl stop anki-robot.target mm-anki-camera mm-qcamera-daemon";
    if let Err(err) = client.run_cmd("head -n1 /anki/bin/vic-gateway").await {
        if err.to_string() == "Process exited with status 1" {
            tracing::info!(
                comp = "",
                "SSH setup: modern CFW detected, not copying vic-cloud"
            );
            //|| strings.Contains(outputWired, "revertDefaultWakeWord") {
            init_command = "mount -o rw,remount,exec /data && systemctl stop anki-robot.target mm-anki-camera mm-qcamera-daemon";
            // my cfw already has a wire-pod compatible vic-cloud
            do_cloud = false;
        } else {
            return Err(do_err(err, "checking if cfw"));
        }
    }
    set_setup_ssh_status(
        "Running initial commands before transfers (screen will go blank, this is normal)...",
    );
    if let Err(err) = client.run_cmd(init_command).await
        && !err.to_string().contains("Process exited with status 1")
    {
        return Err(do_err(err, "initial commands"));
    }
    set_cpu_ram_freq(client, "1267200", "800000", "performance").await;
    set_setup_ssh_status("Waiting a few seconds for filesystem syncing");
    tokio::time::sleep(Duration::from_secs(3)).await;
    set_setup_ssh_status("Transferring bot setup script and certs...");
    let script = std::fs::read(paths.assets().pod_bot_install_path())
        .map_err(|err| do_err(err.into(), "opening setup script"))?;
    client
        .copy_file(script, "/data/pod-bot-install.sh", "0755")
        .await
        .map_err(|err| do_err(err, "copying pod-bot-install"))?;
    let server_config = std::fs::read(paths.data().server_config_path())
        .map_err(|err| do_err(err.into(), "opening server config on disk"))?;
    client
        .copy_file(server_config, "/data/data/server_config.json", "0755")
        .await
        .map_err(|err| do_err(err, "copying server-config.json"))?;
    if do_cloud {
        let cloud = if !PACKAGED {
            std::fs::read(VIC_CLOUD_PATH)
                .map_err(|err| do_err(err.into(), "transferring new vic-cloud"))?
        } else {
            let resp = reqwest::get(VIC_CLOUD_URL).await.map_err(|err| {
                do_err(
                    SshError::Other(err.to_string()),
                    "transferring new vic-cloud (download)",
                )
            })?;
            resp.bytes()
                .await
                .map_err(|err| {
                    do_err(
                        SshError::Other(err.to_string()),
                        "transferring new vic-cloud (download)",
                    )
                })?
                .to_vec()
        };
        set_setup_ssh_status("Transferring new vic-cloud...");
        // Go's retry re-reads a reader it has already drained, so its second
        // attempt sends nothing; this one sends the same bytes again.
        if client
            .copy_file(cloud.clone(), "/anki/bin/vic-cloud", "0755")
            .await
            .is_err()
        {
            tokio::time::sleep(Duration::from_secs(1)).await;
            client
                .copy_file(cloud, "/anki/bin/vic-cloud", "0755")
                .await
                .map_err(|err| do_err(err, "copying vic-cloud"))?;
        }
    }
    let cert_path = if server.epconfig {
        paths.assets().epod_cert_path()
    } else {
        paths.data().cert_path()
    };
    let cert = std::fs::read(cert_path).map_err(|err| do_err(err.into(), "opening cert"))?;
    client
        .copy_file(cert, "/data/data/wirepod-cert.crt", "0755")
        .await
        .map_err(|err| do_err(err, "copying wire-pod cert"))?;
    set_setup_ssh_status("Generating new robot certificate (this may take a while)...");
    client
        .run_cmd("chmod +rwx /data/data/server_config.json /data/data/wirepod-cert.crt /data/pod-bot-install.sh && /data/pod-bot-install.sh")
        .await
        .map_err(|err| do_err(err, "generating new robot cert"))?;
    set_cpu_ram_freq(client, "733333", "500000", "interactive").await;
    client.close().await;
    set_setup_ssh_status("done");
    Ok(())
}

/// `/api-ssh/setup`. The form parsing and the reply writing belong to
/// `wirepod-server`; this is the body between them.
pub fn ssh_setup(paths: Paths, server: ServerConfig, ip: &str, key: &[u8]) -> String {
    if ip.is_empty() {
        return "error: must provide ip".to_string();
    }
    // Go reads `err.Error()` here even when there is no error, and panics.
    if key.len() < 5 {
        return "error: must provide ssh key ()".to_string();
    }
    let (ip, key) = (ip.to_string(), key.to_vec());
    tokio::spawn(async move {
        let _ = setup_bot_via_ssh(&paths, &server, &ip, &key).await;
    });
    "running".to_string()
}

/// `/api-ssh/get_setup_status`.
pub fn get_setup_status() -> String {
    let status = setup_ssh_status();
    if status == "done" || status.contains("error") {
        set_setup_ssh_status("not running");
    }
    status
}

// TODO(M5): http.HandleFunc("/api-ssh/", SSHSetup)

struct AcceptAnyHostKey;

impl client::Handler for AcceptAnyHostKey {
    type Error = russh::Error;

    /// Go's `ssh.InsecureIgnoreHostKey` (`ssh.go:70`).
    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// The connection Go gets from `ssh.Dial` and `scp.NewClientBySSH`.
pub struct Russh {
    session: Handle<AcceptAnyHostKey>,
}

impl Russh {
    async fn dial(ip: &str, key: PrivateKey) -> Result<Self, SshError> {
        let config = Arc::new(client::Config {
            preferred: Preferred {
                key: Cow::Borrowed(HOST_KEY_ALGORITHMS),
                ..Preferred::DEFAULT
            },
            ..client::Config::default()
        });
        let connect = client::connect(config, (ip, 22), AcceptAnyHostKey);
        let mut session = tokio::time::timeout(Duration::from_secs(5), connect)
            .await
            .map_err(|_| SshError::Other("dial tcp: i/o timeout".to_string()))??;
        let hash = session.best_supported_rsa_hash().await?.flatten();
        let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
        if !session.authenticate_publickey("root", key).await?.success() {
            return Err(SshError::Other(
                "ssh: unable to authenticate, no supported methods remain".to_string(),
            ));
        }
        Ok(Self { session })
    }
}

#[async_trait]
impl Transport for Russh {
    async fn run_cmd(&mut self, cmd: &str) -> Result<String, SshError> {
        let mut channel = self.session.channel_open_session().await?;
        channel.exec(true, cmd).await?;
        let (mut output, mut code) = (Vec::new(), None);
        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { ref data } => output.extend_from_slice(data),
                ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                _ => {}
            }
        }
        match code {
            Some(status) if status != 0 => Err(SshError::ExitStatus(status)),
            _ => Ok(String::from_utf8_lossy(&output).into_owned()),
        }
    }

    /// go-scp's sink protocol: `scp -t <path>`, one `C<mode> <size> <name>`
    /// header, the bytes, and a terminating NUL.
    async fn copy_file(
        &mut self,
        contents: Vec<u8>,
        remote_path: &str,
        permissions: &str,
    ) -> Result<(), SshError> {
        let name = remote_path.rsplit('/').next().unwrap_or(remote_path);
        let header = format!("C{permissions} {} {name}\n", contents.len());
        let mut channel = self.session.channel_open_session().await?;
        channel.exec(true, format!("scp -t {remote_path}")).await?;
        channel.data_bytes(header.into_bytes()).await?;
        channel.data_bytes(contents).await?;
        channel.data_bytes(vec![0u8]).await?;
        channel.eof().await?;
        let mut code = None;
        while let Some(msg) = channel.wait().await {
            if let ChannelMsg::ExitStatus { exit_status } = msg {
                code = Some(exit_status);
            }
        }
        match code {
            Some(status) if status != 0 => Err(SshError::ExitStatus(status)),
            _ => Ok(()),
        }
    }

    async fn close(&mut self) {
        let _ = self
            .session
            .disconnect(Disconnect::ByApplication, "", "English")
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wirepod_core::paths::{AssetDir, DataDir};

    #[derive(Default)]
    struct Fake {
        commands: Vec<String>,
        files: Vec<(String, String, usize)>,
        closed: bool,
    }

    #[async_trait]
    impl Transport for Fake {
        async fn run_cmd(&mut self, cmd: &str) -> Result<String, SshError> {
            self.commands.push(cmd.to_string());
            match cmd {
                "uname -a" => Ok("Linux Vector-B6H9 3.18.66 armv7l GNU/Linux".to_string()),
                // The modern-CFW answer, which turns the vic-cloud copy off.
                "head -n1 /anki/bin/vic-gateway" => Err(SshError::ExitStatus(1)),
                _ => Ok(String::new()),
            }
        }

        async fn copy_file(
            &mut self,
            contents: Vec<u8>,
            remote_path: &str,
            permissions: &str,
        ) -> Result<(), SshError> {
            self.files.push((
                remote_path.to_string(),
                permissions.to_string(),
                contents.len(),
            ));
            Ok(())
        }

        async fn close(&mut self) {
            self.closed = true;
        }
    }

    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch() -> (TempDir, Paths) {
        let root = std::env::temp_dir().join("wirepod-setup-ssh");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("certs")).expect("state directory");
        std::fs::create_dir_all(root.join("assets").join("epod")).expect("asset directory");
        std::fs::write(
            root.join("assets").join("pod-bot-install.sh"),
            b"#!/bin/sh\n",
        )
        .expect("script");
        std::fs::write(root.join("assets").join("epod").join("ep.crt"), b"cert\n")
            .expect("certificate");
        std::fs::write(root.join("certs").join("server_config.json"), b"{}").expect("config");
        let paths = Paths::new(DataDir::rooted(&root), AssetDir::new(root.join("assets")));
        (TempDir(root), paths)
    }

    #[tokio::test]
    async fn the_onboarding_run_sends_gos_commands_and_gos_three_files_in_order() {
        let (_dir, paths) = scratch();
        let server = ServerConfig {
            epconfig: true,
            port: "443".to_string(),
            extra: Default::default(),
        };
        let mut client = Fake::default();
        run_setup(&mut client, &paths, &server).await.expect("done");

        assert_eq!(client.commands[0], "uname -a");
        assert_eq!(client.commands[1], "head -n1 /anki/bin/vic-gateway");
        assert!(client.commands[2].starts_with("mount -o rw,remount,exec /data &&"));
        assert!(client.commands[3].contains("scaling_max_freq"));
        assert_eq!(
            client.commands[4],
            "chmod +rwx /data/data/server_config.json /data/data/wirepod-cert.crt /data/pod-bot-install.sh && /data/pod-bot-install.sh"
        );
        assert_eq!(client.commands.len(), 6);
        assert_eq!(
            client
                .files
                .iter()
                .map(|(path, mode, size)| (path.as_str(), mode.as_str(), *size))
                .collect::<Vec<_>>(),
            vec![
                ("/data/pod-bot-install.sh", "0755", 10),
                ("/data/data/server_config.json", "0755", 2),
                ("/data/data/wirepod-cert.crt", "0755", 5),
            ]
        );
        assert!(client.closed);
        assert_eq!(get_setup_status(), "done");
        assert_eq!(setup_ssh_status(), "not running");
    }

    #[test]
    fn an_exit_status_renders_the_text_the_cfw_check_compares_against() {
        assert_eq!(
            SshError::ExitStatus(1).to_string(),
            "Process exited with status 1"
        );
    }
}
