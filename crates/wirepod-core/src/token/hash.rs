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
    /// `base64.CorruptInputError` prints.
    ///
    /// The offset agrees with Go's on every case the probe records, all of
    /// which fault on the first byte. Beyond those the two decoders disagree
    /// about malformed input in two ways that no valid GUID or hash can reach:
    /// Go skips carriage returns and newlines anywhere in the input and ignores
    /// non-zero bits left over in a padded final quantum, where the `base64`
    /// crate rejects both. Being stricter can only turn a would-be
    /// [`Self::Mismatch`] into this error, and every caller of
    /// [`compare_hash_and_token`] logs the error rather than branching on it.
    Decode {
        /// The input byte Go's error names.
        at: usize,
    },
    /// The OS random source refused. Go gets this from `crypto/rand`
    /// (`hashing.go:50-53`, `hashing.go:58-61`) and every call site discards it
    /// with `_` (`jdocs/server.go:135`, `token.go:225`, `token.go:235`).
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

/// The body of Go's `CreateTokenAndHashedToken` after its two `rand.Read`
/// calls (`hashing.go:54-70`).
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
