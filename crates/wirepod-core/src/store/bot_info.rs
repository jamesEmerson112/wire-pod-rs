//! The bot-info file: the list of authenticated robots and their GUIDs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::esn::Esn;
use crate::robot::conn::ConnTarget;

/// The on-disk `botSdkInfo.json`, which Go holds in memory as
/// `vars.BotInfo` of type `vars.RobotInfoStore` (`vars.go:89-98`).
///
/// The file lives beside the jdocs, at `vars.BotInfoPath`. That is
/// `./jdocs/botSdkInfo.json` relative to the working directory for a
/// source build (`vars.go:40-41`) and `<user config dir>/wire-pod/jdocs/
/// botSdkInfo.json` for a packaged one (`vars.go:173`), which on this machine
/// is `%APPDATA%\wire-pod\jdocs\botSdkInfo.json`. It is loaded once at startup
/// (`vars.go:246-252`) and rewritten by the jdocs and token servers as robots
/// authenticate.
///
/// Go's `json.Unmarshal` silently drops every key it does not name, so a
/// rewrite loses anything a fork added. This struct keeps them instead: every
/// field is `#[serde(default)]` and a flattened `extra` map collects the rest,
/// so a load and save is lossless and rolling back to the Go server stays
/// safe. The cost is that the extras re-serialise in sorted key order, because
/// `serde_json`'s object map is a `BTreeMap` without the `preserve_order`
/// feature. That is a recorded deviation; it never reaches the wire, because
/// `/api-sdk/get_sdk_info` serialises [`BotInfoWire`] instead.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct BotInfo {
    /// The GUID used for any robot whose own GUID is empty.
    #[serde(default)]
    pub global_guid: String,
    /// Every robot that has authenticated against this server, in file order.
    #[serde(default)]
    pub robots: Vec<BotInfoRobot>,
    /// Top-level keys this struct does not name, preserved across a round trip.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// One robot in the bot-info file.
///
/// The field order is Go's declaration order (`vars.go:90-97`), which is also
/// its marshal order and therefore part of the `get_sdk_info` contract.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct BotInfoRobot {
    /// The robot's serial, in whatever case the file stores it.
    #[serde(default)]
    pub esn: String,
    /// The robot's address, with no port.
    #[serde(default)]
    pub ip_address: String,
    /// The robot's GUID, which is the bearer token. Empty means "use the
    /// global GUID".
    #[serde(default)]
    pub guid: String,
    /// Whether the robot completed authentication.
    #[serde(default)]
    pub activated: bool,
    /// Per-robot keys this struct does not name, preserved across a round trip.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl BotInfo {
    /// Resolves a serial into the address and token needed to dial the robot,
    /// reproducing Go's scan in `newRobot` (`robot.go:333-346`).
    ///
    /// Three rules come from that loop. The comparison is case-insensitive,
    /// because Go uses `strings.EqualFold` (`robot.go:334`). The loop has no
    /// `break`, so when several entries carry the same serial the **last** one
    /// wins (`robot.go:333-346`). And an entry whose own GUID is the empty
    /// string falls back to [`BotInfo::global_guid`] (`robot.go:338-343`).
    ///
    /// The returned [`ConnTarget::esn`] is the requested serial, already
    /// normalized, not the serial as the file spells it: Go stores
    /// `strings.TrimSpace(strings.ToLower(serial))` (`robot.go:335`).
    ///
    /// `None` is Go's `error: robot not found in SDK info file`
    /// (`robot.go:349`).
    pub fn resolve(&self, esn: &Esn) -> Option<ConnTarget> {
        let mut resolved = None;
        for robot in &self.robots {
            if !robot.esn.eq_ignore_ascii_case(esn.as_str()) {
                continue;
            }
            let guid = if robot.guid.is_empty() {
                self.global_guid.clone()
            } else {
                robot.guid.clone()
            };
            resolved = Some(ConnTarget {
                esn: esn.clone(),
                ip: robot.ip_address.clone(),
                guid,
            });
        }
        resolved
    }
}

/// The `/api-sdk/get_sdk_info` body: [`BotInfo`] without the extras.
///
/// Go marshals `vars.BotInfo` directly (`server.go:190`), and its struct has
/// no room for unknown keys, so the body carries exactly `global_guid` then
/// `robots`, in that order. Serialising [`BotInfo`] itself would append the
/// preserved extras and change the body for any file carrying them, so the
/// route serialises this projection instead. Fields borrow, so the projection
/// costs no allocation beyond the robot vector.
///
/// `serde_json::to_string` matches Go's `json.Marshal`: no spaces and no
/// trailing newline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BotInfoWire<'a> {
    /// The GUID used for any robot whose own GUID is empty.
    pub global_guid: &'a str,
    /// Every robot in the bot-info file, in file order.
    pub robots: Vec<RobotWire<'a>>,
}

/// One robot in the [`BotInfoWire`] body, in Go's declaration order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RobotWire<'a> {
    /// The robot's serial, in whatever case the file stores it.
    pub esn: &'a str,
    /// The robot's address, with no port.
    pub ip_address: &'a str,
    /// The robot's GUID.
    pub guid: &'a str,
    /// Whether the robot completed authentication.
    pub activated: bool,
}

impl<'a> From<&'a BotInfo> for BotInfoWire<'a> {
    fn from(info: &'a BotInfo) -> Self {
        Self {
            global_guid: &info.global_guid,
            robots: info.robots.iter().map(RobotWire::from).collect(),
        }
    }
}

impl<'a> From<&'a BotInfoRobot> for RobotWire<'a> {
    fn from(robot: &'a BotInfoRobot) -> Self {
        Self {
            esn: &robot.esn,
            ip_address: &robot.ip_address,
            guid: &robot.guid,
            activated: robot.activated,
        }
    }
}
