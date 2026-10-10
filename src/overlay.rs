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
            CreateEllipticRgn, CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteDC, DeleteObject,
            DrawTextW, EndPaint, FillRect, FillRgn, GetDC, GetStockObject, SelectClipRgn, SelectObject,
            SetBkMode, SetTextColor, StretchDIBits, AC_SRC_OVER, BACKGROUND_MODE, BITMAPINFO, BITMAPINFOHEADER,
            BI_RGB, BLENDFUNCTION, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
            DEFAULT_QUALITY,             DIB_RGB_COLORS, DT_CENTER, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_TOP, DT_VCENTER,
            DT_WORDBREAK,
            FW_NORMAL, HBITMAP, HDC, HFONT, HGDIOBJ, HOLLOW_BRUSH, InvalidateRect, OUT_DEFAULT_PRECIS,
            PAINTSTRUCT, PS_DOT, PS_SOLID, Rectangle,
            SRCCOPY, TRANSPARENT,
        },
        System::SystemInformation::GetLocalTime,
        UI::{
            Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_ESCAPE},
            Shell::{
                FOLDERID_Documents, SHGetKnownFolderPath, ShellExecuteW, KF_FLAG_DEFAULT,
            },
            Controls::{
                Dialogs::{
                    CommDlgExtendedError, GetSaveFileNameW, OPENFILENAMEW, OFN_OVERWRITEPROMPT,
                    OFN_PATHMUSTEXIST,
                },
                InitCommonControlsEx, INITCOMMONCONTROLSEX, ICC_STANDARD_CLASSES,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, DispatchMessageW, GetCursorPos, GetForegroundWindow, GetMessageW,
                GetSystemMetrics, GetWindowLongPtrW, GetWindowTextW, LoadCursorW,
                PostQuitMessage, RegisterClassW, SendMessageW, SetForegroundWindow,
                SetWindowLongPtrW, SetWindowTextW, TrackPopupMenu, TranslateMessage,
                CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, CBS_DROPDOWNLIST, CBN_SELCHANGE,
                CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, IDYES,
                MB_ICONQUESTION, MB_YESNO, MENU_ITEM_FLAGS, MF_GRAYED, MF_STRING, MSG, MessageBoxW, SM_CXVIRTUALSCREEN,
                SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOWNORMAL,
                TPM_RETURNCMD, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY,
                WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT,
                WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_CHILD, WS_EX_TOOLWINDOW,
                WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, WS_VSCROLL, SetTimer,
            },
        },
    },
};

use crate::{
    blocks::cluster_blocks,
    clipboard,
    lang,
    monitors::{enum_monitors, focus_monitor, Mon},
    ocr::OcrLine,
    translate::{self, TranslateWorker},
    worker::OcrReply,
};

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
    pub translate_source: String,
    pub translate_target: String,
    pub save_dir: Option<String>,
}

/// Source language selection: Auto(detect) or a forced code.
#[derive(Debug, Clone)]
enum SrcSel {
    Auto,
    Lang(String),
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
struct TransRegion {
    rect: (i32, i32, i32, i32),
    font_px: u32,
    text: String,
}

#[derive(Debug, Clone)]
struct Translated {
    regions: Vec<TransRegion>, // translated text per region; empty = untranslated
}

/// Awaiting translation: regions (rect + joined text) + mode + pair.
/// Partial = one region over the selection bounds; Full = one per macro-block.
struct RegionJob {
    rect: (i32, i32, i32, i32),
    font_px: u32,
    text: String,
}

struct PendingT {
    regions: Vec<RegionJob>,
    mode: TMode,
    chain: Vec<String>,
}

/// In-flight translation-model download (progress UI + cancel live here).
struct DlState {
    label: String,
    prog: std::sync::Arc<std::sync::Mutex<(usize, usize, u64, Option<u64>)>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    done_rx: std::sync::mpsc::Receiver<Result<(), String>>,
    cancel_rect: (i32, i32, i32, i32),
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
            Ok(Self { hdc, hbmp, old })
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
    // Phase: Bergamot engine wiring.
    #[allow(dead_code)]
    translate_url: String,
    save_dir: Option<String>,
    hwnd: HWND,
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
    translating: bool,
    pending: Option<PendingT>, // waiting on the translate worker
    btn_save: (i32, i32, i32, i32),
    btn_tr: (i32, i32, i32, i32),
    combo_src_r: (i32, i32, i32, i32),
    combo_tgt_r: (i32, i32, i32, i32),
    // Language dropdowns (escape hatch over auto-detect).
    combo_src: HWND,
    combo_tgt: HWND,
    src_items: Vec<String>, // index 0 = Auto
    tgt_items: Vec<String>,
    src_sel: SrcSel,
    tgt: String,
    src_cfg: String,
    tgt_cfg: String,
    tworker: Option<TranslateWorker>,
    dl: Option<DlState>,
    ox: i32,
    oy: i32,
    mouse: (i32, i32),
    mons: Vec<Mon>,
    focus: Mon,
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
        // Per-monitor geometry (spinner on every screen, buttons on the focus screen).
        let mons = enum_monitors(vx, vy, vw, vh);
        let mut cursor_pt = POINT { x: vx + vw / 2, y: vy + vh / 2 };
        let mut cpt = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut cpt).is_ok() {
            cursor_pt = cpt;
        }
        let focus = focus_monitor(&mons, (cursor_pt.x, cursor_pt.y));

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
            translating: false,
            pending: None,
            btn_save: (0, 0, 0, 0),
            btn_tr: (0, 0, 0, 0),
            combo_src_r: (0, 0, 0, 0),
            combo_tgt_r: (0, 0, 0, 0),
            combo_src: NULL_HWND,
            combo_tgt: NULL_HWND,
            src_items: vec!["Auto".to_string()],
            tgt_items: Vec::new(),
            src_sel: SrcSel::Auto,
            tgt: lang::resolve_target(Some(&req.translate_target)),
            src_cfg: req.translate_source.clone(),
            tgt_cfg: req.translate_target.clone(),
            tworker: None,
            dl: None,
            ox: vx,
            oy: vy,
            mouse: (0, 0),
            mons,
            focus,
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
        // Language dropdowns (escape hatch over auto-detect).
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);
        let langs = pair_langs();
        state.src_items = std::iter::once("Auto".to_string()).chain(langs.clone()).collect();
        state.tgt_items = langs;
        state.combo_src = create_combo(hwnd, state.combo_src_r)?;
        state.combo_tgt = create_combo(hwnd, state.combo_tgt_r)?;
        combo_add(state.combo_src, &state.src_items);
        combo_add(state.combo_tgt, &state.tgt_items);
        combo_select(state.combo_src, 0);
        combo_select(
            state.combo_tgt,
            state.tgt_items.iter().position(|l| l == &state.tgt).unwrap_or(0),
        );
        std::mem::forget(state);
        // Explicit show + topmost (belt and suspenders for exotic shells).
        {
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
    const CBW: i32 = 110;
    const BH: i32 = 44;
    const GAP: i32 = 12;
    // Row on the focus (cursor) monitor: [save][src][tgt][translate].
    let total = BW + CBW * 2 + BW + GAP * 3;
    let x0 = st.focus.cx() - total / 2;
    let mut y = st.focus.t + 24;
    let mut rects = [
        (x0, y, x0 + BW, y + BH),
        (x0 + BW + GAP, y, x0 + BW + GAP + CBW, y + BH),
        (x0 + BW + GAP + CBW + GAP, y, x0 + BW + GAP * 2 + CBW * 2, y + BH),
        (
            x0 + BW + GAP * 2 + CBW * 2 + GAP,
            y,
            x0 + total,
            y + BH,
        ),
    ];
    // Auto-avoid: if any OCR box overlaps the row, move it near the bottom
    // of the same monitor.
    'outer: for l in &st.lines {
        let (x0b, y0b, x1b, y1b) = l.quad.axis_aligned_bounds();
        let (x0b, y0b, x1b, y1b) = (x0b as i32, y0b as i32, x1b as i32, y1b as i32);
        for r in &rects {
            if x0b < r.2 + 8 && x1b > r.0 - 8 && y0b < r.3 + 8 && y1b > r.1 - 8 {
                y = st.focus.b - 140;
                for r in rects.iter_mut() {
                    let h = r.3 - r.1;
                    r.1 = y;
                    r.3 = y + h;
                }
                break 'outer;
            }
        }
    }
    st.btn_save = rects[0];
    st.combo_src_r = rects[1];
    st.combo_tgt_r = rects[2];
    st.btn_tr = rects[3];
    place_combos(st);
}

/// Two-letter codes appearing in any known registry pair, sorted.
/// Baked at build time from the full pair snapshot (offline-safe), so third
/// languages show up in the dropdowns even before any model is downloaded.
fn pair_langs() -> Vec<String> {
    translate::TRANSLATE_LANGS.iter().map(|s| s.to_string()).collect()
}

fn combo_add(hwnd: HWND, items: &[String]) {
    unsafe {
        for it in items {
            let w: Vec<u16> = it.encode_utf16().chain([0]).collect();
            SendMessageW(
                hwnd,
                CB_ADDSTRING,
                None,
                Some(LPARAM(w.as_ptr() as isize)),
            );
        }
    }
}

fn combo_select(hwnd: HWND, idx: usize) {
    unsafe {
        SendMessageW(hwnd, CB_SETCURSEL, Some(WPARAM(idx)), None);
    }
}

fn combo_get(hwnd: HWND) -> usize {
    unsafe {
        let r = SendMessageW(hwnd, CB_GETCURSEL, None, None);
        if r.0 < 0 {
            0
        } else {
            r.0 as usize
        }
    }
}

fn create_combo(hwnd: HWND, r: (i32, i32, i32, i32)) -> Result<HWND> {
    unsafe {
        let h = CreateWindowExW(
            Default::default(),
            w!("COMBOBOX"),
            w!(""),
            WS_CHILD
                | WS_VISIBLE
                | WS_VSCROLL
                | windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                    CBS_DROPDOWNLIST as u32,
                ),
            r.0,
            r.1,
            r.2 - r.0,
            200, // dropdown list height; the edit box autosizes
            Some(hwnd),
            None,
            None,
            None,
        )?;
        Ok(h)
    }
}

fn place_combos(st: &State) {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW};
        for (hwnd, r) in [(st.combo_src, st.combo_src_r), (st.combo_tgt, st.combo_tgt_r)] {
            if hwnd.0.is_null() {
                continue;
            }
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                r.0,
                r.1,
                r.2 - r.0,
                200, // dropdown list height; edit box autosizes
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
            );
        }
    }
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
            if st.dl.as_ref().map(|d| in_rect(p, d.cancel_rect)).unwrap_or(false) {
                if let Some(d) = st.dl.as_ref() {
                    d.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    show_toast(st, "取消中…".to_string());
                }
            } else if in_rect(p, st.btn_save) {
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
            autoselect_at(st, p);
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
                autoselect_at(st, p);
                popup_menu(st);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            // No child BUTTON controls exist anymore (owner-drawn); with
            // TPM_RETURNCMD the popup menu returns directly (see popup_menu).
            // The only WM_COMMAND traffic is CBN_SELCHANGE from the dropdowns.
            if lp.0 != 0 {
                let code = ((wp.0 >> 16) & 0xffff) as u32;
                if code == CBN_SELCHANGE {
                    let hw = HWND(lp.0 as _);
                    if hw == st.combo_src {
                        let i = combo_get(hw);
                        st.src_sel = match st.src_items.get(i).map(|s| s.as_str()) {
                            Some("Auto") | None => SrcSel::Auto,
                            Some(l) => SrcSel::Lang(l.to_string()),
                        };
                    } else if hw == st.combo_tgt {
                        let i = combo_get(hw);
                        if let Some(l) = st.tgt_items.get(i) {
                            st.tgt = l.clone();
                        }
                    }
                }
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
            let level =
                (if remain > 1000 { 8 } else { (remain * 8 / 1000) as u8 + 1 }).min(8);
            if level != st.toast_level {
                st.toast_level = level;
                dirty = true;
            }
        }
    }
    // Download progress + completion.
    if st.dl.is_some() {
        dirty = true; // progress bar + % advance every tick while downloading
        let done = st.dl.as_ref().unwrap().done_rx.try_recv().ok();
        if let Some(r) = done {
            let dl = st.dl.take().unwrap();
            match r {
                Ok(()) => {
                    show_toast(st, format!("模型 {} 下载完成，翻译中…", dl.label));
                    if let Some(pending) = st.pending.take() {
                        submit_translate(st, pending);
                    }
                }
                Err(e) => {
                    st.pending = None;
                    if e.contains("cancelled") {
                        show_toast(st, "已取消下载".to_string());
                    } else {
                        show_toast(st, format!("下载失败：{e}"));
                    }
                }
            }
            dirty = true;
        }
    }
    // Translate worker replies.
    if st.translating {
        if let Some(w) = st.tworker.as_mut() {
            // Drain latest only; stale replies can't exist (one job at a time).
            let mut last = None;
            while let Ok(rep) = w.rx.try_recv() {
                last = Some(rep);
            }
            if let Some(rep) = last {
                st.translating = false;
                if let Some(e) = rep.error {
                    st.pending = None;
                    show_toast(st, format!("翻译失败，已转外部：{e}"));
                    open_url(&fill_url(&st.translate_url, &selected_or_all(st)));
                } else if let Some(pending) = st.pending.take() {
                    // Worker Vec order matches the submitted regions (R4).
                    let regions = pending
                        .regions
                        .iter()
                        .zip(rep.texts.iter())
                        .map(|(r, t)| TransRegion {
                            rect: r.rect,
                            font_px: r.font_px,
                            text: t.clone(),
                        })
                        .collect();
                    let mode = pending.mode;
                    st.translated = Some(Translated { regions });
                    st.tmode = match mode {
                        TMode::Partial => TMode::Partial,
                        _ => TMode::Full,
                    };
                    st.want_full_label = false;
                    set_title(st, "Selectable — 翻译完成");
                }
                dirty = true;
            } else {
                dirty = true; // keep title spinner-ish feedback fresh
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

// ---------------- translation (real engine) ----------------

fn translate_models_dir() -> std::path::PathBuf {
    crate::ocr::models_root().join("translate")
}

/// Resolve (src, tgt, plan) for `text`; Err is user-facing toast text.
/// Auto source excludes the target so mixed text picks the other side (R3).
/// Same -> "无需翻译"; no direct leg and no via-en pivot -> external fallback.
fn resolve_plan(st: &State, text: &str) -> Result<(String, String, Vec<String>), String> {
    let tgt = if st.tgt.trim().is_empty() {
        lang::resolve_target(Some(&st.tgt_cfg))
    } else {
        st.tgt.clone()
    };
    let src = match &st.src_sel {
        SrcSel::Lang(l) => l.clone(),
        SrcSel::Auto => {
            if st.src_cfg.trim().is_empty() || st.src_cfg.eq_ignore_ascii_case("auto") {
                lang::detect_script_except(text, &tgt).to_string()
            } else {
                st.src_cfg.to_lowercase()
            }
        }
    };
    match translate::plan_for(&src, &tgt) {
        translate::Plan::Legs(legs) => Ok((src, tgt, legs)),
        translate::Plan::Same => Err("无需翻译".to_string()),
        translate::Plan::Unsupported => Err("UNSUPPORTED".to_string()),
    }
}

/// Region payload for a scope (R4): Partial = selection bounds as one region,
/// Full = one region per macro-block (clustered + newline-restored).
fn build_regions(st: &State, mode: TMode) -> Vec<RegionJob> {
    match mode {
        TMode::Partial => {
            let Some(r) = st.sel else { return vec![] };
            let text = range_text(st, r);
            if text.trim().is_empty() || st.lines.is_empty() {
                return vec![];
            }
            let n = st.lines.len();
            let ((l0, _), (l1, _)) = r;
            let (a, b) = (l0.min(n - 1), l1.min(n - 1));
            let (mut x0, mut y0, mut x1, mut y1) =
                (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
            let mut hmax = 0u32;
            for li in a..=b {
                let (lx0, ly0, lx1, ly1) = st.lines[li].quad.axis_aligned_bounds();
                x0 = x0.min(lx0 as i32);
                y0 = y0.min(ly0 as i32);
                x1 = x1.max(lx1 as i32);
                y1 = y1.max(ly1 as i32);
                hmax = hmax.max(ly1.saturating_sub(ly0));
            }
            vec![RegionJob {
                rect: (x0, y0, x1, y1),
                font_px: hmax.saturating_sub(4).max(12),
                text,
            }]
        }
        _ => cluster_blocks(&st.lines)
            .into_iter()
            .filter(|b| !b.text.trim().is_empty())
            .map(|b| RegionJob { rect: b.rect, font_px: b.font_px, text: b.text })
            .collect(),
    }
}

/// Entry: translate scope in mode. Downloads models first when missing —
/// no second press needed afterwards.
fn start_translate(st: &mut State, mode: TMode) {
    if !st.ready || st.translating || st.dl.is_some() {
        return;
    }
    let regions = build_regions(st, mode);
    if regions.is_empty() {
        show_toast(st, "先选中要翻译的文字".to_string());
        return;
    }
    // Probe the longest region (R4): a single first box misjudges mixed screens.
    let probe = regions.iter().max_by_key(|r| r.text.len()).map(|r| r.text.clone()).unwrap_or_default();
    let (src, tgt, legs) = match resolve_plan(st, &probe) {
        Ok(v) => v,
        Err(e) if e == "UNSUPPORTED" => {
            // No direct leg and no via-en pivot: external translator fallback.
            show_toast(st, "暂不支持该语种组合，已转外部翻译".to_string());
            open_url(&fill_url(&st.translate_url, &probe));
            return;
        }
        Err(e) => {
            show_toast(st, e);
            return;
        }
    };
    let label = legs.join("+");
    let pending = PendingT { regions, mode, chain: legs.clone() };
    if !translate::chain_complete(&translate_models_dir(), &legs) {
        // R5: third-language downloads ask first (legs + total MB shown);
        // en/locale directions download directly.
        if needs_download_confirm(&src) && !confirm_download(st.hwnd, &legs, &src, &tgt) {
            show_toast(st, "已取消下载".to_string());
            return;
        }
        st.pending = Some(pending);
        start_download(st, legs, label);
        return;
    }
    submit_translate(st, pending);
}

/// R5: confirm when the source is neither English nor the system locale.
fn needs_download_confirm(src: &str) -> bool {
    !(src.eq_ignore_ascii_case("en") || src.eq_ignore_ascii_case(&lang::system_lang()))
}

/// Modal YESNO on the overlay: one line per leg + total MB (a pivot confirms
/// both segments at once, then translates twice). True = proceed to download.
fn confirm_download(hwnd: HWND, legs: &[String], src: &str, tgt: &str) -> bool {
    let total = translate::chain_total_bytes(legs);
    let mb = total as f64 / 1048576.0;
    let body = if legs.len() == 1 {
        format!("需要下载 {src}→{tgt} 翻译模型（约 {mb:.0} MB），现在下载吗？")
    } else {
        // Pivot: spell out both segments (src->en->tgt) plus the summed size.
        let mid = "en";
        format!(
            "需要下载 {src}→{mid} + {mid}→{tgt} 两段翻译模型（共约 {mb:.0} MB），现在下载吗？"
        )
    };
    let msg: Vec<u16> = format!("{body}\0").encode_utf16().collect();
    unsafe {
        MessageBoxW(Some(hwnd), PCWSTR(msg.as_ptr()), w!("Selectable"), MB_YESNO | MB_ICONQUESTION) == IDYES
    }
}

fn submit_translate(st: &mut State, pending: PendingT) {
    let models_dir = translate_models_dir();
    let worker = st.tworker.get_or_insert_with(|| TranslateWorker::spawn(models_dir));
    worker.submit(pending.chain.clone(), pending.regions.iter().map(|r| r.text.clone()).collect());
    st.pending = Some(pending);
    st.translating = true;
    set_title(st, "Selectable — 翻译中…");
}

fn start_download(st: &mut State, legs: Vec<String>, label: String) {
    use std::sync::{atomic::AtomicBool, Arc, Mutex};
    let n_files: usize = legs
        .iter()
        .filter_map(|p| translate::pair_files(p))
        .map(|f| f.len())
        .sum();
    let prog = Arc::new(Mutex::new((0usize, n_files.max(1), 0u64, None::<u64>)));
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let models_dir = translate_models_dir();
    let legs2 = legs.clone();
    let (prog2, cancel2) = (prog.clone(), cancel.clone());
    std::thread::Builder::new()
        .name("translate-dl".to_string())
        .spawn(move || {
            let r = translate::download_chain_blocking(&models_dir, &legs2, &prog2, &cancel2)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(r);
        })
        .expect("spawn download thread");
    // Cancel button sits under the progress panel (rect refined each paint).
    let f = &st.focus;
    let cx = (f.l + f.r) / 2;
    let cy = (f.t + f.b) / 2;
    st.dl = Some(DlState {
        label,
        prog,
        cancel,
        done_rx: rx,
        cancel_rect: (cx - 65, cy + 44, cx + 65, cy + 80),
    });
    invalidate(st);
}

/// R1: right-click with no (or empty) selection first selects the whole box
/// under the cursor, so the menu always acts on something when possible.
fn autoselect_at(st: &mut State, p: (i32, i32)) {
    let has_text = st.sel.map(|r| !range_text(st, r).trim().is_empty()).unwrap_or(false);
    if has_text {
        return;
    }
    if let Some(li) = line_at_point(st, p) {
        st.sel = Some(((li, 0), (li, 0)));
        invalidate(st);
    }
}

/// Context-menu "翻译选中": partial overlay of the current selection.
fn translate_menu(st: &mut State) {
    if selected_text(st).is_empty() {
        show_toast(st, "先选中要翻译的文字".to_string());
        return;
    }
    start_translate(st, TMode::Partial);
}

fn translate_full(st: &mut State) {
    start_translate(st, TMode::Full);
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

/// Toast fade alpha for a quantized level 0..=8: 220 opaque at full, 0 gone.
/// u16 math is load-bearing: the old `level * 220 / 8` in u8 overflowed past
/// level 1 (7*220=1540) — debug builds abort, release builds wrap to garbage.
/// Regression test below pins the table.
fn toast_alpha(level: u8) -> u8 {
    let a = level.min(8);
    if a >= 8 {
        220
    } else {
        (a as u16 * 220 / 8) as u8
    }
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
            // Selection: dark-blue strips + white chars.
            paint_selection(hdc, st);
            // Translated subtitles strictly above the selection (R2): they
            // cover the blue/white strips with their own dark panels.
            if let Some(tr) = st.translated.clone() {
                paint_translated(hdc, st, &tr);
            }
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
            Some(t) => Some((t.text.clone(), toast_alpha(st.toast_level))),
            None => None,
        };
        if let Some((text, alpha)) = toast_data {
            show_toast_paint(hdc, st, &text, alpha);
        }

        // Download progress panel above all else while active.
        if st.dl.is_some() {
            paint_download(hdc, st);
        }

        let _ = BitBlt(front, 0, 0, st.w as i32, st.h as i32, Some(hdc), 0, 0, SRCCOPY);
        let _ = EndPaint(st.hwnd, &ps);
    }
}

fn paint_spinner(hdc: windows::Win32::Graphics::Gdi::HDC, st: &mut State) {
    // One spinner per monitor so dual-screen users never stare at a bezel.
    let centers: Vec<(i32, i32)> = st.mons.iter().map(|m| (m.cx(), m.cy())).collect();
    for (cx, cy) in centers {
        draw_spinner_at(hdc, st, cx, cy);
    }
}

fn draw_spinner_at(hdc: windows::Win32::Graphics::Gdi::HDC, st: &mut State, cx: i32, cy: i32) {
    unsafe {
        // NOTE: brush-via-SelectObject fills demonstrably don't stick on this
        // toolchain (bisected: RoundRect+selected-brush paints border-only,
        // while FillRgn/FillRect with brush-as-parameter works). So every fill
        // here goes through FillRgn/FillRect; SelectObject is only used for
        // pens and fonts (both proven working).
        // Solid dark panel so the spinner reads on any screenshot.
        let panel_rgn = CreateRoundRectRgn(cx - 130, cy - 85, cx + 130, cy + 85, 24, 24);
        let panel = CreateSolidBrush(COLORREF(0x141414));
        let _ = FillRgn(hdc, panel_rgn, panel);
        let _ = DeleteObject(panel.into());
        let _ = DeleteObject(panel_rgn.into());
        // 12 orbiting dots, head bright white fading to gray. Dots alone form
        // the ring; each disc is region-filled (see note above).
        for i in 0..12u32 {
            let k = (i + 12 - st.spinner % 12) % 12;
            let v = (255 - k * 14) as u8;
            let dot_brush =
                CreateSolidBrush(COLORREF((v as u32) | ((v as u32) << 8) | ((v as u32) << 16)));
            let a = (st.spinner as f32 * 30.0 + i as f32 * 30.0).to_radians();
            let (x, y) = (
                (cx as f32 + a.cos() * 44.0) as i32,
                ((cy - 8) as f32 + a.sin() * 44.0) as i32,
            );
            let dot_rgn = CreateEllipticRgn(x - 7, y - 7, x + 7, y + 7);
            let _ = FillRgn(hdc, dot_rgn, dot_brush);
            let _ = DeleteObject(dot_rgn.into());
            let _ = DeleteObject(dot_brush.into());
        }
        // Label under the ring.
        let font = font_for(st, 22);
        let old_font = SelectObject(hdc, font.into());
        SetTextColor(hdc, COLORREF(0xFFFFFF));
        SetBkMode(hdc, TRANSPARENT);
        let mut txt: Vec<u16> = "识别中…".encode_utf16().collect();
        let mut rc = RECT { left: cx - 130, top: cy + 44, right: cx + 130, bottom: cy + 80 };
        DrawTextW(hdc, &mut txt, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        SelectObject(hdc, old_font);
    }
}

/// Download panel geometry (single source for paint + hit test).
fn dl_layout(focus: &Mon) -> ((i32, i32, i32, i32), (i32, i32, i32, i32), (i32, i32, i32, i32)) {
    let cx = (focus.l + focus.r) / 2;
    let cy = (focus.t + focus.b) / 2;
    let panel = (cx - 170, cy - 70, cx + 170, cy + 90);
    let bar = (cx - 140, cy - 6, cx + 140, cy + 16);
    let cancel = (cx - 65, cy + 44, cx + 65, cy + 80);
    (panel, bar, cancel)
}

fn paint_download(hdc: windows::Win32::Graphics::Gdi::HDC, st: &mut State) {
    // Snapshot first: `font_for` needs `&mut st`, so no borrow of `st.dl`
    // may live past this line.
    let Some((label, prog)) = st.dl.as_ref().map(|d| (d.label.clone(), d.prog.clone())) else { return };
    let (panel, bar, cancel) = dl_layout(&st.focus);
    unsafe {
        // Panel.
        let rgn = CreateRoundRectRgn(panel.0, panel.1, panel.2, panel.3, 24, 24);
        let bg = CreateSolidBrush(COLORREF(0x141414));
        let _ = FillRgn(hdc, rgn, bg);
        let _ = DeleteObject(bg.into());
        let _ = DeleteObject(rgn.into());
        // Title.
        let font = font_for(st, 20);
        let old_font = SelectObject(hdc, font.into());
        SetTextColor(hdc, COLORREF(0xFFFFFF));
        SetBkMode(hdc, TRANSPARENT);
        let mut title: Vec<u16> =
            format!("下载翻译模型 {label}").encode_utf16().collect();
        let mut trc = RECT { left: panel.0, top: panel.1 + 10, right: panel.2, bottom: panel.1 + 36 };
        DrawTextW(hdc, &mut title, &mut trc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        // Bar background + fill.
        let (fi, n, done, total) = prog.lock().map(|p| *p).unwrap_or((0, 1, 0, None));
        let bg_brush = CreateSolidBrush(COLORREF(0x3A3A3A));
        let brc = RECT { left: bar.0, top: bar.1, right: bar.2, bottom: bar.3 };
        FillRect(hdc, &brc, bg_brush);
        let _ = DeleteObject(bg_brush.into());
        let frac = if n == 0 {
            0.0
        } else {
            let cur = total.map(|t| done as f64 / t.max(1) as f64).unwrap_or(0.0).clamp(0.0, 1.0);
            ((fi as f64 + cur) / n as f64).clamp(0.0, 1.0)
        };
        let fill_w = ((bar.2 - bar.0) as f64 * frac) as i32;
        if fill_w > 0 {
            let fill = CreateSolidBrush(COLORREF(0x2E7AD6));
            let frc = RECT { left: bar.0, top: bar.1, right: bar.0 + fill_w, bottom: bar.3 };
            FillRect(hdc, &frc, fill);
            let _ = DeleteObject(fill.into());
        }
        // Percent / bytes line.
        let pct = format!(
            "{}/{} · {}%",
            fi.min(n).saturating_add(1).min(n.max(1)),
            n.max(1),
            (frac * 100.0) as u32
        );
        let mut txt: Vec<u16> = pct.encode_utf16().collect();
        let mut prc = RECT { left: panel.0, top: bar.3 + 4, right: panel.2, bottom: cancel.1 - 4 };
        DrawTextW(hdc, &mut txt, &mut prc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        SelectObject(hdc, old_font);
    }
    // Cancel button (same owner-drawn style as the global buttons).
    paint_button(hdc, st, cancel, "取消");
    // Remember for hit-testing (layout is stable, but cheap to refresh).
    if let Some(dl) = st.dl.as_mut() {
        dl.cancel_rect = cancel;
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
    // Fixed-height bar, width by chars, bottom-left of the focus monitor.
    let w = (text.chars().count() as i32 * 20 + 48)
        .min((st.focus.r - st.focus.l) / 2)
        .max(200);
    let h = 52;
    let r = (st.focus.l + 24, st.focus.b - 40 - h, st.focus.l + 24 + w, st.focus.b - 40);
    let font = font_for(st, 20);
    unsafe {
        let old = SelectObject(hdc, font.into());
        blit_bar(hdc, r, COLORREF(0x141414), text, 20, &mut st.fonts, alpha);
        SelectObject(hdc, old);
    }
}

/// Region subtitles (R2/R4): one dark panel per region over whatever is
/// beneath (selection included), word-wrapped. Empty texts paint nothing.
fn paint_translated(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    st: &mut State,
    tr: &Translated,
) {
    unsafe {
        // Fill via brush-as-parameter (SelectObject fills are unreliable here).
        let brush = CreateSolidBrush(COLORREF(0x1E3C1E));
        let old_bk = SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0xFFFFFF));
        for r in &tr.regions {
            if r.text.trim().is_empty() {
                continue;
            }
            let (x0, y0, x1, y1) = r.rect;
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let frc = RECT { left: x0, top: y0, right: x1, bottom: y1 };
            FillRect(hdc, &frc, brush);
            let font = font_for(st, r.font_px);
            let old_font = SelectObject(hdc, font.into());
            let mut txt: Vec<u16> = r.text.encode_utf16().collect();
            let mut rc = RECT { left: x0 + 2, top: y0, right: x1, bottom: y1 };
            DrawTextW(hdc, &mut txt, &mut rc, DT_LEFT | DT_TOP | DT_WORDBREAK | DT_NOPREFIX);
            SelectObject(hdc, old_font);
        }
        SetBkMode(hdc, BACKGROUND_MODE(old_bk as u32));
        let _ = DeleteObject(brush.into());
    }
}

fn paint_selection(hdc: windows::Win32::Graphics::Gdi::HDC, st: &mut State) {
    let Some((a, b)) = st.sel else { return };
    unsafe {
        // FillRect with brush-as-parameter (see note in draw_spinner_at).
        let brush = CreateSolidBrush(COLORREF(0xAD4600)); // dark blue (0x00BBGGRR)
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
                let xs = [pts[0][0] as i32, pts[1][0] as i32, pts[2][0] as i32, pts[3][0] as i32];
                let ys = [pts[0][1] as i32, pts[1][1] as i32, pts[2][1] as i32, pts[3][1] as i32];
                let frc = RECT {
                    left: *xs.iter().min().unwrap(),
                    top: *ys.iter().min().unwrap(),
                    right: *xs.iter().max().unwrap(),
                    bottom: *ys.iter().max().unwrap(),
                };
                FillRect(hdc, &frc, brush);
                let font = font_for(st, h.saturating_sub(2).max(12));
                let old_font = SelectObject(hdc, font.into());
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
        let _ = DeleteObject(brush.into());
    }
}

fn popup_menu(st: &mut State) {
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
        // NOTE: with TPM_RETURNCMD the choice comes back as the return value;
        // no WM_COMMAND is generated. Ignoring it (as before) silently drops
        // every menu action.
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD, pt.x, pt.y, None, st.hwnd, None);
        let _ = DestroyMenu(menu);
        dlog(format!("menu cmd={} at ({},{})", cmd.0, pt.x, pt.y));
        match cmd.0 as usize {
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
                let _ = DestroyWindow(st.hwnd);
            }
            _ => {}
        }
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
        // TODO(#5): offer JPEG next to PNG (filter + save branch + quality).
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
        if GetSaveFileNameW(&mut ofn).as_bool() {            let len = file_buf.iter().position(|&c| c == 0).unwrap_or(0);
            let mut path = std::path::PathBuf::from(String::from_utf16_lossy(&file_buf[..len]));
            if path.extension().is_none() {
                path.set_extension("png");
            }
            let shot = rebuild_shot(st);
            shot.save(&path)?;
            Ok(Some(path))
        } else {
            // FALSE means cancel OR error — tell them apart (issue #4).
            let code = CommDlgExtendedError().0;
            dlog(format!("save-as dialog result: {code:#X}"));
            if code == 0 {
                Ok(None)
            } else {
                Err(anyhow::anyhow!("另存为对话框失败 (0x{code:08X})"))
            }
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
            hwnd: NULL_HWND, rx: std::ptr::null(),
            ready: false, load_error: None, lines: Vec::new(), tier: String::new(),
            spinner: 0, sel: None, press_at: None, anchor: None, cursor: None,
            dragging: false, toast: None, toast_level: 0xFF, tmode: TMode::Idle, want_full_label: false,
            translated: None, translating: false, pending: None,
            combo_src: NULL_HWND, combo_tgt: NULL_HWND,
            src_items: vec!["Auto".to_string()], tgt_items: Vec::new(),
            src_sel: SrcSel::Auto, tgt: String::new(),
            src_cfg: String::new(), tgt_cfg: String::new(),
            tworker: None, dl: None,
            btn_save: (0, 0, 0, 0), btn_tr: (0, 0, 0, 0),
            combo_src_r: (0, 0, 0, 0), combo_tgt_r: (0, 0, 0, 0),
            ox: 0, oy: 0, mouse: (0, 0),
            mons: vec![crate::monitors::Mon { l: 0, t: 0, r: 1920, b: 1080, primary: true }],
            focus: crate::monitors::Mon { l: 0, t: 0, r: 1920, b: 1080, primary: true },
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
    #[test]
    fn dropdown_derives_from_full_registry_table() {

        // Baked TRANSLATE_LANGS (not just the two default pairs): third
        // languages must be selectable before any model is downloaded.
        let langs = pair_langs();
        assert!(langs.len() > 10, "got {} langs", langs.len());
        for want in ["en", "zh", "zh_hant", "ja", "fr", "de", "ko", "ru", "es", "it", "pt"] {
            assert!(langs.contains(&want.to_string()), "missing {want}");
        }
        let mut sorted = langs.clone();
        sorted.sort();
        assert_eq!(langs, sorted, "dropdown order must be sorted");
    }

    #[test]
    fn toast_alpha_table() {
        // Regression: u8 `level * 220` overflowed past level 1 (debug abort,
        // release wrap). Pinned values: level*220/8, 220 cap at 8+.
        assert_eq!(
            (0..=9u8).map(toast_alpha).collect::<Vec<_>>(),
            vec![0, 27, 55, 82, 110, 137, 165, 192, 220, 220]
        );
        assert_eq!(toast_alpha(0xFF), 220, "sentinel clamps, never panics");
        let mut prev = 0u8;
        for l in 0..=9u8 {
            let a = toast_alpha(l);
            assert!(a >= prev, "fade must be monotonic");
            assert!(a <= 220);
            prev = a;
        }
    }

    #[test]
    fn spinner_frame_is_visible() {        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, FillRect, GetDC,
            GetDIBits, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        };
        unsafe {
            // Offscreen canvas with a light background (worst case for white art).
            let screen = GetDC(Some(NULL_HWND));
            let mem = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, 400, 300);
            let old = SelectObject(mem, bmp.into());
            let white = CreateSolidBrush(COLORREF(0xFFFFFF));
            let rc = RECT { left: 0, top: 0, right: 400, bottom: 300 };
            FillRect(mem, &rc, white);
            let _ = DeleteObject(white.into());

            let mut st = toy_state();
            st.mons = vec![crate::monitors::Mon { l: 0, t: 0, r: 400, b: 300, primary: true }];
            draw_spinner_at(mem, &mut st, 200, 150);

            // Read back and check: dark panel pixels must exist, plus bright ones.
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = 400;
            bmi.bmiHeader.biHeight = -300;
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0 as u32;
            let mut buf = vec![0u8; 400 * 300 * 4];
            assert!(GetDIBits(mem, bmp, 0, 300, Some(buf.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS) > 0);
            let (mut dark, mut bright) = (0, 0);
            for px in buf.chunks_exact(4) {
                let lum = px[0] as u32 + px[1] as u32 + px[2] as u32;
                if lum < 120 {
                    dark += 1;
                }
                if lum > 700 {
                    bright += 1;
                }
            }
            eprintln!("spinner pixels: dark={dark} bright={bright}");
            // Persist the frame for eyeballing (temp/ is gitignored).
            let mut img = image::RgbImage::new(400, 300);
            for (i, p) in img.pixels_mut().enumerate() {
                let o = i * 4;
                p.0 = [buf[o + 2], buf[o + 1], buf[o]];
            }
            let _ = img.save("temp/spinner.png");
            assert!(dark > 5000, "panel did not paint dark");
            assert!(bright > 200, "ring/dots/label missing");
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            let _ = windows::Win32::Graphics::Gdi::ReleaseDC(Some(NULL_HWND), screen);
        }
    }

    /// Bisect: which fill primitive actually darkens pixels on this toolchain?
    #[test]
    fn gdi_fill_bisect() {
        use windows::Win32::Graphics::Gdi::*;
        unsafe {
            let screen = GetDC(Some(NULL_HWND));
            for (name, draw) in [
                ("rr_solid", 0),
                ("rr_stockwhite", 1),
                ("fillrgn_round", 2),
                ("fillrect_dark", 3),
            ] {
                let mem = CreateCompatibleDC(Some(screen));
                let bmp = CreateCompatibleBitmap(screen, 200, 150);
                let old = SelectObject(mem, bmp.into());
                // white bg
                let white = CreateSolidBrush(COLORREF(0xFFFFFF));
                let rc = RECT { left: 0, top: 0, right: 200, bottom: 150 };
                FillRect(mem, &rc, white);
                let _ = DeleteObject(white.into());
                if draw == 0 {
                    // Variant A: created dark brush + hollow pen, like the spinner.
                    let b = CreateSolidBrush(COLORREF(0x141414));
                    let ob = SelectObject(mem, b.into());
                    let op = SelectObject(mem, GetStockObject(HOLLOW_BRUSH));
                    let r = RoundRect(mem, 20, 20, 180, 130, 16, 16);
                    eprintln!("{name}: roundrect_ok={} brush_null={} ob_null={} op_null={} brush={:?} hollow={:?}",
                        r.as_bool(), b.is_invalid(),
                        ob.0.is_null(), op.0.is_null(), b.0, GetStockObject(HOLLOW_BRUSH).0);
                    SelectObject(mem, op);
                    SelectObject(mem, ob);
                    let _ = DeleteObject(b.into());
                } else if draw == 1 {
                    // Variant B: stock white brush + stock black pen.
                    let ob = SelectObject(mem, GetStockObject(WHITE_BRUSH));
                    let op = SelectObject(mem, GetStockObject(BLACK_PEN));
                    let r = RoundRect(mem, 20, 20, 180, 130, 16, 16);
                    eprintln!("{name}: roundrect_ok={} ", r.as_bool());
                    SelectObject(mem, op);
                    SelectObject(mem, ob);
                } else if draw == 2 {
                    // Variant C: region fill, brush passed as parameter (no select).
                    let rgn = CreateRoundRectRgn(20, 20, 180, 130, 16, 16);
                    let b = CreateSolidBrush(COLORREF(0x141414));
                    let n = FillRgn(mem, rgn, b);
                    eprintln!("{name}: fillrgn_ok={} rgn_null={}", n.as_bool(), rgn.is_invalid());
                    let _ = DeleteObject(rgn.into());
                    let _ = DeleteObject(b.into());
                } else {
                    // Variant D: plain rect fill, brush as parameter.
                    let b = CreateSolidBrush(COLORREF(0x141414));
                    let rc2 = RECT { left: 20, top: 20, right: 180, bottom: 130 };
                    let n = FillRect(mem, &rc2, b);
                    eprintln!("{name}: fillrect_n={n}");
                    let _ = DeleteObject(b.into());
                }
                // read back dark count
                let mut bmi: BITMAPINFO = std::mem::zeroed();
                bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                bmi.bmiHeader.biWidth = 200;
                bmi.bmiHeader.biHeight = -150;
                bmi.bmiHeader.biPlanes = 1;
                bmi.bmiHeader.biBitCount = 32;
                bmi.bmiHeader.biCompression = BI_RGB.0 as u32;
                let mut buf = vec![0u8; 200 * 150 * 4];
                GetDIBits(mem, bmp, 0, 150, Some(buf.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS);
                let dark = buf.chunks_exact(4).filter(|px| (px[0] as u32 + px[1] as u32 + px[2] as u32) < 120).count();
                eprintln!("{name}: dark={dark}");
                SelectObject(mem, old);
                let _ = DeleteObject(bmp.into());
                let _ = DeleteDC(mem);
            }
            let _ = ReleaseDC(Some(NULL_HWND), screen);
        }
    }
}
