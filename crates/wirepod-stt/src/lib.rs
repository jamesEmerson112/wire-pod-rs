//! SttEngine trait and engine implementations behind Cargo features.
//!
//! The trait itself lands with the engine, because Go's handler takes a
//! `SpeechRequest` by value and that type is written in `wirepod-audio` by the
//! same unit of work.

use std::collections::HashMap;

use async_trait::async_trait;
use wirepod_audio::SpeechRequest;

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

/// One speech engine: Go's `InitFunc func() error`, its `SttHandler interface{}`
/// and the `voiceProcessor` name, which `preqs.New` takes as three arguments
/// (`preqs/server.go:41`).
///
/// Go accepts either handler shape and picks between them by asserting on the
/// function's type at run time, refusing anything else with
/// `"stthandler not of correct type"` (`preqs/server.go:60-67`). Here both
/// shapes are methods and that refusal is their default body, so an engine
/// implements the one it has and [`SttEngine::is_sti`] says which.
#[async_trait]
pub trait SttEngine: Send + Sync {
    /// Go's `voiceProcessor`.
    fn name(&self) -> &str;

    /// Go's `InitFunc`, which `preqs.New` also stores as `vars.SttInitFunc` so
    /// the web UI can call it again.
    fn init(&self) -> Result<(), SttError>;

    /// Go's `isSti`, false for every engine but Rhino.
    fn is_sti(&self) -> bool {
        false
    }

    /// Go's `sttHandler func(sr.SpeechRequest) (string, error)`.
    async fn stt(&self, req: SpeechRequest) -> Result<String, SttError> {
        let _ = req;
        Err(SttError::new("stthandler not of correct type"))
    }

    /// Go's `stiHandler func(sr.SpeechRequest) (string, map[string]string, error)`,
    /// which exists to accomodate Rhino.
    async fn sti(&self, req: SpeechRequest) -> Result<(String, HashMap<String, String>), SttError> {
        let _ = req;
        Err(SttError::new("stthandler not of correct type"))
    }
}
