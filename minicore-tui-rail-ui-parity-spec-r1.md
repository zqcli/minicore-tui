# MiniCore TUI：Rail UI 等价实现开发 Spec

**版本：r1 · 2026-09-05**  
**实施对象：现有 `minicore-tui`，不是新建 TUI。**  
**首期范围：完整 Agent Loop 的对话区、Editor、Footer 及其直接交互。**

> 目标不是“参考 Pi 风格”，而是在相同数据、终端尺寸和主题输入下，使这三个区域的布局、颜色、折叠、编辑、选择、复制和滚动行为与 `zqcli/pi-rail-ui` 一致。保留已经完成的 MiniCore RPC/Loop/History 逻辑；重做显示层，而不重做执行内核。

---

## 1. 基线、证据及文档优先级

| 项目 | 本次读取的基线 | 用途 |
|---|---|---|
| `zqcli/pi-rail-ui` | `main@1d0dd1611a4d9546c64fe9f5b5c966253fb88eba` | 唯一 Rail UI 行为和样式参照 |
| Rail 依赖的 Pi | `0.84.4`，见其 `package.json` | 原生 Editor、Markdown、选择行为的参照；不能改用浮动 latest |
| `zqcli/minicore-tui` | `dev@2b8268dbba81c162b30e984b9b31a58ebc3bba65` | 被修改代码；当前已有 v0.2.1 的 Request 分组、历史对齐和 Markdown 修复 |
| `zqcli/minicore-agent` | `dev@b2e23938d073ab21c2775faa623561ba929a5ed1` | 后端协议与可获得数据 |
| `minicore-runtime` | `87f3cf92b9b5980b0f468174a319cf53427d858e` | 不修改 |
| 用户截图 | 本交付包 `reference.png`，2048×1067 | 整体密度、留白、透明背景、底部结构的视觉目标 |

开始开发时记录实际 HEAD。若代码已前进，先阅读差异；已正确完成的部分保留。本文针对已读取的代码编写，**没有在本地执行这几个仓库的构建或测试**。

优先级：

1. 最新用户要求及本 Spec 明确约定；
2. 固定版本 Rail 源码及其真实渲染/输入结果；
3. 用户截图；
4. 旧 MiniCore TUI Spec。

截图能确定整体画面，但不能确定不可见的键盘规则、展开行为和全部主题 token。截图与源码的细微 RGB 差异可能来自终端主题、截屏和字体渲染；颜色 token 按源码，透明区域继承终端背景。禁止凭截图猜一个新的主题。

旧 Spec 中以下要求被本版本替代：四边圆角 Editor、随 reasoning 变色的 Editor 边框、双行 Footer、总是展开/总是折叠所有 Tool、默认 Request 调试标题、正常完成后显示大块成功横幅。旧 Spec 中 **Loop、Steer、Update、Persistence、关闭清理语义仍保留**。

### 1.1 不是“全部 Pi 产品能力迁移”

首期必须交付三个区域内的等价体验。下列独立产品能力不在首期：Subagent 执行/面板、MCP、Skills、Session Fork、`!bash` 执行入口、图片/视频、Mermaid/LaTeX、新 Provider、审批、压缩和 OpenAI priority tier。

截图的 `subagent` 行是 Tool 卡片的一种内容，不要求为了复现它实现 Subagent 引擎。已有后端若将其作为普通 Tool 提供，使用通用 Tool renderer 即可。

同理，`xhigh` 是截图所用模型的配置值，不是配色名称。当前后端没有该值时不得把 `high` 改名为 `xhigh`，也不能用假值填满 Footer。

---

## 2. 当前实现与目标的实质差距

| 区域 | 已读取的 MiniCore 实现 | Rail 目标 | 处理 |
|---|---|---|---|
| Editor | `src/ui/composer.rs::render` 绘制 `Block::bordered()` / Rounded；`Margin(1,1)` | 只有细左 rail，slate 底色，没有上下左右矩形框 | 替换几何与绘制，保留文本编辑状态 |
| Editor 高度 | `composer_height_phase5`：内容至少 3 行，再加 2 行边框；忙碌时固定高度 | 可见 surface 4–12 行，上限受终端高度 32% 约束；短内容居中补空行 | 统一计算、点击定位和光标使用同一行映射 |
| Footer | `footer.rs::render`：宽屏两行，包含 request/rev 与较长结果描述 | 一行；左侧 cwd@branch/model/thinking/state/duration，右侧 token/cache/context/cost | 替换布局与内容选择，不伪造指标 |
| 对话 | `transcript.rs` 对不同 section 通用地加上下空行 | 不同 surface 有不同 padding；相邻外部 spacer 合并，内部空行不能合并 | 引入一个明确的 section 几何模型 |
| Thinking | 已有每 Request reasoning/text 分离；当前只做总可见性 | 透明底、紫色 rail；3 行阈值自动折叠；单块点击展开 | 在已有分组上增加折叠状态 |
| Tool | `tool.rs` 依赖有限结果预览/总展开开关；RPC 缺路径和命令 | 状态底色+左 rail；三行 simple collapsed view；write 默认折叠；20 行阈值 | renderer 替换；补最小展示数据 |
| 正常过程 | 默认 Request 标题、完成统计块占据对话空间 | 默认只看消息、thinking、tool；内部 request/revision 不打断阅读 | 移到已有详情/日志入口 |
| 鼠标 | 已有滚轮/viewport；尚不能视为拥有 Pi 原生选择能力 | 单块点按、拖选、选词/选段、复制反馈、蓝色滚动条拖动 | 一个确定的命中图，不移植 TS patch/全局 registry |
| 数据 | History 有 text/reasoning/tool result/usage，但无 user timestamp、Tool arguments、branch/context view | 时间行、工具命令/路径、Footer 分支等需要真实来源 | 独立的最小 Agent 只读展示补充，见第 12 节 |

**保留**：`RpcProcess`、Request ID 分发、`App::update` 单写入者、Loop/Request 身份、History 分页游标、Steer 确认语义、`session.update` 下一 Request 生效语义、Blocked/Unsaved、TerminalGuard、现有 CJK/Markdown 修复和关闭回归测试。

**不保留为目标**：原来的 Pi 默认 UI 快照。它们只做旧版本归档，不应阻止 Rail UI 修改。

---

## 3. 交付结构：显示层替换，不搭新框架

```text
Agent RPC ──> 现有 App::update / SessionView / LiveLoop / History
                         │
                         ├── 显示身份、折叠、选择、滚动锚点
                         └── 一份 PreparedConversation
                                  ├── styled rows
                                  ├── section ranges / copy ranges
                                  └── hit geometry
                                            │
                                       Ratatui draw
```

Ratatui、Crossterm、tui-textarea、现有 Markdown 实现及锁文件先不升级。不要同时做依赖升级、执行层重构和 UI 对齐。

建议最多新增这些生产模块：

| 文件 | 内容 |
|---|---|
| `src/ui/rail.rs` | Rail surface、padding、状态 token、单行裁剪等纯函数 |
| `src/state/view.rs` | section 身份、手动折叠、选择、滚动锚点；也可并入现有 state 模块 |
| `src/ui/editor_layout.rs` | Editor 视觉行到原文位置的双向映射 |
| `src/clipboard.rs` | 单一平台剪贴板适配；复制失败返回错误，不连串尝试多个外部工具 |

其余修改已有文件。`src/app.rs` 已较大；**只允许把本次新增的 UI 输入处理抽成 `src/app/ui_actions.rs`**，仍由 `App::update` 调用。不要趁机重写所有 RPC reducer，也不要把新增 renderer、token formatter 全堆回 app.rs。

禁止新增：通用 Component trait 树、Rail registry、主题继承引擎、万能 Hook、全局 Service Locator、Redux、第二套 Event bus、TUI 自有持久化历史。

Rail 仓库中的 TS monkey patch、prototype 包装、Symbol registry、native renderer 兼容补丁不用移植。它们是 Pi 扩展的接入方式，不是本 Rust 应用的必要结构。

---

## 4. 精确布局和样式

### 4.1 总体几何

继续使用 fullscreen alternate screen。对话滚动，Editor 和 Footer 固定在底部。

```text
[1-column app gutter][conversation surfaces              ][scrollbar]
                       ...
[1-column app gutter][ native-style Working... status              ]
[1-column app gutter][▎ slate editor surface                        ]
[1-column app gutter][▎                                            ]
[1-column app gutter][▎                                            ]
[1-column app gutter][▎                                            ]
[1-column app gutter][▸ cwd@branch · model · thinking · ● working ...]
```

固定规则：

- App 左 gutter = **1 cell**，对话、status、Editor、Footer 都在它右侧。
- Rail 字符 = **`▎`**，占 **1 cell**，不是 `│`，不是两列装饰。
- Rail 与其 surface 内容之间的通用 gap = **0**。
- surface 内文本 padding 仍按组件的 native 内容计算。Tool simple 内容有额外 **1 cell** padding；User 的 `textGapWidth=1`；Editor 使用原生 Editor 的内容起点。不要为追求“所有字符同列”擅自抹掉这些差别。
- Assistant 正文通过空白 inset 对齐 Thinking 正文，但不画 rail。
- Rail 区域和填充背景以外保留透明；Footer 无背景块、无 rail、没有额外底部空行。
- Scrollbar 只在 conversation 溢出时显示，不能延伸到 Editor/Footer。
- 不常驻顶部大 Header。保留启动空页面提示，已有会话中不得反复插入 Logo/快捷键块。
- 普通完成不插入 `✓ Turn completed · requests...` 横幅；错误、未确认保存和 Blocked 仍清楚显示，不能为了相似而隐藏真实故障。

### 4.2 Token：直接来自 `ui-style.json`

| Token | RGB / 十六进制 |
|---|---|
| Editor / User background | `(49,50,68)` / `#313244` |
| Editor / User rail；scrollbar thumb | `(137,180,250)` / `#89B4FA` |
| Selection background | `(69,71,90)` / `#45475A` |
| Selection foreground | `(245,245,250)` / `#F5F5FA` |
| Thinking rail | `(203,166,247)` / `#CBA6F7` |
| Tool title | `(205,214,244)` / `#CDD6F4` |
| Tool output | `(166,173,200)` / `#A6ADC8` |
| Tool muted / User timestamp | `(127,132,156)` / `#7F849C` |
| Tool pending bg / rail | `#282B3D` / `#89B4FA` |
| Tool success bg / rail | `#29312E` / `#7B9F88` |
| Tool error bg / rail | `#342B2F` / `#BC7888` |
| Tool cancelled bg / rail | `#292A35` / `#7F849C` |
| Command output rail | `#94E2D5` |
| Resource/status rail | `#FAB387` |
| Footer sky / mint | `#89B4FA` / `#A6E3A1` |
| Footer amber / lilac | `#F9E2AF` / `#CBA6F7` |
| Footer text / muted | `#CDD6F4` / `#7F849C` |

**重要区别**：模型调用的 `bash` 是 `toolExecution`，成功时与其他工具一样用绿色底。Rail 中 `bashExecution` 的黄色 rail/特殊底色专用于用户 `!bash` 系统命令，不得按 tool.name == bash 误套用。当前不新增 `!bash` 执行功能。

不再用 reasoning level 改 Editor rail；Editor rail 始终是配置的蓝色。Reasoning level 作为 Footer 数据展示。

### 4.3 透明与终端主题

Rail 的 Thinking、Assistant、Footer 背景是透明。Rust 使用终端默认背景或现有统一 page background，不单独刷一块旧 `#18181e` 面板。用户截图的大片背景采样约为 `#2D2A2E`，这是该截图的终端背景，不是 Rail 强制主题色。

视觉测试固定相同 page background，比较 Rail 自己输出的颜色。不要把抗锯齿像素差异当成字符网格不一致。

---

## 5. 统一 section 数据：布局、点击、复制必须使用同一份结果

保留现有 `TranscriptBlock` 与 `LiveLoop` 为内容来源，不复制出第二份对话真相。

为显示块生成稳定身份，例如：

```rust
// 名称可按现有代码风格调整；字段语义不能改成文本哈希。
struct SectionId {
    session_id: String,
    loop_id: Option<String>,
    request_index: Option<u32>,
    kind: SectionKind,
    ordinal: u32,                 // 同一 Request 内第几个 thinking/text run
    tool_call_id: Option<String>,
}

enum FoldOverride { Expanded, Collapsed }
```

User Prompt 可用 LoopId + kind + occurrence；已存储的 User/Steering 使用 History index 辅助匹配。同一段重复文本必须是不同 section。禁止用文本作为折叠或 timestamp 的唯一 key。

`PreparedConversation` 至少输出：

- 用于绘制的 styled rows；
- 每个 section 的起止逻辑行、有效内容列区间、是否可折叠；
- 每行对应的可复制文本范围；
- content 高度与 scrollbar 几何。

只保留**一份**命中数据。绘制、total_lines、滚轮、点击折叠、拖选、复制、Editor 外部边界都使用同一布局结果。现有 `PreparedTranscriptCache` 可以直接扩充，不要求再造一套 cache 管理器。

缓存键增加折叠/选择呈现 revision；点击后必须失效相关 section。折叠状态不写 History、不写 Agent Store。

Live→History 对齐时，稳定 SectionId 尽量不变，必须保留手动折叠状态和滚动锚点。已退休 Loop 的迟到 Event 不得重建卡片；现有 r2 逻辑保持。

---

## 6. Message UI/UX

### 6.1 User Prompt

- 蓝 rail、slate surface；继承 native Markdown 文本。
- surface 内上下 padding 各 1 行；外部 spacer 与内部 padding 分开，内部填色空行不能被 `append_section` 当作外部空白吞掉。
- 时间行位于正文之后、底部 padding 之前。
- 时间格式与 `formatUserMessageTimestamp()` 一致：本地时区、en-US 12 小时制，例如 `2:05 PM · 9/5/2026`。
- 新消息以 Agent 接受时间为准；重开不变。旧历史没有时间时显示 `time unavailable` 的同款 muted 时间行，不用 `now()` 冒充历史时间。
- 选中复制遵循 Rail：包含选择到的时间内容，不包含 rail/gutter/padding。

### 6.2 Assistant Thinking

- 透明背景，紫色 rail，主题的 thinkingText 前景与斜体。
- 连续 thinking parts 按 Rail `nativeAssistantRailBlocks()` 的方式合并为 thinking run；不要把整个 Loop 的所有 reasoning 提到最前。
- 默认为 **3 个原文逻辑行阈值**。Rail 当前实现以 rawLines 判断超限，再预览已渲染的前 3 行；hidden count 也按其源函数计算。不能未经说明改成“超过 3 个软换行就折叠”。
- 原样复现该版本 `collapseHint()` 的提示和样式，生成参考 fixture 锁定，避免手写一个相似字符串。
- 用户手动展开后，新增 delta、Resize、final/history replace 不得再次自动收起。
- 单击可切换本块；全局 `Ctrl+T` 保留隐藏/显示 reasoning 的现有作用，不能拿它替代单块折叠。
- 没有收到 reasoning 时不要在对话中伪造一段 `Thinking...`；使用底部正在工作状态。

### 6.3 Assistant Text

- 透明背景，无 rail；内容列与 Thinking 内容列对齐。
- 继续支持粗体、斜体、代码块、列表、链接等现有 Markdown，不移除 v0.2.1 已完成的修复。
- 基本顺序是 **Request 0 的 parts/Tool → Request 1 的 parts/Tool**。对于标准 reasoning→text 响应，必须保持该顺序。
- 若 Agent 返回有序 `parts`，忠实保留其顺序；不得为了“思考在前”重排一个实际上先 text 后 reasoning 的响应。旧 flattened 字段继续用于旧数据，不能推断已经丢失的原始顺序。
- 默认不显示 `Request #... · rev...` 标题。需要诊断时使用已有详情/日志入口。
- Streaming 用现有 plain/cache 策略，最高每帧一次重排；最终 Markdown 出来时保留滚动锚点和选择，不重复渲染正文。

### 6.4 Tool execution

Simple collapsed 内容严格为三行：

```text
bash
$ cargo test --all-targets
... (68 more lines, ctrl+o to expand)
```

规则：

- 三行均裁成单视觉行，不 wrap 成多行；`\n`、`\r`、`\t` 等空白按 Rail `collapsedSimpleLine()` 压成单空格。
- 卡片 surface 填满可用宽度；三行之外的前后 blank 是透明外部 spacer，不能涂成大块厚卡。
- 标题使用 Tool title；detail 使用 Tool output；hint 使用 muted，`ctrl+o` 保留 dim 的键名样式。
- 默认折叠阈值 **20 行**；`write` 不管长度默认折叠。短工具默认展开，不能简单地让所有工具都三行。
- 阈值和 `N more lines` 按 `execution-presentation-policy.ts` / `executionHiddenLineCount()` 计算。Tool JSON 行数、write content 行数、edit old/new 行数和结果行数由 Agent 提供真实统计，不用字节数除以行宽猜测。
- 每卡手动折叠优先；`Ctrl+O` 更新全局展开状态，对当前已存在 Tool 生效；之后流式刷新不得撤销用户操作。新卡按参考实例行为初始化，使用差分输入 fixture 锁定。
- 展开必须能访问 Agent 已提供的全部结果，不能固定只显示前 30/40 行且再也无法查看更多；只绘制 viewport 内行即可。
- 同一次 ToolCall 从 pending→success/error/cancelled 原位改变背景和 rail，不删除再插入另一张卡。
- ToolCall 和 ToolResult 使用 `(session_id, loop_id, request_index, tool_call_id)` 关联，不能按名称匹配。

常用 detail 对齐参照：`bash` 为 `$ command`；`read` 为 path 加可选 `:start-end`；`write/edit` 为 path；其他工具使用明确提供的 generic detail。`apply_patch` 无同名 Rail 专用 renderer 时按普通 Tool 展示，不新建 Diff 产品。

### 6.5 Steering、系统输出和异常

- 已入 History 的 steering 是用户消息，沿用 User surface；只保留小型 `steer` 标识区分，不额外设计橙色大卡。
- 仍待确认的 steer 在 Editor 上方显示紧凑 queued/未记录提示；`request_started` 不是逐条送达证明。继续以 History 确认，沿用现有 r2 语义。
- 本地命令普通反馈用透明 surface + mint rail；资源/警告用对应 orange rail。多条同组输出只加一次 group 前空白。
- 正常完成的统计横幅删除；失败、保存未确认、blocked 不能删除。用同一简洁 rail notice 展示，而不是另建全屏错误产品。
- `persistence=failed` 保留 unsaved live 内容，禁发新 Prompt；不得因为“视觉清爽”清掉上轮结果。

---

## 7. 折叠、滚动、鼠标与复制

### 7.1 行定位与滚动锚点

折叠/展开前记录 `{section_id, section_local_row, viewport_screen_row}`。重算后尽量保持同一内容在同一屏幕行。单击展开时关闭 follow-tail，防止展开内容立刻被底部跟随挤出屏幕。

用户停在底部且没有主动选择/展开时，新输出继续自动跟随。向上滚动后不抢回底部；保留 End/Ctrl+End 回底功能。不保留一个与渲染器竞争的额外 viewport。

### 7.2 点击仲裁

一次鼠标事件按以下优先级处理：Overlay/selector → scrollbar drag → Editor → conversation selection → section toggle。

Tool/Thinking 折叠只在**左键释放**、press/release 在同一内容位置、没有拖动、没有词/段选区、没有链接动作时触发，参照 `handleRailSectionClick()`。拖动选择不能顺便折叠；单击 Editor 不能折叠其上方卡片。

### 7.3 选择

首期属于对话 UX 的必要能力：字符拖选、双击选词、三击选段、跨可见块拖选、靠近 viewport 边缘自动滚动选择。

- 使用内容坐标，不使用输出 ANSI 字符串坐标。
- CJK、emoji/组合字符按 grapheme + terminal cell width 对应到原文；不能切开 UTF-8 或宽字符半格。
- 复制只含所选内容，不含 rail、gutter、右侧填充、scrollbar、人工 spacer 或 ANSI 控制码。
- User timestamp 遵循参考的 includeTimestamp；工具折叠 hint 属于装饰，不把它冒充工具输出复制。
- 流式追加期间选区不跳；原选区文本保留。必要时冻结本次选择的可见行映射，不冻结模型运行。

### 7.4 剪贴板

封装一个 `Clipboard::set_text`；优先沿用已有实现，没有时允许一个兼容 MSRV 的平台剪贴板库。禁止实现“系统命令 A→B→C→OSC52”的多重自动兜底链。

复制成功显示 Footer `selection copied`，mint 色，**1800ms** 后消失，不用右上角 Toast。失败只给短 Notice，不显示成功。测试替换 clipboard port，不访问真实剪贴板。

### 7.5 蓝色 scrollbar

- 单列蓝色 thumb、透明 track；只覆盖 conversation。
- wheel 每步 3 行，PageUp/Down 使用现有 viewport。
- 拖动时只移动 thumb preview；松手才提交内容 scroll offset，符合 Rail，而不是持续重排整段历史。
- 动画时长按配置 **90ms**，通过现有 tick 驱动。不要像 TS 扩展那样绕过 Ratatui 直接写 terminal cell。
- thumb 范围至少 1 行；到顶/到底精确可达；拖动期间新的 delta 不让 thumb 跳回底部。

---

## 8. Editor 的等价实现

### 8.1 几何和高度

把 `Block::bordered()`、Rounded、横向线和标题从 `composer::render` 移除，换成 Rail surface。固定蓝 rail，不随 high/low 改色。

```text
responsive_max = max(4, min(12, floor(terminal_rows * 0.32)))
editor_rows    = max(4, min(responsive_max, native_body_rows))
```

此处是 surface 可见行数，不再额外加两行边框。保留至少一行 conversation；极小终端用现有安全提示。

短内容补空行：`top_padding = floor((target_rows - body_rows)/2)`，剩余放底部。长内容裁成窗口，保证 cursor 可见，参照 `fitEditorBodyRows()`。Slash completion rows 接在 fitted editor body 后面，属于 Editor 区域的交互，不计入普通正文高度；总布局仍不得压到 Footer。

### 8.2 一份文本真相和一份映射

保留 `state/composer.rs::Composer` / tui-textarea，继续负责文本、cursor、Undo/Redo、history。不要换掉编辑引擎。

新增纯计算 `EditorLayout`，同时供 render、鼠标点击、光标、选择、height 使用：

```text
visual row → logical line + raw range + grapheme/cell mapping
raw cursor → visual row + column
visible top + centered padding → screen position
```

替换 `cursor_cell` / `cursor_wrap_pos` 中与 wrap_plain 各算一遍的位置逻辑。新增 `Composer::move_to(...)` 等小方法，外部不要直接修改 TextArea internals。

点击 padding 不生成非法位置；点击宽字符中间落在其起点；点击行尾落在有效行尾。软换行边界用同一策略，不能画在下一格、编辑却发生在上一行。

### 8.3 需要保留/补齐的编辑行为

多行输入、Enter 提交、Shift+Enter/Ctrl+J 换行、方向键、Home/End、选择、Undo/Redo、输入历史、bracketed paste、IME hardware cursor、可见行鼠标点按定位。

Idle 提交 Prompt，Running 提交 Steer；send/steer in-flight 的 editor revision 保护、失败恢复输入逻辑保持。不因为换 UI 重新设计请求队列。

空 Editor 应接近截图：slate surface + cursor，不常驻长篇 `Type a message...`/`Steer current turn...` 指导文字。模式和错误通过短 Status/Footer 表达；Waiting/Blocked 必须仍可知，不假装可发送。

### 8.4 粘贴标记

Rail 明确高亮 `[paste #N +L lines]` / `[paste #N C chars]` 这类 native marker。首期要实现大段粘贴的显示折叠/高亮，并确保发给模型的是完整原文。

- 原文仍是唯一 buffer；marker 是 Editor 的视图投影，不作为真实 Prompt 内容发送。
- 保留 paste range 与编号的少量本地信息；普通键盘输入恰好含 `[paste #1...]` 不能被误认成隐藏 payload。
- 光标跨 marker、删除、Undo/Redo、历史恢复必须有测试；编辑进入隐藏范围时先展开，不让显示与真实位置脱节。
- Pi 原生触发阈值和编辑操作结果由 **0.84.4 的 reference 输入 fixture**锁定，不能根据最新 Pi 或凭空设一个阈值。
- 不新建通用 rich-text 编辑引擎；若现有 TextArea 不能直接表达，先实现“raw buffer + folded presentation ranges”，而非第二份可变文本副本。

### 8.5 Slash autocomplete

从现有 local command 列表生成补全，不能写第二份命令清单。普通输入不弹列表；`/` 前缀显示候选，选中文本蓝色，底部保留 1 行，主列宽度按 12–32 cell 限制。

Up/Down 选择，Tab 完成，Esc 关闭；Enter 的完成/执行行为以 reference fixture 为准，不能在未完成参数时自动执行命令。仅提供 MiniCore 已支持命令，不为了凑列表提供不可用的 `/rail-agent`、`/rail-oai-fast`、Skills。

---

## 9. Footer：严格一行，不再两行

### 9.1 结构

```text
▸ minicore-tui@dev · gpt 6 astra · high · ● working · 1h1m       ↑4.2m ↓67.1k R7.9m W0 · ctx 18.09%
```

数据是示例，禁止作为实值。截屏中的 `xhigh` 只在后端真正支持时显示。

左侧顺序与 `renderSimpleFooter()` 一致：cwd short + branch → model short → thinking → 可用的 fast label → ready/working → duration → queued → selection copied。

右侧顺序：input/output/cache read/cache write → context → cost（有值或订阅时才出现）。无背景、无 rail、无底部 gap。工作区只显示 basename，最多 20 cell；branch 最多 16；model 最多 24。

### 9.2 文本格式

- 分隔符为 muted ` · `。
- ready 为 mint `● ready`；运行为 amber `● working`。
- 用 `formatNum()` 同样的量级：小于 1000 整数、1000–999999 一位小数 k、之后一位小数 m。
- **R 是 cache read，W 是 cache write，不是 reasoning token。** reasoning 不额外累加进 output。
- 时间：本次 Loop wall time 向下取分钟，`0m`、`59m`、`1h1m`；新 Request、Steer、换模型不重置。Loop 结束后保留该耗时，下一 Loop 再重置。
- context 有可信值时两位小数；不足 70% lilac，≥70% amber。没有值显示 **`ctx ?`**。
- cost `0<cost<.01` 四位小数，`.01≤cost<1` 三位，≥1 两位；正成本或订阅才显示，订阅加 ` (sub)`。没有计费来源时隐藏，不新建价格系统。
- model short 按源函数替换 claude/gemini/gpt 前缀、去指定日期/-latest/-preview 后缀、连字符变空格、合并空白，再裁剪；保留原始模型 ID，不改 RPC 值。

### 9.3 窄屏算法

直接按 `fitAligned()` 规则：先保证右组；右组已占满时裁右组；否则给左组 `width-rightWidth-1`，中间至少一个空格。不能自动变两行，也不要随意换成我们偏好的 token 优先级。

Blocked/Unsaved 等危险状态同时有独立短 Status/Notice，不能只放在可能被 Footer 裁掉的左组。正常 request/revision 信息不塞回默认 Footer；保留在 session details。

### 9.4 数据和增量

Footer formatter 只接收 `FooterView`，不每帧读磁盘、查 Git、遍历整个 History 或调用 RPC。

History assistant usage 作为已保存部分来源，当前 Loop 的实时 usage 作为临时部分。`turn.wait` 的 Loop total 不能再叠加已统计的同 Loop assistant usage。以 `(loop_id, request_index)` 去重替换，而不是“每收到 Event 就加一次”。重开只统计已存储部分；失败未保存的 usage 单独标记，不进入已保存累计。

缺失值不得变成假 0；空会话且已确认没有请求可显示 0。部分请求未返回 usage，显示已知部分加 `?` 标记或整个指标 `?`；在详情里解释，不把不完整统计冒充准确值。

### 9.5 只读详情

可新增一个轻量 `/rail-session` 别名，复用已有 Overlay，展示 Session ID、模型、reasoning、usage、context 来源、pending revision、保存状态。它不管理 Subagent、不更改配置、不新增 settings 框架。

---

## 10. 交互与功能的保留边界

必须保持现有：Prompt→Model→Tool→Model、多 Request、Steer、同 Loop 换模/思考等级、取消、关闭/重开、多 Session、History 对齐、Event gap、Blocked 结果保留。

显示来源优先级：

1. 持久化结果经 `turn.wait` 确认后，用 `session.history` 校准；
2. 运行中 Event 只维护 live view；
3. 新展示字段同样只读、可丢；不能反向驱动执行。

新 Model 选择保存后，Footer 可以显示 Session 的已选 model，当前 Request 的实际 model/revision 在 details 保留；下一 Request 开始前不能重写之前 Assistant 卡片的模型来源。

不要新增本地 follow-up queue。Running 时输入继续走真正 `turn.steer`。Queued 只说明后端已接受，不声称已经应用或保存。

---

## 11. 为什么必须有少量 Agent 展示补充

当前 `src/history.rs::ToolCallView` 只有 ID/name/call_index；Tool arguments 被明确剔除；`UserHistoryView` 没有时间；`ToolFinished` Event 只有 outcome/content_bytes；现有 TUI Footer没有 branch、context、cost 数据。

所以，只改 TUI 可实现配色/边框，但无法真实复现截图的工具命令/路径、即时结果和时间行。

本 Spec **允许下面一小块 Agent 只读扩展，禁止修改 Runtime 或放宽执行规则**。这是为了实现当前要求而新增的展示权限，不应继续机械遵循旧文档中“任何工具参数都不可进入 TUI”的限制。

严格边界：展示允许用户已经让本地工具执行的 command/path；不发送认证对象、Provider raw response、opaque reasoning；不在 tracing/RPC error/debug dump 记录展示内容。不能宣称“命令预览绝不含秘密”——用户自己写在命令里的秘密也会出现在本地视图，README 必须说明。

---

## 12. Agent 最小只读扩展

### 12.1 新增字段，不改变现有执行 RPC

保留 `turn.send/wait/cancel/steer`、`session.update/history`。允许新增 **一个** `session.presentation` 只读方法，并给既有 History/Event 增加 optional 字段。TUI 仍只经 RPC，不读 Store/Workspace，不自行运行 `git`。

建议结构（这是待开发 schema，不是声称当前已存在）：

```rust
struct ToolDisplay {
    detail: String,                 // command/path/range；不是整份 raw arguments
    expanded_input: Option<String>, // 展开时需要的白名单输入正文；无 ANSI
    input_line_count: Option<usize>,
    hidden_line_count: Option<usize>,
    truncated: bool,
}

// ToolCallView 新增 display: Option<ToolDisplay>
// UserHistoryView 新增 timestamp: Option<String>  // RFC3339
// ToolFinished.result 新增 content: Option<String>
// AssistantHistoryView 新增 parts: Option<Vec<AssistantDisplayPart>>
```

`AssistantDisplayPart` 只允许可见 text、可见 reasoning、ToolCall ID 引用；从已经 sanitize 的 `AssistantHistory.content` 保留顺序，不带 encrypted/signature。旧的 flattened text/reasoning/tool_calls 保留以兼容现有客户端。

Tool detail formatter 在 Agent 中只有一份，供 live/history 复用：bash→`$ command`，read→path/range，write/edit→path，其他→明确的有界 generic detail。**不要把 write content、edit old/new 和完整 patch 偷渡进 detail**。需要与 Rail 的展开内容等价时，通过单独的 `expanded_input` 明确提供：write 的 content、edit 的 old/new 文本、apply_patch 的 patch；TUI 仅做代码/差异展示，不执行它们。已知工具使用固定字段白名单，不把整份 invocation、运行环境或 Provider 对象输出为 JSON。

输入正文受当前 Tool arguments 原有大小上限约束；展示文本超过限制时明确 `truncated=true`。折叠提示中的 hidden count 必须对应展开后能够展示的输入/结果行；若受截断限制，另写“原始内容已截断”，不能承诺不存在的更多行。对于完整数据，按 Rail 原函数计算同样的 N。展开不能只显示 ToolResult 一行，却声称隐藏了数百行可展开的 write 内容。

这是本地 UI 的内容权限变更：命令和上述输入正文可能包含用户写入的秘密，必须在协议文档说明其会到达本地 TUI；仍然禁止进入日志、错误或 Debug dump。新展示结构使用脱敏 Debug（只含 identity、长度、truncated），不能直接 derive 出完整字符串。

### 12.2 Live 数据采集不依赖猜测事件时序

Runtime `ToolContext` 只有 cancellation/deadline/progress，**没有** loop_id/request_index；不得在实现中调用不存在的字段，也不要等 ToolStarted Event 才猜参数。

在 Agent 装配 Model/Tool 的边界增加两个薄包装，复用同一份 per-session `Presentation`：

- Model wrapper 在 `Model::start` 从真实 ModelCallContext 记录当前 `(loop_id, request_index)`；一次 Session 同时只有一个 Loop，下一 Model Request 在当前 Tool batch 后发生。
- Tool wrapper 在 `execute(invocation, context)` 开始时读取并固定这个 RequestKey 和 ToolCallId，计算有界 ToolDisplay；执行结束时从真正的 ToolOutput 取得结果内容。
- 使用现有 `AgentEventSink` 发 `tool_presentation`（含完整 identity）或给现有事件补字段。允许它早于/晚于 ToolStarted，TUI 合并到同卡。
- wrapper 原样传播 cancellation/deadline/error/result，不重试、不改成功/失败、不再创建 Agent Loop。
- 短同步状态读写不持锁跨 await；显示失败只影响显示，不改变工具结果。不建立第三套后台 worker/event bus。
- 展示缓存只保留当前 Loop；完成后 detail 从真实 History 参数重新生成，结果从 History 读取；不要永久复制全部 ToolOutput。

增加的自定义事件须更新 TUI DTO，并对未知只读事件保持现有兼容规则。还要覆盖同 Model 被两个 Session 共享、运行中换模型和取消时的身份隔离测试。

### 12.3 时间的存储

Agent 在接受 Prompt/Steer 时记录时间。最终只为真正进入 report History 的 User item保存对应时间。

- 给 **新写入的** `StoredLoopRecord` 添加 optional presentation metadata，例如按 User occurrence 对齐的 `user_times`；沿用同一 JSONL append，不另建数据库/文件。
- 不改 Runtime HistoryItem，不改核心 sanitize 结果，不重写旧 JSONL。
- 多个相同文本的 prompt/steer 按 occurrence/FIFO 对齐，不做 text→唯一时间 map。
- 旧记录读为 None；TUI 不用当前时间补值。
- 失败未保存的时间只在 live 存在，与现有 persistence 语义一致。
- `turn.send` / `turn.steer` 成功响应可附加 optional `accepted_at`，保持原有 `turn` / `ok` 字段；TUI 按原 RequestId 关联 live User 时间。响应到来前显示 pending 状态，不伪造已经被 Agent 接受的时间。这个字段不改变提交/应用/持久化的三个确认边界。

### 12.4 Footer 数据

`session.presentation` 建议返回：

```json
{
  "session_id": "ses_...",
  "model_label": null,
  "git_branch": "dev",
  "context": {"tokens": null, "window": null, "percent": null, "kind": "unknown"},
  "cost_usd": null,
  "using_subscription": null,
  "last_loop": {"loop_id": "...", "started_at": "...", "finished_at": null}
}
```

说明：

- branch 由 Agent 在 Workspace 中通过固定参数的 Git 查询取得，不经 shell 拼接；不是 repo 或没有 Git时为 null。创建/open及一批 Tool 结束后刷新即可，不每帧查询。
- model_label 可选友好名称；没有时用 model profile ID，不能暴露凭据或修改请求 ModelRef。
- context 必须来自 Provider/已有请求计量。**累计输入 token 不是当前上下文占用。**没有可靠值为 unknown，TUI 显示 `ctx ?`。若只能估算，明确 `kind=estimated`，UI 用 `ctx ~NN.NN%`，不冒充截图的精确值。
- cost/subscription 当前没有来源时为 null；本期不建价格目录，不硬编码模型窗口或价格。
- 给请求相关的 context 数据附可选 `(loop_id, request_index)`，避免换模型后的旧结果覆盖新值。
- 当前 Loop usage 可通过该方法或 request 完成后的只读事件提供；只需按 Request 更新，不做 token 级 telemetry。

TUI 在 create/open、request 边界或 Turn 完成时刷新；短时间多个触发合并为一个在途请求。没有 per-frame RPC。

### 12.5 能力缺失不是假数据通行证

旧 Agent 仍可显示 `ctx ?`、无branch、无旧timestamp，但不得在最终验收报告里把这些情形写成“截图信息已完全对齐”。首期正式测试必须使用完成上述必要字段的 Agent binary，至少验证新会话时间、Tool detail、Tool结果、usage和真实 Git branch。

Context/cost 无 Provider数据时的 unknown/省略，是 Rail 支持的状态，不阻止组件同构；但缺少数据这一事实必须明确记录。`xhigh`、subagent orchestration 等不在当前后端能力内，不能用伪数据冒充。

---

## 13. 文件级修改方案

### 13.1 minicore-tui

| 文件/方法 | 必须实施的修改 |
|---|---|
| `src/theme.rs` | 增加/替换 Rail tokens；透明 surface；Editor 蓝色固定；旧 reasoning_color 可留给菜单文字，不再控制 Editor |
| `src/ui/rail.rs`（新增） | `surface_row`、rail/inset/padding、single-line clip、section外部spacing；参数用具体 enum，不接受任意动态theme/registry |
| `src/ui/layout.rs` | 替换 `composer_height_phase5`、`footer_height`；Footer恒1行；建立统一 `screen_layout`；修正 `busy`，last_result不是持续Working |
| `src/ui/mod.rs::render` | 使用统一布局；不重复估算dock；不画旧圆框/双Footer；Normal完成不占Status行 |
| `src/ui/transcript.rs::prepare_cache` | 扩展为 rows + section ranges + copy ranges；键包含view revision |
| `build_durable_lines` / `live_section` | 同一section renderer用于live/history；使用真实parts顺序；不默认Request标题；不能全Loop提前thinking |
| `last_result_lines` | 正常Completed不输出大横幅；failed/unsaved留下紧凑异常提示 |
| `src/ui/user.rs` | User蓝rail/slate、原padding、时间行 |
| `src/ui/assistant.rs` / `reasoning.rs` | Assistant无rail对齐；Thinking紫rail、3行规则与手动fold |
| `src/ui/tool.rs` | 状态surface、simple三行、20行阈值、write默认fold、单卡开关、展开完整已收到结果；不用name判断bashExecution |
| `src/ui/composer.rs` | 去Rounded/Block::bordered；调用EditorLayout；填满slate；hardware cursor由同一map定位 |
| `src/ui/editor_layout.rs`（新增） | wrapping/center-padding/visible-window/grapheme mapping，支持单击和selection |
| `src/state/composer.rs` | 保留TextArea；加受控move/select方法、paste ranges；rawtext唯一事实、marker不发模型 |
| `src/state/transcript.rs` / `tool.rs` | 接受optional display/timestamp/parts；稳定SectionId；取消默认只依赖全局bool的fold决定 |
| `src/state/view.rs`（新增或并入现有） | per-section overrides、copy selection、scroll anchor、scrollbar drag preview；不得含RPC业务 |
| `src/app.rs::update` | 新UI事件分发；保留请求分发和r2保护；新增history/live显示数据合并 |
| `src/app/ui_actions.rs`（可选新增） | 本期新增鼠标/折叠/复制的具体handler，避免继续膨胀app.rs |
| `src/event.rs` / `keymap.rs` | 完善mouse down/move/up/double/triple选择、Ctrl+O、Ctrl+T、autocomplete按键；不能抢现有Cancel/Steer |
| `src/ui/footer.rs` | 将`render/status_line/sides_line`改为Rail一行；新增纯`footer_view/format_num/format_cost/fit_aligned`；去request/rev常驻显示 |
| `src/ui/status.rs` | 仅busy/错误等需要时显示简短Working；不要重复完成卡统计 |
| `src/clipboard.rs`（新增） | 一个clipboard实现+测试mock，成功后触发1800ms Footer提示 |
| `src/protocol.rs` | 展示字段optional；新增presentation请求/事件DTO；不重写Loop协议 |
| `src/main.rs` | 将统一layout/hit geometry提交App；clipboard副作用集中；不让render执行IO |
| 现有UI snapshots/tests | 更新旧圆框/双Footer断言；加入Rail差分及颜色断言；不可只一键重录然后宣称一致 |

### 13.2 minicore-agent：独立小提交

| 文件 | 范围 |
|---|---|
| `src/presentation.rs`（新增） | 具体ToolDisplay formatter、per-session展示状态、薄Model/Tool包装、Footer只读数据；无UI颜色/像素 |
| `src/history.rs` | 从真实History生成optional display/parts；user time通过Agent metadata映射；保留原字段 |
| `src/event.rs` | optional展示数据及必要只读事件；继续bounded/best-effort |
| `src/agent.rs` / `src/sessions.rs` | 装配display context、接受输入时间、查询presentation；不改Loop控制和blocked处理 |
| `src/store.rs` | optional新Loop展示元数据，serde default读取旧记录；禁止重写历史 |
| `src/rpc/protocol.rs` / `server.rs` | `session.presentation`和扩展DTO；不增加修改Store或执行命令的RPC |
| `docs/rpc.md` / tests | 明确本地展示权限、字段缺失、消息尺寸、脱敏和兼容规则 |

Runtime 不修改。Agent 不加入 Rail 色值、折叠偏好、鼠标坐标或 terminal width；它提供内容事实，TUI决定排版。

---

## 14. 源码等价测试：不能只看自己的快照

### 14.1 固定参考环境

在开发/测试工具目录准备参考实例，安装 Rail 基线锁定的 Pi 0.84.4。Node 只用于离线生成 fixture，不作为 TUI 运行依赖，也不要求普通 Rust CI 在线拉包。

复制自己项目的函数、样式和测试用例进行移植是允许的；保留原来源说明。不要为“独立实现”而重新猜已经确定的行为。

### 14.2 Reference fixtures

新增 `tests/fixtures/rail/`：同一个输入模型分别喂给 Rail 真实 renderer 与 Rust renderer，导出 normalized cell grid：字符、fg、bg、bold、italic、下划线、光标坐标。透明色以统一 terminal-default 标识保存，不用随机截图像素。

至少覆盖：

1. 空 Editor、1行、4行、超过12行、中间光标、软换行、CJK/emoji、paste marker、slash候选；
2. User Markdown+固定timestamp、重复文本不同timestamp；
3. Thinking 2/3/4/长逻辑行、长单行软wrap、hidden状态、手动展开后继续stream；
4. Tool pending/success/error/cancelled，write短内容、普通Tool19/20/21行，长命令单行裁剪；
5. Thinking→Text→Tool→下一Request 的混合完整loop；
6. Footer ready/working/queued/copied、time边界、k/m边界、context69.99/70、未知context、nullcost、窄屏；
7. scrollbar默认/顶/中/底/拖动preview/释放commit。

几何尺寸至少：80×24、120×40、宽屏（与截图所在终端相同cols/rows）；另测60×16可用性，不把该尺寸下必须裁剪的数据伪装完整。

### 14.3 需要黑盒锁定的 native 行为

Rail自己没实现而继承Pi的部分，不可凭空写阈值：Editor paste触发、输入光标/selection边界、Slash Enter行为、词/段选择、Markdown内生padding。

先用固定版Pi录制少量输入序列fixture，再移植结果。该录制是测试工作，不是运行时适配器。找到某行为无法相同时，在差异表里明确标记，不用修改“期望值”来通过。

### 14.4 视觉验收

`TestBackend` 文本快照不验证颜色，需要逐cell断言样式。截图验收使用同样字体、字号、行高、终端背景、缩放、cols/rows；对照 `reference.png` 检查密度，不比较不同环境的抗锯齿。

首期至少交付四张真实运行截图：混合Loop进行中、同Loop完成后、单块展开并保留滚动位置、Editor多行与单行Footer。不得只交组件孤立截图。

---

## 15. 回归与验收清单

| ID | 必须通过 |
|---|---|
| RAIL-01 | active screen无旧圆角Editor四边框、无双行Footer、无常驻Request调试标题 |
| RAIL-02 | gutter1 / rail1 / gap0与native组件padding正确，无二次inset |
| RAIL-03 | 透明区域、Editor/User、Thinking、四种Tool状态颜色与reference cells一致 |
| RAIL-04 | Editor4–12行/32%/居中padding/光标可见，与reference相同 |
| RAIL-05 | Editor点按、wrap、IME cursor和CJK/emoji使用同一行映射 |
| RAIL-06 | paste marker可高亮折叠；提交为原文，Undo/Redo不复制/丢失payload |
| RAIL-07 | Slash完成仅列出现有命令，键盘行为与reference fixture一致 |
| RAIL-08 | User时间行位置/格式/颜色正确；重复文本时间不串；未知旧时间不伪造 |
| RAIL-09 | Thinking3行阈值及hidden计数与source一致；手动fold状态跨stream/final保持 |
| RAIL-10 | Assistant和Thinking同列关系正确；跨Request、parts顺序正确 |
| RAIL-11 | Tool三行simple格式正确；write默认fold，其他按20行阈值 |
| RAIL-12 | 模型bash使用Tool状态surface，不误用`!bash`黄色样式 |
| RAIL-13 | Tool展开能读所有已提供结果，长detail不挤坏布局 |
| RAIL-14 | 单击只改本section；拖选/双击/链接不误折叠 |
| RAIL-15 | Ctrl+O全局规则、Ctrl+T可见性与手动override无抢夺 |
| RAIL-16 | 折叠、Resize、live→history后锚点稳定；follow-tail不抢用户浏览 |
| RAIL-17 | 蓝scrollbar只在溢出显示；拖动只preview，释放commit |
| RAIL-18 | 字符/词/段选择、跨块和边缘滚动有效 |
| RAIL-19 | 复制无rail/ANSI/padding；成功在Footer显示1800ms，不生成Toast |
| RAIL-20 | Footer一行、字段顺序/颜色、basename@branch与reference一致 |
| RAIL-21 | formatNum/formatCost/context阈值/duration/fitAligned边界一致 |
| RAIL-22 | cache R/W不误当reasoning；History/live/wait重复数据不重复计费计数 |
| RAIL-23 | 无context/cost时用unknown/省略，不用截图数值填充 |
| RAIL-24 | Agent提供live/history一致的ToolDisplay，新User timestamp可重开 |
| RAIL-25 | Agent展示包装不改变真实工具结果、取消、deadline或Loop顺序 |
| RAIL-26 | 两Session共用Model和中途换Model时展示identity不串线 |
| RAIL-27 | optional元数据不重写旧History；旧客户端/旧记录可读 |
| RAIL-28 | 同Loop send→多Request→tool→final、Steer、session.update保持 |
| RAIL-29 | Event丢失后保存成功从History对齐；保存失败不清空unsaved |
| RAIL-30 | Blocked再send失败不丢原wait结果，close错误先查询真实state |
| RAIL-31 | Agent/TUI日志无prompt、command detail、tool result、opaque reasoning |
| RAIL-32 | 默认测试离线，MSRV与三平台CI回归；真实PTY终端恢复测试 |
| RAIL-33 | Reference生成结果已提交且出处固定；不是只替换自身snapshots |
| RAIL-34 | 四张完整运行截图和差异表齐全；不声称完成后端不存在的功能 |

补充压力场景：6个Loop/10个Request的reasoning+text+tools混合历史；每次最后文本不能重复；用户在第2个Tool展开时后台继续生成、切Session再切回仍保留状态。20 Turn RPC soak继续运行，不新增第二套模拟Loop。

---

## 16. 开发顺序与合并门槛

| 阶段 | 独立提交建议 | 门槛 |
|---|---|---|
| 0 | `test(rail): pin visual reference and capture contract fixtures` | 固定三repo HEAD、source style和reference输入；完整基线先通过 |
| 1 | `feat(agent): expose bounded presentation data for local clients` | 第12节必要数据；不改Runtime；独立review脱敏/身份/Store兼容 |
| 2 | `refactor(ui): unify rail geometry and one-line dock layout` | 去旧框、固定Editor与Footer；尺寸/颜色grid测试 |
| 3 | `feat(ui): render rail messages and stable per-section folding` | User/Thinking/Text/Tool、padding、SectionId、source阈值 |
| 4 | `feat(editor): match rail editing layout and input presentation` | 光标、点击、paste、autocomplete；不能回退编辑正确性 |
| 5 | `feat(ui): add anchored selection copying and rail scrollbar` | 一份hit map；click/drag不冲突；90ms thumb preview |
| 6 | `feat(footer): match rail identity usage and transient feedback` | 实值、不重复累计、右对齐、clipboard反馈 |
| 7 | `test(rail): verify end-to-end visual and interaction parity` | 差分fixtures、PTY截图、soak、恢复/失败回归、差异报告 |

阶段1可与纯UIfixture并行，但最终验收不能只用伪造ToolDetail跳过真实Agent接入。

不规定必须删多少行代码。优先删除旧框/旧footer/重复坐标算法，不拆解与本任务无关的已有App业务代码。

---

## 17. 验证命令及交付要求

沿用当前工具链和锁文件：

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
```

Rail参考生成在独立开发环境执行其 `npm run check` 和本期新增fixture生成脚本。默认Rust测试直接消费已提交fixtures，不需网络、真实模型、真实用户文件或真实剪贴板。

每次最终报告必须写：

- TUI、Agent、Rail实际起始/结束HEAD，Runtime确认未改；
- 修改了哪些生产文件、保留了哪些现有业务路径；
- RAIL-01～34的实际结果；未执行的不写通过；
- 来源到目标的函数对照表和仍存在的视觉/行为差异；
- 四张完整会话截图、颜色/坐标fixture比较、CJK/paste/copy结果；
- Agent新增只读字段与敏感内容边界；
- context/cost/旧timestamp等缺失数据的真实限制。

**完成定义**：已确认数据下，对话区、Editor和Footer的呈现和操作通过固定版Rail差分；真实Agent跑通多轮、工具、Steer、换模、取消和History对齐；未知指标诚实展示；不引入插件系统、动态布局框架、TUI自有执行逻辑或Runtime改动。

---

## 18. 源码定位索引

以下为本次实际读取的主要来源，链接固定到提交，开发者应直接打开对应函数，而不是依赖旧Spec描述。

### Rail UI

- [S1] `ui-style.json`：几何、颜色、阈值、Footer宽度。  
  https://github.com/zqcli/pi-rail-ui/blob/1d0dd1611a4d9546c64fe9f5b5c966253fb88eba/ui-style.json
- [S2] `rail/rail-surface.ts`：`EditorSurfaceRenderer::{contentWidth,targetInputHeight,renderSurfaceRow}`、`SurfaceContentInsetBlock`。  
  https://github.com/zqcli/pi-rail-ui/blob/1d0dd1611a4d9546c64fe9f5b5c966253fb88eba/rail/rail-surface.ts
- [S3] `components/editor/rail-editor.ts`：`splitNativeEditorRows`、`fitEditorBodyRows`、`moveCursorToMousePosition`、`RailEditor::render`、paste marker样式。  
  https://github.com/zqcli/pi-rail-ui/blob/1d0dd1611a4d9546c64fe9f5b5c966253fb88eba/components/editor/rail-editor.ts
- [S4] `components/messages/assistant-message-rail.ts`：`AssistantThinkingRailBlock`、`nativeAssistantRailBlocks`、`renderAssistantMessageRail`。  
  https://github.com/zqcli/pi-rail-ui/blob/1d0dd1611a4d9546c64fe9f5b5c966253fb88eba/components/messages/assistant-message-rail.ts
- [S5] `components/messages/user-message.ts` / `user-message-timestamps.ts`：surface、timestamp位置与en-US格式。  
  https://github.com/zqcli/pi-rail-ui/blob/1d0dd1611a4d9546c64fe9f5b5c966253fb88eba/components/messages/user-message.ts
- [S6] `components/executions/execution-presentation-policy.ts`、`execution-collapse.ts`、`execution-rail.ts`：默认折叠、hidden count、三行simple和tool detail。  
  https://github.com/zqcli/pi-rail-ui/blob/1d0dd1611a4d9546c64fe9f5b5c966253fb88eba/components/executions/execution-rail.ts
- [S7] `components/executions/rail-click.ts::handleRailSectionClick`：release仲裁和关闭followingEnd。  
  https://github.com/zqcli/pi-rail-ui/blob/1d0dd1611a4d9546c64fe9f5b5c966253fb88eba/components/executions/rail-click.ts
- [S8] `components/footer/footer.ts` / `footer-session-snapshot.ts`：`renderSimpleFooter`、`fitAligned`、`formatDuration`、`formatNum`、`formatCost`、model short。  
  https://github.com/zqcli/pi-rail-ui/blob/1d0dd1611a4d9546c64fe9f5b5c966253fb88eba/components/footer/footer.ts
- [S9] `README_zh.md`、`package.json`：继承native输入/选择/scroll，Pi0.84.4，独立Subagent范围。  
  https://github.com/zqcli/pi-rail-ui/blob/1d0dd1611a4d9546c64fe9f5b5c966253fb88eba/README_zh.md

### 当前 MiniCore

- [T1] `src/ui/mod.rs` / `layout.rs` / `composer.rs`：旧圆框和两行Footer的实际入口。  
  https://github.com/zqcli/minicore-tui/blob/2b8268dbba81c162b30e984b9b31a58ebc3bba65/src/ui/mod.rs
- [T2] `src/ui/transcript.rs`：`build_durable_lines`、`live_section`、`last_result_lines`。  
  https://github.com/zqcli/minicore-tui/blob/2b8268dbba81c162b30e984b9b31a58ebc3bba65/src/ui/transcript.rs
- [T3] `src/ui/footer.rs`：现有Footer的数据与布局。  
  https://github.com/zqcli/minicore-tui/blob/2b8268dbba81c162b30e984b9b31a58ebc3bba65/src/ui/footer.rs
- [T4] `src/state/composer.rs` / `src/state/transcript.rs` / `src/app.rs`：单文本状态、render cache及App单写入者。  
  https://github.com/zqcli/minicore-tui/blob/2b8268dbba81c162b30e984b9b31a58ebc3bba65/src/state/composer.rs
- [A1] Agent `src/history.rs`：当前字段缺口、保留的内部Tool arguments与sanitize边界。  
  https://github.com/zqcli/minicore-agent/blob/b2e23938d073ab21c2775faa623561ba929a5ed1/src/history.rs
- [A2] Agent `src/event.rs`：当前ToolStarted/Finished/Event边界。  
  https://github.com/zqcli/minicore-agent/blob/b2e23938d073ab21c2775faa623561ba929a5ed1/src/event.rs
- [R1] Runtime `src/tools/context.rs`：ToolContext只有cancellation/deadline/progress。  
  https://github.com/zqcli/minicore-runtime/blob/87f3cf92b9b5980b0f468174a319cf53427d858e/src/tools/context.rs

---

## 19. 可直接交给开发 Agent 的任务摘要

在 `minicore-tui` 当前代码上做 Rail UI 等价显示改造。以本文固定的 `pi-rail-ui` 源码和用户截图为参照，不再沿用默认Pi圆角Editor/双行Footer。保留现有执行协议、Request分组、Steer、模型更新、History对齐、Blocked和终端恢复。

先建立source reference fixtures，再统一surface/layout/hit geometry；实现User/Thinking/Text/Tool原样式与折叠；Editor无框蓝rail、原生编辑行为、paste和slash补全；Footer严格单行及原格式；最后补选择/复制/蓝scrollbar和真实混合loop截图。

缺少的Tool detail、timestamp、branch等由Agent只读展示补充提供。Runtime不改，TUI不读取Store/Workspace、不启动工具。没有数据就显示unknown，不伪造截图数值、不实现Subagent引擎。所有新增UI状态只由App::update修改，不建设通用插件/主题/组件框架。最终逐项报告真实差异，而不是用“类似Pi”作为验收结论。
