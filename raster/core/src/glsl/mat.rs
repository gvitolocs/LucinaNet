/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! glsl.h's matrix types: `mat2`, `mat3`, `mat4` and their `_scalar` forms, `mat3x4_scalar`, `mat4x3_scalar`
//! (column-major: `m[c]` is column `c`), with the products, `transpose`, `inverse` and constructors glsl.h has.
//!
//! A product `m[0].x * v.x + m[1].x * v.y + ...` is summed left to right with each multiply rounded on its own, as
//! the C++ written in glsl.h (the strict-float oracle).

use super::*;
use core::ops::{Index, IndexMut, Mul};

macro_rules! matrix {
    ($m:ident, $col:ident, $n:literal) => {
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $m {
            pub data: [$col; $n],
        }

        impl Index<usize> for $m {
            type Output = $col;
            #[inline(always)]
            fn index(&self, i: usize) -> &$col {
                &self.data[i]
            }
        }

        impl IndexMut<usize> for $m {
            #[inline(always)]
            fn index_mut(&mut self, i: usize) -> &mut $col {
                &mut self.data[i]
            }
        }
    };
}

matrix!(mat2_scalar, vec2_scalar, 2);
matrix!(mat2, vec2, 2);
matrix!(mat3_scalar, vec3_scalar, 3);
matrix!(mat3, vec3, 3);
matrix!(mat3x4_scalar, vec4_scalar, 3);
matrix!(mat4x3_scalar, vec3_scalar, 4);
matrix!(mat4_scalar, vec4_scalar, 4);
matrix!(mat4, vec4, 4);

macro_rules! scalar_eq {
    ($($m:ident),*) => {$(
        impl PartialEq for $m {
            fn eq(&self, o: &Self) -> bool {
                self.data == o.data
            }
        }
    )*};
}
scalar_eq!(mat2_scalar, mat3_scalar, mat3x4_scalar, mat4x3_scalar, mat4_scalar);

// ---- constructors and implicit conversions ---------------------------------------------------------------------

impl mat2_scalar {
    #[inline(always)]
    pub const fn new(a: vec2_scalar, b: vec2_scalar) -> Self {
        Self { data: [a, b] }
    }
    /// `mat2_scalar(float a)`: every element `a` (the C++ fills both columns with `vec2_scalar(a)`).
    #[inline(always)]
    pub const fn splat(a: f32) -> Self {
        Self::new(vec2_scalar::splat(a), vec2_scalar::splat(a))
    }
}

impl mat2 {
    #[inline(always)]
    pub fn new(a: vec2, b: vec2) -> Self {
        Self { data: [a, b] }
    }
    #[inline(always)]
    pub fn splat(a: Float) -> Self {
        Self::new(vec2::splat(a), vec2::splat(a))
    }
}

impl mat3_scalar {
    #[inline(always)]
    pub const fn new(a: vec3_scalar, b: vec3_scalar, c: vec3_scalar) -> Self {
        Self { data: [a, b, c] }
    }
}

impl mat3 {
    #[inline(always)]
    pub fn new(a: vec3, b: vec3, c: vec3) -> Self {
        Self { data: [a, b, c] }
    }
    /// One scalar matrix per lane.
    #[inline(always)]
    pub fn from_scalars(s0: mat3_scalar, s1: mat3_scalar, s2: mat3_scalar, s3: mat3_scalar) -> Self {
        Self::new(
            vec3::from_scalars(s0[0], s1[0], s2[0], s3[0]),
            vec3::from_scalars(s0[1], s1[1], s2[1], s3[1]),
            vec3::from_scalars(s0[2], s1[2], s2[2], s3[2]),
        )
    }
    /// Nine lane values, column by column.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub fn from_floats(
        d1: Float,
        d2: Float,
        d3: Float,
        d4: Float,
        d5: Float,
        d6: Float,
        d7: Float,
        d8: Float,
        d9: Float,
    ) -> Self {
        Self::new(vec3::new(d1, d2, d3), vec3::new(d4, d5, d6), vec3::new(d7, d8, d9))
    }
}

impl mat3x4_scalar {
    #[inline(always)]
    pub const fn new(a: vec4_scalar, b: vec4_scalar, c: vec4_scalar) -> Self {
        Self { data: [a, b, c] }
    }
}

impl mat4x3_scalar {
    #[inline(always)]
    pub const fn new(a: vec3_scalar, b: vec3_scalar, c: vec3_scalar, d: vec3_scalar) -> Self {
        Self { data: [a, b, c, d] }
    }
}

impl mat4_scalar {
    #[inline(always)]
    pub const fn new(a: vec4_scalar, b: vec4_scalar, c: vec4_scalar, d: vec4_scalar) -> Self {
        Self { data: [a, b, c, d] }
    }
    /// `load_from_ptr`: 16 floats, column by column.
    #[inline(always)]
    pub fn load_from_ptr(f: &[f32]) -> Self {
        Self::new(
            vec4_scalar::load_from_ptr(&f[0..4]),
            vec4_scalar::load_from_ptr(&f[4..8]),
            vec4_scalar::load_from_ptr(&f[8..12]),
            vec4_scalar::load_from_ptr(&f[12..16]),
        )
    }
}

impl mat4 {
    #[inline(always)]
    pub fn new(a: vec4, b: vec4, c: vec4, d: vec4) -> Self {
        Self { data: [a, b, c, d] }
    }
}

impl From<f32> for mat2_scalar {
    #[inline(always)]
    fn from(a: f32) -> Self {
        Self::splat(a)
    }
}

impl From<Float> for mat2 {
    #[inline(always)]
    fn from(a: Float) -> Self {
        Self::splat(a)
    }
}

impl From<mat2_scalar> for mat2 {
    #[inline(always)]
    fn from(s: mat2_scalar) -> Self {
        Self::new(vec2::from(s[0]), vec2::from(s[1]))
    }
}

impl From<mat3_scalar> for mat3 {
    #[inline(always)]
    fn from(s: mat3_scalar) -> Self {
        Self::new(vec3::from(s[0]), vec3::from(s[1]), vec3::from(s[2]))
    }
}

impl From<mat4_scalar> for mat4 {
    #[inline(always)]
    fn from(s: mat4_scalar) -> Self {
        Self::new(vec4::from(s[0]), vec4::from(s[1]), vec4::from(s[2]), vec4::from(s[3]))
    }
}

impl From<mat4> for mat3 {
    /// The top-left 3x3 block.
    #[inline(always)]
    fn from(m: mat4) -> Self {
        Self::new(
            vec3::new(m[0].x, m[0].y, m[0].z),
            vec3::new(m[1].x, m[1].y, m[1].z),
            vec3::new(m[2].x, m[2].y, m[2].z),
        )
    }
}

impl From<mat4_scalar> for mat3_scalar {
    #[inline(always)]
    fn from(m: mat4_scalar) -> Self {
        Self::new(
            vec3_scalar::new(m[0].x, m[0].y, m[0].z),
            vec3_scalar::new(m[1].x, m[1].y, m[1].z),
            vec3_scalar::new(m[2].x, m[2].y, m[2].z),
        )
    }
}

impl From<mat4> for mat2 {
    /// The top-left 2x2 block.
    #[inline(always)]
    fn from(m: mat4) -> Self {
        Self::new(vec2::new(m[0].x, m[0].y), vec2::new(m[1].x, m[1].y))
    }
}

impl From<mat4_scalar> for mat2_scalar {
    #[inline(always)]
    fn from(m: mat4_scalar) -> Self {
        Self::new(vec2_scalar::new(m[0].x, m[0].y), vec2_scalar::new(m[1].x, m[1].y))
    }
}

// ---- products --------------------------------------------------------------------------------------------------

/// `a[0] * b[0] + a[1] * b[1] + ...`, left to right.
macro_rules! summed {
    ($name:ident, $t:ty) => {
        #[inline(always)]
        fn $name<const N: usize>(a: [$t; N], b: [$t; N]) -> $t {
            let mut acc = a[0] * b[0];
            for i in 1..N {
                acc += a[i] * b[i];
            }
            acc
        }
    };
}
summed!(sum_s, f32);
summed!(sum_v, Float);

/// Row `$r` of a matrix as an array of `$e` (column `i` element `r`, broadcast if scalar).
macro_rules! row_elems {
    ($e:ty, $m:ident, $r:ident, [$($i:literal $c:ident),+]) => {
        [$(<$e>::from($m[$i].$r)),+]
    };
}

/// The vector's components as an array of `$e`.
macro_rules! vec_elems {
    ($e:ty, $v:ident, [$($i:literal $c:ident),+]) => {
        [$(<$e>::from($v.$c)),+]
    };
}

/// `m * v` for one matrix type, row by row. Scalar matrix elements meeting lane vectors are broadcast, as the C++
/// `float * Float`.
macro_rules! mat_mul_vec {
    ($m:ident * $v:ident -> $o:ident, $sum:ident, $e:ty, [$($r:ident),+], $cols:tt) => {
        impl Mul<$v> for $m {
            type Output = $o;
            #[inline(always)]
            fn mul(self, v: $v) -> $o {
                let m = self;
                let b = vec_elems!($e, v, $cols);
                $o::new($($sum(row_elems!($e, m, $r, $cols), b)),+)
            }
        }
    };
}

mat_mul_vec!(mat2_scalar * vec2_scalar -> vec2_scalar, sum_s, f32, [x, y], [0 x, 1 y]);
mat_mul_vec!(mat2_scalar * vec2 -> vec2, sum_v, Float, [x, y], [0 x, 1 y]);
mat_mul_vec!(mat2 * vec2 -> vec2, sum_v, Float, [x, y], [0 x, 1 y]);
mat_mul_vec!(mat3_scalar * vec3_scalar -> vec3_scalar, sum_s, f32, [x, y, z], [0 x, 1 y, 2 z]);
mat_mul_vec!(mat3_scalar * vec3 -> vec3, sum_v, Float, [x, y, z], [0 x, 1 y, 2 z]);
mat_mul_vec!(mat3 * vec3 -> vec3, sum_v, Float, [x, y, z], [0 x, 1 y, 2 z]);
mat_mul_vec!(mat3x4_scalar * vec3_scalar -> vec4_scalar, sum_s, f32, [x, y, z, w], [0 x, 1 y, 2 z]);
mat_mul_vec!(mat3x4_scalar * vec3 -> vec4, sum_v, Float, [x, y, z, w], [0 x, 1 y, 2 z]);
mat_mul_vec!(mat4x3_scalar * vec4_scalar -> vec3_scalar, sum_s, f32, [x, y, z], [0 x, 1 y, 2 z, 3 w]);
mat_mul_vec!(mat4x3_scalar * vec4 -> vec3, sum_v, Float, [x, y, z], [0 x, 1 y, 2 z, 3 w]);
mat_mul_vec!(mat4_scalar * vec4_scalar -> vec4_scalar, sum_s, f32, [x, y, z, w], [0 x, 1 y, 2 z, 3 w]);
mat_mul_vec!(mat4_scalar * vec4 -> vec4, sum_v, Float, [x, y, z, w], [0 x, 1 y, 2 z, 3 w]);
mat_mul_vec!(mat4 * vec4 -> vec4, sum_v, Float, [x, y, z, w], [0 x, 1 y, 2 z, 3 w]);

impl Mul for mat3_scalar {
    type Output = mat3_scalar;
    /// Column `c` of the result is `a * b[c]`.
    #[inline(always)]
    fn mul(self, b: mat3_scalar) -> mat3_scalar {
        let mut r = mat3_scalar::default();
        for c in 0..3 {
            r[c] = self * b[c];
        }
        r
    }
}

impl Mul<f32> for mat2_scalar {
    type Output = mat2_scalar;
    #[inline(always)]
    fn mul(self, f: f32) -> mat2_scalar {
        let mut u = self;
        u[0].x *= f;
        u[0].y *= f;
        u[1].x *= f;
        u[1].y *= f;
        u
    }
}

impl Mul<Float> for mat2 {
    type Output = mat2;
    #[inline(always)]
    fn mul(self, f: Float) -> mat2 {
        let mut u = self;
        u[0].x *= f;
        u[0].y *= f;
        u[1].x *= f;
        u[1].y *= f;
        u
    }
}

// ---- transpose / inverse ---------------------------------------------------------------------------------------

pub trait Transpose {
    type Output;
    fn transpose(self) -> Self::Output;
}

#[inline(always)]
pub fn transpose<T: Transpose>(m: T) -> T::Output {
    m.transpose()
}

impl Transpose for mat3 {
    type Output = mat3;
    #[inline(always)]
    fn transpose(self) -> mat3 {
        let m = self;
        mat3::new(
            vec3::new(m[0].x, m[1].x, m[2].x),
            vec3::new(m[0].y, m[1].y, m[2].y),
            vec3::new(m[0].z, m[1].z, m[2].z),
        )
    }
}

impl Transpose for mat3_scalar {
    type Output = mat3_scalar;
    #[inline(always)]
    fn transpose(self) -> mat3_scalar {
        let m = self;
        mat3_scalar::new(
            vec3_scalar::new(m[0].x, m[1].x, m[2].x),
            vec3_scalar::new(m[0].y, m[1].y, m[2].y),
            vec3_scalar::new(m[0].z, m[1].z, m[2].z),
        )
    }
}

impl Transpose for mat3x4_scalar {
    type Output = mat4x3_scalar;
    #[inline(always)]
    fn transpose(self) -> mat4x3_scalar {
        let m = self;
        mat4x3_scalar::new(
            vec3_scalar::new(m[0].x, m[1].x, m[2].x),
            vec3_scalar::new(m[0].y, m[1].y, m[2].y),
            vec3_scalar::new(m[0].z, m[1].z, m[2].z),
            vec3_scalar::new(m[0].w, m[1].w, m[2].w),
        )
    }
}

pub trait Inverse {
    fn inverse(self) -> Self;
}

#[inline(always)]
pub fn inverse<T: Inverse>(m: T) -> T {
    m.inverse()
}

impl Inverse for mat2 {
    /// `mat2(vec2(v[1].y, -v[0].y), vec2(-v[1].x, v[0].x)) * (1. / det)`; the double `1.` meets a `Float` and
    /// becomes `1.0f`.
    #[inline(always)]
    fn inverse(self) -> mat2 {
        let v = self;
        let det = v[0].x * v[1].y - v[0].y * v[1].x;
        mat2::new(vec2::new(v[1].y, -v[0].y), vec2::new(-v[1].x, v[0].x)) * (1.0f32 / det)
    }
}

impl Inverse for mat2_scalar {
    /// As for `mat2`, but here `1. / det` is a double division, rounded to float when it meets
    /// `operator*(mat2_scalar, float)`.
    #[inline(always)]
    fn inverse(self) -> mat2_scalar {
        let v = self;
        let det = v[0].x * v[1].y - v[0].y * v[1].x;
        mat2_scalar::new(vec2_scalar::new(v[1].y, -v[0].y), vec2_scalar::new(-v[1].x, v[0].x))
            * ((1.0 / det as f64) as f32)
    }
}

// ---- if_then_else / force_scalar / get_nth ---------------------------------------------------------------------

impl IfThenElse<mat2> for I32 {
    type Output = mat2;
    /// As the C++, which builds the second column from `t[0]` and `e[1]` (a bug kept for identical output).
    #[inline(always)]
    fn if_then_else(self, t: mat2, e: mat2) -> mat2 {
        mat2::new(if_then_else(self, t[0], e[0]), if_then_else(self, t[0], e[1]))
    }
}

impl IfThenElse<mat3> for I32 {
    type Output = mat3;
    #[inline(always)]
    fn if_then_else(self, t: mat3, e: mat3) -> mat3 {
        mat3::new(if_then_else(self, t[0], e[0]), if_then_else(self, t[1], e[1]), if_then_else(self, t[2], e[2]))
    }
}

impl IfThenElse<mat4> for I32 {
    type Output = mat4;
    #[inline(always)]
    fn if_then_else(self, t: mat4, e: mat4) -> mat4 {
        mat4::new(
            if_then_else(self, t[0], e[0]),
            if_then_else(self, t[1], e[1]),
            if_then_else(self, t[2], e[2]),
            if_then_else(self, t[3], e[3]),
        )
    }
}

impl ForceScalar for mat3 {
    type Output = mat3_scalar;
    #[inline(always)]
    fn force_scalar(self) -> mat3_scalar {
        mat3_scalar::new(force_scalar(self[0]), force_scalar(self[1]), force_scalar(self[2]))
    }
}

impl ForceScalar for mat4 {
    type Output = mat4_scalar;
    #[inline(always)]
    fn force_scalar(self) -> mat4_scalar {
        mat4_scalar::new(force_scalar(self[0]), force_scalar(self[1]), force_scalar(self[2]), force_scalar(self[3]))
    }
}

impl GetNth for mat3 {
    type Output = mat3_scalar;
    #[inline(always)]
    fn get_nth(self, n: usize) -> mat3_scalar {
        mat3_scalar::new(get_nth(self[0], n), get_nth(self[1], n), get_nth(self[2], n))
    }
}

// ---- make_mat* ---------------------------------------------------------------------------------------------------

macro_rules! mat_maker {
    ($tr:ident, $f:ident { $($arg:ty => $out:ty = |$p:pat_param| $e:expr;)* }) => {
        pub trait $tr {
            type Output;
            fn $f(self) -> Self::Output;
        }
        /// One argument or a tuple of them, as the C++ overloads.
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

type F9 = (f32, f32, f32, f32, f32, f32, f32, f32, f32);
type F12 = (f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32);
type F16 = (f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32);

mat_maker!(MakeMat2, make_mat2 {
    f32 => mat2_scalar = |n| mat2_scalar::splat(n);
    mat2_scalar => mat2_scalar = |m| m;
    (vec2_scalar, vec2_scalar) => mat2_scalar = |(x, y)| mat2_scalar::new(x, y);
    mat4_scalar => mat2_scalar = |m| mat2_scalar::from(m);
    Float => mat2 = |n| mat2::splat(n);
    mat2 => mat2 = |m| m;
    mat4 => mat2 = |m| mat2::from(m);
    (vec2, vec2) => mat2 = |(x, y)| mat2::new(x, y);
});

mat_maker!(MakeMat3, make_mat3 {
    mat3_scalar => mat3_scalar = |m| m;
    (vec3_scalar, vec3_scalar, vec3_scalar) => mat3_scalar = |(x, y, z)| mat3_scalar::new(x, y, z);
    F9 => mat3_scalar = |(m0, m1, m2, m3, m4, m5, m6, m7, m8)| mat3_scalar::new(
        vec3_scalar::new(m0, m1, m2),
        vec3_scalar::new(m3, m4, m5),
        vec3_scalar::new(m6, m7, m8),
    );
    mat4_scalar => mat3_scalar = |m| mat3_scalar::from(m);
    mat3 => mat3 = |m| m;
    mat4 => mat3 = |m| mat3::from(m);
    (vec3, vec3, vec3) => mat3 = |(x, y, z)| mat3::new(x, y, z);
});

mat_maker!(MakeMat3x4, make_mat3x4 {
    F12 => mat3x4_scalar = |(m0, m1, m2, m3, m4, m5, m6, m7, m8, m9, m10, m11)| mat3x4_scalar::new(
        vec4_scalar::new(m0, m1, m2, m3),
        vec4_scalar::new(m4, m5, m6, m7),
        vec4_scalar::new(m8, m9, m10, m11),
    );
});

mat_maker!(MakeMat4, make_mat4 {
    mat4_scalar => mat4_scalar = |m| m;
    (vec4_scalar, vec4_scalar, vec4_scalar, vec4_scalar) => mat4_scalar = |(x, y, z, w)| mat4_scalar::new(x, y, z, w);
    F16 => mat4_scalar = |(m0, m1, m2, m3, m4, m5, m6, m7, m8, m9, m10, m11, m12, m13, m14, m15)| mat4_scalar::new(
        vec4_scalar::new(m0, m1, m2, m3),
        vec4_scalar::new(m4, m5, m6, m7),
        vec4_scalar::new(m8, m9, m10, m11),
        vec4_scalar::new(m12, m13, m14, m15),
    );
    mat4 => mat4 = |m| m;
    (vec4, vec4, vec4, vec4) => mat4 = |(x, y, z, w)| mat4::new(x, y, z, w);
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mat4_scalar_times_vec4_scalar() {
        let m = make_mat4((1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0));
        assert_eq!(m * vec4_scalar::new(1.0, 2.0, 3.0, 4.0), vec4_scalar::new(90.0, 100.0, 110.0, 120.0));
    }

    #[test]
    fn transpose_mat3x4() {
        let m = make_mat3x4((1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0));
        let t = transpose(m);
        assert_eq!(t[3], vec3_scalar::new(4.0, 8.0, 12.0));
        assert_eq!(t * vec4_scalar::new(1.0, 0.0, 0.0, 0.0), vec3_scalar::new(1.0, 5.0, 9.0));
    }

    #[test]
    fn mat2_if_then_else_keeps_the_cpp_column_bug() {
        let col = |a: f32, b: f32| vec2::from(vec2_scalar::new(a, b));
        let t = mat2::new(col(1.0, 2.0), col(3.0, 4.0));
        let e = mat2::new(col(5.0, 6.0), col(7.0, 8.0));
        let r = if_then_else(I32::new([-1, 0, -1, 0]), t, e);
        assert_eq!(r[0].x.0, [1.0, 5.0, 1.0, 5.0]);
        // Second column: t[0] where the mask is set, e[1] elsewhere.
        assert_eq!(r[1].x.0, [1.0, 7.0, 1.0, 7.0]);
        assert_eq!(r[1].y.0, [2.0, 8.0, 2.0, 8.0]);
    }

    #[test]
    fn inverse_mat2_scalar() {
        let m = make_mat2((vec2_scalar::new(2.0, 0.0), vec2_scalar::new(0.0, 4.0)));
        assert_eq!(inverse(m), mat2_scalar::new(vec2_scalar::new(0.5, -0.0), vec2_scalar::new(-0.0, 0.25)));
    }

    #[test]
    fn submatrices() {
        let m = make_mat4((1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0));
        assert_eq!(make_mat3(m)[2], vec3_scalar::new(9.0, 10.0, 11.0));
        assert_eq!(make_mat2(m)[1], vec2_scalar::new(5.0, 6.0));
        let lanes = mat3::from(make_mat3(m));
        assert_eq!(force_scalar(lanes), make_mat3(m));
        assert_eq!(get_nth(lanes, 3), make_mat3(m));
    }
}
