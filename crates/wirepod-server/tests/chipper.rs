//! The chipper service over a real gRPC connection on an ephemeral loopback port.

use std::net::SocketAddr;
use std::sync::Arc;

use tonic::{Request, Status};
use wirepod_proto::chippergrpc2 as pb;
use wirepod_proto::chippergrpc2::chipper_grpc_client::ChipperGrpcClient;
use wirepod_proto::chippergrpc2::chipper_grpc_server::ChipperGrpcServer;
use wirepod_server::chipper::{Options, Server};
use wirepod_server::vtt::{IntentProcessor, IntentRequest, IntentResponse};

/// Serves `server` on `127.0.0.1:0` and hands back the address it landed on.
async fn spawn_chipper(server: Server) -> SocketAddr {
    let router = tonic::service::Routes::new(ChipperGrpcServer::new(server)).into_axum_router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral loopback port");
    let addr = listener.local_addr().expect("read the bound address");
    tokio::spawn(async move {
        while let Ok((tcp, _peer)) = listener.accept().await {
            let router = router.clone();
            tokio::spawn(async move {
                let io = hyper_util::rt::TokioIo::new(tcp);
                let svc = hyper_util::service::TowerToHyperService::new(router);
                let builder = hyper_util::server::conn::auto::Builder::new(
                    hyper_util::rt::TokioExecutor::new(),
                );
                let _ = builder.serve_connection(io, svc).await;
            });
        }
    });
    addr
}

async fn client(addr: SocketAddr) -> ChipperGrpcClient<tonic::transport::Channel> {
    ChipperGrpcClient::connect(format!("http://{addr}"))
        .await
        .expect("dial the chipper service")
}

/// A processor that answers with the fields the handler filled in for it.
struct Echo;

#[tonic::async_trait]
impl IntentProcessor for Echo {
    async fn process_intent(&self, req: IntentRequest) -> Result<IntentResponse, Status> {
        let reply = pb::IntentResponse {
            device_id: req.device.clone(),
            session: req.lang_string.clone(),
            is_final: true,
            ..Default::default()
        };
        req.send.send(Ok(reply)).await.expect("send the response");
        Ok(IntentResponse {
            intent: None,
            params: String::new(),
            duration: None,
        })
    }
}

#[tokio::test]
async fn a_connection_check_that_gets_every_frame_answers_success() {
    let addr = spawn_chipper(Server::new(Options::new())).await;
    let mut client = client(addr).await;

    let frame = pb::StreamingConnectionCheckRequest {
        device_id: "00303f28".to_owned(),
        total_audio_ms: 300,
        audio_per_request: 100,
        ..Default::default()
    };
    let frames = tokio_stream::iter(vec![frame.clone(), frame.clone(), frame]);

    let mut responses = client
        .streaming_connection_check(Request::new(frames))
        .await
        .expect("the connection check is accepted")
        .into_inner();
    let reply = responses
        .message()
        .await
        .expect("read the response")
        .expect("one response");

    assert_eq!(reply.status, "Success");
    assert_eq!(reply.frames_received, 3);
}

#[tokio::test]
async fn a_connection_check_whose_stream_ends_early_answers_error() {
    let addr = spawn_chipper(Server::new(Options::new())).await;
    let mut client = client(addr).await;

    let frame = pb::StreamingConnectionCheckRequest {
        device_id: "00303f28".to_owned(),
        total_audio_ms: 300,
        audio_per_request: 100,
        ..Default::default()
    };
    let frames = tokio_stream::iter(vec![frame]);

    let mut responses = client
        .streaming_connection_check(Request::new(frames))
        .await
        .expect("the connection check is accepted")
        .into_inner();
    let reply = responses
        .message()
        .await
        .expect("read the response")
        .expect("one response");

    assert_eq!(reply.status, "Error");
    assert_eq!(reply.frames_received, 1);
}

#[tokio::test]
async fn an_unset_processor_and_text_intent_both_answer_unimplemented() {
    let addr = spawn_chipper(Server::new(Options::new())).await;
    let mut client = client(addr).await;

    let requests = tokio_stream::iter(vec![pb::StreamingIntentRequest::default()]);
    let status = client
        .streaming_intent(Request::new(requests))
        .await
        .expect_err("no intent processor is set");
    assert_eq!(status.code(), tonic::Code::Unimplemented);

    let status = client
        .text_intent(Request::new(pb::TextRequest::default()))
        .await
        .expect_err("text intent is unimplemented");
    assert_eq!(status.code(), tonic::Code::Unimplemented);
}

#[tokio::test]
async fn the_intent_processor_sees_the_first_request_and_its_reply_reaches_the_robot() {
    let server = Server::new(Options::new().with_intent_processor(Arc::new(Echo)));
    let addr = spawn_chipper(server).await;
    let mut client = client(addr).await;

    let first = pb::StreamingIntentRequest {
        device_id: "00303f28".to_owned(),
        language_code: pb::LanguageCode::German as i32,
        ..Default::default()
    };
    let requests = tokio_stream::iter(vec![first]);

    let mut responses = client
        .streaming_intent(Request::new(requests))
        .await
        .expect("the intent stream is accepted")
        .into_inner();
    let reply = responses
        .message()
        .await
        .expect("read the response")
        .expect("one response");

    assert_eq!(reply.device_id, "00303f28");
    assert_eq!(reply.session, "GERMAN");
    assert!(reply.is_final);
}
