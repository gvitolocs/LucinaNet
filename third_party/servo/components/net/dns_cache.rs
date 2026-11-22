/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet: a host name cache with one lookup in flight per name. The rules (lifetimes, when a lookup starts, how many
//! entries) are the Verus-checked functions of `lucinanet-net-policy`; this file is the glue: the map, the blocking
//! lookup, the wakeups. Each number states its source where it is defined.
//!
//! The cache sits behind [`Resolver`], the interface a lower layer (a Juno system service) can implement in its place: the key is
//! a plain host name, the answer a list of addresses, both `serde`-able.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lucinanet_net_policy::dns::{
    Action, MAX_ENTRIES, Slot as PolicySlot, decide, evict_count, is_fresh,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

/// The addresses for one name, or why there are none.
pub type DnsAnswer = Result<Arc<[SocketAddr]>, Arc<DnsError>>;
pub type BoxFuture<T> = Pin<Box<dyn std::future::Future<Output = T> + Send>>;

/// A failed lookup, cloneable so every waiter gets it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DnsError {
    pub message: String,
}

impl DnsError {
    pub fn to_io(&self) -> io::Error {
        io::Error::other(self.message.clone())
    }
}

/// The key of a cache entry: a host name as the URL parser wrote it, lower case.
#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsKey(pub String);

/// What the connector needs from a name service. The default is [`DnsCache`] in this process; a Juno system
/// service can implement the same two calls over a socket.
pub trait Resolver: Send + Sync + 'static {
    /// The addresses of `host` (ports are 0), from a cache when one is fresh.
    fn resolve(&self, host: &str) -> BoxFuture<DnsAnswer>;
    /// Start a lookup for `host` and keep the answer (`rel=dns-prefetch`). Returns at once.
    fn prefetch(&self, host: &str);
}

/// The blocking lookup behind the cache (`getaddrinfo` by default). Tests give it a counting fake.
pub trait NameLookup: Send + Sync + 'static {
    fn lookup(&self, host: &str) -> io::Result<Vec<SocketAddr>>;
}

/// A monotonic clock as the time since some start, so tests can move it.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Duration;
}

pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

enum Entry {
    Ready { answer: DnsAnswer, stored: Duration },
    InFlight(u64, watch::Receiver<Option<DnsAnswer>>),
}

struct Inner {
    entries: HashMap<DnsKey, Entry>,
    next_id: u64,
}

pub struct DnsCache {
    inner: Arc<Mutex<Inner>>,
    lookup: Arc<dyn NameLookup>,
    clock: Arc<dyn Clock>,
    max_entries: usize,
}

fn failure(answer: &DnsAnswer) -> bool {
    answer.is_err()
}

fn age_ms(now: Duration, stored: Duration) -> u64 {
    now.saturating_sub(stored).as_millis().min(u64::MAX as u128) as u64
}

impl DnsCache {
    pub fn new(lookup: Arc<dyn NameLookup>, clock: Arc<dyn Clock>) -> Self {
        Self::with_capacity(lookup, clock, MAX_ENTRIES)
    }

    pub fn with_capacity(
        lookup: Arc<dyn NameLookup>,
        clock: Arc<dyn Clock>,
        max_entries: usize,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                entries: HashMap::new(),
                next_id: 0,
            })),
            lookup,
            clock,
            max_entries: max_entries.max(1),
        }
    }

    /// `getaddrinfo` with the default numbers and the real clock.
    pub fn system() -> Self {
        Self::new(
            Arc::new(crate::connector::GetAddrInfo),
            Arc::new(SystemClock::new()),
        )
    }

    /// Entries held now (fresh or not yet swept).
    pub fn len(&self) -> usize {
        self.inner.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop expired entries, then the oldest ones, until one more fits (`evict_count` says how many).
    fn make_room(inner: &mut Inner, now: Duration, cap: usize) {
        if evict_count(inner.entries.len(), cap) == 0 {
            return;
        }
        inner.entries.retain(|_, entry| match entry {
            Entry::Ready { answer, stored } => is_fresh(age_ms(now, *stored), failure(answer)),
            Entry::InFlight(..) => true,
        });
        let mut drop_n = evict_count(inner.entries.len(), cap);
        while drop_n > 0 {
            let oldest = inner
                .entries
                .iter()
                .filter_map(|(key, entry)| match entry {
                    Entry::Ready { stored, .. } => Some((key.clone(), *stored)),
                    Entry::InFlight(..) => None,
                })
                .min_by_key(|(_, stored)| *stored)
                .map(|(key, _)| key);
            match oldest {
                Some(key) => {
                    inner.entries.remove(&key);
                },
                None => break,
            }
            drop_n -= 1;
        }
    }

    fn resolve_shared(&self, host: &str) -> Result<DnsAnswer, watch::Receiver<Option<DnsAnswer>>> {
        let key = DnsKey(host.to_ascii_lowercase());
        let now = self.clock.now();
        let mut inner = self.inner.lock();
        let slot = match inner.entries.get(&key) {
            None => PolicySlot::Absent,
            Some(Entry::InFlight(..)) => PolicySlot::InFlight,
            Some(Entry::Ready { answer, stored }) => PolicySlot::Ready {
                age_ms: age_ms(now, *stored),
                failure: failure(answer),
            },
        };
        match decide(&slot) {
            Action::Hit => {
                if let Some(Entry::Ready { answer, .. }) = inner.entries.get(&key) {
                    return Ok(answer.clone());
                }
            },
            Action::Attach => {
                if let Some(Entry::InFlight(_, receiver)) = inner.entries.get(&key) {
                    return Err(receiver.clone());
                }
            },
            Action::Start => {},
        }
        // Nothing usable: one lookup for everyone who asks while it runs.
        inner.entries.remove(&key);
        Self::make_room(&mut inner, now, self.max_entries);
        let id = inner.next_id;
        inner.next_id += 1;
        let state = self.inner.clone();
        let lookup = self.lookup.clone();
        let clock = self.clock.clone();
        let name = key.0.clone();
        let stored_key = key.clone();
        let (sender, receiver) = watch::channel(None);
        crate::async_runtime::runtime_handle().spawn_blocking(move || {
            let started = Instant::now();
            let found = std::panic::catch_unwind(AssertUnwindSafe(|| lookup.lookup(&name)))
                .unwrap_or_else(|_| Err(io::Error::other("the name lookup panicked")));
            crate::connector::net_phase(&format!("dns {name}"), started);
            let answer: DnsAnswer = match found {
                Ok(addrs) if !addrs.is_empty() => Ok(Arc::from(addrs)),
                Ok(_) => Err(Arc::new(DnsError {
                    message: format!("no addresses for {name}"),
                })),
                Err(error) => Err(Arc::new(DnsError {
                    message: error.to_string(),
                })),
            };
            let mut inner = state.lock();
            // Store only if this lookup still owns the slot.
            if matches!(inner.entries.get(&stored_key), Some(Entry::InFlight(slot_id, _)) if *slot_id == id)
            {
                inner.entries.insert(
                    stored_key,
                    Entry::Ready {
                        answer: answer.clone(),
                        stored: clock.now(),
                    },
                );
            }
            drop(inner);
            sender.send_replace(Some(answer));
        });
        inner
            .entries
            .insert(key, Entry::InFlight(id, receiver.clone()));
        Err(receiver)
    }
}

async fn wait_for_answer(mut receiver: watch::Receiver<Option<DnsAnswer>>) -> DnsAnswer {
    match receiver.wait_for(Option::is_some).await {
        Ok(answer) => answer.clone().expect("waited for a value"),
        Err(_) => Err(Arc::new(DnsError {
            message: "the name lookup did not finish".into(),
        })),
    }
}

impl Resolver for DnsCache {
    fn resolve(&self, host: &str) -> BoxFuture<DnsAnswer> {
        match self.resolve_shared(host) {
            Ok(answer) => Box::pin(std::future::ready(answer)),
            Err(receiver) => Box::pin(wait_for_answer(receiver)),
        }
    }

    fn prefetch(&self, host: &str) {
        // A started lookup finishes and stores its answer even when nobody waits for it.
        let _ = self.resolve_shared(host);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;

    struct FakeClock(AtomicU64);
    impl FakeClock {
        fn advance(&self, by: Duration) {
            self.0.fetch_add(by.as_millis() as u64, Ordering::SeqCst);
        }
    }
    impl Clock for Arc<FakeClock> {
        fn now(&self) -> Duration {
            Duration::from_millis(self.0.load(Ordering::SeqCst))
        }
    }

    struct CountingLookup {
        calls: AtomicUsize,
        fail: bool,
        delay: Duration,
    }
    impl NameLookup for CountingLookup {
        fn lookup(&self, _host: &str) -> io::Result<Vec<SocketAddr>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(self.delay);
            if self.fail {
                Err(io::Error::other("nxdomain"))
            } else {
                Ok(vec!["192.0.2.1:0".parse().unwrap()])
            }
        }
    }

    fn cache(fail: bool, delay_ms: u64) -> (DnsCache, Arc<CountingLookup>, Arc<FakeClock>) {
        let lookup = Arc::new(CountingLookup {
            calls: AtomicUsize::new(0),
            fail,
            delay: Duration::from_millis(delay_ms),
        });
        let clock = Arc::new(FakeClock(AtomicU64::new(0)));
        let cache = DnsCache::new(lookup.clone(), Arc::new(clock.clone()));
        (cache, lookup, clock)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hit_and_an_expiry() {
        let (cache, lookup, clock) = cache(false, 0);
        assert!(cache.resolve("Example.org").await.is_ok());
        assert!(cache.resolve("example.org").await.is_ok());
        assert_eq!(
            lookup.calls.load(Ordering::SeqCst),
            1,
            "second ask is a hit"
        );
        clock.advance(Duration::from_secs(59));
        assert!(cache.resolve("example.org").await.is_ok());
        assert_eq!(
            lookup.calls.load(Ordering::SeqCst),
            1,
            "still fresh at 59 s"
        );
        clock.advance(Duration::from_secs(2));
        assert!(cache.resolve("example.org").await.is_ok());
        assert_eq!(lookup.calls.load(Ordering::SeqCst), 2, "expired at 61 s");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failures_are_kept_a_few_seconds() {
        let (cache, lookup, clock) = cache(true, 0);
        assert!(cache.resolve("dead.invalid").await.is_err());
        assert!(cache.resolve("dead.invalid").await.is_err());
        assert_eq!(lookup.calls.load(Ordering::SeqCst), 1);
        clock.advance(Duration::from_secs(6));
        assert!(cache.resolve("dead.invalid").await.is_err());
        assert_eq!(lookup.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn many_askers_share_one_lookup() {
        let (cache, lookup, _clock) = cache(false, 100);
        let cache = Arc::new(cache);
        let tasks: Vec<_> = (0..32)
            .map(|_| {
                let cache = cache.clone();
                tokio::spawn(async move { cache.resolve("shared.example").await })
            })
            .collect();
        for task in tasks {
            assert!(task.await.unwrap().is_ok());
        }
        assert_eq!(lookup.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_prefetch_fills_the_cache_for_the_next_ask() {
        let (cache, lookup, _clock) = cache(false, 20);
        cache.prefetch("prefetched.example");
        assert!(cache.resolve("prefetched.example").await.is_ok());
        assert_eq!(lookup.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_cache_does_not_grow_past_its_limit() {
        let lookup = Arc::new(CountingLookup {
            calls: AtomicUsize::new(0),
            fail: false,
            delay: Duration::ZERO,
        });
        let clock = Arc::new(FakeClock(AtomicU64::new(0)));
        let cache = DnsCache::with_capacity(lookup, Arc::new(clock.clone()), 3);
        for n in 0..10 {
            clock.advance(Duration::from_millis(10));
            assert!(cache.resolve(&format!("h{n}.example")).await.is_ok());
        }
        assert!(cache.len() <= 3, "held {}", cache.len());
    }
}
