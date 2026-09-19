//! Go's `servers/chipper/knowledgegraph.go`: the knowledge graph stream.

use std::time::Instant;

use tonic::{Request, Response, Status, Streaming};
use wirepod_proto::chippergrpc2 as pb;

use crate::chipper::server::Server;
use crate::vtt::{KnowledgeGraphRequest, ResponseStream, response_channel};

/// StreamingKnowledgeGraph is used for knowledge graph request/responses
pub async fn streaming_knowledge_graph(
    server: &Server,
    request: Request<Streaming<pb::StreamingKnowledgeGraphRequest>>,
) -> Result<Response<ResponseStream<pb::KnowledgeGraphResponse>>, Status> {
    let recv_time = Instant::now();

    let Some(processor) = server.kg.clone() else {
        return Err(Status::unimplemented("no knowledge graph processor"));
    };

    let mut stream = request.into_inner();
    let req = match stream.message().await {
        Ok(Some(req)) => req,
        Ok(None) => {
            tracing::info!("Knowledge graph error");
            tracing::info!("stream closed before the first request");
            return Err(Status::invalid_argument("no knowledge graph request"));
        }
        Err(err) => {
            tracing::info!("Knowledge graph error");
            tracing::info!("{err}");
            return Err(err);
        }
    };

    let (send, responses) = response_channel();
    let errors = send.clone();
    let kg_request = KnowledgeGraphRequest {
        time: recv_time,
        device: req.device_id.clone(),
        session: req.session.clone(),
        lang_string: req.language_code().as_str_name().to_owned(),
        audio_codec: req.audio_encoding(),
        // Why is this not passed
        mode: pb::RobotMode::VoiceCommand,
        first_req: req,
        stream,
        send,
    };

    tokio::spawn(async move {
        if let Err(err) = processor.process_knowledge_graph(kg_request).await {
            tracing::info!("Knowledge graph error");
            tracing::info!("{err}");
            let _ = errors.send(Err(err)).await;
        }
    });

    Ok(Response::new(responses))
}
