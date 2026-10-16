// Fullscreen overlay, M3 edition:
// - Pops instantly with a spinner; OCR arrives async from the worker thread.
// - Owner-drawn translucent global buttons (Save / Translate), top-center,
//   shifted down if they would cover text.
// - Click = whole box; press-drag = live char-range selection (CTC strips,
//   white-on-dark-blue, cross-box in reading order; I-beam over text).
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
            PAINTSTRUCT, PS_SOLID, Rectangle,
            SRCCOPY, TRANSPARENT,
        },
        System::SystemInformation::GetLocalTime,
        UI::{
            Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL, VK_ESCAPE},
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
                CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, DispatchMessageW, GetCursorPos, GetForegroundWindow, GetMessageW,
                GetSystemMetrics, GetWindowLongPtrW, GetWindowTextW, LoadCursorW, SetCursor,
                PostQuitMessage, RegisterClassW, SendMessageW, SetForegroundWindow,
                SetWindowLongPtrW, SetWindowTextW, TrackPopupMenu, TranslateMessage,
                CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, CBS_DROPDOWNLIST, CBN_CLOSEUP,
                CBN_SELCHANGE,
                CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, IDC_IBEAM, IDYES,
                MB_ICONQUESTION, MB_YESNO, MENU_ITEM_FLAGS, MF_GRAYED, MF_STRING, MSG, MessageBoxW, SM_CXVIRTUALSCREEN,
                SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOWNORMAL,
                TPM_RETURNCMD, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY,
                WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT,
                WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN, WS_EX_TOOLWINDOW,
                WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, WS_VSCROLL, KillTimer, SetTimer, SM_CXSCREEN, SM_CYSCREEN,
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
const M_ABOUT: usize = 2008;
const M_SELECT_ALL: usize = 2009;
const M_TRANSLATE_ALL: usize = 2010;
const M_SETTINGS: usize = 2011;
const TICK_MS: u32 = 50;
const TIMER_TICK: usize = 1;
const TIMER_CLOSE: usize = 2;
// Dedicated fade driver: the 50ms logic tick only yields ~16fps fades
// (6%+ alpha steps read as flicker), so the toast fade runs on its own
// 30ms timer (~26fps, ~8 alpha units per frame). Spinner/download stay
// on the slow tick; selection never conflicts (no layer caching).
const TIMER_FADE: usize = 3;
const FADE_MS: u32 = 30;
const TAP_MS: u64 = 300;
const TAP_PX: i32 = 6;

pub struct OverlayRequest {
    pub shot: RgbImage,
    pub active_title: String,
    pub search_url: String,
    pub translate_url: String,
    pub translate_mode: String,
    pub external_auto_exit: bool,
    pub external_fallback: bool,
    pub external_fallback_ask: bool,
    pub translate_source: String,
    pub translate_target: String,
    pub save_dir: Option<String>,
    pub ui_lang: String,
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
    translate_url: String,
    translate_mode: String,
    external_auto_exit: bool,
    external_fallback: bool,
    external_fallback_ask: bool,
    save_dir: Option<String>,
    ui_lang: String,
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
    // Toast: last painted alpha 0..=220; 0xFF forces repaint on next paint.
    toast_level: u8,
    tmode: TMode,
    translated: Option<Translated>,
    translating: bool,
    pending: Option<PendingT>, // waiting on the translate worker
    btn_save: (i32, i32, i32, i32),
    btn_tr_sel: (i32, i32, i32, i32),
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

/// Dev harness: `--dialog-test` shows the fallback consent dialog standalone
/// (no OCR, no overlay), prints the choice, exits. Deterministic UI check
/// without model juggling; persistence is unit-tested, not exercised here.
pub fn run_dialog_test() -> Result<()> {
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let c = ask_external_fallback(NULL_HWND, "translate.google.com", (w / 2, h / 2));
    println!("dialog-test: open={} remember={}", c.open, c.remember);
    Ok(())
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
            translate_mode: req.translate_mode,
            external_auto_exit: req.external_auto_exit,
            external_fallback: req.external_fallback,
            external_fallback_ask: req.external_fallback_ask,
            save_dir: req.save_dir,
            ui_lang: req.ui_lang,
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
            translated: None,
            translating: false,
            pending: None,
            btn_save: (0, 0, 0, 0),
            btn_tr_sel: (0, 0, 0, 0),
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
            WS_POPUP | WS_VISIBLE | WS_CLIPCHILDREN,
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
    // Row on the focus (cursor) monitor: [save][src][tgt][tr-sel][tr-full].
    let total = BW * 3 + CBW * 2 + GAP * 4;
    let x0 = st.focus.cx() - total / 2;
    let mut y = st.focus.t + 24;
    let x1 = x0 + BW;
    let x2 = x1 + GAP + CBW;
    let x3 = x2 + GAP + CBW;
    let x4 = x3 + GAP + BW;
    let mut rects = [
        (x0, y, x1, y + BH),
        (x1 + GAP, y, x2, y + BH),
        (x2 + GAP, y, x3, y + BH),
        (x3 + GAP, y, x4, y + BH),
        (x4 + GAP, y, x0 + total, y + BH),
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
    st.btn_tr_sel = rects[3];
    st.btn_tr = rects[4];
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

/// Anything started (in flight or painted) offers cancel; idle offers work.
/// One predicate drives the big button, its paint label, and the menu item.
fn tr_busy(st: &State) -> bool {
    st.translating || st.tmode != TMode::Idle
}

/// Esc priority: cancel painted/in-flight translation first, then clear the
/// selection, and only when neither exists close the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscAction {
    CancelTranslation,
    ClearSelection,
    Close,
}

fn esc_action(st: &State) -> EscAction {
    if tr_busy(st) || st.translated.is_some() {
        EscAction::CancelTranslation
    } else if has_selection(st) {
        EscAction::ClearSelection
    } else {
        EscAction::Close
    }
}

/// Shared cancel for the button and the menu: drop the job (a late worker
/// reply finds pending=None and is discarded; the reply drain takes latest
/// only, so nothing poisons the next run) and clear painted subtitles.
fn cancel_translation(st: &mut State) {
    st.translated = None;
    st.pending = None;
    st.translating = false;
    st.tmode = TMode::Idle;
    st.toast = None;
    st.toast_level = 0xFF;
    set_title(st, "Selectable");
    invalidate(st);
}

/// Translate button: idle starts full; busy cancels back to original.
fn translate_button(st: &mut State) {
    if tr_busy(st) {
        cancel_translation(st);
    } else {
        translate_full(st);
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
        WM_TIMER => {
            if wp.0 == TIMER_FADE {
                // Fast fade driver: self-kills once the toast is gone.
                if st.toast.is_none() {
                    unsafe {
                        let _ = KillTimer(Some(st.hwnd), TIMER_FADE);
                    }
                } else if poll_toast(st) {
                    invalidate(st);
                }
            } else {
                on_tick(st, wp.0);
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // Child controls (comboboxes) keep default handling.
            if (wp.0 as isize) != (st.hwnd.0 as isize) {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
            // I-beam over OCR text, arrow elsewhere (buttons/empty area).
            let over_text = unsafe {
                let mut pt = POINT { x: 0, y: 0 };
                if GetCursorPos(&mut pt).is_ok() {
                    line_at_point(st, (pt.x - st.ox, pt.y - st.oy)).is_some()
                } else {
                    false
                }
            };
            let id = if over_text { IDC_IBEAM } else { IDC_ARROW };
            unsafe {
                if let Ok(h) = LoadCursorW(None, id) {
                    SetCursor(Some(h));
                }
            }
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            let p = (loword(lp.0) as i32, hiword(lp.0) as i32);
            if st.dl.as_ref().map(|d| in_rect(p, d.cancel_rect)).unwrap_or(false) {
                if let Some(d) = st.dl.as_ref() {
                    d.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    show_toast(st, "取消中…".to_string());
                }
            } else if in_rect(p, st.btn_save) {
                dlog(format!("down save {p:?}"));
                do_quick_save(st);
            } else if in_rect(p, st.btn_tr_sel) {
                dlog(format!("down translate-sel {p:?}"));
                if tr_busy(st) {
                    cancel_translation(st);
                } else {
                    translate_menu(st);
                }
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
                    if st.anchor.is_none() {
                        st.anchor = st.cursor;
                    }
                    // Live browser-style highlight: selection follows the
                    // cursor during the drag (no rubber band anymore).
                    let live = norm_range(st.anchor, st.cursor);
                    if live != st.sel {
                        st.sel = live;
                        if st.sel.is_some() {
                            set_title(st, &format!("Selectable — 已选中 {} 字", count_chars(st)));
                        } else {
                            set_title(st, "Selectable");
                        }
                        invalidate(st);
                    }
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
            // The only WM_COMMAND traffic is CBN_SELCHANGE/CBN_CLOSEUP from the
            // dropdowns (real child HWNDs). They steal keyboard focus on use,
            // which would silently break overlay keys (Esc/Ctrl+C) afterwards,
            // so focus always returns to the overlay itself.
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
                    unsafe {
                        let _ = SetFocus(Some(st.hwnd));
                    }
                } else if code == CBN_CLOSEUP {
                    unsafe {
                        let _ = SetFocus(Some(st.hwnd));
                    }
                }
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = wp.0 as u32;
            if vk == VK_ESCAPE.0 as u32 {
                match esc_action(st) {
                    EscAction::CancelTranslation => cancel_translation(st),
                    EscAction::ClearSelection => {
                        st.sel = None;
                        st.toast = None;
                        st.toast_level = 0xFF;
                        set_title(st, "Selectable");
                        invalidate(st);
                    }
                    EscAction::Close => {
                        let _ = DestroyWindow(hwnd);
                    }
                }
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
                let mut info = format!("ready lines={} save={:?} tr_sel={:?} tr={:?}", st.lines.len(), st.btn_save, st.btn_tr_sel, st.btn_tr);
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
    if poll_toast(st) {
        dirty = true;
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
                    let text = selected_or_all(st);
                    let msg = format!("{e:#}");
                    translate_failure_fallback(st, &text, &msg);
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
    if st.lines.is_empty() {
        return String::new();
    }
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

/// Non-empty text selection (empty carets don't count).
fn has_selection(st: &State) -> bool {
    st.sel.map(|r| !range_text(st, r).trim().is_empty()).unwrap_or(false)
}

/// Per-line subtitle regions for a selection (refined partial): rect = bbox
/// of the SELECTED strips on that line (never larger than the blue box),
/// text = the selected slice. Mirrors paint_selection's span math so
/// subtitles hug the selection: partial-line starts/ends, inter-line gaps
/// and unrelated blocks stay uncovered. Full mode keeps macro-block regions.
fn selection_regions(st: &State, r: (Pos, Pos)) -> Vec<RegionJob> {
    let ((l0, c0), (l1, c1)) = r;
    let mut out = Vec::new();
    for li in l0..=l1.min(st.lines.len().saturating_sub(1)) {
        let line = &st.lines[li];
        if !line.quad.ordered().is_horizontal() || line.chars.is_empty() {
            // Vertical fallback: whole box, whole text (same as blue box).
            if line.text.trim().is_empty() {
                continue;
            }
            let (x0, y0, x1, y1) = line.quad.axis_aligned_bounds();
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let h = y1.saturating_sub(y0).max(8);
            out.push(RegionJob {
                rect: (x0 as i32, y0 as i32, x1 as i32, y1 as i32),
                font_px: h.saturating_sub(2).max(12),
                text: line.text.clone(),
            });
            continue;
        }
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
        let (s, e) = (s.min(line.chars.len()), e.min(line.chars.len()));
        if s >= e {
            continue;
        }
        let text: String = line.chars[s..e].iter().map(|c| c.text.as_str()).collect();
        if text.trim().is_empty() {
            continue;
        }
        let (mut x0, mut y0, mut x1, mut y1) =
            (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
        for c in &line.chars[s..e] {
            let strip = q.hstrip(c.f0, c.f1);
            for p in strip.points {
                x0 = x0.min(p[0]);
                y0 = y0.min(p[1]);
                x1 = x1.max(p[0]);
                y1 = y1.max(p[1]);
            }
        }
        let (x0, y0, x1, y1) = (x0 as i32, y0 as i32, x1 as i32, y1 as i32);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let h = (y1 - y0).max(8) as u32;
        out.push(RegionJob { rect: (x0, y0, x1, y1), font_px: h.saturating_sub(2).max(12), text });
    }
    out
}

/// Region payload for a scope: Partial = one region per selected line
/// (selection_regions), Full = one region per macro-block (R4).
fn build_regions(st: &State, mode: TMode) -> Vec<RegionJob> {
    match mode {
        TMode::Partial => {
            let Some(r) = st.sel else { return vec![] };
            if range_text(st, r).trim().is_empty() || st.lines.is_empty() {
                return vec![];
            }
            selection_regions(st, r)
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
    if !st.ready || st.translating {
        return;
    }
    if st.dl.is_some() {
        // The button/menu offer cancel while busy, so this only fires when a
        // download panel (with its own 取消) owns the job: say so, don't mute.
        show_toast(st, "下载中…".to_string());
        return;
    }
    let regions = build_regions(st, mode);
    if regions.is_empty() {
        show_toast(st, "先选中要翻译的文字".to_string());
        return;
    }
    // Probe (R3/R4): partial probes the raw selection concat (the refined
    // regions may span several blocks and misjudge mixed screens); full
    // probes the longest macro-block.
    let probe = match mode {
        TMode::Partial => selected_text(st),
        _ => regions.iter().max_by_key(|r| r.text.len()).map(|r| r.text.clone()).unwrap_or_default(),
    };
    let (src, tgt, legs) = match resolve_plan(st, &probe) {
        Ok(v) => v,
        Err(e) if e == "UNSUPPORTED" => {
            // No direct leg and no via-en pivot: external translator fallback.
            let url = fill_url(&st.translate_url, &probe);
            open_external(st, &url, "暂不支持该语种组合，已转外部翻译");
            return;
        }
        Err(e) => {
            dlog(format!("start_translate resolve-err: {e} probe={}", preview(&probe, 60)));
            show_toast(st, e);
            return;
        }
    };
    // Engine switch (single dispatch): external skips the local engine and
    // opens the translator URL; online is not wired yet and rides bergamot
    // with a notice; anything else is bergamot.
    if st.translate_mode == "external" {
        let text = match mode {
            TMode::Partial => selected_text(st),
            _ => all_text(st),
        };
        external_translate(st, &text, &tgt);
        return;
    }
    if st.translate_mode == "online" {
        show_toast(st, "online 翻译未接入，已回退本地引擎".to_string());
    }
    let label = legs.join("+");
    dlog(format!("start_translate legs={label} src={src} tgt={tgt}"));
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

/// Engine-failure fallback: auto-open when the user enabled it; otherwise
/// ask once, disclosing the destination host. Remembered answers persist to
/// config.toml (a write failure only toasts — the in-memory choice still
/// applies to this run).
fn translate_failure_fallback(st: &mut State, text: &str, err: &str) {
    let url = fill_url(&st.translate_url, text);
    if st.external_fallback {
        open_external(st, &url, &format!("翻译失败，已转外部：{err}"));
        return;
    }
    if !st.external_fallback_ask {
        show_toast(st, format!("翻译失败：{err}"));
        return;
    }
    let host = url_host(&url).to_string();
    let f = &st.focus;
    let center = ((f.l + f.r) / 2, (f.t + f.b) / 2);
    match ask_external_fallback(st.hwnd, &host, center) {
        FallbackChoice { open: true, remember } => {
            if remember {
                st.external_fallback = true;
                if let Err(e) = persist_translate_key("external_fallback", "true") {
                    // Window dies on open below; the in-memory flag still
                    // holds for this run, the file retry happens next time.
                    dlog(format!("persist external_fallback failed: {e:#}"));
                }
            }
            open_external(st, &url, &format!("翻译失败，已转外部：{err}"));
        }
        FallbackChoice { open: false, remember } => {
            if remember {
                st.external_fallback_ask = false;
                if let Err(e) = persist_translate_key("external_fallback_ask", "false") {
                    show_toast(st, format!("记住选择失败：{e:#}"));
                    return;
                }
            }
            show_toast(st, "已取消".to_string());
        }
    }
}

fn persist_translate_key(key: &str, value: &str) -> anyhow::Result<()> {
    let path = config_write_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(&path, set_translate_key(&old, key, value))?;
    Ok(())
}

/// Result of the fallback consent dialog.
struct FallbackChoice {
    open: bool,
    remember: bool,
}

/// Client-space geometry of the fallback consent dialog (all px).
const FALLBACK_MARGIN: i32 = 16;
const FALLBACK_TEXT_W: i32 = 428;
const FALLBACK_CLIENT_W: i32 = FALLBACK_MARGIN * 2 + FALLBACK_TEXT_W;
const FALLBACK_BODY_Y: i32 = 12;
const FALLBACK_CHECK_H: i32 = 24;
const FALLBACK_BTN_W: i32 = 96;
const FALLBACK_BTN_H: i32 = 30;

/// Pure layout: client size + control rows from the measured body height.
/// Returns `(client_w, client_h, check_y, btn_y)`.
/// Regression anchor: the old fixed 215px frame clipped Yes/No (its client
/// area was ~185px); buttons must always end inside the client rect.
fn fallback_layout(body_h: i32) -> (i32, i32, i32, i32) {
    let check_y = FALLBACK_BODY_Y + body_h.max(32) + 10;
    let btn_y = check_y + FALLBACK_CHECK_H + 14;
    let client_h = btn_y + FALLBACK_BTN_H + FALLBACK_MARGIN;
    (FALLBACK_CLIENT_W, client_h, check_y, btn_y)
}

/// Modal Yes/No + "don't ask again" checkbox for the engine-failure
/// fallback. Hand-rolled (no .rc in this repo): popup window + STATIC +
/// CHECKBOX + two buttons centered on the focus monitor (never the bezel
/// seam), owner disabled during the modal loop. Esc/X = No without remembering.
///
/// Controls take the system message font (same source as Notepad) via
/// `WM_SETFONT`; the frame is measured from the wrapped body text
/// (`DT_CALCRECT` + `AdjustWindowRectEx`), so any host length keeps Yes/No
/// on screen.
fn ask_external_fallback(owner: HWND, host: &str, center: (i32, i32)) -> FallbackChoice {
    use windows::Win32::UI::WindowsAndMessaging::{
        AdjustWindowRectEx, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BM_GETCHECK, BN_CLICKED, CreateWindowExW,
        DefWindowProcW, DispatchMessageW, GetMessageW,
        IsDialogMessageW, RegisterClassW, SetWindowLongPtrW, GetWindowLongPtrW,
        ShowWindow, TranslateMessage, GWLP_ID, GWLP_USERDATA, HTCAPTION,
        IDCANCEL, IDOK, SW_SHOWNORMAL, WINDOW_STYLE, WM_CAPTURECHANGED, WM_CLOSE, WM_LBUTTONUP,
        WM_MOUSEMOVE, WM_NCCREATE, WM_NCLBUTTONDOWN, WM_SETFONT,
        WNDCLASSW, WNDCLASS_STYLES, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
        WS_EX_DLGMODALFRAME, WS_POPUP,
        WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
    use windows::Win32::Graphics::Gdi::CreateSolidBrush;

    struct Dlg {
        check: HWND,
        open: bool,
        remember: bool,
        done: bool,
        drag: crate::settings::SysDrag,
    }

    unsafe extern "system" fn dlg_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        use windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
        if msg == WM_NCCREATE {
            let cs = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dlg;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let d = &mut *ptr;
        match msg {
            // Owned caption drag (same fix as the settings dialog: the system
            // move loop replays every mouse report at high report rates).
            WM_NCLBUTTONDOWN if wp.0 == HTCAPTION as usize => {
                d.drag.begin(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE if d.drag.active => {
                d.drag.on_move(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONUP if d.drag.active => {
                d.drag.end();
                LRESULT(0)
            }
            WM_CAPTURECHANGED if d.drag.active => {
                d.drag.active = false;
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = (wp.0 & 0xffff) as u32;
                let code = ((wp.0 >> 16) & 0xffff) as u32;
                if code == BN_CLICKED && (id == IDOK.0 as u32 || id == IDCANCEL.0 as u32) {
                    if d.drag.active {
                        // Enter/Esc mid-drag: drop/snap back, stay open.
                        if id == IDCANCEL.0 as u32 {
                            d.drag.cancel(hwnd);
                        } else {
                            d.drag.end();
                        }
                        LRESULT(0)
                    } else {
                        d.open = id == IDOK.0 as u32;
                        d.remember = SendMessageW(d.check, BM_GETCHECK, None, None).0 == 1; // BST_CHECKED
                        d.done = true;
                        LRESULT(0)
                    }
                } else {
                    DefWindowProcW(hwnd, msg, wp, lp)
                }
            }
            WM_CLOSE => {
                d.done = true; // X = No, forget.
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }

    unsafe {
        static mut REGISTERED: bool = false;
        if !REGISTERED {
            // Dialog-gray brush intentionally leaked (process-lifetime class).
            let bg = CreateSolidBrush(COLORREF(0xF0F0F0));
            let cls = WNDCLASSW {
                // No CS_HREDRAW|CS_VREDRAW (same drag-lag as the settings
                // dialog): a move must bit-blt, not repaint all children.
                style: WNDCLASS_STYLES(0),
                lpfnWndProc: Some(dlg_proc),
                hInstance: windows::Win32::Foundation::HINSTANCE(null_mut()),
                hbrBackground: bg.into(),
                lpszClassName: w!("SelectableFallback"),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..std::mem::zeroed()
            };
            let _ = RegisterClassW(&cls);
            REGISTERED = true;
        }
        // Center on the focus monitor (the button row lives there too).
        // The frame is derived from the client rect: fixed W/H clipped the
        // buttons because the caption eats ~30px (see fallback_layout).
        let body = format!("本地翻译失败。\n是否用外部浏览器继续？\n\n选中文本将被发送到：{host}");
        let (hfont, owned) = crate::settings::system_message_font();
        let body_h = crate::settings::measure_text_h(hfont, &body, FALLBACK_TEXT_W);
        let (client_w, client_h, check_y, btn_y) = fallback_layout(body_h);
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN | WS_CLIPSIBLINGS;
        let mut frame = RECT { left: 0, top: 0, right: client_w, bottom: client_h };
        let _ = AdjustWindowRectEx(&mut frame, style, false, WS_EX_DLGMODALFRAME);
        let win_w = frame.right - frame.left;
        let win_h = frame.bottom - frame.top;
        let (cx, cy) = center;
        let mut state = Box::new(Dlg { check: NULL_HWND, open: false, remember: false, done: false, drag: crate::settings::SysDrag::default() });
        let ptr = state.as_mut() as *mut Dlg;
        let dlg = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            w!("SelectableFallback"),
            w!("本地翻译失败"),
            style,
            cx - win_w / 2,
            cy - win_h / 2,
            win_w,
            win_h,
            Some(owner),
            None,
            None,
            Some(ptr as *const _),
        )
        .unwrap_or_default();
        if dlg.is_invalid() {
            if owned {
                let _ = DeleteObject(hfont.into());
            }
            return FallbackChoice { open: false, remember: false };
        }
        let wbody: Vec<u16> = body.encode_utf16().chain([0]).collect();
        let body_ctl = CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            PCWSTR(wbody.as_ptr()),
            WS_CHILD | WS_VISIBLE, // SS_LEFT == 0: plain static wraps by default.
            FALLBACK_MARGIN, FALLBACK_BODY_Y, FALLBACK_TEXT_W, body_h,
            Some(dlg),
            None,
            None,
            None,
        )
        .unwrap_or_default();
        let check = CreateWindowExW(
            Default::default(),
            w!("BUTTON"),
            w!("下次不再问"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
            FALLBACK_MARGIN, check_y, 200, FALLBACK_CHECK_H,
            Some(dlg),
            None,
            None,
            None,
        )
        .unwrap_or_default();
        let no_x = client_w - FALLBACK_MARGIN - FALLBACK_BTN_W;
        let yes_x = no_x - 10 - FALLBACK_BTN_W;
        let yes = CreateWindowExW(
            Default::default(),
            w!("BUTTON"),
            w!("是"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            yes_x, btn_y, FALLBACK_BTN_W, FALLBACK_BTN_H,
            Some(dlg),
            None,
            None,
            None,
        )
        .unwrap_or_default();
        let no = CreateWindowExW(
            Default::default(),
            w!("BUTTON"),
            w!("否"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            no_x, btn_y, FALLBACK_BTN_W, FALLBACK_BTN_H,
            Some(dlg),
            None,
            None,
            None,
        )
        .unwrap_or_default();
        // System font on every control (hand-rolled windows get none by
        // default, which falls back to the legacy bitmap font).
        for c in [body_ctl, check, yes, no] {
            SendMessageW(c, WM_SETFONT, Some(WPARAM(hfont.0 as usize)), Some(LPARAM(1)));
        }
        // IDs drive the result (Enter/Tab/Esc via IsDialogMessage below).
        SetWindowLongPtrW(yes, GWLP_ID, IDOK.0 as isize);
        SetWindowLongPtrW(no, GWLP_ID, IDCANCEL.0 as isize);
        (*ptr).check = check;
        let _ = SetFocus(Some(yes));
        let _ = ShowWindow(dlg, SW_SHOWNORMAL);
        let _ = EnableWindow(owner, false);
        // Modal loop with dialog navigation (Tab/Enter/Esc).
        let mut msg: MSG = std::mem::zeroed();
        while !(*ptr).done {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if !r.as_bool() {
                break;
            }
            if IsDialogMessageW(dlg, &msg).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let choice = FallbackChoice { open: (*ptr).open, remember: (*ptr).remember };
        // Same teardown order as settings: clear USERDATA before DestroyWindow
        // so WM_DESTROY/WM_NCDESTROY never derefs a freed Box (UB).
        SetWindowLongPtrW(dlg, GWLP_USERDATA, 0);
        let _ = DestroyWindow(dlg);
        drop(Box::from_raw(ptr));
        std::mem::forget(state);
        if owned {
            let _ = DeleteObject(hfont.into());
        }
        let _ = EnableWindow(owner, true);
        let _ = SetForegroundWindow(owner);
        let _ = SetFocus(Some(owner));
        choice
    }
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

/// Select every OCR line (menu 全选). Empty screen selects nothing.
fn select_all(st: &mut State) {
    let Some(last) = st.lines.len().checked_sub(1) else {
        st.sel = None;
        return;
    };
    let c = st.lines[last].chars.len();
    st.sel = Some(((0, 0), (last, c)));
    set_title(st, &format!("Selectable — 已选中 {} 字", count_chars(st)));
    invalidate(st);
}

/// R1: right-click with no (or empty) selection first selects the whole box
/// under the cursor, so the menu always acts on something when possible.
/// Must select ((li,0),(li,n)) like a tap: an empty caret range paints zero
/// strips (no blue box), toasts nothing, and leaves the menu grayed.
fn autoselect_at(st: &mut State, p: (i32, i32)) {
    if has_selection(st) {
        return;
    }
    if let Some(li) = line_at_point(st, p) {
        let n = st.lines[li].chars.len();
        st.sel = Some(((li, 0), (li, n)));
        set_title(st, &format!("Selectable — 已选中 {} 字", count_chars(st)));
        invalidate(st);
    }
}

/// Context-menu "翻译选中文本": partial overlay of the current selection.
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

fn show_toast(st: &mut State, text: String) {
    let now = Instant::now();
    st.toast = Some(Toast { text, deadline: now + Duration::from_secs(3) });
    st.toast_level = 0xFF; // force first paint
    // Arm the fast fade driver (idempotent re-arm; self-kills on expiry).
    unsafe {
        let _ = SetTimer(Some(st.hwnd), TIMER_FADE, FADE_MS, None);
    }
    invalidate(st);
}

/// Shared toast-fade step: expiry clears the toast, otherwise alpha tracks
/// remaining time. Returns true when the visible alpha changed.
fn poll_toast(st: &mut State) -> bool {
    let expired = st.toast.as_ref().map(|t| Instant::now() >= t.deadline).unwrap_or(false);
    if expired {
        st.toast = None;
        st.toast_level = 0xFF;
        return true;
    }
    if st.toast.is_none() {
        return false;
    }
    let remain = st.toast.as_ref().unwrap().deadline.saturating_duration_since(Instant::now()).as_millis();
    let alpha = toast_fade_alpha(remain);
    if alpha != st.toast_level {
        st.toast_level = alpha;
        true
    } else {
        false
    }
}

/// Toast fade alpha from remaining lifetime in ms: 220 opaque while more
/// than 1s remains, linear 220->0 across the last second, 0 at expiry.
/// u128 math: `remain * 220` can exceed u64 range in theory; the cast to u8
/// happens after clamping to 220. Regression test below pins the curve.
fn toast_fade_alpha(remain_ms: u128) -> u8 {
    if remain_ms > 1000 {
        220
    } else {
        ((remain_ms * 220 / 1000).min(220)) as u8
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
            let old_brush = SelectObject(hdc, hollow);
            let old_pen = SelectObject(hdc, red.into());
            for l in &st.lines {
                let (x0, y0, x1, y1) = l.quad.axis_aligned_bounds();
                let _ = Rectangle(hdc, x0 as i32, y0 as i32, x1 as i32, y1 as i32);
            }
            // Selection: dark-blue strips + white chars (live during drag).
            paint_selection(hdc, st);
            // Translated subtitles strictly above the selection (R2): they
            // cover the blue/white strips with their own dark panels.
            if let Some(tr) = st.translated.clone() {
                paint_translated(hdc, st, &tr);
            }
            SelectObject(hdc, old_pen);
            SelectObject(hdc, old_brush);
            let _ = DeleteObject(red.into());
            // Buttons.
            paint_button(hdc, st, st.btn_save, "保存");
            paint_button(hdc, st, st.btn_tr_sel, if tr_busy(st) { "取消翻译" } else { "翻译选中" });
            paint_button(hdc, st, st.btn_tr, if tr_busy(st) { "取消翻译" } else { "翻译" });
        } else {
            // Spinner while the worker runs.
            paint_spinner(hdc, st);
            if let Some(e) = &st.load_error {
                show_toast_paint(hdc, st, &format!("OCR 失败：{e}"), 220);
            }
        }

        // Toast on top of everything (alpha tracked per-tick; 0xFF = first
        // paint before any tick, treat as fully opaque).
        let toast_data: Option<(String, u8)> = match &st.toast {
            Some(t) => Some((
                t.text.clone(),
                if st.toast_level == 0xFF { 220 } else { st.toast_level },
            )),
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
        let has_sel = has_selection(st);
        let ui = crate::lang::resolve_ui_lang(Some(&st.ui_lang));
        let mi = crate::settings::append_item;
        mi(menu, menu_flags(has_sel), M_COPY_SEL, crate::lang::tr(ui, "menu.copy_sel"));
        mi(menu, MF_STRING, M_COPY_ALL, crate::lang::tr(ui, "menu.copy_all"));
        mi(menu, menu_flags(!st.lines.is_empty()), M_SELECT_ALL, crate::lang::tr(ui, "menu.select_all"));
        mi(menu, menu_flags(has_sel), M_SEARCH, crate::lang::tr(ui, "menu.search"));
        if tr_busy(st) {
            // Cancel needs no selection: both rows cancel, always enabled.
            mi(menu, MF_STRING, M_TRANSLATE, crate::lang::tr(ui, "menu.cancel_tr"));
            mi(menu, MF_STRING, M_TRANSLATE_ALL, crate::lang::tr(ui, "menu.cancel_tr"));
        } else {
            mi(menu, menu_flags(has_sel), M_TRANSLATE, crate::lang::tr(ui, "menu.tr_sel"));
            mi(menu, menu_flags(!st.lines.is_empty()), M_TRANSLATE_ALL, crate::lang::tr(ui, "menu.tr_all"));
        }
        mi(menu, MF_STRING, M_SAVE, crate::lang::tr(ui, "menu.save"));
        mi(menu, MF_STRING, M_SAVE_AS, crate::lang::tr(ui, "menu.save_as"));
        mi(menu, MF_STRING, M_SETTINGS, crate::lang::tr(ui, "menu.settings"));
        mi(menu, MF_STRING, M_ABOUT, crate::lang::tr(ui, "menu.about"));
        mi(menu, MF_STRING, M_CLOSE, crate::lang::tr(ui, "menu.close"));
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
            M_SELECT_ALL => select_all(st),
            M_SEARCH => {
                let url = fill_url(&st.search_url, &selected_or_all(st));
                open_external(st, &url, "已在浏览器中打开搜索");
            }
            M_TRANSLATE => {
                if tr_busy(st) {
                    cancel_translation(st);
                } else {
                    translate_menu(st);
                }
            }
            M_TRANSLATE_ALL => {
                if tr_busy(st) {
                    cancel_translation(st);
                } else {
                    translate_full(st);
                }
            }
            M_SAVE => do_quick_save(st),
            M_SAVE_AS => do_save_as(st),
            M_SETTINGS => open_settings(st),
            M_ABOUT => crate::about::show_about(st.hwnd),
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

/// Settings from the overlay menu: no live hotkey owner here (the tray owns
/// registration), so hotkey/ui/OCR changes report restart notes via toast.
fn open_settings(st: &mut State) {
    let f = &st.focus;
    let center = ((f.l + f.r) / 2, (f.t + f.b) / 2);
    let o = crate::settings::run_settings(st.hwnd, center, None);
    if !o.saved {
        return;
    }
    if o.needs_restart {
        show_toast(st, "设置已保存，部分生效需重启".to_string());
    } else {
        show_toast(st, "设置已保存".to_string());
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

/// Extension-driven encoder (issue #5): jpg/jpeg go through an explicit
/// quality-90 JPEG pass (the `save` default of 75 rings on screenshot text);
/// everything else keeps the `image` crate's extension dispatch (png, …).
fn save_shot(shot: &RgbImage, path: &std::path::Path) -> Result<()> {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("jpg") | Some("jpeg") => {
            let f = std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
            let mut w = std::io::BufWriter::new(f);
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, 90);
            enc.encode(&shot.as_raw()[..], shot.width(), shot.height(), image::ExtendedColorType::Rgb8)
                .with_context(|| format!("jpeg-encode {}", path.display()))?;
            Ok(())
        }
        _ => {
            shot.save(path).with_context(|| format!("save {}", path.display()))?;
            Ok(())
        }
    }
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
        let filter: Vec<u16> = "PNG 图片\0*.png\0JPEG 图片\0*.jpg;*.jpeg\0所有文件\0*.*\0\0"
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
                // lpstrDefExt always suggests png, so honor the picked
                // filter (issue #5): JPEG slot -> .jpg, else .png.
                path.set_extension(if ofn.nFilterIndex == 2 { "jpg" } else { "png" });
            }
            let shot = rebuild_shot(st);
            save_shot(&shot, &path)?;
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

/// Host part of a URL for the fallback consent dialog
/// ("translate.google.com"), so users see WHERE their text goes.
fn url_host(url: &str) -> &str {
    let s = url.split("://").nth(1).unwrap_or(url);
    let s = s.split('/').next().unwrap_or(s);
    s.split('?').next().unwrap_or(s)
}

/// Set `key = value` under `[translate]`, preserving everything else
/// (comments, order, other sections). Thin wrapper over the generalized
/// settings writer; the regression tests below pin the behavior.
fn set_translate_key(src: &str, key: &str, value: &str) -> String {
    crate::settings::set_section_key(src, "translate", key, value)
}

/// Where AppConfig::load reads from (exe dir first, then cwd); the same file
/// is the write target so remembered choices actually reload.
fn config_write_path() -> std::path::PathBuf {
    crate::settings::config_write_path()
}

/// Bergamot pair code to a BCP47 tag for external translator URLs
/// (Google wants zh-CN, not zh).
fn external_lang_tag(code: &str) -> &str {
    match code.trim().to_lowercase().as_str() {
        "zh" => "zh-CN",
        "zh_hant" => "zh-TW",
        _ => code,
    }
}

/// External mode: skip the local engine entirely and open the translator
/// URL for the scope text (partial = selection, full = everything). The
/// default template carries {tgt}; auto-exit applies like every external open.
fn external_translate(st: &mut State, text: &str, tgt: &str) {
    if text.trim().is_empty() {
        show_toast(st, "先选中要翻译的文字".to_string());
        return;
    }
    let url = fill_url(&st.translate_url, text).replace("{tgt}", external_lang_tag(tgt));
    // Borrow ends here (owned String); open_external takes &mut.
    open_external(st, &url, "已在浏览器中打开翻译");
}

fn open_url(url: &str) -> bool {
    unsafe {
        let u = HSTRING::from(url);
        ShellExecuteW(None, w!("open"), &u, None, None, SW_SHOWNORMAL).0 as isize > 32
    }
}

/// External browser launch shared by search + translate fallbacks. With
/// external_auto_exit (default) the fullscreen cover closes on success so the
/// user actually sees the just-opened browser; otherwise (or when the launch
/// itself fails) a toast explains the state and the overlay stays.
fn open_external(st: &mut State, url: &str, stay_msg: &str) {
    if open_url(url) {
        if st.external_auto_exit {
            unsafe {
                let _ = DestroyWindow(st.hwnd);
            }
            return;
        }
    } else {
        show_toast(st, "打开浏览器失败".to_string());
        return;
    }
    show_toast(st, stay_msg.to_string());
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

    #[test]
    fn save_shot_dispatches_on_extension() {
        // Tiny gradient so JPEG has something to compress.
        let mut img = RgbImage::new(32, 16);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([(x * 8) as u8, 100, 200]);
        }
        let dir = std::path::PathBuf::from("temp");
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("save-shot-test.png");
        let jpg = dir.join("save-shot-test.jpg");
        save_shot(&img, &png).unwrap();
        save_shot(&img, &jpg).unwrap();
        let png_bytes = std::fs::read(&png).unwrap();
        let jpg_bytes = std::fs::read(&jpg).unwrap();
        assert!(png_bytes.starts_with(&[0x89, b'P', b'N', b'G']), "not png");
        assert!(jpg_bytes.starts_with(&[0xFF, 0xD8]), "not jpeg");
        assert!(!png_bytes.is_empty() && !jpg_bytes.is_empty());
        let _ = std::fs::remove_file(&png);
        let _ = std::fs::remove_file(&jpg);
    }

    fn toy_state() -> State {
        State {
            bgra: Vec::new(), w: 0, h: 0, title: String::new(),
            search_url: String::new(), translate_url: String::new(), translate_mode: "bergamot".to_string(),
            external_auto_exit: true, external_fallback: false, external_fallback_ask: true,
            save_dir: None, ui_lang: "auto".to_string(),
            hwnd: NULL_HWND, rx: std::ptr::null(),
            ready: false, load_error: None, lines: Vec::new(), tier: String::new(),
            spinner: 0, sel: None, press_at: None, anchor: None, cursor: None,
            dragging: false, toast: None, toast_level: 0xFF, tmode: TMode::Idle,
            translated: None, translating: false, pending: None,
            combo_src: NULL_HWND, combo_tgt: NULL_HWND,
            src_items: vec!["Auto".to_string()], tgt_items: Vec::new(),
            src_sel: SrcSel::Auto, tgt: String::new(),
            src_cfg: String::new(), tgt_cfg: String::new(),
            tworker: None, dl: None,
            btn_save: (0, 0, 0, 0), btn_tr_sel: (0, 0, 0, 0), btn_tr: (0, 0, 0, 0),
            combo_src_r: (0, 0, 0, 0), combo_tgt_r: (0, 0, 0, 0),
            ox: 0, oy: 0,
            mons: vec![crate::monitors::Mon { l: 0, t: 0, r: 1920, b: 1080, primary: true }],
            focus: crate::monitors::Mon { l: 0, t: 0, r: 1920, b: 1080, primary: true },
            back: None,
            fonts: HashMap::new(),
        }
    }

    fn toy_line(text: &str) -> OcrLine {
        toy_line_at(text, 0.0)
    }

    fn toy_line_at(text: &str, y0: f32) -> OcrLine {
        let n = text.chars().count().max(1) as f32;
        OcrLine {
            quad: crate::geometry::Quad::from_xyxy(0.0, y0, 100.0, y0 + 20.0),
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
    fn r1_autoselect_takes_whole_box() {
        // Regression: autoselect_at used to set an empty caret ((li,0),(li,0))
        // which paints zero strips (no blue box) and leaves the menu grayed.
        let mut st = toy_state();
        st.lines = vec![toy_line("abcd")];
        autoselect_at(&mut st, (50, 10));
        assert_eq!(st.sel, Some(((0, 0), (0, 4))));
        assert_eq!(selected_text(&st), "abcd");
        // Existing non-empty selection is left alone (even off-box clicks).
        autoselect_at(&mut st, (500, 500));
        assert_eq!(st.sel, Some(((0, 0), (0, 4))));
        // Empty caret upgrades to the whole box.
        st.sel = Some(((0, 2), (0, 2)));
        autoselect_at(&mut st, (50, 10));
        assert_eq!(selected_text(&st), "abcd");
    }
    #[test]
    fn select_all_covers_every_line() {
        let mut st = toy_state();
        st.lines = vec![toy_line("abcd"), toy_line("截屏")];
        select_all(&mut st);
        assert_eq!(st.sel, Some(((0, 0), (1, 2))));
        assert_eq!(selected_text(&st), "abcd\n截屏");
        // Empty screen: selects nothing, never panics.
        st.lines.clear();
        select_all(&mut st);
        assert!(st.sel.is_none());
    }

    #[test]
    fn esc_priority_chain() {
        let mut st = toy_state();
        // Nothing: close.
        assert_eq!(esc_action(&st), EscAction::Close);
        // Empty caret: still close (not a selection).
        st.sel = Some(((0, 0), (0, 0)));
        assert_eq!(esc_action(&st), EscAction::Close);
        // Selection: clear it first.
        st.lines = vec![toy_line("abcd")];
        st.sel = Some(((0, 0), (0, 4)));
        assert_eq!(esc_action(&st), EscAction::ClearSelection);
        // In-flight job beats selection.
        st.translating = true;
        assert_eq!(esc_action(&st), EscAction::CancelTranslation);
        st.translating = false;
        // Painted subtitles beat selection.
        st.translated = Some(Translated { regions: Vec::new() });
        st.tmode = TMode::Partial;
        assert_eq!(esc_action(&st), EscAction::CancelTranslation);
    }

    #[test]
    fn external_tags_map_to_bcp47() {
        assert_eq!(external_lang_tag("zh"), "zh-CN");
        assert_eq!(external_lang_tag("zh_hant"), "zh-TW");
        assert_eq!(external_lang_tag("en"), "en");
        assert_eq!(external_lang_tag("ja"), "ja");
    }

    #[test]
    fn url_host_extracts() {
        assert_eq!(url_host("https://translate.google.com/?sl=auto&text=x"), "translate.google.com");
        assert_eq!(url_host("https://www.bing.com/search?q=x"), "www.bing.com");
        assert_eq!(url_host("https://host:8080/p?q=x"), "host:8080");
        assert_eq!(url_host("https://host?q=x"), "host");
        assert_eq!(url_host("plain-host/path"), "plain-host");
    }

    #[test]
    fn fallback_layout_buttons_fit_client() {
        // The old fixed 215px frame clipped Yes/No (client was ~185px).
        for body_h in [8, 32, 60, 92, 150, 300] {
            let (w, h, check_y, btn_y) = fallback_layout(body_h);
            assert_eq!(w, 460);
            assert!(check_y >= 12 + body_h.max(32) + 10, "check below body {body_h}");
            assert!(btn_y >= check_y + 24, "buttons below checkbox");
            assert!(h >= btn_y + 30 + 16, "buttons inside client: h={h} btn_y={btn_y}");
        }
    }

    #[test]
    fn translate_key_roundtrips() {
        // Replace in place, comments survive.
        let src = "# comment\n[translate]\nsource = \"bergamot\"\n";
        let out = set_translate_key(src, "external_fallback", "true");
        assert!(out.contains("external_fallback = true"));
        assert!(out.contains("# comment"));
        assert!(out.contains("source = \"bergamot\""));
        // Second write replaces, never duplicates.
        let out2 = set_translate_key(&out, "external_fallback", "false");
        assert_eq!(out2.matches("external_fallback =").count(), 1);
        // Insert into an existing section (after the header).
        let out3 = set_translate_key("[translate]\nsource = \"x\"\n[search]\nengine = \"y\"\n", "external_fallback_ask", "false");
        assert!(out3.contains("[translate]\nexternal_fallback_ask = false\nsource = \"x\""));
        // Commented key does not count as present.
        let out4 = set_translate_key("[translate]\n#external_fallback = true\n", "external_fallback", "true");
        assert_eq!(out4.matches("external_fallback =").count(), 2); // comment + real
        assert!(out4.contains("\nexternal_fallback = true\n"));
        // Missing file/section: created.
        assert_eq!(set_translate_key("", "external_fallback", "true"), "[translate]\nexternal_fallback = true\n");
        // Prefix trap: external_fallback must not match external_fallback_ask.
        let out5 = set_translate_key("[translate]\nexternal_fallback_ask = true\n", "external_fallback", "true");
        assert!(out5.contains("external_fallback_ask = true"));
        assert!(out5.contains("\nexternal_fallback = true\n"));
    }

    #[test]
    fn partial_regions_hug_selection() {
        // Subtitles never exceed the blue box: mid-line start/end across two
        // lines yields per-line regions with sliced text; the gap stays bare.
        let mut st = toy_state();
        st.lines = vec![toy_line_at("abcd", 0.0), toy_line_at("efgh", 100.0)];
        st.sel = Some(((0, 1), (1, 3)));
        let regs = build_regions(&st, TMode::Partial);
        assert_eq!(regs.len(), 2);
        assert_eq!(regs[0].text, "bcd");
        assert_eq!(regs[1].text, "efg");
        assert_eq!((regs[0].rect.0, regs[0].rect.2), (25, 100));
        assert_eq!((regs[1].rect.0, regs[1].rect.2), (0, 75));
        assert!(regs[0].rect.3 <= 20, "must not reach into the gap");
        assert!(regs[1].rect.1 >= 100, "must not reach into the gap");
    }

    #[test]
    fn partial_splits_multiline_blocks() {
        // Same block, different granularity: full sends one block region
        // (context), partial sends one region per selected line (precision).
        let mut st = toy_state();
        st.lines = vec![toy_line_at("ab", 0.0), toy_line_at("cd", 25.0)];
        st.sel = Some(((0, 0), (1, 2)));
        let full = build_regions(&st, TMode::Full);
        assert_eq!(full.len(), 1);
        let partial = build_regions(&st, TMode::Partial);
        assert_eq!(partial.len(), 2);
        assert_eq!(partial[0].text, "ab");
        assert_eq!(partial[1].text, "cd");
    }

    #[test]
    fn partial_regions_follow_blocks() {
        // Refined partial: select-all converges to the full layout (one
        // region per intersected block), single-box keeps one region.
        let mut st = toy_state();
        st.lines = vec![toy_line_at("hello", 0.0), toy_line_at("world", 100.0)];
        st.sel = Some(((0, 0), (1, 5)));
        let partial = build_regions(&st, TMode::Partial);
        let full = build_regions(&st, TMode::Full);
        assert_eq!(partial.len(), 2);
        assert_eq!(full.len(), 2);
        assert_eq!(partial[0].text, full[0].text);
        assert_eq!(partial[1].text, full[1].text);
        st.sel = Some(((0, 0), (0, 5)));
        let one = build_regions(&st, TMode::Partial);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].text, "hello");
    }

    #[test]
    fn translate_busy_unifies_button_and_menu() {
        // One predicate drives both affordances: idle -> work, busy -> cancel.
        let mut st = toy_state();
        assert!(!tr_busy(&st));
        st.translating = true;
        assert!(tr_busy(&st), "in-flight job must offer cancel");
        st.translating = false;
        st.tmode = TMode::Partial;
        assert!(tr_busy(&st), "painted subtitles must offer cancel");
        st.tmode = TMode::Full;
        assert!(tr_busy(&st));
    }

    #[test]
    fn cancel_translation_resets_everything() {
        let mut st = toy_state();
        st.translated = Some(Translated { regions: Vec::new() });
        st.pending = Some(PendingT { regions: Vec::new(), mode: TMode::Full, chain: Vec::new() });
        st.translating = true;
        st.tmode = TMode::Full;
        cancel_translation(&mut st);
        assert!(st.translated.is_none());
        assert!(st.pending.is_none());
        assert!(!st.translating);
        assert_eq!(st.tmode, TMode::Idle);
        assert!(!tr_busy(&st), "cancel returns to the translate affordance");
    }

    #[test]
    fn selection_paints_blue_box() {
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, FillRect, GetDC,
            SelectObject,
        };
        unsafe {
            let screen = GetDC(Some(NULL_HWND));
            let mem = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, 400, 300);
            let old = SelectObject(mem, bmp.into());
            let white = CreateSolidBrush(COLORREF(0xFFFFFF));
            let rc = RECT { left: 0, top: 0, right: 400, bottom: 300 };
            FillRect(mem, &rc, white);
            let _ = DeleteObject(white.into());

            // Selection blue 0xAD4600 is 0xBBGGRR: B173 G70 R0.
            let mut st = toy_state();
            st.lines = vec![toy_line("abcd")];
            st.sel = Some(((0, 0), (0, 4)));
            paint_selection(mem, &mut st);

            let (buf, img) = readback_400x300(mem, bmp);
            let _ = img.save("temp/selection.png");
            // Toy quad is (0,0,100,20): blue must exist inside, white outside.
            let (mut blue_in, mut blue_out) = (0, 0);
            for y in 0..300 {
                for x in 0..400 {
                    let o = (y * 400 + x) * 4;
                    let (b, g, r) = (buf[o], buf[o + 1], buf[o + 2]);
                    let is_blue = b > 100 && g < 120 && r < 60;
                    if !is_blue {
                        continue;
                    }
                    if x < 100 && y < 20 {
                        blue_in += 1;
                    } else {
                        blue_out += 1;
                    }
                }
            }
            eprintln!("selection pixels: blue_in={blue_in} blue_out={blue_out}");
            assert!(blue_in > 500, "selection box did not paint blue");
            assert!(blue_out == 0, "selection leaked outside its box");
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            let _ = windows::Win32::Graphics::Gdi::ReleaseDC(Some(NULL_HWND), screen);
        }
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
        // Regression: fade curve must be monotonic, capped at 220, pinned
        // at key points (opaque above 1s, ~half at 500ms, 0 at expiry).
        assert_eq!(toast_fade_alpha(3000), 220);
        assert_eq!(toast_fade_alpha(1001), 220);
        assert_eq!(toast_fade_alpha(1000), 220);
        assert_eq!(toast_fade_alpha(500), 110);
        assert_eq!(toast_fade_alpha(1), 0);
        assert_eq!(toast_fade_alpha(0), 0);
        let mut prev = 0u8;
        for ms in (0..=3000u128).step_by(37) {
            let a = toast_fade_alpha(ms);
            assert!(a <= 220);
            assert!(a >= prev, "fade must be monotonic in remaining time");
            prev = a;
        }
        // Strict monotonic spot-checks across the fade second.
        assert!(toast_fade_alpha(900) > toast_fade_alpha(500));
        assert!(toast_fade_alpha(500) > toast_fade_alpha(100));
    }

    /// Offscreen helper: 400x300 white canvas, read back as RGBA buf + RgbImage.
    #[cfg(test)]
    fn readback_400x300(
        mem: windows::Win32::Graphics::Gdi::HDC,
        bmp: windows::Win32::Graphics::Gdi::HBITMAP,
    ) -> (Vec<u8>, image::RgbImage) {
        use windows::Win32::Graphics::Gdi::{
            GetDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        };
        unsafe {
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = 400;
            bmi.bmiHeader.biHeight = -300;
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0 as u32;
            let mut buf = vec![0u8; 400 * 300 * 4];
            assert!(
                GetDIBits(mem, bmp, 0, 300, Some(buf.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS)
                    > 0
            );
            let mut img = image::RgbImage::new(400, 300);
            for (i, p) in img.pixels_mut().enumerate() {
                let o = i * 4;
                p.0 = [buf[o + 2], buf[o + 1], buf[o]];
            }
            (buf, img)
        }
    }

    #[test]
    fn download_panel_paints() {
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, FillRect, GetDC,
            SelectObject,
        };
        unsafe {
            let screen = GetDC(Some(NULL_HWND));
            let mem = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, 400, 300);
            let old = SelectObject(mem, bmp.into());
            let white = CreateSolidBrush(COLORREF(0xFFFFFF));
            let rc = RECT { left: 0, top: 0, right: 400, bottom: 300 };
            FillRect(mem, &rc, white);
            let _ = DeleteObject(white.into());

            let mut st = toy_state();
            st.focus = crate::monitors::Mon { l: 0, t: 0, r: 400, b: 300, primary: true };
            let (_tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
            st.dl = Some(DlState {
                label: "jaen+enzh".to_string(),
                prog: std::sync::Arc::new(std::sync::Mutex::new((1usize, 4usize, 512u64, Some(1024u64)))),
                cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                done_rx: rx,
                cancel_rect: (0, 0, 0, 0),
            });
            paint_download(mem, &mut st);

            let (buf, img) = readback_400x300(mem, bmp);
            let _ = img.save("temp/dl-panel.png");
            // Panel bg 0x141414 (dark) + bar fill 0x2E7AD6 (R214 G122 B46).
            // frac = (1 + 512/1024) / 4 = 0.375 -> ~105x22 blue px.
            let (mut dark, mut blue) = (0, 0);
            for px in buf.chunks_exact(4) {
                let lum = px[0] as u32 + px[1] as u32 + px[2] as u32;
                if lum < 120 {
                    dark += 1;
                }
                if px[2] > 150 && (px[2] as i32) > (px[0] as i32) + 50 {
                    blue += 1;
                }
            }
            eprintln!("dl panel pixels: dark={dark} blue={blue}");
            assert!(dark > 5000, "panel did not paint dark");
            assert!(blue > 500, "progress fill missing");
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            let _ = windows::Win32::Graphics::Gdi::ReleaseDC(Some(NULL_HWND), screen);
        }
    }

    #[test]
    fn translated_subtitle_covers_region() {
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, FillRect, GetDC,
            SelectObject,
        };
        unsafe {
            let screen = GetDC(Some(NULL_HWND));
            let mem = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, 400, 300);
            let old = SelectObject(mem, bmp.into());
            let white = CreateSolidBrush(COLORREF(0xFFFFFF));
            let rc = RECT { left: 0, top: 0, right: 400, bottom: 300 };
            FillRect(mem, &rc, white);
            let _ = DeleteObject(white.into());

            // Subtitle bg 0x1E3C1E (R30 G60 B30) over rect, white CJK text.
            let mut st = toy_state();
            let tr = Translated {
                regions: vec![TransRegion {
                    rect: (50, 60, 250, 120),
                    font_px: 20,
                    text: "你好世界".to_string(),
                }],
            };
            paint_translated(mem, &mut st, &tr);

            let (buf, img) = readback_400x300(mem, bmp);
            let _ = img.save("temp/subtitle.png");
            let (mut green_in, mut white_in, mut white_out) = (0, 0, 0);
            for y in 0..300 {
                for x in 0..400 {
                    let o = (y * 400 + x) * 4;
                    let (b, g, r) = (buf[o], buf[o + 1], buf[o + 2]);
                    let lum = b as u32 + g as u32 + r as u32;
                    let inside = x >= 50 && x < 250 && y >= 60 && y < 120;
                    if inside {
                        // Greenish panel (G dominant, dark).
                        if g > r && g > b && g < 150 {
                            green_in += 1;
                        }
                        if lum > 700 {
                            white_in += 1;
                        }
                    } else if lum < 700 {
                        white_out += 1;
                    }
                }
            }
            eprintln!("subtitle pixels: green_in={green_in} white_in={white_in} nonwhite_out={white_out}");
            assert!(green_in > 3000, "subtitle panel did not cover its region");
            assert!(white_in > 20, "subtitle text missing");
            assert!(white_out == 0, "paint leaked outside the region");
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            let _ = windows::Win32::Graphics::Gdi::ReleaseDC(Some(NULL_HWND), screen);
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
