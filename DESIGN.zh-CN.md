[English](DESIGN.md) | [简体中文](DESIGN.zh-CN.md)

# Selectable 设计文档

> Windows 截屏即选词工具：按 `Shift+PrintScreen` 弹出全屏 Overlay，背景为当前截屏，
> 其上文字可选择、复制、翻译、搜索。编译型语言（Rust），离线优先，绿色便携。

## 1. 目标与非目标

目标（MVP 全做）：

1. `Shift+PrintScreen` 触发，全屏最高窗口，背景 = 当前截屏。
2. OCR 后文字框可选：点击框复制、拖选复制、一键全复制。
3. 右键菜单：就地翻译、全文翻译、外置翻译（丢到浏览器翻译器）、搜索引擎搜索。
4. 首版只注册一个热键，但热键系统按“将来可改默认、可加多组合”来抽象。
5. OCR 首发 PP-OCRv6 **medium**，过大/过慢则降 small；binary 同一个，只换模型三件套。
6. 绿色版：`exe + 模型文件夹`，无安装包，不写注册表（开机自启除外，可选）。

非目标：PDF/文档解析（那是 PaddleOCR-VL 的活）、手写输入法、云端账号同步。

## 2. 总体架构

```
托盘常驻进程（单 exe）
├─ 热键监听：RegisterHotKey(MOD_SHIFT, VK_SNAPSHOT)
├─ 抓屏：DXGI Desktop Duplication → 全屏无边框 Overlay（topmost）
├─ OCR Worker 线程池：ORT session 常驻，det → rec batch，取消/超时
├─ Overlay 渲染：box 层 + 透明文字层（点选/拖选）+ 右键菜单
├─ 翻译引擎：Bergamot 离线（enzh/zhen）+ 在线 API 插槽 + 外置 URL
└─ 配置：config.toml（热键、模型级联偏好、搜索引擎、翻译源）
```

关键决策：模型常驻内存、推理异步，UI 永不卡；截图触发只做推理，不做冷启动。

## 3. OCR 引擎：PP-OCRv6（ONNX Runtime，经 Rust `ort` 调用）

### 3.1 为什么不是 `deploy/cpp_infer`

`cpp_infer` 是 demo 性质：OpenCV + `paddle_inference.dll` 全家桶几百 MB，
只支持 CPU/CUDA，不支持 DirectML/Intel/AMD/高通 NPU 通用加速，
异步/取消/超时都要自己加。成品体验 Rust + ORT 胜。

### 3.2 三档对照（已向 HF API / 官方文档核实）

| 档 | 参数 | det ONNX | rec ONNX | models/ 合计约 | 语言 |
|----|------|----------|----------|---------------|------|
| tiny | 1.5M | ~2MB | ~5MB | 10–15MB | 49 种（不含日文） |
| small | 7.7M | ~9.8MB | ~21MB | ~31MB | 50 种统一 |
| medium | 34.5M | ~62MB | ~76MB | ~139MB | 50 种统一 |

small/medium 单一模型统一支持简中/繁中/英文/日文 + 46 拉丁语系，不用换模型。
rec 输入形状 v6 为 `[3,48,320]`（注意 v5 是 32 高，不通用）。

### 3.3 三件套与切换规则

每档 = `det.onnx + rec.onnx + inference.yml`（字典内嵌在 yml 的 `character_dict` 里，
**无需单独下载字典**，程序启动时从 yml 解析，字典与模型天然一致）。
medium ↔ small 可直接互换；切 tiny 必须连 yml 一起换（字典更小）。

启动级联（已定）：

- `config.toml` 有 `model` 设置 → 只用该档，文件缺失则报错退出（记日志+弹窗，不静默降级）。
- 无设置 → `medium → small → tiny` 从大往小试，第一个三件套齐全的胜出；全缺则报错，
  提示运行 `node tools/fetch-models.mjs`。

方向分类器（cls）MVP 跳过：截图极少遇到 180° 旋转文字。

## 4. 翻译方案：Firefox Bergamot 离线优先

- Paddle 的 `PP-DocTranslation` 是文档级 + LLM 重型管线，不适合截图划词，不用。
- 采用 Firefox Translations 同款 **Bergamot**（Marian NMT fork，intgemm 量化，
  C++ 原生库，MPL-2.0）：Rust 侧静态链接，模型放 `models/translate/`。
- 首版只带 `enzh` / `zhen` 两对（`zh→en` 约 50MB，`en→zh` 约 33MB，base 档；tiny 档
  BLEU 低约 8%，截图短句用 base）。
- 注意 CJK 分句：中文需先分句再送模型（对标火狐 WASM 2.x 的 CJK 处理）。
- 在线 API 做成插槽（可配 key），外置翻译走 URL scheme，Bergamot 编译卡住也不挡路。
- 署名：关于页注明 Mozilla / Bergamot consortium（EU Horizon 2020 grant 825303）；
  OCR 模型权重版权归 Baidu/PaddleOCR（Apache-2.0 转分发）。

## 5. 热键

- 首版只注册 `Shift+PrintScreen`（`RegisterHotKey(MOD_SHIFT, VK_SNAPSHOT)`）。
  裸 PrintScreen 在 Win11 默认被截图工具占用，不可靠；`Win+Shift+S` 抢不过 Explorer，
  将来如需支持则用 `WH_KEYBOARD_LL` 低级钩子 fallback（Flameshot 同款路线，无需管理员）。
- 代码按 `Vec<Hotkey>` 抽象，现在长度为 1，选项系统后期加改默认/多组合无需重构。

## 6. 绿色版布局

```
Selectable/
├─ Selectable.exe
├─ onnxruntime.dll (+ DirectML，按需)
├─ config.toml
└─ models/
   ├─ medium|small|tiny/det.onnx, rec.onnx, inference.yml
   └─ translate/enzh/, zhen/
```

无安装包，不写注册表；开机自启做成可选（计划任务/启动文件夹二选一）。

## 7. 入库策略（库与模型怎么进 repo）

- **Rust 库**：只进 `Cargo.toml`（crates.io registry），不 vendor。ORT 原生二进制由
  `ort` crate 构建时自动获取，不提交。
- **OCR/翻译模型**：**永不进 git**。`models/` 被 `.gitignore` 屏蔽；
  `tools/models.lock.json` 锁定 HF commit（不可变）+ 文件名 + 约略体积；
  `node tools/fetch-models.mjs [--tier small]` 下载（Node 零依赖，只用内置 https/fs）。
  CI 同理：先跑 fetch 再 build。
- **Bergamot C++ 库**：二期再定（git submodule vs 预编译 lib），一期先外置 URL + 在线插槽。

## 8. MVP 清单

1. 托盘 + `Shift+PrintScreen` + DXGI 抓屏 + 全屏 Overlay。
2. ORT 常驻 + medium 首发 + 级联 + 三件套切换。
3. 点选/拖选/全复制 + 右键菜单（就地/全文/外置翻译/搜索）。
4. Bergamot enzh/zhen 内置。
5. `config.toml` + 选项页地基（改热键/换模型/换搜索引擎）。

## 9. 前置依赖（本机）

Rust（rustup）、MSVC 或 Clang、CMake（Bergamot 二期）、Node（仅拉模型脚本）。
本机当前有 Node/git，无 cargo，装 rustup 后再验证 `cargo build`。
