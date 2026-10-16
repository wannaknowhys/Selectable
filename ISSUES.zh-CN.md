[English](ISSUES.md) | [简体中文](ISSUES.zh-CN.md)

# Selectable 问题跟踪

> 用户实测反馈，按严重程度排序。修一个勾一个，关联代码位置方便回查。

## #5 另存为可选 PNG/JPEG（已做完）

- **内容**：`GetSaveFileNameW` 的过滤器和保存分支目前只有 PNG；加上 JPEG
  （`image` 依赖已只保留 png+jpeg 两个 codec）。
- **状态**：已关闭。过滤器现为 PNG / JPEG / 所有文件；手输扩展名优先，
  无扩展名跟所选过滤器槽位；JPEG 用 quality 90（默认 75 在截图文字上振铃）。
  左键快速保存保持 PNG。

## #1 选中≠复制：只有 Ctrl+C 才是复制

- **现象**：点框/拖选后文字直接进了剪贴板，不符合预期。
- **原因**：M3 把"选中"和"复制"绑在一起（`copy_selection` 跟在选择后面）。
- **方案**：选择只做高亮（+标题栏计数），复制只走三处：
  `Ctrl+C`（overlay 聚焦时）、右键菜单「复制选中/全部」、托盘无。空选按 Ctrl+C 给 toast
  「先选中要复制的文字」，不静默复制全部。
- **状态**：已修（真机验证：点框剪贴板不变，Ctrl+C 落袋且带空格）。

## #2 选中过程中和之后，框和按钮闪烁

- **现象**：拖选/选中后红框和顶部按钮持续闪。
- **原因分析**：
  1. 每次 `WM_MOUSEMOVE` / 50ms 定时器都全窗口 `InvalidateRect` 重绘；
  2. 按钮每次用 `AlphaBlend` 现混，toast/背景每帧重算；
  3. 没有后缓冲，GDI 直接画在前台 DC 上。
- **方案**：后缓冲（内存 DC 一次画完再整块 `BitBlt`）+ 脏区失效（只刷选择变化/动画区域）
  + 定时器只在 spinner/toast 活跃时重绘。先上后缓冲，量化的卡顿再跟。
- **状态**：已关闭（真机确认：拖选/选中后不闪）。落地：后缓冲 + 脏门控 timer +
  主窗口 `WS_CLIPCHILDREN`（不再盖住子下拉框）+ toast 独立 30ms 平滑淡出。
  全窗 `InvalidateRect` 保留：全量合成约 7–8ms，脏区重画明确不做。

## #3 Loading 转圈疑似没画出来

- **现象**：快捷键后没看到中间转圈。
- **处理**：已加强为深色圆角底盘 + 白色圆环 + 12 颗白→灰渐变大圆点 + 中央「识别中…」
  白字（原来 12 个小灰点在复杂底图上确实看不清）。绘制链路与按钮/toast 同构，
  均为已验证原语。
- **状态**：已关闭（真机确认：识别时转圈可见）。

## #4 右键保存按钮没弹出另存为窗口

- **现象**：右键「保存」按钮无反应。
- **原因（已找到）**：`TrackPopupMenu` 用了 `TPM_RETURNCMD`——选中项走**返回值**，
  根本不产生 `WM_COMMAND`，旧代码把返回值直接丢了，所有菜单项点了都石沉大海
  （复制选中/全部没反应同因）。已改成分发返回值，真机验证菜单→保存截图落文件。
- **还剩一步**：`GetSaveFileNameW` 返回 FALSE 时区分取消/出错（已加
  `CommDlgExtendedError` 报码+toast），真机确认：对话框正常弹出，取消无 toast，
  保存成功有 toast。
- **状态**：已关闭。

## 待办（新增，大致按依赖排序）

## #6 CI 发版流水线（todo）

- **内容**：`v*` tag 触发 `.github/workflows` 发版（现在整个目录缺失）：
  6 个 zip（binary、binary+small、binary+medium、small、medium、translate-models
  enzh+zhen）+ `SHA256SUMS`。CI 只调 xtask（`build-translate` →
  `dist --release-only --tier all`），第 6 包的翻译模型在 CI 内现拉。
  只跑 `windows-latest`，版本号唯一真相源 `Cargo.toml`。
- **状态**：没开始（DESIGN §12.9 第 8 步）。

## #7 界面全量多语言（deferred，CI 之后）

- **内容**：全部应用字符串（菜单/按钮/toast/对话框）走 string table，
  `config.toml [ui] lang` 选择界面语言（默认 `auto` 跟系统 locale），首版中英
  两表，查表带回退链。现在只有托盘/overlay 菜单 + 设置框跟 `[ui] lang`，
  toast/about 还是中文。
- **状态**：deferred（DESIGN §12.11）。

## #8 设置框选项卡化（todo）

- **内容**：单个设置框越来越挤。按 Tab 控件分组（热键 / OCR / 翻译 / 搜索 /
  保存 / 界面），底下还是同一套行级保留注释 writer + 校验。
- **状态**：待排期。待确认：选项卡 = 按 config section 分？

## #9 在线翻译provider（todo，待拍板）

- **内容**：`[translate] source = "online"` 现在 toast 回退本地。需要定：
  provider 选型、key 存哪（本地 config，永不进 git）、每次调用的隐私 consent。
- **状态**：等决议（哪家？key 怎么管？）。

## #10 热键：LL hook 兜底 + 改键范围（todo，待定范围）

- **内容**：裸 PrintScreen 被 Win11 截图工具占、`Win+Shift+S` 被 Explorer 占，
  要 cover 得上 `WH_KEYBOARD_LL` hook fallback（Flameshot 路线，无需管理员）。
  设置框热键现在是 v1 占位（单组合，冲突保留旧键）。
- **状态**：等定范围（hook 兜底？多组合改键？都要？）。

## #11 应用/托盘图标（todo，待美术 + 范围）

- **内容**：exe + 托盘图标还是系统默认。需要：`.ico` 美术来源，构建时嵌入，
  托盘 `NOTIFYICONDATAW` 跟着换。
- **状态**：等美术（ICO 谁出？）+ 范围（只 exe，还是托盘一起？）。

## #12 显存：闲置模型卸载（todo，待数据）

- **内容**：ORT session（+DirectML）常驻，低显存机器上占着内存不放。
  候选形态：闲置超时自动卸载、手动加载/卸载开关、按档关 GPU。
- **状态**：等数据（实测显存占用多少？N 分钟闲置自卸，还是给开关？）。
