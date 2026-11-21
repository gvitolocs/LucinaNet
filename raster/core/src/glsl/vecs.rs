/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! glsl.h's vector types: `vec2..4`, `ivec2..4`, `bvec2..4`, their `_scalar` forms, the `_ref` l-value swizzles,
//! and the built-ins defined on them.
//!
//! Swizzles: `v.sel(X)` is one component, `v.sel2(X, Y)`, `v.sel3(..)`, `v.sel4(..)` build the 2-, 3- and
//! 4-component values the C++ `sel(c1, c2, ...)` overloads return; `v.lsel2(..)` etc. return the `_ref` types for
//! assignment through a swizzle. The `make_*` constructors take one argument or a tuple of them
//! (`make_vec4((xy, z, w))`); all-scalar arguments give the `_scalar` type as in the C++, lane arguments the lane
//! type. Mixed arguments convert with `.into()` first, which is the conversion the C++ does implicitly.

use super::*;
use core::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

#[inline(always)]
fn none<T>(c: XYZW, n: usize) -> T {
    // The C++ marks this UNREACHABLE; a swizzle beyond the type's components is a shader compiler bug.
    panic!("component {c:?} out of range for a {n}-component vector")
}

// ---- float vectors -----------------------------------------------------------------------------------------------

macro_rules! fvec {
    ($v:ident, $s:ident, $n:literal, [$($c:ident: $C:ident = $i:literal),+]) => {
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $v { $(pub $c: Float),+ }

        #[derive(Clone, Copy, Debug, Default, PartialEq)]
        pub struct $s { $(pub $c: f32),+ }

        impl $s {
            #[inline(always)]
            pub const fn new($($c: f32),+) -> Self { Self { $($c),+ } }
            #[inline(always)]
            pub const fn splat(a: f32) -> Self { Self { $($c: a),+ } }
            #[inline(always)]
            pub fn select(&self, c: XYZW) -> f32 {
                match c { $(XYZW::$C => self.$c,)+ #[allow(unreachable_patterns)] _ => none(c, $n) }
            }
            #[inline(always)]
            pub fn select_mut(&mut self, c: XYZW) -> &mut f32 {
                match c { $(XYZW::$C => &mut self.$c,)+ #[allow(unreachable_patterns)] _ => none(c, $n) }
            }
            #[inline(always)]
            pub fn sel(&self, c1: XYZW) -> f32 { self.select(c1) }
            #[inline(always)]
            pub fn sel2(&self, c1: XYZW, c2: XYZW) -> vec2_scalar { vec2_scalar::new(self.select(c1), self.select(c2)) }
            #[inline(always)]
            pub fn sel3(&self, c1: XYZW, c2: XYZW, c3: XYZW) -> vec3_scalar {
                vec3_scalar::new(self.select(c1), self.select(c2), self.select(c3))
            }
            #[inline(always)]
            pub fn sel4(&self, c1: XYZW, c2: XYZW, c3: XYZW, c4: XYZW) -> vec4_scalar {
                vec4_scalar::new(self.select(c1), self.select(c2), self.select(c3), self.select(c4))
            }
            /// Component `n` (0-based), the C++ `operator[]`.
            #[inline(always)]
            pub fn get(&self, n: usize) -> f32 { [$(self.$c),+][n] }
            #[inline(always)]
            fn map(self, f: impl Fn(f32) -> f32) -> Self { Self { $($c: f(self.$c)),+ } }
            #[inline(always)]
            fn zip(self, b: Self, f: impl Fn(f32, f32) -> f32) -> Self { Self { $($c: f(self.$c, b.$c)),+ } }
            #[inline(always)]
            fn zip3(self, b: Self, c: Self, f: impl Fn(f32, f32, f32) -> f32) -> Self { Self { $($c: f(self.$c, b.$c, c.$c)),+ } }
        }

        impl $v {
            #[inline(always)]
            pub fn new($($c: Float),+) -> Self { Self { $($c),+ } }
            #[inline(always)]
            pub fn splat(a: Float) -> Self { Self { $($c: a),+ } }
            /// The C++ constructor from four scalar values, one per lane.
            #[inline(always)]
            pub fn from_scalars(s0: $s, s1: $s, s2: $s, s3: $s) -> Self {
                Self { $($c: Float::new([s0.$c, s1.$c, s2.$c, s3.$c])),+ }
            }
            #[inline(always)]
            pub fn select(&self, c: XYZW) -> Float {
                match c { $(XYZW::$C => self.$c,)+ #[allow(unreachable_patterns)] _ => none(c, $n) }
            }
            #[inline(always)]
            pub fn select_mut(&mut self, c: XYZW) -> &mut Float {
                match c { $(XYZW::$C => &mut self.$c,)+ #[allow(unreachable_patterns)] _ => none(c, $n) }
            }
        }

        /// `Float& operator[](int index)`: the C++ switch, UNREACHABLE past the last component.
        impl core::ops::Index<usize> for $v {
            type Output = Float;
            #[inline(always)]
            fn index(&self, i: usize) -> &Float {
                $(if i == XYZW::$C as usize { return &self.$c; })+
                panic!("index {i} out of range for a {}-component vector", $n)
            }
        }

        impl core::ops::IndexMut<usize> for $v {
            #[inline(always)]
            fn index_mut(&mut self, i: usize) -> &mut Float {
                $(if i == XYZW::$C as usize { return &mut self.$c; })+
                panic!("index {i} out of range for a {}-component vector", $n)
            }
        }

        impl $v {
            #[inline(always)]
            pub fn sel(&self, c1: XYZW) -> Float { self.select(c1) }
            #[inline(always)]
            pub fn sel2(&self, c1: XYZW, c2: XYZW) -> vec2 { vec2::new(self.select(c1), self.select(c2)) }
            #[inline(always)]
            pub fn sel3(&self, c1: XYZW, c2: XYZW, c3: XYZW) -> vec3 {
                vec3::new(self.select(c1), self.select(c2), self.select(c3))
            }
            #[inline(always)]
            pub fn sel4(&self, c1: XYZW, c2: XYZW, c3: XYZW, c4: XYZW) -> vec4 {
                vec4::new(self.select(c1), self.select(c2), self.select(c3), self.select(c4))
            }
            /// Component `n` (0-based), the C++ `operator[](int)`.
            #[inline(always)]
            pub fn get(&self, n: usize) -> Float { [$(self.$c),+][n] }
            /// Mutable component `n` (0-based).
            #[inline(always)]
            pub fn get_mut(&mut self, n: usize) -> &mut Float {
                let mut i = 0usize;
                $(if i == n { return &mut self.$c; } i += 1;)+
                let _ = i;
                panic!("component {n} out of range for a {}-component vector", $n)
            }
            fn refs(&mut self) -> [Option<&mut Float>; 4] {
                let mut r: [Option<&mut Float>; 4] = [None, None, None, None];
                $(r[$i] = Some(&mut self.$c);)+
                r
            }
            /// `lsel(c1, c2)`: an l-value swizzle. Components must be distinct, as GLSL requires.
            #[inline(always)]
            pub fn lsel2(&mut self, c1: XYZW, c2: XYZW) -> vec2_ref<'_> {
                let mut r = self.refs();
                let x = r[c1 as usize].take().expect("lsel component");
                let y = r[c2 as usize].take().expect("lsel component");
                vec2_ref { x, y }
            }
            #[inline(always)]
            pub fn lsel3(&mut self, c1: XYZW, c2: XYZW, c3: XYZW) -> vec3_ref<'_> {
                let mut r = self.refs();
                let x = r[c1 as usize].take().expect("lsel component");
                let y = r[c2 as usize].take().expect("lsel component");
                let z = r[c3 as usize].take().expect("lsel component");
                vec3_ref { x, y, z }
            }
            #[inline(always)]
            fn map(self, f: impl Fn(Float) -> Float) -> Self { Self { $($c: f(self.$c)),+ } }
            #[inline(always)]
            fn zip(self, b: Self, f: impl Fn(Float, Float) -> Float) -> Self { Self { $($c: f(self.$c, b.$c)),+ } }
            #[inline(always)]
            fn zip3(self, b: Self, c: Self, f: impl Fn(Float, Float, Float) -> Float) -> Self { Self { $($c: f(self.$c, b.$c, c.$c)),+ } }
            /// `l == r`: the lanes where every component is equal.
            #[inline(always)]
            pub fn cmp_eq(&self, r: &Self) -> I32 {
                let mut m = I32::splat(-1);
                $(m &= self.$c.cmp_eq(r.$c);)+
                m
            }
            /// `l != r`: the lanes where any component differs.
            #[inline(always)]
            pub fn cmp_ne(&self, r: &Self) -> I32 {
                let mut m = I32::splat(0);
                $(m |= self.$c.cmp_ne(r.$c);)+
                m
            }
        }

        impl From<$s> for $v {
            #[inline(always)]
            fn from(s: $s) -> Self { Self { $($c: Float::splat(s.$c)),+ } }
        }
        impl From<Float> for $v {
            #[inline(always)]
            fn from(a: Float) -> Self { Self::splat(a) }
        }
        impl From<f32> for $v {
            #[inline(always)]
            fn from(a: f32) -> Self { Self::splat(Float::splat(a)) }
        }
        impl From<f32> for $s {
            #[inline(always)]
            fn from(a: f32) -> Self { Self::splat(a) }
        }
        impl From<i32> for $s {
            #[inline(always)]
            fn from(a: i32) -> Self { Self::splat(a as f32) }
        }

        fvec_ops!($v, Float, f32);
        fvec_ops!($s, f32);
        fvec_mixed_ops!($v, $s);

        impl Neg for $v {
            type Output = Self;
            #[inline(always)]
            fn neg(self) -> Self { self.map(|a| -a) }
        }
        impl Neg for $s {
            type Output = Self;
            #[inline(always)]
            fn neg(self) -> Self { self.map(|a| -a) }
        }

        impl IfThenElse<$v> for I32 {
            type Output = $v;
            #[inline(always)]
            fn if_then_else(self, t: $v, e: $v) -> $v { t.zip(e, |a, b| if_then_else(self, a, b)) }
        }

        impl ForceScalar for $v {
            type Output = $s;
            #[inline(always)]
            fn force_scalar(self) -> $s { $s { $($c: self.$c[0]),+ } }
        }
        impl GetNth for $v {
            type Output = $s;
            #[inline(always)]
            fn get_nth(self, n: usize) -> $s { $s { $($c: self.$c[n]),+ } }
        }
        impl PutNth<$s> for $v {
            #[inline(always)]
            fn put_nth(&mut self, n: usize, src: $s) { $(self.$c[n] = src.$c;)+ }
        }
        impl PutNthComponent<f32> for $s {
            #[inline(always)]
            fn put_nth_component(&mut self, n: usize, src: f32) {
                let mut i = 0usize;
                $(if i == n { self.$c = src; } i += 1;)+
                let _ = i;
            }
        }
        impl InitInterp for $s {
            type Output = $v;
            #[inline(always)]
            fn init_interp(self, step: $s) -> $v { $v { $($c: init_interp(self.$c, step.$c)),+ } }
        }

        impl Step for $v {
            type Output = $v;
            #[inline(always)]
            fn step(self, x: $v) -> $v { self.zip(x, step) }
        }
        impl Step for $s {
            type Output = $s;
            #[inline(always)]
            fn step(self, x: $s) -> $s { self.zip(x, step) }
        }
        /// GLSL's `step(float edge, genType x)`: C++ reaches `step($s, $s)` through `$s`'s implicit constructor
        /// from a `float`.
        impl Step<$s> for f32 {
            type Output = $s;
            #[inline(always)]
            fn step(self, x: $s) -> $s { <$s>::from(self).step(x) }
        }
        /// The lane form: `step(Float, $v)` through `$v`'s constructor from a `Float`.
        impl Step<$v> for Float {
            type Output = $v;
            #[inline(always)]
            fn step(self, x: $v) -> $v { <$v>::from(self).step(x) }
        }
        impl Floor for $v {
            #[inline(always)]
            fn floor(self) -> $v { self.map(floor) }
        }

        /// `mix(x, y, a)` with an `f32` weight: `(y - x) * a + x` in the type of `x`.
        impl Mix<$v, f32> for $v {
            type Output = $v;
            #[inline(always)]
            fn mix(self, y: $v, a: f32) -> $v { self.zip(y, |x, y| (y - x) * a + x) }
        }
        impl Mix<$v, Float> for $v {
            type Output = $v;
            #[inline(always)]
            fn mix(self, y: $v, a: Float) -> $v { self.zip(y, |x, y| (y - x) * a + x) }
        }
        impl Mix<$v, $v> for $v {
            type Output = $v;
            #[inline(always)]
            fn mix(self, y: $v, a: $v) -> $v { self.zip3(y, a, |x, y, a| (y - x) * a + x) }
        }
        /// The C++ `mix(T, T, vecN_scalar)`: one weight per component.
        impl Mix<$v, $s> for $v {
            type Output = $v;
            #[inline(always)]
            fn mix(self, y: $v, a: $s) -> $v { $v { $($c: (y.$c - self.$c) * a.$c + self.$c),+ } }
        }
        impl Mix<$s, $s> for $s {
            type Output = $s;
            #[inline(always)]
            fn mix(self, y: $s, a: $s) -> $s { self.zip3(y, a, |x, y, a| (y - x) * a + x) }
        }
        impl Mix<$s, f32> for $s {
            type Output = $s;
            #[inline(always)]
            fn mix(self, y: $s, a: f32) -> $s { self.zip(y, |x, y| (y - x) * a + x) }
        }
        /// The C++ `mix(T x, U y, A a)` with `T = vecN_scalar`, which returns `T::vector_type`.
        impl Mix<$v, Float> for $s {
            type Output = $v;
            #[inline(always)]
            fn mix(self, y: $v, a: Float) -> $v { Mix::mix($v::from(self), y, a) }
        }
        impl Mix<$v, $v> for $s {
            type Output = $v;
            #[inline(always)]
            fn mix(self, y: $v, a: $v) -> $v { Mix::mix($v::from(self), y, a) }
        }
        impl Mix<$s, Float> for $s {
            type Output = $v;
            #[inline(always)]
            fn mix(self, y: $s, a: Float) -> $v { Mix::mix($v::from(self), $v::from(y), a) }
        }
    };
}

macro_rules! fvec_ops {
    ($t:ident, $e:ty $(, $e2:ty)?) => {
        fvec_ops!(@op $t, $e $(, $e2)?; Add add AddAssign add_assign +);
        fvec_ops!(@op $t, $e $(, $e2)?; Sub sub SubAssign sub_assign -);
        fvec_ops!(@op $t, $e $(, $e2)?; Mul mul MulAssign mul_assign *);
        fvec_ops!(@op $t, $e $(, $e2)?; Div div DivAssign div_assign /);
    };
    (@op $t:ident, $e:ty $(, $e2:ty)?; $tr:ident $f:ident $atr:ident $af:ident $o:tt) => {
        fvec_ops!(@common $t, $e $(, $e2)?; $tr $f $atr $af $o);
        impl $tr<$e> for $t {
            type Output = $t;
            #[inline(always)]
            fn $f(self, b: $e) -> $t { self.map(|a| a $o b) }
        }
        impl $atr<$e> for $t {
            #[inline(always)]
            fn $af(&mut self, b: $e) { *self = *self $o b; }
        }
        $(
            impl $tr<$e2> for $t {
                type Output = $t;
                #[inline(always)]
                fn $f(self, b: $e2) -> $t { self.map(|a| a $o b) }
            }
            impl $atr<$e2> for $t {
                #[inline(always)]
                fn $af(&mut self, b: $e2) { *self = *self $o b; }
            }
        )?
    };
    (@common $t:ident, $e:ty $(, $e2:ty)?; $tr:ident $f:ident $atr:ident $af:ident $o:tt) => {
        impl $tr for $t {
            type Output = $t;
            #[inline(always)]
            fn $f(self, b: $t) -> $t { self.zip(b, |a, b| a $o b) }
        }
        impl $tr<$t> for $e {
            type Output = $t;
            #[inline(always)]
            fn $f(self, b: $t) -> $t { b.map(|x| self $o x) }
        }
        impl $atr for $t {
            #[inline(always)]
            fn $af(&mut self, b: $t) { *self = *self $o b; }
        }
        $(
            impl $tr<$t> for $e2 {
                type Output = $t;
                #[inline(always)]
                fn $f(self, b: $t) -> $t { b.map(|x| self $o x) }
            }
        )?
    };
}

// A lane vector meets a scalar vector: the scalar one converts to lanes, as the C++ implicit constructor does.
macro_rules! fvec_mixed_ops {
    ($v:ident, $s:ident) => {
        fvec_mixed_ops!(@op $v, $s; Add add AddAssign add_assign +);
        fvec_mixed_ops!(@op $v, $s; Sub sub SubAssign sub_assign -);
        fvec_mixed_ops!(@op $v, $s; Mul mul MulAssign mul_assign *);
        fvec_mixed_ops!(@op $v, $s; Div div DivAssign div_assign /);
        impl Mul<Float> for $s {
            type Output = $v;
            #[inline(always)]
            fn mul(self, b: Float) -> $v { $v::from(self) * b }
        }
        impl Mul<$s> for Float {
            type Output = $v;
            #[inline(always)]
            fn mul(self, b: $s) -> $v { self * $v::from(b) }
        }
    };
    (@op $v:ident, $s:ident; $tr:ident $f:ident $atr:ident $af:ident $o:tt) => {
        impl $tr<$s> for $v {
            type Output = $v;
            #[inline(always)]
            fn $f(self, b: $s) -> $v { self $o $v::from(b) }
        }
        impl $tr<$v> for $s {
            type Output = $v;
            #[inline(always)]
            fn $f(self, b: $v) -> $v { $v::from(self) $o b }
        }
        impl $atr<$s> for $v {
            #[inline(always)]
            fn $af(&mut self, b: $s) { *self = *self $o $v::from(b); }
        }
    };
}

fvec!(vec2, vec2_scalar, 2, [x: X = 0, y: Y = 1]);
fvec!(vec3, vec3_scalar, 3, [x: X = 0, y: Y = 1, z: Z = 2]);
fvec!(vec4, vec4_scalar, 4, [x: X = 0, y: Y = 1, z: Z = 2, w: W = 3]);

/// GLSL `mix(x, y, a)`: `(y - x) * a + x`, or a select for a `bvec` weight.
pub trait Mix<Y, A> {
    type Output;
    fn mix(self, y: Y, a: A) -> Self::Output;
}

#[inline(always)]
pub fn mix<T: Mix<Y, A>, Y, A>(x: T, y: Y, a: A) -> T::Output {
    x.mix(y, a)
}

impl Mix<Float, Float> for Float {
    type Output = Float;
    #[inline(always)]
    fn mix(self, y: Float, a: Float) -> Float {
        (y - self) * a + self
    }
}
impl Mix<Float, f32> for Float {
    type Output = Float;
    #[inline(always)]
    fn mix(self, y: Float, a: f32) -> Float {
        (y - self) * a + self
    }
}
impl Mix<f32, f32> for f32 {
    type Output = f32;
    #[inline(always)]
    fn mix(self, y: f32, a: f32) -> f32 {
        (y - self) * a + self
    }
}

impl vec2_scalar {
    #[inline(always)]
    pub fn lsel2(&mut self, c1: XYZW, c2: XYZW) -> vec2_scalar_ref<'_> {
        let mut r: [Option<&mut f32>; 2] = [Some(&mut self.x), Some(&mut self.y)];
        let x = r[c1 as usize].take().expect("lsel component");
        let y = r[c2 as usize].take().expect("lsel component");
        vec2_scalar_ref { x, y }
    }
}

impl vec3_scalar {
    #[inline(always)]
    pub fn lsel2(&mut self, c1: XYZW, c2: XYZW) -> vec2_scalar_ref<'_> {
        let mut r: [Option<&mut f32>; 3] = [Some(&mut self.x), Some(&mut self.y), Some(&mut self.z)];
        let x = r[c1 as usize].take().expect("lsel component");
        let y = r[c2 as usize].take().expect("lsel component");
        vec2_scalar_ref { x, y }
    }
}

impl vec4_scalar {
    #[inline(always)]
    pub fn from_vec3(xyz: vec3_scalar, w: f32) -> Self {
        Self::new(xyz.x, xyz.y, xyz.z, w)
    }
    /// `load_from_ptr`: four consecutive floats.
    #[inline(always)]
    pub fn load_from_ptr(f: &[f32]) -> Self {
        Self::new(f[0], f[1], f[2], f[3])
    }
    fn refs(&mut self) -> [Option<&mut f32>; 4] {
        [Some(&mut self.x), Some(&mut self.y), Some(&mut self.z), Some(&mut self.w)]
    }
    #[inline(always)]
    pub fn lsel2(&mut self, c1: XYZW, c2: XYZW) -> vec2_scalar_ref<'_> {
        let mut r = self.refs();
        let x = r[c1 as usize].take().expect("lsel component");
        let y = r[c2 as usize].take().expect("lsel component");
        vec2_scalar_ref { x, y }
    }
    #[inline(always)]
    pub fn lsel3(&mut self, c1: XYZW, c2: XYZW, c3: XYZW) -> vec3_scalar_ref<'_> {
        let mut r = self.refs();
        let x = r[c1 as usize].take().expect("lsel component");
        let y = r[c2 as usize].take().expect("lsel component");
        let z = r[c3 as usize].take().expect("lsel component");
        vec3_scalar_ref { x, y, z }
    }
}

impl vec2 {
    /// The explicit C++ `vec2(ivec2)`.
    #[inline(always)]
    pub fn from_ivec2(a: ivec2) -> Self {
        Self::new(cast(a.x), cast(a.y))
    }
}

impl vec3 {
    #[inline(always)]
    pub fn from_vec2(a: vec2, z: Float) -> Self {
        Self::new(a.x, a.y, z)
    }
    /// The explicit C++ `vec3(vec4)`: drops `w`.
    #[inline(always)]
    pub fn from_vec4(v: vec4) -> Self {
        Self::new(v.x, v.y, v.z)
    }
}

impl vec4 {
    #[inline(always)]
    pub fn from_vec3(xyz: vec3, w: Float) -> Self {
        Self::new(xyz.x, xyz.y, xyz.z, w)
    }
    #[inline(always)]
    pub fn from_vec2s(xy: vec2, zw: vec2) -> Self {
        Self::new(xy.x, xy.y, zw.x, zw.y)
    }
    #[inline(always)]
    pub fn from_vec2_zw(xy: vec2, z: Float, w: Float) -> Self {
        Self::new(xy.x, xy.y, z, w)
    }
    #[inline(always)]
    pub fn from_xy_vec2(x: Float, y: Float, zw: vec2) -> Self {
        Self::new(x, y, zw.x, zw.y)
    }
    #[inline(always)]
    pub fn lsel4(&mut self, c1: XYZW, c2: XYZW, c3: XYZW, c4: XYZW) -> vec4_ref<'_> {
        let mut r = self.refs();
        let x = r[c1 as usize].take().expect("lsel component");
        let y = r[c2 as usize].take().expect("lsel component");
        let z = r[c3 as usize].take().expect("lsel component");
        let w = r[c4 as usize].take().expect("lsel component");
        vec4_ref { x, y, z, w }
    }
    /// GLSL's non-constant indexing `v[i]` with a lane index: lane `k` takes component `index[k]` of lane `k`,
    /// and 0 where the index is outside 0..4.
    #[inline(always)]
    pub fn index_lanes(&self, index: I32) -> Float {
        let mut ret = Float::splat(0.0);
        for k in 0..4 {
            ret[k] = match index[k] {
                0 => self.x[k],
                1 => self.y[k],
                2 => self.z[k],
                3 => self.w[k],
                _ => 0.0,
            };
        }
        ret
    }
}

// ---- the _ref l-value swizzles ------------------------------------------------------------------------------------

/// `vec2_scalar_ref`: two components of a scalar vector, assigned through.
pub struct vec2_scalar_ref<'a> {
    pub x: &'a mut f32,
    pub y: &'a mut f32,
}

impl vec2_scalar_ref<'_> {
    #[inline(always)]
    pub fn assign(&mut self, a: vec2_scalar) {
        *self.x = a.x;
        *self.y = a.y;
    }
    #[inline(always)]
    pub fn mul_assign(&mut self, a: vec2_scalar) {
        *self.x *= a.x;
        *self.y *= a.y;
    }
    #[inline(always)]
    pub fn get(&self) -> vec2_scalar {
        vec2_scalar::new(*self.x, *self.y)
    }
}

/// `vec3_scalar_ref`.
pub struct vec3_scalar_ref<'a> {
    pub x: &'a mut f32,
    pub y: &'a mut f32,
    pub z: &'a mut f32,
}

impl vec3_scalar_ref<'_> {
    #[inline(always)]
    pub fn assign(&mut self, a: vec3_scalar) {
        *self.x = a.x;
        *self.y = a.y;
        *self.z = a.z;
    }
    #[inline(always)]
    pub fn get(&self) -> vec3_scalar {
        vec3_scalar::new(*self.x, *self.y, *self.z)
    }
}

/// `vec2_ref`: two components of a lane vector, assigned through.
pub struct vec2_ref<'a> {
    pub x: &'a mut Float,
    pub y: &'a mut Float,
}

impl vec2_ref<'_> {
    #[inline(always)]
    pub fn assign(&mut self, a: vec2) {
        *self.x = a.x;
        *self.y = a.y;
    }
    #[inline(always)]
    pub fn div_assign_float(&mut self, a: Float) {
        *self.x /= a;
        *self.y /= a;
    }
    #[inline(always)]
    pub fn div_assign(&mut self, a: vec2) {
        *self.x /= a.x;
        *self.y /= a.y;
    }
    #[inline(always)]
    pub fn add_assign(&mut self, a: vec2) {
        *self.x += a.x;
        *self.y += a.y;
    }
    #[inline(always)]
    pub fn sub_assign(&mut self, a: vec2) {
        *self.x -= a.x;
        *self.y -= a.y;
    }
    #[inline(always)]
    pub fn mul_assign(&mut self, a: vec2) {
        *self.x *= a.x;
        *self.y *= a.y;
    }
    #[inline(always)]
    pub fn get(&self) -> vec2 {
        vec2::new(*self.x, *self.y)
    }
}

/// `vec3_ref`.
pub struct vec3_ref<'a> {
    pub x: &'a mut Float,
    pub y: &'a mut Float,
    pub z: &'a mut Float,
}

impl vec3_ref<'_> {
    #[inline(always)]
    pub fn assign(&mut self, a: vec3) {
        *self.x = a.x;
        *self.y = a.y;
        *self.z = a.z;
    }
    #[inline(always)]
    pub fn div_assign_float(&mut self, a: Float) {
        *self.x /= a;
        *self.y /= a;
        *self.z /= a;
    }
    #[inline(always)]
    pub fn mul_assign_float(&mut self, a: Float) {
        *self.x *= a;
        *self.y *= a;
        *self.z *= a;
    }
    #[inline(always)]
    pub fn get(&self) -> vec3 {
        vec3::new(*self.x, *self.y, *self.z)
    }
}

/// `vec4_ref`.
pub struct vec4_ref<'a> {
    pub x: &'a mut Float,
    pub y: &'a mut Float,
    pub z: &'a mut Float,
    pub w: &'a mut Float,
}

impl vec4_ref<'_> {
    #[inline(always)]
    pub fn assign(&mut self, a: vec4) {
        *self.x = a.x;
        *self.y = a.y;
        *self.z = a.z;
        *self.w = a.w;
    }
    #[inline(always)]
    pub fn get(&self) -> vec4 {
        vec4::new(*self.x, *self.y, *self.z, *self.w)
    }
}

// ---- built-ins on float vectors (the overload set glsl.h defines) -------------------------------------------------

macro_rules! componentwise_min_max {
    ($($t:ident: $e:ty),*) => {$(
        impl Min for $t {
            type Output = $t;
            #[inline(always)]
            fn min(self, b: $t) -> $t { self.zip(b, min) }
        }
        impl Max for $t {
            type Output = $t;
            #[inline(always)]
            fn max(self, b: $t) -> $t { self.zip(b, max) }
        }
    )*};
}
componentwise_min_max!(vec2: Float, vec2_scalar: f32, vec3: Float, vec3_scalar: f32);

macro_rules! lane_min_max_float {
    ($($t:ident),*) => {$(
        impl Min<Float> for $t {
            type Output = $t;
            #[inline(always)]
            fn min(self, b: Float) -> $t { self.map(|a| min(a, b)) }
        }
        impl Max<Float> for $t {
            type Output = $t;
            #[inline(always)]
            fn max(self, b: Float) -> $t { self.map(|a| max(a, b)) }
        }
    )*};
}
lane_min_max_float!(vec2, vec3);

impl Max<f32> for vec2_scalar {
    type Output = vec2_scalar;
    #[inline(always)]
    fn max(self, b: f32) -> vec2_scalar {
        self.map(|a| max(a, b))
    }
}

macro_rules! clamp_impls {
    (lanes: $($t:ident),*) => {$(
        impl Clamp<Float, Float> for $t {
            type Output = $t;
            #[inline(always)]
            fn clamp(self, lo: Float, hi: Float) -> $t { self.map(|a| clamp(a, lo, hi)) }
        }
        impl Clamp for $t {
            type Output = $t;
            #[inline(always)]
            fn clamp(self, lo: $t, hi: $t) -> $t { self.zip3(lo, hi, clamp) }
        }
    )*};
    (scalars: $($t:ident),*) => {$(
        impl Clamp<f32, f32> for $t {
            type Output = $t;
            #[inline(always)]
            fn clamp(self, lo: f32, hi: f32) -> $t { self.map(|a| clamp(a, lo, hi)) }
        }
        impl Clamp for $t {
            type Output = $t;
            #[inline(always)]
            fn clamp(self, lo: $t, hi: $t) -> $t { self.zip3(lo, hi, clamp) }
        }
    )*};
}
clamp_impls!(lanes: vec2, vec3, vec4);
clamp_impls!(scalars: vec2_scalar, vec4_scalar);

macro_rules! componentwise_unary {
    ($tr:ident, $f:ident: $($t:ident),*) => {$(
        impl $tr for $t {
            #[inline(always)]
            fn $f(self) -> $t { self.map($f) }
        }
    )*};
}
componentwise_unary!(Sqrt, sqrt: vec2, vec2_scalar);
componentwise_unary!(Recip, recip: vec2, vec2_scalar);
componentwise_unary!(InverseSqrt, inversesqrt: vec2, vec2_scalar);
componentwise_unary!(Abs, abs: vec2, vec2_scalar, vec3);
componentwise_unary!(Sign, sign: vec2, vec2_scalar, vec3);
componentwise_unary!(Fract, fract: vec2, vec2_scalar);

impl Floor for vec2_scalar {
    #[inline(always)]
    fn floor(self) -> vec2_scalar {
        self.map(f32::floor)
    }
}

impl Pow for vec2_scalar {
    type Output = vec2_scalar;
    #[inline(always)]
    fn pow(self, y: vec2_scalar) -> vec2_scalar {
        self.zip(y, pow)
    }
}

macro_rules! lane_pow {
    ($($t:ident),*) => {$(
        impl Pow for $t {
            type Output = $t;
            #[inline(always)]
            fn pow(self, y: $t) -> $t { self.zip(y, pow) }
        }
    )*};
}
lane_pow!(vec2, vec3);

impl Mod for vec2 {
    #[inline(always)]
    fn r#mod(self, b: vec2) -> vec2 {
        self.zip(b, r#mod)
    }
}

impl Length for vec2 {
    type Output = Float;
    #[inline(always)]
    fn length(self) -> Float {
        sqrt(self.x * self.x + self.y * self.y)
    }
}

impl Length for vec2_scalar {
    type Output = f32;
    /// `hypotf(x, y)`.
    #[inline(always)]
    fn length(self) -> f32 {
        self.x.hypot(self.y)
    }
}

impl Dot for vec2 {
    type Output = Float;
    #[inline(always)]
    fn dot(self, b: vec2) -> Float {
        self.x * b.x + self.y * b.y
    }
}
impl Dot for vec2_scalar {
    type Output = f32;
    #[inline(always)]
    fn dot(self, b: vec2_scalar) -> f32 {
        self.x * b.x + self.y * b.y
    }
}
impl Dot for vec3 {
    type Output = Float;
    #[inline(always)]
    fn dot(self, b: vec3) -> Float {
        self.x * b.x + self.y * b.y + self.z * b.z
    }
}
impl Dot for vec3_scalar {
    type Output = f32;
    #[inline(always)]
    fn dot(self, b: vec3_scalar) -> f32 {
        self.x * b.x + self.y * b.y + self.z * b.z
    }
}

impl RoundFast for vec4 {
    type Output = ivec4;
    #[inline(always)]
    fn roundfast(self, scale: Float) -> ivec4 {
        ivec4::new(
            roundfast(self.x, scale),
            roundfast(self.y, scale),
            roundfast(self.z, scale),
            roundfast(self.w, scale),
        )
    }
}

macro_rules! vec_element_type {
    ($($t:ty => $e:ty),*) => {$(impl ElementType for $t { type ty = $e; })*};
}
vec_element_type!(vec2 => f32, vec2_scalar => f32, vec3 => f32, vec3_scalar => f32, vec4 => f32, vec4_scalar => f32,
    ivec2 => i32, ivec2_scalar => i32, ivec4 => i32, ivec4_scalar => i32);

// ---- comparisons: lessThan & co ---------------------------------------------------------------------------------

macro_rules! cmp_fn {
    ($tr:ident, $f:ident, $cmp:ident, $op:tt) => {
        pub trait $tr {
            type Output;
            fn $f(self, y: Self) -> Self::Output;
        }
        #[inline(always)]
        pub fn $f<T: $tr>(x: T, y: T) -> T::Output {
            x.$f(y)
        }
        impl $tr for f32 {
            type Output = bool;
            #[inline(always)]
            fn $f(self, y: f32) -> bool { self $op y }
        }
        impl $tr for i32 {
            type Output = bool;
            #[inline(always)]
            fn $f(self, y: i32) -> bool { self $op y }
        }
        impl $tr for Float {
            type Output = I32;
            #[inline(always)]
            fn $f(self, y: Float) -> I32 { self.$cmp(y) }
        }
        impl $tr for I32 {
            type Output = I32;
            #[inline(always)]
            fn $f(self, y: I32) -> I32 { self.$cmp(y) }
        }
    };
}
cmp_fn!(LessThanEqual, lessThanEqual, cmp_le, <=);
cmp_fn!(LessThan, lessThan, cmp_lt, <);
cmp_fn!(GreaterThan, greaterThan, cmp_gt, >);
cmp_fn!(GreaterThanEqual, greaterThanEqual, cmp_ge, >=);
cmp_fn!(Equal, equal, cmp_eq, ==);
cmp_fn!(NotEqual, notEqual, cmp_ne, !=);

macro_rules! cmp_vec {
    ($tr:ident, $f:ident: $($v:ident => $b:ident [$($c:ident),+]),*) => {$(
        impl $tr for $v {
            type Output = $b;
            #[inline(always)]
            fn $f(self, y: $v) -> $b { $b { $($c: $f(self.$c, y.$c)),+ } }
        }
    )*};
}
cmp_vec!(LessThanEqual, lessThanEqual: vec3 => bvec3 [x, y, z], vec2 => bvec2 [x, y], vec2_scalar => bvec2_scalar [x, y],
    vec4 => bvec4 [x, y, z, w], vec4_scalar => bvec4_scalar [x, y, z, w]);
cmp_vec!(LessThan, lessThan: vec2 => bvec2 [x, y], vec2_scalar => bvec2_scalar [x, y], vec4 => bvec4 [x, y, z, w],
    vec4_scalar => bvec4_scalar [x, y, z, w]);
cmp_vec!(GreaterThan, greaterThan: vec2 => bvec2 [x, y], vec2_scalar => bvec2_scalar [x, y], vec4 => bvec4 [x, y, z, w],
    vec4_scalar => bvec4_scalar [x, y, z, w]);
cmp_vec!(GreaterThanEqual, greaterThanEqual: vec4 => bvec4 [x, y, z, w]);
cmp_vec!(Equal, equal: vec2 => bvec2 [x, y], vec2_scalar => bvec2_scalar [x, y], vec4 => bvec4 [x, y, z, w],
    vec4_scalar => bvec4_scalar [x, y, z, w]);
cmp_vec!(NotEqual, notEqual: vec2 => bvec2 [x, y], vec2_scalar => bvec2_scalar [x, y], vec4 => bvec4 [x, y, z, w],
    vec4_scalar => bvec4_scalar [x, y, z, w], ivec4 => bvec4 [x, y, z, w], ivec4_scalar => bvec4_scalar [x, y, z, w]);

// ---- bool vectors -----------------------------------------------------------------------------------------------

macro_rules! bvec {
    ($v:ident, $s:ident, $s1:ident, $n:literal, [$($c:ident: $C:ident),+]) => {
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $v { $(pub $c: Bool),+ }

        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct $s { $(pub $c: bool),+ }

        /// The one-`bool` form `make_bvecN(bool)` returns; it converts to the full scalar vector.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct $s1 { pub x: bool }

        impl From<$s1> for $s {
            #[inline(always)]
            fn from(a: $s1) -> $s { $s::splat(a.x) }
        }
        impl From<bool> for $s1 {
            #[inline(always)]
            fn from(a: bool) -> $s1 { $s1 { x: a } }
        }
        impl From<bool> for $s {
            #[inline(always)]
            fn from(a: bool) -> $s { $s::splat(a) }
        }
        impl From<Bool> for $v {
            #[inline(always)]
            fn from(a: Bool) -> $v { $v::splat(a) }
        }

        impl $s {
            #[inline(always)]
            pub const fn new($($c: bool),+) -> Self { Self { $($c),+ } }
            #[inline(always)]
            pub const fn splat(a: bool) -> Self { Self { $($c: a),+ } }
            #[inline(always)]
            pub fn select(&self, c: XYZW) -> bool {
                match c { $(XYZW::$C => self.$c,)+ #[allow(unreachable_patterns)] _ => none(c, $n) }
            }
            #[inline(always)]
            pub fn sel(&self, c1: XYZW) -> bool { self.select(c1) }
            #[inline(always)]
            pub fn sel2(&self, c1: XYZW, c2: XYZW) -> bvec2_scalar { bvec2_scalar::new(self.select(c1), self.select(c2)) }
        }

        impl $v {
            #[inline(always)]
            pub fn new($($c: Bool),+) -> Self { Self { $($c),+ } }
            #[inline(always)]
            pub fn splat(a: Bool) -> Self { Self { $($c: a),+ } }
            #[inline(always)]
            pub fn select(&self, c: XYZW) -> Bool {
                match c { $(XYZW::$C => self.$c,)+ #[allow(unreachable_patterns)] _ => none(c, $n) }
            }
            #[inline(always)]
            pub fn select_mut(&mut self, c: XYZW) -> &mut Bool {
                match c { $(XYZW::$C => &mut self.$c,)+ #[allow(unreachable_patterns)] _ => none(c, $n) }
            }
            #[inline(always)]
            pub fn sel(&self, c1: XYZW) -> Bool { self.select(c1) }
        }

        impl core::ops::Not for $v {
            type Output = $v;
            /// `~v` per component.
            #[inline(always)]
            fn not(self) -> $v { $v { $($c: !self.$c),+ } }
        }
    };
}
bvec!(bvec2, bvec2_scalar, bvec2_scalar1, 2, [x: X, y: Y]);
bvec!(bvec3, bvec3_scalar, bvec3_scalar1, 3, [x: X, y: Y, z: Z]);
bvec!(bvec4, bvec4_scalar, bvec4_scalar1, 4, [x: X, y: Y, z: Z, w: W]);

impl bvec2 {
    #[inline(always)]
    pub fn sel2(&self, c1: XYZW, c2: XYZW) -> bvec2 {
        bvec2::new(self.select(c1), self.select(c2))
    }
}

impl bvec2_scalar {
    #[inline(always)]
    pub fn sel4(&self, c1: XYZW, c2: XYZW, c3: XYZW, c4: XYZW) -> bvec4_scalar {
        bvec4_scalar::new(self.select(c1), self.select(c2), self.select(c3), self.select(c4))
    }
}

impl bvec4 {
    #[inline(always)]
    pub fn from_bvec2s(x: bvec2, y: bvec2) -> Self {
        Self::new(x.x, x.y, y.x, y.y)
    }
}

impl Any for bvec4 {
    type Output = Bool;
    #[inline(always)]
    fn any(self) -> Bool {
        self.x | self.y | self.z | self.w
    }
}
impl Any for bvec4_scalar {
    type Output = bool;
    #[inline(always)]
    fn any(self) -> bool {
        self.x | self.y | self.z | self.w
    }
}
impl Any for bvec2 {
    type Output = Bool;
    #[inline(always)]
    fn any(self) -> Bool {
        self.x | self.y
    }
}
impl Any for bvec2_scalar {
    type Output = bool;
    #[inline(always)]
    fn any(self) -> bool {
        self.x | self.y
    }
}
impl All for bvec2 {
    type Output = Bool;
    #[inline(always)]
    fn all(self) -> Bool {
        self.x & self.y
    }
}
impl All for bvec2_scalar {
    type Output = bool;
    #[inline(always)]
    fn all(self) -> bool {
        self.x & self.y
    }
}
impl All for bvec4 {
    type Output = Bool;
    #[inline(always)]
    fn all(self) -> Bool {
        self.x & self.y & self.z & self.w
    }
}
impl All for bvec4_scalar {
    type Output = bool;
    #[inline(always)]
    fn all(self) -> bool {
        self.x & self.y & self.z & self.w
    }
}

macro_rules! bvec_select {
    ($($b:ident, $bs:ident, $bs1:ident => $v:ident, $s:ident [$($c:ident),+]);*) => {$(
        impl IfThenElse<$v> for $b {
            type Output = $v;
            #[inline(always)]
            fn if_then_else(self, t: $v, e: $v) -> $v { $v { $($c: if_then_else(self.$c, t.$c, e.$c)),+ } }
        }
        /// `mix(x, y, bvecN)` = `if_then_else(a, y, x)`.
        impl Mix<$v, $b> for $v {
            type Output = $v;
            #[inline(always)]
            fn mix(self, y: $v, a: $b) -> $v { if_then_else(a, y, self) }
        }
        impl Mix<$s, $b> for $s {
            type Output = $v;
            #[inline(always)]
            fn mix(self, y: $s, a: $b) -> $v { if_then_else(a, $v::from(y), $v::from(self)) }
        }
        impl Mix<$v, $bs> for $v {
            type Output = $v;
            #[inline(always)]
            fn mix(self, y: $v, a: $bs) -> $v { $v { $($c: if a.$c { y.$c } else { self.$c }),+ } }
        }
        impl Mix<$s, $bs> for $s {
            type Output = $s;
            #[inline(always)]
            fn mix(self, y: $s, a: $bs) -> $s { $s { $($c: if a.$c { y.$c } else { self.$c }),+ } }
        }
        impl Mix<$v, $bs1> for $v {
            type Output = $v;
            #[inline(always)]
            fn mix(self, y: $v, a: $bs1) -> $v { if a.x { y } else { self } }
        }
        impl Mix<$s, $bs1> for $s {
            type Output = $s;
            #[inline(always)]
            fn mix(self, y: $s, a: $bs1) -> $s { if a.x { y } else { self } }
        }
    )*};
}
bvec_select!(bvec2, bvec2_scalar, bvec2_scalar1 => vec2, vec2_scalar [x, y];
    bvec3, bvec3_scalar, bvec3_scalar1 => vec3, vec3_scalar [x, y, z];
    bvec4, bvec4_scalar, bvec4_scalar1 => vec4, vec4_scalar [x, y, z, w]);

// ---- integer vectors --------------------------------------------------------------------------------------------

macro_rules! ivec {
    ($v:ident, $s:ident, $n:literal, [$($c:ident: $C:ident),+]) => {
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $v { $(pub $c: I32),+ }

        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
        pub struct $s { $(pub $c: i32),+ }

        impl $s {
            #[inline(always)]
            pub const fn new($($c: i32),+) -> Self { Self { $($c),+ } }
            #[inline(always)]
            pub const fn splat(a: i32) -> Self { Self { $($c: a),+ } }
            #[inline(always)]
            pub fn select(&self, c: XYZW) -> i32 {
                match c { $(XYZW::$C => self.$c,)+ #[allow(unreachable_patterns)] _ => none(c, $n) }
            }
            #[inline(always)]
            pub fn select_mut(&mut self, c: XYZW) -> &mut i32 {
                match c { $(XYZW::$C => &mut self.$c,)+ #[allow(unreachable_patterns)] _ => none(c, $n) }
            }
            #[inline(always)]
            pub fn sel(&self, c1: XYZW) -> i32 { self.select(c1) }
            #[inline(always)]
            pub fn sel2(&self, c1: XYZW, c2: XYZW) -> ivec2_scalar { ivec2_scalar::new(self.select(c1), self.select(c2)) }
            /// Component `n` (0-based), the C++ `operator[]`.
            #[inline(always)]
            pub fn get(&self, n: usize) -> i32 { [$(self.$c),+][n] }
        }

        impl $v {
            #[inline(always)]
            pub fn new($($c: I32),+) -> Self { Self { $($c),+ } }
            #[inline(always)]
            pub fn splat(a: I32) -> Self { Self { $($c: a),+ } }
            #[inline(always)]
            pub fn from_scalars(s0: $s, s1: $s, s2: $s, s3: $s) -> Self {
                Self { $($c: I32::new([s0.$c, s1.$c, s2.$c, s3.$c])),+ }
            }
            #[inline(always)]
            pub fn select(&self, c: XYZW) -> I32 {
                match c { $(XYZW::$C => self.$c,)+ #[allow(unreachable_patterns)] _ => none(c, $n) }
            }
            #[inline(always)]
            pub fn select_mut(&mut self, c: XYZW) -> &mut I32 {
                match c { $(XYZW::$C => &mut self.$c,)+ #[allow(unreachable_patterns)] _ => none(c, $n) }
            }
            #[inline(always)]
            pub fn sel(&self, c1: XYZW) -> I32 { self.select(c1) }
            #[inline(always)]
            fn zip(self, b: Self, f: impl Fn(I32, I32) -> I32) -> Self { Self { $($c: f(self.$c, b.$c)),+ } }
            #[inline(always)]
            fn map(self, f: impl Fn(I32) -> I32) -> Self { Self { $($c: f(self.$c)),+ } }
        }

        impl From<$s> for $v {
            #[inline(always)]
            fn from(s: $s) -> Self { Self { $($c: I32::splat(s.$c)),+ } }
        }
        impl From<I32> for $v {
            #[inline(always)]
            fn from(a: I32) -> Self { Self::splat(a) }
        }
        impl From<i32> for $s {
            #[inline(always)]
            fn from(a: i32) -> Self { Self::splat(a) }
        }

        impl Add for $v {
            type Output = $v;
            #[inline(always)]
            fn add(self, b: $v) -> $v { self.zip(b, |a, b| a + b) }
        }
        impl Sub for $v {
            type Output = $v;
            #[inline(always)]
            fn sub(self, b: $v) -> $v { self.zip(b, |a, b| a - b) }
        }
        impl Mul<I32> for $v {
            type Output = $v;
            #[inline(always)]
            fn mul(self, b: I32) -> $v { self.map(|a| a * b) }
        }
        impl core::ops::BitAnd for $v {
            type Output = $v;
            #[inline(always)]
            fn bitand(self, b: $v) -> $v { self.zip(b, |a, b| a & b) }
        }
        impl core::ops::BitAnd<I32> for $v {
            type Output = $v;
            #[inline(always)]
            fn bitand(self, b: I32) -> $v { self.map(|a| a & b) }
        }
        impl core::ops::BitAnd<$v> for I32 {
            type Output = $v;
            #[inline(always)]
            fn bitand(self, b: $v) -> $v { b.map(|a| self & a) }
        }
        impl core::ops::BitAnd<$s> for I32 {
            type Output = $v;
            #[inline(always)]
            fn bitand(self, b: $s) -> $v { $v::from(b).map(|a| self & a) }
        }
        impl AddAssign for $v {
            #[inline(always)]
            fn add_assign(&mut self, b: $v) { *self = *self + b; }
        }
        impl MulAssign<I32> for $v {
            #[inline(always)]
            fn mul_assign(&mut self, b: I32) { *self = *self * b; }
        }
        impl core::ops::ShrAssign<i32> for $v {
            #[inline(always)]
            fn shr_assign(&mut self, shift: i32) { *self = self.map(|a| a >> shift); }
        }

        impl Neg for $s {
            type Output = $s;
            #[inline(always)]
            fn neg(self) -> $s { $s { $($c: self.$c.wrapping_neg()),+ } }
        }
        impl Add for $s {
            type Output = $s;
            #[inline(always)]
            fn add(self, b: $s) -> $s { $s { $($c: self.$c.wrapping_add(b.$c)),+ } }
        }
        impl Add<i32> for $s {
            type Output = $s;
            #[inline(always)]
            fn add(self, b: i32) -> $s { $s { $($c: self.$c.wrapping_add(b)),+ } }
        }
        impl Sub for $s {
            type Output = $s;
            #[inline(always)]
            fn sub(self, b: $s) -> $s { $s { $($c: self.$c.wrapping_sub(b.$c)),+ } }
        }
        impl Sub<i32> for $s {
            type Output = $s;
            #[inline(always)]
            fn sub(self, b: i32) -> $s { $s { $($c: self.$c.wrapping_sub(b)),+ } }
        }
        impl AddAssign for $s {
            #[inline(always)]
            fn add_assign(&mut self, b: $s) { *self = *self + b; }
        }
        impl AddAssign<i32> for $s {
            #[inline(always)]
            fn add_assign(&mut self, b: i32) { *self = *self + b; }
        }
        impl core::ops::ShrAssign<i32> for $s {
            #[inline(always)]
            fn shr_assign(&mut self, shift: i32) { $(self.$c = self.$c.wrapping_shr(shift as u32);)+ }
        }
        impl core::ops::Shl<i32> for $s {
            type Output = $s;
            #[inline(always)]
            fn shl(self, b: i32) -> $s { $s { $($c: self.$c.wrapping_shl(b as u32)),+ } }
        }
        impl core::ops::BitAnd<i32> for $s {
            type Output = $s;
            #[inline(always)]
            fn bitand(self, b: i32) -> $s { $s { $($c: self.$c & b),+ } }
        }
        impl core::ops::BitAnd<$s> for i32 {
            type Output = $s;
            #[inline(always)]
            fn bitand(self, b: $s) -> $s { $s { $($c: self & b.$c),+ } }
        }

        impl IfThenElse<$v> for I32 {
            type Output = $v;
            #[inline(always)]
            fn if_then_else(self, t: $v, e: $v) -> $v { t.zip(e, |a, b| if_then_else(self, a, b)) }
        }
        impl ForceScalar for $v {
            type Output = $s;
            #[inline(always)]
            fn force_scalar(self) -> $s { $s { $($c: self.$c[0]),+ } }
        }
        impl GetNth for $v {
            type Output = $s;
            #[inline(always)]
            fn get_nth(self, n: usize) -> $s { $s { $($c: self.$c[n]),+ } }
        }
        impl PutNth<$s> for $v {
            #[inline(always)]
            fn put_nth(&mut self, n: usize, src: $s) { $(self.$c[n] = src.$c;)+ }
        }
        impl PutNthComponent<i32> for $s {
            #[inline(always)]
            fn put_nth_component(&mut self, n: usize, src: i32) {
                let mut i = 0usize;
                $(if i == n { self.$c = src; } i += 1;)+
                let _ = i;
            }
        }
    };
}
ivec!(ivec2, ivec2_scalar, 2, [x: X, y: Y]);
ivec!(ivec3, ivec3_scalar, 3, [x: X, y: Y, z: Z]);
ivec!(ivec4, ivec4_scalar, 4, [x: X, y: Y, z: Z, w: W]);

impl ivec2 {
    /// The C++ implicit `ivec2(vec2)`: `cast` (truncating conversion) per component.
    #[inline(always)]
    pub fn from_vec2(a: vec2) -> Self {
        Self::new(cast(a.x), cast(a.y))
    }
    /// `ivec2(U32, U32)`: `CONVERT` to `I32`.
    #[inline(always)]
    pub fn from_u32(x: U32, y: U32) -> Self {
        Self::new(x.convert::<i32>(), y.convert::<i32>())
    }
    #[inline(always)]
    pub fn sel2(&self, c1: XYZW, c2: XYZW) -> ivec2 {
        ivec2::new(self.select(c1), self.select(c2))
    }
    #[inline(always)]
    pub fn sel4(&self, c1: XYZW, c2: XYZW, c3: XYZW, c4: XYZW) -> ivec4 {
        ivec4::new(self.select(c1), self.select(c2), self.select(c3), self.select(c4))
    }
}

impl From<vec2> for ivec2 {
    #[inline(always)]
    fn from(a: vec2) -> Self {
        Self::from_vec2(a)
    }
}

impl ivec3 {
    #[inline(always)]
    pub fn from_ivec2(a: ivec2, b: I32) -> Self {
        Self::new(a.x, a.y, b)
    }
    #[inline(always)]
    pub fn from_vec2(a: vec2, b: Float) -> Self {
        Self::new(cast(a.x), cast(a.y), cast(b))
    }
}

impl ivec4 {
    #[inline(always)]
    pub fn from_ivec2(a: ivec2, b: I32, c: I32) -> Self {
        Self::new(a.x, a.y, b, c)
    }
    #[inline(always)]
    pub fn sel2(&self, c1: XYZW, c2: XYZW) -> ivec2 {
        ivec2::new(self.select(c1), self.select(c2))
    }
    #[inline(always)]
    pub fn sel3(&self, c1: XYZW, c2: XYZW, c3: XYZW) -> ivec3 {
        ivec3::new(self.select(c1), self.select(c2), self.select(c3))
    }
}

impl IfThenElse<vec3> for ivec3 {
    type Output = vec3;
    #[inline(always)]
    fn if_then_else(self, t: vec3, e: vec3) -> vec3 {
        vec3::new(if_then_else(self.x, t.x, e.x), if_then_else(self.y, t.y, e.y), if_then_else(self.z, t.z, e.z))
    }
}

// ---- make_* constructors ----------------------------------------------------------------------------------------

macro_rules! maker {
    ($tr:ident, $f:ident { $($arg:ty => $out:ty = |$p:pat_param| $e:expr;)* }) => {
        pub trait $tr {
            type Output;
            fn $f(self) -> Self::Output;
        }
        #[inline(always)]
        pub fn $f<T: $tr>(args: T) -> T::Output {
            args.$f()
        }
        $(
            impl $tr for $arg {
                type Output = $out;
                #[inline(always)]
                fn $f(self) -> $out { let $p = self; $e }
            }
        )*
    };
}

maker!(MakeBvec2, make_bvec2 {
    bool => bvec2_scalar1 = |n| bvec2_scalar1 { x: n };
    (bool, bool) => bvec2_scalar = |(x, y)| bvec2_scalar::new(x, y);
    Bool => bvec2 = |n| bvec2::splat(n);
    (Bool, Bool) => bvec2 = |(x, y)| bvec2::new(x, y);
});

maker!(MakeBvec3, make_bvec3 {
    bool => bvec3_scalar1 = |n| bvec3_scalar1 { x: n };
});

maker!(MakeBvec4, make_bvec4 {
    bool => bvec4_scalar1 = |n| bvec4_scalar1 { x: n };
    (bool, bool, bool, bool) => bvec4_scalar = |(x, y, z, w)| bvec4_scalar::new(x, y, z, w);
    (bvec2_scalar, bvec2_scalar) => bvec4_scalar = |(a, b)| bvec4_scalar::new(a.x, a.y, b.x, b.y);
    Bool => bvec4 = |n| bvec4::splat(n);
    (bvec2, bvec2) => bvec4 = |(x, y)| bvec4::from_bvec2s(x, y);
    (Bool, Bool, Bool, Bool) => bvec4 = |(x, y, z, w)| bvec4::new(x, y, z, w);
});

maker!(MakeVec2, make_vec2 {
    f32 => vec2_scalar = |n| vec2_scalar::new(n, n);
    (f32, f32) => vec2_scalar = |(x, y)| vec2_scalar::new(x, y);
    (i32, i32) => vec2_scalar = |(x, y)| vec2_scalar::new(x as f32, y as f32);
    ivec2_scalar => vec2_scalar = |v| vec2_scalar::new(v.x as f32, v.y as f32);
    ivec3_scalar => vec2_scalar = |s| vec2_scalar::new(s.x as f32, s.y as f32);
    Float => vec2 = |n| vec2::splat(n);
    vec2_scalar => vec2 = |n| vec2::from(n);
    vec2 => vec2 = |n| n;
    ivec2 => vec2 = |n| vec2::from_ivec2(n);
    (Float, Float) => vec2 = |(x, y)| vec2::new(x, y);
});

maker!(MakeIvec2, make_ivec2 {
    i32 => ivec2_scalar = |n| ivec2_scalar::new(n, n);
    u32 => ivec2_scalar = |n| ivec2_scalar::new(n as i32, n as i32);
    (i32, i32) => ivec2_scalar = |(x, y)| ivec2_scalar::new(x, y);
    (u32, u32) => ivec2_scalar = |(x, y)| ivec2_scalar::new(x as i32, y as i32);
    vec2_scalar => ivec2_scalar = |v| ivec2_scalar::new(f32_to_i32_scalar(v.x), f32_to_i32_scalar(v.y));
    I32 => ivec2 = |n| ivec2::splat(n);
    vec2 => ivec2 = |n| ivec2::from_vec2(n);
    ivec2_scalar => ivec2 = |n| ivec2::from(n);
    ivec2 => ivec2 = |n| n;
    (I32, I32) => ivec2 = |(x, y)| ivec2::new(x, y);
    (U32, U32) => ivec2 = |(x, y)| ivec2::from_u32(x, y);
});

maker!(MakeIvec3, make_ivec3 {
    i32 => ivec3_scalar = |n| ivec3_scalar::new(n, n, n);
    (ivec2_scalar, i32) => ivec3_scalar = |(v, z)| ivec3_scalar::new(v.x, v.y, z);
    (i32, i32, i32) => ivec3_scalar = |(x, y, z)| ivec3_scalar::new(x, y, z);
    I32 => ivec3 = |n| ivec3::splat(n);
    (ivec2, I32) => ivec3 = |(a, b)| ivec3::from_ivec2(a, b);
    (vec2, Float) => ivec3 = |(a, b)| ivec3::from_vec2(a, b);
    (I32, I32, I32) => ivec3 = |(x, y, z)| ivec3::new(x, y, z);
});

maker!(MakeIvec4, make_ivec4 {
    i32 => ivec4_scalar = |n| ivec4_scalar::new(n, n, n, n);
    (ivec2_scalar, i32, i32) => ivec4_scalar = |(xy, z, w)| ivec4_scalar::new(xy.x, xy.y, z, w);
    (i32, i32, i32, i32) => ivec4_scalar = |(x, y, z, w)| ivec4_scalar::new(x, y, z, w);
    vec4_scalar => ivec4_scalar = |v| ivec4_scalar::new(
        f32_to_i32_scalar(v.x), f32_to_i32_scalar(v.y), f32_to_i32_scalar(v.z), f32_to_i32_scalar(v.w));
    I32 => ivec4 = |n| ivec4::splat(n);
    ivec4_scalar => ivec4 = |n| ivec4::from(n);
    ivec4 => ivec4 = |n| n;
    (ivec2, I32, I32) => ivec4 = |(a, b, c)| ivec4::from_ivec2(a, b, c);
    (I32, I32, I32, I32) => ivec4 = |(x, y, z, w)| ivec4::new(x, y, z, w);
});

maker!(MakeVec3, make_vec3 {
    f32 => vec3_scalar = |n| vec3_scalar::new(n, n, n);
    (vec2_scalar, f32) => vec3_scalar = |(v, z)| vec3_scalar::new(v.x, v.y, z);
    (f32, f32, f32) => vec3_scalar = |(x, y, z)| vec3_scalar::new(x, y, z);
    (i32, i32, f32) => vec3_scalar = |(x, y, z)| vec3_scalar::new(x as f32, y as f32, z);
    Float => vec3 = |n| vec3::splat(n);
    vec3_scalar => vec3 = |n| vec3::from(n);
    vec3 => vec3 = |n| n;
    vec4 => vec3 = |n| vec3::from_vec4(n);
    (vec2, Float) => vec3 = |(a, z)| vec3::from_vec2(a, z);
    (Float, Float, Float) => vec3 = |(x, y, z)| vec3::new(x, y, z);
});

maker!(MakeVec4, make_vec4 {
    f32 => vec4_scalar = |n| vec4_scalar::splat(n);
    (vec2_scalar, f32, f32) => vec4_scalar = |(v, z, w)| vec4_scalar::new(v.x, v.y, z, w);
    (vec2_scalar, vec2_scalar) => vec4_scalar = |(a, b)| vec4_scalar::new(a.x, a.y, b.x, b.y);
    (vec3_scalar, f32) => vec4_scalar = |(v, w)| vec4_scalar::new(v.x, v.y, v.z, w);
    (f32, f32, f32, f32) => vec4_scalar = |(x, y, z, w)| vec4_scalar::new(x, y, z, w);
    (f32, f32, vec2_scalar) => vec4_scalar = |(x, y, v)| vec4_scalar::new(x, y, v.x, v.y);
    ivec4_scalar => vec4_scalar = |v| vec4_scalar::new(v.x as f32, v.y as f32, v.z as f32, v.w as f32);
    Float => vec4 = |n| vec4::splat(n);
    vec4_scalar => vec4 = |n| vec4::from(n);
    vec4 => vec4 = |n| n;
    (vec3, Float) => vec4 = |(a, w)| vec4::from_vec3(a, w);
    (vec2, vec2) => vec4 = |(a, b)| vec4::from_vec2s(a, b);
    (vec2, Float, Float) => vec4 = |(a, z, w)| vec4::from_vec2_zw(a, z, w);
    (Float, Float, vec2) => vec4 = |(x, y, zw)| vec4::from_xy_vec2(x, y, zw);
    (Float, Float, Float, Float) => vec4 = |(x, y, z, w)| vec4::new(x, y, z, w);
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swizzles_and_refs() {
        let mut v = vec4::new(Float::splat(1.0), Float::splat(2.0), Float::splat(3.0), Float::splat(4.0));
        assert_eq!(v.sel2(W, X).x[0], 4.0);
        v.lsel2(Z, X).assign(vec2::from(vec2_scalar::new(7.0, 8.0)));
        assert_eq!((v.x[0], v.z[0]), (8.0, 7.0));
        let s = make_vec4((make_vec2(1.0f32), 3.0f32, 4.0f32));
        assert_eq!(s, vec4_scalar::new(1.0, 1.0, 3.0, 4.0));
        assert_eq!(v.index_lanes(I32::new([0, 1, 2, 9])).0, [8.0, 2.0, 7.0, 0.0]);
    }

    #[test]
    fn bvec_mix_selects() {
        let x = vec2::from(1.0f32);
        let y = vec2::from(2.0f32);
        let m = bvec2::new(I32::new([-1, 0, -1, 0]), I32::splat(0));
        let r = mix(x, y, m);
        assert_eq!(r.x.0, [2.0, 1.0, 2.0, 1.0]);
        assert_eq!(r.y.0, [1.0; 4]);
        assert!(any(bvec4_scalar::new(false, true, false, false)));
    }
}
