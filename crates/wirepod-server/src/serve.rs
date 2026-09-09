//! Serving the router on a plain TCP listener.
//!
//! Go binds its two plain listeners with `http.ListenAndServe`, port 80 from
//! `BeginServer` (`server.go:824`) and the configurable web port from
//! `StartWebServer` (`webserver.go:441`). Neither call is cancellable: the
//! chipper listeners are the ones `RestartServer` closes
//! (`initwirepod/startserver.go:120-128`), and these two run for the process
//! lifetime.
//!
//! [`serve_plain`] is the smallest thing that serves [`build_router`]'s value
//! and can be told to stop, which is the seed of P1's supervisor. It takes an
//! already-bound listener rather than an address so the caller decides what
//! happens when the port is taken, and so a test can bind an ephemeral port and
//! read the number back. It takes the token rather than a future so that one
//! token can eventually stop several listeners at once, which is what
//! `RestartServer` closing four handles turns into.
//!
//! What is deliberately absent is everything P1 adds around it: TLS, the tonic
//! services, the second listener, and awaiting the old task before rebinding.
//! This is a function, not a supervisor.
//!
//! [`build_router`]: crate::build_router

use axum::Router;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Serves `router` on `listener` until `cancel` is cancelled.
///
/// The shutdown is graceful in axum's sense: the listener stops accepting as
/// soon as the token fires, and the call returns once every connection already
/// accepted has finished. A request in flight is therefore answered rather than
/// cut off, which matters for `/api-sdk/disconnect`, whose settle keeps a
/// request parked for three seconds.
///
/// The router is turned into a make-service with no connect info, because no
/// handler on this surface reads the peer address. Go's `connCheck` does read
/// it, for the jdocs pinger bookkeeping and the mDNS re-announce
/// (`jdocspinger.go:193-221`), and both of those are deferred to P1 as
/// deviation 2. Whichever commit un-defers them changes this line to
/// `into_make_service_with_connect_info::<SocketAddr>()` and adds the extractor
/// to that one handler.
///
/// The error is the accept loop's own. A bind failure cannot arise here,
/// because the listener is already bound.
pub async fn serve_plain(
    listener: TcpListener,
    router: Router,
    cancel: CancellationToken,
) -> std::io::Result<()> {
    axum::serve(listener, router.into_make_service())
        .with_graceful_shutdown(cancel.cancelled_owned())
        .await
}
