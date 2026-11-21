/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Compiles `../tests/oracle/shim.cc` against swgl's headers, and `../tests/oracle/shim_t.cc` (which includes
//! swgl's gl.cc, with a stub `load_shader.h` found first, and includes `shim_x.cc` for the span fast paths) with swgl's own flags (swgl/build.rs, clang branch, release:
//! `NDEBUG`), so the oracle computes what swgl computes. Only built as a dev-dependency of lucinanet-raster.

fn main() {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let shim = manifest.join("../tests/oracle/shim.cc");
    let swgl_src = manifest.join("../../../swgl/src");
    let shim_t = manifest.join("../tests/oracle/shim_t.cc");
    let stub = manifest.join("../tests/oracle/stub");
    for f in [
        "gl_defs.h",
        "glsl.h",
        "program.h",
        "vector_type.h",
        "gl.cc",
        "texture.h",
        "blend.h",
        "composite.h",
        "rasterize.h",
        "swgl_ext.h",
        "lucina_prof.h",
    ] {
        println!("cargo:rerun-if-changed={}", swgl_src.join(f).display());
    }
    println!("cargo:rerun-if-changed={}", shim.display());
    println!("cargo:rerun-if-changed={}", shim_t.display());
    println!("cargo:rerun-if-changed={}", manifest.join("../tests/oracle/shim_x.cc").display());
    println!("cargo:rerun-if-changed={}", stub.join("load_shader.h").display());

    let mut build = oracle_build();
    build.cpp(true).file(&shim).include(&swgl_src);
    strict(&mut build);
    build.compile("swgl_glsl_oracle");

    // gl.cc as swgl/build.rs compiles it for release, plus the strict-float flags.
    let mut build = oracle_build();
    build
        .cpp(true)
        .file(&shim_t)
        .include(&stub)
        .include(&swgl_src)
        .define("NDEBUG", None)
        .define("_GLIBCXX_USE_CXX11_ABI", Some("0"));
    strict(&mut build);
    build.compile("swgl_t_oracle");
}

fn strict(build: &mut cc::Build) {
    let tool = build.get_compiler();
    assert!(tool.is_like_clang(), "the oracle must be built with clang, as swgl is (CXX=clang++)");
    build
        .flag("-std=c++20")
        .flag("-fno-exceptions")
        .flag("-fno-rtti")
        .flag("-fno-math-errno")
        // The oracle is swgl's strict-float build (SWGL_STRICT_FP=1 in swgl/build.rs, raster/DESIGN.md
        // "Correctness bar"): no fast-math, no FMA contraction, so the C++ rounds every operation in source order.
        .flag("-fno-fast-math")
        .flag("-ffp-contract=off")
        .flag("-mrecip=none")
        // swgl is built optimised (its build.rs overrides -Oz with -O2); keep the oracle optimised whatever the test
        // profile, so it runs the same instruction selection as the shipped library.
        .opt_level(3)
        .warnings(false);
}

/// A C++ build that uses clang++ unless the environment names a compiler (CXX, CXX_<target>, HOST_CXX), so a plain
/// `cargo test` (the gate sets no CXX) builds the oracle with the compiler swgl itself uses.
fn oracle_build() -> cc::Build {
    let mut build = cc::Build::new();
    let target = std::env::var("TARGET").unwrap_or_default();
    let named = ["CXX".to_string(), "HOST_CXX".to_string(), format!("CXX_{target}"), format!("CXX_{}", target.replace('-', "_"))]
        .iter()
        .any(|k| std::env::var_os(k).is_some());
    println!("cargo:rerun-if-env-changed=CXX");
    if !named {
        build.compiler("clang++");
    }
    build
}
