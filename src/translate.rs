//! Bergamot offline translation (route A: native engine DLL + C shim).
//! One process-wide engine (a single BlockingService/logger) holds many loaded
//! models by pair name. Compiled against the real engine only when
//! `cargo xtask build-translate` has run (build.rs sets `translate_native`);
//! otherwise a stub keeps plain builds working.

#[cfg(translate_native)]
mod native {
    use std::ffi::{CStr, CString};
    use std::os::raw::{c_char, c_int, c_void};
    use std::path::Path;

    extern "C" {
        fn bt_init() -> *mut c_void;
        fn bt_load(engine: *mut c_void, name: *const c_char, config: *const c_char) -> c_int;
        fn bt_translate(engine: *mut c_void, name: *const c_char, text: *const c_char) -> *mut c_char;
        fn bt_free(s: *mut c_char);
        fn bt_close(engine: *mut c_void);
        fn bt_last_error() -> *const c_char;
    }

    fn last_error() -> String {
        unsafe {
            let p = bt_last_error();
            if p.is_null() {
                "unknown".to_string()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        }
    }

    fn cstr(s: &str) -> anyhow::Result<CString> {
        CString::new(s).map_err(|_| anyhow::anyhow!("interior nul"))
    }

    pub struct Engine {
        raw: *mut c_void,
    }

    // Confined to the translate worker thread, like ort Sessions.
    unsafe impl Send for Engine {}

    impl Engine {
        pub fn new() -> anyhow::Result<Self> {
            let raw = unsafe { bt_init() };
            if raw.is_null() {
                anyhow::bail!("bt_init failed: {}", last_error());
            }
            Ok(Self { raw })
        }

        /// Load a pair (e.g. "enzh"): reads models/translate/{pair}/config.yml.
        pub fn load(&self, models_dir: &Path, pair: &str) -> anyhow::Result<()> {
            let cfg = models_dir.join(pair).join("config.yml");
            let name = cstr(pair)?;
            let path = cstr(&cfg.to_string_lossy())?;
            let rc = unsafe { bt_load(self.raw, name.as_ptr(), path.as_ptr()) };
            if rc != 0 {
                anyhow::bail!("bt_load {pair} failed: {}", last_error());
            }
            Ok(())
        }

        pub fn translate(&self, pair: &str, text: &str) -> anyhow::Result<String> {
            let name = cstr(pair)?;
            let ctext = cstr(text)?;
            let out = unsafe { bt_translate(self.raw, name.as_ptr(), ctext.as_ptr()) };
            if out.is_null() {
                anyhow::bail!("bt_translate {pair} failed: {}", last_error());
            }
            let s = unsafe { CStr::from_ptr(out).to_string_lossy().into_owned() };
            unsafe { bt_free(out) };
            Ok(s)
        }
    }

    impl Drop for Engine {
        fn drop(&mut self) {
            unsafe { bt_close(self.raw) };
        }
    }
}

#[cfg(not(translate_native))]
mod native {
    use std::path::Path;

    pub struct Engine;
    impl Engine {
        pub fn new() -> anyhow::Result<Self> {
            anyhow::bail!("translation engine not built (run `cargo xtask build-translate`)")
        }
        pub fn load(&self, _models_dir: &Path, _pair: &str) -> anyhow::Result<()> {
            anyhow::bail!("translation engine not built")
        }
        pub fn translate(&self, _pair: &str, _text: &str) -> anyhow::Result<String> {
            anyhow::bail!("translation engine not built")
        }
    }
}

pub use native::Engine;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn translate_models_root() -> PathBuf {
        // Prefer the dev-fetched models; the spike copies live under temp/.
        for cand in [
            PathBuf::from("models/translate"),
            PathBuf::from("temp/translate-models"),
        ] {
            if cand.join("enzh/config.yml").is_file() {
                return cand;
            }
        }
        PathBuf::from("models/translate")
    }

    #[test]
    fn translate_one_sentence_each_way() {
        let root = translate_models_root();
        if !root.join("enzh/config.yml").is_file() {
            eprintln!("skip: no translate models (fetch enzh+zhen first)");
            return;
        }
        let engine = Engine::new().expect("engine init");
        engine.load(&root, "enzh").expect("load enzh");
        engine.load(&root, "zhen").expect("load zhen");
        let zh = engine.translate("enzh", "Hello, world!").expect("translate enzh");
        eprintln!("en->zh: {zh}");
        assert!(zh.chars().count() >= 2, "empty translation");
        let en = engine.translate("zhen", "你好，世界。").expect("translate zhen");
        eprintln!("zh->en: {en}");
        assert!(en.split_whitespace().count() >= 2, "empty translation");
    }
}
