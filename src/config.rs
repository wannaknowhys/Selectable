// App configuration: config.toml next to the exe (or cwd), all optional.
// Explicit `ocr.model` wins or hard-errors when files are missing; otherwise the
// cascade (default medium -> small -> tiny) picks the first complete triple.
use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Default)]
struct FileConfig {
    #[serde(default)]
    hotkey: HotkeySection,
    #[serde(default)]
    ocr: OcrSection,
    #[serde(default)]
    search: SearchSection,
    #[serde(default)]
    translate: TranslateSection,
    #[serde(default)]
    save: SaveSection,
}

#[derive(Debug, Deserialize)]
struct HotkeySection {
    #[serde(default = "default_combos")]
    combos: Vec<String>,
}

impl Default for HotkeySection {
    fn default() -> Self {
        Self { combos: default_combos() }
    }
}

fn default_combos() -> Vec<String> {
    vec!["Shift+PrintScreen".to_string()]
}

#[derive(Debug, Deserialize, Default)]
struct OcrSection {
    #[serde(default)]
    model: Option<String>,
    #[serde(default = "default_cascade")]
    cascade: Vec<String>,
    #[serde(default = "default_batch")]
    rec_batch_size: usize,
    #[serde(default = "default_true")]
    use_directml: bool,
}

fn default_cascade() -> Vec<String> {
    vec!["medium".to_string(), "small".to_string(), "tiny".to_string()]
}

fn default_batch() -> usize {
    6
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct SearchSection {
    /// Preset name (see search::PRESETS); explicit engine_url wins when set.
    #[serde(default = "default_search_engine")]
    engine: String,
    #[serde(default)]
    engine_url: Option<String>,
}

impl Default for SearchSection {
    fn default() -> Self {
        Self { engine: default_search_engine(), engine_url: None }
    }
}

fn default_search_engine() -> String {
    crate::search::default_preset().to_string()
}

fn default_search_url() -> String {
    "https://www.bing.com/search?q={text}".to_string()
}

#[derive(Debug, Deserialize)]
struct TranslateSection {
    #[serde(default = "default_translate_url")]
    external_url: String,
    /// Exit the overlay after any external browser launch (search + the
    /// translate failure/unsupported fallbacks). The fullscreen cover would
    /// otherwise hide the just-opened browser.
    #[serde(default = "default_true")]
    external_auto_exit: bool,
    /// Source language override ("auto" = detect per trigger).
    #[serde(default = "default_auto")]
    source_lang: String,
    /// Target language ("auto" = follow system locale).
    #[serde(default = "default_auto")]
    target_lang: String,
}

impl Default for TranslateSection {
    fn default() -> Self {
        Self {
            external_url: default_translate_url(),
            external_auto_exit: true,
            source_lang: default_auto(),
            target_lang: default_auto(),
        }
    }
}

fn default_auto() -> String {
    "auto".to_string()
}

fn default_translate_url() -> String {
    "https://translate.google.com/?sl=auto&tl=zh-CN&text={text}".to_string()
}

#[derive(Debug, Deserialize, Default)]
struct SaveSection {
    #[serde(default)]
    dir: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    // Phase: options UI (hotkey remapping).
    #[allow(dead_code)]
    pub combos: Vec<String>,
    pub explicit_model: Option<String>,
    pub cascade: Vec<String>,
    pub rec_batch_size: usize,
    pub use_directml: bool,
    pub search_url: String,
    pub translate_url: String,
    pub external_auto_exit: bool,
    pub translate_source: String,
    pub translate_target: String,
    pub save_dir: Option<String>,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        for path in [dir.join("config.toml"), PathBuf::from("config.toml")] {
            if path.is_file() {
                let text = std::fs::read_to_string(&path)?;
                let fc: FileConfig = toml::from_str(&text)?;
                return Ok(Self {
                    combos: fc.hotkey.combos,
                    explicit_model: fc.ocr.model,
                    cascade: fc.ocr.cascade,
                    rec_batch_size: fc.ocr.rec_batch_size.max(1),
                    use_directml: fc.ocr.use_directml,
                    search_url: crate::search::resolve_url(
                        fc.search.engine_url.as_deref(),
                        &fc.search.engine,
                    ),
                    translate_url: fc.translate.external_url,
                    external_auto_exit: fc.translate.external_auto_exit,
                    translate_source: fc.translate.source_lang,
                    translate_target: fc.translate.target_lang,
                    save_dir: fc.save.dir,
                });
            }
        }
        Ok(Self {
            combos: default_combos(),
            explicit_model: None,
            cascade: default_cascade(),
            rec_batch_size: default_batch(),
            use_directml: true,
            search_url: default_search_url(),
            translate_url: default_translate_url(),
            external_auto_exit: true,
            translate_source: default_auto(),
            translate_target: default_auto(),
            save_dir: None,
        })
    }
}
