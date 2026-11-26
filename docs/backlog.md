# MiniCore 待开发功能与缺陷修复清单

> **文档定位**：MiniCore（`minicore-host` / `minicore-tui` / `minicore-web`）统一研发待办清单（Roadmap & Bug Backlog），仅用于需求规划与问题追踪。  
> **更新日期**：2026-11-26  
> **当前基线**：`0.12.16`（`minicore-host` `e38e7f3` / `minicore-tui` `1591029` / `minicore-web` `9412cd0`）  
> **实测来源**：
> 1. 会话 `fjsg8pzmcz8g5dpgmt31`（多智能体代码审查 + 真实浏览器 Web 桌面/移动双端巡检）
> 2. 会话 `gg3933bnvh2vdsqm732m`（1 主 + 4 子独立项目长程协作编排实测）
>
> **状态约定**：`[ ]` 待开发 · `[-]` 进行中 · `[x]` 已完成

---

## 一、待开发新功能需求（Features）

### `F1` [P1] 优化 `agent` 工具 Action 体系与委派协议（`minicore-host` 主导，三端联动）

- [ ] **`F1` [P1] 优化 `agent` 工具 6 大 Action 的语义边界、参数易用性与路由安全**
  - **背景与问题来源**：
    - 现有 `agent` 工具包含 `create`、`send`、`inspect`、`wait`、`update`、`control` 6 个 action（见 `minicore-host/crates/host-tools/src/schema.rs` 与 `action.rs`）。
    - 在两轮真实长程编排实测（`fjsg8pzm` 与 `gg3933bn`）中，暴露出 5 类高频摩擦与安全隐患：
      1. **`create` 分支易误选且参数层级易错**：`resource: "agent"` 强制必填 `mode`，导致模型在 `gg3933bn` 中误选 `resource: "agent"` + `mode: "persistent"`（残留 4 个持久化 Agent/会话）；顶层字段与 `settings` 嵌套边界不直观，导致首次批量 `create` 4 次全错；允许不带 `text` 创建空壳，导致 4 个子会话创建后原地空转 81 秒。
      2. **`send` 缺少专用父会话路由目标且未限制跨委派树投递**：子会话 `tasktrail-renderer` 误传 `target: {kind: "agent_default"}` 导致消息串台并意外唤醒无关默认会话 `c7rxcj3a`（白跑 65s `max` 推理），同时主会话漏收汇报。
      3. **`inspect` 默认返回过重、缺乏批量态查询且对已释放 `memory` 会话报错**：两次实测均向主上下文灌入 `116KB–118KB` 冗余数据；子会话回收后查询报 `AGENT_SESSION_NOT_FOUND`。
      4. **`wait` 使用率偏低、缺少面向子任务集合的等待语义**：`gg3933bn` 全程 `0` 次调用 `wait`，改用 21 次 `inspect` 轮询；模型不明确何时该 `wait` 何时该 `inspect`。
      5. **委派权限缺少硬隔离**：子会话为调用 `send` 汇报不得不保留完整 `agent` 工具，无法硬禁止 `create` 派生。
  - **优化设计方案**：
    1. **收敛并清晰化 `create` 委派入口**：
       - 工具描述与示例强引导“任务委派统一用 `resource: "session"`（默认 `memory`）+ 同步传入 `name` 与初始 `text` 一步启动”；
       - 明确区分“临时任务委派（`session` + 默认 `memory`）”与“显式新建持久化 Agent（`agent` + `persistent`，仅在用户明确要求时使用）”；
       - 校验错误提示与 schema 示例明确标出 `settings` 嵌套结构，减少模型参数放错层级的无效重试。
    2. **升级 `send` 路由语义与隔离边界（联动 `H7`）**：
       - 为委派子会话新增专用 `target: { kind: "parent" }`（Host 自动绑定至当前委派关系的精确父 `agent_id + session_id`），彻底消除手拼父会话 ID 或误用 `agent_default` 的风险；
       - 在委派作用域内强制禁止通过 `agent_default` 或越界 `session_id` 向委派树外投递消息。
    3. **重构 `inspect` 为“默认轻量摘要 + 按需详情 + 批量状态”（联动 `H2` / `H3`）**：
       - 默认返回 ~`1–2KB` 紧凑摘要（运行状态、当前轮次/耗时、最近进展摘录、终态交付标记），提供显式 `detail: "full"` / `history_page` 拉取细节；
       - 支持批量查询或 `list_children` / `scope` 状态概览，一次调用即可掌握全部子任务进度；
       - 对已释放 `memory` 子会话返回语义化关闭状态（`lifecycle: "closed"`, `ephemeral_released: true`），不再误报归属错误。
    4. **增强 `wait` 并优化编排引导（联动 `H10`）**：
       - 明确引导主会话派发后优先使用 `wait` 阻塞等待子任务回报或完成，避免高频轮询 `inspect`；
       - 评估支持可选等待条件（如等待任意子任务完成/发消息，或指定子会话集合变更返回），提升多路编排精准度。
    5. **委派深度与派生权限硬控制（联动 `H4` / `H9`）**：
       - 在 `create(resource: "session")` 中支持可选 `max_delegation_depth`（如 `0` 表示禁止再派生）或 `allow_spawn: false`；Host 在工具执行层硬拒绝 `create`，同时保留 `send(target: parent)` 汇报能力；
       - 在 `scope.rs` 补齐单根作用域活跃子任务数量上限 `max_active_children`。
  - **验收标准**：
    - 模型一次创建并启动子任务成功率显著提升，不再无故产生持久化 Agent/会话残留；
    - 子会话无法向委派树外会话串台投递；
    - 单轮 4 子任务巡检注入主上下文体积从 `30–68KB` 降至 `<8KB`；
    - 禁止派生参数生效后，子会话调用 `agent(create)` 直接被 Host 拦截并返回明确错误，但 `send` 汇报正常。

---

### `F2` [P1] 新增 `/usage` 全局使用情况统计功能与可视化界面（三端协同：`Host` + `TUI` + `Web`）

- [ ] **`F2` [P1] 实现 `/usage` 跨会话用量统计（模型、Key/Provider、GitHub 按天热力图、Token/缓存、会话明细）**
  - **产品目标**：
    - 统一统计当前 Host 数据目录下**所有会话**（包含历史持久化会话、当前活跃会话及可追踪委派子任务）的使用情况；
    - 参考市面上成熟 AI 编程工具与用量分析产品（Claude Code `/usage` & `ccusage`、Codex CLI usage、OpenAI / Anthropic Console、GitHub Contribution Graph、Linear Insights）的交互与信息层次，打造**自上而下排列、信息清晰、克制美观**的 TUI + Web 双端统计视图。
  - **架构边界（严格遵守 `AGENTS.md`）**：
    - **`minicore-host` 拥有统计聚合与缓存**：新增原生只读查询接口（如 `usage.summary` / `usage.query`），负责扫描/增量索引会话历史并产出结构化统计快照；
    - **`minicore-tui` 与 `minicore-web` 只负责展示与交互**：UI 渲染层严禁直接读盘或遍历 `~/.minicore/data/sessions`；
    - **凭据安全红线**：Key 维度**绝不存储、传输或展示任何 API Key 明文**，仅展示配置中的 `provider_id` + `api_key_env` 变量名（如 `cus-resp / CUS_RESP_API_KEY`）或非可逆脱敏配置 Profile 标签。

  - **核心统计维度与指标定义**：
    1. **时间范围筛选**：支持 `7D`（近 7 天）、`30D`（近 30 天，默认）、`90D`、`1Y`（近 365 天）、`All`（全部历史）；支持切换“含委派子会话 / 仅主会话”。
    2. **Token 与缓存指标**：
       - `input_tokens`（未命中缓存输入）、`cache_read_tokens`（缓存命中读取）、`cache_write_tokens`（缓存写入）、`output_tokens`（输出）、`reasoning_tokens`（推理思考）、`total_tokens`；
       - `cache_hit_rate` = `cache_read / (input + cache_read + cache_write)`（直观看清缓存收益）；
       - 辅助指标：总请求数（`requests`）、总 Turn 数、失败/重试次数。
    3. **模型维度（By Model）**：按 `provider/model` 统计请求数、输入/输出/缓存/推理 Token、缓存命中率、占比条、活跃会话数。
    4. **Key / Provider 维度（By Key / Profile）**：按 `provider` + `api_key_env`（或 Provider 配置标识）聚合总 Token、请求数、涉及模型列表与最后使用时间，方便多中转站/多 Key 额度核对。
    5. **按天热力图（GitHub Contribution Style）**：按自然日（本地时区）聚合每日总 Token 与会话数，映射到 5 阶热力强度（`0` 无活动 / L1 / L2 / L3 / L4 峰值），计算总活跃天数、当前连续活跃天数（Current Streak）、最长连续天数（Longest Streak）与单日峰值。
    6. **会话统计与排行（Sessions）**：总会话数（主会话 + 委派子任务）、平均单会话 Token / 轮次，以及 Top Sessions 明细表（支持按总用量 / 最近活跃 / 请求数排序，并支持一键跳转打开目标会话）。

  - **自上而下（Top-Down）界面布局设计**：

    整体遵循**五层垂直流式卡片结构**，从“全局一眼结论 → 时间热力规律 → Token 构成趋势 → 模型与 Key 拆解 → 具体会话下钻”逐级展开，桌面端居中定宽（如 `max-width: 1120px`）呼吸感充足，移动端与 TUI 窄屏自动单列折叠：

    ```text
    ┌────────────────────────────────────────────────────────────────────────────┐
    │  Usage  ·  All Sessions                  [7D] [30D*] [90D] [1Y] [All]  ↻  │
    │  Filter: [All Models ▾]  [All Keys ▾]  [☑ Include Delegated]              │
    ├────────────────────────────────────────────────────────────────────────────┤
    │  ① 核心 KPI 概览（等宽指标卡，自左向右，窄屏自动换行）                        │
    │  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐ ┌──────────────┐      │
    │  │ Total Tokens │ │ Cache Hit    │ │ Sessions     │ │ Active Days  │      │
    │  │ 14.82M       │ │ 84.6%        │ │ 42 main      │ │ 18 / 30d     │      │
    │  │ In 1.9M ·Out │ │ Read 12.1M   │ │ +68 tasks    │ │ Streak 7d 🔥 │      │
    │  │ 412K ·Rsn 408│ │ Write 0.4M   │ │ 618 reqs     │ │ Peak 3.98M/d │      │
    │  └──────────────┘ └──────────────┘ └──────────────┘ └──────────────┘      │
    ├────────────────────────────────────────────────────────────────────────────┤
    │  ② GitHub 风格按天用量热力图（Daily Activity Heatmap）                       │
    │  14.82M tokens in the last 365 days               Less  ░ ▒ ▓ █  More     │
    │       Jun   Jul   Aug   Sep   Oct   Nov                                    │
    │  Mon  ░ ░ ▒ ░ ▓ ▒ ░ ░ ▒ ▓ █ ▓ ▒ ░ ▒ ▓ ▒ ░ ▓ █ ▓ ▒                          │
    │  Wed  ░ ▒ ▓ ▒ ░ ▒ ▓ ▒ ░ ▒ ▓ ▒ ░ ▓ █ ▓ ▒ ░ ▒ ▓ █ ▓                          │
    │  Fri  ░ ░ ▒ ░ ▒ ▓ ░ ░ ▒ ░ ▒ ▓ ░ ▒ ▓ ▒ ░ ▒ ▓ ▒ ▓ █                          │
    │  Selected: 2026-11-26 · 5.42M tokens · 3 sessions · Top: gpt-6.1-sol       │
    ├────────────────────────────────────────────────────────────────────────────┤
    │  ③ Token 构成与每日趋势（Stacked Daily Bar · Input / Cache / Output / Rsn） │
    │  [Tokens*] [Requests]                                                      │
    │  4M ┤                                          █  █  ■ Uncached Input      │
    │  2M ┤              ▓      ▓        ▓  █        █  █  ■ Cache Read          │
    │  1M ┤   ▒   ▒  ▓   █   ▒  █   ▒    █  █   ▒    █  █  ■ Cache Write         │
    │  0  ┴───┴───┴──┴───┴───┴──┴───┴────┴──┴───┴────┴──┴  ■ Output  ■ Reasoning │
    │       11/15   11/18   11/21   11/24      11/26                             │
    ├────────────────────────────────────────────────────────────────────────────┤
    │  ④ 模型与 Key/Provider 分布（桌面双列并排 · 移动/TUI 上下堆叠）               │
    │  ┌─ By Model ──────────────────────┐ ┌─ By Key / Provider ───────────────┐ │
    │  │ cus-resp/gpt-6.1-sol      58%   │ │ cus-resp · CUS_RESP_API_KEY  76%  │ │
    │  │ ████████████░░░░░░░░  8.6M      │ │ ███████████████░░░░░  11.3M       │ │
    │  │ 142 req · Cache 91% · Out 84K   │ │ 412 req · 28 sessions · 2m ago    │ │
    │  │                                 │ │                                   │ │
    │  │ cus-resp/gpt-6-luna       31%   │ │ openai · OPENAI_API_KEY      24%  │ │
    │  │ ██████░░░░░░░░░░░░░░  4.6M      │ │ █████░░░░░░░░░░░░░░░   3.5M       │ │
    │  │ 210 req · Cache 79% · Out 62K   │ │ 206 req · 14 sessions · 1d ago    │ │
    │  └─────────────────────────────────┘ └───────────────────────────────────┘ │
    ├────────────────────────────────────────────────────────────────────────────┤
    │  ⑤ 会话统计排行（Top Sessions · 支持排序与回车/点击跳转）                     │
    │  Session                    Model        Tasks  Total    Cache  Last Active│
    │  ▸ 代码审查 0.12.16 (fjsg)  gpt-6.1-sol    14   3.98M     95%   20:57      │
    │    TaskTrail 编排 (gg39)    gpt-6-luna      4   1.31M     84%   21:42      │
    │    ...                                                                     │
    └────────────────────────────────────────────────────────────────────────────┘
    ```

  - **双端交互与视觉规范**：
    - **Web 端**：
      - 入口：支持输入框 `/usage` 命令 + 左侧栏底部轻量导航入口（不与现有 Tasks/Settings 重复）；
      - 视觉：沿用现有亮/暗双主题语义 Token（CSS 变量），热力图与堆叠柱状图采用 SVG/Canvas 清晰绘制，Hover 单元格显示精确日期 Tooltip，点击某天可联动过滤下方会话列表，点击会话行直接切换进入对应会话；
      - 响应式：桌面双列卡片 + 52 周完整热力图；移动端自动切换为 2×2 KPI 栅格 + 横向可滑动热力图 + 上下堆叠卡片。
    - **TUI 端**：
      - 入口：输入 `/usage` 打开全屏只读视图（遵循 `docs/ui-spec.md` 与 `/diff` 视图一致的交互约定）；
      - 键盘操作：`1`–`5` 快速切换时间范围，`Tab` 切换焦点区域，`↑↓` / `PgUp` / `PgDn` 滚动视图，热力图支持方向键移动光标查看单日明细，会话列表按 `Enter` 跳转到目标会话，`Esc` / `q` 退出返回原会话；
      - 绘制：自适应终端宽度（窄终端自动缩短热力图展示周数、单列展示模型与 Key 卡片），纯字符块 + 主题调色板渲染，无定时器轮询。
  - **性能与数据完整性要点**：
    - Host 端引入轻量持久化聚合缓存（按会话文件 `mtime`/位置增量更新），避免历史会话增多后打开 `/usage` 卡顿；
    - 历史会话记录中未携带 `api_key_env` 字段的旧数据归类为 `<provider> (legacy / unknown key)`，不伪造数据；
    - `memory` 模式下已回收子任务的用量应在委派完成交接/Host 增量账本中记录摘要，避免回收后统计口径缺失。

---

## 二、实测会话运行画像（问题溯源依据）

### 2.1 会话一：`fjsg8pzmcz8g5dpgmt31`（多智能体代码审查 + Web 实测）

| 指标 | 数值 |
|---|---|
| 总耗时 | `23m 24s`（`20:34:21` → `20:57:45`，单次 Root Turn） |
| 模型配置 | Root：`cus-resp/gpt-6.1-sol`（`max`）；14 个委派子/孙会话：`gpt-6.1-terra`（`max`） |
| Root 轮次与工具调用 | 36 次模型请求，35 轮工具；`bash` × 34、`agent` × 31（`create`: 4, `inspect`: 10, `send`: 11, `wait`: 6） |
| 工具失败数 | 1 次（`agent(inspect, 8hygbjmh)` 在子会话关闭 9 秒后调用 → `code: 2202` `Session does not belong to this Agent`） |
| Root Token 消耗 | 未命中缓存输入 `170,224` · 命中缓存 `3,807,104` · 输出 `16,006` · 推理 `10,538`（峰值上下文约 `192K`） |
| 瞬时重试 | 3 次 `model.retry_scheduled`（L2 `2schen0m` 1 次、Root 2 次），均在 1s 后第 2 次尝试自动恢复 |
| 拓扑（15 会话） | `1 Root + 4 L1 + 10 L2`（递归深度上限 `2` 生效；14 个委派全部 `memory` 回收，零磁盘残留） |

### 2.2 会话二：`gg3933bnvh2vdsqm732m`（1+4 长程协作编排实测）

| 指标 | 数值 |
|---|---|
| 任务内容 | 在 `/tmp/agent-orchestration-lab.reHddZ` 独立实现无依赖 Python CLI `TaskTrail`，中途响应用户 steer 与主仓库隔离 |
| 总耗时 | `10m 30s`（`21:32:21` → `21:42:51`，单次 Root Turn） |
| 模型配置 | Root：`cus-resp/gpt-6-luna`（`max`）；4 个 L1 子会话：`cus-resp/gpt-6-luna`（`low`），`0` 个 L2 孙会话 |
| Root 轮次与工具调用 | 26 次模型请求，25 轮工具；`agent` × 43（`create`: 8 [4 错 + 4 成], `inspect`: 21 [116KB], `send`: 14, `wait`: 0, `close`: 0） |
| Root Token 消耗 | 未命中缓存输入 `209,501` · 命中缓存 `1,089,664` · 输出 `10,325` · 推理 `15,824`（峰值上下文 `89,917`），零重试 |
| 交付与异常 | 26/26 单测通过；暴露 ① `renderer` 误用 `agent_default` 意外唤醒无关会话 `c7rxcj3a`（65.4s / 4 reqs）；② 显式传 `mode: "persistent"` 残留 4 个持久化 Agent/会话 |

---

## 三、待修复缺陷清单（Bug Backlog）

总览：**24 项 P1/P2 缺陷**（`minicore-web` 6 项 · `minicore-host` 10 项 · `minicore-tui` 8 项）+ **4 项 P3 优化备忘**。

---

### A. `minicore-web`（P1 × 2，P2 × 4）

- [ ] **`W1` [P1] 打开已结束的 `memory` 任务自动滚到底部，裁掉卡片头部与报告开头**
  - **位置**：`minicore-web/web/tasks.js`（`L477-L522`）
  - **发生场景**：在 `fjsg8pzm` 结束后，用户通过 Web 打开 `Tasks` 抽屉并点击任意已完成的 `memory` 子任务（如 `review-tasks-delegation` 或 `review-transcript-rendering`）查看最终报告。
  - **现象与根因**：详情容器初次挂载时内容为空，`wasBottom` 计算为 `true`；渲染完静态 `.tasks-delivered` 交付结果卡片后无条件执行 `scrollTop = scrollHeight` 贴底滚动，导致超过一屏的长报告直接跳到末尾，卡片头部与开头结论全部被裁出视口。
  - **影响**：每次查看已完成任务都必须手动滚回顶部，易误以为头部信息丢失。
  - **改进方案**：仅在任务处于实时 `running` 流式输出阶段自动贴底跟随；已结束任务或 `.tasks-delivered` 回退卡片初次渲染时固定 `scrollTop = 0`。

- [ ] **`W2` [P1] 已结束的 `memory` 任务显示红色错误条、顶部复制按钮置灰且底部保留无效输入框**
  - **位置**：`minicore-web/web/tasks.js`（联动 `H2`）
  - **发生场景**：在 `fjsg8pzm` 结束后打开 Web `Tasks` 详情页查看任意已完成的 `memory` 子任务（桌面/移动、亮/暗主题均必现）。
  - **现象与根因**：
    1. `memory` 子会话完成后已被 Host 释放，`session.get` 返回 `Session does not belong to this Agent`，前端直接渲染醒目的红色 `.tasks-error` 横幅；
    2. 顶栏 `Copy task result` 未回退读取 `parentDeliveredResult(row)` 而置灰禁用，卡片内部却又重复渲染了 `Copy result` 按钮；
    3. 底部 `.tasks-compose` 输入框在子会话已销毁后仍占据约 `70px` 高度且无法输入。
  - **影响**：正常成功的子任务看起来像报错失败，操作按钮不一致，有效阅读区域被无效输入栏浪费。
  - **改进方案**：
    1. 当 `row.status === 'completed'` 且存在 `parentDeliveredResult(row)`（或 Host 按 `H2` 返回 `ephemeral_released: true`）时抑制红条，改为卡片头部中性弱提示；
    2. `scopedResult()` 回退读取 `parentDeliveredResult(row)` 使顶栏复制按钮可用，并移除卡片内重复按钮；
    3. 子会话关闭/释放态隐藏 `.tasks-compose`（或折叠为单行只读说明）。

- [ ] **`W3` [P2] 移动端（`390px`）Tasks 详情副标题截断 `#id` 与 `from Main`**
  - **位置**：`minicore-web/web/tasks.js`、`minicore-web/web/styles.css`
  - **发生场景**：在移动端窄屏（实测 `390×844`）打开 `fjsg8pzm` 的子任务详情页顶部栏。
  - **现象与根因**：副标题顺序为 `Completed · Assistant · #8hyg...`，默认 Agent 名称 `Assistant` 占据中间主要宽度，挤掉了短 ID 和 `from Main` 来源信息。
  - **影响**：移动端无法一眼辨别当前子任务 ID 与派发方。
  - **改进方案**：调整顺序为 `Status · from <dispatcher> · #shortId`，在 `<640px` 窄屏下省略默认 Agent 名称或移至次级行。

- [ ] **`W4` [P2] 主对话流 `.u.task-notice` 显示原始 ID，未展示子任务短名称**
  - **位置**：`minicore-web/web/render.js`
  - **发生场景**：在 `fjsg8pzm` 主对话时间线中查看子任务中途回传与最终交付提示块。
  - **现象与根因**：提示块渲染原始 `Agent input · stp2pp50`，未查表关联 `DelegationAssociation.name`（如 `review-tasks-delegation`）。
  - **影响**：主时间线与 `Tasks` 面板命名割裂，用户需自行对照 8 位随机短 ID。
  - **改进方案**：关联显示 `<name> (#<shortId>) · Result delivered`；若发生多次中间 `agent(send)` 交互，在 `Worked` 折叠摘要中补充互动次数。

- [ ] **`W5` [P2] 悬浮回到底部按钮 `#jump` 与底部文本行重叠**
  - **位置**：`minicore-web/web/styles.css`
  - **发生场景**：在桌面端或移动端向上滚动浏览 `fjsg8pzm` 主对话长报告，位于底部 `Tasks · 4 completed` 状态条上方。
  - **现象与根因**：`#jump` 水平居中悬浮在正文最底行正上方，未避让文字内容列。
  - **影响**：阅读历史内容时底行文字常被按钮遮挡。
  - **改进方案**：将 `#jump` 对齐至内容区右边缘，或增加底部安全留白偏移。

- [ ] **`W6` [P2] 侧边栏大量 `Untitled session` 导致历史会话难以辨识**
  - **位置**：`minicore-web/web/sidebar.js`（配合 `H6`）
  - **发生场景**：在 Web 左侧边栏切换或查找历史会话（实测同时出现 9 条 `Untitled session`）。
  - **现象与根因**：未手动 `/rename` 时会话 `title` 为空，前端统一展示 `Untitled session`。
  - **影响**：必须逐条点开才能区分会话内容。
  - **改进方案**：配合 Host 自动生成默认标题（`H6`）；前端对历史空标题会话可回退展示首条用户消息截断摘要。

---

### B. `minicore-host`（P1 × 6，P2 × 4）

- [ ] **`H1` [P1] Remote WS/WSS 在认证前关闭帧/消息长度上限，未认证连接可触发 DoS 或拖垮监听**
  - **位置**：`minicore-host` `host-transport`（`serve_ws` / `serve_wss`），调用方 `minicore-tui/src/host/remote.rs`（`L108-L111`）
  - **发生场景**：通过 `/remote` 开启本地远程监听后，任意未认证客户端在完成 `initialize` 密钥校验前发送声明超大长度的 WS 帧或消息。
  - **现象与根因**：建连阶段即放开了 `max_frame_size` / `max_message_size`，`tungstenite` 按帧头长度分配内存，且单连接异常可能扩散影响监听主循环。
  - **影响**：单个未认证客户端即可制造内存放大 DoS 或令远程服务中断。
  - **改进方案**：认证前施加严格帧/消息上限（如 `64 KiB`），认证后切换合理协议上限，并隔离单连接任务错误防止扩散。

- [ ] **`H2` [P1] 已回收的 `memory` 子会话被 `inspect` / `session.get` 查询时返回误导性错误 `AGENT_SESSION_NOT_FOUND`**
  - **位置**：`minicore-host` 委派生命周期与 `inspect` / `session.get` 处理器
  - **发生场景**：① `fjsg8pzm` 中子会话 `8hygbjmh` 关闭 9 秒后主会话调用 `agent(inspect)`；② 任务完成后 Web/TUI 点开已结束的 `memory` 子任务详情。
  - **现象与根因**：`memory` 会话释放后无终态留存，后续查询直接返回 `code: 2202`（`Session does not belong to this Agent`）。
  - **影响**：主会话折叠条出现误导性 `1 failed`，Web 端全员弹出红色错误横幅（联动 `W2`）。
  - **改进方案**：在活跃根作用域及委托归档中保留轻量终态记录，返回语义化终态（`lifecycle: "closed"`, `ephemeral_released: true`、完成时间与交付摘要）。

- [ ] **`H3` [P1] `agent(inspect)` 默认返回体过大，两轮实测均向主上下文灌入超 116KB**
  - **位置**：`minicore-host/crates/host-tools/src/core_adapter/inspect.rs`（联动 `F1`）
  - **发生场景**：① `fjsg8pzm` 10 次 `inspect` 注入超 `118KB`（单次 4 子会话达 `68.5KB`），推高主上下文至 `192K`；② `gg3933bn` 21 次 `inspect` 再次注入 **`116,417` 字节**（单轮 4 子会话达 `30–38KB`）。
  - **现象与根因**：默认序列化了过长且重复的历史片段与完整元数据，而调度检查只需状态与简短进展。
  - **影响**：浪费 Token 与缓存空间，挤占主会话有效上下文并提前触发压缩。
  - **改进方案**：默认返回约 `1–2KB` 的紧凑结构化摘要，通过显式参数 `detail: "full"` / `history_page` 按需拉取全文。

- [ ] **`H4` [P1] 多级扇出失控（1→4→10 = 15 会话）与主/子工作重复**
  - **位置**：`minicore-host/crates/host-tools/src/scope.rs`、`minicore-host/crates/host-tools/src/schema.rs`（联动 `F1`）
  - **发生场景**：`fjsg8pzm` 仅需对单仓库做 4 模块审查，4 个 L1 全部再派 2–3 个 `max` 推理 L2 膨胀成 15 会话；同时主会话在等待期间自行在 `/tmp` 编译 `cargo test`（占用 `3.6GB`）并提前审查，拉长总耗时至 `23m 24s`。
  - **现象与根因**：仅限递归深度（depth ≤ 2），未限制单根作用域活跃委派总数，也缺少避免 L1 无意义扇出与主会话越界重复干活的约束。
  - **影响**：时间、算力成本与限流重试风险成倍放大。
  - **改进方案**：在 `scope.rs` 增加单根作用域 `max_active_children` 上限（建议默认 `6–8`），并在 `schema.rs` 明确 L1 优先直做、L2 必须命名、Root 专注协调不重复干活。

- [ ] **`H7` [P1] 委派子会话误用 `agent_default` 路由目标，导致跨会话串台唤醒无关会话并丢失汇报**
  - **位置**：`minicore-host` `agent(send)` 路由解析与委派作用域校验逻辑（联动 `F1`）
  - **发生场景**：`gg3933bnvh2vdsqm732m` 中 `tasktrail-renderer`（`qcjymns2`）于 `21:37:34` 汇报中途进展时（前一次 `21:37:25` 还因漏传 `action` 报错），将目标误写成 `target: {kind: "agent_default", agent_id: "xy8ags1n7x1c6httvcd5"}` 而非精确父会话 `gg3933bn`。
  - **现象与根因**：Host 未校验委派子会话 `send` 目标是否属于当前委派树，直接投递到父 Agent 默认会话 `c7rxcj3a`。
  - **影响**：无关历史会话 `c7rxcj3a` 在用户不知情下被唤醒跑了一整个 4 请求、`65.4s` 的 `gpt-6-luna (max)` Turn 且上下文被污染；主会话 `gg3933bn` 完全漏收该进度汇报。
  - **改进方案**：新增专用 `target: {kind: "parent"}` 自动解析精确父会话；强制委派树路由隔离，拒绝子会话经 `agent_default` 或越界会话投递。

- [ ] **`H8` [P1] 模型显式传 `resource: "agent", mode: "persistent"` 且收尾不清理，造成磁盘与侧边栏持久污染**
  - **位置**：`minicore-host` `agent(create)` schema 说明与委派元数据关联逻辑（联动 `F1`）
  - **发生场景**：`gg3933bnvh2vdsqm732m` `#23` 中，主会话无视 `0.12.16` 默认 `memory` 委派机制，显式调用 `create(resource: "agent", mode: "persistent")` 创建 4 个子任务且收尾未关闭/删除。
  - **现象与根因**：`resource: "agent"` 强制要求传 `mode` 诱使模型填 `persistent`；先建独立持久 Agent（不带 `text`）再 `send` 导致子 `session.json` 中 `parent: null`、`closed: null`，主会话 `delegations` 记录也未关联短名称。
  - **影响**：`~/.minicore/data/agents/` 与 `sessions/` 永久残留 4 个孤立 Agent 与未关闭会话，直接污染本地列表与侧边栏。
  - **改进方案**：引导任务委派统一走 `resource: "session"`（默认 `memory`），明确禁止无故传 `persistent`；补齐两步委派时的 `parent` 与短名称关联（并清理本次残留的 4 个 `tasktrail-*` 测试数据）。

- [ ] **`H5` [P2] `memory` 模式关闭或重连后 L2 孙任务不可见，且 `host.log` 缺少 `delegation_name` 与 `delegated_by`**
  - **位置**：`minicore-host` 委派归档索引与结构化日志
  - **发生场景**：`fjsg8pzm` 结束后通过 Web 重连查看 `Tasks`，以及查阅 `host.log` 还原三级委派树。
  - **现象与根因**：`memory` 释放后 Root 归档仅留 4 个直接 L1，10 个 L2 全部丢失；`host.log` 未记录 `delegation_name` 与 `delegated_by`。
  - **影响**：事后复盘与排障无法还原完整委派树。
  - **改进方案**：在 L1 交接与 Root 归档中附带轻量 `descendants` 摘要（`session_id`、`parent_session_id`、`name`、`outcome`、起止时间）；`host.log` 补齐 `delegation_name` 与 `delegated_by`。

- [ ] **`H6` [P2] 缺少默认会话自动标题**
  - **位置**：`minicore-host` 会话首条用户输入处理路径
  - **发生场景**：新建会话直接对话（如 `fjsg8pzm`、`gg3933bn`）未手动 `/rename`。
  - **现象与根因**：`title` 默认长期为空。
  - **影响**：侧边栏出现大排 `Untitled session`（联动 `W6`）。
  - **改进方案**：`title` 为空且收到首条用户消息时自动清洗截断生成初始默认标题（约 36 字符），保留 `/rename` 覆盖能力。

- [ ] **`H9` [P2] 委派权限缺少硬约束，“禁止继续派生”仅靠 Prompt 软约束**
  - **位置**：`minicore-host` 委派权限与深度控制（联动 `F1`）
  - **发生场景**：`gg3933bn` 明确要求 4 个子 agent 不再继续派生，但 4 个子会话的 `effective_config.tools` 均保留了完整 `agent` 工具（含 `create`）。
  - **现象与根因**：子会话为调用 `send` 汇报无法移除整个 `agent` 工具，Host 尚未解耦 `send` 与 `create` 权限。
  - **影响**：遇更主动模型时 Prompt 约束容易失效，导致失控扇出。
  - **改进方案**：委派参数支持 `max_delegation_depth: 0` / `allow_spawn: false`，硬禁止 `create` 同时保留 `send`。

- [ ] **`H10` [P2] 委派参数结构与同步节奏引导不足：先建空壳空转 + 参数层级易错 + 零次 `wait`**
  - **位置**：`minicore-host` `agent` 工具 schema 与提示词（联动 `F1`）
  - **发生场景**：`gg3933bn` 中 `+62s` 因把 `instructions` 放顶层导致 4 次 `create` 全报错；`+179s` 创建 4 个子会话未带 `text` 空转 **81 秒**（期间白查一轮 4 个空 `inspect`）；全程 **0 次 `wait`**、21 次 `inspect` 轮询；验收通过后又多发客套 `send` + `inspect` 白耗 ~36 秒。
  - **现象与根因**：顶层/嵌套字段易混淆，未强引导一步带 `text` 创建与 `wait` 阻塞等待。
  - **影响**：白白损耗 1–2 分钟墙钟时间与多余上下文/推理开销。
  - **改进方案**：在 schema 描述与错误提示中给出一步创建范式、优先 `wait` 准则与验收收敛原则。

---

### C. `minicore-tui`（P1 × 2，P2 × 6，P3 × 4）

> 以下条目均由会话 `fjsg8pzm` 审查定位，并已对照 `HEAD 1591029` 源码核实。

- [ ] **`T1` [P1] Tasks 列表实时重排可能导致草稿发送到错误的子会话**
  - **位置**：`minicore-tui/src/app/mod.rs`（`L718-L729`, `L903-L909`）
  - **发生场景**：在 `Tasks` 浮层选中子会话 A 输入草稿准备发送，提交瞬间另一子会话 B 状态变化触发列表重排。
  - **现象与根因**：`Overlay::Delegations` 用数字下标 `selected` 跟踪当前项，提交时未与草稿原目标身份核验，重排后原下标指向常驻子会话 B，导致把 A 的草稿发给 B。
  - **影响**：违反原始目标归属契约，产生跨会话误发送。
  - **改进方案**：选中态与草稿目标统一锚定 `AgentId + SessionId`，重排后按身份重算下标并在发送前强制校验一致。

- [ ] **`T2` [P1] Inspector 常驻会话 LRU 淘汰（>8）静默丢弃未发送草稿并破坏 F8 重发**
  - **位置**：`minicore-tui/src/app/resident.rs`（`L66-L78`）、`minicore-tui/src/app/mod.rs`（`L373-L382`）
  - **发生场景**：超过 8 个子任务（如 `fjsg8pzm` 共 14 个）时，在子会话 A 保留未发送草稿或未确认发送（F8 重发），随后连续打开查看其他超过 8 个子会话触发 LRU 淘汰。
  - **现象与根因**：淘汰最久未用常驻会话时直接丢弃 `Session.draft`、`returned` 及绑定原 epoch 的未确认记录。
  - **影响**：未发送草稿静默丢失，F8 重发上下文损坏。
  - **改进方案**：LRU 淘汰优先跳过含草稿/未确认态的会话，或在卸载时将其转存独立按地址索引表并在重加载时无损恢复。

- [ ] **`T3` [P2] 将 L1 加载到 `resident` 后，其 L2 孙任务从根 Tasks 列表中消失**
  - **位置**：`minicore-tui/src/app/agent_cards.rs`（`L29-L32`）、`minicore-tui/src/app/overlay.rs`（`L420-L430`）
  - **发生场景**：存在三级委派时点开查看某个 L1 详情（加载进入 `resident`），再切回根会话打开 `Tasks` 列表。
  - **现象与根因**：`register_delegation` 将 L2 卡片从 detached `observer.card` 迁入 L1 transcript，而 `cards_for_root` 只扫描 detached `observer.card`。
  - **影响**：查看一次 L1 后，其下属所有 L2 孙任务从根 Tasks 列表凭空消失。
  - **改进方案**：`cards_for_root` 同时聚合 detached observer 与已加载常驻子会话 transcript 中的委派卡片并按 `SessionId` 去重。

- [ ] **`T4` [P2] Diff 视图原地刷新在文件列表清空或变化时保留陈旧文件与内容**
  - **位置**：`minicore-tui/src/app/diff.rs`（`L284-L359`）、`minicore-tui/src/ui/views/diff.rs`（`L332-L345`）
  - **发生场景**：Diff 视图停留某变更文件时，外部提交/还原清空了工作区变更列表或移除了该文件，随后原地刷新。
  - **现象与根因**：刷新未清空失效的选中路径、旧内容缓存与滚动偏移。
  - **影响**：界面继续展示早已失效的旧 Diff。
  - **改进方案**：校验选中路径是否仍存在；失效时回落首项，列表为空时立即清空预览与滚动状态。

- [ ] **`T5` [P2] Diff 视图自动折行后总行数超过 `rows.len()` 滚动上限，底部内容无法滚到**
  - **位置**：`minicore-tui/src/app/diff.rs`（`L256-L260`）、`minicore-tui/src/ui/views/diff.rs`（`L347-L369`）
  - **发生场景**：窄终端打开含长行的 Diff 文件并向下滚动到底部。
  - **现象与根因**：滚动上限按未折行的逻辑行 `rows.len()` 计算，小于折行后的实际视觉总行数。
  - **影响**：文件尾部改动不可达。
  - **改进方案**：按当前视口宽度计算并缓存折行后的视觉总行数作为最大滚动上限，窗口尺寸变化时重算。

- [ ] **`T6` [P2] Markdown CJK 强调预处理向 4 空格缩进代码块注入 `U+E000` 且未清理**
  - **位置**：`minicore-tui/src/ui/markdown.rs`（`L172-L193`, `L500-L506`）
  - **发生场景**：模型输出使用 4 空格缩进（非围栏）的 Markdown 代码块且内含触发 CJK 强调预处理的字符组合。
  - **现象与根因**：预处理未跳过缩进代码块，插入的私用区占位符 `U+E000` 未被清理。
  - **影响**：终端渲染异常且划词复制代码会带入脏字符 `U+E000`。
  - **改进方案**：预处理识别并跳过 4 空格/Tab 缩进代码块，最终输出阶段兜底剥离占位符。

- [ ] **`T7` [P2] `/remote` 启动过程中执行 `/remote off` 无法取消启动**
  - **位置**：`minicore-tui/src/app/actions.rs`（`L1025-L1063`, `L1082-L1085`）
  - **发生场景**：执行 `/remote` 后监听尚未就绪（`remote_starting == true`, `remote == None`），立刻执行 `/remote off` 取消。
  - **现象与根因**：`/remote off` 未取消进行中的启动任务，延迟到达的 `RemoteStarted` 仍会开启服务并移交控制权。
  - **影响**：用户明确关闭远程后后台端口仍在监听。
  - **改进方案**：引入启动代际序号与取消标记；`/remote off` 置取消态，收到过期 `RemoteStarted` 时立即停止新监听且不移交控制权。

- [ ] **`T8` [P2] 窄屏（约 `45 cols`）下 Agent 派发行可能整行折叠为空白**
  - **位置**：`minicore-tui/src/ui/blocks.rs`（`L863-L884`）、`minicore-tui/src/ui/agent_cards.rs`（`L46-L65`）
  - **发生场景**：在约 `45` 列宽的窄分屏终端查看含子任务派发记录的主对话流。
  - **现象与根因**：先为右侧徽章预留完整宽度把左侧压到 `0`，随后右侧又因放不下被丢弃，左右全空。
  - **影响**：派发卡片行变成整行空白。
  - **改进方案**：左侧任务名保留最小可见宽度优先，右侧徽章按剩余空间降级或省略。

#### P3 备忘（低优先级细节优化）

- [ ] **`T9` [P3]** `card.task` 短名称回填逻辑阻止完整任务提示文本回填（`src/app/agent_cards.rs:323-334`）——短名称先写入后阻断完整 prompt 回填，详情页看不到原任务描述。
- [ ] **`T10` [P3]** 嵌套 Markdown 列表选中复制时丢失层级缩进（`src/ui/rails.rs:53-60`）——嵌套列表缩进被当 chrome 剥离，复制后拍平为一级。
- [ ] **`T11` [P3]** 子任务详情视图中鼠标拖拽划词选择被忽略（`src/app/mouse.rs:137-145`）——子详情视图无法鼠标划词复制。
- [ ] **`T12` [P3]** 流式输出缺口回补后缺少 `regroup` 调用（`src/app/control.rs:930-944`）——缺口补齐后未重组相邻块，折叠组可能分裂。

---

## 四、建议排期批次与验证约定

1. **第一批（安全、路由隔离与核心一致性 · P1）**：`H1` → `H7` → `H8` → `T1` → `T2` → `H2` + `W2` + `W1`
2. **第二批（`agent` 工具体系升级与编排效率 · P1/P2）**：`F1`（整合 `H3` + `H4` + `H9` + `H10`）→ `H5` + `T3` → `W4`
3. **第三批（`/usage` 全局统计功能 · P1）**：`F2`（`Host` 聚合接口与增量缓存 → `Web` 五层可视化页 → `TUI` `/usage` 全屏视图）
4. **第四批（体验打磨与边界修复 · P2/P3）**：`H6` + `W6` → `W3` + `W5` → `T4` ~ `T8` → `T9` ~ `T12`

**构建与运维约定**：
- Rust 编译、clippy、单测及 Intel Mac（`x86_64-apple-darwin`）交叉编译均在远端构建机执行，**不在本机 macOS 编译 Rust**。
- TUI 真实交互验证可在本机 iTerm2 进行；Web 端执行 `npm run check` / `npm test` / `npm run build` 与 Playwright 桌面/移动、亮/暗双主题回归。
- 本地测试残留提醒：会话 `gg3933bn` 遗留在 `~/.minicore/data/` 的 4 个 `tasktrail-*` 持久化 Agent/会话与 `/tmp/agent-orchestration-lab.reHddZ`、`/tmp/tasktrail-help.txt` 可按需清理。
- 历史测试提醒：`minicore-web` 既有 3 条涉及旧 `failure_paused` / compaction retry 语义的浏览器测试与 Host `0.12.14+` 行为存在历史偏差，后续统一对齐。

---

## 五、更新记录

| 日期 | 编号 | 状态变更 | 说明 |
|---|---|---|---|
| 2026-11-26 | `W1`–`W6`, `H1`–`H6`, `T1`–`T12` | 新建 | 基于会话 `fjsg8pzmcz8g5dpgmt31` 实测与代码审查整理初稿 |
| 2026-11-26 | `H7`–`H10` 及全部条目 | 补充细化 | 追加会话 `gg3933bnvh2vdsqm732m` 运行画像与 4 项新问题，补齐全部条目的场景/根因/影响/方案 |
| 2026-11-26 | `F1`, `F2` | 升级为待开发功能清单 | 升级为研发待办清单，新增 `F1`（优化 `agent` action 体系）与 `F2`（`/usage` 跨会话统计功能及自上而下五层界面设计） |
