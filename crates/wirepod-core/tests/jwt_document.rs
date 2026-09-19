//! The `vic.AppTokens` document read back the way `encoding/json` reads it.
//!
//! Every expectation below was printed by a throwaway Go program holding
//! `ClientToken` and `ClientTokenManager` copied verbatim from
//! `wire-pod/chipper/pkg/servers/token/hashing.go:36-45`, running
//! `json.Unmarshal` into a zero manager and then `json.Marshal` over the
//! result, and printing the input, the error, the list length and the output
//! with `%q`. The Go literals are the consts in the next section; nothing here
//! is written by hand, because the point of the decoder this file drives is
//! that it does what Go does rather than what a `serde` derive would.
//!
//! The one thing Go's output cannot show is what this port keeps and Go drops.
//! `encoding/json` discards every key its struct does not name, so a Go run
//! proves only that the key is gone; that it survives here is the deliberate
//! forward-compatibility difference `crate::gojson::Extra` exists for, and the
//! round-trip test says so where it asserts it.
//!
//! Where a stored hash would go, these use a run of `A`s or `B`s of the length
//! a real one has. Everything that touches the disk runs in a directory under
//! the system temporary directory, named for the process, so nothing here can
//! reach the repository or the live `%APPDATA%\wire-pod` the Go server is
//! serving from.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use wirepod_core::gojson::Decoded;
use wirepod_core::store::jdocs::{Jdoc, JdocsStore, parse_jdocs};
use wirepod_core::token::jwt::{
    APP_TOKENS_DOC, ClientToken, ClientTokenManager, marshal_client_tokens, write_token_hash,
};
use wirepod_core::wallclock::{FixedWallClock, WallTime};

// ---------------------------------------------------------------------------
// What the Go program printed
// ---------------------------------------------------------------------------

/// A stand-in for the hash the token server stores: the length a real one has
/// and none of its bytes.
const PLACEHOLDER_HASH: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// The same for a hash a document already carried before a write.
const SEED_HASH: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

/// The instant the clock below is stopped at, in UTC.
const ISSUED_UNIX_SECS: i64 = 1_757_446_496;
/// Its nanoseconds, chosen so that `time.RFC3339Nano` prints all nine digits
/// rather than trimming trailing zeros.
const ISSUED_NANOS: u32 = 789_012_345;
/// What Go's `time.Unix(1757446496, 789012345).UTC().Format(TimeFormat)`
/// printed for it (`token.go:29`, `:110`).
const ISSUED_AT: &str = "2025-09-09T19:34:56.789012345Z";

/// A document whose three folded keys Go still matches: `Client_Tokens`,
/// `HASH` and `issued_AT` (`decode.go:694-697`, `fold.go:20-37`).
const GO_FOLDED_IN: &str = concat!(
    r#"{"Client_Tokens":[{"HASH":"#,
    r#""AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","#,
    r#""client_name":"wirepod","app_id":"SDK","#,
    r#""issued_AT":"2025-09-09T19:34:56.789012345Z"}]}"#,
);

/// What Go marshalled the folded document back out as, which is the canonical
/// spelling of all three keys.
const GO_FOLDED_OUT: &str = concat!(
    r#"{"client_tokens":[{"hash":"#,
    r#""AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","#,
    r#""client_name":"wirepod","app_id":"SDK","#,
    r#""issued_at":"2025-09-09T19:34:56.789012345Z"}]}"#,
);

/// Two client tokens, then a second `client_tokens` key holding one whose only
/// field is `hash`.
const GO_DUPLICATE_SHORTER_IN: &str = concat!(
    r#"{"client_tokens":[{"hash":"a","client_name":"cn","app_id":"ai","issued_at":"ia"},"#,
    r#"{"hash":"b","client_name":"cn2","app_id":"ai2","issued_at":"ia2"}],"#,
    r#""client_tokens":[{"hash":"z"}]}"#,
);

/// What Go left: one element, whose `hash` is the second occurrence's and
/// whose other three fields are the first element's.
const GO_DUPLICATE_SHORTER_OUT: &str =
    r#"{"client_tokens":[{"hash":"z","client_name":"cn","app_id":"ai","issued_at":"ia"}]}"#;

/// One client token, then a second `client_tokens` key holding two.
const GO_DUPLICATE_LONGER_IN: &str = concat!(
    r#"{"client_tokens":[{"hash":"a","client_name":"cn"}],"#,
    r#""client_tokens":[{"app_id":"ai"},{"hash":"new"}]}"#,
);

/// What Go left: element zero merged, element one fresh.
const GO_DUPLICATE_LONGER_OUT: &str = concat!(
    r#"{"client_tokens":[{"hash":"a","client_name":"cn","app_id":"ai","issued_at":""},"#,
    r#"{"hash":"new","client_name":"","app_id":"","issued_at":""}]}"#,
);

/// Three occurrences of `client_tokens`: two elements, then one, then two.
/// The second occurrence truncates and the third grows back past that length,
/// which is the only shape that shows what `SetLen` keeps.
const GO_THREE_OCCURRENCES_IN: &str = concat!(
    r#"{"client_tokens":[{"hash":"#,
    r#""AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","client_name":"first0"},"#,
    r#"{"hash":"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB","#,
    r#""client_name":"first1"}],"#,
    r#""client_tokens":[{"app_id":"mid"}],"#,
    r#""client_tokens":[{"issued_at":"third0"},{"issued_at":"third1"}]}"#,
);

/// What Go left: element one is the element the second occurrence truncated
/// away, still carrying the first occurrence's `hash` and `client_name`,
/// because `SetLen` shortens the slice and leaves the backing array alone
/// (`decode.go:585`).
const GO_THREE_OCCURRENCES_OUT: &str = concat!(
    r#"{"client_tokens":[{"hash":"#,
    r#""AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","#,
    r#""client_name":"first0","app_id":"mid","issued_at":"third0"},"#,
    r#"{"hash":"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB","#,
    r#""client_name":"first1","app_id":"","issued_at":"third1"}]}"#,
);

/// What this port leaves instead, which is the recorded difference: a [`Vec`]
/// has no backing array to shorten into, so the truncation drops the element
/// and the third occurrence pushes a fresh one.
const PORT_THREE_OCCURRENCES_OUT: &str = concat!(
    r#"{"client_tokens":[{"hash":"#,
    r#""AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","#,
    r#""client_name":"first0","app_id":"mid","issued_at":"third0"},"#,
    r#"{"hash":"","client_name":"","app_id":"","issued_at":"third1"}]}"#,
);

/// A populated `client_tokens`, then a second one holding `null`.
const GO_ARRAY_THEN_NULL_IN: &str = concat!(
    r#"{"client_tokens":[{"hash":"a","client_name":"cn","app_id":"ai","issued_at":"ia"}],"#,
    r#""client_tokens":null}"#,
);

/// What Go left: the nil slice, which marshals to `null`.
const GO_NULL_OUT: &str = r#"{"client_tokens":null}"#;

/// An empty `client_tokens` array, which is the one input that leaves Go with
/// a slice that is empty and **not** nil: `decode.go:588-590` replaces the
/// field with `reflect.MakeSlice(t, 0, 0)` when the array it just read had no
/// elements.
const GO_EMPTY_ARRAY_IN: &str = r#"{"client_tokens":[]}"#;

/// What Go marshalled that back out as, which is the same bytes it read: a
/// non-nil empty slice is `[]` where a nil one is `null`.
const GO_EMPTY_ARRAY_OUT: &str = r#"{"client_tokens":[]}"#;

/// A populated `client_tokens`, then a second one holding a string.
const GO_ARRAY_THEN_STRING_IN: &str = concat!(
    r#"{"client_tokens":[{"hash":"a","client_name":"cn","app_id":"ai","issued_at":"ia"}],"#,
    r#""client_tokens":"nope"}"#,
);

/// What Go left: the list untouched.
const GO_ARRAY_THEN_STRING_OUT: &str =
    r#"{"client_tokens":[{"hash":"a","client_name":"cn","app_id":"ai","issued_at":"ia"}]}"#;

/// A `client_tokens` holding a string, then one holding an array, then a key
/// the struct does not name.
const GO_STRING_THEN_ARRAY_IN: &str = concat!(
    r#"{"client_tokens":"nope","client_tokens":[{"hash":"h","client_name":"cn","#,
    r#""app_id":"ai","issued_at":"t"}],"unknown":1}"#,
);

/// What Go left: the second occurrence's array, and `unknown` dropped.
const GO_STRING_THEN_ARRAY_OUT: &str =
    r#"{"client_tokens":[{"hash":"h","client_name":"cn","app_id":"ai","issued_at":"t"}]}"#;

/// Go's message for a `client_tokens` that is not an array, rendered the way
/// [`wirepod_core::DecodeFault`] renders it. Go's own text is
/// `json: cannot unmarshal string into Go struct field
/// ClientTokenManager.client_tokens of type []main.ClientToken`, whose package
/// qualifier this port drops as `crate::store::jdocs` drops it from
/// `[]vars.botjdoc`.
const GO_NOT_AN_ARRAY_FAULT: &str =
    "cannot unmarshal string into client_tokens of type []ClientToken";

/// The same for every other kind a `client_tokens` value can be, because the
/// fault names the slice the key matched and not the value it found. Go's own
/// text for the first of these is `json: cannot unmarshal bool into Go struct
/// field ClientTokenManager.client_tokens of type []main.ClientToken`, and the
/// four differ only in the word after `unmarshal`. Go reported length 0 and
/// marshalled [`GO_NULL_OUT`] for all four.
const GO_NOT_AN_ARRAY_CASES: [(&str, &str); 4] = [
    (
        r#"{"client_tokens":true}"#,
        "cannot unmarshal bool into client_tokens of type []ClientToken",
    ),
    (
        r#"{"client_tokens":1}"#,
        "cannot unmarshal number into client_tokens of type []ClientToken",
    ),
    (
        r#"{"client_tokens":{"a":1}}"#,
        "cannot unmarshal object into client_tokens of type []ClientToken",
    ),
    (r#"{"client_tokens":"nope"}"#, GO_NOT_AN_ARRAY_FAULT),
];

/// An element that is a number, followed by a good one.
const GO_BAD_ELEMENT_IN: &str =
    r#"{"client_tokens":[1,{"hash":"h","client_name":"cn","app_id":"ai","issued_at":"t"}]}"#;

/// What Go left: two elements, the first at its zero value.
const GO_BAD_ELEMENT_OUT: &str = concat!(
    r#"{"client_tokens":[{"hash":"","client_name":"","app_id":"","issued_at":""},"#,
    r#"{"hash":"h","client_name":"cn","app_id":"ai","issued_at":"t"}]}"#,
);

/// Go's message for that element. Go's own text names the element type rather
/// than the slice type, and puts the list's own key in the field path because
/// `array` pushes nothing onto the field stack: `json: cannot unmarshal number
/// into Go struct field ClientTokenManager.client_tokens of type
/// main.ClientToken`.
const GO_BAD_ELEMENT_FAULT: &str = "cannot unmarshal number into client_tokens of type ClientToken";

/// A `hash` that is a number, inside an otherwise good element.
const GO_BAD_SCALAR_IN: &str =
    r#"{"client_tokens":[{"hash":1,"client_name":"cn","app_id":"ai","issued_at":"t"}]}"#;

/// What Go left: the element's other three fields decoded and `hash` at its
/// zero value.
const GO_BAD_SCALAR_OUT: &str =
    r#"{"client_tokens":[{"hash":"","client_name":"cn","app_id":"ai","issued_at":"t"}]}"#;

/// Go's message for it, which carries the two-segment path `json: cannot
/// unmarshal number into Go struct field ClientToken.client_tokens.hash of
/// type string`.
const GO_BAD_SCALAR_FAULT: &str = "cannot unmarshal number into client_tokens.hash of type string";

/// One client token carrying a key the struct does not name, and one such key
/// beside `client_tokens`.
const GO_UNKNOWN_IN: &str = concat!(
    r#"{"client_tokens":[{"hash":"h","client_name":"cn","app_id":"ai","#,
    r#""issued_at":"t","mystery":{"deep":[1,2]}}],"stray":"s"}"#,
);

/// What Go left: both unknown keys gone.
const GO_UNKNOWN_OUT: &str =
    r#"{"client_tokens":[{"hash":"h","client_name":"cn","app_id":"ai","issued_at":"t"}]}"#;

/// One bare client token carrying a folded key, a duplicated one and an
/// unknown one, for the struct's own [`serde::Deserialize`] entry.
const GO_SINGLE_TOKEN_IN: &str = concat!(
    r#"{"HASH":"h","Client_Name":"cn","hash":"h2","APP_ID":"ai","#,
    r#""issued_AT":"t","mystery":1}"#,
);

/// What Go left: the folds matched, the second `hash` won, and `mystery` is
/// gone.
const GO_SINGLE_TOKEN_OUT: &str =
    r#"{"hash":"h2","client_name":"cn","app_id":"ai","issued_at":"t"}"#;

/// The document `WriteTokenHash` writes on a first authentication, as
/// `json.Marshal` wrote it.
const GO_APP_TOKENS_DOC: &str = concat!(
    r#"{"client_tokens":[{"hash":"#,
    r#""AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","#,
    r#""client_name":"wirepod","app_id":"SDK","#,
    r#""issued_at":"2025-09-09T19:34:56.789012345Z"}]}"#,
);

/// A `json_doc` that already holds one client token, for the arm that is dead
/// in the running server and reachable by seeding the store under the bare
/// serial.
const GO_SEEDED_DOC: &str = concat!(
    r#"{"client_tokens":[{"hash":"#,
    r#""BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB","#,
    r#""client_name":"wirepod","app_id":"SDK","#,
    r#""issued_at":"2020-01-02T03:04:05.06Z"}]}"#,
);

/// What Go's `token.go:108-115` left over that `json_doc`: the seeded token,
/// then the appended one.
const GO_APPENDED_DOC: &str = concat!(
    r#"{"client_tokens":[{"hash":"#,
    r#""BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB","#,
    r#""client_name":"wirepod","app_id":"SDK","#,
    r#""issued_at":"2020-01-02T03:04:05.06Z"},{"hash":"#,
    r#""AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","#,
    r#""client_name":"wirepod","app_id":"SDK","#,
    r#""issued_at":"2025-09-09T19:34:56.789012345Z"}]}"#,
);

/// [`GO_SEEDED_DOC`] with one key added: a lone-surrogate escape, the six
/// characters backslash, `u`, `d`, `8`, `0`, `0`.
///
/// Raw strings throughout, so that Rust leaves the escape alone and the six
/// characters reach the decoder the way they would from a hand-edited file.
const GO_SURROGATE_KEY_IN: &str = concat!(
    r#"{"client_tokens":[{"hash":"#,
    r#""BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB","#,
    r#""client_name":"wirepod","app_id":"SDK","#,
    r#""issued_at":"2020-01-02T03:04:05.06Z"}],"#,
    r#""\ud800":1}"#,
);

/// What Go left for it: no error, one client token, and the whole seeded
/// document back. `checkValid` accepts the escape, `unquoteBytes` decodes it to
/// U+FFFD (`decode.go:1277-1288`), and a key spelled U+FFFD matches no tag, so
/// Go drops it the way it drops any unknown key.
const GO_SURROGATE_KEY_OUT: &str = GO_SEEDED_DOC;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A ceiling on anything awaited, generous enough that only a hang reaches it.
/// Real durations, because this crate's tests never pause the runtime's clock.
const CEILING: Duration = Duration::from_secs(30);

/// The serial the tests write under, which is not a robot this machine has
/// ever seen.
const TEST_ESN: &str = "00000000";

/// One `json.Unmarshal` into a zero manager: what decoded, and the first
/// fault's message.
fn decode(document: &str) -> (ClientTokenManager, Option<String>) {
    let decoded: Decoded<ClientTokenManager> =
        serde_json::from_str(document).expect("the document is one JSON value");
    (decoded.value, decoded.fault.map(|fault| fault.to_string()))
}

/// The same, asserting that Go reported no error for this document.
fn decode_clean(document: &str) -> ClientTokenManager {
    let (manager, fault) = decode(document);
    assert_eq!(fault, None, "Go reported no error for {document}");
    manager
}

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
            "wirepod-jwtdoc-{label}-{}-{nanos:x}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("could not create the temporary directory");
        Self { path }
    }

    /// Where the store writes.
    fn jdocs_file(&self) -> PathBuf {
        self.path.join("jdocs.json")
    }

    /// The bytes currently in the file.
    fn file(&self) -> Vec<u8> {
        fs::read(self.jdocs_file()).expect("jdocs.json is missing")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A store in its own temporary directory, and a clock stopped at
/// [`ISSUED_AT`] in UTC.
fn store_and_clock(label: &str) -> (TempDir, JdocsStore, FixedWallClock) {
    let directory = TempDir::new(label);
    let store = JdocsStore::new(
        directory
            .jdocs_file()
            .to_str()
            .expect("the temporary path is UTF-8")
            .to_owned(),
    );
    let clock = FixedWallClock::new(WallTime::new(ISSUED_UNIX_SECS, ISSUED_NANOS), 0);
    (directory, store, clock)
}

// ---------------------------------------------------------------------------
// The four decoder rules, over this document
// ---------------------------------------------------------------------------

/// Rule 1 (`decode.go:694-697`): a key matches a tag exactly or by fold, and
/// the fold is an ASCII upper-casing, so `Client_Tokens`, `HASH` and
/// `issued_AT` all reach the fields Go folds them to.
///
/// A `serde` derive matches on the exact tag and nothing else, so all three of
/// these would have landed in the unknown map instead and the document would
/// have come back out with the keys doubled.
#[test]
fn a_folded_key_reaches_the_field_go_folds_it_to() {
    let manager = decode_clean(GO_FOLDED_IN);

    assert_eq!(manager.client_tokens.len(), 1, "Go decoded one element");
    let token = &manager.client_tokens[0];
    assert_eq!(token.hash, PLACEHOLDER_HASH, "HASH did not reach hash");
    assert_eq!(token.client_name, "wirepod");
    assert_eq!(token.app_id, "SDK");
    assert_eq!(
        token.issued_at, ISSUED_AT,
        "issued_AT did not reach issued_at"
    );
    assert!(
        token.extra.is_empty() && manager.extra.is_empty(),
        "a folded key was kept as an unknown one instead of being matched"
    );

    assert_eq!(
        marshal_client_tokens(&manager),
        GO_FOLDED_OUT,
        "the folded document did not come back out the way Go marshalled it"
    );
}

/// The plain [`serde_json::from_str`] entry is the same decode path.
///
/// Two tests outside this file read a document back that way
/// (`tests/jwt.rs`'s `a_second_write_replaces_the_document_rather_than_appending_to_it`
/// and `tests/token_hash.rs`'s live-document walk), so both structs carry a
/// hand-written [`serde::Deserialize`] that delegates to the same
/// [`Decoded`]. Without it a derive would decide those reads, and a derive
/// matches the exact tag and nothing else: neither the fold nor the duplicate
/// rule would hold there.
#[test]
fn the_plain_deserialize_entry_decodes_the_way_the_document_entry_does() {
    let through_decoded = decode_clean(GO_FOLDED_IN);
    let through_from_str: ClientTokenManager =
        serde_json::from_str(GO_FOLDED_IN).expect("the document is one JSON value");
    assert_eq!(
        through_from_str, through_decoded,
        "from_str and Decoded are two different decoders"
    );

    let duplicated: ClientTokenManager =
        serde_json::from_str(GO_DUPLICATE_SHORTER_IN).expect("the document is one JSON value");
    assert_eq!(
        marshal_client_tokens(&duplicated),
        GO_DUPLICATE_SHORTER_OUT,
        "from_str did not apply the duplicate-key rule"
    );

    // And one bare client token, which is the other struct's own entry. Go's
    // answer for this document is [`GO_SINGLE_TOKEN_OUT`]: `HASH` and
    // `Client_Name` fold, the second `hash` wins over the first, and `mystery`
    // is dropped there and kept here.
    let single: ClientToken =
        serde_json::from_str(GO_SINGLE_TOKEN_IN).expect("the document is one JSON value");
    assert_eq!(single.hash, "h2", "the last occurrence of hash did not win");
    assert_eq!(single.client_name, "cn");
    assert_eq!(single.app_id, "ai");
    assert_eq!(single.issued_at, "t");
    assert_eq!(single.extra.get("mystery"), Some(&Value::from(1)));
    assert_eq!(
        marshal_client_tokens(&ClientTokenManager {
            client_tokens: vec![ClientToken {
                extra: Default::default(),
                ..single
            }],
            extra: Default::default(),
        }),
        format!(r#"{{"client_tokens":[{GO_SINGLE_TOKEN_OUT}]}}"#),
        "the four named fields are not what Go left"
    );
}

/// Rule 2 (`decode.go:762`, and `:544-587` for the slice): a duplicate
/// `client_tokens` is decoded into the elements the first occurrence left,
/// element by element, and the list is then truncated to the second
/// occurrence's length.
///
/// So a shorter second occurrence keeps the first element's other fields and
/// drops the tail, and a longer one grows with fresh zero values past the
/// length it found.
#[test]
fn a_duplicate_client_tokens_key_merges_element_wise_and_takes_the_second_length() {
    let shorter = decode_clean(GO_DUPLICATE_SHORTER_IN);
    assert_eq!(
        shorter.client_tokens.len(),
        1,
        "the list was not truncated to the second occurrence's length"
    );
    let merged = &shorter.client_tokens[0];
    assert_eq!(
        merged.hash, "z",
        "the second occurrence's scalar did not win"
    );
    assert_eq!(
        merged.client_name, "cn",
        "a field the second occurrence omits did not keep the first's value"
    );
    assert_eq!(merged.app_id, "ai");
    assert_eq!(merged.issued_at, "ia");
    assert_eq!(marshal_client_tokens(&shorter), GO_DUPLICATE_SHORTER_OUT);

    let longer = decode_clean(GO_DUPLICATE_LONGER_IN);
    assert_eq!(longer.client_tokens.len(), 2, "the list did not grow");
    assert_eq!(
        longer.client_tokens[0].hash, "a",
        "element zero was replaced rather than merged into"
    );
    assert_eq!(longer.client_tokens[0].client_name, "cn");
    assert_eq!(
        longer.client_tokens[0].app_id, "ai",
        "the second occurrence's field did not reach element zero"
    );
    assert_eq!(
        longer.client_tokens[1].hash, "new",
        "the element past the first occurrence's length is not the second's"
    );
    assert_eq!(
        longer.client_tokens[1].client_name, "",
        "the element past the length was not fresh"
    );
    assert_eq!(marshal_client_tokens(&longer), GO_DUPLICATE_LONGER_OUT);
}

/// The one place the rule above and Go part, pinned here so that it is a
/// recorded difference rather than an untested assumption.
///
/// Go's truncation is `v.SetLen(i)` (`decode.go:585`), which shortens the
/// slice and leaves the backing array alone, so an element a shorter second
/// occurrence dropped is still there for a longer third occurrence to grow
/// back onto and decode into. Go's answer for
/// [`GO_THREE_OCCURRENCES_IN`] is therefore
/// [`GO_THREE_OCCURRENCES_OUT`], whose second element still carries the first
/// occurrence's `hash` and `client_name`.
///
/// A [`Vec`] has no such shadow: `truncate` drops the element, and the third
/// occurrence pushes a fresh [`Default`], so the port answers
/// [`PORT_THREE_OCCURRENCES_OUT`] with the second element's first three fields
/// empty. Reproducing Go would mean keeping a shadow list beside every list
/// field for the sake of a document with three occurrences of one key in it,
/// which nothing but a hand edit can produce, so the difference stands and
/// this test is what says so.
///
/// Go resets the backing array whenever an occurrence is empty
/// (`decode.go:588-590`), so the same shape with `[]` in the middle agrees in
/// both languages; that is the second half of this test.
#[test]
fn a_third_occurrence_does_not_see_the_element_a_shorter_second_one_dropped() {
    let manager = decode_clean(GO_THREE_OCCURRENCES_IN);

    assert_eq!(
        manager.client_tokens.len(),
        2,
        "the third occurrence did not grow the list back"
    );
    assert_eq!(
        manager.client_tokens[0].app_id, "mid",
        "the second occurrence's field did not reach element zero"
    );
    assert_eq!(
        manager.client_tokens[0].issued_at, "third0",
        "the third occurrence's field did not reach element zero"
    );
    assert_eq!(
        manager.client_tokens[1].hash, "",
        "the element the truncation dropped came back, so the Vec has a shadow \
         and this test no longer describes the port"
    );
    assert_eq!(manager.client_tokens[1].issued_at, "third1");
    assert_eq!(
        marshal_client_tokens(&manager),
        PORT_THREE_OCCURRENCES_OUT,
        "the port's answer moved"
    );

    // And Go's, for the contrast this test exists for.
    assert_ne!(
        GO_THREE_OCCURRENCES_OUT, PORT_THREE_OCCURRENCES_OUT,
        "the two literals agree, so this test proves nothing about the difference"
    );
    assert!(
        GO_THREE_OCCURRENCES_OUT.contains(r#""client_name":"first1""#),
        "Go's literal no longer carries the field the truncation kept"
    );
}

/// Rule 3, the slice half (`decode.go:899-903`): a `null` empties the list
/// rather than leaving it alone, because a slice is one of the four kinds
/// `literalStore`'s null arm calls `SetZero` on. Everything else falls through
/// to Go's own "otherwise, ignore null for primitives/string".
///
/// The two cases are the only ones there are: a `null` on its own, which is
/// indistinguishable from leaving an empty list alone, and a `null` after a
/// populated occurrence of the same key, which is where the two behaviours
/// part. The Go run that printed [`GO_NULL_OUT`] for the second is why this
/// test is named the way it is.
#[test]
fn a_null_client_tokens_empties_the_list_the_way_go_zeroes_a_slice() {
    let after_array = decode_clean(GO_ARRAY_THEN_NULL_IN);
    assert!(
        after_array.client_tokens.is_empty(),
        "the null left the first occurrence's element in place, where Go zeroes the slice"
    );
    assert_eq!(marshal_client_tokens(&after_array), GO_NULL_OUT);

    let alone = decode_clean(GO_NULL_OUT);
    assert!(alone.client_tokens.is_empty());
    assert_eq!(marshal_client_tokens(&alone), GO_NULL_OUT);
}

/// The other half of that rule, and the one input where the port and Go write
/// different bytes for the same empty list.
///
/// Go distinguishes a nil slice from an empty one, and `{"client_tokens":[]}`
/// gives it the second: `decode.go:588-590` replaces the field with
/// `reflect.MakeSlice(t, 0, 0)` when the array it read had no elements, so the
/// re-marshal is [`GO_EMPTY_ARRAY_OUT`], the same bytes that went in. A
/// [`Vec`] has one empty state, and
/// [`wirepod_core::token::jwt::serialize_client_tokens`] writes it as `null`
/// because that is what the only empty manager the Go server can reach
/// marshals to: `WriteTokenHash` declares `var tokenJson ClientTokenManager`
/// and only ever appends to it, so a nil slice is the one it has.
///
/// A `[]` therefore reaches disk as `null`. It can only get into a `json_doc`
/// by hand, both spellings decode to the same empty list on the way back in,
/// and the Go server reads `null` as the nil slice it wrote, so nothing
/// observes the change but a byte comparison of the file. Recorded as such in
/// `docs/phases/P4-sdk-app/deviations.md`, and pinned here.
#[test]
fn an_empty_array_re_marshals_as_null_where_go_writes_an_empty_array() {
    let manager = decode_clean(GO_EMPTY_ARRAY_IN);

    assert!(
        manager.client_tokens.is_empty(),
        "the empty array did not decode to an empty list"
    );
    assert_eq!(
        marshal_client_tokens(&manager),
        GO_NULL_OUT,
        "the port no longer writes an empty list as null"
    );

    // Go's answer, which is the bytes it read.
    assert_ne!(
        GO_EMPTY_ARRAY_OUT, GO_NULL_OUT,
        "the two literals agree, so this test proves nothing about the difference"
    );
    assert_eq!(
        GO_EMPTY_ARRAY_OUT, GO_EMPTY_ARRAY_IN,
        "Go's round trip of an empty array is not the bytes it read"
    );

    // And the read-back agrees whichever spelling is on disk, which is why the
    // difference stops at the bytes.
    assert_eq!(
        decode_clean(GO_EMPTY_ARRAY_OUT),
        decode_clean(GO_NULL_OUT),
        "the two spellings do not decode to the same manager"
    );
}

/// Rule 4 (`decode.go:243-247`): a `client_tokens` whose value is not an array
/// records the first fault, leaves the field exactly as it was, and decoding
/// carries on through the rest of the document.
///
/// Go's own error names the slice type, `[]main.ClientToken` in the probe and
/// `[]token.ClientToken` in the server; this port drops the package qualifier,
/// as `crate::store::jdocs` does for `[]vars.botjdoc`.
#[test]
fn a_client_tokens_that_is_not_an_array_records_one_fault_and_decodes_the_rest() {
    // The list keeps what an earlier occurrence left.
    let (after_array, fault) = decode(GO_ARRAY_THEN_STRING_IN);
    assert_eq!(fault.as_deref(), Some(GO_NOT_AN_ARRAY_FAULT));
    assert_eq!(
        after_array.client_tokens.len(),
        1,
        "the string cleared the list, where Go leaves it untouched"
    );
    assert_eq!(
        marshal_client_tokens(&after_array),
        GO_ARRAY_THEN_STRING_OUT
    );

    // And a later occurrence still decodes, which is what "carries on" means.
    let (then_array, fault) = decode(GO_STRING_THEN_ARRAY_IN);
    assert_eq!(
        fault.as_deref(),
        Some(GO_NOT_AN_ARRAY_FAULT),
        "the fault is the first one, not the last"
    );
    assert_eq!(
        then_array.client_tokens.len(),
        1,
        "decoding stopped at the fault instead of carrying on"
    );
    assert_eq!(then_array.client_tokens[0].hash, "h");

    // Go's output drops `unknown`; this port keeps it, which is the
    // forward-compatibility difference and not an accident.
    assert_eq!(
        then_array.extra.get("unknown"),
        Some(&Value::from(1)),
        "the unknown key was dropped, so a Go rewrite and a Rust one differ"
    );
    assert_eq!(
        GO_STRING_THEN_ARRAY_OUT,
        r#"{"client_tokens":[{"hash":"h","client_name":"cn","app_id":"ai","issued_at":"t"}]}"#,
        "this is what Go marshalled, without the unknown key"
    );
}

/// The same fault for every other kind a `client_tokens` value can be, because
/// the type a fault names is the field's and not the value's.
///
/// The test above drives one of the four, and one spelling of a message is not
/// evidence about the other three: a fault built from the value's own kind, or
/// from [`ClientToken`] rather than the slice, would pass it and fail here.
/// Go's own text names `[]main.ClientToken` in the probe and
/// `[]token.ClientToken` in the server, and this port drops the package
/// qualifier as `crate::store::jdocs` does for `[]vars.botjdoc`.
///
/// Go reported length 0 and marshalled [`GO_NULL_OUT`] for all four, because
/// the field keeps the nil slice it started with.
#[test]
fn every_non_array_client_tokens_value_records_the_slice_type() {
    for (document, expected) in GO_NOT_AN_ARRAY_CASES {
        let (manager, fault) = decode(document);

        assert_eq!(
            fault.as_deref(),
            Some(expected),
            "the fault for {document} is not the one Go printed"
        );
        assert!(
            manager.client_tokens.is_empty(),
            "{document} left something in the list"
        );
        assert_eq!(
            marshal_client_tokens(&manager),
            GO_NULL_OUT,
            "{document} did not leave the field at the value Go leaves it at"
        );
    }
}

/// `decode.go:544-558`: Go grows the slice before it decodes into the element
/// it has just made, so an element whose type does not fit still occupies its
/// slot at the zero value and every element after it keeps its index.
///
/// The fault names the element type and the list's own key, because `array`
/// pushes nothing onto the field stack.
#[test]
fn an_element_that_is_not_an_object_still_takes_its_slot() {
    let (manager, fault) = decode(GO_BAD_ELEMENT_IN);

    assert_eq!(fault.as_deref(), Some(GO_BAD_ELEMENT_FAULT));
    assert_eq!(
        manager.client_tokens.len(),
        2,
        "the bad element was skipped rather than taking its slot"
    );
    assert_eq!(
        manager.client_tokens[0],
        Default::default(),
        "the bad element's slot is not the zero value"
    );
    assert_eq!(
        manager.client_tokens[1].hash, "h",
        "the good element moved down into the bad one's slot"
    );
    assert_eq!(marshal_client_tokens(&manager), GO_BAD_ELEMENT_OUT);
}

/// A fault inside a good element carries the two-segment path
/// `client_tokens.hash`, because the list's key is still on Go's field stack
/// when the element's own object loop pushes `hash` onto it
/// (`decode.go:725-733`, `:249-262`), and the rest of that element still
/// decodes.
#[test]
fn a_bad_field_inside_a_client_token_is_reported_under_the_lists_path() {
    let (manager, fault) = decode(GO_BAD_SCALAR_IN);

    assert_eq!(fault.as_deref(), Some(GO_BAD_SCALAR_FAULT));
    assert_eq!(manager.client_tokens.len(), 1);
    assert_eq!(
        manager.client_tokens[0].hash, "",
        "the number reached the field"
    );
    assert_eq!(
        manager.client_tokens[0].client_name, "cn",
        "the fields after the fault did not decode"
    );
    assert_eq!(marshal_client_tokens(&manager), GO_BAD_SCALAR_OUT);
}

/// A key neither struct names survives a read and a rewrite, wherever it sits.
///
/// This is the one place the port deliberately differs from Go: Go drops every
/// unknown key, so [`GO_UNKNOWN_OUT`] is shorter than the document that went
/// in, and a Go rewrite of a file a fork or a later version wrote erases the
/// additions. Keeping them is what lets the two servers take turns over the
/// same `%APPDATA%\wire-pod` state.
#[test]
fn an_unknown_key_inside_a_client_token_survives_a_round_trip() {
    let manager = decode_clean(GO_UNKNOWN_IN);

    assert_eq!(manager.client_tokens.len(), 1);
    assert_eq!(
        manager.client_tokens[0].extra.get("mystery"),
        Some(&serde_json::json!({"deep": [1, 2]})),
        "the key inside the client token was dropped"
    );
    assert_eq!(
        manager.extra.get("stray"),
        Some(&Value::from("s")),
        "the key beside client_tokens was dropped"
    );
    assert_eq!(
        marshal_client_tokens(&manager),
        GO_UNKNOWN_IN,
        "the round trip did not reproduce the document it read"
    );

    // Go's own answer, for contrast: both keys gone.
    assert_ne!(
        GO_UNKNOWN_OUT, GO_UNKNOWN_IN,
        "the Go literals agree, so this test proves nothing about the difference"
    );
}

/// The document the token server writes decodes and re-marshals to the same
/// bytes, which is what makes a read-modify-write of `vic.AppTokens` safe.
#[test]
fn the_decoded_document_re_marshals_to_the_bytes_go_wrote() {
    let manager = decode_clean(GO_APP_TOKENS_DOC);

    assert_eq!(manager.client_tokens.len(), 1);
    assert_eq!(manager.client_tokens[0].hash, PLACEHOLDER_HASH);
    assert_eq!(
        marshal_client_tokens(&manager),
        GO_APP_TOKENS_DOC,
        "the document did not survive a decode and a re-marshal"
    );

    // And the seeded document the write test starts from, which carries a
    // fraction Go trims to two digits.
    let seeded = decode_clean(GO_SEEDED_DOC);
    assert_eq!(seeded.client_tokens[0].hash, SEED_HASH);
    assert_eq!(marshal_client_tokens(&seeded), GO_SEEDED_DOC);
}

// ---------------------------------------------------------------------------
// The write itself
// ---------------------------------------------------------------------------

/// `token.go:108-115` over a `json_doc` that already holds a client token: the
/// decoded list is what the new token is appended to, so the document comes
/// out two long.
///
/// This arm is dead in the running server, because `token.go:101` looks the
/// document up under the bare serial and `token.go:125` stores it under `vic:`
/// plus the serial, so nothing the server writes is ever found. Seeding the
/// store under the bare serial is the only way to reach it, and reaching it is
/// the only way to see that the decode is Go's rather than a fresh manager.
#[tokio::test]
async fn write_token_hash_appends_to_a_seeded_document_the_way_go_would() {
    let (directory, store, clock) = store_and_clock("seeded");

    // The lookup's own spelling, which is the bare serial.
    store
        .add_jdoc(
            TEST_ESN,
            APP_TOKENS_DOC,
            Jdoc {
                doc_version: 7,
                fmt_version: 3,
                client_metadata: "seeded-by-hand".to_owned(),
                json_doc: GO_SEEDED_DOC.to_owned(),
                extra: Default::default(),
            },
        )
        .await;

    tokio::time::timeout(
        CEILING,
        write_token_hash(&store, TEST_ESN, PLACEHOLDER_HASH, &clock),
    )
    .await
    .expect("write_token_hash did not finish")
    .expect("the rewrite failed");

    let docs = store.snapshot();
    assert_eq!(docs.len(), 2, "the seed and the write shared one entry");
    let written = docs
        .iter()
        .find(|entry| entry.thing == format!("vic:{TEST_ESN}"))
        .expect("nothing was stored under the prefixed serial");

    // `token.go:103-107` is skipped when the lookup hits, so the three fields
    // are the seeded document's rather than the new-token literals. Go copies
    // them one at a time into a fresh `AJdoc` at `token.go:120-124`.
    assert_eq!(written.jdoc.doc_version, 7, "token.go:122");
    assert_eq!(written.jdoc.fmt_version, 3, "token.go:123");
    assert_eq!(
        written.jdoc.client_metadata, "seeded-by-hand",
        "token.go:121"
    );
    assert_eq!(
        written.jdoc.json_doc, GO_APPENDED_DOC,
        "the appended document is not the bytes Go's json.Marshal writes"
    );

    // The appended token is the second, and the seeded one is untouched.
    let manager = decode_clean(&written.jdoc.json_doc);
    assert_eq!(manager.client_tokens.len(), 2, "token.go:114 appends");
    assert_eq!(manager.client_tokens[0].hash, SEED_HASH);
    assert_eq!(manager.client_tokens[1].hash, PLACEHOLDER_HASH);
    assert_eq!(manager.client_tokens[1].issued_at, ISSUED_AT);

    let parsed = parse_jdocs(&directory.file()).expect("the file is not the shape Go writes");
    assert_eq!(parsed, docs, "the file and the list disagree");
}

/// The empty `json_doc` every reachable call has is not one JSON value, so Go
/// returns before storing anything (`decode.go:98-105`) and the manager stays
/// at its zero value. A document that is truncated part way through is the
/// same case.
#[tokio::test]
async fn a_json_doc_that_is_not_one_json_value_leaves_the_manager_empty() {
    for seed in ["", "   ", r#"{"client_tokens":[{"hash":"z"}]"#] {
        let (_directory, store, clock) = store_and_clock("malformed");
        store
            .add_jdoc(
                TEST_ESN,
                APP_TOKENS_DOC,
                Jdoc {
                    doc_version: 1,
                    fmt_version: 1,
                    client_metadata: String::new(),
                    json_doc: seed.to_owned(),
                    extra: Default::default(),
                },
            )
            .await;

        tokio::time::timeout(
            CEILING,
            write_token_hash(&store, TEST_ESN, PLACEHOLDER_HASH, &clock),
        )
        .await
        .expect("write_token_hash did not finish")
        .expect("the rewrite failed");

        let docs = store.snapshot();
        let written = docs
            .iter()
            .find(|entry| entry.thing == format!("vic:{TEST_ESN}"))
            .expect("nothing was stored under the prefixed serial");
        assert_eq!(
            written.jdoc.json_doc, GO_APP_TOKENS_DOC,
            "the malformed json_doc {seed:?} left something behind"
        );
    }
}

/// The second class of document that empties the manager here, which Go loads
/// in full. Recorded rather than fixed in this commit.
///
/// `crate::gojson`'s object loop asks for each key as a [`String`], and
/// `serde_json` refuses to build one from a lone-surrogate escape, so the
/// refusal propagates out of the whole decode and `write_token_hash` falls back
/// to an empty manager. The appended token is then the only one, and the
/// document written is [`GO_APP_TOKENS_DOC`].
///
/// Go accepts the same document. `checkValid` does not look inside an escape,
/// `unquoteBytes` decodes a lone surrogate to U+FFFD
/// (`decode.go:1277-1288`, and the `Unmarshal` doc comment at
/// `decode.go:95-96` says so), and a key spelled U+FFFD matches no tag and is
/// dropped like any unknown key. A Go run over [`GO_SURROGATE_KEY_IN`]
/// reported no error, one client token and [`GO_SURROGATE_KEY_OUT`], so Go's
/// document after the same write would be [`GO_APPENDED_DOC`], two tokens
/// long.
///
/// The one in a key is the case that matters, because it costs the whole
/// document. The same escape in a value is narrower and already recorded: it
/// is a type error that leaves one field alone. The abort is not particular to
/// this file either, since `apiConfig.json` and `jdocs.json` read through the
/// same loop; the fix belongs to that loop and to a commit that can test all
/// three.
#[tokio::test]
async fn a_lone_surrogate_in_a_key_empties_the_manager_where_go_stores_a_replacement_character() {
    // The mechanism, before the write that shows its effect.
    assert!(
        serde_json::from_str::<Decoded<ClientTokenManager>>(GO_SURROGATE_KEY_IN).is_err(),
        "the decoder now accepts the escape, so this test no longer describes the port"
    );

    let (_directory, store, clock) = store_and_clock("surrogate-key");
    store
        .add_jdoc(
            TEST_ESN,
            APP_TOKENS_DOC,
            Jdoc {
                doc_version: 1,
                fmt_version: 1,
                client_metadata: String::new(),
                json_doc: GO_SURROGATE_KEY_IN.to_owned(),
                extra: Default::default(),
            },
        )
        .await;

    tokio::time::timeout(
        CEILING,
        write_token_hash(&store, TEST_ESN, PLACEHOLDER_HASH, &clock),
    )
    .await
    .expect("write_token_hash did not finish")
    .expect("the rewrite failed");

    let docs = store.snapshot();
    let written = docs
        .iter()
        .find(|entry| entry.thing == format!("vic:{TEST_ESN}"))
        .expect("nothing was stored under the prefixed serial");
    assert_eq!(
        written.jdoc.json_doc, GO_APP_TOKENS_DOC,
        "the seeded token survived, so the manager was not emptied"
    );

    // Go's outcome for the same write, quoted so the difference is visible
    // here rather than only in the prose above.
    assert_ne!(
        GO_APPENDED_DOC, GO_APP_TOKENS_DOC,
        "the two literals agree, so this test proves nothing about the difference"
    );
    assert_eq!(
        GO_SURROGATE_KEY_OUT, GO_SEEDED_DOC,
        "Go's decode of the document no longer gives back the seeded token"
    );
}
