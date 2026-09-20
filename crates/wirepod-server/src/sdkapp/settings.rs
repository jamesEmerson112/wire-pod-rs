//! The robot-settings arms of Go's `sdkapp/server.go`: the twelve
//! `update_settings` routes and `get_sdk_settings`.

use std::time::Duration;

use axum::response::Response;
use wirepod_core::logger::COMP_SDK;
use wirepod_core::{AppState, Jdoc, JdocKind, RobotEntry};
use wirepod_vector::urlreqs;

use crate::{literals, reply};

/// What the robot puts in the settings jdoc while it is still booting.
const BOOT_MARKER: &str = "BStat.ReactedToTriggerWord";

/// Go's `time.Sleep(time.Second / 2)` between attempts.
const RETRY_SLEEP: Duration = Duration::from_millis(500);

/// Go's `if i > 3`, which allows five attempts in all.
const MAX_RETRIES: u32 = 3;

/// `server.go:212`.
const BOT_REFUSES: &str = "error: bot refuses to return robotsettings";

pub async fn eye_color(entry: &RobotEntry, color: &str) -> Response {
    urlreqs::set_preset_eye_color(&entry.target, color).await;
    reply::text(literals::DONE)
}

/// Go writes the two form values concatenated with `Fprint`, so a `%` in
/// either is echoed literally.
pub async fn custom_eye_color(entry: &RobotEntry, hue: &str, sat: &str) -> Response {
    urlreqs::set_custom_eye_color(&entry.target, hue, sat).await;
    reply::text(format!("{hue}{sat}"))
}

pub async fn set_string(entry: &RobotEntry, setting: &str, value: &str) -> Response {
    urlreqs::set_setting_sdk_string(&entry.target, setting, value).await;
    reply::text(literals::DONE)
}

pub async fn set_intbool(entry: &RobotEntry, setting: &str, value: &str) -> Response {
    urlreqs::set_setting_sdk_intbool(&entry.target, setting, value).await;
    reply::text(literals::DONE)
}

/// `server.go:197-229`: pull the settings jdoc, retrying while the robot is
/// still answering with its boot document, then store it and write it out.
///
/// Go sets `application/octet-stream` after `WriteHeader`, so the header never
/// reaches the wire and the body is sniffed text.
pub async fn get_sdk_settings(state: &AppState, entry: &RobotEntry) -> Response {
    let mut i = 0;
    loop {
        let named = match entry.conn.pull_jdocs(&[JdocKind::RobotSettings]).await {
            Ok(named) => named,
            // Go writes the bare error text with no `error: ` prefix.
            Err(err) => return reply::text(err.to_string()),
        };
        // Go indexes `NamedJdocs[0]`; the seam refuses an empty answer instead.
        let Some(first) = named.into_iter().next() else {
            return reply::text(BOT_REFUSES);
        };
        let doc = first.doc;
        if doc.json_doc.contains(BOOT_MARKER) {
            tokio::time::sleep(RETRY_SLEEP).await;
            if i > MAX_RETRIES {
                tracing::debug!(target: COMP_SDK, "Bot refuses to return RobotSettings jdoc...");
                tracing::debug!(target: COMP_SDK, "Returned Jdoc:  {}", doc.json_doc);
                return reply::text(BOT_REFUSES);
            }
            i += 1;
            continue;
        }
        let json = doc.json_doc.clone();
        // Go copies three fields into `vars.AJdoc` and drops `client_metadata`.
        let outcome = state
            .jdocs()
            .add_jdoc(
                &format!("vic:{}", entry.esn),
                "vic.RobotSettings",
                Jdoc {
                    doc_version: doc.doc_version,
                    fmt_version: doc.fmt_version,
                    json_doc: doc.json_doc,
                    ..Jdoc::default()
                },
            )
            .await;
        if let Err(err) = outcome.written {
            tracing::warn!(target: COMP_SDK, "write jdocs: {err}");
        }
        tracing::debug!(target: COMP_SDK, "Updating vic.RobotSettings (source: sdkapp)");
        return reply::text(json);
    }
}
