//! The JWT the token server hands the robot, and the hash it writes beside it:
//! the port of the two halves of `pkg/servers/token/token.go` that produce
//! bytes rather than control flow.
//!
//! `CreateJWT` (`token.go:185-270`) builds a seven-claim payload, wraps it in
//! the `golang-jwt/jwt` library's RS512 header, and hands the robot the three
//! base64url segments. `WriteTokenHash` (`token.go:99-128`) puts the stored
//! hash of the GUID that went out with it into the robot's `vic.AppTokens`
//! jdoc. Everything here is a pure function over injected inputs: the clock,
//! the token id, the requestor and the jdocs store all arrive as arguments, so
//! nothing in this module reads a global or a wall clock of its own. Go's
//! control flow around them, the transient token stores and the peer address
//! lookup, is the token server's and arrives with it.
//!
//! # The signature is not a signature
//!
//! Go generates a throwaway 1024-bit RSA key per request and signs with it
//! (`token.go:266-267`). The key is a local variable; nothing stores it,
//! publishes it or hands a peer a public half, and a grep over the whole Go
//! checkout finds `rsa.` at exactly three places, this one and the two
//! certificate generators in `pkg/wirepod/setup/certs.go`. The robot parses
//! what it is given with `ParseUnverified`
//! (`vector-cloud/internal/token/identity/identity.go:158`), which is the only
//! parse of a token anywhere in either tree, so no verifier exists on either
//! side of the wire and the signature slot is checked by nobody.
//!
//! This port therefore fills that slot with [`SIGNATURE_LEN`] CSPRNG bytes,
//! which is the length an RS512 signature over a 1024-bit key has, rather than
//! taking an RSA dependency to compute a number no peer will look at. That is
//! deviation 28, reserved in `docs/phases/P4-sdk-app/deviations.md`. The bytes
//! are drawn rather than fixed so that two tokens issued in the same second
//! still differ, which is the one observable property Go's signature has.
//!
//! # Where the bytes come from
//!
//! The header, the claim payload, both base64url segments, the key order and
//! the two hard-coded literals are recorded by the `claims` section of
//! `docs/phases/P1-robot-connect-auth/go-probe/expected.txt`, which is the
//! stdout of a Go program built against `github.com/golang-jwt/jwt` v3.2.2,
//! the version the Go server pins. `crates/wirepod-core/tests/jwt.rs` drives
//! this module from those lines rather than from anything written by hand.
//! The `vic.AppTokens` document's bytes come from a throwaway Go program built
//! from the two structs at `hashing.go:36-45`, transcribed into that test.

use std::fmt;
use std::io;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::esn::Esn;
use crate::gojson::{Extra, go_marshal};
use crate::store::jdocs::{Jdoc, JdocsStore};
use crate::timefmt::{add_months, rfc3339_nano};
use crate::wallclock::WallClock;

// ---------------------------------------------------------------------------
// The literals Go hard codes
// ---------------------------------------------------------------------------

/// Go's `UserId` (`token.go:31`), the `user_id` claim.
pub const USER_ID: &str = "wirepod";

/// The `token_type` claim (`token.go:263`).
pub const TOKEN_TYPE: &str = "user+robot";

/// The `requestor_id` a token carries when the ESN behind the request is not
/// known (`token.go:187`).
///
/// Go's comment at `token.go:258-260` says why it is a serial at all: the
/// official servers read the robot's factory certificate and wire-pod cannot,
/// so the first token a robot is ever handed claims this fixed serial and
/// every later one claims the robot's own.
pub const DEFAULT_REQUESTOR_ID: &str = "vic:00601b50";

/// The JWT header the `golang-jwt/jwt` library writes for
/// `jwt.SigningMethodRS512` (`token.go:254`).
///
/// `jwt.NewWithClaims` fills `Header` with `typ` and `alg` and `SigningString`
/// marshals that map, so the key order is Go's map encoder's and therefore
/// alphabetical. These are the bytes the probe's `kind=header` case recorded,
/// not a spelling chosen here.
pub const HEADER: &[u8] = br#"{"alg":"RS512","typ":"JWT"}"#;

/// The signing method's name, as the header spells it and as
/// `jwt.SigningMethodRS512.Alg()` answers.
pub const ALG: &str = "RS512";

/// The length of the signature slot: an RS512 signature over the 1024-bit key
/// Go generates at `token.go:266` is 128 bytes.
///
/// See the module doc for why these bytes are drawn rather than computed.
pub const SIGNATURE_LEN: usize = 128;

/// The `client_name` on every stored client token (`token.go:111`).
pub const CLIENT_NAME: &str = "wirepod";

/// The `app_id` on every stored client token (`token.go:113`).
pub const APP_ID: &str = "SDK";

/// The jdoc `WriteTokenHash` reads and writes (`token.go:101`, `token.go:125`).
pub const APP_TOKENS_DOC: &str = "vic.AppTokens";

/// The `client_metadata` a freshly created `vic.AppTokens` document carries
/// (`token.go:106`).
pub const NEW_TOKEN_METADATA: &str = "wirepod-new-token";

/// The `doc_version` and `fmt_version` a freshly created `vic.AppTokens`
/// document carries (`token.go:104-105`).
///
/// One constant for both because Go writes the same literal twice, and because
/// the document never accumulates: see [`write_token_hash`].
pub const NEW_TOKEN_VERSION: u64 = 1;

// ---------------------------------------------------------------------------
// The one thing that can fail
// ---------------------------------------------------------------------------

/// The OS random source refused, drawing either a token id or a signature.
///
/// Go draws neither through a fallible path it looks at: `uuid.New`
/// (`token.go:181`) panics on a failed read and `rsa.GenerateKey`'s error is
/// discarded with `_` (`token.go:266`). Modern Go documents `crypto/rand` as
/// never returning an error and as crashing the program irrecoverably if the
/// system source fails, so the discarded slot there is always nil. Here the
/// failure is a real outcome, and a caller that dropped it would hand the
/// robot a token with an empty `token_id` or an empty signature segment.
///
/// This is the same argument
/// [`TokenHashError::Random`](crate::token::hash::TokenHashError::Random)
/// makes for the GUID draw, in a separate type because none of that enum's
/// other five variants can happen here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RandomError(pub getrandom::Error);

impl fmt::Display for RandomError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::error::Error for RandomError {}

// ---------------------------------------------------------------------------
// The claims
// ---------------------------------------------------------------------------

/// Who a token is issued to, as `CreateJWT` decides it.
///
/// Go starts at the default (`token.go:187`) and overwrites it with `vic:` plus
/// the serial once the peer address has been matched to a robot
/// (`token.go:222`). The two arms are that decision, lifted out of the control
/// flow so this module never touches a peer address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Requestor {
    /// The ESN behind the request is not known, so the token claims
    /// [`DEFAULT_REQUESTOR_ID`] (`token.go:187`).
    Unknown,
    /// The ESN is known, so the token claims `vic:` plus the serial
    /// (`token.go:222`).
    Robot(Esn),
}

impl Requestor {
    /// The `requestor_id` claim this requestor produces.
    pub fn id(&self) -> String {
        match self {
            Self::Unknown => DEFAULT_REQUESTOR_ID.to_owned(),
            Self::Robot(esn) => format!("vic:{esn}"),
        }
    }
}

/// Go's `jwt.MapClaims` as `CreateJWT` fills it (`token.go:254-265`).
///
/// The field order is the order the bytes come out in, and it is not this
/// file's choice: `jwt.MapClaims` is a `map[string]interface{}`, so Go's map
/// encoder sorts the keys with `strings.Compare`
/// (`encoding/json/encode.go:745-775`), which is byte order over the raw key
/// strings. The seven names happen to sort the same way they are written at
/// `token.go:255-264`, and the probe records the order as a case of its own so
/// that a reordering here fails a test rather than quietly changing the
/// payload.
///
/// `token_type` and `user_id` are `&'static str` because Go hard codes both and
/// a token that claimed anything else would not be this server's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Claims {
    /// One calendar month after `iat`, formatted with `time.RFC3339Nano`
    /// (`token.go:196`).
    pub expires: String,
    /// When the token was issued, formatted with `time.RFC3339Nano`
    /// (`token.go:195`).
    pub iat: String,
    /// Go writes an untyped `nil` here (`token.go:257`), which marshals to
    /// `null`. The unit type is what keeps that the only thing this field can
    /// be.
    pub permissions: (),
    /// [`Requestor::id`] (`token.go:261`).
    pub requestor_id: String,
    /// A UUID v4, one per token request (`token.go:250`, `token.go:262`).
    pub token_id: String,
    /// [`TOKEN_TYPE`] (`token.go:263`).
    pub token_type: &'static str,
    /// [`USER_ID`] (`token.go:264`).
    pub user_id: &'static str,
}

impl Claims {
    /// The claim set for one token request, reading `clock` the way
    /// `CreateJWT` reads `time.Now` (`token.go:195-196`).
    ///
    /// The clock is read **twice**, once per claim, because Go reads it twice:
    /// `currentTime` and `expiresAt` come from two separate `time.Now()` calls
    /// a few hundred nanoseconds apart, so the two claims carry different
    /// sub-second fractions. Nothing downstream can observe that, but the
    /// payload is a byte contract and a second call is what it costs to match
    /// it.
    ///
    /// Each claim's UTC offset is resolved at its own instant rather than once
    /// for both. The two are a calendar month apart and can sit on opposite
    /// sides of a daylight saving transition, which is the rule the probe's
    /// `addmonth_local` section records; for roughly two months a year
    /// `expires` carries a different offset from `iat`.
    ///
    /// `token_id` is injected rather than drawn here so that a test can pin the
    /// payload byte for byte. [`generate_token_id`] is what the server passes.
    pub fn new(requestor: &Requestor, token_id: impl Into<String>, clock: &dyn WallClock) -> Self {
        // `token.go:195`.
        let issued = clock.now();
        let iat = rfc3339_nano(issued, clock.utc_offset_secs_at(issued.unix_secs));
        // `token.go:196`, on a second reading of the clock.
        let expiry = add_months(clock.now(), clock);
        let expires = rfc3339_nano(expiry, clock.utc_offset_secs_at(expiry.unix_secs));

        Self {
            expires,
            iat,
            permissions: (),
            requestor_id: requestor.id(),
            token_id: token_id.into(),
            token_type: TOKEN_TYPE,
            user_id: USER_ID,
        }
    }
}

/// The claim payload, as `SigningString` marshals it before encoding
/// (`golang-jwt/jwt@v3.2.2/token.go:65-83`).
///
/// # Panics
///
/// Never. Every field is a string or the unit type, serialising into a [`Vec`]
/// cannot fail at the writer, and there is no number to be a NaN.
pub fn marshal_claims(claims: &Claims) -> Vec<u8> {
    go_marshal(claims).expect("a claim set holds nothing unserialisable")
}

// ---------------------------------------------------------------------------
// The token id
// ---------------------------------------------------------------------------

/// The lowercase hex alphabet a UUID is written in.
const HEX: [u8; 16] = *b"0123456789abcdef";

/// Formats sixteen random bytes as a UUID v4, which is what Go's `uuid.New`
/// plus `String` produces (`token.go:180-183`,
/// `github.com/google/uuid`'s `NewRandom` and `Version 4, Variant RFC4122`).
///
/// Two of the sixteen bytes are overwritten rather than used as drawn: the high
/// nibble of byte 6 becomes `4`, naming the version, and the top two bits of
/// byte 8 become `10`, naming the RFC 4122 variant. The rest is the draw. That
/// leaves 122 random bits, which is what makes the id unique in practice and
/// what the probe's placeholder `00000000-0000-4000-8000-000000000000` is
/// shaped like.
///
/// Split out from [`generate_token_id`] so a test can pin the two fixed nibbles
/// against a known input instead of against a draw.
pub fn uuid_v4(random: &[u8; 16]) -> String {
    let mut bytes = *random;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    let mut out = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// Draws a fresh `token_id`, reproducing `GenerateUUID` (`token.go:180-183`).
///
/// # Errors
///
/// [`RandomError`] when the OS random source refuses. Go's `uuid.New` panics
/// there instead; either way nothing continues with a token id that is not
/// random.
pub fn generate_token_id() -> Result<String, RandomError> {
    let mut random = [0u8; 16];
    getrandom::getrandom(&mut random).map_err(RandomError)?;
    Ok(uuid_v4(&random))
}

// ---------------------------------------------------------------------------
// The three segments
// ---------------------------------------------------------------------------

/// One JWT segment, as the library's `EncodeSegment` writes it
/// (`golang-jwt/jwt@v3.2.2/token.go:97-99`).
///
/// That function is `base64.RawURLEncoding.EncodeToString`: the URL alphabet,
/// so `+` and `/` become `-` and `_`, and no padding, so no `=` ever reaches a
/// token. Both halves matter on the wire, since a segment travels in an
/// `Authorization` header and in a file name on the robot.
pub fn encode_segment(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The two encoded segments a signature would be taken over, joined by a dot
/// (`golang-jwt/jwt@v3.2.2/token.go:65-83`).
///
/// Nothing signs it here, for the reason the module doc gives; it exists
/// because the probe records it as a case and because it is the half of the
/// token that carries meaning.
pub fn signing_input(header: &[u8], claims: &[u8]) -> String {
    format!("{}.{}", encode_segment(header), encode_segment(claims))
}

/// The whole token: three base64url segments joined by dots.
pub fn encode(header: &[u8], claims: &[u8], signature: &[u8]) -> String {
    format!(
        "{}.{}",
        signing_input(header, claims),
        encode_segment(signature)
    )
}

/// Draws the bytes that go in the signature slot.
///
/// # Errors
///
/// [`RandomError`] when the OS random source refuses. See the module doc for
/// why these are drawn rather than computed, and [`RandomError`] for why the
/// failure is not discarded the way Go discards `rsa.GenerateKey`'s.
pub fn random_signature() -> Result<[u8; SIGNATURE_LEN], RandomError> {
    let mut signature = [0u8; SIGNATURE_LEN];
    getrandom::getrandom(&mut signature).map_err(RandomError)?;
    Ok(signature)
}

/// Marshals `claims`, draws a signature and encodes all three segments, which
/// is `token.go:254-267` with the key generation replaced.
///
/// # Errors
///
/// [`RandomError`] when the signature draw fails.
pub fn issue_token(claims: &Claims) -> Result<String, RandomError> {
    let signature = random_signature()?;
    Ok(encode(HEADER, &marshal_claims(claims), &signature))
}

/// What `CreateJWT` returns, in domain types (`token.go:189`, `token.go:268`).
///
/// `tokenpb.TokenBundle` has three fields (`proto/token/token.proto:86-91`) and
/// Go sets two of them: `token` at `token.go:268` and `client_token` at
/// `token.go:244` or `token.go:247`. `sts_token` is never touched, so there is
/// no slot for it here. The second field is named for what it holds rather than
/// for the wire: the value is the GUID from
/// [`create_token_and_hashed_token`](crate::token::hash::create_token_and_hashed_token),
/// or Go's `GlobalGUID` when no GUID was issued.
///
/// This crate depends on neither `wirepod-proto` nor tonic, so the conversion
/// to the generated message belongs to the crate that owns the token service.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenBundle {
    /// The three encoded segments, `tokenpb.TokenBundle.token`.
    pub token: String,
    /// The GUID the robot keeps, `tokenpb.TokenBundle.client_token`.
    pub client_token_guid: String,
}

// ---------------------------------------------------------------------------
// The stored hash
// ---------------------------------------------------------------------------

/// Go's `ClientToken` (`hashing.go:36-41`), one entry in the `vic.AppTokens`
/// document.
///
/// Field order is Go's declaration order, which is what `encoding/json`
/// marshals in and therefore the order of the bytes inside `json_doc`. None of
/// the four tags carries `omitempty`, so all four are always written, an empty
/// one as `""`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientToken {
    /// `hashing.go:37`: the stored hash of the GUID, which is
    /// [`TokenPair::guid_hash`](crate::token::hash::TokenPair::guid_hash).
    #[serde(default)]
    pub hash: String,
    /// `hashing.go:38`: always [`CLIENT_NAME`] (`token.go:111`).
    #[serde(default)]
    pub client_name: String,
    /// `hashing.go:39`: always [`APP_ID`] (`token.go:113`).
    #[serde(default)]
    pub app_id: String,
    /// `hashing.go:40`: when the token was issued, `time.RFC3339Nano` in local
    /// time (`token.go:110`).
    #[serde(default)]
    pub issued_at: String,
    /// Keys inside one client token this struct does not name, preserved across
    /// a round trip. Go drops them.
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// Go's `ClientTokenManager` (`hashing.go:43-45`), the whole `vic.AppTokens`
/// document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientTokenManager {
    /// `hashing.go:44`: every client token the robot has been issued, oldest
    /// first. In the running Go server this list is always exactly one long,
    /// for the reason [`write_token_hash`] sets out.
    #[serde(default)]
    pub client_tokens: Vec<ClientToken>,
    /// Keys inside the document this struct does not name, preserved across a
    /// round trip. Go drops them.
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// The `vic.AppTokens` document's bytes, as `json.Marshal(tokenJson)` writes
/// them (`token.go:115`).
///
/// A [`String`] rather than a byte vector because the result goes straight into
/// [`Jdoc::json_doc`], which is JSON text inside a JSON string.
///
/// # Panics
///
/// Never, for the reason [`marshal_claims`] gives, and because a
/// [`serde_json::Value`] in an [`Extra`] map cannot hold a NaN. Go discards its
/// own marshal error at this site and then stores the nil bytes as an empty
/// `json_doc` anyway (`token.go:115-119`), which would empty the document.
pub fn marshal_client_tokens(manager: &ClientTokenManager) -> String {
    let bytes = go_marshal(manager).expect("a client token list holds nothing unserialisable");
    String::from_utf8(bytes).expect("`go_marshal` writes UTF-8")
}

/// Go's `WriteTokenHash` (`token.go:99-128`): appends one client token to the
/// robot's `vic.AppTokens` jdoc and rewrites the file.
///
/// # The document never accumulates
///
/// Go looks the document up under the **bare** serial (`token.go:101`) and
/// stores it under `vic:` plus the serial (`token.go:125`). Nothing else ever
/// writes a jdoc under a bare serial, so the lookup never finds what a previous
/// call wrote: every call starts from the blank document, sets `doc_version` and
/// `fmt_version` to [`NEW_TOKEN_VERSION`] and `client_metadata` to
/// [`NEW_TOKEN_METADATA`] (`token.go:103-107`), unmarshals an empty string into
/// an empty manager (`token.go:108`), and appends its one token
/// (`token.go:114`). The document the robot ends up with therefore holds
/// exactly one client token and stays at version 1 however many times the robot
/// authenticates.
///
/// That is reproduced rather than fixed, and the two spellings are why
/// [`JdocsStore::get_jdoc`] takes a bare `&str`: normalising the lookup would
/// make the document accumulate, which changes a file the Go server reads back.
/// It is also why the `if jdocExists` arm below cannot be reached from the
/// running server, and why this port's decode of an existing `json_doc` is
/// dead code in the same way `CompareHashAndToken` is.
///
/// # The clock
///
/// Read once, at `token.go:110`, and formatted with `time.RFC3339Nano` in local
/// time (`token.go:29`), so `issued_at` carries the offset in effect when the
/// robot authenticated.
///
/// # Errors
///
/// The rewrite's, where Go discards `os.WriteFile`'s result inside `WriteJdocs`
/// (`vars.go:317`) and returns nil unconditionally. Go also calls `WriteJdocs`
/// a second time at `token.go:126`; [`JdocsStore::add_jdoc`] has already
/// written by then and its documentation says why the second write buys
/// nothing here.
pub async fn write_token_hash(
    jdocs: &JdocsStore,
    esn: &str,
    token_hash: &str,
    clock: &dyn WallClock,
) -> io::Result<()> {
    // `token.go:101`, under the bare serial, and `token.go:103-107` filling the
    // blank document the miss hands back.
    let existing = jdocs.get_jdoc(esn, APP_TOKENS_DOC);
    let (doc_version, fmt_version, client_metadata, json_doc) = match existing {
        Some(jdoc) => (
            jdoc.doc_version,
            jdoc.fmt_version,
            jdoc.client_metadata,
            jdoc.json_doc,
        ),
        None => (
            NEW_TOKEN_VERSION,
            NEW_TOKEN_VERSION,
            NEW_TOKEN_METADATA.to_owned(),
            String::new(),
        ),
    };

    // `token.go:108`, whose error Go discards. An empty `json_doc`, which is
    // what every reachable call has, fails to decode in both languages and
    // leaves the manager empty.
    let mut manager: ClientTokenManager = serde_json::from_str(&json_doc).unwrap_or_default();

    // `token.go:109-114`. The clock is read here, where Go reads it.
    let issued = clock.now();
    manager.client_tokens.push(ClientToken {
        hash: token_hash.to_owned(),
        client_name: CLIENT_NAME.to_owned(),
        app_id: APP_ID.to_owned(),
        issued_at: rfc3339_nano(issued, clock.utc_offset_secs_at(issued.unix_secs)),
        extra: Extra::new(),
    });

    // `token.go:115-124`. Go copies the four named fields one at a time into a
    // fresh `AJdoc`, so anything else the looked-up document carried is
    // dropped; the explicit fields below do the same.
    let jdoc = Jdoc {
        doc_version,
        fmt_version,
        client_metadata,
        json_doc: marshal_client_tokens(&manager),
        extra: Extra::new(),
    };

    // `token.go:125`, under `vic:` plus the serial. Go discards the version
    // `AddJdoc` answers with, and so does this.
    jdocs
        .add_jdoc(&format!("vic:{esn}"), APP_TOKENS_DOC, jdoc)
        .await
        .written
}
