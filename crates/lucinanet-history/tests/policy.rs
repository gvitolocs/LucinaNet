use lucinanet_history::ledger::Ledger;
use lucinanet_history::policy::*;

/// nsSHistory.cpp CalcMaxTotalViewers (gecko-dev f6049f2e, desktop, bias 14), as C++ computes it.
fn firefox_formula(mib: u64) -> u32 {
    let kib = (mib * 1024) as f64;
    let x = kib.ln() / 2f64.ln() - 14.0;
    let mut viewers = 0u32;
    if x > 0.0 {
        viewers = (x * x - x + 2.001) as u32;
        viewers /= 4;
    }
    viewers.min(8)
}

#[test]
fn firefox_table_matches_the_formula_for_every_mib() {
    for mib in 1..=65536u64 {
        assert_eq!(viewers_for_ram(mib), firefox_formula(mib), "{mib} MiB");
    }
    assert_eq!(viewers_for_ram(0), 0);
}

#[test]
fn firefox_table_matches_the_comment_in_the_source() {
    // RAM | DocumentViewers (desktop column of the table in nsSHistory.cpp)
    let want = [(32, 0), (64, 1), (128, 2), (256, 3), (512, 5), (768, 6), (1024, 8), (2048, 8), (3072, 8), (4096, 8)];
    for (mib, n) in want {
        assert_eq!(viewers_for_ram(mib), n, "{mib} MiB");
    }
}

#[test]
fn shared_available_splits_what_is_above_the_reserve() {
    assert_eq!(shared_available(4096, 3000, 0), 3000);
    assert_eq!(shared_available(4096, 3000, 1), 3000);
    assert_eq!(shared_available(4096, 3000, 2), 512 + (3000 - 512) / 2);
    assert_eq!(shared_available(4096, 512, 4), 512);
    assert_eq!(shared_available(4096, 100, 4), 100);
    for tabs in 1..=16u64 {
        for avail in (0..=5000u64).step_by(13) {
            let s = shared_available(4096, avail, tabs);
            assert!(s <= avail);
            if avail > 512 {
                assert!(s >= 512);
            }
        }
    }
    // Four tabs on Juno with 3000 MiB free: 512 + 622 = 1134 each, so 2 pages in all per tab, not 4.
    assert_eq!(max_entries(4096, shared_available(4096, 3000, 4), 0), 2);
}

#[test]
fn side_depth_is_the_viewer_window_at_most() {
    assert_eq!(side_depth(61440, 50000, 0), 3);
    assert_eq!(side_depth(4096, 3000, 0), 3); // 4 in all, 3 per side
    assert_eq!(side_depth(4096, 512 + 250, 0), 1);
    assert_eq!(side_depth(4096, 512 + 500, 0), 2);
    assert_eq!(side_depth(61440, 50000, PRESSURE_CRITICAL), 0);
    assert_eq!(side_depth(61440, 50000, PRESSURE_LOW), 3);
    assert_eq!(side_depth(4096, 400, 0), 0);
}

#[test]
fn pressure_levels_from_polling() {
    assert_eq!(pressure_from_available(0), PRESSURE_CRITICAL);
    assert_eq!(pressure_from_available(299), PRESSURE_CRITICAL);
    assert_eq!(pressure_from_available(300), PRESSURE_LOW);
    assert_eq!(pressure_from_available(499), PRESSURE_LOW);
    assert_eq!(pressure_from_available(500), PRESSURE_NORMAL);
    assert_eq!(pressure_from_available(u64::MAX), PRESSURE_NORMAL);
}

#[test]
fn reserve_and_ttl() {
    assert_eq!(reserve_mib(0), 500);
    assert_eq!(reserve_mib(4096), 512);
    assert_eq!(reserve_mib(61440), 7680);
    assert_eq!(ttl_secs(4096), 600);
    assert_eq!(ttl_secs(8191), 600);
    assert_eq!(ttl_secs(8192), 1800);
}

#[test]
fn juno_four_gib_sweep_is_monotone_and_gradual() {
    // Juno: 4096 MiB total. Never a fixed 0 or 2: it follows the memory left.
    let mut last = 0;
    let mut seen = std::collections::BTreeSet::new();
    for avail in 0..=4096u64 {
        let n = max_entries(4096, avail, pressure_from_available(avail));
        assert!(n >= last, "not monotone at {avail}");
        assert!(n <= 8);
        last = n;
        seen.insert(n);
    }
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
    assert_eq!(max_entries(4096, 512, 0), 0); // at the reserve: nothing
    assert_eq!(max_entries(4096, 513, 0), 0);
    assert_eq!(max_entries(4096, 512 + 250, 0), 1);
    assert_eq!(max_entries(4096, 512 + 500, 0), 2);
    assert_eq!(max_entries(4096, 3000, 0), 4); // a quarter of the machine, 1024 / 250
}

#[test]
fn big_machine_gets_eight() {
    assert_eq!(max_entries(61440, 19000, 0), 8);
    assert_eq!(max_entries(61440, 8000, 0), 1); // 8000 - 7680 = 320
    assert_eq!(max_entries(61440, 7680, 0), 0);
}

#[test]
fn pressure_overrides_memory() {
    assert_eq!(max_entries(61440, 50000, PRESSURE_CRITICAL), 0);
    assert_eq!(max_entries(61440, 50000, 200), 0);
    assert_eq!(max_entries(61440, 50000, PRESSURE_LOW), 3); // Chromium: 3 on moderate pressure
    assert_eq!(max_entries(4096, 3000, PRESSURE_LOW), 3);
    assert_eq!(max_entries(4096, 600, PRESSURE_LOW), 0); // low pressure never adds
}

#[test]
fn tiny_machines_keep_nothing() {
    for total in [0, 1, 31, 32, 63] {
        for avail in [0, total, 100_000] {
            assert_eq!(max_entries(total, avail, 0), 0);
        }
    }
}

#[test]
fn exhaustive_bounds() {
    for total in (0..=16384u64).step_by(64) {
        for avail in (0..=total + 512).step_by(37) {
            for p in 0..=3u8 {
                let n = max_entries(total, avail, p);
                assert!(n <= MAX_ENTRIES && n <= viewers_for_ram(total));
                if avail <= reserve_mib(total) || p >= 2 {
                    assert_eq!(n, 0);
                }
                if p == 1 {
                    assert!(n <= 3);
                }
                assert!(side_depth(total, avail, p) <= VIEWER_WINDOW);
            }
        }
    }
}

#[test]
fn fresh_entries_counts_from_the_newest() {
    assert_eq!(fresh_entries(&[], 600), 0);
    assert_eq!(fresh_entries(&[5], 600), 1);
    assert_eq!(fresh_entries(&[601], 600), 0);
    assert_eq!(fresh_entries(&[700, 601, 600, 10], 600), 2);
    assert_eq!(fresh_entries(&[10, 20, 30], 600), 3);
    assert_eq!(fresh_entries(&[10, 900, 30], 600), 1); // stops at the first stale one from the newest side
}

#[test]
fn plan_cases() {
    // Big machine, ttl 1800: every entry fresh, cap 8.
    let p = plan(61440, 30000, 0, &[900, 800, 100, 50, 10]);
    assert_eq!((p.keep, p.evict), (5, 0));
    // More entries than the cap: the oldest go.
    let ages: Vec<u64> = (1..=12).rev().collect();
    let p = plan(61440, 30000, 0, &ages);
    assert_eq!((p.keep, p.evict), (8, 4));
    // Juno: ttl 600, two stale.
    let p = plan(4096, 3000, 0, &[2000, 700, 300, 100]);
    assert_eq!((p.keep, p.evict), (2, 2));
    // Pressure: three (Chromium, moderate), then none.
    assert_eq!(plan(4096, 3000, PRESSURE_LOW, &[1, 2, 3, 4]).keep, 3);
    assert_eq!(plan(4096, 3000, PRESSURE_CRITICAL, &[1, 2, 3]).evict, 3);
    // No entries.
    let p = plan(4096, 3000, 0, &[]);
    assert_eq!((p.keep, p.evict), (0, 0));
}

#[test]
fn plan_sums_for_all_lengths() {
    for len in 0..20usize {
        let ages: Vec<u64> = (0..len as u64).rev().map(|a| a * 120).collect();
        for avail in [0u64, 800, 1500, 3000, 4000] {
            let p = plan(4096, avail, pressure_from_available(avail), &ages);
            assert_eq!(p.keep + p.evict, len);
            assert!(p.keep <= 8);
        }
    }
}

fn urls(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn ledger_ages() {
    let mut l = Ledger::default();
    l.update(&urls(&["a"]), 0, 0);
    l.update(&urls(&["a", "b"]), 1, 10); // a left at 10
    l.update(&urls(&["a", "b", "c"]), 2, 100); // b left at 100
    let (past, future) = l.ages(130);
    assert_eq!(past, vec![120, 30]); // a, then b (closest last)
    assert!(future.is_empty());
    l.update(&urls(&["a", "b", "c"]), 1, 200); // back to b: c left at 200
    let (past, future) = l.ages(230);
    assert_eq!(past, vec![220]);
    assert_eq!(future, vec![30]);
    // A new page from b drops c: d is new.
    l.update(&urls(&["a", "b", "d"]), 2, 300);
    let (past, future) = l.ages(310);
    assert_eq!(past, vec![300, 10]);
    assert!(future.is_empty());
}

#[test]
fn ledger_future_is_farthest_first() {
    let mut l = Ledger::default();
    l.update(&urls(&["a", "b", "c", "d"]), 3, 0);
    l.update(&urls(&["a", "b", "c", "d"]), 0, 100); // d left at 100
    let (past, future) = l.ages(150);
    assert!(past.is_empty());
    assert_eq!(future, vec![50, 150, 150]); // d (left at 100) is farthest, then c, then b (the closest, last)
}
