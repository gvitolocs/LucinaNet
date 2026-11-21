/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet: one connection per HTTP/2 origin, and at most six per origin for HTTP/1.1.
//!
//! hyper-util's legacy client starts a connect for every request that finds no idle connection and, when the
//! first of them turns out to speak h2, throws the others away after their TCP and TLS handshakes (bbc.com: 138
//! connections to 9 hosts). The gate sits in front of it:
//!
//! * A request to an origin with no connection waits while one request of that origin, the *probe*, makes the first
//!   one. The probe's connection is in the pool once its response headers are in, so the others find it there and
//!   multiplex on it. If the origin turned out to speak HTTP/1.1 they are let go as soon as that is known (the
//!   connect), and open their own connections.
//! * Every connect takes a [`ConnSlot`] first, from a first-come-first-served queue; an origin has at most
//!   [`MAX_CONNECTIONS_PER_ORIGIN`] of them. A slot lives as long as the stream and is given back whenever the stream
//!   drops (success, error or cancel); a waiter that is dropped leaves the queue. When the last stream of an origin is
//!   gone the gate forgets what it learned.
//!
//! The rules are the Verus-checked functions of `lucinanet-net-policy` (`slots::may_take`, `taken`, `released`); this file
//! is the glue. The key is [`OriginKey`], plain data a system-wide network service can use unchanged.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use lucinanet_net_policy::slots::{self, MAX_PER_ORIGIN};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

pub const MAX_CONNECTIONS_PER_ORIGIN: usize = MAX_PER_ORIGIN;

/// Scheme, host and port: what a connection is for.
#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginKey {
    pub secure: bool,
    pub host: String,
    pub port: u16,
}

impl OriginKey {
    pub fn from_uri(uri: &http::Uri) -> Option<OriginKey> {
        let secure = match uri.scheme_str()? {
            "https" => true,
            "http" => false,
            _ => return None,
        };
        let authority = uri.authority()?;
        Some(OriginKey {
            secure,
            host: authority.host().to_ascii_lowercase(),
            port: authority
                .port_u16()
                .unwrap_or(if secure { 443 } else { 80 }),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Unknown,
    Http1,
    Http2,
}

/// What a request has to do before it goes to the client. Pure: the whole policy is this function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Send the request.
    Go,
    /// Send it, and keep the others waiting until its headers are in.
    Probe,
    /// Wait for the state to change and decide again.
    Wait,
}

pub fn next_step(secure: bool, protocol: Protocol, probing: bool) -> Step {
    if !secure {
        // Cleartext has no ALPN: HTTP/1.1 only, every request on its own.
        return Step::Go;
    }
    match (protocol, probing) {
        (Protocol::Http1, _) => Step::Go,
        (Protocol::Http2, false) => Step::Go,
        (Protocol::Unknown, false) => Step::Probe,
        (Protocol::Http2, true) | (Protocol::Unknown, true) => Step::Wait,
    }
}

struct State {
    protocol: Protocol,
    probing: bool,
    live: usize,
    /// Waiters for a slot, in the order they came.
    queue: VecDeque<u64>,
    changed: watch::Sender<u64>,
}

impl State {
    fn new() -> State {
        State {
            protocol: Protocol::Unknown,
            probing: false,
            live: 0,
            queue: VecDeque::new(),
            changed: watch::channel(0).0,
        }
    }

    fn idle(&self) -> bool {
        self.live == 0 && !self.probing && self.queue.is_empty()
    }
}

pub struct OriginGate {
    states: Mutex<HashMap<OriginKey, State>>,
    next_ticket: Mutex<u64>,
    max_per_origin: usize,
}

impl Default for OriginGate {
    fn default() -> Self {
        Self::new(MAX_CONNECTIONS_PER_ORIGIN)
    }
}

/// Held by the request that makes the first connection of an origin. Others wait until it is dropped (or the
/// origin turns out to be HTTP/1.1).
pub struct ProbeGuard {
    gate: Arc<OriginGate>,
    key: OriginKey,
}

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        let mut states = self.gate.states.lock();
        if let Some(state) = states.get_mut(&self.key) {
            state.probing = false;
            state.changed.send_modify(|n| *n += 1);
            if state.idle() {
                states.remove(&self.key);
            }
        }
    }
}

/// One connection's share of its origin. Dropping it frees the share.
pub struct ConnSlot {
    gate: Arc<OriginGate>,
    key: OriginKey,
}

impl ConnSlot {
    /// The connection's ALPN result: h2 keeps the others waiting for the probe's headers, anything else lets them go.
    pub fn negotiated(&self, h2: bool) {
        let mut states = self.gate.states.lock();
        if let Some(state) = states.get_mut(&self.key) {
            state.protocol = if h2 { Protocol::Http2 } else { Protocol::Http1 };
            state.changed.send_modify(|n| *n += 1);
        }
    }
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        let mut states = self.gate.states.lock();
        if let Some(state) = states.get_mut(&self.key) {
            state.live = slots::released(state.live);
            if state.live == 0 {
                // Nothing is open: the next request starts from nothing, as after a restart of the origin.
                state.protocol = Protocol::Unknown;
            }
            state.changed.send_modify(|n| *n += 1);
            if state.idle() {
                states.remove(&self.key);
            }
        }
    }
}

/// A place in an origin's queue; if the waiting request is dropped, the place is given up.
struct Ticket<'a> {
    gate: &'a Arc<OriginGate>,
    key: &'a OriginKey,
    id: u64,
    queued: bool,
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        if !self.queued {
            return;
        }
        let mut states = self.gate.states.lock();
        if let Some(state) = states.get_mut(self.key) {
            if let Some(at) = state.queue.iter().position(|id| *id == self.id) {
                state.queue.remove(at);
            }
            // The waiters behind move up; the new front may now take a slot.
            state.changed.send_modify(|n| *n += 1);
            if state.idle() {
                states.remove(self.key);
            }
        }
    }
}

impl OriginGate {
    pub fn new(max_per_origin: usize) -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
            next_ticket: Mutex::new(0),
            max_per_origin: max_per_origin.max(1),
        }
    }

    /// Decide for a request. `None` sends it; `Some(guard)` sends it as the probe.
    pub async fn enter(self: &Arc<Self>, key: &OriginKey) -> Option<ProbeGuard> {
        loop {
            let mut changed;
            {
                let mut states = self.states.lock();
                let state = states.entry(key.clone()).or_insert_with(State::new);
                match next_step(key.secure, state.protocol, state.probing) {
                    Step::Go => {
                        if state.idle() {
                            states.remove(key);
                        }
                        return None;
                    },
                    Step::Probe => {
                        state.probing = true;
                        return Some(ProbeGuard {
                            gate: self.clone(),
                            key: key.clone(),
                        });
                    },
                    Step::Wait => {
                        // Subscribed while the lock is held, so a change after it is not missed.
                        changed = state.changed.subscribe();
                    },
                }
            }
            let _ = changed.changed().await;
        }
    }

    /// A share of the origin for a connection about to be made. Waiters are served first come, first served and
    /// only while the origin has room.
    pub async fn acquire(self: &Arc<Self>, key: &OriginKey) -> ConnSlot {
        let id = {
            let mut next = self.next_ticket.lock();
            *next += 1;
            *next
        };
        {
            let mut states = self.states.lock();
            let state = states.entry(key.clone()).or_insert_with(State::new);
            state.queue.push_back(id);
        }
        let mut ticket = Ticket {
            gate: self,
            key,
            id,
            queued: true,
        };
        loop {
            let mut changed;
            {
                let mut states = self.states.lock();
                let state = states.entry(key.clone()).or_insert_with(State::new);
                let position = state.queue.iter().position(|queued| *queued == id);
                if let Some(position) = position
                    && slots::may_take(position, state.live, self.max_per_origin)
                {
                    state.queue.pop_front();
                    state.live = slots::taken(state.live, self.max_per_origin);
                    ticket.queued = false;
                    // The next waiter is now at the front.
                    state.changed.send_modify(|n| *n += 1);
                    return ConnSlot {
                        gate: self.clone(),
                        key: key.clone(),
                    };
                }
                changed = state.changed.subscribe();
            }
            let _ = changed.changed().await;
        }
    }

    /// Connections held by `key` now.
    pub fn live(&self, key: &OriginKey) -> usize {
        self.states.lock().get(key).map_or(0, |state| state.live)
    }

    /// Requests waiting for a slot of `key`.
    pub fn waiting(&self, key: &OriginKey) -> usize {
        self.states
            .lock()
            .get(key)
            .map_or(0, |state| state.queue.len())
    }

    pub fn protocol(&self, key: &OriginKey) -> Protocol {
        self.states
            .lock()
            .get(key)
            .map_or(Protocol::Unknown, |state| state.protocol)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    fn key(host: &str) -> OriginKey {
        OriginKey {
            secure: true,
            host: host.into(),
            port: 443,
        }
    }

    #[test]
    fn next_step_covers_every_state() {
        use Protocol::*;
        use Step::*;
        for (secure, protocol, probing, want) in [
            (false, Unknown, false, Go),
            (false, Unknown, true, Go),
            (false, Http1, false, Go),
            (false, Http2, true, Go),
            (true, Unknown, false, Probe),
            (true, Unknown, true, Wait),
            (true, Http1, false, Go),
            (true, Http1, true, Go),
            (true, Http2, false, Go),
            (true, Http2, true, Wait),
        ] {
            assert_eq!(
                next_step(secure, protocol, probing),
                want,
                "{secure} {protocol:?} {probing}"
            );
        }
    }

    #[test]
    fn keys_come_from_uris() {
        let k = OriginKey::from_uri(&"https://Example.org/a?b".parse().unwrap()).unwrap();
        assert_eq!(k, key("example.org"));
        let k = OriginKey::from_uri(&"http://example.org:8080/".parse().unwrap()).unwrap();
        assert_eq!((k.secure, k.port), (false, 8080));
        assert!(OriginKey::from_uri(&"ftp://example.org/".parse().unwrap()).is_none());
        let json = serde_json::to_string(&k).unwrap();
        assert_eq!(serde_json::from_str::<OriginKey>(&json).unwrap(), k);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn many_requests_make_one_connect_when_the_origin_speaks_h2() {
        let gate = Arc::new(OriginGate::default());
        let connects = Arc::new(AtomicUsize::new(0));
        let k = key("h2.example");
        let mut tasks = Vec::new();
        for _ in 0..40 {
            let (gate, connects, k) = (gate.clone(), connects.clone(), k.clone());
            tasks.push(tokio::spawn(async move {
                let probe = gate.enter(&k).await;
                if probe.is_some() {
                    // The probe connects, learns h2, and answers some time later.
                    let slot = gate.acquire(&k).await;
                    connects.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    slot.negotiated(true);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    drop(probe);
                    // The connection stays open.
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    drop(slot);
                } else {
                    // A request that went on after the probe: finds the connection, no connect.
                    assert_eq!(gate.protocol(&k), Protocol::Http2);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(connects.load(Ordering::SeqCst), 1);
        assert_eq!(gate.live(&k), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn h1_origins_are_released_at_the_connect_and_capped_at_six() {
        let gate = Arc::new(OriginGate::default());
        let k = key("h1.example");
        let open = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..30 {
            let (gate, k, open, peak) = (gate.clone(), k.clone(), open.clone(), peak.clone());
            tasks.push(tokio::spawn(async move {
                let probe = gate.enter(&k).await;
                let slot = gate.acquire(&k).await;
                let now = open.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                slot.negotiated(false);
                // Held until the response is in.
                tokio::time::sleep(Duration::from_millis(20)).await;
                drop(probe);
                open.fetch_sub(1, Ordering::SeqCst);
                drop(slot);
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= MAX_CONNECTIONS_PER_ORIGIN);
        assert!(
            peak.load(Ordering::SeqCst) > 1,
            "HTTP/1.1 requests ran in parallel"
        );
        assert_eq!(gate.live(&k), 0);
        assert_eq!(gate.waiting(&k), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn waiters_get_slots_in_the_order_they_came() {
        let gate = Arc::new(OriginGate::new(1));
        let k = key("fifo.example");
        let first = gate.acquire(&k).await;
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for n in 0..8u32 {
            let (task_gate, task_key, order) = (gate.clone(), k.clone(), order.clone());
            tasks.push(tokio::spawn(async move {
                let slot = task_gate.acquire(&task_key).await;
                order.lock().push(n);
                tokio::time::sleep(Duration::from_millis(5)).await;
                drop(slot);
            }));
            // Let each waiter reach the queue before the next one starts.
            while gate.waiting(&k) < (n + 1) as usize {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
        drop(first);
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(*order.lock(), (0..8).collect::<Vec<_>>());
        assert_eq!(gate.live(&k), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cancelled_waiter_leaves_the_queue_and_leaks_nothing() {
        let gate = Arc::new(OriginGate::new(1));
        let k = key("cancel.example");
        let first = gate.acquire(&k).await;
        let doomed = {
            let (gate, k) = (gate.clone(), k.clone());
            tokio::spawn(async move { gate.acquire(&k).await })
        };
        let patient = {
            let (gate, k) = (gate.clone(), k.clone());
            tokio::spawn(async move { gate.acquire(&k).await })
        };
        while gate.waiting(&k) < 2 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        doomed.abort();
        let _ = doomed.await;
        assert_eq!(gate.waiting(&k), 1, "the dropped waiter left the queue");
        drop(first);
        let slot = patient.await.unwrap();
        assert_eq!(gate.live(&k), 1);
        drop(slot);
        assert_eq!(gate.live(&k), 0, "no slot leaked");
        assert_eq!(gate.waiting(&k), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_connect_gives_its_slot_back() {
        let gate = Arc::new(OriginGate::new(2));
        let k = key("error.example");
        for _ in 0..10 {
            let slot = gate.acquire(&k).await;
            // the connect failed: the slot drops with the error
            drop(slot);
        }
        assert_eq!(gate.live(&k), 0);
        let (a, b) = (gate.acquire(&k).await, gate.acquire(&k).await);
        assert_eq!(gate.live(&k), 2);
        drop((a, b));
        assert_eq!(gate.live(&k), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_probe_hands_over_to_the_next_request() {
        let gate = Arc::new(OriginGate::default());
        let k = key("down.example");
        let first = gate.enter(&k).await;
        assert!(first.is_some());
        let waiter = {
            let (gate, k) = (gate.clone(), k.clone());
            tokio::spawn(async move { gate.enter(&k).await.is_some() })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiter.is_finished(), "waits while the probe runs");
        drop(first); // the probe failed without a connection
        assert!(waiter.await.unwrap(), "the next request becomes the probe");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_origin_is_forgotten_when_its_last_connection_closes() {
        let gate = Arc::new(OriginGate::default());
        let k = key("idle.example");
        let probe = gate.enter(&k).await;
        let slot = gate.acquire(&k).await;
        slot.negotiated(true);
        drop(probe);
        assert!(gate.enter(&k).await.is_none());
        drop(slot);
        assert_eq!(gate.protocol(&k), Protocol::Unknown);
        assert!(
            gate.enter(&k).await.is_some(),
            "the next burst is single-flight again"
        );
    }

    #[tokio::test]
    async fn cleartext_requests_are_never_held() {
        let gate = Arc::new(OriginGate::default());
        let k = OriginKey {
            secure: false,
            host: "plain.example".into(),
            port: 80,
        };
        for _ in 0..5 {
            assert!(gate.enter(&k).await.is_none());
        }
    }
}
