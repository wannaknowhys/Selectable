# Changelog — Selectable

## Unreleased

### Behavior change: external fallbacks now ask first (privacy)
- Engine-failure and unsupported-pair fallbacks no longer open the browser
  on their own. Failures show a consent dialog (destination host disclosed,
  with a "don't ask again" checkbox persisted to `config.toml`);
  unsupported pairs only toast. Gate: `[translate] external_fallback`
  (default `false`). Explicit search clicks and `source = "external"` are
  unaffected (clicking IS consent).
- `translate_url` default template uses `{tgt}` (BCP47, e.g. `zh-CN`).

### Added
- Save-As dialog now offers PNG and JPEG (plus all-files); a typed extension
  wins, extensionless names follow the picked filter; JPEG encodes at quality
  90. Quick-save stays PNG.
- `[translate] source = bergamot|online|external` switch (online falls back
  to local with a notice until wired); `external` mode opens the translator
  URL directly (partial = selection, full = everything).
- `[search] engine` presets (bing/google/ddg/baidu/wikipedia/qwant) sourced
  from the local Firefox search-config registry; explicit `engine_url` wins.
- Right-click menu: select-all, translate-selected-text, translate-all-text;
  staged Esc (cancel translation → clear selection → close).

### Fixed
- Overlay selection is now live browser-style: press-drag paints the blue/white
  char-range highlight as the cursor moves (no more rubber-band box); the cursor
  shows an I-beam over OCR text, arrow elsewhere.
- Overlay main window uses `WS_CLIPCHILDREN`, so parent repaints no longer paint
  over the language dropdowns (the `Auto`/target flicker is gone).
- Toast fade is smooth: linear 220→0 over the last second on a dedicated 30ms
  timer (~33 frames per fade) instead of 8 quantized steps on the 50ms tick.
- Fallback consent dialog now uses the system message font (same as Notepad)
  and measures the frame from the wrapped body text, so Yes/No buttons are
  never clipped regardless of host length.
- Settings + fallback dialogs own the caption drag (queued moves collapse to
  the latest position) instead of the system move loop, so the window tracks
  the cursor at 1000Hz+ mouse report rates instead of trailing behind.

### Added
- Settings dialog (tray menu + overlay menu): every `config.toml` key is
  GUI-editable (hotkey with key-capture box, OCR tier/cascade/batch/GPU,
  translate engine + languages + URL + auto-exit/fallback switches, search
  engine + custom URL, save folder); writes preserve comments, invalid
  values are rejected with a message and never brick startup. Hotkey
  re-registers live from the tray (restart-note from the overlay);
  OCR/ui-language changes need a restart. System-reserved combos
  (bare PrintScreen, Win+Shift+S) are refused.
- `[ui] lang = auto|zh|en` (default `auto` = system locale): tray + overlay
  menus and the settings dialog follow it (restart to apply). Toasts/about
  stay Chinese until the next i18n pass.
