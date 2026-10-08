// Fullscreen overlay: screenshot background + OCR boxes + Save button.
// Blocking show_overlay() runs its own message loop until dismissed (Esc/menu).
// Save-As uses the standard system dialog (GetSaveFileNameW), nothing hand-rolled.
use std::ptr::null_mut;

use anyhow::{Context, Result};
use image::RgbImage;
use windows::{
    core::{w, HSTRING, PCWSTR, PWSTR},
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, SYSTEMTIME, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreatePen, DeleteObject, EndPaint, GetStockObject,
            HOLLOW_BRUSH, InvalidateRect, PAINTSTRUCT, PS_DOT, PS_SOLID, Rectangle, SelectObject,
            StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
        },
        System::SystemInformation::GetLocalTime,
        UI::{
            Input::KeyboardAndMouse::VK_ESCAPE,
            Shell::{
                FOLDERID_Documents, SHGetKnownFolderPath, ShellExecuteW, KF_FLAG_DEFAULT,
            },
            Controls::Dialogs::{
                GetSaveFileNameW, OPENFILENAMEW, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, DispatchMessageW, GetCursorPos, GetForegroundWindow, GetMessageW,
                GetSystemMetrics, GetWindowLongPtrW, GetWindowTextW,
                LoadCursorW, PostQuitMessage, RegisterClassW, SetForegroundWindow,
                SetWindowLongPtrW, SetWindowTextW, TrackPopupMenu, TranslateMessage,
                CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HMENU, IDC_ARROW,
                MENU_ITEM_FLAGS, MF_GRAYED, MF_STRING, MSG, SM_CXVIRTUALSCREEN,
                SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOWNORMAL,
                TPM_RETURNCMD, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY,
                WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT,
                WM_RBUTTONUP, WM_TIMER, WINDOW_STYLE, WNDCLASSW, WS_CHILD, WS_EX_TOOLWINDOW,
                WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, BS_PUSHBUTTON, SetTimer,
            },
        },
    },
};

use crate::{clipboard, ocr::OcrLine};

const NULL_HWND: HWND = HWND(null_mut());
const BTN_SAVE: i32 = 1001;
const M_COPY_SEL: usize = 2001;
const M_COPY_ALL: usize = 2002;
const M_SEARCH: usize = 2003;
const M_TRANSLATE: usize = 2004;
const M_SAVE: usize = 2005;
const M_SAVE_AS: usize = 2006;
const M_CLOSE: usize = 2007;

pub struct OverlayRequest {
    pub shot: RgbImage,
    pub lines: Vec<OcrLine>,
    pub active_title: String,
    pub search_url: String,
    pub translate_url: String,
    pub save_dir: Option<String>,
}

struct State {
    bgra: Vec<u8>,
    w: u32,
    h: u32,
    lines: Vec<OcrLine>,
    selected: Vec<usize>,
    drag_from: Option<(i32, i32)>,
    drag_to: (i32, i32),
    title: String,
    search_url: String,
    translate_url: String,
    save_dir: Option<String>,
    hwnd: HWND,
    btn: HWND,
}

fn state_from(hwnd: HWND) -> *mut State {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State }
}

pub fn active_window_title() -> String {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(fg, &mut buf);
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

pub fn show_overlay(req: OverlayRequest) -> Result<()> {
    show_overlay_inner(req, None)
}

/// Same as show_overlay but destroys the window after `ms` (headless smoke test).
pub fn show_overlay_autoclose(req: OverlayRequest, ms: u32) -> Result<()> {
    show_overlay_inner(req, Some(ms))
}

fn show_overlay_inner(req: OverlayRequest, autoclose_ms: Option<u32>) -> Result<()> {
    unsafe {
        static mut REGISTERED: bool = false;
        if !REGISTERED {
            let cls = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: windows::Win32::Foundation::HINSTANCE(null_mut()),
                lpszClassName: w!("SelectableOverlay"),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..std::mem::zeroed()
            };
            let _ = RegisterClassW(&cls);
            REGISTERED = true;
        }
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);

        let (w, h) = req.shot.dimensions();
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for (i, p) in req.shot.pixels().enumerate() {
            let o = i * 4;
            bgra[o] = p[2];
            bgra[o + 1] = p[1];
            bgra[o + 2] = p[0];
            bgra[o + 3] = 255;
        }
        let mut state = Box::new(State {
            bgra,
            w,
            h,
            lines: req.lines,
            selected: Vec::new(),
            drag_from: None,
            drag_to: (0, 0),
            title: req.active_title,
            search_url: req.search_url,
            translate_url: req.translate_url,
            save_dir: req.save_dir,
            hwnd: NULL_HWND,
            btn: NULL_HWND,
        });

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("SelectableOverlay"),
            w!("Selectable"),
            WS_POPUP | WS_VISIBLE,
            vx,
            vy,
            vw,
            vh,
            None,
            None,
            None,
            Some(state.as_mut() as *mut State as *const _),
        )?;
        state.hwnd = hwnd;
        // Save button, bottom-right above the taskbar area.
        let btn = CreateWindowExW(
            Default::default(),
            w!("BUTTON"),
            w!("保存"),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            vw - 150,
            vh - 76,
            130,
            40,
            Some(hwnd),
            Some(HMENU(BTN_SAVE as _)),
            None,
            None,
        )?;
        state.btn = btn;
        // Keep the box alive for the window lifetime; freed on WM_DESTROY.
        std::mem::forget(state);
        if let Some(ms) = autoclose_ms {
            unsafe {
                let _ = SetTimer(Some(hwnd), 1, ms, None);
            }
        }
        println!("overlay: window shown ({}x{})", vw, vh);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lp.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let ptr = state_from(hwnd);
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let st = &mut *ptr;
    match msg {
        WM_PAINT => {
            paint(st);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = (loword(lp.0) as i32, hiword(lp.0) as i32);
            if let Some(i) = hit_box(st, x, y) {
                st.selected = vec![i];
                let text = st.lines[i].text.clone();
                match clipboard::set_text(&text) {
                    Ok(_) => set_status(st, &format!("已复制：{}", preview(&text))),
                    Err(e) => set_status(st, &format!("复制失败：{e:#}")),
                }
            } else {
                st.selected.clear();
                st.drag_from = Some((x, y));
                st.drag_to = (x, y);
            }
            invalidate(st);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if st.drag_from.is_some() {
                st.drag_to = (loword(lp.0) as i32, hiword(lp.0) as i32);
                invalidate(st);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(from) = st.drag_from.take() {
                let r = norm_rect(from, st.drag_to);
                st.selected = boxes_in(st, r);
                if !st.selected.is_empty() {
                    let text = selected_text(st);
                    match clipboard::set_text(&text) {
                        Ok(_) => set_status(st, &format!("已复制 {} 行", st.selected.len())),
                        Err(e) => set_status(st, &format!("复制失败：{e:#}")),
                    }
                }
                invalidate(st);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            popup_menu(st);
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            // Right-click on the Save button -> system Save-As dialog.
            if HWND(wp.0 as _) == st.btn {
                do_save_as(st);
            } else {
                popup_menu(st);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xffff) as usize;
            if lp.0 != 0 {
                // From the Save button.
                if id == BTN_SAVE as usize {
                    do_quick_save(st);
                }
            } else {
                match id {
                    M_COPY_SEL => {
                        let text = selected_text(st);
                        finish_clip(st, clipboard::set_text(&text), &format!("已复制 {} 行", st.selected.len()));
                    }
                    M_COPY_ALL => {
                        let text = all_text(st);
                        finish_clip(st, clipboard::set_text(&text), &format!("已复制全部 {} 行", st.lines.len()));
                    }
                    M_SEARCH => open_url(&fill_url(&st.search_url, &selected_or_all(st))),
                    M_TRANSLATE => open_url(&fill_url(&st.translate_url, &selected_or_all(st))),
                    M_SAVE => do_quick_save(st),
                    M_SAVE_AS => do_save_as(st),
                    M_CLOSE => {
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wp.0 as u32 == VK_ESCAPE.0 as u32 {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            drop(Box::from_raw(ptr));
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_TIMER => {
            println!("overlay: auto-close timer");
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn loword(v: isize) -> u16 {
    (v & 0xffff) as u16
}
fn hiword(v: isize) -> u16 {
    ((v >> 16) & 0xffff) as u16
}
fn norm_rect(a: (i32, i32), b: (i32, i32)) -> (i32, i32, i32, i32) {
    (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}
fn preview(s: &str) -> String {
    const N: usize = 24;
    if s.chars().count() > N {
        format!("{}…", s.chars().take(N).collect::<String>())
    } else {
        s.to_string()
    }
}

fn invalidate(st: &State) {
    unsafe {
        let _ = InvalidateRect(Some(st.hwnd), None, false);
    }
}

fn set_status(st: &State, msg: &str) {
    unsafe {
        let t = HSTRING::from(format!("Selectable — {msg}"));
        let _ = SetWindowTextW(st.hwnd, &t);
    }
}

fn finish_clip(st: &State, r: Result<()>, ok: &str) {
    match r {
        Ok(_) => set_status(st, ok),
        Err(e) => set_status(st, &format!("复制失败：{e:#}")),
    }
}

fn hit_box(st: &State, x: i32, y: i32) -> Option<usize> {
    for (i, l) in st.lines.iter().enumerate().rev() {
        if l.quad.contains_point(x as f32, y as f32) {
            return Some(i);
        }
    }
    None
}

fn boxes_in(st: &State, r: (i32, i32, i32, i32)) -> Vec<usize> {
    st.lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            let (x0, y0, x1, y1) = l.quad.axis_aligned_bounds();
            (x0 as i32) < r.2 && (x1 as i32) > r.0 && (y0 as i32) < r.3 && (y1 as i32) > r.1
        })
        .map(|(i, _)| i)
        .collect()
}

fn selected_text(st: &State) -> String {
    st.selected.iter().map(|&i| st.lines[i].text.as_str()).collect::<Vec<_>>().join("\n")
}
fn all_text(st: &State) -> String {
    st.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
}
fn selected_or_all(st: &State) -> String {
    if st.selected.is_empty() {
        all_text(st)
    } else {
        selected_text(st)
    }
}

fn paint(st: &State) {
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let hdc = BeginPaint(st.hwnd, &mut ps);
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = st.w as i32;
        bmi.bmiHeader.biHeight = -(st.h as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0 as u32;
        StretchDIBits(
            hdc,
            0,
            0,
            st.w as i32,
            st.h as i32,
            0,
            0,
            st.w as i32,
            st.h as i32,
            Some(st.bgra.as_ptr() as *const _),
            &bmi,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
        let hollow = GetStockObject(HOLLOW_BRUSH);
        let red = CreatePen(PS_SOLID, 2, COLORREF(0x0000FF));
        let green = CreatePen(PS_SOLID, 3, COLORREF(0x00FF00));
        let dot = CreatePen(PS_DOT, 1, COLORREF(0x000000));
        let old_brush = SelectObject(hdc, hollow);
        let old_pen = SelectObject(hdc, red.into());
        for (i, l) in st.lines.iter().enumerate() {
            if st.selected.contains(&i) {
                SelectObject(hdc, green.into());
            } else {
                SelectObject(hdc, red.into());
            }
            let (x0, y0, x1, y1) = l.quad.axis_aligned_bounds();
            let _ = Rectangle(hdc, x0 as i32, y0 as i32, x1 as i32, y1 as i32);
        }
        if let Some(from) = st.drag_from {
            SelectObject(hdc, dot.into());
            let r = norm_rect(from, st.drag_to);
            let _ = Rectangle(hdc, r.0, r.1, r.2, r.3);
        }
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        let _ = DeleteObject(red.into());
        let _ = DeleteObject(green.into());
        let _ = DeleteObject(dot.into());
        let _ = EndPaint(st.hwnd, &ps);
    }
}

fn popup_menu(st: &State) {
    unsafe {
        let menu = CreatePopupMenu().unwrap_or_default();
        if menu.is_invalid() {
            return;
        }
        let has_sel = !st.selected.is_empty();
        let _ = AppendMenuW(menu, menu_flags(has_sel), M_COPY_SEL, w!("复制选中文本"));
        let _ = AppendMenuW(menu, MF_STRING, M_COPY_ALL, w!("复制全部文本"));
        let _ = AppendMenuW(menu, menu_flags(has_sel), M_SEARCH, w!("浏览器搜索"));
        let _ = AppendMenuW(menu, menu_flags(has_sel), M_TRANSLATE, w!("外部翻译"));
        let _ = AppendMenuW(menu, MF_STRING, M_SAVE, w!("保存截图"));
        let _ = AppendMenuW(menu, MF_STRING, M_SAVE_AS, w!("另存为…"));
        let _ = AppendMenuW(menu, MF_STRING, M_CLOSE, w!("关闭"));
        let mut pt = windows::Win32::Foundation::POINT { x: 0, y: 0 };
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(st.hwnd);
        let _ = TrackPopupMenu(menu, TPM_RETURNCMD, pt.x, pt.y, None, st.hwnd, None);
        let _ = DestroyMenu(menu);
    }
}

fn menu_flags(enabled: bool) -> MENU_ITEM_FLAGS {
    if enabled {
        MF_STRING
    } else {
        MF_STRING | MF_GRAYED
    }
}

// ---------------- saving ----------------

fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    while s.ends_with(' ') || s.ends_with('.') {
        s.pop();
    }
    if s.is_empty() {
        s = "screenshot".to_string();
    }
    if s.chars().count() > 60 {
        s = s.chars().take(60).collect();
    }
    s
}

fn local_stamp() -> String {
    unsafe {
        let st: SYSTEMTIME = GetLocalTime();
        format!("{:04}-{:02}-{:02}-{:02}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond)
    }
}

pub fn default_filename(title: &str) -> String {
    format!("{}-{}.png", local_stamp(), sanitize(title))
}

pub fn screenshot_dir(override_dir: Option<&str>) -> Result<std::path::PathBuf> {
    if let Some(d) = override_dir.filter(|d| !d.is_empty()) {
        return Ok(std::path::PathBuf::from(d));
    }
    unsafe {
        let p: PWSTR = SHGetKnownFolderPath(&FOLDERID_Documents, KF_FLAG_DEFAULT, None)
            .context("SHGetKnownFolderPath(Documents) failed")?;
        let s = p.to_string().context("documents path not unicode")?;
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
        Ok(std::path::PathBuf::from(s).join("ScreenShot"))
    }
}

fn unique_path(mut path: std::path::PathBuf) -> std::path::PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().unwrap().to_string_lossy().to_string();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for i in 1..1000 {
        path.set_file_name(format!("{stem}_{i}{ext}"));
        if !path.exists() {
            return path;
        }
    }
    path
}

fn save_failed(st: &State, e: &anyhow::Error) {
    eprintln!("save failed: {e:#}");
    set_status(st, &format!("保存失败：{e:#}"));
}

fn do_quick_save(st: &State) {
    match (|| -> Result<String> {
        let dir = screenshot_dir(st.save_dir.as_deref())?;
        std::fs::create_dir_all(&dir)?;
        let path = unique_path(dir.join(default_filename(&st.title)));
        let shot = rebuild_shot(st);
        shot.save(&path)?;
        Ok(path.to_string_lossy().to_string())
    })() {
        Ok(p) => set_status(st, &format!("已保存：{p}")),
        Err(e) => save_failed(st, &e),
    }
}

fn rebuild_shot(st: &State) -> RgbImage {
    let mut img = RgbImage::new(st.w, st.h);
    for (i, p) in img.pixels_mut().enumerate() {
        let o = i * 4;
        p.0 = [st.bgra[o + 2], st.bgra[o + 1], st.bgra[o]];
    }
    img
}

fn do_save_as(st: &State) {
    match save_as_dialog(st) {
        Ok(Some(p)) => set_status(st, &format!("已保存：{}", p.to_string_lossy())),
        Ok(None) => {}
        Err(e) => save_failed(st, &e),
    }
}

fn save_as_dialog(st: &State) -> Result<Option<std::path::PathBuf>> {
    unsafe {
        let dir = screenshot_dir(st.save_dir.as_deref())?;
        std::fs::create_dir_all(&dir)?;
        let name = default_filename(&st.title);
        let mut file_buf: Vec<u16> = name.encode_utf16().chain([0]).collect();
        file_buf.resize(1024, 0);
        let filter: Vec<u16> = "PNG 图片\0*.png\0所有文件\0*.*\0\0"
            .encode_utf16()
            .collect();
        let dir_w: Vec<u16> = dir.to_string_lossy().encode_utf16().chain([0]).collect();
        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = st.hwnd;
        ofn.lpstrFilter = PCWSTR(filter.as_ptr());
        ofn.lpstrFile = PWSTR(file_buf.as_mut_ptr());
        ofn.nMaxFile = file_buf.len() as u32;
        ofn.lpstrInitialDir = PCWSTR(dir_w.as_ptr());
        ofn.lpstrDefExt = w!("png");
        ofn.Flags = OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST;
        if GetSaveFileNameW(&mut ofn).as_bool() {
            let len = file_buf.iter().position(|&c| c == 0).unwrap_or(0);
            let mut path = std::path::PathBuf::from(String::from_utf16_lossy(&file_buf[..len]));
            if path.extension().is_none() {
                path.set_extension("png");
            }
            let shot = rebuild_shot(st);
            shot.save(&path)?;
            Ok(Some(path))
        } else {
            Ok(None) // cancelled (or error 0); nothing to report
        }
    }
}

// ---------------- urls ----------------

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn fill_url(template: &str, text: &str) -> String {
    template.replace("{text}", &url_encode(text))
}

fn open_url(url: &str) {
    unsafe {
        let u = HSTRING::from(url);
        let _ = ShellExecuteW(None, w!("open"), &u, None, None, SW_SHOWNORMAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_filename_has_stamp_and_clean_title() {
        let n = default_filename("记事本: test?.txt  ");
        assert!(n.ends_with("-记事本_ test_.txt.png"), "got {n}");
        assert!(n.starts_with(|c: char| c.is_ascii_digit()), "got {n}");
        let empty = default_filename("");
        assert!(empty.ends_with("-screenshot.png"), "got {empty}");
    }

    #[test]
    fn screenshot_dir_defaults_under_documents() {
        let d = screenshot_dir(None).unwrap();
        assert!(d.ends_with("ScreenShot"));
        assert!(d.parent().unwrap().is_dir());
    }
}
