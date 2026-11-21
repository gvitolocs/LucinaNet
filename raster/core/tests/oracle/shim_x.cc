/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Test-only oracle for raster/core/src/ext.rs (tests/oracle_ext.rs): swgl's span fast paths (swgl_ext.h), called
// through the whole of gl.cc so that blend_pixels, the samplers and swgl's statics are the real ones. Built by
// raster/core/oracle/build.rs as part of the one gl.cc translation unit: shim_t.cc includes this file after gl.cc,
// and its helpers are in namespace x so they do not meet shim_t.cc's.
//
// The swgl_ext.h macros only expand inside a FragmentShaderImpl member (they use swgl_OutRGBA8, swgl_OutR8,
// swgl_SpanLength, gl_FragCoord), so each wrapper sets up swgl's statics from an XState, points XShader's span at
// the caller's buffer and calls one XShader member. It returns the span length left, or -1000 if the span pointer
// did not advance by the pixels drawn.
//
// Not exported, because the macros do not compile wherever they are used (so no shader uses them):
// swgl_commitGradientColorRGBA8 names the nonexistent `swgl_OutRGBA`, and swgl_commitTextureLinearColorR8's
// nearest branch has no applyColor(PackedR8, WideR8).

namespace x {

struct XTex {
  const void* buf;
  uint32_t stride;
  uint32_t height;
  uint32_t width;
  uint32_t format;
  uint32_t filter;
};

struct XState {
  uint32_t blend_key;
  int32_t clip_flags;
  uint16_t blend_color[16];
  uint16_t blend_alpha[16];
  uint16_t blendcolor[8];
  const uint8_t* clip_mask_buf;
  int32_t opaque_start;
  uint32_t opaque_size;
  float left_aa_dist[4];
  float right_aa_dist[4];
  float aa_slope[4];
  float frag_coord[16];
  float secondary[16];
};

static ALWAYS_INLINE Float ldf(const float* p) { return Float{p[0], p[1], p[2], p[3]}; }
static ALWAYS_INLINE vec2 ldv2(const float* p) { return vec2(ldf(p), ldf(p + 4)); }
static ALWAYS_INLINE vec4 ldv4(const float* p) { return vec4(ldf(p), ldf(p + 4), ldf(p + 8), ldf(p + 12)); }
static ALWAYS_INLINE vec2_scalar lds2(const float* p) { return vec2_scalar{p[0], p[1]}; }
static ALWAYS_INLINE vec3_scalar lds3(const float* p) { return vec3_scalar{p[0], p[1], p[2]}; }
static ALWAYS_INLINE vec4_scalar lds4(const float* p) { return vec4_scalar{p[0], p[1], p[2], p[3]}; }
static ALWAYS_INLINE mat3_scalar ldm3(const float* p) {
  return mat3_scalar(lds3(p), lds3(p + 3), lds3(p + 6));
}
static ALWAYS_INLINE WideRGBA8 ldw(const uint16_t* p) {
  WideRGBA8 w;
  memcpy(&w, p, sizeof(w));
  return w;
}

template <typename S>
static void fill(S* s, const XTex* t) {
  s->buf = (uint32_t*)t->buf;
  s->stride = t->stride;
  s->height = t->height;
  s->width = t->width;
  s->format = TextureFormat(t->format);
  s->filter = TextureFilter(t->filter);
}

struct XShader : FragmentShaderImpl {
  // Solid and color chunks.
  void solidRGBA8(vec4_scalar v) { swgl_commitSolidRGBA8(v); }
  void solidR8(float v) { swgl_commitSolidR8(v); }
  void partialSolidRGBA8(int n, vec4_scalar v) { swgl_commitPartialSolidRGBA8(n, v); }
  void partialSolidR8(int n, float v) { swgl_commitPartialSolidR8(n, v); }
  void colorRGBA8(vec4 c) { swgl_commitColorRGBA8(c); }
  void colorR8(Float c) { swgl_commitColorR8(c); }

  // Linear.
  template <typename S> void linRGBA8(S s, vec2 p, vec4_scalar r) { swgl_commitTextureLinearRGBA8(s, p, r); }
  template <typename S> void linR8(S s, vec2 p, vec4_scalar r) { swgl_commitTextureLinearR8(s, p, r); }
  template <typename S> void partLinR8(int n, S s, vec2 p, vec4_scalar r) {
    swgl_commitPartialTextureLinearR8(n, s, p, r);
  }
  template <typename S> void partLinInvR8(int n, S s, vec2 p, vec4_scalar r) {
    swgl_commitPartialTextureLinearInvertR8(n, s, p, r);
  }
  template <typename S> void linColorRGBA8(S s, vec2 p, vec4_scalar r, vec4_scalar c) {
    swgl_commitTextureLinearColorRGBA8(s, p, r, c);
  }
  template <typename S> void linColorRGBA8f(S s, vec2 p, vec4_scalar r, float c) {
    swgl_commitTextureLinearColorRGBA8(s, p, r, c);
  }
  template <typename S> void linR8ToRGBA8(S s, vec2 p, vec4_scalar r) {
    swgl_commitTextureLinearR8ToRGBA8(s, p, r);
  }
  template <typename S> void linColorR8ToRGBA8(S s, vec2 p, vec4_scalar r, vec4_scalar c) {
    swgl_commitTextureLinearColorR8ToRGBA8(s, p, r, c);
  }
  template <typename S>
  void linRepRGBA8(S s, vec2 p, vec2_scalar t, vec4_scalar rep, vec4_scalar r) {
    swgl_commitTextureLinearRepeatRGBA8(s, p, t, rep, r);
  }
  template <typename S>
  void linRepColorRGBA8(S s, vec2 p, vec2_scalar t, vec4_scalar rep, vec4_scalar r, vec4_scalar c) {
    swgl_commitTextureLinearRepeatColorRGBA8(s, p, t, rep, r, c);
  }

  // Nearest and by-filter.
  template <typename S> void nearRGBA8(S s, vec2 p, vec4_scalar r) { swgl_commitTextureNearestRGBA8(s, p, r); }
  template <typename S> void nearColorRGBA8(S s, vec2 p, vec4_scalar r, vec4_scalar c) {
    swgl_commitTextureNearestColorRGBA8(s, p, r, c);
  }
  template <typename S>
  void nearRepRGBA8(S s, vec2 p, vec2_scalar t, vec4_scalar rep, vec4_scalar r) {
    swgl_commitTextureNearestRepeatRGBA8(s, p, t, rep, r);
  }
  template <typename S>
  void nearRepColorRGBA8(S s, vec2 p, vec2_scalar t, vec4_scalar rep, vec4_scalar r, vec4_scalar c) {
    swgl_commitTextureNearestRepeatColorRGBA8(s, p, t, rep, r, c);
  }
  template <typename S> void texRGBA8(S s, vec2 p, vec4_scalar r) { swgl_commitTextureRGBA8(s, p, r); }
  template <typename S> void texColorRGBA8(S s, vec2 p, vec4_scalar r, vec4_scalar c) {
    swgl_commitTextureColorRGBA8(s, p, r, c);
  }
  template <typename S>
  void texRepRGBA8(S s, vec2 p, vec2_scalar t, vec4_scalar rep, vec4_scalar r) {
    swgl_commitTextureRepeatRGBA8(s, p, t, rep, r);
  }
  template <typename S>
  void texRepColorRGBA8(S s, vec2 p, vec2_scalar t, vec4_scalar rep, vec4_scalar r, vec4_scalar c) {
    swgl_commitTextureRepeatColorRGBA8(s, p, t, rep, r, c);
  }

  // Blur.
  void blurRGBA8(sampler2D s, vec2 p, vec4_scalar r, bool h, int rad, vec2_scalar c) {
    swgl_commitGaussianBlurRGBA8(s, p, r, h, rad, c);
  }
  void blurR8(sampler2D s, vec2 p, vec4_scalar r, bool h, int rad, vec2_scalar c) {
    swgl_commitGaussianBlurR8(s, p, r, h, rad, c);
  }

  // YUV.
  template <typename S>
  void yuv1(S s0, vec2 p0, vec4_scalar r0, vec3_scalar b, mat3_scalar m, int k) {
    swgl_commitTextureLinearYUV(s0, p0, r0, b, m, k);
  }
  template <typename S>
  void yuv2(S s0, vec2 p0, vec4_scalar r0, S s1, vec2 p1, vec4_scalar r1, vec3_scalar b, mat3_scalar m, int k) {
    swgl_commitTextureLinearYUV(s0, p0, r0, s1, p1, r1, b, m, k);
  }
  template <typename S>
  void yuv3(S s0, vec2 p0, vec4_scalar r0, S s1, vec2 p1, vec4_scalar r1, S s2, vec2 p2, vec4_scalar r2,
            vec3_scalar b, mat3_scalar m, int k) {
    swgl_commitTextureLinearYUV(s0, p0, r0, s1, p1, r1, s2, p2, r2, b, m, k);
  }
  template <typename S>
  void cyuv1(S s0, vec2 p0, vec4_scalar r0, vec3_scalar b, mat3_scalar m, int k, vec4_scalar c) {
    swgl_commitTextureLinearColorYUV(s0, p0, r0, b, m, k, c);
  }
  template <typename S>
  void cyuv2(S s0, vec2 p0, vec4_scalar r0, S s1, vec2 p1, vec4_scalar r1, vec3_scalar b, mat3_scalar m, int k,
             vec4_scalar c) {
    swgl_commitTextureLinearColorYUV(s0, p0, r0, s1, p1, r1, b, m, k, c);
  }
  template <typename S>
  void cyuv3(S s0, vec2 p0, vec4_scalar r0, S s1, vec2 p1, vec4_scalar r1, S s2, vec2 p2, vec4_scalar r2,
             vec3_scalar b, mat3_scalar m, int k, vec4_scalar c) {
    swgl_commitTextureLinearColorYUV(s0, p0, r0, s1, p1, r1, s2, p2, r2, b, m, k, c);
  }

  // Gradients.
  void gradient(sampler2D s, int a, Float e) { swgl_commitGradientRGBA8(s, a, e); }
  void linGrad(sampler2D s, int a, float size, bool tr, bool gr, vec2 pos, vec2_scalar dir, float so) {
    swgl_commitLinearGradientRGBA8(s, a, size, tr, gr, pos, dir, so);
  }
  void ditherLinGrad(sampler2D s, int a, float size, bool tr, bool gr, vec2 pos, vec2_scalar dir, float so) {
    swgl_commitDitheredLinearGradientRGBA8(s, a, size, tr, gr, pos, dir, so);
  }
  void linGradStops(sampler2D s, int oa, int ca, float size, bool gr, vec2 pos, vec2_scalar dir, float so) {
    swgl_commitLinearGradientFromStopsRGBA8(s, oa, ca, size, gr, pos, dir, so);
  }
  void ditherLinGradStops(sampler2D s, int oa, int ca, float size, bool gr, vec2 pos, vec2_scalar dir, float so) {
    vec4 fragCoord = gl_FragCoord;
    swgl_commitDitheredLinearGradientFromStopsRGBA8(s, oa, ca, size, false, gr, pos, dir, so);
  }
  void radGrad(sampler2D s, int a, float size, bool rep, vec2 pos, float radius) {
    swgl_commitRadialGradientRGBA8(s, a, size, rep, pos, radius);
  }
  void ditherRadGrad(sampler2D s, int a, float size, bool rep, vec2 pos, float radius) {
    swgl_commitDitheredRadialGradientRGBA8(s, a, size, rep, pos, radius);
  }
  void radGradStops(sampler2D s, int oa, int ca, float size, bool rep, vec2 pos, float sr) {
    swgl_commitRadialGradientFromStopsRGBA8(s, oa, ca, size, rep, pos, sr);
  }
  void ditherRadGradStops(sampler2D s, int oa, int ca, float size, bool rep, vec2 pos, float sr) {
    swgl_commitDitheredRadialGradientFromStopsRGBA8(s, oa, ca, size, rep, pos, sr);
  }

  // State intrinsics.
  void clipMask(sampler2D mask, vec2_scalar off, vec2_scalar origin, vec2_scalar size) {
    swgl_clipMask(mask, off, origin, size);
  }
  template <typename E> void antiAlias(E e) { swgl_antiAlias(e); }
  void dropShadow(vec4_scalar c) { swgl_blendDropShadow(c); }
  void subpixelText(vec4_scalar c) { swgl_blendSubpixelText(c); }
};

static XShader XS_;
static Context* x_ctx = nullptr;

static void setup(const XState* st, void* span_buf, bool rgba8) {
  if (!x_ctx) {
    x_ctx = CreateContext();
    MakeCurrent(x_ctx);
  }
  blend_key = BlendKey(st->blend_key);
  swgl_ClipFlags = st->clip_flags;
  swgl_BlendColorRGBA8 = ldw(st->blend_color);
  swgl_BlendAlphaRGBA8 = ldw(st->blend_alpha);
  memcpy(&ctx->blendcolor, st->blendcolor, sizeof(ctx->blendcolor));
  swgl_SpanBuf = span_buf;
  swgl_ClipMaskBuf = (uint8_t*)st->clip_mask_buf;
  swgl_OpaqueStart = (const uint8_t*)span_buf + std::ptrdiff_t(st->opaque_start) * (rgba8 ? 4 : 1);
  swgl_OpaqueSize = st->opaque_size;
  swgl_LeftAADist = ldf(st->left_aa_dist);
  swgl_RightAADist = ldf(st->right_aa_dist);
  swgl_AASlope = ldf(st->aa_slope);
  XS_.gl_FragCoord = ldv4(st->frag_coord);
  XS_.gl_SecondaryFragColor = ldv4(st->secondary);
  fragment_shader = &XS_;
}

static int32_t finish32(uint32_t* buf, int32_t len) {
  return XS_.swgl_OutRGBA8 - buf == len - XS_.swgl_SpanLength ? XS_.swgl_SpanLength : -1000;
}
static int32_t finish8(uint8_t* buf, int32_t len) {
  return XS_.swgl_OutR8 - buf == len - XS_.swgl_SpanLength ? XS_.swgl_SpanLength : -1000;
}

#define BEGIN32                \
  setup(st, buf, true);        \
  XS_.swgl_OutRGBA8 = buf;       \
  XS_.swgl_OutR8 = nullptr;      \
  XS_.swgl_SpanLength = len;
#define BEGIN8                 \
  setup(st, buf, false);       \
  XS_.swgl_OutR8 = buf;          \
  XS_.swgl_OutRGBA8 = nullptr;   \
  XS_.swgl_SpanLength = len;

// `call` with S_ bound to the XTex `t` as a sampler2D (rect == 0) or a sampler2DRect (rect != 0).
#define WITH_SAMPLER(t, rect, call)   \
  do {                                \
    if (rect) {                       \
      sampler2DRect_impl i_;          \
      fill(&i_, t);                   \
      sampler2DRect S_ = &i_;         \
      call;                           \
    } else {                          \
      sampler2D_impl i_;              \
      fill(&i_, t);                   \
      sampler2D S_ = &i_;             \
      call;                           \
    }                                 \
  } while (0)

#define WITH_SAMPLERS3(t0, t1, t2, rect, call)                                    \
  do {                                                                            \
    if (rect) {                                                                   \
      sampler2DRect_impl i0_, i1_, i2_;                                           \
      fill(&i0_, t0);                                                             \
      fill(&i1_, t1);                                                             \
      fill(&i2_, t2);                                                             \
      sampler2DRect S0_ = &i0_, S1_ = &i1_, S2_ = &i2_;                           \
      call;                                                                       \
    } else {                                                                      \
      sampler2D_impl i0_, i1_, i2_;                                               \
      fill(&i0_, t0);                                                             \
      fill(&i1_, t1);                                                             \
      fill(&i2_, t2);                                                             \
      sampler2D S0_ = &i0_, S1_ = &i1_, S2_ = &i2_;                               \
      call;                                                                       \
    }                                                                             \
  } while (0)

static sampler2D tex2d(sampler2D_impl* i, const XTex* t) {
  fill(i, t);
  return i;
}

// ---- solid and color --------------------------------------------------------------------------------------------

EXPORT int32_t x_commitSolidRGBA8(const XState* st, uint32_t* buf, int32_t len, const float* v) {
  BEGIN32 XS_.solidRGBA8(lds4(v));
  return finish32(buf, len);
}
EXPORT int32_t x_commitSolidR8(const XState* st, uint8_t* buf, int32_t len, float v) {
  BEGIN8 XS_.solidR8(v);
  return finish8(buf, len);
}
EXPORT int32_t x_commitPartialSolidRGBA8(const XState* st, uint32_t* buf, int32_t len, int32_t n, const float* v) {
  BEGIN32 XS_.partialSolidRGBA8(n, lds4(v));
  return finish32(buf, len);
}
EXPORT int32_t x_commitPartialSolidR8(const XState* st, uint8_t* buf, int32_t len, int32_t n, float v) {
  BEGIN8 XS_.partialSolidR8(n, v);
  return finish8(buf, len);
}
EXPORT int32_t x_commitColorRGBA8(const XState* st, uint32_t* buf, int32_t len, const float* c) {
  BEGIN32 XS_.colorRGBA8(ldv4(c));
  return finish32(buf, len);
}
EXPORT int32_t x_commitColorR8(const XState* st, uint8_t* buf, int32_t len, const float* c) {
  BEGIN8 XS_.colorR8(ldf(c));
  return finish8(buf, len);
}

// ---- linear -----------------------------------------------------------------------------------------------------

EXPORT int32_t x_commitTextureLinearRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s, int32_t rect,
                                          const float* p, const float* r) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.linRGBA8(S_, ldv2(p), lds4(r)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureLinearR8(const XState* st, uint8_t* buf, int32_t len, const XTex* s, int32_t rect,
                                       const float* p, const float* r) {
  BEGIN8 WITH_SAMPLER(s, rect, XS_.linR8(S_, ldv2(p), lds4(r)));
  return finish8(buf, len);
}
EXPORT int32_t x_commitPartialTextureLinearR8(const XState* st, uint8_t* buf, int32_t len, int32_t n, const XTex* s,
                                              int32_t rect, const float* p, const float* r) {
  BEGIN8 WITH_SAMPLER(s, rect, XS_.partLinR8(n, S_, ldv2(p), lds4(r)));
  return finish8(buf, len);
}
EXPORT int32_t x_commitPartialTextureLinearInvertR8(const XState* st, uint8_t* buf, int32_t len, int32_t n,
                                                    const XTex* s, int32_t rect, const float* p, const float* r) {
  BEGIN8 WITH_SAMPLER(s, rect, XS_.partLinInvR8(n, S_, ldv2(p), lds4(r)));
  return finish8(buf, len);
}
EXPORT int32_t x_commitTextureLinearColorRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s,
                                               int32_t rect, const float* p, const float* r, const float* c) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.linColorRGBA8(S_, ldv2(p), lds4(r), lds4(c)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureLinearColorRGBA8f(const XState* st, uint32_t* buf, int32_t len, const XTex* s,
                                                int32_t rect, const float* p, const float* r, float c) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.linColorRGBA8f(S_, ldv2(p), lds4(r), c));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureLinearR8ToRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s,
                                              int32_t rect, const float* p, const float* r) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.linR8ToRGBA8(S_, ldv2(p), lds4(r)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureLinearColorR8ToRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s,
                                                   int32_t rect, const float* p, const float* r, const float* c) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.linColorR8ToRGBA8(S_, ldv2(p), lds4(r), lds4(c)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureLinearRepeatRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s,
                                                int32_t rect, const float* p, const float* t, const float* rep,
                                                const float* r) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.linRepRGBA8(S_, ldv2(p), lds2(t), lds4(rep), lds4(r)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureLinearRepeatColorRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s,
                                                     int32_t rect, const float* p, const float* t, const float* rep,
                                                     const float* r, const float* c) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.linRepColorRGBA8(S_, ldv2(p), lds2(t), lds4(rep), lds4(r), lds4(c)));
  return finish32(buf, len);
}

// ---- nearest and by filter --------------------------------------------------------------------------------------

EXPORT int32_t x_commitTextureNearestRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s, int32_t rect,
                                           const float* p, const float* r) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.nearRGBA8(S_, ldv2(p), lds4(r)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureNearestColorRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s,
                                                int32_t rect, const float* p, const float* r, const float* c) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.nearColorRGBA8(S_, ldv2(p), lds4(r), lds4(c)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureNearestRepeatRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s,
                                                 int32_t rect, const float* p, const float* t, const float* rep,
                                                 const float* r) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.nearRepRGBA8(S_, ldv2(p), lds2(t), lds4(rep), lds4(r)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureNearestRepeatColorRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s,
                                                      int32_t rect, const float* p, const float* t, const float* rep,
                                                      const float* r, const float* c) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.nearRepColorRGBA8(S_, ldv2(p), lds2(t), lds4(rep), lds4(r), lds4(c)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s, int32_t rect,
                                    const float* p, const float* r) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.texRGBA8(S_, ldv2(p), lds4(r)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureColorRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s, int32_t rect,
                                         const float* p, const float* r, const float* c) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.texColorRGBA8(S_, ldv2(p), lds4(r), lds4(c)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureRepeatRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s, int32_t rect,
                                          const float* p, const float* t, const float* rep, const float* r) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.texRepRGBA8(S_, ldv2(p), lds2(t), lds4(rep), lds4(r)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureRepeatColorRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s,
                                               int32_t rect, const float* p, const float* t, const float* rep,
                                               const float* r, const float* c) {
  BEGIN32 WITH_SAMPLER(s, rect, XS_.texRepColorRGBA8(S_, ldv2(p), lds2(t), lds4(rep), lds4(r), lds4(c)));
  return finish32(buf, len);
}

// ---- blur -------------------------------------------------------------------------------------------------------

EXPORT int32_t x_commitGaussianBlurRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s, const float* p,
                                         const float* r, int32_t hori, int32_t radius, const float* coeffs) {
  BEGIN32 sampler2D_impl i;
  XS_.blurRGBA8(tex2d(&i, s), ldv2(p), lds4(r), hori != 0, radius, lds2(coeffs));
  return finish32(buf, len);
}
EXPORT int32_t x_commitGaussianBlurR8(const XState* st, uint8_t* buf, int32_t len, const XTex* s, const float* p,
                                      const float* r, int32_t hori, int32_t radius, const float* coeffs) {
  BEGIN8 sampler2D_impl i;
  XS_.blurR8(tex2d(&i, s), ldv2(p), lds4(r), hori != 0, radius, lds2(coeffs));
  return finish8(buf, len);
}

// ---- YUV --------------------------------------------------------------------------------------------------------

EXPORT int32_t x_commitTextureLinearYUV1(const XState* st, uint32_t* buf, int32_t len, const XTex* s0, int32_t rect,
                                         const float* p0, const float* r0, const float* b, const float* m,
                                         int32_t k) {
  BEGIN32 WITH_SAMPLER(s0, rect, XS_.yuv1(S_, ldv2(p0), lds4(r0), lds3(b), ldm3(m), k));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureLinearYUV2(const XState* st, uint32_t* buf, int32_t len, const XTex* s0, const XTex* s1,
                                         int32_t rect, const float* p0, const float* r0, const float* p1,
                                         const float* r1, const float* b, const float* m, int32_t k) {
  BEGIN32 WITH_SAMPLERS3(s0, s1, s1, rect,
                         XS_.yuv2(S0_, ldv2(p0), lds4(r0), S1_, ldv2(p1), lds4(r1), lds3(b), ldm3(m), k));
  return finish32(buf, len);
}
// The stages of blendYUV for R16 + RG16 planes on the first chunk (tests/oracle_ext.rs, texture_yuv_stages): taps
// [x0..x3, y0..y3] of both planes, raw Y and UV samples, the samples shifted by the colour depth, the six YUVMatrix
// vectors and the converted pixels.
template <typename S>
static void yuv2_r16_stages(S sampler0, S sampler1, const float* p0, const float* r0, const float* p1,
                            const float* r1, const float* b, const float* m, int32_t k, int32_t* taps,
                            int16_t* raw, int16_t* shifted, int16_t* mat, uint8_t* px) {
  vec2 uv0 = ldv2(p0), uv1 = ldv2(p1);
  vec4_scalar uv_rect0 = lds4(r0), uv_rect1 = lds4(r1);
  LINEAR_QUANTIZE_UV(sampler0, uv0, uv_step0, uv_rect0, min_uv0, max_uv0);
  LINEAR_QUANTIZE_UV(sampler1, uv1, uv_step1, uv_rect1, min_uv1, max_uv1);
  (void)uv_step0;
  (void)uv_step1;
  ivec2 i0 = ivec2(clamp(uv0, min_uv0, max_uv0));
  ivec2 i1 = ivec2(clamp(uv1, min_uv1, max_uv1));
  memcpy(taps, &i0.x, 16);
  memcpy(taps + 4, &i0.y, 16);
  memcpy(taps + 8, &i1.x, 16);
  memcpy(taps + 12, &i1.y, 16);
  auto y = textureLinearUnpackedR16(sampler0, i0);
  auto uv = textureLinearUnpackedRG16(sampler1, i1);
  memcpy(raw, &y, 8);
  memcpy(raw + 4, &uv, 16);
  int rescaleBits = (16 - k - 1) - 8;
  y = y >> rescaleBits;
  uv = uv >> rescaleBits;
  memcpy(shifted, &y, 8);
  memcpy(shifted + 4, &uv, 16);
  const auto M = YUVMatrix::From(lds3(b), ldm3(m), k);
  memcpy(mat, &M, 96);
  PackedRGBA8 out = M.convert(zip(y, y), uv);
  memcpy(px, &out, 16);
}
EXPORT void x_yuv2_r16_stages(const XTex* s0, const XTex* s1, int32_t rect, const float* p0, const float* r0,
                              const float* p1, const float* r1, const float* b, const float* m, int32_t k,
                              int32_t* taps, int16_t* raw, int16_t* shifted, int16_t* mat, uint8_t* px) {
  WITH_SAMPLERS3(s0, s1, s1, rect, yuv2_r16_stages(S0_, S1_, p0, r0, p1, r1, b, m, k, taps, raw, shifted, mat, px));
}
EXPORT int32_t x_commitTextureLinearYUV3(const XState* st, uint32_t* buf, int32_t len, const XTex* s0, const XTex* s1,
                                         const XTex* s2, int32_t rect, const float* p0, const float* r0,
                                         const float* p1, const float* r1, const float* p2, const float* r2,
                                         const float* b, const float* m, int32_t k) {
  BEGIN32 WITH_SAMPLERS3(s0, s1, s2, rect,
                         XS_.yuv3(S0_, ldv2(p0), lds4(r0), S1_, ldv2(p1), lds4(r1), S2_, ldv2(p2), lds4(r2), lds3(b),
                                ldm3(m), k));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureLinearColorYUV1(const XState* st, uint32_t* buf, int32_t len, const XTex* s0,
                                              int32_t rect, const float* p0, const float* r0, const float* b,
                                              const float* m, int32_t k, const float* c) {
  BEGIN32 WITH_SAMPLER(s0, rect, XS_.cyuv1(S_, ldv2(p0), lds4(r0), lds3(b), ldm3(m), k, lds4(c)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureLinearColorYUV2(const XState* st, uint32_t* buf, int32_t len, const XTex* s0,
                                              const XTex* s1, int32_t rect, const float* p0, const float* r0,
                                              const float* p1, const float* r1, const float* b, const float* m,
                                              int32_t k, const float* c) {
  BEGIN32 WITH_SAMPLERS3(s0, s1, s1, rect,
                         XS_.cyuv2(S0_, ldv2(p0), lds4(r0), S1_, ldv2(p1), lds4(r1), lds3(b), ldm3(m), k, lds4(c)));
  return finish32(buf, len);
}
EXPORT int32_t x_commitTextureLinearColorYUV3(const XState* st, uint32_t* buf, int32_t len, const XTex* s0,
                                              const XTex* s1, const XTex* s2, int32_t rect, const float* p0,
                                              const float* r0, const float* p1, const float* r1, const float* p2,
                                              const float* r2, const float* b, const float* m, int32_t k,
                                              const float* c) {
  BEGIN32 WITH_SAMPLERS3(s0, s1, s2, rect,
                         XS_.cyuv3(S0_, ldv2(p0), lds4(r0), S1_, ldv2(p1), lds4(r1), S2_, ldv2(p2), lds4(r2),
                                 lds3(b), ldm3(m), k, lds4(c)));
  return finish32(buf, len);
}

// ---- gradients --------------------------------------------------------------------------------------------------

EXPORT int32_t x_commitGradientRGBA8(const XState* st, uint32_t* buf, int32_t len, const XTex* s, int32_t a,
                                     const float* e) {
  BEGIN32 sampler2D_impl i;
  XS_.gradient(tex2d(&i, s), a, ldf(e));
  return finish32(buf, len);
}

#define LINGRAD(name, member)                                                                                   \
  EXPORT int32_t name(const XState* st, uint32_t* buf, int32_t len, const XTex* s, int32_t a, float size,         \
                      int32_t tr, int32_t gr, const float* pos, const float* dir, float so) {                   \
    BEGIN32 sampler2D_impl i;                                                                                   \
    XS_.member(tex2d(&i, s), a, size, tr != 0, gr != 0, ldv2(pos), lds2(dir), so);                                \
    return finish32(buf, len);                                                                                  \
  }
LINGRAD(x_commitLinearGradientRGBA8, linGrad)
LINGRAD(x_commitDitheredLinearGradientRGBA8, ditherLinGrad)

#define LINGRADSTOPS(name, member)                                                                              \
  EXPORT int32_t name(const XState* st, uint32_t* buf, int32_t len, const XTex* s, int32_t oa, int32_t ca,       \
                      float size, int32_t gr, const float* pos, const float* dir, float so) {                   \
    BEGIN32 sampler2D_impl i;                                                                                   \
    XS_.member(tex2d(&i, s), oa, ca, size, gr != 0, ldv2(pos), lds2(dir), so);                                    \
    return finish32(buf, len);                                                                                  \
  }
LINGRADSTOPS(x_commitLinearGradientFromStopsRGBA8, linGradStops)
LINGRADSTOPS(x_commitDitheredLinearGradientFromStopsRGBA8, ditherLinGradStops)

#define RADGRAD(name, member)                                                                                   \
  EXPORT int32_t name(const XState* st, uint32_t* buf, int32_t len, const XTex* s, int32_t a, float size,         \
                      int32_t rep, const float* pos, float radius) {                                            \
    BEGIN32 sampler2D_impl i;                                                                                   \
    XS_.member(tex2d(&i, s), a, size, rep != 0, ldv2(pos), radius);                                               \
    return finish32(buf, len);                                                                                  \
  }
RADGRAD(x_commitRadialGradientRGBA8, radGrad)
RADGRAD(x_commitDitheredRadialGradientRGBA8, ditherRadGrad)

#define RADGRADSTOPS(name, member)                                                                              \
  EXPORT int32_t name(const XState* st, uint32_t* buf, int32_t len, const XTex* s, int32_t oa, int32_t ca,       \
                      float size, int32_t rep, const float* pos, float sr) {                                    \
    BEGIN32 sampler2D_impl i;                                                                                   \
    XS_.member(tex2d(&i, s), oa, ca, size, rep != 0, ldv2(pos), sr);                                              \
    return finish32(buf, len);                                                                                  \
  }
RADGRADSTOPS(x_commitRadialGradientFromStopsRGBA8, radGradStops)
RADGRADSTOPS(x_commitDitheredRadialGradientFromStopsRGBA8, ditherRadGradStops)

// ---- no span ----------------------------------------------------------------------------------------------------

EXPORT int32_t x_validateGradient(const XTex* s, int32_t ax, int32_t ay, int32_t entries) {
  sampler2D_impl i;
  return swgl_validateGradient(tex2d(&i, s), ivec2_scalar{ax, ay}, entries);
}
EXPORT int32_t x_validateGradientFromStops(const XTex* s, int32_t ax, int32_t ay, int32_t entries) {
  sampler2D_impl i;
  return swgl_validateGradientFromStops(tex2d(&i, s), ivec2_scalar{ax, ay}, entries);
}
EXPORT int32_t x_isTexture(const XTex* s, int32_t rect, int32_t which) {
  int32_t r = 0;
  WITH_SAMPLER(s, rect,
               r = which == 0   ? swgl_isTextureLinear(S_)
                   : which == 1 ? swgl_isTextureRGBA8(S_)
                                : swgl_isTextureR8(S_));
  return r;
}

struct XClip {
  int32_t clip_flags;
  int32_t offset[2];
  int32_t bounds[4];
  const void* mask_buf;
};
EXPORT void x_clipMask(int32_t flags_in, const XTex* mask, const float* off, const float* origin, const float* size,
                       XClip* out) {
  static sampler2D_impl m;
  fill(&m, mask);
  swgl_ClipFlags = flags_in;
  swgl_ClipMask = nullptr;
  swgl_ClipMaskOffset = {0, 0};
  swgl_ClipMaskBounds = {0, 0, 0, 0};
  XS_.clipMask(&m, lds2(off), lds2(origin), lds2(size));
  out->clip_flags = swgl_ClipFlags;
  out->offset[0] = swgl_ClipMaskOffset.x;
  out->offset[1] = swgl_ClipMaskOffset.y;
  out->bounds[0] = swgl_ClipMaskBounds.x0;
  out->bounds[1] = swgl_ClipMaskBounds.y0;
  out->bounds[2] = swgl_ClipMaskBounds.x1;
  out->bounds[3] = swgl_ClipMaskBounds.y1;
  out->mask_buf = swgl_ClipMask ? swgl_ClipMask->buf : nullptr;
}
EXPORT void x_antiAlias(int32_t flags_in, int32_t kind, int32_t edges, int32_t* out_flags, int32_t* out_mask) {
  swgl_ClipFlags = flags_in;
  swgl_AAEdgeMask = 0;
  if (kind == 0) {
    XS_.antiAlias(int(edges));
  } else if (kind == 1) {
    XS_.antiAlias(edges != 0);
  } else {
    XS_.antiAlias(bvec4_scalar{(edges & 1) != 0, (edges & 2) != 0, (edges & 4) != 0, (edges & 8) != 0});
  }
  *out_flags = swgl_ClipFlags;
  *out_mask = swgl_AAEdgeMask;
}

struct XOverride {
  int32_t clip_flags;
  uint32_t blend_override;
  uint16_t color[16];
  uint16_t alpha[16];
};
static void override_out(XOverride* out) {
  out->clip_flags = swgl_ClipFlags;
  out->blend_override = swgl_BlendOverride;
  memcpy(out->color, &swgl_BlendColorRGBA8, sizeof(out->color));
  memcpy(out->alpha, &swgl_BlendAlphaRGBA8, sizeof(out->alpha));
}
static void override_reset(int32_t flags_in) {
  swgl_ClipFlags = flags_in;
  swgl_BlendOverride = BLEND_KEY_NONE;
  swgl_BlendColorRGBA8 = 0;
  swgl_BlendAlphaRGBA8 = 0;
}
EXPORT void x_blendDropShadow(int32_t flags_in, const float* c, XOverride* out) {
  override_reset(flags_in);
  XS_.dropShadow(lds4(c));
  override_out(out);
}
EXPORT void x_blendSubpixelText(int32_t flags_in, const float* c, XOverride* out) {
  override_reset(flags_in);
  XS_.subpixelText(lds4(c));
  override_out(out);
}

}  // namespace x
