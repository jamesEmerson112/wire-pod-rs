//! Translation of `pkg/wirepod/speechrequest/speechrequest.go`.

use async_trait::async_trait;
use wirepod_core::ConnError;

/// Go keeps the gRPC stream in a `Stream interface{}` and type-switches it in
/// six places. The three tonic stream types are distinct, so each gets a small
/// adapter and the request reads through this instead.
#[async_trait]
pub trait ChunkSource: Send {
    /// One `stream.Recv()`: the next audio chunk, or `None` at end of stream.
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, ConnError>;
}
