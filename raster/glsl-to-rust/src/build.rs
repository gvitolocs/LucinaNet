/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The preprocessing half of swgl's `build.rs`, as functions a build script (raster/core's) or a test can call:
//! the same shader list, the same `#define`s, the same `#include` expansion and the same C preprocessor run, so
//! the Rust is generated from exactly the text glsl-to-cxx translates. The one difference: the `.glsl` files read
//! are returned instead of printed as `cargo:rerun-if-changed`, so the caller decides.

use std::collections::HashSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use webrender_build::shader::{get_shader_features, ShaderFeatureFlags};

/// Every program swgl builds: `"name"` or `"name feature,feature"`, sorted, as swgl's `main` computes them.
pub fn shader_keys() -> Vec<String> {
    let shader_flags = ShaderFeatureFlags::GL
        | ShaderFeatureFlags::DUAL_SOURCE_BLENDING
        | ShaderFeatureFlags::ADVANCED_BLEND_EQUATION
        | ShaderFeatureFlags::DEBUG;
    let mut shaders: Vec<String> = Vec::new();
    for (name, features) in get_shader_features(shader_flags) {
        shaders.extend(features.iter().map(|f| if f.is_empty() { name.to_owned() } else { format!("{name} {f}") }));
    }
    shaders.sort();
    shaders
}

// Shader key is in "name feature,feature" format.
// File name needs to be formatted as "name_feature_feature".
pub fn shader_file(shader_key: &str) -> String {
    shader_key.replace([' ', ','], "_")
}

fn process_imports(
    shader_dir: &Path,
    shader: &str,
    included: &mut HashSet<String>,
    output: &mut String,
    sources: &mut Vec<PathBuf>,
) {
    if !included.insert(shader.into()) {
        return;
    }
    let path = shader_dir.join(format!("{shader}.glsl"));
    let source = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {}", path.display(), e));
    sources.push(path);
    for line in source.lines() {
        if let Some(imports) = line.strip_prefix("#include ") {
            let imports = imports.split(',');
            for import in imports {
                process_imports(shader_dir, import, included, output, sources);
            }
        } else if line.starts_with("#version ") || line.starts_with("#extension ") {
            // ignore
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
}

/// The preprocessed vertex and fragment shader of one program, and the `.glsl` files they came from.
pub struct Preprocessed {
    pub vert: PathBuf,
    pub frag: PathBuf,
    pub sources: Vec<PathBuf>,
}

/// Writes `<out_dir>/<file>.c`, `<file>.vert` and `<file>.frag` for `shader_key`, as swgl's `translate_shader`
/// does before calling the translator.
pub fn preprocess(shader_key: &str, shader_dir: &Path, out_dir: &Path) -> Preprocessed {
    let mut imported = String::from("#define SWGL 1\n#define __VERSION__ 150\n");
    let _ = writeln!(imported, "#define WR_MAX_VERTEX_TEXTURE_WIDTH {}U", webrender_build::MAX_VERTEX_TEXTURE_WIDTH);

    let (basename, features) = shader_key.split_at(shader_key.find(' ').unwrap_or(shader_key.len()));
    if !features.is_empty() {
        for feature in features.trim().split(',') {
            let _ = writeln!(imported, "#define WR_FEATURE_{feature}");
        }
    }

    let mut sources = Vec::new();
    process_imports(shader_dir, basename, &mut HashSet::new(), &mut imported, &mut sources);

    let shader = shader_file(shader_key);

    std::fs::create_dir_all(out_dir).unwrap();
    let imp_name = out_dir.join(format!("{shader}.c"));
    std::fs::write(&imp_name, imported).unwrap();

    // We need to ensure that the C preprocessor does not pull compiler flags from the host or
    // target environment. Set all `CFLAGS` or `CXXFLAGS` env. vars. to empty to work around this.
    let _suppressed: Vec<EnvVarGuard> = cflags_env_vars()
        .map(|(key, value)| {
            std::env::set_var(&key, "");
            EnvVarGuard { key, old_value: Some(value) }
        })
        .collect();

    let mut build = cc::Build::new();
    build.no_default_flags(true).cargo_metadata(false).warnings(false);
    if std::env::var_os("TARGET").is_none() {
        // Outside a build script (a test, the binary): cc needs the triple cargo would have passed.
        let t = env!("GLSL_TO_RUST_TARGET");
        build.target(t).host(t).opt_level(0);
    }
    if let Ok(tool) = build.try_get_compiler() {
        if tool.is_like_msvc() {
            build.flag("/EP");
            if tool.path().to_str().is_some_and(|p| p.contains("clang")) {
                build.flag("/clang:-undef");
            } else {
                build.flag("/u");
            }
        } else {
            build.flag("-xc").flag("-P").flag("-undef");
        }
    }
    build.file(&imp_name);
    let vs = build.clone().define("WR_VERTEX_SHADER", Some("1")).expand();
    let fs = build.clone().define("WR_FRAGMENT_SHADER", Some("1")).expand();
    let vert = out_dir.join(format!("{shader}.vert"));
    let frag = out_dir.join(format!("{shader}.frag"));
    std::fs::write(&vert, vs).unwrap();
    std::fs::write(&frag, fs).unwrap();
    Preprocessed { vert, frag, sources }
}

/// Preprocesses and translates one program. Returns the Rust module's source and the `.glsl` files read.
pub fn translate_program(shader_key: &str, shader_dir: &Path, out_dir: &Path) -> (String, Vec<PathBuf>) {
    let p = preprocess(shader_key, shader_dir, out_dir);
    let args =
        vec!["glsl_to_rust".to_string(), p.vert.to_string_lossy().into_owned(), p.frag.to_string_lossy().into_owned()];
    (crate::translate(&mut args.into_iter()), p.sources)
}

/// Translates every program into `<out_dir>/<file>.rs` and writes `<out_dir>/shaders.rs`: one `mod` per program
/// (absolute `#[path]`s, so the file can be `include!`d from anywhere in the runtime crate) and `load_shader`,
/// the Rust form of the `load_shader.h` swgl's build script writes. Returns every `.glsl` file read.
pub fn write_shaders(shader_dir: &Path, out_dir: &Path) -> Vec<PathBuf> {
    let keys = shader_keys();
    let pre = out_dir.join("pre");
    let mut sources = Vec::new();
    let mut mods = String::new();
    let mut loads = String::new();
    for key in &keys {
        let file = shader_file(key);
        let (code, srcs) = translate_program(key, shader_dir, &pre);
        let path = out_dir.join(format!("{file}.rs"));
        std::fs::write(&path, code).unwrap();
        for s in srcs {
            if !sources.contains(&s) {
                sources.push(s);
            }
        }
        let _ = writeln!(mods, "#[path = {:?}]\npub mod {file};", path.to_string_lossy());
        let _ = writeln!(loads, "        {key:?} => Some({file}::{file}_program::loader),");
    }
    let shaders = format!(
        "// Generated by glsl-to-rust (`build::write_shaders`): one module per WebRender program.\n\n{mods}\n\
         /// The program for a shader key (`\"brush_solid\"`, `\"brush_image TEXTURE_2D\"`), as swgl's `load_shader`.\n\
         pub fn load_shader(name: &str) -> Option<crate::program::ProgramLoader> {{\n    match name {{\n{loads}        \
         _ => None,\n    }}\n}}\n"
    );
    std::fs::write(out_dir.join("shaders.rs"), shaders).unwrap();
    sources
}

struct EnvVarGuard {
    key: std::ffi::OsString,
    old_value: Option<std::ffi::OsString>,
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        let Self { key, old_value } = &*self;
        if let Some(old_value) = old_value.as_ref() {
            std::env::set_var(key, old_value);
        } else {
            std::env::remove_var(key);
        }
    }
}

fn cflags_env_vars() -> impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)> {
    std::env::vars_os().filter(|(key, _value)| {
        key.to_str().is_some_and(|key_utf8| ["CFLAGS", "CXXFLAGS"].iter().any(|opt_name| key_utf8.contains(opt_name)))
    })
}
