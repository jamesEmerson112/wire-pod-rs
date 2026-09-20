//! The photo arms of Go's `sdkapp/server.go`: `get_image_ids`, `get_image`,
//! `get_image_thumb` and `delete_image`.

use axum::body::Body;
use axum::response::Response;
use http::{HeaderValue, header};
use wirepod_core::RobotEntry;
use wirepod_core::logger::COMP_SDK;
use wirepod_proto::anki::vector::external_interface as pb;
use wirepod_vector::status_error;

use crate::sdkapp::{NO_SDK_CLIENT, sdk_client};
use crate::{literals, reply};

/// Go's `strconv.Atoi`, error text included, because the failure is written
/// into the response body.
fn atoi(raw: &str) -> Result<i64, String> {
    raw.parse::<i64>().map_err(|_| {
        let reason = match raw.strip_prefix(['+', '-']).unwrap_or(raw) {
            digits if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
                "value out of range"
            }
            _ => "invalid syntax",
        };
        format!("strconv.Atoi: parsing {raw:?}: {reason}")
    })
}

/// Go's `w.Write(resp.Image)`, whose content type is whatever `net/http`
/// sniffs from the first bytes, and which is absent when nothing is written.
fn image(data: Vec<u8>) -> Response {
    if data.is_empty() {
        return reply::empty();
    }
    let sniffed = if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else {
        "application/octet-stream"
    };
    let mut response = Response::new(Body::from(data));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(sniffed));
    response
}

/// `server.go:525-536`. Go discards the RPC error and then dereferences the
/// nil response; the failure becomes an `error: ` body instead.
pub async fn get_image_ids(entry: &RobotEntry) -> Response {
    let Some(mut client) = sdk_client(entry) else {
        return reply::text(format!("{}{NO_SDK_CLIENT}", literals::ERROR_PREFIX));
    };
    let resp = match client.photos_info(pb::PhotosInfoRequest {}).await {
        Ok(resp) => resp.into_inner(),
        Err(status) => {
            let err = status_error(&status);
            tracing::error!(target: COMP_SDK, "photos info: {err}");
            return reply::text(format!("{}{err}", literals::ERROR_PREFIX));
        }
    };
    let ids: Vec<u32> = resp
        .photo_infos
        .iter()
        .map(|photo| photo.photo_id)
        .collect();
    // Go's nil slice marshals as `null`, and the web UI compares against it.
    if ids.is_empty() {
        return reply::text("null");
    }
    match serde_json::to_string(&ids) {
        Ok(body) => reply::text(body),
        Err(_) => reply::text("null"),
    }
}

pub async fn get_image(entry: &RobotEntry, id: &str) -> Response {
    let photo_id = match atoi(id) {
        Ok(photo_id) => photo_id as u32,
        Err(err) => return reply::text(format!("{}{err}", literals::ERROR_PREFIX)),
    };
    let Some(mut client) = sdk_client(entry) else {
        return reply::text(format!("{}{NO_SDK_CLIENT}", literals::ERROR_PREFIX));
    };
    match client.photo(pb::PhotoRequest { photo_id }).await {
        // `resp.Success` is never checked.
        Ok(resp) => image(resp.into_inner().image),
        Err(status) => reply::text(format!(
            "{}{}",
            literals::ERROR_PREFIX,
            status_error(&status)
        )),
    }
}

pub async fn get_image_thumb(entry: &RobotEntry, id: &str) -> Response {
    let photo_id = match atoi(id) {
        Ok(photo_id) => photo_id as u32,
        Err(err) => return reply::text(format!("{}{err}", literals::ERROR_PREFIX)),
    };
    let Some(mut client) = sdk_client(entry) else {
        return reply::text(format!("{}{NO_SDK_CLIENT}", literals::ERROR_PREFIX));
    };
    match client.thumbnail(pb::ThumbnailRequest { photo_id }).await {
        Ok(resp) => image(resp.into_inner().image),
        Err(status) => reply::text(format!(
            "{}{}",
            literals::ERROR_PREFIX,
            status_error(&status)
        )),
    }
}

pub async fn delete_image(entry: &RobotEntry, id: &str) -> Response {
    let photo_id = match atoi(id) {
        Ok(photo_id) => photo_id as u32,
        Err(err) => return reply::text(format!("{}{err}", literals::ERROR_PREFIX)),
    };
    let Some(mut client) = sdk_client(entry) else {
        return reply::text(format!("{}{NO_SDK_CLIENT}", literals::ERROR_PREFIX));
    };
    match client
        .delete_photo(pb::DeletePhotoRequest { photo_id })
        .await
    {
        // The response and its `success` flag are discarded.
        Ok(_) => reply::text(literals::DONE),
        Err(status) => reply::text(format!(
            "{}{}",
            literals::ERROR_PREFIX,
            status_error(&status)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_id_carries_gos_atoi_message() {
        assert_eq!(atoi("12"), Ok(12));
        assert_eq!(
            atoi("abc"),
            Err(r#"strconv.Atoi: parsing "abc": invalid syntax"#.to_owned())
        );
        assert_eq!(
            atoi(""),
            Err(r#"strconv.Atoi: parsing "": invalid syntax"#.to_owned())
        );
        assert_eq!(
            atoi("99999999999999999999"),
            Err(r#"strconv.Atoi: parsing "99999999999999999999": value out of range"#.to_owned())
        );
    }

    /// Go converts the `int` to a `uint32`, so a negative id wraps.
    #[test]
    fn a_negative_id_wraps_the_way_gos_conversion_does() {
        assert_eq!(atoi("-1").expect("parsed") as u32, u32::MAX);
    }
}
