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

绿色目录由 `cargo xtask dist` 组装（`dist/Selectable/`，git 忽略）：
`Selectable.exe + DirectML.dll + models/<档>/ + config.toml`。
cargo 只负责编译，不管装配；xtask 会复用已编好的 exe，并自动补齐缺失的模型档
（调 `tools/fetch-models.mjs`），已有 `config.toml` 永不覆盖。`dist.bat` 是双击入口。
cmake 现在不引入（纯 Rust 用 xtask 是惯用法），Bergamot C++ 阶段再用 cmake 编译翻译库。

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

## 10. 保存功能（M2，已定）

- Overlay 右下角常驻「保存」按钮（原生 Win32 BUTTON 子窗口）。
- 左键：按 `日期-时间-当前激活窗口名.png` 存到 `我的文档\ScreenShot\`（如
  `2026-10-08-14-32-10-记事本.png`），文件夹不存在就新建。
  - 日期时间取本地 `SYSTEMTIME`，`{yyyy}-{MM}-{dd}-{HH}-{mm}-{ss}`。
  - 「当前激活窗口」指按热键瞬间的前台窗口（Overlay 弹出前抓取标题），非法文件名字符
    `<>:"/\|?*` 及控制字符替换为 `_`，尾部空格/点去掉，超长截断，空标题兜底 `screenshot`。
- 右键按钮：弹出**系统标准另存为对话框**，用 Win32 API 直接调（`IFileSaveDialog`，
  Rust 经 `windows` crate 做 COM 调用即可，不手搓），默认文件名同左键规则、默认目录
  同上，扩展名 png。
- PNG 编码用 `image` crate；保存失败弹系统提示（MessageBox）并记日志，不静默丢图。

## 11. 交互总装（M3，已定；B 项待拍板）

- **托盘化 + 无黑窗**：release 用 `windows_subsystem = "windows"`（debug 留黑窗看日志）；
  托盘图标手搓 `NOTIFYICONDATAW`，右键菜单截图/退出，双击截图。
- **全局按钮**：保存/翻译两个 layered 自绘半透明按钮，顶部中间；OCR 出框后若压住文字
  则整体下移到下方空位。翻译按钮是状态机（见下）。
- **异步流程**：热键→立刻弹 Overlay（底图 + spinner：圆环 12 扇形深灰→透明锥形渐变，
  50ms 一帧旋转）→ 常驻工作线程 OCR → `PostMessage` 回 UI 线程上框、撤 spinner。
  Session 不出工作线程，保证 `!Send` 安全。
- **选择模型**：点按（<300ms 且位移<6px）= 选中当前整框；长按拖选 = 字符级区间，
  跨框按阅读顺序拼接；选中区深蓝底白字重绘（字体用 YaHei 近似，字号≈框高）。
- **B（字符级精度）已实施**：Paddle 单字坐标是 `CTCLabelDecode` 后处理（Apache-2.0，
  非新模型，已抄入 `decode()`）：每个 emit 字符记录 CTC timestep 跨度→crop 横向比例→
  行框边插值反推字条，长按按字条定起止，不再均分。横排文字准；竖排回退整框。
- **Toast**：复制后左下角圆角半透明条，白字显示所复制文字，3 秒淡出（100ms 步进），
  超长截断。
- **翻译（占位）**：引擎未接，按钮/菜单动作为 toast 提示；状态机已定：
  `翻译 →（右键部分翻译）覆盖+按钮变"取消翻译" →（按）回原文+按钮变"全文翻译"`
  `→（按）全文覆盖+按钮变"取消翻译" →（按）回原文+"全文翻译"…`。
  引擎落地后只换执行函数，状态机不动。

## 12. 翻译子系统（M4，实施顺序即清单）

> 路线：A（Bergamot 原生静态库 + 薄 C shim + cmake），Windows runner 出包。
> 模型 MPL-2.0（见仓库根 LICENSE 考证），分发合规动作见 §12.7。

### 12.1 语种判定（OCR 结果 → 源语言）

- v1 用**字形启发式**（零依赖、离线）：含平假名/片假名→ja，含谚文→ko，
  含西里尔→ru，含 CJK 汉字→zh，其余拉丁→en（可配置默认）。
- 繁简不分（z哼模型按 zh 统一处理，字典/行为以实测为准）；置信度低或混合文本
  取占比最高的脚本；`config.toml [translate] source_lang` 可强制指定，覆盖自动判定。

### 12.2 目标语言

- 默认 `target_lang = "auto"`：取系统 locale 主语言（`GetUserDefaultLocaleName`，
  如 zh-CN→zh），映射到 Bergamot 方向码；配置文件可覆盖为固定值。
- 源语言 == 目标语言 → 不翻译，toast「无需翻译」。

### 12.3 语对解析与模型获取

- 方向名 `{src}{tgt}`（如 enzh、zhen）。启动/触发时查本地 `models/translate/{pair}/`
  是否齐套（model + vocab + shortlist + config）。
- 缺失 → 查 Mozilla registry（`models.lock.json` 已有地址，版本 pin 死）该方向是否存在：
  不存在 → toast「暂不支持该语种组合」；存在 → 进 §12.4 下载流程。
- 首版 zip 只带 enzh/zhen（见 §12.7 打包）。

### 12.4 下载进度 UI（复用 loading 区）

- 触发翻译且模型缺失时，Overlay 中央转圈区切换为**下载进度条**（圆角条 + 百分比文字）
  + 一个**取消按钮**（与保存/翻译同套自绘按钮）。
- 下载器走 WinHTTP（`windows` crate `Win32_Networking_WinHttp`，零第三方依赖，
  与全仓零依赖原则一致），按块读并上报字节进度，原子 flag 做取消；
  取消后删残缺文件，回到 idle 状态。registry 若给 hash/size 则校验。
- 下载完成 → 同一次触发内直接进翻译，不用用户再按一次。

### 12.5 翻译执行

- 常驻工作线程执行（与 OCR worker 同模式，Session/模型不出线程）；
  UI 照旧 50ms 轮询。CJK 先按 `。！？!?\n…` 分句再送模型，拉丁按常规断句。
- 结果进现有部分/全文覆盖渲染与状态机（代码已就绪，`translate_engine` 换实即可）。

### 12.6 Bergamot 构建（路线 A）

- 第三方源码 `third_party/bergamot-translator` 做 git submodule（pin rev，
  起点用 translateLocally 验证过的 `9271618`），
  另加我们自己的薄 C shim（C ABI，`translate_init/open/translate/close` 四个函数）。
- `cargo xtask build-translate` 调 cmake（MSVC 本地 / runner 同一套，不在 YAML 里写第二遍）；
  Rust 侧手写 `extern "C"` 绑定 + `cargo:rustc-link-lib=static`。
- **Windows 配方的关键（抄 translateLocally 的作业，不抄它的 Qt）**：
  - MKL 用预编译静态包直链（`mkl-2020.1-windows-static.zip`，解压设 `MKLROOT` 即可，
    不装 Intel 全家桶；OpenBLAS 明确不用，短句场景会慢一个数量级）；
  - vcpkg（runner 自带，本地需装）只装 `protobuf/pcre2`（`x64-windows-static`，
    release-only），Qt/GUI/CLI 相关一律不要——我们只要 translator 核心库；
  - `cmake -DUSE_STATIC_LIBS=ON -DBUILD_ARCH=x86-64`（基线 x64 兼容优先，
    intgemm 运行时自己 dispatch，avx 变体以后再说）；
  - 他们的 `cmake/` 目录有零星 Windows 修正，spike 时按需取用并署名
    （该仓 MIT，比 MPL 还松）。
- 先做 spike：本机 Windows 编过 + 翻一句中英。若上游主分支 Windows 撑不住，
  fallback 改 translateLocally 的 fork 布局（同样 cmake+MSVC，有 Windows 发行版先例），
- 构建产物（静态库）不进 git；CI 缓存 cmake 构建目录加速。

### 12.7 打包与合规

- release 共 6 个 zip：binary、binary+small、binary+medium、small、medium、
  translate-models（enzh+zhen，首版仅此）。版本号唯一真相源 `Cargo.toml`，
  打 `v*` tag 触发；包名带版本+平台，附 `SHA256SUMS`。
- CI 一律调 xtask（`build-translate` → `dist --release-only --tier all`），
  本地/CI 同一条路；翻译模型由 xtask 在 CI 内从 registry 拉取后打第 6 包。
  CI 只跑 `windows-latest`（见 §12.6，Linux 编 Win32/MSVC/ORT/DirectML 是自虐）。
- 首版第 6 包只含 enzh+zhen（决议：日韩等方向走运行时按需下载，不进包；
  管道与测试与语种无关，加包只是下载量问题，有真实需求再加）。
- 合规：新增 `THIRD-PARTY-NOTICES`（模型 mozilla/firefox-translations-models、
  库 browsermt/bergamot-translator，MPL-2.0 全文链接，EU grant 署名），
  进仓库 + 每个 zip 一份；About 页同样署名。
- exe 无签名，release notes 先写明 SmartScreen 提示。

### 12.8 语种下拉框（逃生通道，已定）

- Overlay 翻译行扩展为 `[源下拉][目标下拉][翻译]`：源默认 `Auto（本次判定：xx）`，
  目标默认系统 locale；两个都是标准 Win32 COMBOBOX 子窗口（`CBS_DROPDOWNLIST`，
  原生外观键盘可用，比自绘省事），选项= registry 实际存在的方向并集。
- 任一下拉改动 → 重走 §12.3 resolve（本地有则直接翻，没有则下载/报不支持）；
  与自动判定冲突时手动选择优先，本次 Overlay 有效（config 不自动改写）。

### 12.9 实施顺序
1. spike：本机编过 + 翻一句（定 fallback）。
2. submodule + C shim + Rust FFI + `xtask build-translate`（本地/CI 同路）。
3. `models.lock` 翻译词条 + dev 拉取 + 运行时 WinHTTP 下载器（含取消）。
4. 语种判定 + 目标解析 + 语对 resolve。
5. 下载进度 UI + 取消按钮。
6. 工作线程翻译 + 状态机激活 + 语种下拉框 UI（覆盖渲染已就绪）。
7. THIRD-PARTY-NOTICES + About + 文档。
8. CI YAML：tag 触发，6 包 + SHA256SUMS。
