/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Test-only oracle for texture.h, blend.h and composite.h: includes swgl's gl.cc itself, so the static functions
// and statics (blend_key, swgl_ClipMaskBuf, ...) are in this translation unit, and exports extern "C" wrappers
// (t_<name>) around them. load_shader.h comes from tests/oracle/stub (empty); load_shader is defined at the end, with one program ("flat"). Built by
// raster/core/oracle/build.rs as swgl's strict-float release build (SWGL_STRICT_FP=1, NDEBUG).

#include "gl.cc"

#define EXPORT extern "C" __attribute__((visibility("default")))

// ---- samplers and textures as plain data -----------------------------------------------------------------------

// A sampler: buffer, stride in sampler units (bytes for 1-byte texels, words for 2, dwords from 4), size, format
// (TextureFormat), filter (TextureFilter).
struct TSampler {
  const void* buf;
  uint32_t stride;
  uint32_t width;
  uint32_t height;
  int32_t format;
  int32_t filter;
};

template <typename S>
static void fill_sampler(S* s, const TSampler* t) {
  s->buf = (uint32_t*)t->buf;
  s->stride = t->stride;
  s->width = t->width;
  s->height = t->height;
  s->format = TextureFormat(t->format);
}

static sampler2D_impl mk2d(const TSampler* t) {
  sampler2D_impl s;
  fill_sampler(&s, t);
  s.filter = TextureFilter(t->filter);
  return s;
}

static sampler2DRect_impl mkrect(const TSampler* t) {
  sampler2DRect_impl s;
  fill_sampler(&s, t);
  s.filter = TextureFilter(t->filter);
  return s;
}

static isampler2D_impl mki(const TSampler* t) {
  isampler2D_impl s;
  fill_sampler(&s, t);
  return s;
}

// A texture with an external buffer of `stride` bytes per row, at `offset`, optionally with a delayed clear
// (`rows`: the cleared-rows bit vector to install, `delay`: the rows still to clear).
struct TTexture {
  void* buf;
  int32_t stride;
  int32_t width;
  int32_t height;
  uint32_t internal_format;
  int32_t off_x;
  int32_t off_y;
  uint32_t mag_filter;
  int32_t delay;
  uint32_t clear_val;
  const uint32_t* rows;
};

static void mktex(Texture& t, const TTexture* d) {
  t.internal_format = d->internal_format;
  t.width = d->width;
  t.height = d->height;
  t.set_should_free(false);
  t.set_buffer(d->buf, d->stride);
  t.offset = IntPoint(d->off_x, d->off_y);
  t.mag_filter = d->mag_filter;
  if (d->rows) {
    t.enable_delayed_clear(d->clear_val);
    memcpy(t.cleared_rows, d->rows, ((t.height + 31) / 32) * sizeof(uint32_t));
    t.delay_clear = d->delay;
  }
}

static ALWAYS_INLINE Float ldf(const float* p) { return Float{p[0], p[1], p[2], p[3]}; }
static ALWAYS_INLINE I32 ldi(const int32_t* p) { return I32{p[0], p[1], p[2], p[3]}; }
static ALWAYS_INLINE void stv4(float* o, const vec4& v) {
  memcpy(o, &v.x, 16);
  memcpy(o + 4, &v.y, 16);
  memcpy(o + 8, &v.z, 16);
  memcpy(o + 12, &v.w, 16);
}
static ALWAYS_INLINE void stiv4(int32_t* o, const ivec4& v) {
  memcpy(o, &v.x, 16);
  memcpy(o + 4, &v.y, 16);
  memcpy(o + 8, &v.z, 16);
  memcpy(o + 12, &v.w, 16);
}

// ---- texture.h ---------------------------------------------------------------------------------------------

// texelFetch(sampler2D, ivec2, 0) (kind 0), texelFetch(sampler2DRect, ivec2) (kind 1), and the typed samplers
// sampler2DRGBA32F (2), sampler2DRGBA8 (3), sampler2DR8 (4), sampler2DRG8 (5).
EXPORT void t_texel_fetch(const TSampler* t, int kind, const int32_t* x, const int32_t* y, float* o) {
  ivec2 P(ldi(x), ldi(y));
  vec4 r;
  switch (kind) {
    case 0: {
      sampler2D_impl s = mk2d(t);
      r = texelFetch(&s, P, 0);
      break;
    }
    case 1: {
      sampler2DRect_impl s = mkrect(t);
      r = texelFetch(&s, P);
      break;
    }
    case 2: {
      sampler2DRGBA32F_impl s;
      fill_sampler(&s, t);
      r = texelFetch(&s, P, 0);
      break;
    }
    case 3: {
      sampler2DRGBA8_impl s;
      fill_sampler(&s, t);
      r = texelFetch(&s, P, 0);
      break;
    }
    case 4: {
      sampler2DR8_impl s;
      fill_sampler(&s, t);
      r = texelFetch(&s, P, 0);
      break;
    }
    default: {
      sampler2DRG8_impl s;
      fill_sampler(&s, t);
      r = texelFetch(&s, P, 0);
      break;
    }
  }
  stv4(o, r);
}

// texelFetch(sampler2D | typed, ivec2_scalar, 0): kinds as t_texel_fetch (no rect).
EXPORT void t_texel_fetch_s(const TSampler* t, int kind, int32_t x, int32_t y, float* o) {
  ivec2_scalar P(x, y);
  vec4_scalar r;
  switch (kind) {
    case 0: {
      sampler2D_impl s = mk2d(t);
      r = texelFetch(&s, P, 0);
      break;
    }
    case 2: {
      sampler2DRGBA32F_impl s;
      fill_sampler(&s, t);
      r = texelFetch(&s, P, 0);
      break;
    }
    case 3: {
      sampler2DRGBA8_impl s;
      fill_sampler(&s, t);
      r = texelFetch(&s, P, 0);
      break;
    }
    case 4: {
      sampler2DR8_impl s;
      fill_sampler(&s, t);
      r = texelFetch(&s, P, 0);
      break;
    }
    default: {
      sampler2DRG8_impl s;
      fill_sampler(&s, t);
      r = texelFetch(&s, P, 0);
      break;
    }
  }
  memcpy(o, &r, 16);
}

EXPORT void t_texel_fetch_i(const TSampler* t, const int32_t* x, const int32_t* y, int32_t* o) {
  isampler2D_impl s = mki(t);
  stiv4(o, texelFetch(&s, ivec2(ldi(x), ldi(y)), 0));
}

EXPORT void t_texel_fetch_i_s(const TSampler* t, int32_t x, int32_t y, int32_t* o) {
  isampler2D_impl s = mki(t);
  ivec4_scalar r = texelFetch(&s, ivec2_scalar(x, y), 0);
  memcpy(o, &r, 16);
}

// texelFetchPtr + texelFetchUnchecked at (dx, dy), for sampler2D (RGBA32F) or isampler2D (RGBA32I).
EXPORT void t_texel_fetch_ptr(const TSampler* t, int is_int, const int32_t* x, const int32_t* y, int min_x,
                              int max_x, int min_y, int max_y, int dx, int dy, void* o) {
  ivec2 P(ldi(x), ldi(y));
  if (is_int) {
    isampler2D_impl s = mki(t);
    auto f = texelFetchPtr(&s, P, min_x, max_x, min_y, max_y);
    stiv4((int32_t*)o, texelFetchUnchecked(&s, f, dx, dy));
  } else {
    sampler2D_impl s = mk2d(t);
    auto f = texelFetchPtr(&s, P, min_x, max_x, min_y, max_y);
    stv4((float*)o, texelFetchUnchecked(&s, f, dx, dy));
  }
}

EXPORT void t_texel_fetch_ptr_s(const TSampler* t, int is_int, int32_t x, int32_t y, int min_x, int max_x, int min_y,
                                int max_y, int dx, int dy, void* o) {
  ivec2_scalar P(x, y);
  if (is_int) {
    isampler2D_impl s = mki(t);
    auto f = texelFetchPtr(&s, P, min_x, max_x, min_y, max_y);
    ivec4_scalar r = texelFetchUnchecked(&s, f, dx, dy);
    memcpy(o, &r, 16);
  } else {
    sampler2D_impl s = mk2d(t);
    auto f = texelFetchPtr(&s, P, min_x, max_x, min_y, max_y);
    vec4_scalar r = texelFetchUnchecked(&s, f, dx, dy);
    memcpy(o, &r, 16);
  }
}

// texture(sampler2D | sampler2DRect, vec2): nearest or linear by the sampler's filter, every format.
EXPORT void t_texture(const TSampler* t, int rect, const float* x, const float* y, float* o) {
  vec2 P(ldf(x), ldf(y));
  if (rect) {
    sampler2DRect_impl s = mkrect(t);
    stv4(o, texture(&s, P));
  } else {
    sampler2D_impl s = mk2d(t);
    stv4(o, texture(&s, P));
  }
}

EXPORT void t_texture_s(const TSampler* t, float x, float y, float* o) {
  sampler2D_impl s = mk2d(t);
  vec4_scalar r = texture(&s, vec2_scalar(x, y));
  memcpy(o, &r, 16);
}

// The packed and unpacked linear filters on quantized coordinates: kind 0 textureLinearUnpackedRGBA8 (16 u16),
// 1 textureLinearPackedRGBA8 (16 u8), 2 textureNearestPackedRGBA8 (16 u8), 3 textureLinearUnpackedR8 (4 u16),
// 4 textureLinearPackedR8 (4 u8), 5 textureLinearUnpackedRG8 (8 u16), 6 textureLinearPackedRG8 (8 u8),
// 7 textureLinearUnpackedR16 (4 i16), 8 textureLinearUnpackedRG16 (8 i16), 9 textureLinearPlanarRGBA8 (16 u16),
// 10 textureLinearPlanarRG8 (8 u16), 11 textureLinearPlanarYUY2 (12 u16).
EXPORT void t_linear_packed(const TSampler* t, int rect, int kind, const int32_t* x, const int32_t* y, void* o) {
  ivec2 i(ldi(x), ldi(y));
  sampler2D_impl s2 = mk2d(t);
  sampler2DRect_impl sr = mkrect(t);
#define DISPATCH(f) (rect ? f(&sr, i) : f(&s2, i))
  switch (kind) {
    case 0: {
      auto r = DISPATCH(textureLinearUnpackedRGBA8);
      memcpy(o, &r, 32);
      break;
    }
    case 1: {
      auto r = DISPATCH(textureLinearPackedRGBA8);
      memcpy(o, &r, 16);
      break;
    }
    case 2: {
      auto r = DISPATCH(textureNearestPackedRGBA8);
      memcpy(o, &r, 16);
      break;
    }
    case 3: {
      auto r = DISPATCH(textureLinearUnpackedR8);
      memcpy(o, &r, 8);
      break;
    }
    case 4: {
      auto r = DISPATCH(textureLinearPackedR8);
      memcpy(o, &r, 4);
      break;
    }
    case 5: {
      auto r = DISPATCH(textureLinearUnpackedRG8);
      memcpy(o, &r, 16);
      break;
    }
    case 6: {
      auto r = DISPATCH(textureLinearPackedRG8);
      memcpy(o, &r, 8);
      break;
    }
    case 7: {
      auto r = DISPATCH(textureLinearUnpackedR16);
      memcpy(o, &r, 8);
      break;
    }
    case 8: {
      auto r = DISPATCH(textureLinearUnpackedRG16);
      memcpy(o, &r, 16);
      break;
    }
    case 9: {
      auto r = DISPATCH(textureLinearPlanarRGBA8);
      memcpy(o, &r.rg, 16);
      memcpy((char*)o + 16, &r.ba, 16);
      break;
    }
    case 10: {
      auto r = DISPATCH(textureLinearPlanarRG8);
      memcpy(o, &r.rg, 16);
      break;
    }
    default: {
      auto r = DISPATCH(textureLinearPlanarYUY2);
      memcpy(o, &r.y, 8);
      memcpy((char*)o + 8, &r.u, 8);
      memcpy((char*)o + 16, &r.v, 8);
      break;
    }
  }
#undef DISPATCH
}

// gaussianBlurHorizontal / Vertical<uint32_t> (rgba8, 16 u16 out) or <uint8_t> (4 u16 out).
EXPORT void t_gaussian(const TSampler* t, int rgba8, int vertical, int32_t x, int32_t y, int lo, int hi, int radius,
                       float coeff, float coeff_step, uint16_t* o) {
  sampler2D_impl s = mk2d(t);
  ivec2_scalar i(x, y);
  if (rgba8) {
    auto r = vertical ? gaussianBlurVertical<uint32_t>(&s, i, lo, hi, radius, coeff, coeff_step)
                      : gaussianBlurHorizontal<uint32_t>(&s, i, lo, hi, radius, coeff, coeff_step);
    memcpy(o, &r, 32);
  } else {
    auto r = vertical ? gaussianBlurVertical<uint8_t>(&s, i, lo, hi, radius, coeff, coeff_step)
                      : gaussianBlurHorizontal<uint8_t>(&s, i, lo, hi, radius, coeff, coeff_step);
    memcpy(o, &r, 8);
  }
}

// prepare_texture(t, skip) on a texture with a delayed clear: the buffer afterwards, and the state left.
EXPORT void t_prepare_texture(const TTexture* d, const int32_t* skip, int32_t* delay_out, uint32_t* rows_out,
                              int32_t* has_rows) {
  Texture t;
  mktex(t, d);
  IntRect r;
  if (skip) r = IntRect{skip[0], skip[1], skip[2], skip[3]};
  prepare_texture(t, skip ? &r : nullptr);
  *delay_out = t.delay_clear;
  *has_rows = t.cleared_rows != nullptr;
  if (t.cleared_rows) memcpy(rows_out, t.cleared_rows, ((t.height + 31) / 32) * sizeof(uint32_t));
}

// ---- blend.h -----------------------------------------------------------------------------------------------

// The raster statics the blend stage reads.
struct TBlendState {
  int32_t key;
  void* span_buf;
  uint8_t* clip_mask_buf;
  const uint8_t* opaque_start;
  uint32_t opaque_size;
  float left_aa[4];
  float right_aa[4];
  float aa_slope[4];
  uint16_t blendcolor[8];
  uint16_t blend_color_rgba8[16];
  uint16_t blend_alpha_rgba8[16];
  float secondary[16];
};

static Context t_ctx;
static FragmentShaderImpl t_fs;

static void set_blend_state(const TBlendState* b) {
  ctx = &t_ctx;
  fragment_shader = &t_fs;
  blend_key = BlendKey(b->key);
  swgl_SpanBuf = b->span_buf;
  swgl_ClipMaskBuf = b->clip_mask_buf;
  swgl_OpaqueStart = b->opaque_start;
  swgl_OpaqueSize = b->opaque_size;
  swgl_LeftAADist = ldf(b->left_aa);
  swgl_RightAADist = ldf(b->right_aa);
  swgl_AASlope = ldf(b->aa_slope);
  memcpy(&ctx->blendcolor, b->blendcolor, 16);
  memcpy(&swgl_BlendColorRGBA8, b->blend_color_rgba8, 32);
  memcpy(&swgl_BlendAlphaRGBA8, b->blend_alpha_rgba8, 32);
  t_fs.gl_SecondaryFragColor = vec4(ldf(b->secondary), ldf(b->secondary + 4), ldf(b->secondary + 8),
                                    ldf(b->secondary + 12));
}

EXPORT void t_blend_pixels_rgba8(const TBlendState* b, uint32_t* buf, const uint8_t* pdst, const uint16_t* src,
                                 int span, uint16_t* o) {
  set_blend_state(b);
  PackedRGBA8 d;
  WideRGBA8 s;
  memcpy(&d, pdst, 16);
  memcpy(&s, src, 32);
  WideRGBA8 r = blend_pixels(buf, d, s, span);
  memcpy(o, &r, 32);
}

EXPORT void t_blend_pixels_r8(const TBlendState* b, uint8_t* buf, const uint16_t* dst, const uint16_t* src, int span,
                              uint16_t* o) {
  set_blend_state(b);
  WideR8 d, s;
  memcpy(&d, dst, 8);
  memcpy(&s, src, 8);
  WideR8 r = blend_pixels(buf, d, s, span);
  memcpy(o, &r, 8);
}

// commit_blend_span<BLEND> (kind 0) / its partial form with len (kind 1) / commit_solid_span<BLEND> (kind 2) of a
// wide RGBA8 (or R8 if r8) value at buf.
EXPORT void t_commit(const TBlendState* b, int r8, int blend, int kind, void* buf, const uint16_t* r, int len) {
  set_blend_state(b);
  if (r8) {
    WideR8 v;
    memcpy(&v, r, 8);
    uint8_t* p = (uint8_t*)buf;
    if (kind == 0) blend ? commit_blend_span<true>(p, v) : commit_blend_span<false>(p, v);
    else if (kind == 1) blend ? commit_blend_span<true>(p, v, len) : commit_blend_span<false>(p, v, len);
    else blend ? commit_solid_span<true>(p, v, len) : commit_solid_span<false>(p, v, len);
  } else {
    WideRGBA8 v;
    memcpy(&v, r, 32);
    uint32_t* p = (uint32_t*)buf;
    if (kind == 0) blend ? commit_blend_span<true>(p, v) : commit_blend_span<false>(p, v);
    else if (kind == 1) blend ? commit_blend_span<true>(p, v, len) : commit_blend_span<false>(p, v, len);
    else blend ? commit_solid_span<true>(p, v, len) : commit_solid_span<false>(p, v, len);
  }
}

// pack_pixels_RGBA8 of vec4 (kind 0), Float (1), float (2), vec4_scalar (3), WideRGBA32F (4); packR8 / pack_pixels_R8
// of Float (5).
EXPORT void t_pack_pixels(int kind, const float* v, float scale, uint16_t* o) {
  switch (kind) {
    case 0: {
      WideRGBA8 r = pack_pixels_RGBA8(vec4(ldf(v), ldf(v + 4), ldf(v + 8), ldf(v + 12)), scale);
      memcpy(o, &r, 32);
      break;
    }
    case 1: {
      WideRGBA8 r = pack_pixels_RGBA8(ldf(v), scale);
      memcpy(o, &r, 32);
      break;
    }
    case 2: {
      WideRGBA8 r = pack_pixels_RGBA8(v[0], scale);
      memcpy(o, &r, 32);
      break;
    }
    case 3: {
      WideRGBA8 r = pack_pixels_RGBA8(vec4_scalar{v[0], v[1], v[2], v[3]}, scale);
      memcpy(o, &r, 32);
      break;
    }
    case 4: {
      WideRGBA32F w;
      memcpy(&w, v, 64);
      WideRGBA8 r = pack_pixels_RGBA8(w, scale);
      memcpy(o, &r, 32);
      break;
    }
    default: {
      WideR8 r = pack_pixels_R8(ldf(v), scale);
      memcpy(o, &r, 8);
      break;
    }
  }
}

EXPORT void t_muldiv255(const uint16_t* x, const uint16_t* y, uint16_t* o) {
  WideRGBA8 a, b;
  memcpy(&a, x, 32);
  memcpy(&b, y, 32);
  WideRGBA8 r = muldiv255(a, b);
  memcpy(o, &r, 32);
}

// ---- composite.h -------------------------------------------------------------------------------------------

EXPORT void t_scale_blit(const TTexture* src, const int32_t* src_req, const TTexture* dst, const int32_t* dst_req,
                         int composite, int invert_y, const int32_t* clip) {
  Texture s, d;
  mktex(s, src);
  mktex(d, dst);
  IntRect sr{src_req[0], src_req[1], src_req[2], src_req[3]};
  IntRect dr{dst_req[0], dst_req[1], dst_req[2], dst_req[3]};
  IntRect cr{clip[0], clip[1], clip[2], clip[3]};
  if (composite) scale_blit<true>(s, sr, d, dr, invert_y, cr);
  else scale_blit<false>(s, sr, d, dr, invert_y, cr);
}

EXPORT void t_linear_blit(const TTexture* src, const int32_t* src_req, const TTexture* dst, const int32_t* dst_req,
                          int composite, int invert_x, int invert_y, const int32_t* clip) {
  Texture s, d;
  mktex(s, src);
  mktex(d, dst);
  IntRect sr{src_req[0], src_req[1], src_req[2], src_req[3]};
  IntRect dr{dst_req[0], dst_req[1], dst_req[2], dst_req[3]};
  IntRect cr{clip[0], clip[1], clip[2], clip[3]};
  if (composite) linear_blit<true>(s, sr, d, dr, invert_x, invert_y, cr);
  else linear_blit<false>(s, sr, d, dr, invert_x, invert_y, cr);
}

// Composite(dst, src, srcX, srcY, srcW, srcH, dstX, dstY, dstW, dstH, opaque, flipX, flipY, filter, clipX, clipY,
// clipW, clipH): a[0..16] are the ints in that order (opaque/flips as 0/1).
EXPORT void t_composite(const TTexture* dst, const TTexture* src, const int32_t* a) {
  Texture s, d;
  mktex(s, src);
  mktex(d, dst);
  Composite(&d, &s, a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14],
            a[15]);
}

EXPORT void t_apply_mask(const TTexture* dst, const TTexture* mask) {
  Texture m, d;
  mktex(m, mask);
  mktex(d, dst);
  ApplyMask(&d, &m);
}

// CompositeYUV(dst, y, u, v, colorSpace, colorDepth, srcX, srcY, srcW, srcH, dstX, dstY, dstW, dstH, flipX, flipY,
// clipX, clipY, clipW, clipH): a[0..18] are the ints from colorSpace on.
EXPORT void t_composite_yuv(const TTexture* dst, const TTexture* y, const TTexture* u, const TTexture* v,
                            const int32_t* a) {
  Texture d, ty, tu, tv;
  mktex(d, dst);
  mktex(ty, y);
  mktex(tu, u);
  mktex(tv, v);
  CompositeYUV(&d, &ty, &tu, &tv, YUVRangedColorSpace(a[0]), a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9],
               a[10], a[11], a[12], a[13], a[14], a[15]);
}

// BlitFramebuffer through the GL entry points: src and dst become the color attachments of the read and draw
// framebuffers (SetTextureBuffer), then BlitFramebuffer(a[0..8], GL_COLOR_BUFFER_BIT, filter).
EXPORT void t_blit_framebuffer(const TTexture* src, const TTexture* dst, const int32_t* a, uint32_t filter) {
  Context* c = CreateContext();
  MakeCurrent(c);
  GLuint tex[2], fb[2];
  GenTextures(2, tex);
  GenFramebuffers(2, fb);
  const TTexture* d[2] = {src, dst};
  for (int i = 0; i < 2; i++) {
    SetTextureBuffer(tex[i], d[i]->internal_format, d[i]->width, d[i]->height, d[i]->stride, d[i]->buf, 0, 0);
    ctx->textures[tex[i]].offset = IntPoint(d[i]->off_x, d[i]->off_y);
  }
  BindFramebuffer(GL_READ_FRAMEBUFFER, fb[0]);
  FramebufferTexture2D(GL_READ_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex[0], 0);
  BindFramebuffer(GL_DRAW_FRAMEBUFFER, fb[1]);
  FramebufferTexture2D(GL_DRAW_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex[1], 0);
  BlitFramebuffer(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], GL_COLOR_BUFFER_BIT, filter);
  MakeCurrent(nullptr);
  DestroyContext(c);
}

// ---- "flat": a hand-written program for the rasterizer oracle (tests/rasterize_flat.rs) ------------------------
//
// Positions from attribute 0 (vec2), uniform 0 (`uColor`) the fragment colour, uniform 1 (`uZW`) the depth in [0]
// and per-vertex Ws in [1..3] (vertex 0's W is 1; `[1] == 0` means every W is 1, the 2D path). Clip coordinates are
// the position scaled by W. tests/rasterize_flat.rs writes the same program against lucinanet_raster::program.

namespace flat {

struct Frag : FragmentShaderImpl {
  float color[4] = {0, 0, 0, 0};

  static void init_span(FragmentShaderImpl*, const void*, const void*) {}
  static void run(FragmentShaderImpl* impl) {
    Frag* s = static_cast<Frag*>(impl);
    s->gl_FragColor = vec4(Float(s->color[0]), Float(s->color[1]), Float(s->color[2]), Float(s->color[3]));
  }
  static void skip(FragmentShaderImpl*, int) {}

  Frag() {
    init_span_func = &init_span;
    run_func = &run;
    skip_func = &skip;
    init_span_w_func = &init_span;
    run_w_func = &run;
    skip_w_func = &skip;
  }
};

struct Vert : VertexShaderImpl {
  Frag* frag = nullptr;
  vec2 pos;
  float z = 0;
  float w[4] = {1, 0, 1, 1};

  static void set_uniform_1i(VertexShaderImpl*, int, int) {}
  static void set_uniform_4fv(VertexShaderImpl* impl, int index, const float* value) {
    Vert* s = static_cast<Vert*>(impl);
    if (index == 0) {
      memcpy(s->frag->color, value, sizeof(s->frag->color));
    } else if (index == 1) {
      s->z = value[0];
      memcpy(s->w, value, sizeof(s->w));
      s->w[0] = 1;
    }
  }
  static void set_uniform_matrix4fv(VertexShaderImpl*, int, const float*) {}
  static void init_batch(VertexShaderImpl*) {}
  static void load_attribs(VertexShaderImpl* impl, VertexAttrib* attribs, uint32_t start, int instance, int count) {
    Vert* s = static_cast<Vert*>(impl);
    load_attrib(s->pos, attribs[0], start, instance, count);
  }
  static void run_primitive(VertexShaderImpl* impl, char*, size_t) {
    Vert* s = static_cast<Vert*>(impl);
    Float w = s->w[1] == 0 ? Float(1.0f) : Float{s->w[0], s->w[1], s->w[2], s->w[3]};
    s->gl_Position = vec4(s->pos.x * w, s->pos.y * w, Float(s->z) * w, w);
  }

  Vert() {
    set_uniform_1i_func = &set_uniform_1i;
    set_uniform_4fv_func = &set_uniform_4fv;
    set_uniform_matrix4fv_func = &set_uniform_matrix4fv;
    init_batch_func = &init_batch;
    load_attribs_func = &load_attribs;
    run_primitive_func = &run_primitive;
  }
};

struct Program : ProgramImpl {
  Vert vert;
  Frag frag;

  Program() { vert.frag = &frag; }
  int get_uniform(const char* name) const override {
    if (strcmp(name, "uColor") == 0) return 0;
    if (strcmp(name, "uZW") == 0) return 1;
    return -1;
  }
  void bind_attrib(const char*, int) override {}
  int get_attrib(const char*) const override { return 0; }
  size_t interpolants_size() const override { return 0; }
  VertexShaderImpl* get_vertex_shader() override { return &vert; }
  FragmentShaderImpl* get_fragment_shader() override { return &frag; }
  const char* get_name() const override { return "flat"; }
};

static ProgramImpl* loader() { return new Program; }

}  // namespace flat

// ---- "grad": flat with one interpolated value, for the parallel rasterizer (tests/rasterize_flat.rs) ----------
//
// The vertex shader also outputs v = (x + y) * 0.25 + 0.5 of its position; the colour is (v, 0.25, 1 - v, 1).

namespace grad {

struct Frag : flat::Frag {
  Float v = 0;
  float vstep = 0;

  static void init_span(FragmentShaderImpl* impl, const void* interps, const void* step) {
    Frag* s = static_cast<Frag*>(impl);
    float i = *static_cast<const float*>(interps);
    float d = *static_cast<const float*>(step);
    s->v = init_interp(i, d);
    s->vstep = d * 4.0f;
  }
  static void run(FragmentShaderImpl* impl) {
    Frag* s = static_cast<Frag*>(impl);
    s->gl_FragColor = vec4(s->v, Float(0.25f), Float(1.0f) - s->v, Float(1.0f));
    s->v += s->vstep;
  }
  static void skip(FragmentShaderImpl* impl, int steps) {
    Frag* s = static_cast<Frag*>(impl);
    s->v += s->vstep * (steps * 0.25f);
  }

  Frag() {
    init_span_func = &init_span;
    run_func = &run;
    skip_func = &skip;
    init_span_w_func = &init_span;
    run_w_func = &run;
    skip_w_func = &skip;
  }
};

struct Vert : flat::Vert {
  static void run_primitive(VertexShaderImpl* impl, char* interps, size_t interp_stride) {
    flat::Vert::run_primitive(impl, interps, interp_stride);
    Vert* s = static_cast<Vert*>(impl);
    Float v = (s->pos.x + s->pos.y) * 0.25f + 0.5f;
    for (int k = 0; k < 4; k++) {
      float f = v[k];
      memcpy(interps + k * interp_stride, &f, sizeof(float));
    }
  }
  Vert() { run_primitive_func = &run_primitive; }
};

struct Program : ProgramImpl {
  Vert vert;
  Frag frag;

  Program() { vert.frag = &frag; }
  int get_uniform(const char* name) const override {
    if (strcmp(name, "uColor") == 0) return 0;
    if (strcmp(name, "uZW") == 0) return 1;
    return -1;
  }
  void bind_attrib(const char*, int) override {}
  int get_attrib(const char*) const override { return 0; }
  size_t interpolants_size() const override { return sizeof(float); }
  VertexShaderImpl* get_vertex_shader() override { return &vert; }
  FragmentShaderImpl* get_fragment_shader() override { return &frag; }
  const char* get_name() const override { return "grad"; }
};

static ProgramImpl* loader() { return new Program; }

}  // namespace grad

// ---- "feedback": flat whose colour is read from the texture on unit 0 (tests/rasterize_flat.rs) ----------------
//
// The colour is the texel at (gl_FragCoord.x, gl_FragCoord.y - 1) of the 2D texture on unit 0, its red, green and blue
// rotated (g, b, r, a). Bound to the colour target, each row is the row above it rotated: a feedback loop.

namespace feedback {

struct Frag : flat::Frag {
  sampler2D_impl sampler_impl;
  sampler2D sampler = &sampler_impl;

  static void run(FragmentShaderImpl* impl) {
    Frag* s = static_cast<Frag*>(impl);
    ivec2 p(cast(s->gl_FragCoord.x), cast(s->gl_FragCoord.y - 1.0f));
    vec4 c = texelFetchRGBA8(s->sampler, p);
    s->gl_FragColor = vec4(c.y, c.z, c.x, c.w);
    s->step_fragcoord();
  }
  static void skip(FragmentShaderImpl* impl, int steps) { static_cast<Frag*>(impl)->step_fragcoord(steps); }

  Frag() {
    run_func = &run;
    skip_func = &skip;
    run_w_func = &run;
    skip_w_func = &skip;
  }
};

struct Vert : flat::Vert {
  Frag* fb_frag = nullptr;
  static void init_batch(VertexShaderImpl* impl) {
    Vert* s = static_cast<Vert*>(impl);
    s->fb_frag->sampler = lookup_sampler(&s->fb_frag->sampler_impl, 0);
  }
  Vert() { init_batch_func = &init_batch; }
};

struct Program : ProgramImpl {
  Vert vert;
  Frag frag;

  Program() {
    vert.frag = &frag;
    vert.fb_frag = &frag;
  }
  int get_uniform(const char* name) const override {
    if (strcmp(name, "uColor") == 0) return 0;
    if (strcmp(name, "uZW") == 0) return 1;
    return -1;
  }
  void bind_attrib(const char*, int) override {}
  int get_attrib(const char*) const override { return 0; }
  size_t interpolants_size() const override { return 0; }
  VertexShaderImpl* get_vertex_shader() override { return &vert; }
  FragmentShaderImpl* get_fragment_shader() override { return &frag; }
  const char* get_name() const override { return "feedback"; }
};

static ProgramImpl* loader() { return new Program; }

}  // namespace feedback

ProgramLoader load_shader(const char* name) {
  if (strcmp(name, "flat") == 0) return flat::loader;
  if (strcmp(name, "grad") == 0) return grad::loader;
  if (strcmp(name, "feedback") == 0) return feedback::loader;
  return nullptr;
}

// The span fast paths (swgl_ext.h), in this same translation unit.
#include "shim_x.cc"
