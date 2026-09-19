//! Go's `servers/chipper/intent_graph.go`: the intent graph stream.

use std::time::Instant;

use tonic::{Request, Response, Status, Streaming};
use wirepod_proto::chippergrpc2 as pb;

use crate::chipper::server::Server;
use crate::vtt::{IntentGraphRequest, ResponseStream, response_channel};

/// StreamingIntentGraph handles intent graph request streams
pub async fn streaming_intent_graph(
    server: &Server,
    request: Request<Streaming<pb::StreamingIntentGraphRequest>>,
) -> Result<Response<ResponseStream<pb::IntentGraphResponse>>, Status> {
    let recv_time = Instant::now();

    let Some(processor) = server.intent_graph.clone() else {
        return Err(Status::unimplemented("no intent graph processor"));
    };

    let mut stream = request.into_inner();
    let req = match stream.message().await {
        Ok(Some(req)) => req,
        Ok(None) => {
            tracing::info!("Intent graph stream error");
            tracing::info!("stream closed before the first request");
            return Err(Status::invalid_argument("no intent graph request"));
        }
        Err(err) => {
            tracing::info!("Intent graph stream error");
            tracing::info!("{err}");
            return Err(err);
        }
    };

    let (send, responses) = response_channel();
    let errors = send.clone();
    let graph_request = IntentGraphRequest {
        time: recv_time,
        device: req.device_id.clone(),
        session: req.session.clone(),
        lang_string: req.language_code().as_str_name().to_owned(),
        audio_codec: req.audio_encoding(),
        // Go leaves Mode unset here, so it keeps the zero value.
        mode: pb::RobotMode::VoiceCommand,
        first_req: req,
        stream,
        send,
    };

    tokio::spawn(async move {
        if let Err(err) = processor.process_intent_graph(graph_request).await {
            tracing::info!("Intent graph processing error");
            tracing::info!("{err}");
            let _ = errors.send(Err(err)).await;
        }
    });

    Ok(Response::new(responses))
}
