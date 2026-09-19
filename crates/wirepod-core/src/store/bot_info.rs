//! The bot-info file: the list of authenticated robots and their GUIDs.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use crate::esn::Esn;
use crate::gojson::{
    Decoded, Extra, Faults, GoObject, go_marshal, store_bool, store_list, store_string,
};
use crate::paths::DataDir;
use crate::persist::WriteGate;
use crate::robot::conn::ConnTarget;
use crate::token::stores::host_of;

/// `vars.BotInfo.GlobalGUID` as `StoreBotInfo` sets it on every call
/// (`botInfoStorer.go:135`).
pub const GLOBAL_GUID: &str = "tni1TRsTRTaNSapjo0Y+Sw==";

/// The mode every `os.WriteFile` of this file passes (`botInfoStorer.go:152`,
/// `jdocs/server.go:52`, `:77`, `token/token.go:95`).
pub const BOT_INFO_FILE_MODE: u32 = 0o644;

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
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
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

impl GoObject for BotInfoRobot {
    const TAGS: &'static [&'static str] = &["esn", "ip_address", "guid", "activated"];
    const PREFIX: &'static str = "robots.";
    const GO_TYPE: &'static str = "struct";

    fn store(
        &mut self,
        tag: &'static str,
        raw: &RawValue,
        faults: &mut Faults,
    ) -> serde_json::Result<()> {
        match tag {
            "esn" => store_string(&mut self.esn, raw, Self::PREFIX, tag, faults),
            "ip_address" => store_string(&mut self.ip_address, raw, Self::PREFIX, tag, faults),
            "guid" => store_string(&mut self.guid, raw, Self::PREFIX, tag, faults),
            "activated" => store_bool(&mut self.activated, raw, Self::PREFIX, tag, faults),
            _ => {}
        }
        Ok(())
    }

    fn unknown(&mut self) -> &mut Extra {
        &mut self.extra
    }
}

impl<'de> Deserialize<'de> for BotInfoRobot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Decoded::<Self>::deserialize(deserializer)?.value)
    }
}

impl GoObject for BotInfo {
    const TAGS: &'static [&'static str] = &["global_guid", "robots"];
    const PREFIX: &'static str = "";
    const GO_TYPE: &'static str = "RobotInfoStore";

    fn store(
        &mut self,
        tag: &'static str,
        raw: &RawValue,
        faults: &mut Faults,
    ) -> serde_json::Result<()> {
        match tag {
            "global_guid" => {
                store_string(&mut self.global_guid, raw, Self::PREFIX, tag, faults);
                Ok(())
            }
            "robots" => store_list(&mut self.robots, raw, Self::PREFIX, tag, "[]struct", faults),
            _ => Ok(()),
        }
    }

    fn unknown(&mut self) -> &mut Extra {
        &mut self.extra
    }
}

impl<'de> Deserialize<'de> for BotInfo {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Decoded::<Self>::deserialize(deserializer)?.value)
    }
}

impl BotInfo {
    /// Go's `IsBotInInfo` (`botInfoStorer.go:19-26`).
    pub fn is_bot_in_info(&self, esn: &str) -> bool {
        self.robots
            .iter()
            .any(|robot| esn == robot.esn.trim().to_ascii_lowercase())
    }

    /// Go's `StoreBotInfo` (`botInfoStorer.go:130-153`) without the write.
    ///
    /// `false` is where Go indexes `strings.Split(thing, ":")[1]` on a `thing`
    /// carrying no colon and takes the process down; nothing is changed and the
    /// caller writes nothing.
    pub fn store_bot_info(&mut self, peer_addr: &str, thing: &str) -> bool {
        let ip_addr = host_of(peer_addr).trim();
        let Some(bot_esn) = thing.split(':').nth(1) else {
            tracing::debug!(
                comp = "",
                "thing {thing} carries no colon, not storing bot info"
            );
            return false;
        };
        let bot_esn = bot_esn.trim();
        self.global_guid = GLOBAL_GUID.to_owned();
        let mut append_new = true;
        for robot in self.robots.iter_mut() {
            if robot.esn == bot_esn {
                append_new = false;
                robot.ip_address = ip_addr.to_owned();
            }
        }
        if append_new {
            tracing::debug!(comp = "", "Adding {bot_esn} to bot info store");
            self.robots.push(BotInfoRobot {
                esn: bot_esn.to_owned(),
                ip_address: ip_addr.to_owned(),
                guid: String::new(),
                activated: false,
                extra: Extra::new(),
            });
        }
        true
    }

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

/// Go's `json.Marshal(vars.BotInfo)` (`botInfoStorer.go:151`).
///
/// # Panics
///
/// Never: every field is a string, a bool or a list of them, and a
/// [`serde_json::Value`] in an [`Extra`] map cannot hold a NaN.
pub fn marshal_bot_info(info: &BotInfo) -> Vec<u8> {
    go_marshal(info).expect("a bot-info file holds nothing unserialisable")
}

/// The gate every write of `botSdkInfo.json` goes through.
pub fn bot_info_gate(dir: &DataDir) -> WriteGate {
    WriteGate::new(dir.bot_info_path(), BOT_INFO_FILE_MODE)
}

/// `os.WriteFile(vars.BotInfoPath, json.Marshal(vars.BotInfo), 0644)`, which is
/// the tail of `StoreBotInfo` and of the three other writers.
///
/// # Errors
///
/// Whatever the write reports. Go discards it.
pub async fn write_bot_info(dir: &DataDir, info: &BotInfo) -> io::Result<()> {
    let bytes = marshal_bot_info(info);
    bot_info_gate(dir).write(move || bytes).await
}

/// `os.ReadFile(vars.BotInfoPath)` followed by `json.Unmarshal`
/// (`token/token.go:59-67`), which reads the file rather than the in-memory
/// copy.
///
/// # Errors
///
/// The read's, and a decode fault as an [`io::Error`] where Go returns the
/// `json.Unmarshal` error.
pub async fn read_bot_info(dir: &DataDir) -> io::Result<BotInfo> {
    let path = PathBuf::from(dir.bot_info_path());
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(path))
        .await
        .map_err(io::Error::other)??;
    let decoded: Decoded<BotInfo> = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    match decoded.fault {
        Some(fault) => Err(io::Error::other(fault)),
        None => Ok(decoded.value),
    }
}
