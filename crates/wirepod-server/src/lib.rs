//! The HTTP surface of the SDK app: the router, the request preamble and the
//! handlers behind `/api-sdk/*`, `/api/*` and `/ok`.
//!
//! Go serves this whole surface from `http.DefaultServeMux` on two listeners,
//! port 80 from `BeginServer` (`server.go:824`) and the configurable web port
//! from `StartWebServer` (`webserver.go:441`), so every route answers on both.
//! [`router::build_router`] builds the union once and [`router::listener_specs`]
//! names the two listeners that will serve it; nothing in this crate binds a
//! port.
//!
//! Three things about this surface are contract rather than convenience. The
//! bodies are byte-exact, because the vendored web UI reads several of them as
//! literal strings, so every one of them is a constant in [`literals`] with a
//! verbatim test. No handler ever inspects the request method. And parameters
//! come from Go's `FormValue` merge rather than from the query string alone,
//! which [`form`] reproduces: the dashboard POSTs `serial` in the query and
//! other parameters in a urlencoded body, so both have to be read.
//!
//! The TLS listener, the tonic services, mDNS and the restart supervisor are
//! the rest of this crate's eventual responsibility and arrive with P1.
#![deny(clippy::await_holding_lock)]

pub mod api;
pub mod conncheck;
pub mod form;
pub mod literals;
pub mod reply;
pub mod router;
pub mod sdkapp;
#[cfg(feature = "test-util")]
pub mod test_support;

pub use crate::form::Form;
pub use crate::router::{
    CONN_CHECK_PORT, DEFAULT_WEB_PORT, ListenerSpec, build_router, listener_specs,
};
pub use crate::sdkapp::SLICE_ROUTES;
