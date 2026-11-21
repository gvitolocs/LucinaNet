/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Port of swgl's `vector_type.h`: the lane vectors every shader value is made of.
//!
//! swgl builds with clang, where `VectorType<T, N>` is `T __attribute__((ext_vector_type(N)))`. That is the branch
//! ported here: lane-wise arithmetic, comparisons that return lane masks (all ones / zero, in the signed integer type
//! of the lane's width), `__builtin_convertvector` (`CONVERT`, here [`Vector::convert`]), `__builtin_shufflevector`
//! (`SHUFFLE`, here [`shuffle`]), and the helpers built on them.
//!
//! Integer arithmetic wraps (what the hardware does where C++ calls it undefined). Float to integer conversions
//! follow the instructions clang emits for `fptosi`/`fptoui` on each target, so out-of-range and NaN lanes give the
//! same bits as swgl: see [`Cvt`].

use core::fmt;
use core::ops::{
    Add, AddAssign, BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Div, DivAssign, Index, IndexMut,
    Mul, MulAssign, Neg, Not, Rem, RemAssign, Shl, ShlAssign, Shr, ShrAssign, Sub, SubAssign,
};

/// `VectorType<T, N>`: `N` lanes of `T`, 16-byte aligned so 128-bit vectors map onto one SSE2/NEON register.
#[derive(Clone, Copy)]
#[repr(C, align(16))]
pub struct Vector<T, const N: usize>(pub [T; N]);

pub type V2<T> = Vector<T, 2>;
pub type V4<T> = Vector<T, 4>;
pub type V8<T> = Vector<T, 8>;
pub type V16<T> = Vector<T, 16>;

pub type Float = V4<f32>;
pub type I32 = V4<i32>;
pub type I16 = V4<i16>;
pub type U64 = V4<u64>;
pub type U32 = V4<u32>;
pub type U16 = V4<u16>;
pub type U8 = V4<u8>;
pub type Bool = V4<i32>;

/// A lane type: the scalar arithmetic clang's vector extension applies per lane.
pub trait Lane: Copy + Default + PartialEq + PartialOrd + fmt::Debug + Send + Sync + 'static {
    /// The lane type of a comparison result: the signed integer of the same width.
    type Mask: IntLane;
    /// 16-bit integer lanes: their wrapping `+ - *` and shifts are SSE2's `paddw psubw pmullw psraw psrlw`.
    const INT16: bool = false;
    /// Signed integer lanes (arithmetic right shift).
    const SIGNED: bool = false;
    fn l_add(self, b: Self) -> Self;
    fn l_sub(self, b: Self) -> Self;
    fn l_mul(self, b: Self) -> Self;
    fn l_div(self, b: Self) -> Self;
    fn l_neg(self) -> Self;
    /// `!x` on a vector: the mask of lanes equal to zero.
    fn l_is_zero(self) -> bool;
}

/// Integer lanes: bitwise operators, shifts and `%`.
pub trait IntLane: Lane + Eq {
    const ZERO: Self;
    const ALL_ONES: Self;
    fn l_and(self, b: Self) -> Self;
    fn l_or(self, b: Self) -> Self;
    fn l_xor(self, b: Self) -> Self;
    fn l_not(self) -> Self;
    fn l_shl(self, n: i32) -> Self;
    fn l_shr(self, n: i32) -> Self;
    fn l_rem(self, b: Self) -> Self;
    #[inline(always)]
    fn from_bool(b: bool) -> Self {
        if b {
            Self::ALL_ONES
        } else {
            Self::ZERO
        }
    }
}

macro_rules! int_lane {
    ($($t:ty => $m:ty),*) => {$(
        impl Lane for $t {
            type Mask = $m;
            const INT16: bool = core::mem::size_of::<$t>() == 2;
            const SIGNED: bool = <$t>::MIN != 0;
            #[inline(always)] fn l_add(self, b: Self) -> Self { self.wrapping_add(b) }
            #[inline(always)] fn l_sub(self, b: Self) -> Self { self.wrapping_sub(b) }
            #[inline(always)] fn l_mul(self, b: Self) -> Self { self.wrapping_mul(b) }
            #[inline(always)] fn l_div(self, b: Self) -> Self { self.wrapping_div(b) }
            #[inline(always)] fn l_neg(self) -> Self { self.wrapping_neg() }
            #[inline(always)] fn l_is_zero(self) -> bool { self == 0 }
        }
        impl IntLane for $t {
            const ZERO: Self = 0;
            const ALL_ONES: Self = !0;
            #[inline(always)] fn l_and(self, b: Self) -> Self { self & b }
            #[inline(always)] fn l_or(self, b: Self) -> Self { self | b }
            #[inline(always)] fn l_xor(self, b: Self) -> Self { self ^ b }
            #[inline(always)] fn l_not(self) -> Self { !self }
            #[inline(always)] fn l_shl(self, n: i32) -> Self { self.wrapping_shl(n as u32) }
            #[inline(always)] fn l_shr(self, n: i32) -> Self { self.wrapping_shr(n as u32) }
            #[inline(always)] fn l_rem(self, b: Self) -> Self { self.wrapping_rem(b) }
        }
    )*};
}
int_lane!(i8 => i8, u8 => i8, i16 => i16, u16 => i16, i32 => i32, u32 => i32, i64 => i64, u64 => i64);

macro_rules! float_lane {
    ($($t:ty => $m:ty),*) => {$(
        impl Lane for $t {
            type Mask = $m;
            #[inline(always)] fn l_add(self, b: Self) -> Self { self + b }
            #[inline(always)] fn l_sub(self, b: Self) -> Self { self - b }
            #[inline(always)] fn l_mul(self, b: Self) -> Self { self * b }
            #[inline(always)] fn l_div(self, b: Self) -> Self { self / b }
            #[inline(always)] fn l_neg(self) -> Self { -self }
            #[inline(always)] fn l_is_zero(self) -> bool { self == 0.0 }
        }
    )*};
}
float_lane!(f32 => i32, f64 => i64);

impl<T: Lane, const N: usize> Default for Vector<T, N> {
    #[inline(always)]
    fn default() -> Self {
        Self([T::default(); N])
    }
}

impl<T: fmt::Debug, const N: usize> fmt::Debug for Vector<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.iter()).finish()
    }
}

impl<T: Lane, const N: usize> From<T> for Vector<T, N> {
    /// The C++ `VectorType(T n)` broadcast, which clang applies implicitly wherever a scalar meets a vector.
    #[inline(always)]
    fn from(n: T) -> Self {
        Self::splat(n)
    }
}

impl<T: Lane, const N: usize> From<[T; N]> for Vector<T, N> {
    #[inline(always)]
    fn from(a: [T; N]) -> Self {
        Self(a)
    }
}

impl<T: Lane, const N: usize> Vector<T, N> {
    pub const LANES: usize = N;

    #[inline(always)]
    pub fn splat(n: T) -> Self {
        Self([n; N])
    }
    #[inline(always)]
    pub fn new(a: [T; N]) -> Self {
        Self(a)
    }
    #[inline(always)]
    pub fn to_array(self) -> [T; N] {
        self.0
    }
    #[inline(always)]
    fn map(self, f: impl Fn(T) -> T) -> Self {
        let mut r = self.0;
        for x in r.iter_mut() {
            *x = f(*x);
        }
        Self(r)
    }
    #[inline(always)]
    fn zip_with(self, b: Self, f: impl Fn(T, T) -> T) -> Self {
        let mut r = self.0;
        for (x, y) in r.iter_mut().zip(b.0) {
            *x = f(*x, y);
        }
        Self(r)
    }
    #[inline(always)]
    fn compare(self, b: Self, f: impl Fn(T, T) -> bool) -> Vector<T::Mask, N> {
        let mut r = [<T::Mask as IntLane>::ZERO; N];
        for (m, (x, y)) in r.iter_mut().zip(self.0.into_iter().zip(b.0)) {
            *m = <T::Mask as IntLane>::from_bool(f(x, y));
        }
        Vector(r)
    }

    /// `a == b`: all ones where equal.
    #[inline(always)]
    pub fn cmp_eq(self, b: impl Into<Self>) -> Vector<T::Mask, N> {
        self.compare(b.into(), |x, y| x == y)
    }
    /// `a != b` (true for NaN lanes).
    #[inline(always)]
    pub fn cmp_ne(self, b: impl Into<Self>) -> Vector<T::Mask, N> {
        self.compare(b.into(), |x, y| x != y)
    }
    #[inline(always)]
    pub fn cmp_lt(self, b: impl Into<Self>) -> Vector<T::Mask, N> {
        self.compare(b.into(), |x, y| x < y)
    }
    #[inline(always)]
    pub fn cmp_gt(self, b: impl Into<Self>) -> Vector<T::Mask, N> {
        self.compare(b.into(), |x, y| x > y)
    }
    #[inline(always)]
    pub fn cmp_le(self, b: impl Into<Self>) -> Vector<T::Mask, N> {
        self.compare(b.into(), |x, y| x <= y)
    }
    #[inline(always)]
    pub fn cmp_ge(self, b: impl Into<Self>) -> Vector<T::Mask, N> {
        self.compare(b.into(), |x, y| x >= y)
    }
    /// `!v`: all ones where the lane is zero.
    #[inline(always)]
    pub fn lnot(self) -> Vector<T::Mask, N> {
        self.compare(self, |x, _| x.l_is_zero())
    }

    /// `__builtin_convertvector(v, VectorType<U, N>)` (`CONVERT`).
    #[inline(always)]
    pub fn convert<U: Lane>(self) -> Vector<U, N>
    where
        T: Cvt<U>,
    {
        Vector(T::cvt_array(self.0))
    }

    #[inline(always)]
    pub fn x(self) -> T {
        self.0[0]
    }
    #[inline(always)]
    pub fn y(self) -> T {
        self.0[1]
    }
    #[inline(always)]
    pub fn z(self) -> T {
        self.0[2]
    }
    #[inline(always)]
    pub fn w(self) -> T {
        self.0[3]
    }
    #[inline(always)]
    pub fn set_x(&mut self, v: T) {
        self.0[0] = v;
    }
    #[inline(always)]
    pub fn set_y(&mut self, v: T) {
        self.0[1] = v;
    }
    #[inline(always)]
    pub fn set_z(&mut self, v: T) {
        self.0[2] = v;
    }
    #[inline(always)]
    pub fn set_w(&mut self, v: T) {
        self.0[3] = v;
    }

    /// `v.swizzle<INDEXES...>()` / clang's `v.xyzw`-style access: lanes of `self` by index.
    #[inline(always)]
    pub fn swizzle<const M: usize>(self, idx: [usize; M]) -> Vector<T, M> {
        let mut r = [T::default(); M];
        for i in 0..M {
            r[i] = self.0[idx[i]];
        }
        Vector(r)
    }
}

impl<T, const N: usize> Index<usize> for Vector<T, N> {
    type Output = T;
    #[inline(always)]
    fn index(&self, i: usize) -> &T {
        &self.0[i]
    }
}

impl<T, const N: usize> IndexMut<usize> for Vector<T, N> {
    #[inline(always)]
    fn index_mut(&mut self, i: usize) -> &mut T {
        &mut self.0[i]
    }
}

// The named swizzles vector_type.h defines for its non-clang branch (`xxxx`, `zwxy`, ...); clang has them natively.
macro_rules! swizzles {
    ($($name:ident = [$($i:expr),*];)*) => {
        impl<T: Lane> Vector<T, 4> {
            $(#[inline(always)] pub fn $name(self) -> Vector<T, { [$($i),*].len() }> { self.swizzle([$($i),*]) })*
        }
    };
}
swizzles! {
    xxxx = [0, 0, 0, 0]; yyyy = [1, 1, 1, 1]; zzzz = [2, 2, 2, 2]; wwww = [3, 3, 3, 3];
    xxyy = [0, 0, 1, 1]; xxzz = [0, 0, 2, 2]; yyww = [1, 1, 3, 3]; zzww = [2, 2, 3, 3];
    xyxy = [0, 1, 0, 1]; xzxz = [0, 2, 0, 2]; ywyw = [1, 3, 1, 3]; zwzw = [2, 3, 2, 3];
    zwxy = [2, 3, 0, 1]; zyxw = [2, 1, 0, 3]; xxyz = [0, 0, 1, 2]; xyyz = [0, 1, 1, 2];
    xyzz = [0, 1, 2, 2]; xzyw = [0, 2, 1, 3]; yzwx = [1, 2, 3, 0]; wxyz = [3, 0, 1, 2];
    wzyx = [3, 2, 1, 0];
    xxxxyyyy = [0, 0, 0, 0, 1, 1, 1, 1]; zzzzwwww = [2, 2, 2, 2, 3, 3, 3, 3];
    xyzwxyzw = [0, 1, 2, 3, 0, 1, 2, 3]; xyxyxyxy = [0, 1, 0, 1, 0, 1, 0, 1];
    zwzwzwzw = [2, 3, 2, 3, 2, 3, 2, 3]; xxyyzzww = [0, 0, 1, 1, 2, 2, 3, 3];
    xxxxyyyyzzzzwwww = [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3];
}
impl<T: Lane> Vector<T, 4> {
    #[inline(always)]
    pub fn xy(self) -> Vector<T, 2> {
        self.swizzle([0, 1])
    }
    #[inline(always)]
    pub fn zw(self) -> Vector<T, 2> {
        self.swizzle([2, 3])
    }
}

// ---- operators -------------------------------------------------------------------------------------------------

macro_rules! vec_binop {
    ($tr:ident, $f:ident, $atr:ident, $af:ident, $lane:ident, $bound:ident) => {
        impl<T: $bound, const N: usize> $tr for Vector<T, N> {
            type Output = Self;
            #[inline(always)]
            fn $f(self, b: Self) -> Self {
                self.zip_with(b, T::$lane)
            }
        }
        impl<T: $bound, const N: usize> $atr for Vector<T, N> {
            #[inline(always)]
            fn $af(&mut self, b: Self) {
                *self = self.zip_with(b, T::$lane);
            }
        }
    };
}
// LLVM leaves 16-bit lane arithmetic scalar in the samplers and blend code (one `imul` and `movzwl`/`movswl` per
// lane) where swgl's clang vector code uses one SSE2 instruction per register. On x86_64 these ops on vectors of 8
// bytes or a multiple of 16 run through SSE2 directly; the instructions wrap exactly as the lane functions do.
#[cfg(target_arch = "x86_64")]
pub(crate) mod sse16 {
    use super::Vector;
    use core::arch::x86_64::*;

    #[inline(always)]
    pub const fn fits<T, const N: usize>() -> bool {
        let bytes = core::mem::size_of::<T>() * N;
        bytes == 8 || (bytes > 0 && bytes.is_multiple_of(16))
    }

    /// `f` on each 16-byte register of `a` and `b` (the low 8 bytes of one for an 8-byte vector).
    #[inline(always)]
    pub fn zip<T: Copy, const N: usize>(
        a: Vector<T, N>,
        b: Vector<T, N>,
        f: impl Fn(__m128i, __m128i) -> __m128i,
    ) -> Vector<T, N> {
        let mut r = a;
        let bytes = core::mem::size_of::<T>() * N;
        let (pa, pb, pr) = (a.0.as_ptr() as *const u8, b.0.as_ptr() as *const u8, r.0.as_mut_ptr() as *mut u8);
        // SAFETY: SSE2 is part of x86_64. The callers check `fits` (8 bytes, or whole 16-byte registers), so every
        // load and store is inside the three arrays; the unaligned forms do not rely on `align(16)`.
        unsafe {
            if bytes == 8 {
                let v = f(_mm_loadl_epi64(pa as *const __m128i), _mm_loadl_epi64(pb as *const __m128i));
                _mm_storel_epi64(pr as *mut __m128i, v);
            } else {
                let mut o = 0;
                while o < bytes {
                    let v = f(_mm_loadu_si128(pa.add(o) as *const __m128i), _mm_loadu_si128(pb.add(o) as *const __m128i));
                    _mm_storeu_si128(pr.add(o) as *mut __m128i, v);
                    o += 16;
                }
            }
        }
        r
    }
}

// The same on aarch64 with NEON (`add sub mul and orr eor` on `.8h`, `sshl`/`ushl` by a negative count for `>>`),
// which is what clang emits for swgl's vector code there.
#[cfg(target_arch = "aarch64")]
pub(crate) mod neon16 {
    use super::Vector;
    use core::arch::aarch64::*;

    #[inline(always)]
    pub const fn fits<T, const N: usize>() -> bool {
        let bytes = core::mem::size_of::<T>() * N;
        bytes == 8 || (bytes > 0 && bytes.is_multiple_of(16))
    }

    /// `f` on each 16-byte register of `a` and `b` (the low 8 bytes of one for an 8-byte vector).
    #[inline(always)]
    pub fn zip<T: Copy, const N: usize>(
        a: Vector<T, N>,
        b: Vector<T, N>,
        f: impl Fn(int16x8_t, int16x8_t) -> int16x8_t,
    ) -> Vector<T, N> {
        let mut r = a;
        let bytes = core::mem::size_of::<T>() * N;
        let (pa, pb, pr) = (a.0.as_ptr() as *const i16, b.0.as_ptr() as *const i16, r.0.as_mut_ptr() as *mut i16);
        // SAFETY: NEON is part of aarch64. The callers check `fits` (8 bytes, or whole 16-byte registers), so every
        // load and store is inside the three arrays; `vld1`/`vst1` need only the lanes' 2-byte alignment, which
        // `T` (2 bytes for the 16-bit ops, any integer for the bit ops) and `align(16)` give.
        unsafe {
            if bytes == 8 {
                let z = vdup_n_s16(0);
                let v = f(vcombine_s16(vld1_s16(pa), z), vcombine_s16(vld1_s16(pb), z));
                vst1_s16(pr, vget_low_s16(v));
            } else {
                let mut o = 0;
                while o < bytes / 2 {
                    vst1q_s16(pr.add(o), f(vld1q_s16(pa.add(o)), vld1q_s16(pb.add(o))));
                    o += 8;
                }
            }
        }
        r
    }
}

macro_rules! vec_binop_sse {
    ($tr:ident, $f:ident, $atr:ident, $af:ident, $lane:ident, $bound:ident, $int16_only:expr, $sse:ident, $neon:ident) => {
        impl<T: $bound, const N: usize> $tr for Vector<T, N> {
            type Output = Self;
            #[inline(always)]
            fn $f(self, b: Self) -> Self {
                #[cfg(target_arch = "x86_64")]
                if (T::INT16 || !$int16_only) && sse16::fits::<T, N>() {
                    // SAFETY: SSE2 is part of x86_64.
                    return sse16::zip(self, b, |x, y| unsafe { core::arch::x86_64::$sse(x, y) });
                }
                #[cfg(target_arch = "aarch64")]
                if (T::INT16 || !$int16_only) && neon16::fits::<T, N>() {
                    // SAFETY: NEON is part of aarch64.
                    return neon16::zip(self, b, |x, y| unsafe { core::arch::aarch64::$neon(x, y) });
                }
                self.zip_with(b, T::$lane)
            }
        }
        impl<T: $bound, const N: usize> $atr for Vector<T, N> {
            #[inline(always)]
            fn $af(&mut self, b: Self) {
                *self = $tr::$f(*self, b);
            }
        }
    };
}
vec_binop_sse!(Add, add, AddAssign, add_assign, l_add, Lane, true, _mm_add_epi16, vaddq_s16);
vec_binop_sse!(Sub, sub, SubAssign, sub_assign, l_sub, Lane, true, _mm_sub_epi16, vsubq_s16);
vec_binop_sse!(Mul, mul, MulAssign, mul_assign, l_mul, Lane, true, _mm_mullo_epi16, vmulq_s16);
vec_binop!(Div, div, DivAssign, div_assign, l_div, Lane);
vec_binop_sse!(BitAnd, bitand, BitAndAssign, bitand_assign, l_and, IntLane, false, _mm_and_si128, vandq_s16);
vec_binop_sse!(BitOr, bitor, BitOrAssign, bitor_assign, l_or, IntLane, false, _mm_or_si128, vorrq_s16);
vec_binop_sse!(BitXor, bitxor, BitXorAssign, bitxor_assign, l_xor, IntLane, false, _mm_xor_si128, veorq_s16);
vec_binop!(Rem, rem, RemAssign, rem_assign, l_rem, IntLane);

// Scalar operands broadcast, on either side, as clang does for `v + 1.0f` and `1.0f - v`.
macro_rules! vec_scalar_ops {
    ($bound:ident: $($t:ty),*) => {$(
        vec_scalar_ops!(@one $t, Add, add, AddAssign, add_assign);
        vec_scalar_ops!(@one $t, Sub, sub, SubAssign, sub_assign);
        vec_scalar_ops!(@one $t, Mul, mul, MulAssign, mul_assign);
        vec_scalar_ops!(@one $t, Div, div, DivAssign, div_assign);
    )*};
    (@one $t:ty, $tr:ident, $f:ident, $atr:ident, $af:ident) => {
        impl<const N: usize> $tr<$t> for Vector<$t, N> {
            type Output = Self;
            #[inline(always)]
            fn $f(self, b: $t) -> Self {
                $tr::$f(self, Self::splat(b))
            }
        }
        impl<const N: usize> $tr<Vector<$t, N>> for $t {
            type Output = Vector<$t, N>;
            #[inline(always)]
            fn $f(self, b: Vector<$t, N>) -> Vector<$t, N> {
                $tr::$f(Vector::<$t, N>::splat(self), b)
            }
        }
        impl<const N: usize> $atr<$t> for Vector<$t, N> {
            #[inline(always)]
            fn $af(&mut self, b: $t) {
                $atr::$af(self, Self::splat(b))
            }
        }
    };
}
vec_scalar_ops!(Lane: f32, f64, i8, u8, i16, u16, i32, u32, i64, u64);

macro_rules! vec_scalar_bitops {
    ($($t:ty),*) => {$(
        vec_scalar_bitops!(@one $t, BitAnd, bitand, BitAndAssign, bitand_assign);
        vec_scalar_bitops!(@one $t, BitOr, bitor, BitOrAssign, bitor_assign);
        vec_scalar_bitops!(@one $t, BitXor, bitxor, BitXorAssign, bitxor_assign);
        vec_scalar_bitops!(@one $t, Rem, rem, RemAssign, rem_assign);
    )*};
    (@one $t:ty, $tr:ident, $f:ident, $atr:ident, $af:ident) => {
        impl<const N: usize> $tr<$t> for Vector<$t, N> {
            type Output = Self;
            #[inline(always)]
            fn $f(self, b: $t) -> Self {
                $tr::$f(self, Self::splat(b))
            }
        }
        impl<const N: usize> $tr<Vector<$t, N>> for $t {
            type Output = Vector<$t, N>;
            #[inline(always)]
            fn $f(self, b: Vector<$t, N>) -> Vector<$t, N> {
                $tr::$f(Vector::<$t, N>::splat(self), b)
            }
        }
        impl<const N: usize> $atr<$t> for Vector<$t, N> {
            #[inline(always)]
            fn $af(&mut self, b: $t) {
                $atr::$af(self, Self::splat(b))
            }
        }
    };
}
vec_scalar_bitops!(i8, u8, i16, u16, i32, u32, i64, u64);

impl<T: Lane, const N: usize> Neg for Vector<T, N> {
    type Output = Self;
    #[inline(always)]
    fn neg(self) -> Self {
        self.map(T::l_neg)
    }
}

impl<T: IntLane, const N: usize> Not for Vector<T, N> {
    type Output = Self;
    /// `~v`.
    #[inline(always)]
    fn not(self) -> Self {
        self.map(T::l_not)
    }
}

impl<T: IntLane, const N: usize> Shl<i32> for Vector<T, N> {
    type Output = Self;
    #[inline(always)]
    fn shl(self, n: i32) -> Self {
        self.map(|x| x.l_shl(n))
    }
}

impl<T: IntLane, const N: usize> Shr<i32> for Vector<T, N> {
    type Output = Self;
    /// Arithmetic for signed lanes, logical for unsigned ones.
    #[inline(always)]
    fn shr(self, n: i32) -> Self {
        #[cfg(target_arch = "x86_64")]
        if T::INT16 && (0..16).contains(&n) && sse16::fits::<T, N>() {
            use core::arch::x86_64::*;
            // SAFETY: SSE2 is part of x86_64; below the lane width psraw/psrlw equal `wrapping_shr`.
            return sse16::zip(self, self, |x, _| unsafe {
                let c = _mm_cvtsi32_si128(n);
                if T::SIGNED {
                    _mm_sra_epi16(x, c)
                } else {
                    _mm_srl_epi16(x, c)
                }
            });
        }
        #[cfg(target_arch = "aarch64")]
        if T::INT16 && (0..16).contains(&n) && neon16::fits::<T, N>() {
            use core::arch::aarch64::*;
            // SAFETY: NEON is part of aarch64; a shift left by `-n`, below the lane width, is `wrapping_shr`.
            return neon16::zip(self, self, |x, _| unsafe {
                let c = vdupq_n_s16(-(n as i16));
                if T::SIGNED {
                    vshlq_s16(x, c)
                } else {
                    vreinterpretq_s16_u16(vshlq_u16(vreinterpretq_u16_s16(x), c))
                }
            });
        }
        self.map(|x| x.l_shr(n))
    }
}

impl<T: IntLane, const N: usize> ShlAssign<i32> for Vector<T, N> {
    #[inline(always)]
    fn shl_assign(&mut self, n: i32) {
        *self = *self << n;
    }
}

impl<T: IntLane, const N: usize> ShrAssign<i32> for Vector<T, N> {
    #[inline(always)]
    fn shr_assign(&mut self, n: i32) {
        *self = *self >> n;
    }
}

impl<T: IntLane, const N: usize> Vector<T, N> {
    /// `a && b` on vectors, which the C++ defines as `a & b`.
    #[inline(always)]
    pub fn land(self, b: Self) -> Self {
        self & b
    }
    /// `a || b` on vectors, which the C++ defines as `a | b`.
    #[inline(always)]
    pub fn lor(self, b: Self) -> Self {
        self | b
    }
}

// ---- CONVERT ---------------------------------------------------------------------------------------------------

/// Lane conversion as `__builtin_convertvector` performs it.
///
/// Integer to integer truncates or extends, integer to float rounds to nearest. Float to integer is `fptosi` /
/// `fptoui`, which C++ leaves undefined out of range; swgl gets whatever the instruction clang picks returns, so
/// that is what is reproduced:
/// - x86_64 (SSE2): `cvttps2dq`, which gives `0x80000000` for NaN and out-of-range lanes. To `u32`, clang emits
///   `cvttps2dq(v) | (cvttps2dq(v - 2^31) & (cvttps2dq(v) >> 31))`. To narrower integers it converts to `i32` first.
/// - aarch64 (NEON): `fcvtzs` / `fcvtzu`, saturating with NaN to 0, which is Rust's `as`.
pub trait Cvt<U>: Sized + Copy {
    fn cvt(self) -> U;
    #[inline(always)]
    fn cvt_array<const N: usize>(a: [Self; N]) -> [U; N]
    where
        U: Copy + Default,
    {
        let mut r = [U::default(); N];
        for i in 0..N {
            r[i] = a[i].cvt();
        }
        r
    }
}

/// `fptosi` to `i32` with the target's instruction semantics (see [`Cvt`]).
#[inline(always)]
pub fn f32_to_i32(f: f32) -> i32 {
    #[cfg(target_arch = "x86_64")]
    {
        // cvttss2si / cvttps2dq: the "integer indefinite" value for NaN and anything outside [-2^31, 2^31).
        if (-2147483648.0..2147483648.0).contains(&f) {
            f as i32
        } else {
            i32::MIN
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        f as i32
    }
}

/// `fptoui` to `u32` as clang lowers it for a vector lane (see [`Cvt`]).
#[inline(always)]
pub fn f32_to_u32(f: f32) -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        let a = f32_to_i32(f);
        let b = f32_to_i32(f - 2147483648.0);
        (a | (b & (a >> 31))) as u32
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        f as u32
    }
}

macro_rules! cvt_as {
    ($($from:ty => [$($to:ty),*]);* $(;)?) => {$($(
        impl Cvt<$to> for $from {
            #[inline(always)]
            fn cvt(self) -> $to { self as $to }
        }
    )*)*};
}
cvt_as! {
    i8 => [i8, u8, i16, u16, i32, u32, i64, u64, f32, f64];
    u8 => [i8, u8, i16, u16, i32, u32, i64, u64, f32, f64];
    i16 => [i8, u8, i16, u16, i32, u32, i64, u64, f32, f64];
    u16 => [i8, u8, i16, u16, i32, u32, i64, u64, f32, f64];
    i32 => [i8, u8, i32, u32, i64, u64, f32, f64];
    u32 => [i8, u8, i32, u32, i64, u64, f32, f64];
    i64 => [i8, u8, i16, u16, i32, u32, i64, u64, f32, f64];
    u64 => [i8, u8, i16, u16, i32, u32, i64, u64, f32, f64];
    f32 => [f32, f64];
    f64 => [f32, f64];
}

// 32-bit to 16-bit lanes truncate, as `__builtin_convertvector` does. On aarch64 four lanes at a time are one
// `xtn` (`vmovn_s32`), what clang emits for swgl's CONVERT; LLVM left the lane loop scalar there (umov/and/orr per
// lane in textureLinearUnpackedR8's computeFracX).
macro_rules! cvt_narrow32 {
    ($($from:ty => $to:ty),*) => {$(
        impl Cvt<$to> for $from {
            #[inline(always)]
            fn cvt(self) -> $to {
                self as $to
            }
            #[inline(always)]
            fn cvt_array<const N: usize>(a: [$from; N]) -> [$to; N] {
                #[cfg(target_arch = "aarch64")]
                if N.is_multiple_of(4) {
                    use core::arch::aarch64::*;
                    let mut r = [0 as $to; N];
                    let mut k = 0;
                    while k < N {
                        // SAFETY: NEON is part of aarch64; lanes `k..k + 4` are inside both arrays (`N % 4 == 0`).
                        unsafe {
                            let v = vld1q_s32(a.as_ptr().add(k) as *const i32);
                            vst1_s16(r.as_mut_ptr().add(k) as *mut i16, vmovn_s32(v));
                        }
                        k += 4;
                    }
                    return r;
                }
                a.map(|x| x as $to)
            }
        }
    )*};
}
cvt_narrow32!(i32 => i16, i32 => u16, u32 => i16, u32 => u16);

impl Cvt<i32> for f32 {
    #[inline(always)]
    fn cvt(self) -> i32 {
        f32_to_i32(self)
    }
    #[inline(always)]
    fn cvt_array<const N: usize>(a: [f32; N]) -> [i32; N] {
        #[cfg(target_arch = "x86_64")]
        if N.is_multiple_of(4) {
            let mut r = [0i32; N];
            for (src, dst) in a.as_chunks::<4>().0.iter().zip(r.as_chunks_mut::<4>().0.iter_mut()) {
                *dst = sse::cvttps_epi32(Float::new(*src)).0;
            }
            return r;
        }
        let mut r = [0i32; N];
        for i in 0..N {
            r[i] = f32_to_i32(a[i]);
        }
        r
    }
}

impl Cvt<u32> for f32 {
    #[inline(always)]
    fn cvt(self) -> u32 {
        f32_to_u32(self)
    }
}

macro_rules! cvt_float_narrow {
    ($($to:ty),*) => {$(
        impl Cvt<$to> for f32 {
            /// clang converts to `i32` (`cvttps2dq` / `fcvtzs`) and truncates; only in-range values are defined.
            #[inline(always)]
            fn cvt(self) -> $to { f32_to_i32(self) as $to }
        }
    )*};
}
cvt_float_narrow!(i8, u8, i16, u16);

impl Cvt<i64> for f32 {
    #[inline(always)]
    fn cvt(self) -> i64 {
        self as i64
    }
}

impl Cvt<u64> for f32 {
    #[inline(always)]
    fn cvt(self) -> u64 {
        self as u64
    }
}

// ---- shuffles and halves -----------------------------------------------------------------------------------------

/// `SHUFFLE(a, b, ...)` (`__builtin_shufflevector`): index `i < N` picks `a[i]`, `i >= N` picks `b[i - N]`.
/// `-1` (an undefined lane in clang) gives zero.
#[inline(always)]
pub fn shuffle<T: Lane, const N: usize, const M: usize>(
    a: Vector<T, N>,
    b: Vector<T, N>,
    idx: [i32; M],
) -> Vector<T, M> {
    let mut r = [T::default(); M];
    for i in 0..M {
        let k = idx[i];
        if k >= 0 {
            let k = k as usize;
            r[i] = if k < N { a.0[k] } else { b.0[k - N] };
        }
    }
    Vector(r)
}

/// Vectors that have a double-width counterpart: `combine`, `repeat2`, `expand`, `zip`.
pub trait Combine: Sized {
    type Double;
    fn combine(self, b: Self) -> Self::Double;
}

/// Vectors that split into halves: `lowHalf`, `highHalf`.
pub trait Halves: Sized {
    type Half;
    fn low_half(self) -> Self::Half;
    fn high_half(self) -> Self::Half;
}

macro_rules! combine_impl {
    ($($n:literal => $d:literal),*) => {$(
        impl<T: Lane> Combine for Vector<T, $n> {
            type Double = Vector<T, $d>;
            #[inline(always)]
            fn combine(self, b: Self) -> Vector<T, $d> {
                let mut r = [T::default(); $d];
                r[..$n].copy_from_slice(&self.0);
                r[$n..].copy_from_slice(&b.0);
                Vector(r)
            }
        }
        impl<T: Lane> Halves for Vector<T, $d> {
            type Half = Vector<T, $n>;
            #[inline(always)]
            fn low_half(self) -> Vector<T, $n> {
                let mut r = [T::default(); $n];
                r.copy_from_slice(&self.0[..$n]);
                Vector(r)
            }
            #[inline(always)]
            fn high_half(self) -> Vector<T, $n> {
                let mut r = [T::default(); $n];
                r.copy_from_slice(&self.0[$n..]);
                Vector(r)
            }
        }
    )*};
}
combine_impl!(2 => 4, 4 => 8, 8 => 16);

/// `combine(a, b)`: `a` in the low lanes, `b` in the high lanes.
#[inline(always)]
pub fn combine<V: Combine>(a: V, b: V) -> V::Double {
    a.combine(b)
}

/// The four-argument `combine(a, b, c, d)` = `combine(combine(a, b), combine(c, d))`.
#[inline(always)]
pub fn combine4<V>(a: V, b: V, c: V, d: V) -> <V::Double as Combine>::Double
where
    V: Combine,
    V::Double: Combine,
{
    combine(combine(a, b), combine(c, d))
}

#[inline(always)]
pub fn low_half<V: Halves>(a: V) -> V::Half {
    a.low_half()
}

#[inline(always)]
pub fn high_half<V: Halves>(a: V) -> V::Half {
    a.high_half()
}

/// `expand(V4) -> V8`: the low four lanes are `a`; clang leaves the high four undefined, here they are zero.
#[inline(always)]
pub fn expand<T: Lane>(a: Vector<T, 4>) -> Vector<T, 8> {
    shuffle(a, a, [0, 1, 2, 3, -1, -1, -1, -1])
}

/// `combineLow(a, b)` = `combine(lowHalf(a), lowHalf(b))`.
#[inline(always)]
pub fn combine_low<V>(a: V, b: V) -> V
where
    V: Halves,
    V::Half: Combine<Double = V>,
{
    combine(low_half(a), low_half(b))
}

/// `combineHigh(a, b)` = `combine(highHalf(a), highHalf(b))`.
#[inline(always)]
pub fn combine_high<V>(a: V, b: V) -> V
where
    V: Halves,
    V::Half: Combine<Double = V>,
{
    combine(high_half(a), high_half(b))
}

#[inline(always)]
pub fn repeat2<V: Combine + Copy>(a: V) -> V::Double {
    combine(a, a)
}

#[inline(always)]
pub fn repeat4<V>(a: V) -> <V::Double as Combine>::Double
where
    V: Combine + Copy,
    V::Double: Combine,
{
    combine4(a, a, a, a)
}

/// `zipLow`/`zipHigh`: interleave the low (high) halves of two vectors of the same width.
pub trait Zip: Sized {
    fn zip_low(self, b: Self) -> Self;
    fn zip_high(self, b: Self) -> Self;
}

impl<T: Lane> Zip for Vector<T, 4> {
    #[inline(always)]
    fn zip_low(self, b: Self) -> Self {
        shuffle(self, b, [0, 4, 1, 5])
    }
    #[inline(always)]
    fn zip_high(self, b: Self) -> Self {
        shuffle(self, b, [2, 6, 3, 7])
    }
}

impl<T: Lane> Zip for Vector<T, 8> {
    #[inline(always)]
    fn zip_low(self, b: Self) -> Self {
        shuffle(self, b, [0, 8, 1, 9, 2, 10, 3, 11])
    }
    #[inline(always)]
    fn zip_high(self, b: Self) -> Self {
        shuffle(self, b, [4, 12, 5, 13, 6, 14, 7, 15])
    }
}

impl<T: Lane> Zip for Vector<T, 16> {
    /// For 16 lanes the C++ takes the low 8 lanes of each (not an interleave).
    #[inline(always)]
    fn zip_low(self, b: Self) -> Self {
        shuffle(self, b, [0, 1, 2, 3, 4, 5, 6, 7, 16, 17, 18, 19, 20, 21, 22, 23])
    }
    #[inline(always)]
    fn zip_high(self, b: Self) -> Self {
        shuffle(self, b, [8, 9, 10, 11, 12, 13, 14, 15, 24, 25, 26, 27, 28, 29, 30, 31])
    }
}

#[inline(always)]
pub fn zip_low<V: Zip>(a: V, b: V) -> V {
    a.zip_low(b)
}

#[inline(always)]
pub fn zip_high<V: Zip>(a: V, b: V) -> V {
    a.zip_high(b)
}

/// `zip2Low`: interleave pairs of lanes from the low halves.
#[inline(always)]
pub fn zip2_low<T: Lane>(a: Vector<T, 8>, b: Vector<T, 8>) -> Vector<T, 8> {
    shuffle(a, b, [0, 1, 8, 9, 2, 3, 10, 11])
}

/// `zip2High`: interleave pairs of lanes from the high halves.
#[inline(always)]
pub fn zip2_high<T: Lane>(a: Vector<T, 8>, b: Vector<T, 8>) -> Vector<T, 8> {
    shuffle(a, b, [4, 5, 12, 13, 6, 7, 14, 15])
}

/// `zip(a, b)`: full interleave into a vector twice as wide.
pub trait ZipFull: Combine {
    fn zip(self, b: Self) -> Self::Double;
}

impl<T: Lane> ZipFull for Vector<T, 4> {
    #[inline(always)]
    fn zip(self, b: Self) -> Vector<T, 8> {
        shuffle(self, b, [0, 4, 1, 5, 2, 6, 3, 7])
    }
}

impl<T: Lane> ZipFull for Vector<T, 8> {
    #[inline(always)]
    fn zip(self, b: Self) -> Vector<T, 16> {
        shuffle(self, b, [0, 8, 1, 9, 2, 10, 3, 11, 4, 12, 5, 13, 6, 14, 7, 15])
    }
}

#[inline(always)]
pub fn zip<V: ZipFull>(a: V, b: V) -> V::Double {
    a.zip(b)
}

// ---- bit_cast and unaligned access -------------------------------------------------------------------------------

/// Plain data whose first `SIZE` bytes are its value: the vectors and the primitive lane types.
///
/// # Safety
/// Every bit pattern of `SIZE` bytes must be a valid value, and the value must occupy bytes `0..SIZE`.
pub unsafe trait Pod: Copy {
    const SIZE: usize;
}

macro_rules! pod_prim {
    ($($t:ty),*) => {$(
        // SAFETY: primitive integers and floats accept every bit pattern.
        unsafe impl Pod for $t { const SIZE: usize = core::mem::size_of::<$t>(); }
    )*};
}
pod_prim!(i8, u8, i16, u16, i32, u32, i64, u64, f32, f64);

// SAFETY: `Vector` is `repr(C)` around `[T; N]`, so the lanes are bytes `0..N * T::SIZE`; padding up to the
// alignment comes after them and is never read.
unsafe impl<T: Pod, const N: usize> Pod for Vector<T, N> {
    const SIZE: usize = N * T::SIZE;
}

/// `bit_cast<D>(src)`: reinterpret the bytes of a value of the same size.
#[inline(always)]
pub fn bit_cast<D: Pod, S: Pod>(src: S) -> D {
    const { assert!(D::SIZE == S::SIZE) };
    let mut d = core::mem::MaybeUninit::<D>::zeroed();
    // SAFETY: both types hold `SIZE` bytes of value at offset 0 and accept any bit pattern (`Pod`); `d` is zeroed so
    // any padding past `SIZE` is initialised.
    unsafe {
        core::ptr::copy_nonoverlapping(&src as *const S as *const u8, d.as_mut_ptr() as *mut u8, S::SIZE);
        d.assume_init()
    }
}

/// `unaligned_load<T>(p)`: read a `T` from bytes at any alignment.
///
/// # Safety
/// `p` must be valid for reading `T::SIZE` bytes.
#[inline(always)]
pub unsafe fn unaligned_load<T: Pod>(p: *const u8) -> T {
    let mut d = core::mem::MaybeUninit::<T>::zeroed();
    // SAFETY: the caller guarantees `T::SIZE` readable bytes at `p`; `d` has room for them (`Pod`).
    unsafe {
        core::ptr::copy_nonoverlapping(p, d.as_mut_ptr() as *mut u8, T::SIZE);
        d.assume_init()
    }
}

/// `unaligned_store(p, v)`: write `v`'s bytes at any alignment.
///
/// # Safety
/// `p` must be valid for writing `T::SIZE` bytes.
#[inline(always)]
pub unsafe fn unaligned_store<T: Pod>(p: *mut u8, v: T) {
    // SAFETY: the caller guarantees `T::SIZE` writable bytes at `p`; `v` holds that many bytes of value.
    unsafe { core::ptr::copy_nonoverlapping(&v as *const T as *const u8, p, T::SIZE) }
}

/// Safe slice forms of [`unaligned_load`] / [`unaligned_store`]; panic if the slice is too short.
#[inline(always)]
pub fn load_bytes<T: Pod>(bytes: &[u8]) -> T {
    assert!(bytes.len() >= T::SIZE);
    // SAFETY: the slice holds at least `T::SIZE` bytes (checked above).
    unsafe { unaligned_load(bytes.as_ptr()) }
}

#[inline(always)]
pub fn store_bytes<T: Pod>(bytes: &mut [u8], v: T) {
    assert!(bytes.len() >= T::SIZE);
    // SAFETY: the slice holds at least `T::SIZE` writable bytes (checked above).
    unsafe { unaligned_store(bytes.as_mut_ptr(), v) }
}

// ---- conversions to the platform SIMD registers ------------------------------------------------------------------

// Every intrinsic call goes through one of these wrappers. SAFETY for all of them: the intrinsics only need SSE
// and SSE2 (resp. NEON), which are part of the x86_64 (resp. aarch64) baseline every Rust target for these
// architectures enables, and they take and return plain register values (no memory access).

#[cfg(target_arch = "x86_64")]
pub(crate) mod sse {
    use super::{Float, I32};
    use core::arch::x86_64 as a;
    use core::arch::x86_64::{__m128, __m128i};

    #[inline(always)]
    pub fn ps(v: Float) -> __m128 {
        // SAFETY: Float is 16 bytes of f32 lanes, the same size and layout as __m128.
        unsafe { core::mem::transmute::<Float, __m128>(v) }
    }
    #[inline(always)]
    pub fn from_ps(v: __m128) -> Float {
        // SAFETY: as above; every bit pattern is a valid Float.
        unsafe { core::mem::transmute::<__m128, Float>(v) }
    }
    #[inline(always)]
    pub fn from_si(v: __m128i) -> I32 {
        // SAFETY: I32 is 16 bytes of i32 lanes, the same size as __m128i; every bit pattern is valid.
        unsafe { core::mem::transmute::<__m128i, I32>(v) }
    }
    #[inline(always)]
    pub fn si(v: I32) -> __m128i {
        // SAFETY: as above.
        unsafe { core::mem::transmute::<I32, __m128i>(v) }
    }

    macro_rules! wrap {
        ($($name:ident($($arg:ident: $t:ty),*) -> $r:ty = $e:expr;)*) => {$(
            #[inline(always)]
            pub fn $name($($arg: $t),*) -> $r {
                // SAFETY: see the comment above this module (SSE/SSE2 baseline, register-only).
                unsafe { $e }
            }
        )*};
    }
    wrap! {
        min_ps(x: Float, y: Float) -> Float = from_ps(a::_mm_min_ps(ps(x), ps(y)));
        max_ps(x: Float, y: Float) -> Float = from_ps(a::_mm_max_ps(ps(x), ps(y)));
        sqrt_ps(x: Float) -> Float = from_ps(a::_mm_sqrt_ps(ps(x)));
        rcp_ps(x: Float) -> Float = from_ps(a::_mm_rcp_ps(ps(x)));
        rsqrt_ps(x: Float) -> Float = from_ps(a::_mm_rsqrt_ps(ps(x)));
        rcp_ss(x: f32) -> f32 = a::_mm_cvtss_f32(a::_mm_rcp_ss(a::_mm_set_ss(x)));
        rsqrt_ss(x: f32) -> f32 = a::_mm_cvtss_f32(a::_mm_rsqrt_ss(a::_mm_set_ss(x)));
        cvtss_si32(x: f32) -> i32 = a::_mm_cvtss_si32(a::_mm_set_ss(x));
        cvtps_epi32(x: Float) -> I32 = from_si(a::_mm_cvtps_epi32(ps(x)));
        cvttps_epi32(x: Float) -> I32 = from_si(a::_mm_cvttps_epi32(ps(x)));
        movemask_ps(x: I32) -> i32 = a::_mm_movemask_ps(a::_mm_castsi128_ps(si(x)));
    }
}

#[cfg(target_arch = "aarch64")]
pub(crate) mod neon {
    use super::Float;
    use core::arch::aarch64 as a;
    use core::arch::aarch64::float32x4_t;

    #[inline(always)]
    pub fn f32x4(v: Float) -> float32x4_t {
        // SAFETY: Float is 16 bytes of f32 lanes, the same size and layout as float32x4_t.
        unsafe { core::mem::transmute::<Float, float32x4_t>(v) }
    }
    #[inline(always)]
    pub fn from_f32x4(v: float32x4_t) -> Float {
        // SAFETY: as above; every bit pattern is a valid Float.
        unsafe { core::mem::transmute::<float32x4_t, Float>(v) }
    }

    macro_rules! wrap {
        ($($name:ident($($arg:ident: $t:ty),*) -> $r:ty = $e:expr;)*) => {$(
            #[inline(always)]
            pub fn $name($($arg: $t),*) -> $r {
                // SAFETY: see the comment above the `sse` module (NEON baseline, register-only).
                #[allow(unused_unsafe)]
                unsafe { $e }
            }
        )*};
    }
    wrap! {
        vminq_f32(x: Float, y: Float) -> Float = from_f32x4(a::vminq_f32(f32x4(x), f32x4(y)));
        vmaxq_f32(x: Float, y: Float) -> Float = from_f32x4(a::vmaxq_f32(f32x4(x), f32x4(y)));
        vrsqrteq_f32(x: Float) -> Float = from_f32x4(a::vrsqrteq_f32(f32x4(x)));
        vrsqrtsq_f32(x: Float, y: Float) -> Float = from_f32x4(a::vrsqrtsq_f32(f32x4(x), f32x4(y)));
        vrecpeq_f32(x: Float) -> Float = from_f32x4(a::vrecpeq_f32(f32x4(x)));
        vrecpsq_f32(x: Float, y: Float) -> Float = from_f32x4(a::vrecpsq_f32(f32x4(x), f32x4(y)));
        vabsq_f32(x: Float) -> Float = from_f32x4(a::vabsq_f32(f32x4(x)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_are_all_ones() {
        let a = Float::new([1.0, 2.0, f32::NAN, -0.0]);
        let b = Float::new([1.0, 3.0, f32::NAN, 0.0]);
        assert_eq!(a.cmp_eq(b).0, [-1, 0, 0, -1]);
        assert_eq!(a.cmp_ne(b).0, [0, -1, -1, 0]);
        assert_eq!(V8::<u16>::splat(3).cmp_gt(V8::splat(2)).0, [-1i16; 8]);
    }

    #[test]
    fn halves_and_zips() {
        let a = V8::<u16>::new([0, 1, 2, 3, 4, 5, 6, 7]);
        let b = V8::<u16>::new([8, 9, 10, 11, 12, 13, 14, 15]);
        assert_eq!(zip_low(a, b).0, [0, 8, 1, 9, 2, 10, 3, 11]);
        assert_eq!(zip2_high(a, b).0, [4, 5, 12, 13, 6, 7, 14, 15]);
        assert_eq!(combine(low_half(a), high_half(b)).0, [0, 1, 2, 3, 12, 13, 14, 15]);
        assert_eq!(combine_high(a, b).0, [4, 5, 6, 7, 12, 13, 14, 15]);
        let w: V16<u16> = zip(a, b);
        assert_eq!(w.0[..4], [0, 8, 1, 9]);
        assert_eq!(bit_cast::<u32, U8>(U8::new([1, 2, 3, 4])), 0x0403_0201);
    }

    #[test]
    fn float_to_int_like_the_hardware() {
        let v = Float::new([f32::NAN, 3e9, -1.5, 2.5]);
        #[cfg(target_arch = "x86_64")]
        assert_eq!(v.convert::<i32>().0, [i32::MIN, i32::MIN, -1, 2]);
        #[cfg(target_arch = "aarch64")]
        assert_eq!(v.convert::<i32>().0, [0, i32::MAX, -1, 2]);
    }
}
