//! The face arms of Go's `sdkapp/server.go`: `get_faces`, `rename_face`,
//! `delete_face` and `add_face`.

use axum::response::Response;
use serde::Serialize;
use wirepod_core::RobotEntry;
use wirepod_proto::anki::vector::external_interface as pb;
use wirepod_vector::status_error;

use crate::sdkapp::{NO_SDK_CLIENT, sdk_client};
use crate::{literals, reply};

/// The intent Go sends to start face enrollment.
const MEET_VICTOR: &str = "intent_meet_victor";

/// One `LoadedKnownFace` as `json.Marshal` renders the generated Go struct:
/// the proto field names, every one `omitempty`.
#[derive(Serialize)]
struct Face<'a> {
    #[serde(skip_serializing_if = "zero_i64")]
    seconds_since_first_enrolled: i64,
    #[serde(skip_serializing_if = "zero_i64")]
    seconds_since_last_updated: i64,
    #[serde(skip_serializing_if = "zero_i64")]
    seconds_since_last_seen: i64,
    #[serde(skip_serializing_if = "zero_i64")]
    last_seen_seconds_since_epoch: i64,
    #[serde(skip_serializing_if = "zero_i32")]
    face_id: i32,
    #[serde(skip_serializing_if = "empty")]
    name: &'a str,
}

fn zero_i64(value: &i64) -> bool {
    *value == 0
}

fn zero_i32(value: &i32) -> bool {
    *value == 0
}

fn empty(value: &&str) -> bool {
    value.is_empty()
}

/// Go's `json.Marshal(resp.Faces)`, whose nil slice is `null` rather than `[]`.
fn faces_json(faces: &[pb::LoadedKnownFace]) -> String {
    if faces.is_empty() {
        return "null".to_owned();
    }
    let wire: Vec<Face<'_>> = faces
        .iter()
        .map(|face| Face {
            seconds_since_first_enrolled: face.seconds_since_first_enrolled,
            seconds_since_last_updated: face.seconds_since_last_updated,
            seconds_since_last_seen: face.seconds_since_last_seen,
            last_seen_seconds_since_epoch: face.last_seen_seconds_since_epoch,
            face_id: face.face_id,
            name: &face.name,
        })
        .collect();
    serde_json::to_string(&wire).unwrap_or_else(|_| "null".to_owned())
}

/// Go's `strconv.Atoi` with the error discarded, then narrowed to `int32`.
fn face_id(raw: &str) -> i32 {
    raw.parse::<i64>().unwrap_or(0) as i32
}

pub async fn get_faces(entry: &RobotEntry) -> Response {
    let Some(mut client) = sdk_client(entry) else {
        return reply::text(NO_SDK_CLIENT);
    };
    match client
        .request_enrolled_names(pb::RequestEnrolledNamesRequest {})
        .await
    {
        Ok(resp) => reply::text(faces_json(&resp.into_inner().faces)),
        // The bare error text, with no `error: ` prefix.
        Err(status) => reply::text(status_error(&status).to_string()),
    }
}

pub async fn rename_face(entry: &RobotEntry, id: &str, oldname: &str, newname: &str) -> Response {
    let Some(mut client) = sdk_client(entry) else {
        return reply::text(NO_SDK_CLIENT);
    };
    match client
        .update_enrolled_face_by_id(pb::UpdateEnrolledFaceByIdRequest {
            face_id: face_id(id),
            old_name: oldname.to_owned(),
            new_name: newname.to_owned(),
        })
        .await
    {
        Ok(_) => reply::text(literals::SUCCESS),
        Err(status) => reply::text(status_error(&status).to_string()),
    }
}

pub async fn delete_face(entry: &RobotEntry, id: &str) -> Response {
    let Some(mut client) = sdk_client(entry) else {
        return reply::text(NO_SDK_CLIENT);
    };
    match client
        .erase_enrolled_face_by_id(pb::EraseEnrolledFaceByIdRequest {
            face_id: face_id(id),
        })
        .await
    {
        Ok(_) => reply::text(literals::SUCCESS),
        Err(status) => reply::text(status_error(&status).to_string()),
    }
}

pub async fn add_face(entry: &RobotEntry, name: &str) -> Response {
    let Some(mut client) = sdk_client(entry) else {
        return reply::text(NO_SDK_CLIENT);
    };
    match client
        .app_intent(pb::AppIntentRequest {
            intent: MEET_VICTOR.to_owned(),
            param: name.to_owned(),
        })
        .await
    {
        Ok(_) => reply::text(literals::SUCCESS),
        Err(status) => reply::text(status_error(&status).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_face_list_is_null_and_a_zero_field_is_dropped() {
        assert_eq!(faces_json(&[]), "null");
        assert_eq!(
            faces_json(&[pb::LoadedKnownFace {
                seconds_since_last_seen: 12,
                face_id: 1,
                name: "James".to_owned(),
                ..pb::LoadedKnownFace::default()
            }]),
            r#"[{"seconds_since_last_seen":12,"face_id":1,"name":"James"}]"#
        );
    }

    #[test]
    fn a_bad_face_id_is_zero_the_way_gos_discarded_atoi_is() {
        assert_eq!(face_id("7"), 7);
        assert_eq!(face_id(""), 0);
        assert_eq!(face_id("nine"), 0);
    }
}
