/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Bit-exactness of `lucinanet_raster::glsl` against swgl's C++ (`tests/oracle/shim.cc`, built with swgl's
//! flags by the `lucinanet-raster-oracle` dev-dependency).
//!
//! Every check runs 100,000 inputs from a fixed seed through both and requires identical bits; NaN results only
//! have to be NaN on both sides (payloads are not compared). Inputs mix edge values (±0, denormals, ±inf, NaN,
//! ±1e30, the ±2^31 and 2^32 conversion limits, values one ulp around .5 boundaries) with random bit patterns and
//! random values in the ranges shaders use. The list of covered functions is in `tests/oracle/README.md`.

#![allow(clippy::excessive_precision)]

use lucinanet_raster::glsl::*;
use lucinanet_raster_oracle as o;

const N: usize = 100_000;
const SEED: u64 = 0x5eed_6c51_5753_4c21;

// ---- inputs ------------------------------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(salt: &str) -> Self {
        let mut h = SEED;
        for b in salt.bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
        Rng(h)
    }
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}

const EDGES: &[f32] = &[
    0.0,
    -0.0,
    1.0,
    -1.0,
    0.5,
    -0.5,
    1.5,
    -1.5,
    2.5,
    -2.5,
    0.499_999_97,
    0.500_000_06,
    -0.499_999_97,
    1.499_999_9,
    2.0,
    3.0,
    255.0,
    127.5,
    254.5,
    0.999_999_94,
    1.000_000_1,
    f32::MIN_POSITIVE,
    -f32::MIN_POSITIVE,
    1.0e-45,
    -1.0e-45,
    1.17e-38,
    5.0e-39,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::NAN,
    -f32::NAN,
    1.0e30,
    -1.0e30,
    f32::MAX,
    f32::MIN,
    8_388_608.0,
    8_388_607.5,
    16_777_216.0,
    2_147_483_648.0,
    -2_147_483_648.0,
    2_147_483_520.0,
    4_294_967_296.0,
    3.0e9,
    -3.0e9,
    1.0e10,
    0.1,
    -0.1,
    std::f32::consts::PI,
    -std::f32::consts::PI,
    std::f32::consts::FRAC_PI_2,
    65535.0,
    1.0e-10,
];

/// The shape of the random part of the inputs.
#[derive(Clone, Copy)]
enum Dom {
    /// Edges, any bit pattern, small values, values near .5 boundaries.
    Any,
    /// Like `Any` without NaN/inf-heavy random bits: mostly finite, moderate values (for transcendentals).
    Finite,
    /// [0, 1] colour values, with some edges.
    Unit,
    /// Values a conversion to a narrow integer keeps defined: [0, 255].
    Byte,
}

fn near_half(r: &mut Rng) -> f32 {
    let k = (r.below(2001) as i64 - 1000) as f32;
    let base = k + 0.5;
    match r.below(3) {
        0 => base,
        1 => f32::from_bits(base.to_bits().wrapping_add(1)),
        _ => f32::from_bits(base.to_bits().wrapping_sub(1)),
    }
}

fn gen(r: &mut Rng, dom: Dom) -> f32 {
    let pick = r.below(100);
    match dom {
        Dom::Any => match pick {
            0..=14 => EDGES[r.below(EDGES.len() as u64) as usize],
            15..=44 => f32::from_bits(r.next() as u32),
            45..=64 => r.range(-10.0, 10.0),
            65..=79 => near_half(r),
            80..=89 => r.unit(),
            _ => r.range(-1.0e6, 1.0e6),
        },
        Dom::Finite => match pick {
            0..=9 => EDGES[r.below(EDGES.len() as u64) as usize],
            10..=19 => {
                let f = f32::from_bits(r.next() as u32);
                if f.is_finite() {
                    f
                } else {
                    r.range(-100.0, 100.0)
                }
            }
            20..=49 => r.range(-10.0, 10.0),
            50..=64 => near_half(r),
            65..=84 => r.unit(),
            _ => r.range(-200.0, 200.0),
        },
        Dom::Unit => match pick {
            0..=4 => EDGES[r.below(EDGES.len() as u64) as usize],
            5..=14 => near_half(r) * (1.0 / 1024.0),
            _ => r.unit(),
        },
        Dom::Byte => match pick {
            0..=9 => [0.0, 0.5, 1.0, 127.5, 254.5, 255.0, 0.499_999_97, 254.999_98][r.below(8) as usize],
            _ => r.range(0.0, 255.0),
        },
    }
}

fn floats<const K: usize>(r: &mut Rng, dom: Dom) -> [f32; K] {
    core::array::from_fn(|_| gen(r, dom))
}

fn ints(r: &mut Rng) -> [i32; 4] {
    const IEDGE: &[i32] =
        &[0, 1, -1, i32::MIN, i32::MAX, 255, 256, -256, 0x7fff, 0x8000, 0xffff, 0x10000, 1 << 24, (1 << 24) + 1];
    core::array::from_fn(|_| match r.below(4) {
        0 => IEDGE[r.below(IEDGE.len() as u64) as usize],
        1 => (r.below(2001) as i32) - 1000,
        _ => r.next() as i32,
    })
}

// ---- comparison --------------------------------------------------------------------------------------------------

trait Bits: Copy + std::fmt::Debug {
    fn same(self, other: Self) -> bool;
}
impl Bits for f32 {
    fn same(self, o: f32) -> bool {
        self.to_bits() == o.to_bits() || (self.is_nan() && o.is_nan())
    }
}
macro_rules! int_bits {
    ($($t:ty),*) => {$(impl Bits for $t { fn same(self, o: $t) -> bool { self == o } })*};
}
int_bits!(i32, u32, i16, u16, u8, bool);
impl<T: Bits, const K: usize> Bits for [T; K] {
    fn same(self, o: Self) -> bool {
        self.iter().zip(o.iter()).all(|(a, b)| a.same(*b))
    }
}

struct Check {
    name: &'static str,
    failures: usize,
    examples: Vec<String>,
}

impl Check {
    fn new(name: &'static str) -> Self {
        Check { name, failures: 0, examples: Vec::new() }
    }
    fn cmp<I: std::fmt::Debug, T: Bits>(&mut self, input: I, rust: T, cpp: T) {
        if !rust.same(cpp) {
            self.failures += 1;
            if self.examples.len() < 6 {
                self.examples.push(format!("  input {input:?}\n    rust {rust:?}\n    c++  {cpp:?}"));
            }
        }
    }
    fn finish(self) {
        if self.failures != 0 {
            FAILED.with(|f| {
                f.borrow_mut().push(format!(
                    "{}: {} of {N} inputs differ from the C++\n{}",
                    self.name,
                    self.failures,
                    self.examples.join("\n")
                ))
            });
        }
    }
}

thread_local! {
    /// The failed checks of the running test (each test runs on its own thread).
    static FAILED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Fails the test if any of its checks differed from the C++, listing all of them.
fn report() {
    let failed = FAILED.with(|f| std::mem::take(&mut *f.borrow_mut()));
    assert!(failed.is_empty(), "{} check(s) differ from the C++:\n{}", failed.len(), failed.join("\n"));
}

fn fl(a: [f32; 4]) -> Float {
    Float::new(a)
}

fn check_f1(
    name: &'static str,
    dom: Dom,
    cpp: unsafe extern "C" fn(*const f32, *mut f32),
    rust: impl Fn(Float) -> Float,
) {
    let mut r = Rng::new(name);
    let mut c = Check::new(name);
    for _ in 0..N {
        let a = floats::<4>(&mut r, dom);
        let mut want = [0f32; 4];
        // SAFETY: the shim reads 4 floats from `a` and writes 4 to `want`.
        unsafe { cpp(a.as_ptr(), want.as_mut_ptr()) };
        c.cmp(a, rust(fl(a)).0, want);
    }
    c.finish();
}

type CppF2 = unsafe extern "C" fn(*const f32, *const f32, *mut f32);

fn check_f2(name: &'static str, da: Dom, db: Dom, cpp: CppF2, rust: impl Fn(Float, Float) -> Float) {
    let mut r = Rng::new(name);
    let mut c = Check::new(name);
    for _ in 0..N {
        let a = floats::<4>(&mut r, da);
        let b = floats::<4>(&mut r, db);
        let mut want = [0f32; 4];
        // SAFETY: the shim reads 4 floats from each input and writes 4.
        unsafe { cpp(a.as_ptr(), b.as_ptr(), want.as_mut_ptr()) };
        c.cmp((a, b), rust(fl(a), fl(b)).0, want);
    }
    c.finish();
}

type CppF3 = unsafe extern "C" fn(*const f32, *const f32, *const f32, *mut f32);

fn check_f3(name: &'static str, d: [Dom; 3], cpp: CppF3, rust: impl Fn(Float, Float, Float) -> Float) {
    let mut r = Rng::new(name);
    let mut c = Check::new(name);
    for _ in 0..N {
        let a = floats::<4>(&mut r, d[0]);
        let b = floats::<4>(&mut r, d[1]);
        let z = floats::<4>(&mut r, d[2]);
        let mut want = [0f32; 4];
        // SAFETY: the shim reads 4 floats from each input and writes 4.
        unsafe { cpp(a.as_ptr(), b.as_ptr(), z.as_ptr(), want.as_mut_ptr()) };
        c.cmp((a, b, z), rust(fl(a), fl(b), fl(z)).0, want);
    }
    c.finish();
}

type CppFFI = unsafe extern "C" fn(*const f32, *const f32, *mut i32);

fn check_ff_i(name: &'static str, da: Dom, db: Dom, cpp: CppFFI, rust: impl Fn(Float, Float) -> I32) {
    let mut r = Rng::new(name);
    let mut c = Check::new(name);
    for _ in 0..N {
        let a = floats::<4>(&mut r, da);
        let b = floats::<4>(&mut r, db);
        let mut want = [0i32; 4];
        // SAFETY: the shim reads 4 floats from each input and writes 4 ints.
        unsafe { cpp(a.as_ptr(), b.as_ptr(), want.as_mut_ptr()) };
        c.cmp((a, b), rust(fl(a), fl(b)).0, want);
    }
    c.finish();
}

fn check_f_i(
    name: &'static str,
    dom: Dom,
    cpp: unsafe extern "C" fn(*const f32, *mut i32),
    rust: impl Fn(Float) -> I32,
) {
    let mut r = Rng::new(name);
    let mut c = Check::new(name);
    for _ in 0..N {
        let a = floats::<4>(&mut r, dom);
        let mut want = [0i32; 4];
        // SAFETY: the shim reads 4 floats and writes 4 ints.
        unsafe { cpp(a.as_ptr(), want.as_mut_ptr()) };
        c.cmp(a, rust(fl(a)).0, want);
    }
    c.finish();
}

fn check_ii_i(
    name: &'static str,
    cpp: unsafe extern "C" fn(*const i32, *const i32, *mut i32),
    rust: impl Fn(I32, I32) -> I32,
) {
    let mut r = Rng::new(name);
    let mut c = Check::new(name);
    for _ in 0..N {
        let a = ints(&mut r);
        let b = ints(&mut r);
        let mut want = [0i32; 4];
        // SAFETY: the shim reads 4 ints from each input and writes 4.
        unsafe { cpp(a.as_ptr(), b.as_ptr(), want.as_mut_ptr()) };
        c.cmp((a, b), rust(I32::new(a), I32::new(b)).0, want);
    }
    c.finish();
}

fn check_s1(name: &'static str, dom: Dom, cpp: unsafe extern "C" fn(f32) -> f32, rust: impl Fn(f32) -> f32) {
    let mut r = Rng::new(name);
    let mut c = Check::new(name);
    for _ in 0..N {
        let a = gen(&mut r, dom);
        // SAFETY: plain value call.
        let want = unsafe { cpp(a) };
        c.cmp(a, rust(a), want);
    }
    c.finish();
}

fn check_s2(
    name: &'static str,
    da: Dom,
    db: Dom,
    cpp: unsafe extern "C" fn(f32, f32) -> f32,
    rust: impl Fn(f32, f32) -> f32,
) {
    let mut r = Rng::new(name);
    let mut c = Check::new(name);
    for _ in 0..N {
        let a = gen(&mut r, da);
        let b = gen(&mut r, db);
        // SAFETY: plain value call.
        let want = unsafe { cpp(a, b) };
        c.cmp((a, b), rust(a, b), want);
    }
    c.finish();
}

/// A generic driver for the wrappers with their own shapes: `make` builds the inputs, `cpp` and `rust` compute
/// the outputs as plain arrays.
fn check_with<I: Copy + std::fmt::Debug, T: Bits>(
    name: &'static str,
    make: impl Fn(&mut Rng) -> I,
    cpp: impl Fn(I) -> T,
    rust: impl Fn(I) -> T,
) {
    let mut r = Rng::new(name);
    let mut c = Check::new(name);
    for _ in 0..N {
        let i = make(&mut r);
        c.cmp(i, rust(i), cpp(i));
    }
    c.finish();
}

fn v2(a: &[f32]) -> vec2 {
    vec2::new(fl(a[0..4].try_into().unwrap()), fl(a[4..8].try_into().unwrap()))
}
fn v3(a: &[f32]) -> vec3 {
    vec3::new(fl(a[0..4].try_into().unwrap()), fl(a[4..8].try_into().unwrap()), fl(a[8..12].try_into().unwrap()))
}
fn v4(a: &[f32]) -> vec4 {
    vec4::new(
        fl(a[0..4].try_into().unwrap()),
        fl(a[4..8].try_into().unwrap()),
        fl(a[8..12].try_into().unwrap()),
        fl(a[12..16].try_into().unwrap()),
    )
}
fn out2(v: vec2) -> [f32; 8] {
    let mut o = [0f32; 8];
    o[..4].copy_from_slice(&v.x.0);
    o[4..].copy_from_slice(&v.y.0);
    o
}
fn out3(v: vec3) -> [f32; 12] {
    let mut o = [0f32; 12];
    o[..4].copy_from_slice(&v.x.0);
    o[4..8].copy_from_slice(&v.y.0);
    o[8..].copy_from_slice(&v.z.0);
    o
}
fn out4(v: vec4) -> [f32; 16] {
    let mut o = [0f32; 16];
    o[..4].copy_from_slice(&v.x.0);
    o[4..8].copy_from_slice(&v.y.0);
    o[8..12].copy_from_slice(&v.z.0);
    o[12..].copy_from_slice(&v.w.0);
    o
}

// ---- lane arithmetic and comparisons ----------------------------------------------------------------------------

#[test]
fn lane_arithmetic() {
    check_f2("add", Dom::Any, Dom::Any, o::o_add, |x, y| x + y);
    check_f2("sub", Dom::Any, Dom::Any, o::o_sub, |x, y| x - y);
    check_f2("mul", Dom::Any, Dom::Any, o::o_mul, |x, y| x * y);
    check_f2("div", Dom::Any, Dom::Any, o::o_div, |x, y| x / y);
    check_f1("neg", Dom::Any, o::o_neg, |x| -x);
    check_f3("madd", [Dom::Any; 3], o::o_madd, |x, y, z| x * y + z);
    check_f2("sub_scalar_lhs", Dom::Any, Dom::Any, o::o_sub_scalar_lhs, |x, y| 1.0f32 - x * y);
    report();
}

#[test]
fn lane_comparisons() {
    check_ff_i("lt", Dom::Any, Dom::Any, o::o_lt, |x, y| x.cmp_lt(y));
    check_ff_i("le", Dom::Any, Dom::Any, o::o_le, |x, y| x.cmp_le(y));
    check_ff_i("gt", Dom::Any, Dom::Any, o::o_gt, |x, y| x.cmp_gt(y));
    check_ff_i("ge", Dom::Any, Dom::Any, o::o_ge, |x, y| x.cmp_ge(y));
    check_ff_i("eq", Dom::Any, Dom::Any, o::o_eq, |x, y| x.cmp_eq(y));
    check_ff_i("ne", Dom::Any, Dom::Any, o::o_ne, |x, y| x.cmp_ne(y));
    check_ii_i("i_gt", o::o_i_gt, |x, y| x.cmp_gt(y));
    report();
}

#[test]
fn integer_lanes() {
    check_ii_i("i_add", o::o_i_add, |x, y| x + y);
    check_ii_i("i_mul", o::o_i_mul, |x, y| x * y);
    check_ii_i("i_and_not", o::o_i_and_not, |x, y| x & !y);
    check_ii_i("i_xor", o::o_i_xor, |x, y| x ^ y);
    check_ii_i("i_clamp_0_y", o::o_i_clamp_0_y, |x, y| clamp(x, I32::splat(0), y));
    check_with(
        "i_shr",
        |r| (ints(r), r.below(32) as i32),
        // SAFETY: 4 ints in, 4 out.
        |(a, n)| unsafe {
            let mut o = [0i32; 4];
            o::o_i_shr(a.as_ptr(), n, o.as_mut_ptr());
            o
        },
        |(a, n)| (I32::new(a) >> n).0,
    );
    check_with(
        "i_shl",
        |r| (ints(r), r.below(32) as i32),
        // SAFETY: 4 ints in, 4 out.
        |(a, n)| unsafe {
            let mut o = [0i32; 4];
            o::o_i_shl(a.as_ptr(), n, o.as_mut_ptr());
            o
        },
        |(a, n)| (I32::new(a) << n).0,
    );
    check_with(
        "u_shr",
        |r| (ints(r).map(|v| v as u32), r.below(32) as i32),
        // SAFETY: 4 ints in, 4 out.
        |(a, n)| unsafe {
            let mut o = [0u32; 4];
            o::o_u_shr(a.as_ptr(), n, o.as_mut_ptr());
            o
        },
        |(a, n)| (U32::new(a) >> n).0,
    );
    report();
}

#[test]
fn selects_and_mask_tests() {
    let masks = |r: &mut Rng| -> [i32; 4] {
        core::array::from_fn(|_| match r.below(5) {
            0 => r.next() as i32,
            1 | 2 => -1,
            _ => 0,
        })
    };
    check_with(
        "if_then_else",
        |r| (masks(r), floats::<4>(r, Dom::Any), floats::<4>(r, Dom::Any)),
        // SAFETY: 4 lanes per argument.
        |(c, a, b)| unsafe {
            let mut o = [0f32; 4];
            o::o_if_then_else(c.as_ptr(), a.as_ptr(), b.as_ptr(), o.as_mut_ptr());
            o
        },
        |(c, a, b)| if_then_else(I32::new(c), fl(a), fl(b)).0,
    );
    check_with(
        "if_then",
        |r| (masks(r), floats::<4>(r, Dom::Any)),
        // SAFETY: 4 lanes per argument.
        |(c, a)| unsafe {
            let mut o = [0f32; 4];
            o::o_if_then(c.as_ptr(), a.as_ptr(), o.as_mut_ptr());
            o
        },
        |(c, a)| if_then(I32::new(c), fl(a)).0,
    );
    // Masks from comparisons (all ones / zero) as the C++ functions expect, plus arbitrary bits to pin the
    // sign-bit (SSE2) reading.
    let cond = |r: &mut Rng| -> [i32; 4] {
        if r.below(4) == 0 {
            core::array::from_fn(|_| r.next() as i32)
        } else {
            let all = r.below(3);
            core::array::from_fn(|_| if all == 0 || (all == 1 && r.below(2) == 0) { -1 } else { 0 })
        }
    };
    type CppTest = unsafe extern "C" fn(*const i32) -> i32;
    type MaskTest = (&'static str, CppTest, fn(Bool) -> bool);
    let tests: [MaskTest; 4] = [
        ("test_all", o::o_test_all, test_all),
        ("test_any", o::o_test_any, test_any),
        ("test_none", o::o_test_none, test_none),
        ("test_equal", o::o_test_equal, test_equal),
    ];
    for (name, cpp, rust) in tests {
        check_with(
            name,
            cond,
            // SAFETY: 4 ints in.
            |c| unsafe { cpp(c.as_ptr()) != 0 },
            |c| rust(I32::new(c)),
        );
    }
    report();
}

// ---- conversions and rounding modes -----------------------------------------------------------------------------

#[test]
fn conversions() {
    check_f_i("cvt_f32_i32", Dom::Any, o::o_cvt_f32_i32, |x| x.convert::<i32>());
    check_f_i("cast_f32", Dom::Any, o::o_cast_f32, cast);
    check_f1("cast_roundtrip", Dom::Any, o::o_cast_roundtrip, |x| cast(cast(x)));
    check_with(
        "cvt_f32_u32",
        |r| floats::<4>(r, Dom::Any),
        // SAFETY: 4 lanes in and out.
        |a| unsafe {
            let mut o = [0u32; 4];
            o::o_cvt_f32_u32(a.as_ptr(), o.as_mut_ptr());
            o
        },
        |a| fl(a).convert::<u32>().0,
    );
    check_with(
        "cvt_i32_f32",
        ints,
        // SAFETY: 4 lanes in and out.
        |a| unsafe {
            let mut o = [0f32; 4];
            o::o_cvt_i32_f32(a.as_ptr(), o.as_mut_ptr());
            o
        },
        |a| I32::new(a).convert::<f32>().0,
    );
    check_with(
        "cvt_u32_f32",
        |r| ints(r).map(|v| v as u32),
        // SAFETY: 4 lanes in and out.
        |a| unsafe {
            let mut o = [0f32; 4];
            o::o_cvt_u32_f32(a.as_ptr(), o.as_mut_ptr());
            o
        },
        |a| U32::new(a).convert::<f32>().0,
    );
    check_with(
        "cast_u32",
        |r| ints(r).map(|v| v as u32),
        // SAFETY: 4 lanes in and out.
        |a| unsafe {
            let mut o = [0f32; 4];
            o::o_cast_u32(a.as_ptr(), o.as_mut_ptr());
            o
        },
        |a| cast(U32::new(a)).0,
    );
    check_with(
        "make_int_lanes",
        |r| floats::<4>(r, Dom::Any),
        // SAFETY: 4 lanes in and out.
        |a| unsafe {
            let mut o = [0i32; 4];
            o::o_make_int_lanes(a.as_ptr(), o.as_mut_ptr());
            o
        },
        |a| make_int(fl(a)).0,
    );
    check_with(
        "make_uint_lanes",
        |r| floats::<4>(r, Dom::Any),
        // SAFETY: 4 lanes in and out.
        |a| unsafe {
            let mut o = [0u32; 4];
            o::o_make_uint_lanes(a.as_ptr(), o.as_mut_ptr());
            o
        },
        |a| make_uint(fl(a)).0,
    );
    check_with(
        "make_float_u32",
        |r| ints(r).map(|v| v as u32),
        // SAFETY: 4 lanes in and out.
        |a| unsafe {
            let mut o = [0f32; 4];
            o::o_make_float_u32(a.as_ptr(), o.as_mut_ptr());
            o
        },
        |a| make_float(U32::new(a)).0,
    );
    // SAFETY (all three): plain value calls.
    check_with("make_int_s", |r| gen(r, Dom::Any), |a| unsafe { o::o_make_int_s(a) }, make_int);
    check_with("make_uint_s", |r| gen(r, Dom::Any), |a| unsafe { o::o_make_uint_s(a) }, make_uint);
    check_with("make_float_u32_s", |r| r.next() as u32, |a| unsafe { o::o_make_float_u32_s(a) }, make_float);
    report();
}

#[test]
fn rounding_modes() {
    check_ff_i("roundeven", Dom::Any, Dom::Any, o::o_roundeven, roundeven);
    check_ff_i("roundzero", Dom::Any, Dom::Any, o::o_roundzero, roundzero);
    check_ff_i("roundfast", Dom::Any, Dom::Any, o::o_roundfast, roundfast);
    check_f_i("round_pixel", Dom::Unit, o::o_round_pixel, |x| round_pixel(x, 255.0));
    // SAFETY (both): plain value calls.
    check_with(
        "roundeven_s",
        |r| (gen(r, Dom::Any), gen(r, Dom::Any)),
        |(a, b)| unsafe { o::o_roundeven_s(a, b) },
        |(a, b)| roundeven_scalar(a, b),
    );
    check_with(
        "roundzero_s",
        |r| (gen(r, Dom::Any), gen(r, Dom::Any)),
        |(a, b)| unsafe { o::o_roundzero_s(a, b) },
        |(a, b)| roundzero_scalar(a, b),
    );
    report();
}

// ---- approximations ---------------------------------------------------------------------------------------------

#[test]
fn approximations() {
    check_f1("recip", Dom::Any, o::o_recip, recip);
    check_f1("inversesqrt", Dom::Any, o::o_inversesqrt, inversesqrt);
    check_f1("sqrt", Dom::Any, o::o_sqrt, sqrt);
    check_f1("approx_log2", Dom::Any, o::o_approx_log2, approx_log2);
    check_f1("approx_pow2", Dom::Finite, o::o_approx_pow2, approx_pow2);
    check_f1("exp", Dom::Finite, o::o_exp, exp);
    check_f1("exp2", Dom::Finite, o::o_exp2, exp2);
    check_f1("log", Dom::Any, o::o_log, log);
    check_f1("log2", Dom::Any, o::o_log2, log2);
    check_f2("pow", Dom::Any, Dom::Finite, o::o_pow, pow);
    check_s1("recip_s", Dom::Any, o::o_recip_s, recip);
    check_s1("inversesqrt_s", Dom::Any, o::o_inversesqrt_s, inversesqrt);
    check_s1("sqrt_s", Dom::Any, o::o_sqrt_s, sqrt);
    report();
}

#[test]
fn transcendentals() {
    check_f1("sin", Dom::Any, o::o_sin, sin);
    check_f1("cos", Dom::Any, o::o_cos, cos);
    check_f1("tan", Dom::Any, o::o_tan, tan);
    check_f1("atan", Dom::Any, o::o_atan, atan);
    check_f2("atan2", Dom::Any, Dom::Any, o::o_atan2, atan2);
    check_s1("exp_s", Dom::Any, o::o_exp_s, exp);
    check_s1("exp2_s", Dom::Any, o::o_exp2_s, exp2);
    check_s1("log_s", Dom::Any, o::o_log_s, log);
    check_s1("log2_s", Dom::Any, o::o_log2_s, log2);
    check_s1("sin_s", Dom::Any, o::o_sin_s, sin);
    check_s1("cos_s", Dom::Any, o::o_cos_s, cos);
    check_s1("atan_s", Dom::Any, o::o_atan_s, atan);
    check_s2("atan2_s", Dom::Any, Dom::Any, o::o_atan2_s, atan2);
    check_s2("pow_s", Dom::Any, Dom::Any, o::o_pow_s, pow);
    report();
}

// ---- rounding and shaping built-ins -----------------------------------------------------------------------------

#[test]
fn shaping_builtins() {
    check_f1("fract", Dom::Any, o::o_fract, fract);
    check_f1("floor", Dom::Any, o::o_floor, floor);
    check_f1("ceil", Dom::Any, o::o_ceil, ceil);
    check_f1("round", Dom::Any, o::o_round, round);
    check_f1("abs", Dom::Any, o::o_abs, abs);
    check_f1("sign", Dom::Any, o::o_sign, sign);
    check_f2("min", Dom::Any, Dom::Any, o::o_min, min);
    check_f2("max", Dom::Any, Dom::Any, o::o_max, max);
    check_f3("clamp", [Dom::Any, Dom::Unit, Dom::Unit], o::o_clamp, clamp);
    check_f3("mix", [Dom::Any, Dom::Any, Dom::Unit], o::o_mix, mix);
    check_f2("step", Dom::Any, Dom::Any, o::o_step, step);
    check_f2("mod", Dom::Any, Dom::Any, o::o_mod, r#mod);
    check_s1("floor_s", Dom::Any, o::o_floor_s, floor);
    check_s1("ceil_s", Dom::Any, o::o_ceil_s, ceil);
    check_s1("round_s", Dom::Any, o::o_round_s, round);
    check_s1("fract_s", Dom::Any, o::o_fract_s, fract);
    check_s1("abs_s", Dom::Any, o::o_abs_s, abs);
    check_s1("sign_s", Dom::Any, o::o_sign_s, sign);
    check_s2("min_s", Dom::Any, Dom::Any, o::o_min_s, min);
    check_s2("max_s", Dom::Any, Dom::Any, o::o_max_s, max);
    check_s2("step_s", Dom::Any, Dom::Any, o::o_step_s, step);
    check_with(
        "mix_s",
        |r| (gen(r, Dom::Any), gen(r, Dom::Any), gen(r, Dom::Unit)),
        // SAFETY: plain value call.
        |(x, y, a)| unsafe { o::o_mix_s(x, y, a) },
        |(x, y, a)| mix(x, y, a),
    );
    check_with(
        "clamp_s",
        |r| (gen(r, Dom::Any), gen(r, Dom::Unit), gen(r, Dom::Unit)),
        // SAFETY: plain value call.
        |(x, y, a)| unsafe { o::o_clamp_s(x, y, a) },
        |(x, y, a)| clamp(x, y, a),
    );
    check_with(
        "init_interp",
        |r| (gen(r, Dom::Any), gen(r, Dom::Any)),
        // SAFETY: writes 4 floats.
        |(a, b)| unsafe {
            let mut o = [0f32; 4];
            o::o_init_interp(a, b, o.as_mut_ptr());
            o
        },
        |(a, b)| init_interp(a, b).0,
    );
    check_with(
        "dfdx",
        |r| floats::<4>(r, Dom::Any),
        // SAFETY: reads 4 floats.
        |a| unsafe { o::o_dfdx(a.as_ptr()) },
        |a| dFdx(fl(a)),
    );
    report();
}

// ---- vectors ----------------------------------------------------------------------------------------------------

macro_rules! lanes_fn {
    // A wrapper (in: K1 floats, K2 floats) -> KO outputs of type T.
    ($cpp:path, [$k1:literal, $k2:literal] -> [$t:ty; $ko:literal]) => {
        |(a, b): ([f32; $k1], [f32; $k2])| -> [$t; $ko] {
            let mut o = [<$t>::default(); $ko];
            // SAFETY: the wrapper reads $k1 and $k2 floats and writes $ko values.
            unsafe { $cpp(a.as_ptr(), b.as_ptr(), o.as_mut_ptr()) };
            o
        }
    };
    ($cpp:path, [$k1:literal] -> [$t:ty; $ko:literal]) => {
        |a: [f32; $k1]| -> [$t; $ko] {
            let mut o = [<$t>::default(); $ko];
            // SAFETY: the wrapper reads $k1 floats and writes $ko values.
            unsafe { $cpp(a.as_ptr(), o.as_mut_ptr()) };
            o
        }
    };
}

#[test]
fn vector_builtins() {
    let pair = |d: Dom| move |r: &mut Rng| (floats::<8>(r, d), floats::<8>(r, d));
    check_with("dot2", pair(Dom::Any), lanes_fn!(o::o_dot2, [8, 8] -> [f32; 4]), |(a, b)| dot(v2(&a), v2(&b)).0);
    check_with(
        "dot3",
        |r| (floats::<12>(r, Dom::Any), floats::<12>(r, Dom::Any)),
        lanes_fn!(o::o_dot3, [12, 12] -> [f32; 4]),
        |(a, b)| dot(v3(&a), v3(&b)).0,
    );
    check_with(
        "dot3_s",
        |r| (floats::<3>(r, Dom::Any), floats::<3>(r, Dom::Any)),
        // SAFETY: reads 3 floats from each.
        |(a, b)| unsafe { o::o_dot3_s(a.as_ptr(), b.as_ptr()) },
        |(a, b)| dot(vec3_scalar::new(a[0], a[1], a[2]), vec3_scalar::new(b[0], b[1], b[2])),
    );
    check_with(
        "dot2_s",
        |r| (floats::<2>(r, Dom::Any), floats::<2>(r, Dom::Any)),
        // SAFETY: reads 2 floats from each.
        |(a, b)| unsafe { o::o_dot2_s(a.as_ptr(), b.as_ptr()) },
        |(a, b)| dot(vec2_scalar::new(a[0], a[1]), vec2_scalar::new(b[0], b[1])),
    );
    check_with("length2", |r| floats::<8>(r, Dom::Any), lanes_fn!(o::o_length2, [8] -> [f32; 4]), |a| length(v2(&a)).0);
    check_with(
        "length2_s",
        |r| (gen(r, Dom::Any), gen(r, Dom::Any)),
        // SAFETY: plain value call.
        |(x, y)| unsafe { o::o_length2_s(x, y) },
        |(x, y)| length(vec2_scalar::new(x, y)),
    );
    check_with(
        "normalize2",
        |r| floats::<8>(r, Dom::Any),
        lanes_fn!(o::o_normalize2, [8] -> [f32; 8]),
        |a| out2(normalize(v2(&a))),
    );
    check_with(
        "normalize2_s",
        |r| floats::<2>(r, Dom::Any),
        lanes_fn!(o::o_normalize2_s, [2] -> [f32; 2]),
        |a| {
            let n = normalize(vec2_scalar::new(a[0], a[1]));
            [n.x, n.y]
        },
    );
    check_with("distance2", pair(Dom::Any), lanes_fn!(o::o_distance2, [8, 8] -> [f32; 4]), |(a, b)| {
        distance(v2(&a), v2(&b)).0
    });
    check_with(
        "fwidth",
        |r| floats::<8>(r, Dom::Any),
        lanes_fn!(o::o_fwidth, [8] -> [f32; 2]),
        |a| {
            let w = fwidth(v2(&a));
            [w.x, w.y]
        },
    );
    check_with(
        "mix_vec4",
        |r| (floats::<16>(r, Dom::Any), floats::<16>(r, Dom::Any), floats::<4>(r, Dom::Unit)),
        |(a, b, c)| {
            let mut o = [0f32; 16];
            // SAFETY: 16 + 16 + 4 floats in, 16 out.
            unsafe { o::o_mix_vec4(a.as_ptr(), b.as_ptr(), c.as_ptr(), o.as_mut_ptr()) };
            o
        },
        |(a, b, c)| out4(mix(v4(&a), v4(&b), fl(c))),
    );
    check_with(
        "mix_vec4_scalar",
        |r| (floats::<4>(r, Dom::Any), floats::<4>(r, Dom::Any), floats::<4>(r, Dom::Unit)),
        |(a, b, c)| {
            let mut o = [0f32; 4];
            // SAFETY: 4 floats per argument.
            unsafe { o::o_mix_vec4_scalar(a.as_ptr(), b.as_ptr(), c.as_ptr(), o.as_mut_ptr()) };
            o
        },
        |(a, b, c)| {
            let s = |v: [f32; 4]| vec4_scalar::new(v[0], v[1], v[2], v[3]);
            let m = mix(s(a), s(b), s(c));
            [m.x, m.y, m.z, m.w]
        },
    );
    check_with(
        "clamp_vec4",
        |r| (floats::<16>(r, Dom::Any), floats::<4>(r, Dom::Unit), floats::<4>(r, Dom::Unit)),
        |(a, b, c)| {
            let mut o = [0f32; 16];
            // SAFETY: 16 + 4 + 4 floats in, 16 out.
            unsafe { o::o_clamp_vec4(a.as_ptr(), b.as_ptr(), c.as_ptr(), o.as_mut_ptr()) };
            o
        },
        |(a, b, c)| out4(clamp(v4(&a), fl(b), fl(c))),
    );
    check_with("step_vec2", pair(Dom::Any), lanes_fn!(o::o_step_vec2, [8, 8] -> [f32; 8]), |(a, b)| {
        out2(step(v2(&a), v2(&b)))
    });
    check_with(
        "floor_vec4",
        |r| floats::<16>(r, Dom::Any),
        lanes_fn!(o::o_floor_vec4, [16] -> [f32; 16]),
        |a| out4(floor(v4(&a))),
    );
    check_with(
        "pow_vec3",
        |r| (floats::<12>(r, Dom::Any), floats::<12>(r, Dom::Finite)),
        lanes_fn!(o::o_pow_vec3, [12, 12] -> [f32; 12]),
        |(a, b)| out3(pow(v3(&a), v3(&b))),
    );
    check_with(
        "pow_vec2",
        |r| (floats::<8>(r, Dom::Any), floats::<8>(r, Dom::Finite)),
        lanes_fn!(o::o_pow_vec2, [8, 8] -> [f32; 8]),
        |(a, b)| out2(pow(v2(&a), v2(&b))),
    );
    check_with("mod_vec2", pair(Dom::Any), lanes_fn!(o::o_mod_vec2, [8, 8] -> [f32; 8]), |(a, b)| {
        out2(r#mod(v2(&a), v2(&b)))
    });
    check_with(
        "vec4_div_float",
        |r| (floats::<16>(r, Dom::Any), floats::<4>(r, Dom::Any)),
        lanes_fn!(o::o_vec4_div_float, [16, 4] -> [f32; 16]),
        |(a, b)| out4(v4(&a) / fl(b)),
    );
    check_with(
        "vec2_mul_scalar",
        |r| (floats::<8>(r, Dom::Any), floats::<2>(r, Dom::Any)),
        lanes_fn!(o::o_vec2_mul_scalar, [8, 2] -> [f32; 8]),
        |(a, b)| out2(v2(&a) * vec2_scalar::new(b[0], b[1])),
    );
    check_with(
        "vec4_eq",
        |r| {
            let a = floats::<16>(r, Dom::Any);
            let mut b = floats::<16>(r, Dom::Any);
            // Make most lanes equal so the all-components-equal case is exercised.
            for i in 0..16 {
                if r.below(4) != 0 {
                    b[i] = a[i];
                }
            }
            (a, b)
        },
        lanes_fn!(o::o_vec4_eq, [16, 16] -> [i32; 4]),
        |(a, b)| v4(&a).cmp_eq(&v4(&b)).0,
    );
    check_with(
        "roundfast_vec4",
        |r| (floats::<16>(r, Dom::Unit), floats::<4>(r, Dom::Byte)),
        lanes_fn!(o::o_roundfast_vec4, [16, 4] -> [i32; 16]),
        |(a, b)| {
            let v = roundfast(v4(&a), fl(b));
            let mut o = [0i32; 16];
            o[..4].copy_from_slice(&v.x.0);
            o[4..8].copy_from_slice(&v.y.0);
            o[8..12].copy_from_slice(&v.z.0);
            o[12..].copy_from_slice(&v.w.0);
            o
        },
    );
    report();
}

// ---- matrices ---------------------------------------------------------------------------------------------------

fn m4s(m: &[f32]) -> mat4_scalar {
    mat4_scalar::load_from_ptr(m)
}

#[test]
fn matrices() {
    check_with(
        "mat4s_mul_vec4s",
        |r| (floats::<16>(r, Dom::Any), floats::<4>(r, Dom::Any)),
        lanes_fn!(o::o_mat4s_mul_vec4s, [16, 4] -> [f32; 4]),
        |(m, v)| {
            let r = m4s(&m) * vec4_scalar::load_from_ptr(&v);
            [r.x, r.y, r.z, r.w]
        },
    );
    check_with(
        "mat4s_mul_vec4",
        |r| (floats::<16>(r, Dom::Any), floats::<16>(r, Dom::Any)),
        lanes_fn!(o::o_mat4s_mul_vec4, [16, 16] -> [f32; 16]),
        |(m, v)| out4(m4s(&m) * v4(&v)),
    );
    check_with(
        "mat4_mul_vec4",
        |r| (floats::<64>(r, Dom::Any), floats::<16>(r, Dom::Any)),
        lanes_fn!(o::o_mat4_mul_vec4, [64, 16] -> [f32; 16]),
        |(m, v)| out4(mat4::new(v4(&m[0..16]), v4(&m[16..32]), v4(&m[32..48]), v4(&m[48..64])) * v4(&v)),
    );
    check_with(
        "mat3_mul_vec3",
        |r| (floats::<36>(r, Dom::Any), floats::<12>(r, Dom::Any)),
        lanes_fn!(o::o_mat3_mul_vec3, [36, 12] -> [f32; 12]),
        |(m, v)| out3(mat3::new(v3(&m[0..12]), v3(&m[12..24]), v3(&m[24..36])) * v3(&v)),
    );
    check_with(
        "mat3s_mul_mat3s",
        |r| (floats::<9>(r, Dom::Any), floats::<9>(r, Dom::Any)),
        lanes_fn!(o::o_mat3s_mul_mat3s, [9, 9] -> [f32; 9]),
        |(a, b)| {
            let mk = |m: [f32; 9]| make_mat3((m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8]));
            let r = mk(a) * mk(b);
            let mut o = [0f32; 9];
            for c in 0..3 {
                o[c * 3] = r[c].x;
                o[c * 3 + 1] = r[c].y;
                o[c * 3 + 2] = r[c].z;
            }
            o
        },
    );
    check_with(
        "mat3x4s_mul_vec3",
        |r| (floats::<12>(r, Dom::Any), floats::<12>(r, Dom::Any)),
        lanes_fn!(o::o_mat3x4s_mul_vec3, [12, 12] -> [f32; 16]),
        |(m, v)| {
            let mm = make_mat3x4((m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11]));
            out4(mm * v3(&v))
        },
    );
    check_with(
        "mat3x4s_mul_vec3s",
        |r| (floats::<12>(r, Dom::Any), floats::<3>(r, Dom::Any)),
        lanes_fn!(o::o_mat3x4s_mul_vec3s, [12, 3] -> [f32; 4]),
        |(m, v)| {
            let mm = make_mat3x4((m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11]));
            let r = mm * vec3_scalar::new(v[0], v[1], v[2]);
            [r.x, r.y, r.z, r.w]
        },
    );
    check_with(
        "mat4x3s_mul_vec4",
        |r| (floats::<12>(r, Dom::Any), floats::<16>(r, Dom::Any)),
        lanes_fn!(o::o_mat4x3s_mul_vec4, [12, 16] -> [f32; 12]),
        |(m, v)| {
            let mm = transpose(make_mat3x4((m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11])));
            out3(mm * v4(&v))
        },
    );
    check_with(
        "mat4x3s_mul_vec4s",
        |r| (floats::<12>(r, Dom::Any), floats::<4>(r, Dom::Any)),
        lanes_fn!(o::o_mat4x3s_mul_vec4s, [12, 4] -> [f32; 3]),
        |(m, v)| {
            let mm = transpose(make_mat3x4((m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11])));
            let r = mm * vec4_scalar::load_from_ptr(&v);
            [r.x, r.y, r.z]
        },
    );
    check_with(
        "mat3s_mul_vec3s",
        |r| (floats::<9>(r, Dom::Any), floats::<3>(r, Dom::Any)),
        lanes_fn!(o::o_mat3s_mul_vec3s, [9, 3] -> [f32; 3]),
        |(m, v)| {
            let mm = make_mat3((m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8]));
            let r = mm * vec3_scalar::new(v[0], v[1], v[2]);
            [r.x, r.y, r.z]
        },
    );
    check_with(
        "mat3s_mul_vec3",
        |r| (floats::<9>(r, Dom::Any), floats::<12>(r, Dom::Any)),
        lanes_fn!(o::o_mat3s_mul_vec3, [9, 12] -> [f32; 12]),
        |(m, v)| out3(make_mat3((m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8])) * v3(&v)),
    );
    check_with(
        "mat2_mul_vec2",
        |r| (floats::<16>(r, Dom::Any), floats::<8>(r, Dom::Any)),
        lanes_fn!(o::o_mat2_mul_vec2, [16, 8] -> [f32; 8]),
        |(m, v)| out2(mat2::new(v2(&m[0..8]), v2(&m[8..16])) * v2(&v)),
    );
    check_with(
        "inverse_mat2s",
        |r| floats::<4>(r, Dom::Any),
        lanes_fn!(o::o_inverse_mat2s, [4] -> [f32; 4]),
        |m| {
            let r = inverse(mat2_scalar::new(vec2_scalar::new(m[0], m[1]), vec2_scalar::new(m[2], m[3])));
            [r[0].x, r[0].y, r[1].x, r[1].y]
        },
    );
    check_with(
        "inverse_mat2",
        |r| floats::<16>(r, Dom::Any),
        lanes_fn!(o::o_inverse_mat2, [16] -> [f32; 16]),
        |m| {
            let r = inverse(mat2::new(v2(&m[0..8]), v2(&m[8..16])));
            let mut o = [0f32; 16];
            o[..8].copy_from_slice(&out2(r[0]));
            o[8..].copy_from_slice(&out2(r[1]));
            o
        },
    );
    report();
}

// ---- vector_type.h: zips, halves, packs and CONVERT between lane types --------------------------------------------

fn u16s<const K: usize>(r: &mut Rng) -> [u16; K] {
    core::array::from_fn(|_| match r.below(4) {
        0 => [0u16, 1, 0x7f, 0x80, 0xff, 0x100, 0x7fff, 0x8000, 0xffff][r.below(9) as usize],
        1 => r.below(256) as u16,
        _ => r.next() as u16,
    })
}
fn u8s(r: &mut Rng) -> [u8; 16] {
    core::array::from_fn(|_| r.next() as u8)
}

macro_rules! raw2 {
    ($cpp:path, $ti:ty, $to:ty, $n:literal) => {
        |(a, b): ([$ti; _], [$ti; _])| -> [$to; $n] {
            let mut o = [<$to>::default(); $n];
            // SAFETY: the wrapper reads one vector from each input and writes $n lanes.
            unsafe { $cpp(a.as_ptr(), b.as_ptr(), o.as_mut_ptr()) };
            o
        }
    };
}
macro_rules! raw1 {
    ($cpp:path, $ti:ty, $to:ty, $n:literal) => {
        |a: [$ti; _]| -> [$to; $n] {
            let mut o = [<$to>::default(); $n];
            // SAFETY: the wrapper reads one vector and writes $n lanes.
            unsafe { $cpp(a.as_ptr(), o.as_mut_ptr()) };
            o
        }
    };
}

#[test]
fn zips_and_halves() {
    let p16 = |r: &mut Rng| (u16s::<8>(r), u16s::<8>(r));
    let p8 = |r: &mut Rng| (u8s(r), u8s(r));
    let w8 = V8::<u16>::new;
    check_with("zip_u16", p16, raw2!(o::o_zip_u16, u16, u16, 16), |(a, b)| zip(w8(a), w8(b)).0);
    check_with("ziplow_u16", p16, raw2!(o::o_ziplow_u16, u16, u16, 8), |(a, b)| zip_low(w8(a), w8(b)).0);
    check_with("ziphigh_u16", p16, raw2!(o::o_ziphigh_u16, u16, u16, 8), |(a, b)| zip_high(w8(a), w8(b)).0);
    check_with("zip2low_u16", p16, raw2!(o::o_zip2low_u16, u16, u16, 8), |(a, b)| zip2_low(w8(a), w8(b)).0);
    check_with("zip2high_u16", p16, raw2!(o::o_zip2high_u16, u16, u16, 8), |(a, b)| zip2_high(w8(a), w8(b)).0);
    check_with("combine_halves_u16", p16, raw2!(o::o_combine_halves_u16, u16, u16, 8), |(a, b)| {
        combine(high_half(w8(a)), low_half(w8(b))).0
    });
    check_with("combinelow_u16", p16, raw2!(o::o_combinelow_u16, u16, u16, 8), |(a, b)| combine_low(w8(a), w8(b)).0);
    check_with("ziplow_u8", p8, raw2!(o::o_ziplow_u8, u8, u8, 16), |(a, b)| zip_low(V16::new(a), V16::new(b)).0);
    check_with("ziphigh_u8", p8, raw2!(o::o_ziphigh_u8, u8, u8, 16), |(a, b)| zip_high(V16::new(a), V16::new(b)).0);
    check_with(
        "shuffle_f",
        |r| (floats::<4>(r, Dom::Any), floats::<4>(r, Dom::Any)),
        raw2!(o::o_shuffle_f, f32, f32, 8),
        |(a, b)| shuffle(fl(a), fl(b), [3, 4, 1, 6, 0, 7, 2, 5]).0,
    );
    check_with("swizzle_zwxy", |r| floats::<4>(r, Dom::Any), raw1!(o::o_swizzle_zwxy, f32, f32, 4), |a| fl(a).zwxy().0);
    report();
}

#[test]
fn packs_and_lane_conversions() {
    check_with("pack_u16_u8", u16s::<8>, raw1!(o::o_pack_u16_u8, u16, u8, 8), |a| V8::new(a).convert::<u8>().0);
    check_with("unpack_u8_u16", u8s, raw1!(o::o_unpack_u8_u16, u8, u16, 16), |a| V16::new(a).convert::<u16>().0);
    check_with("cvt_i32_u16", ints, raw1!(o::o_cvt_i32_u16, i32, u16, 4), |a| I32::new(a).convert::<u16>().0);
    check_with("cvt_u16_i32", u16s::<4>, raw1!(o::o_cvt_u16_i32, u16, i32, 4), |a| U16::new(a).convert::<i32>().0);
    check_with(
        "cvt_i32_i16x8",
        |r| (ints(r), ints(r)),
        raw2!(o::o_cvt_i32_i16x8, i32, i16, 8),
        |(a, b)| combine(I32::new(a), I32::new(b)).convert::<i16>().0,
    );
    // Float -> u8 is only defined in range (clang may saturate or wrap otherwise): [0, 255].
    check_with(
        "cvt_f32_u8",
        |r| floats::<4>(r, Dom::Byte),
        raw1!(o::o_cvt_f32_u8, f32, u8, 4),
        |a| fl(a).convert::<u8>().0,
    );
    check_with(
        "u16_mul_shr",
        |r| (u16s::<8>(r), u16s::<8>(r)),
        raw2!(o::o_u16_mul_shr, u16, u16, 8),
        |(a, b)| ((V8::<u16>::new(a) * V8::new(b) + 0x80u16) >> 8).0,
    );
    check_with(
        "u16_lt",
        |r| (u16s::<8>(r), u16s::<8>(r)),
        raw2!(o::o_u16_lt, u16, i16, 8),
        |(a, b)| V8::<u16>::new(a).cmp_lt(V8::new(b)).0,
    );
    check_with(
        "u8_add",
        |r| (u8s(r), u8s(r)),
        raw2!(o::o_u8_add, u8, u8, 16),
        |(a, b)| (V16::<u8>::new(a) + V16::new(b)).0,
    );
    check_with("u8_bitcast_u32", u8s, raw1!(o::o_u8_bitcast_u32, u8, u32, 4), |a| {
        bit_cast::<V4<u32>, V16<u8>>(V16::new(a)).0
    });
    report();
}

/// What the generated shaders reach through C++'s implicit conversions and that the runtime spells out:
/// `step(float, vec4_scalar)`, `step(Float, vec4)`, `vec4[int]`, `if_then_else` on `Array<vec2, 4>`.
#[test]
fn implicit_conversions_of_generated_shaders() {
    check_with(
        "step_f_vec4_scalar",
        |r| (gen(r, Dom::Any), floats::<4>(r, Dom::Any)),
        |(e, x)| {
            let mut o = [0f32; 4];
            // SAFETY: 4 floats in, 4 out.
            unsafe { o::o_step_f_vec4_scalar(e, x.as_ptr(), o.as_mut_ptr()) };
            o
        },
        |(e, x)| {
            let r = step(e, vec4_scalar::new(x[0], x[1], x[2], x[3]));
            [r.x, r.y, r.z, r.w]
        },
    );
    check_with(
        "step_F_vec4",
        |r| (floats::<4>(r, Dom::Any), floats::<16>(r, Dom::Any)),
        lanes_fn!(o::o_step_F_vec4, [4, 16] -> [f32; 16]),
        |(e, x)| out4(step(fl(e), v4(&x))),
    );
    check_with(
        "vec4_index",
        |r| (floats::<16>(r, Dom::Any), r.below(4) as i32),
        |(a, i)| {
            let mut o = [0f32; 4];
            // SAFETY: 16 floats in, 4 out.
            unsafe { o::o_vec4_index(a.as_ptr(), i, o.as_mut_ptr()) };
            o
        },
        |(a, i)| v4(&a)[i as usize].0,
    );
    check_with(
        "ite_array_vec2_4",
        |r| {
            let c: [i32; 4] = core::array::from_fn(|_| if r.below(2) == 0 { 0 } else { -1 });
            (c, floats::<32>(r, Dom::Any), floats::<32>(r, Dom::Any))
        },
        |(c, t, e)| {
            let mut o = [0f32; 32];
            // SAFETY: 4 ints and 2 x 32 floats in, 32 out.
            unsafe { o::o_ite_array_vec2_4(c.as_ptr(), t.as_ptr(), e.as_ptr(), o.as_mut_ptr()) };
            o
        },
        |(c, t, e)| {
            let at: [vec2; 4] = core::array::from_fn(|i| v2(&t[8 * i..8 * i + 8]));
            let ae: [vec2; 4] = core::array::from_fn(|i| v2(&e[8 * i..8 * i + 8]));
            let r = if_then_else(I32::new(c), at, ae);
            let mut o = [0f32; 32];
            for i in 0..4 {
                o[8 * i..8 * i + 8].copy_from_slice(&out2(r[i]));
            }
            o
        },
    );
    report();
}
