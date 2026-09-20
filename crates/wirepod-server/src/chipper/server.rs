//! Go's `servers/chipper/server.go`: the service type and its constructor.

use std::sync::Arc;

use tonic::{Request, Response, Status, Streaming};
use wirepod_proto::chippergrpc2 as pb;
use wirepod_proto::chippergrpc2::chipper_grpc_server::ChipperGrpc;

use crate::chipper::options::Options;
use crate::chipper::{connectioncheck, intent, intent_graph, knowledgegraph, textintent};
use crate::vtt::{IntentGraphProcessor, IntentProcessor, KgProcessor, ResponseStream};

/// Server defines the service used.
#[derive(Clone)]
pub struct Server {
    pub(crate) intent: Option<Arc<dyn IntentProcessor>>,
    pub(crate) kg: Option<Arc<dyn KgProcessor>>,
    pub(crate) intent_graph: Option<Arc<dyn IntentGraphProcessor>>,
}

impl Server {
    /// New accepts a list of args and returns the service. Go's `New` returns
    /// an error it never sets.
    pub fn new(opts: Options) -> Self {
        Self {
            intent: opts.intent,
            kg: opts.kg,
            intent_graph: opts.intent_graph,
        }
    }
}

// A trait implementation cannot be split across modules the way Go splits
// methods across files, so each RPC delegates to the module that holds the Go
// file it came from.
#[tonic::async_trait]
impl ChipperGrpc for Server {
    type StreamingIntentStream = ResponseStream<pb::IntentResponse>;
    type StreamingKnowledgeGraphStream = ResponseStream<pb::KnowledgeGraphResponse>;
    type StreamingIntentGraphStream = ResponseStream<pb::IntentGraphResponse>;
    type StreamingConnectionCheckStream = ResponseStream<pb::ConnectionCheckResponse>;

    async fn text_intent(
        &self,
        request: Request<pb::TextRequest>,
    ) -> Result<Response<pb::IntentResponse>, Status> {
        textintent::text_intent(self, request).await
    }

    async fn streaming_intent(
        &self,
        request: Request<Streaming<pb::StreamingIntentRequest>>,
    ) -> Result<Response<Self::StreamingIntentStream>, Status> {
        intent::streaming_intent(self, request).await
    }

    async fn streaming_knowledge_graph(
        &self,
        request: Request<Streaming<pb::StreamingKnowledgeGraphRequest>>,
    ) -> Result<Response<Self::StreamingKnowledgeGraphStream>, Status> {
        knowledgegraph::streaming_knowledge_graph(self, request).await
    }

    async fn streaming_intent_graph(
        &self,
        request: Request<Streaming<pb::StreamingIntentGraphRequest>>,
    ) -> Result<Response<Self::StreamingIntentGraphStream>, Status> {
        intent_graph::streaming_intent_graph(self, request).await
    }

    async fn streaming_connection_check(
        &self,
        request: Request<Streaming<pb::StreamingConnectionCheckRequest>>,
    ) -> Result<Response<Self::StreamingConnectionCheckStream>, Status> {
        connectioncheck::streaming_connection_check(self, request).await
    }
}
