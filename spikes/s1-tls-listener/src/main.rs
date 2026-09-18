//! Spike S1: prove the robot-facing listener stack.
//!
//! One TLS listener (rustls, optional ALPN) served by hyper-util's auto
//! builder (h2 preface sniffing when ALPN is absent -- the cmux equivalent),
//! routing gRPC `chippergrpc2.ChipperGrpc/StreamingConnectionCheck` plus the
//! HTTP `/ok` and `/ok:80` conn-check endpoints; a second plain-HTTP listener
//! for the robot's step-1 conncheck; optional mDNS `escapepod` registration.
//!
//! `--selftest` starts the servers on the configured ports and runs client
//! probes against them in-process (HTTPS /ok, HTTP /ok:80, and a real
//! StreamingConnectionCheck over TLS with certificate verification disabled,
//! exactly like the robot's grpc-go client).

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::Stream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status, Streaming};
use wirepod_proto::chippergrpc2::chipper_grpc_client::ChipperGrpcClient;
use wirepod_proto::chippergrpc2::chipper_grpc_server::{ChipperGrpc, ChipperGrpcServer};
use wirepod_proto::chippergrpc2::{
    ConnectionCheckResponse, IntentGraphResponse, IntentResponse, KnowledgeGraphResponse,
    StreamingConnectionCheckRequest, TextRequest,
};

#[derive(Clone, Copy)]
struct Args {
    tls_port: u16,
    http_port: u16,
    alpn: bool,
    mdns: bool,
    selftest: bool,
}

// The escape-pod key pair the listener serves. `assets/epod/` is the copy
// vendored byte-identically from the Go repo, so the spike no longer depends
// on a Go checkout sitting at one particular absolute path. The manifest dir
// is two levels under the workspace root, and resolving from it rather than
// from the working directory is what `xtask::repo_root` does for the same
// reason. `--cert` and `--key` still override both.
const DEFAULT_CERT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/epod/ep.crt");
const DEFAULT_KEY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/epod/ep.key");

fn parse_args() -> (Args, String, String) {
    let mut a = Args {
        tls_port: 443,
        http_port: 80,
        alpn: true,
        mdns: false,
        selftest: false,
    };
    let mut cert = DEFAULT_CERT.to_string();
    let mut key = DEFAULT_KEY.to_string();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut it = argv.iter();
    while let Some(f) = it.next() {
        match f.as_str() {
            "--tls-port" => a.tls_port = it.next().unwrap().parse().unwrap(),
            "--http-port" => a.http_port = it.next().unwrap().parse().unwrap(),
            "--cert" => cert = it.next().unwrap().clone(),
            "--key" => key = it.next().unwrap().clone(),
            "--alpn" => a.alpn = it.next().unwrap() == "on",
            "--mdns" => a.mdns = true,
            "--selftest" => a.selftest = true,
            other => panic!("unknown flag {other}"),
        }
    }
    (a, cert, key)
}

#[tokio::main]
async fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install ring crypto provider");
    let (args, cert, key) = parse_args();

    let router = build_router();
    tokio::spawn(serve_tls(args, cert.clone(), key.clone(), router.clone()));
    tokio::spawn(serve_plain(args, router));

    let _mdns_guard = if args.mdns {
        Some(register_mdns())
    } else {
        None
    };

    if args.selftest {
        tokio::time::sleep(Duration::from_millis(400)).await; // let listeners bind
        run_selftest(args).await;
        println!(
            "S1 SELFTEST PASS (alpn={})",
            if args.alpn { "on" } else { "off" }
        );
        return;
    }

    println!(
        "s1-tls-listener up: tls :{} (alpn {}), http :{}, mdns {}",
        args.tls_port,
        if args.alpn { "on" } else { "off" },
        args.http_port,
        if args.mdns { "on" } else { "off" }
    );
    tokio::signal::ctrl_c().await.ok();
}

// ---------------- server side ----------------

fn build_router() -> axum::Router {
    let grpc = ChipperGrpcServer::new(ChipperSvc);
    tonic::service::Routes::new(grpc)
        .into_axum_router()
        .route("/ok", axum::routing::any(ok_handler))
        // "/ok:80" contains a legacy literal colon; handle it in the fallback
        // to avoid any router-syntax ambiguity.
        .fallback(fallback_handler)
}

async fn ok_handler() -> &'static str {
    println!("[hit] /ok");
    "ok"
}

async fn fallback_handler(req: http::Request<axum::body::Body>) -> axum::response::Response {
    if req.uri().path() == "/ok:80" {
        println!("[hit] /ok:80");
        return axum::response::IntoResponse::into_response("ok");
    }
    println!("[miss] {} {}", req.method(), req.uri().path());
    axum::response::IntoResponse::into_response((http::StatusCode::NOT_FOUND, "not found"))
}

fn load_tls(cert: &str, key: &str, alpn: bool) -> Arc<rustls::ServerConfig> {
    let certs: Vec<_> = rustls_pemfile::certs(&mut std::io::BufReader::new(
        std::fs::File::open(cert).expect("open cert"),
    ))
    .collect::<Result<_, _>>()
    .expect("parse cert");
    let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(
        std::fs::File::open(key).expect("open key"),
    ))
    .expect("read key")
    .expect("no private key found");
    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("tls config");
    if alpn {
        cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    }
    Arc::new(cfg)
}

async fn serve_tls(args: Args, cert: String, key: String, router: axum::Router) {
    let tls_cfg = load_tls(&cert, &key, args.alpn);
    let acceptor = tokio_rustls::TlsAcceptor::from(tls_cfg);
    let addr = SocketAddr::from(([0, 0, 0, 0], args.tls_port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("bind tls :{}: {e}", args.tls_port));
    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            continue;
        };
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            match acceptor.accept(tcp).await {
                Ok(tls) => {
                    println!("[tls] conn from {peer}");
                    let io = hyper_util::rt::TokioIo::new(tls);
                    let svc = hyper_util::service::TowerToHyperService::new(router);
                    let builder = hyper_util::server::conn::auto::Builder::new(
                        hyper_util::rt::TokioExecutor::new(),
                    );
                    if let Err(e) = builder.serve_connection(io, svc).await {
                        println!("[tls] conn {peer} ended: {e}");
                    }
                }
                Err(e) => println!("[tls] handshake from {peer} failed: {e}"),
            }
        });
    }
}

async fn serve_plain(args: Args, router: axum::Router) {
    let addr = SocketAddr::from(([0, 0, 0, 0], args.http_port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("bind http :{}: {e}", args.http_port));
    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            continue;
        };
        let router = router.clone();
        tokio::spawn(async move {
            println!("[http] conn from {peer}");
            let io = hyper_util::rt::TokioIo::new(tcp);
            let svc = hyper_util::service::TowerToHyperService::new(router);
            let builder =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
            let _ = builder.serve_connection(io, svc).await;
        });
    }
}

fn outbound_ip() -> std::net::IpAddr {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").expect("udp bind");
    sock.connect("8.8.8.8:80").expect("udp connect");
    sock.local_addr().expect("local addr").ip()
}

fn register_mdns() -> mdns_sd::ServiceDaemon {
    let daemon = mdns_sd::ServiceDaemon::new().expect("mdns daemon");
    let ip = outbound_ip();
    let props = [("txtv", "0"), ("lo", "1"), ("la", "2")];
    let info = mdns_sd::ServiceInfo::new(
        "_app-proto._tcp.local.",
        "escapepod",
        "escapepod.local.",
        ip,
        8084,
        &props[..],
    )
    .expect("mdns service info");
    daemon.register(info).expect("mdns register");
    println!("[mdns] registered escapepod / _app-proto._tcp -> {ip}:8084");
    daemon
}

struct ChipperSvc;

type RespStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl ChipperGrpc for ChipperSvc {
    type StreamingIntentStream = RespStream<IntentResponse>;
    type StreamingKnowledgeGraphStream = RespStream<KnowledgeGraphResponse>;
    type StreamingIntentGraphStream = RespStream<IntentGraphResponse>;
    type StreamingConnectionCheckStream = RespStream<ConnectionCheckResponse>;

    async fn text_intent(
        &self,
        _request: Request<TextRequest>,
    ) -> Result<Response<IntentResponse>, Status> {
        Err(Status::unimplemented("no text intents"))
    }

    async fn streaming_intent(
        &self,
        _request: Request<Streaming<wirepod_proto::chippergrpc2::StreamingIntentRequest>>,
    ) -> Result<Response<Self::StreamingIntentStream>, Status> {
        Err(Status::unimplemented("spike"))
    }

    async fn streaming_knowledge_graph(
        &self,
        _request: Request<Streaming<wirepod_proto::chippergrpc2::StreamingKnowledgeGraphRequest>>,
    ) -> Result<Response<Self::StreamingKnowledgeGraphStream>, Status> {
        Err(Status::unimplemented("spike"))
    }

    async fn streaming_intent_graph(
        &self,
        _request: Request<Streaming<wirepod_proto::chippergrpc2::StreamingIntentGraphRequest>>,
    ) -> Result<Response<Self::StreamingIntentGraphStream>, Status> {
        Err(Status::unimplemented("spike"))
    }

    async fn streaming_connection_check(
        &self,
        request: Request<Streaming<StreamingConnectionCheckRequest>>,
    ) -> Result<Response<Self::StreamingConnectionCheckStream>, Status> {
        let mut stream = request.into_inner();
        let first = stream
            .next()
            .await
            .ok_or_else(|| Status::invalid_argument("empty stream"))?
            .map_err(|e| Status::internal(e.to_string()))?;
        let expected = if first.audio_per_request == 0 {
            1
        } else {
            first.total_audio_ms / first.audio_per_request
        };
        println!(
            "[grpc] connection check from device {} fw {} (expecting {expected} frames)",
            first.device_id, first.firmware_version
        );
        let mut frames_received: u32 = 1;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut timed_out = false;
        while frames_received < expected {
            match tokio::time::timeout_at(deadline, stream.next()).await {
                Ok(Some(Ok(_))) => frames_received += 1,
                Ok(Some(Err(_)) | None) => break,
                Err(_) => {
                    timed_out = true;
                    break;
                }
            }
        }
        let status = if timed_out { "Timeout" } else { "Success" };
        println!("[grpc] connection check done: {status}, {frames_received} frames");
        let resp = ConnectionCheckResponse {
            status: status.to_string(),
            frames_received,
        };
        Ok(Response::new(Box::pin(tokio_stream::once(Ok(resp)))))
    }
}

// ---------------- self-test client side ----------------

#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn insecure_client_config(alpn_h2: bool) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
        .with_no_client_auth();
    if alpn_h2 {
        cfg.alpn_protocols = vec![b"h2".to_vec()];
    }
    Arc::new(cfg)
}

async fn tls_connect(
    port: u16,
    alpn_h2: bool,
) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("tcp connect");
    let connector = tokio_rustls::TlsConnector::from(insecure_client_config(alpn_h2));
    let name = rustls::pki_types::ServerName::try_from("escapepod.local").unwrap();
    connector.connect(name, tcp).await.expect("tls connect")
}

async fn run_selftest(args: Args) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // (a) HTTPS GET /ok over TLS (http/1.1)
    let mut tls = tls_connect(args.tls_port, false).await;
    tls.write_all(b"GET /ok HTTP/1.1\r\nHost: escapepod.local\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    tls.read_to_end(&mut buf).await.ok();
    let text = String::from_utf8_lossy(&buf);
    assert!(text.starts_with("HTTP/1.1 200"), "https /ok failed: {text}");
    assert!(text.ends_with("ok"), "https /ok body: {text}");
    println!("selftest: https /ok -> 200 ok");

    // (b) plain HTTP GET /ok:80
    let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", args.http_port))
        .await
        .expect("plain connect");
    tcp.write_all(b"HEAD /ok:80?emresn=selftest HTTP/1.1\r\nHost: escapepod.local\r\nUser-Agent: Victor-CCHECK/selftest\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    tcp.read_to_end(&mut buf).await.ok();
    let text = String::from_utf8_lossy(&buf);
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "http /ok:80 failed: {text}"
    );
    println!("selftest: http /ok:80 -> 200");

    // (c) real gRPC StreamingConnectionCheck over TLS (client offers h2 ALPN;
    // with --alpn off the server answers none and hyper's preface sniff must
    // carry it -- the exact cmux-parity scenario).
    let port = args.tls_port;
    let connector = tower::service_fn(move |_uri: hyper::Uri| async move {
        let tls = tls_connect(port, true).await;
        Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(tls))
    });
    let channel = tonic::transport::Endpoint::from_static("http://escapepod.local")
        .connect_with_connector(connector)
        .await
        .expect("grpc channel");
    let mut client = ChipperGrpcClient::new(channel);
    let frames = vec![
        StreamingConnectionCheckRequest {
            session: "selftest".into(),
            device_id: "00303f28".into(),
            input_audio: vec![0u8; 3200],
            firmware_version: "selftest".into(),
            app_key: "oDoa0quieSeir6goowai7f".into(),
            total_audio_ms: 200,
            audio_per_request: 100,
        },
        StreamingConnectionCheckRequest {
            input_audio: vec![0u8; 3200],
            ..Default::default()
        },
    ];
    let resp = client
        .streaming_connection_check(tokio_stream::iter(frames))
        .await
        .expect("connection check rpc");
    let mut stream = resp.into_inner();
    let answer = stream
        .next()
        .await
        .expect("one response")
        .expect("response ok");
    assert_eq!(answer.status, "Success", "connection check status");
    assert_eq!(answer.frames_received, 2, "frames received");
    println!(
        "selftest: grpc StreamingConnectionCheck -> {} ({} frames)",
        answer.status, answer.frames_received
    );
}
