//! Go's `pkg/vtt`: the request and response types the chipper service hands its processors.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_stream::Stream;
use tonic::{Status, Streaming};
use wirepod_proto::chippergrpc2 as pb;

/// Go's request structs carry one `grpc.ServerStream`, which both receives and
/// sends. tonic splits the two halves, so a request here carries the inbound
/// half as `stream` and this as `send`.
pub type Sender<T> = mpsc::Sender<Result<T, Status>>;

/// How many responses a processor may queue before it waits on the robot.
const RESPONSE_BUFFER: usize = 8;

/// The receiving half of a [`Sender`], which is what tonic streams back.
pub struct ResponseStream<T>(mpsc::Receiver<Result<T, Status>>);

/// A [`Sender`] and the stream that drains it.
pub fn response_channel<T>() -> (Sender<T>, ResponseStream<T>) {
    let (send, receive) = mpsc::channel(RESPONSE_BUFFER);
    (send, ResponseStream(receive))
}

impl<T> Stream for ResponseStream<T> {
    type Item = Result<T, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

/// IntentRequest is the necessary request type for VTT intent processors
pub struct IntentRequest {
    pub time: Instant,
    pub stream: Streaming<pb::StreamingIntentRequest>,
    pub send: Sender<pb::IntentResponse>,
    pub device: String,
    pub session: String,
    pub lang_string: String,
    pub first_req: pb::StreamingIntentRequest,
    pub audio_codec: pb::AudioEncoding,
}

/// IntentResponse is the response type VTT intent processors
pub struct IntentResponse {
    pub intent: Option<pb::IntentResponse>,
    pub params: String,
    pub duration: Option<Duration>,
}

/// IntentGraphRequest is the necessary request type for VTT intent processors
pub struct IntentGraphRequest {
    pub time: Instant,
    pub stream: Streaming<pb::StreamingIntentGraphRequest>,
    pub send: Sender<pb::IntentGraphResponse>,
    pub device: String,
    pub session: String,
    pub lang_string: String,
    pub first_req: pb::StreamingIntentGraphRequest,
    pub audio_codec: pb::AudioEncoding,

    // KnowledgeGraph specific
    pub mode: pb::RobotMode,
}

/// IntentGraphResponse is the response type VTT intent processors
pub struct IntentGraphResponse {
    pub intent: Option<pb::IntentGraphResponse>,
    pub params: String,
    pub duration: Option<Duration>,
}

/// KnowledgeGraphRequest is the necessary request type for VTT knowledge graph processors
pub struct KnowledgeGraphRequest {
    pub time: Instant,
    pub stream: Streaming<pb::StreamingKnowledgeGraphRequest>,
    pub send: Sender<pb::KnowledgeGraphResponse>,
    pub device: String,
    pub session: String,
    pub lang_string: String,
    pub first_req: pb::StreamingKnowledgeGraphRequest,
    pub mode: pb::RobotMode,
    pub audio_codec: pb::AudioEncoding,
}

/// KnowledgeGraphResponse is the response type VTT knowledge graph processors
pub struct KnowledgeGraphResponse {
    pub intent: Option<pb::KnowledgeGraphResponse>,
    pub params: String,
    pub duration: Option<Duration>,
}

#[tonic::async_trait]
pub trait IntentProcessor: Send + Sync + 'static {
    async fn process_intent(&self, req: IntentRequest) -> Result<IntentResponse, Status>;
}

#[tonic::async_trait]
pub trait KgProcessor: Send + Sync + 'static {
    async fn process_knowledge_graph(
        &self,
        req: KnowledgeGraphRequest,
    ) -> Result<KnowledgeGraphResponse, Status>;
}

#[tonic::async_trait]
pub trait IntentGraphProcessor: Send + Sync + 'static {
    async fn process_intent_graph(
        &self,
        req: IntentGraphRequest,
    ) -> Result<IntentGraphResponse, Status>;
}

#[cfg(test)]
mod tests {
    use tokio_stream::StreamExt;

    use super::*;

    #[tokio::test]
    async fn what_a_processor_sends_is_what_the_stream_carries() {
        let (send, mut responses) = response_channel();
        send.send(Ok(pb::IntentResponse::default()))
            .await
            .expect("queue a response");
        send.send(Err(Status::internal("intent failed")))
            .await
            .expect("queue the error");
        drop(send);

        assert!(responses.next().await.expect("a first item").is_ok());
        let status = responses
            .next()
            .await
            .expect("a second item")
            .expect_err("the error arrives after the response");
        assert_eq!(status.message(), "intent failed");
        assert!(responses.next().await.is_none());
    }
}
