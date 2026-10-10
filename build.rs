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
    emit_translate_manifest(&root);
}

/// Read tools/models.lock.json translate.pinned and emit a Rust manifest so
/// the runtime downloader never duplicates the file list.
fn emit_translate_manifest(root: &Path) {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let dest = out_dir.join("translate_manifest.rs");
    let lock_path = root.join("tools/models.lock.json");
    let lock_text = match std::fs::read_to_string(&lock_path) {
        Ok(t) => t.trim_start_matches('\u{feff}').to_string(),
        Err(_) => return,
    };
    let lock: serde_json::Value = match serde_json::from_str(&lock_text) {
        Ok(v) => v,
        Err(_) => return,
    };
    let empty = serde_json::Map::new();
    let pinned = lock
        .pointer("/translate/pinned")
        .and_then(|v| v.as_object())
        .unwrap_or(&empty);
    let registry = lock
        .pointer("/translate/registry")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut code = String::from(
        "/// Generated from tools/models.lock.json translate.pinned (do not edit).\n\
         pub struct TranslateFile { pub key: &'static str, pub path: &'static str, pub size: u64, pub sha256: &'static str }\n",
    );
    let mut arms = String::new();
    for (pair, spec) in pinned {
        let upper = pair.to_uppercase();
        code.push_str(&format!(
            "pub static PAIR_{upper}: &[TranslateFile] = &["
        ));
        if let Some(files) = spec.pointer("/files").and_then(|v| v.as_object()) {
            for (key, f) in files {
                let p = f.pointer("/path").and_then(|v| v.as_str()).unwrap_or("");
                let size = f.pointer("/uncompressedSize").and_then(|v| v.as_u64()).unwrap_or(0);
                let sha = f.pointer("/uncompressedHash").and_then(|v| v.as_str()).unwrap_or("");
                code.push_str(&format!(
                    "TranslateFile {{ key: \"{key}\", path: \"{p}\", size: {size}, sha256: \"{sha}\" }},"
                ));
            }
        }
        code.push_str("];\n");
        arms.push_str(&format!("\"{pair}\" => Some(PAIR_{upper}),"));
    }
    code.push_str(&format!(
        "pub static TRANSLATE_BASE: &str = \"{}\";\n\
         pub static TRANSLATE_PAIRS: &[&str] = &[{}];\n\
         pub fn pair_files(pair: &str) -> Option<&'static [TranslateFile]> {{ match pair {{ {arms} _ => None }} }}\n",
        registry.trim_end_matches("/db/models.json"),
        pinned.keys().map(|k| format!("\"{k}\"")).collect::<Vec<_>>().join(",")
    ));
    let _ = std::fs::write(&dest, code);
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
