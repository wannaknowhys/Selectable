//! Language detection (script heuristic, offline) + target resolution.
//! Bergamot direction codes: two-letter pairs like "enzh" (en->zh).

/// Detect the dominant script: ja (kana present) > ko > ru > zh > en.
/// CJK Han without kana/hangul counts as zh (Hans/Hant not distinguished v1).
pub fn detect_script(text: &str) -> &'static str {
    let (mut ja, mut ko, mut ru, mut han, mut other) = (0u32, 0, 0, 0, 0);
    for c in text.chars() {
        if c.is_whitespace() || c.is_ascii_punctuation() {
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
    if ja > 0 {
        "ja"
    } else if ko > 0 {
        "ko"
    } else if ru > han && ru > 0 {
        "ru"
    } else if han > 0 {
        "zh"
    } else if other > 0 || !text.trim().is_empty() {
        "en"
    } else {
        "en"
    }
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
        assert_eq!(detect_script("Hello 世界 mixed"), "zh");
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
}
