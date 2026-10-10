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
  over 3s (8 quantized levels, ~8 repaints instead of 60; alpha comes from the
  pure `toast_alpha()` — u16 math, since u8 `level*220` overflows and aborts;
  the `toast_alpha_table` regression test pins the table), truncated when long.
- **Translation (placeholder)**: engine not wired yet; actions toast instead. State
  machine (final): `Translate → (context-menu partial) overlay + "Cancel" → (press)
  original + "Full" → (press) full overlay + "Cancel" → (press) original + "Full"…`.
  Only the executor changes when the engine lands.

## 12. Translation subsystem (M4; order below is the build sequence)

> Route: A (native Bergamot static lib + thin C shim + cmake), Windows runners.
> Models are MPL-2.0 (see repo-root LICENSE research); compliance in §12.7.

### 12.1 Source language (from OCR text)

- v1 uses a **script heuristic** (zero deps, offline): Hiragana/Katakana → ja,
  Hangul → ko, Cyrillic → ru, Han → zh, otherwise Latin → en (configurable default).
- No Hans/Hant split (zh handled as one); low-confidence/mixed text takes the
  dominant script; `config.toml [translate] source_lang` overrides auto-detect.

### 12.2 Target language

- Default `target_lang = "auto"`: system locale primary subtag
  (`GetUserDefaultLocaleName`, e.g. zh-CN → zh), mapped to Bergamot codes;
  overridable in config. Source == target → toast "nothing to translate".

### 12.3 Pair resolution & model fetch

- Direction name `{src}{tgt}` (enzh, zhen). Check local
  `models/translate/{pair}/` (model + vocab + shortlist + config) on trigger.
- If missing, look the direction up in the Mozilla registry (pinned in
  `models.lock.json`): absent → toast "pair not supported"; present → §12.4.
- v1 zips ship enzh/zhen only (§12.7).

### 12.4 Download progress UI (reuses the loading area)

- When translation needs missing models, the spinner area becomes a **progress
  bar** (rounded bar + percent) plus a **Cancel button** (same owner-drawn style).
- Downloader over WinHTTP (`windows` crate `Win32_Networking_WinHttp`, zero
  third-party deps), chunked reads with byte progress, atomic-flag cancel;
  partials deleted on cancel; registry hashes/sizes verified when provided.
- After download, translation proceeds in the same trigger — no second press.

### 12.5 Translation execution

- Runs on the resident worker thread (same pattern as OCR); UI polls at 50ms.
  CJK pre-split on `。！？!?\n…`, Latin on normal sentence breaks.
- Results feed the existing partial/full overlay rendering + state machine
  (already wired; only `translate_engine` gets a real body).

### 12.6 Bergamot build (route A)

- Third-party source as git submodule at `third_party/bergamot-translator`
  (pinned rev) plus our thin C shim (C ABI: init/open/translate/close).
- `cargo xtask build-translate` drives cmake (same MSVC path locally and on CI,
  never duplicated in YAML); Rust side hand-written `extern "C"` + static link.
- Spike first: build on local Windows + translate one sentence. If upstream
  Windows support is broken, fall back to translateLocally's fork layout
  (same cmake+MSVC, proven Windows releases; their recipe: prebuilt static MKL
  zip via `MKLROOT`, vcpkg `protobuf/pcre2` only — no Qt, since we only need the
  translator core — `USE_STATIC_LIBS=ON`, `BUILD_ARCH=x86-64` baseline;
  their cmake fixes are MIT, reuse with attribution).
- **Spike result (verified)**: upstream pin `9271618` builds first try, no fallback
  needed. Notes: MKL via explicit `-DMKL_*_LIBRARY` paths (no `MKLROOT` env, no
  vcpkg; static trio baked into the binary, zero new files next to the exe);
  `CL=/utf-8 /DPCRE2_STATIC` through the compiler env; internal PCRE2
  (`SSPLIT_USE_INTERNAL_PCRE2=ON`) needs a one-line cmake-minimum patch (carried
  like upstream's own `patches/`); skip `app/` (install-name issue), we link
  libs only; single-vocab models list the vocab twice; `BUILD_ARCH=x86-64`.
   Both directions translate with excellent quality.
- **Formalize findings (landed)**: upstream defaults to `/MT`, which conflicts
  with the Rust final link (`/MD`, ort-sys). `native/patches/msvc-dynamic-crt.patch`
  is applied idempotently by xtask onto the submodule working tree (the
  `m third_party/...` marker in `git status` is expected; a fresh clone just
  re-runs xtask — no fork, nothing committed); the MKL static triple links
  straight into `translate_engine.dll`, zero extra files beside the exe, zero
  `MKLROOT`; one Marian service per process (a single Engine owned by the
  worker thread, many pairs resident — tests must not build a second one in
  parallel either); the translation file manifest is generated by build.rs from
  `tools/models.lock.json` into `OUT_DIR` (`TRANSLATE_LANGS`/`pair_files`/
  `TRANSLATE_BASE`/`TRANSLATE_KNOWN_PAIRS`, the only copy the Rust side trusts;
  `pinned` default pairs + `pivot` via-en legs baked into one table, `allPairs`
  is the registry snapshot); the worker splits into sentences before translating
  and rejoins per leg target (no spaces for zh targets, single space otherwise),
  pivots run both hops back-to-back on the same worker (texts -> mid -> final),
  replies keep 1:1 line mapping.
- Built libs never enter git; CI caches the cmake build dir.

### 12.7 Packaging & compliance

- Six release zips: binary, binary+small, binary+medium, small, medium,
  translate-models (enzh+zhen in v1). Single version source `Cargo.toml`,
  `v*` tags trigger; names carry version+platform, plus `SHA256SUMS`.
- CI only calls xtask (`build-translate` → `dist --release-only --tier all`);
  translation models are fetched from the registry inside CI for the 6th zip.
  CI runs `windows-latest` only (Linux building Win32/MSVC/ORT/DirectML is pain).
- v1 6th zip ships enzh+zhen only (decided: ja/ko etc. ride the runtime
  on-demand download; pipeline and testing are language-agnostic, bundling more
  is just bandwidth — add on real demand).
- Compliance: new `THIRD-PARTY-NOTICES` (models mozilla/firefox-translations-models,
  lib browsermt/bergamot-translator, full MPL-2.0 link, EU grant attribution)
  in repo + every zip; same attribution in About.
- Unsigned exe: release notes warn about SmartScreen for now.

### 12.8 Language dropdowns (escape hatch, decided)

- The overlay translate row becomes `[source dropdown][target dropdown][Translate]`:
  source defaults to `Auto (detected: xx)`, target to the system locale; both are
  standard Win32 COMBOBOX children (`CBS_DROPDOWNLIST`, native look + keyboard),
  options = union of registry-available directions.
- Changing either re-runs the §12.3 resolve (translate if local, else
  download/unsupported toast); manual choice wins over auto-detect for this
  overlay session (config file untouched).

### 12.9 Build sequence

1. Spike: local Windows build + one translated sentence (locks the fallback).
2. Submodule + C shim + Rust FFI + `xtask build-translate` (same path for CI).
3. `models.lock` translation entries + dev fetch + runtime WinHTTP downloader (cancel included).
4. Language detect + target resolution + pair resolve.
5. Download progress UI + cancel button.
6. Worker-thread translation + state-machine activation + language dropdown UI (rendering ready).
7. THIRD-PARTY-NOTICES + About + docs.
8. CI YAML: tag releases, 6 zips + SHA256SUMS.
9. Pixel-level verification (partly done: dropdowns/`ja` pick, translate button,
  full Chinese subtitles over boxes, pivot confirm with both legs + 99MB, jaen
  chain download on disk; left: download-panel shot, subtitle close-up,
  right-click menu). Live-session method: run the overlay foreground via
  `agy-run` (`lpDesktop = WinSta0\Default`) into the interactive desktop,
  `SELECTABLE_DEBUG=1` logs exact coords to `clicks.log`; under dual screens
  computer-use lands at half the sent X (right half unreachable) — send `2P`
  for physical P, drive modal boxes with keys (Enter/Esc/arrows).

### 12.10 Translation interaction refinements (R1–R5, user-decided, landed)

- **R1 right-click auto-select**: on right-click with no selection (or empty
  selection text), first select the whole box under the cursor, then pop the
  menu; cursor over no box keeps old behavior (grayed items).
- **R2 subtitles over selection**: paint order becomes translated subtitles >
  blue-background selection (it was reversed — selection covered subtitles).
  Subtitles paint as whole regions: partial = one region over the selection
  bounds, full = one region per macro-block, `DT_WORDBREAK` wrapping.
- **R3 mixed text excludes the target**: count scripts in the probe text,
  candidates = all − target, most frequent wins; all-target text reports
  "nothing to translate". Probe text: partial = joined selection,
  full = longest macro-block (one pair per job; mixed screens go with the
  dominant block).
- **R4 full mode translates macro-blocks**: sort lines by (y,x) and cluster —
  vertical gap > 1.5× median line height or no horizontal overlap starts a new
  block; empty-text lines are natural boundaries. In-block joining: hard
  breaks (next line indented > 2 char widths / gap > 0.7 line height / next
  starts with a •/-/number marker / previous line ragged-right) emit `\n`,
  otherwise soft joins (no separator for adjacent CJK, single space otherwise).
  Each block goes to MT as one paragraph (context preserved); results paint
  back as whole regions (no more per-line 1:1; worker Vec order still holds).
- **R5 third-language download confirmation**: when the source ∉ {en, system
  locale} and models are missing, ask first via `MessageBoxW(YESNO)` (pair +
  total MB); Yes enters the download panel, No toasts cancellation (no
  external fallback). Missing en/locale models still download directly.
- **R5-pivot auto-pivot (two hops via en)**: the live registry holds 118 pairs
  with no direct `jazh/zhja/frzh` — only via-en legs. `translate::plan_for`
  tries direct first (counts only when both the snapshot and the baked file
  table hit), else falls back to `src->en + en->tgt` when neither side is en
  (v1 pivots via en only); external fallback only when both are missing. One
  confirm box lists both legs + total MB (summed baked sizes); one download
  panel pulls both legs with global progress; the same worker loads each leg
  and translates sentence-split leg by leg (texts -> mid -> final, replies
  still 1:1 with regions). Pivot legs (`translate.pivot`, ja/fr/de/ko/ru/es/
  it/pt <-> en, 16 legs; candidate rule: first whose releaseStatus contains
  Release, else the first) bake into the same table as the defaults — zero
  live registry fetches at runtime.
- **latin-hint (zero dependencies)**: OCR yields Unicode only, and Latin
  scripts share one block across languages (Paddle `cls` only detects
  orientation; Firefox likewise relies on lang hints/manual picks), so v1
  skips fastText. When the Latin bucket wins, accents (é/è/ñ/ü/ß/ç… score 2)
  plus stopwords (le/la/les/est, der/die/das, el/los… score 1) vote; >= 2 and
  disagreeing with the excluded target refines it (fr/de/es/it/pt). Misfires
  (e.g. café) are overridden by the source dropdown and stay
  passthrough-shaped through the en pivot.
- **Source dropdown derived from the baked pair table**: build.rs splits
  `TRANSLATE_LANGS` from the `allPairs` snapshot (with the `zh_hant` special
  case), usable offline; third languages are selectable before any model is
  downloaded and flow into the R5 confirm + download chain.
