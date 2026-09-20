//! Serving the router on a plain TCP listener.
//!
//! Go binds its two plain listeners with `http.ListenAndServe`, port 80 from
//! `BeginServer` (`server.go:824`) and the configurable web port from
//! `StartWebServer` (`webserver.go:441`). Neither call is cancellable.
//!
//! [`serve_plain`] is the smallest thing that serves [`build_router`]'s value
//! and can be told to stop. It takes an already-bound listener rather than an
//! address so the caller decides what happens when the port is taken, and so a
//! test can bind an ephemeral port and read the number back.
//!
//! [`build_router`]: crate::build_router

use std::net::SocketAddr;

use axum::Router;
use axum::extract::ConnectInfo;
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::peer::PeerAddr;

/// Serves `router` on `listener` until `cancel` is cancelled.
///
/// The shutdown is graceful in axum's sense: the listener stops accepting as
/// soon as the token fires, and the call returns once every connection already
/// accepted has finished.
///
/// The service carries connect info, and [`attach_peer`] copies it into every
/// request as a [`PeerAddr`], which is what `connCheck` and the gRPC handlers
/// read where Go reads `r.RemoteAddr` and `peer.FromContext`.
///
/// The error is the accept loop's own. A bind failure cannot arise here,
/// because the listener is already bound.
pub async fn serve_plain(
    listener: TcpListener,
    router: Router,
    cancel: CancellationToken,
) -> std::io::Result<()> {
    let router = router.layer(middleware::from_fn(attach_peer));
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(cancel.cancelled_owned())
    .await
}

/// Copies the connection's remote address into the request extensions.
async fn attach_peer(mut req: Request, next: Next) -> Response {
    if let Some(ConnectInfo(addr)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() {
        req.extensions_mut().insert(PeerAddr(addr));
    }
    next.run(req).await
}
