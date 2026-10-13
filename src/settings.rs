// Settings dialog: every config.toml key editable in-GUI (DESIGN §12.12).
// One implementation, two entries (tray menu + overlay menu). Reuses the
// fallback-dialog patterns: system message font, measured frame, modal loop.
//
// Hotkey capture records WM_KEYDOWN directly (no hook); names come from
// GetKeyNameTextW (locale-aware), unknown VKs stay 0xNN. Only combos
// RegisterHotKey can take are accepted; system-reserved ones are refused
// with a message. The dialog only writes the file; applying (re-register,
// cfg reload) belongs to the caller, reported via SettingsOutcome.
use std::ptr::null_mut;

use anyhow::Result;
use windows::{
    core::{w, HSTRING, PCWSTR},
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{CreateSolidBrush, DeleteObject, HFONT},
        UI::{
            Input::KeyboardAndMouse::{EnableWindow, SetFocus, VK_ESCAPE},
            WindowsAndMessaging::{
                AdjustWindowRectEx, AppendMenuW, BN_CLICKED, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON,
                BM_GETCHECK, BM_SETCHECK, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, CBS_DROPDOWNLIST,
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                GetMessageW, GetWindowLongPtrW, GetWindowTextW, IsDialogMessageW, LoadCursorW, MessageBoxW, MB_OK,
                MB_ICONEXCLAMATION, RegisterClassW, SendMessageW, SetForegroundWindow,
                SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage,
                GWLP_ID, GWLP_USERDATA, HMENU, IDCANCEL, IDOK, MENU_ITEM_FLAGS,
                MSG, SW_SHOWNORMAL, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_KEYDOWN,
                WM_NCCREATE, WM_SETFONT, WNDCLASSW, WNDCLASS_STYLES, WS_BORDER, WS_CAPTION, WS_CHILD,
                WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
                WS_EX_DLGMODALFRAME, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, ES_AUTOHSCROLL,
            },
        },
    },
};

use crate::{
    config::FileConfig,
    hotkey,
    lang::{resolve_ui_lang, tr},
    search, translate,
};

// ---------------- shared dialog helpers (also used by overlay) ----------------

/// Nul-terminated UTF-16 for one synchronous Win32 call.
pub(crate) fn widen(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// System message font: the same source Notepad/dialogs use. Returns the
/// font plus whether the caller owns it (stock objects must not be deleted).
/// Never fails: stock font fallback.
pub(crate) fn system_message_font() -> (HFONT, bool) {
    use windows::Win32::Graphics::Gdi::{CreateFontIndirectW, GetStockObject, DEFAULT_GUI_FONT};
    use windows::Win32::UI::WindowsAndMessaging::{
        NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        SystemParametersInfoW,
    };
    unsafe {
        let mut ncm: NONCLIENTMETRICSW = std::mem::zeroed();
        ncm.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
        let ok = SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            Some(&mut ncm as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok();
        if ok {
            let f = CreateFontIndirectW(&ncm.lfMessageFont);
            if !f.is_invalid() {
                return (f, true);
            }
        }
        (HFONT(GetStockObject(DEFAULT_GUI_FONT).0), false)
    }
}

/// Wrapped height of `text` at `text_w` px in `font` (screen DC, discarded).
pub(crate) fn measure_text_h(font: HFONT, text: &str, text_w: i32) -> i32 {
    use windows::Win32::Graphics::Gdi::{
        DrawTextW, GetDC, ReleaseDC, SelectObject, DT_CALCRECT, DT_LEFT, DT_NOPREFIX, DT_WORDBREAK,
    };
    use windows::Win32::Foundation::HWND as RawHwnd;
    unsafe {
        let dc = GetDC(Some(RawHwnd(null_mut())));
        let old = SelectObject(dc, font.into());
        let mut txt: Vec<u16> = text.encode_utf16().collect();
        let mut rc = RECT { left: 0, top: 0, right: text_w, bottom: 0 };
        DrawTextW(dc, &mut txt, &mut rc, DT_CALCRECT | DT_WORDBREAK | DT_LEFT | DT_NOPREFIX);
        SelectObject(dc, old);
        let _ = ReleaseDC(Some(RawHwnd(null_mut())), dc);
        (rc.bottom - rc.top).max(40)
    }
}

/// Where AppConfig::load reads from (exe dir first, then cwd); the same file
/// is the write target so saved settings actually reload.
pub(crate) fn config_write_path() -> std::path::PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()));
    if let Some(dir) = &exe_dir {
        if dir.join("config.toml").is_file() {
            return dir.join("config.toml");
        }
    }
    if std::path::PathBuf::from("config.toml").is_file() {
        return std::path::PathBuf::from("config.toml");
    }
    exe_dir.unwrap_or_else(|| std::path::PathBuf::from(".")).join("config.toml")
}

/// Append a menu item with a runtime (translated) string. The wide buffer
/// only needs to outlive the synchronous call.
pub(crate) fn append_item(menu: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: &str) {
    unsafe {
        let w = widen(text);
        let _ = AppendMenuW(menu, flags, id, PCWSTR(w.as_ptr()));
    }
}

// ---------------- line-preserving writer (generalized) ----------------

/// Set `key = value` under `[section]`, preserving everything else
/// (comments, order, other sections). Creates the file/section when missing.
/// Line-based on purpose: no new deps, comments survive byte-for-byte.
/// `value` is pre-formatted TOML (bare `true`, quoted `"x"`, `["a","b"]`).
pub fn set_section_key(src: &str, section: &str, key: &str, value: &str) -> String {
    let nl = if src.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> =
        if src.is_empty() { Vec::new() } else { src.split('\n').map(str::to_string).collect() };
    if lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    let header = format!("[{section}]");
    let is_header = |l: &str| l.trim().starts_with('[');
    let is_key = |l: &str| {
        let t = l.trim_start();
        !t.starts_with('#') && t.starts_with(key) && t[key.len()..].trim_start().starts_with('=')
    };
    let hdr = lines.iter().position(|l| l.trim() == header);
    match hdr {
        Some(h) => {
            let end =
                lines.iter().skip(h + 1).position(|l| is_header(l)).map(|i| h + 1 + i).unwrap_or(lines.len());
            if let Some(pos) = lines[h + 1..end].iter().position(|l| is_key(l)) {
                lines[h + 1 + pos] = format!("{key} = {value}");
            } else {
                lines.insert(h + 1, format!("{key} = {value}"));
            }
        }
        None => {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(header);
            lines.push(format!("{key} = {value}"));
        }
    }
    lines.join(nl) + nl
}

/// TOML double-quoted string (escapes `\` and `"`; values are single-line).
pub fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn toml_str_array(items: &[String]) -> String {
    format!("[{}]", items.iter().map(|s| toml_string(s)).collect::<Vec<_>>().join(", "))
}

// ---------------- validation (pure, unit-tested) ----------------

pub fn validate_batch(s: &str) -> Result<usize, &'static str> {
    let n: usize = s.trim().parse().map_err(|_| "bad_number")?;
    if (1..=64).contains(&n) {
        Ok(n)
    } else {
        Err("bad_number")
    }
}

pub fn validate_cascade(s: &str) -> Result<Vec<String>, &'static str> {
    let tiers: Vec<String> = s.split(',').map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()).collect();
    if tiers.is_empty() || tiers.iter().any(|t| !matches!(t.as_str(), "tiny" | "small" | "medium")) {
        return Err("bad_choice");
    }
    Ok(tiers)
}

pub fn validate_model(s: &str) -> Result<Option<String>, &'static str> {
    let t = s.trim().to_lowercase();
    if t.is_empty() {
        return Ok(None);
    }
    if matches!(t.as_str(), "tiny" | "small" | "medium") {
        return Ok(Some(t));
    }
    Err("bad_choice")
}

/// "auto" or a baked registry code (case-insensitive) -> canonical lowercase.
pub fn validate_lang_code(s: &str) -> Result<String, &'static str> {
    let t = s.trim().to_lowercase();
    if t == "auto" || t.is_empty() {
        return Ok("auto".to_string());
    }
    if translate::TRANSLATE_LANGS.iter().any(|c| *c == t) {
        return Ok(t);
    }
    Err("bad_choice")
}

/// Non-empty http(s) URL containing `{text}` (+ `{tgt}` for translators).
pub fn validate_url_template(s: &str, need_tgt: bool) -> Result<String, &'static str> {
    let t = s.trim().to_string();
    if !(t.starts_with("http://") || t.starts_with("https://")) || !t.contains("{text}") {
        return Err("need_http_text");
    }
    if need_tgt && !t.contains("{tgt}") {
        return Err("need_tgt");
    }
    Ok(t)
}

pub fn validate_ui_lang(s: &str) -> Result<String, &'static str> {
    match s.trim().to_lowercase().as_str() {
        "auto" | "" => Ok("auto".to_string()),
        "zh" => Ok("zh".to_string()),
        "en" => Ok("en".to_string()),
        _ => Err("bad_choice"),
    }
}

pub fn validate_engine(s: &str) -> Result<String, &'static str> {
    let t = s.trim().to_lowercase();
    if search::preset_url(&t).is_some() {
        return Ok(t);
    }
    Err("bad_choice")
}

// ---------------- outcome ----------------

/// What changed (caller applies: tray re-registers the hotkey live and
/// reloads cfg; anything flagged here that cannot apply live needs restart).
#[derive(Debug, Default)]
pub struct SettingsOutcome {
    pub saved: bool,
    pub hotkey_changed: bool,
    pub new_combo: String,
    pub ui_lang_changed: bool,
    pub ocr_changed: bool,
    pub needs_restart: bool,
}

/// Dev harness: `--settings-test` shows the dialog standalone (no tray,
/// no overlay), prints the outcome, exits.
pub fn run_settings_test() -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let o = run_settings(HWND(null_mut()), (w / 2, h / 2), None);
    println!(
        "settings-test: saved={} hotkey_changed={} new_combo={} ui_lang_changed={} ocr_changed={} needs_restart={}",
        o.saved, o.hotkey_changed, o.new_combo, o.ui_lang_changed, o.ocr_changed, o.needs_restart
    );
    Ok(())
}

// ---------------- dialog ----------------

const CHANGE_ID: isize = 4101;
const ROW_H: i32 = 30;
const EDIT_H: i32 = 24;
const MARGIN: i32 = 16;
const LABEL_W: i32 = 150;
const CLIENT_W: i32 = 560;

struct Ctrls {
    combo_source: HWND,
    combo_src_lang: HWND,
    combo_tgt_lang: HWND,
    edit_ext_url: HWND,
    check_auto_exit: HWND,
    check_fallback: HWND,
    check_fallback_ask: HWND,
    combo_engine: HWND,
    edit_engine_url: HWND,
    edit_save_dir: HWND,
    edit_ocr_model: HWND,
    edit_cascade: HWND,
    edit_batch: HWND,
    check_directml: HWND,
    combo_ui: HWND,
    hotkey_text: HWND,
    hotkey_hint: HWND,
    change_btn: HWND,
    ok_btn: HWND,
    src_langs: Vec<String>,
    tgt_langs: Vec<String>,
    engines: Vec<String>,
}

struct Dlg {
    ctrls: Ctrls,
    ui: &'static str,
    capturing: bool,
    pending_hotkey: Option<hotkey::Hotkey>,
    orig_combo: String,
    orig_ui_lang: String,
    orig_ocr: (String, String, usize, bool),
    hotkey_owner: Option<HWND>,
    out: SettingsOutcome,
    done: bool,
}

struct Ctor {
    dlg: HWND,
    y: i32,
    fonted: Vec<HWND>,
}

impl Ctor {
    fn label(&mut self, text: &str) {
        unsafe {
            let h = CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                PCWSTR(widen(text).as_ptr()),
                WS_CHILD | WS_VISIBLE,
                MARGIN, self.y + 3, LABEL_W, EDIT_H,
                Some(self.dlg), None, None, None,
            )
            .unwrap_or_default();
            self.fonted.push(h);
        }
    }

    fn header(&mut self, text: &str) {
        unsafe {
            let h = CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                PCWSTR(widen(text).as_ptr()),
                WS_CHILD | WS_VISIBLE,
                MARGIN, self.y + 3, CLIENT_W - MARGIN * 2, EDIT_H,
                Some(self.dlg), None, None, None,
            )
            .unwrap_or_default();
            self.fonted.push(h);
            self.y += ROW_H;
        }
    }

    fn combo(&mut self, items: &[String], sel: usize) -> HWND {
        unsafe {
            let h = CreateWindowExW(
                Default::default(),
                w!("COMBOBOX"),
                w!(""),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
                MARGIN + LABEL_W, self.y, CLIENT_W - MARGIN * 2 - LABEL_W, 200,
                Some(self.dlg), None, None, None,
            )
            .unwrap_or_default();
            add_strings(h, items);
            select_combo(h, sel);
            self.fonted.push(h);
            self.y += ROW_H;
            h
        }
    }

    fn edit(&mut self, initial: &str) -> HWND {
        unsafe {
            let h = CreateWindowExW(
                Default::default(),
                w!("EDIT"),
                PCWSTR(widen(initial).as_ptr()),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                MARGIN + LABEL_W, self.y, CLIENT_W - MARGIN * 2 - LABEL_W, EDIT_H,
                Some(self.dlg), None, None, None,
            )
            .unwrap_or_default();
            self.fonted.push(h);
            self.y += ROW_H;
            h
        }
    }

    fn check(&mut self, text: &str, on: bool) -> HWND {
        unsafe {
            let h = CreateWindowExW(
                Default::default(),
                w!("BUTTON"),
                PCWSTR(widen(text).as_ptr()),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
                MARGIN + LABEL_W, self.y, CLIENT_W - MARGIN * 2 - LABEL_W, EDIT_H,
                Some(self.dlg), None, None, None,
            )
            .unwrap_or_default();
            if on {
                SendMessageW(h, BM_SETCHECK, Some(WPARAM(1)), None);
            }
            self.fonted.push(h);
            self.y += ROW_H;
            h
        }
    }
}

fn msg_box(owner: HWND, ui: &'static str, body_key: &str, detail: &str) {
    unsafe {
        let b = widen(&format!("{}\n\n{detail}", tr(ui, body_key)));
        let t = widen(&tr(ui, "settings.err_title"));
        MessageBoxW(Some(owner), PCWSTR(b.as_ptr()), PCWSTR(t.as_ptr()), MB_OK | MB_ICONEXCLAMATION);
    }
}

fn err_box(dlg: HWND, ui: &'static str, key: &str, detail: String) -> bool {
    msg_box(dlg, ui, key, &detail);
    false
}

fn edit_text(hwnd: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 1024];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize]).trim().to_string()
    }
}

fn combo_index(hwnd: HWND) -> usize {
    unsafe {
        let r = SendMessageW(hwnd, CB_GETCURSEL, None, None);
        if r.0 < 0 {
            0
        } else {
            r.0 as usize
        }
    }
}

fn set_static(hwnd: HWND, text: &str) {
    unsafe {
        let _ = SetWindowTextW(hwnd, &HSTRING::from(text));
    }
}

fn add_strings(hwnd: HWND, items: &[String]) {
    unsafe {
        for it in items {
            let w = widen(it);
            SendMessageW(hwnd, CB_ADDSTRING, None, Some(LPARAM(w.as_ptr() as isize)));
        }
    }
}

fn select_combo(hwnd: HWND, idx: usize) {
    unsafe {
        SendMessageW(hwnd, CB_SETCURSEL, Some(WPARAM(idx)), None);
    }
}

fn is_checked(hwnd: HWND) -> bool {
    unsafe { SendMessageW(hwnd, BM_GETCHECK, None, None).0 == 1 }
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
        WM_KEYDOWN if d.capturing => {
            // Capture mode owns every key (the modal loop bypasses
            // IsDialogMessageW for WM_KEYDOWN while capturing).
            let vk = wp.0 as u32;
            let mods = hotkey::current_modifiers();
            const IGNORE: [u32; 8] = [0x10, 0x11, 0x12, 0x5B, 0x5C, 0x14, 0x90, 0x91];
            if IGNORE.contains(&vk) {
                return LRESULT(0); // lone modifier/lock: keep waiting.
            }
            if vk == VK_ESCAPE.0 as u32 && mods == 0 {
                end_capture(d, false); // bare Esc cancels capture.
                return LRESULT(0);
            }
            let cand = hotkey::Hotkey { mods, vk };
            if hotkey::is_reserved_combo(&cand) {
                set_static(d.ctrls.hotkey_hint, &tr(d.ui, "settings.reserved"));
            } else if !hotkey::probe_combo(hwnd, &cand) {
                set_static(d.ctrls.hotkey_hint, &tr(d.ui, "settings.taken"));
            } else {
                d.pending_hotkey = Some(cand);
                end_capture(d, true);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xffff) as usize;
            let code = ((wp.0 >> 16) & 0xffff) as u32;
            if code == BN_CLICKED && id as isize == CHANGE_ID {
                d.capturing = true;
                set_static(d.ctrls.hotkey_hint, &tr(d.ui, "settings.press_key"));
                let _ = SetFocus(Some(hwnd));
                LRESULT(0)
            } else if code == BN_CLICKED && id == IDOK.0 as usize {
                if d.capturing {
                    end_capture(d, false); // OK while capturing: stop capture, stay open.
                } else if apply_settings(hwnd, d) {
                    d.out.saved = true;
                    d.done = true;
                }
                LRESULT(0)
            } else if code == BN_CLICKED && id == IDCANCEL.0 as usize {
                if d.capturing {
                    end_capture(d, false); // Esc-equivalent: stop capture, stay open.
                } else {
                    d.done = true;
                }
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
        WM_CLOSE => {
            d.done = true;
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn end_capture(d: &mut Dlg, kept: bool) {
    d.capturing = false;
    let label = match (kept, &d.pending_hotkey) {
        (true, Some(h)) | (false, Some(h)) => hotkey::combo_display(h),
        _ => hotkey::combo_display(&hotkey::parse_combo(&d.orig_combo).unwrap_or_else(hotkey::default_hotkey)),
    };
    set_static(d.ctrls.hotkey_text, &label);
    set_static(d.ctrls.hotkey_hint, &tr(d.ui, "settings.hotkey_hint"));
}

/// Validate + write the file. False = error shown, dialog stays open.
fn apply_settings(dlg: HWND, d: &mut Dlg) -> bool {
    let c = &d.ctrls;
    // Combos resolve through their parallel value lists (index 0 fallback).
    let sources = ["bergamot".to_string(), "external".to_string(), "online".to_string()];
    let ui_modes = ["auto".to_string(), "zh".to_string(), "en".to_string()];
    let source_raw = sources.get(combo_index(c.combo_source)).cloned().unwrap_or_default();
    let source = crate::config::normalize_source_mode(&source_raw).to_string();
    let src_lang = c.src_langs.get(combo_index(c.combo_src_lang)).cloned().unwrap_or_default();
    let tgt_lang = c.tgt_langs.get(combo_index(c.combo_tgt_lang)).cloned().unwrap_or_default();
    let engine = c.engines.get(combo_index(c.combo_engine)).cloned().unwrap_or_default();
    let ui_lang = ui_modes.get(combo_index(c.combo_ui)).cloned().unwrap_or_default();

    let src_lang = match validate_lang_code(&src_lang) {
        Ok(v) => v,
        Err(_) => return err_box(dlg, d.ui, "settings.invalid", format!("{}: {src_lang}", tr(d.ui, "settings.srclang"))),
    };
    let tgt_lang = match validate_lang_code(&tgt_lang) {
        Ok(v) => v,
        Err(_) => return err_box(dlg, d.ui, "settings.invalid", format!("{}: {tgt_lang}", tr(d.ui, "settings.tgtlang"))),
    };
    let ext_url = match validate_url_template(&edit_text(c.edit_ext_url), true) {
        Ok(v) => v,
        Err(k) => return err_box(dlg, d.ui, "settings.invalid", format!("{}: {}", tr(d.ui, "settings.exturl"), tr(d.ui, k))),
    };
    let engine_url_raw = edit_text(c.edit_engine_url);
    let engine_url = if engine_url_raw.is_empty() {
        String::new()
    } else {
        match validate_url_template(&engine_url_raw, false) {
            Ok(v) => v,
            Err(k) => return err_box(dlg, d.ui, "settings.invalid", format!("{}: {}", tr(d.ui, "settings.engineurl"), tr(d.ui, k))),
        }
    };
    let engine = match validate_engine(&engine) {
        Ok(v) => v,
        Err(_) => return err_box(dlg, d.ui, "settings.invalid", format!("{}: {engine}", tr(d.ui, "settings.engine"))),
    };
    let ui_lang = match validate_ui_lang(&ui_lang) {
        Ok(v) => v,
        Err(_) => return err_box(dlg, d.ui, "settings.invalid", ui_lang.clone()),
    };
    let batch = match validate_batch(&edit_text(c.edit_batch)) {
        Ok(v) => v,
        Err(_) => {
            return err_box(dlg, d.ui, "settings.invalid", format!("{}: 1-64", tr(d.ui, "settings.batch")));
        }
    };
    let cascade = match validate_cascade(&edit_text(c.edit_cascade)) {
        Ok(v) => v,
        Err(_) => return err_box(dlg, d.ui, "settings.invalid", format!("{}: tiny,small,medium", tr(d.ui, "settings.cascade"))),
    };
    let model = match validate_model(&edit_text(c.edit_ocr_model)) {
        Ok(v) => v,
        Err(_) => return err_box(dlg, d.ui, "settings.invalid", format!("{}: tiny|small|medium", tr(d.ui, "settings.model"))),
    };
    let save_dir = edit_text(c.edit_save_dir);
    let auto_exit = is_checked(c.check_auto_exit);
    let fallback = is_checked(c.check_fallback);
    let fallback_ask = is_checked(c.check_fallback_ask);
    let directml = is_checked(c.check_directml);
    let new_combo = match &d.pending_hotkey {
        Some(h) => hotkey::format_combo(h),
        None => d.orig_combo.clone(),
    };

    // One atomic rewrite of every managed key (comments preserved).
    let path = config_write_path();
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return err_box(dlg, d.ui, "settings.invalid", path.display().to_string());
        }
    }
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    let put = |t: String, sec: &str, key: &str, val: &str| set_section_key(&t, sec, key, val);
    text = put(text, "hotkey", "combos", &toml_str_array(&[new_combo.clone()]));
    match &model {
        // model=None means "auto cascade": drop the key, never write "".
        Some(m) => text = put(text, "ocr", "model", &toml_string(m)),
        None => text = remove_key(&text, "ocr", "model"),
    }
    text = put(text, "ocr", "cascade", &toml_str_array(&cascade));
    text = put(text, "ocr", "rec_batch_size", &batch.to_string());
    text = put(text, "ocr", "use_directml", if directml { "true" } else { "false" });
    text = put(text, "translate", "source", &toml_string(&source));
    text = put(text, "translate", "source_lang", &toml_string(&src_lang));
    text = put(text, "translate", "target_lang", &toml_string(&tgt_lang));
    text = put(text, "translate", "external_url", &toml_string(&ext_url));
    text = put(text, "translate", "external_auto_exit", if auto_exit { "true" } else { "false" });
    text = put(text, "translate", "external_fallback", if fallback { "true" } else { "false" });
    text = put(text, "translate", "external_fallback_ask", if fallback_ask { "true" } else { "false" });
    text = put(text, "search", "engine", &toml_string(&engine));
    if engine_url.is_empty() {
        text = remove_key(&text, "search", "engine_url");
    } else {
        text = put(text, "search", "engine_url", &toml_string(&engine_url));
    }
    if save_dir.is_empty() {
        text = remove_key(&text, "save", "dir");
    } else {
        text = put(text, "save", "dir", &toml_string(&save_dir));
    }
    text = put(text, "ui", "lang", &toml_string(&ui_lang));
    if std::fs::write(&path, text).is_err() {
        return err_box(dlg, d.ui, "settings.invalid", path.display().to_string());
    }

    d.out.hotkey_changed = new_combo != d.orig_combo;
    d.out.new_combo = new_combo;
    d.out.ui_lang_changed = ui_lang != d.orig_ui_lang;
    // Normalized compare (raw edits like "06" or "Medium" must not flag).
    let ocr_now = (
        model.unwrap_or_default(),
        cascade.join(","),
        batch,
        directml,
    );
    d.out.ocr_changed = ocr_now != d.orig_ocr;
    d.out.needs_restart = d.out.ocr_changed || d.out.ui_lang_changed || (d.out.hotkey_changed && d.hotkey_owner.is_none());
    true
}

/// Drop one key line from a section (for "empty = unset" optionals).
fn remove_key(src: &str, section: &str, key: &str) -> String {
    let header = format!("[{section}]");
    let mut lines: Vec<String> = src.split('\n').map(str::to_string).collect();
    if let Some(h) = lines.iter().position(|l| l.trim() == header) {
        let end =
            lines.iter().skip(h + 1).position(|l| l.trim().starts_with('[')).map(|i| h + 1 + i).unwrap_or(lines.len());
        if let Some(pos) = lines[h + 1..end]
            .iter()
            .position(|l| {
                let t = l.trim_start();
                !t.starts_with('#') && t.starts_with(key) && t[key.len()..].trim_start().starts_with('=')
            })
        {
            lines.remove(h + 1 + pos);
        }
    }
    lines.join("\n")
}

/// Modal settings dialog. `hotkey_owner` (the tray window) decides whether a
/// hotkey change can apply live; None (overlay/test) means restart-note.
pub fn run_settings(owner: HWND, center: (i32, i32), hotkey_owner: Option<HWND>) -> SettingsOutcome {
    let raw = std::fs::read_to_string(config_write_path()).unwrap_or_default();
    let fc: FileConfig = toml::from_str(&raw).unwrap_or_default();
    let ui = resolve_ui_lang(Some(&fc.ui.lang));
    let orig_combo = fc.hotkey.combos.first().cloned().unwrap_or_else(|| "Shift+PrintScreen".to_string());
    let out = unsafe { run_settings_inner(owner, center, hotkey_owner, &fc, ui, &orig_combo) };
    out.unwrap_or_default()
}

#[allow(clippy::too_many_lines)]
unsafe fn run_settings_inner(
    owner: HWND,
    center: (i32, i32),
    hotkey_owner: Option<HWND>,
    fc: &FileConfig,
    ui: &'static str,
    orig_combo: &str,
) -> Option<SettingsOutcome> {
    static mut REGISTERED: bool = false;
    if !REGISTERED {
        let bg = CreateSolidBrush(COLORREF(0xF0F0F0));
        let cls = WNDCLASSW {
            // No CS_HREDRAW|CS_VREDRAW: full invalidation on every move makes
            // 30+ child controls repaint per frame (visible slide/lag while
            // dragging). Plain dialogs bit-blt on move instead.
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(dlg_proc),
            hInstance: windows::Win32::Foundation::HINSTANCE(null_mut()),
            hbrBackground: bg.into(),
            lpszClassName: w!("SelectableSettings"),
            hCursor: LoadCursorW(None, windows::Win32::UI::WindowsAndMessaging::IDC_ARROW).unwrap_or_default(),
            ..std::mem::zeroed()
        };
        let _ = RegisterClassW(&cls);
        REGISTERED = true;
    }

    // Value lists (parallel to dropdown order; index 0 = safe fallback).
    let mut src_langs = vec!["auto".to_string()];
    src_langs.extend(translate::TRANSLATE_LANGS.iter().map(|s| s.to_string()));
    let mut tgt_langs = src_langs.clone();
    let mut engines: Vec<String> = search::PRESETS.iter().map(|p| p.name.to_string()).collect();
    let cur_engine = fc.search.engine.trim().to_lowercase();
    if !cur_engine.is_empty() && !engines.contains(&cur_engine) {
        engines.insert(0, cur_engine.clone());
    }
    let cur_src = fc.translate.source_lang.trim().to_lowercase();
    if !cur_src.is_empty() && !src_langs.contains(&cur_src) {
        src_langs.insert(1, cur_src);
    }
    let cur_tgt = fc.translate.target_lang.trim().to_lowercase();
    if !cur_tgt.is_empty() && !tgt_langs.contains(&cur_tgt) {
        tgt_langs.insert(1, cur_tgt);
    }
    let find = |list: &[String], v: &str| list.iter().position(|s| s == v).unwrap_or(0);

    // Row plan first (frame measured from content, like the fallback dialog).
    let rows: Vec<Row> = build_rows();
    let increments = rows.iter().filter(|r| **r != Row::Buttons).count();
    let client_h = MARGIN * 2 + increments as i32 * ROW_H + 14 + 30;
    // CLIPCHILDREN+CLIPSIBLINGS: dragging must not repaint the ~50 child
    // controls (labels/fields/buttons) every frame; with class style 0 the
    // frame bit-blts on move instead of full invalidation.
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN | WS_CLIPSIBLINGS;
    let mut frame = RECT { left: 0, top: 0, right: CLIENT_W, bottom: client_h };
    let _ = AdjustWindowRectEx(&mut frame, style, false, WS_EX_DLGMODALFRAME);
    let win_w = frame.right - frame.left;
    let win_h = frame.bottom - frame.top;
    // Clamp into the virtual screen: a tall dialog centered on a small
    // monitor must keep its caption grabbable (top cut off = undraggable).
    let (vx, vy, vw, vh) = unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
            SM_YVIRTUALSCREEN,
        };
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let (cx, cy) = center;
    let pos_x = (cx - win_w / 2).clamp(vx, (vx + vw - win_w).max(vx));
    let pos_y = (cy - win_h / 2).clamp(vy, (vy + vh - win_h).max(vy));
    let mut state = Box::new(Dlg {
        ctrls: Ctrls {
            combo_source: HWND(null_mut()),
            combo_src_lang: HWND(null_mut()),
            combo_tgt_lang: HWND(null_mut()),
            edit_ext_url: HWND(null_mut()),
            check_auto_exit: HWND(null_mut()),
            check_fallback: HWND(null_mut()),
            check_fallback_ask: HWND(null_mut()),
            combo_engine: HWND(null_mut()),
            edit_engine_url: HWND(null_mut()),
            edit_save_dir: HWND(null_mut()),
            edit_ocr_model: HWND(null_mut()),
            edit_cascade: HWND(null_mut()),
            edit_batch: HWND(null_mut()),
            check_directml: HWND(null_mut()),
            combo_ui: HWND(null_mut()),
            hotkey_text: HWND(null_mut()),
            hotkey_hint: HWND(null_mut()),
            change_btn: HWND(null_mut()),
            ok_btn: HWND(null_mut()),
            src_langs,
            tgt_langs,
            engines,
        },
        ui,
        capturing: false,
        pending_hotkey: None,
        orig_combo: orig_combo.to_string(),
        orig_ui_lang: fc.ui.lang.clone(),
        orig_ocr: (
            fc.ocr.model.clone().unwrap_or_default().to_lowercase(),
            fc.ocr.cascade.join(",").to_lowercase(),
            fc.ocr.rec_batch_size,
            fc.ocr.use_directml,
        ),
        hotkey_owner,
        out: SettingsOutcome::default(),
        done: false,
    });
    let ptr = state.as_mut() as *mut Dlg;
    let dlg = CreateWindowExW(
        WS_EX_DLGMODALFRAME,
        w!("SelectableSettings"),
        PCWSTR(widen(&tr(ui, "settings.title")).as_ptr()),
        style,
        pos_x,
        pos_y,
        win_w,
        win_h,
        Some(owner),
        None,
        None,
        Some(ptr as *const _),
    )
    .unwrap_or_default();
    if dlg.is_invalid() {
        return None;
    }

    // Build controls row by row.
    let (hfont, owned) = system_message_font();
    let mut ctor = Ctor { dlg, y: MARGIN, fonted: Vec::new() };
    ctor.header(&tr(ui, "settings.hotkey"));
    ctor.label(&tr(ui, "settings.hotkey"));
    let hotkey_text = CreateWindowExW(
        Default::default(),
        w!("STATIC"),
        PCWSTR(widen(&hotkey::combo_display(&hotkey::parse_combo(orig_combo).unwrap_or_else(hotkey::default_hotkey))).as_ptr()),
        WS_CHILD | WS_VISIBLE,
        MARGIN + LABEL_W, ctor.y + 3, 240, EDIT_H,
        Some(dlg), None, None, None,
    )
    .unwrap_or_default();
    ctor.fonted.push(hotkey_text);
    let change_btn = CreateWindowExW(
        Default::default(),
        w!("BUTTON"),
        PCWSTR(widen(&tr(ui, "settings.change")).as_ptr()),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP,
        MARGIN + LABEL_W + 250, ctor.y, 110, EDIT_H + 2,
        Some(dlg), None, None, None,
    )
    .unwrap_or_default();
    SetWindowLongPtrW(change_btn, GWLP_ID, CHANGE_ID);
    ctor.fonted.push(change_btn);
    ctor.y += ROW_H;
    ctor.label("");
    let hotkey_hint = CreateWindowExW(
        Default::default(),
        w!("STATIC"),
        PCWSTR(widen(&tr(ui, "settings.hotkey_hint")).as_ptr()),
        WS_CHILD | WS_VISIBLE,
        MARGIN + LABEL_W, ctor.y + 3, CLIENT_W - MARGIN * 2 - LABEL_W, EDIT_H,
        Some(dlg), None, None, None,
    )
    .unwrap_or_default();
    ctor.fonted.push(hotkey_hint);
    ctor.y += ROW_H;
    (*ptr).ctrls.hotkey_text = hotkey_text;
    (*ptr).ctrls.hotkey_hint = hotkey_hint;
    (*ptr).ctrls.change_btn = change_btn;

    ctor.header(&tr(ui, "settings.translate"));
    ctor.label(&tr(ui, "settings.source"));
    let combo_source = ctor.combo(&["bergamot".to_string(), "external".to_string(), "online".to_string()], find(&["bergamot".to_string()], &crate::config::normalize_source_mode(&fc.translate.source)));
    ctor.label(&tr(ui, "settings.srclang"));
    let c = (*ptr).ctrls.src_langs.clone();
    let combo_src_lang = ctor.combo(&c, find(&c, &cur_src_nonempty(&fc.translate.source_lang)));
    ctor.label(&tr(ui, "settings.tgtlang"));
    let c = (*ptr).ctrls.tgt_langs.clone();
    let combo_tgt_lang = ctor.combo(&c, find(&c, &cur_src_nonempty(&fc.translate.target_lang)));
    ctor.label(&tr(ui, "settings.exturl"));
    let edit_ext_url = ctor.edit(&fc.translate.external_url);
    let check_auto_exit = ctor.check(&tr(ui, "settings.autoexit"), fc.translate.external_auto_exit);
    let check_fallback = ctor.check(&tr(ui, "settings.fallback"), fc.translate.external_fallback);
    let check_fallback_ask = ctor.check(&tr(ui, "settings.fallbackask"), fc.translate.external_fallback_ask);
    (*ptr).ctrls.combo_source = combo_source;
    (*ptr).ctrls.combo_src_lang = combo_src_lang;
    (*ptr).ctrls.combo_tgt_lang = combo_tgt_lang;
    (*ptr).ctrls.edit_ext_url = edit_ext_url;
    (*ptr).ctrls.check_auto_exit = check_auto_exit;
    (*ptr).ctrls.check_fallback = check_fallback;
    (*ptr).ctrls.check_fallback_ask = check_fallback_ask;

    ctor.header(&tr(ui, "settings.search"));
    ctor.label(&tr(ui, "settings.engine"));
    let c = (*ptr).ctrls.engines.clone();
    let combo_engine = ctor.combo(&c, find(&c, &cur_engine));
    ctor.label(&tr(ui, "settings.engineurl"));
    let edit_engine_url = ctor.edit(&fc.search.engine_url.clone().unwrap_or_default());
    (*ptr).ctrls.combo_engine = combo_engine;
    (*ptr).ctrls.edit_engine_url = edit_engine_url;

    ctor.header(&tr(ui, "settings.save"));
    ctor.label(&tr(ui, "settings.savedir"));
    let edit_save_dir = ctor.edit(&fc.save.dir.clone().unwrap_or_default());
    (*ptr).ctrls.edit_save_dir = edit_save_dir;

    ctor.header(&tr(ui, "settings.ocr"));
    ctor.label(&tr(ui, "settings.model"));
    let edit_ocr_model = ctor.edit(&fc.ocr.model.clone().unwrap_or_default());
    ctor.label(&tr(ui, "settings.cascade"));
    let edit_cascade = ctor.edit(&fc.ocr.cascade.join(","));
    ctor.label(&tr(ui, "settings.batch"));
    let edit_batch = ctor.edit(&fc.ocr.rec_batch_size.to_string());
    let check_directml = ctor.check(&tr(ui, "settings.directml"), fc.ocr.use_directml);
    (*ptr).ctrls.edit_ocr_model = edit_ocr_model;
    (*ptr).ctrls.edit_cascade = edit_cascade;
    (*ptr).ctrls.edit_batch = edit_batch;
    (*ptr).ctrls.check_directml = check_directml;

    ctor.header(&tr(ui, "settings.ui"));
    ctor.label(&tr(ui, "settings.uilang"));
    let combo_ui = ctor.combo(&["auto".to_string(), "zh".to_string(), "en".to_string()], find(&["auto".to_string()], &fc.ui.lang.trim().to_lowercase()));
    (*ptr).ctrls.combo_ui = combo_ui;

    ctor.y += 14;
    let btn_w = 110;
    let ok_btn = CreateWindowExW(
        Default::default(),
        w!("BUTTON"),
        PCWSTR(widen(&tr(ui, "settings.ok")).as_ptr()),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
        CLIENT_W - MARGIN - btn_w * 2 - 10, ctor.y, btn_w, 30,
        Some(dlg), None, None, None,
    )
    .unwrap_or_default();
    let cancel_btn = CreateWindowExW(
        Default::default(),
        w!("BUTTON"),
        PCWSTR(widen(&tr(ui, "settings.cancel")).as_ptr()),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP,
        CLIENT_W - MARGIN - btn_w, ctor.y, btn_w, 30,
        Some(dlg), None, None, None,
    )
    .unwrap_or_default();
    SetWindowLongPtrW(ok_btn, GWLP_ID, IDOK.0 as isize);
    SetWindowLongPtrW(cancel_btn, GWLP_ID, IDCANCEL.0 as isize);
    ctor.fonted.push(ok_btn);
    ctor.fonted.push(cancel_btn);

    (*ptr).ctrls.ok_btn = ok_btn;
    for h in ctor.fonted {
        SendMessageW(h, WM_SETFONT, Some(WPARAM(hfont.0 as usize)), Some(LPARAM(1)));
    }

    let _ = SetFocus(Some(ok_btn));
    let _ = ShowWindow(dlg, SW_SHOWNORMAL);
    let _ = EnableWindow(owner, false);
    let mut msg: MSG = std::mem::zeroed();
    while !(*ptr).done {
        let r = GetMessageW(&mut msg, None, 0, 0);
        if !r.as_bool() {
            break;
        }
        // Capture mode owns keys: bypass dialog navigation so Esc/Enter/Tab
        // reach dlg_proc as WM_KEYDOWN instead of becoming IDOK/IDCANCEL.
        if (*ptr).capturing && msg.message == WM_KEYDOWN {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            continue;
        }
        if IsDialogMessageW(dlg, &msg).as_bool() {
            continue;
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    let out = SettingsOutcome {
        saved: (*ptr).out.saved,
        hotkey_changed: (*ptr).out.hotkey_changed,
        new_combo: (*ptr).out.new_combo.clone(),
        ui_lang_changed: (*ptr).out.ui_lang_changed,
        ocr_changed: (*ptr).out.ocr_changed,
        needs_restart: (*ptr).out.needs_restart,
    };
    drop(Box::from_raw(ptr));
    std::mem::forget(state); // else the original Box double-frees (heap corruption).
    let _ = DestroyWindow(dlg);
    if owned {
        let _ = DeleteObject(hfont.into());
    }
    let _ = EnableWindow(owner, true);
    let _ = SetForegroundWindow(owner);
    let _ = SetFocus(Some(owner));
    Some(out)
}

fn cur_src_nonempty(s: &str) -> String {
    let t = s.trim().to_lowercase();
    if t.is_empty() {
        "auto".to_string()
    } else {
        t
    }
}

/// Row plan (keeps frame math honest; also documents the dialog shape).
#[derive(Debug, PartialEq, Eq)]
enum Row {
    Header,
    Field,
    Hotkey,
    Hint,
    Buttons,
}

fn build_rows() -> Vec<Row> {
    use Row::*;
    vec![
        Header, Hotkey, Hint,
        Header, Field, Field, Field, Field, Field, Field, Field,
        Header, Field, Field,
        Header, Field,
        Header, Field, Field, Field, Field,
        Header, Field,
        Buttons,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_writer_roundtrips() {
        let src = "# c\n[translate]\nsource = \"bergamot\"\n\n[search]\nengine = \"bing\"\n";
        let out = set_section_key(src, "translate", "source", "\"external\"");
        assert!(out.contains("source = \"external\""));
        assert!(out.contains("# c"));
        assert!(out.contains("engine = \"bing\""));
        // New section appended.
        let out2 = set_section_key("", "ui", "lang", "\"zh\"");
        assert_eq!(out2, "[ui]\nlang = \"zh\"\n");
        // Missing section in a non-empty file.
        let out3 = set_section_key("[search]\nengine = \"bing\"\n", "ui", "lang", "\"auto\"");
        assert!(out3.contains("[ui]\nlang = \"auto\""));
        // Commented key does not count; prefix trap.
        let out4 = set_section_key("[translate]\n#source = \"x\"\n", "translate", "source", "\"y\"");
        assert!(out4.contains("#source = \"x\""));
        assert!(out4.contains("\nsource = \"y\"\n"));
        let out5 = set_section_key("[translate]\nexternal_fallback_ask = true\n", "translate", "external_fallback", "true");
        assert!(out5.contains("external_fallback_ask = true"));
        assert!(out5.contains("\nexternal_fallback = true\n"));
    }

    #[test]
    fn toml_string_escapes() {
        assert_eq!(toml_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(toml_str_array(&["a".to_string(), "b c".to_string()]), "[\"a\", \"b c\"]");
    }

    #[test]
    fn validators_accept_and_reject() {
        assert_eq!(validate_batch("6"), Ok(6));
        assert!(validate_batch("0").is_err());
        assert!(validate_batch("65").is_err());
        assert!(validate_batch("x").is_err());
        assert_eq!(validate_cascade("medium,small"), Ok(vec!["medium".to_string(), "small".to_string()]));
        assert!(validate_cascade("huge").is_err());
        assert!(validate_cascade("").is_err());
        assert_eq!(validate_model(""), Ok(None));
        assert_eq!(validate_model("tiny"), Ok(Some("tiny".to_string())));
        assert!(validate_model("huge").is_err());
        assert_eq!(validate_lang_code("AUTO"), Ok("auto".to_string()));
        assert!(validate_lang_code("xx").is_err());
        assert!(validate_url_template("https://h/?q={text}", false).is_ok());
        assert!(validate_url_template("https://h/?q={text}", true).is_err());
        assert!(validate_url_template("ftp://h/{text}", false).is_err());
        assert_eq!(validate_ui_lang("ZH"), Ok("zh".to_string()));
        assert!(validate_ui_lang("fr").is_err());
        assert_eq!(validate_engine("Google"), Ok("google".to_string()));
        assert!(validate_engine("yandex").is_err());
    }

    #[test]
    fn remove_key_drops_optionals() {
        let src = "[search]\nengine = \"bing\"\nengine_url = \"https://x\"\n[save]\n";
        let out = remove_key(src, "search", "engine_url");
        assert!(!out.contains("engine_url"));
        assert!(out.contains("engine = \"bing\""));
        // Missing key: unchanged.
        assert_eq!(remove_key(src, "search", "nope"), src);
    }

    #[test]
    fn settings_row_plan_fits_typical_screens() {
        // 23 content rows * 30px + margins/buttons must stay under 900px.
        let rows = build_rows();
        let increments = rows.iter().filter(|r| **r != Row::Buttons).count();
        assert_eq!(increments, 23);
        let h = MARGIN * 2 + increments as i32 * ROW_H + 14 + 30;
        assert!(h < 900, "client_h={h}");
        assert_eq!(rows.iter().filter(|r| **r == Row::Field).count(), 15);
    }
}
