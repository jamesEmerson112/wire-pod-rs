//! Translation of `pkg/wirepod/preqs/intent_graph.go`.

use std::collections::HashMap;

use tonic::Status;
use wirepod_audio::{RequestKind, RequestParts, req_to_speech_request};
use wirepod_intent::intentparam::param_checker_slots_en_us;
use wirepod_intent::match_intent_send::{
    intent_pass, knowledge_graph_response_ig, process_text_all,
};
use wirepod_server::vtt::{IntentGraphProcessor, IntentGraphRequest, IntentGraphResponse};

use crate::preqs::intent::{empty_param, error_param};
use crate::preqs::knowledgegraph::{houndify_text_request, init_knowledge};
use crate::preqs::server::Server;
use crate::preqs::{AudioChunks, Globals, IntentGraphSend};

/// Go's `(nil, nil)`, as in `intent.go`.
fn no_response() -> IntentGraphResponse {
    IntentGraphResponse {
        intent: None,
        params: String::new(),
        duration: None,
    }
}

#[tonic::async_trait]
impl IntentGraphProcessor for Server {
    async fn process_intent_graph(
        &self,
        req: IntentGraphRequest,
    ) -> Result<IntentGraphResponse, Status> {
        let success_matched;
        let device = req.device.clone();
        let session = req.session.clone();
        let globals = Globals::read(&self.state, &device);
        let ctx = globals.context();
        let sink = IntentGraphSend {
            send: req.send,
            device: device.clone(),
            session: session.clone(),
        };
        let speech_req = req_to_speech_request(RequestParts {
            kind: RequestKind::IntentGraph,
            device: device.clone(),
            session: req.session,
            stream: Box::new(AudioChunks::new(req.stream)),
            first_req: req.first_req.input_audio,
        });
        // Read before the engine takes the request by value, as in `intent.rs`.
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
        // Go carries a commented-out openai branch above this one, which is not
        // translated.
        if !success_matched {
            if globals.intent_graph && globals.knowledge_enable {
                if globals.knowledge_provider == "houndify" {
                    if transcribed_text.chars().count() >= 8 {
                        tracing::debug!(comp = "llm", bot = %device, "forwarding to houndify");
                        // Errors without this for whatever reason even though I
                        // think it should be inited already
                        init_knowledge(&self.state);
                        let api_response = houndify_text_request(
                            &self.state,
                            &transcribed_text,
                            &device,
                            &session,
                        );
                        if !api_response.is_empty()
                            && !api_response.contains("not enabled")
                            && !api_response.contains("Knowledge graph is not enabled")
                            && !api_response.contains("Didn't get that!")
                        {
                            let _ = knowledge_graph_response_ig(
                                &sink,
                                &api_response,
                                &transcribed_text,
                            )
                            .await;
                            tracing::info!(comp = "intent", bot = %device, "request served via houndify");
                            return Ok(no_response());
                        }
                        // If Houndify fails or returns nothing useful, fall through to unmatched
                        tracing::debug!(comp = "llm", bot = %device, "houndify returned empty or error response");
                    }
                } else {
                    tracing::debug!(comp = "llm", bot = %device, "making LLM request");
                    // TODO(M4): ttr.StreamingKGSim(req, req.Device,
                    // transcribedText, false), which logs "request served" and
                    // returns here whether or not it failed. Without it the
                    // request falls through to unmatched below.
                }
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

    use super::*;
    use crate::preqs::server::tests::{FakeEngine, server};
    use crate::preqs::tests::empty_stream;

    async fn run(server: &Server) -> Vec<pb::IntentGraphResponse> {
        let (send, mut responses) = response_channel();
        let req = IntentGraphRequest {
            time: std::time::Instant::now(),
            stream: empty_stream(),
            send,
            device: "00303f28".to_owned(),
            session: "session".to_owned(),
            lang_string: "ENGLISH_US".to_owned(),
            first_req: pb::StreamingIntentGraphRequest::default(),
            audio_codec: pb::AudioEncoding::LinearPcm,
            mode: pb::RobotMode::VoiceCommand,
        };
        server
            .process_intent_graph(req)
            .await
            .expect("process the intent graph request");
        let mut sent = Vec::new();
        while let Some(response) = responses.next().await {
            sent.push(response.expect("a response"));
        }
        sent
    }

    #[tokio::test]
    async fn a_match_goes_out_as_an_intent_graph_response() {
        let server = server(FakeEngine::saying("hello"));
        let sent = run(&server).await;
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].intent_result.clone().unwrap_or_default().action,
            "intent_greeting_hello"
        );
        assert_eq!(sent[0].response_type, pb::IntentGraphMode::Intent as i32);
    }

    #[tokio::test]
    async fn an_enabled_houndify_provider_falls_through_to_unmatched() {
        let server = server(FakeEngine::saying("what is the airspeed of a swallow"));
        server.state.update_config(|config| {
            config.knowledge.enable = true;
            config.knowledge.intentgraph = true;
            config.knowledge.provider = "houndify".to_owned();
        });
        let sent = run(&server).await;
        assert_eq!(
            sent[0].intent_result.clone().unwrap_or_default().action,
            "intent_system_unmatched"
        );
    }
}
