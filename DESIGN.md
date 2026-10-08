[English](DESIGN.md) | [简体中文](DESIGN.zh-CN.md)

# Selectable Design (English)

> Screenshot-to-selectable-text for Windows: press `Shift+PrintScreen` for a topmost
> fullscreen overlay showing the current screenshot, with text you can select, copy,
> translate, and search. Compiled (Rust), offline-first, portable (no installer).

## 1. Goals / Non-goals

MVP (all in scope):

1. `Shift+PrintScreen` brings up a topmost fullscreen window backed by the live screenshot.
2. Post-OCR text boxes are selectable: click-to-copy, drag-select, copy-all.
3. Context menu: in-place translation, full-text translation, external translation
   (open in browser translator), search-engine lookup.
4. One hotkey for v1, but the hotkey subsystem is abstracted (`Vec<Hotkey>`) for future
   remapping / multiple combos.
5. OCR ships PP-OCRv6 **medium** first, falling back to small if too big/slow;
   same binary, only the model triple changes.
6. Portable layout: `exe + models folder`, no installer, no registry writes
   (optional autostart excepted). Assembled by `cargo xtask dist` into
   `dist/Selectable/` (gitignored): `Selectable.exe + DirectML.dll + models/<tier>/`
   `+ config.toml`, with missing tiers backfilled and existing `config.toml` kept.
   No cmake for now (xtask is the Rust idiom); cmake arrives with Bergamot C++.

Non-goals: PDF/document parsing (PaddleOCR-VL territory), handwriting IME, cloud sync.

## 2. Architecture

```
Resident tray process (single exe)
├─ Hotkey: RegisterHotKey(MOD_SHIFT, VK_SNAPSHOT)
├─ Capture: DXGI Desktop Duplication → borderless topmost fullscreen overlay
├─ OCR worker pool: resident ORT sessions, det → batched rec, cancellation/timeout
├─ Overlay rendering: box layer + transparent text layer (click/drag) + context menu
├─ Translation: offline Bergamot (enzh/zhen) + online-API slot + external URLs
└─ Config: config.toml (hotkeys, model cascade, search engine, translation source)
```

Models stay resident; inference is async; the UI never blocks on cold start.

## 3. OCR: PP-OCRv6 via ONNX Runtime (Rust `ort` crate)

### 3.1 Why not `deploy/cpp_infer`

`cpp_infer` is demo-grade: hundreds of MB (OpenCV + `paddle_inference.dll`),
CPU/CUDA only — no DirectML / Intel / AMD / Qualcomm NPU coverage — and no
async/cancel/timeout. Rust + ORT wins for a shipping product.

### 3.2 Tier table (verified against HF API + official docs)

| Tier | Params | det ONNX | rec ONNX | models/ total ≈ | Languages |
|------|--------|----------|----------|-----------------|-----------|
| tiny | 1.5M | ~2MB | ~5MB | 10–15MB | 49 (no Japanese) |
| small | 7.7M | ~9.8MB | ~21MB | ~31MB | 50 unified |
| medium | 34.5M | ~62MB | ~76MB | ~139MB | 50 unified |

small/medium cover Simplified/Traditional Chinese, English, Japanese + 46 Latin-script
languages in one model. v6 rec input is `[3,48,320]` (v5 used height 32 — not interchangeable).

### 3.3 Model triple and cascade

Each tier = `det.onnx + rec.onnx + inference.yml` (the char dictionary is embedded in
the yml's `character_dict` — **no separate dict download**; parse it at startup so the
dict always matches the weights). medium ↔ small are drop-in; tiny needs its own yml.

Startup cascade (decided): explicit `model` in `config.toml` → use it or hard-error
(log + dialog, no silent downgrade). No setting → try `medium → small → tiny`,
first complete triple wins; none → error telling the user to run the fetch script.

The textline-orientation classifier is skipped for MVP (screenshots are rarely upside-down).

## 4. Translation: offline-first Firefox Bergamot

- Paddle's `PP-DocTranslation` is a document-scale LLM pipeline — wrong tool for
  screenshot snippets. Not used.
- Firefox Translations' **Bergamot** (Marian NMT fork, intgemm quantized, native C++,
  MPL-2.0): statically linked, models under `models/translate/`.
- v1 ships `enzh` / `zhen` only (base tier: zh→en ≈ 50MB, en→zh ≈ 33MB; tiny loses
  ~8% BLEU, so base it is for short snippets).
- CJK sentence segmentation is required before feeding Chinese text in.
- Online APIs are a configurable slot; external-translator URLs land first so a
  Bergamot build delay blocks nothing.
- Attribution: Mozilla / Bergamot consortium (EU Horizon 2020 grant 825303) in About;
  OCR weights © Baidu/PaddleOCR (redistributed under Apache-2.0).

## 5. Hotkeys

- v1 registers only `Shift+PrintScreen`. Bare PrintScreen is owned by the Snipping Tool
  on Win11 (unreliable); `Win+Shift+S` is owned by Explorer — a future `WH_KEYBOARD_LL`
  hook fallback (the Flameshot approach, no admin needed) can cover it later.
- `Vec<Hotkey>` abstraction from day one; the options UI later adds remapping/multi-combo
  without refactoring.

## 6. Portable layout

```
Selectable/
├─ Selectable.exe
├─ onnxruntime.dll (+ DirectML as needed)
├─ config.toml
└─ models/
   ├─ medium|small|tiny/det.onnx, rec.onnx, inference.yml
   └─ translate/enzh/, zhen/
```

## 7. Ingress policy (libs & models into the repo)

- **Rust libs**: `Cargo.toml` only (crates.io registry), never vendored. ORT native
  binaries are fetched at build time by the `ort` crate.
- **Models**: **never in git**. `models/` is gitignored; `tools/models.lock.json` pins
  immutable HF commits + filenames + approximate sizes;
  `node tools/fetch-models.mjs [--tier small]` downloads them (zero-dep Node).
  CI fetches before building.
- **Bergamot C++ lib**: decided in phase 2 (submodule vs prebuilt lib); phase 1 uses
  external URLs + the online slot.

## 8. MVP checklist

1. Tray + `Shift+PrintScreen` + DXGI capture + fullscreen overlay.
2. Resident ORT + medium-first cascade + triple switching.
3. Click/drag/copy-all + context menu (in-place / full / external translation / search).
4. Built-in Bergamot enzh/zhen.
5. `config.toml` + options-page foundation (hotkeys / models / search engine).

## 9. Local prerequisites

Rust (rustup), MSVC or Clang, CMake (phase 2 Bergamot), Node (fetch script only).

## 10. Save feature (M2, decided)

- A native Win32 BUTTON ("Save") lives at the overlay's bottom-right corner.
- Left-click: save as `date-time-activewindow.png` into `Documents\ScreenShot\`
  (e.g. `2026-10-08-14-32-10-Notepad.png`), creating the folder if needed.
  - Local `SYSTEMTIME`, pattern `{yyyy}-{MM}-{dd}-{HH}-{mm}-{ss}`.
  - "Active window" = the foreground window at hotkey time (title grabbed before the
    overlay pops). Illegal characters `<>:"/\|?*` and controls become `_`, trailing
    spaces/dots stripped, long names truncated, empty titles fall back to `screenshot`.
- Right-click the button: the **standard system Save dialog** via Win32
  (`IFileSaveDialog` through the `windows` crate's COM bindings — no hand-rolled UI),
  prefilled with the same default name/folder, png extension.
- PNG encoding via the `image` crate; failures show a MessageBox and are logged.

## 11. Interaction assembly (M3, decided; item B pending approval)

- **Tray + no console**: `windows_subsystem = "windows"` in release (console kept in
  debug for logs); hand-rolled `NOTIFYICONDATAW` tray icon, context menu
  (capture/quit), double-click captures.
- **Global buttons**: Save/Translate as layered owner-drawn translucent buttons,
  top-center; after OCR, if they cover text they shift down to free space.
- **Async flow**: hotkey → overlay pops instantly (screenshot + spinner: 12-segment
  conic dark-gray-to-transparent ring, 50ms frames) → resident worker-thread OCR →
  `PostMessage` results back, spinner removed. Sessions never leave the worker thread.
- **Selection**: click (<300ms, <6px move) selects the whole box; press-drag selects a
  char range, spanning boxes in reading order; selection redrawn white-on-dark-blue
  (YaHei approximation, size ≈ box height).
- **B (char precision) implemented**: Paddle's single-char coordinates are
  `CTCLabelDecode` post-processing (Apache-2.0, not a new model — copied into
  `decode()`): each emitted char keeps its CTC timestep span → crop x-fraction →
  screen strip via quad-edge interpolation. Long-press selects by strips, no more
  even splitting. Accurate for horizontal text; vertical falls back to whole-box.
- **Toast**: bottom-left rounded translucent strip showing copied text, fades out
  over 3s (100ms steps), truncated when long.
- **Translation (placeholder)**: engine not wired yet; actions toast instead. State
  machine (final): `Translate → (context-menu partial) overlay + "Cancel" → (press)
  original + "Full" → (press) full overlay + "Cancel" → (press) original + "Full"…`.
  Only the executor changes when the engine lands.
