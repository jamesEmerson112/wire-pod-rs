//! The certificate route of `pkg/wirepod/config-ws/webserver.go`.

use axum::response::Response;

use crate::{literals, reply};

pub fn generate_certs() -> Response {
    // TODO(M5): botsetup.CreateCertCombo()
    reply::text(literals::DONE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::body_of;

    #[tokio::test]
    async fn the_web_ui_reads_the_body_as_the_word_done() {
        assert_eq!(body_of(generate_certs()).await, "done");
    }
}
