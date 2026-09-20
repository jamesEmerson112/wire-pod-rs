//! SttEngine trait and engine implementations behind Cargo features.
//!
//! The trait itself lands with the engine, because Go's handler takes a
//! `SpeechRequest` by value and that type is written in `wirepod-audio` by the
//! same unit of work.

#[cfg(feature = "stt-vosk")]
pub mod vosk;
#[cfg(feature = "stt-vosk")]
pub mod vosk_context;

/// Go returns a plain `error` from every engine entry point, and calls
/// `log.Fatal` for the failures it treats as fatal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SttError {
    pub message: String,
}

impl SttError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SttError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SttError {}
