//! `session-certs/`: the boot walk, the `RecurringInfo` list, the writer, and
//! the DER walk that turns a certificate into a robot name.
//!
//! Every case runs in a directory under the system temporary directory, named
//! for the process, so nothing here reads or writes the live
//! `%APPDATA%\wire-pod\session-certs`. The one certificate any of them uses is
//! `assets/epod/ep.crt`, the escape-pod certificate vendored in this
//! repository, which is public: it is the certificate the server presents to
//! every robot. No key, GUID, token or robot serial from this machine appears
//! anywhere below; the serials are placeholders.
//!
//! There is no `#[ignore]` live check here. The live `session-certs` directory
//! on this machine is empty, not even carrying the `placeholder`, so a live
//! check would assert the empty-directory case the temporary directories
//! already cover, and would read the operator's directory to do it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wirepod_core::paths::DataDir;
use wirepod_core::persist::TemporaryIn;
use wirepod_core::store::bot_info::{BotInfo, BotInfoRobot};
use wirepod_core::store::session_certs::{
    PLACEHOLDER_NAME, ReadSessionCertsOutcome, RecurringInfo, SESSION_CERT_FILE_MODE,
    SessionCertStore, certificate_der, issuer_common_name, read_session_certs, session_cert_gate,
    session_cert_read_path, write_session_cert,
};

/// The escape-pod certificate, vendored byte-identically from the Go repository
/// and served to every robot, so nothing about it is private.
const EP_CRT: &[u8] = include_bytes!("../../../assets/epod/ep.crt");

/// A ceiling on every awaited operation, generous enough that only a hang
/// reaches it. Real durations, because this crate's tests never pause the
/// runtime's clock.
const CEILING: Duration = Duration::from_secs(30);

/// `assets/epod/ep.crt`'s issuer common name, which is the empty string.
///
/// Recorded from Go: a throwaway program over this exact file printed
///
/// ```text
/// issuer.CommonName=""
/// issuer.String="O=Digital Dream Labs,L=Pittsburgh,ST=Pennsylvania,C=US,..."
/// subject.CommonName="escapepod.local"
/// ```
///
/// so its issuer carries no `commonName` attribute at all while its subject
/// carries one. That asymmetry is what makes this file able to tell a walk that
/// reads the issuer from one that reads the subject.
const EP_CRT_ISSUER_CN: &str = "";

/// `assets/epod/ep.crt`'s subject common name, from the same program. Nothing
/// in the port should ever produce it.
const EP_CRT_SUBJECT_CN: &str = "escapepod.local";

// ---------------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------------

/// A pod directory under the system temporary directory, with `session-certs/`
/// inside it, removed when the test ends.
struct Pod {
    path: PathBuf,
}

impl Pod {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is before the epoch")
            .as_nanos();
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wirepod-sessioncerts-{label}-{}-{nanos:x}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("could not create the temporary directory");
        Self { path }
    }

    fn dir(&self) -> DataDir {
        DataDir::rooted(&self.path)
    }

    /// Creates `session-certs/` and seeds Go's own placeholder, which is what
    /// `vars.go:186` and the installer leave there.
    fn with_session_certs(self) -> Self {
        let certs = self.certs();
        fs::create_dir_all(&certs).expect("could not create session-certs/");
        fs::write(certs.join(PLACEHOLDER_NAME), b"").expect("could not seed the placeholder");
        self
    }

    fn certs(&self) -> PathBuf {
        self.dir().session_cert_dir()
    }

    fn seed(&self, name: &str, bytes: &[u8]) {
        fs::write(self.certs().join(name), bytes).expect("could not seed an entry");
    }
}

impl Drop for Pod {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The names in a directory, sorted, so a leftover temporary is visible.
fn entry_names(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .expect("could not list the directory")
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

/// A bot-info file naming one robot.
fn bot_info(esn: &str, ip: &str) -> BotInfo {
    BotInfo {
        robots: vec![BotInfoRobot {
            esn: esn.to_owned(),
            ip_address: ip.to_owned(),
            ..BotInfoRobot::default()
        }],
        ..BotInfo::default()
    }
}

// ---------------------------------------------------------------------------
// ReadSessionCerts
// ---------------------------------------------------------------------------

/// The ordinary boot: a placeholder and one certificate named after a robot.
///
/// The entry name is the serial, the address comes from the bot-info robot with
/// that serial, and the name is the certificate's issuer common name, which for
/// `ep.crt` is the empty string.
#[tokio::test]
async fn a_certificate_named_after_a_robot_becomes_one_recurring_info_entry() {
    let pod = Pod::new("one").with_session_certs();
    pod.seed("00000001", EP_CRT);

    let load = tokio::time::timeout(
        CEILING,
        read_session_certs(&pod.dir(), &bot_info("00000001", "192.0.2.11")),
    )
    .await
    .expect("the walk hung");

    assert!(matches!(load.outcome, ReadSessionCertsOutcome::Complete));
    assert_eq!(
        load.info,
        vec![RecurringInfo {
            id: EP_CRT_ISSUER_CN.to_owned(),
            esn: "00000001".to_owned(),
            ip: "192.0.2.11".to_owned(),
        }],
        "the placeholder was read as a robot, or the entry was built wrong"
    );
    // The walk must read the issuer, not the subject. `ep.crt` is the one file
    // that can tell them apart, because only its subject carries a common name.
    assert_ne!(load.info[0].id, EP_CRT_SUBJECT_CN);
}

/// A serial the bot-info file does not carry keeps its entry and leaves the
/// address empty (`vars.go:381`, `:389-393`, `:397`).
#[tokio::test]
async fn a_robot_missing_from_the_bot_info_file_keeps_an_empty_address() {
    let pod = Pod::new("noaddress").with_session_certs();
    pod.seed("00000001", EP_CRT);

    let load = tokio::time::timeout(
        CEILING,
        read_session_certs(&pod.dir(), &bot_info("00000009", "192.0.2.99")),
    )
    .await
    .expect("the walk hung");

    assert_eq!(load.info.len(), 1);
    assert_eq!(load.info[0].esn, "00000001");
    assert_eq!(load.info[0].ip, "", "an unrelated robot's address was used");
}

/// The address lookup is `==` and stops at the first match (`vars.go:390-392`),
/// where the bot-info resolver folds case and lets the last match win.
#[tokio::test]
async fn the_address_lookup_is_exact_and_takes_the_first_match() {
    let pod = Pod::new("exact").with_session_certs();
    pod.seed("00000001", EP_CRT);

    let info = BotInfo {
        robots: vec![
            BotInfoRobot {
                esn: "00000001".to_owned(),
                ip_address: "192.0.2.11".to_owned(),
                ..BotInfoRobot::default()
            },
            BotInfoRobot {
                esn: "00000001".to_owned(),
                ip_address: "192.0.2.22".to_owned(),
                ..BotInfoRobot::default()
            },
            BotInfoRobot {
                esn: "00000002".to_owned(),
                ip_address: "192.0.2.33".to_owned(),
                ..BotInfoRobot::default()
            },
        ],
        ..BotInfo::default()
    };
    let load = tokio::time::timeout(CEILING, read_session_certs(&pod.dir(), &info))
        .await
        .expect("the walk hung");
    assert_eq!(
        load.info[0].ip, "192.0.2.11",
        "the break at :392 was dropped"
    );

    // And a serial that differs only in case is a miss, because the comparison
    // is not a fold.
    let pod = Pod::new("exact-case").with_session_certs();
    pod.seed("00E20145", EP_CRT);
    let load = tokio::time::timeout(
        CEILING,
        read_session_certs(&pod.dir(), &bot_info("00e20145", "192.0.2.11")),
    )
    .await
    .expect("the walk hung");
    assert_eq!(load.info[0].ip, "", "the comparison folded case");
}

/// A file that is not a PEM certificate logs and ends the walk, leaving every
/// later entry unread.
///
/// Go dereferences the nil `pem.Decode` result here and the process dies
/// (`vars.go:387-388`), which also leaves the later entries unread. Deviations
/// 31 and 32: the log line replaces the panic and the early return is kept, so
/// the shape of what is missing afterwards is Go's.
#[tokio::test]
async fn a_file_that_is_not_a_certificate_ends_the_walk() {
    let pod = Pod::new("notpem").with_session_certs();
    pod.seed("00000001", b"this is not a certificate\n");
    pod.seed("00000002", EP_CRT);

    let load = tokio::time::timeout(
        CEILING,
        read_session_certs(&pod.dir(), &bot_info("00000002", "192.0.2.22")),
    )
    .await
    .expect("the walk hung");

    assert!(
        matches!(&load.outcome, ReadSessionCertsOutcome::NotACertificate { esn } if esn == "00000001"),
        "the bad entry was not reported: {:?}",
        load.outcome
    );
    assert!(
        load.info.is_empty(),
        "the walk continued past the bad entry and read 00000002"
    );

    // The later entry really would have produced one, so the assertion above is
    // about the early return and not about an entry that was never going to
    // appear.
    let pod = Pod::new("notpem-control").with_session_certs();
    pod.seed("00000002", EP_CRT);
    let load = tokio::time::timeout(
        CEILING,
        read_session_certs(&pod.dir(), &bot_info("00000002", "192.0.2.22")),
    )
    .await
    .expect("the walk hung");
    assert_eq!(load.info.len(), 1);
}

/// An entry that cannot be read ends the walk too, which is Go's own early
/// return rather than a deviation (`vars.go:383-386`).
///
/// The unreadable entry is a directory, because `os.ReadFile` and
/// [`std::fs::read`] both refuse one on every platform, and because
/// `ReadSessionCerts` takes every entry name as a serial without asking whether
/// it is a file (`vars.go:376-380`).
#[tokio::test]
async fn an_entry_that_cannot_be_read_ends_the_walk() {
    let pod = Pod::new("unreadable").with_session_certs();
    fs::create_dir(pod.certs().join("00000001")).expect("could not seed a directory entry");
    pod.seed("00000002", EP_CRT);

    let load = tokio::time::timeout(
        CEILING,
        read_session_certs(&pod.dir(), &bot_info("00000002", "192.0.2.22")),
    )
    .await
    .expect("the walk hung");

    assert!(
        matches!(&load.outcome, ReadSessionCertsOutcome::Unreadable { esn, .. } if esn == "00000001"),
        "the unreadable entry was not reported: {:?}",
        load.outcome
    );
    assert!(
        load.info.is_empty(),
        "the walk continued past the unreadable entry"
    );
}

/// A missing `session-certs/` logs the error and returns, which is what a first
/// boot before `vars.Init` creates the directory looks like
/// (`vars.go:371-375`).
#[tokio::test]
async fn a_missing_directory_is_logged_and_returns_nothing() {
    let pod = Pod::new("missing");
    let load = tokio::time::timeout(
        CEILING,
        read_session_certs(&pod.dir(), &bot_info("00000001", "192.0.2.11")),
    )
    .await
    .expect("the walk hung");

    assert!(
        matches!(load.outcome, ReadSessionCertsOutcome::Unlistable(_)),
        "a missing directory was not reported: {:?}",
        load.outcome
    );
    assert!(load.info.is_empty());
}

/// A directory holding nothing but the placeholder yields nothing, which is the
/// state this machine's own directory is in.
#[tokio::test]
async fn a_directory_holding_only_the_placeholder_yields_nothing() {
    let pod = Pod::new("empty").with_session_certs();
    let load = tokio::time::timeout(
        CEILING,
        read_session_certs(&pod.dir(), &bot_info("00000001", "192.0.2.11")),
    )
    .await
    .expect("the walk hung");

    assert!(matches!(load.outcome, ReadSessionCertsOutcome::Complete));
    assert!(load.info.is_empty());
}

/// The walk takes the entries in name order, because `os.ReadDir` sorts and
/// [`std::fs::read_dir`] does not.
#[tokio::test]
async fn the_walk_is_in_name_order() {
    let pod = Pod::new("order").with_session_certs();
    for name in ["00000003", "00000001", "00000002"] {
        pod.seed(name, EP_CRT);
    }
    let load = tokio::time::timeout(
        CEILING,
        read_session_certs(&pod.dir(), &bot_info("00000001", "192.0.2.11")),
    )
    .await
    .expect("the walk hung");

    assert_eq!(
        load.info
            .iter()
            .map(|entry| entry.esn.as_str())
            .collect::<Vec<_>>(),
        ["00000001", "00000002", "00000003"]
    );
}

/// The store wrapper is the same walk with the list behind its mutex.
#[tokio::test]
async fn the_store_loads_what_the_walk_produced() {
    let pod = Pod::new("store").with_session_certs();
    pod.seed("00000001", EP_CRT);

    let loaded = tokio::time::timeout(
        CEILING,
        SessionCertStore::load(&pod.dir(), &bot_info("00000001", "192.0.2.11")),
    )
    .await
    .expect("the load hung");

    assert!(matches!(loaded.outcome, ReadSessionCertsOutcome::Complete));
    assert_eq!(loaded.store.len(), 1);
    assert_eq!(loaded.store.snapshot()[0].esn, "00000001");
}

/// The three JSON tags and the field order are Go's declaration order
/// (`vars.go:100-107`).
///
/// Go never marshals `RecurringInfoStore` today, so nothing on the wire depends
/// on this yet. The route C20 brings will, and this is what keeps the tags from
/// being invented there.
#[test]
fn a_recurring_info_entry_marshals_with_gos_tags_in_gos_order() {
    let entry = RecurringInfo {
        id: "Vector-R2D2".to_owned(),
        esn: "00000001".to_owned(),
        ip: "192.0.2.11".to_owned(),
    };
    assert_eq!(
        serde_json::to_string(&entry).expect("a recurring info entry serialises"),
        r#"{"id":"Vector-R2D2","esn":"00000001","ip":"192.0.2.11"}"#
    );
}

// ---------------------------------------------------------------------------
// AddToRInfo
// ---------------------------------------------------------------------------

/// `AddToRInfo` updates the entry whose serial matches and appends only when
/// none does (`vars.go:402-416`).
///
/// A version that always appended would pass any test that only looked at the
/// last entry, so the length is asserted at every step and the updated entry is
/// read back by position.
#[test]
fn add_to_r_info_updates_in_place_and_appends_only_a_new_serial() {
    let store = SessionCertStore::new();
    assert!(store.is_empty());

    assert!(
        store.add_to_r_info("00000001", "Vector-A1B2", "192.0.2.11"),
        "the first call did not append"
    );
    assert!(
        store.add_to_r_info("00000002", "Vector-C3D4", "192.0.2.12"),
        "a new serial did not append"
    );
    assert_eq!(store.len(), 2);

    // The same serial again updates the entry that is already there and adds
    // nothing.
    assert!(
        !store.add_to_r_info("00000001", "Vector-Z9Y8", "192.0.2.99"),
        "an existing serial appended"
    );
    assert_eq!(store.len(), 2, "an existing serial appended");
    let info = store.snapshot();
    assert_eq!(
        info[0],
        RecurringInfo {
            id: "Vector-Z9Y8".to_owned(),
            esn: "00000001".to_owned(),
            ip: "192.0.2.99".to_owned(),
        },
        "the first entry was not updated in place"
    );
    assert_eq!(info[1].id, "Vector-C3D4", "the second entry moved");
}

/// The serial comparison is `==` and not `EqualFold` (`vars.go:405`), so a
/// serial in the other case appends rather than updating.
#[test]
fn add_to_r_info_does_not_fold_case() {
    let store = SessionCertStore::new();
    store.add_to_r_info("00E20145", "Vector-A1B2", "192.0.2.11");
    assert!(
        store.add_to_r_info("00e20145", "Vector-C3D4", "192.0.2.12"),
        "the comparison folded case"
    );
    assert_eq!(store.len(), 2);
}

// ---------------------------------------------------------------------------
// Writing a session certificate
// ---------------------------------------------------------------------------

/// The write lands at the bare serial and leaves nothing else in either
/// directory.
///
/// `session-certs/` is the one directory Go enumerates and reads names out of,
/// so the assertion is Go's own loop: every name in it is either the
/// placeholder or something Go will take for a robot serial.
#[tokio::test]
async fn a_written_certificate_lands_under_the_bare_serial() {
    let pod = Pod::new("write").with_session_certs();

    tokio::time::timeout(
        CEILING,
        write_session_cert(&pod.dir(), "00000001", EP_CRT.to_vec()),
    )
    .await
    .expect("the write hung")
    .expect("the write failed");

    assert_eq!(
        fs::read(pod.certs().join("00000001")).expect("the certificate is missing"),
        EP_CRT
    );
    assert_eq!(entry_names(&pod.certs()), ["00000001", PLACEHOLDER_NAME]);
    assert_eq!(
        entry_names(&pod.path),
        ["session-certs"],
        "a temporary survived in the pod root"
    );
    for name in entry_names(&pod.certs()) {
        assert!(
            !name.contains(".tmp"),
            "vars.go:380 would read the temporary {name} as a robot serial"
        );
    }

    // And the boot walk reads back exactly what was written.
    let load = tokio::time::timeout(
        CEILING,
        read_session_certs(&pod.dir(), &bot_info("00000001", "192.0.2.11")),
    )
    .await
    .expect("the walk hung");
    assert_eq!(load.info.len(), 1);
}

/// The two spellings of one robot's certificate name the same file: the writer
/// concatenates the way `jdocs/server.go:123` does and the reader joins the way
/// `vars.go:382` and `config-ws/webserver.go:511` do.
#[test]
fn the_two_spellings_of_a_certificate_name_the_same_file() {
    let pod = Pod::new("spelling");
    let dir = pod.dir();
    let written = dir.session_cert_path("00000001");
    let read = session_cert_read_path(&dir, "00000001");
    assert_eq!(read, dir.session_cert_dir().join("00000001"));
    assert_eq!(
        Path::new(&written).canonicalize().ok(),
        read.canonicalize().ok(),
        "neither path exists yet, so both canonicalise to None"
    );
    assert!(
        written.ends_with("/00000001"),
        "{written} is not concatenated"
    );
}

/// The writer's temporary goes in the pod root, never in the directory the boot
/// walk enumerates, and the write carries Go's mode.
///
/// A temporary left in `session-certs/` by a process that died mid-write is a
/// bogus robot at the next boot here and a panic at the next Go boot
/// (`vars.go:377-388`). Where a temporary lands cannot be seen once the write
/// has finished, and racing a write in flight would make the check depend on
/// timing, so the gate is asserted instead;
/// `crates/wirepod-core/tests/persist.rs` pins what an aborted write with this
/// placement actually leaves behind.
///
/// The mode is asserted here for a related reason: on Windows nothing on disk
/// records it at all, and on Unix the umask masks `0755` down to something a
/// wrong mode could also have produced.
#[test]
fn the_writer_keeps_its_temporary_out_of_the_walked_directory() {
    let pod = Pod::new("placement");
    let gate = session_cert_gate(&pod.dir(), "00000001");
    assert_eq!(gate.temporary_in(), TemporaryIn::ParentDirectory);
    assert_eq!(gate.path(), pod.dir().session_cert_path("00000001"));
    assert_eq!(
        gate.mode(),
        SESSION_CERT_FILE_MODE,
        "the writer does not carry `jdocs/server.go:123`'s mode"
    );
}

/// `jdocs/server.go:123`'s mode reaches the file the writer creates.
///
/// Unix only, because the mode is: Windows has no permission bit set for the
/// assertion to read. The shape is `tests/persist.rs`'s, which asserts that no
/// bit beyond the requested ones is set and so does not depend on the umask,
/// plus the bits that tell `0755` from the `0644` the jdocs and bot-info files
/// use: a umask can only take an execute bit away and never grant one, so a
/// file written with `0755` keeps at least one under any umask an operator
/// would set, and a file written with `0644` has none under any umask at all.
#[cfg(unix)]
#[tokio::test]
async fn the_session_certificates_mode_is_gos() {
    use std::os::unix::fs::PermissionsExt;

    let pod = Pod::new("certmode").with_session_certs();
    tokio::time::timeout(
        CEILING,
        write_session_cert(&pod.dir(), "00000001", EP_CRT.to_vec()),
    )
    .await
    .expect("the write hung")
    .expect("the write failed");

    let got = fs::metadata(pod.certs().join("00000001"))
        .expect("the certificate is missing")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        got & !SESSION_CERT_FILE_MODE,
        0,
        "mode {got:o} carries a bit {SESSION_CERT_FILE_MODE:o} did not ask for"
    );
    assert_ne!(
        got & 0o111,
        0,
        "mode {got:o} carries no execute bit at all, so it cannot be 0755"
    );
}

// ---------------------------------------------------------------------------
// The DER walk
// ---------------------------------------------------------------------------

/// `ep.crt` walks, and its issuer carries no common name.
///
/// Both halves matter. The walk reaching the issuer at all is what the
/// certificate proves, and the empty answer is what tells a walk that reads the
/// issuer from one that reads the subject: the subject's common name is
/// `escapepod.local` and a walk that read it would answer that instead of
/// `None`.
#[test]
fn the_walk_reads_the_issuer_and_not_the_subject() {
    let der = certificate_der(EP_CRT).expect("ep.crt is a PEM certificate");
    assert_eq!(
        der.len(),
        1289,
        "ep.crt's DER is not the length Go reported"
    );
    assert_eq!(
        issuer_common_name(&der),
        None,
        "ep.crt's issuer carries no common name"
    );
    assert_eq!(
        EP_CRT_ISSUER_CN, "",
        "the recorded issuer common name is the empty string"
    );
    assert_ne!(
        issuer_common_name(&der),
        Some(EP_CRT_SUBJECT_CN.to_owned()),
        "the walk read the subject"
    );
}

/// A certificate whose issuer does carry a common name answers it.
///
/// The certificate is `ep.crt` with its issuer `Name` replaced, because no
/// certificate with a `Vector-XXXX` issuer may be committed: a real one belongs
/// to a robot. The splice was verified against Go, which parses the result with
/// `x509.ParseCertificate` and reports
/// `issuer.CommonName="Vector-R2D2"`, `issuer.String="CN=Vector-R2D2"` and the
/// original `subject.CommonName="escapepod.local"`, so the bytes this builds
/// are bytes Go agrees about.
#[test]
fn an_issuer_common_name_is_read_back() {
    let der = spliced_issuer("Vector-R2D2");
    assert_eq!(
        der.len(),
        1181,
        "the splice is not the length Go reported for it"
    );
    assert_eq!(
        issuer_common_name(&der),
        Some("Vector-R2D2".to_owned()),
        "the issuer common name was not read"
    );
}

/// A v1 certificate, whose TBSCertificate carries no `[0]` version element at
/// all, still walks down to its issuer.
///
/// `version [0] EXPLICIT Version DEFAULT v1` is optional in RFC 5280 and Go
/// reads it with `ReadOptionalASN1Integer(..., 0)`
/// (`crypto/x509/parser.go:909`), so an absent element is v1 and the serial is
/// simply the first field. Every other certificate in this file is v3, `ep.crt`
/// included, so without this case the branch that steps over the element would
/// be the only one exercised and taking the branch unconditionally would go
/// unnoticed.
///
/// The splice was verified against Go, which parses these exact bytes with
/// `x509.ParseCertificate` and reports `version=1`,
/// `issuer.CommonName="Vector-R2D2"`, `issuer.String="CN=Vector-R2D2"` and the
/// original `subject.CommonName="escapepod.local"`, at a length of 1176 bytes:
/// five fewer than the v3 splice, which is the version element.
#[test]
fn a_certificate_with_no_version_element_still_walks() {
    let der = spliced_without_version("Vector-R2D2");
    assert_eq!(
        der.len(),
        1176,
        "the v1 splice is not the length Go reported for it"
    );
    assert_eq!(
        der.len() + EP_CRT_VERSION.len(),
        spliced_issuer("Vector-R2D2").len(),
        "the v1 splice differs from the v3 one by something other than the version element"
    );
    assert_eq!(
        issuer_common_name(&der),
        Some("Vector-R2D2".to_owned()),
        "a v1 certificate's issuer common name was not read"
    );
}

/// The last `commonName` in the issuer wins, because `FillFromRDNSequence`
/// assigns once per matching attribute without breaking
/// (`crypto/x509/pkix/pkix.go:144-161`).
#[test]
fn the_last_issuer_common_name_wins() {
    let der = spliced(&[
        rdn_common_name("Vector-FIRST"),
        rdn_common_name("Vector-LAST"),
    ]);
    assert_eq!(issuer_common_name(&der), Some("Vector-LAST".to_owned()));
}

/// The four string types a common name is accepted in
/// (`crypto/x509/parser.go:60-108`).
#[test]
fn the_four_accepted_string_types_are_read() {
    for tag in [0x0c_u8, 0x13, 0x14, 0x16] {
        let der = spliced(&[rdn(tag, b"Vector-R2D2")]);
        assert_eq!(
            issuer_common_name(&der),
            Some("Vector-R2D2".to_owned()),
            "tag {tag:#04x} was refused"
        );
    }
    // A `PrintableString` carrying a character Go's `isPrintable` refuses ends
    // the walk, because Go's own `parseName` refuses it too and the certificate
    // never parses.
    let der = spliced(&[rdn(0x13, b"Vector_R2D2")]);
    assert_eq!(issuer_common_name(&der), None);
    // An `IA5String` above U+007F likewise (`x509.go:1169-1178`).
    let der = spliced(&[rdn(0x16, &[b'V', 0xc3, 0xa9])]);
    assert_eq!(issuer_common_name(&der), None);
    // A tag that is not a string at all.
    let der = spliced(&[rdn(0x02, b"1")]);
    assert_eq!(issuer_common_name(&der), None);
}

/// Every prefix of a real certificate answers `None` and none of them panics,
/// which is the property the whole walk is written for: the file it is handed
/// is whatever an operator, a crash or a rollback left in `session-certs/`.
#[test]
fn every_truncation_of_a_certificate_is_refused_without_panicking() {
    let der = certificate_der(EP_CRT).expect("ep.crt is a PEM certificate");

    // Every prefix, not a sample: the walk steps through five nested elements
    // and a length field, and a sample would miss whichever boundary a mistake
    // sits on.
    for cut in 0..der.len() {
        assert_eq!(
            issuer_common_name(&der[..cut]),
            None,
            "a certificate truncated to {cut} bytes answered a name"
        );
    }
    // And the whole thing still walks, so the loop above is not passing because
    // the walk refuses everything.
    assert!(issuer_common_name(&spliced_issuer("Vector-R2D2")).is_some());

    // A truncated PEM file is refused at the PEM step rather than the DER one.
    for cut in 0..EP_CRT.len() {
        let _ = certificate_der(&EP_CRT[..cut]);
    }
    assert!(certificate_der(b"not a certificate at all").is_none());
    assert!(certificate_der(b"").is_none());
}

/// Documents that are not certificates are refused rather than walked into.
#[test]
fn a_document_that_is_not_a_certificate_is_refused() {
    // An empty SEQUENCE, a SEQUENCE of SEQUENCEs whose fields have the wrong
    // tags, and a length that runs past the end.
    assert_eq!(issuer_common_name(&[0x30, 0x00]), None);
    assert_eq!(
        issuer_common_name(&[0x30, 0x06, 0x30, 0x04, 0x30, 0x00, 0x30, 0x00]),
        None
    );
    assert_eq!(issuer_common_name(&[0x30, 0x7f, 0x30, 0x01]), None);
    // An indefinite length, which is BER and not DER.
    assert_eq!(issuer_common_name(&[0x30, 0x80, 0x00, 0x00]), None);
    assert_eq!(issuer_common_name(&[]), None);
}

/// A PEM block whose label the reader cannot name. The body is `MAA=`, which is
/// the base64 of an empty ASN.1 `SEQUENCE` and not a key of any kind; the
/// reader never decodes it, because it drops the whole block on the label.
const UNKNOWN_LABEL_BLOCK: &[u8] = b"-----BEGIN FOO-----\nMAA=\n-----END FOO-----\n";

/// A PEM block with a label the reader does know and this port does not want.
/// The body is the same empty `SEQUENCE`: no key, real or invented, appears in
/// this repository.
const PRIVATE_KEY_BLOCK: &[u8] = b"-----BEGIN PRIVATE KEY-----\nMAA=\n-----END PRIVATE KEY-----\n";

/// A certificate revocation list's label, with the same empty body.
const CRL_BLOCK: &[u8] = b"-----BEGIN X509 CRL-----\nMAA=\n-----END X509 CRL-----\n";

/// A first block whose label the reader cannot name is skipped and the
/// certificate after it is read, where `pem.Decode` would have taken the first
/// block and killed Go.
///
/// This is the candidate deviation `certificate_der`'s doc names, and what
/// decides it is the label and not the type: the reader skips a block it cannot
/// turn into an item at all and stops on one it can, which the case below is
/// the other half of.
#[test]
fn an_unnameable_pem_label_is_skipped_and_the_certificate_after_it_is_read() {
    let mut file = UNKNOWN_LABEL_BLOCK.to_vec();
    file.extend_from_slice(EP_CRT);
    assert_eq!(
        certificate_der(&file),
        certificate_der(EP_CRT),
        "the certificate after an unnameable block was not reached"
    );
}

/// A first block the reader can name but this port does not want ends the walk,
/// whatever follows it.
///
/// `pem.Decode` takes that same first block, hands it to `ParseCertificate`,
/// and dies on the nil result (`vars.go:387-388`), so both stop here and only
/// the way they stop differs. A certificate revocation list is the case worth
/// spelling out, because its DER would walk: a CRL is a `SEQUENCE` whose first
/// field is a `TBSCertList`, so accepting the item and then walking it would
/// read a revocation list's issuer as a robot name.
#[test]
fn a_nameable_pem_section_that_is_not_a_certificate_is_refused() {
    let mut file = PRIVATE_KEY_BLOCK.to_vec();
    file.extend_from_slice(EP_CRT);
    assert_eq!(
        certificate_der(&file),
        None,
        "a block the reader named as a private key was not refused"
    );

    let mut file = CRL_BLOCK.to_vec();
    file.extend_from_slice(EP_CRT);
    assert_eq!(
        certificate_der(&file),
        None,
        "a block the reader named as a revocation list was not refused"
    );
    assert_eq!(certificate_der(CRL_BLOCK), None);
}

// ---------------------------------------------------------------------------
// Building a certificate whose issuer carries a common name
// ---------------------------------------------------------------------------

/// Where `ep.crt`'s issuer `Name` sits inside its DER, as a throwaway Go
/// program reported after walking the TBSCertificate's fields:
///
/// ```text
/// cert: tag=30 hdr=4 len=1285 total=1289
/// tbs:  off=4 tag=30 hdr=4 len=749
///   field 3: off=50 tag=30 hdr=3 len=129
/// ```
///
/// So the issuer occupies `50..182` and the TBSCertificate's contents occupy
/// `8..757`, both of which the splice below rebuilds around.
const EP_CRT_ISSUER: std::ops::Range<usize> = 50..182;
/// The TBSCertificate's contents, from the same walk.
const EP_CRT_TBS: std::ops::Range<usize> = 8..757;
/// The `[0] EXPLICIT Version` element the TBSCertificate opens with, which the
/// same program printed as `a0 03 02 01 02`: `[0]` wrapping `INTEGER 2`, the
/// DER spelling of v3. [`spliced_without_version`] drops exactly these bytes.
const EP_CRT_VERSION: &[u8] = &[0xa0, 0x03, 0x02, 0x01, 0x02];

/// One `AttributeTypeAndValue` holding `commonName` with the given value and
/// string tag, wrapped in its own `RelativeDistinguishedName` SET.
fn rdn(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut atv = vec![0x06, 0x03, 0x55, 0x04, 0x03];
    atv.push(tag);
    atv.push(u8::try_from(value.len()).expect("a short value"));
    atv.extend_from_slice(value);

    let mut sequence = vec![0x30, u8::try_from(atv.len()).expect("a short attribute")];
    sequence.extend_from_slice(&atv);

    let mut set = vec![
        0x31,
        u8::try_from(sequence.len()).expect("a short attribute"),
    ];
    set.extend_from_slice(&sequence);
    set
}

/// [`rdn`] with the `UTF8String` tag, which is what a Vector session
/// certificate uses.
fn rdn_common_name(value: &str) -> Vec<u8> {
    rdn(0x0c, value.as_bytes())
}

/// `ep.crt` with its issuer replaced by an `RDNSequence` of the given relative
/// distinguished names, and the two enclosing lengths rebuilt.
///
/// Both enclosing elements keep their two-byte long-form length, because every
/// name this builds is far shorter than the 129-byte issuer it replaces and
/// both totals stay well above 255.
fn spliced(rdns: &[Vec<u8>]) -> Vec<u8> {
    spliced_versioned(rdns, true)
}

/// [`spliced`] with the optional `[0] EXPLICIT Version` element kept or
/// dropped, which is the difference between a v3 certificate and a v1 one.
fn spliced_versioned(rdns: &[Vec<u8>], keep_version: bool) -> Vec<u8> {
    let der = certificate_der(EP_CRT).expect("ep.crt is a PEM certificate");

    let mut name_contents = Vec::new();
    for rdn in rdns {
        name_contents.extend_from_slice(rdn);
    }
    let mut name = vec![
        0x30,
        u8::try_from(name_contents.len()).expect("a short name"),
    ];
    name.extend_from_slice(&name_contents);

    let mut tbs_contents = der[EP_CRT_TBS.start..EP_CRT_ISSUER.start].to_vec();
    tbs_contents.extend_from_slice(&name);
    tbs_contents.extend_from_slice(&der[EP_CRT_ISSUER.end..EP_CRT_TBS.end]);

    if !keep_version {
        assert_eq!(
            &tbs_contents[..EP_CRT_VERSION.len()],
            EP_CRT_VERSION,
            "the TBSCertificate does not open with the version element"
        );
        tbs_contents.drain(..EP_CRT_VERSION.len());
    }

    let mut certificate_contents = long_form(0x30, &tbs_contents);
    certificate_contents.extend_from_slice(&der[EP_CRT_TBS.end..]);
    long_form(0x30, &certificate_contents)
}

/// [`spliced`] with a single `commonName` attribute, which is the shape a
/// Vector session certificate's issuer has.
fn spliced_issuer(common_name: &str) -> Vec<u8> {
    spliced(&[rdn_common_name(common_name)])
}

/// [`spliced_issuer`] with the `[0]` version element dropped, which is what a
/// v1 certificate looks like.
fn spliced_without_version(common_name: &str) -> Vec<u8> {
    spliced_versioned(&[rdn_common_name(common_name)], false)
}

/// One element with the two-byte long-form length both of `ep.crt`'s enclosing
/// SEQUENCEs use.
fn long_form(tag: u8, contents: &[u8]) -> Vec<u8> {
    let length = u16::try_from(contents.len()).expect("under 64 kilobytes");
    assert!(length > 0xff, "the length would not be two bytes in DER");
    let mut out = vec![tag, 0x82, (length >> 8) as u8, length as u8];
    out.extend_from_slice(contents);
    out
}
