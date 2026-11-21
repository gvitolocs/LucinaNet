/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Test-only: the `extern "C"` wrappers of `../tests/oracle/shim.cc` around swgl's C++ GLSL runtime.
//! Each `o_<name>` computes the C++ function the shim names it after; lane arguments are arrays of four lanes
//! per component (see the shim). Generated from the shim's declarations; keep the two in step.

pub mod ext;

unsafe extern "C" {
    pub fn o_add(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_sub(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_mul(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_div(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_neg(a: *const f32, o: *mut f32);
    pub fn o_madd(a: *const f32, b: *const f32, c: *const f32, o: *mut f32);
    pub fn o_sub_scalar_lhs(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_lt(a: *const f32, b: *const f32, o: *mut i32);
    pub fn o_le(a: *const f32, b: *const f32, o: *mut i32);
    pub fn o_gt(a: *const f32, b: *const f32, o: *mut i32);
    pub fn o_ge(a: *const f32, b: *const f32, o: *mut i32);
    pub fn o_eq(a: *const f32, b: *const f32, o: *mut i32);
    pub fn o_ne(a: *const f32, b: *const f32, o: *mut i32);
    pub fn o_i_add(a: *const i32, b: *const i32, o: *mut i32);
    pub fn o_i_mul(a: *const i32, b: *const i32, o: *mut i32);
    pub fn o_i_and_not(a: *const i32, b: *const i32, o: *mut i32);
    pub fn o_i_xor(a: *const i32, b: *const i32, o: *mut i32);
    pub fn o_i_gt(a: *const i32, b: *const i32, o: *mut i32);
    pub fn o_i_clamp_0_y(a: *const i32, b: *const i32, o: *mut i32);
    pub fn o_cvt_f32_i32(a: *const f32, o: *mut i32);
    pub fn o_cvt_i32_f32(a: *const i32, o: *mut f32);
    pub fn o_cast_f32(a: *const f32, o: *mut i32);
    pub fn o_cast_roundtrip(a: *const f32, o: *mut f32);
    pub fn o_roundeven(a: *const f32, b: *const f32, o: *mut i32);
    pub fn o_roundzero(a: *const f32, b: *const f32, o: *mut i32);
    pub fn o_roundfast(a: *const f32, b: *const f32, o: *mut i32);
    pub fn o_round_pixel(a: *const f32, o: *mut i32);
    pub fn o_recip(a: *const f32, o: *mut f32);
    pub fn o_inversesqrt(a: *const f32, o: *mut f32);
    pub fn o_sqrt(a: *const f32, o: *mut f32);
    pub fn o_approx_log2(a: *const f32, o: *mut f32);
    pub fn o_approx_pow2(a: *const f32, o: *mut f32);
    pub fn o_exp(a: *const f32, o: *mut f32);
    pub fn o_exp2(a: *const f32, o: *mut f32);
    pub fn o_log(a: *const f32, o: *mut f32);
    pub fn o_log2(a: *const f32, o: *mut f32);
    pub fn o_pow(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_sin(a: *const f32, o: *mut f32);
    pub fn o_cos(a: *const f32, o: *mut f32);
    pub fn o_tan(a: *const f32, o: *mut f32);
    pub fn o_atan(a: *const f32, o: *mut f32);
    pub fn o_atan2(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_fract(a: *const f32, o: *mut f32);
    pub fn o_floor(a: *const f32, o: *mut f32);
    pub fn o_ceil(a: *const f32, o: *mut f32);
    pub fn o_round(a: *const f32, o: *mut f32);
    pub fn o_abs(a: *const f32, o: *mut f32);
    pub fn o_sign(a: *const f32, o: *mut f32);
    pub fn o_min(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_max(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_clamp(a: *const f32, b: *const f32, c: *const f32, o: *mut f32);
    pub fn o_mix(a: *const f32, b: *const f32, c: *const f32, o: *mut f32);
    pub fn o_step(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_mod(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_recip_s(x: f32) -> f32;
    pub fn o_inversesqrt_s(x: f32) -> f32;
    pub fn o_sqrt_s(x: f32) -> f32;
    pub fn o_floor_s(x: f32) -> f32;
    pub fn o_ceil_s(x: f32) -> f32;
    pub fn o_round_s(x: f32) -> f32;
    pub fn o_fract_s(x: f32) -> f32;
    pub fn o_abs_s(x: f32) -> f32;
    pub fn o_sign_s(x: f32) -> f32;
    pub fn o_exp_s(x: f32) -> f32;
    pub fn o_exp2_s(x: f32) -> f32;
    pub fn o_log_s(x: f32) -> f32;
    pub fn o_log2_s(x: f32) -> f32;
    pub fn o_sin_s(x: f32) -> f32;
    pub fn o_cos_s(x: f32) -> f32;
    pub fn o_atan_s(x: f32) -> f32;
    pub fn o_atan2_s(x: f32, y: f32) -> f32;
    pub fn o_pow_s(x: f32, y: f32) -> f32;
    pub fn o_min_s(x: f32, y: f32) -> f32;
    pub fn o_max_s(x: f32, y: f32) -> f32;
    pub fn o_step_s(x: f32, y: f32) -> f32;
    pub fn o_i_shr(a: *const i32, n: i32, o: *mut i32);
    pub fn o_i_shl(a: *const i32, n: i32, o: *mut i32);
    pub fn o_u_shr(a: *const u32, n: i32, o: *mut u32);
    pub fn o_if_then_else(c: *const i32, a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_if_then(c: *const i32, a: *const f32, o: *mut f32);
    pub fn o_test_all(c: *const i32) -> i32;
    pub fn o_test_any(c: *const i32) -> i32;
    pub fn o_test_none(c: *const i32) -> i32;
    pub fn o_test_equal(c: *const i32) -> i32;
    pub fn o_cvt_f32_u32(a: *const f32, o: *mut u32);
    pub fn o_cvt_u32_f32(a: *const u32, o: *mut f32);
    pub fn o_cast_u32(a: *const u32, o: *mut f32);
    pub fn o_make_int_lanes(a: *const f32, o: *mut i32);
    pub fn o_make_uint_lanes(a: *const f32, o: *mut u32);
    pub fn o_make_float_u32(a: *const u32, o: *mut f32);
    pub fn o_make_int_s(x: f32) -> i32;
    pub fn o_make_uint_s(x: f32) -> u32;
    pub fn o_make_float_u32_s(x: u32) -> f32;
    pub fn o_roundeven_s(x: f32, y: f32) -> i32;
    pub fn o_roundzero_s(x: f32, y: f32) -> i32;
    pub fn o_init_interp(a: f32, b: f32, o: *mut f32);
    pub fn o_dfdx(a: *const f32) -> f32;
    pub fn o_mix_s(x: f32, y: f32, a: f32) -> f32;
    pub fn o_clamp_s(x: f32, y: f32, a: f32) -> f32;
    pub fn o_dot2(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_dot3(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_dot3_s(a: *const f32, b: *const f32) -> f32;
    pub fn o_dot2_s(a: *const f32, b: *const f32) -> f32;
    pub fn o_length2(a: *const f32, o: *mut f32);
    pub fn o_length2_s(x: f32, y: f32) -> f32;
    pub fn o_normalize2(a: *const f32, o: *mut f32);
    pub fn o_normalize2_s(a: *const f32, o: *mut f32);
    pub fn o_distance2(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_fwidth(a: *const f32, o: *mut f32);
    pub fn o_mix_vec4(a: *const f32, b: *const f32, c: *const f32, o: *mut f32);
    pub fn o_mix_vec4_scalar(a: *const f32, b: *const f32, c: *const f32, o: *mut f32);
    pub fn o_clamp_vec4(a: *const f32, b: *const f32, c: *const f32, o: *mut f32);
    pub fn o_step_vec2(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_step_f_vec4_scalar(e: f32, x: *const f32, o: *mut f32);
    pub fn o_step_F_vec4(e: *const f32, x: *const f32, o: *mut f32);
    pub fn o_vec4_index(a: *const f32, i: i32, o: *mut f32);
    pub fn o_ite_array_vec2_4(c: *const i32, t: *const f32, e: *const f32, o: *mut f32);
    pub fn o_floor_vec4(a: *const f32, o: *mut f32);
    pub fn o_pow_vec3(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_pow_vec2(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_mod_vec2(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_vec4_div_float(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_vec2_mul_scalar(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_vec4_eq(a: *const f32, b: *const f32, o: *mut i32);
    pub fn o_roundfast_vec4(a: *const f32, b: *const f32, o: *mut i32);
    pub fn o_mat4s_mul_vec4s(m: *const f32, v: *const f32, o: *mut f32);
    pub fn o_mat4s_mul_vec4(m: *const f32, v: *const f32, o: *mut f32);
    pub fn o_mat4_mul_vec4(m: *const f32, v: *const f32, o: *mut f32);
    pub fn o_mat3_mul_vec3(m: *const f32, v: *const f32, o: *mut f32);
    pub fn o_mat3s_mul_mat3s(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_mat3x4s_mul_vec3(m: *const f32, v: *const f32, o: *mut f32);
    pub fn o_mat3x4s_mul_vec3s(m: *const f32, v: *const f32, o: *mut f32);
    pub fn o_mat4x3s_mul_vec4(m: *const f32, v: *const f32, o: *mut f32);
    pub fn o_mat4x3s_mul_vec4s(m: *const f32, v: *const f32, o: *mut f32);
    pub fn o_mat3s_mul_vec3s(m: *const f32, v: *const f32, o: *mut f32);
    pub fn o_mat3s_mul_vec3(m: *const f32, v: *const f32, o: *mut f32);
    pub fn o_mat2_mul_vec2(m: *const f32, v: *const f32, o: *mut f32);
    pub fn o_inverse_mat2s(m: *const f32, o: *mut f32);
    pub fn o_inverse_mat2(m: *const f32, o: *mut f32);
    pub fn o_zip_u16(a: *const u16, b: *const u16, o: *mut u16);
    pub fn o_ziplow_u16(a: *const u16, b: *const u16, o: *mut u16);
    pub fn o_ziphigh_u16(a: *const u16, b: *const u16, o: *mut u16);
    pub fn o_zip2low_u16(a: *const u16, b: *const u16, o: *mut u16);
    pub fn o_zip2high_u16(a: *const u16, b: *const u16, o: *mut u16);
    pub fn o_ziplow_u8(a: *const u8, b: *const u8, o: *mut u8);
    pub fn o_ziphigh_u8(a: *const u8, b: *const u8, o: *mut u8);
    pub fn o_combine_halves_u16(a: *const u16, b: *const u16, o: *mut u16);
    pub fn o_combinelow_u16(a: *const u16, b: *const u16, o: *mut u16);
    pub fn o_pack_u16_u8(a: *const u16, o: *mut u8);
    pub fn o_unpack_u8_u16(a: *const u8, o: *mut u16);
    pub fn o_cvt_i32_u16(a: *const i32, o: *mut u16);
    pub fn o_cvt_u16_i32(a: *const u16, o: *mut i32);
    pub fn o_cvt_i32_i16x8(a: *const i32, b: *const i32, o: *mut i16);
    pub fn o_cvt_f32_u8(a: *const f32, o: *mut u8);
    pub fn o_u16_mul_shr(a: *const u16, b: *const u16, o: *mut u16);
    pub fn o_u16_lt(a: *const u16, b: *const u16, o: *mut i16);
    pub fn o_u8_add(a: *const u8, b: *const u8, o: *mut u8);
    pub fn o_u8_bitcast_u32(a: *const u8, o: *mut u32);
    pub fn o_shuffle_f(a: *const f32, b: *const f32, o: *mut f32);
    pub fn o_swizzle_zwxy(a: *const f32, o: *mut f32);
}

/// gl.cc's `extern "C"` functions (compiled whole by `build.rs` in `../tests/oracle/shim_t.cc`, the one translation unit
/// holding gl.cc, whose only shader is the hand-written "flat"), with
/// the signatures `swgl/src/swgl_fns.rs` declares for the shipped library. gl.cc keeps its current context in a static:
/// callers serialise their use.
pub mod gl {
    #![allow(non_snake_case)]
    use std::ffi::{c_char, c_void};

    unsafe extern "C" {
        pub fn CreateContext() -> *mut c_void;
        pub fn DestroyContext(ctx: *mut c_void);
        pub fn MakeCurrent(ctx: *mut c_void);
        pub fn ReferenceContext(ctx: *mut c_void);
        pub fn InitDefaultFramebuffer(x: i32, y: i32, width: i32, height: i32, stride: i32, buf: *mut c_void);
        pub fn GetColorBuffer(fbo: u32, flush: u8, width: *mut i32, height: *mut i32, stride: *mut i32) -> *mut c_void;
        pub fn ResolveFramebuffer(fbo: u32);
        pub fn SetTextureBuffer(tex: u32, internal_format: u32, width: i32, height: i32, stride: i32, buf: *mut c_void, min_width: i32, min_height: i32);
        pub fn GenTextures(n: i32, result: *mut u32);
        pub fn DeleteTexture(n: u32);
        pub fn BindTexture(target: u32, texture: u32);
        pub fn ActiveTexture(texture: u32);
        pub fn TexStorage2D(target: u32, levels: i32, internal_format: u32, width: i32, height: i32);
        pub fn TexImage2D(target: u32, level: i32, internal_format: i32, width: i32, height: i32, border: i32, format: u32, ty: u32, data: *const c_void);
        pub fn TexSubImage2D(target: u32, level: i32, xoffset: i32, yoffset: i32, width: i32, height: i32, format: u32, ty: u32, data: *const c_void);
        pub fn PixelStorei(name: u32, param: i32);
        pub fn GenBuffers(n: i32, result: *mut u32);
        pub fn DeleteBuffer(n: u32);
        pub fn BindBuffer(target: u32, buffer: u32);
        pub fn BufferData(target: u32, size: isize, data: *const c_void, usage: u32);
        pub fn BufferSubData(target: u32, offset: isize, size: isize, data: *const c_void);
        pub fn MapBuffer(target: u32, access: u32) -> *mut c_void;
        pub fn MapBufferRange(target: u32, offset: isize, length: isize, access: u32) -> *mut c_void;
        pub fn UnmapBuffer(target: u32) -> u8;
        pub fn GenFramebuffers(n: i32, result: *mut u32);
        pub fn DeleteFramebuffer(n: u32);
        pub fn BindFramebuffer(target: u32, fb: u32);
        pub fn FramebufferTexture2D(target: u32, attachment: u32, textarget: u32, texture: u32, level: i32);
        pub fn GenRenderbuffers(n: i32, result: *mut u32);
        pub fn DeleteRenderbuffer(n: u32);
        pub fn BindRenderbuffer(target: u32, rb: u32);
        pub fn RenderbufferStorage(target: u32, internalformat: u32, width: i32, height: i32);
        pub fn FramebufferRenderbuffer(target: u32, attachment: u32, renderbuffertarget: u32, renderbuffer: u32);
        pub fn CheckFramebufferStatus(target: u32) -> u32;
        pub fn ClearColor(r: f32, g: f32, b: f32, a: f32);
        pub fn ClearDepth(depth: f64);
        pub fn Clear(mask: u32);
        pub fn ClearColorRect(fbo: u32, xoffset: i32, yoffset: i32, width: i32, height: i32, r: f32, g: f32, b: f32, a: f32);
        pub fn ClearTexSubImage(target: u32, level: i32, xoffset: i32, yoffset: i32, zoffset: i32, width: i32, height: i32, depth: i32, format: u32, ty: u32, data: *const c_void);
        pub fn ClearTexImage(target: u32, level: i32, format: u32, ty: u32, data: *const c_void);
        pub fn Enable(cap: u32);
        pub fn Disable(cap: u32);
        pub fn SetScissor(x: i32, y: i32, width: i32, height: i32);
        pub fn SetViewport(x: i32, y: i32, width: i32, height: i32);
        pub fn ReadPixels(x: i32, y: i32, width: i32, height: i32, format: u32, ty: u32, data: *mut c_void);
        pub fn CopyImageSubData(src_name: u32, src_target: u32, src_level: i32, src_x: i32, src_y: i32, src_z: i32, dst_name: u32, dst_target: u32, dst_level: i32, dst_x: i32, dst_y: i32, dst_z: i32, src_width: i32, src_height: i32, src_depth: i32);
        pub fn CopyTexSubImage2D(target: u32, level: i32, xoffset: i32, yoffset: i32, x: i32, y: i32, width: i32, height: i32);
        pub fn InvalidateFramebuffer(target: u32, num_attachments: i32, attachments: *const u32);
        pub fn LockTexture(tex: u32) -> *mut c_void;
        pub fn LockFramebuffer(fbo: u32) -> *mut c_void;
        pub fn LockResource(resource: *mut c_void);
        pub fn UnlockResource(resource: *mut c_void) -> i32;
        pub fn GetResourceBuffer(resource: *mut c_void, width: *mut i32, height: *mut i32, stride: *mut i32) -> *mut c_void;
        pub fn GetIntegerv(pname: u32, params: *mut i32);
        pub fn GetBooleanv(pname: u32, params: *mut u8);
        pub fn GetString(name: u32) -> *const c_char;
        pub fn GetStringi(name: u32, index: u32) -> *const c_char;
        pub fn GetError() -> u32;
        pub fn DepthMask(flag: u8);
        pub fn BlendFunc(srgb: u32, drgb: u32, sa: u32, da: u32);
        pub fn BlendEquation(mode: u32);
        pub fn GenQueries(n: i32, result: *mut u32);
        pub fn BeginQuery(target: u32, id: u32);
        pub fn EndQuery(target: u32);
        pub fn GetQueryObjectui64v(id: u32, pname: u32, params: *mut u64);
        pub fn DeleteQuery(n: u32);
        pub fn DepthFunc(func: u32);
        pub fn CreateShader(shader_type: u32) -> u32;
        pub fn ShaderSourceByName(shader: u32, name: *const c_char);
        pub fn AttachShader(program: u32, shader: u32);
        pub fn CreateProgram() -> u32;
        pub fn LinkProgram(program: u32);
        pub fn GetLinkStatus(program: u32) -> i32;
        pub fn UseProgram(program: u32);
        pub fn GetUniformLocation(program: u32, name: *const c_char) -> i32;
        pub fn Uniform4fv(location: i32, count: i32, value: *const f32);
        pub fn GenVertexArrays(n: i32, result: *mut u32);
        pub fn BindVertexArray(vao: u32);
        pub fn VertexAttribPointer(index: u32, size: i32, type_: u32, normalized: u8, stride: i32, offset: *const c_void);
        pub fn EnableVertexAttribArray(index: u32);
        pub fn DrawElementsInstanced(mode: u32, count: i32, type_: u32, indices: isize, instancecount: i32);
        pub fn GetTextureBuffer(target: u32, name: u32, bound: u8, width: *mut i32, height: *mut i32, stride: *mut i32, bpp: *mut i32) -> *mut c_void;
    }
}

// ---- ../tests/oracle/shim_t.cc: texture.h, blend.h and composite.h through gl.cc ---------------------------

/// `TSampler`: buffer, stride in sampler units, size, `TextureFormat`, `TextureFilter` (as `u32`s).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TSampler {
    pub buf: *const u8,
    pub stride: u32,
    pub width: u32,
    pub height: u32,
    pub format: i32,
    pub filter: i32,
}

/// `TTexture`: a texture over an external buffer, optionally with a delayed clear (`rows` non-null).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TTexture {
    pub buf: *mut u8,
    pub stride: i32,
    pub width: i32,
    pub height: i32,
    pub internal_format: u32,
    pub off_x: i32,
    pub off_y: i32,
    pub mag_filter: u32,
    pub delay: i32,
    pub clear_val: u32,
    pub rows: *const u32,
}

/// `TBlendState`: the raster statics the blend stage reads.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TBlendState {
    pub key: i32,
    pub span_buf: *mut u8,
    pub clip_mask_buf: *mut u8,
    pub opaque_start: *const u8,
    pub opaque_size: u32,
    pub left_aa: [f32; 4],
    pub right_aa: [f32; 4],
    pub aa_slope: [f32; 4],
    pub blendcolor: [u16; 8],
    pub blend_color_rgba8: [u16; 16],
    pub blend_alpha_rgba8: [u16; 16],
    pub secondary: [f32; 16],
}

unsafe extern "C" {
    pub fn t_texel_fetch(t: *const TSampler, kind: i32, x: *const i32, y: *const i32, o: *mut f32);
    pub fn t_texel_fetch_s(t: *const TSampler, kind: i32, x: i32, y: i32, o: *mut f32);
    pub fn t_texel_fetch_i(t: *const TSampler, x: *const i32, y: *const i32, o: *mut i32);
    pub fn t_texel_fetch_i_s(t: *const TSampler, x: i32, y: i32, o: *mut i32);
    pub fn t_texel_fetch_ptr(
        t: *const TSampler,
        is_int: i32,
        x: *const i32,
        y: *const i32,
        min_x: i32,
        max_x: i32,
        min_y: i32,
        max_y: i32,
        dx: i32,
        dy: i32,
        o: *mut u8,
    );
    pub fn t_texel_fetch_ptr_s(
        t: *const TSampler,
        is_int: i32,
        x: i32,
        y: i32,
        min_x: i32,
        max_x: i32,
        min_y: i32,
        max_y: i32,
        dx: i32,
        dy: i32,
        o: *mut u8,
    );
    pub fn t_texture(t: *const TSampler, rect: i32, x: *const f32, y: *const f32, o: *mut f32);
    pub fn t_texture_s(t: *const TSampler, x: f32, y: f32, o: *mut f32);
    pub fn t_linear_packed(t: *const TSampler, rect: i32, kind: i32, x: *const i32, y: *const i32, o: *mut u8);
    pub fn t_gaussian(
        t: *const TSampler,
        rgba8: i32,
        vertical: i32,
        x: i32,
        y: i32,
        lo: i32,
        hi: i32,
        radius: i32,
        coeff: f32,
        coeff_step: f32,
        o: *mut u16,
    );
    pub fn t_prepare_texture(
        d: *const TTexture,
        skip: *const i32,
        delay_out: *mut i32,
        rows_out: *mut u32,
        has_rows: *mut i32,
    );
    pub fn t_blend_pixels_rgba8(
        b: *const TBlendState,
        buf: *mut u32,
        pdst: *const u8,
        src: *const u16,
        span: i32,
        o: *mut u16,
    );
    pub fn t_blend_pixels_r8(
        b: *const TBlendState,
        buf: *mut u8,
        dst: *const u16,
        src: *const u16,
        span: i32,
        o: *mut u16,
    );
    pub fn t_commit(b: *const TBlendState, r8: i32, blend: i32, kind: i32, buf: *mut u8, r: *const u16, len: i32);
    pub fn t_pack_pixels(kind: i32, v: *const f32, scale: f32, o: *mut u16);
    pub fn t_muldiv255(x: *const u16, y: *const u16, o: *mut u16);
    pub fn t_scale_blit(
        src: *const TTexture,
        src_req: *const i32,
        dst: *const TTexture,
        dst_req: *const i32,
        composite: i32,
        invert_y: i32,
        clip: *const i32,
    );
    pub fn t_linear_blit(
        src: *const TTexture,
        src_req: *const i32,
        dst: *const TTexture,
        dst_req: *const i32,
        composite: i32,
        invert_x: i32,
        invert_y: i32,
        clip: *const i32,
    );
    pub fn t_composite(dst: *const TTexture, src: *const TTexture, a: *const i32);
    pub fn t_apply_mask(dst: *const TTexture, mask: *const TTexture);
    pub fn t_composite_yuv(
        dst: *const TTexture,
        y: *const TTexture,
        u: *const TTexture,
        v: *const TTexture,
        a: *const i32,
    );
    pub fn t_blit_framebuffer(src: *const TTexture, dst: *const TTexture, a: *const i32, filter: u32);
}
