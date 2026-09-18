# MiniCore TUI v0.3：全项目重构与基础 Coding 工作流实施 Spec

**修订：r1 · 2026-09-17**  
**实施仓库：`zqcli/minicore-tui`；目标版本：建议 `0.3.0`。**  
**范围：整个 TUI 项目，不仅是新增面板。允许内部 API、旧后端兼容和局部操作语义发生 breaking changes。**

> 保留现有 Rail 对话区、Editor、单行 Footer 和成熟终端交互；重构协议、状态、历史与工具数据路径、渲染和副作用执行，再完成常见 Coding 工作流。不重写 Agent/Runtime，不建设通用 UI 框架。

## 0. 如何使用本 Spec

开发 Agent 按第 27 节的顺序实施，每阶段保持可构建、可测试。阶段 A/B 是新后端正确性的前置门槛，不能只解除版本检查便宣称支持 Agent 0.5；阶段 C 是性能和状态收敛，阶段 D/E 才扩充用户操作。

文中 Rust 片段是**目标接口与所有权示意**，不是声称仓库已经存在的代码。已有等价实现应复用。Wire DTO 必须按固定 Agent 源码和真实进程 fixture 实现，不得照着示意类型猜 JSON 字段。

本 Spec 替代旧 TUI Spec 中的协议、数据组织和实现阶段。现有 Rail UI 的精确几何、颜色、交互快照继续作为视觉基线。旧 Agent/Runtime 的历史规格不是当前执行语义的依据。

本次完成了固定提交的源码与接口核对；没有在本地构建项目或执行性能基准。后文延迟和内存数值是**验收目标/应用预算**，不是已测结果。

---

## 1. 固定基线与重构目标

### 1.1 基线

| 项目 | 分支/提交 | 已读取版本 | 本轮处理 |
|---|---|---|---|
| minicore-tui | `dev@9d11ee69c4efa02ef1e5bff143662b48dc3194de` | 0.2.8 | 被重构对象 |
| minicore-agent | `dev@061743369459299e66be97bf97d2b27352a39914` | 0.5.0 / protocol 1 | 固定对接，不修改 |
| minicore-runtime | `dev@6cd2bdbc634437dea925495c61c7eb0be10ba171` | 0.4.1 | 仅理解语义，不引入依赖、不修改 |

开始开发先记录三个实际 HEAD；若已前进，先做协议差异检查。没有差异时更新 `docs/backend.md`；有差异时先修订 fixture 与本 Spec 的具体映射，不能在代码中暗加旧协议兼容分支。[S1][S2][S3]

### 1.2 必须交付的五个结果

1. **后端正确对接**：Protocol v1、准备/压缩、Turn 结果补读、分块历史、工具事实与输出、文件和 Diff。
2. **状态可理解**：执行、数据同步、视图分开；配置 reload 不再重装整份历史；所有可能产生副作用的操作不盲目重试。
3. **性能可验证**：Live 更新不复制全部历史；视口布局和命中共用数据；主循环不等待剪贴板、编辑器或发送队列容量。
4. **基础工作流完整**：继续会话、独立草稿、搜索/复制/导出、文件引用与预览、只读 Diff、工具详情、压缩入口。
5. **代码可维护**：一个 package，具体结构、普通函数、少量明确 enum；不用 Manager/Factory/Repository 层叠命名或通用插件框架。

### 1.3 保留、替换、新增

| 类别 | 内容 |
|---|---|
| 保留 | Rust/Ratatui/Crossterm/tui-textarea、Rail 样式、TerminalGuard、单 App owner、Request ID 分发、Session/Loop/Request/Tool 身份、模型更新、Steer、取消、Blocked 展示、既有 Unicode/鼠标修复 |
| 替换 | 0.3.x package-version 门禁、旧 History 主读取路径、全量 `PreparedConversation` 拼接、嵌套扫描 Tool、同步副作用、reload staged-history 事务、多组独立同步布尔值 |
| 新增 | Protocol v1 握手、完整分块读取、权威补读、有界查询、块级布局、独立草稿、常用搜索/导出、工具/文件/Diff 详情、准备与压缩操作 |

### 1.4 明确不做

本轮不实现审批 UI、自动批准、插件、MCP、Subagent、会话树/Fork、自动回滚、Git 写操作、完整 PTY、用户任意 `!shell`、图片、多客户端、云同步、自动重连、执行状态跨进程恢复。

不再新增 Follow-up 调度器。保留真正的 Steer，但**未发送的 Steer 不自动变成新 Prompt**。现有多条本地 Steer 可以保留为一个有界、绑定具体 Loop 的简单 FIFO；跨 Loop 自动继续属于本轮删除的隐式行为。

不删除 Agent 的任何接口，不迁移/重写其 Store，不要求 Agent 删除旧 `session.history`。只是新 TUI 的主读取路径不再依赖旧的有界展示页。

---

## 2. 三层职责与不可破坏的不变量

### 2.1 职责

| 层 | 拥有的事实与操作 |
|---|---|
| Runtime | 一次 AgentLoop、模型/工具循环、Request 边界、取消和 Steer 执行语义 |
| Agent | Session/History/保存、模型配置、工具/进程事实、Workspace、Changes、Context/Compaction、只读查询 |
| TUI | 终端、输入、草稿、显示、选择/滚动、RPC 客户端、明确的本地复制/导出/外部草稿编辑 |

TUI 不执行 Git，不读取 Agent 数据目录，不自行扫描 Workspace，不调用 Provider，不拼接或修改 Agent 的 summary 文件。唯一允许的本地文件写入是 TUI 配置、明确请求的导出和外部编辑器草稿临时文件。

### 2.2 不变量

- 一个进程只有一个 `App::update` 业务状态写入者；后台任务只返回结果。
- 一个 Session 同时至多一个运行 Loop；`TurnRef = session_id + loop_id`；Tool 始终使用完整四元组身份。
- `session.update` 只影响下一次真正发出的 Request，不能改写已运行 Request/Tool 的标签。
- Steer ACK 是接受，不等于已应用，更不等于已保存；只用真实 receipt 或最终结果确认。
- Event 是快速展示数据，不是完整历史；`turn.wait`/`turn.result`/读取结果是确认来源。
- 执行成功、主 History 保存成功、辅助工具数据保存成功、进程终止确认是不同事实。
- `persistence=failed` 不表示工具没有执行，也不保证磁盘完全没写；不得自动再跑。
- 关闭详情不取消工具；关闭 Session/退出 Agent 是显式生命周期操作。
- 缺失、部分、过期、未确认不能显示成 0、空成功或完整结果。

这些边界来自当前后端语义，不是可被前端方便性覆盖的约定。[S4][S5][S6]

---

## 3. 全项目目标组织

### 3.1 数据流

```text
Terminal input ─┐
RPC frame ──────┼─> App::update ─> 明确的 AppCommand
Local IO result ┤       │                 │
Layout result ──┘       │                 └─> RpcProcess / LocalJobs
                       ▼
               Session + History + View + Draft
                       │
               ConversationLayout / PanelLayout
                       │
                   Ratatui draw
```

`AppCommand` 只是本应用有限操作的 enum，不是通用 Effect runtime。

### 3.2 目录（沿用已有结构，不为目录一致性搬动无关文件）

```text
src/
  main.rs                 # 组装、select、公平调度、关闭
  terminal.rs             # 保留；增加外部编辑器 suspend/resume
  rpc.rs                  # 子进程、帧、发送准入、背压、读写/退出所有权
  protocol.rs             # 作为 protocol/ 门面；不必全局改 import
  protocol/
    handshake.rs          # Ping、Protocol v1、已知 capabilities
    session.rs            # Session/Context/Compact/Update
    read.rs               # session.read、turn.result、chunk decoder
    tool.rs               # ToolRef、read/output/process
    workspace.rs          # files/read/search/status
    changes.rs            # list/diff
    event.rs              # Agent Event wire enum
  app.rs                  # App、update 分发，不再容纳所有业务
  app/
    session.rs            # browse/open/create/close/delete/rename/update
    turn.rs               # submit/steer/cancel/wait/result/prepare
    queries.rs            # 两个只读在途槽、generation、合并刷新
    history.rs            # pin/window/增量/结果替换
    panels.rs             # 面板打开/关闭/焦点/查询结果路由
    ui_actions.rs         # 保留纯鼠标、折叠、选择入口；移走业务 IO
  state/
    session.rs            # 后端观察 + 少量本地操作状态
    history.rs            # HistoryWindow、Item/Turn 索引
    turn.rs               # LiveLoop/LiveRequest/Steer 状态
    tool.rs               # 单一 ToolKey -> ToolFacts 缓存
    view.rs               # SectionId、锚点、折叠、选择、布局索引
    composer.rs           # 保留编辑器包装、增量字节/粘贴状态
    panels.rs             # MainView、Focus、具体只读页状态
  ui/
    mod.rs / layout.rs    # 主区/Editor/单行 Footer；无业务 IO
    transcript.rs         # 保留名字；改成视口绘制
    conversation_layout.rs # 块级测量、行映射、共享缓存
    tool_detail.rs / file_preview.rs / changes.rs
    ...                   # 既有 Rail/user/assistant/reasoning/composer/footer
  jobs.rs                 # 剪贴板、草稿编辑器、导出；各一个 owned job
  export.rs               # Markdown 流式写出，不复制 UI 装饰
  config.rs               # 少量 TUI 偏好
  command.rs / keymap.rs / clipboard.rs / markdown.rs / theme.rs
```

上述新增文件在对应功能落地时创建，禁止先生成空模块/trait 占位。`protocol/` 子文件只放使用到的类型和转换函数，不复制整个后端 SDK。

### 3.3 核心对象

```rust
struct App {
    backend: BackendInfo,
    sessions: SessionsState,
    active: Option<SessionId>,
    pending: HashMap<RequestId, PendingRequest>,
    queries: QuerySlots,
    main_view: MainView,
    dock: Dock,
    focus: Focus,
    notices: Notices,
    preferences: UiConfig,
    dirty: DirtyState,
}

struct SessionView {
    info: SessionInfo,
    epoch: u64,
    access: SessionAccess,       // Browsing / Open / Opening / Closing
    observed: Option<SessionStateWire>,
    operation: LocalOperation,  // 本地未确认提交/Loop/手动压缩，不复制后端状态机
    history: HistoryWindow,
    live: Option<LiveLoop>,
    completion: Option<Completion>,
    config_update: Option<PendingConfigUpdate>,
    composer: Composer,         // 每会话一份草稿、undo、光标
    view: ConversationView,     // 锚点/折叠/选择，与业务状态分离
}
```

`operation` 表达 TUI 发出的尚未完成操作；`observed` 是后端最近一次观察。界面状态由一个 `session_activity(&SessionView)` 函数推导，不让 renderer 任意组合十几个布尔值。

### 3.4 状态整理

```rust
enum LocalOperation {
    None,
    Submitting(Submission),  // 有 request_id；可能尚无 LoopId
    Loop(ActiveTurn),
    Compacting(CompactOperation),
}

enum ReadState {
    Idle,
    Loading(ReadJob),
    Failed(ReadFailure),
}

enum Confirmation {
    Confirmed,
    NeedsRead,
    Unknown,
}
```

关闭/删除是独立生命周期请求，不用把所有组合塞入一个巨型 enum。历史补读失败不应自动阻止一个已确认 Open/Idle Session 的执行；是否可执行来自后端活动状态，不来自“所有历史是否加载完”。

现有 `reconcile_inflight`、`needs_post_wait_history`、`result_unconfirmed` 等应迁移到具体操作/读取状态，再删除多余字段。不能只把旧字段包进一个新 struct 而保留全部分支。

### 3.5 Generation 使用范围

读取请求记录 `{session_id, epoch, view_generation}`。关闭/重新打开 Session 增加 epoch；换文件、换 Tool、换搜索词增加该视图 generation。迟到的旧读取结果不覆盖新视图，但仍释放在途额度。

执行请求按精确 `TurnRef`、提交 ID、压缩 operation ID 结算，**不受面板 generation 过滤**。关闭面板或切 Session 不能使 `turn.wait`、保存失败或已接受 Steer 的响应被丢弃。

Catalog 列表更新只需一个 generation；本地成功 rename/delete 会使更早的 catalog 请求过期，不再永久保存越来越多的 title override/tombstone 集合。

---

## 4. 后端协议迁移：一次迁移，不做双栈

### 4.1 握手

`agent.ping` 必须读取 `version`、`protocol_version`、`capabilities`。要求 `protocol_version == 1`，移除 `is_supported_agent_version()` 中 `minor == 3` 的门禁。

本次正式验证对象是固定 Agent 0.5.0。Protocol v1 相同不等于任意未知实现自动兼容；能力检查通过后仍以本项目契约 fixture/E2E 作为发布验证。

基础必要 capabilities：

```text
session.read, turn.result, session.context,
tool.read, tool.output,
workspace.read, workspace.files, workspace.search, workspace.status,
changes.list, changes.diff, deferred.waiter_limit
```

缺少必要能力时给出明确诊断，不回退到 Agent 0.3。`session.compact` 等未单独列入 capabilities 的方法按固定 0.5 基线实现，不虚构 capability 名；若实际服务返回 method_not_found，禁用对应入口并报告不兼容。

模型 reasoning 以 `model.list.supported_reasoning` 为准，保留已有 minimal/xhigh/max 等已支持值，不退回旧的五个枚举值；选择只能提交服务端宣告的值，不自动降级。

### 4.2 RPC 使用范围

| 用户任务 | 方法 | 实现要求 |
|---|---|---|
| 启动/配置 | ping/reload/shutdown、model.list/profile.list | 不触碰历史存储 |
| 会话 | list/create/open/close/delete/rename/update/state | browse 与 execute 分开 |
| 历史与结果 | session.read、turn.wait、turn.result | 新主路径，完整分块读取 |
| Loop | turn.send/steer/cancel | 处理延迟 admission |
| 上下文 | session.context/compact/compact.cancel | 手动和自动准备都可见 |
| 工具 | tool.read/tool.output | 精确 ToolRef、按需读 |
| 文件 | workspace.files/read/search/status | 由 Agent 决定 Workspace 边界 |
| 改动 | changes.list/diff | 只读，cursor/ref 不透明 |
| 兼容展示 | session.presentation、现有 tool_presentation Event | 可继续用于 Rail 摘要；不是结果权威 |

`session.history` 可以留在 fixture/诊断中，但不再承担新主历史窗口或完整导出。禁止同时长期维护两份主历史列表。[S4][S5]

### 4.3 传输规则

保留 NDJSON、单 writer、单 stdout reader、独立 stderr/child owner。入站未知只读 Event 类型忽略；已知 Event 字段损坏、JSON 非法、帧中途 EOF 返回协议错误。Response 必须有唯一 result/error。

TUI 自己发送 u64 Request ID 即可。`id:null` 的服务端解析错误作为连接诊断处理，不错误关联某个发送请求。不要把 `params:null` 发给 Agent；空参数为 `{}` 或省略。

Backend Error 只展示安全 message/kind；`retryable=true` 是允许重新尝试的分类，不是自动重放执行操作的指令。

### 4.4 具体替换点

- `protocol.rs::PingResult`：增加协议版本和能力集合。
- `protocol.rs::is_supported_agent_version`：替换为 `validate_backend(&PingResult)`。
- `OutgoingRequest`：增加新查询/压缩方法构造；不把 Params 到处用 `json!` 散落。
- `AgentEventWire`：加入 tool_invocation/tool_execution/tool_process、当前 compaction 状态；保留现有 request_usage、steer_progress。
- 新原始 Read Item DTO 与旧 `HistoryItemView` **必须是不同类型**。`session.read` 的 User 使用 `input`，Assistant 使用 `content`，其 JSON 不是旧 history 展示 DTO。[S7][S8]

---

## 5. 主循环、RPC 背压和任务所有权

### 5.1 主循环禁止等待的工作

禁止在 `App::update`、draw、主线程命令分发中执行：剪贴板进程等待、外部编辑器等待、导出文件写入、大文本 Markdown/搜索/完整 item 解析、等待 RPC 发送队列空位。

保留已有事件批处理公平性：每轮最多 64 个已到达 RPC 事件或约 4ms，随后检查输入、控制意图和渲染 deadline。空闲时不持续 draw；Busy 最高 30fps，spinner 独立约 10fps。

### 5.2 发送准入

将 `RpcProcess::send().await` 在 UI 路径上的使用替换为同步快速准入：

```rust
fn try_send(&self, request: OutgoingRequest, class: SendClass)
    -> Result<(), SendError>;
```

保留单 FIFO writer，容量建议 32；普通操作只使用前 28 个槽，4 个槽保留取消/关闭/退出等控制操作。**保留容量不等于重排已入队请求**；update→steer 等依赖仍按原顺序发送。

发送前检查序列化后整行不超过 1 MiB。队列满且尚未接受，属于确定未发送：保留草稿/操作输入并提示 Busy；同一只读刷新合并。控制意图按精确目标保留一份，下次有空间重试入队，禁止每次 Esc 生成一条新取消请求。

已入队之后的写失败可能存在部分写入：移除连接的正常执行能力，标记结果未确认；不能自动重新发送。Request ID 在准入前登记，准入失败经一个处理函数撤销登记，不漏 Pending。

### 5.3 只读查询额度

Agent 同时最多 4 个 read query、32 个总 deferred。TUI 正常只使用 **2 个只读在途槽**：交互前景和历史/结果补读共享；执行 wait 单独计数。总 outstanding deferred 目标不超过 16。[S4]

实现一个具体 `QuerySlots`，提供：

```rust
fn request_query(&mut self, key: QueryKey, request: ReadRequest);
fn on_query_finished(&mut self, request_id: RequestId);
fn invalidate_scope(&mut self, scope: ReadScope);
```

`QueryKey` 是本应用有限读取对象的 enum。重复同对象刷新只记录一个 refresh-needed；不用通用优先级调度器。前景交互不被后台读取饿死，持续工具输出也不能永久占满两个槽。

关闭视图只停止后续页/轮询并使结果过期；它不取消已经在 Agent 执行的 query。**该请求收到响应或连接结束前仍占额度**。没有通用 `rpc.cancel` 就不能假装远端查询已取消。

`resource_exhausted`/`query_limit`：显示可重试状态，停止分页风暴；用户重试或下一个正常刷新周期再申请。不得为一个失败请求启动递归 fallback。

### 5.4 输入接收内存

保留 32 MiB 单 stdout frame 上限；对未处理帧另外计入 64 MiB 总 wire-byte 预算，不能只按 128 个事件计数。预算 token 随帧所有权在消费后释放；stderr 允许丢弃超限日志并记录数量，不得挤掉响应。

这是编码字节预算，不宣称等于进程 RSS；JSON 和布局对象的额外占用另做性能测量。JSON 解析尽量在 reader/有界解码任务，不在 draw 中。单个合法超大 frame 仍须经过预算检查。

### 5.5 本地副作用

`jobs.rs` 保存剪贴板、外部编辑器、导出各自的 owned job，不新增插件式任务注册。结果携带 session/draft generation，返回 `AppEvent::JobFinished`。

剪贴板保留现有平台选择方式，只改成非阻塞调用；外部子进程使用明确参数、deadline、kill 后 wait。不能仅用 `spawn_blocking` 包一段永久阻塞代码，就声称 timeout/abort 能终止它；已启动 blocking task 不支持这种取消。[S11]

`run_commands()` 改为分发/启动任务，不在循环里等待它们完成。所有任务在 shutdown 按 owner 关闭或请求停止、join，已开始的文件写入不宣称能回滚。

---

## 6. 完整历史：分块解码、稳定前缀、窗口读取

### 6.1 权威数据结构

`protocol/read.rs` 至少实现：

```rust
struct ReadCursor { item: usize, offset: usize }
struct ReadChunk {
    index: usize,
    offset: usize,
    total_bytes: usize,
    encoding: String,
    data: String,
    complete: bool,
}
struct SnapshotPin { captured_end: u64, history_revision: String, total: usize }
struct ReadSessionResult {
    session: SessionInfo,
    items: Vec<ReadChunk>,
    next_cursor: Option<ReadCursor>,
    total: usize,
    records: Vec<ReadTurnSummary>,
    records_truncated: bool,
    history_revision: String,
    captured_end: u64,
    trailing_incomplete: bool,
}
```

`captured_end` 是后端返回的完整 JSONL 前缀边界，**不是 item 数量，不做客户端运算**；`total` 才是对应可读 item 数量。`records` 是这些 chunks 关联的 Turn 摘要，不代表所有 Turn 均已读出。[S7]

### 6.2 ChunkAssembler

```rust
fn push(&mut self, chunk: ReadChunk) -> Result<Option<DecodedItem>, ReadError>;
```

要求：encoding 为 `utf8_json`；同 item offset 连续；`total_bytes` 保持；只按 `data.as_bytes().len()` 推进；`complete` 时实际字节数恰好相等，拼完再解析。

`data` 已经是外层 JSON 解码后的字符串，不能对它重复 unescape。拼出的对象是 `{ "item": <sanitized Runtime HistoryItem>, "timestamp": <optional> }`，不是旧 IndexedHistoryItem。[S7][S8]

只暂存一个未完整 item。不要按远端 `total_bytes` 一次预分配。默认自动解码单 item 上限 8 MiB；超过时显示明确 Large Item 占位和单独读取/原始导出入口，不静默裁掉正文然后标为完整。

### 6.3 初始读取与尾部打开

1. `session.read(cursor={item:0,offset:0}, limit:1, max_bytes:65536)` 获取 pin/total。
2. 对普通短会话从 0 读取；长会话默认从 `max(total-200,0)` 开始，后续请求携带刚得到的 `captured_end/history_revision`。
3. 已从第一步得到且属于目标窗口的 chunk 可复用；不属于窗口的部分丢弃 assembler，不伪装成已加载 item。
4. 每页使用后端 `next_cursor`，默认 `limit:20,max_bytes:262144`，不能自己使用 `items.len()` 推下一页。
5. 显示“较早消息未加载”，向上滚动或用户点击时读取前面的已固定前缀范围。

后端允许在取得 pin 后按 item 边界开始另一段读取。第一次就发送非零 cursor 但不带 pin 是非法请求，禁止实现这种尾部优化。[S7]

### 6.4 新 Turn 完成后的读取

旧 pin 固定在旧前缀，不能带旧 pin 期待读到新 Turn。

- 先用旧 pin 验证旧前缀仍有效（可在旧 `total` 边界做一个最小读取）；失败则将当前视图标为 stale，并提供重新加载，不拼接两代历史。
- 从 cursor 0 获取新 pin；验证 `new.total >= old.total`。
- 从旧已确认 total 的 item 边界读取新增范围，携带新 pin。
- 同一 Session 只运行一条此类增量链；期间又有刷新需求记录一次 pending-refresh，完成后补一次。

不因为 `/reload` 配置而刷新 pin。重开/显式刷新/主结果持久化才需要推进历史。较早窗口是否驻留与完整历史 total 分开记录。

### 6.5 HistoryWindow

```rust
struct HistoryWindow {
    pin: Option<SnapshotPin>,
    items: BTreeMap<usize, Arc<Message>>,
    turns: BTreeMap<LoopId, Arc<TurnSummary>>,
    read: ReadState,
    loaded_ranges: Vec<Range<usize>>,
    bytes: usize,
}
```

连续区间合并成少量 ranges。窗口缓存默认全局正文 32 MiB，优先保留活动会话当前视口、最近结果；先逐出非活动会话远离视口的已确认内容。逐出只影响 TUI 缓存，不调用 `session.close`，不改变后端 History。

超大 item 是可见占位；未加载的历史间隙是 Load Earlier/Later 行。不能把未加载范围当作空白历史，也不能给它虚构像素高度。

### 6.6 单一语义对象

Wire DTO 解码后只保留一份 `Arc<Message>`，包含有序 Assistant parts；工具结果通过 ToolKey 引用。不要同时长期存 raw JSON、重复的 text/reasoning 聚合串、TranscriptBlock clone 和 copy text 全文。

`ReadTurnSummary` 与 `turn.result` 的 Usage 用 `(session_id,loop_id)` 去重。整轮总 Usage 优先，只有没有总数时才聚合唯一 Request Usage；unknown 不能补零；summary utility_usage 不加进普通 Turn Usage。

---

## 7. 权威完成与结果补读

### 7.1 正常路径

`turn.send` 获得 TurnRef 后立即登记一个 `turn.wait`；每个 Turn 只登记一次。流式 Event 更新 Live；wait 结果按精确身份结算，不依赖 `turn_finished` 事件。

`persistence=persisted`：用新 pin 读取新增 History。此过程未完成时保留 Live 或完整 retained result，不闪空。新 items 替换临时项后再清理 Live。

### 7.2 未确认/丢事件

新增 `recover_turn(turn)`：调用 `turn.result`，它返回 `pending/live/stored`、可选 outcome/persistence、分块 items 和摘要。

- pending：仍在运行，等待已有 wait；只有当前前景确需恢复时适度再查。
- live：以保留报告补齐内容；读取完也要尊重 persistence 字段。
- stored：结果来自已保存记录；与 History 按身份去重，不累计两次。
- turn_not_found：显示无法确认；不得自动 `turn.send`。

`turn.result` item index 是**该 Turn 内索引**，不能直接作为 Session 全局 item index 插入主历史。保留为 TurnResult 来源，直到 session.read 返回对应全局记录后替换。[S5][S7]

### 7.3 保存失败

`persistence=failed`：保留执行结果、发起 retained result 补读、标注“保存未确认，工具副作用可能已发生”。查询不到完整报告时保留已有 Live 并注明不完整。

Session 是否 Blocked 以 state/已确认结果为准；禁止 send/steer/update。允许阅读、复制/导出未确认结果（必须标注来源）、关闭。关闭前一次确认即可，不建立恢复向导。

重开只读取 Store 实际能恢复的内容。不能承诺失败 append 必然没有写入，也不能将普通 persisted 描述为事务/fsync 的崩溃保证。[S5]

---

## 8. 提交、准备、压缩、取消：统一控制语义

### 8.1 Submission

```rust
struct Submission {
    request_id: RequestId,
    local_id: u64,
    session_epoch: u64,
    editor_revision: u64,
    text: Arc<str>,
    preparation: Option<OperationRef>,
    cancel_requested: bool,
}
```

发送准入成功才把文本变成临时 User Card。用户可继续写新草稿；失败恢复只在原草稿 revision 未改变时执行，否则保存为可点击“恢复到 Editor”的未发送项，不能覆盖新文字。

### 8.2 自动准备

`turn.send` 可以长时间 deferred，不能用“没立即返回 TurnRef”判失败或重发。状态显示 Preparing/Compacting；用 `session.context` 观察，在活动前景至多每 500ms 一次，后台至多 2s；没有工作时停轮询。

operation ID 必须由本次准备的真实观察得到。若用户先取消但 operation ID 还未知，记录 `cancel_requested`；待确认对应准备 ID 后 `session.compact.cancel`，或先返回 TurnRef 则立刻 `turn.cancel`。不能取消任意“最近一个压缩任务”。

准备失败保留原输入，展示明确 `context_uncompressible`、query/preparation failure；不得自动删工具、降模型、减提示词后再次发送。[S4][S5]

### 8.3 手动压缩

`/compact` 仅在已确认 loaded/idle/settled/unblocked 时可启动。生成本进程唯一 operation ID，例如 `tui-<nonce>-<counter>`，不重用；发送后保持 deferred 请求并观察 `session.context`。

取消使用同一 `session_id + operation_id`。取消请求被接受不表示生成或原子写已撤回；最终以 compact result 为准。

结果：`compacted/noop/failed/unknown_write`。前两者刷新 context；failed 保留之前历史；unknown_write 停止自动执行，重新读取 session state/context，再由用户决定继续或重开，**不自动重试压缩**。

压缩不刷新或删除原始 History，也不把 summary body 插入系统提示或普通聊天卡片。Context 页展示 coverage、估算预算、独立 utility usage 和失败分类，不显示伪造的精确百分比。[S6]

### 8.4 单一取消入口

```rust
fn request_cancel(&mut self, session_id: &SessionId) -> Vec<AppCommand>;
```

按已知对象路由：Submission 准备 operation→compact.cancel；运行 Loop→turn.cancel；手动 Compact→compact.cancel；没有明确目标→读取确认并提示，不能猜 ID。

Esc 在详情/选择/搜索中先退出该视图；在对话根视图且没有当前选择时才调用取消。`/cancel` 是明确停止当前操作，详情标题写“停止当前轮”，不写“关闭工具”。

---

## 9. 模型更新与 Steer

### 9.1 模型/reasoning

Active Session 选择模型或思考级别调用 `session.update`，无 Active Session 时更新新会话配置。只提交能力列表中的合法组合；不支持当前 reasoning 时让用户选择，不自动降级。

更新成功立即更新已保存 SessionInfo；`active_revision=Some(n)` 只显示“等待下一 Request 使用”。RequestStarted 证明相应 revision/model/reasoning 后清理 pending 提示。`None` 表示没有更新当前已封口 Loop，留给下一轮。

Update 与 Steer 的发送顺序由用户操作顺序决定；不跨 await 并发乱序。Preparing/手动压缩时禁用变更，真实竞态失败按 Agent 返回值展示。

### 9.2 Steer 最小队列

保留多条排队能力，但只在明确的当前 Loop 内：本地未发送上限建议 8 条/累计 256 KiB，同一时刻最多一条 steer RPC 在途。

每条记录：本地 ID、TurnRef、文字、提交时 editor_revision、ACK/receipt 状态。只从真实 ACK 的 steer_index 与后端 steer_progress/presentation receipt 确认 applied；仅仅 RequestStarted 不能当作逐条应用回执。[S9]

状态收敛为：`Local → Sending → Accepted → Applied → Recorded`；失败/断线为 `Rejected/Unknown/NotRecorded`。不增加跨 Loop 历史队列系统。

ACK 成功只清理同 revision 的输入；新文字不受迟到 ACK 影响。QueueFull 保留原内容，不反复立即发送。当前 Loop 结束时：未发送项暂停，用户可放回 Editor；已接受但未记录项显示未记录/未确认，**不自动重发、不转成新 Prompt**。

### 9.3 Request 与内容顺序

LiveLoop 按 request_index 保存 Request。每个 Request 保留可见 parts 的到达顺序，不用“所有 Thinking 放最前、全部 Text 再后”重排。

Tool 以完整 ToolKey 注册一次，可被事件更新，也可由 history/turn.result 确认。ToolExecution 已确认 succeeded 后迟到 ToolStarted 不能退回 running；来源冲突标记需读取 tool.read，不凭本地时钟选赢家。

---

## 10. Session 操作与 reload 收敛

### 10.1 浏览与执行分开

Session Selector 默认当前 Workspace、最近活动优先，支持切换全部和搜索 title/path/model。Enter 可继续会话，独立“查看历史”操作只用 `session.read`，不先 open。

浏览 closed Session 不初始化模型、工具或 Workspace。缺失 Workspace/模型也能读出的旧记录应能浏览、重命名、导出。首次发送前要求明确“继续对话”，调用 session.open；open 失败不销毁已读取历史。

### 10.2 新建与继续

`/new` 用当前 Workspace 和最近显式选择快速创建；“自定义新建”仍可打开完整表单。用户没有明确 Workspace 时使用已有 CLI/current-dir 选择，不隐式跳到全局最近项目。

增加 `--continue`：只选择与当前 Workspace 可明确匹配的最近 Session；无匹配则展示列表，不猜别名/其他项目。`--session <id>` 选择明确 Session。启动/打开均不自动发 Prompt。

### 10.3 草稿与视图

切 Session 保存整份 Composer（文本、光标、撤销/粘贴标记）以及 view anchor/folds；不只保存一个 String。正常切换不 close 后台 Session、不 cancel Loop。

已关闭视图可释放布局/工具输出缓存；未发送草稿保留到明确丢弃或 TUI 退出。退出不恢复 active execution，本轮默认不把草稿写磁盘。

### 10.4 rename/close/delete

复用 `session.rename`，成功后更新 metadata；ACK 丢失先重读 metadata，不重复写。rename 不影响 Loop/model/history。

close 是显式停止并释放 Session：保留原处理中的结果接收，直到后端完成；界面不提前删 unsaved result。delete 只对 closed Session，确认后调用；不承诺撤销文件副作用。

### 10.5 reload

将当前 `ReloadProgress`/`ReloadHistoryStage`/整历史 staged replacement 改为：

```text
agent.reload → 成功后刷新 Models/Profiles/必要 metadata → 更新 Catalog generation
```

不 clear live、不重新创建 Loop、不触发整历史刷新、不因 reload 清除独立草稿和选择。显式 `/refresh` 才刷新当前主视图的数据；同一入口不得同时承担“重读配置”和“重置全部会话状态”。

reload 要求重启或不可用时明确显示；不自动杀 Agent。过期列表响应不覆盖较新的 rename/update，使用一代 catalog/meta query token 即可。

---

## 11. 对话渲染重构：稳定历史不参与每次 Live 全量复制

### 11.1 当前替换点

`ui/transcript.rs::prepare_conversation`、`all_lines_with_durable`、`build_durable_prepared`、`selection_text` 是主要改造位置。

当前 durable cache 命中后仍向新数组追加所有历史行；Assistant ToolCall 查找对应 ToolBlock 会扫描整个 blocks。改为共享正文、一次索引、分段布局、只拼接视口。[S10]

### 11.2 三层数据

```text
Message / ToolFacts（源数据，Arc，共享一次）
    ↓
SectionLayout（某一块在某宽度/主题/折叠状态下的布局）
    ↓
VisibleConversation（当前视口及一屏 overscan，供 draw/hit/copy）
```

```rust
struct LayoutKey {
    section: SectionId,
    revision: u64,
    width: u16,
    theme: ThemeKind,
    folded: bool,
}

struct SectionLayout {
    key: LayoutKey,
    rows: Vec<VisualRow>,
    source: Arc<SectionSource>,
}

struct ConversationLayout {
    sections: Vec<SectionRef>,
    offsets: Vec<usize>,        // 已测量块的行前缀和；只存整数
    live_sections: Vec<SectionRef>,
    viewport: VisibleConversation,
}
```

不要同时缓存另一份全对话 `Vec<Line>` 和全对话 `CopyRange.text`。VisibleConversation 最多保留视口附近行，引用 SectionLayout；Ratatui 需要 owned Line 时只克隆可见行。

### 11.3 身份与索引

- Assistant/Thinking：Session + Loop + Request + part ordinal。
- Tool：完整 ToolKey，与其结果的 History index 无关，避免结果到达后折叠身份改变。
- User：Session + Loop + User occurrence（Prompt/Steering）；不能以文本作为唯一键。
- Summary/只读历史占位：快照内稳定 index。

History 投影时一次建立 `ToolKey -> ToolFacts/位置` 索引。ToolResult 到来 O(1) 查找更新；renderer 禁止 `.blocks.iter().find(...)` 逐 Tool 重扫所有历史。

### 11.4 增量更新

- 新 Delta：只更新当前 Request 的活动文本段；已结束 parts/Request 不重排。
- 工具进度：只使对应 Tool section 失效，不使全部历史失效。
- 折叠一个块：只重排该块；后面 prefix offsets 更新整数允许 O(n)，不重建正文/Markdown。
- 用户滚动/选择/spinner：不改变内容布局 key。
- 新历史页：只生成新块并追加索引；旧块 Arc 保持。
- Width/theme 变化：使相应布局失效，但分批计算，不在一次 UI 事件中阻塞整个大历史。

初期不需要 Fenwick tree、虚拟 DOM 或增量 Markdown 解析器。稳定前缀 + Live 尾部分离已经消除主要成本；折叠/resize 的整数索引 O(n) 可以接受。

### 11.5 布局计算与巨大段落

已完成消息解析 Markdown 一次，缓存语义片段；按宽度生成行缓存。Live 文本先普通换行，完成后切到 Markdown。不能每个 Token 对整个 Request 重新解析所有已结束段落。

提供具体布局任务：

```rust
fn prepare_section(input: LayoutInput, cancel: &AtomicBool) -> Result<SectionLayout, LayoutError>;
fn apply_layout_result(&mut self, result: LayoutResult);
fn visible_rows(&self, viewport: Range<usize>) -> impl Iterator<Item = VisualRowRef<'_>>;
```

只有一个布局 worker 在途，队列有限。每批优先视口和相邻块。大文本按逻辑段/行分批，循环检查取消或过期 generation；不用为每个 Block spawn 一个 task。

首次加载/resize 期间可显示“布局中”占位，但所有已显示块的 hit/copy 必须对应当前 frame 的同一布局。不能用旧宽度坐标接收新界面点击。待布局完成恢复精确 scrollbar；尚未加载历史不计为已知行。

### 11.6 渲染预算

默认布局缓存 48 MiB，正文缓存 32 MiB，工具流缓存 16 MiB（见第 21 节）。保留视口及相邻块，逐出可重建布局时不丢正文；逐出正文时保留读取定位和占位，不截断后冒充完整。

预算按 String/Vec capacity 和共享对象一次计费；不通过 `size_of` 宣称精确 RSS。临时 JSON/绘制分配另测。工具几百 KB 输出不应被分别复制到 ToolFacts、TranscriptBlock、CopyRange、PreparedConversation 四份。

### 11.7 测量、命中、滚动共用事实

一次 prepare 产生：可见行、Section 边界、文本源映射、可点击区域。draw、scrollbar、鼠标点击和复制只用它，不各自重新测量。

滚动锚点保存 `{SectionId, source_offset, screen_row}`，不是只保存绝对总行号。展开工具、加载前页、live→saved 替换、resize 后尽量保持锚点位置；锚点源已被淘汰时定位最近保留项并提示，不跳到任意历史。

### 11.8 选择与复制

`VisualRow` 必须标明源字节区间、hard newline/soft wrap、装饰区域。复制以 source range 取正文：不包含 Rail、时间、折叠提示、右 padding；soft wrap 不添加真实换行；真实空行、缩进和代码换行保持。

工具被部分保留时只复制可确认的内容并提示 partial，不能拼出不存在的前缀。选择跨未加载区间时要求先加载或限制为已加载内容，不能静默越过缺口。

字符坐标区分 UTF-8 bytes、grapheme 和终端 cells；不得 `as u16` 截断长行游标后当作正确位置。行定位使用前缀 offsets 的二分或已准备的视口索引；跨行选择按 Section/源区间顺序遍历，不能每选一行再扫描全部 CopyRange。

---

## 12. Editor 与独立草稿

### 12.1 不重写编辑器

保留 tui-textarea 及现有 Rail Editor 的文本投影、粘贴标记、IME、Undo/Redo、鼠标选词/选段。先修数据路径，不引入 Rope/自研富文本树。

`Composer::content()` 只在发送、导出草稿、外部编辑器或明确全文操作时 materialize。`can_insert` 使用保存的 byte_len；普通输入且没有 paste ranges 时不要执行全文前后 diff。

新增/调整：

```rust
fn byte_len(&self) -> usize;
fn apply_edit(&mut self, edit: EditorEdit) -> EditOutcome;
fn submission(&self) -> SubmissionText;
fn layout(&mut self, width: u16) -> &EditorLayout;
```

已有 TextArea API 不能直接给出 edit delta 时，在受影响的逻辑行计算差量；Undo/Redo 后重算一次可以接受。不得仅把旧 `content()` 调用藏进 helper。

草稿预算包含 undo/redo 与粘贴投影保留的数据，而不只计算当前可见文本。使用编辑库已有历史容量设置限制旧撤销记录；预算不足时可以淘汰最旧撤销记录并保留当前草稿，不能无提示删除未发送内容。没有合适 API 时先用已测的固定历史条数上限，不为此重写编辑器。

### 12.2 编辑响应规则

- 文字上限 256 KiB，且最终 RPC 整行仍需经过 1 MiB 检查。
- 大粘贴标记是显示投影，真正提交保留原文。
- Prompt/Steer 发送使用 captured revision；迟到 ACK 不清除新编辑。
- 切 Session/打开面板不丢文本、光标、undo、粘贴范围。
- 输入历史搜索选中只放回 Editor，不自动发送。
- Running Editor 支持 Steer；Preparing/手动压缩时可以写草稿但不能提交；Blocked 保留可编辑草稿并禁用发送。

### 12.3 快捷键与命令

现有键位优先，新的常见能力首先提供 Slash 命令；禁止全局抢占普通输入。`command.rs` 使用一个静态表同时驱动 parse/help/completion，不再维护三份不同命令名单。

保留 `/new /resume /sessions /model /reasoning /theme /clear /help /logs /cancel /reload /quit /close /delete`。补齐 `/rename /search /copy /export /files /grep /diff /tool /context /compact /editor /settings /refresh`。

这是明确的命令列表，不是可注册命令平台。`/clear` 明确“只清理本地显示并重读”，不删除 Store。`/copy` 默认复制最后一个已完成 Assistant 可见回答，不默认复制 Thinking。

### 12.4 外部编辑器

`/editor` 编辑当前草稿的临时文件，禁止直接打开 Workspace 路径作为这一命令的目标。

配置为 executable + args 数组，追加临时文件路径，避免把 `$EDITOR` 作为任意 shell 命令拼接。未配置时可使用一个明确的环境变量程序值；含 shell 拼接/不明确参数时提示用户配置，不连串猜测。

进入编辑器前暂停终端输入与绘制并 restore，Agent stdout/stderr 继续读取，App 仍处理后端结果；编辑器返回后重新 enter 并 invalidate viewport。回写草稿前验证 session/revision；不覆盖用户后来产生的另一份草稿。

编辑器是用户明确选择的本地程序，只允许这个草稿工作流；不意味着 TUI 可以执行模型提供的 shell。

---

## 13. UI 结构与焦点：面板只是整项目的一部分

### 13.1 保持现有 Rail 主界面

不恢复旧 Pi 默认布局：没有圆角四边 Editor，没有 reasoning 变色边框，没有双行 Footer，没有常驻大 Header。

保留当前源码/快照中的：一列左 gutter、蓝色 `▎`、slate User/Editor 背景、透明 Assistant/Thinking 背景、单行 Footer、Tool 状态色、单块/全局折叠、鼠标复制与蓝色 scrollbar。

Editor 继续按 4–12 可见行/约终端高度 32% 规则；现有更小屏幕处理不退步。Thinking 按既有逻辑行规则折叠，Tool 20 行阈值及 write 默认折叠保持；本轮新增状态不改基础像素几何。

### 13.2 三类容器

| 容器 | 用途 | 关闭行为 |
|---|---|---|
| 底部临时 Dock | 模型、reasoning、Session、文件候选、Settings、输入历史 | 恢复原 Editor/草稿 |
| 主区详情 | 工具详情、文件预览、改动列表/Diff、Context | 返回对话，保留阅读锚点 |
| 小确认 Overlay | 删除、释放 unsaved result、明确覆盖导出 | 仅确认该操作 |

搜索使用底部一行输入，对话仍可见，不占一个主区详情页。

```rust
enum MainView {
    Conversation,
    ToolDetail(ToolDetailState),
    FilePreview(FilePreviewState),
    Changes(ChangeReviewState),
    Context(ContextViewState),
}
enum Focus { Main, Editor, Dock, Search, Confirmation }
```

只保留一个主详情，不建设页面堆栈/router/Panel trait。Diff 状态自己保存列表→单文件二级位置；文件预览可记录一个明确 ReturnTarget，不能扩展为通用 navigation graph。

### 13.3 统一键盘规则

- 详情打开默认焦点在正文；F6 在正文和 Editor 间切换。
- 详情中 Tab/Shift+Tab 切适用标签；Editor 聚焦时继续既有补全/思考选择键义。
- Esc：确认/选择/搜索先退出；详情返回对话；对话有选择先清选择；无选择且有当前操作才请求取消。
- 对话/详情 PageUp/PageDown 滚动当前焦点区域；End 恢复该区域 follow tail。
- 点击 Tool 折叠目标仍是展开/收起；标题内独立 `[详情]` 命中和 `/tool` 精确选择进入详情，不能让原单击折叠变成跳页。
- 删除/取消不得被普通字母 `q`、输入历史面板快捷键等误触发。

### 13.4 Tool 详情 layout

```text
← 对话  bash · request 2                         running · 12s
$ cargo test --all-targets
cwd: ~/project
[标准输出]  标准错误  结果  输入  改动（有数据时）
──────────────────────────────────────────────────────────────
可滚动、有界、按页读取的正文
...
                                  ↓ 跟随输出   [复制] [刷新]
──────────────────────────────────────────────────────────────
▎原 Editor / 当前 Session 草稿
▎
▸ workspace@branch · model · thinking · state          usage
```

显示标题一行、摘要最多三行、标签一行，正文占剩余主区。窄屏折叠摘要；Footer 永远单行。主区不强制常驻左右分栏，不增加终端 IDE 布局。

正在阅读某标签时错误不会强制切标签；可给 stderr/错误标签加标记。关闭详情不取消；“停止当前轮”路由统一 cancel。

---

## 14. 工具：卡片、事实、输出和进程状态

### 14.1 数据 owner

`state/tool.rs` 维护一个 `HashMap<ToolKey, ToolFacts>`。ToolKey 为 `{session_id,loop_id,request_index,tool_call_id}`。卡片、详情、结果、Diff 都引用同一身份，不按名字或“最近一条 bash”查询。

现有 `ToolDisplay` 可用于 Rail 卡片短摘要。明确的调用/执行事实由 `tool.read` 补齐；未知时显示工具名，不从工具输出猜命令和路径。

### 14.2 卡片与详情的区别

- 折叠卡片：名称、真实摘要、状态、既有折叠提示，消费小型事件/已缓存事实。
- 展开卡片：有限预览；保持用户折叠选择，不加载几十 MB。
- 详情页：按当前标签分页读，能显示完整保留范围与缺失原因。

隐藏卡片不持续拉输出。进入视口需要事实时可排队查询，优先点击对象；不能为了历史 Rail 摘要同时启动所有 Tool 查询。

### 14.3 tool.read

Params 使用完整 ToolRef，加 `max_bytes:262144`。读取 invocation、execution、command（存在时）。

`awaiting_policy` 不是 running；`started_at` 缺失不伪造开始时间；input 是请求参数，不等于工具验证通过；recording saved/failed/memory_only 不改变真实 execution outcome。[S5]

Tool 终态只能由真实 Runtime outcome / tool.read 确认；Bash 非零 exit code 是一次已执行完成的命令结果，详情标为“退出码非零”，不能伪造为 RPC 网络失败。

### 14.4 tool.output

Params：完整 ToolRef + `stream:input|output|stdout|stderr` + `offset` + `max_bytes`。默认 16 KiB，用户“加载更多”可用 64 KiB，但不超后端允许上限。

```rust
struct StreamView {
    stream: ToolStream,
    next_offset: u64,
    retained_start: u64,
    eof: bool,
    availability: Availability,
    truncated: bool,
    bytes: BoundedChunks,
    utf8_tail: Vec<u8>,   // stdout/stderr 最多 3 个尚未完整的 UTF-8 尾字节
}
```

- input/output 是 UTF-8 byte offsets；stdout/stderr 的 data 是 base64，offset 是**解码后原始字节**，不是 base64 字符位置。
- 只用 next_offset 继续；同范围 Event 与 query 重叠时裁掉已经收到的 prefix，不复制两次。
- Event chunk 有 gap、dropped/expired 时读取 authoritative window；base_offset 前移则插入“之前输出已不再保留”标记。
- gap notice 可为空且 eof=false；按指示 next_offset 跳到保留开始继续，不当作空结束。
- pending/unavailable/partial/expired/available 是不同状态。空 stdout 达到真正 EOF 也可以 available；expired 不是“没有输出”。
- EOF 前末尾半个 Unicode 字符留待下页；非法字节只影响显示替换，不能改变原 offset；gap 后清除 UTF-8 尾部状态。
- 两条流分别有序，不构造没有证据的混合时间线。[S5]

### 14.5 刷新策略

打开详情先 tool.read，再读当前标签。运行中当前标签按 tool_process 提示刷新，最多 250ms 一次；事件缺失时最多 500ms 周期补读。一次 query 未结束不启动同 key 第二个。

切标签可保留各自最后页/滚动位置，停止旧标签轮询。关闭详情停止全部额外轮询；Agent Tool 继续执行。完成后将保留尾部读完直到 eof，而不是一收到 ToolFinished 就停止取输出。

每个已打开流保留最多 1 MiB UI 原始数据，超出逐出头部并显示保留窗口。辅助输出本身可能被后端淘汰，TUI 不承诺无限回看。

### 14.6 取消和结束事实

命令 `cancelling`、`cancelled/timed_out`、`termination_confirmed`、`output_complete` 分开显示。请求停止不等于进程组已停止；没有退出码就省略，不能填 0。

进程输出不是 PTY：CR、ANSI、OSC 不得执行，见第 19 节。不上完整终端模拟器。

---

## 15. 文件引用、预览与项目文本搜索

### 15.1 @file 的本期语义

本轮做**路径引用 + 可读预览**，不自动把文件内容附加到模型输入。标签/提示明确“引用路径，模型需要时再读”。预览文件不创建 History，不意味着模型看过内容。

选择后插入可读的工作区相对路径引用；包含空格/引号使用稳定转义表示。路径 token 与原文位置有映射；编辑 token 后退化为普通文字即可，不建设附件对象库。

禁止偷偷把整文件编码到 Prompt、写 summary 或额外调用 Model。真正内容附件另立 Agent 输入契约后再做。

### 15.2 workspace.files

输入 `@` 或 `/files` 打开 Dock 文件候选：`directory`、`recursive:true`、`query`、`limit:100`、`max_bytes:65536`。

输入 debounce 150ms，一次在途查询，query generation 防止旧结果覆盖；游标原样传递，只对已收到候选本地排序。该接口按遍历返回，不宣称全局最优/全部文件已列出。

`truncated/scan_complete/stopped_by/skipped_count` 在底部简短显示；没有 next_cursor 时不能继续编造下一页。deadline 后提示缩小范围/手动刷新，不能反复从头扫描。[S4]

### 15.3 workspace.read

文件主区只读预览使用：

```json
{"session_id":"ses_...","path":"src/main.rs","start_line":1,
 "line_byte_offset":0,"max_lines":400,"max_bytes":65536,"if_revision":null}
```

第一页获取 revision；后续携带相同 if_revision 和 next_range。不要只递增行号，可能在同一行中继续。

`ok` 才追加正文；`changed` 停止拼页并展示“文件已改变，重新读取”；binary/too_large 显示原因，不当作空文件。行号、软换行、源字节映射只由 TUI 显示层添加，返回 content 保留 CRLF 和无结尾换行。[S4]

### 15.4 workspace.search

`/grep` 提供普通字面文本搜索，无正则开关。参数：query、可选 paths（最多后端允许数）、case_sensitive、cursor、max_matches:100、max_bytes:65536。

显示 path、真实行号、匹配范围；范围是 line_text 内 UTF-8 bytes，先验证再转终端 cells。选中结果打开 FilePreview 到对应位置。分页绑定原请求参数；换 query/path 清 cursor。skipped_files、partial 与扫描结束原因不能隐藏。[S12]

### 15.5 Workspace 所有权

所有文件接口要求 loaded Session。浏览旧历史时使用文件功能需用户明确打开 Session；不能从返回 path 自己构造本地 root 去读。TUI 不实现 ignore 规则、symlink 安全校验或文件扫描后端。

---

## 16. 改动审查：一个只读 Changes 主页

### 16.1 入口与范围

`/diff` 默认 Workspace 范围；可以切 Session、明确某个 Turn。列表→选择文件→单栏 unified diff；Esc 从文件返回列表，再返回对话。

范围标签必须准确：

| 范围 | 标签含义 |
|---|---|
| workspace | 当前工作区变化，来源不确定 |
| session/turn | 有工具记录的原生 write/edit/apply_patch 变化 |

Bash、外部编辑器和此前用户修改不自动归因给某个 Tool。Tool before/after 与 Git HEAD 对比是不同语义，不混用。

### 16.2 changes.list

传 `session_id, scope, cursor, limit:100,max_bytes:65536`。保留 opaque change_ref，不解析/制造 `workspace:` token。records 按真实独立 ToolRef 呈现，不把同文件多次编辑合成虚构总 diff。

显示 path、kind、origin、commit_state、details_available、coverage。unknown commit 不称为未修改，partial capture 不称为完整证据。

### 16.3 changes.diff

传原 change_ref、合法 comparison、context_lines:3、cursor、max_bytes:65536。工作区可切 head_to_index/index_to_worktree/head_to_worktree；工具来源固定 tool_before_after。

返回 hunk/line fragments 必须按 hunk、行号、line_byte_offset 拼接，line_complete 后才视为完整行。颜色和行号属于 TUI，不能先添加再参与字节游标。

stale 时保留旧视图并显示刷新；**不把旧 cursor 带入新 comparison 自动续页**。binary/unavailable/截断均显示明确状态，不伪造全删全加。版本变化以返回 base_version/target_version 为准。[S4]

### 16.4 Footer 分支

branch 的新事实来自显式 `workspace.status`，不是反复调用 session.presentation 就一定刷新。打开执行 Session 时可发一次只读 status 观察；之后仅在用户打开 Changes/点击刷新时更新，不在每 Tool/每 Token/每次 draw 时运行 Git。

status 不完整时显示 unknown/stale，保留“上次观察”含义，不能将失败误报成非 Git 仓库。

### 16.5 不做写操作

本轮不提供 stage、commit、restore、reset、rebase、冲突编辑或回滚。详情中的文件只读，点击命令/路径不执行 shell。

---

## 17. 对话搜索、消息导航、复制与导出

### 17.1 搜索范围

`/search` 在底部一行输入，对话不离开；默认搜索当前 Session 已读取正文、可见 Thinking、工具名/已读结果，并显示覆盖范围。

用户选择“搜索完整会话”才启动 pinned `session.read` 流式扫描。最多保存 500 个匹配摘要，不长期保存扫描过的全部正文；可停止，结果标记未完成。不可解析的超大 item 或辅助输出已过期要记为未搜索，不能报告“全会话无匹配”。

普通字面匹配即可，不添加正则/跨 Session 索引。输入改变只让搜索 generation 失效；在途 query 仍按第 5 节回收。

### 17.2 导航

新增上一/下一 User Prompt 和回最新；Steering 是独立消息类型，可在普通搜索命中但默认 Prompt 跳转不把每条 Steer 当作新任务。

跳转目标未驻留时按其 snapshot/item index 读取窗口；展开折叠块只是临时 search override。退出搜索恢复用户的手动 fold，不把临时展开写回全局设置。

### 17.3 复制

复用非阻塞 ClipboardPort：选区、当前消息、代码块、最后完整回答。源内容不含 Rail/时间/软换行。剪贴板失败保留选择并提示，不阻塞 App、不连串调用多种后端工具兜底。

不能为了“完整复制”静默执行几百个远端 query。未加载的内容提示加载或通过导出操作明确获取。

### 17.4 Markdown 导出

`/export` 打开小表单选择本地目标、是否包含 Thinking/Tool（默认两者关闭）。默认只导出已保存 History，使用一个固定 pin 逐页读取、逐 item 写文件，内存不持有全部 Markdown。

写到用户目标旁边的唯一临时文件，成功后原子式提交到明确路径；目标存在需用户确认。不写 Agent Store。取消删除未提交临时文件；写/rename 结果不明时报告未确认，不宣称旧文件必然未变。

导出 unsaved Turn 必须独立明确选择，并在文件开头标注保存未确认/内容可能不完整。读取失败、records_truncated、未读大项时禁止输出一个没有限制说明的“完整导出”。单 item 超过自动解码预算可提供原始 JSON 分块导出，不能为了导出盲目解除内存上限。

导出视图固定在 captured_end；导出期间新 Turn 不加入这份导出。完整记录来源与已见字段由 converter 决定，opaque provider data 不得写出。

---

## 18. Settings、启动与帮助

### 18.1 轻量偏好

`config.rs` 只保存 UI：theme、Thinking/Tool 默认展开、外部编辑器 executable/args、显式 Agent executable/config 路径、少量已实现按键覆盖。

本轮不建设快捷键 DSL；可先只提供设置表单中的有限动作映射，检查重复。CLI 覆盖本地偏好，偏好覆盖内置默认。Provider key/endpoint/model catalogue 不复制到 TUI 设置文件。

配置缺失用默认；无效配置给路径与安全解析错误，不能 silently reset 后覆盖用户文件。

### 18.2 启动错误分类

区分 executable 不存在、配置路径不存在、Agent 配置拒绝、Protocol 不兼容、Provider 运行失败、存储失败。不得统一显示“连接错误”。

只启动一个 Agent。普通退出先 agent.shutdown，持续读取已有 deferred 结果/日志；响应最后到来后等 Child exit。超过应用等待预算恢复终端并给出强制停止选项/按当前明确退出策略 kill+wait，不承诺任意操作系统 IO 都能被硬 deadline 中断。

### 18.3 帮助一致性

命令/快捷键描述从静态 action 表产生；清理旧“模型不可变”“不能 Steer”“没有压缩”“Bash 只能结束 direct child”等过时文案。

安全提示更新为真实边界：工具可自动执行，Bash 不等于沙箱；过程取消和终止确认分开；TUI 不实现审批。后台 process group/job object 的事实来自 Agent，不重复实现。

---

## 19. 内容安全与终端显示

所有来自模型、Tool、路径、日志、文件和 Diff 的文本先通过统一安全显示边界。原文与用于终端显示的 safe text 区分。

- 不直接输出 ANSI、OSC 52、OSC 8、控制序列到终端；将不可显示控制符转成可见转义。
- 保留正常换行；tab 按现有宽度规则展开。Bash 的 CR 不执行光标回退/清行，不模拟完整终端。
- Markdown link 默认只显示文本；打开链接只允许明确用户操作和允许的 URL scheme，不执行本地命令。
- 默认复制可见安全文本/已验证代码源；不能画面显示转义却悄悄复制隐藏终端控制符。原始字节只在明确的原始导出中保留。
- 字节游标永远对应 Agent 原始 bytes，sanitize 后字符串长度不能用来向 Agent 请求下一页。
- Debug 不打印 raw frame、Prompt/Steer、arguments、Tool output、文件内容和摘要；只记录身份、方法、字节数和安全 error kind。

渲染安全不是安全沙箱。TUI 不得用文本过滤声称工具执行已被隔离。

---

## 20. 推荐的请求与刷新策略

| 对象 | 何时读取 | 何时停止 |
|---|---|---|
| Models/Profiles | bootstrap、成功 reload | 响应完成；不周期轮询 |
| Session metadata | bootstrap、用户刷新、成功 mutation 后按需 | 无后台无限刷新 |
| session.state | open、控制操作竞态、丢事件需确认 | 已确认后停止；运行前景低频兜底 |
| session.context | 准备/压缩中，或打开 Context 页 | 无操作且面板关闭 |
| turn.result | wait 不可用/结果不完整/保存失败/历史详情 | pending 按需等待；读完停止 |
| session.read | 打开窗口、加载更多、完成增量、搜索/导出 | 页链完成/视图失效/用户停止 |
| tool.read | 卡片需事实、工具详情、终态缺事件 | 终态确认且详情关闭 |
| tool.output | 当前展开详情标签 | 隐藏/关闭；终态尾部读完 EOF |
| workspace.files/search | 用户查询，150ms debounce | 新 generation；无有效 cursor |
| workspace.read | 文件预览当前页 | 关闭/换文件/版本变化 |
| workspace.status | 显式查看项目/改动或用户刷新 | 一次观察完成 |
| changes.list/diff | 用户查看/分页/刷新 | 关闭/来源过期 |

不在 renderer 发请求；一次 draw 不启动 Git、文件扫描、历史重读或 Context 估算。

---

## 21. 资源预算与退化规则

所有预算集中在一个 `limits.rs` 或已有配置常量区，避免散落 magic numbers。以下为第一版默认值，可在基准后小幅调整，但不能删除有界策略。

| 资源 | 默认预算 | 超限处理 |
|---|---:|---|
| 出站单行 | 1 MiB（含换行） | 本地拒绝，输入保留 |
| 出站队列 | 32 项，其中 4 项控制余量 | 准入失败/查询合并，不等待主循环 |
| 入站单帧 | 32 MiB | 协议错误，安全关闭 |
| 入站待处理 wire bytes | 64 MiB | reader 背压，不静默丢响应 |
| 普通 read 在途 | 2 | 有界待办/合并刷新 |
| 本地 deferred 总数 | 16 | 限制新准入；已有完成不丢 |
| Composer 单草稿 | 256 KiB | 拒绝新增，保留已有内容 |
| 所有草稿 | 8 MiB | 提示清理/关闭草稿，不默默丢未发送文字 |
| 本地未发 Steer | 8 项、共 256 KiB | 保留 Editor，提示队列满 |
| History 正文缓存 | 32 MiB | 逐出可重新读取内容，保留占位与定位 |
| 单 item 自动完整解码 | 8 MiB | 大项阅读/导出入口，不能静默缺字 |
| 布局缓存 | 48 MiB | 优先逐出非视口布局，重新按需计算 |
| Tool UI 流窗口 | 每流 1 MiB、总 16 MiB | 丢弃旧展示缓存、注明窗口，可向后端补读 |
| Live 展示 | 每 Loop 4 MiB、总 16 MiB | 停止保留早期 Delta，标记需结果补读；不影响执行 |
| 日志 | 200 行、每行 4096 bytes | 截断/丢弃并计数 |
| 搜索命中 | 500 条 | 显示更多未列出，用户缩小查询 |

不能用缓存预算删 Agent History；也不能为了永不超内存悄悄取消用户执行。元数据保留必要索引；没有驻留数据则需要重新读取。Byte accounting 是 payload/capacity 预算，不是内存安全证明。

---

## 22. 删除和保留的技术债

### 22.1 必须删除

- 只允许 Agent 0.3.x 的版本门禁和旧协议 fallback。
- 以旧 `session.history` 为唯一全量历史来源的加载器。
- 同一历史正文重复存在 raw item、block clone、copy text、全局 prepared 行数组的结构。
- Tool 查询按名称/最近调用匹配；渲染路径的全 blocks 逐次查找。
- 配置 reload 对全部 History 的 staged replacement、与它绑定的多类 wait 来源分支。
- Running/Finishing 的未发送 Steer 自动升级成新 Prompt。
- 主循环里同步剪贴板和 await 发送队列容量。
- 将未知 Usage 填零、将所有 workspace 变化归因于当前 Agent 的表现。
- 隐藏真实失败的自动 retry/reopen/re-send。

### 22.2 必须保留或等价迁移

- TerminalGuard、panic/resize/退出恢复、事件公平调度。
- 精确 TurnRef/ToolKey、未确认保存状态、取消后补读。
- 多 Session 和后台 Loop、模型下一请求生效、真实 Steer receipt。
- CJK/Emoji/grapheme、IME 真实光标、粘贴/undo、Rail scrollbar 与点击复制。
- 原始 Store/RPC 隐私边界、日志脱敏、错误分类。
- 现有 bug regression tests：如果旧实现被替换，迁移相同的用户可见断言，不能仅删除失败测试。

### 22.3 禁止的“重构成果”

不能仅移动函数然后保留全量复制/所有旧标记；不能通过减少消息类型或删掉 Steer/复制/后台 Session 达成删行数；不能以更改快照接受 Rail UI 退化。

---

## 23. 文件与方法级实施清单

| 当前文件/方法 | 操作 | 目标/验收 |
|---|---|---|
| `Cargo.toml` | 保留框架版本与 MSRV；建议版本改 0.3.0 | 不同时升级 Ratatui/Crossterm/编辑器 |
| `protocol.rs::is_supported_agent_version` | 删除 package-minor gate | `protocol/handshake.rs::validate_backend` |
| `protocol.rs::parse_frame` | 保留 envelope 严格校验；更新 Event/响应 decode | 新 Protocol v1 fixture 与 unknown additive 字段测试 |
| `protocol.rs` 旧 History DTO | 不再复用到 session.read | `protocol/read.rs` 原始 envelope + ChunkAssembler |
| `rpc.rs::RpcProcess::send` | UI 路径改快速 try_send | 队列满不阻塞输入；未发送/未确认分开 |
| `rpc.rs::stdout_reader` | 加总 wire-byte budget | Frame 所有权释放额度；无响应静默丢弃 |
| `rpc.rs::terminate_with_observer` | 保留排空/回收责任 | 完成响应/末尾 stderr 不因 Child exit 被提前丢弃 |
| `main.rs::run_commands` | 改为启动 jobs，不等待结果 | 剪贴板/导出/外部编辑器慢时 App 活着 |
| `main.rs::prepare_frame` | 从全 conversation 准备改为视口及必要布局 | 输入/spinner 不导致历史重排 |
| `app.rs` | 留 update 分发、全局导航、少量时钟 | 业务方法移到有限 app 子模块 |
| `app.rs::RequestKind` | 统一普通/reload wait；新增 typed Query/Compact 来源 | 没有 ReloadHistoryStage/全局历史事务 |
| `app/session.rs`（新增） | browse/open/close/rename/update/reload | metadata 与执行/历史读取边界分清 |
| `app/turn.rs`（新增） | submit/prepare/cancel/wait/result/steer | 每种身份有明确 owner，迟到结果不丢 |
| `app/queries.rs`（新增） | 两在途槽、局部 generation、刷新合并 | 不变成通用 scheduler/DI |
| `app/history.rs`（新增） | snapshot pin/window/increment/result merge | Session 全局 index 与 Turn 局部 index 不混 |
| `app/ui_actions.rs` | 保留点击/选区/scroll，拆出面板事件 | renderer 不发 RPC，长 IO 不在鼠标 handler |
| `state/session.rs` | 将分散 flags 收敛为具体操作/读取状态 | 非法组合由类型/方法约束，不靠到处 reset |
| `state/transcript.rs` | 逐步由 history 规范化对象替代 | 提供迁移门面短期过编译；最终只有一份正文 |
| `state/tool.rs` | ToolFacts 单索引 | 回填直接定位；不创建多份大输出 |
| `state/view.rs` | 源锚点、LayoutKey、VisibleConversation | 点击/复制/绘制同几何 |
| `state/composer.rs` | byte_len/delta/布局缓存/每 Session所有权 | 普通输入不多次 join 全文 |
| `ui/transcript.rs::prepare_conversation` | 改为调用分段布局/视口组合 | 不 clone 全 durable lines |
| `ui/transcript.rs::build_durable_prepared` | 逐块缓存，去掉 Tool 全表查找 | 新页追加/单块折叠局部失效 |
| `ui/transcript.rs::selection_text` | 使用 SourceMap 逻辑范围 | 复制无装饰、softwrap 不加换行 |
| `ui/composer.rs` / `editor_layout.rs` | 复用现有 Rail/IME 映射 | 不更换编辑器和颜色几何 |
| `ui/footer.rs` | 消费真实 cached observation | 一行；ctx 估算/unknown、branch last observation |
| `ui/selector.rs` 与既有 Dock | 加 browse/execute、文件、设置入口 | 草稿与旧选择保留 |
| `ui/tool_detail.rs` / `file_preview.rs` / `changes.rs`（新增） | 具体详情 render | 无通用 Panel trait |
| `clipboard.rs` / `jobs.rs` / `export.rs` | 明确任务执行与提交结果 | typed errors、无 raw 内容日志 |
| `command.rs` / `keymap.rs` | 单命令表，焦点优先级，F6 | completion/help/parse 一致 |
| docs/fixtures/tests | 指向固定 Agent 0.5；旧设计标 superseded | 保留历史文档但不再作为当前行为说明 |

### 23.1 依赖控制

保留当前 Cargo.lock 和 Rust 1.85.0。当前 Ratatui 0.29.0、Crossterm 0.28.1、tui-textarea 0.7.0、pulldown-cmark 0.12.2 等不借本轮升级。[S1]

允许为直接需求增加：base64 解码库、可靠临时文件工具；将现有 dev `toml` 依赖移到 runtime 支持 UI 设置；按需开启 tokio `fs`。取消 token 可复用已有机制或增加一个 tokio-util 依赖，不同时引入另一套 task 框架。具体版本必须经 Rust 1.85 CI 验证后锁定。

禁止新增 axum、gRPC、WebSocket、数据库、通用 DI、状态管理框架、动态库插件、通用虚拟列表框架。

---

## 24. 确定性测试设计

### 24.1 测试分层

| 层 | 方式 | 验证对象 |
|---|---|---|
| Protocol | 固定 Agent 源码/真实进程生成的脱敏 fixture | 实际字段、raw item、chunk、opaque cursor |
| Transport | tokio duplex + scripted child | partial frame、背压、退出、乱序 |
| State | 向 App 注入确定性事件，不开终端 | 各操作状态/身份/迟到响应 |
| Read/model | fake query pages +真实 decoder | 分块、pin、窗口、去重、来源 |
| UI | TestBackend/现有快照 | Rail 几何、焦点、窄屏、复制/命中 |
| Performance | Release synthetic workload | 重排次数、克隆字节、延迟/内存 |
| Integration | 固定 Agent 0.5 二进制 + loopback Model | 真请求/真实文件/工具/压缩/读取 |

默认测试离线，不要求 API key、不访问真实 Workspace。真实 Agent E2E 可以由 CI 单独 job 构建固定后端后运行；不是所有集成测试都永久 ignored。真实 Provider live smoke 可继续手动 ignored，并单独报告。

### 24.2 Protocol fixture 必须重新生成

新增 `tests/fixtures/agent-v1/manifest.json` 记录 Agent/Runtime HEAD、protocol_version、生成脚本、脱敏规则。必须含：

```text
ping、catalog、session state idle/running/preparing/compaction/blocked
session.read 首页面、半个item、同item结束、records_truncated、trailing_incomplete
turn.result pending/live-failed/stored
tool.read awaiting_policy/running/terminal + command
tool.output utf8/base64/pending/gap/partial/expired/clean-empty-EOF
workspace.read line-partial/changed/binary/too-large
workspace.files/search partial-page/deadline
workspace.status detached/unavailable
changes.list/diff complete/partial/stale/fragments
compact compacted/noop/failed/unknown_write
```

`session.read` chunks 必须是**真实 Runtime item envelope**，不能把旧 `HistoryItemView` 包个 chunk 外壳冒充。测试将原 raw canonical item 分到多个 UTF-8 边界，再验证复原与标准序列化相等。[S7][S8]

### 24.3 关键时序必须逐一覆盖

1. TurnStarted/RequestStarted 先于 send ACK；ACK 只绑定同一 Loop，随后只注册一个 wait。
2. 自动准备超过原请求超时习惯但未返回 Loop：界面仍响应，不能重复 send。
3. Esc 早于 preparation ID；ID 返回后仅取消对应操作；若 Loop 先启动则取消对应 Loop。
4. wait 到来时 Event 最后几帧尚在队列：按权威结果完成，不因迟到 Delta 重开已完成 Loop。
5. 保存失败，Live 又有 gap：turn.result 补齐 retained report，仍显示保存未确认。
6. 用户关闭 Tool A 后打开 Tool B，A query 迟到：只释放槽、不污染 B、不取消 B。
7. Session close/reopen 期间旧 state/history 到达：读取 generation 拦截；旧精确 wait 仍记录结果。
8. reload 发生在运行中：History/Live/draft 没有被清空或重新提交。
9. rename 成功后旧 session.list 返回：不会恢复旧 title。
10. Steer ACK 到达时用户已输入新文字：新文字保留；重复文本按 occurrence/receipt 对齐。
11. 当前 Loop 结束，本地仍有未发送 Steer：没有新 turn.send；显示待处理。
12. Tool stdout Event 和分页返回重叠：原始字节只出现一次。
13. 文件第一半页后发生变化：后半页不混入旧 revision。
14. Diff 页间版本变化：stale 不自动拼成混合 diff。
15. 子进程退出先于末尾 response/stderr：主循环不提前丢掉已在管道中的结果。
16. Query 等待页面关闭：远端槽在真实响应前仍计数，无超并发请求。

### 24.4 功能测试文件建议

```text
tests/protocol_v1.rs
tests/read_chunks.rs
tests/session_flows.rs
tests/turn_control.rs
tests/compaction_flows.rs
tests/tool_streams.rs
tests/workspace_queries.rs
tests/changes_review.rs
tests/editor_drafts.rs
tests/search_export.rs
tests/rpc_backpressure.rs
tests/rail_snapshots.rs
tests/performance.rs
tests/agent_v1_e2e.rs
```

现有等价 test 文件直接扩展，不为清单形式强制创建重复文件。

---

## 25. 性能验收：测结构，再测时间

### 25.1 必须通过的结构性指标

在测试专用 `PerfCounters` 中记录 layout calls、historical text bytes cloned、viewport rows materialized、pending queries、retained bytes。不得为指标引入线上 telemetry 平台。

- 5 万已加载显示行，当前 Request 连续 1000 个 Delta：未逐出的稳定历史块重排次数为 0；历史正文复制字节为 0。
- 单次 viewport preparation 的 owned rows 与 viewport+overscan 成比例，不与整历史行数成比例。
- Tool lookup 在投影时建索引，更新时不调用全 blocks 搜索；基准中 Tool 数增长不产生平方级重建。
- 光标移动、spinner、选择拖动不会触发内容 Markdown 解析。
- 256 KiB 粘贴后的普通字符输入：没有每字符多次 join 全文的调用链。
- 隐藏 Tool/文件面板没有周期 query；Query 数保持本 Spec 上限。
- 离开大量 Session 后缓存预算回落；不能只递增、不释放。

### 25.2 时间指标（固定机器 Release build）

| 场景 | 目标 |
|---|---|
| 120×40、5 万显示行背景、持续流式输出 | 输入到下一帧 P95 < 50ms；记录 P99 |
| 256 KiB 输入草稿持续编辑 | 单次编辑 P95 < 30ms，不能可见秒级卡顿 |
| 空闲 30 秒 | 无内容变化时 draw 次数不因 heartbeat 持续增长 |
| 剪贴板 helper 挂起 2 秒 | 期间输入、滚动、取消与 RPC 仍被处理 |
| Agent 暂停读 stdin 且输出大量事件 | TUI 仍响应控制，无发送/接收环形死锁 |
| resize 长历史 | 输入仍可用；重排分批，保持阅读锚点 |

这些不是共享 CI 的硬毫秒断言。CI 检查前述计数、额度与确定性时序；固定工作站记录延迟分布、CPU、峰值 RSS 和缓存容量。优化前后用相同 fixture、终端尺寸、build profile 比较。

### 25.3 性能失败时的取舍

先消除全量复制/重复布局/无谓 IO，再决定是否替换容器。禁止在未测量前引入 Rope、通用增量解析器、Fenwick tree、专用 actor 框架。

不能通过丢弃消息、把旧历史偷偷截断成摘要、禁用选择复制或把所有 Tool 默认隐藏来通过性能测试。

---

## 26. 整体验收矩阵

每项在 `docs/refactor-acceptance.md` 中对应测试名/命令/证据。状态用 Passed / Failed / Not run / Not applicable，不使用未经度量的“完成度 98%”。

| ID | 必须证明的行为 | 主要位置 |
|---|---|---|
| REF-01 | 对接固定 Agent 0.5 / Protocol 1，不依赖 Agent/Runtime crate | protocol、E2E |
| REF-02 | 协议版本与 capability 检查；无 0.3 fallback | handshake tests |
| REF-03 | 保留 extended reasoning，选择不静默降级 | catalog/update tests |
| REF-04 | Response/Event 任意交错，Partial frame 和 EOF 正确 | transport tests |
| REF-05 | 发送满不阻塞 UI，普通请求未接受时草稿保留 | backpressure tests |
| REF-06 | 控制余量不重排已入队同 Session 请求 | FIFO test |
| REF-07 | read/deferred/字节预算真实执行，过期 query 仍计在途 | query tests |
| REF-08 | 异步副作用不阻塞输入/RPC，任务有 owner | job tests |
| REF-09 | session.read 解码真实 Runtime item，跨页 UTF-8 复原 | chunk tests |
| REF-10 | pinned 前缀/非零 cursor/更新 pin 的流程合法 | snapshot tests |
| REF-11 | Session 全局 index 与 Turn 局部 index 不混用 | result tests |
| REF-12 | 只读浏览不 open Session、不要求 Workspace 可用 | real Agent test |
| REF-13 | 大项/缺页/records_truncated/trailing_incomplete 不伪装完整 | read tests |
| REF-14 | send 可 deferred；准备过程可见且不会自动重发 | preparation flow |
| REF-15 | 取消分别使用明确 operation ID 或 TurnRef | cancel races |
| REF-16 | 手动 compact 四种结果和 unknown_write 正确 | compact flow |
| REF-17 | Context 估算范围、Utility Usage 与普通 Usage 分开 | context tests |
| REF-18 | Update 下一 Request 生效；当前 Tool 不改标签 | update+tool flow |
| REF-19 | Steer 接受/应用/记录分开；不跨 Loop 自动变 Prompt | steer flow |
| REF-20 | ACK 不清除新 revision 草稿 | editor/steer test |
| REF-21 | wait 失败/丢事件可精确 turn.result；不会重跑工具 | result recovery |
| REF-22 | 保存失败 retained result 可读，Blocked 与 unknown 正确 | persistence flow |
| REF-23 | 取消不宣称文件回滚，关闭不丢尚在接收的结果 | lifecycle test |
| REF-24 | reload 不清 History/Live/draft，不重装整历史 | reload test |
| REF-25 | 每 Session 草稿/Undo/粘贴/光标独立 | draft tests |
| REF-26 | 新建/继续/rename/删除语义明确，无跨项目猜测 | session tests |
| REF-27 | 稳定正文单份 Arc，Tool 单索引 | model/alloc test |
| REF-28 | Live 更新零历史全文 clone，零稳定历史重排 | perf counters |
| REF-29 | viewport/click/copy 共用布局；softwrap 不添加复制换行 | layout tests |
| REF-30 | 折叠、resize、历史补页保持锚点 | viewport tests |
| REF-31 | Rail 主区/Editor/单行 Footer 基线保持 | snapshots |
| REF-32 | 面板开关/焦点不误发送、不误取消 | focus tests |
| REF-33 | Tool 卡片不按完成时间重新排序，不重复插入 | tool ordering |
| REF-34 | tool.read 区分 awaiting_policy、running、terminal | facts tests |
| REF-35 | base64/raw offset/UTF-8 尾部/gap/EOF 全部正确 | stream tests |
| REF-36 | stdout/stderr 不伪造总顺序；终止请求≠确认 | command tests |
| REF-37 | 仅可见工具按需读取，隐藏停止；partial/expired可见 | query/UI tests |
| REF-38 | @file 明确路径引用，预览不偷偷附加内容 | editor/files flow |
| REF-39 | workspace.files/search partial 和 cursor 规则正确 | workspace tests |
| REF-40 | workspace.read 同版本跨行内分页，changed 不混拼 | file tests |
| REF-41 | Changes workspace/tool 来源与三种对比正确 | diff tests |
| REF-42 | opaque change_ref 不解析；stale/分行片段正确 | diff pagination |
| REF-43 | Footer branch 来自显式 status 观察，renderer 无 IO | presentation tests |
| REF-44 | 会话搜索有覆盖说明，未加载/超大项不报全局无匹配 | search tests |
| REF-45 | Prompt 跳转、临时折叠展开不破坏用户选择 | navigation tests |
| REF-46 | 复制/导出无 Rail/虚假换行；导出固定 pin、内存有界 | export tests |
| REF-47 | 外部编辑器期间后台 RPC 继续，返回不覆盖别的草稿 | editor job tests |
| REF-48 | ANSI/OSC/control 显示安全且不改变后端 offset | text tests |
| REF-49 | 日志不含消息/命令/结果/文件/secret | redaction tests |
| REF-50 | 所有 cache/queue 有界且后台会话可释放大缓存 | stress tests |
| REF-51 | 原 CJK/IME/鼠标/scrollbar/Terminal restore 回归保持 | regression suite |
| REF-52 | 常见命令表、补全、帮助一致 | action table test |
| REF-53 | 无审批/插件/Subagent/PTY/Git 写操作/自动重连 | scope inspection |
| REF-54 | 固定 Agent 进程 E2E 覆盖读取/工具/压缩/文件/Diff | CI integration |
| REF-55 | Rust 1.85、stable、三平台原测试继续通过 | CI |
| REF-56 | Release 性能前后对比有真实数据，无未执行冒充通过 | perf report |

---

## 27. 实施阶段与合并门槛

### 阶段 A：锁定回归与协议事实

**提交建议：**`test(refactor): pin rail behavior and agent v1 fixtures`

任务：固定三个 HEAD；保存当前 Rail snapshots；导出命令清单与现有 E2E 场景；从真实 Agent 0.5 获取新 DTO fixtures；增加发送背压、长历史 clone、Preparing 的失败测试。

交付：`docs/backend.md`、`tests/fixtures/agent-v1/`、基线性能记录。此时不升级依赖、不改主题、不改执行行为。

门槛：能够清楚证明旧版本的问题，而不是先重写后根据新行为修改全部测试。

### 阶段 B：Protocol v1 与执行正确性迁移

**提交建议分两笔：**

```text
refactor(protocol): consume agent v1 authoritative read and control APIs
fix(control): handle deferred admission compaction and retained results
```

任务：握手、新 Read/Tool/Context DTO、chunk decoder、Submission/取消/压缩、turn.result、generation、旧协议主路径删除。

交付：可通过固定新 Agent 完成 send→tool→wait→read；压缩准备可取消；保存失败能读取 retained report。UI 可暂沿用旧布局模型的短期桥接，但桥接在阶段 C 删除，不作为长期第二份正文。

门槛：REF-01～23 中相关项通过，既有 Steer/换模不能退化。

### 阶段 C：状态收敛与性能主干

**提交建议：**

```text
refactor(app): separate execution reads and view state
refactor(render): use shared sections and viewport-only composition
fix(io): keep clipboard and rpc backpressure off the ui loop
```

任务：拆 app 职责、简化 reload、窗口数据/单 Tool 索引、块缓存/SourceMap、非阻塞发送和 jobs、Composer 长文本路径。

交付：没有全历史 Line clone；现有 Rail 屏幕基本不变；缓存与副作用有界。

门槛：REF-24～32、48～51 及结构性性能指标通过。不能以“下阶段再优化”留下新面板依赖的旧慢路径。

### 阶段 D：常用 Session/Editor/阅读工作流

**提交建议：**

```text
feat(workflow): add isolated drafts read-only browsing and quick continue
feat(reading): add search navigation copy and bounded export
feat(editor): add an owned external-editor workflow and ui preferences
```

任务：独立草稿、browse/continue、rename、搜索/Prompt 跳转/复制/导出、Settings/外部编辑器、统一命令表。

门槛：用户可开始、持续对话、找回输入、查阅与带走结果；没有为导出或搜索无限加载全历史。

### 阶段 E：工具、文件和改动审查

**提交建议：**

```text
feat(tools): add a read-only detail view with bounded process streams
feat(workspace): add file references previews and literal search
feat(changes): add scoped read-only change review
feat(context): expose manual and automatic compaction state
```

任务：按第 13～16 节实现具体面板、轮询/分页策略，完善 Context 页与 Footer 来源。面板使用阶段 B/C 的数据模型，禁止再自建一套 RPC owner。

门槛：关闭面板不停止执行；新功能没有增加常驻 Sidebar、多窗口布局或文件副作用。

### 阶段 F：验证与正式收口

**提交建议：**`test(release): verify full coding workflows against pinned agent 0.5`

任务：全套 E2E、三平台、性能、Terminal/剪贴板/外部编辑器手动验证；删除临时 adapter/死 flags/重复正文；更新 README/帮助/迁移说明，建议版本 0.3.0。

只有全部必交付项目达到验收才标为完成；某阶段可单独合并不等于整轮开发完成。

---

## 28. 验证命令与报告

每个提交至少：

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
```

文档检查跨平台不要依赖 Unix 一行环境变量写法：CI 用 `env: RUSTDOCFLAGS: -D warnings`，运行 `cargo doc --locked --no-deps`。

保留现有 Rust 1.85/stable 与 Ubuntu/macOS/Windows job。依赖检查使用 `cargo tree -d` 检查真正不兼容的重复实例，不把整个依赖图中任意重复版本都判失败。

性能：

```bash
cargo test --release --locked --test performance -- --ignored --nocapture
```

真实 Agent E2E 由测试 harness 配置明确的 `MINICORE_AGENT_BIN` 和临时测试 config，Provider 指向受测试控制的 loopback mock。固定 Agent 构建在独立 CI job/目录完成，TUI Cargo 不增加 Agent/Runtime 依赖，不在普通 build.rs 中 clone/编译后端。

### 28.1 交付文件

```text
docs/backend.md                固定后端/协议/capabilities/fixture来源
docs/architecture.md           新owner、数据流、缓存和取消规则
docs/keybindings.md            实际支持的动作和焦点优先级
docs/migration-0.2-to-0.3.md    breaking changes与保留行为
docs/refactor-acceptance.md    REF-01～56的证据
docs/performance.md            基准机器、Release参数、前后数据、限制
CHANGELOG.md                   用户可见变更
```

不要把所有历史 Spec/临时 agent 讨论复制进当前架构文档。历史文档标 superseded 可保留，当前 README 只指向实际生效的契约。

### 28.2 开发 Agent 最终报告

必须列出起始/最终 HEAD、提交列表、保留/替换模块、旧代码删除、RPC 方法覆盖、用户可见 breaking changes、测试结果、性能数据和未执行项。真实 Provider/终端手工测试没有执行就写 Not run。

不输出 API key、用户文本、真实工具输入输出或不受控日志；测试数据使用 synthetic 内容。

---

## 29. 重要设计选择的最终裁决

| 问题 | 本 Spec 的选择 |
|---|---|
| 整个项目重写还是局部改几个面板？ | 重构协议、状态、数据和性能主干，保留成熟 Rail/Terminal/编辑引擎；再交付基础工作流 |
| 直接引入 Agent Rust crate 提速？ | 不采用；先修全量复制、阻塞和查询路径 |
| 新建 TUI framework？ | 不采用；具体 App/MainView/QuerySlots/Jobs 即可 |
| 旧 Agent 0.3 兼容？ | 新版本不维护；Agent 本身旧接口不删除 |
| Event 丢失怎么办？ | 精确结果/History/Tool 查询确认，不重放执行 |
| 保存失败就丢弃 Live？ | 不；读取 retained result，保留未确认语义 |
| 所有 History 都常驻吗？ | 不；固定前缀、窗口、字节预算、可见缺口 |
| 能按全部 History 建一份大显示数组吗？ | 不；共享块布局，视口组合 |
| 需要高级虚拟列表/增量 parser 吗？ | 先不用；共享稳定前缀+活动尾部和具体块缓存足够 |
| Detail 是侧栏还是新任务页？ | 替换主区的只读视图；对话中的 Rail Tool 卡片仍是默认形态 |
| @file 是内容附件吗？ | 本期只是路径引用与预览，明确告知；不隐式附加 |
| Follow-up 自动运行？ | 不；未发 Steer 留给用户，不跨 Loop 自动转 Prompt |
| Compaction 要改 Runtime？ | 不；用 Agent 已有能力，只补正确 UI/取消/结果 |
| Diff 能回滚代码吗？ | 不；本期只读 |
| 关闭查询面板等于取消 Agent query？ | 不；停止后续读取，已发请求继续记在途直到完成 |
| 是否借机实现审批/插件/Subagent？ | 不 |

---

## 30. 源码依据与开发定位

以下是本 Spec 的固定参考。要求开发时阅读实际源文件，不以旧回答中的版本百分比或伪代码推断当前 API。

- **[S1] TUI 基线与依赖**：[`Cargo.toml`](https://github.com/zqcli/minicore-tui/blob/9d11ee69c4efa02ef1e5bff143662b48dc3194de/Cargo.toml)、[`src/protocol.rs`](https://github.com/zqcli/minicore-tui/blob/9d11ee69c4efa02ef1e5bff143662b48dc3194de/src/protocol.rs)。
- **[S2] Agent 基线**：[`0617433`](https://github.com/zqcli/minicore-agent/commit/061743369459299e66be97bf97d2b27352a39914)、该提交 `README.md` 和 `Cargo.toml`。
- **[S3] Runtime 边界**：[`README.md`](https://github.com/zqcli/minicore-runtime/blob/6cd2bdbc634437dea925495c61c7eb0be10ba171/README.md)。
- **[S4] Agent Protocol v1**：[`docs/rpc.md`](https://github.com/zqcli/minicore-agent/blob/061743369459299e66be97bf97d2b27352a39914/docs/rpc.md)，握手、额度、Workspace、Changes、控制与错误章节。
- **[S5] Tool / Turn 结果**：同一 `docs/rpc.md` 的 `turn.wait`、`turn.result`、`tool.read`、`tool.output` 及 process stream 章节。
- **[S6] Context / Compaction**：同一 `docs/rpc.md` 的 `session.compact`、`session.compact.cancel`、`session.context`，以及实际 `src/sessions/` 的准备/压缩实现。
- **[S7] 完整读取 DTO 与编码**：[`src/read.rs`](https://github.com/zqcli/minicore-agent/blob/061743369459299e66be97bf97d2b27352a39914/src/read.rs)，`ReadCursor`、`ReadItemChunk`、`ReadSessionResult`、`TurnResultPage`、`ReadItemEnvelope`、`pack_items`。
- **[S8] 原始 History 结构**：[`src/history.rs`](https://github.com/zqcli/minicore-runtime/blob/6cd2bdbc634437dea925495c61c7eb0be10ba171/src/history.rs)。TUI 只在自己的 protocol adapter 表达其 JSON，不直接依赖 Runtime。
- **[S9] 真实 Event**：[`src/event.rs`](https://github.com/zqcli/minicore-agent/blob/061743369459299e66be97bf97d2b27352a39914/src/event.rs)，RequestUsage、SteerProgress、ToolInvocation/Execution/Process。
- **[S10] TUI 当前性能/状态路径**：[`src/ui/transcript.rs`](https://github.com/zqcli/minicore-tui/blob/9d11ee69c4efa02ef1e5bff143662b48dc3194de/src/ui/transcript.rs)、[`src/state/session.rs`](https://github.com/zqcli/minicore-tui/blob/9d11ee69c4efa02ef1e5bff143662b48dc3194de/src/state/session.rs)、[`src/state/composer.rs`](https://github.com/zqcli/minicore-tui/blob/9d11ee69c4efa02ef1e5bff143662b48dc3194de/src/state/composer.rs)、[`src/main.rs`](https://github.com/zqcli/minicore-tui/blob/9d11ee69c4efa02ef1e5bff143662b48dc3194de/src/main.rs)、[`src/rpc.rs`](https://github.com/zqcli/minicore-tui/blob/9d11ee69c4efa02ef1e5bff143662b48dc3194de/src/rpc.rs)。
- **[S11] 任务取消语义**：[Tokio `spawn_blocking` 官方文档](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)；实现时结合锁定 Tokio 版本验证，不把 async timeout 当作 OS 工作已停止的证据。
- **[S12] 文件搜索**：同一 Agent `docs/rpc.md` 中 `workspace.search`，字面匹配、UTF-8 范围、遍历/分页/不完整结果契约。

### 30.1 开发执行指令

请基于本 Spec 对 **整个 minicore-tui** 实施重构，而不是只新增几个面板。

先固定回归和新协议，再改状态与数据路径，再做性能、工作流和面板。每阶段交付可审查提交。不得升级后端、修改 Runtime、自动迁移 Agent Store 或添加未授权执行能力。

优先用已有成熟代码实现目标。允许删掉旧兼容和补丁式状态，但每一项用户已有核心行为都要有迁移测试；不能通过删能力让代码看起来更短。

最终交付应同时证明：**Agent 0.5 对接正确、Rail UI 保持、长会话/长输出不拖垮主循环、常用操作完整、状态和失败含义可信、代码责任清楚。**
