//! Translation of `pkg/wirepod/ttr/words2num.go`.
//!
//! Every pattern turns Unicode classes off for `\d`, `\w`, `\s` and `\b`,
//! because Go's `regexp` treats them as ASCII-only.

use std::collections::HashMap;

use regex::Regex;

use crate::localization as lcztn;

// This file contains words2num. It is given the spoken text and returns a string which contains the true number.

fn whisper_speech_to_num(input: &str) -> String {
    // whisper returns actual numbers in its response
    // ex. "set a timer for 10 minutes and 11 seconds"
    let mut total_seconds: i64 = 0;

    let minute_pattern = Regex::new(r"(?-u)(\d+)\s*minute").unwrap();
    let second_pattern = Regex::new(r"(?-u)(\d+)\s*second").unwrap();

    if let Some(m) = minute_pattern.captures(input)
        && let Ok(minutes) = m[1].parse::<i64>()
    {
        total_seconds = total_seconds.wrapping_add(minutes.wrapping_mul(60));
    }
    if let Some(m) = second_pattern.captures(input)
        && let Ok(seconds) = m[1].parse::<i64>()
    {
        total_seconds = total_seconds.wrapping_add(seconds);
    }

    total_seconds.to_string()
}

// Go keeps this map in a package global and rebuilds it on every call; here
// each call builds its own, and the language comes from the caller instead of
// `vars.APIConfig`.
fn text_to_number(language: &str) -> HashMap<&'static str, i64> {
    let t = |key| lcztn::get_text(language, key);
    HashMap::from([
        (t(lcztn::STR_ZERO), 0),
        (t(lcztn::STR_ONE), 1),
        (t(lcztn::STR_TWO), 2),
        (t(lcztn::STR_THREE), 3),
        (t(lcztn::STR_FOUR), 4),
        (t(lcztn::STR_FIVE), 5),
        (t(lcztn::STR_SIX), 6),
        (t(lcztn::STR_SEVEN), 7),
        (t(lcztn::STR_EIGHT), 8),
        (t(lcztn::STR_NINE), 9),
        (t(lcztn::STR_TEN), 10),
        (t(lcztn::STR_ELEVEN), 11),
        (t(lcztn::STR_TWELVE), 12),
        (t(lcztn::STR_THIRTEEN), 13),
        (t(lcztn::STR_FOURTEEN), 14),
        (t(lcztn::STR_FIFTEEN), 15),
        (t(lcztn::STR_SIXTEEN), 16),
        (t(lcztn::STR_SEVENTEEN), 17),
        (t(lcztn::STR_EIGHTEEN), 18),
        (t(lcztn::STR_NINETEEN), 19),
        (t(lcztn::STR_TWENTY), 20),
        (t(lcztn::STR_THIRTY), 30),
        (t(lcztn::STR_FOURTY), 40),
        (t(lcztn::STR_FIFTY), 50),
        (t(lcztn::STR_SIXTY), 60),
        (t(lcztn::STR_SEVENTY), 70),
        (t(lcztn::STR_EIGHTY), 80),
        (t(lcztn::STR_NINETY), 90),
        (t(lcztn::STR_ONE_HUNDRED), 100),
    ])
}

pub fn words2num(language: &str, input: &str) -> String {
    let text_to_number = text_to_number(language);

    let contains_num = Regex::new(r"(?-u)\b\d+\b").unwrap().is_match(input);
    if std::env::var("STT_SERVICE").as_deref() == Ok("whisper.cpp") && contains_num {
        return whisper_speech_to_num(input);
    }
    let mut total_seconds: i64 = 0;

    let input = input.to_lowercase();
    if input.contains(lcztn::get_text(language, lcztn::STR_ONE_HOUR))
        || input.contains(lcztn::get_text(language, lcztn::STR_ONE_HOUR_ALT))
    {
        return "3600".to_string();
    }

    let minute = lcztn::get_text(language, lcztn::STR_MINUTE);
    let second = lcztn::get_text(language, lcztn::STR_SECOND);
    let hour = lcztn::get_text(language, lcztn::STR_HOUR);
    let str_regex_time_pattern =
        format!(r"(?-u:(\d+|\w+(?:-\w+)?)\s*)({minute}|{second}|{hour})s?");

    let Ok(time_pattern) = Regex::new(&str_regex_time_pattern) else {
        // Go's MustCompile panics here.
        return "0".to_string();
    };

    for m in time_pattern.captures_iter(&input) {
        let unit = &m[2];
        let number = &m[1];

        let value = number
            .parse::<i64>()
            .unwrap_or_else(|_| map_text_to_number(&text_to_number, number));

        if unit == minute {
            total_seconds = total_seconds.wrapping_add(value.wrapping_mul(60));
        } else if unit == second {
            total_seconds = total_seconds.wrapping_add(value);
        } else if unit == hour {
            total_seconds = total_seconds.wrapping_add(value.wrapping_mul(3600));
        }
    }

    total_seconds.to_string()
}

fn map_text_to_number(text_to_number: &HashMap<&'static str, i64>, text: &str) -> i64 {
    if let Some(val) = text_to_number.get(text) {
        return *val;
    }
    let mut sum = 0;
    for part in text.split('-') {
        if let Some(val) = text_to_number.get(part) {
            sum += val;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoken_durations_become_seconds() {
        assert_eq!(words2num("en-US", "set a timer for ten minutes"), "600");
        assert_eq!(
            words2num("en-US", "set a timer for twenty-five seconds"),
            "25"
        );
        assert_eq!(
            words2num("en-US", "timer for two minutes and 30 seconds"),
            "150"
        );
        assert_eq!(words2num("en-US", "set a timer for an hour"), "3600");
        assert_eq!(words2num("it-IT", "timer di cinque minuti"), "300");
        assert_eq!(words2num("en-US", "hello there"), "0");
    }

    #[test]
    fn whisper_numbers_are_read_directly() {
        assert_eq!(
            whisper_speech_to_num("set a timer for 10 minutes and 11 seconds"),
            "611"
        );
    }
}
