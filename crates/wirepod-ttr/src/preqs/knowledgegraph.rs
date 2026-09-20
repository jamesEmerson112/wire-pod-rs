//! Translation of `pkg/wirepod/preqs/knowledgegraph.go`.

use regex::Regex;
use serde_json::Value;
use tonic::Status;
use wirepod_audio::{RequestKind, RequestParts, SpeechRequest, req_to_speech_request};
use wirepod_core::AppState;
use wirepod_proto::chippergrpc2 as pb;
use wirepod_server::vtt::{KgProcessor, KnowledgeGraphRequest, KnowledgeGraphResponse, Sender};

use crate::preqs::AudioChunks;
use crate::preqs::server::Server;

// TODO(M5): var HKGclient houndify.Client

/// Go type-asserts every value below without checking, which panics whenever
/// Houndify answers with a shape it did not expect; each one is checked here.
pub fn parse_spoken_response(server_response_json: &str) -> Result<String, String> {
    let result: serde_json::Map<String, Value> = match serde_json::from_str(server_response_json) {
        Ok(result) => result,
        Err(err) => {
            tracing::info!(comp = "", "{err}");
            return Err("failed to decode json".to_owned());
        }
    };
    // `strings.EqualFold` folds the whole of Unicode; the value compared is the
    // literal `OK`.
    let status = result
        .get("Status")
        .and_then(Value::as_str)
        .ok_or("no Status in response")?;
    if !status.eq_ignore_ascii_case("OK") {
        return Err(result
            .get("ErrorMessage")
            .and_then(Value::as_str)
            .unwrap_or("no ErrorMessage in response")
            .to_owned());
    }
    let num_to_return = result
        .get("NumToReturn")
        .and_then(Value::as_f64)
        .ok_or("no NumToReturn in response")?;
    if num_to_return < 1.0 {
        return Err("no results to return".to_owned());
    }
    result
        .get("AllResults")
        .and_then(Value::as_array)
        .and_then(|all| all.first())
        .and_then(Value::as_object)
        .and_then(|first| first.get("SpokenResponseLong"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "no SpokenResponseLong in response".to_owned())
}

pub fn init_knowledge(state: &AppState) {
    let config = state.config();
    if config.knowledge.enable && config.knowledge.provider == "houndify" {
        if config.knowledge.id.is_empty() || config.knowledge.key.is_empty() {
            state.update_config(|config| config.knowledge.enable = false);
            tracing::info!(
                comp = "",
                "Houndify Client Key or ID was empty, not initializing kg client"
            );
        } else {
            // TODO(M5): houndify.Client{ClientID, ClientKey} and
            // HKGclient.EnableConversationState()
            tracing::info!(comp = "", "Initialized Houndify client");
        }
    }
}

pub const NO_RESULT: &str = "NoResultCommand";

fn houndify_kg(state: &AppState, req: SpeechRequest) -> String {
    let api_response;
    let config = state.config();
    if config.knowledge.enable && config.knowledge.provider == "houndify" {
        tracing::info!(comp = "", "Sending request to Houndify...");
        // TODO(M5): StreamAudioToHoundify(req, HKGclient)
        let _ = req;
        let server_response = String::new();
        api_response = parse_spoken_response(&server_response).unwrap_or_default();
        tracing::info!(comp = "", "Houndify response: {api_response}");
    } else {
        api_response = "Houndify is not enabled.".to_owned();
        tracing::info!(comp = "", "Houndify is not enabled.");
    }
    api_response
}

async fn streaming_kg(
    server: &Server,
    send: &Sender<pb::KnowledgeGraphResponse>,
    session: &str,
    device: &str,
    speech_req: SpeechRequest,
) -> String {
    // have him start "thinking" right after the text is transcribed
    let bot = speech_req.device.clone();
    let Ok(transcribed_text) = server.engine.stt(speech_req).await else {
        return "There was an error.".to_owned();
    };
    let kg = pb::KnowledgeGraphResponse {
        session: session.to_owned(),
        device_id: device.to_owned(),
        command_type: NO_RESULT.to_owned(),
        spoken_text: "bla bla bla bla bla bla bla bla bla bla".to_owned(),
        ..Default::default()
    };
    let _ = send.send(Ok(kg)).await;
    // TODO(M4): ttr.StreamingKGSim(req, req.Device, transcribedText, true),
    // whose error Go logs and carries on from.
    let _ = transcribed_text;
    tracing::info!(comp = "", "(KG) Bot {bot} request served.");
    String::new()
}

/// Takes a SpeechRequest, figures out knowledgegraph provider, makes request,
/// returns API response
pub fn kg_request(state: &AppState, speech_req: SpeechRequest) -> String {
    let config = state.config();
    if config.knowledge.enable && config.knowledge.provider == "houndify" {
        return houndify_kg(state, speech_req);
    }
    "Knowledge graph is not enabled. This can be enabled in the web interface.".to_owned()
}

#[tonic::async_trait]
impl KgProcessor for Server {
    async fn process_knowledge_graph(
        &self,
        req: KnowledgeGraphRequest,
    ) -> Result<KnowledgeGraphResponse, Status> {
        init_knowledge(&self.state);
        let device = req.device.clone();
        let session = req.session.clone();
        let speech_req = req_to_speech_request(RequestParts {
            kind: RequestKind::KnowledgeGraph,
            device: device.clone(),
            session: req.session,
            stream: Box::new(AudioChunks::new(req.stream)),
            first_req: req.first_req.input_audio,
        });
        let config = self.state.config();
        if config.knowledge.enable && config.knowledge.provider != "houndify" {
            streaming_kg(self, &req.send, &session, &device, speech_req).await;
        } else {
            let bot = speech_req.device.clone();
            let api_response = kg_request(&self.state, speech_req);
            let kg = pb::KnowledgeGraphResponse {
                session,
                device_id: device,
                command_type: NO_RESULT.to_owned(),
                spoken_text: api_response,
                ..Default::default()
            };
            tracing::info!(comp = "", "(KG) Bot {bot} request served.");
            if let Err(err) = req.send.send(Ok(kg)).await {
                return Err(Status::internal(err.to_string()));
            }
        }
        Ok(KnowledgeGraphResponse {
            intent: None,
            params: String::new(),
            duration: None,
        })
    }
}

fn clean_houndify_response(response: &str) -> String {
    // This should remove the "Redirected from" text
    let re = Regex::new(r"^Redirected from [^.]+\.\s*").expect("a valid pattern");
    re.replace_all(response, "").into_owned()
}

pub fn houndify_text_request(
    state: &AppState,
    query_text: &str,
    device: &str,
    session: &str,
) -> String {
    let config = state.config();
    if !config.knowledge.enable || config.knowledge.provider != "houndify" {
        return "Houndify is not enabled.".to_owned();
    }

    tracing::info!(comp = "", "Sending text request to Houndify...");

    // TODO(M5): HKGclient.TextSearch(houndify.TextRequest{Query: query_text,
    // UserID: device, RequestID: session}), whose transport error Go logs
    // before returning the empty string.
    let _ = (query_text, device, session);
    let server_response = String::new();

    let api_response = match parse_spoken_response(&server_response) {
        Ok(api_response) => api_response,
        Err(err) => {
            tracing::info!(comp = "", "Error parsing Houndify response: {err}");
            tracing::info!(comp = "", "Raw response: {server_response}");
            return String::new();
        }
    };

    let api_response = clean_houndify_response(&api_response);

    tracing::info!(comp = "", "Houndify response: {api_response}");
    api_response
}

#[cfg(test)]
mod tests {
    use tokio_stream::StreamExt;
    use wirepod_server::vtt::response_channel;

    use super::*;
    use crate::preqs::server::tests::{FakeEngine, server};
    use crate::preqs::tests::empty_stream;

    #[test]
    fn a_response_of_the_wrong_shape_is_an_error_rather_than_a_panic() {
        let ok = r#"{"Status":"ok","NumToReturn":1,
            "AllResults":[{"SpokenResponseLong":"Redirected from Wikipedia. It is a bird."}]}"#;
        assert_eq!(
            parse_spoken_response(ok).unwrap(),
            "Redirected from Wikipedia. It is a bird."
        );
        assert_eq!(
            clean_houndify_response("Redirected from Wikipedia. It is a bird."),
            "It is a bird."
        );

        assert_eq!(
            parse_spoken_response(r#"{"Status":"Error","ErrorMessage":"nope"}"#),
            Err("nope".to_owned())
        );
        assert_eq!(
            parse_spoken_response(r#"{"Status":"OK","NumToReturn":0}"#),
            Err("no results to return".to_owned())
        );
        // Every one of these is an unchecked type assertion in Go.
        assert!(parse_spoken_response("{").is_err());
        assert!(parse_spoken_response("{}").is_err());
        assert!(parse_spoken_response(r#"{"Status":"OK","NumToReturn":1}"#).is_err());
        assert!(
            parse_spoken_response(r#"{"Status":"OK","NumToReturn":1,"AllResults":[{}]}"#).is_err()
        );
    }

    #[tokio::test]
    async fn a_disabled_knowledge_graph_answers_the_web_interface_text() {
        let server = server(FakeEngine::saying("who are you"));
        let (send, mut responses) = response_channel();
        let req = KnowledgeGraphRequest {
            time: std::time::Instant::now(),
            stream: empty_stream(),
            send,
            device: "00303f28".to_owned(),
            session: "session".to_owned(),
            lang_string: "ENGLISH_US".to_owned(),
            first_req: pb::StreamingKnowledgeGraphRequest::default(),
            mode: pb::RobotMode::VoiceCommand,
            audio_codec: pb::AudioEncoding::LinearPcm,
        };
        server
            .process_knowledge_graph(req)
            .await
            .expect("process the knowledge graph request");
        let sent = responses
            .next()
            .await
            .expect("a response")
            .expect("no error");
        assert_eq!(sent.command_type, NO_RESULT);
        assert_eq!(
            sent.spoken_text,
            "Knowledge graph is not enabled. This can be enabled in the web interface."
        );
        assert_eq!(sent.device_id, "00303f28");
    }
}
