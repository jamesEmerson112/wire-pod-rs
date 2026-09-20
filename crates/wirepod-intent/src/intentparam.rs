//! Translation of `pkg/wirepod/ttr/intentparam.go`.
//!
//! Go reads the robot's `vic.RobotSettings` jdoc through `vars.GetJdoc` at the
//! top of two of these functions; here [`bot_location_and_units`] does that
//! parse and the caller puts the result in [`IntentContext`].

use std::collections::HashMap;

use serde::Deserialize;

use crate::localization as lcztn;
use crate::match_intent_send::{IntentContext, IntentSink, intent_pass, one_param};
use crate::words2num::words2num;

/// The fields `robotSettingsJson` declares that the two param checkers read.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RobotSettingsJson {
    default_location: String,
    temp_is_fahrenheit: bool,
}

/// Go's `vars.GetJdoc("vic:"+botSerial, "vic.RobotSettings")` block, which
/// `ParamChecker` and `ParamCheckerSlotsEnUS` open with. `None` is Go's
/// "jdoc does not exist"; the defaults are Go's.
pub fn bot_location_and_units(robot_settings_json: Option<&str>) -> (String, String) {
    let mut bot_location = "San Francisco".to_owned();
    let mut bot_units = "F".to_owned();
    if let Some(json_doc) = robot_settings_json {
        match serde_json::from_str::<RobotSettingsJson>(json_doc) {
            Err(err) => {
                tracing::debug!(comp = "", "Error unmarshaling json in paramchecker");
                tracing::debug!(comp = "", "{err}");
            }
            Ok(robot_settings) => {
                bot_location = robot_settings.default_location;
                bot_units = if robot_settings.temp_is_fahrenheit {
                    "F".to_owned()
                } else {
                    "C".to_owned()
                };
            }
        }
    }
    (bot_location, bot_units)
}

// Go's strings.SplitAfter, which keeps the separator on each piece and a
// trailing empty piece.
fn split_after<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
    if sep.is_empty() {
        return s.split("").filter(|piece| !piece.is_empty()).collect();
    }
    let mut pieces: Vec<&str> = s.split_inclusive(sep).collect();
    if s.ends_with(sep) {
        pieces.push("");
    }
    pieces
}

/// The `splitPhrase` block the username and given-name branches repeat six
/// times. Go indexes `splitPhrase[1]` with no length check; an absent element
/// reads as the empty string here. Go's `len == 4` and `len > 4` arms are
/// identical, so they are one arm.
fn split_name(speech_text: &str, splitter: &str) -> String {
    let split_phrase = split_after(speech_text, splitter);
    let mut name = split_phrase.get(1).unwrap_or(&"").trim().to_owned();
    if split_phrase.len() == 3 {
        name = format!("{name} {}", split_phrase[2].trim());
    } else if split_phrase.len() >= 4 {
        name = format!(
            "{name} {} {}",
            split_phrase[2].trim(),
            split_phrase[3].trim()
        );
    }
    name
}

/// The eye-colour chain, which `ParamChecker` and `prehistoricParamChecker`
/// run identically. `None` is Go's else arm, which clears the parameter and
/// leaves the intent alone.
fn eye_color(language: &str, speech_text: &str) -> Option<&'static str> {
    let text = |key| lcztn::get_text(language, key);
    if speech_text.contains(text(lcztn::STR_EYE_COLOR_PURPLE)) {
        Some("COLOR_PURPLE")
    } else if speech_text.contains(text(lcztn::STR_EYE_COLOR_BLUE))
        || speech_text.contains(text(lcztn::STR_EYE_COLOR_SAPPHIRE))
    {
        Some("COLOR_BLUE")
    } else if speech_text.contains(text(lcztn::STR_EYE_COLOR_YELLOW)) {
        Some("COLOR_YELLOW")
    } else if speech_text.contains(text(lcztn::STR_EYE_COLOR_TEAL))
        || speech_text.contains(text(lcztn::STR_EYE_COLOR_TEAL2))
    {
        Some("COLOR_TEAL")
    } else if speech_text.contains(text(lcztn::STR_EYE_COLOR_GREEN)) {
        Some("COLOR_GREEN")
    } else if speech_text.contains(text(lcztn::STR_EYE_COLOR_ORANGE)) {
        Some("COLOR_ORANGE")
    } else {
        None
    }
}

/// The volume chain, likewise identical in both.
fn volume_level(language: &str, speech_text: &str) -> &'static str {
    let text = |key| lcztn::get_text(language, key);
    if speech_text.contains(text(lcztn::STR_VOLUME_MEDIUM_LOW)) {
        "VOLUME_2"
    } else if speech_text.contains(text(lcztn::STR_VOLUME_LOW))
        || speech_text.contains(text(lcztn::STR_VOLUME_QUIET))
    {
        "VOLUME_1"
    } else if speech_text.contains(text(lcztn::STR_VOLUME_MEDIUM_HIGH)) {
        "VOLUME_4"
    } else if speech_text.contains(text(lcztn::STR_VOLUME_MEDIUM))
        || speech_text.contains(text(lcztn::STR_VOLUME_NORMAL))
        || speech_text.contains(text(lcztn::STR_VOLUME_REGULAR))
    {
        "VOLUME_3"
    } else if speech_text.contains(text(lcztn::STR_VOLUME_HIGH))
        || speech_text.contains(text(lcztn::STR_VOLUME_LOUD))
    {
        "VOLUME_5"
    } else if speech_text.contains(text(lcztn::STR_VOLUME_MUTE))
        || speech_text.contains(text(lcztn::STR_VOLUME_NOTHING))
        || speech_text.contains(text(lcztn::STR_VOLUME_SILENT))
        || speech_text.contains(text(lcztn::STR_VOLUME_OFF))
        || speech_text.contains(text(lcztn::STR_VOLUME_ZERO))
    {
        // there is no VOLUME_0 :(
        "VOLUME_1"
    } else {
        "VOLUME_1"
    }
}

fn weather_params(weather: crate::match_intent_send::Weather) -> HashMap<String, String> {
    let (condition, is_forecast, local_datetime, speakable_location_string, temperature, unit) =
        weather;
    HashMap::from([
        ("condition".to_owned(), condition),
        ("is_forecast".to_owned(), is_forecast),
        ("local_datetime".to_owned(), local_datetime),
        (
            "speakable_location_string".to_owned(),
            speakable_location_string,
        ),
        ("temperature".to_owned(), temperature),
        ("temperature_unit".to_owned(), unit),
    ])
}

// stt
// Go declares the five working variables up front and every branch writes all
// of them; the branches that then ignore what they wrote are what the first
// allow covers. The second is Go's playmessage and recordmessage branches,
// which are identical here where prehistoricParamChecker gives them different
// intents.
#[allow(unused_assignments, clippy::if_same_then_else)]
pub async fn param_checker(
    sink: &dyn IntentSink,
    ctx: &IntentContext<'_>,
    intent: &str,
    speech_text: &str,
    bot_serial: &str,
) {
    let text = |key| lcztn::get_text(ctx.language, key);
    let mut intent_param = String::new();
    let mut intent_param_value = String::new();
    let mut new_intent = String::new();
    let mut is_param = false;
    let mut intent_params: HashMap<String, String> = HashMap::new();
    let bot_location = ctx.bot_location;
    let bot_units = ctx.bot_units;
    // Go declares botPlaySpecific and botIsEarlyOpus as false and never
    // assigns them, so this block and the botIsEarlyOpus block at the end are
    // unreachable in the Go server. Both are translated as written.
    let bot_play_specific = false;
    let bot_is_early_opus = false;
    let mut do_weather_error = false;

    // The jdoc read Go does here is bot_location_and_units above, and its
    // result reaches this function through ctx.
    if bot_play_specific {
        if intent.contains("intent_play_blackjack") {
            is_param = true;
            new_intent = "intent_play_specific_extend".to_owned();
            intent_param = "entity_behavior".to_owned();
            intent_param_value = "blackjack".to_owned();
            intent_params = one_param(&intent_param, &intent_param_value);
        } else if intent.contains("intent_play_fistbump") {
            is_param = true;
            new_intent = "intent_play_specific_extend".to_owned();
            intent_param = "entity_behavior".to_owned();
            intent_param_value = "fist_bump".to_owned();
            intent_params = one_param(&intent_param, &intent_param_value);
        } else if intent.contains("intent_play_rollcube") {
            is_param = true;
            new_intent = "intent_play_specific_extend".to_owned();
            intent_param = "entity_behavior".to_owned();
            intent_param_value = "roll_cube".to_owned();
            intent_params = one_param(&intent_param, &intent_param_value);
        } else if intent.contains("intent_play_popawheelie") {
            is_param = true;
            new_intent = "intent_play_specific_extend".to_owned();
            intent_param = "entity_behavior".to_owned();
            intent_param_value = "pop_a_wheelie".to_owned();
            intent_params = one_param(&intent_param, &intent_param_value);
        } else if intent.contains("intent_play_pickupcube") {
            is_param = true;
            new_intent = "intent_play_specific_extend".to_owned();
            intent_param = "entity_behavior".to_owned();
            intent_param_value = "pick_up_cube".to_owned();
            intent_params = one_param(&intent_param, &intent_param_value);
        } else if intent.contains("intent_play_keepaway") {
            is_param = true;
            new_intent = "intent_play_specific_extend".to_owned();
            intent_param = "entity_behavior".to_owned();
            intent_param_value = "keep_away".to_owned();
            intent_params = one_param(&intent_param, &intent_param_value);
        } else {
            new_intent = intent.to_owned();
            intent_param = String::new();
            intent_param_value = String::new();
            is_param = false;
            intent_params = one_param(&intent_param, &intent_param_value);
        }
    }
    tracing::debug!(comp = "", "Checking params for candidate intent {intent}");
    if intent.contains("intent_photo_take_extend") {
        is_param = true;
        new_intent = intent.to_owned();
        if speech_text.contains(text(lcztn::STR_ME)) || speech_text.contains(text(lcztn::STR_SELF))
        {
            intent_param = "entity_photo_selfie".to_owned();
            intent_param_value = "photo_selfie".to_owned();
        } else {
            intent_param = "entity_photo_selfie".to_owned();
            intent_param_value = String::new();
        }
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_imperative_eyecolor") {
        is_param = true;
        new_intent = "intent_imperative_eyecolor_specific_extend".to_owned();
        intent_param = "eye_color".to_owned();
        match eye_color(ctx.language, speech_text) {
            Some(color) => intent_param_value = color.to_owned(),
            None => {
                new_intent = intent.to_owned();
                intent_param_value = String::new();
                is_param = false;
            }
        }
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_weather_extend") {
        is_param = true;
        new_intent = intent.to_owned();
        let weather = ctx
            .hooks
            .weather_parser(speech_text, bot_location, bot_units)
            .await;
        // weatherParser signals a configuration error by returning "test" as
        // the local datetime.
        if weather.2 == "test" {
            new_intent = "intent_system_unmatched".to_owned();
            is_param = false;
            do_weather_error = true;
        } else {
            intent_params = weather_params(weather);
        }
    } else if intent.contains("intent_imperative_volumelevel_extend") {
        is_param = true;
        new_intent = intent.to_owned();
        intent_param = "volume_level".to_owned();
        intent_param_value = volume_level(ctx.language, speech_text).to_owned();
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_names_username_extend") {
        if ctx.vosk_grammer_enable {
            let mut guid = String::new();
            let mut target = String::new();
            let mut matched = false;
            for bot in ctx.robots {
                if bot_serial == bot.esn {
                    guid = bot.guid.clone();
                    target = format!("{}:443", bot.ip_address);
                    matched = true;
                    break;
                }
            }
            if matched
                && let Err(err) = ctx
                    .hooks
                    .say_text(
                        bot_serial,
                        &guid,
                        &target,
                        "You must add a face in the web interface. It cannot be done via voice by default.",
                    )
                    .await
            {
                tracing::debug!(comp = "", "error connecting to vector: {err}");
            }
            tracing::debug!(
                comp = "",
                "You must add a face via the web interface (Bot Settings -> Connect -> Faces)."
            );
            tracing::info!(
                comp = "",
                "You must add a face via the web interface (Bot Settings -> Connect -> Faces)."
            );
        }
        let mut name_splitter = "";
        is_param = true;
        new_intent = intent.to_owned();
        if speech_text.contains(text(lcztn::STR_NAME_IS)) {
            name_splitter = text(lcztn::STR_NAME_IS);
        } else if speech_text.contains(text(lcztn::STR_NAME_IS2)) {
            name_splitter = text(lcztn::STR_NAME_IS2);
        } else if speech_text.contains(text(lcztn::STR_NAME_IS3)) {
            name_splitter = text(lcztn::STR_NAME_IS3);
        }
        if !name_splitter.is_empty() {
            let username = split_name(speech_text, name_splitter);
            tracing::debug!(comp = "", "Name parsed from speech: `{username}`");
            intent_param = "username".to_owned();
            intent_param_value = username;
            intent_params = one_param(&intent_param, &intent_param_value);
        } else {
            tracing::debug!(comp = "", "No name parsed from speech");
            intent_param = "username".to_owned();
            intent_param_value = String::new();
            intent_params = one_param(&intent_param, &intent_param_value);
        }
    } else if intent.contains("intent_clock_settimer_extend") {
        is_param = true;
        new_intent = intent.to_owned();
        let timer_secs = words2num(ctx.language, speech_text);
        tracing::debug!(comp = "", "Seconds parsed from speech: {timer_secs}");
        intent_param = "timer_duration".to_owned();
        intent_param_value = timer_secs;
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_global_stop_extend") {
        is_param = true;
        new_intent = intent.to_owned();
        intent_param = "what_to_stop".to_owned();
        intent_param_value = "timer".to_owned();
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_message_playmessage_extend") {
        is_param = true;
        new_intent = intent.to_owned();
        intent_param = "given_name".to_owned();
        if speech_text.contains(text(lcztn::STR_FOR)) {
            intent_param_value = split_name(speech_text, text(lcztn::STR_FOR));
        } else {
            intent_param_value = String::new();
        }
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_message_recordmessage_extend") {
        is_param = true;
        new_intent = intent.to_owned();
        intent_param = "given_name".to_owned();
        if speech_text.contains(text(lcztn::STR_FOR)) {
            intent_param_value = split_name(speech_text, text(lcztn::STR_FOR));
        } else {
            intent_param_value = String::new();
        }
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent_param.is_empty() {
        new_intent = intent.to_owned();
        intent_param = String::new();
        intent_param_value = String::new();
        is_param = false;
        intent_params = one_param(&intent_param, &intent_param_value);
    }
    if bot_is_early_opus {
        if intent.contains("intent_imperative_praise") {
            is_param = false;
            new_intent = "intent_imperative_affirmative".to_owned();
            intent_param = String::new();
            intent_param_value = String::new();
            intent_params = one_param(&intent_param, &intent_param_value);
        } else if intent.contains("intent_imperative_abuse") {
            is_param = false;
            new_intent = "intent_imperative_negative".to_owned();
            intent_param = String::new();
            intent_param_value = String::new();
            intent_params = one_param(&intent_param, &intent_param_value);
        } else if intent.contains("intent_imperative_love") {
            is_param = false;
            new_intent = "intent_greeting_hello".to_owned();
            intent_param = String::new();
            intent_param_value = String::new();
            intent_params = one_param(&intent_param, &intent_param_value);
        }
    }
    let _ = intent_pass(sink, ctx, &new_intent, speech_text, intent_params, is_param).await;
    if do_weather_error {
        if ctx.weather_enable {
            tracing::debug!(comp = "", "The weather API is not configured properly.");
            // TODO(M4): KGSim(botSerial, "The weather API is not configured properly. Please check the wire pod logs for more details.")
        } else {
            tracing::debug!(comp = "", "The weather API is not configured.");
            // TODO(M4): KGSim(botSerial, "The weather API is not configured.")
        }
    }
}

// stintent
#[allow(unused_assignments)]
pub async fn param_checker_slots_en_us(
    sink: &dyn IntentSink,
    ctx: &IntentContext<'_>,
    intent: &str,
    slots: &HashMap<String, String>,
    is_opus: bool,
) {
    let slot = |key: &str| slots.get(key).map(String::as_str).unwrap_or_default();
    let mut intent_param = String::new();
    let mut intent_param_value = String::new();
    let mut new_intent = String::new();
    let mut is_param = false;
    let mut intent_params: HashMap<String, String> = HashMap::new();
    let bot_location = ctx.bot_location;
    let bot_units = ctx.bot_units;
    // Unreachable in Go, as in ParamChecker above.
    let bot_play_specific = false;
    let bot_is_early_opus = false;
    if intent.contains("volume") {
        if !slot("volume").is_empty() {
            new_intent = "intent_imperative_volumelevel_extend".to_owned();
            is_param = true;
            intent_param = "volume_level".to_owned();
            if slot("volume").contains("medium low") {
                intent_param_value = "VOLUME_2".to_owned();
            } else if slot("volume").contains("low") {
                intent_param_value = "VOLUME_1".to_owned();
            } else if slot("volume").contains("medium high") {
                intent_param_value = "VOLUME_4".to_owned();
            } else if slot("volume").contains("high") {
                intent_param_value = "VOLUME_5".to_owned();
            } else if slot("volume").contains("medium") {
                intent_param_value = "VOLUME_3".to_owned();
            } else {
                intent_param_value = "VOLUME_1".to_owned();
            }
        } else {
            is_param = false;
            intent_param = String::new();
            intent_param_value = String::new();
        }
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("eyecolor") {
        is_param = true;
        new_intent = "intent_imperative_eyecolor_specific_extend".to_owned();
        intent_param = "eye_color".to_owned();
        if slot("eye_color").contains("purple") {
            intent_param_value = "COLOR_PURPLE".to_owned();
        } else if slot("eye_color").contains("blue") || slot("eye_color").contains("sapphire") {
            intent_param_value = "COLOR_BLUE".to_owned();
        } else if slot("eye_color").contains("yellow") {
            intent_param_value = "COLOR_YELLOW".to_owned();
        } else if slot("eye_color").contains("teal") || slot("eye_color").contains("tell") {
            intent_param_value = "COLOR_TEAL".to_owned();
        } else if slot("eye_color").contains("green") {
            intent_param_value = "COLOR_GREEN".to_owned();
        } else if slot("eye_color").contains("orange") {
            intent_param_value = "COLOR_ORANGE".to_owned();
        } else {
            new_intent = intent.to_owned();
            intent_param_value = String::new();
            is_param = false;
        }
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("_selfie") {
        new_intent = "intent_photo_take_extend".to_owned();
        intent_param = "entity_photo_selfie".to_owned();
        intent_param_value = "photo_selfie".to_owned();
        is_param = true;
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("_noselfie") {
        new_intent = "intent_photo_take_extend".to_owned();
        intent_param = "entity_photo_selfie".to_owned();
        intent_param_value = String::new();
        is_param = true;
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("settimer") {
        is_param = true;
        new_intent = intent.to_owned();
        let slot_num = slot("num");
        let slot_unit = slot("unit");
        // Go's Atoi leaves timerSecs at 0 and logs the error.
        let mut timer_secs: i64 = match slot_num.parse() {
            Ok(secs) => secs,
            Err(err) => {
                tracing::debug!(comp = "", "{err}");
                0
            }
        };
        if !slot_num.is_empty() && !slot_unit.is_empty() {
            if slot_unit.contains("minute") {
                timer_secs *= 60;
            } else if slot_unit.contains("hour") {
                timer_secs *= 60 * 60;
            }
        }
        tracing::debug!(comp = "", "Seconds parsed from speech: {timer_secs}");
        intent_param = "timer_duration".to_owned();
        intent_param_value = timer_secs.to_string();
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("global_stop_extend") {
        is_param = true;
        new_intent = intent.to_owned();
        intent_param = "what_to_stop".to_owned();
        intent_param_value = "timer".to_owned();
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_knowledgegraph_prompt") {
        is_param = false;
        new_intent = "intent_knowledge_promptquestion".to_owned();
        intent_param = String::new();
        intent_param_value = String::new();
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_weather_extend") {
        is_param = true;
        new_intent = intent.to_owned();
        // Go passes this literal rather than anything the robot said.
        let weather = ctx
            .hooks
            .weather_parser("what's the weather", bot_location, bot_units)
            .await;
        intent_params = weather_params(weather);
    } else if intent_param.is_empty() {
        new_intent = intent.to_owned();
        intent_param = String::new();
        intent_param_value = String::new();
        is_param = false;
        intent_params = one_param(&intent_param, &intent_param_value);
    }
    if is_opus || bot_is_early_opus || bot_play_specific {
        if intent.contains("intent_play_blackjack") {
            is_param = true;
            new_intent = "intent_play_specific_extend".to_owned();
            intent_param = "entity_behavior".to_owned();
            intent_param_value = "blackjack".to_owned();
            intent_params = one_param(&intent_param, &intent_param_value);
        } else if intent.contains("intent_play_fistbump") {
            is_param = true;
            new_intent = "intent_play_specific_extend".to_owned();
            intent_param = "entity_behavior".to_owned();
            intent_param_value = "fist_bump".to_owned();
            intent_params = one_param(&intent_param, &intent_param_value);
        } else if intent.contains("intent_play_rollcube") {
            is_param = true;
            new_intent = "intent_play_specific_extend".to_owned();
            intent_param = "entity_behavior".to_owned();
            intent_param_value = "roll_cube".to_owned();
            intent_params = one_param(&intent_param, &intent_param_value);
        } else if intent.contains("intent_imperative_praise") {
            is_param = false;
            new_intent = "intent_imperative_affirmative".to_owned();
            intent_param = String::new();
            intent_param_value = String::new();
            intent_params = one_param(&intent_param, &intent_param_value);
        } else if intent.contains("intent_imperative_love") {
            is_param = false;
            new_intent = "intent_greeting_hello".to_owned();
            intent_param = String::new();
            intent_param_value = String::new();
            intent_params = one_param(&intent_param, &intent_param_value);
        } else if intent.contains("intent_imperative_abuse") {
            is_param = false;
            new_intent = "intent_imperative_negative".to_owned();
            intent_param = String::new();
            intent_param_value = String::new();
            intent_params = one_param(&intent_param, &intent_param_value);
        }
    }
    // Go passes the intent name where every other call site passes the spoken
    // text, so the robot receives it as the query text. Kept as written.
    let _ = intent_pass(sink, ctx, &new_intent, intent, intent_params, is_param).await;
}

#[allow(unused_assignments)]
pub async fn prehistoric_param_checker(
    sink: &dyn IntentSink,
    ctx: &IntentContext<'_>,
    intent: &str,
    speech_text: &str,
) {
    // intent.go detects if the stream uses opus or PCM.
    // If the stream is PCM, it is likely a bot with 0.10.
    // This accounts for the newer 0.10.1### builds.
    let text = |key| lcztn::get_text(ctx.language, key);
    let mut intent_param = String::new();
    let mut intent_param_value = String::new();
    let new_intent;
    let mut is_param = false;
    let mut intent_params: HashMap<String, String> = HashMap::new();
    // Go hardcodes these here rather than reading the jdoc.
    let bot_location = "San Francisco";
    let bot_units = "F";
    if intent.contains("intent_photo_take_extend") {
        is_param = true;
        new_intent = intent.to_owned();
        if speech_text.contains(text(lcztn::STR_ME)) || speech_text.contains(text(lcztn::STR_SELF))
        {
            intent_param = "entity_photo_selfie".to_owned();
            intent_param_value = "photo_selfie".to_owned();
        } else {
            intent_param = "entity_photo_selfie".to_owned();
            intent_param_value = String::new();
        }
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_imperative_eyecolor") {
        // leaving stuff like this in case someone wants to add features like this to older software
        is_param = true;
        intent_param = "eye_color".to_owned();
        match eye_color(ctx.language, speech_text) {
            Some(color) => {
                new_intent = "intent_imperative_eyecolor_specific_extend".to_owned();
                intent_param_value = color.to_owned();
            }
            None => {
                new_intent = intent.to_owned();
                intent_param_value = String::new();
                is_param = false;
            }
        }
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_weather_extend") {
        is_param = true;
        new_intent = intent.to_owned();
        let weather = ctx
            .hooks
            .weather_parser(speech_text, bot_location, bot_units)
            .await;
        intent_params = weather_params(weather);
    } else if intent.contains("intent_imperative_volumelevel_extend") {
        is_param = true;
        new_intent = intent.to_owned();
        intent_param = "volume_level".to_owned();
        intent_param_value = volume_level(ctx.language, speech_text).to_owned();
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_names_username_extend") {
        let mut name_splitter = "";
        is_param = true;
        new_intent = "intent_names_username".to_owned();
        if speech_text.contains(text(lcztn::STR_NAME_IS)) {
            name_splitter = text(lcztn::STR_NAME_IS);
        } else if speech_text.contains(text(lcztn::STR_NAME_IS2)) {
            name_splitter = text(lcztn::STR_NAME_IS2);
        } else if speech_text.contains(text(lcztn::STR_NAME_IS3)) {
            name_splitter = text(lcztn::STR_NAME_IS3);
        }
        if !name_splitter.is_empty() {
            let username = split_name(speech_text, name_splitter);
            tracing::debug!(comp = "", "Name parsed from speech: `{username}`");
            intent_param = "username".to_owned();
            intent_param_value = username;
            intent_params = one_param(&intent_param, &intent_param_value);
        } else {
            tracing::debug!(comp = "", "No name parsed from speech");
            intent_param = "username".to_owned();
            intent_param_value = String::new();
            intent_params = one_param(&intent_param, &intent_param_value);
        }
    } else if intent.contains("intent_clock_settimer_extend") {
        is_param = true;
        new_intent = "intent_clock_settimer".to_owned();
        let timer_secs = words2num(ctx.language, speech_text);
        tracing::debug!(comp = "", "Seconds parsed from speech: {timer_secs}");
        intent_param = "timer_duration".to_owned();
        intent_param_value = timer_secs;
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_global_stop_extend") {
        is_param = true;
        new_intent = "intent_global_stop".to_owned();
        intent_param = "what_to_stop".to_owned();
        intent_param_value = "timer".to_owned();
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_message_playmessage_extend") {
        is_param = true;
        new_intent = "intent_message_playmessage".to_owned();
        intent_param = "given_name".to_owned();
        if speech_text.contains(text(lcztn::STR_FOR)) {
            intent_param_value = split_name(speech_text, text(lcztn::STR_FOR));
        } else {
            intent_param_value = String::new();
        }
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_message_recordmessage_extend") {
        is_param = true;
        new_intent = "intent_message_recordmessage".to_owned();
        intent_param = "given_name".to_owned();
        if speech_text.contains(text(lcztn::STR_FOR)) {
            intent_param_value = split_name(speech_text, text(lcztn::STR_FOR));
        } else {
            intent_param_value = String::new();
        }
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_play_blackjack") {
        is_param = true;
        new_intent = "intent_play_specific_extend".to_owned();
        intent_param = "entity_behavior".to_owned();
        intent_param_value = "blackjack".to_owned();
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_play_fistbump") {
        is_param = true;
        new_intent = "intent_play_specific_extend".to_owned();
        intent_param = "entity_behavior".to_owned();
        intent_param_value = "fist_bump".to_owned();
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_play_rollcube") {
        is_param = true;
        new_intent = "intent_play_specific_extend".to_owned();
        intent_param = "entity_behavior".to_owned();
        intent_param_value = "roll_cube".to_owned();
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_imperative_praise") {
        is_param = false;
        new_intent = "intent_imperative_affirmative".to_owned();
        intent_param = String::new();
        intent_param_value = String::new();
        intent_params = one_param(&intent_param, &intent_param_value);
    } else if intent.contains("intent_imperative_abuse") {
        is_param = false;
        new_intent = "intent_imperative_negative".to_owned();
        intent_param = String::new();
        intent_param_value = String::new();
        intent_params = one_param(&intent_param, &intent_param_value);
    } else {
        new_intent = intent.to_owned();
        intent_param = String::new();
        intent_param_value = String::new();
        is_param = false;
        intent_params = one_param(&intent_param, &intent_param_value);
    }
    let _ = intent_pass(sink, ctx, &new_intent, speech_text, intent_params, is_param).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::match_intent_send::tests::{FakeHooks, FakeSink, context};
    use crate::match_intent_send::{IntentHooks, RequestKind, Weather};

    #[tokio::test]
    async fn param_checker_extracts_the_parameter_the_robot_receives() {
        let hooks = FakeHooks;
        let ctx = context(&hooks);
        let cases = [
            (
                "intent_imperative_eyecolor",
                "set your eyes to sapphire",
                "intent_imperative_eyecolor_specific_extend",
                Some(("eye_color", "COLOR_BLUE")),
            ),
            // No colour in the text clears the parameter and keeps the intent.
            (
                "intent_imperative_eyecolor",
                "set your eyes to beige",
                "intent_imperative_eyecolor",
                None,
            ),
            (
                "intent_imperative_volumelevel_extend",
                "set your volume to medium high",
                "intent_imperative_volumelevel_extend",
                Some(("volume_level", "VOLUME_4")),
            ),
            (
                "intent_clock_settimer_extend",
                "set a timer for ten minutes",
                "intent_clock_settimer_extend",
                Some(("timer_duration", "600")),
            ),
            (
                "intent_names_username_extend",
                "my name is james",
                "intent_names_username_extend",
                Some(("username", "james")),
            ),
            (
                "intent_greeting_hello",
                "hello",
                "intent_greeting_hello",
                None,
            ),
        ];
        for (intent, speech, want_intent, want_param) in cases {
            let sink = FakeSink::new(RequestKind::Intent);
            param_checker(&sink, &ctx, intent, speech, "00303f28").await;
            let result = sink.last();
            assert_eq!(result.action, want_intent, "{intent}");
            match want_param {
                Some((param, value)) => assert_eq!(
                    result.parameters.get(param),
                    Some(&value.to_owned()),
                    "{intent}"
                ),
                None => assert!(result.parameters.is_empty(), "{intent}"),
            }
        }
    }

    struct UnconfiguredWeather;

    #[async_trait::async_trait]
    impl IntentHooks for UnconfiguredWeather {
        async fn weather_parser(&self, _: &str, _: &str, _: &str) -> Weather {
            (
                String::new(),
                String::new(),
                "test".to_owned(),
                String::new(),
                String::new(),
                String::new(),
            )
        }

        async fn say_text(&self, _: &str, _: &str, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn weather_config_error_unmatches_and_slots_send_the_intent_name() {
        let hooks = FakeHooks;
        let ctx = context(&hooks);

        let broken = UnconfiguredWeather;
        let mut broken_ctx = context(&hooks);
        broken_ctx.hooks = &broken;
        let sink = FakeSink::new(RequestKind::Intent);
        param_checker(
            &sink,
            &broken_ctx,
            "intent_weather_extend",
            "what's the weather",
            "00303f28",
        )
        .await;
        assert_eq!(sink.last().action, "intent_system_unmatched");
        assert!(sink.last().parameters.is_empty());

        let sink = FakeSink::new(RequestKind::Intent);
        let slots = HashMap::from([
            ("num".to_owned(), "5".to_owned()),
            ("unit".to_owned(), "minutes".to_owned()),
        ]);
        param_checker_slots_en_us(&sink, &ctx, "settimer", &slots, false).await;
        let result = sink.last();
        assert_eq!(
            result.parameters.get("timer_duration"),
            Some(&"300".to_owned())
        );
        // The query text is the intent name, not anything spoken.
        assert_eq!(result.query_text, "settimer");

        let sink = FakeSink::new(RequestKind::Intent);
        prehistoric_param_checker(
            &sink,
            &ctx,
            "intent_clock_settimer_extend",
            "set a timer for an hour",
        )
        .await;
        assert_eq!(sink.last().action, "intent_clock_settimer");
        assert_eq!(
            sink.last().parameters.get("timer_duration"),
            Some(&"3600".to_owned())
        );
    }
}
