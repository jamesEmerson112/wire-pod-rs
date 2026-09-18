//! Token hashing and GUID generation: the port of
//! `pkg/servers/token/hashing.go`, whose own comment at `hashing.go:14` says it
//! is mostly copied from vector-cloud.
//!
//! This is the parity gate the phase rests on. The `vic.AppTokens` jdoc already
//! sitting in the live data directory holds a hash Go produced, and the robot
//! holds the matching GUID in its own token store. Nothing re-issues either at
//! cutover, so if this module does not reproduce Go's algorithm exactly the
//! existing association is lost and the robot has to be onboarded again.
//! `tests/token_hash.rs` carries an `#[ignore]` test that checks the live pair
//! on this machine.
//!
//! The algorithm has no HMAC, no iteration count and no key derivation. The
//! GUID is sixteen CSPRNG bytes in standard base64. The stored hash is one
//! SHA-256 pass over the sixteen token bytes followed by sixteen fresh salt
//! bytes, then the standard base64 of that digest followed by the salt in the
//! clear: forty eight raw bytes, sixty four characters (`hashing.go:47-71`,
//! `hashing.go:130-136`). Keeping the salt beside the digest is what lets
//! [`compare_hash_and_token`] re-hash a presented GUID later.
//!
//! Every expectation is recorded rather than written by hand. The `hash`
//! section of `docs/phases/P1-robot-connect-auth/go-probe/expected.txt` is the
//! stdout of the Go program committed beside it, and `tests/token_hash.rs`
//! drives this module from it.
//!
//! Three of `hashing.go`'s items are deliberately not here. `DecodeAndCompare`
//! (`hashing.go:73-86`) is not ported, because a grep over the whole Go
//! checkout finds nothing that calls it, and it is in turn the only caller of
//! `CompareHashAndToken` (`:89`) in this module, so the compare path is dead in
//! the running Go server. The two live calls at
//! `vector-cloud/gateway/tokens.go:110` and `:118` reach a separate module's
//! own copy (`vector-cloud/internal/token/compare_and_hash.go:33`), which runs
//! on the robot rather than here. The live gate in `tests/token_hash.rs` is
//! therefore the only thing that exercises this port's copy.
//! `ClientToken` (`:36-41`) and `ClientTokenManager` (`:43-45`) are the shape
//! `WriteTokenHash` marshals into the `vic.AppTokens` document (`token.go:102`,
//! `token.go:109-115`), so they went to the commit that ported that function
//! and live in [`crate::token::jwt`] beside it. `tests/token_hash.rs` reads the
//! live document through those rather than through a copy of its own.

use std::cmp::Ordering;
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq as _;

/// The number of random bytes behind a GUID (`hashing.go:17`).
pub const TOKEN_SIZE: usize = 16;

/// The number of random salt bytes stored beside a digest (`hashing.go:18`).
pub const SALT_SIZE: usize = 16;

/// The digest length. Go spells it `sha256.Size` (`hashing.go:19`).
pub const HASH_SIZE: usize = 32;

/// The length of a stored hash before base64: digest then salt
/// (`hashing.go:64-65`, `hashing.go:117`).
pub const HASHED_RAW_LEN: usize = HASH_SIZE + SALT_SIZE;

/// The length of an encoded GUID. Derived rather than written down, so that a
/// wrong [`TOKEN_SIZE`] fails the probe's recorded constant instead of hiding
/// behind a matching literal.
pub const GUID_B64_LEN: usize = base64_len(TOKEN_SIZE);

/// The length of an encoded stored hash, derived the same way.
pub const HASHED_B64_LEN: usize = base64_len(HASHED_RAW_LEN);

/// The length of `n` bytes in padded base64: four characters for every
/// three-byte group, the last group padded out.
const fn base64_len(n: usize) -> usize {
    n.div_ceil(3) * 4
}

/// What Go's token hashing returns in its `error` slot.
///
/// The five string constants at `hashing.go:21-25` are reproduced verbatim by
/// `Display`, because they are what the server logs, and the probe records
/// them. Go declares `errTokenTooLong` and `errTokenTooShort` and never returns
/// them; they survive from vector-cloud, where the token itself was
/// length-checked. They are kept here so the recorded constant table has
/// something to compare against, and so a later commit that grows a length
/// check spells the message the way Go would have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenHashError {
    /// The presented token does not hash to the stored digest
    /// (`hashing.go:113`).
    Mismatch,
    /// The decoded hash is longer than digest plus salt (`hashing.go:117-118`).
    HashTooLong,
    /// The decoded hash is shorter than digest plus salt
    /// (`hashing.go:119-120`).
    HashTooShort,
    /// Declared by Go and never returned (`hashing.go:24`).
    TokenTooLong,
    /// Declared by Go and never returned (`hashing.go:25`).
    TokenTooShort,
    /// Either argument was not valid standard base64 (`hashing.go:91-98`).
    /// `at` is the offending input byte, which is what Go's
    /// `base64.CorruptInputError` prints, subject to the second caveat below.
    ///
    /// Two differences from Go live here, and neither can be reached by a GUID
    /// or a hash that Go's own encoder wrote.
    ///
    /// The first is strictness, and it runs the opposite way from hardening.
    /// Go's `StdEncoding` is not strict: it skips carriage returns and newlines
    /// anywhere in its input (Go's `encoding/base64/base64.go:340-342`,
    /// `:357-359`) and discards non-zero bits left over in a padded final
    /// quantum, because the check that would reject them is guarded by
    /// `enc.strict` (`base64.go:394-396`, `:401-403`). Go therefore decodes a
    /// non-canonical spelling of a valid GUID back to that GUID's bytes and
    /// answers `nil`, where the `base64` crate rejects the spelling and this
    /// port answers `Decode`.
    ///
    /// So the port can turn one of Go's successful verifications into this
    /// error, and that is the direction worth stating: a GUID Go would have
    /// authenticated is one this port refuses. It cannot go the other way. The
    /// four places the two decoders part company over whether to accept an
    /// input at all (`base64.go:340-342` and `:357-359`, `:394-396` and
    /// `:401-403`) are all places where Go lets something through that the
    /// crate does not, so nothing this port accepts is something Go rejected.
    /// Nothing stored is affected either, because Go's own encoder never writes
    /// a line break and never leaves non-zero bits in a final quantum, so every
    /// GUID and hash already on disk decodes identically under both. That makes
    /// this hardening rather than a live regression, but it is still a
    /// difference, and it is a candidate numbered deviation for C23 to record in
    /// `docs/phases/P4-sdk-app/deviations.md`. The two inputs that reproduce it
    /// are the recording's own sixteen-byte token vector respelled with
    /// non-zero bits left in its final quantum, and the same vector with a
    /// newline appended.
    ///
    /// The second difference is `at` itself. It agrees with Go whenever the
    /// fault is a symbol outside the alphabet, wherever in the input that
    /// symbol sits: the crate's `InvalidByte` offset and Go's
    /// `CorruptInputError(si - 1)` (`base64.go:346`) are the same number, and
    /// that is the shape both recorded cases take. It disagrees on every other
    /// shape, because the crate's three remaining errors do not carry a Go
    /// offset to begin with. `InvalidLength` counts valid symbols rather than
    /// naming a position (`base64-0.22.1/src/decode.rs:19-21`), so a five
    /// symbol input reports one more than Go does. `InvalidPadding` carries no
    /// position at all and is reported here as zero, where Go names the offset
    /// it stopped at: for a six symbol input and for one missing its padding
    /// character Go answers 4, the start of the quantum it could not finish
    /// (`base64.go:320-327`), and `tests/token_hash.rs` pins both.
    /// `InvalidLastSymbol` is the non-canonical trailing bits above, where Go
    /// does not fault at all and so has no offset to compare. Nothing branches
    /// on the number: every caller of [`compare_hash_and_token`] logs the error
    /// and goes no further.
    Decode {
        /// The input byte Go's error names.
        at: usize,
    },
    /// The OS random source refused. Go gets this from `crypto/rand`
    /// (`hashing.go:50-53`, `hashing.go:58-61`) and every call site discards it
    /// with `_` (`jdocs/server.go:135`, `token.go:225`, `token.go:235`).
    ///
    /// Go's underscore is safe there in a way the port's would not be here.
    /// Modern Go documents `crypto/rand.Read` as never returning an error and
    /// as crashing the program irrecoverably if the system source fails (Go's
    /// `crypto/rand/rand.go:60-66`, read from the 1.24.4 toolchain installed
    /// here), and the server's module asks for Go 1.25 or newer
    /// (`chipper/go.mod:3`), so that is the behaviour it gets: the discarded
    /// slot is always nil and the Go server never continues past a failed draw.
    /// Here the variant is a real outcome, and a caller that mirrored Go by
    /// dropping it would hand the robot an empty GUID and write an empty hash
    /// into `vic.AppTokens`.
    /// The commits that port `WriteTokenHash` and the token server must treat
    /// it as fatal rather than discard it.
    Random(getrandom::Error),
}

impl fmt::Display for TokenHashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mismatch => f.write_str("hash mismatch"),
            Self::HashTooLong => f.write_str("hash too long"),
            Self::HashTooShort => f.write_str("hash too short"),
            Self::TokenTooLong => f.write_str("token too long"),
            Self::TokenTooShort => f.write_str("token too short"),
            Self::Decode { at } => write!(f, "illegal base64 data at input byte {at}"),
            Self::Random(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for TokenHashError {}

/// What Go's `CreateTokenAndHashedToken` returns in its two string slots
/// (`hashing.go:47`).
///
/// The GUID goes to the robot and is never stored by the server. The hash goes
/// into the `vic.AppTokens` jdoc and is never enough to recover the GUID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenPair {
    /// The bearer token the robot keeps: standard base64 of [`TOKEN_SIZE`]
    /// random bytes, [`GUID_B64_LEN`] characters.
    pub guid: String,
    /// What the server stores: standard base64 of digest then salt,
    /// [`HASHED_B64_LEN`] characters.
    pub guid_hash: String,
}

/// A stored hash split into its two halves, Go's unexported `hashed`
/// (`hashing.go:28-34`).
///
/// Borrowed rather than owned because Go's `newFromHash` slices its input
/// rather than copying it (`hashing.go:123`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hashed<'a> {
    /// The digest on its own, [`HASH_SIZE`] bytes.
    pub hash: &'a [u8],
    /// The salt the digest was taken with, [`SALT_SIZE`] bytes.
    pub salt: &'a [u8],
}

/// One SHA-256 pass over `token` followed by `salt`, which is Go's unexported
/// `hash` (`hashing.go:130-136`).
///
/// Renamed because a bare `hash` re-exported from the crate root would say
/// nothing about what it hashes. The concatenation order is load-bearing: salt
/// first would produce a different digest for every robot already associated,
/// and the probe's recorded vectors are what pin it.
///
/// Neither argument is length-checked, matching Go, because
/// [`compare_hash_and_token`] feeds it whatever a presented GUID decoded to.
pub fn hash_token(token: &[u8], salt: &[u8]) -> [u8; HASH_SIZE] {
    let mut digest = Sha256::new();
    digest.update(token);
    digest.update(salt);
    digest.finalize().into()
}

/// The deterministic part of Go's `CreateTokenAndHashedToken`, with its two
/// `rand.Read` calls lifted out (`hashing.go:54-70`). The cited range still
/// straddles the second draw, because Go encodes the GUID at `:54` before
/// drawing the salt at `:58`.
///
/// Split out from [`create_token_and_hashed_token`] so that the probe's
/// recorded vectors, which fix both the token and the salt, drive exactly the
/// code the server runs.
pub fn encode_token_and_hash(token: &[u8; TOKEN_SIZE], salt: &[u8; SALT_SIZE]) -> TokenPair {
    let mut raw = [0u8; HASHED_RAW_LEN];
    raw[..HASH_SIZE].copy_from_slice(&hash_token(token, salt));
    raw[HASH_SIZE..].copy_from_slice(salt);
    TokenPair {
        guid: STANDARD.encode(token),
        guid_hash: STANDARD.encode(raw),
    }
}

/// Draws a fresh GUID and its stored hash, reproducing
/// `CreateTokenAndHashedToken` (`hashing.go:47-71`).
///
/// The token bytes are drawn before the salt bytes, as in Go, so a seeded
/// source would hand both implementations the same pair. `crypto/rand` is the
/// OS source, which `getrandom` is here.
///
/// The two draws are independent, and that matters more than it looks. The
/// salt travels in the clear in the second half of the stored hash
/// (`hashing.go:64-65`), so a build whose salt came from the token bytes would
/// still verify against itself, still differ from the next generation, and
/// would publish every robot's GUID inside `jdocs.json`. The probe's vectors
/// cannot see that, because they fix both inputs and never reach a draw, so
/// `tests/token_hash.rs` pins the independence directly.
pub fn create_token_and_hashed_token() -> Result<TokenPair, TokenHashError> {
    let mut token = [0u8; TOKEN_SIZE];
    getrandom::getrandom(&mut token).map_err(TokenHashError::Random)?;
    let mut salt = [0u8; SALT_SIZE];
    getrandom::getrandom(&mut salt).map_err(TokenHashError::Random)?;
    Ok(encode_token_and_hash(&token, &salt))
}

/// Splits a decoded stored hash into digest and salt, reproducing
/// `newFromHash` (`hashing.go:116-128`).
///
/// Go tests the two length errors in that order, longer first, and the probe
/// records both at the boundary lengths. A `match` on the ordering rather than
/// Go's `if`/`else if` keeps the same three outcomes without tripping
/// `clippy::comparison_chain`.
pub fn new_from_hash(hashed_token: &[u8]) -> Result<Hashed<'_>, TokenHashError> {
    match hashed_token.len().cmp(&HASHED_RAW_LEN) {
        Ordering::Greater => return Err(TokenHashError::HashTooLong),
        Ordering::Less => return Err(TokenHashError::HashTooShort),
        Ordering::Equal => {}
    }
    let (hash, salt) = hashed_token.split_at(HASH_SIZE);
    Ok(Hashed { hash, salt })
}

/// Checks a presented GUID against a stored hash, reproducing
/// `CompareHashAndToken` (`hashing.go:89-114`).
///
/// Both arguments are decoded, the stored hash is split, the presented token is
/// re-hashed with the salt that came out of the split, and the two digests are
/// compared in constant time. The token is never length-checked, which is why
/// the probe's short and long token cases answer `hash mismatch` rather than a
/// length error.
///
/// The constant-time compare is Go's `subtle.ConstantTimeCompare`
/// (`hashing.go:109`). It cannot change any answer this function gives, so no
/// test can detect its loss; it is here because comparing a secret with `==`
/// leaks how many leading bytes of a guess were right.
pub fn compare_hash_and_token(hashed_token: &str, token: &str) -> Result<(), TokenHashError> {
    let hashed_bytes = decode_std(hashed_token)?;
    let token_bytes = decode_std(token)?;

    let hashed = new_from_hash(&hashed_bytes)?;
    let new_hash = hash_token(&token_bytes, hashed.salt);

    if bool::from(hashed.hash.ct_eq(&new_hash[..])) {
        return Ok(());
    }
    Err(TokenHashError::Mismatch)
}

/// `base64.StdEncoding.DecodeString` with Go's error shape (`hashing.go:91`,
/// `hashing.go:95`).
///
/// The offset carried out of here is the `base64` crate's. Only `InvalidByte`
/// and `InvalidLastSymbol` name a position at all; the crate documents
/// `InvalidLength` as a count of valid symbols rather than an offset
/// (`base64-0.22.1/src/decode.rs:19-21`) and `InvalidPadding` carries nothing,
/// which is why it is reported as zero. `InvalidByte` is the one that matches
/// Go, and it is the only one a recorded case reaches;
/// [`TokenHashError::Decode`] sets out the rest.
fn decode_std(text: &str) -> Result<Vec<u8>, TokenHashError> {
    use base64::DecodeError;

    STANDARD.decode(text).map_err(|error| {
        let at = match error {
            DecodeError::InvalidByte(at, _) | DecodeError::InvalidLastSymbol(at, _) => at,
            DecodeError::InvalidLength(at) => at,
            DecodeError::InvalidPadding => 0,
        };
        TokenHashError::Decode { at }
    })
}
