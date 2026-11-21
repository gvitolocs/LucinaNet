//! Sets `LUCINAJS_BUILD_ID`: a hash of every source file of this crate. On-disk bytecode (the function cache, the script cache)
//! is only valid for the engine build that wrote it, and a package version does not change when opcodes or layouts do.
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let mut files = vec![];
    collect(&root, &mut files);
    files.sort();
    // DefaultHasher::new() has fixed keys, so the id is the same for the same sources.
    let mut h = DefaultHasher::new();
    for file in &files {
        file.strip_prefix(&root).unwrap().hash(&mut h);
        fs::read(file).unwrap().hash(&mut h);
        println!("cargo:rerun-if-changed={}", file.display());
    }
    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rustc-env=LUCINAJS_BUILD_ID={:016x}", h.finish());
}
