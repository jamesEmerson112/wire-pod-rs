//! Translation of `pkg/wirepod/preqs`, the `wirepod.Server` the chipper
//! service is handed.
//!
//! Go reaches the rest of the world from these four files through package
//! globals and a `grpc.ServerStream` it keeps in an `interface{}`. What
//! replaces that lives here rather than in any of the four translated modules:
//! the adapter that reads audio off the three tonic streams, the two sinks that
//! write a response onto the channel the chipper service drains, the two calls
//! `wirepod-intent` takes as hooks, and the per-request read of the `vars`
//! globals the matcher wants.

pub mod intent;
pub mod intent_graph;
pub mod knowledgegraph;
pub mod server;

use std::sync::Arc;

use tonic::Streaming;
use wirepod_audio::ChunkSource;
use wirepod_core::intents::CustomIntent;
use wirepod_core::{AppState, BotInfoRobot, ConnError, Esn};
use wirepod_intent::intentparam::bot_location_and_units;
use wirepod_intent::match_intent_send::{
    IntentContext, IntentHooks, IntentSink, RequestKind, SendError, Weather,
};
use wirepod_proto::chippergrpc2 as pb;
use wirepod_server::vtt::Sender;
use wirepod_vector::status_error;

use crate::{bcontrol, weather};

/// The one field `ReqToSpeechRequest` and its two getters read off a streamed
/// message, which the three request types spell the same way.
pub trait AudioChunk {
    fn into_input_audio(self) -> Vec<u8>;
}

macro_rules! audio_chunk {
    ($($message:ty),+ $(,)?) => {
        $(impl AudioChunk for $message {
            fn into_input_audio(self) -> Vec<u8> {
                self.input_audio
            }
        })+
    };
}

audio_chunk!(
    pb::StreamingIntentRequest,
    pb::StreamingIntentGraphRequest,
    pb::StreamingKnowledgeGraphRequest,
);

/// Go's `stream.Recv()`, type-switched over the three stream types in six
/// places. `wirepod-audio` may not depend on tonic, so the switch is this.
pub struct AudioChunks<T>(Streaming<T>);

impl<T> AudioChunks<T> {
    pub fn new(stream: Streaming<T>) -> Self {
        Self(stream)
    }
}

#[tonic::async_trait]
impl<T: AudioChunk + Send + 'static> ChunkSource for AudioChunks<T> {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, ConnError> {
        match self.0.message().await {
            Ok(chunk) => Ok(chunk.map(AudioChunk::into_input_audio)),
            Err(status) => Err(status_error(&status)),
        }
    }
}

/// Go writes the protobuf straight onto `req.Stream`.
pub struct IntentSend {
    pub send: Sender<pb::IntentResponse>,
    pub device: String,
    pub session: String,
}

#[tonic::async_trait]
impl IntentSink for IntentSend {
    fn kind(&self) -> RequestKind {
        RequestKind::Intent
    }

    fn device(&self) -> &str {
        &self.device
    }

    fn session(&self) -> &str {
        &self.session
    }

    async fn send_intent(&self, response: pb::IntentResponse) -> Result<(), SendError> {
        self.send
            .send(Ok(response))
            .await
            .map_err(|err| SendError(err.to_string()))
    }
}

pub struct IntentGraphSend {
    pub send: Sender<pb::IntentGraphResponse>,
    pub device: String,
    pub session: String,
}

#[tonic::async_trait]
impl IntentSink for IntentGraphSend {
    fn kind(&self) -> RequestKind {
        RequestKind::IntentGraph
    }

    fn device(&self) -> &str {
        &self.device
    }

    fn session(&self) -> &str {
        &self.session
    }

    async fn send_intent_graph(&self, response: pb::IntentGraphResponse) -> Result<(), SendError> {
        self.send
            .send(Ok(response))
            .await
            .map_err(|err| SendError(err.to_string()))
    }
}

/// The two calls `wirepod-intent` leaves to its caller.
pub struct Hooks(Arc<AppState>);

#[tonic::async_trait]
impl IntentHooks for Hooks {
    async fn weather_parser(
        &self,
        speech_text: &str,
        bot_location: &str,
        bot_units: &str,
    ) -> Weather {
        weather::weather_parser(&self.0, speech_text, bot_location, bot_units).await
    }

    async fn say_text(
        &self,
        bot_serial: &str,
        _guid: &str,
        _target: &str,
        text: &str,
    ) -> Result<(), String> {
        // Go dials a fresh connection from the GUID and the target; the
        // registry resolves the same robot from its serial and caches it.
        let entry = self
            .0
            .get_robot(&Esn::new(bot_serial))
            .await
            .map_err(|err| err.to_string())?;
        bcontrol::say_text(&entry, text)
            .await
            .map_err(|err| err.desc)
    }
}

/// The `vars` globals the matcher reads, gathered once per request because Go
/// reads them once per request.
pub struct Globals {
    pub language: String,
    pub intent_graph: bool,
    pub knowledge_enable: bool,
    pub knowledge_provider: String,
    weather_enable: bool,
    custom_intents: Option<Vec<CustomIntent>>,
    robots: Vec<BotInfoRobot>,
    bot_location: String,
    bot_units: String,
    hooks: Hooks,
}

impl Globals {
    pub fn read(state: &Arc<AppState>, esn: &str) -> Self {
        let config = state.config();
        let settings = state
            .jdocs()
            .get_jdoc(&format!("vic:{esn}"), "vic.RobotSettings");
        let (bot_location, bot_units) =
            bot_location_and_units(settings.as_ref().map(|jdoc| jdoc.json_doc.as_str()));
        Self {
            language: config.stt.language.clone(),
            intent_graph: config.knowledge.intentgraph,
            knowledge_enable: config.knowledge.enable,
            knowledge_provider: config.knowledge.provider.clone(),
            weather_enable: config.weather.enable,
            custom_intents: state
                .custom_intents()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            robots: state.with_bot_info(|info| info.robots.clone()),
            bot_location,
            bot_units,
            hooks: Hooks(Arc::clone(state)),
        }
    }

    pub fn context(&self) -> IntentContext<'_> {
        IntentContext {
            language: &self.language,
            intent_graph: self.intent_graph,
            weather_enable: self.weather_enable,
            // `vars.VoskGrammerEnable` is declared and read but never assigned.
            vosk_grammer_enable: false,
            custom_intents: self.custom_intents.as_deref(),
            robots: &self.robots,
            // TODO(M5): ttr.LoadPlugins fills the three plugin arrays.
            plugins: &[],
            bot_location: &self.bot_location,
            bot_units: &self.bot_units,
            hooks: &self.hooks,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use bytes::Bytes;
    use http_body_util::Empty;
    use tonic::codec::{Codec, ProstCodec};

    use super::*;

    /// A tonic stream that ends at once, which is what the robot's stream looks
    /// like to a handler whose engine never reads past the first request.
    pub(crate) fn empty_stream<T: prost::Message + Default + 'static>() -> Streaming<T> {
        let mut codec = ProstCodec::<T, T>::default();
        Streaming::new_request(codec.decoder(), Empty::<Bytes>::new(), None, None)
    }
}
