//! `Debug` output of the two state files that hold credentials: every secret
//! prints as `<redacted>`, and every other field still prints.

use serde_json::Value;
use wirepod_core::{ApiConfig, BotInfo, BotInfoRobot, BotInfoWire, Env};

fn both_forms(value: &impl std::fmt::Debug) -> [String; 2] {
    [format!("{value:?}"), format!("{value:#?}")]
}

fn assert_redacted(value: &impl std::fmt::Debug, secrets: &[&str], shown: &[&str]) {
    for text in both_forms(value) {
        for secret in secrets {
            assert!(!text.contains(secret), "{secret} printed in {text}");
        }
        for field in shown {
            assert!(text.contains(field), "{field} missing from {text}");
        }
        assert!(text.contains("<redacted>"), "no redaction in {text}");
    }
}

#[test]
fn api_config_and_env_print_no_key_or_client_id() {
    let mut config = ApiConfig::default();
    config.weather.provider = "fake-weather-provider".to_string();
    config.weather.key = "fake-weather-key-0001".to_string();
    config.knowledge.provider = "fake-llm-provider".to_string();
    config.knowledge.key = "fake-llm-key-0002".to_string();
    config.knowledge.id = "fake-client-id-0003".to_string();
    config.knowledge.extra.insert(
        "fake_new_key".to_string(),
        Value::String("fake-extra-secret-0004".to_string()),
    );
    config.stt.provider = "fake-stt-provider".to_string();
    config.extra.insert(
        "fake_top_level".to_string(),
        Value::String("fake-extra-secret-0005".to_string()),
    );
    assert_redacted(
        &config,
        &[
            "fake-weather-key-0001",
            "fake-llm-key-0002",
            "fake-client-id-0003",
            "fake-extra-secret-0004",
            "fake-extra-secret-0005",
        ],
        &[
            "fake-weather-provider",
            "fake-llm-provider",
            "fake-stt-provider",
            "fake_new_key",
            "fake_top_level",
        ],
    );

    let env = Env {
        weatherapi_provider: "fake-weather-provider".to_string(),
        weatherapi_key: "fake-weather-key-0001".to_string(),
        knowledge_id: "fake-client-id-0003".to_string(),
        knowledge_key: "fake-llm-key-0002".to_string(),
        ..Env::default()
    };
    assert_redacted(
        &env,
        &[
            "fake-weather-key-0001",
            "fake-llm-key-0002",
            "fake-client-id-0003",
        ],
        &["fake-weather-provider"],
    );
}

#[test]
fn bot_info_prints_no_guid() {
    let mut robot = BotInfoRobot {
        esn: "fake-esn-0001".to_string(),
        ip_address: "192.0.2.1".to_string(),
        guid: "fake-robot-guid-0002".to_string(),
        activated: true,
        ..BotInfoRobot::default()
    };
    robot.extra.insert(
        "fake_token".to_string(),
        Value::String("fake-extra-secret-0003".to_string()),
    );
    let info = BotInfo {
        global_guid: "fake-global-guid-0004".to_string(),
        robots: vec![robot],
        ..BotInfo::default()
    };
    assert_redacted(
        &info,
        &[
            "fake-robot-guid-0002",
            "fake-extra-secret-0003",
            "fake-global-guid-0004",
        ],
        &["fake-esn-0001", "192.0.2.1", "fake_token"],
    );
    assert_redacted(
        &BotInfoWire::from(&info),
        &["fake-robot-guid-0002", "fake-global-guid-0004"],
        &["fake-esn-0001", "192.0.2.1"],
    );
}
