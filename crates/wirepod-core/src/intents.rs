//! Translation of the intent types and loaders in `pkg/vars/vars.go`.
//!
//! Go keeps the results in the package globals `IntentList`, `CustomIntents`,
//! `CustomIntentsExist` and `DownloadedVoskModels`; here each loader returns
//! its value and the caller stores it.

use serde::{Deserialize, Deserializer, Serialize};

use crate::paths::{AssetDir, DataDir};

// Go writes a nil slice as `null` and reads `null` back as the zero value.
fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JsonIntent {
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub keyphrases: Vec<String>,
    #[serde(rename = "requiresexact", deserialize_with = "null_default")]
    pub require_exact_match: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomIntentParams {
    #[serde(rename = "paramname", deserialize_with = "null_default")]
    pub param_name: String,
    #[serde(rename = "paramvalue", deserialize_with = "null_default")]
    pub param_value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomIntent {
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub description: String,
    #[serde(deserialize_with = "null_default")]
    pub utterances: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    pub intent: String,
    #[serde(deserialize_with = "null_default")]
    pub params: CustomIntentParams,
    #[serde(deserialize_with = "null_default")]
    pub exec: String,
    #[serde(rename = "execargs", deserialize_with = "null_default")]
    pub exec_args: Vec<String>,
    #[serde(rename = "issystem", deserialize_with = "null_default")]
    pub is_system_intent: bool,
    #[serde(rename = "luascript", deserialize_with = "null_default")]
    pub lua_script: String,
}

pub fn get_downloaded_vosk_models(data: &DataDir) -> Vec<String> {
    let mut downloaded_vosk_models = Vec::new();
    let array = match std::fs::read_dir(data.vosk_model_dir()) {
        Ok(array) => array,
        Err(err) => {
            tracing::info!(comp = "", "{err}");
            return downloaded_vosk_models;
        }
    };
    for dir in array.flatten() {
        downloaded_vosk_models.push(dir.file_name().to_string_lossy().into_owned());
    }
    // os.ReadDir returns entries sorted by filename.
    downloaded_vosk_models.sort();
    downloaded_vosk_models
}

/// `None` is Go's `CustomIntentsExist == false`.
pub fn load_custom_intents(data: &DataDir) -> Option<Vec<CustomIntent>> {
    let json_bytes = std::fs::read(data.custom_intents_path()).ok()?;
    let custom_intents: Vec<CustomIntent> = serde_json::from_slice(&json_bytes).unwrap_or_default();
    tracing::info!(comp = "", "Loaded custom intents:");
    for intent in &custom_intents {
        tracing::info!(comp = "", "{}", intent.name);
    }
    Some(custom_intents)
}

pub fn load_intents(assets: &AssetDir, language: &str) -> std::io::Result<Vec<JsonIntent>> {
    let json_file = std::fs::read(assets.intent_data_path(language))?;
    let json_intents: Vec<JsonIntent> = serde_json::from_slice(&json_file)?;
    Ok(json_intents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vendored_locale_loads() {
        let assets = AssetDir::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets"));
        for language in [
            "en-US", "it-IT", "es-ES", "fr-FR", "de-DE", "pt-BR", "pl-PL", "zh-CN", "tr-TR",
            "ru-RU", "nt-NL", "uk-UA", "vi-VN", "ko-KR",
        ] {
            let intents = load_intents(&assets, language).unwrap();
            assert!(!intents.is_empty(), "{language}");
            assert!(intents.iter().all(|i| !i.name.is_empty()), "{language}");
        }
        assert!(load_intents(&assets, "xx-XX").is_err());
    }

    #[test]
    fn a_custom_intent_written_by_go_reads_back() {
        let json = r#"[{"name":"n","description":"d","utterances":["hello"],"intent":"intent_x",
            "params":{"paramname":"","paramvalue":""},"exec":"","execargs":null,
            "issystem":false,"luascript":""}]"#;
        let parsed: Vec<CustomIntent> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed[0].utterances, vec!["hello"]);
        assert!(parsed[0].exec_args.is_empty());
    }
}
