/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use core::convert::Infallible;
use std::io::{self, Cursor};
use std::net::TcpListener as StdTcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use crossbeam_channel::unbounded;
use embedder_traits::{EmbedderMsg, EmbedderProxy, EventLoopWaker, GenericEmbedderProxy};
use futures::future::ready;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request as HyperRequest, Response as HyperResponse};
use hyper_util::rt::tokio::TokioIo;
use net_traits::AsyncRuntime;
use net_traits::blob_url_store::UrlWithBlobClaim;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use servo_default_resources as _;
use servo_url::ServoUrl;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{self, TlsAcceptor};

use crate::async_runtime::{
    async_runtime_initialized, init_async_runtime, spawn_blocking_task, spawn_task,
};
pub use crate::hosts::replace_host_table;

static CRT_FILE: &[u8] = include_bytes!("tests/self_signed_certificate_for_testing.crt");
static PEM_FILE: &[u8] = include_bytes!("tests/privatekey_for_testing.key");

static ASYNC_RUNTIME: LazyLock<Arc<Mutex<Box<dyn AsyncRuntime>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(init_async_runtime())));

pub fn create_embedder_proxy() -> EmbedderProxy {
    create_generic_embedder_proxy::<EmbedderMsg>()
}

pub fn create_generic_embedder_proxy<T>() -> GenericEmbedderProxy<T> {
    if !async_runtime_initialized() {
        let _init = ASYNC_RUNTIME.clone();
    }
    let (sender, _) = unbounded();
    let event_loop_waker = || {
        struct DummyEventLoopWaker {}
        impl DummyEventLoopWaker {
            fn new() -> DummyEventLoopWaker {
                DummyEventLoopWaker {}
            }
        }
        impl EventLoopWaker for DummyEventLoopWaker {
            fn wake(&self) {}
            fn clone_box(&self) -> Box<dyn EventLoopWaker> {
                Box::new(DummyEventLoopWaker {})
            }
        }

        Box::new(DummyEventLoopWaker::new())
    };

    GenericEmbedderProxy {
        sender: sender,
        event_loop_waker: event_loop_waker(),
    }
}

#[derive(Debug)]
pub struct Server {
    pub close_channel: tokio::sync::oneshot::Sender<()>,
    pub certificates: Option<Vec<CertificateDer<'static>>>,
}

impl Server {
    pub fn close(self) {
        self.close_channel.send(()).expect("err closing server:");
    }
}

pub fn make_server<H>(handler: H) -> (Server, UrlWithBlobClaim)
where
    H: Fn(HyperRequest<Incoming>, &mut HyperResponse<BoxBody<Bytes, hyper::Error>>)
        + Send
        + Sync
        + 'static,
{
    if !async_runtime_initialized() {
        let _ = &*ASYNC_RUNTIME;
    }
    let handler = Arc::new(handler);

    let listener = StdTcpListener::bind("0.0.0.0:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let listener = spawn_blocking_task(async move { TcpListener::from_std(listener).unwrap() });

    let url_string = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let url = UrlWithBlobClaim::new(ServoUrl::parse(&url_string).unwrap(), None);

    let graceful = hyper_util::server::graceful::GracefulShutdown::new();

    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
    let server = async move {
        loop {
            let stream = tokio::select! {
                stream = listener.accept() => stream.unwrap().0,
                _val = &mut rx => {
                    let _ = graceful.shutdown();
                    break;
                }
            };

            let handler = handler.clone();

            let stream = stream.into_std().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::new(5, 0)))
                .unwrap();
            let stream = TcpStream::from_std(stream).unwrap();
            let http = http1::Builder::new();
            let conn = http.serve_connection(
                TokioIo::new(stream),
                service_fn(move |req: HyperRequest<Incoming>| {
                    let mut response =
                        HyperResponse::new(Empty::new().map_err(|_| unreachable!()).boxed());
                    handler(req, &mut response);
                    ready(Ok::<_, Infallible>(response))
                }),
            );
            let conn = graceful.watch(conn);
            spawn_task(async move {
                let _ = conn.await;
            });
        }
    };

    let _ = spawn_task(server);
    (
        Server {
            close_channel: tx,
            certificates: None,
        },
        url,
    )
}

/// Given a path to a file containing PEM certificates, load and parse them into
/// a vector of RusTLS [Certificate]s.
fn load_certificates_from_pem() -> Result<Vec<CertificateDer<'static>>, Box<dyn std::error::Error>>
{
    let mut cursor = Cursor::new(CRT_FILE);
    Ok(CertificateDer::pem_reader_iter(&mut cursor).collect::<Result<Vec<_>, _>>()?)
}

/// Given a path to a file containing PEM keys, load and parse them into
/// a vector of RusTLS [PrivateKey]s.
fn load_private_key_from_file() -> Result<PrivateKeyDer<'static>, Box<dyn std::error::Error>> {
    let mut cursor = Cursor::new(PEM_FILE);
    let mut keys =
        PrivatePkcs8KeyDer::pem_reader_iter(&mut cursor).collect::<Result<Vec<_>, _>>()?;

    match keys.len() {
        0 => Err(format!("No PKCS8-encoded private key found").into()),
        1 => Ok(PrivateKeyDer::try_from(keys.remove(0))?),
        _ => Err(format!("More than one PKCS8-encoded private key found").into()),
    }
}

pub fn make_ssl_server<H>(handler: H) -> (Server, UrlWithBlobClaim)
where
    H: Fn(HyperRequest<Incoming>, &mut HyperResponse<BoxBody<Bytes, hyper::Error>>)
        + Send
        + Sync
        + 'static,
{
    if !async_runtime_initialized() {
        let _ = &*ASYNC_RUNTIME;
    }
    let handler = Arc::new(handler);
    let listener = StdTcpListener::bind("[::0]:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let listener = spawn_blocking_task(async move { TcpListener::from_std(listener).unwrap() });

    let url_string = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let url = UrlWithBlobClaim::new(ServoUrl::parse(&url_string).unwrap(), None);

    let certificates = load_certificates_from_pem().expect("Invalid certificate");
    let key = load_private_key_from_file().expect("Invalid key");

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates.clone(), key)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))
        .expect("Could not create rustls ServerConfig");
    let acceptor = TlsAcceptor::from(Arc::new(config));

    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
    let server = async move {
        loop {
            let stream = tokio::select! {
                stream = listener.accept() => stream.unwrap().0,
                _ = &mut rx => break
            };

            let stream = stream.into_std().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::new(5, 0)))
                .unwrap();
            let stream = TcpStream::from_std(stream).unwrap();

            let handler = handler.clone();
            let acceptor = acceptor.clone();

            let stream = match acceptor.accept(stream).await {
                Ok(stream) => stream,
                Err(_) => {
                    eprintln!("Error handling TLS stream.");
                    continue;
                },
            };

            let _ = http1::Builder::new()
                .serve_connection(
                    TokioIo::new(stream),
                    service_fn(move |req: HyperRequest<Incoming>| {
                        let mut response =
                            HyperResponse::new(Empty::new().map_err(|_| unreachable!()).boxed());
                        handler(req, &mut response);
                        ready(Ok::<_, Infallible>(response))
                    }),
                )
                .await;
        }
    };

    spawn_task(server);

    (
        Server {
            close_channel: tx,
            certificates: Some(certificates),
        },
        url,
    )
}

/// What a [`make_counting_tls_server`] has seen.
#[derive(Default)]
pub struct ServerStats {
    /// TCP connections accepted.
    pub connections: AtomicUsize,
    /// TLS handshakes that resumed an earlier session.
    pub resumed: AtomicUsize,
    /// Requests answered.
    pub requests: AtomicUsize,
}

impl ServerStats {
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
    pub fn resumed(&self) -> usize {
        self.resumed.load(Ordering::SeqCst)
    }
    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

/// LucinaNet: an HTTPS server (the test certificate) that serves every connection at once, speaks HTTP/2 when
/// `h2` (ALPN `h2`) or HTTP/1.1 otherwise, answers each request after `delay`, and counts connections, resumed
/// handshakes and requests. `https://localhost:<port>`.
pub fn make_counting_tls_server(h2: bool, delay: Duration) -> (Server, ServoUrl, Arc<ServerStats>) {
    if !async_runtime_initialized() {
        let _ = &*ASYNC_RUNTIME;
    }
    let listener = StdTcpListener::bind("[::0]:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let listener = spawn_blocking_task(async move { TcpListener::from_std(listener).unwrap() });
    let url = ServoUrl::parse(&format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    ))
    .unwrap();

    let certificates = load_certificates_from_pem().expect("Invalid certificate");
    let key = load_private_key_from_file().expect("Invalid key");
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates.clone(), key)
        .expect("Could not create rustls ServerConfig");
    config.alpn_protocols = if h2 {
        vec![b"h2".to_vec()]
    } else {
        vec![b"http/1.1".to_vec()]
    };
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let stats = Arc::new(ServerStats::default());

    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
    let server_stats = stats.clone();
    spawn_task(async move {
        loop {
            let stream = tokio::select! {
                stream = listener.accept() => stream.unwrap().0,
                _ = &mut rx => break,
            };
            server_stats.connections.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            let stats = server_stats.clone();
            spawn_task(async move {
                let Ok(stream) = acceptor.accept(stream).await else {
                    return;
                };
                if matches!(
                    stream.get_ref().1.handshake_kind(),
                    Some(rustls::HandshakeKind::Resumed)
                ) {
                    stats.resumed.fetch_add(1, Ordering::SeqCst);
                }
                let service = service_fn(move |_req: HyperRequest<Incoming>| {
                    let stats = stats.clone();
                    async move {
                        tokio::time::sleep(delay).await;
                        stats.requests.fetch_add(1, Ordering::SeqCst);
                        Ok::<HyperResponse<BoxBody<Bytes, hyper::Error>>, Infallible>(
                            HyperResponse::new(
                                Full::new(Bytes::from_static(b"ok"))
                                    .map_err(|_| unreachable!())
                                    .boxed(),
                            ),
                        )
                    }
                });
                if h2 {
                    let _ = hyper::server::conn::http2::Builder::new(
                        hyper_util::rt::TokioExecutor::new(),
                    )
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
                } else {
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                }
            });
        }
    });

    (
        Server {
            close_channel: tx,
            certificates: Some(certificates),
        },
        url,
        stats,
    )
}

pub fn make_body(bytes: Vec<u8>) -> BoxBody<Bytes, hyper::Error> {
    Full::new(Bytes::from(bytes))
        .map_err(|_| unreachable!())
        .boxed()
}
