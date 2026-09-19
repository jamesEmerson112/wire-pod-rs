//! `sdk_config.ini`, table-driven from the recorded probe output.
//!
//! `docs/phases/P1-robot-connect-auth/ini-probe/expected.txt` is the stdout of
//! the Go program committed beside it, which calls the real
//! `gopkg.in/ini.v1` v1.67.3 through the same four call shapes
//! `pkg/servers/jdocs/botInfoStorer.go` uses. Every expectation here is
//! therefore a recording rather than a hand-written literal: the `setting`
//! section pins the library knobs that decide the layout, and the `file`
//! section pins the complete bytes of one written file per case.
//!
//! The file's format is documented in the phase README and the parser below is
//! the same shape as `tests/gofmt_f32.rs`'s: comment lines start with `#`,
//! every other line is three tab-separated fields, and the third is always a Go
//! `%q` literal.
//!
//! **The line break.** The recording was made on Windows with `ini.LineBreak`
//! pinned to `"\r\n"`, which is what the library's own `init` does there
//! (`ini.go:60-64`) and what the file on this machine holds. The writer makes
//! the same platform choice, so on a Unix host the expectation is transformed
//! by [`expected`] and the `\n` variant is what is asserted. That transform is
//! the only edit any expectation receives, it touches nothing but the line
//! break, and no recorded value is ever written out by hand here.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wirepod_core::paths::sdk_ini_dir;
use wirepod_core::store::bot_info::{BotInfo, BotInfoRobot};
use wirepod_core::store::sdk_ini::{
    DEFAULT_FORMAT_LEFT, DEFAULT_FORMAT_RIGHT, DEFAULT_HEADER, DEFAULT_SECTION, DELIMITER_WRITTEN,
    INI_VERSION, IniEdit, IniError, IniFile, KEY_VALUE_DELIMITER_ON_WRITE, LINE_BREAK,
    PRETTY_EQUAL, PRETTY_FORMAT, PRETTY_SECTION, SDK_CERT_FILE_MODE, SDK_CONFIG_FILE,
    SDK_INI_DIR_MODE, SDK_INI_FILE_MODE, SdkIniStore, SecondaryOutcome, cert_file_path, cert_value,
    sdk_cert_gate, sdk_ini_gate,
};
use wirepod_core::store::session_certs::SESSION_CERT_FILE_MODE;

const EXPECTED: &str = include_str!("data/ini-probe/expected.txt");

/// A ceiling on every awaited operation, generous enough that only a hang
/// reaches it. Real durations, because this crate's tests never pause the
/// runtime's clock.
const CEILING: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// The recording
// ---------------------------------------------------------------------------

/// One parsed line: the raw input column, its pairs in order, the unquoted
/// output and the line number every failure message names.
struct Case {
    input: &'static str,
    pairs: Vec<(&'static str, &'static str)>,
    want: String,
    line: usize,
}

impl Case {
    /// The value of one input key, or a failure naming the line.
    fn pair(&self, key: &str) -> &str {
        self.pairs
            .iter()
            .find(|(name, _)| *name == key)
            .unwrap_or_else(|| panic!("line {}: no {key}= pair in {}", self.line, self.input))
            .1
    }
}

/// Undoes Go's `%q` quoting.
///
/// Only the escapes the recording can produce are accepted, so a probe change
/// that starts emitting some other escape fails here rather than comparing
/// against a literal that no longer means the same bytes.
fn unquote(quoted: &str, line: usize) -> String {
    let body = quoted
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or_else(|| panic!("line {line}: the output column is not a quoted literal"));
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let escape = chars
            .next()
            .unwrap_or_else(|| panic!("line {line}: the output column ends in a lone backslash"));
        out.push(match escape {
            '\\' => '\\',
            '"' => '"',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            other => panic!("line {line}: unsupported escape in the output column: {other}"),
        });
    }
    out
}

/// Every case in one section of the recording, with the comment lines dropped.
///
/// The whole file is walked far enough to hold the README's promise that the
/// (section, input) pair is unique, because that promise is what makes it safe
/// to key anything on the pair.
fn cases_in(wanted: &str) -> Vec<Case> {
    let mut parsed = Vec::new();
    let mut seen: BTreeMap<(&str, &str), usize> = BTreeMap::new();

    for (index, raw) in EXPECTED.lines().enumerate() {
        let line = index + 1;
        let text = raw.strip_suffix('\r').unwrap_or(raw);
        if text.starts_with('#') {
            continue;
        }
        assert!(
            !text.is_empty(),
            "line {line}: the recording has no blank lines"
        );

        let mut columns = text.split('\t');
        let section = columns
            .next()
            .unwrap_or_else(|| panic!("line {line}: missing the section column"));
        let input = columns
            .next()
            .unwrap_or_else(|| panic!("line {line}: missing the input column"));
        let output = columns
            .next()
            .unwrap_or_else(|| panic!("line {line}: missing the output column"));
        assert!(
            columns.next().is_none(),
            "line {line}: more than three columns"
        );

        if let Some(first) = seen.insert((section, input), line) {
            panic!("line {line}: {section} / {input} already appeared on line {first}");
        }

        if section != wanted {
            continue;
        }

        let pairs: Vec<(&str, &str)> = input
            .split(' ')
            .map(|piece| {
                piece
                    .split_once('=')
                    .unwrap_or_else(|| panic!("line {line}: {piece} is not a key=value pair"))
            })
            .collect();
        assert_eq!(
            pairs.first().map(|(key, _)| *key),
            Some("kind"),
            "line {line}: the first input pair is not kind="
        );

        parsed.push(Case {
            input,
            pairs,
            want: unquote(output, line),
            line,
        });
    }

    assert!(
        !parsed.is_empty(),
        "the recording has no {wanted} section at all"
    );
    parsed
}

/// One recorded file expectation, with the line break moved to this platform's.
///
/// The recording carries the Windows `\r\n` that `ini.go:60-64` produces, and
/// the writer emits `\n` everywhere else for the same reason. Nothing but the
/// line break is touched: no recorded case carries a lone carriage return
/// inside a value, which [`the_recording_carries_no_stray_carriage_return`]
/// checks rather than assumes.
fn expected(recorded: &str) -> String {
    if cfg!(windows) {
        recorded.to_owned()
    } else {
        recorded.replace("\r\n", "\n")
    }
}

// ---------------------------------------------------------------------------
// The probe's own inputs, as `ini-probe/main.go` spells them
// ---------------------------------------------------------------------------

/// `main.go`'s `sdkIniPath`. Not a path on this machine: the probe's own fixed
/// placeholder, with the trailing separator that `vars.SDKIniPath` carries.
const SDK_INI_PATH: &str = r"C:\probe\.anki_vector\";

/// One `WriteToIniPrimary` call, which is `main.go`'s `primaryEdit`.
struct Primary {
    bot_name: &'static str,
    esn: &'static str,
    guid: &'static str,
    ip: &'static str,
}

/// One `WriteToIniSecondary` call, which is `main.go`'s `secondaryEdit`. The
/// last two fields are the values Go derives inside the function from the
/// certificate it downloads, and they are passed in for the same reason the
/// probe passes them in.
struct Secondary {
    esn: &'static str,
    guid: &'static str,
    ip: &'static str,
    bot_name: &'static str,
    cert_path: &'static str,
}

const ALPHA: Primary = Primary {
    bot_name: "Vector-A1B2",
    esn: "00000001",
    guid: "AAECAwQFBgcICQoLDA0ODw==",
    ip: "192.0.2.11",
};
const BETA: Primary = Primary {
    bot_name: "Vector-C3D4",
    esn: "00000002",
    guid: "Dw4NDAsKCQgHBgUEAwIBAA==",
    ip: "192.0.2.12",
};
const ALPHA_MOVED: Primary = Primary {
    bot_name: "Vector-Z9Y8",
    esn: "00000001",
    guid: "Wl1gY2ZpbG9ydXh7foGEhw==",
    ip: "192.0.2.99",
};

const ALPHA_SECONDARY: Secondary = Secondary {
    esn: "00000001",
    guid: "Wl1gY2ZpbG9ydXh7foGEhw==",
    ip: "192.0.2.99",
    bot_name: "Vector-Z9Y8",
    cert_path: r"C:\probe\.anki_vector\Vector-Z9Y8-00000001.cert",
};
const BETA_SECONDARY: Secondary = Secondary {
    esn: "00000002",
    guid: "Dw4NDAsKCQgHBgUEAwIBAA==",
    ip: "192.0.2.12",
    bot_name: "Vector-C3D4",
    cert_path: r"C:\probe\.anki_vector\Vector-C3D4-00000002.cert",
};

/// `main.go`'s `writeToIniPrimary`: load or start empty, run each edit, and
/// take the bytes `SaveTo` would have written.
fn write_primary(existing: Option<&str>, edits: &[&Primary]) -> String {
    let mut file = match existing {
        None => IniFile::empty(),
        Some(text) => IniFile::load(text.as_bytes()).expect("the probe's input parses"),
    };
    for edit in edits {
        file.write_to_ini_primary(SDK_INI_PATH, edit.bot_name, edit.esn, edit.guid, edit.ip);
    }
    String::from_utf8(file.to_bytes()).expect("the writer produces UTF-8")
}

/// `main.go`'s `writeToIniSecondary`, with the update arm and the create arm in
/// the order Go runs them: the create arm only when the update arm matched
/// nothing.
fn write_secondary(existing: Option<&str>, edits: &[&Secondary]) -> String {
    let mut file = match existing {
        None => IniFile::empty(),
        Some(text) => IniFile::load(text.as_bytes()).expect("the probe's input parses"),
    };
    for edit in edits {
        if file
            .write_to_ini_secondary_update(edit.esn, edit.guid, edit.ip)
            .is_none()
        {
            file.write_to_ini_secondary_create(
                edit.esn,
                edit.guid,
                edit.ip,
                edit.bot_name,
                edit.cert_path,
            );
        }
    }
    String::from_utf8(file.to_bytes()).expect("the writer produces UTF-8")
}

/// `main.go`'s case 5 input: a file carrying a key and a whole section this
/// server never writes, the second with a much longer key name so the per
/// section alignment is visible.
const UNKNOWN: &str = concat!(
    "[00000001]\r\n",
    r"cert  = C:\probe\.anki_vector\Vector-A1B2-00000001.cert",
    "\r\n",
    "ip    = 192.0.2.11\r\n",
    "name  = Vector-A1B2\r\n",
    "guid  = AAECAwQFBgcICQoLDA0ODw==\r\n",
    "extra = kept by the fork\r\n",
    "\r\n",
    "[some-other-tool]\r\n",
    "a_very_long_key_name = 1\r\n",
    "b                    = 2\r\n",
);

/// `main.go`'s case 6 input: a section holding one of the four keys.
const PARTIAL: &str = "[00000001]\r\nname = Vector-A1B2\r\n";

/// `main.go`'s case 9 input: a section holding cert and name but neither guid
/// nor ip.
const SECONDARY_PARTIAL: &str = concat!(
    "[00000001]\r\n",
    r"cert = C:\probe\.anki_vector\Vector-A1B2-00000001.cert",
    "\r\n",
    "name = Vector-A1B2\r\n",
);

// ---------------------------------------------------------------------------
// The `file` section
// ---------------------------------------------------------------------------

#[test]
fn the_written_bytes_match_the_recorded_probe() {
    // The three cases whose input is another case's output, produced here the
    // way the probe produces them rather than restated.
    let one = write_primary(None, &[&ALPHA]);
    let two = write_primary(None, &[&ALPHA, &BETA]);

    let mut asserted = 0usize;
    for case in cases_in("file") {
        let (line, input) = (case.line, case.input);
        let got = match case.pair("kind") {
            "create" => match case.pair("sections") {
                "1" => one.clone(),
                "2" => two.clone(),
                other => panic!("line {line}: unknown sections={other} for kind=create"),
            },
            "update" => match case.pair("sections") {
                "1" => write_primary(Some(&one), &[&ALPHA_MOVED]),
                "2" => write_primary(Some(&two), &[&ALPHA_MOVED]),
                other => panic!("line {line}: unknown sections={other} for kind=update"),
            },
            "update_unknown_survives" => write_primary(Some(UNKNOWN), &[&ALPHA_MOVED]),
            "update_partial_section" => write_primary(Some(PARTIAL), &[&ALPHA_MOVED]),
            "create_after_existing" => write_primary(Some(&one), &[&BETA]),
            "secondary_update_all_keys" => write_secondary(Some(&one), &[&ALPHA_SECONDARY]),
            "secondary_update_missing_keys" => {
                write_secondary(Some(SECONDARY_PARTIAL), &[&ALPHA_SECONDARY])
            }
            "secondary_create" => write_secondary(None, &[&ALPHA_SECONDARY]),
            "secondary_create_after_existing" => write_secondary(Some(&one), &[&BETA_SECONDARY]),
            "quote_backtick" => quoted("Vector-A`B2"),
            "quote_hash" => quoted("Vector-A#B2"),
            "quote_semicolon" => quoted("Vector-A;B2"),
            "quote_trailing_space" => quoted("Vector-A1B2 "),
            other => panic!("line {line}: unknown kind {other} in the file section"),
        };

        assert_eq!(got, expected(&case.want), "line {line}: {input}");
        asserted += 1;
    }

    // The recording's own number, recounted whenever the probe changes, so
    // deleting a recorded line fails here instead of quietly shrinking the
    // coverage.
    assert_eq!(
        asserted, 15,
        "the file section records fifteen written files"
    );
}

/// `main.go`'s `quoted` helper: one create against an empty file whose bot name
/// carries the trigger, so it lands both in the `name` key and in the middle of
/// the `cert` path.
fn quoted(bot_name: &str) -> String {
    let mut file = IniFile::empty();
    file.write_to_ini_primary(
        SDK_INI_PATH,
        bot_name,
        "00000001",
        "AAECAwQFBgcICQoLDA0ODw==",
        "192.0.2.11",
    );
    String::from_utf8(file.to_bytes()).expect("the writer produces UTF-8")
}

/// The transform [`expected`] applies is only ever a line break, which is only
/// true while no recorded case carries a lone carriage return.
#[test]
fn the_recording_carries_no_stray_carriage_return() {
    for case in cases_in("file") {
        let stray = case
            .want
            .match_indices('\r')
            .any(|(at, _)| case.want.as_bytes().get(at + 1) != Some(&b'\n'));
        assert!(
            !stray,
            "line {}: a carriage return that is not part of a line break",
            case.line
        );
    }
}

// ---------------------------------------------------------------------------
// The `setting` section
// ---------------------------------------------------------------------------

#[test]
fn the_library_knobs_match_the_recorded_probe() {
    let mut asserted = 0usize;
    for case in cases_in("setting") {
        let (line, want) = (case.line, case.want.as_str());
        match case.pair("name") {
            "module" => assert_eq!(INI_VERSION, want, "line {line}"),
            // The recording pins the Windows byte pair, which the writer uses
            // on Windows and only there; the default beside it is what the
            // writer uses everywhere else.
            "line_break_written" => {
                assert_eq!(want, "\r\n", "line {line}");
                if cfg!(windows) {
                    assert_eq!(LINE_BREAK, want, "line {line}");
                }
            }
            "line_break_default" => {
                assert_eq!(want, "\n", "line {line}");
                if !cfg!(windows) {
                    assert_eq!(LINE_BREAK, want, "line {line}");
                }
            }
            "DefaultSection" => assert_eq!(DEFAULT_SECTION, want, "line {line}"),
            "DefaultHeader" => assert_eq!(DEFAULT_HEADER.to_string(), want, "line {line}"),
            "PrettySection" => assert_eq!(PRETTY_SECTION.to_string(), want, "line {line}"),
            "PrettyFormat" => assert_eq!(PRETTY_FORMAT.to_string(), want, "line {line}"),
            "PrettyEqual" => assert_eq!(PRETTY_EQUAL.to_string(), want, "line {line}"),
            "DefaultFormatLeft" => assert_eq!(DEFAULT_FORMAT_LEFT, want, "line {line}"),
            "DefaultFormatRight" => assert_eq!(DEFAULT_FORMAT_RIGHT, want, "line {line}"),
            "KeyValueDelimiterOnWrite" => {
                assert_eq!(KEY_VALUE_DELIMITER_ON_WRITE, want, "line {line}");
            }
            "delimiter_written" => assert_eq!(DELIMITER_WRITTEN, want, "line {line}"),
            // The four key orders are asserted out of the model rather than out
            // of a constant, so a writer that set the right keys in the wrong
            // order fails here as well as in the file section.
            "primary_create_key_order" => assert_eq!(primary_create_order(), want, "line {line}"),
            "primary_update_key_order" => assert_eq!(primary_update_order(), want, "line {line}"),
            "secondary_create_key_order" => {
                assert_eq!(secondary_create_order(), want, "line {line}");
            }
            "secondary_update_key_order" => {
                assert_eq!(secondary_update_order(), want, "line {line}");
            }
            other => panic!("line {line}: unknown setting {other}"),
        }
        asserted += 1;
    }

    assert_eq!(asserted, 16, "the setting section records sixteen knobs");
}

/// The key names one section holds, comma separated, which is how the
/// recording spells an order.
fn order_of(file: &IniFile, esn: &str) -> String {
    file.sections()
        .iter()
        .find(|section| section.name() == esn)
        .expect("the section was created")
        .keys()
        .iter()
        .map(|key| key.name())
        .collect::<Vec<_>>()
        .join(",")
}

/// A section that folds to the serial and holds none of the four keys, so an
/// update appends all four in its own call order.
fn section_without_the_four_keys() -> IniFile {
    IniFile::load(b"[00000001]\r\nunrelated = 1\r\n").expect("the seed parses")
}

fn primary_create_order() -> String {
    let mut file = IniFile::empty();
    assert_eq!(
        file.write_to_ini_primary(SDK_INI_PATH, "Vector-A1B2", "00000001", "guid", "ip"),
        IniEdit::Created
    );
    order_of(&file, "00000001")
}

fn primary_update_order() -> String {
    let mut file = section_without_the_four_keys();
    assert_eq!(
        file.write_to_ini_primary(SDK_INI_PATH, "Vector-A1B2", "00000001", "guid", "ip"),
        IniEdit::Updated
    );
    order_of(&file, "00000001")
        .strip_prefix("unrelated,")
        .expect("the seed key is still first")
        .to_owned()
}

fn secondary_create_order() -> String {
    let mut file = IniFile::empty();
    assert_eq!(
        file.write_to_ini_secondary_create(
            "00000001",
            "guid",
            "ip",
            "Vector-A1B2",
            "the-cert-path"
        ),
        IniEdit::Created
    );
    order_of(&file, "00000001")
}

fn secondary_update_order() -> String {
    let mut file = section_without_the_four_keys();
    assert!(
        file.write_to_ini_secondary_update("00000001", "guid", "ip")
            .is_some()
    );
    order_of(&file, "00000001")
        .strip_prefix("unrelated,")
        .expect("the seed key is still first")
        .to_owned()
}

// ---------------------------------------------------------------------------
// The rules the recording cannot reach on its own
// ---------------------------------------------------------------------------

/// The section walk folds case, where `NewSection` compares exactly.
///
/// A file whose section is spelled in the other case is the same robot, so the
/// update arm has to find it. An exact comparison would create a second section
/// for the same serial, which the SDK then reads whichever way its own lookup
/// folds.
#[test]
fn a_section_is_matched_case_insensitively() {
    let mut file = IniFile::load(b"[00E20145]\r\nname = Vector-A1B2\r\n").expect("the seed parses");
    assert_eq!(
        file.write_to_ini_primary(SDK_INI_PATH, "Vector-Z9Y8", "00e20145", "guid", "1.2.3.4"),
        IniEdit::Updated,
        "the differently cased section was not matched"
    );
    assert_eq!(
        file.sections().len(),
        2,
        "a second section was created for the same robot"
    );
    assert_eq!(
        file.sections()[1].get_key("name").map(|key| key.value()),
        Some("Vector-Z9Y8")
    );
}

/// Every section the walk folds to is updated, because none of the three
/// writers breaks out of its loop.
#[test]
fn every_folding_section_is_updated() {
    let mut file = IniFile::load(b"[00E20145]\r\nip = old\r\n\r\n[00e20145]\r\nip = old\r\n")
        .expect("the seed parses");
    file.write_to_ini_primary(SDK_INI_PATH, "Vector-Z9Y8", "00e20145", "guid", "1.2.3.4");
    for section in &file.sections()[1..] {
        assert_eq!(
            section.get_key("ip").map(|key| key.value()),
            Some("1.2.3.4")
        );
    }
}

/// The `cert` value is a concatenation and the file beside it is a join, and on
/// Windows the two spellings differ.
///
/// `sdk_ini_dir` glues `"/.anki_vector/"` onto a home directory that already
/// carries backslashes (`vars.go:207-209`), so the value the SDK reads out of
/// the ini carries both separators and the path the certificate is written at
/// carries only the platform's. Building the value with a join would quietly
/// rewrite what the SDK reads.
#[test]
fn the_cert_value_is_concatenated_and_the_cert_file_is_joined() {
    let home = if cfg!(windows) {
        Path::new(r"C:\Users\probe")
    } else {
        Path::new("/home/probe")
    };
    let dir = sdk_ini_dir(home);
    let value = cert_value(&dir, "Vector-A1B2", "00000001");
    let path = cert_file_path(&dir, "Vector-A1B2", "00000001");

    assert_eq!(
        value,
        format!("{dir}Vector-A1B2-00000001.cert"),
        "the cert value is not the plain concatenation"
    );
    if cfg!(windows) {
        assert_eq!(
            value,
            r"C:\Users\probe/.anki_vector/Vector-A1B2-00000001.cert"
        );
        assert_eq!(
            path,
            PathBuf::from(r"C:\Users\probe\.anki_vector\Vector-A1B2-00000001.cert")
        );
        assert_ne!(
            value,
            path.to_string_lossy(),
            "the two spellings are the same, so nothing here is being tested"
        );
    } else {
        assert_eq!(value, "/home/probe/.anki_vector/Vector-A1B2-00000001.cert");
        assert_eq!(
            path,
            PathBuf::from("/home/probe/.anki_vector/Vector-A1B2-00000001.cert")
        );
    }
}

/// An empty `DEFAULT` section is written as nothing at all, header and
/// separating line break included (`file.go:363-372`).
#[test]
fn the_empty_default_section_is_skipped() {
    let bytes = IniFile::empty().to_bytes();
    assert!(bytes.is_empty(), "an empty file wrote {bytes:?}");

    let mut file = IniFile::empty();
    file.write_to_ini_primary(SDK_INI_PATH, "Vector-A1B2", "00000001", "guid", "1.2.3.4");
    let text = String::from_utf8(file.to_bytes()).expect("UTF-8");
    assert!(
        text.starts_with("[00000001]"),
        "the DEFAULT section reached the file: {text:?}"
    );
}

/// A key written above any section header belongs to `DEFAULT`, which is then
/// written back without a header and followed by the blank line every section
/// but the last gets.
#[test]
fn a_filled_default_section_keeps_its_keys_and_loses_its_header() {
    let file =
        IniFile::load(b"loose = 1\r\n\r\n[00000001]\r\nip = 1.2.3.4\r\n").expect("the seed parses");
    assert_eq!(file.sections()[0].name(), DEFAULT_SECTION);
    assert_eq!(
        file.sections()[0].get_key("loose").map(|key| key.value()),
        Some("1")
    );
    assert_eq!(
        String::from_utf8(file.to_bytes()).expect("UTF-8"),
        expected("loose = 1\r\n\r\n[00000001]\r\nip = 1.2.3.4\r\n")
    );
}

/// A load and a save of shapes the committed recording does not hold.
///
/// The recording pins what the Go server itself writes, which is four keys of
/// plain text per section and nothing else. `sdk_config.ini` is the user's
/// file, though, so a rewrite meets comments, a `DEFAULT` section, duplicate
/// names, `:` as a delimiter, quoted and continued values, and a BOM, and it
/// has to put each of them back the way `ini.v1` would. These expectations were
/// read off the real library by a throwaway Go program under a copy of
/// `ini-probe`'s module, one `ini.Load` and `WriteTo` per line. They are
/// **not** a recording: nothing generates or verifies them, and a future commit
/// that wants them regenerable should add them to `ini-probe/main.go` instead.
///
/// The `\r\n` here is the structural line break, so the whole case moves to
/// this platform's through [`expected`], input and expectation alike. That is
/// why the multi-line value's own interior break moves with it.
#[test]
fn a_round_trip_of_the_shapes_the_recording_does_not_hold() {
    // (label, input, output), each verbatim from the throwaway program's stdout.
    let cases: &[(&str, &str, &str)] = &[
        (
            "comment_section_and_key",
            "; about the robot\r\n[00000001]\r\n#the address\r\nip = 1.2.3.4 ; inline\r\n",
            "; about the robot\r\n[00000001]\r\n# the address\r\n; inline\r\nip = 1.2.3.4\r\n",
        ),
        (
            "comment_two_lines",
            "# one\r\n# two\r\n[a]\r\nk = v\r\n",
            "# one\r\n# two\r\n[a]\r\nk = v\r\n",
        ),
        (
            "default_with_keys",
            "loose = 1\r\n\r\n[00000001]\r\nip = 1.2.3.4\r\n",
            "loose = 1\r\n\r\n[00000001]\r\nip = 1.2.3.4\r\n",
        ),
        ("default_only", "loose = 1\r\n", "loose = 1\r\n"),
        (
            "duplicate_section",
            "[a]\r\nk = 1\r\n\r\n[b]\r\nk = 2\r\n\r\n[a]\r\nj = 3\r\n",
            "[a]\r\nk = 1\r\nj = 3\r\n\r\n[b]\r\nk = 2\r\n",
        ),
        (
            "duplicate_key",
            "[a]\r\nk = 1\r\nk = 2\r\n",
            "[a]\r\nk = 2\r\n",
        ),
        (
            "auto_increment",
            "[a]\r\n- = 1\r\n- = 2\r\n",
            "[a]\r\n-  = 1\r\n-  = 2\r\n",
        ),
        ("empty_value", "[a]\r\nk =\r\n", "[a]\r\nk = \r\n"),
        ("no_trailing_newline", "[a]\r\nk = v", "[a]\r\nk = v\r\n"),
        (
            "surrounded_double_quote",
            "[a]\r\nk = \"padded \"\r\n",
            "[a]\r\nk = \"padded \"\r\n",
        ),
        // `hasSurroundedQuote` is false when a quote appears anywhere but the
        // two ends (`parser.go:231-234`), so this value keeps its own quotes
        // through the load and needs none added on the way out.
        (
            "quote_in_middle",
            "[a]\r\nk = \"a\"b\"\r\n",
            "[a]\r\nk = \"a\"b\"\r\n",
        ),
        // The writer's whitespace arm is `len(TrimSpace(val)) != len(val)`
        // (`file.go:466`), which trims both ends, so a value whose only
        // whitespace is leading is quoted exactly as a trailing one is.
        (
            "leading_space",
            "[a]\r\nk = \" x\"\r\n",
            "[a]\r\nk = \" x\"\r\n",
        ),
        (
            "backtick_value",
            "[a]\r\nk = `has # hash`\r\n",
            "[a]\r\nk = `has # hash`\r\n",
        ),
        (
            "triple_quote_value",
            "[a]\r\nk = \"\"\"has ` tick\"\"\"\r\n",
            "[a]\r\nk = \"\"\"has ` tick\"\"\"\r\n",
        ),
        (
            "multiline_value",
            "[a]\r\nk = \"\"\"line one\r\nline two\"\"\"\r\nj = 2\r\n",
            "[a]\r\nk = \"\"\"line one\r\nline two\"\"\"\r\nj = 2\r\n",
        ),
        (
            "continuation",
            "[a]\r\nk = one \\\r\n two\r\nj = 2\r\n",
            "[a]\r\nk = one two\r\nj = 2\r\n",
        ),
        (
            "inline_comment_only",
            "[a]\r\nk = v # why\r\n",
            "[a]\r\n# why\r\nk = v\r\n",
        ),
        ("colon_delimiter", "[a]\r\nk: v\r\n", "[a]\r\nk = v\r\n"),
        (
            "key_with_equals",
            "[a]\r\n`k=x` = v\r\n",
            "[a]\r\n`k=x` = v\r\n",
        ),
        ("bom_utf8", "\u{feff}[a]\r\nk = v\r\n", "[a]\r\nk = v\r\n"),
        ("indented_key", "[a]\r\n   k = v\r\n", "[a]\r\nk = v\r\n"),
        (
            "section_trailing_comment",
            "[a] ; hello\r\nk = v\r\n",
            "; hello\r\n[a]\r\nk = v\r\n",
        ),
        ("quoted_key_name", "\"k\" = v\r\n", "k = v\r\n"),
        (
            "single_quoted_value",
            "[a]\r\nk = 'padded '\r\n",
            "[a]\r\nk = \"padded \"\r\n",
        ),
    ];

    for (label, input, want) in cases {
        let file = IniFile::load(expected(input).as_bytes())
            .unwrap_or_else(|error| panic!("{label}: the input did not load: {error}"));
        assert_eq!(
            String::from_utf8(file.to_bytes()).expect("UTF-8"),
            expected(want),
            "{label}"
        );
    }
}

/// The four lines `ini.Load` refuses, with the messages the library gives them.
///
/// Every wire-pod caller discards the error, so only the refusal matters: the
/// two `botInfoStorer.go` writers replace the file from `ini.Empty()` and
/// `ChangeGUIDInIni` returns without writing. The texts are here so a reader
/// can see that the conditions are the library's rather than invented. They
/// come from the same throwaway Go program as the table above.
#[test]
fn the_parser_refuses_what_the_library_refuses() {
    let cases: &[(&str, &str)] = &[
        ("[a]\r\nbare\r\n", "key-value delimiter not found: bare\r\n"),
        ("[a]\r\n= v\r\n", "empty key name: = v\r\n"),
        ("[a\r\nk = v\r\n", "unclosed section: [a\r\n"),
        ("[]\r\nk = v\r\n", "empty section name"),
    ];
    for (input, want) in cases {
        let error =
            IniFile::load(expected(input).as_bytes()).expect_err("the library refuses this line");
        assert_eq!(error.to_string(), expected(want));
    }
}

/// The two-byte UTF-16 byte order marks are stripped, both ways round.
///
/// `parser.BOM` takes `fe ff` and `ff fe` as a mark and `ef bb bf` as the UTF-8
/// one (`parser.go:83-104`), so a file saved as UTF-16 by a text editor loses
/// its mark and the rest is read as bytes. It needs a test of its own because
/// the recording's `bom_utf8` case is a `&str`, and neither UTF-16 mark is
/// valid UTF-8: left in place, each becomes a replacement character and the
/// first line stops being a section header.
///
/// Both outputs were read off the real library, which answers
/// `"[a]\r\nk = v\r\n"` for each.
#[test]
fn a_utf16_byte_order_mark_is_stripped() {
    let body = expected("[a]\r\nk = v\r\n");
    for mark in [[0xfe_u8, 0xff], [0xff, 0xfe]] {
        let mut input = mark.to_vec();
        input.extend_from_slice(body.as_bytes());
        let file = IniFile::load(&input)
            .unwrap_or_else(|error| panic!("{mark:02x?}: the input did not load: {error}"));
        assert_eq!(
            String::from_utf8(file.to_bytes()).expect("UTF-8"),
            body,
            "{mark:02x?}"
        );
    }
}

/// A key name opens with `"""` only when the whole line is longer than six
/// bytes (`parser.go:135`), and the line still carries its own line break when
/// the length is taken (`parser.go:418`, `:476`).
///
/// The two cases below straddle that threshold, so both were read off the real
/// library. `"""x""" = y` is long either way and its name is `x`.
/// `"""x=y` as a last line with no break is six bytes: the library reads it
/// with a single `"`, which makes the name empty, and refuses the file with
/// `error creating new key: empty key name`. A threshold that let `"""` open
/// that line would look for a closing `"""` instead and refuse it with
/// `missing closing key quote`, which is a different variant here.
#[test]
fn a_triple_quoted_key_name_needs_more_than_six_bytes_of_line() {
    let file = IniFile::load(expected("[a]\r\n\"\"\"x\"\"\" = y\r\n").as_bytes())
        .expect("a long triple-quoted key name loads");
    assert_eq!(
        String::from_utf8(file.to_bytes()).expect("UTF-8"),
        expected("[a]\r\nx = y\r\n")
    );

    let error = IniFile::load(expected("[a]\r\n\"\"\"x=y").as_bytes())
        .expect_err("the library refuses a six-byte line opening with three quotes");
    assert!(
        matches!(error, IniError::EmptyKeyName(_)),
        "the short line was refused as {error:?} rather than for an empty key name"
    );
}

/// `ChangeGUIDInIni` folds against its `esn` argument rather than against each
/// robot's own serial (`token/token.go:163`), so every robot in the bot-info
/// file is written into the one section the argument names and the last one
/// wins.
#[test]
fn the_token_path_writes_every_robot_into_the_one_named_section() {
    let mut file =
        IniFile::load(b"[00000001]\r\nip = old\r\nguid = old\r\n").expect("the seed parses");
    let bot_info = BotInfo {
        global_guid: "global".to_owned(),
        robots: vec![
            BotInfoRobot {
                esn: "00000001".to_owned(),
                ip_address: "1.1.1.1".to_owned(),
                guid: "first".to_owned(),
                ..BotInfoRobot::default()
            },
            BotInfoRobot {
                esn: "00000002".to_owned(),
                ip_address: "2.2.2.2".to_owned(),
                // An empty GUID falls back to the global one
                // (`token.go:166-170`).
                guid: String::new(),
                ..BotInfoRobot::default()
            },
        ],
        ..BotInfo::default()
    };

    assert_eq!(file.update_ip_and_guid("00000001", &bot_info), 0);
    let section = &file.sections()[1];
    assert_eq!(
        section.get_key("ip").map(|key| key.value()),
        Some("2.2.2.2")
    );
    assert_eq!(
        section.get_key("guid").map(|key| key.value()),
        Some("global"),
        "the last robot in the list did not win"
    );

    // A serial with no section is one log line per robot (`token.go:173-175`).
    let mut empty = IniFile::empty();
    assert_eq!(empty.update_ip_and_guid("00000009", &bot_info), 2);
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// A directory under the system temporary directory, removed when the test
/// ends. Nothing here ever touches the real `~/.anki_vector`.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is before the epoch")
            .as_nanos();
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wirepod-sdkini-{label}-{}-{nanos:x}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("could not create the temporary directory");
        Self { path }
    }

    /// The SDK directory spelling the store takes, trailing separator and all.
    fn sdk_dir(&self) -> String {
        format!("{}/", self.path.to_string_lossy())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[tokio::test]
async fn the_store_creates_the_file_and_then_updates_it_in_place() {
    let dir = TempDir::new("primary");
    let store = SdkIniStore::new(dir.sdk_dir());

    tokio::time::timeout(
        CEILING,
        store.write_to_ini_primary("Vector-A1B2", "00000001", "a-guid", "192.0.2.11"),
    )
    .await
    .expect("the write hung")
    .expect("the write failed");

    let on_disk = fs::read_to_string(store.path()).expect("the ini is missing");
    assert_eq!(
        on_disk,
        expected(concat!("[00000001]\r\n", "cert = ",)).to_owned()
            + &cert_value(store.dir(), "Vector-A1B2", "00000001")
            + &expected(concat!(
                "\r\n",
                "ip   = 192.0.2.11\r\n",
                "name = Vector-A1B2\r\n",
                "guid = a-guid\r\n",
            ))
    );

    // The second call takes the update arm, which keeps the file's key order
    // and only changes the values.
    tokio::time::timeout(
        CEILING,
        store.write_to_ini_primary("Vector-Z9Y8", "00000001", "b-guid", "192.0.2.99"),
    )
    .await
    .expect("the write hung")
    .expect("the write failed");

    let updated = fs::read_to_string(store.path()).expect("the ini is missing");
    let file = IniFile::load(updated.as_bytes()).expect("the written file parses");
    assert_eq!(file.sections().len(), 2, "a second section appeared");
    let section = &file.sections()[1];
    assert_eq!(
        section
            .keys()
            .iter()
            .map(|key| key.name())
            .collect::<Vec<_>>(),
        ["cert", "ip", "name", "guid"],
        "the update arm reordered the keys"
    );
    assert_eq!(
        section.get_key("guid").map(|key| key.value()),
        Some("b-guid")
    );
}

/// The secondary writer stops where Go would have reached the DDL servers,
/// which is deviation 36, and writes nothing.
#[tokio::test]
async fn the_secondary_writer_stops_at_the_dead_ddl_servers() {
    let dir = TempDir::new("secondary");
    let store = SdkIniStore::new(dir.sdk_dir());

    let outcome = tokio::time::timeout(
        CEILING,
        store.write_to_ini_secondary("00000001", "a-guid", "192.0.2.11"),
    )
    .await
    .expect("the write hung")
    .expect("the call failed");
    assert!(matches!(outcome, SecondaryOutcome::DdlUnavailable));
    assert!(
        fs::metadata(store.path()).is_err(),
        "deviation 36 wrote a file"
    );

    // With a section already there the update arm runs, sets guid then ip, and
    // saves.
    tokio::time::timeout(
        CEILING,
        store.write_to_ini_primary("Vector-A1B2", "00000001", "a-guid", "192.0.2.11"),
    )
    .await
    .expect("the write hung")
    .expect("the write failed");
    let outcome = tokio::time::timeout(
        CEILING,
        store.write_to_ini_secondary("00000001", "b-guid", "192.0.2.99"),
    )
    .await
    .expect("the write hung")
    .expect("the call failed");
    assert!(
        matches!(outcome, SecondaryOutcome::Updated(Some(name)) if name == "Vector-A1B2"),
        "the update arm did not read the bot name back"
    );
    let file = IniFile::load(&fs::read(store.path()).expect("the ini is missing"))
        .expect("the written file parses");
    assert_eq!(
        file.sections()[1].get_key("guid").map(|key| key.value()),
        Some("b-guid")
    );
}

/// `ChangeGUIDInIni` returns without writing when the file will not load, which
/// is the one arm of the three that does not fall back to an empty file.
#[tokio::test]
async fn the_token_path_writes_nothing_when_the_file_is_missing() {
    let dir = TempDir::new("token");
    let store = SdkIniStore::new(dir.sdk_dir());
    let bot_info = BotInfo {
        robots: vec![BotInfoRobot {
            esn: "00000001".to_owned(),
            ip_address: "1.2.3.4".to_owned(),
            guid: "a-guid".to_owned(),
            ..BotInfoRobot::default()
        }],
        ..BotInfo::default()
    };

    tokio::time::timeout(CEILING, store.update_ip_and_guid("00000001", &bot_info))
        .await
        .expect("the call hung")
        .expect("the call failed");
    assert!(
        fs::metadata(store.path()).is_err(),
        "a missing ini was created"
    );
}

/// `ChangeGUIDInIni` saves whether or not any robot matched, because the save
/// sits past the loop (`token/token.go:177`).
///
/// The visible consequence is that a call which changed nothing still rewrites
/// a hand-edited file into the library's layout: the keys gain their per section
/// padding and the delimiter gains its spaces. The expectation was read off the
/// real library, which answers `"[00000009]\r\nip      = old\r\nlongkey = 1\r\n"`
/// for this input.
#[tokio::test]
async fn the_token_path_saves_even_when_no_robot_matched() {
    let dir = TempDir::new("tokennomatch");
    let store = SdkIniStore::new(dir.sdk_dir());
    let seeded = expected("[00000009]\r\nip=old\r\nlongkey=1\r\n");
    fs::write(store.path(), &seeded).expect("could not seed the ini");

    let bot_info = BotInfo {
        robots: vec![BotInfoRobot {
            esn: "00000001".to_owned(),
            ip_address: "1.2.3.4".to_owned(),
            guid: "a-guid".to_owned(),
            ..BotInfoRobot::default()
        }],
        ..BotInfo::default()
    };

    // The serial names no section in the file, so nothing is edited.
    tokio::time::timeout(CEILING, store.update_ip_and_guid("00000001", &bot_info))
        .await
        .expect("the call hung")
        .expect("the call failed");

    let on_disk = fs::read_to_string(store.path()).expect("the ini is missing");
    assert_ne!(
        on_disk, seeded,
        "the unmatched call left the file as it found it, where `token.go:177` saves"
    );
    assert_eq!(
        on_disk,
        expected("[00000009]\r\nip      = old\r\nlongkey = 1\r\n")
    );
}

/// The four modes C9 reproduces, against the literals Go spells at each call
/// site.
///
/// The constants are asserted against literals rather than against each other
/// because they are the only place a mode is observable on Windows, and because
/// the umask makes even a Unix file no witness: it masks the `0666` the library
/// asks for down to the same `0644` a mistake would have asked for. The gates
/// are asserted beside them so that a writer built with the wrong constant
/// fails too.
#[test]
fn the_four_modes_are_gos_own_literals() {
    // `os.WriteFile(vars.SessionCertPath+"/"+esn, ..., 0755)`
    // (`jdocs/server.go:123`).
    assert_eq!(SESSION_CERT_FILE_MODE, 0o755);
    // `os.WriteFile(fullPath, ..., 0755)` beside the ini (`jdocs/server.go:121`).
    assert_eq!(SDK_CERT_FILE_MODE, 0o755);
    // `os.WriteFile(filename, buf.Bytes(), 0666)`, which is `SaveTo`'s
    // (`gopkg.in/ini.v1 v1.67.3 file.go:534`).
    assert_eq!(SDK_INI_FILE_MODE, 0o666);
    // `os.Mkdir(vars.SDKIniPath, 0755)` (`jdocs/server.go:117`,
    // `botInfoStorer.go:35`, `:73`).
    assert_eq!(SDK_INI_DIR_MODE, 0o755);

    let gate = sdk_ini_gate(SDK_INI_PATH);
    assert_eq!(gate.mode(), SDK_INI_FILE_MODE);
    assert_eq!(gate.path(), format!("{SDK_INI_PATH}{SDK_CONFIG_FILE}"));
    assert_eq!(
        SdkIniStore::new(SDK_INI_PATH.to_owned()).path(),
        gate.path(),
        "the store does not write the ini through this gate"
    );

    let gate = sdk_cert_gate(SDK_INI_PATH, "Vector-A1B2", "00000001");
    assert_eq!(gate.mode(), SDK_CERT_FILE_MODE);
    assert_eq!(
        PathBuf::from(gate.path()),
        cert_file_path(SDK_INI_PATH, "Vector-A1B2", "00000001"),
        "the certificate gate does not name the joined spelling"
    );
}

/// Those modes reach the two files and the directory the store creates.
///
/// Unix only, because the modes are. The shape is `tests/persist.rs`'s: no bit
/// beyond the requested ones is set, which no umask can make false. The umask
/// is also why this cannot stand on its own: it masks `0666` to `0644`, so the
/// ini file alone cannot tell Go's mode from a wrong one, and
/// [`the_four_modes_are_gos_own_literals`] is what does. What the bits below do
/// add is the execute bit, which a umask can only take away and never grant: a
/// `0755` file or directory keeps at least one of the three under any umask an
/// operator would set, and a `0666` file has none of them under any umask at
/// all.
#[cfg(unix)]
#[tokio::test]
async fn gos_modes_reach_the_files_the_store_creates() {
    use std::os::unix::fs::PermissionsExt;

    let outer = TempDir::new("modes");
    // A directory that does not exist yet, so the store creates it itself.
    let sdk = format!("{}/.anki_vector/", outer.path.to_string_lossy());
    let store = SdkIniStore::new(sdk.clone());

    tokio::time::timeout(
        CEILING,
        store.write_cert("Vector-A1B2", "00000001", b"body".to_vec()),
    )
    .await
    .expect("the write hung")
    .expect("the write failed");
    tokio::time::timeout(
        CEILING,
        store.write_to_ini_primary("Vector-A1B2", "00000001", "a-guid", "192.0.2.11"),
    )
    .await
    .expect("the write hung")
    .expect("the write failed");

    let mode_of = |path: &Path| {
        fs::metadata(path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
            .permissions()
            .mode()
            & 0o777
    };

    let directory = mode_of(Path::new(&sdk));
    assert_eq!(
        directory & !SDK_INI_DIR_MODE,
        0,
        "the SDK directory carries a bit {SDK_INI_DIR_MODE:o} did not ask for: {directory:o}"
    );
    assert_ne!(
        directory & 0o111,
        0,
        "the SDK directory {directory:o} carries no execute bit at all, so it cannot be 0755"
    );

    let certificate = mode_of(&cert_file_path(&sdk, "Vector-A1B2", "00000001"));
    assert_eq!(certificate & !SDK_CERT_FILE_MODE, 0);
    assert_ne!(
        certificate & 0o111,
        0,
        "the certificate {certificate:o} carries no execute bit at all, so it cannot be 0755"
    );

    let ini = mode_of(Path::new(store.path()));
    assert_eq!(ini & !SDK_INI_FILE_MODE, 0);
    assert_eq!(
        ini & 0o111,
        0,
        "the ini file {ini:o} is executable, which 0666 never asks for"
    );
}

/// The certificate beside the ini lands at the joined spelling, with the SDK
/// directory created first when it is missing (`jdocs/server.go:114-121`).
#[tokio::test]
async fn the_certificate_beside_the_ini_creates_the_directory_it_needs() {
    let outer = TempDir::new("cert");
    // A directory that does not exist yet, so the stat at `server.go:115`
    // fails and the mkdir runs.
    let sdk = format!("{}/.anki_vector/", outer.path.to_string_lossy());
    let store = SdkIniStore::new(sdk.clone());

    tokio::time::timeout(
        CEILING,
        store.write_cert(
            "Vector-A1B2",
            "00000001",
            b"-----BEGIN CERTIFICATE-----\n".to_vec(),
        ),
    )
    .await
    .expect("the write hung")
    .expect("the write failed");

    let path = cert_file_path(&sdk, "Vector-A1B2", "00000001");
    assert_eq!(
        fs::read(&path).expect("the certificate is missing"),
        b"-----BEGIN CERTIFICATE-----\n"
    );
    // Nothing else, so no temporary survived.
    let mut names: Vec<String> = fs::read_dir(Path::new(&sdk))
        .expect("could not list the SDK directory")
        .map(|entry| {
            entry
                .expect("could not read an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(names, ["Vector-A1B2-00000001.cert"]);
}

/// The ini file is named the way Go names it: the directory string with the
/// file name concatenated straight onto it, no separator added.
#[test]
fn the_ini_file_is_named_by_concatenation() {
    let store = SdkIniStore::new(r"C:\probe\.anki_vector\".to_owned());
    assert_eq!(store.path(), r"C:\probe\.anki_vector\sdk_config.ini");
    assert!(store.path().ends_with(SDK_CONFIG_FILE));
}
