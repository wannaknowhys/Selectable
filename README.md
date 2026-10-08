[English](README.md) | [简体中文](README.zh-CN.md)

# Selectable

Windows screenshot-to-selectable-text: press `Shift+PrintScreen`, get a topmost
fullscreen overlay of your screenshot with text you can select, copy, translate, and search.

- Compiled (Rust), offline-first, portable (`exe + models/`, no installer).
- OCR: PP-OCRv6 via ONNX Runtime (medium first, cascades to small/tiny).
- Translation: offline Firefox Bergamot (enzh/zhen) + online-API slot + external URLs.

See the full design: [DESIGN.md](DESIGN.md) / [设计文档](DESIGN.zh-CN.md).

## Quick start

```cmd
node tools\fetch-models.mjs --tier small
cargo run
```

Models are never committed — see [models/README.md](models/README.md).
