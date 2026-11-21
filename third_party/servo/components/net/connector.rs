/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::RefCell;
use std::collections::hash_map::HashMap;
use std::convert::TryFrom;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use std::{fmt, io};

use futures::task::{Context, Poll};
use futures::{Future, TryFutureExt};
use http::uri::{Authority, Uri as Destination};
use http_body_util::combinators::BoxBody;
use hyper::body::Bytes;
use hyper::rt::Executor;
use hyper_rustls::{HttpsConnector as HyperRustlsHttpsConnector, MaybeHttpsStream};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::dns::Name;
use hyper_util::client::legacy::connect::proxy::Tunnel;
use hyper_util::client::legacy::connect::{
    Connected, Connection, HttpConnector as HyperHttpConnector,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use log::warn;
use parking_lot::Mutex;
use rustls::client::danger::ServerCertVerifier;
use rustls::client::{ClientConnection, EchStatus, Resumption};
use rustls::crypto::CryptoProvider;
use rustls::{CipherSuite, ClientConfig, NamedGroup, ProtocolVersion};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use servo_config::pref;
use tokio::net::TcpStream;
use tower::Service;

use crate::async_runtime::spawn_task;
use crate::dns_cache::{BoxFuture, DnsCache, NameLookup, Resolver};
use crate::hosts::replace_host;
use crate::origin_gate::{ConnSlot, OriginGate, OriginKey};

pub const BUF_SIZE: usize = 32768;

/// ALPN identifier for HTTP/2 (RFC 7540 §3.1).
pub const ALPN_H2: &str = "h2";

/// Name lookups as Chromium's system resolver makes them on POSIX: `getaddrinfo` with `AI_ADDRCONFIG`,
/// so that a host with no IPv6 asks for A records only. hyper's `GaiResolver` asks for every family:
/// one more query and one more answer to wait for, for addresses that cannot be reached. On Juno,
/// which has no IPv6, that second answer is sometimes lost and musl waits 2.5 s to ask again.
///
/// LucinaNet: the answers come from a [`Resolver`] (by default a [`DnsCache`] over `getaddrinfo`), so a name is
/// asked once a minute and once at a time.
#[derive(Clone)]
pub struct AddrConfigResolver {
    resolver: Arc<dyn Resolver>,
}

impl AddrConfigResolver {
    pub fn new(resolver: Arc<dyn Resolver>) -> Self {
        Self { resolver }
    }
}

impl Service<Name> for AddrConfigResolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = io::Error;
    type Future = std::pin::Pin<Box<dyn Future<Output = io::Result<Self::Response>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: Name) -> Self::Future {
        let host = name.as_str().to_owned();
        let answer = self.resolver.resolve(&host);
        Box::pin(async move {
            let started = Instant::now();
            let answer = answer.await;
            net_phase(&format!("dns-wait {host}"), started);
            match answer {
                Ok(addrs) => Ok(addrs.to_vec().into_iter()),
                Err(error) => Err(error.to_io()),
            }
        })
    }
}

/// The blocking lookup under the cache: `getaddrinfo` with `AI_ADDRCONFIG`.
pub struct GetAddrInfo;

impl NameLookup for GetAddrInfo {
    fn lookup(&self, host: &str) -> io::Result<Vec<SocketAddr>> {
        lookup_host(host).map(Iterator::collect)
    }
}

/// LucinaNet: with `LUCINANET_TIMING`, one line per step of a connection (`netphase <uptime> <step> <ms>`), the
/// steps Servo's navigation timing leaves at zero (domain lookup, connect, secure connection).
pub fn net_phase(step: &str, started: std::time::Instant) {
    static ON: LazyLock<bool> = LazyLock::new(|| std::env::var_os("LUCINANET_TIMING").is_some());
    if *ON {
        // CLOCK_MONOTONIC to the microsecond, the clock LucinaNet's own `phase` lines use.
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: a valid clock id and a timespec to fill.
        #[allow(unsafe_code)]
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now)
        };
        eprintln!(
            "netphase {}.{:06} {step} {:.1} ms",
            now.tv_sec,
            now.tv_nsec / 1000,
            started.elapsed().as_secs_f64() * 1e3
        );
    }
}

#[allow(unsafe_code)]
fn lookup_host(host: &str) -> io::Result<std::vec::IntoIter<SocketAddr>> {
    let name = std::ffi::CString::new(host)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a host name with a NUL"))?;
    // SAFETY: an all-zero `addrinfo` is a valid set of hints.
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_flags = libc::AI_ADDRCONFIG;
    hints.ai_socktype = libc::SOCK_STREAM;
    let mut found = std::ptr::null_mut();
    // SAFETY: a C string, the hints, and an out pointer whose list is freed below.
    let result = unsafe { libc::getaddrinfo(name.as_ptr(), std::ptr::null(), &hints, &mut found) };
    if result != 0 {
        return Err(io::Error::other(format!(
            "failed to look up address information: getaddrinfo error {result}"
        )));
    }
    let mut addrs = Vec::new();
    let mut next = found;
    while !next.is_null() {
        // SAFETY: a node of the list getaddrinfo returned, whose address matches its family.
        unsafe {
            let info = &*next;
            match info.ai_family {
                libc::AF_INET => {
                    let sin = &*(info.ai_addr as *const libc::sockaddr_in);
                    let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                    addrs.push(SocketAddr::from((ip, 0)));
                },
                libc::AF_INET6 => {
                    let sin6 = &*(info.ai_addr as *const libc::sockaddr_in6);
                    let ip = std::net::Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                    addrs.push(SocketAddr::V6(std::net::SocketAddrV6::new(
                        ip,
                        0,
                        sin6.sin6_flowinfo,
                        sin6.sin6_scope_id,
                    )));
                },
                _ => {},
            }
            next = info.ai_next;
        }
    }
    // SAFETY: the list getaddrinfo returned, freed once.
    unsafe { libc::freeaddrinfo(found) };
    Ok(addrs.into_iter())
}

#[derive(Clone)]
pub struct ServoHttpConnector {
    inner: HyperHttpConnector<AddrConfigResolver>,
}

impl ServoHttpConnector {
    fn new(resolver: Arc<dyn Resolver>) -> ServoHttpConnector {
        let mut inner = HyperHttpConnector::new_with_resolver(AddrConfigResolver::new(resolver));
        inner.enforce_http(false);
        inner.set_happy_eyeballs_timeout(None);
        inner.set_connect_timeout(Some(Duration::from_secs(pref!(network_connection_timeout))));
        ServoHttpConnector { inner }
    }
}

impl Service<Destination> for ServoHttpConnector {
    type Response = TokioIo<TcpStream>;
    type Error = ConnectionError;
    type Future =
        std::pin::Pin<Box<dyn Future<Output = Result<TokioIo<TcpStream>, ConnectionError>> + Send>>;

    fn call(&mut self, dest: Destination) -> Self::Future {
        // Perform host replacement when making the actual TCP connection.
        let mut new_dest = dest.clone();
        let mut parts = dest.into_parts();

        if let Some(auth) = parts.authority {
            let host = auth.host();
            let host = replace_host(host);

            let authority = if let Some(port) = auth.port() {
                format!("{}:{}", host, port.as_str())
            } else {
                (*host).to_string()
            };

            if let Ok(authority) = Authority::from_maybe_shared(authority) {
                parts.authority = Some(authority);
                if let Ok(dest) = Destination::from_parts(parts) {
                    new_dest = dest
                }
            }
        }

        let started = std::time::Instant::now();
        let future = self.inner.call(new_dest);
        Box::pin(async move {
            let stream = future
                .await
                .map_err(|error| ConnectionError::HttpError(BoxError::from(error)));
            net_phase("dns+tcp", started);
            stream
        })
    }

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A connection made ahead of any request (`rel=preconnect`): the TLS stream, and its share of the origin.
type PrewarmedConnection<R> = (MaybeHttpsStream<R>, ConnSlot);

/// How long a connection nobody asked for is kept: Chromium `ClientSocketPoolManager::unused_idle_socket_timeout`
/// (`kPreconnectIntervalSec` = 60, net/socket/client_socket_pool_manager.cc at c75b41dc).
pub const PRECONNECT_IDLE: Duration = Duration::from_secs(60);

struct PrewarmEntry<S> {
    ready: Vec<(Instant, S)>,
    connecting: usize,
    changed: tokio::sync::watch::Sender<u64>,
}

/// Connections opened ahead of requests, by origin, waiting for the connect that wants them.
pub struct Prewarmed<S> {
    entries: Mutex<HashMap<String, PrewarmEntry<S>>>,
}

impl<S> Default for Prewarmed<S> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl<S> Prewarmed<S> {
    /// Claim the right to open one connection for `key`; none if there is one open or opening already.
    fn begin(&self, key: &str) -> bool {
        let mut entries = self.entries.lock();
        let entry = entries
            .entry(key.to_owned())
            .or_insert_with(|| PrewarmEntry {
                ready: Vec::new(),
                connecting: 0,
                changed: tokio::sync::watch::channel(0).0,
            });
        let now = Instant::now();
        entry
            .ready
            .retain(|(made, _)| now.duration_since(*made) < PRECONNECT_IDLE);
        if entry.ready.is_empty() && entry.connecting == 0 {
            entry.connecting = 1;
            true
        } else {
            false
        }
    }

    fn finish(&self, key: &str, made: Option<S>) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.get_mut(key) {
            entry.connecting = entry.connecting.saturating_sub(1);
            if let Some(stream) = made {
                entry.ready.push((Instant::now(), stream));
            }
            entry.changed.send_modify(|n| *n += 1);
            if entry.ready.is_empty() && entry.connecting == 0 {
                entries.remove(key);
            }
        }
    }

    /// Drop connections that waited too long.
    fn sweep(&self, key: &str) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.get_mut(key) {
            let now = Instant::now();
            entry
                .ready
                .retain(|(made, _)| now.duration_since(*made) < PRECONNECT_IDLE);
            if entry.ready.is_empty() && entry.connecting == 0 {
                entries.remove(key);
            }
        }
    }

    /// A connection for `key`: one already made, or the one being made. `None` when there is none.
    async fn take(&self, key: &str) -> Option<S> {
        loop {
            let mut changed;
            {
                let mut entries = self.entries.lock();
                let entry = entries.get_mut(key)?;
                let now = Instant::now();
                entry
                    .ready
                    .retain(|(made, _)| now.duration_since(*made) < PRECONNECT_IDLE);
                if let Some((_, stream)) = entry.ready.pop() {
                    if entry.ready.is_empty() && entry.connecting == 0 {
                        entries.remove(key);
                    }
                    return Some(stream);
                }
                if entry.connecting == 0 {
                    entries.remove(key);
                    return None;
                }
                changed = entry.changed.subscribe();
            }
            let _ = changed.changed().await;
        }
    }
}

/// `scheme://authority`, what the pool and the connector both key a connection by.
fn origin_string(dst: &Destination) -> Option<String> {
    Some(format!("{}://{}", dst.scheme_str()?, dst.authority()?))
}

pub struct InstrumentedConnector<T, R = TokioIo<TcpStream>> {
    inner: HyperRustlsHttpsConnector<T>,
    gate: Arc<OriginGate>,
    prewarmed: Arc<Prewarmed<PrewarmedConnection<R>>>,
}

impl<T: Clone, R> Clone for InstrumentedConnector<T, R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            gate: self.gate.clone(),
            prewarmed: self.prewarmed.clone(),
        }
    }
}

impl<T, R> InstrumentedConnector<T, R> {
    fn new(inner: HyperRustlsHttpsConnector<T>, gate: Arc<OriginGate>) -> Self {
        Self {
            inner,
            gate,
            prewarmed: Arc::new(Prewarmed::default()),
        }
    }
}

pub struct InstrumentedStream<T> {
    inner: MaybeHttpsStream<T>,
    tls_info: RefCell<Option<TlsHandshakeInfo>>,
    /// LucinaNet: this stream's share of its origin's connection limit, given back when it closes.
    _slot: Option<ConnSlot>,
}

impl<T: Unpin> Unpin for InstrumentedStream<T> {}

impl<T> fmt::Debug for InstrumentedStream<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstrumentedStream")
            .field("tls_info", &self.tls_info)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct TlsHandshakeInfo {
    pub protocol_version: Option<ProtocolVersion>,
    pub cipher_suite: Option<CipherSuite>,
    pub kea_group_name: Option<NamedGroup>,
    pub signature_scheme_name: Option<String>,
    pub alpn_protocol: Option<String>,
    pub certificate_chain_der: Vec<Vec<u8>>,
    pub used_ech: bool,
    /// LucinaNet: the handshake resumed an earlier session (a TLS ticket), saving the certificate exchange.
    pub resumed: bool,
}

impl TlsHandshakeInfo {
    fn from_connection(conn: &ClientConnection) -> Self {
        let protocol_version = conn.protocol_version();
        let cipher_suite = conn.negotiated_cipher_suite().map(|suite| suite.suite());
        let kea_group_name = conn
            .negotiated_key_exchange_group()
            .map(|group| group.name());
        let certificate_chain_der = conn
            .peer_certificates()
            .map(|certs| certs.iter().map(|cert| cert.as_ref().to_vec()).collect())
            .unwrap_or_default();
        let alpn_protocol = conn
            .alpn_protocol()
            .map(|proto| String::from_utf8_lossy(proto).into_owned());
        let used_ech = matches!(conn.ech_status(), EchStatus::Accepted);
        let resumed = matches!(conn.handshake_kind(), Some(rustls::HandshakeKind::Resumed));

        Self {
            protocol_version,
            cipher_suite,
            kea_group_name,
            signature_scheme_name: None,
            alpn_protocol,
            certificate_chain_der,
            used_ech,
            resumed,
        }
    }
}

impl<T> InstrumentedStream<T>
where
    T: Connection + hyper::rt::Read + hyper::rt::Write + Unpin,
{
    fn from_maybe_https_stream(stream: MaybeHttpsStream<T>, slot: Option<ConnSlot>) -> Self {
        match stream {
            MaybeHttpsStream::Http(inner) => {
                if let Some(slot) = &slot {
                    slot.negotiated(false);
                }
                Self {
                    inner: MaybeHttpsStream::Http(inner),
                    tls_info: RefCell::new(None),
                    _slot: slot,
                }
            },
            MaybeHttpsStream::Https(tls_stream) => {
                let (_tcp, tls) = tls_stream.inner().get_ref();
                let tls_info = TlsHandshakeInfo::from_connection(tls);
                if let Some(slot) = &slot {
                    slot.negotiated(tls.alpn_protocol() == Some(ALPN_H2.as_bytes()));
                }

                Self {
                    inner: MaybeHttpsStream::Https(tls_stream),
                    tls_info: RefCell::new(Some(tls_info)),
                    _slot: slot,
                }
            },
        }
    }
}

impl<T> Connection for InstrumentedStream<T>
where
    T: Connection + hyper::rt::Read + hyper::rt::Write + Unpin,
{
    fn connected(&self) -> Connected {
        let connected = match &self.inner {
            MaybeHttpsStream::Http(stream) => stream.connected(),
            MaybeHttpsStream::Https(stream) => {
                let (tcp, tls) = stream.inner().get_ref();
                if tls.alpn_protocol() == Some(ALPN_H2.as_bytes()) {
                    tcp.inner().connected().negotiated_h2()
                } else {
                    tcp.inner().connected()
                }
            },
        };
        if let Some(info) = self.tls_info.borrow_mut().take() {
            connected.extra(info)
        } else {
            connected
        }
    }
}

impl<T> hyper::rt::Read for InstrumentedStream<T>
where
    T: Connection + hyper::rt::Read + hyper::rt::Write + Unpin,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<Result<(), io::Error>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<T> hyper::rt::Write for InstrumentedStream<T>
where
    T: Connection + hyper::rt::Read + hyper::rt::Write + Unpin,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }
}

impl<T, R> Service<Destination> for InstrumentedConnector<T, R>
where
    T: Service<Destination, Response = R> + Clone + Send + 'static,
    R: Connection + hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    T::Future: Send + 'static,
    T::Error: Into<BoxError>,
{
    type Response = InstrumentedStream<R>;
    type Error = BoxError;
    type Future =
        std::pin::Pin<Box<dyn Future<Output = Result<InstrumentedStream<R>, BoxError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, dst: Destination) -> Self::Future {
        let target = dst.to_string();
        let origin = OriginKey::from_uri(&dst);
        let pool_key = origin_string(&dst);
        let mut inner = self.inner.clone();
        let gate = self.gate.clone();
        let prewarmed = self.prewarmed.clone();
        Box::pin(async move {
            let started = Instant::now();
            // A connection opened ahead for this origin (rel=preconnect) goes first.
            if let Some(key) = &pool_key
                && let Some((stream, slot)) = prewarmed.take(key).await
            {
                net_phase(&format!("preconnected {target} (used)"), started);
                return Ok(InstrumentedStream::from_maybe_https_stream(
                    stream,
                    Some(slot),
                ));
            }
            // At most six connections for an origin: wait for a share before opening one.
            let slot = match &origin {
                Some(key) => Some(gate.acquire(key).await),
                None => None,
            };
            let stream = inner
                .call(dst)
                .await
                .map_err(|error| -> BoxError { error })?;
            net_phase(&format!("connected {target} (dns+tcp+tls)"), started);
            Ok(InstrumentedStream::from_maybe_https_stream(stream, slot))
        })
    }
}

impl<T, R> InstrumentedConnector<T, R>
where
    T: Service<Destination, Response = R> + Clone + Send + 'static,
    R: Connection + hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    T::Future: Send + 'static,
    T::Error: Into<BoxError>,
{
    /// Open the connection (name, TCP, TLS) for `dst` ahead of the request that will want it. Does nothing if
    /// the origin has a connection already or one is being made.
    pub async fn preconnect(&self, dst: Destination) {
        let (Some(origin), Some(key)) = (OriginKey::from_uri(&dst), origin_string(&dst)) else {
            return;
        };
        if self.gate.live(&origin) > 0 || !self.prewarmed.begin(&key) {
            return;
        }
        let started = Instant::now();
        let target = dst.to_string();
        let slot = self.gate.acquire(&origin).await;
        let mut inner = self.inner.clone();
        match inner.call(dst).await {
            Ok(stream) => {
                net_phase(&format!("preconnected {target} (made)"), started);
                self.prewarmed.finish(&key, Some((stream, slot)));
                // A connection nobody wants within the idle time is closed.
                let prewarmed = self.prewarmed.clone();
                tokio::time::sleep(PRECONNECT_IDLE + Duration::from_millis(100)).await;
                prewarmed.sweep(&key);
            },
            Err(error) => {
                let error: BoxError = error.into();
                log::debug!("preconnect to {target} failed: {error}");
                self.prewarmed.finish(&key, None);
            },
        }
    }
}

pub type Connector = InstrumentedConnector<ServoHttpConnector>;
pub type TlsConfig = ClientConfig;

#[derive(Clone, Debug, Default)]
struct CertificateErrorOverrideManagerInternal {
    /// A mapping of certificates and their hosts, which have seen certificate errors.
    /// This is used to later create an override in this [CertificateErrorOverrideManager].
    certificates_failing_to_verify: HashMap<ServerName<'static>, CertificateDer<'static>>,
    /// A list of certificates that should be accepted despite encountering verification
    /// errors.
    overrides: Vec<CertificateDer<'static>>,
}

/// This data structure is used to track certificate verification errors and overrides.
/// It tracks:
///  - A list of [Certificate]s with verification errors mapped by their [ServerName]
///  - A list of [Certificate]s for which to ignore verification errors.
#[derive(Clone, Debug, Default)]
pub struct CertificateErrorOverrideManager(Arc<Mutex<CertificateErrorOverrideManagerInternal>>);

impl CertificateErrorOverrideManager {
    pub fn new() -> Self {
        Self(Default::default())
    }

    /// Add a certificate to this manager's list of certificates for which to ignore
    /// validation errors.
    pub fn add_override(&self, certificate: &CertificateDer<'static>) {
        self.0.lock().overrides.push(certificate.clone());
    }

    /// Given the a string representation of a sever host name, remove information about
    /// a [Certificate] with verification errors. If a certificate with
    /// verification errors was found, return it, otherwise None.
    pub(crate) fn remove_certificate_failing_verification(
        &self,
        host: &str,
    ) -> Option<CertificateDer<'static>> {
        let server_name = match ServerName::try_from(host) {
            Ok(name) => name.to_owned(),
            Err(error) => {
                warn!("Could not convert host string into RustTLS ServerName: {error:?}");
                return None;
            },
        };
        self.0
            .lock()
            .certificates_failing_to_verify
            .remove(&server_name)
    }
}

#[derive(Clone, Debug, Default)]
pub enum CACertificates<'de> {
    #[default]
    Default,
    Override(Vec<CertificateDer<'de>>),
}

/// Create a [TlsConfig] to use for managing a HTTP connection. This currently creates
/// a rustls [ClientConfig].
///
/// FIXME: The `ignore_certificate_errors` argument ignores all certificate errors. This
/// is used when running the WPT tests, because rustls currently rejects the WPT certificiate.
/// See <https://github.com/servo/servo/issues/30080>
#[servo_tracing::instrument(skip_all)]
pub fn create_tls_config(
    ca_certificates: CACertificates<'static>,
    ignore_certificate_errors: bool,
    override_manager: CertificateErrorOverrideManager,
) -> TlsConfig {
    create_tls_config_with_sessions(
        ca_certificates,
        ignore_certificate_errors,
        override_manager,
        Resumption::in_memory_sessions(TLS_SESSIONS_KEPT),
    )
}

/// How many TLS sessions (tickets) one client keeps. rustls' default is 256; Chromium keeps 1024 in its
/// `SSLClientSessionCache`.
pub const TLS_SESSIONS_KEPT: usize = 1024;

/// [`create_tls_config`] with the session store given: a [`Resumption`] over any
/// `rustls::client::ClientSessionStore`, so a Juno system service can hold the tickets for every app and a
/// second connection to a host, from any of them, skips the certificate exchange.
#[servo_tracing::instrument(skip_all)]
pub fn create_tls_config_with_sessions(
    ca_certificates: CACertificates<'static>,
    ignore_certificate_errors: bool,
    override_manager: CertificateErrorOverrideManager,
    resumption: Resumption,
) -> TlsConfig {
    let verifier = CertificateVerificationOverrideVerifier::new(
        ca_certificates,
        ignore_certificate_errors,
        override_manager,
    );
    // TODO: After <https://github.com/rustls/rustls-platform-verifier/pull/204> is merged,
    // `dangerous` can be removed.
    let mut config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.resumption = resumption;
    config
}

#[derive(Clone)]
struct TokioExecutor {}

impl<F> Executor<F> for TokioExecutor
where
    F: Future<Output = ()> + 'static + std::marker::Send,
{
    fn execute(&self, fut: F) {
        spawn_task(fut);
    }
}

static CRYPTO_PROVIDER_CACHE: LazyLock<Arc<CryptoProvider>> = LazyLock::new(|| {
    CryptoProvider::get_default()
        .cloned()
        // The embedder should have initialized the default crypto provider before
        // initializing servo, so this should never fail.
        .unwrap_or_else(|| {
            warn!("Default crypto provider not initialized before first access in connector.");
            Arc::new(rustls_graviola::default_provider())
        })
});

/// A cache for the default rustls platform verifier.
///
/// Instantiating a new verifier can be expensive, since it can read through all certificates:
/// <https://github.com/rustls/rustls-platform-verifier/blob/996b1c903491641b17b3c9afb65d1352f6fc6b76/rustls-platform-verifier/src/verification/others.rs#L92>
static RUSTLS_PLATFORM_VERIFIER_CACHE: LazyLock<Arc<rustls_platform_verifier::Verifier>> =
    LazyLock::new(|| {
        Arc::new(
            // LucinaNet: Juno has no system CA store, so the roots travel with the binary. System roots
            // still count when a system has them.
            rustls_platform_verifier::Verifier::new_with_extra_roots(
                webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().cloned(),
                CRYPTO_PROVIDER_CACHE.clone(),
            )
            .expect("Could not initialize platform certificate verifier"),
        )
    });

/// Prewarm the TLS stack to speed up the first connection
///
/// Currently, this force-seeds the crypto provider (from graviola),
/// which with aws-lc took around 30-50ms according to samply, spent in
/// `tree_jitter_initialize_once`. If we don't call this function, then
/// the initialization will happen much later, on a tokio runtime thread.
#[inline]
pub fn prewarm_tls() {
    #[servo_tracing::instrument]
    fn prewarm_tls_impl() {
        let mut sink = [0u8; 32];
        // The first access can be slow, if the provider needs to gather entropy.
        let _ = CRYPTO_PROVIDER_CACHE.secure_random.fill(&mut sink);
        // Note: We don't need to explicitly force initialize RUSTLS_PLATFORM_VERIFIER_CACHE,
        // since the resource manager thread will do that during startup.
    }

    if let Err(error) = std::thread::Builder::new()
        .name("Net-TLS-prewarm".into())
        .spawn(prewarm_tls_impl)
    {
        warn!("Failed to spawn thread to prewarm TLS: {error:?}");
    }
}

#[derive(Debug)]
struct CertificateVerificationOverrideVerifier {
    main_verifier: Arc<dyn ServerCertVerifier>,
    ignore_certificate_errors: bool,
    override_manager: CertificateErrorOverrideManager,
}

impl CertificateVerificationOverrideVerifier {
    fn new(
        ca_certficates: CACertificates<'static>,
        ignore_certificate_errors: bool,
        override_manager: CertificateErrorOverrideManager,
    ) -> Self {
        // From <https://github.com/rustls/rustls-platform-verifier/blob/main/README.md>:
        // > Some manual setup is required, outside of cargo, to use this crate on
        // > Android. In order to use Android's certificate verifier, the crate needs to
        // > call into the JVM. A small Kotlin component must be included in your app's
        // > build to support rustls-platform-verifier.
        //
        // Since we cannot count on embedders to do this setup, just stick with webpki roots
        // on Android.
        let use_webpki_roots = cfg!(target_os = "android") || pref!(network_use_webpki_roots);
        let main_verifier = if !use_webpki_roots {
            let verifier = match ca_certficates {
                CACertificates::Default => RUSTLS_PLATFORM_VERIFIER_CACHE.clone(),
                // Android doesn't support `Verifier::new_with_extra_roots`, but currently Android
                // never uses the platform verifier at all.
                CACertificates::Override(_certificates) => {
                    #[cfg(target_os = "android")]
                    unreachable!("Android should always use the WebPKI verifier.");
                    #[cfg(not(target_os = "android"))]
                    {
                        let verifier = rustls_platform_verifier::Verifier::new_with_extra_roots(
                            _certificates,
                            CRYPTO_PROVIDER_CACHE.clone(),
                        )
                        .expect("Could not initialize platform certificate verifier");
                        Arc::new(verifier)
                    }
                },
            };
            verifier as Arc<dyn ServerCertVerifier>
        } else {
            let mut root_store =
                rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            match ca_certficates {
                CACertificates::Default => {},
                CACertificates::Override(certificates) => {
                    for certificate in certificates {
                        if root_store.add(certificate).is_err() {
                            log::error!("Could not add an override certificate.");
                        }
                    }
                },
            }
            rustls::client::WebPkiServerVerifier::builder(root_store.into())
                .build()
                .expect("Could not initialize platform certificate verifier.")
                as Arc<dyn ServerCertVerifier>
        };

        Self {
            main_verifier,
            ignore_certificate_errors,
            override_manager,
        }
    }
}

impl rustls::client::danger::ServerCertVerifier for CertificateVerificationOverrideVerifier {
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.main_verifier
            .verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.main_verifier
            .verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.main_verifier.supported_verify_schemes()
    }

    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let started = std::time::Instant::now();
        let verified = self.main_verifier.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        );
        net_phase("certificate verification", started);
        let error = match verified {
            Ok(result) => return Ok(result),
            Err(error) => error,
        };

        if self.ignore_certificate_errors {
            warn!("Ignoring certficate error: {error:?}");
            return Ok(rustls::client::danger::ServerCertVerified::assertion());
        }

        // If there's an override for this certificate, just accept it.
        for cert_with_exception in &*self.override_manager.0.lock().overrides {
            if *end_entity == *cert_with_exception {
                return Ok(rustls::client::danger::ServerCertVerified::assertion());
            }
        }
        self.override_manager
            .0
            .lock()
            .certificates_failing_to_verify
            .insert(server_name.to_owned(), end_entity.clone().into_owned());
        Err(error)
    }
}

pub type BoxedBody = BoxBody<Bytes, hyper::Error>;

#[derive(Debug)]
/// The error type for the MaybeProxyConnector
pub enum ConnectionError {
    HttpError(BoxError),
    // It looks like currently the type is not exported.
    ProxyError(BoxError),
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ConnectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ConnectionError::HttpError(error) => error.source(),
            ConnectionError::ProxyError(error) => error.source(),
        }
    }
}

#[derive(Clone)]
/// A proxy connector. This will automatically open a proxy connection if the uri matches the proxy uri.
/// Also respects 'no_proxy'.
pub struct ProxyConnector {
    /// A client without proxy for `no_proxy` matches.
    client: ServoHttpConnector,
    /// Matcher to see if we should forward to the proxy or not.
    matcher: std::sync::Arc<hyper_util::client::proxy::matcher::Matcher>,
}

impl ProxyConnector {
    fn new(resolver: Arc<dyn Resolver>) -> Self {
        let matcher_builder = hyper_util::client::proxy::matcher::Matcher::builder()
            .http(servo_config::pref!(network_http_proxy_uri))
            .https(servo_config::pref!(network_https_proxy_uri))
            .no(servo_config::pref!(network_http_no_proxy));
        ProxyConnector {
            client: ServoHttpConnector::new(resolver),
            matcher: std::sync::Arc::new(matcher_builder.build()),
        }
    }
}

// Just forward everything to the inner type except that we modify the errors returned.
impl Service<Destination> for ProxyConnector {
    type Response = TokioIo<TcpStream>;
    type Error = ConnectionError;
    type Future =
        std::pin::Pin<Box<dyn Future<Output = Result<TokioIo<TcpStream>, ConnectionError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.client
            .poll_ready(cx)
            .map_err(|error| ConnectionError::ProxyError(BoxError::from(error)))
    }

    fn call(&mut self, req: Destination) -> Self::Future {
        match self.matcher.intercept(&req) {
            Some(intercept) => {
                let mut tunnel = Tunnel::new(intercept.uri().clone(), self.client.clone());
                let final_tunnel = if let Some(auth) = intercept.basic_auth() {
                    tunnel.with_auth(auth.clone())
                } else {
                    tunnel
                }
                .call(req)
                .map_err(|error| ConnectionError::ProxyError(BoxError::from(error)));
                Box::pin(final_tunnel)
            },
            None => Box::pin(
                self.client
                    .call(req)
                    .map_err(|error| ConnectionError::ProxyError(BoxError::from(error))),
            ),
        }
    }
}

pub type HyperClient = Client<InstrumentedConnector<ProxyConnector>, BoxedBody>;

/// hyper's client behind the origin gate: what the fetch code sends requests through.
#[derive(Clone)]
pub struct ServoClient {
    client: HyperClient,
    gate: Arc<OriginGate>,
    connector: InstrumentedConnector<ProxyConnector>,
    resolver: Arc<dyn Resolver>,
}

type ClientResponse = http::Response<hyper::body::Incoming>;
type ClientError = hyper_util::client::legacy::Error;

impl ServoClient {
    /// Send a request. A request to an origin with no connection yet waits for the first request of that origin
    /// to make one (see [`OriginGate`]); the rest go straight to hyper.
    pub fn request(
        &self,
        request: http::Request<BoxedBody>,
    ) -> BoxFuture<Result<ClientResponse, ClientError>> {
        let client = self.client.clone();
        let gate = self.gate.clone();
        Box::pin(async move {
            let probe = match OriginKey::from_uri(request.uri()) {
                Some(key) => gate.enter(&key).await,
                None => None,
            };
            let result = client.request(request).await;
            // Headers are in (or the request failed): the connection is in hyper's pool for the others.
            drop(probe);
            result
        })
    }

    /// `rel=preconnect`: name, TCP and TLS for `url`'s origin now, for the first request to use.
    pub fn preconnect(&self, url: &url::Url) {
        let Some(dst) = origin_uri(url) else { return };
        let connector = self.connector.clone();
        crate::async_runtime::runtime_handle().spawn(async move {
            connector.preconnect(dst).await;
        });
    }

    /// `rel=dns-prefetch`: the address of `url`'s host, into the cache.
    pub fn dns_prefetch(&self, url: &url::Url) {
        if let Some(host) = url.host_str() {
            self.resolver.prefetch(&replace_host(host));
        }
    }

    /// The gate, for tests and for the memory report.
    pub fn gate(&self) -> &Arc<OriginGate> {
        &self.gate
    }
}

/// `scheme://host[:port]/` as the connector is asked for it.
fn origin_uri(url: &url::Url) -> Option<Destination> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?;
    let authority = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    Destination::builder()
        .scheme(url.scheme())
        .authority(authority)
        .path_and_query("/")
        .build()
        .ok()
}

pub fn create_http_client(tls_config: TlsConfig) -> ServoClient {
    create_http_client_with_resolver(tls_config, Arc::new(DnsCache::system()))
}

/// [`create_http_client`] with the name service given, for a system-wide resolver or a test.
pub fn create_http_client_with_resolver(
    tls_config: TlsConfig,
    resolver: Arc<dyn Resolver>,
) -> ServoClient {
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(ProxyConnector::new(resolver.clone()));
    let gate = Arc::new(OriginGate::default());
    let connector = InstrumentedConnector::new(connector, gate.clone());

    let client = Client::builder(TokioExecutor {})
        .http1_title_case_headers(true)
        // LucinaNet: hyper allows 16 KiB of response headers and resets the stream above that as a
        // protocol error. YouTube sends more. Chromium allows 256 KiB.
        .http2_max_header_list_size(256 * 1024)
        // This is necessary to allow hyper to perform various background tasks for HTTP/2.
        .timer(TokioTimer::new())
        // This is necessary for hyper to reap unused idle keep-alive connections.
        .pool_timer(TokioTimer::new())
        // Other browsers control the maximum connections per host, typically set to 6, but hyper
        // does not allow controlling this directly. LucinaNet: the origin gate holds that limit
        // (`MAX_CONNECTIONS_PER_ORIGIN`); this one bounds the idle ones kept.
        .pool_max_idle_per_host(crate::origin_gate::MAX_CONNECTIONS_PER_ORIGIN)
        .build(connector.clone());
    ServoClient {
        client,
        gate,
        connector,
        resolver,
    }
}
