// Selectable hotkey: parsed combos (stored as strings in config.toml),
// registered on a caller-owned window; the owner pumps messages.
//
// v1 scope: single active combo (first of `combos`), re-registered live by
// the tray when changed in Settings. Combos RegisterHotKey cannot take
// (system-reserved) are rejected: statically when known, otherwise by a
// live probe register on the settings dialog itself.
use anyhow::{Context, Result};
use windows::Win32::{
    Foundation::HWND,
    UI::Input::KeyboardAndMouse::{
        GetKeyNameTextW, GetKeyState, MapVirtualKeyW, RegisterHotKey, UnregisterHotKey,
        HOT_KEY_MODIFIERS, MAPVK_VK_TO_VSC, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, VK_CONTROL,
        VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_SNAPSHOT,
    },
};

pub const HOTKEY_ID: i32 = 1;
/// Scratch id for the live probe (never collides: real ids stay small).
const PROBE_ID: i32 = 9137;

/// Parsed combo: MOD_* bits + a virtual-key code (VIRTUAL_KEY.0 value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

pub fn default_hotkey() -> Hotkey {
    Hotkey { mods: MOD_SHIFT.0, vk: VK_SNAPSHOT.0 as u32 }
}

/// Parse "Shift+PrintScreen" / "Ctrl+Alt+S" / "0x41" (bare key, no mods).
/// Modifiers (case-insensitive): Shift, Ctrl/Control, Alt, Win/Super/Meta.
/// Returns None for unknown tokens (caller keeps the old combo + reports).
pub fn parse_combo(raw: &str) -> Option<Hotkey> {
    let mut mods = 0u32;
    let mut key: Option<u32> = None;
    let mut parts = raw.split('+').map(str::trim).filter(|s| !s.is_empty()).peekable();
    while let Some(p) = parts.next() {
        let last = parts.peek().is_none();
        let up = p.to_ascii_uppercase();
        if !last {
            mods |= match up.as_str() {
                "SHIFT" => MOD_SHIFT.0,
                "CTRL" | "CONTROL" => MOD_CONTROL.0,
                "ALT" => MOD_ALT.0,
                "WIN" | "WINDOWS" | "SUPER" | "META" => MOD_WIN.0,
                _ => return None,
            };
        } else {
            // Trailing modifier with no key ("Ctrl+") is invalid.
            if matches!(up.as_str(), "SHIFT" | "CTRL" | "CONTROL" | "ALT" | "WIN" | "WINDOWS" | "SUPER" | "META") {
                return None;
            }
            key = Some(parse_vk(&up)?);
        }
    }
    Some(Hotkey { mods, vk: key? })
}

/// Single key token -> VK code: A-Z, 0-9, F1-F24, 0xNN, or a named key.
fn parse_vk(up: &str) -> Option<u32> {
    if up.len() == 1 {
        let c = up.as_bytes()[0];
        if c.is_ascii_alphanumeric() {
            return Some(c as u32);
        }
        return None;
    }
    if let Some(hex) = up.strip_prefix("0X") {
        return u32::from_str_radix(hex, 16).ok().filter(|v| *v <= 0xFF);
    }
    if let Some(n) = up.strip_prefix('F') {
        let f: u32 = n.parse().ok()?;
        if (1..=24).contains(&f) {
            return Some(0x6F + f); // VK_F1 = 0x70
        }
        return None;
    }
    Some(match up {
        "PRINTSCREEN" | "PRTSC" | "PRTSCR" | "SNAPSHOT" => 0x2C,
        "SPACE" => 0x20,
        "ENTER" | "RETURN" => 0x0D,
        "TAB" => 0x09,
        "ESC" | "ESCAPE" => 0x1B,
        "BACKSPACE" => 0x08,
        "DELETE" | "DEL" => 0x2E,
        "INSERT" | "INS" => 0x2D,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PGUP" => 0x21,
        "PAGEDOWN" | "PGDN" => 0x22,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "PAUSE" => 0x13,
        "SCROLLLOCK" => 0x91,
        "CAPSLOCK" => 0x14,
        "NUMLOCK" => 0x90,
        _ => return None,
    })
}

/// Canonical config form ("Ctrl+Shift+S"): ASCII, fixed modifier order.
pub fn format_combo(h: &Hotkey) -> String {
    let mut parts = Vec::new();
    if h.mods & MOD_CONTROL.0 != 0 {
        parts.push("Ctrl".to_string());
    }
    if h.mods & MOD_SHIFT.0 != 0 {
        parts.push("Shift".to_string());
    }
    if h.mods & MOD_ALT.0 != 0 {
        parts.push("Alt".to_string());
    }
    if h.mods & MOD_WIN.0 != 0 {
        parts.push("Win".to_string());
    }
    parts.push(canonical_key_name(h.vk));
    parts.join("+")
}

fn canonical_key_name(vk: u32) -> String {
    if (0x41..=0x5A).contains(&vk) || (0x30..=0x39).contains(&vk) {
        return (vk as u8 as char).to_string();
    }
    if (0x70..=0x87).contains(&vk) {
        return format!("F{}", vk - 0x6F);
    }
    match vk {
        0x2C => "PrintScreen",
        0x20 => "Space",
        0x0D => "Enter",
        0x09 => "Tab",
        0x1B => "Esc",
        0x08 => "Backspace",
        0x2E => "Delete",
        0x2D => "Insert",
        0x24 => "Home",
        0x23 => "End",
        0x21 => "PgUp",
        0x22 => "PgDn",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        _ => return format!("0x{vk:02X}"),
    }
    .to_string()
}

/// UI form: same modifiers + locale-aware key name (`GetKeyNameTextW`,
/// so a Russian layout shows its own glyphs); unnamed keys stay `0xNN`.
pub fn combo_display(h: &Hotkey) -> String {
    let mut parts = Vec::new();
    if h.mods & MOD_CONTROL.0 != 0 {
        parts.push("Ctrl".to_string());
    }
    if h.mods & MOD_SHIFT.0 != 0 {
        parts.push("Shift".to_string());
    }
    if h.mods & MOD_ALT.0 != 0 {
        parts.push("Alt".to_string());
    }
    if h.mods & MOD_WIN.0 != 0 {
        parts.push("Win".to_string());
    }
    parts.push(vk_display_name(h.vk));
    parts.join("+")
}

/// Locale-aware VK name; `0xNN` when the layout has no name for it.
pub fn vk_display_name(vk: u32) -> String {
    if vk > 0xFF {
        return format!("0x{vk:02X}");
    }
    unsafe {
        let sc = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC);
        if sc != 0 {
            for ext in [0u32, 0x0100_0000] {
                let mut buf = [0u16; 64];
                let n = GetKeyNameTextW(((sc << 16) | ext) as i32, &mut buf);
                if n > 0 {
                    return String::from_utf16_lossy(&buf[..n as usize]);
                }
            }
        }
    }
    format!("0x{vk:02X}")
}

/// Combos Windows owns regardless of RegisterHotKey: refuse with a message
/// instead of a silent probe failure.
pub fn is_reserved_combo(h: &Hotkey) -> bool {
    // Bare PrintScreen (clipboard/snip) and Win+PrintScreen (save shot).
    if h.vk == VK_SNAPSHOT.0 as u32 && (h.mods == 0 || h.mods == MOD_WIN.0) {
        return true;
    }
    // Win+Shift+S: system snipping overlay.
    if h.vk == 0x53 && h.mods == (MOD_WIN.0 | MOD_SHIFT.0) {
        return true;
    }
    false
}

/// Live probe: try the combo on `hwnd` with a scratch id. False = taken or
/// otherwise unregistrable (caller keeps the old combo + reports).
pub fn probe_combo(hwnd: HWND, h: &Hotkey) -> bool {
    unsafe {
        if RegisterHotKey(Some(hwnd), PROBE_ID, HOT_KEY_MODIFIERS(h.mods), h.vk).is_err() {
            return false;
        }
        let _ = UnregisterHotKey(Some(hwnd), PROBE_ID);
    }
    true
}

/// Modifier state right now (for the capture box): Shift/Ctrl/Alt/Win.
pub fn current_modifiers() -> u32 {
    unsafe {
        let mut mods = 0u32;
        if GetKeyState(VK_SHIFT.0 as i32) < 0 {
            mods |= MOD_SHIFT.0;
        }
        if GetKeyState(VK_CONTROL.0 as i32) < 0 {
            mods |= MOD_CONTROL.0;
        }
        if GetKeyState(VK_MENU.0 as i32) < 0 {
            mods |= MOD_ALT.0;
        }
        if GetKeyState(VK_LWIN.0 as i32) < 0 || GetKeyState(VK_RWIN.0 as i32) < 0 {
            mods |= MOD_WIN.0;
        }
        mods
    }
}

/// Register `h` on `hwnd` (receives WM_HOTKEY with `id`).
pub fn register_on(hwnd: HWND, id: i32, h: &Hotkey) -> Result<()> {
    unsafe {
        RegisterHotKey(Some(hwnd), id, HOT_KEY_MODIFIERS(h.mods), h.vk)
            .with_context(|| format!("RegisterHotKey({}) failed (already taken?)", format_combo(h)))?;
    }
    Ok(())
}

pub fn unregister_on(hwnd: HWND, id: i32) {
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), id);
    }
}

/// Swap the live combo: unregister first, then register. False = the new
/// combo is taken (caller keeps the old one live and reports).
pub fn unregister_then_register(hwnd: HWND, h: &Hotkey) -> bool {
    unregister_on(hwnd, HOTKEY_ID);
    register_on(hwnd, HOTKEY_ID, h).is_ok()
}

/// Unregister the tray hotkey id (tray teardown).
pub fn unregister_shift_printscreen(hwnd: HWND) {
    unregister_on(hwnd, HOTKEY_ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_roundtrips() {
        let h = parse_combo("Shift+PrintScreen").unwrap();
        assert_eq!((h.mods, h.vk), (MOD_SHIFT.0, VK_SNAPSHOT.0 as u32));
        assert_eq!(format_combo(&h), "Shift+PrintScreen");
        let c = parse_combo("ctrl+alt+S").unwrap();
        assert_eq!(format_combo(&c), "Ctrl+Alt+S");
        let w = parse_combo("Win+F12").unwrap();
        assert_eq!(format_combo(&w), "Win+F12");
        // Bare key, hex, case/space tolerance.
        assert_eq!(format_combo(&parse_combo(" s ").unwrap()), "S");
        assert_eq!(parse_combo("0x41").unwrap().vk, 0x41);
        // Invalid: unknown names, dangling modifier, empty.
        assert!(parse_combo("Foo+Bar").is_none());
        assert!(parse_combo("Ctrl+").is_none());
        assert!(parse_combo("").is_none());
        assert!(parse_combo("Shift").is_none());
    }

    #[test]
    fn reserved_combos_flagged() {
        assert!(is_reserved_combo(&parse_combo("PrintScreen").unwrap()));
        assert!(is_reserved_combo(&parse_combo("Win+PrintScreen").unwrap()));
        assert!(is_reserved_combo(&parse_combo("Win+Shift+S").unwrap()));
        assert!(!is_reserved_combo(&parse_combo("Shift+PrintScreen").unwrap()));
        assert!(!is_reserved_combo(&parse_combo("Ctrl+Alt+S").unwrap()));
    }

    #[test]
    fn unnamed_vk_falls_back_to_hex() {
        // 0xF5 is unassigned: no layout names it, in any session.
        assert_eq!(vk_display_name(0xF5), "0xF5");
        assert_eq!(vk_display_name(0x1FF), "0x1FF");
        // Named keys never come back empty (exact glyph is layout-dependent).
        assert!(!vk_display_name(0x41).is_empty());
    }
}
