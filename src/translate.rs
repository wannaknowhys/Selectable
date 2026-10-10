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

/// Translation plan for src->tgt over the baked registry snapshot
/// (TRANSLATE_KNOWN_PAIRS) and the baked file tables (pair_files).
/// v1 pivots only via en: ja->zh runs ja->en then en->zh on the same worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// src == tgt: nothing to translate.
    Same,
    /// Ordered engine legs, back-to-back (1 direct, 2 via pivot).
    Legs(Vec<String>),
    /// No direct leg and no pivot (caller falls back to external).
    Unsupported,
}

pub fn plan_for(src: &str, tgt: &str) -> Plan {
    // None when identical (nothing to translate), case-insensitive.
    if crate::lang::pair_for(src, tgt).is_none() {
        return Plan::Same;
    }
    let (s, t) = (src.to_lowercase(), tgt.to_lowercase());
    let direct = format!("{s}{t}");
    if TRANSLATE_KNOWN_PAIRS.contains(&direct.as_str()) && pair_files(&direct).is_some() {
        return Plan::Legs(vec![direct]);
    }
    if s != "en" && t != "en" {
        let (a, b) = (format!("{s}en"), format!("en{t}"));
        if TRANSLATE_KNOWN_PAIRS.contains(&a.as_str())
            && TRANSLATE_KNOWN_PAIRS.contains(&b.as_str())
            && pair_files(&a).is_some()
            && pair_files(&b).is_some()
        {
            return Plan::Legs(vec![a, b]);
        }
    }
    Plan::Unsupported
}

/// All legs present on disk?
pub fn chain_complete(models_dir: &Path, legs: &[String]) -> bool {
    legs.iter().all(|p| pair_complete(models_dir, p))
}

/// Summed baked model bytes over the legs (confirm-box MB figure).
pub fn chain_total_bytes(legs: &[String]) -> u64 {
    legs.iter()
        .filter_map(|p| pair_files(p))
        .flat_map(|fs| fs.iter().map(|f| f.size))
        .sum()
}

/// Download every leg in order into models/translate/{leg}/ + config.yml.
/// Progress is global across legs: (file_idx, total_files, done, total?),
/// so the single download panel just works for 1- and 2-leg plans.
pub fn download_chain_blocking(
    models_dir: &std::path::Path,
    legs: &[String],
    prog: &std::sync::Arc<std::sync::Mutex<(usize, usize, u64, Option<u64>)>>,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<()> {
    if legs.len() == 1 {
        return download_pair_blocking(models_dir, &legs[0], prog, cancel);
    }
    let total: usize = legs
        .iter()
        .filter_map(|p| pair_files(p))
        .map(|fs| fs.len())
        .sum();
    let mut base = 0usize;
    for leg in legs {
        let files = pair_files(leg).ok_or_else(|| anyhow::anyhow!("unknown pair {leg}"))?;
        download_pair_files(models_dir, leg, files, base, total, prog, cancel)?;
        base += files.len();
    }
    if let Ok(mut p) = prog.lock() {
        *p = (total, total, 0, None);
    }
    Ok(())
}

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
    let files = pair_files(pair).ok_or_else(|| anyhow::anyhow!("unknown pair {pair}"))?;
    let total = files.len();
    download_pair_files(models_dir, pair, files, 0, total, prog, cancel)?;
    // Mark complete.
    if let Ok(mut p) = prog.lock() {
        *p = (total, total, 0, None);
    }
    Ok(())
}

fn download_pair_files(
    models_dir: &std::path::Path,
    pair: &str,
    files: &[TranslateFile],
    base_idx: usize,
    total_files: usize,
    prog: &std::sync::Arc<std::sync::Mutex<(usize, usize, u64, Option<u64>)>>,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<()> {
    use std::io::Read as _;
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
                        *p = (base_idx + i, total_files, done, total);
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

/// Target side of a compact leg code ("zhen" -> "en", "enzh_hant" -> "zh_hant").
fn leg_target_is_cjk(leg: &str) -> bool {
    leg_target(leg).starts_with("zh")
}

fn leg_target(leg: &str) -> &str {
    if leg.ends_with("zh_hant") {
        "zh_hant"
    } else if leg.len() >= 2 {
        &leg[leg.len() - 2..]
    } else {
        ""
    }
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
    pub chain: Vec<String>,
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
                    if job.chain.is_empty() {
                        let _ = rep_tx.send(TranslateReply {
                            texts: Vec::new(),
                            error: Some("empty translation chain".to_string()),
                        });
                        continue;
                    }
                    let mut ok = true;
                    for leg in &job.chain {
                        if !loaded.contains(leg) {
                            if let Err(e) = engine.load(&models_dir, leg) {
                                let _ = rep_tx.send(TranslateReply {
                                    texts: Vec::new(),
                                    error: Some(format!("{e:#}")),
                                });
                                ok = false;
                                break;
                            }
                            loaded.insert(leg.clone());
                        }
                    }
                    if !ok {
                        continue;
                    }
                    // Sentence-split for stability, then rejoin per line so the
                    // reply keeps 1:1 line mapping with the job. Each leg
                    // joins in its own target style (CJK targets without
                    // spaces); pivot legs run back-to-back: texts -> mid ->
                    // final. Re-splitting per leg keeps the chain robust to
                    // either join style.
                    let mut cur = job.texts.clone();
                    let mut err: Option<String> = None;
                    for leg in &job.chain {
                        let joiner = if leg_target_is_cjk(leg) { "" } else { " " };
                        let mut out = Vec::with_capacity(cur.len());
                        for t in &cur {
                            let mut parts = Vec::new();
                            for s in split_sentences(t) {
                                match engine.translate(leg, &s) {
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
                        if err.is_some() {
                            break;
                        }
                        cur = out;
                    }
                    let _ = rep_tx.send(TranslateReply { texts: cur, error: err });
                }
            })
            .expect("spawn translate worker");
        Self { tx: job_tx, rx: rep_rx }
    }

    pub fn submit(&self, chain: Vec<String>, texts: Vec<String>) {
        let _ = self.tx.send(TranslateJob { chain, texts });
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

    #[test]
    fn chain_planning() {
        // Direct legs (baked file tables).
        assert_eq!(plan_for("en", "zh"), Plan::Legs(vec!["enzh".to_string()]));
        assert_eq!(plan_for("zh", "en"), Plan::Legs(vec!["zhen".to_string()]));
        assert_eq!(plan_for("ja", "en"), Plan::Legs(vec!["jaen".to_string()]));
        // Pivot via en (no direct jazh/frzh in the registry).
        assert_eq!(
            plan_for("ja", "zh"),
            Plan::Legs(vec!["jaen".to_string(), "enzh".to_string()])
        );
        assert_eq!(
            plan_for("fr", "zh"),
            Plan::Legs(vec!["fren".to_string(), "enzh".to_string()])
        );
        assert_eq!(
            plan_for("zh", "ja"),
            Plan::Legs(vec!["zhen".to_string(), "enja".to_string()])
        );
        // Same language: nothing to do.
        assert_eq!(plan_for("en", "EN"), Plan::Same);
        assert_eq!(plan_for("zh", "zh"), Plan::Same);
        // Known registry pair but no baked files (e.g. en-ca): external fallback.
        assert_eq!(plan_for("en", "ca"), Plan::Unsupported);
        // Pivot leg missing files on one side: unsupported, not half-chain.
        assert_eq!(plan_for("ca", "zh"), Plan::Unsupported);
        // leg target styles drive the joiner.
        assert!(leg_target_is_cjk("enzh"));
        assert!(leg_target_is_cjk("enzh_hant"));
        assert!(!leg_target_is_cjk("zhen"));
        assert!(!leg_target_is_cjk("jaen"));
        // Baked chain sizes are nonzero (confirm-box MB figure).
        assert!(chain_total_bytes(&["jaen".to_string(), "enzh".to_string()]) > 0);
        assert!(chain_total_bytes(&["enzh".to_string()]) > 0);
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
        let w = TranslateWorker::spawn(root.clone());
        w.submit(
            vec!["enzh".to_string()],
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
        // Pivot path on the same worker (single engine): fr->en->zh when the
        // fren leg is fetched; skipped otherwise (fetch fren to cover it).
        if root.join("fren/config.yml").is_file() {
            w.submit(
                vec!["fren".to_string(), "enzh".to_string()],
                vec!["Bonjour le monde.".to_string()],
            );
            let rep = w
                .rx
                .recv_timeout(std::time::Duration::from_secs(180))
                .expect("worker pivot reply");
            assert!(rep.error.is_none(), "pivot error: {:?}", rep.error);
            assert_eq!(rep.texts.len(), 1, "line mapping must stay 1:1");
            eprintln!("worker fr->en->zh: {}", rep.texts[0]);
            assert!(rep.texts[0].chars().count() >= 2, "empty pivot translation");
        } else {
            eprintln!("skip: no fren models (fetch fren for pivot coverage)");
        }
    }
}
