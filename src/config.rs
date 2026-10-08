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

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub combos: Vec<String>,
    pub explicit_model: Option<String>,
    pub cascade: Vec<String>,
    pub rec_batch_size: usize,
    pub use_directml: bool,
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
                });
            }
        }
        Ok(Self {
            combos: default_combos(),
            explicit_model: None,
            cascade: default_cascade(),
            rec_batch_size: default_batch(),
            use_directml: true,
        })
    }
}
