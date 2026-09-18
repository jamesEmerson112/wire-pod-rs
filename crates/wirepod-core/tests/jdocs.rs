//! `jdocs.json`: the byte layout, `encoding/json`'s decoder, and the four
//! operations Go's `vars` package performs on the list.
//!
//! The load-bearing case is [`the_live_file_round_trips_byte_for_byte`], which
//! reads the committed copy of this machine's own `jdocs.json` and asserts that
//! parsing and re-marshalling it reproduces all 5336 bytes. Everything else
//! exists to say which part broke when that one fails.
//!
//! The fixture is the live file with exactly two kinds of value replaced by
//! same-byte-length ASCII placeholders: every `hash` inside the `vic.AppTokens`
//! document's `json_doc`, which is a token hash, and the `vic.RobotSettings`
//! `default_location`, which is the operator's own address. Both were replaced
//! by a script that worked on the raw bytes, never printed either value, and
//! asserted that the output was the same length as the input and still
//! re-marshalled to itself through Go's own structs. The serial, the versions
//! and the timestamps are the real ones, so the fixture is the real document's
//! shape, order and length with none of its secrets.
//!
//! Every expected byte string below either comes out of the fixture or was
//! printed by a throwaway Go program built from the structs at `vars.go:130-144`
//! and transcribed here. None of them is hand-written, because the point of
//! each is what Go's marshaller does rather than what this file's author
//! believes it does.
//!
//! Everything that touches the disk runs in a directory under the system
//! temporary directory, named for the process, so nothing here can reach the
//! repository or the live `%APPDATA%\wire-pod` the Go server is serving from.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing_subscriber::layer::SubscriberExt;
use wirepod_core::gojson::{DecodeFault, Extra};
use wirepod_core::logger::{LogLayer, LogRing, ManualLogClock};
use wirepod_core::paths::DataDir;
use wirepod_core::store::jdocs::{
    AddOutcome, BotJdoc, JDOCS_FILE_MODE, Jdoc, JdocsDecodeError, JdocsLoadOutcome, JdocsStore,
    marshal_jdocs, parse_jdocs,
};
use wirepod_core::test_support::install_tracing_backstop;

/// The committed copy of this machine's `jdocs.json`, redacted.
const FIXTURE: &str =
    include_str!("../../../docs/phases/P1-robot-connect-auth/fixtures/jdocs.json");

/// What the live file measured when it was copied. A second statement of the
/// number, so that a fixture regenerated from a changed live file fails here
/// rather than quietly moving what "byte for byte" means.
const FIXTURE_LEN: usize = 5336;

/// A ceiling on anything awaited, generous enough that only a hang reaches it.
/// Real durations, because this crate's tests never pause the runtime's clock.
const CEILING: Duration = Duration::from_secs(30);

/// The five documents the robot stores, in the order the file holds them.
const DOCUMENT_NAMES: [&str; 5] = [
    "vic.AppTokens",
    "vic.RobotSettings",
    "vic.AccountSettings",
    "vic.UserEntitlements",
    "vic.RobotLifetimeStats",
];

/// The one serial this machine has ever authenticated, as the file spells the
/// `thing`.
const THING: &str = "vic:00303f28";

// ---------------------------------------------------------------------------
// A directory to work in
// ---------------------------------------------------------------------------

/// A directory under the system temporary directory, removed when the test
/// ends.
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
            "wirepod-jdocs-{label}-{}-{nanos:x}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("could not create the temporary directory");
        // `vars.Init` creates the jdocs directory before anything writes into
        // it (`vars.go:185`); nothing in this crate does, so the test stands in
        // for that step.
        fs::create_dir_all(path.join("jdocs")).expect("could not create the jdocs directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// The data directory a boot would be pointed at.
    fn data_dir(&self) -> DataDir {
        DataDir::rooted(&self.path)
    }

    /// Where `jdocs.json` lands, spelled with one separator so a test can open
    /// it without depending on how Go spells the same file.
    fn jdocs_file(&self) -> PathBuf {
        self.path().join("jdocs").join("jdocs.json")
    }

    /// The bytes currently in `jdocs.json`.
    fn file(&self) -> Vec<u8> {
        fs::read(self.jdocs_file()).expect("jdocs.json is missing")
    }

    /// Puts `bytes` in `jdocs.json`, replacing anything there.
    fn seed(&self, bytes: &[u8]) {
        fs::write(self.jdocs_file(), bytes).expect("could not seed jdocs.json");
    }

    /// The names in the jdocs directory, sorted, so a leftover temporary is
    /// visible.
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.path.join("jdocs"))
            .expect("could not list the jdocs directory")
            .map(|entry| {
                entry
                    .expect("could not read a directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// Small builders
// ---------------------------------------------------------------------------

/// A document with the two versions set and nothing else, which is the shape
/// every jdocs writer but the token server produces.
fn doc(doc_version: u64, json_doc: &str) -> Jdoc {
    Jdoc {
        doc_version,
        fmt_version: 1,
        json_doc: json_doc.to_owned(),
        ..Jdoc::default()
    }
}

/// One entry.
fn entry(thing: &str, name: &str, jdoc: Jdoc) -> BotJdoc {
    BotJdoc {
        thing: thing.to_owned(),
        name: name.to_owned(),
        jdoc,
        extra: Extra::new(),
    }
}

/// The marshalled form of `docs`, as text.
fn written(docs: &[BotJdoc]) -> String {
    String::from_utf8(marshal_jdocs(docs)).expect("the rewrite is not UTF-8")
}

/// The fixture, parsed.
fn fixture() -> Vec<BotJdoc> {
    parse_jdocs(FIXTURE.as_bytes()).expect("the live file did not parse")
}

/// The instant every ring below stamps its entries with. Anything but zero:
/// `get_entries` keeps an entry only when its stamp is strictly greater than
/// the `since` it was asked for (`logger.go:227`).
const STAMPED_AT: i64 = 1_767_000_000_000;

/// Every log line one body recorded, as `(level, comp, msg)`.
fn recorded_lines(ring: &LogRing) -> Vec<(String, String, String)> {
    ring.get_entries(wirepod_core::logger::LogLevel::Debug, 0)
        .into_iter()
        .map(|entry| (entry.level, entry.comp, entry.msg))
        .collect()
}

/// Installs `ring` as this thread's subscriber until the guard is dropped, over
/// the process-wide backstop.
///
/// Every test below that reaches `vars.go:242`'s log line holds one of these,
/// including the ones that assert nothing about the log. That is not tidiness:
/// `tracing` caches per callsite whether anyone is interested in it, globally
/// and across threads, and while at most one dispatcher is registered it
/// decides that by asking only the calling thread, so a callsite first reached
/// while nothing is installed caches "never" for the rest of the process. The
/// harness runs these tests in parallel, so a loader test without a ring would
/// decide, for whichever log test ran next, that the line is uninteresting.
///
/// [`install_tracing_backstop`] is what closes that, and its doc says why a
/// global default is the only shape that does: it is the one subscriber that is
/// live on every thread, so no thread can ever be the one that caches "never".
/// It replaces the `rebuild_interest_cache` this helper used to call, which
/// only repaired callsites that already existed when it ran. The `set_default`
/// stays, because a scoped default wins over the global on its own thread and
/// is what lets each test read its own ring back.
fn watching(ring: &Arc<LogRing>) -> tracing::subscriber::DefaultGuard {
    install_tracing_backstop();
    let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(ring)));
    tracing::subscriber::set_default(subscriber)
}

/// A ring wired to a subscriber, for a body that logs.
fn ring() -> Arc<LogRing> {
    Arc::new(LogRing::new(Arc::new(ManualLogClock::new(
        STAMPED_AT,
        "2026.01.02 03:04:05",
    ))))
}

// ---------------------------------------------------------------------------
// The byte round trip
// ---------------------------------------------------------------------------

/// The one case the rest of this file exists to explain. A real `jdocs.json`,
/// written by the Go server on this machine, parses and marshals back to
/// exactly the bytes it arrived as.
#[test]
fn the_live_file_round_trips_byte_for_byte() {
    assert_eq!(
        FIXTURE.len(),
        FIXTURE_LEN,
        "the fixture is no longer the {FIXTURE_LEN} bytes the live file measured"
    );
    assert!(
        !FIXTURE.ends_with('\n'),
        "the fixture grew a trailing newline; os.WriteFile writes json.Marshal's bytes and nothing else (vars.go:316-317)"
    );
    assert!(
        !FIXTURE.contains('\r'),
        "the fixture grew a carriage return"
    );

    let docs = fixture();

    assert_eq!(
        written(&docs),
        FIXTURE,
        "the rewrite of a real jdocs.json is not the bytes it came from"
    );
}

/// The fixture is the shape it claims to be, so that a regenerated one that
/// quietly lost a document cannot pass the round trip by comparing two equally
/// wrong files.
#[test]
fn the_fixture_is_the_five_documents_this_machine_holds() {
    let docs = fixture();

    assert_eq!(
        docs.len(),
        5,
        "the live file no longer holds five documents"
    );
    assert_eq!(
        docs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        DOCUMENT_NAMES,
        "the five documents, or their order, moved"
    );
    assert!(
        docs.iter().all(|d| d.thing == THING),
        "the live file gained a second robot"
    );
    assert!(
        docs.iter()
            .all(|d| d.extra.is_empty() && d.jdoc.extra.is_empty()),
        "the live file carried a key these structs do not name"
    );
}

/// `client_metadata` carries `omitempty` (`vars.go:133`), and only the token
/// server ever sets it (`token/token.go:106`), so exactly one of the five
/// documents has the key at all. A struct that wrote `""` back would add four
/// keys to the file and still parse.
#[test]
fn exactly_one_document_carries_client_metadata() {
    let docs = fixture();

    let carrying: Vec<&str> = docs
        .iter()
        .filter(|d| !d.jdoc.client_metadata.is_empty())
        .map(|d| d.name.as_str())
        .collect();

    assert_eq!(
        carrying,
        ["vic.AppTokens"],
        "the set of documents carrying client_metadata moved"
    );
    assert_eq!(
        FIXTURE.matches("\"client_metadata\":").count(),
        1,
        "the file carries the key more than once"
    );
}

/// The two redacted values are placeholders of the length the live ones had, so
/// a fixture regenerated without the redaction step fails here rather than
/// committing a hash.
#[test]
fn the_fixture_carries_placeholders_and_not_the_live_values() {
    let docs = fixture();
    let tokens = docs
        .iter()
        .find(|d| d.name == "vic.AppTokens")
        .expect("the fixture has no vic.AppTokens document");
    let settings = docs
        .iter()
        .find(|d| d.name == "vic.RobotSettings")
        .expect("the fixture has no vic.RobotSettings document");

    let hash = field_of(&tokens.jdoc.json_doc, "client_tokens", "hash");
    assert!(
        hash.starts_with("REDACTED"),
        "the fixture's token hash is not a placeholder"
    );
    assert_eq!(hash.len(), 64, "the hash placeholder changed length");

    let location = settings_field(&settings.jdoc.json_doc, "default_location");
    assert!(
        location.starts_with("REDACTED"),
        "the fixture's default_location is not a placeholder"
    );
    assert_eq!(
        location.len(),
        43,
        "the location placeholder changed length"
    );
}

/// One string out of the first element of an array inside a `json_doc`.
fn field_of(json_doc: &str, array: &str, key: &str) -> String {
    let value: serde_json::Value =
        serde_json::from_str(json_doc).expect("the json_doc is not JSON");
    value[array][0][key]
        .as_str()
        .expect("the key is not a string")
        .to_owned()
}

/// One string out of a `json_doc` object.
fn settings_field(json_doc: &str, key: &str) -> String {
    let value: serde_json::Value =
        serde_json::from_str(json_doc).expect("the json_doc is not JSON");
    value[key]
        .as_str()
        .expect("the key is not a string")
        .to_owned()
}

// ---------------------------------------------------------------------------
// The tags, the order and `omitempty`
// ---------------------------------------------------------------------------

/// Each of the four `omitempty` fields disappears on its own and leaves the
/// other three where they were, and the entry's own three keys are written
/// whatever they hold.
///
/// Every expected string is the stdout of a throwaway Go program built from
/// `vars.go:130-144`, marshalling the same six values.
#[test]
fn each_omitempty_field_is_dropped_on_its_own() {
    let full = Jdoc {
        doc_version: 3,
        fmt_version: 2,
        client_metadata: "m".to_owned(),
        json_doc: "d".to_owned(),
        extra: Extra::new(),
    };
    let one = |jdoc: Jdoc| written(&[entry("t", "n", jdoc)]);

    assert_eq!(
        one(full.clone()),
        r#"[{"thing":"t","name":"n","jdoc":{"doc_version":3,"fmt_version":2,"client_metadata":"m","json_doc":"d"}}]"#,
        "the field order or the tags moved"
    );
    assert_eq!(
        one(Jdoc {
            doc_version: 0,
            ..full.clone()
        }),
        r#"[{"thing":"t","name":"n","jdoc":{"fmt_version":2,"client_metadata":"m","json_doc":"d"}}]"#,
        "vars.go:131 carries omitempty"
    );
    assert_eq!(
        one(Jdoc {
            fmt_version: 0,
            ..full.clone()
        }),
        r#"[{"thing":"t","name":"n","jdoc":{"doc_version":3,"client_metadata":"m","json_doc":"d"}}]"#,
        "vars.go:132 carries omitempty"
    );
    assert_eq!(
        one(Jdoc {
            client_metadata: String::new(),
            ..full.clone()
        }),
        r#"[{"thing":"t","name":"n","jdoc":{"doc_version":3,"fmt_version":2,"json_doc":"d"}}]"#,
        "vars.go:133 carries omitempty"
    );
    assert_eq!(
        one(Jdoc {
            json_doc: String::new(),
            ..full
        }),
        r#"[{"thing":"t","name":"n","jdoc":{"doc_version":3,"fmt_version":2,"client_metadata":"m"}}]"#,
        "vars.go:134 carries omitempty"
    );
    assert_eq!(
        written(&[entry("", "", Jdoc::default())]),
        r#"[{"thing":"","name":"","jdoc":{}}]"#,
        "none of vars.go:139, :141 and :143 carries omitempty, and all four inner fields do"
    );
}

/// `json_doc` is a JSON string holding JSON text, so `<`, `>` and `&` anywhere
/// inside a robot setting reach the file, and Go escapes all three plus the two
/// JavaScript line terminators (`encoding/json/encode.go:984`, `:1005-1007`,
/// `:1030-1043`). `serde_json`'s stock formatter escapes none of the five,
/// which is deviation 17; [`marshal_jdocs`] goes through `GoFormatter`.
///
/// The expected string is the stdout of the same throwaway Go program,
/// marshalling this exact entry.
#[test]
fn the_html_characters_and_both_line_terminators_are_escaped_inside_json_doc() {
    let document = entry(
        THING,
        "vic.RobotSettings",
        Jdoc {
            doc_version: 7,
            fmt_version: 1,
            client_metadata: "a<b>c&d".to_owned(),
            json_doc: "{\"note\":\"<a href=\\\"x\\\">1 & 2</a>\u{2028}\u{2029}\"}".to_owned(),
            extra: Extra::new(),
        },
    );

    let bytes = written(&[document]);

    assert_eq!(
        bytes,
        r#"[{"thing":"vic:00303f28","name":"vic.RobotSettings","jdoc":{"doc_version":7,"fmt_version":1,"client_metadata":"a\u003cb\u003ec\u0026d","json_doc":"{\"note\":\"\u003ca href=\\\"x\\\"\u003e1 \u0026 2\u003c/a\u003e\u2028\u2029\"}"}}]"#
    );
    assert!(
        bytes.is_ascii(),
        "Go's escaping leaves no non-ASCII byte in this document"
    );
    assert_eq!(
        parse_jdocs(bytes.as_bytes()).expect("the escaped form did not parse")[0]
            .jdoc
            .client_metadata,
        "a<b>c&d",
        "the escape did not survive a round trip"
    );
}

/// Go's slice is nil as well as empty, and `json.Marshal` writes `null` for the
/// nil one. `DeleteData` builds its replacement with `var newdocs []botjdoc`
/// and only appends (`vars.go:322-325`), so an empty list is always the nil
/// one by the time anything writes it.
#[test]
fn an_empty_list_is_gos_nil_slice() {
    assert_eq!(written(&[]), "null", "vars.go:316 over a nil slice");

    assert!(
        parse_jdocs(b"null").expect("null did not parse").is_empty(),
        "a null file loads as an empty list"
    );
    assert!(
        parse_jdocs(b"[]").expect("[] did not parse").is_empty(),
        "an empty array loads as an empty list"
    );
}

// ---------------------------------------------------------------------------
// The operations
// ---------------------------------------------------------------------------

/// `vars.go:346-366`. A match replaces the document and answers the version of
/// the document that has just been stored, because Go reads `latestVersion`
/// back out of the slice after assigning into it (`vars.go:351-352`). No match
/// appends, and the version stays at the `0` it was initialised with
/// (`vars.go:347`).
#[tokio::test]
async fn add_jdoc_answers_the_stored_version_on_a_replace_and_zero_on_an_append() {
    let directory = TempDir::new("add");
    let store = JdocsStore::new(directory.data_dir().jdocs_path());

    let appended = add(&store, THING, "vic.RobotSettings", doc(4, "{}")).await;
    assert_eq!(
        appended.latest_version, 0,
        "vars.go:347: an append answers zero"
    );

    let replaced = add(&store, THING, "vic.RobotSettings", doc(5, "{}")).await;
    assert_eq!(
        replaced.latest_version, 5,
        "vars.go:352: a replace answers the incoming document's version, not the one it replaced"
    );

    let another = add(&store, THING, "vic.AppTokens", doc(1, "{}")).await;
    assert_eq!(
        another.latest_version, 0,
        "a second document under the same thing is still an append"
    );

    let docs = store.snapshot();
    assert_eq!(docs.len(), 2, "the replace appended instead of replacing");
    assert_eq!(docs[0].jdoc.doc_version, 5);
    assert_eq!(
        directory.file(),
        marshal_jdocs(&docs),
        "the file is not the list"
    );
}

/// `vars.go:352` reads `latestVersion` back out of the document it has just
/// stored, so the number answered is the incoming one whether it rose, fell or
/// is zero. A store that answered the higher of the two would pass the rising
/// case on its own, and `WriteDoc` would then tell the robot its write had been
/// accepted at a version nothing holds (`jdocs/server.go:58`).
///
/// Go answered 2, 0 and 7 for these three replacements of a stored 5.
#[tokio::test]
async fn a_replace_answers_the_incoming_version_even_when_it_falls() {
    for incoming in [2u64, 0, 7] {
        let directory = TempDir::new(&format!("replace-version-{incoming}"));
        let store = JdocsStore::with_docs(
            directory.data_dir().jdocs_path(),
            vec![entry(THING, "vic.AppTokens", doc(5, "{}"))],
        );

        let outcome = add(&store, THING, "vic.AppTokens", doc(incoming, "{}")).await;

        assert_eq!(
            outcome.latest_version, incoming,
            "vars.go:352 answers the incoming version, not the larger of the two"
        );
        assert_eq!(
            store.snapshot()[0].jdoc.doc_version,
            incoming,
            "vars.go:351 stores the incoming document whatever version it carries"
        );
    }
}

/// `vars.go:354` breaks out of the loop on the first match, so a file that
/// holds the same `(thing, name)` pair twice has only its first entry replaced
/// and its second left exactly as it was. A store that replaced every match
/// would rewrite a file the Go server would have left alone.
///
/// The expected bytes are the stdout of a throwaway Go program running
/// `AddJdoc` verbatim over this list; it answered 9 and left the second entry
/// at `doc_version` 2.
#[tokio::test]
async fn a_replace_stops_at_the_first_of_a_duplicated_pair() {
    let directory = TempDir::new("duplicate-pair");
    let store = JdocsStore::with_docs(
        directory.data_dir().jdocs_path(),
        vec![
            entry("vic:1", "vic.RobotSettings", doc(1, "first")),
            entry("vic:1", "vic.RobotSettings", doc(2, "second")),
        ],
    );

    let outcome = add(&store, "vic:1", "vic.RobotSettings", doc(9, "incoming")).await;

    assert_eq!(outcome.latest_version, 9);
    let docs = store.snapshot();
    assert_eq!(docs.len(), 2, "vars.go:357 appended instead of replacing");
    assert_eq!(
        docs[0].jdoc.json_doc, "incoming",
        "the first of the pair was not the one replaced"
    );
    assert_eq!(
        (docs[1].jdoc.doc_version, docs[1].jdoc.json_doc.as_str()),
        (2, "second"),
        "vars.go:354 breaks, so the second of a duplicated pair is untouched"
    );
    assert_eq!(
        String::from_utf8(directory.file()).expect("the file is not UTF-8"),
        r#"[{"thing":"vic:1","name":"vic.RobotSettings","jdoc":{"doc_version":9,"fmt_version":1,"json_doc":"incoming"}},{"thing":"vic:1","name":"vic.RobotSettings","jdoc":{"doc_version":2,"fmt_version":1,"json_doc":"second"}}]"#,
        "the file is not the bytes Go's AddJdoc left over the same list"
    );
}

/// `vars.go:351` assigns to `.Jdoc` and not to the entry, so a replace drops
/// whatever unknown keys the old document carried and keeps the ones the entry
/// itself carried.
#[tokio::test]
async fn a_replace_drops_the_documents_extras_and_keeps_the_entrys() {
    let directory = TempDir::new("replace-extras");
    let seeded = r#"[{"thing":"vic:1","fork_only":true,"name":"vic.RobotSettings","jdoc":{"doc_version":1,"fork_inner":7}}]"#;
    let store = JdocsStore::with_docs(
        directory.data_dir().jdocs_path(),
        parse_jdocs(seeded.as_bytes()).expect("the seed did not parse"),
    );

    add(&store, "vic:1", "vic.RobotSettings", doc(2, "{}")).await;

    let docs = store.snapshot();
    assert!(
        docs[0].extra.contains_key("fork_only"),
        "the entry's own unknown key did not survive the replace"
    );
    assert!(
        !docs[0].jdoc.extra.contains_key("fork_inner"),
        "the replaced document kept the old document's unknown key"
    );
}

/// `vars.go:332-339`. Both comparisons are `==`, and the first match wins.
#[tokio::test]
async fn get_jdoc_matches_thing_and_name_exactly() {
    let directory = TempDir::new("get");
    let store = JdocsStore::with_docs(directory.data_dir().jdocs_path(), fixture());

    let hit = store
        .get_jdoc(THING, "vic.RobotSettings")
        .expect("the document is in the fixture");
    assert_eq!(hit.doc_version, 34, "the wrong document came back");

    assert!(
        store.get_jdoc(THING, "vic.NoSuchDoc").is_none(),
        "an unknown name is a miss"
    );
    assert!(
        store
            .get_jdoc("vic:deadbeef", "vic.RobotSettings")
            .is_none(),
        "an unknown thing is a miss"
    );
    assert!(
        store
            .get_jdoc("VIC:00303F28", "vic.RobotSettings")
            .is_none(),
        "vars.go:334 compares with == and not EqualFold, so case matters"
    );
    assert_eq!(
        store.get_jdoc(THING, "vic.NoSuchDoc").unwrap_or_default(),
        Jdoc::default(),
        "a miss has to hand back the zero document token/token.go:103-107 fills in"
    );
}

/// The quirk C10 and C17 have to reproduce. `WriteTokenHash` looks the token
/// document up under the bare serial (`token/token.go:101`) and stores it under
/// `vic:` plus the serial (`token/token.go:125`), so the lookup never finds
/// what the store wrote and `vic.AppTokens` is replaced on every
/// authentication rather than accumulating client tokens.
///
/// The store takes whatever string the call site passes, which is what lets
/// those commits reproduce this without a workaround.
#[tokio::test]
async fn the_token_servers_two_spellings_never_meet() {
    let directory = TempDir::new("token-spelling");
    let store = JdocsStore::new(directory.data_dir().jdocs_path());
    let esn = "00303f28";

    // `token/token.go:101`, before anything is stored.
    assert!(
        store.get_jdoc(esn, "vic.AppTokens").is_none(),
        "a bare serial must not find a vic:-prefixed entry"
    );
    // `token/token.go:125`.
    add(&store, &format!("vic:{esn}"), "vic.AppTokens", doc(1, "{}")).await;

    assert!(
        store.get_jdoc(esn, "vic.AppTokens").is_none(),
        "the store normalised the two spellings together and fixed a Go bug"
    );
    assert!(
        store
            .get_jdoc(&format!("vic:{esn}"), "vic.AppTokens")
            .is_some(),
        "the document is not under the spelling the write used"
    );
}

/// `vars.go:321-330`. Every document belonging to one robot goes at once, the
/// file is rewritten whether or not anything matched, and the comparison is
/// `==`.
#[tokio::test]
async fn delete_data_removes_every_document_of_one_robot() {
    let directory = TempDir::new("delete");
    let mut docs = fixture();
    docs.push(entry("vic:beefcafe", "vic.RobotSettings", doc(1, "{}")));
    let store = JdocsStore::with_docs(directory.data_dir().jdocs_path(), docs);

    store
        .delete_data(THING)
        .await
        .expect("the delete did not write");

    let left = store.snapshot();
    assert_eq!(left.len(), 1, "the other robot's document went too");
    assert_eq!(left[0].thing, "vic:beefcafe");
    assert_eq!(directory.file(), marshal_jdocs(&left));

    // A `thing` nobody holds still rewrites the file (`vars.go:329`).
    fs::remove_file(directory.jdocs_file()).expect("could not take the file away");
    store
        .delete_data("vic:nosuchbot")
        .await
        .expect("the delete did not write");
    assert_eq!(
        directory.file(),
        marshal_jdocs(&left),
        "a delete that matched nothing skipped the write"
    );
}

/// `vars.go:322-328`: the replacement list starts as Go's nil slice, so
/// removing the last document leaves nil and `json.Marshal` writes the literal
/// `null`. The Go server then loads that `null` back as an empty list, which is
/// the other half of the same rule.
#[tokio::test]
async fn deleting_the_last_document_writes_the_literal_null() {
    let directory = TempDir::new("delete-last");
    let store = JdocsStore::with_docs(directory.data_dir().jdocs_path(), fixture());

    store
        .delete_data(THING)
        .await
        .expect("the delete did not write");

    assert_eq!(
        directory.file(),
        b"null",
        "vars.go:322-328 leaves the nil slice, which marshals to null and not to []"
    );
    assert!(
        store.snapshot().is_empty(),
        "the list did not empty with the file"
    );

    // And back in through the loader.
    let _guard = watching(&ring());
    let loaded = tokio::time::timeout(CEILING, JdocsStore::load(&directory.data_dir()))
        .await
        .expect("the load hung");
    assert!(
        matches!(loaded.outcome, JdocsLoadOutcome::Loaded),
        "a null file is a clean load, not a fault"
    );
    assert!(
        loaded.store.snapshot().is_empty(),
        "null did not load as an empty list"
    );
}

/// `vars.go:315-318`, the bare write every caller makes after an `AddJdoc`.
#[tokio::test]
async fn write_puts_the_marshalled_list_in_the_file() {
    let directory = TempDir::new("write");
    let store = JdocsStore::with_docs(directory.data_dir().jdocs_path(), fixture());

    store.write().await.expect("the write failed");

    assert_eq!(
        String::from_utf8(directory.file()).expect("the file is not UTF-8"),
        FIXTURE,
        "a write of the loaded fixture is not the fixture"
    );
    assert_eq!(
        directory.entries(),
        ["jdocs.json"],
        "the replacement left a temporary behind"
    );
}

/// Every write goes through [`wirepod_core::persist::write_atomic`], so the
/// file is swapped rather than truncated and refilled. That is the
/// atomic-persistence deviation C23 records, and it is what makes the state
/// file safe against a crash and against two writers at once where Go's
/// `os.WriteFile` (`vars.go:317`) is neither.
///
/// A second hard link to the original file is the witness: a rename unlinks the
/// name and leaves the witness pointing at the old bytes, while a write in
/// place would change what the witness reads.
#[tokio::test]
async fn a_store_write_replaces_the_file_rather_than_rewriting_it_in_place() {
    let directory = TempDir::new("swap");
    let witness = directory.path().join("witness.json");
    directory.seed(b"original");
    fs::hard_link(directory.jdocs_file(), &witness)
        .expect("the temporary filesystem has no hard links");

    let store = JdocsStore::with_docs(directory.data_dir().jdocs_path(), fixture());
    store.write().await.expect("the write failed");

    assert_eq!(
        String::from_utf8(directory.file()).expect("the file is not UTF-8"),
        FIXTURE
    );
    assert_eq!(
        fs::read(&witness).expect("the witness is missing"),
        b"original",
        "the store rewrote the file in place instead of replacing it"
    );
}

// ---------------------------------------------------------------------------
// Go's decoder, through the loop `crate::config` shares
// ---------------------------------------------------------------------------

/// `decode.go:698-733`: a second occurrence of a key is applied on top of what
/// the first left, so a scalar takes the last value and a nested object is the
/// union of the two. Both halves are what a Go run reported for these exact
/// documents.
#[test]
fn a_duplicate_key_is_applied_on_top_of_the_first() {
    let scalar = parse_jdocs(br#"[{"thing":"first","thing":"second","name":"n"}]"#)
        .expect("the document did not parse");
    assert_eq!(scalar[0].thing, "second", "the last duplicate wins");

    let nested =
        parse_jdocs(br#"[{"jdoc":{"doc_version":1},"jdoc":{"fmt_version":2},"thing":"t"}]"#)
            .expect("the document did not parse");
    assert_eq!(
        (nested[0].jdoc.doc_version, nested[0].jdoc.fmt_version),
        (1, 2),
        "a duplicated object is merged, not replaced"
    );
}

/// `decode.go:694-697`: a key matches a tag exactly, or failing that matches
/// the first tag it folds to. Go decoded this document into all four of these
/// fields.
#[test]
fn a_case_variant_key_fills_the_field_it_folds_to() {
    let docs =
        parse_jdocs(br#"[{"THING":"t","Name":"n","JDoc":{"Doc_Version":5,"JSON_DOC":"d"}}]"#)
            .expect("the document did not parse");

    assert_eq!(docs[0].thing, "t");
    assert_eq!(docs[0].name, "n");
    assert_eq!(docs[0].jdoc.doc_version, 5);
    assert_eq!(docs[0].jdoc.json_doc, "d");
    assert!(
        docs[0].extra.is_empty() && docs[0].jdoc.extra.is_empty(),
        "a key that folds to a tag is not an unknown key"
    );
}

/// `decode.go:899-903`: a `null` leaves a non-pointer field exactly as it was,
/// at both levels, and reports nothing.
#[test]
fn a_null_leaves_the_field_alone() {
    let docs = parse_jdocs(
        br#"[{"thing":"t","name":null,"jdoc":null},{"thing":"u","jdoc":{"doc_version":null,"client_metadata":null,"fmt_version":9}}]"#,
    )
    .expect("the document did not parse");

    assert_eq!(
        docs[0].name, "",
        "a null did not leave the field at its zero"
    );
    assert_eq!(docs[0].jdoc, Jdoc::default());
    assert_eq!(docs[1].jdoc.doc_version, 0);
    assert_eq!(docs[1].jdoc.client_metadata, "");
    assert_eq!(
        docs[1].jdoc.fmt_version, 9,
        "a null stopped the rest of the object"
    );
}

/// `decode.go:243-247`: the first type error is recorded, decoding carries on
/// to the end of the document, and the partly filled value is what the caller
/// keeps. Go reported exactly one error for this document and still filled
/// `name`.
#[test]
fn a_type_error_is_recorded_once_and_decoding_carries_on() {
    let error = parse_jdocs(br#"[{"thing":123,"name":"n","jdoc":{"doc_version":"x"}}]"#)
        .expect_err("a type error has to be reported");

    assert_eq!(
        error.fault,
        DecodeFault::Type {
            path: "thing".to_owned(),
            found: "number".to_owned(),
            want: "string",
        },
        "the first fault is not the one Go reports"
    );
    assert_eq!(error.docs.len(), 1, "the entry was dropped");
    assert_eq!(error.docs[0].thing, "", "the bad field was written anyway");
    assert_eq!(
        error.docs[0].name, "n",
        "decoding stopped at the fault instead of carrying on"
    );
}

/// The number branch is the one Go quotes the literal in (`decode.go:1008`),
/// and `strconv.ParseUint` refuses a sign, a fraction and an overflow while
/// accepting the whole of `uint64`.
#[test]
fn the_version_fields_take_what_parse_uint_takes() {
    let parsed = parse_jdocs(br#"[{"jdoc":{"doc_version":18446744073709551615}}]"#)
        .expect("u64::MAX did not parse");
    assert_eq!(parsed[0].jdoc.doc_version, u64::MAX);

    for (document, literal) in [
        (&br#"[{"jdoc":{"doc_version":-1}}]"#[..], "-1"),
        (&br#"[{"jdoc":{"doc_version":1.5}}]"#[..], "1.5"),
        (
            &br#"[{"jdoc":{"doc_version":18446744073709551616}}]"#[..],
            "18446744073709551616",
        ),
    ] {
        let error = parse_jdocs(document).expect_err("the literal has to be refused");
        assert_eq!(
            error.fault,
            DecodeFault::Type {
                path: "jdoc.doc_version".to_owned(),
                found: format!("number {literal}"),
                want: "uint64",
            }
        );
    }

    let wrong_type =
        parse_jdocs(br#"[{"jdoc":{"fmt_version":"x"}}]"#).expect_err("a string has to be refused");
    assert_eq!(
        wrong_type.fault,
        DecodeFault::Type {
            path: "jdoc.fmt_version".to_owned(),
            found: "string".to_owned(),
            want: "uint64",
        },
        "only the number branch quotes the literal"
    );
}

/// `decode.go:650-652` and the literal arms: a root that is not an array is a
/// type error against the slice and leaves it empty, and a `null` root is not
/// an error at all. Go reported the same four errors and the same empty slice.
#[test]
fn a_root_that_is_not_an_array_is_a_type_error_against_the_slice() {
    for (document, found) in [
        (&br#"{"thing":"t"}"#[..], "object"),
        (&b"\"hello\""[..], "string"),
        (&b"12"[..], "number"),
        (&b"true"[..], "bool"),
    ] {
        let error = parse_jdocs(document).expect_err("a non-array root has to be reported");
        assert_eq!(
            error.fault,
            DecodeFault::Type {
                path: String::new(),
                found: found.to_owned(),
                want: "[]botjdoc",
            }
        );
        assert!(
            error.docs.is_empty(),
            "the list was filled from a non-array"
        );
    }
}

/// `decode.go:502-592`: an element whose type does not fit is still an element.
/// Go decoded `[1,{"thing":"t"}]` into two entries, the first of them zero, and
/// recorded one fault whose field path is empty because only the object loop
/// pushes onto the field stack.
#[test]
fn an_element_that_is_not_an_object_is_kept_as_a_zero_entry() {
    let error = parse_jdocs(br#"[1,{"thing":"t"}]"#).expect_err("a bad element has to be reported");

    assert_eq!(
        error.fault,
        DecodeFault::Type {
            path: String::new(),
            found: "number".to_owned(),
            want: "botjdoc",
        }
    );
    assert_eq!(error.docs.len(), 2, "the bad element was dropped");
    assert_eq!(error.docs[0], BotJdoc::default());
    assert_eq!(error.docs[1].thing, "t");
}

/// `decode.go:98-105`: Go checks the whole document before it decodes anything,
/// so malformed bytes leave the list exactly as it was rather than half filled.
#[test]
fn a_malformed_document_leaves_the_list_empty() {
    let error = parse_jdocs(br#"[{"thing":"t"}"#).expect_err("malformed bytes have to be reported");

    assert!(
        matches!(error.fault, DecodeFault::Malformed(_)),
        "a syntax error is not a type error"
    );
    assert!(
        error.docs.is_empty(),
        "a malformed document filled the list"
    );
}

/// Go's decoder drops every key its struct does not name (`decode.go:734-736`),
/// so a Go rewrite erases anything a fork wrote. These structs keep them, at
/// both levels, which is what makes taking turns with the Go server safe.
///
/// The Go-shaped rewrite beside it is the same throwaway program's output for
/// the entry with the unknown keys gone, which is what Go would have written.
#[test]
fn unknown_keys_survive_at_both_levels() {
    let document = r#"[{"thing":"vic:1","name":"vic.RobotSettings","jdoc":{"doc_version":2,"fork_inner":{"a":1}},"fork_outer":["x"]}]"#;

    let docs = parse_jdocs(document.as_bytes()).expect("the document did not parse");

    assert_eq!(
        docs[0].extra.get("fork_outer"),
        Some(&serde_json::json!(["x"])),
        "the entry's unknown key was dropped"
    );
    assert_eq!(
        docs[0].jdoc.extra.get("fork_inner"),
        Some(&serde_json::json!({"a": 1})),
        "the document's unknown key was dropped"
    );

    let rewritten = written(&docs);
    let again = parse_jdocs(rewritten.as_bytes()).expect("the rewrite did not parse");
    assert_eq!(again, docs, "the rewrite lost something on the way back");

    // What Go writes for the same entry, which is the same document with both
    // unknown keys gone. The extras are appended in sorted order rather than in
    // file order, which is deviation 4.
    let stripped: Vec<BotJdoc> = docs
        .iter()
        .map(|d| entry(&d.thing, &d.name, doc_without_extras(&d.jdoc)))
        .collect();
    assert_eq!(
        written(&stripped),
        r#"[{"thing":"vic:1","name":"vic.RobotSettings","jdoc":{"doc_version":2}}]"#,
        "with the extras gone the bytes are not Go's"
    );
}

/// The known half of a document.
fn doc_without_extras(jdoc: &Jdoc) -> Jdoc {
    Jdoc {
        extra: Extra::new(),
        ..jdoc.clone()
    }
}

// ---------------------------------------------------------------------------
// The loader
// ---------------------------------------------------------------------------

/// `vars.go:239`: the stat guards the whole body, so a missing file means Go
/// reads nothing, decodes nothing and says nothing.
#[tokio::test]
async fn a_missing_file_loads_nothing_and_logs_nothing() {
    let directory = TempDir::new("load-missing");
    let ring = ring();

    let loaded = {
        let _guard = watching(&ring);
        tokio::time::timeout(CEILING, JdocsStore::load(&directory.data_dir()))
            .await
            .expect("the load hung")
    };

    assert!(matches!(loaded.outcome, JdocsLoadOutcome::Missing));
    assert!(loaded.store.snapshot().is_empty());
    assert!(
        recorded_lines(&ring).is_empty(),
        "vars.go:239 guards the log line with the stat, so nothing is logged"
    );
    assert_eq!(
        loaded.store.path(),
        directory.data_dir().jdocs_path(),
        "the store is not pointed at vars.JdocsPath"
    );
}

/// `vars.go:242`: the log line is outside both discarded errors, so Go
/// announces a good file, an unreadable one and one that is not JSON in exactly
/// the same words. The outcome is what tells them apart here.
#[tokio::test]
async fn a_file_that_is_there_is_announced_however_it_parses() {
    for (label, bytes) in [
        ("good", FIXTURE.as_bytes()),
        ("empty", &b""[..]),
        ("garbage", &b"not json at all"[..]),
        ("type-error", &br#"[{"thing":7}]"#[..]),
    ] {
        let directory = TempDir::new(&format!("load-{label}"));
        directory.seed(bytes);
        let ring = ring();

        let loaded = {
            let _guard = watching(&ring);
            tokio::time::timeout(CEILING, JdocsStore::load(&directory.data_dir()))
                .await
                .expect("the load hung")
        };

        let took_the_right_arm = match (label, &loaded.outcome) {
            ("good", JdocsLoadOutcome::Loaded) => true,
            // An empty file and a garbage one are both `checkValid` failures,
            // which Go finds before it decodes anything (`decode.go:98-105`).
            ("empty" | "garbage", JdocsLoadOutcome::ParseFailed(DecodeFault::Malformed(_))) => true,
            ("type-error", JdocsLoadOutcome::ParseFailed(DecodeFault::Type { .. })) => true,
            _ => false,
        };
        assert!(
            took_the_right_arm,
            "{label} took the wrong arm: {:?}",
            loaded.outcome
        );
        assert_eq!(
            recorded_lines(&ring),
            [(
                "DEBUG".to_owned(),
                String::new(),
                "Loaded jdocs file".to_owned()
            )],
            "vars.go:242 logs one line for {label} and nothing else"
        );
    }
}

/// The fourth arm, which no file contents can reach. `vars.go:239` stats and
/// `vars.go:240` reads, and both errors are discarded, so a path that stats and
/// will not read carries nil bytes into the unmarshal and is still announced by
/// the line at `vars.go:242`. A directory in the file's place is that path on
/// both platforms: `os.Stat` and `fs::metadata` describe it, and `os.ReadFile`
/// and `fs::read` refuse it.
#[tokio::test]
async fn a_file_that_stats_but_cannot_be_read_is_announced_and_loads_nothing() {
    let directory = TempDir::new("load-unreadable");
    fs::create_dir(directory.jdocs_file()).expect("could not put a directory in the file's place");
    let ring = ring();

    let loaded = {
        let _guard = watching(&ring);
        tokio::time::timeout(CEILING, JdocsStore::load(&directory.data_dir()))
            .await
            .expect("the load hung")
    };

    assert!(
        matches!(&loaded.outcome, JdocsLoadOutcome::Unreadable(_)),
        "a path that stats and will not read took the wrong arm: {:?}",
        loaded.outcome
    );
    assert!(
        loaded.store.snapshot().is_empty(),
        "vars.go:241 unmarshals nil bytes, so nothing is in the list"
    );
    assert_eq!(
        recorded_lines(&ring),
        [(
            "DEBUG".to_owned(),
            String::new(),
            "Loaded jdocs file".to_owned()
        )],
        "vars.go:242 sits outside the discarded read error, so the line is still logged"
    );
}

/// The good file loads the whole list, and nothing was written on the way: Go's
/// loader reads and never writes.
#[tokio::test]
async fn a_good_file_loads_the_whole_list_and_writes_nothing() {
    let directory = TempDir::new("load-good");
    directory.seed(FIXTURE.as_bytes());

    let _guard = watching(&ring());
    let loaded = tokio::time::timeout(CEILING, JdocsStore::load(&directory.data_dir()))
        .await
        .expect("the load hung");

    assert_eq!(loaded.store.snapshot(), fixture());
    assert_eq!(
        String::from_utf8(directory.file()).expect("the file is not UTF-8"),
        FIXTURE,
        "the loader rewrote the file"
    );
    assert_eq!(directory.entries(), ["jdocs.json"]);
}

/// A type error at load leaves the partly decoded list in the store, because
/// Go's caller reads the global whatever the discarded error said.
#[tokio::test]
async fn a_type_error_at_load_still_fills_the_store() {
    let directory = TempDir::new("load-partial");
    directory.seed(br#"[{"thing":7,"name":"vic.RobotSettings","jdoc":{"doc_version":3}}]"#);

    let _guard = watching(&ring());
    let loaded = tokio::time::timeout(CEILING, JdocsStore::load(&directory.data_dir()))
        .await
        .expect("the load hung");

    let docs = loaded.store.snapshot();
    assert_eq!(docs.len(), 1, "vars.go:241 keeps what decoded");
    assert_eq!(docs[0].name, "vic.RobotSettings");
    assert_eq!(docs[0].jdoc.doc_version, 3);
}

// ---------------------------------------------------------------------------
// The file on disk
// ---------------------------------------------------------------------------

/// The mode Go hands `os.WriteFile` at the one jdocs write site
/// (`vars.go:317`). Stated on every platform, because the mode is part of the
/// contract even where the filesystem cannot show it.
#[test]
fn the_write_site_passes_gos_mode_constant() {
    assert_eq!(JDOCS_FILE_MODE, 0o644, "vars.go:317 passes 0644");
}

/// `vars.go:317` passes `0644`, and that is the mode the file it creates gets.
///
/// The umask masks every mode this process asks for and there is no portable
/// way to read it, so a file written at `0o777` says what survives: `mode &
/// probe` is `mode & !umask` for any mode, which makes the expectation exact
/// without touching a process-wide setting other tests share.
///
/// Unix only, because the mode is: Windows has no permission bit set for the
/// assertion to read.
#[cfg(unix)]
#[tokio::test]
async fn the_jdocs_write_site_passes_gos_mode() {
    use std::os::unix::fs::PermissionsExt;

    let directory = TempDir::new("mode");
    let probe = directory.path().join("umask-probe");
    wirepod_core::persist::write_atomic(&probe, b"x".to_vec(), 0o777)
        .await
        .expect("the probe write failed");
    let want = JDOCS_FILE_MODE
        & (fs::metadata(&probe)
            .expect("the probe is missing")
            .permissions()
            .mode()
            & 0o777);

    let store = JdocsStore::with_docs(directory.data_dir().jdocs_path(), fixture());
    store.write().await.expect("the write failed");

    assert_eq!(
        fs::metadata(directory.jdocs_file())
            .expect("jdocs.json is missing")
            .permissions()
            .mode()
            & 0o777,
        want,
        "vars.go:317 did not pass JDOCS_FILE_MODE"
    );
}

/// Sixteen concurrent `AddJdoc` calls, each writing the whole file.
///
/// Each takes the list's lock to change it, then the gate to marshal and
/// write it, so the file on disk is always one writer's bytes in full: it
/// parses, and every entry in it is one of the sixteen. Once all sixteen have
/// returned it is the whole list, because the last of them to take the gate
/// marshalled after the last of them had changed the list. Go has the first
/// property nowhere, because `os.WriteFile` truncates in place and a reader can
/// catch a half-written file, and the second only where a caller writes twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_adds_never_leave_a_torn_file() {
    const WRITERS: u64 = 16;

    let directory = TempDir::new("concurrent");
    let store = Arc::new(JdocsStore::new(directory.data_dir().jdocs_path()));

    let mut tasks = Vec::new();
    for index in 0..WRITERS {
        let store = Arc::clone(&store);
        tasks.push(tokio::spawn(async move {
            store
                .add_jdoc(
                    &format!("vic:{index:08x}"),
                    "vic.RobotSettings",
                    doc(index + 1, "{}"),
                )
                .await
        }));
    }
    for task in tasks {
        tokio::time::timeout(CEILING, task)
            .await
            .expect("an add hung")
            .expect("an add panicked")
            .written
            .expect("an add failed to write");
    }

    let on_disk = parse_jdocs(&directory.file()).expect("the file on disk is torn");
    for held in &on_disk {
        assert_eq!(held.name, "vic.RobotSettings");
        assert!(
            held.thing.starts_with("vic:") && held.jdoc.doc_version <= WRITERS,
            "the file holds an entry no writer wrote"
        );
    }
    assert_eq!(
        directory.entries(),
        ["jdocs.json"],
        "a failed rename left a temporary behind"
    );

    let settled = store.snapshot();
    assert_eq!(
        settled.len(),
        WRITERS as usize,
        "an add was lost from the list itself, which the lock has to prevent"
    );
    assert_eq!(
        on_disk, settled,
        "the file is not the list, so a write landed out of the order it marshalled in"
    );
}

/// Two mutations that overlap leave the file holding the state the later one
/// produced, whatever the two marshals cost.
///
/// Go's four `AddJdoc` call sites genuinely overlap. The jdocs pinger calls it
/// from a ticker goroutine (`sdkapp/jdocspinger.go:126`) while a robot's
/// `WriteDoc` (`jdocs/server.go:40`) or the `DeleteData` its `ReadDocs` reaches
/// (`jdocs/server.go:83`) runs on a gRPC handler. A store that marshals its
/// list and only then awaits the write lets those two land their replacements
/// in the opposite order from their marshals, and a six-megabyte document
/// against a forty-byte one makes that ordering a foregone conclusion rather
/// than a coincidence: the small write always finishes first, so the large one
/// lands last and the file keeps a state the list has moved past.
///
/// Go converges anyway at its two robot-facing sites, because each calls
/// `WriteJdocs` again right after `AddJdoc` (`jdocs/server.go:40-41`,
/// `token/token.go:125-126`) and that second write re-marshals the list at a
/// later instant. [`wirepod_core::persist::WriteGate`] does the same for every
/// site rather than two: the marshal happens under the gate, so whichever write
/// takes the gate last writes the list as it is then.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_overlapping_mutation_does_not_leave_the_file_behind_the_list() {
    /// Big enough that writing and flushing it takes far longer than writing
    /// the forty bytes the delete produces, so the two renames are ordered by
    /// the size of the writes and not by how the runtime happened to schedule
    /// them.
    const BIG: usize = 6 * 1024 * 1024;

    let directory = TempDir::new("overlap");
    let store = Arc::new(JdocsStore::with_docs(
        directory.data_dir().jdocs_path(),
        vec![entry(
            "vic:big",
            "vic.RobotSettings",
            doc(1, &"a".repeat(BIG)),
        )],
    ));
    store.write().await.expect("the seed write failed");

    let adding = Arc::clone(&store);
    let add = tokio::spawn(async move {
        adding
            .add_jdoc("vic:tiny", "vic.RobotSettings", doc(1, "{}"))
            .await
    });

    // The add is let get as far as its mutation before the delete starts, so
    // the two overlap rather than simply queueing. Without the gate that
    // mutation and the six-megabyte marshal are one critical section, so
    // reading the list back here waits for the marshal too and the delete is
    // guaranteed to be the second of the two to marshal and the first to land.
    tokio::time::timeout(CEILING, async {
        while store.snapshot().len() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the add never reached its mutation");

    tokio::time::timeout(CEILING, store.delete_data("vic:big"))
        .await
        .expect("the delete hung")
        .expect("the delete failed to write");
    tokio::time::timeout(CEILING, add)
        .await
        .expect("the add hung")
        .expect("the add panicked")
        .written
        .expect("the add failed to write");

    let held = store.snapshot();
    assert_eq!(
        held.len(),
        1,
        "the list is not the state the two mutations left"
    );
    assert_eq!(held[0].thing, "vic:tiny");
    assert_eq!(
        directory.file().len(),
        marshal_jdocs(&held).len(),
        "the file holds a state the list has moved past"
    );
    assert_eq!(
        directory.file(),
        marshal_jdocs(&held),
        "the file is not the list"
    );
    assert_eq!(
        directory.entries(),
        ["jdocs.json"],
        "a write left a temporary behind"
    );
}

/// The one helper every operation test uses, so that the ceiling and the write
/// check are stated once.
async fn add(store: &JdocsStore, thing: &str, name: &str, jdoc: Jdoc) -> AddOutcome {
    let outcome = tokio::time::timeout(CEILING, store.add_jdoc(thing, name, jdoc))
        .await
        .expect("add_jdoc hung");
    assert!(outcome.written.is_ok(), "add_jdoc failed to write");
    outcome
}

/// The error type carries what decoded as well as the fault, which is what
/// [`JdocsStore::load`] leans on. It is also a `std::error::Error`, so a caller
/// that only wants the message gets one.
#[test]
fn the_decode_error_carries_both_halves() {
    let error: JdocsDecodeError =
        parse_jdocs(br#"[{"thing":1}]"#).expect_err("a type error has to be reported");

    assert_eq!(
        error.to_string(),
        "cannot unmarshal number into thing of type string"
    );
    assert!(std::error::Error::source(&error).is_some());
    assert_eq!(error.docs.len(), 1);
}
