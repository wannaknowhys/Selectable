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
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
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
        let hk_ok = crate::hotkey::register_shift_printscreen_on(hwnd).is_ok();
        ctx.hwnd = hwnd;
        let tip = if hk_ok { "Selectable (Shift+PrintScreen)" } else { "Selectable (热键被占用，双击截图)" };
        add_tray_icon(hwnd, tip)?;
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
        let _ = AppendMenuW(menu, MF_STRING, T_CAPTURE, w!("截图取词"));
        let _ = AppendMenuW(menu, MF_STRING, T_QUIT, w!("退出"));
        let mut pt = windows::Win32::Foundation::POINT { x: 0, y: 0 };
        let _ = GetCursorPos(&mut pt);
        // hwnd: message-only window has no real HWND for foreground; use its own.
        let hwnd = ctx.hwnd;
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD, pt.x, pt.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            T_CAPTURE => do_capture(ctx),
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
