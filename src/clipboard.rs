// Clipboard: CF_UNICODETEXT via GlobalAlloc (system owns the handle on success).
use anyhow::{Context, Result};
use windows::Win32::Foundation::GlobalFree;
use windows::Win32::{
    Foundation::{HANDLE, HGLOBAL, HWND},
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
        Ole::CF_UNICODETEXT,
    },
};

const NULL_HWND: HWND = HWND(std::ptr::null_mut());

pub fn set_text(text: &str) -> Result<()> {
    unsafe {
        OpenClipboard(Some(NULL_HWND)).context("OpenClipboard failed")?;
        let done = |r: Result<()>| {
            let _ = CloseClipboard();
            r
        };
        if EmptyClipboard().is_err() {
            return done(Err(anyhow::anyhow!("EmptyClipboard failed")));
        }
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let mem: HGLOBAL = match GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            Ok(m) => m,
            Err(e) => return done(Err(anyhow::anyhow!("GlobalAlloc failed: {e}"))),
        };
        let lock = GlobalLock(mem);
        if lock.is_null() {
            let _ = GlobalFree(Some(mem));
            return done(Err(anyhow::anyhow!("GlobalLock failed")));
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), lock as *mut u16, wide.len());
        let _ = GlobalUnlock(mem);
        match SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(mem.0))) {
            Ok(_) => done(Ok(())),
            Err(e) => {
                let _ = GlobalFree(Some(mem));
                done(Err(anyhow::anyhow!("SetClipboardData failed: {e}")))
            }
        }
    }
}
