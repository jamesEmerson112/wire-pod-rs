//! Translation of `pkg/wirepod/stt/vosk/context.go`.

use std::collections::HashSet;

use vosk::Model;
use wirepod_core::intents::{CustomIntent, JsonIntent};
use wirepod_intent::localization::{ALL_STR, get_text};

pub const NUMBERS_EN_US: [&str; 34] = [
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "seconds",
    "minutes",
    "hours",
    "minute",
    "second",
    "hour",
];

fn remove_duplicates(strings: Vec<String>) -> Vec<String> {
    let mut occurred = HashSet::new();
    let mut result = Vec::new();
    for str in strings {
        if occurred.insert(str.clone()) {
            result.push(str);
        }
    }
    result
}

/// Go returns the grammar as the string `["a", "b"]` and hands it to
/// `NewRecognizerGrm`; the Rust crate builds that same string out of the word
/// list, so the list is what comes back here.
///
/// `lang` is unused in Go, where `GetText` reads the configured language out of
/// a global instead. Here it is what that lookup takes, and `Init` passes the
/// same value Go's global holds.
pub fn get_grammer_list(
    model: &mut Model,
    lang: &str,
    intent_list: &[JsonIntent],
    custom_intents: &[CustomIntent],
) -> Vec<String> {
    let mut words_list: Vec<String> = Vec::new();
    // add words in intent json
    for words in intent_list {
        for word in &words.keyphrases {
            for wor in word.split(' ') {
                if model.find_word(wor).is_some() {
                    words_list.push(wor.to_string());
                }
            }
        }
    }
    // add words in localization
    for str in ALL_STR {
        let text = get_text(lang, str);
        for wor in text.split(' ') {
            if model.find_word(wor).is_some() {
                words_list.push(wor.to_string());
            }
        }
    }
    // add custom intent matches
    for intent in custom_intents {
        for utterance in &intent.utterances {
            for wor in utterance.split(' ') {
                if model.find_word(wor).is_some() {
                    words_list.push(wor.to_string());
                }
            }
        }
    }
    // add numbers
    for wor in NUMBERS_EN_US {
        if model.find_word(wor).is_some() {
            words_list.push(wor.to_string());
        }
    }

    remove_duplicates(words_list)
}

/// The string Go builds out of the word list, which is byte for byte what
/// `vosk::Recognizer::new_with_grammar` builds for the same list.
pub fn grammer_string(words_list: &[String]) -> String {
    let mut grammer = String::new();
    for (i, word) in words_list.iter().enumerate() {
        if i == words_list.len() - 1 {
            grammer = grammer + "\"" + word + "\"";
        } else {
            grammer = grammer + "\"" + word + "\"" + ", ";
        }
    }
    format!("[{grammer}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_go_and_the_grammer_renders_as_go_writes_it() {
        let words = remove_duplicates(
            ["hey", "vector", "hey", "one"]
                .iter()
                .map(|w| (*w).to_string())
                .collect(),
        );
        assert_eq!(words, ["hey", "vector", "one"]);
        assert_eq!(grammer_string(&words), r#"["hey", "vector", "one"]"#);
        assert_eq!(grammer_string(&[]), "[]");
    }
}
