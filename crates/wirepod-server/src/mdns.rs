//! Go's `pkg/mdnshandler/mdns.go`: the escapepod mDNS registration.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::Notify;

/// The record Go registers with `zeroconf.RegisterProxy`.
pub const SERVICE_TYPE: &str = "_app-proto._tcp.local.";
pub const INSTANCE_NAME: &str = "escapepod";
pub const HOST_NAME: &str = "escapepod.local.";
pub const PORT: u16 = 8084;
pub const TXT: [(&str, &str); 3] = [("txtv", "0"), ("lo", "1"), ("la", "2")];

/// The service Go browses for to notice a robot arriving on the network.
const VECTOR_SERVICE: &str = "_ankivector._tcp.local.";

/// legacy ZeroConf code
static POSTING_MDNS: AtomicBool = AtomicBool::new(false);
static MDNS_NOW: Notify = Notify::const_new();
static MDNS_TIME_BEFORE_NEXT_REGISTER: Mutex<f32> = Mutex::new(0.0);

/// The record [`post_mdns`] registers, separated out so it can be checked
/// without starting a daemon.
pub fn service_info(ip: IpAddr) -> Result<ServiceInfo, mdns_sd::Error> {
    ServiceInfo::new(SERVICE_TYPE, INSTANCE_NAME, HOST_NAME, ip, PORT, &TXT[..])
}

/// Go's `vars.GetOutboundIP`, which the vars package has not been translated
/// yet.
// TODO(M2): vars.GetOutboundIP()
fn get_outbound_ip() -> IpAddr {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return IpAddr::V4(Ipv4Addr::LOCALHOST);
    };
    if socket.connect("8.8.8.8:80").is_err() {
        return IpAddr::V4(Ipv4Addr::LOCALHOST);
    }
    socket
        .local_addr()
        .map(|addr| addr.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

pub async fn post_mdns_when_new_vector() {
    tokio::time::sleep(Duration::from_secs(5)).await;
    loop {
        let Ok(daemon) = ServiceDaemon::new() else {
            return;
        };
        let entries = match daemon.browse(VECTOR_SERVICE) {
            Ok(entries) => entries,
            Err(err) => {
                tracing::info!("{err}");
                let _ = daemon.shutdown();
                return;
            }
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(80);
        loop {
            match tokio::time::timeout_at(deadline, entries.recv_async()).await {
                Ok(Ok(ServiceEvent::ServiceFound(_, _) | ServiceEvent::ServiceResolved(_))) => {
                    tracing::info!(target: "mdns", "Vector discovered on network, broadcasting mDNS");
                    let _ = daemon.shutdown();
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    post_mdns_now();
                    return;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => break,
            }
        }
        let _ = daemon.shutdown();
    }
}

pub fn post_mdns_now() {
    tracing::info!(target: "mdns", "Broadcasting mDNS now (outside of timer loop)");
    MDNS_NOW.notify_one();
}

pub async fn post_mdns() {
    if std::env::var("DISABLE_MDNS").as_deref() == Ok("true") {
        tracing::info!(target: "mdns", "mDNS is disabled");
        return;
    }
    // Go tests the flag and sets it a few lines later; one swap does both.
    if POSTING_MDNS.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(post_mdns_when_new_vector());
    tokio::spawn(async {
        loop {
            MDNS_NOW.notified().await;
            set_time_before_next_register(30.0);
        }
    });
    tracing::info!(target: "mdns", "Registering escapepod.local on network (loop)");
    loop {
        let ip_addr = get_outbound_ip();
        let daemon = match ServiceDaemon::new().and_then(|daemon| {
            let info = service_info(ip_addr)?;
            daemon.register(info)?;
            Ok(daemon)
        }) {
            Ok(daemon) => Some(daemon),
            Err(err) => {
                tracing::info!(target: "mdns", "{err}");
                None
            }
        };
        if std::env::var("PRINT_MDNS").as_deref() == Ok("true") {
            tracing::info!(target: "mdns", "mDNS broadcasted");
        }
        loop {
            if tick_time_before_next_register() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if let Some(daemon) = daemon {
            let _ = daemon.shutdown();
        }
        tokio::time::sleep(Duration::from_millis(333)).await;
    }
}

fn set_time_before_next_register(value: f32) {
    let mut time = MDNS_TIME_BEFORE_NEXT_REGISTER
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    *time = value;
}

/// One quarter-second of Go's inner timer loop, true when it is time to
/// re-register.
fn tick_time_before_next_register() -> bool {
    let mut time = MDNS_TIME_BEFORE_NEXT_REGISTER
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    if *time >= 30.0 {
        *time = 0.0;
        return true;
    }
    *time += 1.0 / 4.0;
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_is_the_one_the_robot_browses_for() {
        let info = service_info("192.168.8.203".parse().expect("parse the address"))
            .expect("build the record");

        assert_eq!(info.get_type(), SERVICE_TYPE);
        assert_eq!(info.get_fullname(), "escapepod._app-proto._tcp.local.");
        assert_eq!(info.get_hostname(), HOST_NAME);
        assert_eq!(info.get_port(), PORT);
        assert_eq!(info.get_property_val_str("txtv"), Some("0"));
        assert_eq!(info.get_property_val_str("lo"), Some("1"));
        assert_eq!(info.get_property_val_str("la"), Some("2"));
    }

    #[test]
    fn the_timer_loop_re_registers_after_thirty_seconds_of_quarter_ticks() {
        set_time_before_next_register(0.0);
        for _ in 0..120 {
            assert!(!tick_time_before_next_register());
        }
        assert!(tick_time_before_next_register());
    }
}
