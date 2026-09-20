//! Translation of `pkg/wirepod/preqs/server.go`.

use std::sync::{Arc, Mutex, PoisonError};

use wirepod_core::AppState;
use wirepod_core::intents::{JsonIntent, load_intents};
use wirepod_stt::{SttEngine, SttError};

/// Server stores the config
///
/// Go leaves the struct empty and keeps all of this in package globals.
pub struct Server {
    pub(crate) state: Arc<AppState>,
    /// Go's `InitFunc`, `sttHandler`, `stiHandler`, `isSti` and
    /// `VoiceProcessor`, which are one value here.
    pub(crate) engine: Arc<dyn SttEngine>,
    /// Go's `vars.IntentList`.
    pub(crate) intent_list: Mutex<Vec<JsonIntent>>,
}

impl Server {
    /// The `localization` package carries a second copy of this, differing only
    /// in the order of the two statements.
    pub fn reload_vosk(&self) {
        let config = self.state.config();
        if config.stt.provider == "vosk" || config.stt.provider == "whisper.cpp" {
            // Go discards the error from `vars.SttInitFunc()` as well.
            let _ = self.engine.init();
            *self
                .intent_list
                .lock()
                .unwrap_or_else(PoisonError::into_inner) =
                load_intents(self.state.paths().assets(), &config.stt.language).unwrap_or_default();
        }
    }

    /// New returns a new server
    pub fn new(state: Arc<AppState>, engine: Arc<dyn SttEngine>) -> Result<Self, SttError> {
        // Decide the TTS language
        let voice_processor = engine.name().to_owned();
        if voice_processor != "vosk" && voice_processor != "whisper.cpp" {
            state.update_config(|config| config.stt.language = "en-US".to_owned());
        }
        let stt_language = state.config().stt.language.clone();
        let intent_list = load_intents(state.paths().assets(), &stt_language).unwrap_or_default();
        tracing::info!(
            comp = "",
            "Initiating {voice_processor} voice processor with language {stt_language}"
        );
        engine.init()?;

        // Go stores `InitFunc` in `vars.SttInitFunc` for the web UI and then
        // asserts on the handler's function type, refusing anything that is
        // neither shape; the engine is both of those things.

        // TODO(M5): ttr.LoadPlugins()

        Ok(Self {
            state,
            engine,
            intent_list: Mutex::new(intent_list),
        })
    }

    /// Go's readers take `vars.IntentList` directly; a copy is what lets the
    /// matcher be awaited without the lock.
    pub fn intents(&self) -> Vec<JsonIntent> {
        self.intent_list
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;

    use wirepod_audio::SpeechRequest;
    use wirepod_core::paths::{AssetDir, DataDir};
    use wirepod_core::test_support::{FakeConnFactory, FakeRobotConn};
    use wirepod_core::{Paths, RobotConn};

    use super::*;

    /// An engine that transcribes to whatever it was built with.
    pub(crate) struct FakeEngine {
        pub name: String,
        pub transcript: Result<String, SttError>,
        pub is_sti: bool,
    }

    impl FakeEngine {
        pub(crate) fn saying(transcript: &str) -> Self {
            Self {
                name: "vosk".to_owned(),
                transcript: Ok(transcript.to_owned()),
                is_sti: false,
            }
        }
    }

    #[tonic::async_trait]
    impl SttEngine for FakeEngine {
        fn name(&self) -> &str {
            &self.name
        }

        fn init(&self) -> Result<(), SttError> {
            Ok(())
        }

        fn is_sti(&self) -> bool {
            self.is_sti
        }

        async fn stt(&self, _req: SpeechRequest) -> Result<String, SttError> {
            self.transcript.clone()
        }

        async fn sti(
            &self,
            _req: SpeechRequest,
        ) -> Result<(String, HashMap<String, String>), SttError> {
            self.transcript
                .clone()
                .map(|intent| (intent, HashMap::new()))
        }
    }

    pub(crate) fn state() -> Arc<AppState> {
        let conn: Arc<dyn RobotConn> = Arc::new(FakeRobotConn::new());
        let data = DataDir::rooted(std::env::temp_dir().join("wirepod-preqs-tests"));
        let assets = AssetDir::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets"));
        let state = AppState::builder(Arc::new(FakeConnFactory::connecting_to(conn)))
            .paths(Paths::new(data, assets))
            .build();
        // The zero configuration names no language, which loads no intents.
        state.update_config(|config| config.stt.language = "en-US".to_owned());
        state
    }

    pub(crate) fn server(engine: FakeEngine) -> Server {
        Server::new(state(), Arc::new(engine)).expect("build the voice processor")
    }

    #[test]
    fn a_non_vosk_engine_is_forced_to_en_us_and_a_vosk_one_is_not() {
        let houndify_state = state();
        houndify_state.update_config(|config| config.stt.language = "it-IT".to_owned());
        let mut engine = FakeEngine::saying("");
        engine.name = "houndify".to_owned();
        let forced = Server::new(Arc::clone(&houndify_state), Arc::new(engine)).expect("build");
        assert_eq!(houndify_state.config().stt.language, "en-US");
        assert!(
            forced
                .intents()
                .iter()
                .any(|intent| intent.name == "intent_greeting_hello")
        );

        let vosk_state = state();
        vosk_state.update_config(|config| config.stt.language = "it-IT".to_owned());
        let kept =
            Server::new(Arc::clone(&vosk_state), Arc::new(FakeEngine::saying(""))).expect("build");
        assert_eq!(vosk_state.config().stt.language, "it-IT");
        assert_ne!(kept.intents(), forced.intents());
    }

    #[test]
    fn reload_vosk_rereads_the_intent_list_for_the_current_language() {
        let server = server(FakeEngine::saying(""));
        let english = server.intents();
        server.state.update_config(|config| {
            config.stt.provider = "vosk".to_owned();
            config.stt.language = "it-IT".to_owned();
        });
        server.reload_vosk();
        let italian = server.intents();
        assert!(!italian.is_empty());
        assert_ne!(italian, english);

        // A service that is neither vosk nor whisper.cpp leaves the list alone.
        server
            .state
            .update_config(|config| config.stt.provider = "houndify".to_owned());
        server
            .state
            .update_config(|config| config.stt.language = "en-US".to_owned());
        server.reload_vosk();
        assert_eq!(server.intents(), italian);
    }
}
