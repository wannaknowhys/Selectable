[English](ISSUES.md) | [简体中文](ISSUES.zh-CN.md)

# Selectable Issue Tracker

> Real-world feedback from the user, ordered by severity. Checked off one by one,
> with code pointers for later review.

## #5 Save-As with PNG/JPEG choice (done)

- **Content**: the `GetSaveFileNameW` filter and save branch currently offer PNG
  only; add JPEG (the `image` dependency already keeps just the png+jpeg codecs).
- **Status**: closed. Filter is now PNG / JPEG / all-files; an explicit typed
  extension wins, extensionless names follow the picked filter slot; JPEG
  encodes at quality 90 (the `save` default of 75 rings on screenshot text).
  Quick-save (left-click) stays PNG.

## #1 Selection must not copy; only Ctrl+C copies

- **Symptom**: clicking/dragging puts text into the clipboard immediately.
- **Cause**: M3 coupled "select" with "copy" (`copy_selection` follows selection).
- **Fix**: selection only highlights (+ title-bar count). Copy flows only via:
  `Ctrl+C` (overlay focused), context menu "copy selected/all". Empty-selection
  Ctrl+C toasts a hint instead of silently copying everything.
- **Status**: fixed (verified on-device: clicking leaves the clipboard alone,
  Ctrl+C delivers the text with spaces).

## #2 Boxes and buttons flicker during/after selection

- **Symptom**: red boxes and top buttons keep flickering once selected.
- **Likely causes**:
  1. full-window `InvalidateRect` on every `WM_MOUSEMOVE` / 50ms tick;
  2. buttons re-`AlphaBlend`ed every frame, toast/background recomputed;
  3. no backbuffer — GDI paints straight to the foreground DC.
- **Fix**: backbuffer (compose once to a memory DC, single `BitBlt`) + dirty-region
  invalidation (only repaint what changed) + timer repaints only while spinner/toast
  are active. Backbuffer first, then measure.
- **Status**: closed (verified on-device: no flicker during/after selection).
  Landed: backbuffer + dirty-gated timer + `WS_CLIPCHILDREN` on the overlay
  (child comboboxes no longer painted over) + smooth toast fade on its own
  30ms timer. Full-window `InvalidateRect` stays: a full compose costs ~7–8ms,
  so dirty-region repaint is explicitly declined.

## #3 Loading spinner seemingly never drawn

- **Symptom**: no spinner visible after the hotkey.
- **Done**: upgraded to a dark rounded panel + white ring + 12 large white-to-gray
  gradient dots + centered "recognizing…" label (the old 12 small gray dots were
  indeed near-invisible over busy screenshots). Same proven primitives as
  buttons/toast.
- **Status**: closed (verified on-device: spinner visible during recognition).

## #4 Right-clicking Save shows no dialog

- **Symptom**: right-click on Save does nothing visible.
- **Root cause (found)**: `TrackPopupMenu` with `TPM_RETURNCMD` returns the choice
  as its **return value** — no `WM_COMMAND` is ever generated. The old code dropped
  the return value, so every menu item silently did nothing (same cause behind
  dead copy-selected/copy-all). Now dispatching on the return value; verified
  on-device (menu → save lands a file).
- **`GetSaveFileNameW` FALSE now distinguishes cancel vs error
  (`CommDlgExtendedError` + toast). Verified on-device: dialog shows, cancel
  is silent (no toast), successful save toasts.
- **Status**: closed.

## Backlog (new, ordered roughly by dependency)

## #6 CI release pipeline (todo)

- **Content**: tag-triggered releases from `.github/workflows` (missing entirely):
  6 zips (binary, binary+small, binary+medium, small, medium, translate-models
  enzh+zhen) + `SHA256SUMS`. CI only calls xtask (`build-translate` →
  `dist --release-only --tier all`); translation models fetched inside CI for
  the 6th zip. `windows-latest` only; version single-sourced from `Cargo.toml`.
- **Status**: not started (DESIGN §12.9 step 8).

## #7 Full UI i18n (deferred, after CI)

- **Content**: every app string (menus/buttons/toasts/dialogs) through a string
  table; `config.toml [ui] lang` picks the UI language (default `auto` follows
  the system locale); English + Chinese tables first, lookup with fallback chain.
  Today only tray/overlay menus + settings dialog follow `[ui] lang`;
  toasts/about stay Chinese.
- **Status**: deferred (DESIGN §12.11).

## #8 Settings dialog tabs (todo)

- **Content**: the single settings dialog is getting crowded. Group keys into
  tabs (Hotkey / OCR / Translate / Search / Save / Interface) via a Tab control;
  same line-preserving writer + validation underneath.
- **Status**: unscheduled. Open: tab list = config sections (confirm).

## #9 Online translation provider (decided: configurable templates)

- **Content**: `[translate] source = "online"` currently falls back to local with
  a notice.
- **Decision (user)**: online endpoints are **configurable URL templates**:
  several built-in, user can add new ones, and can partially modify based on
  a built-in template (same pattern as the `[search] engine` presets).
- **Status**: design locked, unscheduled.

## #10 Hotkeys: LL-hook fallback + remapping scope (deferred)

- **Content**: bare PrintScreen is owned by the Win11 Snipping Tool and
  `Win+Shift+S` by Explorer; covering them needs a `WH_KEYBOARD_LL` hook
  fallback (Flameshot approach, no admin). The settings hotkey box is a v1
  placeholder (single combo; conflict keeps the old combo).
- **Decision (user)**: both stay in the backlog, neither starts now.
- **Note**: the LL hook does **not** need admin rights and no manifest change
  is required for it (recorded here per the manifest question). Known limit:
  UIPI — the hook can't see keys typed into elevated windows unless our
  process is elevated too; document it when the fallback lands.
- **Status**: deferred.

## #11 App/tray icon (me to draft 4–5 proposals)

- **Content**: exe + tray icon are still stock. Needs: `.ico` art source, embed
  at build time, tray `NOTIFYICONDATAW` picks it up.
- **Decision (user)**: I draft 4–5 icon proposals, user picks (may also ask
  other LLMs for competing drafts).
- **Status**: drafting next.

## #12 GPU/VRAM: unload mode setting (decided: auto/manual switch)

- **Content**: ORT sessions (+ DirectML) stay resident; observed VRAM is **1GB+**.
- **Decision (user)**: a setting controls the unload mode — **auto** (idle
  timeout unloads, next use reloads) or **manual** (tray switch). Timeout
  minutes + exact setting shape TBD at implementation.
- **Status**: design locked, unscheduled.
