//! The token server's own state. Today that is the hashing and GUID generation
//! the robot's association depends on, the three transient stores the token
//! and jdocs servers share, and the JWT and stored-hash write the token server
//! hands back.

pub mod hash;
pub mod jwt;
pub mod stores;

pub use crate::token::hash::{
    GUID_B64_LEN, HASH_SIZE, HASHED_B64_LEN, HASHED_RAW_LEN, Hashed, SALT_SIZE, TOKEN_SIZE,
    TokenHashError, TokenPair, compare_hash_and_token, create_token_and_hashed_token,
    encode_token_and_hash, hash_token, new_from_hash,
};
pub use crate::token::jwt::{
    ALG, APP_ID, APP_TOKENS_DOC, CLIENT_NAME, Claims, ClientToken, ClientTokenManager,
    DEFAULT_REQUESTOR_ID, HEADER, NEW_TOKEN_METADATA, NEW_TOKEN_VERSION, RandomError, Requestor,
    SIGNATURE_LEN, TOKEN_TYPE, TokenBundle, USER_ID, encode, encode_segment, generate_token_id,
    issue_token, marshal_claims, marshal_client_tokens, random_signature, serialize_client_tokens,
    signing_input, uuid_v4, write_token_hash,
};
pub use crate::token::stores::{
    PrimaryEntry, PrimaryWalk, SecondaryEntry, SessionEntry, SessionMatch, TokenStores, host_of,
};
