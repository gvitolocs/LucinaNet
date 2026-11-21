use lucinanet_net_policy::dns::*;
use lucinanet_net_policy::slots::*;

#[test]
fn lifetimes_are_the_documented_numbers() {
    assert_eq!(ttl_ms(false), 60_000);
    assert_eq!(ttl_ms(true), 5_000);
    assert_eq!(MAX_ENTRIES, 800);
    assert_eq!(MAX_PER_ORIGIN, 6);
}

#[test]
fn freshness_is_strictly_below_the_lifetime_for_every_millisecond() {
    for age in 0..=70_000u64 {
        assert_eq!(is_fresh(age, false), age < 60_000, "{age}");
        assert_eq!(is_fresh(age, true), age < 5_000, "{age}");
    }
    assert!(!is_fresh(u64::MAX, false));
}

fn action(slot: &Slot) -> &'static str {
    match decide(slot) {
        Action::Hit => "hit",
        Action::Attach => "attach",
        Action::Start => "start",
    }
}

#[test]
fn decisions() {
    assert_eq!(action(&Slot::Absent), "start");
    assert_eq!(action(&Slot::InFlight), "attach");
    assert_eq!(
        action(&Slot::Ready {
            age_ms: 0,
            failure: false
        }),
        "hit"
    );
    assert_eq!(
        action(&Slot::Ready {
            age_ms: 59_999,
            failure: false
        }),
        "hit"
    );
    assert_eq!(
        action(&Slot::Ready {
            age_ms: 60_000,
            failure: false
        }),
        "start"
    );
    assert_eq!(
        action(&Slot::Ready {
            age_ms: 4_999,
            failure: true
        }),
        "hit"
    );
    assert_eq!(
        action(&Slot::Ready {
            age_ms: 5_000,
            failure: true
        }),
        "start"
    );
}

/// However many ask while the lookup runs, one lookup starts; after it, asks hit until the entry is old.
#[test]
fn one_lookup_per_name_however_many_ask() {
    for askers in [1usize, 2, 7, 50, 1000] {
        let mut starts = 0;
        let mut slot = Slot::Absent;
        for _ in 0..askers {
            match decide(&slot) {
                Action::Start => {
                    starts += 1;
                    slot = Slot::InFlight;
                },
                Action::Attach => {},
                Action::Hit => panic!("a hit before any answer"),
            }
        }
        assert_eq!(starts, 1, "{askers} askers");
        slot = Slot::Ready {
            age_ms: 0,
            failure: false,
        };
        for age in [0u64, 1, 30_000, 59_999] {
            slot = Slot::Ready {
                age_ms: age,
                failure: false,
            };
            assert!(matches!(decide(&slot), Action::Hit));
        }
        slot = Slot::Ready {
            age_ms: 60_000,
            failure: false,
        };
        assert!(matches!(decide(&slot), Action::Start));
        let _ = slot;
    }
}

#[test]
fn capacity_is_never_exceeded() {
    for cap in 1..=900usize {
        for len in 0..=1000usize {
            let evict = evict_count(len, cap);
            assert!(
                len - evict.min(len) < cap || evict == 0 && len < cap,
                "{len} {cap}"
            );
            assert!(len - evict + 1 <= cap || len < cap && evict == 0);
            if len < cap {
                assert_eq!(evict, 0);
            } else {
                assert_eq!(len - evict + 1, cap, "exactly one place is made");
            }
        }
    }
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// Waiters arrive, some give up (the request is dropped), connections finish: slots are taken in arrival order, never
/// more than the limit are in use, and every slot comes back.
#[test]
fn slots_are_first_come_first_served_and_never_leak() {
    for seed in 0..200u64 {
        let mut rng = Lcg(seed);
        let mut queue: Vec<u32> = Vec::new(); // waiter ids in arrival order
        let mut holders: Vec<u32> = Vec::new();
        let mut live = 0usize;
        let mut next_id = 0u32;
        let mut served: Vec<u32> = Vec::new();
        let mut cancelled: Vec<u32> = Vec::new();
        for _ in 0..400 {
            match rng.next() % 4 {
                0 | 1 => {
                    queue.push(next_id);
                    next_id += 1;
                },
                2 => {
                    // a waiter gives up
                    if !queue.is_empty() {
                        let at = (rng.next() as usize) % queue.len();
                        cancelled.push(queue.remove(at));
                    }
                },
                _ => {
                    // a connection ends (success, error or drop: all release)
                    if !holders.is_empty() {
                        let at = (rng.next() as usize) % holders.len();
                        holders.remove(at);
                        live = released(live);
                    }
                },
            }
            // the front waiter takes a slot whenever it may
            while !queue.is_empty() && may_take(0, live, MAX_PER_ORIGIN) {
                let id = queue.remove(0);
                live = taken(live, MAX_PER_ORIGIN);
                holders.push(id);
                served.push(id);
            }
            assert!(live <= MAX_PER_ORIGIN);
            assert_eq!(live, holders.len());
        }
        let mut sorted = served.clone();
        sorted.sort_unstable();
        assert_eq!(served, sorted, "served in arrival order, seed {seed}");
        for _ in 0..holders.len() {
            live = released(live);
        }
        assert_eq!(live, 0, "no slot leaked, seed {seed}");
        assert!(
            !served.iter().any(|id| cancelled.contains(id)),
            "a cancelled waiter was served"
        );
    }
}

#[test]
fn leaving_keeps_the_order_of_the_others() {
    for leaver in 0..10usize {
        for a in 0..10usize {
            for b in (a + 1)..10usize {
                if a == leaver || b == leaver {
                    continue;
                }
                let (na, nb) = (
                    position_after_leave(a, leaver),
                    position_after_leave(b, leaver),
                );
                assert!(na < nb, "{a} {b} {leaver}");
            }
        }
    }
}

#[test]
fn a_double_release_stays_at_zero() {
    assert_eq!(released(released(1)), 0);
    assert_eq!(released(0), 0);
}
