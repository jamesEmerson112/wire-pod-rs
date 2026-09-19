//! Go's `servers/chipper/intent.go`: the voice intent stream.

use std::time::Instant;

use tonic::{Request, Response, Status, Streaming};
use wirepod_proto::chippergrpc2 as pb;

use crate::chipper::server::Server;
use crate::vtt::{IntentRequest, ResponseStream, response_channel};

/// StreamingIntent handles voice streams
pub async fn streaming_intent(
    server: &Server,
    request: Request<Streaming<pb::StreamingIntentRequest>>,
) -> Result<Response<ResponseStream<pb::IntentResponse>>, Status> {
    let recv_time = Instant::now();

    // Go's processor field is never nil because the binary always sets it;
    // here it is optional, so an unset one answers Unimplemented.
    let Some(processor) = server.intent.clone() else {
        return Err(Status::unimplemented("no intent processor"));
    };

    let mut stream = request.into_inner();
    let req = match stream.message().await {
        Ok(Some(req)) => req,
        Ok(None) => {
            tracing::info!("Intent error");
            tracing::info!("stream closed before the first request");
            return Err(Status::invalid_argument("no intent request"));
        }
        Err(err) => {
            tracing::info!("Intent error");
            tracing::info!("{err}");
            return Err(err);
        }
    };

    let (send, responses) = response_channel();
    let errors = send.clone();
    let intent_request = IntentRequest {
        time: recv_time,
        device: req.device_id.clone(),
        session: req.session.clone(),
        lang_string: req.language_code().as_str_name().to_owned(),
        audio_codec: req.audio_encoding(),
        first_req: req,
        stream,
        send,
    };

    // Go blocks in the handler and returns the processor's error to the robot.
    // tonic wants the response stream back before the first response is sent,
    // so the processor runs on its own task and its error travels down that
    // same stream.
    tokio::spawn(async move {
        if let Err(err) = processor.process_intent(intent_request).await {
            tracing::info!("Intent error");
            tracing::info!("{err}");
            let _ = errors.send(Err(err)).await;
        }
    });

    Ok(Response::new(responses))
}
