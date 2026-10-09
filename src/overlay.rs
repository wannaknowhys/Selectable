// Fullscreen overlay, M3 edition:
// - Pops instantly with a spinner; OCR arrives async from the worker thread.
// - Owner-drawn translucent global buttons (Save / Translate), top-center,
//   shifted down if they would cover text.
// - Click = whole box; press-drag = char-range selection (CTC alignment strips),
//   rendered white-on-dark-blue, cross-box in reading order.
// - Copy toast bottom-left, 3s fade. Translation engine is a stub (toasts);
//   its state machine + render path are already wired.
use std::{collections::HashMap, ptr::null_mut, time::{Duration, Instant}};

use anyhow::{Context, Result};
use image::RgbImage;
use windows::{
    core::{w, HSTRING, PCWSTR, PWSTR},
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SYSTEMTIME, WPARAM},
        Graphics::Gdi::{
            AlphaBlend, BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
            CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteDC, DeleteObject,
            DrawTextW, EndPaint, FillRect, GetDC, GetStockObject, SelectClipRgn, SelectObject,
            SetBkMode, SetTextColor, StretchDIBits, AC_SRC_OVER, BACKGROUND_MODE, BITMAPINFO, BITMAPINFOHEADER,
            BI_RGB, BLENDFUNCTION, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
            DEFAULT_QUALITY, DIB_RGB_COLORS, DT_CENTER, DT_LEFT, DT_SINGLELINE, DT_VCENTER,
            FW_NORMAL, HBITMAP, HDC, HFONT, HGDIOBJ, HOLLOW_BRUSH, InvalidateRect, OUT_DEFAULT_PRECIS,
            PAINTSTRUCT, PS_DOT, PS_SOLID, Polygon, Rectangle,
            SRCCOPY, TRANSPARENT,
        },
        System::SystemInformation::GetLocalTime,
        UI::{
            Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_ESCAPE},
            Shell::{
                FOLDERID_Documents, SHGetKnownFolderPath, ShellExecuteW, KF_FLAG_DEFAULT,
            },
            Controls::Dialogs::{
                GetSaveFileNameW, OPENFILENAMEW, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, DispatchMessageW, GetCursorPos, GetForegroundWindow, GetMessageW,
                GetSystemMetrics, GetWindowLongPtrW, GetWindowTextW, LoadCursorW,
                PostQuitMessage, RegisterClassW, SetForegroundWindow,
                SetWindowLongPtrW, SetWindowTextW, TrackPopupMenu, TranslateMessage,
                CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW,
                MENU_ITEM_FLAGS, MF_GRAYED, MF_STRING, MSG, SM_CXVIRTUALSCREEN,
                SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOWNORMAL,
                TPM_RETURNCMD, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY,
                WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT,
                WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW,
                WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, SetTimer,
            },
        },
    },
};

use crate::{clipboard, ocr::OcrLine, worker::OcrReply};

const NULL_HWND: HWND = HWND(null_mut());
const M_COPY_SEL: usize = 2001;
const M_COPY_ALL: usize = 2002;
const M_SEARCH: usize = 2003;
const M_TRANSLATE: usize = 2004;
const M_SAVE: usize = 2005;
const M_SAVE_AS: usize = 2006;
const M_CLOSE: usize = 2007;
const TICK_MS: u32 = 50;
const TIMER_TICK: usize = 1;
const TIMER_CLOSE: usize = 2;
const TAP_MS: u64 = 300;
const TAP_PX: i32 = 6;

pub struct OverlayRequest {
    pub shot: RgbImage,
    pub active_title: String,
    pub search_url: String,
    pub translate_url: String,
    pub save_dir: Option<String>,
}

/// Char position inside the OCR lines.
type Pos = (usize, usize); // (line, char)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TMode {
    Idle,
    Partial,
    Full,
}

#[derive(Debug, Clone)]
struct Translated {
    mode: TMode,
    texts: Vec<String>, // per line; empty = untranslated
}

struct Toast {
    text: String,
    deadline: Instant,
}

/// Persistent offscreen surface: everything is composed here, then presented
/// with a single BitBlt (issue #2 — no more direct-to-front flicker).
struct BackBuf {
    hdc: HDC,
    hbmp: HBITMAP,
    old: HGDIOBJ,
    w: i32,
    h: i32,
}

impl BackBuf {
    fn new(w: i32, h: i32) -> Result<Self> {
        unsafe {
            let screen = GetDC(Some(NULL_HWND));
            let hdc = CreateCompatibleDC(Some(screen));
            if hdc.is_invalid() {
                let _ = windows::Win32::Graphics::Gdi::ReleaseDC(Some(NULL_HWND), screen);
                anyhow::bail!("CreateCompatibleDC failed");
            }
            let hbmp = CreateCompatibleBitmap(screen, w, h);
            let _ = windows::Win32::Graphics::Gdi::ReleaseDC(Some(NULL_HWND), screen);
            if hbmp.is_invalid() {
                let _ = DeleteDC(hdc);
                anyhow::bail!("CreateCompatibleBitmap failed");
            }
            let old = SelectObject(hdc, hbmp.into());
            Ok(Self { hdc, hbmp, old, w, h })
        }
    }
}

impl Drop for BackBuf {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.hdc, self.old);
            let _ = DeleteObject(self.hbmp.into());
            let _ = DeleteDC(self.hdc);
        }
    }
}

struct State {
    bgra: Vec<u8>,
    w: u32,
    h: u32,
    title: String,
    search_url: String,
    translate_url: String,
    save_dir: Option<String>,
    hwnd: HWND,
    vw: i32,
    vh: i32,
    // Async OCR.
    rx: *const std::sync::mpsc::Receiver<OcrReply>,
    ready: bool,
    load_error: Option<String>,
    lines: Vec<OcrLine>,
    tier: String,
    spinner: u32,
    // Selection.
    sel: Option<(Pos, Pos)>,
    press_at: Option<(Instant, (i32, i32))>,
    anchor: Option<Pos>,
    cursor: Option<Pos>,
    dragging: bool,
    // UI extras.
    toast: Option<Toast>,
    toast_level: u8, // last painted fade level; 0xFF forces repaint
    tmode: TMode,
    want_full_label: bool,
    translated: Option<Translated>,
    btn_save: (i32, i32, i32, i32),
    btn_tr: (i32, i32, i32, i32),
    ox: i32,
    oy: i32,
    mouse: (i32, i32),
    back: Option<BackBuf>,
    fonts: HashMap<u32, HFONT>,
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

pub fn show_overlay(req: OverlayRequest, rx: &std::sync::mpsc::Receiver<OcrReply>) -> Result<()> {
    show_overlay_inner(req, rx, None)
}

/// Headless smoke test: destroys the window after `ms`.
pub fn show_overlay_autoclose(
    req: OverlayRequest,
    rx: &std::sync::mpsc::Receiver<OcrReply>,
    ms: u32,
) -> Result<()> {
    show_overlay_inner(req, rx, Some(ms))
}

fn show_overlay_inner(
    req: OverlayRequest,
    rx: &std::sync::mpsc::Receiver<OcrReply>,
    autoclose_ms: Option<u32>,
) -> Result<()> {
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
            title: req.active_title,
            search_url: req.search_url,
            translate_url: req.translate_url,
            save_dir: req.save_dir,
            hwnd: NULL_HWND,
            vw,
            vh,
            rx: rx as *const _,
            ready: false,
            load_error: None,
            lines: Vec::new(),
            tier: String::new(),
            spinner: 0,
            sel: None,
            press_at: None,
            anchor: None,
            cursor: None,
            dragging: false,
            toast: None,
            toast_level: 0xFF,
            tmode: TMode::Idle,
            want_full_label: false,
            translated: None,
            btn_save: (0, 0, 0, 0),
            btn_tr: (0, 0, 0, 0),
            ox: vx,
            oy: vy,
            mouse: (0, 0),
            back: Some(BackBuf::new(vw, vh)?),
            fonts: HashMap::new(),
        });
        layout_buttons(&mut state);

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
        std::mem::forget(state);
        // Explicit show + topmost (belt and suspenders for exotic shells).
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{
                SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SW_SHOW,
                SWP_SHOWWINDOW,
            };
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
            let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut r);
            let vis = windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd);
            println!("overlay: rect=({},{})-({},{}) visible={}", r.left, r.top, r.right, r.bottom, vis.as_bool());
        }
        let _ = SetTimer(Some(hwnd), TIMER_TICK, TICK_MS, None);
        if let Some(ms) = autoclose_ms {
            let _ = SetTimer(Some(hwnd), TIMER_CLOSE, ms, None);
        }
        println!("overlay: window shown ({vw}x{vh})");

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}
fn layout_buttons(st: &mut State) {
    const BW: i32 = 140;
    const BH: i32 = 44;
    const GAP: i32 = 16;
    let total = BW * 2 + GAP;
    let x0 = (st.vw - total) / 2;
    let mut y = 24;
    let rects = [(x0, y, x0 + BW, y + BH), (x0 + BW + GAP, y, x0 + total, y + BH)];
    // Auto-avoid: if any OCR box overlaps the row, move it near the bottom.
    for l in &st.lines {
        let (x0b, y0b, x1b, y1b) = l.quad.axis_aligned_bounds();
        let (x0b, y0b, x1b, y1b) = (x0b as i32, y0b as i32, x1b as i32, y1b as i32);
        for r in &rects {
            if x0b < r.2 + 8 && x1b > r.0 - 8 && y0b < r.3 + 8 && y1b > r.1 - 8 {
                y = st.vh - 140;
                st.btn_save = (x0, y, x0 + BW, y + BH);
                st.btn_tr = (x0 + BW + GAP, y, x0 + total, y + BH);
                return;
            }
        }
    }
    st.btn_save = rects[0];
    st.btn_tr = rects[1];
}

/// Translate button: Idle press jumps straight to full; Partial/Full press
/// cancels back to original (label becomes 全文翻译 per spec).
fn translate_button(st: &mut State) {
    match st.tmode {
        TMode::Idle => translate_full(st),
        TMode::Partial | TMode::Full => {
            st.translated = None;
            st.tmode = TMode::Idle;
            st.want_full_label = true;
            set_title(st, "Selectable");
            invalidate(st);
        }
    }
}

/// Context-menu "翻译选中": partial overlay of the current selection.

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
        WM_TIMER => {
            on_tick(st, wp.0);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let p = (loword(lp.0) as i32, hiword(lp.0) as i32);
            st.mouse = p;
            if in_rect(p, st.btn_save) {
                dlog(format!("down save {p:?}"));
                do_quick_save(st);
            } else if in_rect(p, st.btn_tr) {
                dlog(format!("down translate {p:?}"));
                translate_button(st);
            } else {
                st.press_at = Some((Instant::now(), p));
                st.anchor = char_at_point(st, p);
                st.cursor = st.anchor;
                st.dragging = false;
                dlog(format!("down select {p:?} anchor={:?}", st.anchor));
                unsafe {
                    SetCapture(st.hwnd);
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let p = (loword(lp.0) as i32, hiword(lp.0) as i32);
            st.mouse = p;
            if st.press_at.is_some() {
                let (t0, p0) = st.press_at.unwrap();
                if !st.dragging
                    && (t0.elapsed() > Duration::from_millis(TAP_MS)
                        || dist(p0, p) > TAP_PX)
                {
                    st.dragging = true;
                }
                if st.dragging {
                    st.cursor = char_at_point(st, p).or(st.cursor);
                    invalidate(st);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            let p = (loword(lp.0) as i32, hiword(lp.0) as i32);
            if let Some((t0, p0)) = st.press_at.take() {
                let tap = t0.elapsed() <= Duration::from_millis(TAP_MS) && dist(p0, p) <= TAP_PX;
                if tap && !st.dragging {
                    // Click: select the whole box under cursor. Selection alone
                    // never touches the clipboard (issue #1); copy is Ctrl+C.
                    match line_at_point(st, p) {
                        Some(li) => {
                            let n = st.lines[li].chars.len();
                            st.sel = Some(((li, 0), (li, n)));
                            set_title(st, &format!("Selectable — 已选中 {} 字", count_chars(st)));
                        }
                        None => {
                            st.sel = None;
                            set_title(st, "Selectable");
                        }
                    }
                } else if st.dragging {
                    st.cursor = char_at_point(st, p).or(st.cursor);
                    st.sel = norm_range(st.anchor, st.cursor);
                    if st.sel.is_some() {
                        set_title(st, &format!("Selectable — 已选中 {} 字", count_chars(st)));
                    }
                }
                st.dragging = false;
                st.anchor = None;
                st.cursor = None;
                invalidate(st);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let p = (loword(lp.0) as i32, hiword(lp.0) as i32);
            dlog(format!("rbutton {p:?}"));
            popup_menu(st);
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            // Hit-test the Save button for right-click Save-As (client coords
            // need the virtual-screen origin subtracted).
            let mut pt = POINT { x: 0, y: 0 };
            let _ = GetCursorPos(&mut pt);
            let p = (pt.x - st.ox, pt.y - st.oy);
            if in_rect(p, st.btn_save) {
                do_save_as(st);
            } else {
                popup_menu(st);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xffff) as usize;
            match id {
                M_COPY_SEL => {
                    let text = selected_text(st);
                    copy_text(st, &text);
                }
                M_COPY_ALL => {
                    let text = all_text(st);
                    copy_text(st, &text);
                }
                M_SEARCH => open_url(&fill_url(&st.search_url, &selected_or_all(st))),
                M_TRANSLATE => translate_menu(st),
                M_SAVE => do_quick_save(st),
                M_SAVE_AS => do_save_as(st),
                M_CLOSE => {
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = wp.0 as u32;
            if vk == VK_ESCAPE.0 as u32 {
                let _ = DestroyWindow(hwnd);
            } else if vk == 0x43 && ctrl_held() {
                // Explicit copy only (issue #1).
                copy_text(st, &selected_text(st));
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            for (_, f) in st.fonts.drain() {
                let _ = DeleteObject(f.into());
            }
            drop(Box::from_raw(ptr));
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn on_tick(st: &mut State, id: usize) {
    if id == TIMER_CLOSE as usize {
        unsafe {
            let _ = DestroyWindow(st.hwnd);
        }
        return;
    }
    let mut dirty = false;
    // Spinner always advances while loading.
    if !st.ready {
        st.spinner = st.spinner.wrapping_add(1);
        // Poll the worker without blocking.
        let rx: &std::sync::mpsc::Receiver<OcrReply> = unsafe { &*st.rx };
        while let Ok(rep) = rx.try_recv() {
            if let Some(e) = rep.error {
                st.load_error = Some(e.clone());
                st.ready = true;
                show_toast(st, format!("OCR 失败：{e}"));
            } else {
                st.lines = rep.lines;
                st.tier = rep.tier;
                st.ready = true;
                layout_buttons(st);
                let mut info = format!("ready lines={} save={:?} tr={:?}", st.lines.len(), st.btn_save, st.btn_tr);
                for (i, l) in st.lines.iter().enumerate() {
                    let (x0, y0, x1, y1) = l.quad.axis_aligned_bounds();
                    info.push_str(&format!(" | {i}:({x0},{y0},{x1},{y1})[{:.2}]{}", l.score, preview(&l.text, 40)));
                }
                dlog(info);
                println!("overlay: OCR ready ({} lines, tier={})", st.lines.len(), st.tier);
                set_title(st, &format!("Selectable — {} 行", st.lines.len()));
            }
            dirty = true;
        }
        if !st.ready {
            dirty = true; // keep spinner turning
        }
    }
    if let Some(t) = &st.toast {
        if Instant::now() >= t.deadline {
            st.toast = None;
            st.toast_level = 0xFF;
            dirty = true;
        } else {
            // Quantize the fade to 8 levels: ~8 repaints per toast instead of 60.
            let remain = t.deadline.saturating_duration_since(Instant::now()).as_millis();
            let level = if remain > 1000 { 8 } else { (remain * 8 / 1000) as u8 + 1 };
            if level != st.toast_level {
                st.toast_level = level;
                dirty = true;
            }
        }
    }
    if dirty {
        invalidate(st);
    }
}

// ---------------- selection ----------------

fn line_at_point(st: &State, p: (i32, i32)) -> Option<usize> {
    for (i, l) in st.lines.iter().enumerate().rev() {
        if l.quad.contains_point(p.0 as f32, p.1 as f32) {
            return Some(i);
        }
    }
    None
}

/// Char index under the cursor within line `li` (nearest, clamped).
fn char_at_point(st: &State, p: (i32, i32)) -> Option<Pos> {
    let li = line_at_point(st, p)?;
    let line = &st.lines[li];
    if line.chars.is_empty() || !line.quad.ordered().is_horizontal() {
        return Some((li, 0)); // vertical lines: whole-box granularity
    }
    let q = line.quad.ordered();
    // Project onto the reading direction: fraction along top edge.
    let dx = q.points[1][0] - q.points[0][0];
    let dy = q.points[1][1] - q.points[0][1];
    let len2 = (dx * dx + dy * dy).max(1e-6);
    let mut f = ((p.0 as f32 - q.points[0][0]) * dx + (p.1 as f32 - q.points[0][1]) * dy) / len2;
    f = f.clamp(0.0, 1.0);
    // Nearest char boundary, then bias to the char containing f.
    let mut best = 0;
    let mut best_d = f32::INFINITY;
    for (ci, c) in line.chars.iter().enumerate() {
        let mid = (c.f0 + c.f1) * 0.5;
        let d = (mid - f).abs();
        if d < best_d {
            best_d = d;
            best = ci;
        }
        if f >= c.f0 && f < c.f1 {
            return Some((li, ci));
        }
    }
    Some((li, best))
}

fn norm_range(a: Option<Pos>, b: Option<Pos>) -> Option<(Pos, Pos)> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if x <= y { (x, y) } else { (y, x) }),
        _ => None,
    }
}

fn range_text(st: &State, r: (Pos, Pos)) -> String {
    let ((l0, c0), (l1, c1)) = r;
    let mut out = Vec::new();
    for li in l0..=l1.min(st.lines.len().saturating_sub(1)) {
        let line = &st.lines[li];
        let (a, b) = if li == l0 && li == l1 {
            (c0, c1)
        } else if li == l0 {
            (c0, line.chars.len())
        } else if li == l1 {
            (0, c1)
        } else {
            (0, line.chars.len())
        };
        let s: String = line.chars[a.min(line.chars.len())..b.min(line.chars.len())]
            .iter()
            .map(|c| c.text.as_str())
            .collect();
        // Vertical-fallback lines carry whole-box text in `text`.
        if s.is_empty() && !line.text.is_empty() && a == 0 {
            out.push(line.text.clone());
        } else {
            out.push(s);
        }
    }
    out.join("\n")
}

fn selected_text(st: &State) -> String {
    st.sel.map(|r| range_text(st, r)).unwrap_or_default()
}

fn all_text(st: &State) -> String {
    st.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
}

fn selected_or_all(st: &State) -> String {
    let s = selected_text(st);
    if s.is_empty() {
        all_text(st)
    } else {
        s
    }
}

fn ctrl_held() -> bool {
    unsafe { GetKeyState(VK_CONTROL.0 as i32) & 0x8000u16 as i16 != 0 }
}

fn count_chars(st: &State) -> usize {
    st.sel
        .map(|r| range_text(st, r).chars().filter(|c| *c != '\n').count())
        .unwrap_or(0)
}

/// THE clipboard choke point: every copy flows through here, and the toast is
/// built from the exact string handed to the clipboard. Nothing else in the
/// codebase may call clipboard::set_text.
fn copy_text(st: &mut State, text: &str) {
    if text.is_empty() {
        show_toast(st, "先选中要复制的文字".to_string());
        return;
    }
    match clipboard::set_text(text) {
        Ok(_) => show_toast(st, format!("已复制：{}", preview(text, 120))),
        Err(e) => show_toast(st, format!("复制失败：{e:#}")),
    }
}

fn preview(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

// ---------------- translation (placeholder engine) ----------------

/// Context-menu "翻译选中": partial overlay of the current selection.
fn translate_menu(st: &mut State) {
    if selected_text(st).is_empty() {
        show_toast(st, "先选中要翻译的文字".to_string());
        return;
    }
    // Engine stub: toasts instead of covering text. The state machine below
    // runs unchanged once translate_engine() returns real results.
    match translate_engine(st) {
        Some(tr) => {
            st.translated = Some(tr);
            st.tmode = TMode::Partial;
            invalidate(st);
        }
        None => show_toast(st, "离线翻译未就绪（占位）".to_string()),
    }
}

fn translate_full(st: &mut State) {
    match translate_engine(st) {
        Some(mut tr) => {
            tr.mode = TMode::Full;
            st.translated = Some(tr);
            st.tmode = TMode::Full;
            invalidate(st);
        }
        None => show_toast(st, "离线翻译未就绪（占位）".to_string()),
    }
}

/// Phase-2 slot: Bergamot enzh/zhen. Returns None until wired.
fn translate_engine(_st: &State) -> Option<Translated> {
    None
}

// ---------------- paint ----------------

fn dlog(msg: String) {
    if std::env::var("SELECTABLE_DEBUG").is_ok() {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("temp/clicks.log") {
            let _ = writeln!(f, "{msg}");
        }
    }
}

fn invalidate(st: &State) {
    unsafe {
        let _ = InvalidateRect(Some(st.hwnd), None, false);
    }
}

fn set_title(st: &State, msg: &str) {
    unsafe {
        let t = HSTRING::from(msg.to_string());
        let _ = SetWindowTextW(st.hwnd, &t);
    }
}

fn norm_rect(a: (i32, i32), b: (i32, i32)) -> (i32, i32, i32, i32) {
    (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

fn show_toast(st: &mut State, text: String) {
    let now = Instant::now();
    st.toast = Some(Toast { text, deadline: now + Duration::from_secs(3) });
    st.toast_level = 0xFF; // force first paint
    invalidate(st);
}

fn in_rect(p: (i32, i32), r: (i32, i32, i32, i32)) -> bool {
    p.0 >= r.0 && p.0 < r.2 && p.1 >= r.1 && p.1 < r.3
}
fn dist(a: (i32, i32), b: (i32, i32)) -> i32 {
    (a.0 - b.0).abs().max((a.1 - b.1).abs())
}
fn loword(v: isize) -> u16 {
    (v & 0xffff) as u16
}
fn hiword(v: isize) -> u16 {
    ((v >> 16) & 0xffff) as u16
}

fn font_for(st: &mut State, px: u32) -> windows::Win32::Graphics::Gdi::HFONT {
    let px = px.clamp(12, 64);
    if let Some(f) = st.fonts.get(&px) {
        return *f;
    }
    unsafe {
        let f = CreateFontW(
            px as i32,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            DEFAULT_QUALITY,
            0,
            w!("Microsoft YaHei UI"),
        );
        st.fonts.insert(px, f);
        f
    }
}

/// Translucent filled rounded bar with white text, alpha-blended over `hdc`.
fn blit_bar(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    r: (i32, i32, i32, i32),
    bg: COLORREF,
    text: &str,
    font_px: u32,
    fonts: &mut HashMap<u32, windows::Win32::Graphics::Gdi::HFONT>,
    alpha: u8,
) {
    unsafe {
        let w = (r.2 - r.0).max(1);
        let h = (r.3 - r.1).max(1);
        let screen = GetDC(Some(NULL_HWND));
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old_bmp = SelectObject(mem, bmp.into());
        // Background.
        let brush = CreateSolidBrush(bg);
        let rc = RECT { left: 0, top: 0, right: w, bottom: h };
        FillRect(mem, &rc, brush);
        let _ = DeleteObject(brush.into());
        // Text.
        let old_font = SelectObject(mem, (*fonts.get(&font_px).unwrap()).into());
        SetTextColor(mem, COLORREF(0xFFFFFF));
        SetBkMode(mem, TRANSPARENT);
        let mut txt: Vec<u16> = text.encode_utf16().collect();
        let mut trc = RECT { left: 12, top: 0, right: w - 12, bottom: h };
        DrawTextW(mem, &mut txt, &mut trc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        SelectObject(mem, old_font);
        // Rounded clip on the destination, then blend.
        let rgn = CreateRoundRectRgn(r.0, r.1, r.2, r.3, 18, 18);
        SelectClipRgn(hdc, Some(rgn));
        let bf = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: 0 };
        let _ = AlphaBlend(hdc, r.0, r.1, w, h, mem, 0, 0, w, h, bf);
        SelectClipRgn(hdc, None);
        let _ = DeleteObject(rgn.into());
        SelectObject(mem, old_bmp);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        let _ = windows::Win32::Graphics::Gdi::ReleaseDC(Some(NULL_HWND), screen);
    }
}

fn paint(st: &mut State) {
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let front = BeginPaint(st.hwnd, &mut ps);
        // Compose everything offscreen, present with one blit (issue #2).
        let hdc = st.back.as_ref().expect("backbuffer").hdc;
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = st.w as i32;
        bmi.bmiHeader.biHeight = -(st.h as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0 as u32;
        StretchDIBits(
            hdc, 0, 0, st.w as i32, st.h as i32, 0, 0, st.w as i32, st.h as i32,
            Some(st.bgra.as_ptr() as *const _), &bmi, DIB_RGB_COLORS, SRCCOPY,
        );

        if st.ready {
            // Line boxes.
            let hollow = GetStockObject(HOLLOW_BRUSH);
            let red = CreatePen(PS_SOLID, 2, COLORREF(0x0000FF));
            let dot = CreatePen(PS_DOT, 1, COLORREF(0x000000));
            let old_brush = SelectObject(hdc, hollow);
            let old_pen = SelectObject(hdc, red.into());
            for l in &st.lines {
                let (x0, y0, x1, y1) = l.quad.axis_aligned_bounds();
                let _ = Rectangle(hdc, x0 as i32, y0 as i32, x1 as i32, y1 as i32);
            }
            // Translated overlays (placeholder: none until engine lands).
            if let Some(tr) = st.translated.clone() {
                paint_translated(hdc, st, &tr);
            }
            // Selection: dark-blue strips + white chars.
            paint_selection(hdc, st);
            // Drag rubber band while press-dragging.
            if st.dragging {
                if let Some((_, p0)) = st.press_at {
                    SelectObject(hdc, dot.into());
                    let r = norm_rect(p0, st.mouse);
                    let _ = Rectangle(hdc, r.0, r.1, r.2, r.3);
                }
            }
            SelectObject(hdc, old_pen);
            SelectObject(hdc, old_brush);
            let _ = DeleteObject(red.into());
            let _ = DeleteObject(dot.into());
            // Buttons.
            paint_button(hdc, st, st.btn_save, "保存");
            let label = match st.tmode {
                TMode::Idle if st.want_full_label => "全文翻译",
                TMode::Idle => "翻译",
                TMode::Partial | TMode::Full => "取消翻译",
            };
            paint_button(hdc, st, st.btn_tr, label);
        } else {
            // Spinner while the worker runs.
            paint_spinner(hdc, st);
            if let Some(e) = &st.load_error {
                show_toast_paint(hdc, st, &format!("OCR 失败：{e}"), 220);
            }
        }

        // Toast on top of everything (alpha from the quantized level).
        let toast_data: Option<(String, u8)> = match &st.toast {
            Some(t) => {
                let a = st.toast_level.min(8);
                let alpha = if a >= 8 { 220 } else { a * 220 / 8 };
                Some((t.text.clone(), alpha))
            }
            None => None,
        };
        if let Some((text, alpha)) = toast_data {
            show_toast_paint(hdc, st, &text, alpha);
        }

        let _ = BitBlt(front, 0, 0, st.w as i32, st.h as i32, Some(hdc), 0, 0, SRCCOPY);
        let _ = EndPaint(st.hwnd, &ps);
    }
}

fn paint_spinner(hdc: windows::Win32::Graphics::Gdi::HDC, st: &State) {
    unsafe {
        let (cx, cy) = (st.vw / 2, st.vh / 2);
        for i in 0..12u32 {
            // Conic fade: newest segment brightest.
            let k = (i + 12 - st.spinner % 12) % 12;
            let v = (70 + k * 15) as u8;
            let brush = CreateSolidBrush(COLORREF((v as u32) | ((v as u32) << 8) | ((v as u32) << 16)));
            let a = (st.spinner as f32 * 30.0 + i as f32 * 30.0).to_radians();
            let (x, y) = ((cx as f32 + a.cos() * 30.0) as i32, (cy as f32 + a.sin() * 30.0) as i32);
            let _ = windows::Win32::Graphics::Gdi::Ellipse(hdc, x - 6, y - 6, x + 6, y + 6);
            let old = SelectObject(hdc, brush.into());
            let _ = DeleteObject(brush.into());
            let _ = old;
        }
        // Re-select a stock brush to avoid leaking the DC state (paint restores anyway).
        let hollow = GetStockObject(HOLLOW_BRUSH);
        SelectObject(hdc, hollow);
    }
}

fn paint_button(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    st: &mut State,
    r: (i32, i32, i32, i32),
    label: &str,
) {
    let font = font_for(st, 22);
    unsafe {
        let old = SelectObject(hdc, font.into());
        blit_bar(hdc, r, COLORREF(0x1E1E1E), label, 22, &mut st.fonts, 205);
        SelectObject(hdc, old);
    }
}

fn show_toast_paint(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    st: &mut State,
    text: &str,
    alpha: u8,
) {
    // Measure with a memory DC is overkill; fixed-height bar, width by chars.
    let w = (text.chars().count() as i32 * 20 + 48).min(st.vw / 2).max(200);
    let h = 52;
    let r = (24, st.vh - 40 - h, 24 + w, st.vh - 40);
    let font = font_for(st, 20);
    unsafe {
        let old = SelectObject(hdc, font.into());
        blit_bar(hdc, r, COLORREF(0x141414), text, 20, &mut st.fonts, alpha);
        SelectObject(hdc, old);
    }
}

fn paint_translated(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    st: &mut State,
    tr: &Translated,
) {
    unsafe {
        let brush = CreateSolidBrush(COLORREF(0x1E3C1E));
        let old_brush = SelectObject(hdc, brush.into());
        let old_bk = SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0xFFFFFF));
        let jobs: Vec<((u32, u32, u32, u32), String)> = st
            .lines
            .iter()
            .zip(tr.texts.iter())
            .filter(|(_, t)| !t.is_empty())
            .map(|(l, t)| (l.quad.axis_aligned_bounds(), t.clone()))
            .collect();
        for ((x0, y0, x1, y1), t) in jobs {
            let pts = [
                POINT { x: x0 as i32, y: y0 as i32 },
                POINT { x: x1 as i32, y: y0 as i32 },
                POINT { x: x1 as i32, y: y1 as i32 },
                POINT { x: x0 as i32, y: y1 as i32 },
            ];
            let _ = Polygon(hdc, &pts);
            let font = font_for(st, (y1 - y0).saturating_sub(4).max(12));
            let old_font = SelectObject(hdc, font.into());
            let mut txt: Vec<u16> = t.encode_utf16().collect();
            let mut rc = RECT { left: x0 as i32 + 2, top: y0 as i32, right: x1 as i32, bottom: y1 as i32 };
            DrawTextW(hdc, &mut txt, &mut rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
            SelectObject(hdc, old_font);
        }
        SetBkMode(hdc, BACKGROUND_MODE(old_bk as u32));
        SelectObject(hdc, old_brush);
        let _ = DeleteObject(brush.into());
    }
}

fn paint_selection(hdc: windows::Win32::Graphics::Gdi::HDC, st: &mut State) {
    let Some((a, b)) = st.sel else { return };
    unsafe {
        let brush = CreateSolidBrush(COLORREF(0xAD4600)); // dark blue (0x00BBGGRR)
        let old_brush = SelectObject(hdc, brush.into());
        let old_bk = SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0xFFFFFF));
        let ((l0, c0), (l1, c1)) = (a, b);
        for li in l0..=l1.min(st.lines.len().saturating_sub(1)) {
            // Borrow dance: compute strips first.
            let strips: Vec<(String, [[f32; 2]; 4], u32)> = {
                let line = &st.lines[li];
                if !line.quad.ordered().is_horizontal() || line.chars.is_empty() {
                    // Vertical fallback: whole box, whole text.
                    let (x0, y0, x1, y1) = line.quad.axis_aligned_bounds();
                    vec![(line.text.clone(), [[x0 as f32, y0 as f32], [x1 as f32, y0 as f32], [x1 as f32, y1 as f32], [x0 as f32, y1 as f32]], y1 - y0)]
                } else {
                    let q = line.quad.ordered();
                    let (s, e) = if li == l0 && li == l1 {
                        (c0, c1)
                    } else if li == l0 {
                        (c0, line.chars.len())
                    } else if li == l1 {
                        (0, c1)
                    } else {
                        (0, line.chars.len())
                    };
                    line.chars[s.min(line.chars.len())..e.min(line.chars.len())]
                        .iter()
                        .map(|c| {
                            let strip = q.hstrip(c.f0, c.f1);
                            let h = (strip.height_f32().round() as u32).max(8);
                            (c.text.clone(), strip.points, h)
                        })
                        .collect()
                }
            };
            for (text, pts, h) in strips {
                let ipts = [
                    POINT { x: pts[0][0] as i32, y: pts[0][1] as i32 },
                    POINT { x: pts[1][0] as i32, y: pts[1][1] as i32 },
                    POINT { x: pts[2][0] as i32, y: pts[2][1] as i32 },
                    POINT { x: pts[3][0] as i32, y: pts[3][1] as i32 },
                ];
                let _ = Polygon(hdc, &ipts);
                let font = font_for(st, h.saturating_sub(2).max(12));
                let old_font = SelectObject(hdc, font.into());
                let xs = [ipts[0].x, ipts[1].x, ipts[2].x, ipts[3].x];
                let ys = [ipts[0].y, ipts[1].y, ipts[2].y, ipts[3].y];
                let mut rc = RECT {
                    left: *xs.iter().min().unwrap(),
                    top: *ys.iter().min().unwrap(),
                    right: *xs.iter().max().unwrap(),
                    bottom: *ys.iter().max().unwrap(),
                };
                let mut txt: Vec<u16> = text.encode_utf16().collect();
                DrawTextW(hdc, &mut txt, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                SelectObject(hdc, old_font);
            }
        }
        SetBkMode(hdc, BACKGROUND_MODE(old_bk as u32));
        SelectObject(hdc, old_brush);
        let _ = DeleteObject(brush.into());
    }
}

fn popup_menu(st: &State) {
    unsafe {
        let menu = CreatePopupMenu().unwrap_or_default();
        if menu.is_invalid() {
            return;
        }
        let has_sel = st.sel.map(|r| !range_text(st, r).is_empty()).unwrap_or(false);
        let _ = AppendMenuW(menu, menu_flags(has_sel), M_COPY_SEL, w!("复制选中文本"));
        let _ = AppendMenuW(menu, MF_STRING, M_COPY_ALL, w!("复制全部文本"));
        let _ = AppendMenuW(menu, menu_flags(has_sel), M_SEARCH, w!("浏览器搜索"));
        let _ = AppendMenuW(menu, menu_flags(has_sel), M_TRANSLATE, w!("翻译选中"));
        let _ = AppendMenuW(menu, MF_STRING, M_SAVE, w!("保存截图"));
        let _ = AppendMenuW(menu, MF_STRING, M_SAVE_AS, w!("另存为…"));
        let _ = AppendMenuW(menu, MF_STRING, M_CLOSE, w!("关闭"));
        let mut pt = POINT { x: 0, y: 0 };
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

// ---------------- saving (unchanged from M2) ----------------

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

fn save_failed(st: &mut State, e: &anyhow::Error) {
    eprintln!("save failed: {e:#}");
    show_toast(st, format!("保存失败：{e:#}"));
}

fn do_quick_save(st: &mut State) {
    match (|| -> Result<String> {
        let dir = screenshot_dir(st.save_dir.as_deref())?;
        std::fs::create_dir_all(&dir)?;
        let path = unique_path(dir.join(default_filename(&st.title)));
        let shot = rebuild_shot(st);
        shot.save(&path)?;
        Ok(path.to_string_lossy().to_string())
    })() {
        Ok(p) => show_toast(st, format!("已保存：{p}")),
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

fn do_save_as(st: &mut State) {
    match save_as_dialog(st) {
        Ok(Some(p)) => show_toast(st, format!("已保存：{}", p.to_string_lossy())),
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
    use crate::ocr::CharHit;

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

    fn toy_state() -> State {
        State {
            bgra: Vec::new(), w: 0, h: 0, title: String::new(),
            search_url: String::new(), translate_url: String::new(), save_dir: None,
            hwnd: NULL_HWND, vw: 0, vh: 0, rx: std::ptr::null(),
            ready: false, load_error: None, lines: Vec::new(), tier: String::new(),
            spinner: 0, sel: None, press_at: None, anchor: None, cursor: None,
            dragging: false, toast: None, toast_level: 0xFF, tmode: TMode::Idle, want_full_label: false,
            translated: None,
            btn_save: (0, 0, 0, 0), btn_tr: (0, 0, 0, 0), ox: 0, oy: 0, mouse: (0, 0),
            back: None,
            fonts: HashMap::new(),
        }
    }

    fn toy_line(text: &str) -> OcrLine {
        let n = text.chars().count().max(1) as f32;
        OcrLine {
            quad: crate::geometry::Quad::from_xyxy(0.0, 0.0, 100.0, 20.0),
            text: text.to_string(),
            score: 1.0,
            chars: text
                .chars()
                .enumerate()
                .map(|(i, c)| CharHit {
                    text: c.to_string(),
                    f0: i as f32 / n,
                    f1: (i + 1) as f32 / n,
                    score: 1.0,
                })
                .collect(),
        }
    }

    #[test]
    fn range_text_joins_across_lines() {
        let mut st = toy_state();
        st.lines = vec![toy_line("abcd"), toy_line("截屏")];
        assert_eq!(range_text(&st, ((0, 1), (0, 3))), "bc");
        assert_eq!(range_text(&st, ((0, 2), (1, 2))), "cd\n截屏");
        assert_eq!(range_text(&st, ((1, 0), (1, 1))), "截");
    }
}
