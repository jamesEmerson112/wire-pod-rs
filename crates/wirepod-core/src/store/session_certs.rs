//! `session-certs/`: one file per robot, named by its serial, and the
//! `RecurringInfo` list the boot walk over that directory builds.
//!
//! Go writes a robot's session certificate twice when the robot associates,
//! once beside the SDK ini file and once into `session-certs/`
//! (`jdocs/server.go:121`, `:123`), and reads the second copy back at every
//! boot (`vars.go:368-400`, reached from `vars.go:256`). The walk is what turns
//! a directory of certificates into `vars.RecurringInfo`, the ESN-to-name-to-IP
//! list that the jdocs pinger matches mDNS answers against
//! (`sdkapp/jdocspinger.go:242-244`) and that the `/session-certs/<esn>` route
//! serves from (`config-ws/webserver.go:502-517`).
//!
//! Three things about that walk decide what this module has to do.
//!
//! **Every name in the directory is a robot serial.** The walk takes
//! `entry.Name()` as the ESN with no check beyond skipping the literal
//! `placeholder` (`vars.go:377-380`), so anything else that lands in the
//! directory becomes a robot. That is why [`write_session_cert`] puts its
//! temporary in the pod root through [`TemporaryIn::ParentDirectory`] rather
//! than beside the target: see the [`crate::persist`] module doc, which carries
//! the whole argument, and `crates/wirepod-core/tests/persist.rs`, which pins
//! it.
//!
//! **Go dereferences the PEM block unchecked.** `pem.Decode` returns nil for a
//! file that is not PEM and `x509.ParseCertificate` returns nil for a block
//! that is not a certificate, and both errors are discarded before
//! `cert.Issuer.CommonName` is read (`vars.go:387-388`), so either one is a nil
//! dereference and the process dies at boot. Deviations 31 and 32 are that: the
//! port logs and keeps Go's early return, so the rest of the directory is left
//! unread exactly as a panic would leave it, and the server survives.
//!
//! **The name comes out of the certificate, not out of a file.** The robot's
//! name, `Vector-R2D2` and the like, is the issuer common name of its own
//! session certificate. No crate in `Cargo.lock` exposes a certificate's issuer
//! name, and no crate may be added for it, so [`issuer_common_name`] is a
//! direct DER walk down to that one attribute. It is deliberately the smallest
//! walk that reaches it and it is total: every malformed input answers `None`
//! and none of them panics.
//!
//! Nothing here is persisted as a file of its own. `vars.RecurringInfo`
//! (`vars.go:80`) is rebuilt from the directory at every boot and mutated in
//! memory afterwards, so [`SessionCertStore`] is the list and its two
//! mutators, and the only file this module writes is a robot's certificate.
//!
//! There is no `#[ignore]` live check in the tests beside this module, because
//! the live `%APPDATA%\wire-pod\session-certs` on this machine is empty, not
//! even carrying the `placeholder`: a live check would assert the empty case
//! the temporary-directory tests already cover and would read the operator's
//! directory to do it.

use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;

use crate::paths::DataDir;
use crate::persist::{TemporaryIn, WriteGate};
use crate::store::bot_info::BotInfo;

/// The mode `os.WriteFile` is given for a session certificate under
/// `session-certs/` (`jdocs/server.go:123`).
///
/// `jdocs/botInfoStorer.go:99` writes the same file with `0777` on the path
/// that downloads a certificate from the DDL servers. Deviation 36 does not
/// reproduce that path at all, so `0755` is the only mode a session
/// certificate is ever written with here.
pub const SESSION_CERT_FILE_MODE: u32 = 0o755;

/// The one entry name in `session-certs/` that is not a robot serial
/// (`vars.go:377`).
pub const PLACEHOLDER_NAME: &str = "placeholder";

// ---------------------------------------------------------------------------
// RecurringInfo
// ---------------------------------------------------------------------------

/// One robot in Go's `vars.RecurringInfo` (`vars.go:80`), whose element type is
/// `RecurringInfoStore` (`vars.go:100-107`).
///
/// The field order and the three JSON tags are Go's, comments included: `id` is
/// the robot name such as `Vector-R2D2`, `esn` the serial such as `00e20145`,
/// and `ip` the address with no port. Go never marshals this type today, so the
/// tags are a contract only for the routes C20 brings; deriving `Serialize` now
/// is what keeps them from being invented there.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RecurringInfo {
    /// `Vector-R2D2`: the issuer common name of the robot's session
    /// certificate (`vars.go:102`, filled at `vars.go:396`).
    pub id: String,
    /// `00e20145`: the serial, which is the certificate's file name
    /// (`vars.go:104`, filled at `vars.go:395`).
    pub esn: String,
    /// `192.168.1.150`: the address, with no port, taken from the bot-info
    /// file (`vars.go:106`, filled at `vars.go:397`).
    pub ip: String,
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// Go's `vars.RecurringInfo` (`vars.go:80`) and the two functions that touch
/// it.
///
/// The list is behind a [`std::sync::Mutex`] where Go has none: the boot walk
/// fills it, the jdocs handler appends to it (`jdocs/server.go:125`) and the
/// pinger's mDNS goroutine both reads and updates it
/// (`sdkapp/jdocspinger.go:242-244`), with nothing between them. No guard here
/// crosses an `.await`, which the crate-level `deny(clippy::await_holding_lock)`
/// keeps true.
#[derive(Debug, Default)]
pub struct SessionCertStore {
    /// Go's `vars.RecurringInfo` (`vars.go:80`).
    info: Mutex<Vec<RecurringInfo>>,
}

impl SessionCertStore {
    /// An empty store, which is the state Go's global is in before
    /// `ReadSessionCerts` runs.
    pub fn new() -> Self {
        Self::default()
    }

    /// A store holding `info`, for a test and for [`SessionCertStore::load`].
    pub fn with_info(info: Vec<RecurringInfo>) -> Self {
        Self {
            info: Mutex::new(info),
        }
    }

    /// Runs [`read_session_certs`] and wraps the list it produced in a store,
    /// which is what the boot path C22 brings does at `vars.go:256`.
    pub async fn load(dir: &DataDir, bot_info: &BotInfo) -> LoadedSessionCerts {
        let RecurringInfoLoad { info, outcome } = read_session_certs(dir, bot_info).await;
        LoadedSessionCerts {
            store: Self::with_info(info),
            outcome,
        }
    }

    /// A copy of the whole list, for a caller that has to hold it across an
    /// `.await` or serialise it.
    pub fn snapshot(&self) -> Vec<RecurringInfo> {
        self.locked().clone()
    }

    /// How many robots the list holds.
    pub fn len(&self) -> usize {
        self.locked().len()
    }

    /// Whether the list is empty, which it is until a certificate is read or a
    /// robot associates.
    pub fn is_empty(&self) -> bool {
        self.locked().is_empty()
    }

    /// Go's `AddToRInfo` (`vars.go:402-416`).
    ///
    /// The serial is the only fixed part of a robot, so an entry whose ESN is
    /// already in the list has its name and address overwritten in place and
    /// the function returns; only a serial that is not in the list appends. The
    /// comparison is `==` and not `EqualFold` (`vars.go:405`), so a serial that
    /// differs only in case appends a second entry rather than updating the
    /// first, which is the opposite of what [`BotInfo::resolve`] does with the
    /// bot-info file.
    ///
    /// Answers whether the call appended, which Go does not: the pinger calls
    /// this only for a serial it just found in the list
    /// (`sdkapp/jdocspinger.go:242-244`) and the jdocs handler only for one it
    /// may not have (`jdocs/server.go:125`), so the two call sites want
    /// different answers and neither reads one today.
    pub fn add_to_r_info(&self, esn: &str, id: &str, ip: &str) -> bool {
        let mut info = self.locked();
        // `vars.go:404-409`: the first matching entry is updated and the loop
        // returns, so a duplicated serial leaves the later copies alone.
        for entry in info.iter_mut() {
            if entry.esn == esn {
                entry.id = id.to_owned();
                entry.ip = ip.to_owned();
                return false;
            }
        }
        // `vars.go:411-415`.
        info.push(RecurringInfo {
            id: id.to_owned(),
            esn: esn.to_owned(),
            ip: ip.to_owned(),
        });
        true
    }

    /// The list, with a poisoned lock read through rather than panicked on, for
    /// the reason [`crate::store::jdocs::JdocsStore`] gives: a panic in one
    /// handler must not take the list out of service for the process, and every
    /// mutator above leaves it consistent before it can unwind.
    fn locked(&self) -> MutexGuard<'_, Vec<RecurringInfo>> {
        self.info.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

// ---------------------------------------------------------------------------
// ReadSessionCerts
// ---------------------------------------------------------------------------

/// Which arm of Go's `ReadSessionCerts` ended the walk (`vars.go:368-400`).
///
/// Go returns nothing and logs a bare error for two of these, so naming them
/// lets the boot path tell an absent directory from a corrupt certificate
/// without parsing a log line, and lets a test assert which one ran.
#[derive(Debug)]
pub enum ReadSessionCertsOutcome {
    /// The loop reached the end of the directory (`vars.go:399`). Every entry
    /// that was not the placeholder produced a [`RecurringInfo`].
    Complete,
    /// `os.ReadDir` failed, so Go logged the error and returned before reading
    /// anything (`vars.go:371-375`). A missing `session-certs/` is this arm.
    Unlistable(io::Error),
    /// `os.ReadFile` failed on one entry, so Go logged the error and returned
    /// from the whole function, abandoning every later entry
    /// (`vars.go:382-386`).
    Unreadable {
        /// The entry name, which Go had already taken as the serial.
        esn: String,
        /// What the read failed with.
        error: io::Error,
    },
    /// The entry was read and is not a certificate this can walk, which is
    /// where Go dereferences a nil `pem.Decode` result or a nil
    /// `x509.ParseCertificate` result and dies (`vars.go:387-388`).
    ///
    /// Deviations 31 and 32: the port logs instead of panicking and keeps Go's
    /// early return, because a panic abandons the later entries too.
    NotACertificate {
        /// The entry name, which Go had already taken as the serial.
        esn: String,
    },
}

/// What [`read_session_certs`] produced: Go's `vars.RecurringInfo` as the walk
/// left it, and which arm ended the walk.
#[derive(Debug)]
pub struct RecurringInfoLoad {
    /// The entries built before the walk ended, in directory order.
    pub info: Vec<RecurringInfo>,
    /// Which arm ended it.
    pub outcome: ReadSessionCertsOutcome,
}

/// A loaded [`SessionCertStore`] and what happened on the way.
#[derive(Debug)]
pub struct LoadedSessionCerts {
    /// The store, whichever arm ran.
    pub store: SessionCertStore,
    /// Which arm ended the walk.
    pub outcome: ReadSessionCertsOutcome,
}

/// Go's `ReadSessionCerts` (`vars.go:368-400`), which `vars.Init` calls once at
/// boot (`vars.go:256`).
///
/// The walk lists `session-certs/`, skips the entry named `placeholder`, and
/// takes every other entry name as a robot serial. It reads the entry, decodes
/// it as PEM, parses the block as a certificate, takes the address from the
/// bot-info robot carrying that serial, and appends an entry whose `id` is the
/// certificate's issuer common name.
///
/// Four details are Go's rather than obvious.
///
/// The address lookup is `==` and stops at the first match (`vars.go:389-393`),
/// where [`BotInfo::resolve`] folds case and lets the last match win. A serial
/// the bot-info file does not carry leaves the address empty rather than
/// dropping the entry.
///
/// A read failure returns from the whole function rather than continuing to the
/// next entry (`vars.go:383-386`), so one unreadable certificate hides every
/// robot whose serial sorts after it.
///
/// A file that is not a PEM certificate is a nil dereference in Go
/// (`vars.go:387-388`). Deviations 31 and 32 make it a log line, and the early
/// return is kept because a panic ends the walk too.
///
/// Both log lines are `logger.Println` (`vars.go:369`, `:373`, `:384`), which
/// is DEBUG with an empty component and an empty bot (`logger.go:234-238`), so
/// they are `tracing::debug!` with an explicit empty `comp`. The error text is
/// Rust's rather than Go's `*fs.PathError`, which is the same difference the
/// configuration layer records.
///
/// The listing is sorted by entry name, because `os.ReadDir` sorts and
/// [`std::fs::read_dir`] does not; without it the order of
/// [`RecurringInfoLoad::info`] and of the entries a failure abandons would be
/// whatever the filesystem handed back.
pub async fn read_session_certs(dir: &DataDir, bot_info: &BotInfo) -> RecurringInfoLoad {
    let directory = dir.session_cert_dir();
    // The address lookup, flattened out of `BotInfo` so the walk can run inside
    // `spawn_blocking` without borrowing it. File order is preserved, which is
    // what makes the first match the same match Go takes.
    let addresses: Vec<(String, String)> = bot_info
        .robots
        .iter()
        .map(|robot| (robot.esn.clone(), robot.ip_address.clone()))
        .collect();

    tokio::task::spawn_blocking(move || read_session_certs_blocking(&directory, &addresses))
        .await
        .unwrap_or_else(|error| RecurringInfoLoad {
            info: Vec::new(),
            outcome: ReadSessionCertsOutcome::Unlistable(io::Error::other(error)),
        })
}

/// The blocking half of [`read_session_certs`], which is the whole of Go's
/// function: the directory listing, every file read and both log lines happen
/// here, so their order is Go's and none of them runs on a runtime worker
/// thread that is also carrying a gRPC stream.
fn read_session_certs_blocking(
    directory: &std::path::Path,
    addresses: &[(String, String)],
) -> RecurringInfoLoad {
    // `vars.go:369`.
    tracing::debug!(comp = "", "Reading session certs for robot IDs");

    let mut names = match list_entry_names(directory) {
        Ok(names) => names,
        // `vars.go:372-375`.
        Err(error) => {
            tracing::debug!(comp = "", "{error}");
            return RecurringInfoLoad {
                info: Vec::new(),
                outcome: ReadSessionCertsOutcome::Unlistable(error),
            };
        }
    };
    sort_entry_names(&mut names);

    let mut info = Vec::new();
    for name in names {
        // `vars.go:377-379`.
        if name == PLACEHOLDER_NAME {
            continue;
        }
        // `vars.go:380`: the entry name is the serial, whatever it is.
        let esn = name;

        // `vars.go:382`, which is `filepath.Join` rather than the
        // concatenation `DataDir::session_cert_path` reproduces; the two name
        // the same file and only the joined one is opened here.
        let certificate = match std::fs::read(directory.join(&esn)) {
            Ok(bytes) => bytes,
            // `vars.go:383-386`: logged, and the whole function returns.
            Err(error) => {
                tracing::debug!(comp = "", "{error}");
                return RecurringInfoLoad {
                    info,
                    outcome: ReadSessionCertsOutcome::Unreadable { esn, error },
                };
            }
        };

        // `vars.go:387-388`, where Go dereferences both nil results.
        let Some(issuer) = certificate_issuer(&certificate) else {
            // Deviations 31 and 32. Go has no log line here because it never
            // reaches one, so the text is this module's.
            tracing::debug!(
                comp = "",
                "session-certs/{esn} is not a PEM certificate, abandoning the walk"
            );
            return RecurringInfoLoad {
                info,
                outcome: ReadSessionCertsOutcome::NotACertificate { esn },
            };
        };

        // `vars.go:389-393`: exact comparison, first match, and no match leaves
        // the address empty.
        let ip = addresses
            .iter()
            .find(|(candidate, _)| *candidate == esn)
            .map(|(_, ip)| ip.clone())
            .unwrap_or_default();

        // `vars.go:395-398`.
        info.push(RecurringInfo {
            id: issuer.into_common_name(),
            esn,
            ip,
        });
    }

    RecurringInfoLoad {
        info,
        outcome: ReadSessionCertsOutcome::Complete,
    }
}

/// The entry names in `directory`, in whatever order the filesystem answers:
/// every entry, including directories, named rather than opened.
///
/// The ordering is [`sort_entry_names`]'s job and not this one's, so that the
/// comparison can be tested without a directory to put names in.
///
/// A name that is not valid Unicode is taken lossily, for the reason
/// [`crate::paths`] gives: it is better to walk a robot with a replacement
/// character in its serial than to refuse to read the directory at all. No
/// serial on this machine is anything but ASCII hex.
fn list_entry_names(directory: &std::path::Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
}

/// Puts the entry names in the order `os.ReadDir` hands them over, which is
/// byte order over the name (`os/dir.go:126-128`, whose comparison is
/// `bytealg.CompareString`).
///
/// [`std::fs::read_dir`] promises no order at all. NTFS happens to answer in
/// name order, so on this machine the walk would agree with Go with or without
/// this call and the sort would look like dead weight; on a filesystem that
/// answers in creation order it is the whole of the agreement. What the order
/// decides is [`RecurringInfoLoad::info`]'s own order and, when a read fails,
/// which entries the early return at `vars.go:383-386` abandons.
///
/// Rust's [`Ord`] for [`String`] compares the UTF-8 bytes, which is the
/// comparison `bytealg.CompareString` makes, so the two sorts agree
/// everywhere: `"C"` sorts before `"a"` in both, because `0x43 < 0x61`.
fn sort_entry_names(names: &mut [String]) {
    names.sort();
}

// ---------------------------------------------------------------------------
// Writing a session certificate
// ---------------------------------------------------------------------------

/// Writes one robot's session certificate under `session-certs/`, which is
/// `os.WriteFile(vars.SessionCertPath+"/"+esn, ..., 0755)`
/// (`jdocs/server.go:123`).
///
/// The file name is the bare serial, because that is what the boot walk reads
/// back as one (`vars.go:380`). The temporary goes in the pod root rather than
/// beside the target, through [`TemporaryIn::ParentDirectory`], because a
/// temporary left in `session-certs/` by a process that died mid-write is a
/// bogus robot at the next boot and a panic at the next Go boot; the whole
/// argument is in the [`crate::persist`] module doc.
///
/// The gate is created here rather than held, because a session certificate is
/// written once per association by one handler and there is nothing to order
/// against. What the gate carries is the mode, the temporary's directory and
/// the atomic replacement.
///
/// # Errors
///
/// Whatever [`WriteGate::write`] reports. Go discards this result
/// (`jdocs/server.go:123`); it comes back here so a full disk is visible.
pub async fn write_session_cert(dir: &DataDir, esn: &str, certificate: Vec<u8>) -> io::Result<()> {
    session_cert_gate(dir, esn).write(move || certificate).await
}

/// The gate [`write_session_cert`] writes through: the concatenated file name,
/// Go's mode, and the temporary in the pod root rather than in the directory
/// the boot walk reads.
///
/// It is public so that the placement rule can be asserted directly rather than
/// inferred from a race against a write in flight, which is the only other way
/// to see where a temporary goes.
pub fn session_cert_gate(dir: &DataDir, esn: &str) -> WriteGate {
    WriteGate::with_temporary_in(
        dir.session_cert_path(esn),
        SESSION_CERT_FILE_MODE,
        TemporaryIn::ParentDirectory,
    )
}

/// One robot's session certificate as [`read_session_certs`] opens it, which is
/// `filepath.Join(SessionCertPath, esn)` (`vars.go:382`) rather than the
/// concatenation [`DataDir::session_cert_path`] reproduces for the writer.
///
/// Both name the same file. The concatenated spelling is the one that reaches a
/// log line, so it is the one the writer keeps.
pub fn session_cert_read_path(dir: &DataDir, esn: &str) -> PathBuf {
    dir.session_cert_dir().join(esn)
}

// ---------------------------------------------------------------------------
// The DER walk
// ---------------------------------------------------------------------------

/// What the walk found in a certificate's issuer, which is not the same
/// question as whether it found a name.
///
/// Go's `cert.Issuer.CommonName` is the empty string both for a certificate
/// whose issuer carries no `commonName` attribute and for one it never parsed,
/// because the second case never returns: it panics. The port has to tell them
/// apart, because the first produces a [`RecurringInfo`] with an empty `id` and
/// the second takes the deviation-32 early return.
enum IssuerName {
    /// The certificate walked and its issuer carries no `commonName`, which is
    /// Go's empty `cert.Issuer.CommonName`. `assets/epod/ep.crt` is this: its
    /// subject is `CN=escapepod.local` and its issuer has no common name at
    /// all.
    WithoutCommonName,
    /// The value of the issuer's last `commonName` attribute.
    CommonName(String),
}

impl IssuerName {
    /// The string Go would have put in `RecurringInfo.ID`.
    fn into_common_name(self) -> String {
        match self {
            Self::WithoutCommonName => String::new(),
            Self::CommonName(name) => name,
        }
    }
}

/// The DER of the first certificate in a PEM file, which is `pem.Decode`
/// followed by the type check `x509.ParseCertificate` makes
/// (`vars.go:387-388`).
///
/// `pem.Decode` takes the first block whatever its label is and hands the bytes
/// to `ParseCertificate`, which then refuses anything that is not a
/// certificate. `read_one_from_slice` splits that into two behaviours, and only
/// one of them is a difference.
///
/// A block that becomes an `Item` is answered, and every `Item` but
/// `X509Certificate` falls to the `_` arm below. So a file whose first block is
/// a private key, a revocation list or a certificate request answers `None`
/// here and takes the deviation-32 path, where Go parses that same block as a
/// certificate, fails, and dies on the nil result: both stop, and only the way
/// they stop differs.
///
/// A block that becomes no `Item` at all is skipped outright and the reader
/// goes on to the next block. Two labels do that: one the reader cannot name
/// (`rustls-pki-types-1.15.1/src/pem.rs:311-315`, the `SectionLabel::Unknown`
/// arm) and `ECHCONFIG`, which it can name and does not map
/// (`rustls-pemfile-2.2.0/src/pemfile.rs:95`, continued on at `:76`). That is
/// the difference: a file whose first block is `-----BEGIN FOO-----` and whose
/// second is a certificate answers the certificate here and kills Go. It is a
/// candidate deviation in the direction of surviving a file Go dies on, and
/// `crates/wirepod-core/tests/session_certs.rs` pins both halves.
pub fn certificate_der(pem: &[u8]) -> Option<Vec<u8>> {
    let (item, _) = rustls_pemfile::read_one_from_slice(pem).ok()??;
    match item {
        rustls_pemfile::Item::X509Certificate(der) => Some(der.to_vec()),
        _ => None,
    }
}

/// The issuer common name of a DER-encoded certificate, or `None` when there is
/// not one to read.
///
/// This is `cert.Issuer.CommonName` (`vars.go:388`) with the panic answered
/// instead. `None` covers two different Go outcomes and a caller cannot tell
/// them apart: a document that is not a certificate, where Go dereferences a
/// nil pointer, and a certificate whose issuer carries no common name, where Go
/// answers the empty string. [`read_session_certs`] needs them apart, so it
/// uses the private walk; a caller that only wants the name wants this.
///
/// `assets/epod/ep.crt` is the second case: its subject is
/// `CN=escapepod.local` and its issuer carries no common name at all, so this
/// answers `None` for it.
pub fn issuer_common_name(der: &[u8]) -> Option<String> {
    match issuer_of_der(der)? {
        IssuerName::CommonName(name) => Some(name),
        IssuerName::WithoutCommonName => None,
    }
}

/// [`issuer_common_name`] over a PEM file with the "no common name" case kept
/// separate, which is what the loader needs.
fn certificate_issuer(pem: &[u8]) -> Option<IssuerName> {
    issuer_of_der(&certificate_der(pem)?)
}

/// ASN.1 tags this walk recognises. Everything else ends the walk.
mod tag {
    /// `SEQUENCE`, constructed.
    pub const SEQUENCE: u8 = 0x30;
    /// `SET`, constructed.
    pub const SET: u8 = 0x31;
    /// `INTEGER`.
    pub const INTEGER: u8 = 0x02;
    /// `OBJECT IDENTIFIER`.
    pub const OID: u8 = 0x06;
    /// `[0]`, constructed: the explicit tag around `TBSCertificate.version`.
    pub const CONTEXT_0: u8 = 0xa0;
    /// `UTF8String`.
    pub const UTF8_STRING: u8 = 0x0c;
    /// `PrintableString`.
    pub const PRINTABLE_STRING: u8 = 0x13;
    /// `T61String`, which RFC 5280 calls TeletexString.
    pub const T61_STRING: u8 = 0x14;
    /// `IA5String`.
    pub const IA5_STRING: u8 = 0x16;
}

/// The DER contents of the `id-at-commonName` object identifier, `2.5.4.3`.
///
/// `FillFromRDNSequence` fills `Name.CommonName` from exactly this attribute
/// type (`crypto/x509/pkix/pkix.go:157-160`, the `case 3:` arm of a four-arc
/// `2.5.4.x`).
const COMMON_NAME_OID: &[u8] = &[0x55, 0x04, 0x03];

/// One DER element: its tag and its contents, with the length consumed.
struct Element<'a> {
    /// The identifier octet.
    tag: u8,
    /// The contents octets, without the identifier or the length.
    contents: &'a [u8],
}

/// Reads one DER element off the front of `input`, answering it and whatever
/// follows it, or `None` for anything malformed.
///
/// Only what X.509 guarantees is accepted: a single-byte identifier, and either
/// the short length form or a definite long form of at most four bytes. The
/// indefinite form is not DER, and a length that needs more than four bytes is
/// longer than any certificate, so both are refused rather than parsed.
fn read_element(input: &[u8]) -> Option<(Element<'_>, &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;

    let (length, rest) = if first & 0x80 == 0 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        let (bytes, rest) = rest.split_at(count);
        let length = bytes
            .iter()
            .fold(0usize, |length, byte| (length << 8) | usize::from(*byte));
        (length, rest)
    };

    if rest.len() < length {
        return None;
    }
    let (contents, rest) = rest.split_at(length);
    Some((Element { tag, contents }, rest))
}

/// Walks a DER-encoded certificate down to its issuer name.
///
/// ```text
/// Certificate  ::= SEQUENCE { tbsCertificate TBSCertificate, ... }
/// TBSCertificate ::= SEQUENCE {
///     version         [0] EXPLICIT Version DEFAULT v1,
///     serialNumber        CertificateSerialNumber,   -- INTEGER
///     signature           AlgorithmIdentifier,       -- SEQUENCE
///     issuer              Name,                      -- SEQUENCE
///     ... }
/// ```
///
/// Only the four fields above the issuer are walked, and every one of them is
/// checked for the tag RFC 5280 gives it, so a document that happens to be a
/// `SEQUENCE` of `SEQUENCE`s cannot be read as a certificate. Nothing below the
/// issuer is looked at, which is why the subject, the validity and the public
/// key never have to be understood.
fn issuer_of_der(der: &[u8]) -> Option<IssuerName> {
    let (certificate, _) = read_element(der)?;
    if certificate.tag != tag::SEQUENCE {
        return None;
    }
    let (tbs, _) = read_element(certificate.contents)?;
    if tbs.tag != tag::SEQUENCE {
        return None;
    }

    let mut rest = tbs.contents;
    // `version [0] EXPLICIT`, absent in a v1 certificate.
    let (first, after_version) = read_element(rest)?;
    if first.tag == tag::CONTEXT_0 {
        rest = after_version;
    }
    // `serialNumber`.
    let (serial, after_serial) = read_element(rest)?;
    if serial.tag != tag::INTEGER {
        return None;
    }
    // `signature`, an `AlgorithmIdentifier`.
    let (algorithm, after_algorithm) = read_element(after_serial)?;
    if algorithm.tag != tag::SEQUENCE {
        return None;
    }
    // `issuer`, an `RDNSequence`.
    let (issuer, _) = read_element(after_algorithm)?;
    if issuer.tag != tag::SEQUENCE {
        return None;
    }
    common_name_of(issuer.contents)
}

/// The last `commonName` in an `RDNSequence`, which is the one Go keeps.
///
/// ```text
/// Name             ::= SEQUENCE OF RelativeDistinguishedName
/// RelativeDistinguishedName ::= SET OF AttributeTypeAndValue
/// AttributeTypeAndValue     ::= SEQUENCE { type OBJECT IDENTIFIER, value ANY }
/// ```
///
/// `FillFromRDNSequence` assigns `n.CommonName` once per matching attribute
/// without breaking (`pkix.go:144-161`), so a name carrying two of them keeps
/// the later one, and this reproduces that rather than stopping at the first.
///
/// Attributes whose type is not `commonName` are stepped over without their
/// value being decoded, where Go decodes every attribute in the name and
/// refuses the whole certificate for any value it cannot read
/// (`parser.go:133-142`). That is a candidate deviation in the forgiving
/// direction: a certificate carrying, say, a `BMPString` organisation walks
/// here and would too in Go, but one carrying a value Go rejects walks here and
/// kills Go. A `commonName` in a string type this does not accept is the
/// opposite direction and is a candidate deviation of its own.
fn common_name_of(rdn_sequence: &[u8]) -> Option<IssuerName> {
    let mut found: Option<String> = None;
    let mut rest = rdn_sequence;

    while !rest.is_empty() {
        let (set, after_set) = read_element(rest)?;
        if set.tag != tag::SET {
            return None;
        }
        rest = after_set;

        let mut attributes = set.contents;
        while !attributes.is_empty() {
            let (attribute, after_attribute) = read_element(attributes)?;
            if attribute.tag != tag::SEQUENCE {
                return None;
            }
            attributes = after_attribute;

            let (oid, after_oid) = read_element(attribute.contents)?;
            if oid.tag != tag::OID {
                return None;
            }
            let (value, _) = read_element(after_oid)?;
            if oid.contents != COMMON_NAME_OID {
                continue;
            }
            found = Some(directory_string(value.tag, value.contents)?);
        }
    }

    Some(match found {
        Some(name) => IssuerName::CommonName(name),
        None => IssuerName::WithoutCommonName,
    })
}

/// One `commonName` value, decoded the way `parseASN1String` decodes it
/// (`crypto/x509/parser.go:60-108`), for the four string types a robot
/// certificate can carry it in.
///
/// `PrintableString` and `IA5String` carry Go's own character checks, so a
/// value Go refuses is refused here; refusing it ends the walk, which is the
/// deviation-32 path and is where Go panics. `UTF8String` must be valid UTF-8,
/// which Go also demands.
///
/// `T61String` is where this is stricter than Go. Go hands the bytes back
/// unexamined and a Go string may hold any bytes, where a Rust `String` may
/// not, so a `T61String` common name that is not valid UTF-8 ends the walk
/// here and produces a robot name of mojibake there. That is a candidate
/// deviation. Every Vector session certificate observed spells its issuer
/// common name in `UTF8String`.
///
/// `BMPString` and `NumericString`, which Go also accepts, are not decoded: the
/// first needs UTF-16 and the second cannot hold a `Vector-XXXX` name. Both are
/// the candidate deviation [`common_name_of`] names.
fn directory_string(tag: u8, contents: &[u8]) -> Option<String> {
    match tag {
        // `parser.go:71-75`.
        tag::UTF8_STRING => String::from_utf8(contents.to_vec()).ok(),
        // `parser.go:64-70`.
        tag::PRINTABLE_STRING => {
            if !contents.iter().copied().all(is_printable) {
                return None;
            }
            String::from_utf8(contents.to_vec()).ok()
        }
        // `parser.go:93-98`, whose `isIA5String` refuses anything above
        // U+007F (`x509.go:1169-1178`).
        tag::IA5_STRING => {
            if !contents.is_ascii() {
                return None;
            }
            String::from_utf8(contents.to_vec()).ok()
        }
        // `parser.go:62-63`.
        tag::T61_STRING => String::from_utf8(contents.to_vec()).ok(),
        _ => None,
    }
}

/// Go's `isPrintable` (`crypto/x509/parser.go:35-54`), including the two
/// characters its comments admit are not allowed in a `PrintableString` and
/// that it permits anyway.
fn is_printable(b: u8) -> bool {
    b.is_ascii_lowercase()
        || b.is_ascii_uppercase()
        || b.is_ascii_digit()
        || (b'\''..=b')').contains(&b)
        || (b'+'..=b'/').contains(&b)
        || b == b' '
        || b == b':'
        || b == b'='
        || b == b'?'
        || b == b'*'
        || b == b'&'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The walk's ordering is byte order, which is the ordering `os.ReadDir`
    /// promises (`os/dir.go:126-128`).
    ///
    /// It is tested here rather than through a directory because every
    /// filesystem this repository is built on happens to answer in name order
    /// already, so a directory cannot tell a sorted walk from an unsorted one.
    /// The rows that matter are the ones where byte order and any other
    /// plausible order disagree: an uppercase letter sorts before every
    /// lowercase one, and a digit before both.
    #[test]
    fn entry_names_are_sorted_in_byte_order() {
        // (as the filesystem answered, as the walk reads them)
        let table: &[(&[&str], &[&str])] = &[
            (&["b", "a", "C"], &["C", "a", "b"]),
            (&["a", "B"], &["B", "a"]),
            (
                &["placeholder", "00000002", "00000001"],
                &["00000001", "00000002", "placeholder"],
            ),
            (
                &["Vector-b", "Vector-A", "0"],
                &["0", "Vector-A", "Vector-b"],
            ),
            (&[], &[]),
        ];

        for (answered, ordered) in table {
            let mut names: Vec<String> = answered.iter().map(|name| (*name).to_owned()).collect();
            sort_entry_names(&mut names);
            assert_eq!(names, *ordered, "{answered:?} was not put in byte order");
        }
    }
}
