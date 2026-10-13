// App configuration: config.toml next to the exe (or cwd), all optional.
// Explicit `ocr.model` wins or hard-errors when files are missing; otherwise the
// cascade (default medium -> small -> tiny) picks the first complete triple.
use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Default)]
pub(crate) struct FileConfig {
    #[serde(default)]
    pub(crate) hotkey: HotkeySection,
    #[serde(default)]
    pub(crate) ocr: OcrSection,
    #[serde(default)]
    pub(crate) search: SearchSection,
    #[serde(default)]
    pub(crate) translate: TranslateSection,
    #[serde(default)]
    pub(crate) save: SaveSection,
    #[serde(default)]
    pub(crate) ui: UiSection,
}

#[derive(Debug, Deserialize)]
pub(crate) struct HotkeySection {
    #[serde(default = "default_combos")]
    pub(crate) combos: Vec<String>,
}

impl Default for HotkeySection {
    fn default() -> Self {
        Self { combos: default_combos() }
    }
}

fn default_combos() -> Vec<String> {
    vec!["Shift+PrintScreen".to_string()]
}

/// NOTE: manual Default (not derive): derive would fill custom-defaulted
/// fields (cascade/batch/directml) with empty/zero/false whenever the whole
/// [ocr] section is absent from config.toml, silently bricking OCR startup.
#[derive(Debug, Deserialize)]
pub(crate) struct OcrSection {
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default = "default_cascade")]
    pub(crate) cascade: Vec<String>,
    #[serde(default = "default_batch")]
    pub(crate) rec_batch_size: usize,
    #[serde(default = "default_true")]
    pub(crate) use_directml: bool,
}

impl Default for OcrSection {
    fn default() -> Self {
        Self {
            model: None,
            cascade: default_cascade(),
            rec_batch_size: default_batch(),
            use_directml: true,
        }
    }
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
pub(crate) struct SearchSection {
    /// Preset name (see search::PRESETS); explicit engine_url wins when set.
    #[serde(default = "default_search_engine")]
    pub(crate) engine: String,
    #[serde(default)]
    pub(crate) engine_url: Option<String>,
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
pub(crate) struct TranslateSection {
    /// Engine switch (single dispatch): bergamot (offline, default) |
    /// external (always browser) | online (not wired yet; falls back).
    #[serde(default = "default_source_mode")]
    pub(crate) source: String,
    #[serde(default = "default_translate_url")]
    pub(crate) external_url: String,
    /// Exit the overlay after any external browser launch (search + the
    /// translate failure/unsupported fallbacks). The fullscreen cover would
    /// otherwise hide the just-opened browser.
    #[serde(default = "default_true")]
    pub(crate) external_auto_exit: bool,
    /// Auto-open the browser on engine failure / unsupported pairs WITHOUT
    /// asking. Default off (privacy: no user string leaves the machine
    /// without explicit consent; explicit clicks and source=external bypass).
    #[serde(default)]
    pub(crate) external_fallback: bool,
    /// Ask before the fallback above (NO = one-shot toast). The dialog's
    /// "don't ask again" persists the choice back to config.toml.
    #[serde(default = "default_true")]
    pub(crate) external_fallback_ask: bool,
    /// Source language override ("auto" = detect per trigger).
    #[serde(default = "default_auto")]
    pub(crate) source_lang: String,
    /// Target language ("auto" = follow system locale).
    #[serde(default = "default_auto")]
    pub(crate) target_lang: String,
}

impl Default for TranslateSection {
    fn default() -> Self {
        Self {
            source: default_source_mode(),
            external_url: default_translate_url(),
            external_auto_exit: true,
            external_fallback: false,
            external_fallback_ask: true,
            source_lang: default_auto(),
            target_lang: default_auto(),
        }
    }
}

/// Normalized engine name; unknown values (typos) fall back to bergamot so a
/// bad key can't brick startup.
pub fn normalize_source_mode(raw: &str) -> &'static str {
    match raw.trim().to_lowercase().as_str() {
        "external" => "external",
        "online" => "online",
        _ => "bergamot",
    }
}

fn default_source_mode() -> String {
    "bergamot".to_string()
}

fn default_auto() -> String {
    "auto".to_string()
}

fn default_translate_url() -> String {
    "https://translate.google.com/?sl=auto&tl={tgt}&text={text}".to_string()
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct SaveSection {
    #[serde(default)]
    pub(crate) dir: Option<String>,
}

/// UI language override ("auto" = follow system locale).
#[derive(Debug, Deserialize)]
pub(crate) struct UiSection {
    #[serde(default = "default_auto")]
    pub(crate) lang: String,
}

impl Default for UiSection {
    fn default() -> Self {
        Self { lang: default_auto() }
    }
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
    pub translate_mode: String,
    pub external_auto_exit: bool,
    pub external_fallback: bool,
    pub external_fallback_ask: bool,
    pub translate_source: String,
    pub translate_target: String,
    pub save_dir: Option<String>,
    pub ui_lang: String,
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
                    translate_mode: normalize_source_mode(&fc.translate.source).to_string(),
                    external_auto_exit: fc.translate.external_auto_exit,
                    external_fallback: fc.translate.external_fallback,
                    external_fallback_ask: fc.translate.external_fallback_ask,
                    translate_source: fc.translate.source_lang,
                    translate_target: fc.translate.target_lang,
                    save_dir: fc.save.dir,
                    ui_lang: fc.ui.lang,
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
            translate_mode: default_source_mode(),
            external_auto_exit: true,
            external_fallback: false,
            external_fallback_ask: true,
            translate_source: default_auto(),
            translate_target: default_auto(),
            save_dir: None,
            ui_lang: default_auto(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_mode_normalizes() {
        assert_eq!(normalize_source_mode("bergamot"), "bergamot");
        assert_eq!(normalize_source_mode(" External "), "external");
        assert_eq!(normalize_source_mode("ONLINE"), "online");
        // Default + typos fall back to bergamot (never brick startup).
        assert_eq!(normalize_source_mode(""), "bergamot");
        assert_eq!(normalize_source_mode("extrenal"), "bergamot");
    }

    #[test]
    fn partial_config_keeps_section_defaults() {
        // Regression: a config.toml with only [translate] used to leave
        // OcrSection::default() derived (empty cascade), bricking OCR with
        // `tried []`. Missing sections must equal their manual defaults.
        let fc: FileConfig = toml::from_str("[translate]\nsource = \"external\"\n").unwrap();
        assert_eq!(normalize_source_mode(&fc.translate.source), "external");
        assert_eq!(
            fc.ocr.cascade,
            vec!["medium".to_string(), "small".to_string(), "tiny".to_string()]
        );
        assert_eq!(fc.search.engine, "bing");
        assert!(fc.search.engine_url.is_none());
    }
}
