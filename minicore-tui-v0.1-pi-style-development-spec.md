# MiniCore TUI v0.1：Pi 风格完整 Rust TUI 开发实施规格

> **SUPERSEDED / 已废弃**: 本规范为 v0.1 早期版本，已被 [`minicore-tui-v0.2-agent-v0.3-runtime-v0.4-migration-spec-r2.md`](minicore-tui-v0.2-agent-v0.3-runtime-v0.4-migration-spec-r2.md) 完整取代。请参考 r2 迁移规范。

## 0. 文档用途

本文档用于指导代码 Agent 创建一个新的 Rust 项目：

```text
minicore-tui
```

它是 `minicore-agent` 的终端交互前端，通过 stdio JSON-RPC 与 Agent 通信。

本文档的目标不是开发新的 Agent Runtime，而是：

> 使用 Rust 实现一个视觉结构、交互层级和组件风格与 Pi Coding Agent TUI 高度一致的完整 Coding Agent TUI，并以现有 `minicore-agent` 作为唯一后端。

本项目应做到：

```text
界面像 Pi
交互习惯接近 Pi
代码是独立 Rust 实现
后端能力严格来自 minicore-agent
不复制 Pi 源码
不复制 Pi 品牌或 Logo
不在 TUI 中重做 Agent 能力
```

本规格是可以直接执行的开发要求，包含：

- 产品边界；
- 技术选型；
- Rust crate 结构；
- RPC 子进程与协议处理；
- App 状态；
- Pi 风格布局；
- 精确配色；
- 消息、Reasoning、Tool、编辑器、Footer、选择器；
- 多轮和多 Session；
- 键盘交互；
- Markdown 渲染；
- 性能与终端恢复；
- 测试、快照和验收矩阵；
- 分阶段提交计划。

---

# 1. 参考基线

## 1.1 Agent 后端

```text
repository:
https://github.com/zqcli/minicore-agent

branch:
dev
```

开发开始前必须记录 Agent 当前实际 HEAD，并阅读：

```text
README.md
docs/rpc.md
src/rpc/protocol.rs
src/event.rs
src/agent.rs
```

本文档依据的 Agent 能力包括：

```text
stdio JSON-RPC
单 RPC 客户端
model.list
profile.list
session.list
session.create
session.open
session.close
session.delete
session.state
session.transcript
turn.send
turn.cancel
turn.wait
agent.event
Text/Reasoning 流式输出
ToolStarted/ToolProgress/ToolFinished
创建 Session 时选择模型与 reasoning
同一 Session 多轮
多个 SessionRuntime 并行
```

如果 Agent 的实际协议已经变化，应以 `docs/rpc.md` 和行为测试为准，更新 TUI wire DTO；不得通过直接依赖 Agent Rust crate 绕过 RPC。

## 1.2 Pi TUI 参考范围

UI 参考项目：

```text
repository:
https://github.com/earendil-works/pi
```

重点参考：

```text
packages/tui/
packages/coding-agent/src/modes/interactive/
packages/coding-agent/src/modes/interactive/components/
packages/coding-agent/src/modes/interactive/theme/
```

目标参考的是 Pi 当前的：

```text
alternate-screen / fullscreen interactive layout
```

其核心视觉层级：

```text
可滚动 Transcript
+
底部固定 Dock
    ├── Status
    ├── Editor 或临时 Selector
    └── Footer
```

Pi 的 TUI 同时支持 main-screen 和 alternate-screen；本项目 v0.1 只实现 fullscreen alternate-screen。

原因：

- Rust Ratatui 对全屏固定布局支持成熟；
- Transcript 滚动、Footer 固定和 Overlay 更直接；
- 能高度复现 Pi fullscreen 模式；
- 不需要首版同时处理原生终端 scrollback；
- 减少双渲染模式和退出打印逻辑。

本项目不复制：

```text
Pi Logo
Pi 名称
Pi 包名
Pi 源码
Pi Extension UI
Pi 图片协议
Pi Session Tree
```

---

# 2. 产品目标

## 2.1 完整主流程

用户可以：

```text
启动 minicore-tui
→ TUI 启动 minicore-agent 子进程
→ 查询 Models / Profiles / Sessions
→ 创建或打开 Session
→ 加载 Transcript
→ 输入多行消息
→ 发送 Turn
→ 查看 Text 流式输出
→ 展开或隐藏 Reasoning
→ 查看 Tool 卡片和状态
→ 取消 Turn
→ 可靠等待 Turn outcome
→ 用 durable Transcript 对齐最终界面
→ 继续多轮对话
→ 切换到其他 Session
→ 创建使用其他 Model/Reasoning 的新 Session
→ 正常退出并恢复终端
```

## 2.2 “界面和 Pi 一样”的定义

本规格中的“一样”指：

```text
视觉层级相同
消息间距与卡片关系相同
User Message 使用有背景卡片
Assistant Message 使用无背景正文
Reasoning 使用灰色斜体并可折叠
Tool 使用 Pending/Success/Error 背景卡片
Editor 使用动态 reasoning 颜色边框
Footer 使用紧凑双行信息
Selector 临时替换 Editor/Dock 主体
快捷键与 Pi 主要操作保持接近
整体低噪声、紧凑、终端原生
```

不要求：

```text
字符级完全相同
复制 Pi ASCII Logo
拥有 Pi 当前全部命令
拥有 Pi Extension UI
拥有 Pi 所有 Session 功能
拥有 Pi 图片、Mermaid 或 OAuth UI
```

## 2.3 当前后端无法支持的 Pi 功能

以下能力不属于 TUI v0.1：

```text
当前 Session 热切 Model
当前 Session 热切 Reasoning
真正 Steering
Agent 后端 Follow-up Queue
Session Fork / Branch / Tree
Compaction
实时 Bash stdout/stderr
PTY
@file 模糊文件引用
!command
审批 UI
图片输入与输出
MCP
Skills
Plugin UI
Remote Agent
```

不得在 TUI 中伪造这些能力。

---

# 3. 分层边界

## 3.1 MiniCore Runtime

负责：

```text
单 Session Agent Loop
Conversation durable semantics
Model → Tool → Model
Turn 串行
ToolCall / ToolResult 匹配
Cancellation
Terminal
Restart repair
```

TUI 完全不依赖 `minicore-runtime`。

## 3.2 MiniCore Agent

负责：

```text
Session 管理
Model/Profile 列表
Session 创建时绑定 Model/Reasoning
Store
Workspace
Tool
Context
Model Provider
Turn send/cancel/wait
Transcript
Agent Event
```

TUI 只通过 RPC 使用这些能力。

## 3.3 MiniCore TUI

负责：

```text
终端生命周期
RPC 子进程
RPC frame 分发
界面状态
输入编辑
滚动
渲染
选择器
快捷键
Slash commands
Live Event 展示
Durable Transcript 对齐
错误展示
```

TUI 不直接：

```text
读写 Workspace
执行 Bash
调用 OpenAI
解析 Agent Store 文件
创建 SessionRuntime
决定 Tool 是否可运行
修改 Session Manifest
```

---

# 4. 明确不做的过度设计

v0.1 禁止引入：

```text
通用 TUI Framework
Component Trait 树
Widget Registry
Redux
Elm Framework
通用 Effect Runtime
Dependency Injection
Service Locator
Plugin Registry
Dynamic Theme Loader
Dynamic Keybinding Manager
Protocol Code Generator
Agent Rust SDK
Event Replay
断线自动重连
多 Agent 子进程
多 RPC Client
WebSocket
HTTP
SQLite
Persistent UI State
```

允许使用小型具体 enum：

```text
AppEvent
AppCommand
Dock
Overlay
RequestKind
```

它们是当前应用的数据结构，不是框架。

---

# 5. 技术选型

## 5.1 Rust

```text
edition = 2024
rust-version = 1.85
unsafe_code = forbid
```

与 `minicore-agent` 的 MSRV 保持一致。

## 5.2 TUI

固定首版依赖：

```toml
ratatui = { version = "=0.29.0", default-features = false, features = ["crossterm"] }
crossterm = { version = "=0.28.1", features = ["event-stream"] }
tui-textarea = { version = "=0.7.0", default-features = false, features = ["crossterm"] }
```

理由：

- `tui-textarea 0.7.0` 与 Ratatui 0.29 / Crossterm 0.28 对齐；
- Ratatui 0.29 满足 Rust 1.85；
- 不升级到要求更高 MSRV 的 Ratatui 0.30；
- 避免同一程序编译两份不兼容 Crossterm。

CI 必须运行：

```bash
cargo tree -p crossterm
```

并确认只有一个 Crossterm 主版本实例。

## 5.3 其他依赖

建议：

```toml
tokio = { version = "1", features = [
  "rt-multi-thread",
  "macros",
  "sync",
  "time",
  "io-util",
  "process",
  "signal",
  "fs"
] }

tokio-util = { version = "0.7", features = ["rt"] }
futures-util = "0.3"

serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"

tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }

unicode-width = "0.2"
unicode-segmentation = "1"
tui-markdown = "0.3"
time = { version = "0.3", features = ["formatting", "parsing"] }
```

Dev：

```toml
insta = "1"
tempfile = "3"
pretty_assertions = "1"
```

不引入：

```text
clap derive 大型命令树
tokio-tungstenite
axum
tonic
syntect（v0.1）
arboard（v0.1）
ropey（v0.1）
```

CLI 参数可以使用：

```text
std::env::args
```

或非常小的 `clap` 配置。若使用 Clap，只定义一个平面结构。

---

# 6. Crate 组织

## 6.1 一个 Package

创建：

```text
minicore-tui/
```

一个 Cargo package，包含：

```text
src/lib.rs
src/main.rs
```

`lib.rs` 的目的：

- 让核心状态、RPC codec 和渲染可测试；
- 不代表提供稳定公共 SDK；
- Public export 保持极少。

不要拆成 workspace 和多个 crate。

## 6.2 目录

```text
minicore-tui/
├── Cargo.toml
├── Cargo.lock
├── README.md
├── LICENSE-MIT
├── LICENSE-APACHE
│
├── src/
│   ├── lib.rs
│   ├── main.rs
│   ├── args.rs
│   ├── error.rs
│   ├── terminal.rs
│   ├── theme.rs
│   ├── rpc.rs
│   ├── protocol.rs
│   ├── app.rs
│   ├── event.rs
│   ├── command.rs
│   ├── keymap.rs
│   ├── markdown.rs
│   │
│   ├── state/
│   │   ├── mod.rs
│   │   ├── catalog.rs
│   │   ├── session.rs
│   │   ├── transcript.rs
│   │   ├── turn.rs
│   │   └── tool.rs
│   │
│   └── ui/
│       ├── mod.rs
│       ├── layout.rs
│       ├── header.rs
│       ├── transcript.rs
│       ├── user.rs
│       ├── assistant.rs
│       ├── reasoning.rs
│       ├── tool.rs
│       ├── status.rs
│       ├── composer.rs
│       ├── footer.rs
│       ├── selector.rs
│       ├── help.rs
│       └── error.rs
│
├── tests/
│   ├── protocol.rs
│   ├── rpc_io.rs
│   ├── app_flow.rs
│   ├── render_snapshots.rs
│   ├── agent_e2e.rs
│   └── terminal_restore.rs
│
└── docs/
    ├── architecture.md
    ├── keybindings.md
    └── rpc-contract.md
```

## 6.3 不增加 `components/` trait 系统

`ui/*` 是普通 render 函数：

```rust
pub(crate) fn render_user(
    frame: &mut Frame,
    area: Rect,
    block: &UserBlock,
    theme: &Theme,
);
```

状态较复杂时可以有具体 struct，但不要求统一 `Component` trait。

---

# 7. 进程模型

## 7.1 启动关系

```text
minicore-tui
└── child: minicore-agent --config <path> --stdio
```

TUI 是子进程 owner。

## 7.2 CLI

```bash
minicore-tui \
  --agent-bin minicore-agent \
  --agent-config ./agent.toml \
  --workspace .
```

支持：

```text
--agent-bin PATH
--agent-config PATH
--workspace PATH
--profile ID
--model ID
--reasoning auto|disabled|low|medium|high
--theme dark|light
--debug
--version
```

除：

```text
--agent-bin
--agent-config
```

其余均可选。

默认：

```text
agent-bin = minicore-agent from PATH
workspace = current_dir
theme = dark
```

不自动搜索多个 Agent 安装位置。

## 7.3 启动失败

以下直接进入恢复终端后的普通错误输出：

```text
Agent executable not found
Agent config missing
Child spawn failure
agent.ping failure
RPC protocol failure
```

不要在未完成终端初始化时进入复杂 Error Overlay。

## 7.4 退出

正常退出：

```text
发送 agent.shutdown
→ 等待 Response
→ 等待 Child exit
→ 关闭 RPC tasks
→ 恢复终端
```

若 5 秒内 Child 未退出：

```text
kill child
→ wait
→ 恢复终端
```

不自动重启 Agent。

---

# 8. Terminal 生命周期

## 8.1 Fullscreen 模式

使用：

```text
Crossterm alternate screen
Ratatui fullscreen viewport
应用自己管理 Transcript 滚动
```

进入顺序：

```text
enable_raw_mode
EnterAlternateScreen
EnableBracketedPaste
EnableMouseCapture
Hide cursor（初始化阶段）
创建 Terminal
clear
```

编辑器获得焦点后显示 hardware cursor。

退出顺序必须反向：

```text
Show cursor
DisableMouseCapture
DisableBracketedPaste
LeaveAlternateScreen
disable_raw_mode
```

## 8.2 `TerminalGuard`

```rust
pub struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    restored: bool,
}
```

实现：

```rust
impl TerminalGuard {
    pub fn enter() -> Result<Self, TerminalError>;
    pub fn terminal_mut(&mut self) -> &mut Terminal<...>;
    pub fn restore(&mut self) -> Result<(), TerminalError>;
}
```

`Drop`：

```text
仅 best-effort restore
不 panic
```

主流程仍显式调用 `restore()`。

## 8.3 Panic Hook

进入 TUI 前保存旧 panic hook。

安装新 hook：

```text
best-effort 恢复终端
调用旧 hook
```

不要吞掉 panic。

不要建立全局 crash recovery framework。

## 8.4 Hardware Cursor 与 IME

Composer 和搜索输入获得焦点时：

```text
frame.set_cursor_position(...)
```

必须使用真实终端 cursor。

原因：

- 中文；
- 日文；
- 韩文；
- IME 候选窗口；
- 终端可访问性。

不能只绘制反色 fake cursor。

## 8.5 Resize

接收：

```text
Crossterm Event::Resize
```

处理：

```text
清除布局缓存
清除 Markdown width cache
重新计算 Scroll viewport
标记 dirty
```

不重新加载 Transcript。

---

# 9. 核心事件循环

## 9.1 单状态写入者

只有：

```rust
App::update(...)
```

可以修改 `App` 和 UI state。

以下对象不得直接修改 App：

```text
RPC stdout task
RPC stderr task
Terminal input task
Child wait task
Tick task
```

它们只发送 `AppEvent`。

## 9.2 `AppEvent`

```rust
pub enum AppEvent {
    Terminal(crossterm::event::Event),

    RpcFrame(IncomingFrame),
    AgentLog(String),
    AgentExited(ExitStatus),

    Tick(Instant),
    ShutdownSignal,

    CommandCompleted {
        id: CommandId,
        result: CommandResult,
    },
}
```

如果所有 RPC result 都通过 `RpcFrame` 返回，不需要为普通 RPC 使用 `CommandCompleted`。

`CommandCompleted` 只用于：

```text
external editor
clipboard（未来）
本地文件日志（若有）
```

## 9.3 `AppCommand`

```rust
pub enum AppCommand {
    Rpc {
        request: OutgoingRequest,
        kind: RequestKind,
    },

    OpenExternalEditor {
        initial: String,
    },

    Quit,
}
```

v0.1 可以暂不实现 External Editor，但保留 enum 时不得出现未使用 dead code。

如果未实现，删除该 variant。

## 9.4 Update 结果

```rust
pub struct UpdateResult {
    pub commands: Vec<AppCommand>,
    pub dirty: bool,
}
```

也可以直接返回：

```rust
Vec<AppCommand>
```

由 App 自己维护 dirty。

不要建立通用 Effect trait。

## 9.5 Main Loop

```rust
loop {
    tokio::select! {
        event = terminal_events.recv() => { ... }
        frame = rpc_events.recv() => { ... }
        _ = tick.tick() => { ... }
        _ = signal.recv() => { ... }
    }

    let commands = app.update(event);
    execute_commands(commands, &mut rpc, &app_events).await?;

    if app.should_render(now) {
        terminal.draw(|frame| ui::render(frame, &mut app))?;
        app.mark_rendered(now);
    }

    if app.should_quit() {
        break;
    }
}
```

## 9.6 Render 节流

最大：

```text
30 FPS
```

配置：

```rust
const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(33);
const SPINNER_INTERVAL: Duration = Duration::from_millis(100);
```

RPC Delta 到达时：

```text
更新 state
标记 dirty
```

不立刻 draw。

Tick 只在：

```text
Busy spinner
临时通知超时
Double Ctrl+C 超时
```

时触发 dirty。

---

# 10. RPC 客户端

## 10.1 目标

TUI 不依赖 `minicore-agent` Rust crate。

在：

```text
src/protocol.rs
```

定义本地 wire DTO。

在：

```text
src/rpc.rs
```

实现进程与帧传输。

## 10.2 `RpcProcess`

```rust
pub struct RpcProcess {
    sender: mpsc::Sender<OutgoingRequest>,
    events: mpsc::Receiver<RpcEvent>,

    child: tokio::process::Child,
    tasks: Vec<JoinHandle<()>>,
}
```

可以将 Child ownership 放入单独 task，但必须有明确 owner。

## 10.3 RPC tasks

只允许：

```text
一个 stdin writer task
一个 stdout reader task
一个 stderr reader task
一个 child waiter（可与 RpcProcess::wait 合并）
```

不得为每个请求创建 stdout reader。

## 10.4 Request ID

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestId(u64);
```

从 1 开始单调增加。

不持久化。

## 10.5 Outgoing Request

```rust
pub struct OutgoingRequest {
    pub id: RequestId,
    pub method: &'static str,
    pub params: Value,
}
```

序列化：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "model.list",
  "params": {}
}
```

一行一个 frame。

## 10.6 Incoming Frame

先解析通用 envelope：

```rust
pub enum IncomingFrame {
    Response(RpcResponse),
    Notification(RpcNotification),
}
```

识别规则：

```text
存在 id
→ Response

method == "agent.event"
→ Notification

其他 notification
→ 记录 debug 并忽略
```

错误 JSON：

```text
RPC fatal protocol error
```

不要尝试扫描并恢复后续行。

## 10.7 Frame 大小

```rust
const MAX_RPC_FRAME_BYTES: usize = 8 * 1024 * 1024;
```

使用：

```text
read_until('\n')
```

在缓冲超过上限时终止连接。

不要使用无上限 `lines()`。

## 10.8 stderr

Agent stderr：

- 不写 TUI stdout；
- 转换为 `RpcEvent::AgentLogLine`；
- App 只保留最近 200 行；
- `/logs` 或 Help Overlay 可显示；
- 默认不落盘；
- Agent 退出时 Fatal Overlay 显示最后 20 行。

```rust
const MAX_AGENT_LOG_LINES: usize = 200;
const MAX_AGENT_LOG_LINE_BYTES: usize = 4096;
```

长行截断。

## 10.9 Pending Request

由 `App` 保存：

```rust
HashMap<RequestId, RequestKind>
```

不需要 oneshot map。

收到 Response：

```text
取出 RequestKind
→ 调用对应 update 方法
```

## 10.10 `RequestKind`

```rust
pub enum RequestKind {
    Ping,
    ListModels,
    ListProfiles,
    ListSessions,

    CreateSession,
    OpenSession(SessionId),
    CloseSession(SessionId),
    DeleteSession(SessionId),
    SessionState(SessionId),

    Transcript {
        session_id: SessionId,
        after: Option<ConversationSeq>,
    },

    SendTurn {
        session_id: SessionId,
        local_submission: LocalSubmissionId,
    },

    WaitTurn(TurnRef),
    CancelTurn(TurnRef),

    Shutdown,
}
```

不要用字符串区分内部响应。

## 10.11 `turn.send` 与 `turn.wait`

收到 `turn.send` 成功 Response 后必须立即发送：

```text
turn.wait
```

并保存：

```text
RequestKind::WaitTurn(TurnRef)
```

不得等待 `TurnFinished` Event 再注册。

## 10.12 Agent Child 退出

如果非正常退出：

```text
connection = Failed
composer disabled
显示 Fatal Overlay
显示 exit status 和最近 Agent log
允许 q 退出
```

不自动重启。

---

# 11. Protocol DTO

## 11.1 原则

本地 DTO 只覆盖 TUI 实际使用字段。

不要复制 Agent 所有内部类型。

所有 DTO：

```rust
#[derive(Clone, Debug, Deserialize)]
```

需要发出的 Params：

```rust
#[derive(Serialize)]
```

## 11.2 基础 DTO

至少定义：

```text
PingResult
ModelListResult
ProfileListResult
SessionListResult
CreateSessionResult
OpenSessionResult
SessionStateResult
TranscriptResult
TurnSendResult
TurnWaitResult
TurnCancelResult
RpcError
AgentEventEnvelope
```

## 11.3 Catalog

```rust
pub struct ModelInfo {
    pub id: String,
    pub model_ref: String,
    pub context_window: u64,
    pub supports_tools: bool,
    pub supported_reasoning: Vec<Reasoning>,
}
```

```rust
pub struct ProfileInfo {
    pub id: String,
    pub model: String,
    pub reasoning: Reasoning,
    pub tools: Vec<String>,
}
```

如果 Agent 响应仍包含 approval：

```text
Deserialize 时允许字段存在
TUI 不展示、不使用
```

不要在 DTO 上 `deny_unknown_fields`，因为 Agent 增加只读字段不应破坏 TUI。

## 11.4 Session

```rust
pub struct SessionInfo {
    pub session_id: String,
    pub title: Option<String>,
    pub profile: String,
    pub workspace: String,
    pub model: String,
    pub reasoning: Reasoning,
    pub loaded: bool,
    pub instance_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
```

字段名必须以 Agent 当前 `docs/rpc.md` 为准。

## 11.5 Reasoning

```rust
pub enum Reasoning {
    Auto,
    Disabled,
    Low,
    Medium,
    High,
}
```

Serde：

```text
snake_case
```

未知值：

```text
协议错误
```

本阶段不支持：

```text
minimal
xhigh
max
```

除非 Agent 实际公开。

## 11.6 Session State

```rust
pub enum SessionStatus {
    Idle,
    Running,
    WaitingForInput,
    Closing,
}
```

保留：

```text
session_id
instance_id
status
health
active_turn
pending_interaction
conversation_seq
last_terminal
```

TUI 可以忽略不展示字段，但需反序列化。

## 11.7 Agent Event

至少处理：

```text
session_opened
session_closed
session_state
turn_started
output_delta
tool_started
tool_progress
tool_finished
turn_finished
```

现有：

```text
interaction_requested
interaction_resolved
```

可解析为 `UnsupportedInteraction` Notice，不实现审批 UI。

如果进入 `WaitingForInput`：

```text
显示错误 Notice：
“This session is waiting for an interaction that this TUI version does not support.”
```

不自动 answer。

## 11.8 Transcript

Transcript entry DTO 应覆盖当前 Agent safe projection：

```text
user
assistant
tool_call
tool_result
summary
turn_terminal
```

必须保留：

```text
seq
turn_id
tool_call_id
model
reasoning
text/content
outcome
```

具体 JSON shape 以 Agent `docs/rpc.md` 和协议 fixture 为准。

---

# 12. App 状态

## 12.1 `App`

```rust
pub struct App {
    pub connection: ConnectionState,

    pub catalogs: CatalogState,
    pub sessions: SessionsState,

    pub dock: Dock,
    pub overlay: Option<Overlay>,

    pub composer: Composer,
    pub theme: ThemeKind,

    pub pending_requests: HashMap<RequestId, RequestKind>,
    pub notices: VecDeque<Notice>,
    pub agent_logs: VecDeque<String>,

    pub dirty: bool,
    pub last_render: Instant,
    pub should_quit: bool,
}
```

## 12.2 Connection

```rust
pub enum ConnectionState {
    Starting,
    Ready,
    ShuttingDown,
    Failed(String),
}
```

不实现 Reconnecting。

## 12.3 Catalog

```rust
pub struct CatalogState {
    pub models: Vec<ModelInfo>,
    pub profiles: Vec<ProfileInfo>,
    pub loaded: bool,

    pub next_profile: Option<String>,
    pub next_model: Option<String>,
    pub next_reasoning: Option<Reasoning>,
    pub default_workspace: PathBuf,
}
```

`next_*` 表示新 Session 选择。

不表示当前 Session 已修改。

## 12.4 Sessions

```rust
pub struct SessionsState {
    pub known: BTreeMap<SessionId, SessionView>,
    pub active: Option<SessionId>,
    pub list: Vec<SessionInfo>,
}
```

使用 `BTreeMap` 只为稳定调试，不依赖其 UI 排序。

Session Picker 自己按 updated_at 排序。

## 12.5 `SessionView`

```rust
pub struct SessionView {
    pub info: SessionInfo,
    pub state: Option<SessionState>,

    pub transcript: TranscriptState,
    pub live: Option<LiveTurn>,

    pub scroll: ScrollState,
    pub loading: bool,
    pub event_gap: bool,
}
```

## 12.6 `LiveTurn`

```rust
pub struct LiveTurn {
    pub reference: Option<TurnRef>,
    pub local_submission: LocalSubmissionId,
    pub user_text: String,

    pub text: String,
    pub reasoning: String,
    pub tools: Vec<LiveTool>,

    pub waiting: bool,
    pub cancel_requested: bool,
    pub event_gap: bool,
}
```

`reference` 在 `turn.send` Response 前可以为空。

若 `turn_started` Event 先到，可提前填入。

## 12.7 `LiveTool`

```rust
pub struct LiveTool {
    pub tool_call_id: String,
    pub name: String,
    pub status: ToolStatus,
    pub progress: Option<String>,
}
```

```rust
pub enum ToolStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
}
```

## 12.8 Transcript

```rust
pub struct TranscriptState {
    pub blocks: Vec<TranscriptBlock>,
    pub last_seq: Option<u64>,
    pub next_after: Option<u64>,
    pub complete: bool,

    pub render_cache: TranscriptRenderCache,
}
```

---

# 13. Durable Transcript 与 Live Turn

## 13.1 分离原则

Durable：

```text
session.transcript
```

Live：

```text
AgentEvent
```

二者不能混成一个不可区分 Vec。

## 13.2 发送时

用户提交后：

```text
清空 Composer
创建 LiveTurn.user_text
立即显示 User Card
发送 turn.send
```

若 `turn.send` 失败：

```text
删除 LiveTurn
恢复 Composer 文本
显示 Error Notice
```

## 13.3 Event 更新

```text
OutputDelta(Text)
→ append live.text

OutputDelta(Reasoning)
→ append live.reasoning

ToolStarted
→ create/update LiveTool

ToolProgress
→ update progress

ToolFinished
→ update status
```

## 13.4 Turn 完成

收到 `turn.wait` Response：

1. 保存 outcome；
2. 请求：

```text
session.transcript(after = transcript.last_seq)
```

3. 合并新 durable entries；
4. 删除 LiveTurn；
5. 清除 event_gap；
6. 根据 SessionState 启用 Composer。

不要仅用 Live Text 创建最终 Transcript Block。

## 13.5 Event Gap

任何 Event：

```text
dropped_before > 0
```

则：

```rust
session.event_gap = true;
live.event_gap = true;
```

UI 显示：

```text
“Live events were dropped; final output will be reconciled.”
```

Turn 完成后 transcript 对齐清除。

## 13.6 重复 Event

Tool Event 按：

```text
tool_call_id
```

幂等更新。

OutputDelta 不支持 deduplicate；假设 Agent 同一 Event 不重复。

不建立 Event sequence 去重系统。

## 13.7 Session 切换

后台 Session 的 Event 仍更新其 `SessionView`。

当前 Transcript 只渲染 active Session。

切回后台 Session：

- 若 live view 完整，直接显示；
- 若 event_gap 或 transcript 未加载，发起增量 Transcript 请求。

---

# 14. 屏幕布局

## 14.1 Fullscreen Pi Layout

```text
┌──────────────────────────────────────────────────────┐
│                                                      │
│  Transcript Scroll View                              │
│                                                      │
│  Startup header                                      │
│  User message card                                   │
│  Assistant markdown                                  │
│  Reasoning block                                     │
│  Tool cards                                          │
│                                                      │
├──────────────────────────────────────────────────────┤
│  Status / loader（仅 Busy 时）                       │
├──────────────────────────────────────────────────────┤
│  Composer 或 Selector                                │
├──────────────────────────────────────────────────────┤
│  workspace • session                                 │
│  status / event gap                  model • reasoning│
└──────────────────────────────────────────────────────┘
```

布局代码：

```rust
let [transcript, dock] = Layout::vertical([
    Constraint::Min(1),
    Constraint::Length(dock_height),
]).areas(frame.area());
```

Dock 内：

```rust
let [status, content, footer] = Layout::vertical([
    Constraint::Length(status_height),
    Constraint::Length(content_height),
    Constraint::Length(2),
]).areas(dock);
```

## 14.2 最小终端尺寸

推荐：

```text
minimum width  = 60
minimum height = 16
```

小于该尺寸：

```text
显示居中提示
不绘制复杂组件
仍允许 q / Ctrl+C 退出
```

## 14.3 Responsive

### 宽度 < 80

隐藏：

```text
Session ID
次要快捷键
完整路径
Tool output preview
```

Footer 只显示：

```text
status
model/reasoning
```

### 高度 < 24

- Composer 固定 3 行；
- Status 1 行；
- Footer 1 行；
- Selector 最大 8 行；
- Reasoning 默认折叠；
- Tool output 默认折叠。

### 宽度 >= 120

可以显示：

```text
完整 workspace
session title
model/reasoning
Tool preview
```

---

# 15. Pi 风格视觉规范

## 15.1 页面背景

Dark：

```text
pageBg = #18181e
cardBg = #1e1e24
```

如果终端不支持 TrueColor：

```text
使用最接近的 ANSI 256 色
```

不进行复杂颜色探测；Ratatui/Crossterm 直接输出 RGB。

## 15.2 User Message

视觉：

```text
背景 #343541
前景 #d4d4d4
水平 padding 1
垂直 padding 1
上下与相邻内容保持 1 行间隔
内容按 Markdown 渲染
```

不显示固定 “You” 标签。

## 15.3 Assistant Message

视觉：

```text
无背景
前景 #d4d4d4
水平 padding 1
顶部 1 行空白
Markdown
```

正在 Streaming 时：

- 不显示 Markdown 完整重排动画；
- 使用轻量 Markdown 或普通文本；
- 完成后使用完整 Markdown cache。

## 15.4 Reasoning

展开：

```text
颜色 #808080
Italic
水平 padding 1
与 Text block 保持自然顺序
```

折叠：

```text
Thinking...
```

使用同样灰色斜体。

如果 reasoning 为空但 Agent 正在等待模型：

```text
显示 Status Spinner
不伪造 Thinking 文本
```

## 15.5 Tool Card

Pending：

```text
background #282832
```

Success：

```text
background #283228
```

Error/Denied/Cancelled：

```text
background #3c2828
```

结构：

```text
<tool name>                       <status>
<safe call summary>
<collapsed result preview>
```

Padding：

```text
horizontal 1
vertical 1
```

Tool 卡片前有 1 行间隔。

## 15.6 Status

Busy 状态：

```text
⠋ Working…
⠙ Running read…
⠹ Running bash…
⠸ Cancelling…
```

颜色：

```text
accent #8abeb7
muted #808080
```

Spinner frames：

```text
⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏
```

100ms 更新。

## 15.7 Composer

边框：

```text
普通 border = reasoning level color
焦点 border = 同色 bright/bold
无 Session = #505050
Error = #cc6666
```

Reasoning colors：

```text
disabled = #505050
auto     = #8abeb7
low      = #5f87af
medium   = #81a2be
high     = #b294bb
```

不使用 Pi 的 xhigh/max，因为 Agent 当前未公开。

Placeholder：

```text
Type a message…
```

Running：

```text
Agent is working — Esc to cancel
```

无 Session：

```text
Create or open a session
```

## 15.8 Footer

两行。

第一行：

```text
左：缩短后的 workspace
右：Session title 或短 Session ID
```

第二行：

```text
左：Idle / Working / Tool / Cancelling / Event gap
右：model • reasoning
```

颜色：

```text
普通 dim #666666
当前 Model/Reasoning muted #808080
错误 #cc6666
warning #ffff00
```

当前 Agent 未提供可靠 Context/Cost 累计时：

```text
不显示假 token、百分比或价格
```

## 15.9 Selection

当前选中项：

```text
背景 #3a3a4a
前缀 →
文本 accent #8abeb7
```

当前生效项：

```text
✓ #b5bd68
```

空结果：

```text
No matching items
```

---

# 16. Theme

## 16.1 类型

```rust
pub enum ThemeKind {
    Dark,
    Light,
}
```

```rust
pub struct Theme {
    pub page_bg: Color,
    pub text: Color,
    pub muted: Color,
    pub dim: Color,

    pub accent: Color,
    pub border: Color,
    pub border_accent: Color,
    pub border_muted: Color,

    pub success: Color,
    pub warning: Color,
    pub error: Color,

    pub selected_bg: Color,
    pub user_message_bg: Color,

    pub tool_pending_bg: Color,
    pub tool_success_bg: Color,
    pub tool_error_bg: Color,

    pub md_heading: Color,
    pub md_link: Color,
    pub md_code: Color,
    pub md_code_block: Color,
    pub md_quote: Color,

    pub thinking_disabled: Color,
    pub thinking_auto: Color,
    pub thinking_low: Color,
    pub thinking_medium: Color,
    pub thinking_high: Color,
}
```

不实现动态 theme JSON。

## 16.2 Dark Palette

```text
cyan             #00d7ff
blue             #5f87ff
green            #b5bd68
red              #cc6666
yellow           #ffff00
text             #d4d4d4
gray             #808080
dimGray          #666666
darkGray         #505050
accent           #8abeb7
selectedBg       #3a3a4a
userMessageBg    #343541
toolPendingBg    #282832
toolSuccessBg    #283228
toolErrorBg      #3c2828
pageBg           #18181e
cardBg           #1e1e24

mdHeading        #f0c674
mdLink           #81a2be
mdLinkUrl        #666666
mdCode           #8abeb7
mdCodeBlock      #b5bd68
mdCodeBorder     #808080
mdQuote          #808080
mdListBullet     #8abeb7

thinkingDisabled #505050
thinkingAuto     #8abeb7
thinkingLow      #5f87af
thinkingMedium   #81a2be
thinkingHigh     #b294bb
```

## 16.3 Light Palette

提供内置 light：

```text
pageBg           #f8f8f8
cardBg           #ffffff
text             #1f2328
muted            #6c6c6c
dim              #767676
borderMuted      #b0b0b0
accent           #5a8080
border           #547da7
success          #588458
error            #aa5555
warning          #9a7326
selectedBg       #d0d0e0
userMessageBg    #e8e8e8
toolPendingBg    #e8e8f0
toolSuccessBg    #e8f0e8
toolErrorBg      #f0e8e8
```

Light 是低成本内置能力，不读取外部 theme 文件。

## 16.4 `/theme`

本地 Slash Command：

```text
/theme dark
/theme light
```

只改变 TUI。

不调用 Agent。

不持久化也可接受；若实现 `~/.config/minicore-tui/config.toml`，只保存 theme，不保存 Session/Agent 状态。

---

# 17. Startup Header

## 17.1 位置

作为 Transcript 第一块，而不是固定顶栏。

滚动后自然离开视口。

## 17.2 内容

```text
MINICORE

Coding agent TUI
Esc interrupt · Ctrl+L model · Ctrl+R sessions · /help
```

品牌：

- `MINICORE` 使用 accent/cyan；
- 不使用 Pi logo；
- 版本使用 dim。

## 17.3 状态

连接期间：

```text
Starting agent…
Loading models…
Loading sessions…
```

Ready 后：

```text
Open a session or type /new
```

已通过 CLI Workspace 自动创建/打开时直接进入 Composer。

---

# 18. Transcript 数据模型

## 18.1 `TranscriptBlock`

```rust
pub enum TranscriptBlock {
    Header(HeaderBlock),

    User(UserBlock),
    Assistant(AssistantBlock),
    Tool(ToolBlock),

    Summary(SummaryBlock),
    Terminal(TerminalBlock),
    Notice(NoticeBlock),
}
```

## 18.2 User

```rust
pub struct UserBlock {
    pub seq: Option<u64>,
    pub turn_id: Option<String>,
    pub text: String,
    pub pending: bool,
}
```

## 18.3 Assistant

```rust
pub struct AssistantBlock {
    pub seq: u64,
    pub turn_id: String,
    pub model: String,

    pub parts: Vec<AssistantPart>,
    pub terminal_error: Option<String>,
}
```

```rust
pub enum AssistantPart {
    Text(String),
    Reasoning(String),
}
```

ToolCall 单独渲染为 ToolBlock，不夹在 Assistant Markdown 内。

## 18.4 Tool

```rust
pub struct ToolBlock {
    pub tool_call_id: String,
    pub turn_id: String,
    pub name: String,

    pub arguments: Option<Value>,
    pub result: Option<String>,
    pub outcome: Option<String>,

    pub live_status: Option<ToolStatus>,
    pub progress: Option<String>,
    pub expanded: bool,
}
```

## 18.5 Summary

未来 Compaction 出现 Summary 时：

```text
显示折叠的 “Conversation compacted” 卡片
```

v0.1 不需要主动触发 Compaction。

## 18.6 Terminal

Completed terminal 默认不单独显示。

显示：

```text
Cancelled
Failed
Deadline exceeded
Durability failure
```

作为红/黄 Notice。

---

# 19. Transcript 渲染缓存

## 19.1 缓存单位

每个 Block 缓存：

```rust
pub struct BlockCache {
    pub width: u16,
    pub revision: u64,
    pub theme: ThemeKind,
    pub lines: Vec<Line<'static>>,
}
```

## 19.2 Revision

内容变化时：

```text
revision += 1
```

Streaming LiveTurn 可以每批 delta 增加 revision。

## 19.3 Width

Resize 时 width 变化，缓存失效。

## 19.4 渲染策略

第一版可以：

```text
缓存每个 Block
按当前顺序拼接全部 Line
根据 Scroll offset截取可见行
```

不建立 virtual DOM。

当 Transcript 超过 10,000 行后性能不足，再实现可见 Block 索引。

---

# 20. Markdown

## 20.1 Wrapper

```rust
pub struct MarkdownRenderer {
    theme: ThemeKind,
}
```

使用 `tui-markdown`，再将其 Style 映射到 `Theme`。

如果该 crate 无法满足需要，可用 `pulldown-cmark` 实现同一 wrapper；不要让其他 UI 模块依赖具体 Markdown crate。

## 20.2 必须支持

```text
段落
粗体
斜体
标题
无序列表
有序列表
行内代码
代码块
引用
链接
水平线
```

## 20.3 代码块

v0.1：

- 单色 `mdCodeBlock`；
- 边框 `mdCodeBlockBorder`；
- 保留缩进；
- 横向超长行截断或软换行；
- 不引入完整语法高亮。

未来再引入 Syntect。

## 20.4 Streaming

Streaming 时避免每个 delta 完整解析 Markdown。

策略：

```text
长度较短或帧间隔到达
→ 最多每 100ms 重新解析

或者
→ 正在Streaming时使用plain wrapped text
→ 完成后解析完整Markdown
```

推荐第二种，最简单。

Reasoning Streaming 同样使用普通灰色斜体文本。

---

# 21. Composer

## 21.1 Wrapper

```rust
pub struct Composer {
    textarea: tui_textarea::TextArea<'static>,
    history: VecDeque<String>,
    history_index: Option<usize>,
    draft: Option<String>,
}
```

其他代码不直接访问 `TextArea`。

## 21.2 高度

最小：

```text
3 行内容 + 2 行边框
```

最大：

```text
屏幕高度 40%
```

根据内容行数增长。

Running 时固定为 3 行，显示 placeholder，不接受编辑。

## 21.3 输入

必须支持：

```text
普通字符
Unicode
CJK
Emoji
Backspace/Delete
左右/上下
Home/End
Ctrl+A/E
Ctrl+W
多行
Bracketed Paste
Undo/Redo（由 tui-textarea）
```

## 21.4 提交

```text
Enter
→ 提交
```

换行：

```text
Shift+Enter
Ctrl+J
```

部分终端不报告 Shift+Enter，因此 Ctrl+J 是可靠 fallback。

空或纯空白输入：

```text
不提交
```

## 21.5 历史

进程内保存最近：

```text
100
```

条输入。

在 Composer 第一行按 Up：

```text
上一条历史
```

最后一行按 Down：

```text
下一条/恢复draft
```

不持久化输入历史。

## 21.6 Paste

Bracketed Paste：

- 整块插入；
- 不逐字符 render；
- 插入后只标记一次 dirty；
- 不自动提交；
- 不把 `\r\n` 错误拆成双换行。

## 21.7 Running

当前 Session Running：

```text
Composer disabled
Esc cancel
```

本阶段不在 Composer 中创建 follow-up queue。

---

# 22. Keybindings

## 22.1 Global

| Key | 行为 |
|---|---|
| `Ctrl+C` | Composer 有内容时清空；空时第一次提示，再按一次退出 |
| `Ctrl+D` | Composer 空且 Idle 时退出 |
| `F1` | Help |
| `Ctrl+R` | Session Selector |
| `Ctrl+L` | Model Selector，目标为创建新 Session |
| `Shift+Tab` | Reasoning Selector，目标为创建新 Session |
| `Ctrl+O` | 展开/折叠全部 Tool |
| `Ctrl+T` | 展开/折叠 Reasoning |
| `PageUp` | Transcript 上滚 |
| `PageDown` | Transcript 下滚 |
| `Home` | 跳到 Transcript 顶部（Composer无焦点或Ctrl+Home） |
| `End` | 回到底部 |
| `Esc` | 关闭 Selector；否则 Running时Cancel |
| `q` | Help/Fatal Overlay 中退出；普通Composer中作为字符 |

## 22.2 Composer

| Key | 行为 |
|---|---|
| `Enter` | Send |
| `Shift+Enter` | Newline |
| `Ctrl+J` | Newline |
| `Ctrl+A/E` | 行首/行尾 |
| `Ctrl+W` | 删除前一个单词 |
| `Alt+Up/Down` | 输入历史，可选 |
| `Ctrl+G` | External Editor，Phase 2 |

## 22.3 Selector

| Key | 行为 |
|---|---|
| `Up/Down` | 选择 |
| `Enter` | 确认 |
| `Esc` | 取消 |
| 普通输入 | 搜索 |
| `Ctrl+U` | 清空搜索 |
| `PageUp/PageDown` | 翻页 |

## 22.4 不动态配置

v0.1 Keymap 写在：

```text
src/keymap.rs
```

不实现配置文件映射。

---

# 23. Slash Commands

## 23.1 解析

仅当 Composer 第一非空字符为 `/` 时解析。

```rust
fn parse_command(input: &str) -> Option<LocalCommand>;
```

## 23.2 必须实现

```text
/new
/resume
/sessions
/model
/reasoning
/theme dark
/theme light
/clear
/help
/logs
/quit
```

## 23.3 语义

### `/new`

打开 New Session 流程。

### `/resume` / `/sessions`

打开 Session Selector。

### `/model`

打开 Model Selector。

选中后：

```text
更新 New Session Draft
打开/继续 New Session 创建
```

不修改当前 Session。

### `/reasoning`

同上。

### `/clear`

只清除当前屏幕本地视图，然后重新加载当前 Session Transcript。

不删除 Agent Session。

### `/logs`

显示最近 Agent stderr。

### `/quit`

正常 shutdown。

## 23.4 不实现

```text
!command
@file
/fork
/branch
/compact
/steer
/queue
/settings
/login
/plugin
/mcp
```

未知命令：

```text
显示本地 Notice
不发送给 Agent
```

---

# 24. Dock 与临时面板

## 24.1 `Dock`

```rust
pub enum Dock {
    Composer,
    NewSession(NewSessionState),
    SessionSelector(SessionSelectorState),
    ModelSelector(ModelSelectorState),
    ReasoningSelector(ReasoningSelectorState),
    Help,
    Logs,
}
```

Pi 风格要求：

> Selector 临时替换底部 Editor 区域，而不是覆盖整个 Transcript。

## 24.2 高度

Composer：

```text
5～屏幕40%
```

Selector：

```text
8～14 行
```

Help/Logs：

```text
最多屏幕60%
```

Fatal Error 使用 Overlay。

## 24.3 Dynamic Border

Selector 上下使用：

```text
─
```

或 Ratatui Block border。

边框颜色：

```text
accent
```

选中项使用 selectedBg。

---

# 25. New Session 流程

## 25.1 最小字段

```rust
pub struct NewSessionState {
    pub workspace: String,
    pub profile: String,
    pub model: String,
    pub reasoning: Reasoning,
    pub title: String,
    pub field: NewSessionField,
    pub submitting: bool,
    pub error: Option<String>,
}
```

## 25.2 默认值

```text
workspace = CLI --workspace 或 current_dir
profile = CLI --profile 或 Agent default/第一Profile
model = CLI --model 或 Profile.model
reasoning = CLI --reasoning 或 Profile.reasoning
```

## 25.3 交互

推荐：

```text
Tab / Shift+Tab
→ 字段切换

Enter on profile/model/reasoning
→ 打开对应Selector

Enter on Create
→ session.create
```

## 25.4 Workspace

Workspace 使用普通文本输入。

TUI 不自己读取目录或验证 path。

Agent 是权威校验者。

## 25.5 Create Response

成功：

```text
设置active session
请求Transcript
请求SessionState
Dock回Composer
```

失败：

```text
保留NewSessionState
显示error
不清空字段
```

---

# 26. Model Selector

## 26.1 Pi 风格

结构：

```text
────────────────────────
Select model
<search input>

→ deep
  fast

  (1/2)
────────────────────────
```

## 26.2 搜索

Model 数量通常较少。

v0.1 使用：

```text
case-insensitive substring
```

匹配：

```text
id
model_ref
```

不引入模糊匹配 crate。

## 26.3 列表

每项显示：

```text
model id
context window（紧凑格式）
tools ✓/—
supported reasoning
```

当前 Session Model 标记：

```text
✓ current
```

但选择结果用于新 Session。

顶部显示：

```text
Changing model creates a new session.
```

## 26.4 确认

选择 Model 后：

- 更新 NewSessionState.model；
- 如果当前 reasoning 不支持，保持值并显示提示，不自动降级；
- 打开 Reasoning Selector；
- 用户必须明确选择合法值。

---

# 27. Reasoning Selector

## 27.1 列表

仅展示当前 NewSessionState.model 支持的值。

描述：

```text
auto      Provider default
disabled  No reasoning
low       Light reasoning
medium    Moderate reasoning
high      Deep reasoning
```

## 27.2 Pi 风格颜色

每项使用对应 thinking color。

选中值显示：

```text
→ high   Deep reasoning
```

## 27.3 当前 Session

如果从主界面打开：

```text
顶部显示：
Current session: medium
New session setting:
```

不得让用户以为当前 Session 已变化。

---

# 28. Session Selector

## 28.1 数据

来自：

```text
session.list
```

## 28.2 默认排序

```text
updated_at descending
```

## 28.3 搜索

匹配：

```text
title
workspace
session_id
model
profile
```

case-insensitive substring。

## 28.4 行布局

宽屏：

```text
→ Task title          deep · high      5m
  ~/projects/repo
```

窄屏：

```text
→ Task title · 5m
  deep/high
```

## 28.5 状态

已加载：

```text
●
```

Running：

```text
◉
```

Idle：

```text
○
```

未加载：

```text
空格
```

## 28.6 选择

```text
session.open
→ set active
→ transcript分页
→ session.state
```

如果已 loaded，open 应幂等。

## 28.7 删除

v0.1 不在 Selector 中提供 Delete key。

可以通过未来 `/delete` 增加。

避免误删除。

---

# 29. Tool 渲染

## 29.1 Live Tool

Event 中可能没有完整 arguments/result。

Live 卡片最低显示：

```text
read                             running
```

Progress 存在时：

```text
<progress>
```

## 29.2 Durable Tool

Turn 完成后 Transcript 可能包含 ToolCall arguments 和 ToolResult。

TUI 支持五个已知 Tool 的简单 renderer。

### `read`

Collapsed：

```text
read src/lib.rs
```

Expanded：

```text
read src/lib.rs
<result preview>
```

### `write`

```text
write src/lib.rs
```

不默认显示完整 content。

### `edit`

```text
edit src/config.rs
```

不默认显示 old/new text。

### `apply_patch`

```text
patch src/session.rs
```

Expanded 可显示 patch/result，最多 30 行。

### `bash`

```text
$ cargo test
```

Expanded 显示：

```text
exit_code
stdout
stderr
```

最多 40 行，剩余：

```text
… N more lines
```

## 29.3 Unknown Tool

```text
<tool name>
<pretty JSON arguments when expanded>
<result preview>
```

## 29.4 Collapse

默认：

```text
Call header visible
Result collapsed
```

`Ctrl+O`：

```text
toggle all
```

可选：

```text
Enter on focused Tool toggle one
```

第一版没有 Transcript focus cursor时，只实现 toggle all。

---

# 30. Reasoning 展示

## 30.1 默认

默认：

```text
展开
```

配置：

```text
reasoning_visible = true
```

## 30.2 Toggle

`Ctrl+T`：

```text
所有 Reasoning block 隐藏/显示
```

隐藏时每个连续 reasoning run显示一次：

```text
Thinking...
```

## 30.3 Live

Live reasoning：

- 灰色；
- italic；
- 自动跟随；
- 不写入本地日志。

## 30.4 安全说明

Agent 已经把 Model reasoning作为可显示流发送。

TUI 不尝试显示 Provider opaque continuation。

RPC 中不存在的 encrypted data不得从 Agent stderr或其他来源获取。

---

# 31. Footer 与状态

## 31.1 Workspace 缩短

Home 下：

```text
/home/user/project
→ ~/project
```

Windows：

```text
C:\Users\name\project
→ ~\project
```

不 canonicalize。

只用于展示。

## 31.2 Session 标识

优先：

```text
title
```

没有 title：

```text
session_id 前8字符
```

## 31.3 状态

```text
Starting
Idle
Thinking
Streaming
Running <tool>
Cancelling
Waiting for unsupported interaction
Disconnected
```

## 31.4 Event Gap

Footer 左侧显示：

```text
⚠ live output incomplete
```

直到 Transcript reconcile 完成。

---

# 32. Scrolling

## 32.1 状态

```rust
pub struct ScrollState {
    pub offset: usize,
    pub follow_tail: bool,
    pub new_content: bool,
}
```

`offset` 代表距顶部行偏移。

## 32.2 Follow

用户位于底部：

```text
新内容自动跟随
```

用户向上滚动：

```text
follow_tail=false
new_content=true
```

Footer或Transcript底部显示：

```text
↓ new output
```

End：

```text
follow_tail=true
滚到底部
```

## 32.3 Mouse

Mouse wheel：

```text
3 行/步
```

不实现点击选择和拖动滚动条。

## 32.4 Overscroll

顶部和底部 clamp。

不实现惯性滚动。

---

# 33. 错误与 Notice

## 33.1 Notice

```rust
pub struct Notice {
    pub level: NoticeLevel,
    pub text: String,
    pub created_at: Instant,
    pub sticky: bool,
}
```

短错误在 Transcript 或 Status 上方显示。

## 33.2 Fatal Overlay

用于：

```text
Agent child exited
RPC frame malformed
stdout closed unexpectedly
Terminal draw repeatedly failed
```

显示：

```text
错误摘要
Agent exit code
最近20行Agent stderr
q退出
```

## 33.3 RPC Error

展示 Agent 返回的：

```text
message
data.kind
retryable
```

不展示完整 JSON Debug。

## 33.4 No Retry

TUI 不自动重试：

```text
session.create
session.open
turn.send
```

用户可重新操作。

Catalog 初始加载失败：

```text
显示Fatal
```

---

# 34. App 启动流程

## 34.1 Bootstrap

```text
解析args
初始化TUI自己的stderr tracing到文件或禁用
启动Agent child
进入Terminal
agent.ping
model.list
profile.list
session.list
```

RPC 请求可以并发发送。

## 34.2 完成

全部成功：

```text
connection=Ready
```

如果 CLI 指定：

```text
--workspace
```

且无现成 active Session：

```text
打开New Session面板
预填字段
```

不要自动创建，除非增加显式：

```text
--new
```

v0.1 可不实现 `--new`。

## 34.3 无 Session

展示 Header 和：

```text
No session open.
Use /new or /resume.
```

Composer disabled。

---

# 35. Session 打开与 Transcript 分页

## 35.1 Open

```text
session.open
→ SessionInfo
→ session.state
→ session.transcript(after=null, limit=100)
```

## 35.2 分页

继续：

```text
while !complete:
  session.transcript(after=next_after, limit=100)
```

为了不冻结 UI：

- 一次只发一页；
- 收到一页后渲染；
- 再发下一页。

## 35.3 大历史

加载期间：

```text
Composer disabled
Status: Loading history…
```

用户可以 Esc：

```text
停止继续请求后续页
保留已加载内容
```

RPC 已发出的当前页不取消。

## 35.4 增量

Turn 完成：

```text
after=last_seq
```

直到 complete。

---

# 36. 多 Session

## 36.1 后台运行

TUI 可以切换到另一个 Session，而原 Session继续运行。

Events 仍按 session_id更新对应 state。

## 36.2 状态提示

Session Selector显示后台 Running。

## 36.3 同时 Live

每个 SessionView最多一个 LiveTurn。

符合 Agent/MiniCore语义。

## 36.4 退出

`agent.shutdown`关闭所有 loaded Session。

TUI不逐一close。

---

# 37. Agent 功能差距的诚实处理

## 37.1 Model/Reasoning切换

当前 Session不可变。

选择器作用于New Session。

## 37.2 Steering

不实现。

Running时Enter：

```text
不提交
Composer disabled
```

未来可增加本地 Follow-up Queue，但不得称为Steer。

## 37.3 Bash Live Output

不实现。

Tool结束后从Transcript读取结果。

## 37.4 Approval

本阶段不实现。

如果Session进入WaitingForInput：

```text
显示Unsupported Interaction
建议用户使用approval=auto Profile
```

## 37.5 Compaction

不实现UI。

Summary Entry出现时只负责显示。

---

# 38. TUI 配置

## 38.1 可选配置文件

```text
~/.config/minicore-tui/config.toml
```

v0.1只支持：

```toml
theme = "dark"
show_reasoning = true
tool_expanded = false
render_fps = 30
```

如果文件不存在使用默认值。

配置解析失败：

```text
启动失败并输出行号
```

## 38.2 不保存

```text
active Session
Composer draft
scroll
Agent path
credentials
Transcript
```

Agent path/config由CLI提供。

---

# 39. External Editor（第二阶段）

为了接近 Pi，v0.1完成主闭环后可实现：

```text
Ctrl+G
```

流程：

1. 创建Temp file；
2. 写入Composer内容；
3. restore terminal；
4. 运行 `$VISUAL` 或 `$EDITOR`；
5. 重新enter terminal；
6. 读取内容；
7. 更新Composer；
8. 删除Temp file。

如果没有变量：

```text
显示Notice
```

此功能不是v0.1阻塞验收。

不要在第一次提交中实现。

---

# 40. Copy Last Response（第二阶段）

Pi常用操作：

```text
Ctrl+X
```

可以后续通过：

```text
OSC 52
```

复制最后一个Assistant Text。

v0.1不引入Clipboard crate。

如果实现：

- 仅复制Assistant可见Text；
- 不复制Reasoning；
- 不复制ToolResult；
- 失败显示Notice。

非阻塞。

---

# 41. 性能

## 41.1 Render

目标：

```text
idle CPU接近0
busy最多30fps
spinner 10fps
```

## 41.2 RPC Delta批处理

连续Delta：

```text
每条更新state
每33ms最多render一次
```

不要在RPC task合并事件；合并属于App渲染节流。

## 41.3 Markdown

完成消息缓存。

Streaming普通文本。

## 41.4 Transcript

每个Block缓存Lines。

Resize只重新渲染可见/缓存失效Block。

第一版可先重建全部缓存，再根据profile优化。

## 41.5 Tool Output

Expanded preview限制：

```text
最大40行
最大32KiB用于单次UI渲染
```

完整结果仍在Agent Store/Transcript DTO中。

TUI不需要永久保留超过显示上限的第二份String；可保留原DTO或按需重新加载Transcript。

---

# 42. 安全与隐私

## 42.1 stdout

TUI自己的stdout属于终端绘制。

Agent子进程stdout只由RPC reader读取，不直接转发。

## 42.2 stderr

Agent stderr只放入内存Log panel。

不直接打印到alternate screen。

## 42.3 不记录

TUI tracing不得记录：

```text
User Message
Assistant Text
Reasoning
Tool arguments
Tool output
Workspace file内容
API key
Agent raw frame
```

可记录：

```text
RPC method
Request ID
Session ID
Turn ID
error kind
frame bytes
render duration
```

## 42.4 Bash

TUI必须在Help或Footer首次提示：

```text
Tools run automatically.
Bash is not sandboxed.
```

不自行执行Shell。

---

# 43. 测试策略

## 43.1 默认离线

```bash
cargo test --locked --all-targets
```

不得：

- 访问OpenAI；
- 要求minicore-agent已安装；
- 修改真实用户配置；
- 进入真实terminal alternate screen。

## 43.2 RPC Codec

使用：

```text
tokio::io::duplex
```

测试：

```text
response/event交错
乱序response
partial line
multiple lines
8MiB bound
invalid JSON
stdout EOF
stderr line truncation
```

## 43.3 Protocol Fixtures

在：

```text
tests/fixtures/protocol/
```

保存脱敏JSON：

```text
model-list.json
profile-list.json
session-create.json
session-list.json
session-state.json
transcript-page.json
output-delta.json
tool-started.json
tool-finished.json
turn-wait.json
rpc-error.json
```

保证与Agent `docs/rpc.md`一致。

不复制API key或真实workspace。

## 43.4 App Update

测试：

```text
bootstrap
create session
event before send response
text delta
reasoning delta
tool lifecycle
turn wait
transcript reconcile
event gap
cancel
session switch
child exit
```

## 43.5 Render Snapshot

使用：

```rust
ratatui::backend::TestBackend
insta
```

尺寸：

```text
60x16
80x24
120x40
160x50
```

主题：

```text
dark
light
```

快照场景：

```text
startup
empty
user message
assistant markdown
expanded reasoning
hidden reasoning
pending tool
success tool
error tool
running
session selector
model selector
reasoning selector
new session
help
fatal
event gap
small terminal
CJK
```

## 43.6 CJK/Unicode

测试：

```text
中文输入
日文输入
Emoji
组合字符
全角字符
CJK换行
光标列计算
UTF-8截断
```

使用 `unicode-width`，不能用 `String::len()`计算终端宽度。

## 43.7 Composer

测试：

```text
Enter submit
Ctrl+J newline
Shift+Enter newline
paste
history
clear
double Ctrl+C
Running disabled
```

## 43.8 Scroll

测试：

```text
follow tail
user scroll disables follow
new output marker
End resumes follow
resize clamp
mouse wheel
```

## 43.9 Terminal Restore

对TerminalGuard的命令顺序使用mock writer测试。

真正PTY测试可只在Unix `#[ignore]`。

## 43.10 Agent E2E

新增：

```text
tests/agent_e2e.rs
```

默认 ignored。

环境：

```text
MINICORE_AGENT_BIN
MINICORE_AGENT_CONFIG
```

测试：

```text
spawn真实Agent
ping
model.list
profile.list
session.create
turn.send
turn.wait
transcript
shutdown
```

不要求真实OpenAI；Agent config可以指向loopback mock。

---

# 44. CI

## 44.1 Quality

```text
Ubuntu stable
cargo fmt
cargo clippy -D warnings
cargo doc -D warnings
```

## 44.2 Platforms

```text
Ubuntu
macOS
Windows
```

运行：

```bash
cargo test --locked --all-targets
```

## 44.3 MSRV

```text
Rust 1.85.0 on Ubuntu
```

## 44.4 Cargo Tree

CI增加：

```bash
cargo tree -p crossterm
```

并人工/脚本确认不同时出现0.28和0.29。

最小脚本可以：

```bash
cargo tree -p crossterm | grep '^crossterm v'
```

要求只有一行。

---

# 45. 文件级实施方案

## 45.1 `src/main.rs`

只负责：

```text
args
tracing
spawn RpcProcess
TerminalGuard
run app
shutdown
```

不放App逻辑。

## 45.2 `src/terminal.rs`

```text
TerminalGuard
enter/restore
panic hook support
```

## 45.3 `src/rpc.rs`

```text
child spawn
stdin writer
stdout reader
stderr reader
frame bounds
shutdown
```

## 45.4 `src/protocol.rs`

```text
wire DTO
Request builder
Incoming frame parser
AgentEvent parser
```

## 45.5 `src/app.rs`

```text
App
update
request tracking
session activation
turn lifecycle
transcript reconcile
```

## 45.6 `src/event.rs`

```text
AppEvent
RpcEvent
terminal event task
```

## 45.7 `src/command.rs`

```text
AppCommand
LocalCommand
Slash parser
```

不要建立handler registry。

## 45.8 `src/theme.rs`

```text
Theme
dark/light
reasoning colors
```

## 45.9 `src/markdown.rs`

```text
MarkdownRenderer
plain streaming renderer
cache-friendly output
```

## 45.10 `src/state/*`

只保存纯数据和小型helper。

不做IO。

## 45.11 `src/ui/*`

只渲染和计算光标。

不发RPC。

不修改App业务状态，除Composer内部可由App提前更新。

---

# 46. 开发阶段

## Phase 0：Scaffold

提交：

```text
feat: scaffold minicore-tui
```

完成：

```text
Cargo
MSRV
TerminalGuard
Theme
空Fullscreen
CI
```

## Phase 1：RPC

提交：

```text
feat(rpc): manage the minicore-agent stdio process
```

完成：

```text
child lifecycle
frame parser
request IDs
event/response交错
stderr buffer
ping
catalog calls
```

## Phase 2：App 状态

提交：

```text
feat(app): model sessions turns and transcript reconciliation
```

完成：

```text
App update
SessionView
LiveTurn
Transcript
event gap
turn.wait
```

## Phase 3：Pi Visual Core

提交：

```text
feat(ui): implement the pi-style fullscreen conversation layout
```

完成：

```text
Header
User card
Assistant
Reasoning
Tool card
Status
Composer
Footer
Dark/Light theme
```

## Phase 4：Selectors

提交：

```text
feat(ui): add session model and reasoning selectors
```

完成：

```text
New Session
Session selector
Model selector
Reasoning selector
```

## Phase 5：Input 与 Commands

提交：

```text
feat(input): add multiline editing shortcuts and local commands
```

完成：

```text
Composer
history
paste
slash commands
help/logs
scroll
```

## Phase 6：完整 Agent Flow

提交：

```text
test: cover multi-turn rpc and durable transcript reconciliation
```

完成：

```text
protocol fixtures
App flow
render snapshots
ignored Agent E2E
```

## Phase 7：Polish

提交：

```text
docs: document minicore-tui usage and backend limits
```

可选：

```text
External Editor
OSC52 Copy
```

它们不阻塞v0.1。

---

# 47. 验收矩阵

## 47.1 架构

| ID | 验收 |
|---|---|
| MT-001 | 项目名为minicore-tui |
| MT-002 | 一个Cargo package |
| MT-003 | TUI不依赖minicore-agent Rust crate |
| MT-004 | TUI不依赖minicore-runtime |
| MT-005 | TUI只通过stdio JSON-RPC调用Agent |
| MT-006 | 不新增HTTP/WebSocket |
| MT-007 | 不实现Agent Loop |
| MT-008 | 不实现Approval |
| MT-009 | 不实现Steering |
| MT-010 | 不实现Plugin系统 |

## 47.2 技术

| ID | 验收 |
|---|---|
| MT-011 | Rust 1.85通过 |
| MT-012 | Edition 2024 |
| MT-013 | unsafe_code=forbid |
| MT-014 | Ratatui 0.29 |
| MT-015 | Crossterm 0.28 |
| MT-016 | 只有一个Crossterm版本 |
| MT-017 | tui-textarea 0.7 |
| MT-018 | Linux/macOS/Windows测试通过 |

## 47.3 Terminal

| ID | 验收 |
|---|---|
| MT-019 | 使用alternate screen |
| MT-020 | raw mode正确进入 |
| MT-021 | bracketed paste开启 |
| MT-022 | 正常退出恢复 |
| MT-023 | RPC失败恢复 |
| MT-024 | panic best-effort恢复 |
| MT-025 | hardware cursor位于Composer |
| MT-026 | Resize可重绘 |
| MT-027 | 小终端有安全提示 |

## 47.4 RPC

| ID | 验收 |
|---|---|
| MT-028 | TUI启动Agent child |
| MT-029 | stdin只有一个writer |
| MT-030 | stdout只有一个reader |
| MT-031 | event与response可交错 |
| MT-032 | response可乱序 |
| MT-033 | request按ID关联 |
| MT-034 | frame有8MiB上限 |
| MT-035 | stderr不污染Terminal |
| MT-036 | Agent非正常退出显示Fatal |
| MT-037 | 正常退出调用agent.shutdown |
| MT-038 | 超时后可kill Child |

## 47.5 Startup

| ID | 验收 |
|---|---|
| MT-039 | agent.ping成功 |
| MT-040 | model.list加载 |
| MT-041 | profile.list加载 |
| MT-042 | session.list加载 |
| MT-043 | 启动失败有普通错误 |
| MT-044 | 无Session显示/new和/resume提示 |

## 47.6 Session

| ID | 验收 |
|---|---|
| MT-045 | New Session支持workspace |
| MT-046 | 支持Profile |
| MT-047 | 支持Model |
| MT-048 | 支持Reasoning |
| MT-049 | 无效组合显示Agent错误 |
| MT-050 | SessionInfo成为UI事实 |
| MT-051 | Session打开加载state |
| MT-052 | Transcript分页 |
| MT-053 | Session切换 |
| MT-054 | 后台Session Event仍更新 |
| MT-055 | Model切换创建新Session |
| MT-056 | 不显示伪热切换 |

## 47.7 Turn

| ID | 验收 |
|---|---|
| MT-057 | Enter发送非空消息 |
| MT-058 | turn.send后立即turn.wait |
| MT-059 | send失败恢复Composer |
| MT-060 | TurnStarted可先于send response |
| MT-061 | TextDelta实时显示 |
| MT-062 | ReasoningDelta实时显示 |
| MT-063 | ToolStarted实时显示 |
| MT-064 | ToolProgress实时显示 |
| MT-065 | ToolFinished实时显示 |
| MT-066 | Esc取消exact Turn |
| MT-067 | Cancel后等待outcome |
| MT-068 | Wait完成后增量Transcript对齐 |
| MT-069 | 同Session多轮 |
| MT-070 | 两Session并行 |

## 47.8 Event可靠性

| ID | 验收 |
|---|---|
| MT-071 | AgentEvent不作为权威历史 |
| MT-072 | dropped_before标记event gap |
| MT-073 | event gap显示warning |
| MT-074 | Turn完成后Transcript清除gap |
| MT-075 | TurnFinished丢失不影响完成 |
| MT-076 | OutputDelta和Wait response交错可处理 |
| MT-077 | 不增加Event ACK/replay |

## 47.9 Pi视觉

| ID | 验收 |
|---|---|
| MT-078 | Fullscreen Transcript+Dock布局 |
| MT-079 | User Message为背景卡片 |
| MT-080 | Assistant无背景Markdown |
| MT-081 | Reasoning灰色斜体 |
| MT-082 | Reasoning可折叠 |
| MT-083 | Tool Pending背景正确 |
| MT-084 | Tool Success背景正确 |
| MT-085 | Tool Error背景正确 |
| MT-086 | Editor边框随Reasoning变色 |
| MT-087 | Busy Status有Spinner |
| MT-088 | Footer双行 |
| MT-089 | Selector使用accent和selectedBg |
| MT-090 | Dark palette与规范一致 |
| MT-091 | Light palette可用 |
| MT-092 | 不使用Pi Logo和品牌 |

## 47.10 Composer

| ID | 验收 |
|---|---|
| MT-093 | 多行输入 |
| MT-094 | Enter提交 |
| MT-095 | Ctrl+J换行 |
| MT-096 | Shift+Enter换行（终端支持时） |
| MT-097 | Bracketed Paste |
| MT-098 | CJK宽度正确 |
| MT-099 | Emoji不破坏光标 |
| MT-100 | 输入历史 |
| MT-101 | Running时禁用 |
| MT-102 | Ctrl+C清空/双击退出 |

## 47.11 Selection与Command

| ID | 验收 |
|---|---|
| MT-103 | Session Selector搜索 |
| MT-104 | Model Selector搜索 |
| MT-105 | Reasoning Selector只显示可用值 |
| MT-106 | Selector替换Dock主内容 |
| MT-107 | /new |
| MT-108 | /resume |
| MT-109 | /model |
| MT-110 | /reasoning |
| MT-111 | /theme |
| MT-112 | /clear |
| MT-113 | /help |
| MT-114 | /logs |
| MT-115 | /quit |
| MT-116 | 未知命令不发送Agent |

## 47.12 Scrolling与性能

| ID | 验收 |
|---|---|
| MT-117 | Follow tail |
| MT-118 | 用户上滚停止follow |
| MT-119 | New output标记 |
| MT-120 | End恢复follow |
| MT-121 | Mouse wheel |
| MT-122 | Busy render最多30fps |
| MT-123 | Idle不持续重绘 |
| MT-124 | Markdown完成消息缓存 |
| MT-125 | Streaming不每delta全量Markdown |
| MT-126 | Tool preview有界 |

## 47.13 安全

| ID | 验收 |
|---|---|
| MT-127 | TUI不执行Shell |
| MT-128 | Agent stdout不直接打印 |
| MT-129 | Agent stderr有界保存 |
| MT-130 | TUI日志不含消息/Reasoning/Tool内容 |
| MT-131 | Help说明Tool自动执行 |
| MT-132 | Help说明Bash非Sandbox |
| MT-133 | Fatal Overlay不输出raw RPC frame |

## 47.14 测试

| ID | 验收 |
|---|---|
| MT-134 | RPC duplex tests通过 |
| MT-135 | Protocol fixtures通过 |
| MT-136 | App flow tests通过 |
| MT-137 | 60x16 snapshot |
| MT-138 | 80x24 snapshot |
| MT-139 | 120x40 snapshot |
| MT-140 | Dark snapshots |
| MT-141 | Light snapshots |
| MT-142 | CJK snapshots |
| MT-143 | Agent E2E测试存在且ignored |
| MT-144 | 所有默认测试离线 |

---

# 48. 测试命令

每个提交：

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
cargo tree -p crossterm
```

Snapshot：

```bash
cargo insta test
cargo insta review
```

E2E：

```bash
MINICORE_AGENT_BIN=/path/to/minicore-agent \
MINICORE_AGENT_CONFIG=/path/to/agent.toml \
cargo test --locked agent_e2e -- --ignored --nocapture
```

---

# 49. README 必须说明

```text
minicore-tui是minicore-agent的独立Rust前端
完全通过stdio JSON-RPC通信
使用Pi fullscreen模式的视觉与交互风格
不是Pi的fork，也不使用Pi品牌资源
一个Session的Model/Reasoning创建后不可变
切换Model/Reasoning会创建新Session
AgentEvent可能丢失
turn.wait与Transcript是权威结果
Tool当前自动执行
Bash不是Sandbox
无Approval UI
无Steering
无Compaction
无实时Bash输出
同一Agent data_dir不能被多个Agent进程同时使用
```

提供：

```text
安装
Agent配置
启动命令
快捷键
Slash commands
常见错误
MSRV
平台支持
```

---

# 50. 完成定义

`minicore-tui v0.1` 只有在以下条件全部满足时才算完成：

```text
可以启动并拥有minicore-agent子进程
可以通过RPC发现Models/Profiles/Sessions
可以选择Workspace/Profile/Model/Reasoning创建Session
可以打开已有Session并分页加载历史
可以流式显示Text与Reasoning
可以显示Tool生命周期与最终结果
可以取消Turn
可以可靠等待Turn
可以用Transcript对齐Event丢失
可以在同一Session完成多轮
可以在多个Session之间切换
Pi风格Fullscreen界面完成
Dark/Light主题完成
多行Composer完成
Session/Model/Reasoning Selector完成
Slash commands完成
终端在所有主要退出路径恢复
离线测试与快照通过
Linux/macOS/Windows CI通过
Rust 1.85通过
不依赖minicore-agent/minicore-runtime Rust crate
不修改Agent或MiniCore来迁就UI
不实现Approval、Steering、Compaction、Plugin、MCP或Subagent
MT-001～MT-144全部通过
```

最终架构保持：

```text
minicore-tui
    只负责终端交互与RPC客户端

minicore-agent
    负责Session、Model、Tool、Workspace、Store

minicore-runtime
    负责单Session Agent执行语义
```

---

# 51. 给代码 Agent 的最终执行要求

请创建并开发 `minicore-tui`。

要求：

1. 开发前固定 `minicore-agent` 当前 dev HEAD 和 RPC 文档版本；
2. 创建一个 Rust 1.85、Edition 2024 的单 package；
3. 使用 Ratatui 0.29、Crossterm 0.28、tui-textarea 0.7；
4. TUI 与 Agent 只能通过 stdio JSON-RPC 通信；
5. UI 以 Pi fullscreen interactive mode 为视觉参考；
6. 使用本文给出的 Pi 风格消息、Tool、Reasoning、Editor、Footer 和 Palette；
7. 不复制 Pi 源码、Logo 或品牌；
8. 所有 App 状态只能由 `App::update` 修改；
9. RPC必须支持Response/Event交错和乱序Response；
10. `turn.send`成功后立即注册`turn.wait`；
11. AgentEvent只用于实时显示，最终必须通过Transcript对齐；
12. 不实现当前Session热切Model/Reasoning；
13. Model/Reasoning选择用于创建新Session；
14. 不实现审批UI，遇到WaitingForInput显示Unsupported；
15. 不实现Steering、Follow-up Queue、Compaction、MCP、Plugin、Subagent；
16. 不让TUI直接执行Shell或读取Workspace；
17. 不增加通用TUI框架、Component Trait树或协议生成器；
18. 按Phase 0～7独立提交；
19. 每个阶段保持fmt/test/clippy/doc通过；
20. 最终输出：
    - 起始与最终HEAD；
    - 提交列表；
    - 文件清单；
    - RPC兼容版本；
    - 快捷键；
    - Slash commands；
    - UI截图或TestBackend快照；
    - MT-001～MT-144结果；
    - CI结果；
    - Agent E2E结果或未执行说明；
    - 已知限制。
