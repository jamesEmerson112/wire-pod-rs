//! Ogg/Opus decode, high-pass+gain filters, VAD, SpeechRequest.

pub mod speechrequest;

pub use crate::speechrequest::{
    ChunkSource, OggStream, RequestKind, RequestParts, SpeechRequest, VadInst, bytes_to_int_vad,
    bytes_to_samples, req_to_speech_request, split_vad,
};
