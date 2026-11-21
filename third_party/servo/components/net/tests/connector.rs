/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet: the connector's connection policy, against a local HTTPS server that counts what reaches it.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use http_body_util::{BodyExt, Empty};
use net::async_runtime::spawn_blocking_task;
use net::connector::{
    CACertificates, CertificateErrorOverrideManager, ServoClient, TlsConfig, TlsHandshakeInfo,
    create_http_client_with_resolver, create_tls_config,
};
use net::dns_cache::{DnsCache, NameLookup, SystemClock};
use net::origin_gate::MAX_CONNECTIONS_PER_ORIGIN;
use net::test_util::{Server, ServerStats, make_counting_tls_server};
use servo_url::ServoUrl;

/// `localhost` is always 127.0.0.1 here, and every lookup is counted.
struct CountingLocalhost(AtomicUsize);

impl NameLookup for CountingLocalhost {
    fn lookup(&self, _host: &str) -> io::Result<Vec<SocketAddr>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec!["127.0.0.1:0".parse().unwrap()])
    }
}

struct Rig {
    server: Server,
    url: ServoUrl,
    stats: Arc<ServerStats>,
    lookups: Arc<CountingLocalhost>,
    tls: TlsConfig,
    client: ServoClient,
}

fn rig(h2: bool, delay_ms: u64) -> Rig {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let (server, url, stats) = make_counting_tls_server(h2, Duration::from_millis(delay_ms));
    let override_manager = CertificateErrorOverrideManager::new();
    for certificate in server.certificates.as_ref().unwrap() {
        override_manager.add_override(certificate);
    }
    let tls = create_tls_config(CACertificates::Default, false, override_manager);
    let lookups = Arc::new(CountingLocalhost(AtomicUsize::new(0)));
    let client = client_for(&tls, &lookups);
    Rig {
        server,
        url,
        stats,
        lookups,
        tls,
        client,
    }
}

fn client_for(tls: &TlsConfig, lookups: &Arc<CountingLocalhost>) -> ServoClient {
    let cache = DnsCache::new(lookups.clone(), Arc::new(SystemClock::new()));
    create_http_client_with_resolver(tls.clone(), Arc::new(cache))
}

fn get(
    client: &ServoClient,
    url: &ServoUrl,
    path: &str,
) -> impl Future<Output = (u16, Option<bool>)> + use<> {
    let request = http::Request::builder()
        .uri(format!("{}{path}", url.as_str().trim_end_matches('/')))
        .body(Empty::new().map_err(|never| match never {}).boxed())
        .unwrap();
    let response = client.request(request);
    async move {
        let response = response.await.expect("request failed");
        let resumed = response
            .extensions()
            .get::<TlsHandshakeInfo>()
            .map(|info| info.resumed);
        let status = response.status().as_u16();
        let _ = response.into_body().collect().await;
        (status, resumed)
    }
}

fn get_many(client: &ServoClient, url: &ServoUrl, n: usize) -> Vec<(u16, Option<bool>)> {
    let tasks: Vec<_> = (0..n).map(|i| get(client, url, &format!("/{i}"))).collect();
    spawn_blocking_task(async move { futures::future::join_all(tasks).await })
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn twenty_concurrent_requests_to_an_h2_origin_make_one_connection() {
    let rig = rig(true, 50);
    let answers = get_many(&rig.client, &rig.url, 20);
    assert!(answers.iter().all(|(status, _)| *status == 200));
    assert_eq!(rig.stats.requests(), 20);
    assert_eq!(
        rig.stats.connections(),
        1,
        "one h2 connection for the origin"
    );
    assert_eq!(
        rig.lookups.0.load(Ordering::SeqCst),
        1,
        "one name lookup for the origin"
    );
    // A second burst reuses the open connection and the cached address.
    let answers = get_many(&rig.client, &rig.url, 20);
    assert!(answers.iter().all(|(status, _)| *status == 200));
    assert_eq!(rig.stats.connections(), 1);
    assert_eq!(rig.lookups.0.load(Ordering::SeqCst), 1);
    rig.server.close();
}

#[test]
fn an_h1_origin_gets_up_to_six_connections() {
    let rig = rig(false, 100);
    let answers = get_many(&rig.client, &rig.url, 24);
    assert!(answers.iter().all(|(status, _)| *status == 200));
    let connections = rig.stats.connections();
    assert!(
        (2..=MAX_CONNECTIONS_PER_ORIGIN).contains(&connections),
        "{connections} connections for HTTP/1.1"
    );
    rig.server.close();
}

#[test]
fn a_second_connection_to_a_host_resumes_the_tls_session() {
    let rig = rig(true, 0);
    let first = get_many(&rig.client, &rig.url, 1);
    assert_eq!(first[0].1, Some(false), "the first handshake is a full one");
    // The ticket arrives after the handshake; a second client sharing the TLS config finds it.
    std::thread::sleep(Duration::from_millis(200));
    let second_client = client_for(&rig.tls, &rig.lookups);
    let second = get_many(&second_client, &rig.url, 1);
    assert_eq!(second[0].1, Some(true), "the second handshake resumes");
    assert_eq!(rig.stats.connections(), 2);
    assert_eq!(rig.stats.resumed(), 1);
    rig.server.close();
}

#[test]
fn preconnect_opens_the_connection_before_the_first_request() {
    let rig = rig(true, 20);
    rig.client
        .preconnect(&url::Url::parse(rig.url.as_str()).unwrap());
    wait_for("the preconnected connection", || {
        rig.stats.connections() == 1
    });
    assert_eq!(rig.stats.requests(), 0);
    let answers = get_many(&rig.client, &rig.url, 10);
    assert!(answers.iter().all(|(status, _)| *status == 200));
    assert_eq!(
        rig.stats.connections(),
        1,
        "the requests used the preconnected one"
    );
    rig.server.close();
}

#[test]
fn dns_prefetch_fills_the_cache() {
    let rig = rig(true, 0);
    rig.client
        .dns_prefetch(&url::Url::parse(rig.url.as_str()).unwrap());
    wait_for("the prefetched lookup", || {
        rig.lookups.0.load(Ordering::SeqCst) == 1
    });
    let answers = get_many(&rig.client, &rig.url, 3);
    assert!(answers.iter().all(|(status, _)| *status == 200));
    assert_eq!(rig.lookups.0.load(Ordering::SeqCst), 1);
    rig.server.close();
}
