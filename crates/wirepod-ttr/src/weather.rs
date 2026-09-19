//! Translation of `pkg/wirepod/ttr/weather.go`.
//!
//! Go reads `vars.APIConfig` and the wall clock from globals; here both come
//! from the `AppState` the caller passes. Where Go panics on a failed request
//! or an unexpected reply, this returns the same placeholder Go returns when
//! geocoding finds nothing.

use serde::Deserialize;
use wirepod_core::AppState;
use wirepod_core::timefmt::civil_from_days;
use wirepod_intent::localization as lcztn;

/* TODO:
Create seperate functions for weatherAPI and openweathermap,
create a standard for how weather functions should be created
*/

/// condition, is_forecast, local_datetime, speakable_location_string, temperature, temperature_unit
pub type Weather = (String, String, String, String, String, String);

// Only the fields the code reads are declared in the reply structs.

// *** WEATHERAPI.COM ***

#[derive(Default, Deserialize)]
#[serde(default)]
struct WeatherApiLocation {
    name: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct WeatherApiCondition {
    text: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct WeatherApiCurrent {
    last_updated: String,
    temp_c: f64,
    temp_f: f64,
    condition: WeatherApiCondition,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct WeatherApiResponseStruct {
    location: WeatherApiLocation,
    current: WeatherApiCurrent,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct WeatherApiCladEntry {
    #[serde(rename = "APIValue")]
    api_value: String,
    #[serde(rename = "CladType")]
    clad_type: String,
}

// *** OPENWEATHERMAP.ORG ***

#[derive(Default, Deserialize)]
#[serde(default)]
struct OpenWeatherMapApiGeoCodingStruct {
    name: String,
    lat: f64,
    lon: f64,
    country: String,
}

//2.5 API

// Go's unicode.IsPunct covers every Unicode punctuation category; this covers
// ASCII, general punctuation, CJK and fullwidth, which is what the supported
// languages end a sentence with.
fn is_punct(c: char) -> bool {
    matches!(c,
        '!' | '"' | '#' | '%' | '&' | '\'' | '(' | ')' | '*' | ',' | '-' | '.' | '/' | ':'
        | ';' | '?' | '@' | '[' | '\\' | ']' | '_' | '{' | '}'
        | '\u{00A1}' | '\u{00A7}' | '\u{00AB}' | '\u{00B6}' | '\u{00B7}' | '\u{00BB}' | '\u{00BF}'
        | '\u{2010}'..='\u{2027}' | '\u{2030}'..='\u{205E}'
        | '\u{3001}'..='\u{3003}' | '\u{3008}'..='\u{3011}'
        | '\u{FF01}'..='\u{FF03}' | '\u{FF05}'..='\u{FF0A}' | '\u{FF0C}'..='\u{FF0F}'
        | '\u{FF1A}' | '\u{FF1B}' | '\u{FF1F}' | '\u{FF20}')
}

fn remove_end_punctuation(s: &str) -> &str {
    match s.chars().next_back() {
        Some(last) if is_punct(last) => &s[..s.len() - last.len_utf8()],
        _ => s,
    }
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct WeatherStruct {
    pub id: i64,
    pub main: String,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct OpenWeatherMapSys {
    sunset: i64,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct OpenWeatherMapMain {
    temp: f64,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct OpenWeatherMapApiResponseStruct {
    weather: Vec<WeatherStruct>,
    main: OpenWeatherMapMain,
    dt: i64,
    sys: OpenWeatherMapSys,
    name: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct OpenWeatherMapForecastApiResponseStruct {
    list: Vec<OpenWeatherMapApiResponseStruct>,
}

fn placeholder(condition: &str, location: &str) -> Weather {
    (
        condition.to_string(),
        "false".to_string(),
        "test".to_string(), // preferably local time in UTC ISO 8601 format ("2022-06-15 12:21:22.123")
        location.to_string(), // preferably the processed location
        "120".to_string(),
        "C".to_string(),
    )
}

// Go's time.RFC850, "Monday, 02-Jan-06 15:04:05 MST". The zone is written as a
// numeric offset because no zone abbreviation is available here.
fn rfc850(unix_secs: i64, utc_offset_secs: i32) -> String {
    const WEEKDAYS: [&str; 7] = [
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
    ];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let local = unix_secs + i64::from(utc_offset_secs);
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let date = civil_from_days(days);
    let sign = if utc_offset_secs < 0 { '-' } else { '+' };
    let offset = utc_offset_secs.unsigned_abs();
    format!(
        "{}, {:02}-{}-{:02} {:02}:{:02}:{:02} {}{:02}{:02}",
        WEEKDAYS[days.rem_euclid(7) as usize],
        date.day,
        MONTHS[(date.month - 1) as usize],
        date.year.rem_euclid(100),
        secs / 3600,
        secs % 3600 / 60,
        secs % 60,
        sign,
        offset / 3600,
        offset % 3600 / 60,
    )
}

pub async fn get_weather(
    state: &AppState,
    location: &str,
    bot_units: &str,
    hours_from_now: i64,
) -> Weather {
    let weather_enabled;
    let mut condition = String::new();
    let mut is_forecast = String::new();
    let mut local_datetime = String::new();
    let mut speakable_location_string = String::new();
    let mut temperature = String::new();
    let mut temperature_unit = String::new();
    let config = state.config();
    let weather_api_enabled = config.weather.enable;
    let weather_api_key = config.weather.key.clone();
    let mut weather_api_unit = config.weather.unit.clone();
    let weather_api_provider = config.weather.provider.clone();
    if weather_api_enabled && !weather_api_key.is_empty() {
        weather_enabled = true;
        tracing::info!(comp = "", "Weather API enabled");
    } else {
        weather_enabled = false;
        tracing::info!(comp = "", "Weather API not enabled, using placeholder");
        if weather_api_enabled && weather_api_key.is_empty() {
            tracing::info!(
                comp = "",
                "Weather API enabled, but Weather API key not set"
            );
        }
    }
    if weather_enabled {
        if !bot_units.is_empty() {
            match bot_units {
                "F" => {
                    tracing::info!(comp = "", "Weather units set to F");
                    weather_api_unit = "F".to_string();
                }
                "C" => {
                    tracing::info!(comp = "", "Weather units set to C");
                    weather_api_unit = "C".to_string();
                }
                _ => {}
            }
        } else if weather_api_unit != "F" && weather_api_unit != "C" {
            tracing::info!(comp = "", "Weather API unit not set, using F");
            weather_api_unit = "F".to_string();
        }
    }

    if weather_enabled {
        let client = reqwest::Client::new();
        if weather_api_provider == "weatherapi.com" {
            let params = [
                ("key", weather_api_key.as_str()),
                ("q", location),
                ("aqi", "no"),
            ];
            let url = "http://api.weatherapi.com/v1/current.json";
            let resp = match client.post(url).form(&params).send().await {
                Ok(resp) => resp,
                Err(err) => {
                    tracing::info!(comp = "", "{err}");
                    return placeholder("undefined", location);
                }
            };
            let weather_response = resp.text().await.unwrap_or_default();
            let map_path = state.paths().assets().weather_map_path();
            let json_file = std::fs::read(map_path).unwrap_or_default();
            let weather_api_clad_map: Vec<WeatherApiCladEntry> =
                serde_json::from_slice(&json_file).unwrap_or_default();
            let weather_struct: WeatherApiResponseStruct =
                serde_json::from_str(&weather_response).unwrap_or_default();
            let mut matched_value = false;
            for b in &weather_api_clad_map {
                if b.api_value == weather_struct.current.condition.text {
                    condition = b.clad_type.clone();
                    tracing::info!(
                        comp = "",
                        "API Value: {}, Clad Type: {}",
                        b.api_value,
                        b.clad_type
                    );
                    matched_value = true;
                    break;
                }
            }
            if !matched_value {
                condition = weather_struct.current.condition.text.clone();
            }
            is_forecast = "false".to_string();
            local_datetime = weather_struct.current.last_updated;
            speakable_location_string = weather_struct.location.name;
            if weather_api_unit == "C" {
                temperature = (weather_struct.current.temp_c as i64).to_string();
                temperature_unit = "C".to_string();
            } else {
                temperature = (weather_struct.current.temp_f as i64).to_string();
                temperature_unit = "F".to_string();
            }
        } else if weather_api_provider == "openweathermap.org" {
            // First use geocoding api to convert location into coordinates
            // E.G. http://api.openweathermap.org/geo/1.0/direct?q={city name},{state code},{country code}&limit={limit}&appid={API key}
            let Ok(url) = reqwest::Url::parse_with_params(
                "http://api.openweathermap.org/geo/1.0/direct",
                &[
                    ("q", location),
                    ("limit", "1"),
                    ("appid", weather_api_key.as_str()),
                ],
            ) else {
                return placeholder("undefined", location);
            };
            let resp = match client.get(url).send().await {
                Ok(resp) => resp,
                Err(err) => {
                    tracing::info!(comp = "", "{err}");
                    return placeholder("undefined", location);
                }
            };
            let geo_coding_response = resp.text().await.unwrap_or_default();

            let geo_coding_info_struct: Vec<OpenWeatherMapApiGeoCodingStruct> =
                match serde_json::from_str(&geo_coding_response) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        tracing::info!(comp = "", "{err}");
                        tracing::info!(comp = "", "Geolocation API error: {geo_coding_response}");
                        Vec::new()
                    }
                };
            if geo_coding_info_struct.is_empty() {
                tracing::info!(comp = "", "Geo provided no response.");
                return placeholder("undefined", location);
            }
            let lat = format!("{:.6}", geo_coding_info_struct[0].lat);
            let lon = format!("{:.6}", geo_coding_info_struct[0].lon);

            tracing::info!(comp = "", "Lat: {lat}, Lon: {lon}");
            tracing::info!(comp = "", "Name: {}", geo_coding_info_struct[0].name);
            tracing::info!(comp = "", "Country: {}", geo_coding_info_struct[0].country);

            // Now that we have Lat and Lon, let's query the weather
            let mut units = "metric";
            if weather_api_unit == "F" {
                units = "imperial";
            }
            let url = if hours_from_now == 0 {
                format!(
                    "https://api.openweathermap.org/data/2.5/weather?lat={lat}&lon={lon}&units={units}&appid={weather_api_key}"
                )
            } else {
                format!(
                    "https://api.openweathermap.org/data/2.5/forecast?lat={lat}&lon={lon}&units={units}&appid={weather_api_key}"
                )
            };
            let resp = match client.get(url).send().await {
                Ok(resp) => resp,
                Err(err) => {
                    tracing::info!(comp = "", "{err}");
                    return placeholder("undefined", location);
                }
            };
            let weather_response = resp.text().await.unwrap_or_default();

            let parsed = if hours_from_now > 0 {
                // Forecast request: free API results are returned in 3 hours slots
                serde_json::from_str::<OpenWeatherMapForecastApiResponseStruct>(&weather_response)
                    .ok()
                    .and_then(|forecast| forecast.list.get((hours_from_now / 3) as usize).cloned())
            } else {
                // Current weather request
                serde_json::from_str::<OpenWeatherMapApiResponseStruct>(&weather_response).ok()
            };
            let Some(open_weather_map_api_response) = parsed else {
                tracing::info!(comp = "", "Weather API error: {weather_response}");
                return placeholder("undefined", location);
            };
            let Some(first_weather) = open_weather_map_api_response.weather.first() else {
                tracing::info!(comp = "", "Weather API error: {weather_response}");
                return placeholder("undefined", location);
            };

            let condition_code = first_weather.id;
            tracing::info!(comp = "", "{condition_code}");

            if condition_code < 300 {
                // Thunderstorm
                condition = "Thunderstorms".to_string();
            } else if condition_code < 400 {
                // Drizzle
                condition = "Rain".to_string();
            } else if condition_code < 600 {
                // Rain
                condition = "Rain".to_string();
            } else if condition_code < 700 {
                // Snow
                condition = "Snow".to_string();
            } else if condition_code < 800 {
                // Athmosphere
                if first_weather.main == "Mist" || first_weather.main == "Fog" {
                    condition = "Rain".to_string();
                } else {
                    condition = "Windy".to_string();
                }
            } else if condition_code == 800 {
                // Clear
                if open_weather_map_api_response.dt < open_weather_map_api_response.sys.sunset {
                    condition = "Sunny".to_string();
                } else {
                    condition = "Stars".to_string();
                }
            } else if condition_code < 900 {
                // Cloud
                condition = "Cloudy".to_string();
            } else {
                condition = first_weather.main.clone();
            }

            is_forecast = "false".to_string();
            let dt = open_weather_map_api_response.dt;
            local_datetime = rfc850(dt, state.wall().utc_offset_secs_at(dt));
            tracing::info!(comp = "", "{local_datetime}");
            speakable_location_string = open_weather_map_api_response.name.clone();
            temperature = format!("{:.6}", open_weather_map_api_response.main.temp.round());
            if weather_api_unit == "C" {
                temperature_unit = "C".to_string();
            } else {
                temperature_unit = "F".to_string();
            }
        }
    } else {
        return placeholder("Snow", location);
    }
    (
        condition,
        is_forecast,
        local_datetime,
        speakable_location_string,
        temperature,
        temperature_unit,
    )
}

// Go's strings.SplitAfter, which keeps a trailing empty piece.
fn split_after<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
    let mut pieces: Vec<&str> = s.split_inclusive(sep).collect();
    if s.ends_with(sep) {
        pieces.push("");
    }
    pieces
}

pub async fn weather_parser(
    state: &AppState,
    speech_text: &str,
    bot_location: &str,
    bot_units: &str,
) -> Weather {
    let language = state.config().stt.language.clone();
    let specific_location;
    let mut speech_location = String::new();
    let mut hours_from_now;
    let weather_in = lcztn::get_text(&language, lcztn::STR_WEATHER_IN);
    if speech_text.contains(weather_in) {
        let split_phrase = split_after(remove_end_punctuation(speech_text), weather_in);
        // Go indexes element 1 without a check.
        speech_location = split_phrase.get(1).unwrap_or(&"").trim().to_string();
        if std::env::var("STT_SERVICE").as_deref() != Ok("whisper.cpp") {
            if split_phrase.len() == 3 {
                speech_location = format!("{speech_location} {}", split_phrase[2].trim());
            } else if split_phrase.len() >= 4 {
                speech_location = format!(
                    "{speech_location} {} {}",
                    split_phrase[2].trim(),
                    split_phrase[3].trim()
                );
            }
            let split_location: Vec<&str> = speech_location.split(' ').collect();
            if split_location.len() == 2 {
                speech_location = format!("{}, {}", split_location[0], split_location[1]);
            } else if split_location.len() == 3 {
                speech_location = format!(
                    "{} {}, {}",
                    split_location[0], split_location[1], split_location[2]
                );
            }
        }
        tracing::info!(
            comp = "",
            "Location parsed from speech: `{speech_location}`"
        );
        specific_location = true;
    } else {
        tracing::info!(comp = "", "No location parsed from speech");
        specific_location = false;
    }
    hours_from_now = 0;
    let now = state.wall().now();
    let local = now.unix_secs + i64::from(state.wall().utc_offset_secs_at(now.unix_secs));
    let hours = local.rem_euclid(86_400) / 3600;
    if speech_text.contains(lcztn::get_text(
        &language,
        lcztn::STR_WEATHER_THIS_AFTERNOON,
    )) {
        if hours < 14 {
            hours_from_now = 14 - hours;
        }
    } else if speech_text.contains(lcztn::get_text(&language, lcztn::STR_WEATHER_TONIGHT)) {
        if hours < 20 {
            hours_from_now = 20 - hours;
        }
    } else if speech_text.contains(lcztn::get_text(
        &language,
        lcztn::STR_WEATHER_THE_DAY_AFTER_TOMORROW,
    )) {
        hours_from_now = 24 - hours + 24 + 9;
    } else if speech_text.contains(lcztn::get_text(&language, lcztn::STR_WEATHER_FORECAST))
        || speech_text.contains(lcztn::get_text(&language, lcztn::STR_WEATHER_TOMORROW))
    {
        hours_from_now = 24 - hours + 9;
    }
    tracing::info!(
        comp = "",
        "Looking for forecast {hours_from_now} hours from now..."
    );

    let api_location = if specific_location {
        speech_location.as_str()
    } else {
        bot_location
    };
    // call to weather API
    get_weather(state, api_location, bot_units, hours_from_now).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_punctuation_is_removed_once() {
        assert_eq!(
            remove_end_punctuation("weather in paris?"),
            "weather in paris"
        );
        assert_eq!(remove_end_punctuation("巴黎的天气。"), "巴黎的天气");
        assert_eq!(remove_end_punctuation("paris"), "paris");
        assert_eq!(remove_end_punctuation(""), "");
    }

    #[test]
    fn split_after_keeps_the_separator_and_a_trailing_empty_piece() {
        assert_eq!(
            split_after("weather in new york", " in "),
            vec!["weather in ", "new york"]
        );
        assert_eq!(split_after("weather in ", " in "), vec!["weather in ", ""]);
    }

    #[test]
    fn rfc850_matches_go() {
        // time.Unix(1700000000, 0).UTC().Format(time.RFC850) is
        // "Tuesday, 14-Nov-23 22:13:20 UTC".
        assert_eq!(
            rfc850(1_700_000_000, 0),
            "Tuesday, 14-Nov-23 22:13:20 +0000"
        );
        assert_eq!(
            rfc850(1_700_000_000, -8 * 3600),
            "Tuesday, 14-Nov-23 14:13:20 -0800"
        );
    }

    #[tokio::test]
    async fn a_disabled_weather_api_answers_the_placeholder() {
        use std::sync::Arc;
        use wirepod_core::RobotConn;
        use wirepod_core::test_support::{FakeConnFactory, FakeRobotConn};

        let conn: Arc<dyn RobotConn> = Arc::new(FakeRobotConn::new());
        let state = AppState::builder(Arc::new(FakeConnFactory::connecting_to(conn))).build();
        let weather = weather_parser(&state, "what is the weather in london", "", "").await;
        assert_eq!(
            weather,
            (
                "Snow".to_string(),
                "false".to_string(),
                "test".to_string(),
                "london".to_string(),
                "120".to_string(),
                "C".to_string()
            )
        );
    }
}
