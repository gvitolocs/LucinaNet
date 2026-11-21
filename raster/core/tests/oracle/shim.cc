/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Test-only oracle: swgl's GLSL runtime (vector_type.h, glsl.h, program.h) behind extern "C" wrappers, so
// raster/core/tests/oracle_glsl.rs can run the same inputs through the C++ and the Rust and compare bits.
// Built by raster/core/oracle/build.rs as swgl's strict-float build (SWGL_STRICT_FP=1 in swgl/build.rs).
//
// Every wrapper takes its lane arguments as arrays (4 lanes per Float/I32, components in x, y, z, w order) and
// writes its result the same way.

#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <assert.h>
#include <stdio.h>
#include <math.h>

// What gl.cc defines before it includes the headers (clang, non-Windows branch).
#define ALWAYS_INLINE __attribute__((always_inline)) inline
#define NO_INLINE __attribute__((noinline))
#define PREFER_INLINE ALWAYS_INLINE
#define UNREACHABLE __builtin_unreachable()
#define UNUSED [[maybe_unused]]
#define FALLTHROUGH [[fallthrough]]
#define IMPLICIT
#define ALIGNED_DECL(_align, _type) _type __attribute__((aligned(_align)))

#include "gl_defs.h"
#include "glsl.h"
#include "program.h"

using namespace glsl;

static ALWAYS_INLINE Float ldf(const float* p) { return Float{p[0], p[1], p[2], p[3]}; }
static ALWAYS_INLINE I32 ldi(const int32_t* p) { return I32{p[0], p[1], p[2], p[3]}; }
static ALWAYS_INLINE U32 ldu(const uint32_t* p) { return U32{p[0], p[1], p[2], p[3]}; }
static ALWAYS_INLINE void stf(float* o, Float v) { memcpy(o, &v, 16); }
static ALWAYS_INLINE void sti(int32_t* o, I32 v) { memcpy(o, &v, 16); }
static ALWAYS_INLINE void stu(uint32_t* o, U32 v) { memcpy(o, &v, 16); }
static ALWAYS_INLINE vec2 ldv2(const float* p) { return vec2(ldf(p), ldf(p + 4)); }
static ALWAYS_INLINE vec3 ldv3(const float* p) { return vec3(ldf(p), ldf(p + 4), ldf(p + 8)); }
static ALWAYS_INLINE vec4 ldv4(const float* p) { return vec4(ldf(p), ldf(p + 4), ldf(p + 8), ldf(p + 12)); }
static ALWAYS_INLINE void stv2(float* o, vec2 v) { stf(o, v.x); stf(o + 4, v.y); }
static ALWAYS_INLINE void stv3(float* o, vec3 v) { stf(o, v.x); stf(o + 4, v.y); stf(o + 8, v.z); }
static ALWAYS_INLINE void stv4(float* o, vec4 v) { stf(o, v.x); stf(o + 4, v.y); stf(o + 8, v.z); stf(o + 12, v.w); }

#define EXPORT extern "C" __attribute__((visibility("default")))

// Float -> Float
#define F1(name, expr) \
  EXPORT void o_##name(const float* a, float* o) { Float x = ldf(a); stf(o, (expr)); }
// Float, Float -> Float
#define F2(name, expr) \
  EXPORT void o_##name(const float* a, const float* b, float* o) { Float x = ldf(a), y = ldf(b); stf(o, (expr)); }
// Float, Float, Float -> Float
#define F3(name, expr)                                                            \
  EXPORT void o_##name(const float* a, const float* b, const float* c, float* o) { \
    Float x = ldf(a), y = ldf(b), z = ldf(c);                                     \
    stf(o, (expr));                                                               \
  }
// Float, Float -> I32
#define FF_I(name, expr) \
  EXPORT void o_##name(const float* a, const float* b, int32_t* o) { Float x = ldf(a), y = ldf(b); sti(o, (expr)); }
// Float -> I32
#define F_I(name, expr) \
  EXPORT void o_##name(const float* a, int32_t* o) { Float x = ldf(a); sti(o, (expr)); }
// I32 -> Float
#define I_F(name, expr) \
  EXPORT void o_##name(const int32_t* a, float* o) { I32 x = ldi(a); stf(o, (expr)); }
// I32, I32 -> I32
#define II_I(name, expr) \
  EXPORT void o_##name(const int32_t* a, const int32_t* b, int32_t* o) { I32 x = ldi(a), y = ldi(b); sti(o, (expr)); }
// scalar float -> float
#define S1(name, expr) \
  EXPORT float o_##name(float x) { return (expr); }
#define S2(name, expr) \
  EXPORT float o_##name(float x, float y) { return (expr); }

// ---- lane arithmetic and comparisons
F2(add, x + y)
F2(sub, x - y)
F2(mul, x * y)
F2(div, x / y)
F1(neg, -x)
F3(madd, x * y + z)
F2(sub_scalar_lhs, 1.0f - x * y)
FF_I(lt, x < y)
FF_I(le, x <= y)
FF_I(gt, x > y)
FF_I(ge, x >= y)
FF_I(eq, x == y)
FF_I(ne, x != y)
II_I(i_add, x + y)
II_I(i_mul, x * y)
II_I(i_and_not, x & ~y)
II_I(i_xor, x ^ y)
II_I(i_gt, x > y)
II_I(i_clamp_0_y, clamp(x, I32(0), y))
EXPORT void o_i_shr(const int32_t* a, int32_t n, int32_t* o) { sti(o, ldi(a) >> n); }
EXPORT void o_i_shl(const int32_t* a, int32_t n, int32_t* o) { sti(o, ldi(a) << n); }
EXPORT void o_u_shr(const uint32_t* a, int32_t n, uint32_t* o) { stu(o, ldu(a) >> n); }
EXPORT void o_if_then_else(const int32_t* c, const float* a, const float* b, float* o) {
  stf(o, if_then_else(ldi(c), ldf(a), ldf(b)));
}
EXPORT void o_if_then(const int32_t* c, const float* a, float* o) { stf(o, if_then(ldi(c), ldf(a))); }
EXPORT int32_t o_test_all(const int32_t* c) { return test_all(ldi(c)); }
EXPORT int32_t o_test_any(const int32_t* c) { return test_any(ldi(c)); }
EXPORT int32_t o_test_none(const int32_t* c) { return test_none(ldi(c)); }
EXPORT int32_t o_test_equal(const int32_t* c) { return test_equal(ldi(c)); }

// ---- conversions and rounding modes
F_I(cvt_f32_i32, CONVERT(x, I32))
EXPORT void o_cvt_f32_u32(const float* a, uint32_t* o) { stu(o, CONVERT(ldf(a), U32)); }
I_F(cvt_i32_f32, CONVERT(x, Float))
EXPORT void o_cvt_u32_f32(const uint32_t* a, float* o) { stf(o, CONVERT(ldu(a), Float)); }
EXPORT void o_cast_u32(const uint32_t* a, float* o) { stf(o, cast(ldu(a))); }
F_I(cast_f32, cast(x))
F1(cast_roundtrip, cast(cast(x)))
FF_I(roundeven, roundeven(x, y))
FF_I(roundzero, roundzero(x, y))
FF_I(roundfast, roundfast(x, y))
F_I(round_pixel, round_pixel(x))
EXPORT void o_make_int_lanes(const float* a, int32_t* o) { sti(o, make_int(ldf(a))); }
EXPORT void o_make_uint_lanes(const float* a, uint32_t* o) { stu(o, make_uint(ldf(a))); }
EXPORT void o_make_float_u32(const uint32_t* a, float* o) { stf(o, make_float(ldu(a))); }
EXPORT int32_t o_make_int_s(float x) { return make_int(x); }
EXPORT uint32_t o_make_uint_s(float x) { return make_uint(x); }
EXPORT float o_make_float_u32_s(uint32_t x) { return make_float(x); }
EXPORT int32_t o_roundeven_s(float x, float y) { return roundeven(x, y); }
EXPORT int32_t o_roundzero_s(float x, float y) { return roundzero(x, y); }

// ---- approximations and transcendentals on lanes
F1(recip, recip(x))
F1(inversesqrt, inversesqrt(x))
F1(sqrt, sqrt(x))
F1(approx_log2, approx_log2(x))
F1(approx_pow2, approx_pow2(x))
F1(exp, exp(x))
F1(exp2, exp2(x))
F1(log, log(x))
F1(log2, log2(x))
F2(pow, pow(x, y))
F1(sin, sin(x))
F1(cos, cos(x))
F1(tan, tan(x))
F1(atan, atan(x))
F2(atan2, atan(x, y))
F1(fract, fract(x))
F1(floor, floor(x))
F1(ceil, ceil(x))
F1(round, round(x))
F1(abs, abs(x))
F1(sign, sign(x))
F2(min, min(x, y))
F2(max, max(x, y))
F3(clamp, clamp(x, y, z))
F3(mix, mix(x, y, z))
F2(step, step(x, y))
F2(mod, mod(x, y))
EXPORT void o_init_interp(float a, float b, float* o) { stf(o, init_interp(a, b)); }
EXPORT float o_dfdx(const float* a) { return dFdx(ldf(a)); }

// ---- scalar forms
S1(recip_s, recip(x))
S1(inversesqrt_s, inversesqrt(x))
S1(sqrt_s, sqrt(x))
S1(floor_s, floor(x))
S1(ceil_s, ceil(x))
S1(round_s, round(x))
S1(fract_s, fract(x))
S1(abs_s, abs(x))
S1(sign_s, sign(x))
S1(exp_s, exp(x))
S1(exp2_s, exp2(x))
S1(log_s, log(x))
S1(log2_s, log2(x))
S1(sin_s, sin(x))
S1(cos_s, cos(x))
S1(atan_s, atan(x))
S2(atan2_s, atan(x, y))
S2(pow_s, pow(x, y))
S2(min_s, min(x, y))
S2(max_s, max(x, y))
S2(step_s, step(x, y))
EXPORT float o_mix_s(float x, float y, float a) { return mix(x, y, a); }
EXPORT float o_clamp_s(float x, float y, float a) { return clamp(x, y, a); }

// ---- vectors
EXPORT void o_dot2(const float* a, const float* b, float* o) { stf(o, dot(ldv2(a), ldv2(b))); }
EXPORT void o_dot3(const float* a, const float* b, float* o) { stf(o, dot(ldv3(a), ldv3(b))); }
EXPORT float o_dot3_s(const float* a, const float* b) {
  return dot(vec3_scalar{a[0], a[1], a[2]}, vec3_scalar{b[0], b[1], b[2]});
}
EXPORT float o_dot2_s(const float* a, const float* b) { return dot(vec2_scalar{a[0], a[1]}, vec2_scalar{b[0], b[1]}); }
EXPORT void o_length2(const float* a, float* o) { stf(o, length(ldv2(a))); }
EXPORT float o_length2_s(float x, float y) { return length(vec2_scalar{x, y}); }
EXPORT void o_normalize2(const float* a, float* o) { stv2(o, normalize(ldv2(a))); }
EXPORT void o_normalize2_s(const float* a, float* o) {
  vec2_scalar r = normalize(vec2_scalar{a[0], a[1]});
  o[0] = r.x;
  o[1] = r.y;
}
EXPORT void o_distance2(const float* a, const float* b, float* o) { stf(o, distance(ldv2(a), ldv2(b))); }
EXPORT void o_fwidth(const float* a, float* o) {
  vec2_scalar r = fwidth(ldv2(a));
  o[0] = r.x;
  o[1] = r.y;
}
EXPORT void o_mix_vec4(const float* a, const float* b, const float* c, float* o) {
  stv4(o, mix(ldv4(a), ldv4(b), ldf(c)));
}
EXPORT void o_mix_vec4_scalar(const float* a, const float* b, const float* c, float* o) {
  vec4_scalar r = mix(vec4_scalar{a[0], a[1], a[2], a[3]}, vec4_scalar{b[0], b[1], b[2], b[3]},
                      vec4_scalar{c[0], c[1], c[2], c[3]});
  memcpy(o, &r, 16);
}
EXPORT void o_clamp_vec4(const float* a, const float* b, const float* c, float* o) {
  stv4(o, clamp(ldv4(a), ldf(b), ldf(c)));
}
EXPORT void o_step_vec2(const float* a, const float* b, float* o) { stv2(o, step(ldv2(a), ldv2(b))); }
EXPORT void o_floor_vec4(const float* a, float* o) { stv4(o, floor(ldv4(a))); }
EXPORT void o_pow_vec3(const float* a, const float* b, float* o) { stv3(o, pow(ldv3(a), ldv3(b))); }
EXPORT void o_pow_vec2(const float* a, const float* b, float* o) { stv2(o, pow(ldv2(a), ldv2(b))); }
EXPORT void o_mod_vec2(const float* a, const float* b, float* o) { stv2(o, mod(ldv2(a), ldv2(b))); }
EXPORT void o_vec4_div_float(const float* a, const float* b, float* o) { stv4(o, ldv4(a) / ldf(b)); }
EXPORT void o_vec2_mul_scalar(const float* a, const float* b, float* o) {
  stv2(o, ldv2(a) * vec2_scalar{b[0], b[1]});
}
EXPORT void o_vec4_eq(const float* a, const float* b, int32_t* o) { sti(o, ldv4(a) == ldv4(b)); }
EXPORT void o_roundfast_vec4(const float* a, const float* b, int32_t* o) {
  ivec4 r = roundfast(ldv4(a), ldf(b));
  sti(o, r.x);
  sti(o + 4, r.y);
  sti(o + 8, r.z);
  sti(o + 12, r.w);
}

// ---- matrices (columns of 4 floats for the _scalar forms, columns of vec4 lanes for the lane forms)
static ALWAYS_INLINE mat4_scalar ldm4s(const float* m) { return mat4_scalar::load_from_ptr(m); }
EXPORT void o_mat4s_mul_vec4s(const float* m, const float* v, float* o) {
  vec4_scalar r = ldm4s(m) * vec4_scalar::load_from_ptr(v);
  memcpy(o, &r, 16);
}
EXPORT void o_mat4s_mul_vec4(const float* m, const float* v, float* o) { stv4(o, ldm4s(m) * ldv4(v)); }
EXPORT void o_mat4_mul_vec4(const float* m, const float* v, float* o) {
  mat4 mm(ldv4(m), ldv4(m + 16), ldv4(m + 32), ldv4(m + 48));
  stv4(o, mm * ldv4(v));
}
EXPORT void o_mat3_mul_vec3(const float* m, const float* v, float* o) {
  mat3 mm(ldv3(m), ldv3(m + 12), ldv3(m + 24));
  stv3(o, mm * ldv3(v));
}
EXPORT void o_mat3s_mul_mat3s(const float* a, const float* b, float* o) {
  mat3_scalar x = make_mat3(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8]);
  mat3_scalar y = make_mat3(b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8]);
  mat3_scalar r = x * y;
  for (int c = 0; c < 3; c++) {
    o[c * 3 + 0] = r[c].x;
    o[c * 3 + 1] = r[c].y;
    o[c * 3 + 2] = r[c].z;
  }
}
EXPORT void o_mat3x4s_mul_vec3(const float* m, const float* v, float* o) {
  mat3x4_scalar mm = make_mat3x4(m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11]);
  stv4(o, mm * ldv3(v));
}
EXPORT void o_mat3x4s_mul_vec3s(const float* m, const float* v, float* o) {
  mat3x4_scalar mm = make_mat3x4(m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11]);
  vec4_scalar r = mm * vec3_scalar{v[0], v[1], v[2]};
  memcpy(o, &r, 16);
}
EXPORT void o_mat4x3s_mul_vec4(const float* m, const float* v, float* o) {
  mat4x3_scalar mm = transpose(make_mat3x4(m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11]));
  stv3(o, mm * ldv4(v));
}
EXPORT void o_mat4x3s_mul_vec4s(const float* m, const float* v, float* o) {
  mat4x3_scalar mm = transpose(make_mat3x4(m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11]));
  vec3_scalar r = mm * vec4_scalar::load_from_ptr(v);
  o[0] = r.x;
  o[1] = r.y;
  o[2] = r.z;
}
EXPORT void o_mat3s_mul_vec3s(const float* m, const float* v, float* o) {
  mat3_scalar mm = make_mat3(m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8]);
  vec3_scalar r = mm * vec3_scalar{v[0], v[1], v[2]};
  o[0] = r.x;
  o[1] = r.y;
  o[2] = r.z;
}
EXPORT void o_mat3s_mul_vec3(const float* m, const float* v, float* o) {
  mat3_scalar mm = make_mat3(m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8]);
  stv3(o, mm * ldv3(v));
}
EXPORT void o_mat2_mul_vec2(const float* m, const float* v, float* o) {
  mat2 mm(ldv2(m), ldv2(m + 8));
  stv2(o, mm * ldv2(v));
}
EXPORT void o_inverse_mat2s(const float* m, float* o) {
  mat2_scalar r = inverse(mat2_scalar{{m[0], m[1]}, {m[2], m[3]}});
  o[0] = r[0].x;
  o[1] = r[0].y;
  o[2] = r[1].x;
  o[3] = r[1].y;
}
EXPORT void o_inverse_mat2(const float* m, float* o) {
  mat2 r = inverse(mat2(ldv2(m), ldv2(m + 8)));
  stv2(o, r[0]);
  stv2(o + 8, r[1]);
}

// ---- vector_type.h: zips, halves and CONVERT between lane types
typedef V8<uint16_t> U16x8;
typedef V16<uint16_t> U16x16;
typedef V16<uint8_t> U8x16;
static ALWAYS_INLINE U16x8 ld8(const uint16_t* p) { U16x8 v; memcpy(&v, p, 16); return v; }
static ALWAYS_INLINE U8x16 ld16b(const uint8_t* p) { U8x16 v; memcpy(&v, p, 16); return v; }
EXPORT void o_zip_u16(const uint16_t* a, const uint16_t* b, uint16_t* o) {
  U16x16 r = zip(ld8(a), ld8(b));
  memcpy(o, &r, 32);
}
EXPORT void o_ziplow_u16(const uint16_t* a, const uint16_t* b, uint16_t* o) {
  U16x8 r = zipLow(ld8(a), ld8(b));
  memcpy(o, &r, 16);
}
EXPORT void o_ziphigh_u16(const uint16_t* a, const uint16_t* b, uint16_t* o) {
  U16x8 r = zipHigh(ld8(a), ld8(b));
  memcpy(o, &r, 16);
}
EXPORT void o_zip2low_u16(const uint16_t* a, const uint16_t* b, uint16_t* o) {
  U16x8 r = zip2Low(ld8(a), ld8(b));
  memcpy(o, &r, 16);
}
EXPORT void o_zip2high_u16(const uint16_t* a, const uint16_t* b, uint16_t* o) {
  U16x8 r = zip2High(ld8(a), ld8(b));
  memcpy(o, &r, 16);
}
EXPORT void o_ziplow_u8(const uint8_t* a, const uint8_t* b, uint8_t* o) {
  U8x16 r = zipLow(ld16b(a), ld16b(b));
  memcpy(o, &r, 16);
}
EXPORT void o_ziphigh_u8(const uint8_t* a, const uint8_t* b, uint8_t* o) {
  U8x16 r = zipHigh(ld16b(a), ld16b(b));
  memcpy(o, &r, 16);
}
EXPORT void o_combine_halves_u16(const uint16_t* a, const uint16_t* b, uint16_t* o) {
  U16x8 r = combine(highHalf(ld8(a)), lowHalf(ld8(b)));
  memcpy(o, &r, 16);
}
EXPORT void o_combinelow_u16(const uint16_t* a, const uint16_t* b, uint16_t* o) {
  U16x8 r = combineLow(ld8(a), ld8(b));
  memcpy(o, &r, 16);
}
EXPORT void o_pack_u16_u8(const uint16_t* a, uint8_t* o) {
  V8<uint8_t> r = CONVERT(ld8(a), V8<uint8_t>);
  memcpy(o, &r, 8);
}
EXPORT void o_unpack_u8_u16(const uint8_t* a, uint16_t* o) {
  U16x16 r = CONVERT(ld16b(a), U16x16);
  memcpy(o, &r, 32);
}
EXPORT void o_cvt_i32_u16(const int32_t* a, uint16_t* o) {
  U16 r = CONVERT(ldi(a), U16);
  memcpy(o, &r, 8);
}
EXPORT void o_cvt_u16_i32(const uint16_t* a, int32_t* o) {
  U16 v;
  memcpy(&v, a, 8);
  sti(o, CONVERT(v, I32));
}
EXPORT void o_cvt_i32_i16x8(const int32_t* a, const int32_t* b, int16_t* o) {
  V8<int16_t> r = CONVERT(combine(ldi(a), ldi(b)), V8<int16_t>);
  memcpy(o, &r, 16);
}
EXPORT void o_cvt_f32_u8(const float* a, uint8_t* o) {
  U8 r = CONVERT(ldf(a), U8);
  memcpy(o, &r, 4);
}
EXPORT void o_u16_mul_shr(const uint16_t* a, const uint16_t* b, uint16_t* o) {
  U16x8 r = (ld8(a) * ld8(b) + 0x80) >> 8;
  memcpy(o, &r, 16);
}
EXPORT void o_u16_lt(const uint16_t* a, const uint16_t* b, int16_t* o) {
  V8<int16_t> r = ld8(a) < ld8(b);
  memcpy(o, &r, 16);
}
EXPORT void o_u8_add(const uint8_t* a, const uint8_t* b, uint8_t* o) {
  U8x16 r = ld16b(a) + ld16b(b);
  memcpy(o, &r, 16);
}
EXPORT void o_u8_bitcast_u32(const uint8_t* a, uint32_t* o) {
  U8x16 v = ld16b(a);
  V4<uint32_t> r = bit_cast<V4<uint32_t>>(v);
  memcpy(o, &r, 16);
}
EXPORT void o_shuffle_f(const float* a, const float* b, float* o) {
  V8<float> r = SHUFFLE(ldf(a), ldf(b), 3, 4, 1, 6, 0, 7, 2, 5);
  memcpy(o, &r, 32);
}
EXPORT void o_swizzle_zwxy(const float* a, float* o) { stf(o, ldf(a).zwxy); }

// glsl-to-rust phase 2: what the generated shaders reach through C++'s implicit conversions.
// step(float, vec4_scalar): through vec4_scalar's implicit constructor (cs_clip_rectangle).
EXPORT void o_step_f_vec4_scalar(float e, const float* x, float* o) {
  vec4_scalar r = step(e, vec4_scalar{x[0], x[1], x[2], x[3]});
  memcpy(o, &r, 16);
}
// step(Float, vec4): the lane form.
EXPORT void o_step_F_vec4(const float* e, const float* x, float* o) { stv4(o, step(vec4(ldf(e)), ldv4(x))); }
// Float& vec4::operator[](int) (brush_blend's colora[i]).
EXPORT void o_vec4_index(const float* a, int32_t i, float* o) {
  vec4 v = ldv4(a);
  stf(o, v[i]);
}
// if_then_else(I32, Array<vec2, 4>, Array<vec2, 4>) (ps_split_composite's SplitGeometry).
EXPORT void o_ite_array_vec2_4(const int32_t* c, const float* t, const float* e, float* o) {
  Array<vec2, 4> at, ae;
  for (int i = 0; i < 4; i++) {
    at[i] = ldv2(t + 8 * i);
    ae[i] = ldv2(e + 8 * i);
  }
  Array<vec2, 4> r = if_then_else(ldi(c), at, ae);
  for (int i = 0; i < 4; i++) stv2(o + 8 * i, r[i]);
}
