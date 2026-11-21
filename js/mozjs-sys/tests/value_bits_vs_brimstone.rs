// Differential test: `boundary::value_bits` against Brimstone's own `Value` (brimstone_core). Every constructor here must
// produce bit-identical values to Brimstone's, and `kind` must agree with Brimstone's `is_*` predicates.

use brimstone_core::runtime::Value;
use mozjs_sys::boundary::value_bits::{self, Kind};
use mozjs_sys::jsval::DoubleValue;

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

/// `kind(bits)` must agree with Brimstone's predicates on the same bits.
fn check_kind(bits: u64, v: Value) {
    match value_bits::kind(bits) {
        Kind::Zero => assert!(bits == 0),
        Kind::Pointer => assert!(bits != 0 && v.is_pointer()),
        Kind::Undefined => assert!(v.is_undefined()),
        Kind::Null => assert!(v.is_null()),
        Kind::Empty => assert!(v.is_empty()),
        Kind::Bool => assert!(v.is_bool()),
        Kind::Int32 => assert!(v.is_smi()),
        Kind::Double => assert!(v.is_double()),
    }
}

#[test]
fn special_constructors_match_brimstone() {
    assert_eq!(value_bits::undefined(), Value::undefined().as_raw_bits());
    assert_eq!(value_bits::null(), Value::null().as_raw_bits());
    assert_eq!(value_bits::empty(), Value::empty().as_raw_bits());
    assert_eq!(value_bits::boolean(true), Value::bool(true).as_raw_bits());
    assert_eq!(value_bits::boolean(false), Value::bool(false).as_raw_bits());

    check_kind(value_bits::undefined(), Value::undefined());
    check_kind(value_bits::null(), Value::null());
    check_kind(value_bits::empty(), Value::empty());
    check_kind(value_bits::boolean(true), Value::bool(true));
    check_kind(value_bits::boolean(false), Value::bool(false));
}

#[test]
fn int32_matches_brimstone_smi() {
    let mut rng = Xorshift64::new(0x9E37_79B9_7F4A_7C15);
    let check = |i: i32| {
        let ours = value_bits::int32(i);
        let theirs = Value::smi(i);
        assert_eq!(ours, theirs.as_raw_bits(), "int32({i})");
        assert!(theirs.is_smi() && theirs.as_smi() == i);
        check_kind(ours, theirs);
    };
    for i in -70_000..70_000 {
        check(i);
    }
    check(i32::MIN);
    check(i32::MAX);
    for _ in 0..200_000 {
        check(rng.next() as i32);
    }
}

#[test]
fn double_matches_brimstone_number() {
    let mut rng = Xorshift64::new(0xDEAD_BEEF_CAFE_F00D);

    let check = |f: f64| {
        let v = Value::number(f);
        // The full rule, including Brimstone turning integral in-range doubles (and not -0.0) into smis:
        // jsval's DoubleValue applies that rule on top of value_bits::double.
        assert_eq!(
            DoubleValue(f).asBits_,
            v.as_raw_bits(),
            "DoubleValue({f:e} / bits {:#x})",
            f.to_bits()
        );
        if v.is_double() {
            assert_eq!(
                value_bits::double(f.to_bits()),
                v.as_raw_bits(),
                "double({f:e} / bits {:#x})",
                f.to_bits()
            );
        }
        check_kind(v.as_raw_bits(), v);
    };

    // Zeros, infinities, extremes, subnormals, integral values just outside i32 range.
    for f in [
        0.0,
        -0.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MIN,
        f64::MAX,
        f64::MIN_POSITIVE,
        f64::from_bits(1),                       // smallest subnormal
        f64::from_bits(0x000F_FFFF_FFFF_FFFF),   // largest subnormal
        2_147_483_648.0,                         // i32::MAX + 1
        -2_147_483_649.0,                        // i32::MIN - 1
        4_294_967_296.0,                         // 2^32
        1.5,
        -2.5,
        1e300,
        -1e-300,
    ] {
        check(f);
    }

    // NaN with 1,000 different payloads and both signs: all must give Brimstone's canonical NaN bits.
    for k in 0..1_000u64 {
        let payload = (k * 0x0020_0000_0000_0001) & 0x000F_FFFF_FFFF_FFFF | 1;
        check(f64::from_bits(0x7FF8_0000_0000_0000 | payload));
        check(f64::from_bits(0xFFF8_0000_0000_0000 | payload));
    }
    assert!(Value::number(f64::NAN).is_nan());

    // 200,000 pseudo-random bit patterns.
    for _ in 0..200_000 {
        check(f64::from_bits(rng.next()));
    }
}

#[test]
fn private_pointer_is_a_double_to_brimstone() {
    let mut rng = Xorshift64::new(0x1234_5678_9ABC_DEF0);
    for _ in 0..100_000 {
        let p = rng.next() & (value_bits::POINTER_LIMIT - 1);
        let bits = value_bits::private(p);
        let v = Value::from_raw_bits(bits);
        assert!(v.is_double(), "private({p:#x}) seen as double");
        assert!(!v.is_pointer(), "private({p:#x}) not seen as pointer");
        assert_eq!(value_bits::to_private(bits), p);
    }
}

#[test]
fn pointer_kind_matches_brimstone() {
    let mut rng = Xorshift64::new(0x0F0F_0F0F_0F0F_0F0F);
    for _ in 0..10_000 {
        let p = (rng.next() & (value_bits::POINTER_LIMIT - 1)) | 1;
        let bits = value_bits::pointer(p);
        let v = Value::from_raw_bits(bits);
        assert_eq!(value_bits::kind(bits), Kind::Pointer);
        assert!(v.is_pointer());
    }
}
