/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Port of swgl's `glsl.h`: the GLSL types and built-ins generated shaders are written against.
//!
//! Each GLSL type comes in a lane form (`vec4`: four `Float`s, one value per pixel of a 4-pixel chunk) and a
//! `_scalar` form (`vec4_scalar`: four `f32`s, uniform across the chunk). C++ overloads become traits with one
//! free function each (`min(a, b)` calls `Min::min`); C++ implicit conversions become `From` impls and the
//! `make_*` functions.
//!
//! The approximations (`recip`, `inversesqrt`, `approx_log2`, `approx_pow2` and `exp`/`exp2`/`log`/`log2`/`pow`
//! on lanes) use the same instructions as the C++ under clang: SSE2 on x86_64, NEON on aarch64, the portable
//! fallback elsewhere. Everything else rounds each operation separately, in glsl.h's source order: the oracle is
//! swgl built with strict floats (no fast-math, no FMA contraction; raster/DESIGN.md, "Correctness bar").

mod mat;
mod sampler;
mod vecs;
pub mod vector;

pub use mat::*;
pub use sampler::*;
pub use vecs::*;
pub use vector::{
    bit_cast, combine, combine4, combine_high, combine_low, expand, high_half, load_bytes, low_half, repeat2, repeat4,
    shuffle, store_bytes, unaligned_load, unaligned_store, zip, zip2_high, zip2_low, zip_high, zip_low, Bool, Combine,
    Cvt, Float, Halves, IntLane, Lane, Pod, Vector, Zip, ZipFull, I16, I32, U16, U32, U64, U8, V16, V2, V4, V8,
};

#[cfg(target_arch = "aarch64")]
use vector::neon;
#[cfg(target_arch = "x86_64")]
use vector::sse;

// ---- TextureFormat, TextureFilter, XYZW ----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TextureFormat {
    RGBA32F,
    RGBA32I,
    #[default]
    RGBA8,
    R8,
    RG8,
    R16,
    RG16,
    YUY2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TextureFilter {
    #[default]
    NEAREST,
    LINEAR,
}

/// A swizzle component. `R`, `G`, `B`, `A` are the same values as `X`, `Y`, `Z`, `W`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum XYZW {
    X = 0,
    Y = 1,
    Z = 2,
    W = 3,
}

pub const X: XYZW = XYZW::X;
pub const Y: XYZW = XYZW::Y;
pub const Z: XYZW = XYZW::Z;
pub const W: XYZW = XYZW::W;
pub const R: XYZW = XYZW::X;
pub const G: XYZW = XYZW::Y;
pub const B: XYZW = XYZW::Z;
pub const A: XYZW = XYZW::W;

// ---- test_all / test_any / test_none ---------------------------------------------------------------------------

/// The sign bits of the four lanes (`_mm_movemask_ps`).
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn movemask(cond: Bool) -> i32 {
    sse::movemask_ps(cond)
}

/// The lanes narrowed to bytes and read as one `u32`: the non-SSE2 `bit_cast<uint32_t>(CONVERT(cond, U8))`.
#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
fn mask_bytes(cond: Bool) -> u32 {
    bit_cast::<u32, U8>(cond.convert::<u8>())
}

/// Whether every lane is set. SSE2 tests the sign bits (`_mm_movemask_ps(cond) == 0xF`); elsewhere the lanes
/// narrowed to bytes must all be `0xFF`.
#[inline(always)]
pub fn test_all(cond: Bool) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        movemask(cond) == 0xF
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        mask_bytes(cond) == 0xFFFF_FFFF
    }
}

#[inline(always)]
pub fn test_any(cond: Bool) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        movemask(cond) != 0
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        mask_bytes(cond) != 0
    }
}

#[inline(always)]
pub fn test_none(cond: Bool) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        movemask(cond) == 0
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        mask_bytes(cond) == 0
    }
}

/// Whether all lanes equal the first.
#[inline(always)]
pub fn test_equal(cond: Bool) -> bool {
    test_none(cond.cmp_ne(cond.x()))
}

// ---- make_float / make_int / make_uint / force_scalar -----------------------------------------------------------

pub trait MakeFloat {
    type Output;
    fn make_float(self) -> Self::Output;
}
pub trait MakeInt {
    type Output;
    fn make_int(self) -> Self::Output;
}
pub trait MakeUint {
    type Output;
    fn make_uint(self) -> Self::Output;
}

/// `int32_t(f)` / `uint32_t(f)` for a scalar float, as the target's instruction computes it (x86_64:
/// `cvttss2si`, which for `uint32_t` is the 64-bit form truncated to 32 bits).
#[inline(always)]
pub fn f32_to_i32_scalar(f: f32) -> i32 {
    vector::f32_to_i32(f)
}

#[inline(always)]
pub fn f32_to_u32_scalar(f: f32) -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        // cvttss2si with a 64-bit destination: "integer indefinite" outside [-2^63, 2^63) and for NaN.
        let i = if (i64::MIN as f32..-(i64::MIN as f32)).contains(&f) { f as i64 } else { i64::MIN };
        i as u32
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        f as u32
    }
}

macro_rules! make_scalar {
    ($tr:ident, $f:ident, $out:ty: $($t:ty => |$v:ident| $e:expr),*) => {$(
        impl $tr for $t {
            type Output = $out;
            #[inline(always)]
            fn $f(self) -> $out { let $v = self; $e }
        }
    )*};
}
make_scalar!(MakeFloat, make_float, f32: f32 => |n| n, i32 => |n| n as f32, u32 => |n| n as f32, bool => |n| n as i32 as f32);
make_scalar!(MakeInt, make_int, i32: u32 => |n| n as i32, i32 => |n| n, f32 => |n| f32_to_i32_scalar(n), bool => |n| n as i32);
make_scalar!(MakeUint, make_uint, u32: u32 => |n| n, i32 => |n| n as u32, f32 => |n| f32_to_u32_scalar(n), bool => |n| n as u32);

macro_rules! make_lanes {
    ($tr:ident, $f:ident, $lane:ty: $($t:ty),*) => {$(
        impl $tr for Vector<$t, 4> {
            type Output = Vector<$lane, 4>;
            #[inline(always)]
            fn $f(self) -> Vector<$lane, 4> { self.convert::<$lane>() }
        }
    )*};
}
make_lanes!(MakeFloat, make_float, f32: f32, i32, u32);
make_lanes!(MakeInt, make_int, i32: f32, i32, u32);
make_lanes!(MakeUint, make_uint, u32: f32, i32, u32);

#[inline(always)]
pub fn make_float<T: MakeFloat>(v: T) -> T::Output {
    v.make_float()
}
#[inline(always)]
pub fn make_int<T: MakeInt>(v: T) -> T::Output {
    v.make_int()
}
#[inline(always)]
pub fn make_uint<T: MakeUint>(v: T) -> T::Output {
    v.make_uint()
}

/// `force_scalar`: the first lane of a lane value, the value itself for scalars.
pub trait ForceScalar {
    type Output;
    fn force_scalar(self) -> Self::Output;
}

macro_rules! force_scalar_id {
    ($($t:ty),*) => {$(
        impl ForceScalar for $t {
            type Output = $t;
            #[inline(always)]
            fn force_scalar(self) -> $t { self }
        }
    )*};
}
force_scalar_id!(f32, i32, u32, bool);

impl ForceScalar for Float {
    type Output = f32;
    #[inline(always)]
    fn force_scalar(self) -> f32 {
        self[0]
    }
}

impl ForceScalar for I32 {
    type Output = i32;
    #[inline(always)]
    fn force_scalar(self) -> i32 {
        self[0]
    }
}

#[inline(always)]
pub fn force_scalar<T: ForceScalar>(v: T) -> T::Output {
    v.force_scalar()
}

// ---- if_then_else / if_then -----------------------------------------------------------------------------------

/// `if_then_else(c, t, e)`, dispatched on the condition: a lane mask selects bitwise per lane, a scalar
/// (`int32_t` / `bool`) picks one side whole.
pub trait IfThenElse<T> {
    type Output;
    fn if_then_else(self, t: T, e: T) -> Self::Output;
}

#[inline(always)]
pub fn if_then_else<C: IfThenElse<T>, T>(c: C, t: T, e: T) -> C::Output {
    c.if_then_else(t, e)
}

impl<T> IfThenElse<T> for bool {
    type Output = T;
    #[inline(always)]
    fn if_then_else(self, t: T, e: T) -> T {
        if self {
            t
        } else {
            e
        }
    }
}

impl<T> IfThenElse<T> for i32 {
    type Output = T;
    /// `c ? t : e` for every C++ overload taking an `int32_t` condition.
    #[inline(always)]
    fn if_then_else(self, t: T, e: T) -> T {
        if self != 0 {
            t
        } else {
            e
        }
    }
}

impl IfThenElse<f32> for I32 {
    type Output = Float;
    #[inline(always)]
    fn if_then_else(self, t: f32, e: f32) -> Float {
        bit_cast::<Float, I32>(
            (self & bit_cast::<I32, Float>(Float::splat(t))) | (!self & bit_cast::<I32, Float>(Float::splat(e))),
        )
    }
}

/// glsl.h's `if_then_else(I32 c, Array<vec2, SIZE> t, Array<vec2, SIZE> e)`, elementwise (a struct with an array
/// field selects it this way).
impl<const N: usize> IfThenElse<[vec2; N]> for I32 {
    type Output = [vec2; N];
    #[inline(always)]
    fn if_then_else(self, t: [vec2; N], e: [vec2; N]) -> [vec2; N] {
        core::array::from_fn(|i| if_then_else(self, t[i], e[i]))
    }
}

impl IfThenElse<i32> for I32 {
    type Output = I32;
    #[inline(always)]
    fn if_then_else(self, t: i32, e: i32) -> I32 {
        (self & I32::splat(t)) | (!self & I32::splat(e))
    }
}

impl IfThenElse<U32> for I32 {
    type Output = U32;
    #[inline(always)]
    fn if_then_else(self, t: U32, e: U32) -> U32 {
        bit_cast::<U32, I32>((self & bit_cast::<I32, U32>(t)) | (!self & bit_cast::<I32, U32>(e)))
    }
}

impl IfThenElse<Float> for I32 {
    type Output = Float;
    #[inline(always)]
    fn if_then_else(self, t: Float, e: Float) -> Float {
        bit_cast::<Float, I32>((self & bit_cast::<I32, Float>(t)) | (!self & bit_cast::<I32, Float>(e)))
    }
}

impl IfThenElse<I32> for I32 {
    type Output = I32;
    /// The C++ `if_then_else(I32, Bool, Bool)` (`Bool` and `I32` are one type).
    #[inline(always)]
    fn if_then_else(self, t: I32, e: I32) -> I32 {
        (self & t) | (!self & e)
    }
}

impl IfThenElse<I16> for I16 {
    type Output = I16;
    #[inline(always)]
    fn if_then_else(self, t: I16, e: I16) -> I16 {
        (self & t) | (!self & e)
    }
}

/// Cheaper `if_then_else` that gives `Float(0)` where the condition is false.
#[inline(always)]
pub fn if_then(c: I32, t: Float) -> Float {
    bit_cast::<Float, I32>(c & bit_cast::<I32, Float>(t))
}

pub fn swap<T>(a: &mut T, b: &mut T) {
    core::mem::swap(a, b)
}

// ---- min / max / clamp -----------------------------------------------------------------------------------------

pub trait Min<Rhs = Self> {
    type Output;
    fn min(self, b: Rhs) -> Self::Output;
}
pub trait Max<Rhs = Self> {
    type Output;
    fn max(self, b: Rhs) -> Self::Output;
}
pub trait Clamp<Lo = Self, Hi = Lo> {
    type Output;
    fn clamp(self, lo: Lo, hi: Hi) -> Self::Output;
}

#[inline(always)]
pub fn min<A: Min<B>, B>(a: A, b: B) -> A::Output {
    a.min(b)
}
#[inline(always)]
pub fn max<A: Max<B>, B>(a: A, b: B) -> A::Output {
    a.max(b)
}
#[inline(always)]
pub fn clamp<A: Clamp<L, H>, L, H>(a: A, lo: L, hi: H) -> A::Output {
    a.clamp(lo, hi)
}

impl Min for i32 {
    type Output = i32;
    #[inline(always)]
    fn min(self, b: i32) -> i32 {
        if self < b {
            self
        } else {
            b
        }
    }
}
impl Max for i32 {
    type Output = i32;
    #[inline(always)]
    fn max(self, b: i32) -> i32 {
        if self > b {
            self
        } else {
            b
        }
    }
}
impl Clamp for i32 {
    type Output = i32;
    #[inline(always)]
    fn clamp(self, lo: i32, hi: i32) -> i32 {
        Min::min(Max::max(self, lo), hi)
    }
}
impl Min for f32 {
    type Output = f32;
    #[inline(always)]
    fn min(self, b: f32) -> f32 {
        if self < b {
            self
        } else {
            b
        }
    }
}
impl Max for f32 {
    type Output = f32;
    #[inline(always)]
    fn max(self, b: f32) -> f32 {
        if self > b {
            self
        } else {
            b
        }
    }
}
impl Clamp for f32 {
    type Output = f32;
    #[inline(always)]
    fn clamp(self, lo: f32, hi: f32) -> f32 {
        Min::min(Max::max(self, lo), hi)
    }
}

impl Min for Float {
    type Output = Float;
    /// `_mm_min_ps` (`a < b ? a : b`, so `b` for NaN) / `vminq_f32` (NaN-propagating).
    #[inline(always)]
    fn min(self, b: Float) -> Float {
        #[cfg(target_arch = "x86_64")]
        {
            sse::min_ps(self, b)
        }
        #[cfg(target_arch = "aarch64")]
        {
            neon::vminq_f32(self, b)
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            if_then_else(self.cmp_lt(b), self, b)
        }
    }
}
impl Max for Float {
    type Output = Float;
    /// `_mm_max_ps` / `vmaxq_f32`.
    #[inline(always)]
    fn max(self, b: Float) -> Float {
        #[cfg(target_arch = "x86_64")]
        {
            sse::max_ps(self, b)
        }
        #[cfg(target_arch = "aarch64")]
        {
            neon::vmaxq_f32(self, b)
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            if_then_else(self.cmp_gt(b), self, b)
        }
    }
}
impl Clamp for Float {
    type Output = Float;
    #[inline(always)]
    fn clamp(self, lo: Float, hi: Float) -> Float {
        Min::min(Max::max(self, lo), hi)
    }
}
impl Clamp for I32 {
    type Output = I32;
    #[inline(always)]
    fn clamp(self, lo: I32, hi: I32) -> I32 {
        let a = if_then_else(self.cmp_lt(lo), lo, self);
        if_then_else(a.cmp_gt(hi), hi, a)
    }
}

// ---- sqrt / recip / inversesqrt -------------------------------------------------------------------------------

pub trait Sqrt {
    fn sqrt(self) -> Self;
}
pub trait Recip {
    fn recip(self) -> Self;
}
pub trait InverseSqrt {
    fn inversesqrt(self) -> Self;
}

#[inline(always)]
pub fn sqrt<T: Sqrt>(x: T) -> T {
    x.sqrt()
}
/// A fast reciprocal approximation where the platform has one (`rcpps`, `vrecpe` + one Newton step): visually
/// right but numerically imprecise, like the C++.
#[inline(always)]
pub fn recip<T: Recip>(x: T) -> T {
    x.recip()
}
#[inline(always)]
pub fn inversesqrt<T: InverseSqrt>(x: T) -> T {
    x.inversesqrt()
}

impl Sqrt for f32 {
    #[inline(always)]
    fn sqrt(self) -> f32 {
        f32::sqrt(self)
    }
}

impl Sqrt for Float {
    #[inline(always)]
    fn sqrt(self) -> Float {
        #[cfg(target_arch = "x86_64")]
        {
            sse::sqrt_ps(self)
        }
        #[cfg(target_arch = "aarch64")]
        {
            let mut e = neon::vrsqrteq_f32(self);
            e *= neon::vrsqrtsq_f32(self, e * e);
            e *= neon::vrsqrtsq_f32(self, e * e);
            if_then(self.cmp_ne(Float::splat(0.0)), self * e)
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Float::new([self[0].sqrt(), self[1].sqrt(), self[2].sqrt(), self[3].sqrt()])
        }
    }
}

impl Recip for f32 {
    #[inline(always)]
    fn recip(self) -> f32 {
        #[cfg(target_arch = "x86_64")]
        {
            sse::rcp_ss(self)
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            1.0 / self
        }
    }
}

impl Recip for Float {
    #[inline(always)]
    fn recip(self) -> Float {
        #[cfg(target_arch = "x86_64")]
        {
            sse::rcp_ps(self)
        }
        #[cfg(target_arch = "aarch64")]
        {
            let e = neon::vrecpeq_f32(self);
            neon::vrecpsq_f32(self, e) * e
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            1.0 / self
        }
    }
}

impl InverseSqrt for f32 {
    #[inline(always)]
    fn inversesqrt(self) -> f32 {
        #[cfg(target_arch = "x86_64")]
        {
            sse::rsqrt_ss(self)
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            1.0 / self.sqrt()
        }
    }
}

impl InverseSqrt for Float {
    #[inline(always)]
    fn inversesqrt(self) -> Float {
        #[cfg(target_arch = "x86_64")]
        {
            sse::rsqrt_ps(self)
        }
        #[cfg(target_arch = "aarch64")]
        {
            let e = neon::vrsqrteq_f32(self);
            neon::vrsqrtsq_f32(self, e * e) * e
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            1.0 / Sqrt::sqrt(self)
        }
    }
}

// ---- step / abs / sign / cast ---------------------------------------------------------------------------------

pub trait Step<X = Self> {
    type Output;
    fn step(self, x: X) -> Self::Output;
}

/// `step(edge, x)`: 1 where `x >= edge`, else 0.
#[inline(always)]
pub fn step<E: Step<T>, T>(edge: E, x: T) -> E::Output {
    edge.step(x)
}

impl Step for f32 {
    type Output = f32;
    #[inline(always)]
    fn step(self, x: f32) -> f32 {
        (x >= self) as i32 as f32
    }
}

impl Step for Float {
    type Output = Float;
    #[inline(always)]
    fn step(self, x: Float) -> Float {
        if_then(x.cmp_ge(self), Float::splat(1.0))
    }
}

pub trait Abs {
    fn abs(self) -> Self;
}
pub trait Sign {
    fn sign(self) -> Self;
}

#[inline(always)]
pub fn abs<T: Abs>(x: T) -> T {
    x.abs()
}
#[inline(always)]
pub fn sign<T: Sign>(x: T) -> T {
    x.sign()
}

impl Abs for i32 {
    #[inline(always)]
    fn abs(self) -> i32 {
        if self < 0 {
            self.wrapping_neg()
        } else {
            self
        }
    }
}

impl Abs for f32 {
    #[inline(always)]
    fn abs(self) -> f32 {
        f32::abs(self)
    }
}

impl Abs for Float {
    /// `vabsq_f32` on NEON; elsewhere `v & (0 - v)` on the bits, as the C++.
    #[inline(always)]
    fn abs(self) -> Float {
        #[cfg(target_arch = "aarch64")]
        {
            neon::vabsq_f32(self)
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            bit_cast::<Float, I32>(bit_cast::<I32, Float>(self) & bit_cast::<I32, Float>(0.0f32 - self))
        }
    }
}

impl Sign for f32 {
    #[inline(always)]
    fn sign(self) -> f32 {
        1.0f32.copysign(self)
    }
}

impl Sign for Float {
    /// ±1 with the sign bit of `v` (so ±0 and NaN give ±1 too).
    #[inline(always)]
    fn sign(self) -> Float {
        bit_cast::<Float, I32>(
            (bit_cast::<I32, Float>(self) & (0x8000_0000u32 as i32)) | bit_cast::<I32, Float>(Float::splat(1.0)),
        )
    }
}

/// `cast`: lanes between `Float` and `I32` (`U32` is first reinterpreted as `I32`).
pub trait Cast {
    type Output;
    fn cast(self) -> Self::Output;
}

impl Cast for U32 {
    type Output = Float;
    #[inline(always)]
    fn cast(self) -> Float {
        bit_cast::<I32, U32>(self).convert::<f32>()
    }
}
impl Cast for I32 {
    type Output = Float;
    #[inline(always)]
    fn cast(self) -> Float {
        self.convert::<f32>()
    }
}
impl Cast for Float {
    type Output = I32;
    #[inline(always)]
    fn cast(self) -> I32 {
        self.convert::<i32>()
    }
}

#[inline(always)]
pub fn cast<T: Cast>(v: T) -> T::Output {
    v.cast()
}

// ---- floor / ceil / round / fract ---------------------------------------------------------------------------

pub trait Floor {
    fn floor(self) -> Self;
}
pub trait Ceil {
    fn ceil(self) -> Self;
}
pub trait Round {
    fn round(self) -> Self;
}
pub trait Fract {
    fn fract(self) -> Self;
}

#[inline(always)]
pub fn floor<T: Floor>(x: T) -> T {
    x.floor()
}
#[inline(always)]
pub fn ceil<T: Ceil>(x: T) -> T {
    x.ceil()
}
#[inline(always)]
pub fn round<T: Round>(x: T) -> T {
    x.round()
}
#[inline(always)]
pub fn fract<T: Fract>(x: T) -> T {
    x.fract()
}

impl Floor for f32 {
    #[inline(always)]
    fn floor(self) -> f32 {
        f32::floor(self)
    }
}
impl Floor for Float {
    #[inline(always)]
    fn floor(self) -> Float {
        let roundtrip = cast(cast(self));
        roundtrip - if_then(roundtrip.cmp_gt(self), Float::splat(1.0))
    }
}
impl Ceil for f32 {
    #[inline(always)]
    fn ceil(self) -> f32 {
        f32::ceil(self)
    }
}
impl Ceil for Float {
    #[inline(always)]
    fn ceil(self) -> Float {
        let roundtrip = cast(cast(self));
        roundtrip + if_then(roundtrip.cmp_lt(self), Float::splat(1.0))
    }
}
impl Round for f32 {
    /// `roundf`: halfway cases away from zero.
    #[inline(always)]
    fn round(self) -> f32 {
        f32::round(self)
    }
}
impl Round for Float {
    /// `floor(v + 0.5f)`.
    #[inline(always)]
    fn round(self) -> Float {
        Floor::floor(self + 0.5f32)
    }
}
impl Fract for f32 {
    #[inline(always)]
    fn fract(self) -> f32 {
        self - f32::floor(self)
    }
}
impl Fract for Float {
    #[inline(always)]
    fn fract(self) -> Float {
        self - Floor::floor(self)
    }
}

// ---- roundeven / roundzero / roundfast ------------------------------------------------------------------------

/// Round `v * scale` to nearest even: `cvtss2si` with SSE2, else the magic-number trick.
#[inline(always)]
pub fn roundeven_scalar(v: f32, scale: f32) -> i32 {
    #[cfg(target_arch = "x86_64")]
    {
        sse::cvtss_si32(v * scale)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        bit_cast::<i32, f32>(v * scale + 0xC00000 as f32).wrapping_sub(0x4B40_0000)
    }
}

/// Round `v * scale` to nearest even per lane: `cvtps2dq` with SSE2, else the magic-number trick
/// (<http://stereopsis.com/sree/fpu2006.html>).
#[inline(always)]
pub fn roundeven(v: Float, scale: Float) -> I32 {
    #[cfg(target_arch = "x86_64")]
    {
        sse::cvtps_epi32(v * scale)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        bit_cast::<I32, Float>(v * scale + Float::splat(0xC00000 as f32)) - 0x4B40_0000
    }
}

/// Round `v * scale` towards zero.
#[inline(always)]
pub fn roundzero_scalar(v: f32, scale: f32) -> i32 {
    f32_to_i32_scalar(v * scale)
}

#[inline(always)]
pub fn roundzero(v: Float, scale: Float) -> I32 {
    cast(v * scale)
}

/// `roundfast(v, scale)`: `Float -> I32`, and `vec4 -> ivec4` per component.
pub trait RoundFast {
    type Output;
    fn roundfast(self, scale: Float) -> Self::Output;
}

impl RoundFast for Float {
    type Output = I32;
    /// Round `v * scale` whichever way is fastest for positive numbers: nearest even with SSE2 (`cvtps2dq`),
    /// `int(v * scale + 0.5)` elsewhere.
    #[inline(always)]
    fn roundfast(self, scale: Float) -> I32 {
        #[cfg(target_arch = "x86_64")]
        {
            sse::cvtps_epi32(self * scale)
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            cast(self * scale + 0.5f32)
        }
    }
}

#[inline(always)]
pub fn roundfast<T: RoundFast>(v: T, scale: Float) -> T::Output {
    v.roundfast(scale)
}

/// `round_pixel(v, scale = 255.0f)`: pass `255.0` for the C++ default.
#[inline(always)]
pub fn round_pixel<T: RoundFast>(v: T, scale: f32) -> T::Output {
    roundfast(v, Float::splat(scale))
}

/// `ElementType<V>::ty`: the scalar type of a GLSL value. As in the C++, `int`'s element type is `float`.
pub trait ElementType {
    type ty;
}

macro_rules! element_type {
    ($($t:ty => $e:ty),*) => {$(impl ElementType for $t { type ty = $e; })*};
}
element_type!(f32 => f32, i32 => f32, Float => f32, I32 => i32);

// ---- derivatives ----------------------------------------------------------------------------------------------

/// X derivatives are `x[1] - x[0]`; Y derivatives are not available on X spans, so `fwidth` assumes uniform
/// scaling: `abs(dFdx(p.x)) + abs(dFdx(p.y))` for both components.
#[inline(always)]
pub fn fwidth(p: vec2) -> vec2_scalar {
    let d = Abs::abs(shuffle(p.x, p.y, [1, 1, 5, 5]) - shuffle(p.x, p.y, [0, 0, 4, 4]));
    vec2_scalar::splat(d[0] + d[2])
}

pub trait DFdx {
    type Output;
    fn dFdx(self) -> Self::Output;
}
impl DFdx for Float {
    type Output = f32;
    #[inline(always)]
    fn dFdx(self) -> f32 {
        self[1] - self[0]
    }
}
impl DFdx for vec2 {
    type Output = vec2_scalar;
    #[inline(always)]
    fn dFdx(self) -> vec2_scalar {
        vec2_scalar::new(self.x.dFdx(), self.y.dFdx())
    }
}
#[inline(always)]
pub fn dFdx<T: DFdx>(x: T) -> T::Output {
    x.dFdx()
}

// ---- approx_log2 / approx_pow2 and the transcendentals on lanes -----------------------------------------------

/// Fast `log2` (<http://www.machinedlearnings.com/2011/06/fast-approximate-logarithm-exponential.html>).
#[inline(always)]
#[allow(clippy::excessive_precision)] // the C++ constants, digit for digit
pub fn approx_log2(x: Float) -> Float {
    let bits = bit_cast::<U32, Float>(x);
    // e - 127 is a fair approximation of log2(x) in its own right...
    let e = cast(bits) * (1.0f32 / (1 << 23) as f32);
    // ... but using the mantissa to refine its error is _much_ better.
    let m = bit_cast::<Float, U32>((bits & 0x007f_ffffu32) | 0x3f00_0000u32);
    e - 124.225_514_99f32 - 1.498_030_302f32 * m - 1.725_879_99f32 / (0.352_088_706_8f32 + m)
}

/// Fast `2^x`, the inverse of [`approx_log2`].
#[inline(always)]
#[allow(clippy::excessive_precision)]
pub fn approx_pow2(x: Float) -> Float {
    let f = Fract::fract(x);
    bit_cast::<Float, I32>(roundfast(
        Float::splat((1 << 23) as f32),
        x + 121.274_057_5f32 - 1.490_129_07f32 * f + 27.728_023_3f32 / (4.842_525_68f32 - f),
    ))
}

pub trait Pow<Y = Self> {
    type Output;
    fn pow(self, y: Y) -> Self::Output;
}
#[inline(always)]
pub fn pow<T: Pow<Y>, Y>(x: T, y: Y) -> T::Output {
    x.pow(y)
}
impl Pow for f32 {
    type Output = f32;
    #[inline(always)]
    fn pow(self, y: f32) -> f32 {
        self.powf(y)
    }
}
impl Pow for Float {
    type Output = Float;
    /// `x` itself where `x` is 0 or 1, else `approx_pow2(approx_log2(x) * y)`.
    #[inline(always)]
    fn pow(self, y: Float) -> Float {
        if_then_else(
            self.cmp_eq(Float::splat(0.0)) | self.cmp_eq(Float::splat(1.0)),
            self,
            approx_pow2(approx_log2(self) * y),
        )
    }
}

macro_rules! unary_trait {
    ($tr:ident, $f:ident) => {
        pub trait $tr {
            fn $f(self) -> Self;
        }
        #[inline(always)]
        pub fn $f<T: $tr>(x: T) -> T {
            x.$f()
        }
    };
}
unary_trait!(Exp, exp);
unary_trait!(Exp2, exp2);
unary_trait!(Log, log);
unary_trait!(Log2, log2);
unary_trait!(Sin, sin);
unary_trait!(Cos, cos);
unary_trait!(Tan, tan);
unary_trait!(Atan, atan);

impl Exp for f32 {
    #[inline(always)]
    fn exp(self) -> f32 {
        f32::exp(self)
    }
}
impl Exp for Float {
    #[inline(always)]
    fn exp(self) -> Float {
        // The C++ literal 1.4426950408889634074f, which is this constant.
        let l2e = core::f32::consts::LOG2_E;
        approx_pow2(l2e * self)
    }
}
impl Exp2 for f32 {
    #[inline(always)]
    fn exp2(self) -> f32 {
        f32::exp2(self)
    }
}
impl Exp2 for Float {
    #[inline(always)]
    fn exp2(self) -> Float {
        approx_pow2(self)
    }
}
impl Log for f32 {
    #[inline(always)]
    fn log(self) -> f32 {
        f32::ln(self)
    }
}
impl Log for Float {
    #[inline(always)]
    fn log(self) -> Float {
        // The C++ literal 0.69314718f rounds to this constant.
        approx_log2(self) * core::f32::consts::LN_2
    }
}
impl Log2 for f32 {
    #[inline(always)]
    fn log2(self) -> f32 {
        f32::log2(self)
    }
}
impl Log2 for Float {
    #[inline(always)]
    fn log2(self) -> Float {
        approx_log2(self)
    }
}

macro_rules! libm_lanes {
    ($tr:ident, $f:ident, $m:ident) => {
        impl $tr for f32 {
            #[inline(always)]
            fn $f(self) -> f32 {
                f32::$m(self)
            }
        }
        impl $tr for Float {
            /// The libm function on each lane, as the C++.
            #[inline(always)]
            fn $f(self) -> Float {
                Float::new([f32::$m(self[0]), f32::$m(self[1]), f32::$m(self[2]), f32::$m(self[3])])
            }
        }
    };
}
libm_lanes!(Sin, sin, sin);
libm_lanes!(Cos, cos, cos);
libm_lanes!(Tan, tan, tan);
libm_lanes!(Atan, atan, atan);

/// The two-argument GLSL `atan(y, x)` (C++ overloads `atan`; Rust names the arity): `atan2f` per lane.
pub trait Atan2 {
    fn atan2(self, x: Self) -> Self;
}
impl Atan2 for f32 {
    #[inline(always)]
    fn atan2(self, x: f32) -> f32 {
        f32::atan2(self, x)
    }
}
impl Atan2 for Float {
    #[inline(always)]
    fn atan2(self, b: Float) -> Float {
        Float::new([self[0].atan2(b[0]), self[1].atan2(b[1]), self[2].atan2(b[2]), self[3].atan2(b[3])])
    }
}
#[inline(always)]
pub fn atan2<T: Atan2>(a: T, b: T) -> T {
    a.atan2(b)
}

/// GLSL `mod(a, b)` = `a - b * floor(a / b)` (`mod` is a Rust keyword: call it as `r#mod`).
pub trait Mod {
    fn r#mod(self, b: Self) -> Self;
}
impl Mod for Float {
    #[inline(always)]
    fn r#mod(self, b: Float) -> Float {
        self - b * Floor::floor(self / b)
    }
}
#[inline(always)]
pub fn r#mod<T: Mod>(a: T, b: T) -> T {
    a.r#mod(b)
}

// ---- length / distance / normalize / dot ----------------------------------------------------------------------

pub trait Length {
    type Output;
    fn length(self) -> Self::Output;
}
#[inline(always)]
pub fn length<T: Length>(a: T) -> T::Output {
    a.length()
}

/// `distance(a, b)` = `length(a - b)`.
#[inline(always)]
pub fn distance<A, B>(a: A, b: B) -> <A::Output as Length>::Output
where
    A: core::ops::Sub<B>,
    A::Output: Length,
{
    length(a - b)
}

/// `normalize(a)` = `a / length(a)`.
#[inline(always)]
pub fn normalize<T>(a: T) -> T
where
    T: Length + Copy + core::ops::Div<<T as Length>::Output, Output = T>,
{
    a / length(a)
}

pub trait Dot {
    type Output;
    fn dot(self, b: Self) -> Self::Output;
}
#[inline(always)]
pub fn dot<T: Dot>(a: T, b: T) -> T::Output {
    a.dot(b)
}

// ---- any / all ------------------------------------------------------------------------------------------------

pub trait Any {
    type Output;
    fn any(self) -> Self::Output;
}
pub trait All {
    type Output;
    fn all(self) -> Self::Output;
}
#[inline(always)]
pub fn any<T: Any>(x: T) -> T::Output {
    x.any()
}
#[inline(always)]
pub fn all<T: All>(x: T) -> T::Output {
    x.all()
}
impl Any for bool {
    type Output = bool;
    #[inline(always)]
    fn any(self) -> bool {
        self
    }
}
impl All for bool {
    type Output = bool;
    #[inline(always)]
    fn all(self) -> bool {
        self
    }
}

// ---- get_nth / put_nth / put_nth_component / init_interp ------------------------------------------------------

/// `get_nth(a, n)`: lane `n` of a lane value as its scalar form.
pub trait GetNth {
    type Output;
    fn get_nth(self, n: usize) -> Self::Output;
}
#[inline(always)]
pub fn get_nth<T: GetNth>(a: T, n: usize) -> T::Output {
    a.get_nth(n)
}
impl GetNth for I32 {
    type Output = i32;
    #[inline(always)]
    fn get_nth(self, n: usize) -> i32 {
        self[n]
    }
}
impl GetNth for Float {
    type Output = f32;
    #[inline(always)]
    fn get_nth(self, n: usize) -> f32 {
        self[n]
    }
}
impl GetNth for f32 {
    type Output = f32;
    #[inline(always)]
    fn get_nth(self, _n: usize) -> f32 {
        self
    }
}

/// `put_nth(dst, n, src)`: store a scalar value into lane `n`.
pub trait PutNth<S> {
    fn put_nth(&mut self, n: usize, src: S);
}
#[inline(always)]
pub fn put_nth<T: PutNth<S>, S>(dst: &mut T, n: usize, src: S) {
    dst.put_nth(n, src)
}
impl PutNth<f32> for Float {
    #[inline(always)]
    fn put_nth(&mut self, n: usize, src: f32) {
        self[n] = src;
    }
}
impl PutNth<i32> for I32 {
    #[inline(always)]
    fn put_nth(&mut self, n: usize, src: i32) {
        self[n] = src;
    }
}

/// `put_nth_component(dst, n, src)`: set component `n` of a scalar value; out-of-range `n` does nothing.
pub trait PutNthComponent<S> {
    fn put_nth_component(&mut self, n: usize, src: S);
}
#[inline(always)]
pub fn put_nth_component<T: PutNthComponent<S>, S>(dst: &mut T, n: usize, src: S) {
    dst.put_nth_component(n, src)
}
impl PutNthComponent<i32> for i32 {
    #[inline(always)]
    fn put_nth_component(&mut self, n: usize, src: i32) {
        if n == 0 {
            *self = src;
        }
    }
}
impl PutNthComponent<f32> for f32 {
    #[inline(always)]
    fn put_nth_component(&mut self, n: usize, src: f32) {
        if n == 0 {
            *self = src;
        }
    }
}

/// `init_interp(init, step)`: the four lanes `init, init + step, init + 2 step, init + 3 step`, each added to
/// the previous (not multiplied), as the C++.
pub trait InitInterp {
    type Output;
    fn init_interp(self, step: Self) -> Self::Output;
}
#[inline(always)]
pub fn init_interp<T: InitInterp>(init: T, step: T) -> T::Output {
    init.init_interp(step)
}
impl InitInterp for f32 {
    type Output = Float;
    #[inline(always)]
    fn init_interp(self, step: f32) -> Float {
        let init1 = self + step;
        let init2 = init1 + step;
        let init3 = init2 + step;
        Float::new([self, init1, init2, init3])
    }
}

// ---- Array ----------------------------------------------------------------------------------------------------

/// `Array<T, N>`: a GLSL array value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Array<T, const N: usize> {
    pub elements: [T; N],
}

impl<T: Default + Copy, const N: usize> Default for Array<T, N> {
    fn default() -> Self {
        Self { elements: [T::default(); N] }
    }
}

impl<T, const N: usize> core::ops::Index<usize> for Array<T, N> {
    type Output = T;
    #[inline(always)]
    fn index(&self, i: usize) -> &T {
        &self.elements[i]
    }
}

impl<T, const N: usize> core::ops::IndexMut<usize> for Array<T, N> {
    #[inline(always)]
    fn index_mut(&mut self, i: usize) -> &mut T {
        &mut self.elements[i]
    }
}

impl<T: Copy, const N: usize> Array<T, N> {
    /// `convert(s)`: element-wise `T(s[i])`.
    pub fn convert<S: Copy>(&mut self, s: &Array<S, N>)
    where
        T: From<S>,
    {
        for i in 0..N {
            self.elements[i] = T::from(s.elements[i]);
        }
    }
}

impl<const N: usize> IfThenElse<Array<vec2, N>> for I32 {
    type Output = Array<vec2, N>;
    #[inline(always)]
    fn if_then_else(self, t: Array<vec2, N>, e: Array<vec2, N>) -> Array<vec2, N> {
        let mut r = t;
        for i in 0..N {
            r.elements[i] = if_then_else(self, t.elements[i], e.elements[i]);
        }
        r
    }
}
