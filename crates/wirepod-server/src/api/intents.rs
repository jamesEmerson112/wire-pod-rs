//! The custom-intent routes of `pkg/wirepod/config-ws/webserver.go`.

use std::sync::PoisonError;

use axum::response::Response;
use http::StatusCode;
use serde::Deserialize;
use wirepod_core::AppState;
use wirepod_core::gojson::go_marshal;
use wirepod_core::intents::CustomIntent;

use crate::api::error_text;
use crate::reply;

const INVALID_BODY: &str = "invalid request body\n";
const MISSING_FIELD: &str =
    "missing required field (name, description, utterances, and intent are required)\n";
const INVALID_NUMBER: &str = "invalid intent number\n";

/// Go embeds `vars.CustomIntent` beside `number`; the flatten is that
/// embedding.
#[derive(Deserialize)]
struct EditRequest {
    #[serde(default)]
    number: i64,
    #[serde(flatten)]
    intent: CustomIntent,
}

#[derive(Deserialize)]
struct NumberRequest {
    #[serde(default)]
    number: i64,
}

pub fn add_custom_intent(state: &AppState, body: &[u8]) -> Response {
    let Ok(mut intent) = serde_json::from_slice::<CustomIntent>(body) else {
        return error_text(StatusCode::BAD_REQUEST, INVALID_BODY);
    };
    if any_empty(&[&intent.name, &intent.description, &intent.intent])
        || intent.utterances.is_empty()
    {
        return error_text(StatusCode::BAD_REQUEST, MISSING_FIELD);
    }
    intent.lua_script = intent.lua_script.trim().to_owned();
    // TODO(M5): scripting.ValidateLuaScript(intent.LuaScript)
    let mut intents = state
        .custom_intents()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    intents.get_or_insert_with(Vec::new).push(intent);
    save_custom_intents(state, intents.as_deref().unwrap_or_default());
    reply::text("Intent added successfully.")
}

pub fn edit_custom_intent(state: &AppState, body: &[u8]) -> Response {
    let Ok(request) = serde_json::from_slice::<EditRequest>(body) else {
        return error_text(StatusCode::BAD_REQUEST, INVALID_BODY);
    };
    let mut intents = state
        .custom_intents()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    // A list Go has never loaded has length zero, so every number is invalid.
    let Some(list) = intents.as_mut() else {
        return error_text(StatusCode::BAD_REQUEST, INVALID_NUMBER);
    };
    if request.number < 1 || request.number > list.len() as i64 {
        return error_text(StatusCode::BAD_REQUEST, INVALID_NUMBER);
    }
    let intent = &mut list[request.number as usize - 1];
    let edit = request.intent;
    if !edit.name.is_empty() {
        intent.name = edit.name;
    }
    if !edit.description.is_empty() {
        intent.description = edit.description;
    }
    if !edit.utterances.is_empty() {
        intent.utterances = edit.utterances;
    }
    if !edit.intent.is_empty() {
        intent.intent = edit.intent;
    }
    if !edit.params.param_name.is_empty() {
        intent.params.param_name = edit.params.param_name;
    }
    if !edit.params.param_value.is_empty() {
        intent.params.param_value = edit.params.param_value;
    }
    if !edit.exec.is_empty() {
        intent.exec = edit.exec;
    }
    if !edit.lua_script.is_empty() {
        intent.lua_script = edit.lua_script;
        // TODO(M5): scripting.ValidateLuaScript(intent.LuaScript)
    }
    if !edit.exec_args.is_empty() {
        intent.exec_args = edit.exec_args;
    }
    intent.is_system_intent = false;
    save_custom_intents(state, list);
    reply::text("Intent edited successfully.")
}

pub fn get_custom_intents_json(state: &AppState) -> Response {
    let exists = state
        .custom_intents()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some();
    if !exists {
        return error_text(StatusCode::BAD_REQUEST, "you must create an intent first\n");
    }
    match std::fs::read(state.paths().data().custom_intents_path()) {
        Ok(file) => reply::json(String::from_utf8_lossy(&file).into_owned()),
        Err(err) => {
            let response = error_text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not read custom intents file\n",
            );
            tracing::debug!(comp = "", "{err}");
            response
        }
    }
}

pub fn remove_custom_intent(state: &AppState, body: &[u8]) -> Response {
    let Ok(request) = serde_json::from_slice::<NumberRequest>(body) else {
        return error_text(StatusCode::BAD_REQUEST, INVALID_BODY);
    };
    let mut intents = state
        .custom_intents()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let Some(list) = intents.as_mut() else {
        return error_text(StatusCode::BAD_REQUEST, INVALID_NUMBER);
    };
    if request.number < 1 || request.number > list.len() as i64 {
        return error_text(StatusCode::BAD_REQUEST, INVALID_NUMBER);
    }
    list.remove(request.number as usize - 1);
    save_custom_intents(state, list);
    reply::text("Intent removed successfully.")
}

/// Go discards both errors (`webserver.go:411-412`); they are logged here.
fn save_custom_intents(state: &AppState, intents: &[CustomIntent]) {
    let Ok(file) = go_marshal(&intents) else {
        return;
    };
    if let Err(err) = std::fs::write(state.paths().data().custom_intents_path(), file) {
        tracing::debug!(comp = "", "{err}");
    }
}

fn any_empty(values: &[&str]) -> bool {
    values.iter().any(|value| value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{body_of, test_dir, test_state};

    const ONE: &[u8] =
        br#"{"name":"n","description":"d","utterances":["hey"],"intent":"intent_x"}"#;

    #[tokio::test]
    async fn add_edit_and_remove_round_trip_through_the_file() {
        let dir = test_dir("intents");
        let state = test_state(&dir);
        let path = state.paths().data().custom_intents_path();

        let reply = add_custom_intent(&state, ONE);
        assert_eq!(reply.status(), StatusCode::OK, "a complete intent is taken");
        assert_eq!(body_of(reply).await, "Intent added successfully.");
        assert!(std::fs::read_to_string(&path).unwrap().contains("intent_x"));

        assert_eq!(
            edit_custom_intent(&state, br#"{"number":1,"name":"renamed"}"#).status(),
            StatusCode::OK
        );
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("renamed") && saved.contains("hey"));

        assert_eq!(
            remove_custom_intent(&state, br#"{"number":1}"#).status(),
            StatusCode::OK
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_rejections_the_web_ui_sees() {
        let dir = test_dir("intents-reject");
        let state = test_state(&dir);

        // No intent yet, so the list does not exist.
        assert_eq!(
            get_custom_intents_json(&state).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            add_custom_intent(&state, br#"{"name":"n"}"#).status(),
            StatusCode::BAD_REQUEST,
            "a missing description, utterance list or intent is refused"
        );
        assert_eq!(
            add_custom_intent(&state, b"not json").status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            remove_custom_intent(&state, br#"{"number":1}"#).status(),
            StatusCode::BAD_REQUEST
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
