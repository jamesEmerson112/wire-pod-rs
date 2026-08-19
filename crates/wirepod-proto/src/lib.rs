//! Generated gRPC/protobuf code for the wire-pod external contract.
//!
//! - `chippergrpc2` — the inbound voice service the robot dials
//! - `jdocspb` / `tokenpb` — the inbound jdocs and token services
//! - `anki::vector::external_interface` — the outbound robot SDK surface

pub mod chippergrpc2 {
    tonic::include_proto!("chippergrpc2");
}

pub mod jdocspb {
    tonic::include_proto!("jdocspb");
}

pub mod tokenpb {
    tonic::include_proto!("tokenpb");
}

pub mod anki {
    pub mod vector {
        pub mod external_interface {
            tonic::include_proto!("anki.vector.external_interface");
        }
    }
}
