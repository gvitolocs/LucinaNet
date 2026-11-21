// Differential test: `boundary::id_bits` against Brimstone's own `PropertyKey`. For every string, our Latin-1 and UTF-16
// parsers must agree with what `PropertyKey::string` decides (array index or not, and which index), and `index_id` must be
// bit-identical to `PropertyKey::array_index`.

use brimstone_core::runtime::{ContextBuilder, PropertyKey};
use mozjs_sys::boundary::id_bits;

/// Fixed-seed xorshift64, so the pseudo-random inputs are reproducible without any crate.
struct Xorshift64 {
    state: u64,
}

impl Xorshift64 {
    fn new(seed: u64) -> Self {
        Xorshift64 { state: seed | 1 }
    }
    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }
}

/// What Brimstone makes of a string property key: the array index, or None for a plain string key.
fn brimstone_index(mut cx: brimstone_core::runtime::Context, s: &str) -> Option<u32> {
    let handle = cx.alloc_string(s).expect("alloc_string");
    let key = PropertyKey::string(cx, handle).expect("PropertyKey::string");
    if key.is_array_index() {
        Some(key.as_array_index())
    } else {
        None
    }
}

fn check_string(cx: brimstone_core::runtime::Context, s: &str) {
    let theirs = brimstone_index(cx, s);
    let latin1 = id_bits::parse_index_latin1(s.as_bytes());
    let utf16: Vec<u16> = s.encode_utf16().collect();
    let utf16_res = id_bits::parse_index_utf16(&utf16);
    assert_eq!(latin1, theirs, "parse_index_latin1({s:?})");
    assert_eq!(utf16_res, theirs, "parse_index_utf16({s:?})");
}

#[test]
fn parse_index_matches_brimstone() {
    let cx = ContextBuilder::new().build().expect("context");

    let fixed = [
        "", "0", "00", "01", "1", "9", "10", "4294967294", "4294967295", "4294967296", "99999999999", "-0", "-1",
        "+1", "1.0", "1e3", " 1", "1 ", "٣", "１",
    ];
    for s in fixed {
        check_string(cx, s);
    }
    for i in 0..=100_000u32 {
        check_string(cx, &i.to_string());
    }

    // 50,000 pseudo-random short ASCII strings over an alphabet that exercises digits and every rejection rule.
    let mut rng = Xorshift64::new(0xA5A5_5A5A_3C3C_C3C3);
    let alphabet: &[u8] = b"0123456789-+.e x";
    for _ in 0..50_000 {
        let len = 1 + (rng.next() % 12) as usize;
        let s: String = (0..len).map(|_| alphabet[(rng.next() % alphabet.len() as u64) as usize] as char).collect();
        check_string(cx, &s);
    }
}

#[test]
fn index_id_matches_brimstone_array_index() {
    let cx = ContextBuilder::new().build().expect("context");
    let check = |i: u32| {
        let key = PropertyKey::array_index(cx, i).expect("array_index");
        assert!(key.is_array_index() && key.as_array_index() == i);
        assert_eq!(
            id_bits::index_id(i),
            key.to_raw_value().as_raw_bits(),
            "index_id({i})"
        );
    };
    for i in 0..=1_000u32 {
        check(i);
    }
    check(u32::MAX - 1); // 2^32 - 2
    check(1 << 31); // 2^31
    check((1 << 31) - 1); // 2^31 - 1
}
