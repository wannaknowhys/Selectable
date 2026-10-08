[English](README.md) | [简体中文](README.zh-CN.md)

# Selectable

Windows 截屏即选词：按 `Shift+PrintScreen`，弹出最高全屏窗口，背景就是当前截屏，
上面的文字可选择、复制、翻译、搜索。

- 编译型（Rust），离线优先，绿色便携（`exe + models/`，无安装包）。
- OCR：PP-OCRv6 + ONNX Runtime（medium 首发，向 small/tiny 级联）。
- 翻译：火狐 Bergamot 离线（enzh/zhen）+ 在线 API 插槽 + 外置 URL。

完整设计见[设计文档](DESIGN.zh-CN.md) / [DESIGN.md](DESIGN.md)。

## 快速开始

```cmd
node tools\fetch-models.mjs --tier small
cargo run
cargo xtask dist
```

模型永不进 git，见 [models/README.md](models/README.md)。
