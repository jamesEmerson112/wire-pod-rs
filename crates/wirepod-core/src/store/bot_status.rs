//! The jdocs pinger and the bot-status projection `/api/get_bot_status` serves.

use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::clock::Clock;
use crate::store::bot_info::BotInfo;

/// A robot is `online` while its last conn check is no older than this.
///
/// Go's ticker latches `Stopped` once the per-robot counter passes 15
/// (`jdocspinger.go:141`), and the status arm reads `TimeSinceLastCheck <= 15`
/// (`jdocspinger.go:56`).
const ONLINE_MAX_SECS: i64 = 15;

/// A stopped robot is `offline` while its last conn check is younger than
/// this, and `disconnected` from here on (`jdocspinger.go:58`).
const OFFLINE_MAX_SECS: i64 = 120;

/// What `/api/get_bot_status` says about one robot.
///
/// The vocabulary is exactly these three words (`jdocspinger.go:50-62`), and
/// the wire form is the lowercase spelling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BotStatusKind {
    /// A conn check arrived within the last 15 seconds.
    Online,
    /// No conn check for more than 15 seconds but fewer than 120.
    Offline,
    /// No conn check for 120 seconds or more, or none the pinger ever saw.
    /// This is also the value before any pinger entry matches, which is why it
    /// is the default.
    #[default]
    Disconnected,
}

impl BotStatusKind {
    /// The wire spelling, which is what Go's string field holds.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Disconnected => "disconnected",
        }
    }
}

impl fmt::Display for BotStatusKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One element of the `/api/get_bot_status` array.
///
/// The four keys and their order are Go's declaration order
/// (`jdocspinger.go:32-37`), and none carries `omitempty`, so all four are
/// always present.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct BotStatus {
    /// The serial as the bot-info file spells it, in its stored case
    /// (`jdocspinger.go:48`).
    pub esn: String,
    /// The robot's address from the bot-info file.
    pub ip: String,
    /// One of the three status words.
    pub status: BotStatusKind,
    /// Seconds since the last conn check, or `-1` when the pinger has no entry
    /// for this robot (`jdocspinger.go:51`).
    pub timesince: i64,
}

/// One robot the pinger has heard a conn check from.
///
/// Go's `JdocsPingerRobot` also carries the GUID (`jdocspinger.go:26`), which
/// nothing ever reads, and a `Stopped` flag, which is derived here. See
/// [`PingerState::snapshot`].
#[derive(Clone, Debug)]
struct PingerRobot {
    esn: String,
    last_check: Duration,
}

#[derive(Debug)]
struct PingerInner {
    enabled: bool,
    robots: Vec<PingerRobot>,
}

/// The jdocs pinger's record of which robots have checked in recently.
///
/// This is Go's `JdocsPingerBots` (`jdocspinger.go:19-22`): one mutex-guarded
/// list, entirely separate from the robot registry's lock, appended to the
/// first time a robot checks in and never pruned.
///
/// Go tracks age with a `TimeSinceLastCheck` counter that a one-second ticker
/// increments (`jdocspinger.go:136-149`). This stores the clock reading of the
/// last check instead and subtracts on read, which is the same number without
/// a background task. The `Stopped` flag goes the same way: the ticker sets it
/// once the counter passes 15 and only a conn check clears it
/// (`jdocspinger.go:141-144`, `jdocspinger.go:173`), and because the counter
/// only ever grows between checks the flag is exactly "older than 15 seconds".
#[derive(Debug)]
pub struct PingerState {
    inner: Mutex<PingerInner>,
}

impl Default for PingerState {
    fn default() -> Self {
        Self::new()
    }
}

impl PingerState {
    /// An empty pinger with bookkeeping enabled, which is Go's
    /// `PingerEnabled = true` default (`jdocspinger.go:77`).
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(PingerInner {
                enabled: true,
                robots: Vec::new(),
            }),
        }
    }

    /// Whether conn checks are recorded at all.
    pub fn is_enabled(&self) -> bool {
        self.lock().enabled
    }

    /// Turns the bookkeeping on or off.
    ///
    /// Go reads `JDOCS_PINGER_ENABLED=false` once at startup and then skips
    /// both the ticker and the conn-check bookkeeping
    /// (`jdocspinger.go:130-134`, `jdocspinger.go:204`). With the list left
    /// empty, every robot reports `disconnected` with `timesince: -1` forever.
    pub fn set_enabled(&self, enabled: bool) {
        self.lock().enabled = enabled;
    }

    /// Records a conn check arriving from `peer_ip` and answers whether the
    /// caller should pull jdocs from that robot.
    ///
    /// This is Go's `ShouldPingJdocs` (`jdocspinger.go:152-191`) behind the
    /// `PingerEnabled` guard its only caller applies (`jdocspinger.go:204`).
    /// The peer is matched against the bot-info file by **exact** string
    /// equality on `ip_address`, and the scan breaks on the first match
    /// (`jdocspinger.go:155-163`), unlike the case-insensitive
    /// last-match-wins scan behind [`BotInfo::resolve`]. The pinger entry is
    /// then found by exact equality on the serial (`jdocspinger.go:170`), not
    /// by `EqualFold`.
    ///
    /// `true` means the stopped-to-running transition, or a robot the pinger
    /// has never heard from (`jdocspinger.go:174`, `jdocspinger.go:190`); a
    /// robot that was already running answers `false` after having its age
    /// reset (`jdocspinger.go:176-177`). A peer that matches no robot answers
    /// `false` and records nothing.
    ///
    /// Go's handler also decides, before calling this, whether to run mDNS for
    /// an unrecognised peer, by testing the peer IP as a substring of the whole
    /// marshalled bot-info document (`jdocspinger.go:207-216`). That test is
    /// the handler's, not the pinger's, and arrives with mDNS in P1.
    pub fn note_check(&self, info: &BotInfo, peer_ip: &str, clock: &dyn Clock) -> bool {
        let mut inner = self.lock();
        if !inner.enabled {
            return false;
        }
        let Some(robot) = info.robots.iter().find(|robot| robot.ip_address == peer_ip) else {
            return false;
        };
        let now = clock.now();
        if let Some(entry) = inner.robots.iter_mut().find(|entry| entry.esn == robot.esn) {
            let was_stopped = is_stopped(clock.secs_since(entry.last_check));
            entry.last_check = now;
            return was_stopped;
        }
        let esn = robot.esn.clone();
        inner.robots.push(PingerRobot {
            esn,
            last_check: now,
        });
        true
    }

    /// The `/api/get_bot_status` body: one element per robot in the bot-info
    /// file, in file order.
    ///
    /// This is Go's `GetConnectionStatus` (`jdocspinger.go:39-69`). A robot the
    /// pinger has seen but that is absent from the bot-info file is not
    /// reported at all, and a robot with no pinger entry reports
    /// `disconnected` with `timesince: -1`. The inner lookup is **exact** byte
    /// equality on the serial (`jdocspinger.go:54`), unlike every lookup in
    /// `robot.go`, so a case mismatch between the two lists silently reports
    /// `disconnected`; that quirk is reproduced.
    ///
    /// The result is always a `Vec`, never an `Option`, because Go starts from
    /// `[]BotStatus{}` rather than a nil slice precisely so that an empty body
    /// is `[]` and not `null` (`jdocspinger.go:42-45`).
    pub fn snapshot(&self, info: &BotInfo, clock: &dyn Clock) -> Vec<BotStatus> {
        let inner = self.lock();
        let mut statuses = Vec::with_capacity(info.robots.len());
        for robot in &info.robots {
            let mut status = BotStatus {
                esn: robot.esn.clone(),
                ip: robot.ip_address.clone(),
                status: BotStatusKind::Disconnected,
                timesince: -1,
            };
            if let Some(entry) = inner.robots.iter().find(|entry| entry.esn == robot.esn) {
                let elapsed = clock.secs_since(entry.last_check);
                status.timesince = elapsed;
                status.status = classify(elapsed);
            }
            statuses.push(status);
        }
        statuses
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PingerInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Go's latched `Stopped` flag, derived from the age of the last conn check.
///
/// The ticker sets it when the counter passes 15 (`jdocspinger.go:141`).
fn is_stopped(elapsed_secs: i64) -> bool {
    elapsed_secs > ONLINE_MAX_SECS
}

/// The three-way status arm at `jdocspinger.go:56-62`, kept in Go's shape.
fn classify(elapsed_secs: i64) -> BotStatusKind {
    let stopped = is_stopped(elapsed_secs);
    if !stopped && elapsed_secs <= ONLINE_MAX_SECS {
        BotStatusKind::Online
    } else if stopped && elapsed_secs < OFFLINE_MAX_SECS {
        BotStatusKind::Offline
    } else {
        BotStatusKind::Disconnected
    }
}
