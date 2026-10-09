// build.rs: link the Bergamot translation ENGINE DLL built by
// `cargo xtask build-translate`.
//
// Layout note: the engine ships as translate_engine.dll (+ import lib) rather
// than a static lib on purpose — its bundled protobuf-lite collides with the
// copy inside ort (LNK2005 duplicates + heap corruption when statically linked
// together). The DLL boundary plus the plain-C ABI keeps the two copies
// isolated; the shared /MD CRT keeps one heap for malloc/free across it.
//
// Without the built engine this is a no-op (plain `cargo build` keeps working;
// the Rust side compiles a stub instead, see translate.rs).
use std::path::{Path, PathBuf};

fn dll_name() -> &'static str {
    "translate_engine.dll"
}

fn main() {
    println!("cargo::rerun-if-changed=native/shim/bt_shim.cpp");
    println!("cargo::rerun-if-changed=native/shim/bt_shim.h");
    println!("cargo::rerun-if-changed=native/CMakeLists.txt");
    println!("cargo::rustc-check-cfg=cfg(translate_native)");

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bn = root.join("build-native/translate");
    let dll = bn.join(dll_name());
    let implib = bn.join("translate_engine.lib");
    if !dll.is_file() || !implib.is_file() {
        println!("cargo::warning=translate engine DLL missing (run `cargo xtask build-translate`)");
        return;
    }
    // Gate the real FFI module; without it a stub keeps plain builds working.
    println!("cargo::rustc-cfg=translate_native");
    println!("cargo::rustc-link-search=native={}", bn.display());
    println!("cargo::rustc-link-lib=translate_engine");

    // The DLL must sit next to the binaries that load it (Windows has no
    // rpath): copy beside the host exe and the test exes when stale.
    // Failures here only warn; a stale copy is still loadable.
    let profile = std::env::var("PROFILE").unwrap_or_default();
    deploy_dll(&dll, &root.join("target").join(&profile));
    deploy_dll(&dll, &root.join("target").join(&profile).join("deps"));

    // MKL is statically INSIDE the DLL; nothing leaks to the exe dir.
}

/// Copy src -> dst dir if missing or size differs. Never fails the build.
fn deploy_dll(src: &Path, dir: &Path) {
    if !dir.is_dir() {
        return;
    }
    let dst = dir.join(dll_name());
    let same_len = || {
        let a = std::fs::metadata(&dst).ok()?.len();
        let b = std::fs::metadata(src).ok()?.len();
        Some(a == b && a > 0)
    };
    if same_len() != Some(true) {
        let _ = std::fs::copy(src, &dst);
    }
}
