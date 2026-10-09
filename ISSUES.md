[English](ISSUES.md) | [简体中文](ISSUES.zh-CN.md)

# Selectable Issue Tracker

> Real-world feedback from the user, ordered by severity. Checked off one by one,
> with code pointers for later review.

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
- **Status**: open.

## #3 Loading spinner seemingly never drawn

- **Symptom**: no spinner visible after the hotkey.
- **Likely causes** (unconfirmed): the spinner timer/paint path works
  (`--overlay-test` logs paints), but 12 small gray dots are near-invisible over a
  busy screenshot; on small/fast shots it also flashes by. Possibly not drawn at
  all on some machines — instrument first, conclude later.
- **Fix**: bigger/bolder spinner + white ring + centered "recognizing…" text;
  timing logs behind `SELECTABLE_DEBUG`; close only after on-device confirmation.
- **Status**: open.

## #4 Right-clicking Save shows no dialog

- **Symptom**: right-click on Save does nothing visible.
- **Root cause (found)**: `TrackPopupMenu` with `TPM_RETURNCMD` returns the choice
  as its **return value** — no `WM_COMMAND` is ever generated. The old code dropped
  the return value, so every menu item silently did nothing (same cause behind
  dead copy-selected/copy-all). Now dispatching on the return value; verified
  on-device (menu → save lands a file).
- **Left**: `GetSaveFileNameW` FALSE now distinguishes cancel vs error
  (`CommDlgExtendedError` + toast). Close after one on-device dialog sighting.
- **Status**: awaiting on-device confirmation.
