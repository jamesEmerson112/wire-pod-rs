//! Turning tonic failures into `wirepod-core`'s [`ConnError`].
//!
//! The `Display` of a [`ConnError`] is a contract: Go writes
//! `"error: " + err.Error()` into nearly every `/api-sdk/*` failure body
//! (`server.go:61-62`), and for a gRPC failure grpc-go's `err.Error()` is
//! `rpc error: code = <Code> desc = <message>`. `tonic::Status` renders itself
//! differently, so the conversion happens here rather than at the edge, and
//! nothing outside this crate ever formats a `tonic::Status`.

use tonic::{Code, Status};
use wirepod_core::{ConnError, StatusCode};

/// The core status code a tonic [`Code`] names.
///
/// All 17 codes are mapped explicitly. tonic's `Code` has no other variants, so
/// there is no fallback arm and adding one upstream becomes a compile error
/// here rather than a silent `Unknown`.
pub const fn status_code(code: Code) -> StatusCode {
    match code {
        Code::Ok => StatusCode::Ok,
        Code::Cancelled => StatusCode::Canceled,
        Code::Unknown => StatusCode::Unknown,
        Code::InvalidArgument => StatusCode::InvalidArgument,
        Code::DeadlineExceeded => StatusCode::DeadlineExceeded,
        Code::NotFound => StatusCode::NotFound,
        Code::AlreadyExists => StatusCode::AlreadyExists,
        Code::PermissionDenied => StatusCode::PermissionDenied,
        Code::ResourceExhausted => StatusCode::ResourceExhausted,
        Code::FailedPrecondition => StatusCode::FailedPrecondition,
        Code::Aborted => StatusCode::Aborted,
        Code::OutOfRange => StatusCode::OutOfRange,
        Code::Unimplemented => StatusCode::Unimplemented,
        Code::Internal => StatusCode::Internal,
        Code::Unavailable => StatusCode::Unavailable,
        Code::DataLoss => StatusCode::DataLoss,
        Code::Unauthenticated => StatusCode::Unauthenticated,
    }
}

/// The [`ConnError`] a failed RPC produces.
///
/// The status message becomes the description verbatim, because that is the
/// text grpc-go prints after `desc = ` and the dashboard shows it.
pub fn status_error(status: &Status) -> ConnError {
    ConnError::new(status_code(status.code()), status.message())
}

/// The [`ConnError`] a failed dial produces.
///
/// grpc-go's dial failures surface at the first RPC rather than at the dial,
/// because `grpc.Dial` is lazy, so what Go's `newRobot` returns from its
/// `BatteryState` liveness check (`robot.go:365-369`) is an `Unavailable`
/// status whose description carries the transport error text. Mapping a tonic
/// transport failure the same way gives the same
/// `rpc error: code = Unavailable desc = ...` prefix in the response body.
///
/// The description is the whole `source` chain joined with `": "`, because
/// `tonic::transport::Error` alone renders as the useless `transport error`
/// while the cause underneath it names the address and the refusal. The exact
/// wording differs from Go's, which cannot be helped: it comes from the
/// operating system by way of a different runtime.
pub fn dial_error(err: &tonic::transport::Error) -> ConnError {
    let mut desc = err.to_string();
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(err);
    while let Some(cause) = source {
        desc.push_str(": ");
        desc.push_str(&cause.to_string());
        source = cause.source();
    }
    ConnError::new(StatusCode::Unavailable, desc)
}

/// The [`ConnError`] an endpoint that cannot be built produces.
///
/// A malformed target is not a robot failure, but every caller of the seam
/// expects a [`ConnError`], and `Unavailable` is what Go reports for a robot it
/// could not reach for any reason.
pub fn endpoint_error(desc: impl Into<String>) -> ConnError {
    ConnError::new(StatusCode::Unavailable, desc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_maps_to_its_go_name() {
        let pairs = [
            (Code::Ok, "OK"),
            (Code::Cancelled, "Canceled"),
            (Code::Unknown, "Unknown"),
            (Code::InvalidArgument, "InvalidArgument"),
            (Code::DeadlineExceeded, "DeadlineExceeded"),
            (Code::NotFound, "NotFound"),
            (Code::AlreadyExists, "AlreadyExists"),
            (Code::PermissionDenied, "PermissionDenied"),
            (Code::ResourceExhausted, "ResourceExhausted"),
            (Code::FailedPrecondition, "FailedPrecondition"),
            (Code::Aborted, "Aborted"),
            (Code::OutOfRange, "OutOfRange"),
            (Code::Unimplemented, "Unimplemented"),
            (Code::Internal, "Internal"),
            (Code::Unavailable, "Unavailable"),
            (Code::DataLoss, "DataLoss"),
            (Code::Unauthenticated, "Unauthenticated"),
        ];
        assert_eq!(pairs.len(), 17);
        for (code, name) in pairs {
            assert_eq!(status_code(code).go_name(), name);
            assert_eq!(status_code(code).as_wire(), code as i32);
        }
    }

    #[test]
    fn a_status_renders_as_grpc_go_prints_it() {
        let err = status_error(&Status::deadline_exceeded("context deadline exceeded"));
        assert_eq!(
            err.to_string(),
            "rpc error: code = DeadlineExceeded desc = context deadline exceeded"
        );
    }
}
