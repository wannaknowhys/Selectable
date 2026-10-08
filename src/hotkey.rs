// Selectable hotkey listener (v1: single combo, stored as a list for later UI).
// Uses RegisterHotKey + a blocking GetMessage loop on the calling thread.
use anyhow::{Context, Result};
use windows::Win32::{
    Foundation::HWND,
    UI::{
        Input::KeyboardAndMouse::{
            RegisterHotKey, UnregisterHotKey, MOD_SHIFT, VK_SNAPSHOT,
        },
        WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY},
    },
};

pub const HOTKEY_ID: i32 = 1;
const NULL_HWND: HWND = HWND(std::ptr::null_mut());

pub struct Hotkeys {
    registered: Vec<i32>,
}

impl Hotkeys {
    /// Register Shift+PrintScreen.
    pub fn register_shift_printscreen() -> Result<Self> {
        unsafe {
            RegisterHotKey(Some(NULL_HWND), HOTKEY_ID, MOD_SHIFT, VK_SNAPSHOT.0 as u32)
                .context("RegisterHotKey(Shift+PrintScreen) failed (already taken?)")?;
        }
        Ok(Self { registered: vec![HOTKEY_ID] })
    }

    /// Blocking message loop; calls `on_hotkey` for every WM_HOTKEY.
    pub fn run_loop(&self, mut on_hotkey: impl FnMut()) {
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, Some(NULL_HWND), 0, 0).as_bool() {
                if msg.message == WM_HOTKEY {
                    on_hotkey();
                }
            }
        }
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        for id in &self.registered {
            unsafe {
                let _ = UnregisterHotKey(Some(NULL_HWND), *id);
            }
        }
    }
}
