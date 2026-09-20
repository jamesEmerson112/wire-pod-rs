//! The version route of `pkg/wirepod/config-ws/webserver.go`, and the two
//! GitHub lookups it makes.

use axum::response::Response;
use http::StatusCode;
use serde::{Deserialize, Serialize};
use wirepod_core::AppState;

use crate::api::{encode_json, error_text};

/// Go builds both URLs from literals; the host is a parameter here so the
/// failure path can be driven against a closed loopback port.
const GITHUB_API: &str = "https://api.github.com";
const OWNER: &str = "kercre123";
const REPO: &str = "WirePod";

/// Go's `vars.CommitSHA`, which a build-time ldflag sets and nothing here does.
const COMMIT_SHA: &str = "";

// Go's HTTP client sends a user agent of its own; reqwest sends none, and
// GitHub answers 403 to a request without one.
const USER_AGENT: &str = "wire-pod-rs";

#[derive(Serialize)]
struct VersionInfo {
    fromsource: bool,
    installedversion: String,
    installedcommit: String,
    currentversion: String,
    currentcommit: String,
    avail: bool,
}

#[derive(Deserialize)]
struct Commit {
    #[serde(default)]
    sha: String,
}

#[derive(Deserialize)]
struct Release {
    #[serde(default)]
    tag_name: String,
}

pub async fn get_version_info(state: &AppState) -> Response {
    let installed_ver = std::fs::read(state.paths().assets().version_file())
        .map(|ver| String::from_utf8_lossy(&ver).trim().to_owned())
        .unwrap_or_default();
    let current_ver = match get_latest_release_tag(GITHUB_API, OWNER, REPO).await {
        Ok(tag) => tag,
        Err(err) => {
            return error_text(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("error communicating with github (ver): {err}\n"),
            );
        }
    };
    let current_commit = match get_latest_commit_sha(GITHUB_API).await {
        Ok(sha) => sha,
        Err(err) => {
            return error_text(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("error communicating with github (commit): {err}\n"),
            );
        }
    };
    let from_source = installed_ver.is_empty();
    let update_available = if from_source {
        COMMIT_SHA != current_commit.trim()
    } else {
        installed_ver != current_ver.trim()
    };
    encode_json(&VersionInfo {
        fromsource: from_source,
        installedversion: installed_ver,
        installedcommit: COMMIT_SHA.to_owned(),
        currentversion: current_ver.trim().to_owned(),
        currentcommit: current_commit.trim().to_owned(),
        avail: update_available,
    })
}

pub async fn get_latest_commit_sha(api: &str) -> Result<String, String> {
    let response = reqwest::Client::new()
        .get(format!("{api}/repos/kercre123/wire-pod/commits"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(format!("failed to get commits: {}", response.status()));
    }
    let body = response.bytes().await.map_err(|err| err.to_string())?;
    let commits: Vec<Commit> = serde_json::from_slice(&body).map_err(|err| err.to_string())?;
    let Some(first) = commits.first() else {
        return Err("no commits found".to_owned());
    };
    // Go slices the first seven bytes without checking the length.
    first
        .sha
        .get(..7)
        .map(str::to_owned)
        .ok_or_else(|| format!("commit sha is too short: {}", first.sha.len()))
}

pub async fn get_latest_release_tag(api: &str, owner: &str, repo: &str) -> Result<String, String> {
    let response = reqwest::Client::new()
        .get(format!("{api}/repos/{owner}/{repo}/releases/latest"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    let body = response.bytes().await.map_err(|err| err.to_string())?;
    let release: Release = serde_json::from_slice(&body).map_err(|err| err.to_string())?;
    Ok(release.tag_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::body_of;

    /// A loopback port nothing is listening on, so neither lookup leaves this
    /// machine.
    const CLOSED: &str = "http://127.0.0.1:1";

    #[tokio::test]
    async fn the_six_keys_go_out_in_gos_order_with_the_trailing_newline() {
        let reply = encode_json(&VersionInfo {
            fromsource: true,
            installedversion: String::new(),
            installedcommit: String::new(),
            currentversion: "v1.0.0".to_owned(),
            currentcommit: "abcdefg".to_owned(),
            avail: true,
        });
        assert_eq!(
            body_of(reply).await,
            concat!(
                r#"{"fromsource":true,"installedversion":"","installedcommit":"","#,
                r#""currentversion":"v1.0.0","currentcommit":"abcdefg","avail":true}"#,
                "\n"
            )
        );
    }

    #[tokio::test]
    async fn an_unreachable_api_is_an_error_from_both_lookups() {
        assert!(get_latest_release_tag(CLOSED, OWNER, REPO).await.is_err());
        assert!(get_latest_commit_sha(CLOSED).await.is_err());
    }
}
