//! The token server's own state. Today that is the hashing and GUID generation
//! the robot's association depends on and the three transient stores the token
//! and jdocs servers share; the JWT arrives in a later commit.

pub mod hash;
pub mod stores;

pub use crate::token::hash::{
    GUID_B64_LEN, HASH_SIZE, HASHED_B64_LEN, HASHED_RAW_LEN, Hashed, SALT_SIZE, TOKEN_SIZE,
    TokenHashError, TokenPair, compare_hash_and_token, create_token_and_hashed_token,
    encode_token_and_hash, hash_token, new_from_hash,
};
pub use crate::token::stores::{
    PrimaryEntry, PrimaryWalk, SecondaryEntry, SessionEntry, SessionMatch, TokenStores, host_of,
};
