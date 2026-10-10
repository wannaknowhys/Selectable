//! Language detection (script heuristic, offline) + target resolution.
//! Bergamot direction codes: two-letter pairs like "enzh" (en->zh).

/// Detect the dominant script by raw share (no exclusion): ja > ko > ru >
/// zh > en on ties, pure count otherwise. Single API; callers that need a
/// target excluded use detect_script_except. Only unit tests call this
/// directly (the overlay always excludes a target), hence the allow.
#[allow(dead_code)]
pub fn detect_script(text: &str) -> &'static str {
    detect_script_except(text, "")
}

/// Raw script counters shared by detection. Digits and punctuation (ASCII,
/// CJK symbols, fullwidth forms) carry no language signal and are skipped —
/// otherwise "你好，世界。" counts as English evidence via ，。.
fn count_scripts(text: &str) -> (u32, u32, u32, u32, u32) {
    let (mut ja, mut ko, mut ru, mut han, mut other) = (0u32, 0, 0, 0, 0);
    for c in text.chars() {
        if c.is_whitespace() || c.is_ascii_punctuation() || c.is_ascii_digit() {
            continue;
        }
        // CJK symbols/punctuation + fullwidth block: neutral, not "other".
        if matches!(c, '\u{3000}'..='\u{303F}' | '\u{FF00}'..='\u{FFEF}') {
            continue;
        }
        match c {
            // Hiragana, Katakana, halfwidth katakana.
            '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}' | '\u{FF66}'..='\u{FF9F}' => ja += 1,
            // Hangul syllables + jamo + compat jamo.
            '\u{AC00}'..='\u{D7AF}' | '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}' => ko += 1,
            // Cyrillic.
            '\u{0400}'..='\u{04FF}' => ru += 1,
            // CJK unified + extensions + compat.
            '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}'
            | '\u{20000}'..='\u{2FFFF}' => han += 1,
            _ => other += 1,
        }
    }
    (ja, ko, ru, han, other)
}

/// Mixed-text source detection that excludes the target language (R3):
/// rank scripts by char count, most frequent non-target wins. A Latin-bucket
/// win is refined by latin_hint (fr/de/es/it/pt) when the hint disagrees.
/// Returns the exclusion itself when nothing else is present (caller maps
/// that to the "nothing to translate" toast via pair_for's None).
pub fn detect_script_except(text: &str, exclude: &str) -> &'static str {
    let (ja, ko, ru, han, other) = count_scripts(text);
    let ranked: [(&str, u32); 5] =
        [("ja", ja), ("ko", ko), ("ru", ru), ("zh", han), ("en", other)];
    let mut best: Option<(&str, u32)> = None;
    for cand in ranked {
        if cand.0 == exclude || cand.1 == 0 {
            continue;
        }
        if best.map(|b| cand.1 > b.1).unwrap_or(true) {
            best = Some(cand);
        }
    }
    let win = match best {
        Some((code, _)) => code,
        None => {
            return match exclude {
                "ja" => "ja",
                "ko" => "ko",
                "ru" => "ru",
                "zh" => "zh",
                _ => "en",
            }
        }
    };
    // Latin-script languages share one Unicode block: "café" in English text
    // misfires as fr (documented trade; the dropdown always overrides, and a
    // pivot through en keeps the damage passthrough-shaped).
    if win == "en" {
        if let Some(h) = latin_hint(text) {
            if h != exclude {
                return h;
            }
        }
    }
    win
}

/// Latin-script refinement: accented chars (+2) and stopwords (+1) vote for
/// fr/de/es/it/pt; threshold 2, highest wins. Zero dependencies, heuristic —
/// manual source selection always wins over this.
pub fn latin_hint(text: &str) -> Option<&'static str> {
    // Order: fr de es it pt.
    let mut score = [0i32; 5];
    for c in text.chars() {
        match c {
            'é' | 'è' | 'ê' | 'ë' | 'à' | 'â' | 'î' | 'ï' | 'ô' | 'ù' | 'û' | 'ç' | 'œ' => score[0] += 2,
            'ä' | 'ö' | 'ü' | 'ß' => score[1] += 2,
            'ñ' | '¿' | '¡' => score[2] += 2,
            'ì' | 'ò' => score[3] += 2,
            'ã' | 'õ' => score[4] += 2,
            _ => {}
        }
    }
    const FR: &[&str] = &["le", "la", "les", "des", "une", "est", "sont", "et", "ou", "je", "tu", "il", "elle", "nous", "vous", "ils", "elles", "bonjour", "merci", "oui", "non", "avec", "pour", "dans", "plus", "cette", "ces", "mon", "ton", "son"];
    const DE: &[&str] = &["der", "die", "das", "und", "nicht", "ein", "eine", "ist", "sind", "mit", "für", "von", "sich", "danke", "hallo", "aber", "oder", "wenn"];
    const ES: &[&str] = &["el", "los", "las", "una", "este", "esta", "está", "son", "con", "para", "pero", "hola", "gracias", "que", "del", "más", "también"];
    const IT: &[&str] = &["che", "una", "della", "nella", "sono", "con", "per", "non", "piu", "più", "giù", "può", "perché", "grazie", "ciao", "dal", "sul", "come"];
    const PT: &[&str] = &["uma", "para", "com", "não", "voce", "você", "são", "como", "mais", "obrigado", "olá", "dos", "das", "que"];
    for w in text.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        if FR.contains(&w) {
            score[0] += 1;
        }
        if DE.contains(&w) {
            score[1] += 1;
        }
        if ES.contains(&w) {
            score[2] += 1;
        }
        if IT.contains(&w) {
            score[3] += 1;
        }
        if PT.contains(&w) {
            score[4] += 1;
        }
    }
    const CODES: [&str; 5] = ["fr", "de", "es", "it", "pt"];
    let mut best: Option<(&str, i32)> = None;
    for (i, code) in CODES.iter().enumerate() {
        if score[i] >= 2 && best.map(|b| score[i] > b.1).unwrap_or(true) {
            best = Some((code, score[i]));
        }
    }
    best.map(|b| b.0)
}

/// System locale primary subtag ("zh-CN" -> "zh"), lowercased.
pub fn system_lang() -> String {
    unsafe {
        let mut buf = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH
        let n = windows::Win32::Globalization::GetUserDefaultLocaleName(&mut buf);
        if n <= 0 {
            return "en".to_string();
        }
        let name = String::from_utf16_lossy(&buf[..n as usize]);
        name.split(['-', '_']).next().unwrap_or("en").to_lowercase()
    }
}

/// Resolve the effective target: explicit config wins, "auto"/empty follows locale.
pub fn resolve_target(configured: Option<&str>) -> String {
    match configured.map(str::trim) {
        Some(s) if !s.is_empty() && !s.eq_ignore_ascii_case("auto") => {
            s.to_lowercase().replace(['-', '_'], "")
        }
        _ => system_lang().replace(['-', '_'], ""),
    }
}

/// Pair code for src->tgt, or None when identical (nothing to translate).
pub fn pair_for(src: &str, tgt: &str) -> Option<String> {
    if src.eq_ignore_ascii_case(tgt) {
        None
    } else {
        Some(format!("{src}{tgt}").to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_heuristics() {
        assert_eq!(detect_script("Hello, world!"), "en");
        assert_eq!(detect_script("截屏选词 123"), "zh");
        assert_eq!(detect_script("こんにちは世界"), "ja");
        assert_eq!(detect_script("漢字かな交じり"), "ja");
        assert_eq!(detect_script("스크린샷 테스트"), "ko");
        assert_eq!(detect_script("Привет мир"), "ru");
        // Share-count ranking (spec §12.1): latin outnumbers han here, so the
        // bare call says en; the mixed-with-target case pins zh via exclusion.
        assert_eq!(detect_script("Hello 世界 mixed"), "en");
        assert_eq!(detect_script("你好，世界。"), "zh");
        assert_eq!(detect_script("   "), "en");
    }

    #[test]
    fn pair_rules() {
        assert_eq!(pair_for("en", "zh"), Some("enzh".to_string()));
        assert_eq!(pair_for("zh", "en"), Some("zhen".to_string()));
        assert_eq!(pair_for("en", "EN"), None);
        assert_eq!(pair_for("zh", "zh"), None);
    }

    #[test]
    fn target_resolution() {
        assert_eq!(resolve_target(Some("zh")), "zh");
        assert_eq!(resolve_target(Some("AUTO")), system_lang());
        assert_eq!(resolve_target(None), system_lang());
        assert!(!resolve_target(None).is_empty());
    }

    #[test]
    fn except_excludes_target() {
        // R3: zh+en mix, target zh -> en; target en -> zh.
        let mix = "Hello 世界 mixed";
        assert_eq!(detect_script_except(mix, "zh"), "en");
        assert_eq!(detect_script_except(mix, "en"), "zh");
        // CJK punctuation and digits are neutral: pure-target text with
        // ，。/123 stays target (falls back to the exclusion).
        assert_eq!(detect_script_except("你好，世界。", "zh"), "zh");
        assert_eq!(detect_script_except("截屏选词 123 上线", "zh"), "zh");
        // All-target text falls back to the exclusion (pair_for -> None).
        assert_eq!(detect_script_except("截屏选词", "zh"), "zh");
        assert_eq!(detect_script_except("Hello world", "en"), "en");
        // Pure third language survives any exclusion.
        assert_eq!(detect_script_except("こんにちは世界", "zh"), "ja");
        // Latin bucket refined by latin_hint (see latin_hints below).
        assert_eq!(detect_script_except("Bonjour le monde", "zh"), "fr");
        // Empty text -> exclusion (caller reports "nothing to translate").
        assert_eq!(detect_script_except("   ", "zh"), "zh");
    }

    #[test]
    fn latin_hints() {
        assert_eq!(latin_hint("Bonjour le monde"), Some("fr"));
        assert_eq!(latin_hint("Straße und Danke schön"), Some("de"));
        assert_eq!(latin_hint("Hola gracias amigo"), Some("es"));
        assert_eq!(latin_hint("Hello world"), None);
        // Documented trade: lone accent misfires (dropdown overrides).
        assert_eq!(latin_hint("Visit the café"), Some("fr"));
        // Hint refines the latin bucket, never CJK.
        assert_eq!(detect_script_except("Bonjour le monde", "zh"), "fr");
        assert_eq!(detect_script_except("Bonjour le monde", "fr"), "en");
        assert_eq!(detect_script_except("截屏 café 测试", "zh"), "fr");
    }
}
