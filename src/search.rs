//! Search engine presets: URL templates with a `{text}` placeholder (filled by
//! `overlay::fill_url` with the percent-encoded query).
//!
//! Source: the Firefox search-config registry extracted from a local install
//! (browser/omni.ja `defaults/settings/main/search-config-v2.json`: 155
//! engines; global default = google, zh-CN default = baidu). Mozilla partner
//! codes (`pc`/`form`/`t`) are stripped — plain templates work without them.
//! wikipedia ships `en` upstream; the `zh` subdomain is derived for our locale.

pub struct Preset {
    pub name: &'static str,
    pub url: &'static str,
}

pub static PRESETS: &[Preset] = &[
    Preset {
        name: "bing",
        url: "https://www.bing.com/search?q={text}",
    },
    Preset {
        name: "google",
        url: "https://www.google.com/search?q={text}",
    },
    Preset {
        name: "ddg",
        url: "https://duckduckgo.com/?q={text}",
    },
    Preset {
        name: "baidu",
        url: "https://www.baidu.com/baidu?ie=utf-8&wd={text}",
    },
    Preset {
        name: "wikipedia",
        url: "https://zh.wikipedia.org/wiki/Special:Search?search={text}",
    },
    Preset {
        name: "qwant",
        url: "https://www.qwant.com/?q={text}",
    },
];

pub fn default_preset() -> &'static str {
    "bing"
}

/// Case-insensitive preset lookup (" Google " hits).
pub fn preset_url(name: &str) -> Option<&'static str> {
    let key = name.trim().to_lowercase();
    PRESETS.iter().find(|p| p.name == key).map(|p| p.url)
}

/// Final search URL: explicit `engine_url` wins when non-empty, then the
/// `engine` preset, then the bing default (unknown preset names included).
pub fn resolve_url(explicit: Option<&str>, preset: &str) -> String {
    match explicit.map(str::trim) {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => preset_url(preset).unwrap_or(PRESETS[0].url).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_valid_templates() {
        assert!(PRESETS.len() >= 6);
        let mut names: Vec<_> = PRESETS.iter().map(|p| p.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), PRESETS.len(), "duplicate preset names");
        for p in PRESETS {
            assert!(p.url.contains("{text}"), "{} missing placeholder", p.name);
            assert!(p.url.starts_with("https://"), "{} not https", p.name);
            assert_eq!(preset_url(&p.name.to_uppercase()), Some(p.url));
        }
    }

    #[test]
    fn resolution_priority() {
        // Explicit URL always wins.
        assert_eq!(
            resolve_url(Some("https://example.com/?q={text}"), "google"),
            "https://example.com/?q={text}"
        );
        assert_eq!(resolve_url(Some("  "), "google"), preset_url("google").unwrap());
        assert_eq!(resolve_url(None, "ddg"), preset_url("ddg").unwrap());
        // Unknown preset falls back to bing (PRESETS[0]).
        assert_eq!(resolve_url(None, "nope"), PRESETS[0].url);
        assert_eq!(resolve_url(None, ""), PRESETS[0].url);
    }
}
