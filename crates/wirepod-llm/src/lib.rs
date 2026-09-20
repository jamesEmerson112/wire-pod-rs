//! What Go gets from its OpenAI client library, and nothing Go wrote itself.
//!
//! Go builds no HTTP request by hand: `github.com/sashabaranov/go-openai`
//! does it. This crate is that library's part of the job, so the `kgsim`
//! modules stay a file-for-file translation of the Go the server owns.

pub mod chat;
pub mod sse;
