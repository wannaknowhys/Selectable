// Selectable hotkey (v1: single combo, stored as a list for later UI).
// Registered on a caller-owned window; the owner pumps messages.
use anyhow::{Context, Result};
use windows::Win32::{
    Foundation::HWND,
    UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, MOD_SHIFT, VK_SNAPSHOT,
    },
};

pub const HOTKEY_ID: i32 = 1;

/// Register Shift+PrintScreen on `hwnd` (receives WM_HOTKEY).
pub fn register_shift_printscreen_on(hwnd: HWND) -> Result<()> {
    unsafe {
        RegisterHotKey(Some(hwnd), HOTKEY_ID, MOD_SHIFT, VK_SNAPSHOT.0 as u32)
            .context("RegisterHotKey(Shift+PrintScreen) failed (already taken?)")?;
    }
    Ok(())
}

pub fn unregister_shift_printscreen(hwnd: HWND) {
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
    }
}
