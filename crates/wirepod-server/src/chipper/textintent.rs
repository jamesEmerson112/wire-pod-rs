//! Go's `servers/chipper/textintent.go`: the text intent RPC, which is unimplemented.

use tonic::{Request, Response, Status};
use wirepod_proto::chippergrpc2 as pb;

use crate::chipper::server::Server;

/// TextIntent handles text-based request/responses from the device
pub async fn text_intent(
    _server: &Server,
    _request: Request<pb::TextRequest>,
) -> Result<Response<pb::IntentResponse>, Status> {
    Err(Status::unimplemented(""))
}
