// System tray: hand-rolled NOTIFYICONDATAW on a message-only window.
// The same window receives RegisterHotKey notifications, so one GetMessage
// loop drives hotkeys + tray. Double-click captures, right-click menus.
use std::ptr::null_mut;

use anyhow::Result;
use windows::{
    core::w,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        UI::{
            Shell::{
                Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
            },
            WindowsAndMessaging::{
                CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, DispatchMessageW, GetCursorPos, GetWindowLongPtrW, GetMessageW,
                LoadCursorW, LoadIconW, PostQuitMessage, RegisterClassW, SetForegroundWindow,
                SetWindowLongPtrW, TrackPopupMenu, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
                GWLP_USERDATA, HWND_MESSAGE, IDC_ARROW, IDI_APPLICATION, MF_STRING,
                MSG, TPM_RETURNCMD, WM_APP, WM_DESTROY, WM_HOTKEY, WM_LBUTTONDBLCLK,
                WM_NCCREATE, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
            },
        },
    },
};

use crate::{config::AppConfig, worker::OcrWorker};

const WM_TRAY: u32 = WM_APP + 10;
const T_CAPTURE: usize = 3001;
const T_QUIT: usize = 3002;
const T_ABOUT: usize = 3003;
const T_SETTINGS: usize = 3004;

struct Ctx {
    worker: OcrWorker,
    cfg: AppConfig,
    hwnd: HWND,
}

pub fn run_tray(worker: OcrWorker, cfg: AppConfig) -> Result<()> {
    unsafe {
        static mut REGISTERED: bool = false;
        if !REGISTERED {
            let cls = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(tray_wndproc),
                hInstance: windows::Win32::Foundation::HINSTANCE(null_mut()),
                lpszClassName: w!("SelectableTray"),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..std::mem::zeroed()
            };
            let _ = RegisterClassW(&cls);
            REGISTERED = true;
        }
        let mut ctx = Box::new(Ctx { worker, cfg, hwnd: HWND(null_mut()) });
        let hwnd = CreateWindowExW(
            Default::default(),
            w!("SelectableTray"),
            w!("Selectable"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            Some(ctx.as_mut() as *mut Ctx as *const _),
        )?;
        // Hotkey lives on this window now (not the bare thread).
        // First configured combo wins; unparseable/taken falls back to default.
        let hk = ctx
            .cfg
            .combos
            .first()
            .and_then(|s| crate::hotkey::parse_combo(s))
            .unwrap_or_else(crate::hotkey::default_hotkey);
        let hk_ok = crate::hotkey::register_on(hwnd, crate::hotkey::HOTKEY_ID, &hk).is_ok();
        ctx.hwnd = hwnd;
        let tip = if hk_ok {
            format!("Selectable ({})", crate::hotkey::format_combo(&hk))
        } else {
            "Selectable (热键被占用，双击截图)".to_string()
        };
        add_tray_icon(hwnd, &tip)?;
        std::mem::forget(ctx);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

fn ctx_from(hwnd: HWND) -> *mut Ctx {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ctx }
}

fn add_tray_icon(hwnd: HWND, tip: &str) -> Result<()> {
    unsafe {
        let icon = LoadIconW(None, IDI_APPLICATION)?;
        let mut nid: windows::Win32::UI::Shell::NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of_val(&nid) as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = icon;
        for (i, c) in tip.encode_utf16().take(nid.szTip.len() - 1).enumerate() {
            nid.szTip[i] = c;
        }
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            anyhow::bail!("Shell_NotifyIconW(NIM_ADD) failed");
        }
        Ok(())
    }
}

fn remove_tray_icon(hwnd: HWND) {
    unsafe {
        let mut nid: windows::Win32::UI::Shell::NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of_val(&nid) as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

unsafe extern "system" fn tray_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lp.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let ptr = ctx_from(hwnd);
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let ctx = &mut *ptr;
    match msg {
        WM_HOTKEY => {
            do_capture(ctx);
            LRESULT(0)
        }
        WM_TRAY => {
            match lp.0 as u32 {
                WM_RBUTTONUP => tray_menu(ctx),
                WM_LBUTTONDBLCLK => do_capture(ctx),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            remove_tray_icon(hwnd);
            let _ = crate::hotkey::unregister_shift_printscreen(hwnd);
            drop(Box::from_raw(ptr));
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn tray_menu(ctx: &mut Ctx) {
    unsafe {
        let menu = CreatePopupMenu().unwrap_or_default();
        if menu.is_invalid() {
            return;
        }
        let ui = crate::lang::resolve_ui_lang(Some(&ctx.cfg.ui_lang));
        let mi = crate::settings::append_item;
        mi(menu, MF_STRING, T_CAPTURE, crate::lang::tr(ui, "menu.capture"));
        mi(menu, MF_STRING, T_SETTINGS, crate::lang::tr(ui, "menu.settings"));
        mi(menu, MF_STRING, T_ABOUT, crate::lang::tr(ui, "menu.about"));
        mi(menu, MF_STRING, T_QUIT, crate::lang::tr(ui, "menu.quit"));
        let mut pt = windows::Win32::Foundation::POINT { x: 0, y: 0 };
        let _ = GetCursorPos(&mut pt);
        // hwnd: message-only window has no real HWND for foreground; use its own.
        let hwnd = ctx.hwnd;
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD, pt.x, pt.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            T_CAPTURE => do_capture(ctx),
            T_SETTINGS => open_settings(ctx),
            T_ABOUT => crate::about::show_about(HWND(null_mut())),
            T_QUIT => {
                let _ = DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}

fn do_capture(ctx: &Ctx) {
    if let Err(e) = crate::capture_and_show(&ctx.worker, &ctx.cfg, None) {
        eprintln!("capture failed: {e:#}");
    }
}

/// Settings from the tray: centered on the cursor monitor is overkill; the
/// primary screen center is fine for a modal. Hotkey changes re-register
/// live on the tray window; OCR/ui-language changes need a restart.
fn open_settings(ctx: &mut Ctx) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let o = crate::settings::run_settings(ctx.hwnd, (w / 2, h / 2), Some(ctx.hwnd));
    if !o.saved {
        return;
    }
    if o.hotkey_changed {
        let new = crate::hotkey::parse_combo(&o.new_combo);
        let ok = new.map(|h| crate::hotkey::unregister_then_register(ctx.hwnd, &h)).unwrap_or(false);
        if !ok {
            // Keep the old combo live; the file already holds the new one
            // (next restart retries it), but say so loudly.
            settings_note("热键注册失败，已保留旧热键（重启后重试新热键）");
        }
    }
    // Pick up search/save/translate values for later captures.
    if let Ok(cfg) = AppConfig::load() {
        ctx.cfg = cfg;
    }
    if o.ocr_changed || o.ui_lang_changed || (o.hotkey_changed && o.needs_restart) {
        settings_note("设置已保存，OCR/界面语言/热键部分需重启生效");
    }
}

fn settings_note(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
    unsafe {
        let t: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let cap: Vec<u16> = "Selectable".encode_utf16().chain([0]).collect();
        MessageBoxW(
            None,
            windows::core::PCWSTR(t.as_ptr()),
            windows::core::PCWSTR(cap.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}
