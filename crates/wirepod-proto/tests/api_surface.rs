//! Compile-time assertions that the load-bearing services and RPC types exist.

#[allow(unused_imports)]
use wirepod_proto::{
    anki::vector::external_interface::{
        BatteryStateRequest, BehaviorControlRequest, EventRequest, SayTextRequest,
        external_interface_client::ExternalInterfaceClient,
    },
    chippergrpc2::{
        ConnectionCheckResponse, IntentGraphResponse, IntentResponse, KnowledgeGraphResponse,
        StreamingConnectionCheckRequest, StreamingIntentGraphRequest, StreamingIntentRequest,
        StreamingKnowledgeGraphRequest, TextRequest,
        chipper_grpc_server::{ChipperGrpc, ChipperGrpcServer},
    },
    jdocspb::{
        ReadDocsReq, ReadDocsResp, WriteDocReq, WriteDocResp,
        jdocs_server::{Jdocs, JdocsServer},
    },
    tokenpb::{
        AssociatePrimaryUserRequest, AssociatePrimaryUserResponse,
        token_server::{Token, TokenServer},
    },
};

#[test]
fn api_surface_compiles() {
    // The imports above are the assertion; this test passes if the crate links.
}
