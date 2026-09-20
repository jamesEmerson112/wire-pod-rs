//! Go's `pkg/initwirepod/startserver.go`: the TLS listeners, the gRPC and HTTP serving, start, stop and restart.

use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::Request;
use axum::response::Response;
use axum::routing::any;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use tonic::service::Routes;
use tower::ServiceExt;
use wirepod_core::AppState;
use wirepod_proto::chippergrpc2::chipper_grpc_server::ChipperGrpcServer;
use wirepod_proto::jdocspb::jdocs_server::JdocsServer;
use wirepod_proto::tokenpb::token_server::TokenServer;

use crate::chipper::Server;
use crate::jdocs::server::new_jdocs_server;
use crate::peer::PeerAddr;
use crate::token::new_token_server;
use crate::{literals, mdns, reply};

/// Go's second listener, for 2.0.1 compatibility.
const PORT_8084: u16 = 8084;

const OK: &str = "/ok";

/// The conn-check path carrying a literal colon, which axum's matcher reads as
/// a path parameter, so the fallback compares it by hand.
const OK_COLON_80: &str = "/ok:80";

const NOT_SETUP: &str = concat!(
    "\x1b[33m\x1b[1mWire-pod is not setup. ",
    "Use the webserver at port 8080 to set up wire-pod.\x1b[0m"
);

/// Go's `serverOne`, `serverTwo`, `listenerOne`, `listenerTwo` and
/// `chipperServing` globals, which are one running server here.
static SERVING: Mutex<Option<Serving>> = Mutex::new(None);

/// The service `RestartServer` starts again, which Go rebuilds from globals.
static CHIPPER: Mutex<Option<Server>> = Mutex::new(None);

struct Serving {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

async fn serve_ok() -> Response {
    reply::text(literals::OK)
}

/// Go's `httpServe`. cmux splits HTTP/1 off the TLS listener and serves it from
/// its own mux; here the two conn-check routes join the one router.
fn http_serve(router: Router) -> Router {
    router.route(OK, any(serve_ok)).fallback(fallback)
}

async fn fallback(req: Request) -> Response {
    if req.uri().path() == OK_COLON_80 {
        return reply::text(literals::OK);
    }
    reply::file_not_found()
}

/// Go's `grpcServe`.
fn grpc_serve(state: Arc<AppState>, chipper: Server) -> Router {
    Routes::new(ChipperGrpcServer::new(chipper))
        .add_service(JdocsServer::new(new_jdocs_server(Arc::clone(&state))))
        .add_service(TokenServer::new(new_token_server(state)))
        .into_axum_router()
}

/// The one router both listeners serve.
pub fn build_router(state: Arc<AppState>, chipper: Server) -> Router {
    http_serve(grpc_serve(state, chipper))
}

/// The key pair Go hands `tls.Listen`.
///
/// Go advertises no ALPN and lets cmux sniff the protocol; rustls advertises
/// both and hyper-util's auto builder sniffs a client that offers neither.
pub fn load_tls(cert: &Path, key: &Path) -> io::Result<Arc<rustls::ServerConfig>> {
    let certs = rustls_pemfile::certs(&mut io::BufReader::new(std::fs::File::open(cert)?))
        .collect::<Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut io::BufReader::new(std::fs::File::open(key)?))?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no private key found"))?;
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// The listener half of `StartChipper`, over listeners the caller has already
/// bound, so a test can hand it `127.0.0.1:0`.
pub async fn serve_listeners(
    listeners: Vec<TcpListener>,
    tls: Arc<rustls::ServerConfig>,
    router: Router,
    cancel: CancellationToken,
) {
    let acceptor = TlsAcceptor::from(tls);
    let mut tasks = Vec::new();
    for listener in listeners {
        tasks.push(tokio::spawn(accept_loop(
            listener,
            acceptor.clone(),
            router.clone(),
            cancel.clone(),
        )));
    }
    for task in tasks {
        let _ = task.await;
    }
    tracing::info!("Stopping chipper server");
}

async fn accept_loop(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    router: Router,
    cancel: CancellationToken,
) {
    loop {
        let accepted = tokio::select! {
            () = cancel.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let Ok((tcp, peer)) = accepted else {
            continue;
        };
        tokio::spawn(serve_conn(acceptor.clone(), tcp, peer, router.clone()));
    }
}

async fn serve_conn(acceptor: TlsAcceptor, tcp: TcpStream, peer: SocketAddr, router: Router) {
    let tls = match acceptor.accept(tcp).await {
        Ok(tls) => tls,
        Err(err) => {
            tracing::debug!("tls handshake from {peer} failed: {err}");
            return;
        }
    };
    tracing::debug!("tls connection from {peer}");
    let service =
        hyper::service::service_fn(move |mut req: http::Request<hyper::body::Incoming>| {
            // Go reads the peer with `peer.FromContext`; here the accept loop is
            // the only place that knows it, so it goes into the extensions.
            tracing::debug!("{peer} {} {}", req.method(), req.uri().path());
            req.extensions_mut().insert(PeerAddr(peer));
            let router = router.clone();
            async move { router.oneshot(req).await }
        });
    let builder = auto::Builder::new(TokioExecutor::new());
    if let Err(err) = builder.serve_connection(TokioIo::new(tls), service).await {
        tracing::debug!("connection {peer} ended: {err}");
    }
}

pub fn begin_wirepod_specific() -> io::Result<()> {
    // TODO(M2): logger.Init()
    // TODO(M2): vars.Init()
    // TODO(M3): wp.New(sttInitFunc, sttHandlerFunc, voiceProcessorName)
    // TODO(M3): wpweb.SttInitFunc = sttInitFunc
    // TODO(M2): go sdkWeb.BeginServer()
    Ok(())
}

/// Go's android and ios branches are dropped throughout: this port builds for
/// Windows and Linux.
pub async fn start_from_program_init(state: Arc<AppState>, chipper: Server) {
    *CHIPPER.lock().unwrap_or_else(|err| err.into_inner()) = Some(chipper.clone());
    let config = state.config();
    if begin_wirepod_specific().is_err() {
        tracing::info!("{NOT_SETUP}");
    } else if !config.past_initial_setup {
        tracing::info!("{NOT_SETUP}");
    } else if (config.stt.provider == "vosk" || config.stt.provider == "whisper.cpp")
        && config.stt.language.is_empty()
    {
        tracing::info!(
            "\x1b[33m\x1b[1mLanguage value is blank, but STT service is {}. Reinitiating setup process.\x1b[0m",
            config.stt.provider
        );
        tracing::info!("{NOT_SETUP}");
        state.update_config(|config| config.past_initial_setup = false);
    } else if let Err(err) = start_chipper(&state, chipper).await {
        tracing::info!("{err}");
    }
    // TODO(M2): wpweb.StartWebServer()
}

pub async fn restart_server(state: &Arc<AppState>) -> io::Result<()> {
    stop_server().await;
    let chipper = CHIPPER
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone();
    match chipper {
        Some(chipper) => start_chipper(state, chipper).await,
        None => Err(io::Error::other(
            "the chipper service was never initialised",
        )),
    }
}

pub async fn stop_server() {
    let serving = SERVING.lock().unwrap_or_else(|err| err.into_inner()).take();
    if let Some(serving) = serving {
        serving.cancel.cancel();
        // Go closes the listeners and moves on; the old task has to finish here
        // or a rebind races it for the port.
        let _ = serving.task.await;
    }
}

/// Go's `StartChipper`, which blocks in `Serve`. Here it returns once the
/// listeners are bound and serving, so the caller can stop or restart them.
pub async fn start_chipper(state: &Arc<AppState>, chipper: Server) -> io::Result<()> {
    let config = state.config();
    if config.server.epconfig {
        tokio::spawn(mdns::post_mdns());
    }
    let (cert_path, key_path) = if config.server.epconfig {
        (
            state.paths().assets().epod_cert_path(),
            state.paths().assets().epod_key_path(),
        )
    } else {
        (
            state.paths().data().cert_path(),
            state.paths().data().key_path(),
        )
    };

    tracing::info!("Initiating TLS listener, gRPC handler, and REST handler");
    // Go exits the process when the key pair will not load.
    let tls = load_tls(&cert_path, &key_path)?;

    tracing::info!("Starting chipper server at port {}", config.server.port);
    let mut listeners = vec![TcpListener::bind(format!("0.0.0.0:{}", config.server.port)).await?];
    if config.server.epconfig && std::env::var("NO8084").as_deref() != Ok("true") {
        tracing::info!("Starting chipper server at port 8084 for 2.0.1 compatibility");
        listeners.push(TcpListener::bind(("0.0.0.0", PORT_8084)).await?);
    }

    let cancel = CancellationToken::new();
    let task = tokio::spawn(serve_listeners(
        listeners,
        tls,
        build_router(Arc::clone(state), chipper),
        cancel.clone(),
    ));
    *SERVING.lock().unwrap_or_else(|err| err.into_inner()) = Some(Serving { cancel, task });

    tracing::info!("\x1b[33m\x1b[1mwire-pod started successfully!\x1b[0m");
    Ok(())
}
