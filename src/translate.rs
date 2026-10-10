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

include!(concat!(env!("OUT_DIR"), "/translate_manifest.rs"));

use std::path::Path;

/// Download + unpack one pair into models/translate/{pair}/ + config.yml.
/// Progress shared as (file_idx, n_files, done_bytes, total_bytes?).
/// Same layout/semantics as tools/fetch-models.mjs --translate (single source:
/// models.lock.json).
pub fn download_pair_blocking(
    models_dir: &std::path::Path,
    pair: &str,
    prog: &std::sync::Arc<std::sync::Mutex<(usize, usize, u64, Option<u64>)>>,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<()> {
    use std::io::Read as _;
    let files = pair_files(pair).ok_or_else(|| anyhow::anyhow!("unknown pair {pair}"))?;
    let dir = models_dir.join(pair);
    std::fs::create_dir_all(&dir)?;
    let mut names: std::collections::HashMap<&str, String> = Default::default();
    for (i, f) in files.iter().enumerate() {
        let base = f
            .path
            .rsplit('/')
            .next()
            .unwrap_or("file")
            .trim_end_matches(".gz")
            .to_string();
        names.insert(f.key, base.clone());
        let dest = dir.join(&base);
        if dest.is_file() {
            continue;
        }
        let url = format!("{}/{}", TRANSLATE_BASE, f.path);
        let tmp = dir.join(format!("{base}.part"));
        crate::download::download(
            &url,
            &tmp,
            cancel,
            {
                let prog = prog.clone();
                move |done, total| {
                    if let Ok(mut p) = prog.lock() {
                        *p = (i, files.len(), done, total);
                    }
                }
            },
        )?;
        let raw = if f.path.ends_with(".gz") {
            let bytes = std::fs::read(&tmp)?;
            let mut dec = flate2::read::GzDecoder::new(&bytes[..]);
            let mut out = Vec::new();
            dec.read_to_end(&mut out)?;
            out
        } else {
            std::fs::read(&tmp)?
        };
        let _ = std::fs::remove_file(&tmp);
        if f.size != 0 && raw.len() as u64 != f.size {
            anyhow::bail!("size mismatch for {pair}/{base}");
        }
        if !f.sha256.is_empty() {
            use sha2::Digest as _;
            let hash = hex_of(&sha2::Sha256::digest(&raw));
            if hash != f.sha256 {
                anyhow::bail!("sha256 mismatch for {pair}/{base}");
            }
        }
        std::fs::write(&dest, raw)?;
    }
    let get = |k: &str| names.get(k).cloned().unwrap_or_default();
    let vocabs = match (names.get("vocab"), names.get("srcVocab")) {
        (Some(v), _) => vec![v.clone()],
        (_, Some(s)) => vec![s.clone(), names.get("trgVocab").cloned().unwrap_or_default()],
        _ => vec![],
    };
    write_pair_config(models_dir, pair, &vocabs, &get("model"), &get("lexicalShortlist"));
    // Mark complete.
    if let Ok(mut p) = prog.lock() {
        *p = (files.len(), files.len(), 0, None);
    }
    Ok(())
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Local model triple present? (model + vocabs + shortlist + config.yml)
pub fn pair_complete(models_dir: &Path, pair: &str) -> bool {
    let dir = models_dir.join(pair);
    dir.join("config.yml").is_file()
        && dir
            .read_dir()
            .map(|mut rd| rd.any(|e| e.map(|e| e.file_name().to_string_lossy().contains("intgemm")).unwrap_or(false)))
            .unwrap_or(false)
}

/// Write bergamot config.yml (single-vocab models list it twice).
pub fn write_pair_config(models_dir: &Path, pair: &str, vocabs: &[String], model: &str, shortlist: &str) {
    let dir = models_dir.join(pair);
    let _ = std::fs::create_dir_all(&dir);
    let mut vocab_lines = String::new();
    if vocabs.len() == 1 {
        vocab_lines.push_str(&format!("    - {}\n    - {}\n", vocabs[0], vocabs[0]));
    } else {
        for v in vocabs {
            vocab_lines.push_str(&format!("    - {v}\n"));
        }
    }
    let cfg = format!(
        "relative-paths: true\nmodels:\n  - {model}\n\
         vocabs:\n{vocab_lines}shortlist:\n  - {shortlist}\n  - false\n\
         beam-size: 1\nnormalize: 1.0\nword-penalty: 0\nmax-length-break: 128\n\
         mini-batch-words: 1024\nworkspace: 128\nmax-length-factor: 2.0\n\
         skip-cost: true\ncpu-threads: 4\nquiet: true\nquiet-translation: true\n\
         gemm-precision: int8shiftAlphaAll\n"
    );
    let _ = std::fs::write(dir.join("config.yml"), cfg);
}

/// Split text into translatable sentences: CJK breaks on 。！？!?…\n,
/// Latin on .!? plus newlines. Keeps delimiters attached.
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        cur.push(c);
        let cjk_break = matches!(c, '。' | '！' | '？' | '…' | '\n');
        let latin_break = matches!(c, '.' | '!' | '?')
            && chars.peek().map(|n| n.is_whitespace()).unwrap_or(true);
        if cjk_break || latin_break {
            // Swallow following closing quotes/brackets into this sentence.
            while let Some(&n) = chars.peek() {
                if matches!(n, '"' | '\'' | '”' | '’' | '』' | '」' | ')') {
                    cur.push(n);
                    chars.next();
                } else {
                    break;
                }
            }
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
            }
            cur = String::new();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Resident translation worker: owns the Engine (Send-confined) and lazily
/// loads pair models from the models dir. Mirrors OcrWorker's protocol.
pub struct TranslateJob {
    pub pair: String,
    pub texts: Vec<String>,
}

pub struct TranslateReply {
    pub texts: Vec<String>,
    pub error: Option<String>,
}

pub struct TranslateWorker {
    tx: std::sync::mpsc::Sender<TranslateJob>,
    pub rx: std::sync::mpsc::Receiver<TranslateReply>,
}

impl TranslateWorker {
    pub fn spawn(models_dir: std::path::PathBuf) -> Self {
        let (job_tx, job_rx) = std::sync::mpsc::channel::<TranslateJob>();
        let (rep_tx, rep_rx) = std::sync::mpsc::channel::<TranslateReply>();
        std::thread::Builder::new()
            .name("translate-worker".to_string())
            .spawn(move || {
                let engine = match Engine::new() {
                    Ok(e) => e,
                    Err(e) => {
                        let msg = format!("{e:#}");
                        while job_rx.recv().is_ok() {
                            let _ = rep_tx.send(TranslateReply {
                                texts: Vec::new(),
                                error: Some(msg.clone()),
                            });
                        }
                        return;
                    }
                };
                let mut loaded = std::collections::HashSet::new();
                for job in job_rx {
                    if !loaded.contains(&job.pair) {
                        if let Err(e) = engine.load(&models_dir, &job.pair) {
                            let _ = rep_tx.send(TranslateReply {
                                texts: Vec::new(),
                                error: Some(format!("{e:#}")),
                            });
                            continue;
                        }
                        loaded.insert(job.pair.clone());
                    }
                    // Sentence-split for stability, then rejoin per line so the
                    // reply keeps 1:1 line mapping with the job. CJK targets
                    // join without spaces, others with a single space.
                    let joiner = if job.pair.ends_with("zh") { "" } else { " " };
                    let mut out = Vec::with_capacity(job.texts.len());
                    let mut err: Option<String> = None;
                    for t in &job.texts {
                        let mut parts = Vec::new();
                        for s in split_sentences(t) {
                            match engine.translate(&job.pair, &s) {
                                Ok(tr) => parts.push(tr),
                                Err(e) => {
                                    err = Some(format!("{e:#}"));
                                    break;
                                }
                            }
                        }
                        if err.is_some() {
                            break;
                        }
                        out.push(parts.join(joiner));
                    }
                    let _ = rep_tx.send(TranslateReply { texts: out, error: err });
                }
            })
            .expect("spawn translate worker");
        Self { tx: job_tx, rx: rep_rx }
    }

    pub fn submit(&self, pair: String, texts: Vec<String>) {
        let _ = self.tx.send(TranslateJob { pair, texts });
    }
}

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
    fn sentence_splitting() {
        assert_eq!(split_sentences("你好。再见！"), vec!["你好。", "再见！"]);
        assert_eq!(split_sentences("Hello world. Bye!"), vec!["Hello world.", "Bye!"]);
        assert_eq!(split_sentences("第一行\n第二行"), vec!["第一行", "第二行"]);
        assert_eq!(split_sentences("no breaks here"), vec!["no breaks here"]);
        assert!(split_sentences("   ").is_empty());
    }

    // One test owns engines sequentially: the native side allows a single
    // live service per process, so direct + worker roundtrips must not run
    // on parallel test threads.
    #[test]
    fn translate_roundtrips_direct_and_worker() {
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
        drop(engine);
        // Worker path: sentence-split + rejoin keeps 1:1 line mapping.
        let w = TranslateWorker::spawn(root);
        w.submit(
            "enzh".to_string(),
            vec!["Hello, world! Good morning.".to_string()],
        );
        let rep = w
            .rx
            .recv_timeout(std::time::Duration::from_secs(180))
            .expect("worker reply");
        assert!(rep.error.is_none(), "worker error: {:?}", rep.error);
        assert_eq!(rep.texts.len(), 1, "line mapping must stay 1:1");
        eprintln!("worker en->zh: {}", rep.texts[0]);
        assert!(rep.texts[0].chars().count() >= 2, "empty translation");
    }
}
