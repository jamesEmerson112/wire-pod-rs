//! `jdocs.json`: the documents the robot stores on the server.
//!
//! Go keeps the file as one package-level slice, `vars.BotJdocs`
//! (`vars.go:61`), which five call sites read and four mutate with no lock
//! between them. Here it is a value the boot path produces and hands on, with
//! the slice behind a mutex, so a test can point a whole load-mutate-write
//! cycle at a temporary directory and two writers cannot interleave.
//!
//! The file is a JSON array of per-robot entries, each naming a `thing`
//! (`vic:<esn>`), a document `name` (`vic.RobotSettings` and the four others)
//! and the document itself. It is a contract in the same three ways
//! `apiConfig.json` is, and the mechanisms are the same ones.
//!
//! **Byte layout.** `WriteJdocs` is `json.Marshal` straight into `os.WriteFile`
//! with no indentation and no trailing newline (`vars.go:316-317`), so a
//! rewrite has to reproduce Go's marshaller rather than merely produce the same
//! JSON. Three things decide the bytes. Field order and tags come from the
//! struct declarations, which are `vars.go:130-135` and `vars.go:137-144` field
//! for field. All four of the inner document's fields carry `omitempty`, so a
//! zero one is absent rather than present-and-zero, which is why only one of
//! the five documents this machine holds has a `client_metadata` key. And every
//! string is written through [`crate::config::GoFormatter`], reached by
//! [`crate::config::go_marshal`], which reproduces `encoding/json`'s HTML
//! escaping. That last one matters more here than in the config file:
//! `json_doc` is a JSON *string* holding JSON *text*, so a `<`, `>` or `&`
//! anywhere inside a robot setting is escaped by Go and has to be escaped here.
//!
//! **Decoding.** The file is read back at boot and a hand-edited one is on-disk
//! state like any other, so the decoder is Go's rather than `serde`'s. The
//! object loop, the fold, the duplicate rule and the fault recording all come
//! from [`crate::config`]; this module adds only what a root that is an array
//! needs, `DecodedDocs`, and the two structs' tag tables. That is the whole
//! difference between the two files as far as `encoding/json` is concerned.
//!
//! **Forward compatibility.** Go's decoder drops any key its struct does not
//! name, so a fork-only key is lost the moment the Go server rewrites the file.
//! Both structs here carry a `#[serde(flatten)]` map instead, at both levels,
//! so an unknown key survives a read-modify-write and rolling back to the Go
//! server stays safe.
//!
//! One shape has no Rust counterpart. Go's slice is nil or empty as well as
//! populated, and `json.Marshal` writes `null` for the nil one and `[]` for the
//! empty one. A [`Vec`] cannot tell them apart, so [`marshal_jdocs`] writes
//! `null` for any empty list. Every state Go can reach agrees with that:
//! `DeleteData` builds its replacement with `var newdocs []botjdoc` and only
//! appends (`vars.go:322-325`), so removing the last document leaves the nil
//! slice and writes `null`, and every other writer runs after an `AddJdoc` that
//! has just appended or replaced, so it is never empty. `[]` therefore reaches
//! the file only if an operator puts it there by hand, and this server rewrites
//! it as `null` where Go would leave it, which is the one difference the round
//! trip does not preserve.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;

use crate::config::{
    DecodeFault, Extra, Faults, GoObject, Kind, go_marshal, store_object, store_string, store_u64,
};
use crate::paths::DataDir;
use crate::persist::write_atomic;

/// The permission bit set Go hands `os.WriteFile` at the one jdocs write site
/// (`vars.go:317`).
///
/// One constant covers the file because `WriteJdocs` is the only writer; every
/// caller goes through it rather than marshalling the slice itself.
pub const JDOCS_FILE_MODE: u32 = 0o644;

/// What an empty list marshals to, which is Go's nil slice rather than its
/// empty one. The module doc says why the two cannot be told apart here.
const EMPTY_LIST: &[u8] = b"null";

// ---------------------------------------------------------------------------
// The structs, field for field
// ---------------------------------------------------------------------------

/// Go's `vars.AJdoc` (`vars.go:130-135`), one stored document.
///
/// Field order is Go's declaration order, which is the order `encoding/json`
/// marshals in and therefore the order of the file on disk. The names are the
/// `json` halves of Go's tags; the `protobuf` halves beside them are for the
/// wire type this struct is copied from and decide nothing about the file.
///
/// All four fields carry `omitempty`, so each is absent from the file whenever
/// it holds Go's zero value. That is not cosmetic: `vic.RobotSettings` has
/// never carried a `client_metadata`, so the key is simply missing from four of
/// the five documents on this machine, and writing it back as `""` would change
/// the file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Jdoc {
    /// `vars.go:131`, with Go's comment: "first version = 1; 0 => invalid or
    /// doesn't exist". This is the number `WriteDoc` answers with, through
    /// [`AddOutcome::latest_version`].
    #[serde(skip_serializing_if = "is_zero", default)]
    pub doc_version: u64,
    /// `vars.go:132`, with Go's comment: "first version = 1; 0 => invalid".
    #[serde(skip_serializing_if = "is_zero", default)]
    pub fmt_version: u64,
    /// `vars.go:133`, with Go's comment: "arbitrary client-defined string, eg a
    /// data fingerprint (typ "", 32 chars max)". The token server is the only
    /// thing in wire-pod that sets it, to `wirepod-new-token`
    /// (`token/token.go:106`).
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub client_metadata: String,
    /// `vars.go:134`: the document itself, as JSON text inside a JSON string.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub json_doc: String,
    /// Keys inside one document this struct does not name, preserved across a
    /// round trip. Go drops them.
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// Go's `vars.botjdoc` (`vars.go:137-144`), one document belonging to one
/// robot.
///
/// None of the three fields carries `omitempty`, so all three are always
/// written, and an empty [`Jdoc`] is written as `{}` rather than dropped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BotJdoc {
    /// `vars.go:139`, with Go's comment `vic:00000000`: the robot this document
    /// belongs to, spelled `vic:` followed by the serial at every writer
    /// (`jdocspinger.go:126`, `sdkapp/server.go:223`, `token/token.go:125`) and
    /// taken straight off the request at the fourth (`jdocs/server.go:40`).
    #[serde(default)]
    pub thing: String,
    /// `vars.go:141`, with Go's comment `vic.RobotSettings, etc`: which of the
    /// five documents this is.
    #[serde(default)]
    pub name: String,
    /// `vars.go:143`: the document.
    #[serde(default)]
    pub jdoc: Jdoc,
    /// Per-entry keys this struct does not name, preserved across a round trip.
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// `omitempty` for an integer field, which Go's `isEmptyValue` reads as "equal
/// to zero" (`encoding/json/encode.go:318-330`, the `IsZero` arm at
/// `:322-327`).
fn is_zero(value: &u64) -> bool {
    *value == 0
}

// ---------------------------------------------------------------------------
// Go's object loop, struct by struct
// ---------------------------------------------------------------------------

// Each pair below is one struct's half of `encoding/json`'s decoder: the tags
// it answers to, and what one value does to one field. The loop itself,
// the exact-then-folded key match, the duplicate rule and the fault recording
// are `crate::config`'s and are Go's.

impl GoObject for Jdoc {
    const TAGS: &'static [&'static str] =
        &["doc_version", "fmt_version", "client_metadata", "json_doc"];
    const PREFIX: &'static str = "jdoc.";
    const GO_TYPE: &'static str = "AJdoc";

    fn store(
        &mut self,
        tag: &'static str,
        raw: &RawValue,
        faults: &mut Faults,
    ) -> serde_json::Result<()> {
        match tag {
            "doc_version" => store_u64(&mut self.doc_version, raw, Self::PREFIX, tag, faults),
            "fmt_version" => store_u64(&mut self.fmt_version, raw, Self::PREFIX, tag, faults),
            "client_metadata" => {
                store_string(&mut self.client_metadata, raw, Self::PREFIX, tag, faults);
            }
            "json_doc" => store_string(&mut self.json_doc, raw, Self::PREFIX, tag, faults),
            // Unreachable: the key match only ever answers with a tag out of
            // `TAGS`. A tag missing from this list would behave as an unknown
            // key does, which the round trip would see.
            _ => {}
        }
        Ok(())
    }

    fn unknown(&mut self) -> &mut Extra {
        &mut self.extra
    }
}

impl<'de> Deserialize<'de> for Jdoc {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Decoded::<Self>::deserialize(deserializer)?.value)
    }
}

impl GoObject for BotJdoc {
    const TAGS: &'static [&'static str] = &["thing", "name", "jdoc"];
    const PREFIX: &'static str = "";
    const GO_TYPE: &'static str = "botjdoc";

    fn store(
        &mut self,
        tag: &'static str,
        raw: &RawValue,
        faults: &mut Faults,
    ) -> serde_json::Result<()> {
        match tag {
            "thing" => {
                store_string(&mut self.thing, raw, Self::PREFIX, tag, faults);
                Ok(())
            }
            "name" => {
                store_string(&mut self.name, raw, Self::PREFIX, tag, faults);
                Ok(())
            }
            // Merged into whatever the field already holds rather than
            // replacing it, which is why a document carrying `jdoc` twice ends
            // up with the union of the two (`decode.go:599-829`).
            "jdoc" => store_object(&mut self.jdoc, raw, Self::PREFIX, tag, faults),
            _ => Ok(()),
        }
    }

    fn unknown(&mut self) -> &mut Extra {
        &mut self.extra
    }
}

impl<'de> Deserialize<'de> for BotJdoc {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Decoded::<Self>::deserialize(deserializer)?.value)
    }
}

/// One object decoded into a fresh value, for the two [`Deserialize`] impls
/// above.
///
/// `crate::config` has the same shape for its own structs and keeps it private,
/// because a [`Deserialize`] impl can hand back a value or an error and Go's
/// decoder produces both at once. Here the root is a list, so the fault the
/// whole file records comes out of [`DecodedDocs`] instead and this one is only
/// the single-object convenience.
struct Decoded<T> {
    value: T,
}

impl<'de, T: GoObject> Deserialize<'de> for Decoded<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = T::default();
        let mut faults = Faults::default();
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        store_object(&mut value, &raw, "", "", &mut faults).map_err(serde::de::Error::custom)?;
        Ok(Self { value })
    }
}

// ---------------------------------------------------------------------------
// A root that is an array
// ---------------------------------------------------------------------------

/// The Go type name a fault raised against the document as a whole carries.
const ROOT_GO_TYPE: &str = "[]botjdoc";

/// One `json.Unmarshal` of the whole file: what decoded, and the first fault.
///
/// Go's root here is `&BotJdocs`, a slice (`vars.go:241`), so the three arms
/// are the ones `d.value` has for one: an array is decoded element by element;
/// a `null` sets the slice to its zero value and reports nothing
/// (`decode.go:899-903`), which at boot is the empty list it already was; and
/// anything else is a type error against the slice itself and leaves it alone
/// (`decode.go:650-652`, `:929-964`, `:984`).
struct DecodedDocs {
    docs: Vec<BotJdoc>,
    fault: Option<DecodeFault>,
}

impl<'de> Deserialize<'de> for DecodedDocs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut docs = Vec::new();
        let mut faults = Faults::default();
        deserializer.deserialize_any(ListVisitor {
            docs: &mut docs,
            faults: &mut faults,
        })?;
        Ok(Self {
            docs,
            fault: faults.first,
        })
    }
}

/// Go's `d.value(rv)` at the top of an unmarshal into a slice
/// (`decode.go:178`).
struct ListVisitor<'a> {
    docs: &'a mut Vec<BotJdoc>,
    faults: &'a mut Faults,
}

impl<'de> Visitor<'de> for ListVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON array of jdocs entries")
    }

    /// `decode.go:502-592`. Every element is kept, including one whose type
    /// does not fit: Go grows the slice first (`:544-552`) and only then stores
    /// into the element it just made (`:554-558`), so a `[1, {...}]` decodes to
    /// two entries, the first of them zero, and records one fault.
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while let Some(raw) = seq.next_element::<Box<RawValue>>()? {
            let mut entry = BotJdoc::default();
            // The empty prefix and tag are what make the fault's path empty,
            // which is what Go reports for an element: `UnmarshalTypeError`
            // carries an empty `Struct` and `Field` there, because only the
            // object loop pushes onto the field stack.
            store_object(&mut entry, &raw, "", "", self.faults)
                .map_err(serde::de::Error::custom)?;
            self.docs.push(entry);
        }
        Ok(())
    }

    /// `decode.go:899-903`: a `null` sets the slice to its zero value, which is
    /// the empty list, and reports nothing.
    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    /// `decode.go:650-652`. Go skips the object before returning, and `serde`
    /// needs the same: a key left unread is a parse still inside the object.
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        self.faults.save_root(Kind::Object, ROOT_GO_TYPE);
        Ok(())
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
        self.faults.save_root(Kind::Bool, ROOT_GO_TYPE);
        Ok(())
    }

    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
        self.faults.save_root(Kind::Number, ROOT_GO_TYPE);
        Ok(())
    }

    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
        self.faults.save_root(Kind::Number, ROOT_GO_TYPE);
        Ok(())
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
        self.faults.save_root(Kind::Number, ROOT_GO_TYPE);
        Ok(())
    }

    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
        self.faults.save_root(Kind::String, ROOT_GO_TYPE);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The two halves of the file
// ---------------------------------------------------------------------------

/// What `json.Unmarshal` left behind when it reported an error
/// (`decode.go:182`).
///
/// Go has no such type: `vars.go:241` discards the error and reads the slice,
/// which holds every field that decoded before the fault and every one after
/// it. The partly decoded list is therefore part of the contract and travels
/// with the fault here, exactly as [`crate::config::DecodeError`] carries a
/// half-filled configuration.
#[derive(Debug)]
pub struct JdocsDecodeError {
    /// Every entry that decoded, the ones before the fault and the ones after
    /// it alike. Empty for a malformed document, because Go checks the whole
    /// document before it decodes anything (`decode.go:98-105`).
    pub docs: Vec<BotJdoc>,
    /// The first fault, which is the only one Go keeps.
    pub fault: DecodeFault,
}

impl fmt::Display for JdocsDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fault.fmt(formatter)
    }
}

impl std::error::Error for JdocsDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.fault)
    }
}

/// Go's `json.Marshal(BotJdocs)` (`vars.go:316`).
///
/// Compact, in declaration order, with [`crate::config::GoFormatter`]'s string
/// escaping and no trailing newline, which is exactly the bytes `os.WriteFile`
/// puts in the file. An empty list is `null`, for the reason the module doc
/// gives.
///
/// # Panics
///
/// Never. Serialising into a [`Vec`] cannot fail at the writer, every number in
/// the structs is an integer, and a [`serde_json::Value`] in an [`Extra`] map
/// cannot hold a NaN. Go discards its own marshal error at this site too.
pub fn marshal_jdocs(docs: &[BotJdoc]) -> Vec<u8> {
    if docs.is_empty() {
        return EMPTY_LIST.to_vec();
    }
    go_marshal(docs).expect("a jdocs list has no unserialisable value in it")
}

/// Go's `json.Unmarshal(jsonBytes, &BotJdocs)` (`vars.go:241`).
///
/// # Errors
///
/// Anything malformed, and anything whose type does not fit the field it
/// matched. The two are not the same failure. Go checks the whole document
/// before it decodes anything (`decode.go:98-105`), so malformed bytes leave
/// the list empty; a type error is recorded and decoding carries on to the end
/// of the document (`decode.go:243-247`), so every other entry and every other
/// field is filled. Both arms hand back whatever decoded, because Go's caller
/// keeps it either way.
pub fn parse_jdocs(bytes: &[u8]) -> Result<Vec<BotJdoc>, JdocsDecodeError> {
    match serde_json::from_slice::<DecodedDocs>(bytes) {
        Ok(DecodedDocs { docs, fault: None }) => Ok(docs),
        Ok(DecodedDocs {
            docs,
            fault: Some(fault),
        }) => Err(JdocsDecodeError { docs, fault }),
        Err(error) => Err(JdocsDecodeError {
            docs: Vec::new(),
            fault: DecodeFault::Malformed(error.to_string()),
        }),
    }
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// What [`JdocsStore::add_jdoc`] answers with.
///
/// Go returns only the version and discards the write error (`vars.go:365`,
/// `:317`). The write result comes back as well so that a full disk is visible,
/// which is the same choice [`crate::persist::write_atomic`] makes.
#[derive(Debug)]
pub struct AddOutcome {
    /// Go's `latestVersion` (`vars.go:347`): the stored document's
    /// `doc_version` when the call replaced an existing document, and `0` when
    /// it appended a new one. `jdocs/server.go:58` puts it straight into the
    /// `WriteDocResp`.
    pub latest_version: u64,
    /// What the rewrite did.
    pub written: io::Result<()>,
}

/// Which arm of Go's loader ran (`vars.go:239-243`).
///
/// Go returns nothing and logs one line for three of the four arms alike, so
/// naming them here lets the boot path tell a missing file from a corrupt one
/// without parsing a log line, and lets a test assert which one ran.
#[derive(Debug)]
pub enum JdocsLoadOutcome {
    /// `os.Stat` failed, so Go read nothing and logged nothing
    /// (`vars.go:239`).
    Missing,
    /// The file was read and decoded (`vars.go:240-242`).
    Loaded,
    /// The file stats but `os.ReadFile` failed, and Go discarded the error and
    /// carried the nil bytes into `json.Unmarshal` (`vars.go:240`).
    Unreadable(io::Error),
    /// The file was read and was not the JSON these structs describe, and Go
    /// discarded that error too (`vars.go:241`). Whatever decoded is in the
    /// store.
    ParseFailed(DecodeFault),
}

/// A loaded [`JdocsStore`] and what happened on the way.
#[derive(Debug)]
pub struct LoadedJdocs {
    /// The store, whichever arm ran.
    pub store: JdocsStore,
    /// Which of Go's four arms ran.
    pub outcome: JdocsLoadOutcome,
}

/// The jdocs file: Go's `vars.BotJdocs` (`vars.go:61`) and the four functions
/// that touch it.
///
/// The list is behind a [`std::sync::Mutex`] where Go has none at all. Every
/// mutator takes the lock, changes the list, marshals it while still holding
/// the lock and drops the guard before awaiting the write, so the bytes that
/// reach the disk are always one consistent state of the list and no guard
/// crosses an `.await`. What that does not order is the writes themselves: two
/// mutators can marshal in one order and rename in the other, leaving the file
/// holding an earlier state than the one in memory until the next write. Go has
/// the same race and a worse one on top of it, because it marshals outside any
/// lock and `os.WriteFile` truncates in place, so a Go reader can catch a
/// half-written file where a reader here cannot.
#[derive(Debug)]
pub struct JdocsStore {
    /// `vars.JdocsPath` as [`DataDir::jdocs_path`] spells it, mixed separators
    /// and all.
    path: String,
    /// Go's `vars.BotJdocs` (`vars.go:61`).
    docs: Mutex<Vec<BotJdoc>>,
}

impl JdocsStore {
    /// An empty store writing to `path`, which is the state Go's global is in
    /// before `vars.Init` runs and after a boot that found no file.
    pub fn new(path: impl Into<String>) -> Self {
        Self::with_docs(path, Vec::new())
    }

    /// A store holding `docs`, for a test and for the loader below.
    pub fn with_docs(path: impl Into<String>, docs: Vec<BotJdoc>) -> Self {
        Self {
            path: path.into(),
            docs: Mutex::new(docs),
        }
    }

    /// Go's loader (`vars.go:239-243`).
    ///
    /// Go stats the file and, only if the stat succeeds, reads it, unmarshals
    /// it and logs `Loaded jdocs file`. Both the read error and the unmarshal
    /// error are discarded, and the log line is outside both, so Go announces a
    /// file it failed to read and a file that was not JSON in exactly the same
    /// words it announces a good one. That is reproduced: the line is logged on
    /// all three of those arms and on none of them is anything else said. The
    /// outcome carries what the log line does not.
    pub async fn load(dir: &DataDir) -> LoadedJdocs {
        let path = dir.jdocs_path();

        let (docs, outcome) = match read_if_present(PathBuf::from(&path)).await {
            // `vars.go:239`: the stat failed, so the body never ran and nothing
            // was logged.
            OnDisk::Missing => (Vec::new(), JdocsLoadOutcome::Missing),
            // `vars.go:240`: `jsonBytes` is nil and `json.Unmarshal` fails on
            // it, both errors discarded.
            OnDisk::Unreadable(error) => {
                tracing::debug!(comp = "", "Loaded jdocs file");
                (Vec::new(), JdocsLoadOutcome::Unreadable(error))
            }
            OnDisk::Bytes(bytes) => {
                let (docs, outcome) = match parse_jdocs(&bytes) {
                    Ok(docs) => (docs, JdocsLoadOutcome::Loaded),
                    // `vars.go:241`: the error is discarded and the partly
                    // decoded slice is what the rest of the process sees.
                    Err(JdocsDecodeError { docs, fault }) => {
                        (docs, JdocsLoadOutcome::ParseFailed(fault))
                    }
                };
                // `vars.go:242`, which runs whatever the unmarshal did.
                tracing::debug!(comp = "", "Loaded jdocs file");
                (docs, outcome)
            }
        };

        LoadedJdocs {
            store: Self::with_docs(path, docs),
            outcome,
        }
    }

    /// The file this store writes to, as Go spells it.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// A copy of the whole list, for a caller that has to hold it across an
    /// `.await` or wants to compare it with the file.
    pub fn snapshot(&self) -> Vec<BotJdoc> {
        self.locked().clone()
    }

    /// Go's `GetJdoc` (`vars.go:332-339`).
    ///
    /// Both comparisons are `==`, not `EqualFold`, and the loop returns the
    /// first match, so a `thing` that differs only in case is a miss and a
    /// duplicated pair is answered by the earlier entry.
    ///
    /// `None` is Go's `(AJdoc{}, false)`. A caller that wants Go's shape
    /// exactly writes `get_jdoc(..).unwrap_or_default()`, which is what
    /// `WriteTokenHash` needs: it fills three fields of the blank document when
    /// the lookup misses (`token/token.go:101-107`).
    ///
    /// `thing` is a `&str` rather than an [`crate::esn::Esn`] on purpose. Go's
    /// callers disagree about how they spell it, and one of them disagrees with
    /// itself: `WriteTokenHash` looks a document up under the bare serial
    /// (`token/token.go:101`) and stores it under `vic:` plus the serial
    /// (`token/token.go:125`), so the lookup never finds what the store wrote
    /// and `vic.AppTokens` gains one client token per authentication instead of
    /// accumulating them. Normalising here would quietly fix that, and the fix
    /// would change the file the Go server reads, so the store takes whatever
    /// string the call site passes.
    pub fn get_jdoc(&self, thing: &str, name: &str) -> Option<Jdoc> {
        self.locked()
            .iter()
            .find(|entry| entry.name == name && entry.thing == thing)
            .map(|entry| entry.jdoc.clone())
    }

    /// Go's `AddJdoc` (`vars.go:346-366`).
    ///
    /// The loop breaks on the first entry matching both `thing` and `name`
    /// (`vars.go:349-356`), replaces that entry's document wholesale and reads
    /// the version back out of the entry it has just written, so the version
    /// answered is the incoming document's, not the one it replaced. With no
    /// match it appends a new entry and the version stays `0` (`vars.go:347`,
    /// `:357-363`). Replacing the document also drops any unknown keys that
    /// document carried, while the entry's own unknown keys survive, because Go
    /// assigns to `.Jdoc` and not to the entry.
    ///
    /// Go then calls `WriteJdocs` (`vars.go:364`) and both of its callers call
    /// it again immediately (`jdocs/server.go:40-41`, `token/token.go:125-126`),
    /// so the file is written twice with the same bytes. This writes once. The
    /// second write is unobservable for the same reason the first one is
    /// atomic: a reader sees one whole file or the other, and the two are
    /// identical.
    pub async fn add_jdoc(&self, thing: &str, name: &str, jdoc: Jdoc) -> AddOutcome {
        let (latest_version, bytes) = {
            let mut docs = self.locked();
            let found = docs
                .iter()
                .position(|entry| entry.thing == thing && entry.name == name);
            let latest_version = match found {
                Some(index) => {
                    docs[index].jdoc = jdoc;
                    docs[index].jdoc.doc_version
                }
                None => {
                    docs.push(BotJdoc {
                        thing: thing.to_owned(),
                        name: name.to_owned(),
                        jdoc,
                        extra: Extra::new(),
                    });
                    0
                }
            };
            (latest_version, marshal_jdocs(&docs))
        };

        AddOutcome {
            latest_version,
            written: self.write_bytes(bytes).await,
        }
    }

    /// Go's `DeleteData` (`vars.go:321-330`).
    ///
    /// Every entry whose `thing` is not the one named survives, with `==` again
    /// rather than a fold, and every document belonging to that robot goes at
    /// once. The file is rewritten whether or not anything was removed, and
    /// removing the last entry leaves Go's nil slice, which marshals to the
    /// literal `null`.
    pub async fn delete_data(&self, thing: &str) -> io::Result<()> {
        let bytes = {
            let mut docs = self.locked();
            docs.retain(|entry| entry.thing != thing);
            marshal_jdocs(&docs)
        };
        self.write_bytes(bytes).await
    }

    /// Go's `WriteJdocs` (`vars.go:315-318`).
    ///
    /// The mode is the one that call site passes, [`JDOCS_FILE_MODE`], and the
    /// replacement is atomic where Go's `os.WriteFile` truncates in place.
    pub async fn write(&self) -> io::Result<()> {
        let bytes = marshal_jdocs(&self.locked());
        self.write_bytes(bytes).await
    }

    /// The list, with a poisoned lock read through rather than panicked on.
    ///
    /// A panic in one handler must not take the jdocs file out of service for
    /// the process, and every mutator above leaves the list consistent before
    /// it can unwind: the only fallible step, the write, happens after the
    /// guard is gone.
    fn locked(&self) -> std::sync::MutexGuard<'_, Vec<BotJdoc>> {
        self.docs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The one write, so that the mode and the path are stated once.
    async fn write_bytes(&self, bytes: Vec<u8>) -> io::Result<()> {
        write_atomic(PathBuf::from(&self.path), bytes, JDOCS_FILE_MODE).await
    }
}

/// What Go's stat-then-read found (`vars.go:239-240`).
enum OnDisk {
    /// `os.Stat` failed, however it failed.
    Missing,
    /// The file's bytes.
    Bytes(Vec<u8>),
    /// `os.ReadFile` failed on a file that does stat.
    Unreadable(io::Error),
}

/// Runs Go's stat-then-read inside `spawn_blocking`, for the same reason
/// [`crate::persist::write_atomic`] does: these are blocking file operations on
/// a runtime whose worker threads are also carrying gRPC streams.
async fn read_if_present(path: PathBuf) -> OnDisk {
    tokio::task::spawn_blocking(move || {
        if std::fs::metadata(&path).is_err() {
            return OnDisk::Missing;
        }
        match std::fs::read(&path) {
            Ok(bytes) => OnDisk::Bytes(bytes),
            Err(error) => OnDisk::Unreadable(error),
        }
    })
    .await
    .unwrap_or_else(|error| OnDisk::Unreadable(io::Error::other(error)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tag tables are what a key is matched against, so a tag that drifts
    /// from the `Serialize` name would decode a file this server just wrote
    /// into the extras map instead of into the field. Nothing else states both
    /// spellings in one place.
    #[test]
    fn every_tag_is_a_key_the_marshaller_writes() {
        let entry = BotJdoc {
            thing: "vic:00303f28".to_owned(),
            name: "vic.RobotSettings".to_owned(),
            jdoc: Jdoc {
                doc_version: 1,
                fmt_version: 1,
                client_metadata: "m".to_owned(),
                json_doc: "{}".to_owned(),
                extra: Extra::new(),
            },
            extra: Extra::new(),
        };

        let written = String::from_utf8(marshal_jdocs(std::slice::from_ref(&entry)))
            .expect("the rewrite is not UTF-8");

        for tag in BotJdoc::TAGS {
            assert!(
                written.contains(&format!("\"{tag}\":")),
                "the marshaller writes no {tag} key for a tag the decoder answers to"
            );
        }
        for tag in Jdoc::TAGS {
            assert!(
                written.contains(&format!("\"{tag}\":")),
                "the marshaller writes no {tag} key for a tag the decoder answers to"
            );
        }
    }

    /// The prefixes decide the path a fault carries, and Go's is the field
    /// stack joined with dots: empty at the entry and `jdoc.` inside the
    /// document (`decode.go:732-733`, confirmed against a Go run whose
    /// `UnmarshalTypeError` reported `Field="jdoc.doc_version"`).
    #[test]
    fn the_prefixes_are_gos_field_stack() {
        assert_eq!(BotJdoc::PREFIX, "");
        assert_eq!(Jdoc::PREFIX, "jdoc.");
    }
}
