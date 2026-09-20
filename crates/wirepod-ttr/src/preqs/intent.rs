//! Translation of `pkg/wirepod/preqs/intent.go`.

use std::collections::HashMap;

use tonic::Status;
use wirepod_audio::{RequestKind, RequestParts, req_to_speech_request};
use wirepod_intent::intentparam::param_checker_slots_en_us;
use wirepod_intent::match_intent_send::{intent_pass, process_text_all};
use wirepod_server::vtt::{IntentProcessor, IntentRequest, IntentResponse};

use crate::preqs::server::Server;
use crate::preqs::{AudioChunks, Globals, IntentSend};

/// Go's `(nil, nil)`: the protobuf went out on the stream from inside
/// `IntentPass`, never up the call stack.
pub(crate) fn no_response() -> IntentResponse {
    IntentResponse {
        intent: None,
        params: String::new(),
        duration: None,
    }
}

/// Go's `map[string]string{"error": err.Error()}`.
pub(crate) fn error_param(err: &str) -> HashMap<String, String> {
    HashMap::from([("error".to_owned(), err.to_owned())])
}

/// Go's `map[string]string{"": ""}`, whose empty key reaches the robot.
pub(crate) fn empty_param() -> HashMap<String, String> {
    HashMap::from([(String::new(), String::new())])
}

// This is here for compatibility with 1.6 and older software
#[tonic::async_trait]
impl IntentProcessor for Server {
    async fn process_intent(&self, req: IntentRequest) -> Result<IntentResponse, Status> {
        let success_matched;
        let device = req.device.clone();
        let globals = Globals::read(&self.state, &device);
        let ctx = globals.context();
        let sink = IntentSend {
            send: req.send,
            device: device.clone(),
            session: req.session.clone(),
        };
        let speech_req = req_to_speech_request(RequestParts {
            kind: RequestKind::Intent,
            device: device.clone(),
            session: req.session,
            stream: Box::new(AudioChunks::new(req.stream)),
            first_req: req.first_req.input_audio,
        });
        // Go reads `speechReq.IsOpus` after the handler has taken its copy; the
        // engine takes this one by value, so the flag is read first.
        let is_opus = speech_req.is_opus;
        let transcribed_text;
        if !self.engine.is_sti() {
            match self.engine.stt(speech_req).await {
                Ok(text) => transcribed_text = text,
                Err(err) => {
                    let _ = intent_pass(
                        &sink,
                        &ctx,
                        "intent_system_noaudio",
                        &format!("voice processing error: {err}"),
                        error_param(&err.message),
                        true,
                    )
                    .await;
                    return Ok(no_response());
                }
            }
            if transcribed_text.trim().is_empty() {
                let _ = intent_pass(
                    &sink,
                    &ctx,
                    "intent_system_noaudio",
                    "",
                    HashMap::new(),
                    false,
                )
                .await;
                return Ok(no_response());
            }
            success_matched =
                process_text_all(&sink, &ctx, &transcribed_text, &self.intents(), is_opus).await;
        } else {
            match self.engine.sti(speech_req).await {
                Ok((intent, slots)) => {
                    param_checker_slots_en_us(&sink, &ctx, &intent, &slots, is_opus).await;
                    return Ok(no_response());
                }
                Err(err) => {
                    if err.message == "inference not understood" {
                        tracing::debug!(comp = "intent", bot = %device, "no intent matched");
                        let _ = intent_pass(
                            &sink,
                            &ctx,
                            "intent_system_unmatched",
                            "voice processing error",
                            error_param(&err.message),
                            true,
                        )
                        .await;
                        return Ok(no_response());
                    }
                    tracing::info!(comp = "", "{err}");
                    let _ = intent_pass(
                        &sink,
                        &ctx,
                        "intent_system_noaudio",
                        "voice processing error",
                        error_param(&err.message),
                        true,
                    )
                    .await;
                    return Ok(no_response());
                }
            }
        }
        if !success_matched {
            if globals.intent_graph && globals.knowledge_enable {
                tracing::debug!(comp = "llm", bot = %device, "making LLM request");
                // TODO(M4): ttr.StreamingKGSim(req, req.Device, transcribedText,
                // false), which logs "request served" and returns here, and on
                // an error sends intent_system_unmatched and KGSim's apology.
                // Without it the request falls through to unmatched below.
            }
            tracing::debug!(comp = "intent", bot = %device, "no intent matched");
            let _ = intent_pass(
                &sink,
                &ctx,
                "intent_system_unmatched",
                &transcribed_text,
                empty_param(),
                false,
            )
            .await;
            return Ok(no_response());
        }
        tracing::info!(comp = "intent", bot = %device, "request served");
        Ok(no_response())
    }
}

#[cfg(test)]
mod tests {
    use tokio_stream::StreamExt;
    use wirepod_proto::chippergrpc2 as pb;
    use wirepod_server::vtt::response_channel;
    use wirepod_stt::SttError;

    use super::*;
    use crate::preqs::server::tests::{FakeEngine, server};
    use crate::preqs::tests::empty_stream;

    /// Drives one request through the processor and answers what the robot saw.
    async fn run(engine: FakeEngine) -> Vec<pb::IntentResult> {
        let (send, mut responses) = response_channel();
        let server = server(engine);
        let req = IntentRequest {
            time: std::time::Instant::now(),
            stream: empty_stream(),
            send,
            device: "00303f28".to_owned(),
            session: "session".to_owned(),
            lang_string: "ENGLISH_US".to_owned(),
            first_req: pb::StreamingIntentRequest::default(),
            audio_codec: pb::AudioEncoding::LinearPcm,
        };
        server
            .process_intent(req)
            .await
            .expect("process the intent");
        let mut results = Vec::new();
        while let Some(response) = responses.next().await {
            results.push(
                response
                    .expect("a response")
                    .intent_result
                    .unwrap_or_default(),
            );
        }
        results
    }

    async fn actions(engine: FakeEngine) -> Vec<String> {
        run(engine)
            .await
            .into_iter()
            .map(|result| result.action)
            .collect()
    }

    #[tokio::test]
    async fn a_transcript_reaches_the_matcher_and_its_intent_reaches_the_robot() {
        assert_eq!(
            actions(FakeEngine::saying("hello")).await,
            ["intent_greeting_hello"]
        );

        // Nothing matched is Go's unmatched intent, and an all-space transcript
        // is the no-audio one.
        assert_eq!(
            actions(FakeEngine::saying("what is the airspeed of a swallow")).await,
            ["intent_system_unmatched"]
        );
        assert_eq!(
            actions(FakeEngine::saying("   ")).await,
            ["intent_system_noaudio"]
        );
    }

    #[tokio::test]
    async fn a_transcription_error_sends_no_audio_with_the_error_parameter() {
        let engine = FakeEngine {
            name: "vosk".to_owned(),
            transcript: Err(SttError::new("model exploded")),
            is_sti: false,
        };
        let results = run(engine).await;
        assert_eq!(results[0].action, "intent_system_noaudio");
        assert_eq!(
            results[0].query_text,
            "voice processing error: model exploded"
        );
        assert_eq!(
            results[0].parameters.get("error"),
            Some(&"model exploded".to_owned())
        );
    }
}
