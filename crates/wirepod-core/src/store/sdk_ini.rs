//! `~/.anki_vector/sdk_config.ini`: what the Python SDK reads to reach the
//! robot, written the way `gopkg.in/ini.v1` writes it.
//!
//! This file is not the server's own state. It belongs to the Vector Python
//! SDK, which reads the section named by a robot's serial to find the robot's
//! address, its GUID and the session certificate to pin against. wire-pod
//! rewrites it whenever a robot authenticates, through three functions that
//! disagree about which keys they set and in what order:
//! `WriteToIniPrimary` (`jdocs/botInfoStorer.go:31-62`),
//! `WriteToIniSecondary` (`:66-128`) and `ChangeGUIDInIni`
//! (`token/token.go:148-178`). Each is `ini.Load`, a walk over the sections, and
//! `SaveTo`.
//!
//! **Why the bytes are a contract.** The SDK is not the only reader: the file
//! is a user's own configuration, hand-edited and carrying other tools'
//! sections. A rewrite that reflowed it, dropped a comment or reordered a
//! section would be a visible regression in a file the server does not own. So
//! this module reproduces `ini.v1` v1.67.3's `SaveTo` byte for byte rather than
//! writing some other correct INI, and the bytes are recorded rather than
//! guessed: `docs/phases/P1-robot-connect-auth/ini-probe/expected.txt` is the
//! stdout of a Go program that calls the real library, and
//! `crates/wirepod-core/tests/sdk_ini.rs` asserts every one of its thirty-one
//! cases.
//!
//! **The layout rules**, all read out of `ini.v1` v1.67.3 in the module cache
//! and all pinned by the recording's `setting` section:
//!
//! - `PrettyFormat` is true (`ini.go:49`), so a key is padded with spaces out
//!   to the longest key name in its own section and the delimiter is written
//!   as `" = "` (`file.go:338-340`, `:456-459`). The padding is per section.
//! - `PrettySection` is true (`ini.go:46`), so one bare line break separates
//!   sections and the last section does not get one (`file.go:498-503`); the
//!   file ends with exactly the line break after its last key.
//! - `DefaultHeader` is false (`ini.go:43`), so the `DEFAULT` section that the
//!   parser always creates (`parser.go:367-371`) is written without a header,
//!   and skipped outright when it holds no keys (`file.go:363-372`).
//! - The line break is `"\n"`, rewritten to `"\r\n"` on Windows in an `init`
//!   function (`ini.go:35-37`, `:60-64`). It is a platform choice here for the
//!   same reason, which is why the recording was made with the Windows value
//!   pinned explicitly and why the test transforms it under `cfg(unix)`.
//! - A value is quoted only when it has to be, in the three arms at
//!   `file.go:461-468`: a newline or a backtick wraps it in `"""`, a `#` or a
//!   `;` wraps it in backticks, and leading or trailing whitespace wraps it in
//!   `"`. A Windows path full of backslashes trips none of them.
//!
//! **The two spellings of the certificate path.** The value stored under the
//! `cert` key is built by string concatenation, `SDKIniPath + botName + "-" +
//! esn + ".cert"` (`botInfoStorer.go:43`, `:55`, `:83`, `:106`), while the file
//! it names is built with `filepath.Join` (`jdocs/server.go:114`). On Windows
//! those differ: `SDKIniPath` itself is a concatenation ending in a forward
//! slash (`vars.go:207-209`), so the value carries mixed separators and the
//! file's own path is cleaned to backslashes. Both are reproduced, the value by
//! [`cert_value`] and the path by [`cert_file_path`]; [`crate::paths`] has the
//! longer version of this.
//!
//! **What is not reproduced.** Deviation 36: `WriteToIniSecondary`'s create
//! path downloads the session certificate from
//! `session-certs.token.global.anki-services.com`, which is dead, so
//! [`SdkIniStore::write_to_ini_secondary`] writes Go's own "the DDL servers are
//! down" line and stops. The create arm itself is still implemented, as
//! [`IniFile::write_to_ini_secondary_create`], because the recording pins its
//! bytes and because P7 may reach it from a certificate the server already has.

use std::io;
use std::path::PathBuf;

use crate::persist::WriteGate;
use crate::store::bot_info::BotInfo;

// ---------------------------------------------------------------------------
// The library's knobs, as the recording's `setting` section pins them
// ---------------------------------------------------------------------------

/// The `gopkg.in/ini.v1` release this module reproduces, which is the one
/// `chipper/go.mod` pins.
pub const INI_VERSION: &str = "gopkg.in/ini.v1 v1.67.3";

/// `ini.DefaultSection` (`ini.go:33`): the name of the section the parser
/// creates before it reads a line, and the one the writer leaves unheaded.
pub const DEFAULT_SECTION: &str = "DEFAULT";

/// `ini.LineBreak` (`ini.go:35-37`), rewritten to `"\r\n"` on Windows by the
/// `init` at `ini.go:60-64`.
///
/// It is a compile-time platform choice here rather than a runtime global,
/// because nothing in wire-pod ever assigns it. The recording was made with the
/// Windows value pinned, so the test transforms the expectation rather than the
/// writer.
pub const LINE_BREAK: &str = if cfg!(windows) { "\r\n" } else { "\n" };

/// `ini.DefaultHeader` (`ini.go:43`).
pub const DEFAULT_HEADER: bool = false;

/// `ini.PrettySection` (`ini.go:46`).
pub const PRETTY_SECTION: bool = true;

/// `ini.PrettyFormat` (`ini.go:49`).
pub const PRETTY_FORMAT: bool = true;

/// `ini.PrettyEqual` (`ini.go:51`).
pub const PRETTY_EQUAL: bool = false;

/// `ini.DefaultFormatLeft` (`ini.go:53`), which only matters when neither
/// pretty flag is set.
pub const DEFAULT_FORMAT_LEFT: &str = "";

/// `ini.DefaultFormatRight` (`ini.go:55`).
pub const DEFAULT_FORMAT_RIGHT: &str = "";

/// `LoadOptions.KeyValueDelimiterOnWrite`, whose zero value `newFile` fills in
/// with `"="` (`file.go:54-55`).
pub const KEY_VALUE_DELIMITER_ON_WRITE: &str = "=";

/// `LoadOptions.KeyValueDelimiters`, whose zero value `newFile` fills in with
/// `"=:"` (`file.go:51-52`). Either character ends a key name.
pub const KEY_VALUE_DELIMITERS: &[char] = &['=', ':'];

/// The delimiter that actually reaches the file: `fmt.Sprintf(" %s ", ...)`
/// under `PrettyFormat` (`file.go:338-340`).
pub const DELIMITER_WRITTEN: &str = " = ";

/// The file name inside `vars.SDKIniPath`, concatenated onto it with no
/// separator of its own (`botInfoStorer.go:32`, `:60`, `:70`, `:127`,
/// `token/token.go:155`, `:177`).
pub const SDK_CONFIG_FILE: &str = "sdk_config.ini";

/// The mode `SaveTo` hands `os.WriteFile` (`file.go:534`).
pub const SDK_INI_FILE_MODE: u32 = 0o666;

/// The mode both writers create `vars.SDKIniPath` with
/// (`jdocs/server.go:117`, `botInfoStorer.go:35`, `:73`).
pub const SDK_INI_DIR_MODE: u32 = 0o755;

/// The mode a session certificate is written with beside the ini file
/// (`jdocs/server.go:121`).
pub const SDK_CERT_FILE_MODE: u32 = 0o755;

// ---------------------------------------------------------------------------
// The two path spellings
// ---------------------------------------------------------------------------

/// The `cert` value, `SDKIniPath + botName + "-" + esn + ".cert"`
/// (`botInfoStorer.go:43`, `:55`, `:83`, `:106`).
///
/// Concatenation, not a join: `sdk_ini_dir` already ends in a separator and
/// this adds none, so on Windows the value carries whatever mixed separators
/// `vars.go:207-209` produced. This is the spelling the SDK reads out of the
/// file, so it is the one that has to be exact.
pub fn cert_value(sdk_ini_dir: &str, bot_name: &str, esn: &str) -> String {
    format!("{sdk_ini_dir}{bot_name}-{esn}.cert")
}

/// The certificate file beside the ini, `filepath.Join(vars.SDKIniPath,
/// botName + "-" + esn + ".cert")` (`jdocs/server.go:114`).
///
/// `filepath.Join` cleans its result into the platform separator, so this is
/// the same file [`cert_value`] names, spelled differently. The cleaning here
/// is deliberately narrow: it handles the one shape `sdk_ini_dir` produces, an
/// absolute directory with a trailing separator and no `.` or `..` component,
/// joined with a single file name. Anything else is outside what Go's callers
/// can hand it.
pub fn cert_file_path(sdk_ini_dir: &str, bot_name: &str, esn: &str) -> PathBuf {
    let separator = if cfg!(windows) { '\\' } else { '/' };
    let mut cleaned = String::with_capacity(sdk_ini_dir.len() + bot_name.len() + esn.len() + 8);
    let mut last_was_separator = false;
    for character in sdk_ini_dir.chars() {
        let is_separator = character == '/' || (cfg!(windows) && character == '\\');
        if is_separator {
            // `filepath.Clean` collapses a run of separators into one and
            // rewrites each into the platform's.
            if !last_was_separator {
                cleaned.push(separator);
            }
            last_was_separator = true;
            continue;
        }
        cleaned.push(character);
        last_was_separator = false;
    }
    if !last_was_separator {
        cleaned.push(separator);
    }
    cleaned.push_str(&format!("{bot_name}-{esn}.cert"));
    PathBuf::from(cleaned)
}

/// `vars.SDKIniPath + "sdk_config.ini"`, concatenated with no separator
/// (`botInfoStorer.go:32`).
pub fn sdk_config_path(sdk_ini_dir: &str) -> String {
    format!("{sdk_ini_dir}{SDK_CONFIG_FILE}")
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// One key in a section, in the order the file or the writer put it there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IniKey {
    /// The key name as it was parsed or created, without any quoting the
    /// writer may add back.
    name: String,
    /// The value with the parser's quoting removed.
    value: String,
    /// The comment lines that preceded this key plus any inline comment,
    /// already trimmed (`parser.go:523`).
    comment: String,
    /// Whether the key was written as `-` and given an `#N` name
    /// (`parser.go:504-510`), which the writer spells back as `-`
    /// (`file.go:438-439`).
    auto_increment: bool,
}

impl IniKey {
    /// The key name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The value, unquoted.
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// One section, holding its keys in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IniSection {
    /// The name between the brackets, or [`DEFAULT_SECTION`] for the one the
    /// parser creates first.
    name: String,
    /// The comment lines that preceded the header plus any inline comment.
    comment: String,
    /// The keys, in file order.
    keys: Vec<IniKey>,
}

impl IniSection {
    /// The section name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The keys, in file order.
    pub fn keys(&self) -> &[IniKey] {
        &self.keys
    }

    /// `Section.GetKey` without the child-section fallback
    /// (`section.go:108-137`): the key by that exact name, or `None`.
    pub fn get_key(&self, name: &str) -> Option<&IniKey> {
        self.keys.iter().find(|key| key.name == name)
    }

    /// `Section.Key(name).SetValue(value)` (`section.go:166-175`,
    /// `key.go:836-844`): the existing key's value is replaced, and a key that
    /// is not there yet is appended with that value.
    ///
    /// The append is what makes a section missing a key gain it at the end in
    /// the caller's own call order, which is what the recording's
    /// `update_partial_section` and `secondary_update_missing_keys` cases pin.
    pub fn set_key(&mut self, name: &str, value: &str) {
        if let Some(key) = self.keys.iter_mut().find(|key| key.name == name) {
            key.value = value.to_owned();
            return;
        }
        self.keys.push(IniKey {
            name: name.to_owned(),
            value: value.to_owned(),
            comment: String::new(),
            auto_increment: false,
        });
    }

    /// `Section.NewKey` with `AllowShadows` off (`section.go:66-94`): a
    /// duplicate name overwrites the value in place and keeps its position, and
    /// a new name is appended.
    ///
    /// An empty name is Go's `error creating new key: empty key name`
    /// (`section.go:67-68`). The three wire-pod writers never pass one, and the
    /// parser checks for it before it gets here, so nothing is added and
    /// nothing is reported.
    pub fn new_key(&mut self, name: &str, value: &str) {
        if name.is_empty() {
            return;
        }
        self.set_key(name, value);
    }
}

/// An `ini.File` as far as this port needs one: an ordered list of sections,
/// the first of which is always [`DEFAULT_SECTION`].
///
/// Everything the library carries that wire-pod never turns on is absent:
/// shadows, nested values, boolean keys, raw sections, child-section lookup,
/// `%(var)s` expansion and the name and value mappers. Each is a
/// [`LoadOptions`](https://pkg.go.dev/gopkg.in/ini.v1#LoadOptions) field left
/// at its zero value by `ini.Load` (`ini.go:156-158`), so no file wire-pod
/// writes and no option wire-pod passes can reach them. A hand-written file
/// that uses one is the candidate deviation the module doc's last paragraph
/// names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IniFile {
    sections: Vec<IniSection>,
}

/// What the parser refused, mirroring the five errors `ini.Load` can answer for
/// a file with no options set.
///
/// Every wire-pod caller discards this: `ini.Load` failing sends
/// `WriteToIniPrimary` and `WriteToIniSecondary` down the `ini.Empty()` arm
/// (`botInfoStorer.go:33-37`, `:71-75`) and makes `ChangeGUIDInIni` return
/// (`token/token.go:156-159`). It is named rather than erased so that a test,
/// and a future log line, can say which line was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IniError {
    /// `unclosed section: <line>` (`parser.go:437`).
    UnclosedSection(String),
    /// `empty section name` (`file.go:84`), from a `[]` header.
    EmptySectionName,
    /// `key-value delimiter not found: <line>` (`error.go:32`).
    DelimiterNotFound(String),
    /// `empty key name: <line>` (`error.go:47`).
    EmptyKeyName(String),
    /// `missing closing key quote: <line>` (`parser.go:151`).
    MissingClosingKeyQuote(String),
    /// `missing closing key quote from <line> to <next>` (`parser.go:202`),
    /// which ends a `"""` or backtick value that no later line closes.
    UnterminatedValue(String),
}

impl std::fmt::Display for IniError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnclosedSection(line) => write!(f, "unclosed section: {line}"),
            Self::EmptySectionName => f.write_str("empty section name"),
            Self::DelimiterNotFound(line) => write!(f, "key-value delimiter not found: {line}"),
            Self::EmptyKeyName(line) => write!(f, "empty key name: {line}"),
            Self::MissingClosingKeyQuote(line) => write!(f, "missing closing key quote: {line}"),
            Self::UnterminatedValue(line) => write!(f, "missing closing key quote from {line}"),
        }
    }
}

impl std::error::Error for IniError {}

impl Default for IniFile {
    fn default() -> Self {
        Self::empty()
    }
}

impl IniFile {
    /// `ini.Empty()` (`file.go:70-79`), which parses an empty source and so
    /// still holds the `DEFAULT` section the parser always creates.
    pub fn empty() -> Self {
        Self {
            sections: vec![IniSection {
                name: DEFAULT_SECTION.to_owned(),
                comment: String::new(),
                keys: Vec::new(),
            }],
        }
    }

    /// The sections, in file order, `DEFAULT` first.
    pub fn sections(&self) -> &[IniSection] {
        &self.sections
    }

    /// `File.NewSection` with `AllowNonUniqueSections` off
    /// (`file.go:82-110`): a name already present answers the section that is
    /// already there, so a file naming one section twice merges the two.
    ///
    /// The comparison is exact, not a fold, which is the difference that makes
    /// `WriteToIniPrimary`'s create arm safe: it only runs when `EqualFold`
    /// matched nothing, and `EqualFold` matching nothing implies `==` matches
    /// nothing.
    fn new_section(&mut self, name: &str) -> Option<usize> {
        if name.is_empty() {
            // `file.go:83-85`, whose error every wire-pod caller logs and then
            // dereferences; see `SdkIniStore::write_to_ini_primary`.
            return None;
        }
        if let Some(index) = self.sections.iter().position(|s| s.name == name) {
            return Some(index);
        }
        self.sections.push(IniSection {
            name: name.to_owned(),
            comment: String::new(),
            keys: Vec::new(),
        });
        Some(self.sections.len() - 1)
    }

    /// The indexes of every section whose name folds to `esn`, which is the
    /// walk all three writers do (`botInfoStorer.go:39-40`, `:78-79`,
    /// `token/token.go:162-163`).
    ///
    /// None of them breaks out of the loop, so every matching section is
    /// updated rather than only the first. Two sections can match at once only
    /// when they differ in case, because the section list holds one entry per
    /// exact name.
    fn matching_sections(&self, esn: &str) -> Vec<usize> {
        self.sections
            .iter()
            .enumerate()
            .filter(|(_, section)| equal_fold(&section.name, esn))
            .map(|(index, _)| index)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// The three writers, as operations on the model
// ---------------------------------------------------------------------------

/// What `WriteToIniPrimary`'s walk did, which decides which log line Go writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IniEdit {
    /// At least one section folded to the serial, so its keys were set in
    /// place (`botInfoStorer.go:41-47`).
    Updated,
    /// No section matched, so one was created (`botInfoStorer.go:50-59`).
    Created,
    /// No section matched and the serial was empty, so `NewSection` answered an
    /// error and Go went on to dereference a nil section
    /// (`botInfoStorer.go:51-54`). Nothing was changed.
    Refused,
}

impl IniFile {
    /// `WriteToIniPrimary` without the load, the mkdir, the log lines and the
    /// save (`botInfoStorer.go:38-59`).
    ///
    /// The update arm sets `cert`, `name`, `ip`, `guid` in that order, and the
    /// create arm creates them as `cert`, `ip`, `name`, `guid`. The two orders
    /// differ, which is only visible when a key is missing, and the recording
    /// pins both.
    pub fn write_to_ini_primary(
        &mut self,
        sdk_ini_dir: &str,
        bot_name: &str,
        esn: &str,
        guid: &str,
        ip: &str,
    ) -> IniEdit {
        let cert = cert_value(sdk_ini_dir, bot_name, esn);
        let matched = self.matching_sections(esn);
        if !matched.is_empty() {
            for index in matched {
                // `botInfoStorer.go:43-46`.
                let section = &mut self.sections[index];
                section.set_key("cert", &cert);
                section.set_key("name", bot_name);
                section.set_key("ip", ip);
                section.set_key("guid", guid);
            }
            return IniEdit::Updated;
        }

        // `botInfoStorer.go:51-58`.
        let Some(index) = self.new_section(esn) else {
            return IniEdit::Refused;
        };
        let section = &mut self.sections[index];
        section.new_key("cert", &cert);
        section.new_key("ip", ip);
        section.new_key("name", bot_name);
        section.new_key("guid", guid);
        IniEdit::Created
    }

    /// The update arm of `WriteToIniSecondary` (`botInfoStorer.go:78-89`),
    /// answering Go's `certExists`.
    ///
    /// It sets `guid` and then `ip`, and touches neither `cert` nor `name`.
    /// That order is not the primary's and the recording pins it: a section
    /// holding neither key gains them as guid then ip.
    ///
    /// Go reads the bot name out of the matched section on the way
    /// (`:81-82`) with `section.GetKey("name")` and then calls `String()` on
    /// the result without checking it, so a matched section with no `name` key
    /// is a nil dereference. It answers the name here instead, `None` where Go
    /// would die, which is another instance of deviation 31's family. Nothing
    /// in the update arm uses the name; Go computes `certPath` from it and then
    /// only uses `certPath` on the create arm.
    pub fn write_to_ini_secondary_update(
        &mut self,
        esn: &str,
        guid: &str,
        ip: &str,
    ) -> Option<Option<String>> {
        let matched = self.matching_sections(esn);
        if matched.is_empty() {
            return None;
        }
        let mut bot_name = None;
        for index in matched {
            let section = &mut self.sections[index];
            bot_name = section.get_key("name").map(|key| key.value.clone());
            // `botInfoStorer.go:86-87`.
            section.set_key("guid", guid);
            section.set_key("ip", ip);
        }
        Some(bot_name)
    }

    /// The create arm of `WriteToIniSecondary` (`botInfoStorer.go:115-125`).
    ///
    /// `bot_name` and `cert_path` are the two values Go derives from the
    /// certificate it downloads at `:92-110`, so they are parameters here for
    /// the same reason the probe passes them in: deviation 36 does not
    /// reproduce that download, and no recording can hold a network call. The
    /// key order is `cert`, `ip`, `name`, `guid`, which happens to be the
    /// primary create order but is a separate call site.
    pub fn write_to_ini_secondary_create(
        &mut self,
        esn: &str,
        guid: &str,
        ip: &str,
        bot_name: &str,
        cert_path: &str,
    ) -> IniEdit {
        let Some(index) = self.new_section(esn) else {
            return IniEdit::Refused;
        };
        let section = &mut self.sections[index];
        section.new_key("cert", cert_path);
        section.new_key("ip", ip);
        section.new_key("name", bot_name);
        section.new_key("guid", guid);
        IniEdit::Created
    }

    /// `ChangeGUIDInIni` without the load and the save
    /// (`token/token.go:160-176`).
    ///
    /// The outer loop is over the bot-info robots and the inner one over the
    /// sections, but the name it folds against is the `esn` argument and not
    /// `robot.Esn` (`token.go:163`). So every robot in the file is written into
    /// the one section the argument names, and the last robot in the list is
    /// the one whose address and GUID survive. That is Go's, it is reproduced,
    /// and it is why this takes the serial separately from the bot-info file.
    ///
    /// A robot whose own GUID is empty contributes the global one
    /// (`token.go:166-170`), the same fallback [`BotInfo::resolve`] makes.
    ///
    /// Answers how many robots found no section, which is how many times Go
    /// writes `Bot is not in sdk_config.ini. ...` (`token.go:173-175`).
    pub fn update_ip_and_guid(&mut self, esn: &str, bot_info: &BotInfo) -> usize {
        let mut unmatched = 0;
        for robot in &bot_info.robots {
            let matched = self.matching_sections(esn);
            if matched.is_empty() {
                unmatched += 1;
                continue;
            }
            let guid = if robot.guid.is_empty() {
                &bot_info.global_guid
            } else {
                &robot.guid
            };
            for index in matched {
                let section = &mut self.sections[index];
                // `token.go:165`, then `:167` or `:169`.
                section.set_key("ip", &robot.ip_address);
                section.set_key("guid", guid);
            }
        }
        unmatched
    }
}

/// Go's `strings.EqualFold` for the ASCII the section names hold.
///
/// `EqualFold` is Unicode simple case folding; `eq_ignore_ascii_case` is not,
/// and the two disagree for characters such as the Kelvin sign. A robot serial
/// is eight hex digits and a section name a user typed is theirs, so the
/// difference is only reachable through a hand-written section name with a
/// non-ASCII character that folds to one. That is a candidate deviation, and it
/// is the same one [`crate::store::bot_info::BotInfo::resolve`] carries.
fn equal_fold(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

// ---------------------------------------------------------------------------
// The writer: ini.v1's SaveTo bytes
// ---------------------------------------------------------------------------

impl IniFile {
    /// The bytes `File.SaveTo` would put on disk, which are the bytes
    /// `writeToBuffer` builds with an empty indent (`file.go:335-506`,
    /// `:526-535`).
    ///
    /// Every rule is cited on the branch that applies it, and every one of them
    /// is pinned by a case in the recording's `file` section.
    pub fn to_bytes(&self) -> Vec<u8> {
        // `file.go:336-340`. `PrettyFormat` is on, so the left and right
        // format strings never reach the delimiter.
        let equal_sign = if PRETTY_FORMAT || PRETTY_EQUAL {
            DELIMITER_WRITTEN.to_owned()
        } else {
            format!("{DEFAULT_FORMAT_LEFT}{KEY_VALUE_DELIMITER_ON_WRITE}{DEFAULT_FORMAT_RIGHT}")
        };

        let mut out = String::new();
        let last = self.sections.len().saturating_sub(1);
        for (index, section) in self.sections.iter().enumerate() {
            // `file.go:347-361`.
            if !section.comment.is_empty() {
                for line in section.comment.split(LINE_BREAK) {
                    out.push_str(&normalise_comment(line, false));
                    out.push_str(LINE_BREAK);
                }
            }

            // `file.go:363-372`: the header, unless this is a leading
            // `DEFAULT`, and then nothing at all when it is empty. Go's test is
            // `strings.ToUpper(sec.name) != DefaultSection`, which for a
            // section name outside ASCII can differ from this fold; that is the
            // candidate deviation [`equal_fold`] names.
            if index > 0 || DEFAULT_HEADER || !section.name.eq_ignore_ascii_case(DEFAULT_SECTION) {
                out.push('[');
                out.push_str(&section.name);
                out.push(']');
                out.push_str(LINE_BREAK);
            } else if section.keys.is_empty() {
                continue;
            }

            let is_last = index == last;

            // `file.go:392-408`: the alignment width is the longest key name in
            // this section, counted as the writer will spell it.
            let mut align = 0usize;
            if PRETTY_FORMAT {
                for key in &section.keys {
                    align = align.max(written_key_len(&key.name));
                }
            }

            for key in &section.keys {
                // `file.go:413-431`.
                if !key.comment.is_empty() {
                    for line in key.comment.split(LINE_BREAK) {
                        out.push_str(&normalise_comment(line, true));
                        out.push_str(LINE_BREAK);
                    }
                }

                // `file.go:437-444`.
                let name = if key.auto_increment {
                    "-".to_owned()
                } else {
                    quote_key(&key.name)
                };

                out.push_str(&name);
                // `file.go:456-459`.
                if PRETTY_FORMAT {
                    out.push_str(&" ".repeat(align.saturating_sub(name.len())));
                }
                out.push_str(&equal_sign);
                out.push_str(&quote_value(&key.value));
                out.push_str(LINE_BREAK);
            }

            // `file.go:498-503`.
            if PRETTY_SECTION && !is_last {
                out.push_str(LINE_BREAK);
            }
        }

        out.into_bytes()
    }
}

/// How wide a key name is once the writer has quoted it
/// (`file.go:394-406`), which is what the alignment counts.
fn written_key_len(name: &str) -> usize {
    if name.contains('"') || name.contains(KEY_VALUE_DELIMITERS) {
        name.len() + 2
    } else if name.contains('`') {
        name.len() + 6
    } else {
        name.len()
    }
}

/// `file.go:440-443`: a key name carrying a quote or a delimiter is wrapped in
/// backticks, and one carrying a backtick is wrapped in triple quotes. The two
/// arms are in that order, so a name carrying both takes the first.
fn quote_key(name: &str) -> String {
    if name.contains('"') || name.contains(KEY_VALUE_DELIMITERS) {
        format!("`{name}`")
    } else if name.contains('`') {
        format!(r#""""{name}""""#)
    } else {
        name.to_owned()
    }
}

/// `file.go:461-468`, the three value-quoting arms in their own order.
///
/// A newline or a backtick takes the triple-quote arm; a `#` or a `;` takes the
/// backtick arm, because `IgnoreInlineComment` is off; leading or trailing
/// whitespace takes the double-quote arm. The `quote_backtick`, `quote_hash`,
/// `quote_semicolon` and `quote_trailing_space` cases fire one arm each.
///
/// The whitespace test is `len(strings.TrimSpace(val)) != len(val)`, which is a
/// byte-length comparison over Go's `unicode.IsSpace`. Rust's `str::trim` uses
/// the same White_Space property, so `trim().len() != len()` is the same test.
fn quote_value(value: &str) -> String {
    if value.contains('\n') || value.contains('`') {
        format!(r#""""{value}""""#)
    } else if value.contains('#') || value.contains(';') {
        format!("`{value}`")
    } else if value.trim().len() != value.len() {
        format!("\"{value}\"")
    } else {
        value.to_owned()
    }
}

/// One comment line as the writer spells it back.
///
/// Sections and keys differ by one `TrimSpace`: a section comment line that
/// does not already begin with a marker is prefixed verbatim
/// (`file.go:351-352`), and a key comment line is trimmed first
/// (`file.go:421-422`). Both trim the text after an existing marker
/// (`file.go:354`, `:424`).
fn normalise_comment(line: &str, is_key: bool) -> String {
    let mut characters = line.chars();
    match characters.next() {
        Some(marker @ ('#' | ';')) => {
            let rest: String = characters.collect();
            format!("{marker} {}", rest.trim())
        }
        // `lines[i][0]` on an empty line is a panic in Go; it cannot be reached
        // from a parsed comment, because the buffer is trimmed before it is
        // stored and every part of the split carries its marker.
        _ if is_key => format!("; {}", line.trim()),
        _ => format!("; {line}"),
    }
}

// ---------------------------------------------------------------------------
// The parser: ini.v1's Load
// ---------------------------------------------------------------------------

impl IniFile {
    /// `ini.Load` over a byte slice with every option at its zero value
    /// (`ini.go:156-158`, `parser.go:350-527`).
    ///
    /// The `DEFAULT` section is created before a line is read
    /// (`parser.go:367-371`), so it is always section zero even for a file that
    /// never mentions it, and the writer then skips it while it is empty.
    ///
    /// Bytes reach this as text through a lossy conversion, because Go strings
    /// are bytes and Rust strings are not. A `sdk_config.ini` that is not valid
    /// UTF-8 therefore has its invalid bytes replaced where Go would carry them
    /// through a load and a save unchanged. That is a candidate deviation; the
    /// file holds a path, an address, a name and a base64 GUID.
    pub fn load(bytes: &[u8]) -> Result<Self, IniError> {
        let bytes = strip_bom(bytes);
        let text = String::from_utf8_lossy(bytes);
        Parser::new(&text).run()
    }
}

/// `parser.BOM` (`parser.go:75-106`): a UTF-16 mark is two bytes and a UTF-8
/// mark is three, and anything else is content.
fn strip_bom(bytes: &[u8]) -> &[u8] {
    match bytes {
        [0xfe, 0xff, rest @ ..] | [0xff, 0xfe, rest @ ..] => rest,
        [0xef, 0xbb, 0xbf, rest @ ..] => rest,
        _ => bytes,
    }
}

/// The parser's cursor over the raw lines, each still carrying its own line
/// break, which is what `readUntil('\n')` hands back (`parser.go:108-118`).
struct Parser<'a> {
    lines: Vec<&'a str>,
    at: usize,
    comment: String,
    count: u32,
    file: IniFile,
    section: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        let mut lines = Vec::new();
        let mut rest = text;
        while !rest.is_empty() {
            match rest.find('\n') {
                Some(index) => {
                    lines.push(&rest[..=index]);
                    rest = &rest[index + 1..];
                }
                None => {
                    lines.push(rest);
                    rest = "";
                }
            }
        }
        Self {
            lines,
            at: 0,
            comment: String::new(),
            count: 1,
            // `parser.go:371`.
            file: IniFile::empty(),
            section: 0,
        }
    }

    /// One `readUntil('\n')`, answering the line and whether that read reached
    /// the end, which is Go's `p.isEOF`.
    fn read_line(&mut self) -> (&'a str, bool) {
        match self.lines.get(self.at) {
            Some(line) => {
                self.at += 1;
                (line, !line.ends_with('\n'))
            }
            None => ("", true),
        }
    }

    fn run(mut self) -> Result<IniFile, IniError> {
        loop {
            let (raw, eof) = self.read_line();
            let line = trim_start_space(raw);
            if line.is_empty() {
                if eof {
                    break;
                }
                continue;
            }

            // `parser.go:424-430`: the raw line, line break and all, joins the
            // pending comment.
            if line.starts_with('#') || line.starts_with(';') {
                self.comment.push_str(line);
                if eof {
                    break;
                }
                continue;
            }

            // `parser.go:433-467`.
            if line.starts_with('[') {
                let Some(close) = line.rfind(']') else {
                    return Err(IniError::UnclosedSection(line.to_owned()));
                };
                let name = &line[1..close];
                let Some(index) = self.file.new_section(name) else {
                    return Err(IniError::EmptySectionName);
                };
                self.section = index;
                if let Some(comment) = clean_comment(&line[close + 1..]) {
                    self.comment.push_str(comment);
                }
                self.file.sections[index].comment = self.comment.trim().to_owned();
                self.comment.clear();
                self.count = 1;
                if eof {
                    break;
                }
                continue;
            }

            // `parser.go:476-502`.
            let (mut name, offset) = read_key_name(line)?;
            if name.is_empty() {
                // `parser.go:518-521` returns `Section.NewKey`'s error, whose
                // text is `error creating new key: empty key name`
                // (`section.go:68`). The condition is the one that matters and
                // neither text is read: every wire-pod caller of `ini.Load`
                // discards the error.
                return Err(IniError::EmptyKeyName(line.to_owned()));
            }
            let mut auto_increment = false;
            if name == "-" {
                // `parser.go:504-510`.
                auto_increment = true;
                name = format!("#{}", self.count);
                self.count += 1;
            }

            let value = self.read_value(&line[offset..])?;
            // `parser.go:518-524`.
            let section = &mut self.file.sections[self.section];
            section.new_key(&name, &value);
            if let Some(key) = section.keys.iter_mut().find(|key| key.name == name) {
                key.auto_increment = auto_increment;
                key.comment = self.comment.trim().to_owned();
            }
            self.comment.clear();

            if eof {
                break;
            }
        }
        Ok(self.file)
    }

    /// `parser.readValue` with every option at its zero value
    /// (`parser.go:236-311`).
    fn read_value(&mut self, input: &str) -> Result<String, IniError> {
        let line = trim_start_space(input);
        if line.is_empty() {
            return Ok(String::new());
        }

        // `parser.go:246-253`. `UnescapeValueDoubleQuotes` is off, so a leading
        // `"` is not a quote here and is trimmed later by `hasSurroundedQuote`.
        let quote = if line.len() > 3 && line.starts_with(r#"""""#) {
            Some(r#"""""#)
        } else if line.starts_with('`') {
            Some("`")
        } else {
            None
        };

        if let Some(quote) = quote {
            let start = quote.len();
            return match line[start..].rfind(quote) {
                // `parser.go:266`.
                Some(position) => Ok(line[start..start + position].to_owned()),
                // `parser.go:260`.
                None => self.read_multilines(line, &line[start..], quote),
            };
        }

        // `parser.go:269-277`.
        let line = line.trim();
        if let Some(head) = line.strip_suffix('\\') {
            return Ok(self.read_continuation_lines(head));
        }

        // `parser.go:280-297`: `IgnoreInlineComment` and
        // `SpaceBeforeInlineComment` are both off, so the first `#` or `;`
        // anywhere ends the value.
        let mut value = line;
        if let Some(index) = line.find(['#', ';']) {
            self.comment.push_str(&line[index..]);
            value = line[..index].trim();
        }

        // `parser.go:300-302`.
        if has_surrounded_quote(value, '\'') || has_surrounded_quote(value, '"') {
            value = &value[1..value.len() - 1];
        }
        Ok(value.to_owned())
    }

    /// `parser.readMultilines` (`parser.go:175-206`): a `"""` or backtick value
    /// that its own line does not close runs on until a line that does.
    fn read_multilines(
        &mut self,
        line: &str,
        start: &str,
        quote: &str,
    ) -> Result<String, IniError> {
        let mut value = start.to_owned();
        loop {
            let (next, eof) = self.read_line();
            if let Some(position) = next.rfind(quote) {
                // `parser.go:186-190`: a backslash after the closing quote is a
                // continuation and the whole line joins the value instead.
                let after = next[position + quote.len()..].trim_end_matches(['\r', '\n']);
                if after.trim().ends_with('\\') {
                    value.push_str(next);
                    continue;
                }
                value.push_str(&next[..position]);
                if let Some(comment) = clean_comment(&next[position..]) {
                    self.comment.push_str(comment.trim());
                }
                return Ok(value);
            }
            value.push_str(next);
            if eof {
                // `parser.go:202`.
                return Err(IniError::UnterminatedValue(line.to_owned()));
            }
        }
    }

    /// `parser.readContinuationLines` (`parser.go:208-226`): a value ending in
    /// a backslash takes the next line, trimmed, and keeps going while the
    /// joined value still ends in one.
    fn read_continuation_lines(&mut self, start: &str) -> String {
        let mut value = start.to_owned();
        loop {
            let (next, _) = self.read_line();
            let next = next.trim();
            if next.is_empty() {
                return value;
            }
            value.push_str(next);
            if !value.ends_with('\\') {
                return value;
            }
            value.pop();
        }
    }
}

/// `parser.readKeyName` with the delimiters `newFile` filled in
/// (`parser.go:128-173`), answering the name and where the value starts.
fn read_key_name(line: &str) -> Result<(String, usize), IniError> {
    let quote = if line.starts_with('"') {
        if line.len() > 6 && line.starts_with(r#"""""#) {
            Some(r#"""""#)
        } else {
            Some("\"")
        }
    } else if line.starts_with('`') {
        Some("`")
    } else {
        None
    };

    if let Some(quote) = quote {
        let start = quote.len();
        let Some(position) = line[start..].find(quote) else {
            return Err(IniError::MissingClosingKeyQuote(line.to_owned()));
        };
        let position = position + start;
        let Some(index) = line[position + start..].find(KEY_VALUE_DELIMITERS) else {
            return Err(IniError::DelimiterNotFound(line.to_owned()));
        };
        let end = position + index;
        return Ok((line[start..position].trim().to_owned(), end + start + 1));
    }

    let Some(end) = line.find(KEY_VALUE_DELIMITERS) else {
        return Err(IniError::DelimiterNotFound(line.to_owned()));
    };
    if end == 0 {
        return Err(IniError::EmptyKeyName(line.to_owned()));
    }
    Ok((line[..end].trim().to_owned(), end + 1))
}

/// `parser.cleanComment` (`parser.go:120-126`): whatever follows the first `#`
/// or `;`, or nothing.
fn clean_comment(input: &str) -> Option<&str> {
    input.find(['#', ';']).map(|index| &input[index..])
}

/// `parser.hasSurroundedQuote` (`parser.go:231-234`): the first and last
/// characters are the quote and no other character is.
fn has_surrounded_quote(input: &str, quote: char) -> bool {
    let bytes = input.as_bytes();
    let quote = quote as u8;
    bytes.len() >= 2
        && bytes[0] == quote
        && bytes[bytes.len() - 1] == quote
        && bytes[1..].iter().position(|b| *b == quote) == Some(bytes.len() - 2)
}

/// `bytes.TrimLeftFunc(line, unicode.IsSpace)`.
///
/// Go's `unicode.IsSpace` and Rust's `char::is_whitespace` are both the Unicode
/// White_Space property, so the two trim the same characters.
fn trim_start_space(input: &str) -> &str {
    input.trim_start()
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// What [`SdkIniStore::write_to_ini_secondary`] did.
#[derive(Debug)]
pub enum SecondaryOutcome {
    /// A section folded to the serial, so `guid` and `ip` were set and the file
    /// was rewritten (`botInfoStorer.go:78-89`). Carries the bot name the
    /// matched section held, or `None` where Go would dereference a nil key.
    Updated(Option<String>),
    /// No section matched, so Go would have downloaded the session certificate
    /// from the DDL servers (`botInfoStorer.go:91-111`). Deviation 36: the
    /// servers are gone, so nothing is fetched, nothing is written, and Go's
    /// own line at `:95` is logged in place of the download.
    DdlUnavailable,
}

/// `~/.anki_vector/sdk_config.ini` and the three functions that rewrite it.
///
/// Go reloads the file inside every one of them, so there is no in-memory copy
/// to keep: each call here reads, edits and writes. What the store holds is the
/// directory in Go's own spelling, the gate that writes the file, and the turn
/// that keeps a load-edit-save from interleaving with another one. Go has
/// neither: two robots authenticating at once race, and `os.WriteFile`
/// truncates in place while the other reads.
#[derive(Debug)]
pub struct SdkIniStore {
    /// `vars.SDKIniPath` as [`crate::paths::sdk_ini_dir`] spells it: a trailing
    /// separator, and on Windows the mixed separators the concatenation at
    /// `vars.go:207-209` produces.
    dir: String,
    /// `SDKIniPath + "sdk_config.ini"`, with the mode `SaveTo` uses.
    gate: WriteGate,
    /// Whose turn it is to load, edit and save. A [`tokio::sync::Mutex`]
    /// because it is deliberately held across the read and the write, which is
    /// the same reason [`WriteGate`]'s own turn is one. The gate's turn is
    /// uncontended underneath it, since every writer of this file takes this
    /// one first.
    turn: tokio::sync::Mutex<()>,
}

impl SdkIniStore {
    /// The store for the SDK directory `sdk_ini_dir`, which is
    /// [`crate::paths::sdk_ini_dir`]'s answer and carries a trailing separator.
    pub fn new(sdk_ini_dir: impl Into<String>) -> Self {
        let dir = sdk_ini_dir.into();
        Self {
            gate: WriteGate::new(sdk_config_path(&dir), SDK_INI_FILE_MODE),
            dir,
            turn: tokio::sync::Mutex::new(()),
        }
    }

    /// The SDK directory, as Go spells it, trailing separator included.
    pub fn dir(&self) -> &str {
        &self.dir
    }

    /// The ini file, as Go spells it.
    pub fn path(&self) -> &str {
        self.gate.path()
    }

    /// `WriteToIniPrimary` (`botInfoStorer.go:31-62`), called from
    /// `jdocs/server.go:124` when a robot completes primary authentication.
    ///
    /// The `bot already in INI matched` line is written once, where Go writes
    /// it once per matching section (`:42`, inside the loop). The two only
    /// differ for a file naming one serial in two cases, and nothing but the
    /// log reads the count.
    ///
    /// # Errors
    ///
    /// Whatever the write reports. Go discards `SaveTo`'s error (`:60`).
    pub async fn write_to_ini_primary(
        &self,
        bot_name: &str,
        esn: &str,
        guid: &str,
        ip: &str,
    ) -> io::Result<()> {
        let _turn = self.turn.lock().await;
        let mut file = self.load().await;

        match file.write_to_ini_primary(&self.dir, bot_name, esn, guid, ip) {
            // `botInfoStorer.go:42`.
            IniEdit::Updated => tracing::debug!(
                comp = "",
                "WriteToIniPrimary: bot already in INI matched, setting info"
            ),
            // `botInfoStorer.go:50`.
            IniEdit::Created => tracing::debug!(
                comp = "",
                "WriteToIniPrimary: ESN did not match any section in sdk config file, creating"
            ),
            // `botInfoStorer.go:51-54`, where Go logs the error and then
            // dereferences the nil section it was handed. Deviation 31's
            // family: the file is left alone and nothing is written.
            IniEdit::Refused => {
                tracing::debug!(
                    comp = "",
                    "WriteToIniPrimary: ESN did not match any section in sdk config file, creating"
                );
                tracing::debug!(comp = "", "empty section name");
                return Ok(());
            }
        }

        let written = self.save(&file).await;
        // `botInfoStorer.go:61`, which Go writes whatever `SaveTo` answered.
        tracing::debug!(comp = "", "WriteToIniPrimary: successfully wrote INI");
        written
    }

    /// `WriteToIniSecondary` (`botInfoStorer.go:66-128`), called from
    /// `jdocs/server.go:140` for a robot wire-pod has never seen.
    ///
    /// The `Name found from ESN in INI` line is written once, after the update
    /// rather than before it, where Go writes it inside the loop and so once
    /// per matching section (`:80`). Neither difference is observable outside
    /// the log.
    ///
    /// Deviation 36 ends the call where Go would fetch the certificate, so the
    /// create arm at `:115-125` is unreachable from here;
    /// [`IniFile::write_to_ini_secondary_create`] is the arm itself, which the
    /// recording pins and which P7 may reach from a certificate on disk.
    ///
    /// # Errors
    ///
    /// Whatever the write reports.
    pub async fn write_to_ini_secondary(
        &self,
        esn: &str,
        guid: &str,
        ip: &str,
    ) -> io::Result<SecondaryOutcome> {
        let _turn = self.turn.lock().await;
        let mut file = self.load().await;

        let Some(bot_name) = file.write_to_ini_secondary_update(esn, guid, ip) else {
            // `botInfoStorer.go:91`.
            tracing::debug!(
                comp = "",
                "WriteToIniSecondary: getting session cert from DDL server"
            );
            // `botInfoStorer.go:95`, which Go writes when the fetch fails.
            // Deviation 36 writes it in place of the fetch.
            tracing::debug!(
                comp = "",
                "The DDL servers are down at the moment. The cert will not be gotten. The Python SDK will not be configured."
            );
            return Ok(SecondaryOutcome::DdlUnavailable);
        };

        // `botInfoStorer.go:80`.
        tracing::debug!(
            comp = "",
            "WriteToIniSecondary: Name found from ESN in INI, setting info"
        );
        // `botInfoStorer.go:112`, with Go's empty string where the name is
        // missing.
        tracing::debug!(
            comp = "",
            "WriteToIniSecondary: robot name is {}",
            bot_name.clone().unwrap_or_default()
        );
        // `botInfoStorer.go:126`, which Go writes before the save.
        tracing::debug!(comp = "", "WriteToIniSecondary complete");
        self.save(&file).await?;
        Ok(SecondaryOutcome::Updated(bot_name))
    }

    /// `ChangeGUIDInIni` (`token/token.go:148-178`), called from
    /// `token.go:228` after a robot's GUID changes.
    ///
    /// Go returns without writing when `ini.Load` fails (`:156-159`), which is
    /// the one place of the three that does not fall back to an empty file, so
    /// a missing `sdk_config.ini` leaves this a no-op. That is reproduced.
    ///
    /// # Errors
    ///
    /// Whatever the write reports.
    pub async fn update_ip_and_guid(&self, esn: &str, bot_info: &BotInfo) -> io::Result<()> {
        let _turn = self.turn.lock().await;

        // `token.go:155-159`: a failed load logs the error and returns, where
        // the two `botInfoStorer.go` writers fall back to an empty file. The
        // text is this module's rather than Go's `*fs.PathError` or the
        // library's own parse error, the same difference the configuration
        // layer records.
        let bytes = match read_if_present(self.path()).await {
            Some(bytes) => bytes,
            None => {
                tracing::debug!(comp = "", "could not read {}", self.path());
                return Ok(());
            }
        };
        let mut file = match IniFile::load(&bytes) {
            Ok(file) => file,
            Err(error) => {
                tracing::debug!(comp = "", "{error}");
                return Ok(());
            }
        };

        let unmatched = file.update_ip_and_guid(esn, bot_info);
        for _ in 0..unmatched {
            // `token.go:174`, once per robot that found no section.
            tracing::debug!(
                comp = "",
                "Bot is not in sdk_config.ini. Clear your bot's userdata and try authenticating again to create it."
            );
        }
        self.save(&file).await
    }

    /// Writes one robot's session certificate beside the ini file, which is
    /// `os.WriteFile(filepath.Join(SDKIniPath, name+"-"+esn+".cert"), ..., 0755)`
    /// (`jdocs/server.go:114`, `:121`), creating `SDKIniPath` first if it is
    /// missing (`:115-118`).
    ///
    /// The file takes its own gate, because it is a different file from the ini
    /// and a gate covers one file.
    ///
    /// # Errors
    ///
    /// Whatever the write reports. Go discards it (`jdocs/server.go:121`).
    pub async fn write_cert(
        &self,
        bot_name: &str,
        esn: &str,
        certificate: Vec<u8>,
    ) -> io::Result<()> {
        let _turn = self.turn.lock().await;
        self.create_dir_if_missing().await;
        let path = cert_file_path(&self.dir, bot_name, esn);
        let gate = WriteGate::new(path.to_string_lossy().into_owned(), SDK_CERT_FILE_MODE);
        gate.write(move || certificate).await
    }

    /// `ini.Load(SDKIniPath + "sdk_config.ini")` with both writers' fallback
    /// (`botInfoStorer.go:32-37`, `:70-75`).
    ///
    /// Any failure, a missing file and a syntax error alike, logs `Creating
    /// <dir> directory`, creates the directory and starts from `ini.Empty()`.
    /// A file that fails to parse is therefore replaced rather than repaired,
    /// which is Go's and is reproduced.
    async fn load(&self) -> IniFile {
        let bytes = read_if_present(self.path()).await;
        match bytes.as_deref().map(IniFile::load) {
            Some(Ok(file)) => file,
            _ => {
                // `botInfoStorer.go:34`, `:72`.
                tracing::debug!(comp = "", "Creating {} directory", self.dir);
                self.create_dir().await;
                IniFile::empty()
            }
        }
    }

    /// `SaveTo` (`file.go:526-535`) through the gate: the same bytes, the same
    /// mode, and a rename over the target where Go truncates in place.
    async fn save(&self, file: &IniFile) -> io::Result<()> {
        let bytes = file.to_bytes();
        self.gate.write(move || bytes).await
    }

    /// `jdocs/server.go:115-118`: stat the directory, and only when the stat
    /// fails write the `Creating <dir> directory` line and create it.
    async fn create_dir_if_missing(&self) {
        let dir = self.dir.clone();
        let present = tokio::task::spawn_blocking(move || std::fs::metadata(dir).is_ok())
            .await
            .unwrap_or(false);
        if present {
            return;
        }
        // `jdocs/server.go:116`.
        tracing::debug!(comp = "", "Creating {} directory", self.dir);
        self.create_dir().await;
    }

    /// `os.Mkdir(vars.SDKIniPath, 0755)` with its error discarded, as all three
    /// call sites discard it (`jdocs/server.go:117`, `botInfoStorer.go:35`,
    /// `:73`).
    ///
    /// `os.Mkdir` creates one component, not a path, so this is
    /// [`std::fs::create_dir`] and not `create_dir_all`. The mode is applied on
    /// Unix and ignored on Windows, which is what Go's own `os.Mkdir` does:
    /// `syscall.Mkdir` takes the mode on Unix and `CreateDirectory` takes none.
    async fn create_dir(&self) {
        let dir = self.dir.clone();
        let _ = tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let _ = std::fs::DirBuilder::new()
                    .mode(SDK_INI_DIR_MODE)
                    .create(&dir);
            }
            #[cfg(not(unix))]
            {
                let _ = std::fs::create_dir(&dir);
            }
        })
        .await;
    }
}

/// The file's bytes, or `None` however the read failed, which is the only
/// distinction `ini.Load`'s callers make.
///
/// Blocking, inside `spawn_blocking`, for the reason
/// [`crate::persist::write_atomic`] gives.
async fn read_if_present(path: &str) -> Option<Vec<u8>> {
    let path = PathBuf::from(path);
    tokio::task::spawn_blocking(move || std::fs::read(path).ok())
        .await
        .ok()
        .flatten()
}
