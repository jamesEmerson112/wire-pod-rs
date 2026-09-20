//! Go's `sdkapp/urlreqs.go`: the `/v1/update_settings` REST calls.
//!
//! Every body is built by string concatenation with the form values dropped in
//! unescaped, exactly as Go builds it.

use wirepod_core::ConnTarget;

/// Go's `transCfg`, which is `InsecureSkipVerify` on the transport.
///
/// Go builds a fresh `http.Client` per call over one shared transport; this
/// builds the whole client per call, because the transport is where the
/// certificate policy lives and reqwest does not split the two.
async fn update_settings(robot: &ConnTarget, body: String) {
    let url = format!("https://{}/v1/update_settings", robot.grpc_target());
    let client = match reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            tracing::error!(target: "sdkapp", comp = "", "update_settings client: {err}");
            return;
        }
    };
    let sent = client
        .post(url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", robot.guid),
        )
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await;
    // Go's `panic(err)`.
    if let Err(err) = sent {
        tracing::error!(target: "sdkapp", comp = "", "update_settings: {err}");
    }
}

fn custom_eye_color_body(hue: &str, sat: &str) -> String {
    format!(
        r#"{{"update_settings": true, "settings": {{"custom_eye_color": {{"enabled": true, "hue": {hue}, "saturation": {sat}}} }} }}"#
    )
}

fn preset_eye_color_body(value: &str) -> String {
    format!(
        r#"{{"update_settings": true, "settings": {{"custom_eye_color": {{"enabled": false}}, "eye_color": {value}}} }}"#
    )
}

fn string_body(setting: &str, value: &str) -> String {
    format!(r#"{{"update_settings": true, "settings": {{"{setting}": "{value}" }} }}"#)
}

fn intbool_body(setting: &str, value: &str) -> String {
    format!(r#"{{"update_settings": true, "settings": {{"{setting}": {value} }} }}"#)
}

pub async fn set_custom_eye_color(robot: &ConnTarget, hue: &str, sat: &str) {
    update_settings(robot, custom_eye_color_body(hue, sat)).await;
}

pub async fn set_preset_eye_color(robot: &ConnTarget, value: &str) {
    update_settings(robot, preset_eye_color_body(value)).await;
}

pub async fn set_setting_sdk_string(robot: &ConnTarget, setting: &str, value: &str) {
    update_settings(robot, string_body(setting, value)).await;
}

pub async fn set_setting_sdk_intbool(robot: &ConnTarget, setting: &str, value: &str) {
    update_settings(robot, intbool_body(setting, value)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bodies_are_gos_concatenations() {
        assert_eq!(
            custom_eye_color_body("0.5", "1"),
            r#"{"update_settings": true, "settings": {"custom_eye_color": {"enabled": true, "hue": 0.5, "saturation": 1} } }"#
        );
        assert_eq!(
            preset_eye_color_body("3"),
            r#"{"update_settings": true, "settings": {"custom_eye_color": {"enabled": false}, "eye_color": 3} }"#
        );
        assert_eq!(
            string_body("time_zone", "America/Los_Angeles"),
            r#"{"update_settings": true, "settings": {"time_zone": "America/Los_Angeles" } }"#
        );
        assert_eq!(
            intbool_body("master_volume", "4"),
            r#"{"update_settings": true, "settings": {"master_volume": 4 } }"#
        );
    }

    /// Go interpolates the form value straight into the JSON, so a value with a
    /// quote in it produces a malformed body rather than an escaped one.
    #[test]
    fn a_value_is_never_escaped() {
        assert_eq!(
            string_body("locale", "en\"US"),
            r#"{"update_settings": true, "settings": {"locale": "en"US" } }"#
        );
    }
}
