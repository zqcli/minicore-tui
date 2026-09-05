# MiniCore TUI v0.2：适配 Agent v0.3 / Runtime v0.4 的全量迁移与重构实施规格（修订 r2）

> 文档修订：r2 · 2026-09-05（Asia/Tokyo）  
> Agent 基线：`b2e23938d073ab21c2775faa623561ba929a5ed1`（0.3.0）  
> Runtime 基线：`87f3cf92b9b5980b0f468174a319cf53427d858e`（0.4.0，保持不变）  
> TUI 目标版本仍为 0.2.0；r2 是规格修订号，不是新的产品版本。

## 本次修订摘要

这是上一版迁移 Spec 的**完整替换版**，不是需要另行拼接的补丁。
沿用“协议与执行状态重构、Pi 风格视觉组件保留”的方案；不新增审批、插件、自动重试或双协议兼容层。

相对旧 Agent 基线 `edd1cb670dc72f61cb94f44bfdff8ca38b5a4999`，本次核对了 5 个新增提交、9 个变更文件。
RPC 方法、请求/响应字段、错误码、Agent 版本和 Runtime 依赖 revision 没有变更；变化集中在完成结果保留、存储校验、测试与语义说明。[S1]

本次必须落实的修订：

| 主题 | r2 要求 |
|---|---|
| 阻塞后完成结果 | `session_blocked` 只拒绝新操作，不清除旧 TurnRef、wait 请求或既有结果；同一 loaded Session 中保留的阻塞轮次仍可重复 wait。 |
| 持久化措辞 | `persisted` 仅确认当前进程中的追加操作成功；不能宣传为事务提交、fsync 或端到端崩溃持久性保证。 |
| 失败后重开 | 追加失败时内存不合并；磁盘可能没有、部分写入或已有完整行。重开后以 Agent 返回的 History 为准，不保证该轮一定消失或一定恢复。 |
| 关闭语义 | 使用显式 `agent.shutdown` 并持续排空响应；`close/shutdown` 的当前取消路径可能返回 `reason=user`。清理完成不等于全部轮次保存成功。 |
| Store 校验 | 无效 session record 会被列表跳过，显式 open 严格失败。TUI 不补默认值、不自行删工具或修写 Store。 |
| 同 Loop 换模 | 增加 Model A → Tool → Model B 的确定性验收；两个 request 保持同一 `loop_id`，不能误做新 Session/新 Loop。 |

同时纠正上一版三处表达歧义：后续 `request_started` 不是某条 Steer 的逐条应用回执；历史分页使用已加载的连续 item 数推进 cursor；通用 `store_error` 不能直接诊断成“旧格式”。这些是 TUI 解释规则的澄清，不是新增后端能力。

保留原 MIG-001～MIG-140 编号，并增加 MIG-141～MIG-160。验收项是待实施要求，不代表本次已经执行 TUI 测试。


# 0. 文档定位

以下各节保留完整的 v0.1→v0.2 迁移目标。r2 本身只是针对第1.4节五个Agent提交的增量修订；已完成上一版迁移的实现按第64.1节处理，不重复推翻已正确的代码。

本文档用于迁移或重构现有 `minicore-tui` 设计，使其能够正确配合最新的：

```text
minicore-agent  v0.3
minicore-runtime v0.4
```

运行。

本规格替代旧文档：

```text
MiniCore TUI v0.1：Pi 风格完整 Rust TUI 开发实施规格
```

旧文档不再作为 RPC、Session、Turn、History、Model 切换和 Steering 语义的实现依据。

旧文档仍可作为以下部分的视觉参考：

```text
Pi 风格 fullscreen layout
TerminalGuard
Theme
Composer
Markdown
Scrolling
Selector
渲染性能
测试快照
```

本次变更不是普通字段迁移。

最新 Runtime 和 Agent 已发生根本性职责重置：

```text
Runtime v0.3:
    SessionRuntime
    Core 持久化 Conversation
    一个 Runtime 对应一个 loaded Session

Runtime v0.4:
    AgentLoop
    一次性、不可恢复的 live loop
    不拥有 Session
    不持久化 History
    Host 负责完整 Session 和 Store

Agent v0.2:
    适配 Runtime SessionRuntime
    session.transcript
    model/reasoning 创建后不可变
    无真正 steering

Agent v0.3:
    自己拥有 Session、History 和持久化
    每条用户消息启动一个 AgentLoop
    session.history
    session.update
    turn.steer
    persistence failed → Session Blocked
```

因此推荐方案是：

> **对 TUI 的协议层、领域状态层和 Turn 控制层进行全量重构；保留与后端无关的 Pi 风格终端和渲染组件。**

不推荐：

```text
在旧 Transcript DTO 上增加几个兼容字段
同时支持 Agent v0.2 和 v0.3
用适配器把新 History 伪装成旧 Transcript
继续保留 instance_id
继续假设一个 Turn 只有一个 Model Request
继续把 Model/Reasoning 当成不可变 Session 设置
继续把 Running Composer 完全禁用
```

---

# 1. 固定开发基线

## 1.1 最新 Agent

```text
repository:
https://github.com/zqcli/minicore-agent

branch:
dev

reviewed HEAD:
b2e23938d073ab21c2775faa623561ba929a5ed1

crate version:
0.3.0

rust-version:
1.85
```

该 Agent 固定依赖：

```text
minicore-runtime:
87f3cf92b9b5980b0f468174a319cf53427d858e
```

开发开始前必须执行：

```bash
git fetch --all --prune
git switch dev
git pull --ff-only
git rev-parse HEAD
```

若 Agent HEAD 已更新：

1. 阅读最新 `README.md`；
2. 阅读最新 `docs/rpc.md`；
3. 阅读 `src/rpc/protocol.rs`；
4. 阅读 Agent 对应的 `tests/tui_rpc_flow.rs`；
5. 确认本文档定义的 RPC 仍成立；
6. 将实际 Agent HEAD 写入 TUI 的 `docs/backend.md`。

## 1.2 最新 Runtime

```text
repository:
https://github.com/zqcli/minicore-runtime

branch:
dev

reviewed HEAD:
87f3cf92b9b5980b0f468174a319cf53427d858e

crate version:
0.4.0

rust-version:
1.85
```

TUI 不依赖 Runtime Rust crate。

Runtime 仅用于理解 Agent 的语义来源。

## 1.3 Agent CI 与核对范围

本次通过 GitHub 读取 Agent `dev`、5 个新增提交的 diff、相关实现/测试与 RPC 文档。
基线 commit `b2e23938d073ab21c2775faa623561ba929a5ed1` 对应的 CI run
`33897540665` 状态为 `completed / success`。[S10]

本次未在本地重新运行 Agent 测试，也没有读取一份实际的 `minicore-tui` 实现；文件映射以此前 TUI 设计为依据。
代码 Agent 必须对自己的 TUI checkout 复跑单元测试、快照和 E2E，不能将 Agent CI 当成 TUI 已验收。

Runtime 按本轮明确约束保持原基线。Agent 的增量 diff 未修改 `Cargo.toml` / `Cargo.lock`，固定依赖也未变化。[S1]

## 1.4 新增提交与 TUI 影响

| Agent 提交 | 已完成的后端工作 | TUI 要做的工作 |
|---|---|---|
| `bac2b715f7bee3a5865fc581f133dd60acadd1bc` | `cleanup_finished` 在 blocked 时保留 completion；增加 library/RPC 回归。 | 新 send 的错误不能清掉旧结果；重复 wait 幂等；区分内部失败与保存失败。 |
| `e511d9e29c75f7d6a7476baec09fc55ca5fcd379` | 验证同一 Loop 内 A 请求、read 工具、B 请求及最终 revision。 | 验证新 UI 确实按 request 分段，而非取消后重跑。 |
| `cc9ddf7436b49d2360ce5fde16b76e81cd52ef92` | 校验已存 model ID、system prompt、已知且不重复的 tools；坏记录 list 跳过、open 拒绝。 | 正确显示一般 Store 错误，保留其他 Session 的可用性。 |
| `c362446a156dbcc5854930d0dbaac97bb612ba19` | 明确 append 成功、尾部不完整行修复、shutdown barrier 和取消原因的边界。 | 修正所有过强“原子/强持久化/重开必丢”的假设及退出测试。 |
| `b2e23938d073ab21c2775faa623561ba929a5ed1` | Write 测试改用有界 gate 等待和明确 deadline。 | 不修改 Write Tool/RPC；TUI 测试也不得无界等待。 |

这些后端修复已存在，不是交给 TUI 开发者重新实施的任务。[S2][S3][S4][S5][S6]

## 1.5 补丁级能力与版本识别

本次 5 个提交之后 Agent 仍报告 `0.3.0`。`agent.ping` 只能识别协议版本范围，不能证明进程含上述修复。

开发与发布时，在 `docs/backend.md` 记录实际测试的 Agent SHA、Runtime revision 和 CI/E2E 结果；本规格的已验证基线是 `b2e2393…`。
不要求新增 `build_sha` RPC，不让 TUI 在运行时调用 GitHub，不从版本号推断补丁状态。
对其他 `0.3.x` 保持协议兼容，但要通过相同契约测试后再声称支持。

# 2. Runtime v0.4 的新语义

## 2.1 `AgentLoop` 是一次性执行

一次调用：

```text
AgentLoop::start(
    host history
    + fresh user input
    + ExecutionConfig
)
```

只运行一次。

结束后返回：

```text
LoopReport
```

Runtime 不提供：

```text
Session load
Session reopen
Conversation Store
Transcript
durable terminal
Session metadata
```

## 2.2 Host 持久化

Runtime 的：

```text
LoopReport::appended
```

只是当前 Loop 产生的内存增量。

Agent 决定：

```text
是否持久化
持久化到哪里
持久化成功后是否合并
失败时如何阻塞 Session
```

## 2.3 Runtime Update

```text
LoopHandle::update(ExecutionConfig)
```

语义：

- 整套配置原子替换；
- 在下一个真正发出的 Model Request 生效；
- 当前 Model Request 不变；
- 当前 Request 产生的 Tool Batch 使用原配置快照；
- Update 本身不会让一个已经要结束的 Loop 继续运行；
- 返回 `ConfigRevision`。

## 2.4 Runtime Steer

```text
LoopHandle::steer(UserInput)
```

语义：

- Steer 被放入有界内存队列；
- 在下一个 Request Boundary 应用；
- 被应用后进入 History，类型为 `UserMessageKind::Steering`；
- WaitingForInput 时拒绝；
- Loop Finalizing 或 Finished 时拒绝；
- 队列满时返回 QueueFull；
- Cancel、Shutdown 或进程退出前尚未应用的 Steer 可以丢失；
- 最终响应到达时若仍有 pending steer，Loop 会继续进入下一次请求。

## 2.5 Request 是新的 UI 观察边界

一个 Agent Turn / Loop 内可能有多次 Model Request：

```text
Prompt
→ Request 0
→ Tool
→ Request 1
→ Steer
→ Request 2
→ Final
```

每次 Request 有独立：

```text
request_index
config_revision
model
reasoning
Assistant output
Tool batch
Usage
```

旧 TUI 的单一：

```text
LiveTurn.text
LiveTurn.reasoning
```

不能继续表达新语义。

---

# 3. Agent v0.3 的新语义

## 3.1 Session 归 Agent 所有

Agent Session 负责：

```text
SessionRecord
Workspace
History
ExecutionConfig
LoopOptions
一个 active AgentLoop
History JSONL
Session Block 状态
```

每个用户 Prompt 启动一个新的 Runtime AgentLoop。

## 3.2 Store 格式

```text
<data_dir>/sessions/<session_id>/
├── session.json
└── history.jsonl
```

旧格式：

```text
manifest.json
conversation.log
```

不迁移。

TUI 不读取这些文件，也不执行 Store migration。

## 3.3 Loop 级追加与进程内成功边界

一个 Loop 结束后，Agent 按以下顺序处理：[S5][S8][S9]

```text
Runtime LoopReport
→ sanitize History
→ 序列化为一个 StoredLoopRecord JSON line
→ write_all + flush
→ 成功后合并进程内 History
→ 发布 Agent completion
```

这里的“一行一个 Loop”是记录格式与合并单位，**不是文件系统事务原子性承诺**。
当前追加路径不提供端到端 crash-durability proof；不能从 `persisted` 推导出 fsync、掉电不丢或工具副作用与日志原子提交。

- 模型/工具执行期间产生的数据是 Live 展示，不能提前标成已保存。
- `persistence=persisted` 确认该次追加在运行进程内成功，随后可从 `session.history` 对齐。
- 追加中断或报错时，磁盘可能没有新行、只有尾部片段，也可能已有完整行；TUI 不能判断具体哪种。
- Agent 打开文件时可尽力处理不完整尾行；这不是恢复原 AgentLoop，更不是旧 Runtime v0.3 的 unfinished Turn repair。
- 工具可能已经产生副作用，即使本轮 History 未确认保存，也不得自动重跑。

## 3.4 Persistence Failed 与阻塞结果保留

若 `history.jsonl` append 返回错误，Agent：

```text
turn.wait → 正常 RPC result，携带 Loop outcome 和 persistence=failed
Session → blocked / block_reason=persistence
内存 History → 不合并本 Loop
新 turn.send / session.update → 拒绝
```

**新增保证：**在该 Session 保持 loaded、原 completion 未被显式 close/shutdown 释放期间，
后续一次被拒绝的 send 不会销毁之前的 TurnResult；对该旧 TurnRef 重复调用 `turn.wait` 仍可取得同一结果。[S2][S8]

这不是历史 Turn registry，不延伸到 Session 重开或 Agent 重启。
`blocked/internal` 则可能使 `turn.wait` 返回 RPC internal error，不得伪造 `persistence=failed` 的正常 TurnResult。

TUI 必须分别理解：执行 outcome、追加确认状态、Session 是否 blocked。
“没有合并内存”不等于“磁盘确定没有任何本轮字节”。

## 3.5 History 是新权威接口

旧：

```text
session.transcript
```

已删除。

新：

```text
session.history
```

分页依据：

```text
offset
limit
```

返回：

```text
items
next_offset
total
```

## 3.6 Session 配置可更新

```text
session.update
```

可以更新：

```text
model
reasoning
```

更新顺序：

```text
先写 session.json
→ 替换 Session 长期 ExecutionConfig
→ 若有 active Loop，则调用 LoopHandle::update
```

返回：

```text
session
active_revision
```

配置现在不是创建后不可变。

## 3.7 Steering 已正式提供

```text
turn.steer
```

是 Runtime 真正的 request-boundary steering，不是 TUI follow-up queue。

---

# 4. Breaking Change 对照表

| 旧 TUI 假设 | 最新事实 | 必须修改 |
|---|---|---|
| `SessionRuntime` 长期拥有 Session | Runtime 每 Prompt 一个 `AgentLoop` | TUI 只理解 Agent Session/Loop |
| Core 持久化 Conversation | Agent 持久化完整 Loop | Completion 需检查 persistence |
| `session.transcript` | `session.history` | 协议与状态全换 |
| `ConversationSeq` cursor | `offset: usize` | 分页模型重写 |
| `TurnRef.instance_id` | 无 instance_id | 删除全部 instance identity |
| `TurnRef.turn_id` | `loop_id` | DTO 和关联键重写 |
| Session 状态 Idle/Running/Waiting/Closing | Idle/Running/Waiting/Finishing/Blocked | 状态机重写 |
| 一个 Turn 一个 Live Text/Reasoning | 一个 Loop 多 Request | Live 状态按 request_index 分段 |
| 没有 RequestStarted | 有 `request_started` | UI 追踪 model/reasoning/revision |
| Model/Reasoning 不可变 | `session.update` 可热更新 | Selector 语义重写 |
| Update 立即影响所有执行 | 下一 Request 生效 | UI 显示 pending revision |
| Running 时 Composer 禁用 | Running 时可 `turn.steer` | Composer 模式重写 |
| Steering 不支持 | 真正 request-boundary steer | 新增 RPC/UI 状态 |
| TurnTerminal 在 Transcript | History 无 terminal item | 不生成持久 terminal block |
| 重启 repair unfinished Turn | active Loop 不持久化 | 删除 repair 文案 |
| ToolCall arguments 可渲染 | History RPC 不暴露 arguments | Tool Card 不显示命令/path |
| Event 只有 Turn 级 | Event 带 request_index | Tool/Output 按 Request 分组 |
| TurnFinished 只表示成功结束 | 带 `persistence` | UI 显示 saved/unsaved |
| Session 可继续 | Persistence failure 后 Blocked | 增加 Blocked UI |
| Profile 决定 reopen 配置 | SessionRecord 已复制完整配置 | SessionInfo 为事实来源 |

---

# 5. 迁移决策

## 5.1 必须全量替换的模块

如果已有旧 TUI 实现，以下模块按新语义重写：

```text
src/protocol.rs
src/rpc.rs 中 RequestKind
src/app.rs 中 RPC/Event reducer
src/state/session.rs
src/state/transcript.rs
src/state/turn.rs
src/ui/transcript.rs 的数据投影
src/ui/tool.rs 的数据来源
src/ui/composer.rs 的 Running 行为
src/ui/footer.rs 的状态模型
src/ui/selector.rs 的 Model/Reasoning行为
tests/protocol.rs
tests/app_flow.rs
tests/agent_e2e.rs
协议 fixtures
```

## 5.2 可以保留的模块

后端无关实现质量合格时可以保留：

```text
src/terminal.rs
src/theme.rs
src/markdown.rs
Composer 对 tui-textarea 的包装
ScrollState
Pi 风格 Layout
User/Assistant/Reasoning 的基本 render 函数
Selector 的搜索和绘制
Help/Logs/Error Overlay
Terminal restore tests
CJK/Unicode tests
Ratatui snapshots 的视觉基线
```

## 5.3 不实现双协议

不得增加：

```rust
enum BackendProtocol {
    AgentV02,
    AgentV03,
}
```

不得保留：

```text
session.transcript fallback
instance_id fallback
v0.2 SessionState adapter
旧 conversation entry adapter
```

`minicore-tui v0.2` 只支持：

```text
minicore-agent 0.3.x
```

## 5.4 未开始开发的情况

若 `minicore-tui` 尚未有代码：

- 直接按本文目标结构开发；
- 不需要实现迁移脚本；
- 不需要先实现旧 v0.1；
- 旧 Spec 仅用于视觉参考。

---

# 6. 目标整体架构

```text
┌─────────────────────────────────────────────┐
│                minicore-tui                 │
│                                             │
│  TerminalGuard                              │
│  RpcProcess                                 │
│  App                                        │
│  ├── CatalogState                           │
│  ├── SessionView[]                          │
│  ├── HistoryState                           │
│  ├── LiveLoop                               │
│  │   ├── LiveRequest[]                      │
│  │   └── PendingSteer[]                     │
│  ├── Composer                               │
│  ├── Dock / Selector                        │
│  └── Pi-style Render                        │
└──────────────────┬──────────────────────────┘
                   │ stdio JSON-RPC
┌──────────────────▼──────────────────────────┐
│              minicore-agent 0.3             │
│                                             │
│  Agent Session                              │
│  History JSONL                              │
│  Session Update                             │
│  Turn Steer                                 │
│  Agent Event                                │
└──────────────────┬──────────────────────────┘
                   │ Rust API
┌──────────────────▼──────────────────────────┐
│             minicore-runtime 0.4            │
│                                             │
│  one-shot AgentLoop                         │
│  request-boundary update / steer            │
│  LoopReport delta                           │
└─────────────────────────────────────────────┘
```

---

# 7. 技术栈

## 7.1 Rust

```text
edition = 2024
rust-version = 1.85
unsafe_code = forbid
```

## 7.2 TUI 版本

继续使用已与 MSRV 对齐的组合：

```toml
ratatui = { version = "=0.29.0", default-features = false, features = ["crossterm"] }
crossterm = { version = "=0.28.1", features = ["event-stream"] }
tui-textarea = { version = "=0.7.0", default-features = false, features = ["crossterm"] }
```

## 7.3 Markdown

为避免最新 `tui-markdown` 自动进入 Ratatui 0.30 生态，选择一种：

### 推荐

```toml
tui-markdown = { version = "=0.3.5", default-features = false }
```

开发开始时运行：

```bash
cargo tree -d
cargo tree -p ratatui
cargo tree -p crossterm
```

确认无重复不兼容 Ratatui/Crossterm。

### 备选

若该版本和 Ratatui 0.29 无法干净组合：

```toml
pulldown-cmark = "=0.13.0"
```

在现有 `MarkdownRenderer` 内实现最小转换。

不得为了 Markdown 升级整个项目到 Rust 1.88。

## 7.4 其他依赖

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
time = { version = "0.3", features = ["formatting", "parsing"] }
```

Dev：

```toml
insta = "1"
tempfile = "3"
pretty_assertions = "1"
```

---

# 8. Package 与目录

## 8.1 Package

```toml
[package]
name = "minicore-tui"
version = "0.2.0"
edition = "2024"
rust-version = "1.85"
license = "MIT OR Apache-2.0"
```

一个 package：

```text
src/lib.rs
src/main.rs
```

不拆 workspace。

## 8.2 目标目录

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
│   │   ├── history.rs
│   │   ├── live_loop.rs
│   │   ├── request.rs
│   │   └── tool.rs
│   │
│   └── ui/
│       ├── mod.rs
│       ├── layout.rs
│       ├── header.rs
│       ├── history.rs
│       ├── user.rs
│       ├── assistant.rs
│       ├── reasoning.rs
│       ├── steering.rs
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
│   ├── control_flow.rs
│   ├── persistence_flow.rs
│   ├── render_snapshots.rs
│   ├── agent_e2e.rs
│   └── terminal_restore.rs
│
├── tests/fixtures/protocol/
│   ├── ping.json
│   ├── model-list.json
│   ├── profile-list.json
│   ├── session-list.json
│   ├── session-create.json
│   ├── session-update-idle.json
│   ├── session-update-active.json
│   ├── session-state-running.json
│   ├── session-state-blocked.json
│   ├── session-history.json
│   ├── turn-send.json
│   ├── turn-wait-persisted.json
│   ├── turn-wait-persistence-failed.json
│   ├── request-started.json
│   ├── output-delta.json
│   ├── tool-started.json
│   ├── tool-finished.json
│   └── rpc-error.json
│
└── docs/
    ├── architecture.md
    ├── backend.md
    ├── keybindings.md
    ├── protocol.md
    └── migration-v0.1-to-v0.2.md
```

---

# 9. Agent 版本兼容检查

## 9.1 Ping

启动后第一条请求：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "agent.ping",
  "params": {}
}
```

期望：

```json
{
  "result": {
    "version": "0.3.0"
  }
}
```

## 9.2 支持范围

TUI v0.2 的协议版本范围仍为 `minicore-agent 0.3.x`；不支持 0.2.x 或 0.4.x 的未知 breaking 协议。

但 `0.3.0` 版本号不足以区分本轮修复前后的二进制。发布/E2E 使用第 1 节固定的 Agent SHA，
并记录实际后端构建来源。版本检查和补丁验收是两件不同的事。

不增加网络版本查询、双协议分支或新 RPC 字段。

## 9.3 实现

```rust
const SUPPORTED_AGENT_MAJOR: u64 = 0;
const SUPPORTED_AGENT_MINOR: u64 = 3;

fn validate_agent_version(value: &str) -> Result<(), ProtocolError>;
```

只解析：

```text
major.minor.patch
```

允许：

```text
0.3.0
0.3.1
```

不需要引入 semver crate。

预发布版本：

```text
0.3.0-dev
```

开发模式可接受前缀 `0.3.`，正式 release 默认拒绝。

## 9.4 不回退

发现 Agent v0.2：

```text
显示 Fatal:
“minicore-tui v0.2 requires minicore-agent 0.3.x”
```

不启用旧协议。

---

# 10. Agent 进程与 Terminal

## 10.1 启动命令

Agent 当前 CLI 顺序固定：

```bash
minicore-agent --config <agent.toml> --stdio
```

TUI 必须按该顺序启动。

## 10.2 TUI CLI

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

## 10.3 Terminal 生命周期

保留旧 Spec：

```text
alternate screen
raw mode
bracketed paste
mouse capture
hardware cursor
panic hook
显式 restore
Drop best-effort restore
```

## 10.4 Agent 退出与响应排空

正常退出：

```text
停止接受新 Prompt/Steer/Update
→ 保持 stdin writer、stdout reader、stderr reader 存活
→ 发送 agent.shutdown
→ 继续处理此前注册的 turn.wait 响应和待到达事件
→ 处理最终 shutdown 响应
→ 等待 child exit 和 reader EOF
→ join TUI-owned tasks
→ restore terminal
```

`agent.shutdown` 是清理屏障；不是“所有 Loop 保存成功”的汇总确认。
有 `persistence=failed` 或内部错误时，退出提示仍应保留该事实。[S5][S7]

当前 Agent 的 close/shutdown 使用 Runtime 的 user cancellation 路径。
运行中的 Loop 在退出时可能返回 `outcome={type:cancelled,reason:user}`；这不是协议错误。
不要强求 `reason=shutdown`，不要因此重试取消或重跑 Loop。

5 秒超时可作为本 TUI 的强制退出上限，但必须标记为强制终止：
发送 kill、wait child、回收任务、恢复终端，并说明最后结果未确认。
这个时间是客户端退出策略，不是 Agent 承诺的最大收尾时长。

Agent 异常退出时保留已收到的 result 和临时输出；未确认部分标成 unknown，
不得伪造保存成功或自动重启。TUI 不通过直接 Drop Agent/子进程替代显式协议退出。

# 11. RPC 传输

## 11.1 NDJSON

```text
一行一个 JSON-RPC frame
```

只允许：

```text
一个 stdin writer
一个 stdout reader
一个 stderr reader
```

## 11.2 Frame 顺序

允许：

```text
Response 和 Event 交错
turn.wait deferred response 晚于后续请求
Event 早于 turn.send response
session.update response 与 RequestStarted 交错
```

按 Request ID 分发。

## 11.3 Frame 上限

Agent 入站请求上限为 1 MiB。

TUI Composer上限：

```rust
const MAX_COMPOSER_BYTES: usize = 256 * 1024;
```

与 Runtime `BoundedText` 绝对上限一致。

TUI 读取 Agent stdout：

```rust
const MAX_RPC_FRAME_BYTES: usize = 32 * 1024 * 1024;
```

History 页大小：

```rust
const HISTORY_PAGE_LIMIT: usize = 20;
```

理由：

- 单个 Tool/Model Text 有 256 KiB 上限；
- 一个 History page 可能包含多个较大的 item；
- 8 MiB 已不适合最新 History；
- 32 MiB 足以覆盖正常页面，但仍防止无界分配。

若 32 MiB 仍超限：

```text
Fatal Protocol Error
```

不自动以更小页面重试，因为当前 frame 已无法解析。

未来可由 Agent 增加 byte-bounded history pagination。

## 11.4 stderr

保留最近：

```text
200 行
每行 4096 bytes
```

不落盘。

---

# 12. 最新 RPC 方法

TUI 必须支持：

```text
agent.ping
agent.shutdown

profile.list
model.list

session.list
session.create
session.open
session.close
session.delete
session.state
session.update
session.history

turn.send
turn.cancel
turn.wait
turn.steer
```

现有：

```text
interaction.answer
```

只保留 Protocol 能力识别，不在当前 TUI 实现交互审批。

删除旧调用：

```text
session.transcript
```

---

# 13. ID 模型

## 13.1 Session

```rust
pub struct SessionId(String);
```

Wire：

```text
ses_<32 lower-case hex>
```

TUI 不自行生成 SessionId。

## 13.2 Turn

```rust
pub struct TurnRef {
    pub session_id: SessionId,
    pub loop_id: LoopId,
}
```

删除：

```text
instance_id
turn_id
```

## 13.3 Request

```rust
pub struct RequestKey {
    pub loop_id: LoopId,
    pub request_index: u32,
}
```

## 13.4 Config Revision

```rust
pub struct ConfigRevision(u64);
```

TUI 可以内部使用 newtype。

Wire 是 number。

## 13.5 Tool

```rust
pub struct ToolCallId(String);
```

关联键：

```text
session_id
loop_id
request_index
tool_call_id
```

---

# 14. Protocol DTO

## 14.1 DTO 原则

TUI 不依赖 Agent Rust crate。

所有 Response DTO：

```rust
#[derive(Clone, Debug, Deserialize)]
```

不要加：

```rust
#[serde(deny_unknown_fields)]
```

原因：

- Agent 0.3 patch 可以增加只读字段；
- TUI 忽略新字段即可。

所有 Outbound Params：

```rust
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
```

由本地类型保证字段正确。

## 14.2 Reasoning

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reasoning {
    Auto,
    Disabled,
    Low,
    Medium,
    High,
}
```

未知值返回 ProtocolError。

## 14.3 Usage

```rust
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct UsageView {
    #[serde(default)]
    pub input_tokens: Option<u64>,

    #[serde(default)]
    pub output_tokens: Option<u64>,

    #[serde(default)]
    pub reasoning_tokens: Option<u64>,

    #[serde(default)]
    pub cache_read_tokens: Option<u64>,

    #[serde(default)]
    pub cache_write_tokens: Option<u64>,

    #[serde(default)]
    pub provider_total_tokens: Option<u64>,
}
```

不要把缺失字段显示为 0。

显示：

```text
— 
```

或不显示。

---

# 15. Catalog DTO

## 15.1 Model

```rust
pub struct ModelInfo {
    pub id: String,
    pub model_ref: String,
    pub context_window: u64,
    pub supports_tools: bool,
    pub supported_reasoning: Vec<Reasoning>,
}
```

## 15.2 Profile

```rust
pub struct ProfileInfo {
    pub id: String,
    pub model: String,
    pub reasoning: Reasoning,
    pub tools: Vec<String>,
}
```

Agent 可能仍返回 approval 字段。

TUI 忽略。

## 15.3 排序

TUI 可以保留 Agent 顺序，但 Selector显式：

```text
按 id
```

稳定排序。

---

# 16. Session DTO

## 16.1 SessionInfo

```rust
pub struct SessionInfo {
    pub session_id: SessionId,
    pub title: Option<String>,
    pub profile: String,
    pub workspace: String,
    pub model: String,
    pub reasoning: Reasoning,
    pub loaded: bool,
    pub created_at: String,
    pub updated_at: String,
}
```

没有：

```text
instance_id
```

## 16.2 Session Status

```rust
pub enum SessionStatus {
    Idle,
    Running,
    WaitingForInput,
    Finishing,
    Blocked,
}
```

删除：

```text
Closing
```

## 16.3 Block Reason

```rust
pub enum SessionBlockReason {
    Persistence,
    Internal,
}
```

## 16.4 Loop Status

```rust
pub enum LoopStatus {
    Starting,
    RunningModel,
    RunningTools,
    WaitingForInput,
    Finishing,
    Finished,
}
```

## 16.5 LoopState

```rust
pub struct LoopStateView {
    pub loop_id: LoopId,
    pub status: LoopStatus,
    pub request_index: u32,
    pub config_revision: ConfigRevision,
    pub model: Option<String>,
    pub pending_interaction: Option<PendingInteractionView>,
}
```

## 16.6 SessionState

```rust
pub struct SessionStateView {
    pub session_id: SessionId,
    pub status: SessionStatus,
    pub active_loop: Option<LoopStateView>,
    pub block_reason: Option<SessionBlockReason>,
}
```

TUI 不假设：

```text
Running 必然有完整 active_loop fields
```

字段可能因 Event 丢失只能通过 `session.state` 刷新。

---

# 17. Session Update DTO

## 17.1 Params

```rust
pub struct SessionUpdateParams {
    pub session_id: SessionId,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
}
```

至少一个字段非 None。

## 17.2 Result

```rust
pub struct SessionUpdateResult {
    pub session: SessionInfo,
    pub active_revision: Option<ConfigRevision>,
}
```

## 17.3 语义

```text
session:
    Agent 已写入 SessionRecord 的新设置（不是 crash-durability 保证）

active_revision:
    Some(n)
        当前 active Loop 已接受配置候选 revision n
        下一次真正发出的 Request 才采用
    None
        Session Idle
        或 Loop 已 sealed/finalizing
        设置仍会作用于下一 Turn
```

---

# 18. History DTO

## 18.1 Request

```json
{
  "session_id": "...",
  "offset": 0,
  "limit": 20
}
```

## 18.2 Page

```rust
pub struct HistoryPage {
    pub items: Vec<IndexedHistoryItem>,
    pub next_offset: Option<usize>,
    pub total: usize,
}
```

## 18.3 Indexed Item

```rust
pub struct IndexedHistoryItem {
    pub index: usize,
    pub item: HistoryItemView,
}
```

## 18.4 History Item

```rust
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum HistoryItemView {
    User(UserHistoryView),
    Assistant(AssistantHistoryView),
    ToolResult(ToolResultHistoryView),
    Summary(SummaryHistoryView),
}
```

## 18.5 User

```rust
pub struct UserHistoryView {
    pub loop_id: LoopId,
    pub kind: UserMessageKind,
    pub text: String,
}
```

```rust
pub enum UserMessageKind {
    Prompt,
    Steering,
}
```

## 18.6 Assistant

```rust
pub struct AssistantHistoryView {
    pub loop_id: LoopId,
    pub request_index: u32,
    pub model: String,
    pub reasoning_level: Reasoning,
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCallView>,
    pub usage: UsageView,
    pub finish_reason: String,
}
```

`finish_reason` 使用 String，避免 TUI 因 Agent 增加 Provider-neutral值而崩溃。

## 18.7 ToolCall

```rust
pub struct ToolCallView {
    pub tool_call_id: ToolCallId,
    pub name: String,
    pub call_index: u32,
}
```

没有 arguments。

## 18.8 ToolResult

```rust
pub struct ToolResultHistoryView {
    pub loop_id: LoopId,
    pub request_index: u32,
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub outcome: String,
    pub content: String,
}
```

## 18.9 Summary

```rust
pub struct SummaryHistoryView {
    pub content: String,
}
```

## 18.10 没有 Terminal

最新 History RPC 不返回：

```text
TurnTerminal
StoredLoopOutcome
completed_at
requests/tool_rounds summary
```

TUI 不得创建假的 durable Terminal item。

---

# 19. Turn Result DTO

## 19.1 Result

```rust
pub struct TurnResultView {
    pub turn: TurnRef,
    pub outcome: LoopOutcomeView,
    pub usage: UsageView,
    pub requests: u32,
    pub tool_rounds: u16,
    pub final_config_revision: ConfigRevision,
    pub persistence: TurnPersistence,
}
```

## 19.2 Persistence

```rust
pub enum TurnPersistence {
    Persisted,
    Failed,
}
```

## 19.3 Outcome

```rust
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LoopOutcomeView {
    Completed,

    Cancelled {
        reason: String,
    },

    Failed {
        kind: String,
        model_error: Option<ModelErrorView>,
    },
}
```

## 19.4 Model Error

```rust
pub struct ModelErrorView {
    pub kind: String,
    pub delivery: String,
    pub retryable: bool,
    pub retry_after_millis: Option<u64>,
}
```

---

# 20. Agent Event DTO

## 20.1 Event Meta

```rust
pub struct EventMeta {
    pub session_id: SessionId,
    pub loop_id: Option<LoopId>,
    pub dropped_before: u64,
}
```

## 20.2 必须处理

```text
session_opened
session_closed
session_state
turn_started
request_started
output_delta
tool_started
tool_progress
tool_finished
turn_finished
```

## 20.3 可解析但不支持 UI

```text
interaction_requested
interaction_resolved
```

## 20.4 RequestStarted

```rust
pub struct RequestStartedEvent {
    pub turn: TurnRef,
    pub request_index: u32,
    pub config_revision: ConfigRevision,
    pub model: String,
    pub reasoning: Reasoning,
    pub meta: EventMeta,
}
```

这是最新 TUI 的重要事件。

## 20.5 OutputDelta

增加：

```text
request_index
```

不能只按 Loop 合并。

## 20.6 Tool Event

增加：

```text
request_index
```

Tool 必须挂在对应 Request。

---

# 21. App 状态重构

## 21.1 App

```rust
pub struct App {
    pub connection: ConnectionState,
    pub catalogs: CatalogState,
    pub sessions: SessionsState,

    pub dock: Dock,
    pub overlay: Option<Overlay>,
    pub composer: Composer,

    pub pending_requests: HashMap<RequestId, RequestKind>,
    pub notices: VecDeque<Notice>,
    pub agent_logs: VecDeque<String>,

    pub theme: ThemeKind,
    pub dirty: bool,
    pub last_render: Instant,
    pub should_quit: bool,
}
```

## 21.2 SessionView

```rust
pub struct SessionView {
    pub info: SessionInfo,
    pub state: Option<SessionStateView>,

    pub history: HistoryState,
    pub live: Option<LiveLoop>,

    pub config_update: Option<PendingConfigUpdate>,

    pub scroll: ScrollState,
    pub loading: bool,
    pub event_gap: bool,
    pub unsaved_loop: Option<UnsavedLoop>,
    pub last_result: Option<TurnResultView>, // 仅最后结果的进程内展示/幂等处理
}
```

## 21.3 删除旧字段

删除：

```text
instance_id
transcript
next_after ConversationSeq
last_terminal
LiveTurn 单一 text/reasoning
wait_registered bool（改由 Pending Request 事实表示）
```

---

# 22. History State

```rust
pub struct HistoryState {
    pub items: Vec<IndexedHistoryItem>,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub loaded: bool,

    pub presentation: Vec<HistoryBlock>,
    pub render_cache: HistoryRenderCache,
}
```

## 22.1 Page 合并

区分两个数：

```text
items.len() / loaded_end = TUI 已拥有的连续 [0, loaded_end) item 数
page.total              = Agent 该次响应中报告的全部已存 item 数
```

检查页内 index 从请求 offset 连续递增；重复 index 内容相同可忽略，不同则显示协议错误。
更新 `total` 与 `next_offset` 后，继续抓取尚未加载的页，不得把尚未加载的 `total` 当成本地 cursor。
每个 Session 只允许一个 History page 请求在途，不需加入通用分页框架。

## 22.2 初始加载

```text
offset=0
limit=20
```

收到页后立即 render。

继续下一页，直到：

```text
next_offset=None
```

## 22.3 增量加载

Turn 确认 `persistence=persisted` 后，从本地已连续加载到的末尾继续：

```text
offset = history.items.len()
```

不是无条件使用上一页 `total`。例如本地只有 20 项而 total=80，下一次必须从 20 读取。
若已有一次加载正在进行，让它完成至末页再进行当前轮对齐；不要并发重置 cursor。
Session close/reopen 后从 0 重建，以这次 Agent 加载到的 History 为准。

## 22.4 History 的生命周期边界

同一次 loaded 生命周期内，Agent 对外 History 按成功追加后的 item 只增不减，index 可作当前视图的稳定键。

显式 close/reopen 后应清空旧分页游标并从 0 加载；不比较新 total 与旧 total 来证明是否发生数据损失。
出现非预期缩小或 index 冲突时，显示当前 Session 的同步错误，保留其现有可见内容，
不静默修写 Store、不自动重跑 Prompt。

尾部修复、部分追加与旧数据不兼容均由 Agent 处理，不在 TUI 实现。[S5][S9]

# 23. History Presentation

## 23.1 `HistoryBlock`

```rust
pub enum HistoryBlock {
    UserPrompt(UserBlock),
    Steering(SteeringBlock),
    Assistant(AssistantBlock),
    Tool(ToolBlock),
    Summary(SummaryBlock),
    Notice(NoticeBlock),
}
```

没有已存 terminal item。Notice 可表现本次进程观测到的结果，但不能冒充 Agent 返回的 History。
本章中的“已存 History”均指 Agent 确认的追加结果/读取视图，不表示事务日志或断电不丢的保证。

## 23.2 映射规则

### User Prompt

```text
kind=prompt
→ Pi User Card
```

### Steering

```text
kind=steering
→ 紧凑 Steering Card
```

### Assistant

产生：

```text
Assistant Text/Reasoning Block
+
0..N Tool Call placeholders
```

### ToolResult

按：

```text
tool_call_id
```

附加到 Tool Block。

找不到对应 ToolCall：

```text
创建 Orphan ToolResult Block
```

不丢弃数据。

### Summary

显示折叠 Summary。

## 23.3 按 Loop/Request 分组

每个 Assistant/Tool 记录：

```text
loop_id
request_index
```

TUI 内部可构造：

```rust
struct HistoryRequestGroup {
    loop_id: LoopId,
    request_index: u32,
    model: String,
    reasoning: Reasoning,
    assistant: Option<AssistantBlock>,
    tools: Vec<ToolBlock>,
}
```

不需要在 UI 固定显示 Request heading。

仅当：

```text
一个 Loop 多 Request
或 Model/Reasoning 改变
```

显示 dim heading：

```text
request 2 · deep · high
```

---

# 24. Tool 展示限制

## 24.1 最新 RPC 不暴露 Tool Arguments

因此旧 Spec 中以下展示不能可靠实现：

```text
read src/lib.rs
$ cargo test
write src/main.rs
patch src/session.rs
```

TUI 不得：

- 从 ToolResult 文本猜 arguments；
- 解析 stderr 推断 command；
- 读取 Workspace；
- 依赖工具输出格式反推 path。

## 24.2 Live Tool

```text
read                              running
```

或：

```text
bash                              completed
```

可显示：

```text
request index
call index（History时）
progress message
content bytes
```

## 24.3 Stored Tool

展开后显示：

```text
ToolResult content
outcome
```

最大：

```text
40 行
32 KiB render preview
```

## 24.4 后续增强边界

如果未来需要 Pi 级 Tool summary，应由 Agent RPC 增加：

```text
safe presentation
```

例如：

```json
{
  "label": "cargo test",
  "target": "src/lib.rs"
}
```

本阶段不修改 Agent。

---

# 25. LiveLoop

## 25.1 类型

```rust
pub struct LiveLoop {
    pub turn: TurnRef,
    pub prompt: String,

    pub requests: BTreeMap<u32, LiveRequest>,
    pub pending_steers: VecDeque<PendingSteer>,

    pub outcome: Option<TurnResultView>,
    pub persistence: Option<TurnPersistence>,

    pub event_gap: bool,
    pub cancel_requested: bool,
    pub wait_request: Option<RequestId>,
}
```

## 25.2 LiveRequest

```rust
pub struct LiveRequest {
    pub request_index: u32,

    pub config_revision: Option<ConfigRevision>,
    pub model: Option<String>,
    pub reasoning: Option<Reasoning>,

    pub text: String,
    pub reasoning_text: String,
    pub tools: Vec<LiveTool>,

    pub started: bool,
    pub finished_model: bool,
}
```

## 25.3 Lazy Request

如果 `RequestStarted` Event 丢失，但收到：

```text
OutputDelta request_index=N
ToolStarted request_index=N
```

则创建：

```rust
LiveRequest {
    request_index: N,
    config unknown,
    started: false
}
```

并设置：

```text
event_gap=true
```

不要丢掉 Delta。

---

# 26. Turn Send

## 26.1 Local Pending Start

在 RPC Response 前创建：

```rust
pub struct PendingLoopStart {
    pub session_id: SessionId,
    pub submission_id: u64,
    pub text: String,
}
```

UI 立即显示 Pending User Card。

## 26.2 Event 早于 Response

如果收到：

```text
turn_started(session X, loop Y)
```

而 X 有 PendingLoopStart：

```text
建立 LiveLoop(turn Y)
绑定 prompt text
```

随后 `turn.send` response 必须返回同一个 loop_id。

不一致：

```text
Fatal Protocol Error
```

## 26.3 Response 先到

创建 LiveLoop，随后处理 Event。

## 26.4 注册 Wait

`turn.send` success 后立即发送：

```text
turn.wait
```

不等待：

```text
turn_started
request_started
output_delta
```

## 26.5 Send Failure

```text
删除 PendingLoopStart
恢复 Composer 文本
显示 Error
```

如果 Composer 已有新内容：

```text
不要覆盖
显示 failed prompt Notice
```

为了简化，可在 send in-flight 时禁用 Composer，直到 Response。

---

# 27. RequestStarted

## 27.1 更新 LiveRequest

```text
requests[request_index]:
    config_revision
    model
    reasoning
    started=true
```

## 27.2 Session Config Update 的确认

`active_revision=Some(n)` 表示当前 Loop 接受了更新候选，不表示当前请求已切换。
确认依据是同一 `loop_id` 的 `request_started.config_revision` 及其 model/reasoning。

- 与最新 pending update 的 revision、model、reasoning 完全一致：可标记“本请求已采用”。
- 更早的 revision：仍显示 pending，不重写已有 Request 的标签。
- 更高的 revision：可能是后续 update 替代了候选，按实际请求显示；不能称某个未观察到的旧 revision 曾生效。
- `request_started` 先于 update response 到达：先缓存 request，再在处理 response 时比较已有证据；不得永久停在 pending。
- Event 缺失时，可用 `session.state` 校验 config_revision/model；该视图没有 reasoning 字段，未知时不要猜。结束后用 History 的 per-request model/reasoning 对齐。

revision 只在一个 Loop 内比较。新 Loop 初始 revision 回到 0，不构成回退错误。
以上是既有 request-boundary 语义；本次新增测试证明其可在同一 Loop 内真正换模。[S3][S7]

## 27.3 Footer

显示：

```text
request 2 · deep · high · rev 3
```

Current Request配置优先于 SessionInfo。

---

# 28. Session Update

## 28.1 新能力

当前 Session 可以修改：

```text
model
reasoning
```

TUI 必须正式支持：

```text
session.update
```

## 28.2 Model Selector

### 有 Active Session

选择后：

```text
session.update {
  session_id,
  model
}
```

不是创建新 Session。

### 无 Active Session / NewSession Dock

只修改 New Session draft。

## 28.3 Reasoning Selector

逻辑相同。

## 28.4 本地能力校验

TUI 可根据：

```text
model.supported_reasoning
model.supports_tools
```

提前阻止明显非法选择。

Agent仍是权威。

不得自动降级 Reasoning。

## 28.5 Update In-flight

同一 Session 同时最多一个 `session.update` RPC。

Selector disabled直到 Response。

不建立 Update queue。

## 28.6 Response

成功后立即：

```text
SessionView.info = result.session
```

### `active_revision=Some(n)`

显示：

```text
“Saved · applies at next model request (rev n)”
```

保存：

```rust
PendingConfigUpdate {
    revision: Some(n),
    model,
    reasoning,
    state: WaitingBoundary,
}
```

### `active_revision=None`

若 Idle：

```text
“Updated for next turn”
```

若 Running/Finishing：

```text
“Saved for next turn; no active revision was returned.”

不要仅凭可能滞后的UI Running状态推断确切sealed时刻；None不表示保存失败。
```

## 28.7 当前 Tool Batch

更新发生在 Tool 正在运行：

```text
当前 Tool Batch 使用产生它的旧 Request config
下一 Request 使用新 config
```

TUI 不取消 Tool，不重新标记其 Model。

## 28.8 Update 不延长 Loop

只更新 Model/Reasoning 不会强制重新调用模型。当前最终响应可以直接结束；保存的 Session 设置用于下一 Turn。
只有被接受、待应用的 Steer 等既有继续条件才会延长当前 Loop。

TUI 不能用“换模后取消旧 Loop 再发新 Prompt”模拟 `session.update`，也不能在 SessionInfo 改变时
重标当前 request0 的模型。验收必须是同一 `loop_id` 下 request0=Model A、原工具批次完成、request1=Model B。[S3]

# 29. Steering

## 29.1 Composer 模式

```rust
pub enum ComposerMode {
    Prompt,
    Steer,
    DisabledWaiting,
    DisabledFinishing,
    DisabledBlocked,
    DisabledNoSession,
}
```

## 29.2 模式映射

| Session/Loop 状态 | Composer |
|---|---|
| Idle | Prompt |
| RunningModel | Steer |
| RunningTools | Steer |
| Starting | Steer |
| WaitingForInput | DisabledWaiting |
| Finishing | DisabledFinishing |
| Blocked | DisabledBlocked |
| 无 Session | DisabledNoSession |

## 29.3 Prompt

Idle + Enter：

```text
turn.send
```

## 29.4 Steer

Running + Enter：

```text
turn.steer
```

## 29.5 Steer Placeholder

```text
Steer the current turn…
```

状态栏：

```text
Steers apply at the next model request.
```

RunningTools：

```text
Current tool batch will finish first.
```

## 29.6 Steer RPC

```json
{
  "session_id": "...",
  "loop_id": "...",
  "text": "Do not modify tests."
}
```

## 29.7 In-flight

为了保持输入不丢失：

```text
发送 steer 时不立即清空 Composer
禁用 Composer直到 steer Response
```

Response success：

```text
清空 Composer
追加 PendingSteer(state=Queued)
```

Response error：

```text
保留文本
显示 Error
```

## 29.8 PendingSteer

```rust
pub struct PendingSteer {
    pub local_id: u64,
    pub text: String,
    pub state: PendingSteerState,
}

pub enum PendingSteerState {
    Queued,       // turn.steer 返回 ok，只确认接受
    Persisted,    // 在已存 History 中找到对应 steering item
    NotRecorded,  // 完成且 History 对齐后，没有对应 item
    Unconfirmed,  // 写入失败、连接断开或尚不能核对
}
```

local_id 仅用于本地请求/显示关联，不发送给 Agent。它不是后端 Steer ID。
不新建第二个本地待执行队列；这些条目只是已提交指令的临时展示。

## 29.9 Boundary 不是逐条 Steer 回执

`turn.steer` 只有 `{ok:true}`，没有 Steer ID、apply request index 或逐条 applied Event。
由于 stdout 事件可以延迟或丢失，观察到更大的 `request_index` 不能严格证明某条本地 Steer 已被该请求采用。

因此不得把所有 Queued 在下一次 `request_started` 到达时统一改成 Applied，
也不得根据接收时间把它插入“确定的 request 前缀”。可保留“已接受，等待历史确认”提示。
Loop 完成后，用 `session.history` 中真实 `kind=steering` 的 item 确认是否收录及其顺序。

这是对原规格不严谨说明的修正；Runtime 与 Agent 不需要为此新增接口。

## 29.10 Persisted

当本轮 `persistence=Persisted` 且本轮 History 全部分页完成后：

- 只在同一 loop_id 的 `kind=steering` items 中匹配；
- 按原文本及提交 FIFO 处理重复文本，匹配成功以真实 History 替换临时卡片；
- 仅根据 History 声称“已记录”，不虚构具体 request_index；
- 未找到匹配项，显示“本轮历史未记录此指令”，保留用户可见文本，不自动重发。

History 排序优先于本地提交时间；展示保持真实而非猜测的上下文位置。

## 29.11 未记录与未确认

若本轮已成功存储且完整 History 中没有对应 Steering item，可标记 `NotRecorded`。

若追加失败、Agent 崩溃、连接中断或核对请求失败，标记 `Unconfirmed`：
这不证明指令从未应用，也不证明它已永久保存。TUI 不自动重发，避免重复影响已经执行的工具。

提示可用：

```text
Accepted, awaiting history confirmation.
Not included in this turn's stored history.
Could not confirm whether this steer was saved.
```

Runtime 尚未应用的队列项可在取消/结束时丢弃；UI 只能报告现有接口足以证明的结果。

## 29.12 Queue Full

RPC：

```text
-32016 steer_queue_full
```

行为：

```text
保留 Composer文本
显示 Queue full
允许稍后重试
```

不自动排队在 TUI。

## 29.13 WaitingForInput

Steer被拒绝。

当前 TUI不支持审批：

```text
显示 Unsupported interaction
允许 Esc cancel
```

不得自动 Allow。

---

# 30. Turn Wait、Blocked 与追加确认

## 30.1 三个彼此独立的问题

```text
outcome      = Loop 执行怎样结束
persistence  = Agent 本次追加操作是否确认成功
SessionState = 当前 Session 是否还能接受新操作
```

`completed + failed` 是合法结果：执行完成、保存未确认成功、Session 阻塞。
`failed + persisted` 也是合法结果：执行失败，但已有历史被保存。
不要把 RPC 成功响应、执行成功和追加成功压成一个 success 布尔值。

`turn.wait` 提供 Agent-level completion。即使 UI 已收到 `blocked`、`idle` 或 `turn_finished`，
也不能销毁尚未完成的 wait 请求或仅根据事件生成最终结果。[S2][S7][S8]

## 30.2 Persisted

```text
收到 turn.wait(persistence=persisted)
→ 保存 TurnResult（进程内）
→ 从本地连续 History 末尾加载新页
→ 在本 Loop 的 items 全部核对后替换 LiveLoop
→ 清除本 Loop 的 event gap
→ 刷新 session.state
```

UI 可显示“已保存 / Saved”，含义限于“Agent 确认追加成功”。
README/Help 不得将其扩张为“事务提交”“安全落盘”“崩溃不丢失”或“fsync 完成”。
若 History 请求失败，保留 live 展示并标注“保存已确认，历史加载失败”，不回滚成 `persistence=failed`。

## 30.3 执行失败但已保存

仍然加载 History。失败前已完成的 Assistant/ToolResult 可能被收录，未完成流式文本也可能没有最终 History item。
展示 `outcome.kind` 与结构化 model error，不自动重试整 Loop，即使 provider 错误带 `retryable=true`。

## 30.4 Persistence Failed

```text
收到 turn.wait(persistence=failed)
→ 保留实际收到的结果与临时输出
→ LiveLoop 转为 UnsavedLoop
→ 标记 Session blocked/persistence
→ 禁用新 Prompt、Steer 和 Update
→ 保留原 TurnRef 与已知 completion
```

在当前 loaded 生命周期中，`session.history` 不含本次失败追加所对应的内存合并结果，
因此不能用它假装恢复本轮完整输出；但仍可以查看此前已存的历史。

磁盘结果没有由该失败响应完全确定。不能断言“本轮绝未写入”或“重开必定删除本轮”。
工具副作用也不能由 `persistence` 反推。[S5][S9]

## 30.5 UnsavedLoop

复用原有结构，不增加本地持久化 journal：

```rust
pub struct UnsavedLoop {
    pub turn: TurnRef,
    pub prompt: String,
    pub requests: Vec<LiveRequest>,
    pub result: TurnResultView,
    pub event_gap: bool,
}
```

这里 `Unsaved` 的产品含义是“Agent 没有确认保存成功”，不是磁盘取证结论。
`turn.wait` 的 RPC 结果不携带完整 `report.appended`；上述 requests 只是收到的 live 输出，可能不完整。

固定提示建议：

```text
This turn finished, but the Agent did not confirm saving it.
The session is blocked. Tool side effects may already exist.
Closing releases this result; reopening reads whatever the Store can recover.
```

有 event gap 时追加 `Some live output may be missing.`。
不能把完整结果恢复当成重开保证。

## 30.6 Blocked 后的完成结果保留（本次核心修订）

后端已经保证：一次被拒绝的新 send 不会清掉原 blocked Turn 的 completion。[S2]

TUI 必须满足：

- 收到 `session_blocked` 只结束触发它的请求，不清除原 `LiveLoop` / `UnsavedLoop` / `WaitTurn(old_ref)`。
- `blocked` 的 `active_loop` 可能为 null；用此前保留的 TurnRef，而不是从 state 临时重建。
- 普通流程仍在 send 成功后立即注册 wait；不要为了使用该修复改成迟延注册。
- 用户手动刷新完成结果时，若无同 Turn 的 wait 在途，可再次发送 `turn.wait(old_ref)`。不要轮询、退避重试或建立历史查询框架。
- 同一 Turn 重复响应必须幂等：不重复追加卡片、累计 Usage、提交 History 请求或改变已经确认的保存状态。
- 仅在显式 close/shutdown 或正常进入后续生命周期时释放本地引用；本保证不跨 reopen/进程重启。

为去重可复用既有 outcome/result，确有需要时在 SessionView 保留一个 `last_result: Option<TurnResultView>`。
不要增加无界历史 TurnHandle/结果 registry。

## 30.7 Internal Blocked

`block_reason=internal` 不一定有正常的 TurnResult。Agent worker panic/异常退出可能让 wait 返回 RPC internal error。[S2][S8]

TUI 应保留可见 live 内容，标成“结果未确认”，而不是编造 `completed`、`cancelled` 或 `persistence=failed`。
允许显式 close；close 自身返回 internal error 时也可能已经回收了 Session/task，处理见第 52 节。

## 30.8 不自动恢复或重放

不自动执行：

```text
重试 append
close/open
重新发送 Prompt/Steer
重新执行工具
修改 history.jsonl
将 Live 输出伪装成已存 History
```

Blocked 时只允许查看历史/临时输出、对保留的 TurnRef 取结果、显式关闭/重开或退出。
再次 wait 是读同一 completion，不是再次执行或重试保存。

---

# 31. Session Status UI

## 31.1 Idle

```text
Idle
Composer Prompt
```

## 31.2 Running

根据 Loop status：

```text
Starting       Starting…
RunningModel   Thinking…
RunningTools   Running tools…
```

Composer：

```text
Steer
```

## 31.3 WaitingForInput

```text
Waiting for unsupported interaction
```

Composer disabled。

显示：

```text
This TUI version does not implement approval/tool input.
Press Esc to cancel.
```

## 31.4 Finishing

含义：

```text
Runtime结束或正在结束
Agent正在持久化和完成
```

UI：

```text
Saving…
Composer disabled
```

## 31.5 Blocked

```text
Blocked · persistence
Blocked · internal
```

红色状态。

---

# 32. Restart、尾行修复与 Reopen

## 32.1 没有 Active Loop 恢复

Runtime 没有 live Loop load/resume。重新打开 Session 是 Agent 读取现存 record/history，之后如需执行再启动新 Loop。

若进程在 Loop 结束前退出，通常没有完整 LoopRecord；若退出发生在追加/收尾期间，文件可能已有完整记录或尾部片段。
不能仅凭 UI 未收到 wait 响应就断言“整个 Loop 必然丢失”，也不能承诺它已保存。[S5][S9]

## 32.2 尾部不完整行的 best-effort 修复

Agent 可以处理 `history.jsonl` 最后的不完整行，尽力保留此前可读记录。
这不是事务回滚，不恢复丢失 token/工具结果，不恢复待处理 Steer，更不是 Runtime v0.3 的 unfinished Turn repair。

TUI 不实现任何文件修复算法。重新 open 失败时保留错误，由用户决定后续操作。

## 32.3 Graceful Exit

正常退出显式发送 `agent.shutdown`，让 Agent 取消活动 Loop、等待 worker 和追加收尾。
继续接收原 wait 的结果；只有收到相应 `persistence=persisted` 才能说该轮得到追加确认。
shutdown 的 `{ok:true}` 本身不是所有 Loop 的保存成功清单。

当前关闭/退出取消使用 `reason=user`，不要求 `shutdown`。TUI 自己可以显示“关闭期间取消”，
但不能改写 wire outcome，也不能在重开历史时假造独立 shutdown 原因。

## 32.4 Crash 或强制 kill

显示“最后一轮结果/保存状态未确认，重开后以已加载历史为准”。
保留此次进程已收到的失败结果；不要自动重启或重跑命令。

## 32.5 旧格式与通用 Store 错误

旧 Agent v0.2 的 manifest/conversation 格式没有迁移支持，这一限制保留。
但当前 RPC `-32011 / store_error` 不区分旧格式、无效 record、读取失败或其他 Store 原因。

因此通用错误文案应为：

```text
Unable to open this session. Its data may be unavailable,
invalid, or from an unsupported format.
```

除非有额外可靠诊断，不直接说“这一定是旧版数据”。TUI 不探测磁盘、不自动迁移、不删除 Session。

---

# 33. RequestKind

```rust
pub enum RequestKind {
    Ping,
    ListModels,
    ListProfiles,
    ListSessions,

    CreateSession {
        local_id: u64,
    },

    OpenSession(SessionId),
    CloseSession(SessionId),
    DeleteSession(SessionId),
    SessionState(SessionId),

    UpdateSession {
        session_id: SessionId,
        requested_model: Option<String>,
        requested_reasoning: Option<Reasoning>,
    },

    History {
        session_id: SessionId,
        offset: usize,
        mode: HistoryLoadMode,
    },

    SendTurn {
        session_id: SessionId,
        submission_id: u64,
    },

    WaitTurn(TurnRef),
    CancelTurn(TurnRef),

    SteerTurn {
        turn: TurnRef,
        local_steer_id: u64,
    },

    Shutdown,
}
```

删除：

```text
Transcript
instance相关Request
```

---

# 34. App Event Reducer

## 34.1 单写入者

仍然只有：

```rust
App::update(AppEvent)
```

修改App状态。

## 34.2 Event顺序

必须正确处理：

```text
TurnStarted before turn.send response
RequestStarted before turn.send response
OutputDelta before turn.send response
session.update response before/after RequestStarted
turn.wait response before TurnFinished
TurnFinished before turn.wait response
SessionState Blocked before wait response
```

## 34.3 Unknown / 迟到的 Loop Event

Session 有 PendingLoopStart 时可绑定先到的 Event；SessionState 明确有相同 active loop 时可以创建内容缺失的 placeholder，并标记 event gap。

若该 loop_id 已完成，迟到的 `turn_finished`、Delta 或 State 不得重新打开输入中的旧 Loop，
也不能清除新 Loop。保存失败/内部失败的 last result 同样不能被旧 `idle` Event 覆盖成成功。
其他未知 Loop Event 只记录安全 debug，不无限创建对象。

## 34.4 错误只作用于对应请求

`App::update` 必须按 RequestId → RequestKind → TurnRef 处理错误。

```text
SendTurn(new input) → session_blocked
```

只撤销这次新输入提交，不撤销已知 `WaitTurn(old_ref)`、旧 Usage/Outcome 或 Unsaved 卡片。

建议在既有 app/state 文件里使用普通方法：

```rust
fn handle_session_blocked(&mut self, session_id: &SessionId);
fn apply_turn_result(&mut self, expected: &TurnRef, result: TurnResultView);
fn handle_session_close_result(&mut self, session_id: &SessionId, result: RpcResult);
```

这些是职责建议，不要求新增 Service、通用状态机或特定方法名。结果应用先核对 expected TurnRef，重复结果不重复记账。

# 35. Pi 风格布局

保留 fullscreen alternate-screen：

```text
┌──────────────────────────────────────────────────────┐
│                                                      │
│  History / Live Loop Scroll View                     │
│                                                      │
│  User Prompt Card                                    │
│  Assistant Request 0                                 │
│  Tool Cards                                          │
│  Steering Card                                       │
│  Assistant Request 1                                 │
│                                                      │
├──────────────────────────────────────────────────────┤
│  Status                                              │
├──────────────────────────────────────────────────────┤
│  Composer / Selector                                 │
├──────────────────────────────────────────────────────┤
│  workspace • session                                 │
│  state / persistence         model • reasoning • rev │
└──────────────────────────────────────────────────────┘
```

## 35.1 Layout

```rust
let [history, dock] = Layout::vertical([
    Constraint::Min(1),
    Constraint::Length(dock_height),
]).areas(frame.area());
```

Dock：

```rust
let [status, content, footer] = Layout::vertical([
    Constraint::Length(status_height),
    Constraint::Length(content_height),
    Constraint::Length(2),
]).areas(dock);
```

## 35.2 最小终端

```text
60 x 16
```

更小时显示安全提示。

---

# 36. Pi 风格视觉

## 36.1 背景

Dark：

```text
pageBg = #18181e
cardBg = #1e1e24
```

## 36.2 User Prompt

```text
背景 #343541
前景 #d4d4d4
水平padding 1
垂直padding 1
```

## 36.3 Steering Card

保持 Pi 风格紧凑 User 卡片：背景 `#2b303b`、左侧 `↪`、文本 `#c5c8c6`。
状态文案按证据区分：

```text
↪ accepted · awaiting history
↪ recorded in history
↪ not included in history
↪ save status unconfirmed
```

不根据后续 RequestStarted 直接显示某条 Steer `applied`。
颜色、间距和折叠交互不因此重写。

## 36.4 Assistant

```text
无背景
前景 #d4d4d4
水平padding 1
```

## 36.5 Reasoning

```text
#808080
Italic
可折叠
```

## 36.6 Tool

Pending：

```text
#282832
```

Success：

```text
#283228
```

Error/Denied/Cancelled：

```text
#3c2828
```

## 36.7 Persistence Failed

```text
背景 #3c2828
边框 #cc6666
标题 UNSAVED
```

## 36.8 Status

```text
⠋ Thinking…
⠙ Request 2…
⠹ Running tools…
⠸ Steering queued…
⠼ Saving…
⠴ Cancelling…
```

## 36.9 Composer Border

使用当前有效配置：

### Idle

```text
SessionInfo.reasoning
```

### Running

优先：

```text
latest RequestStarted.reasoning
```

### Pending update

右侧显示：

```text
next: high · rev 3
```

不提前改变当前Request颜色。

---

# 37. Theme

保留旧 Dark Palette：

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
steeringBg       #2b303b
toolPendingBg    #282832
toolSuccessBg    #283228
toolErrorBg      #3c2828
pageBg           #18181e
cardBg           #1e1e24

thinkingDisabled #505050
thinkingAuto     #8abeb7
thinkingLow      #5f87af
thinkingMedium   #81a2be
thinkingHigh     #b294bb
```

Light主题保留。

不复制Pi Logo或品牌。

---

# 38. Composer

## 38.1 Prompt模式

Placeholder：

```text
Type a message…
```

Enter：

```text
turn.send
```

## 38.2 Steer模式

Placeholder：

```text
Steer current turn…
```

Enter：

```text
turn.steer
```

Footer说明：

```text
applies at next request boundary
```

## 38.3 Disabled状态

Waiting：

```text
Unsupported interaction — Esc to cancel
```

Finishing：

```text
Saving turn…
```

Blocked：

```text
Session blocked
```

## 38.4 输入上限

```text
256 KiB UTF-8 bytes
```

超过时：

```text
不发送
显示Notice
```

实时显示：

```text
bytes / 262144
```

仅接近上限时显示。

## 38.5 多行与IME

保留：

```text
Enter submit
Shift+Enter/Ctrl+J newline
Bracketed paste
CJK/Emoji width
hardware cursor
history
undo/redo
```

---

# 39. Model Selector

## 39.1 有Active Session

标题：

```text
Update session model
```

说明：

```text
Applies at the next model request.
The current tool batch is not interrupted.
```

选择后：

```text
session.update
```

## 39.2 无Active Session

更新New Session Draft。

## 39.3 Compatibility

显示每个Model：

```text
id
context window
tool support
supported reasoning
```

若当前Session reasoning不被新Model支持：

```text
不提交update
自动打开Reasoning Selector
用户显式选择
```

不是静默降级。

---

# 40. Reasoning Selector

## 40.1 Active Session

选择后：

```text
session.update
```

## 40.2 Running Loop

显示：

```text
Will apply at next request boundary.
```

## 40.3 Update Result

`active_revision=Some(n)`：

```text
Pending rev n
```

收到对应 RequestStarted 后：

```text
Applied
```

`None`：

```text
Saved for next turn
```

---

# 41. Session Selector

## 41.1 Status

```text
○ idle
◉ running
◌ finishing
! blocked
  unloaded
```

## 41.2 Row

```text
→ Title                 deep · high       5m
  ~/project
```

## 41.3 Blocked

红色：

```text
! persistence
```

## 41.4 Select

```text
session.open
→ session.state
→ session.history
```

---

# 42. Footer

## 42.1 第一行

```text
左：workspace
右：title / session id
```

## 42.2 第二行

Idle：

```text
Idle                             deep • high
```

Running：

```text
Request 2 · rev 1                deep • high
```

Pending update：

```text
Request 2 · rev 1      next: fast • low · rev 2
```

Finishing：

```text
Saving…                          fast • low
```

Blocked：

```text
BLOCKED · persistence            unsaved turn
```

## 42.3 Usage

Turn wait完成后可短暂显示：

```text
3 requests · 2 tools · 8.2k in · 1.1k out
```

缺失Token字段不显示。

不计算价格。

---

# 43. Slash Commands

必须实现：

```text
/new
/resume
/sessions
/model
/reasoning
/cancel
/theme dark
/theme light
/clear
/help
/logs
/quit
```

不需要 `/steer`：

```text
Running时Composer Enter就是Steer
```

不实现：

```text
!command
@file
/fork
/branch
/compact
/plugin
/mcp
```

## 43.1 `/model`

有Active Session：

```text
打开Update Model Selector
```

无Active：

```text
New Session Model
```

## 43.2 `/reasoning`

同上。

## 43.3 `/cancel`

仅Active Loop可用。

---

# 44. Keybindings

| Key | 行为 |
|---|---|
| Enter | Idle发送Prompt；Running发送Steer |
| Shift+Enter | 换行 |
| Ctrl+J | 换行 |
| Esc | Running取消；Selector关闭 |
| Ctrl+L | Model Selector / Update |
| Shift+Tab | Reasoning Selector / Update |
| Ctrl+R | Session Selector |
| Ctrl+O | Tool折叠 |
| Ctrl+T | Reasoning折叠 |
| PageUp/Down | History滚动 |
| End | Follow Tail |
| F1 | Help |
| Ctrl+C | 清空；空时二次退出 |
| Ctrl+D | Idle空输入退出 |

Running时Composer不再禁用。

---

# 45. Event Gap

## 45.1 标记

任一Event：

```text
meta.dropped_before > 0
```

设置：

```text
SessionView.event_gap=true
LiveLoop.event_gap=true
```

## 45.2 Request缺失

如果RequestStarted丢失：

```text
LiveRequest config unknown
```

Footer：

```text
request N · config unknown
```

## 45.3 Persisted后

增量History加载完成：

```text
清除event_gap
```

## 45.4 Persistence Failed

当前 loaded History 不合并失败追加的结果，因此不能在此时据此补齐该 Loop 的 live 缺口。
保留 gap 和“临时输出可能不完整”提示；不要把 repeated wait 当作 full output replay。

显式 close/reopen 后，如 Agent 实际加载到了完整记录，可以用新 History 替换临时内容；
没有对应记录则保留“未恢复”说明。这个读取结果优先于 TUI 对失败写入的推测。

# 46. Approval/Interaction

本阶段仍不实现Approval UI。

## 46.1 推荐Profile

```toml
approval = "auto"
```

## 46.2 收到Interaction

显示：

```text
This Agent session requires an interaction unsupported by this TUI.
Press Esc to cancel.
```

可以显示：

```text
tool name
prompt
risk
```

但不发送answer。

## 46.3 不自动允许

严禁：

```text
自动AllowOnce
自动Deny
```

---

# 47. Scrolling

保留：

```rust
pub struct ScrollState {
    pub offset: usize,
    pub follow_tail: bool,
    pub new_content: bool,
}
```

新内容：

- follow_tail=true → 自动滚动；
- 用户上滚 → 不跳底；
- 显示 `↓ new output`；
- End恢复。

后台SessionEvent只更新状态，不改变当前scroll。

---

# 48. Markdown与渲染

## 48.1 Stored Assistant

来自 `session.history` 的 Assistant 使用完整 Markdown 缓存。
“Stored” 表示来源是 Agent 已存历史视图，不表示崩溃安全级别。
来自 Live/Unsaved 的内容继续保留独立来源标识。

## 48.2 Live Text

Streaming使用：

```text
plain wrapped text
```

每100ms或完成后解析Markdown。

## 48.3 Reasoning

plain gray italic。

## 48.4 Steering

不解析复杂Markdown。

作为紧凑User text渲染。

## 48.5 History缓存

缓存key：

```text
item index / live revision
width
theme
expanded state
```

---

# 49. App启动流程

```text
parse args
→ spawn Agent
→ enter terminal
→ agent.ping
→ validate 0.3.x
→ model.list
→ profile.list
→ session.list
→ Ready
```

可以并发发送三条list请求。

没有Session：

```text
Use /new or /resume
```

---

# 50. Session创建

仍支持：

```text
workspace
profile
model
reasoning
title
```

Create成功：

```text
设置active
请求state
请求history offset0
```

SessionInfo是设置事实来源。

---

# 51. Session 打开与 record 校验

```text
session.open
→ 成功后接纳 SessionInfo
→ session.state
→ session.history(offset=0)
```

Agent SessionRecord 复制保存 system prompt、tools、model、reasoning、approval、max tool rounds；
Profile 后续变更/删除不重新定义旧 Session。这一结构没有变化。

## 51.1 本次新增的后端校验

Agent 已检查：[S4]

- model ID 能构造合法 ModelRef；
- system_prompt 非空、在上限内，控制字符仅允许换行与 Tab；
- tools 只能是已知工具且不得重复；
- 既有 record format、字段长度等规则继续生效。

这不是让 TUI 重写配置验证器。TUI 只使用 discovery 返回的配置 ID，最终以 Agent 的判断为准。
不要把 `vendor/model-name:v1.0` 之类合法 ID误用字母数字正则拒绝；显示字符串按原 ID 传回。

## 51.2 列表与显式打开的差别

无效 record 可以从 `session.list` 中被跳过；列表成功不证明所有磁盘数据完好，
也不保证每个已列出的 Session 的 history 一定可以打开。

显式 open 无效数据会返回 Store 错误。TUI 显示错误、保留当前健康 Session，
不自动补空 system prompt、不去重/删 Tool、不自动创建替代 Session。
列表不含某条记录也不能被解释成“它已被删除”；不得据此删除当前本地失败结果。

TUI 不读取或修改 session.json/history.jsonl。通用 Store 错误不要伪造具体 subtype，见第 32.5 节。

---

# 52. Session 关闭

## 52.1 正常关闭

若原 Loop 仍在运行，保留/先注册其 wait，然后发送 `session.close`。
关闭期间用本地 pending CloseSession 请求禁用新输入，UI 可显示 `Closing…`；
这不是新增 wire `SessionStatus::Closing`。

Agent 使用 user cancellation 路径并等待 worker。继续接收 wait，即使它晚于其他控制响应。
close 成功只确认关闭清理，不代替对 `TurnResult.persistence` 的检查。

## 52.2 Blocked / 保存未确认

关闭前提示会释放当前进程保留的 completion 和临时展示；重开只加载 Store 能读取的记录，不能保证完整恢复。
用户明确确认后再关闭。不要显示“重开必丢弃该轮”，因为失败追加的磁盘结果可能不确定。

## 52.3 Close 返回 Internal

当前实现可能先移除 Session 并完成/尝试回收 worker，随后返回 Internal（例如 worker panic）。[S2][S8]

TUI 不得假设任何 close error 都表示“Session 必然仍 loaded”。保留错误说明和结果展示，
在连接仍可用时发一次 `session.list` 或 `session.state` 确认；`session_not_loaded` 表示已卸载。
这只是显式操作后的读取核对，不是自动重开/无限重试。

`session_closed` Event 仍为 best-effort 观察，不可独自证明保存成功。
正常退出最终仍调用 `agent.shutdown`。

---

# 53. 多Session

后台Session可Running。

每个Session：

```text
最多一个LiveLoop
```

Event按SessionId更新。

切换Session不cancel。

Agent shutdown关闭全部。

---

# 54. 安全与日志

## 54.1 TUI不执行Shell

所有Tool由Agent执行。

## 54.2 Agent stderr

仅显示最近200行。

不落盘。

## 54.3 TUI tracing

不记录：

```text
Prompt
Steer text
Assistant Text
Reasoning
ToolResult
RPC raw frame
```

可记录：

```text
method
request id
session id
loop id
request index
error kind
frame bytes
```

## 54.4 Auto Tools

Help/Footer说明：

```text
Tools may run automatically.
Bash is not sandboxed.
```

---

# 55. 代码迁移文件清单

## 55.1 `src/protocol.rs`

全量替换：

```text
instance_id DTO
Transcript DTO
Terminal DTO
old SessionState
old TurnRef
```

实现：

```text
LoopId
ConfigRevision
SessionStatus新值
LoopStatus
SessionUpdate
HistoryPage
HistoryItem
RequestStarted
TurnPersistence
TurnResultView
SteerError
```

## 55.2 `src/rpc.rs`

保留进程/读写结构。

修改：

```text
supported Agent 0.3.x
MAX_RPC_FRAME_BYTES=32MiB
RequestKind
session.history
session.update
turn.steer
turn.wait结果
```

删除：

```text
session.transcript
旧instance fields
```

## 55.3 `src/app.rs`

重写：

```text
PendingLoopStart
LiveLoop多Request
session.update
turn.steer
persistence failed
blocked
history reconcile
```

## 55.4 `src/state/session.rs`

新增：

```text
config_update
unsaved_loop
blocked
new statuses
```

## 55.5 `src/state/history.rs`

替代：

```text
transcript.rs
```

实现offset分页和History projection。

## 55.6 `src/state/live_loop.rs`

替代旧flat LiveTurn。

## 55.7 `src/state/request.rs`

实现per-request state。

## 55.8 `src/ui/history.rs`

按History item渲染。

不依赖Terminal entry。

## 55.9 `src/ui/tool.rs`

删除argument/path/command假设。

## 55.10 `src/ui/composer.rs`

增加Prompt/Steer模式。

## 55.11 `src/ui/footer.rs`

增加：

```text
request index
config revision
pending update
finishing
blocked
persistence
```

## 55.12 `src/ui/selector.rs`

Model/Reasoning应用到：

```text
session.update
```

## 55.13 `src/command.rs`

更新 `/model`、`/reasoning` 语义。

## 55.14 r2 的最小改动位置

已完成前版迁移的项目无需重做 Phase 1～6。只检查以下现有文件：

| TUI 文件 | r2 修改 |
|---|---|
| `src/app.rs` | blocked/send error 不清旧 completion；重复 wait 幂等；迟到旧结果不影响新 Loop；close错误后单次核对。 |
| `src/state/session.rs` | 保留已有 TurnRef/失败结果；区分 block_reason；可用单个 last_result 支持展示，不增加历史 registry。 |
| `src/state/history.rs` | 用实际连续加载数推进 offset；reopen 从0加载。 |
| `src/state/live_loop.rs` / `src/state/request.rs` | 保持同 Loop多Request；不把后续请求事件当作逐条Steer回执。 |
| `src/ui/status.rs` / `src/ui/error.rs` / `src/ui/footer.rs` | 使用“保存未确认”等准确提示；取消原因不必是shutdown。 |
| `src/rpc.rs` / `src/main.rs` | shutdown期间继续读取；强杀标记未确认；保留旧wait的响应分发。 |
| `tests/persistence_flow.rs` / `tests/control_flow.rs` | 加入 blocked结果保留、同Loop换模、关闭时user取消、无效record错误用例。 |
| 文档、fixtures | Agent SHA更新；删除事务/强持久性/重开必丢的错误承诺；保留现有wire字段。 |

`src/protocol.rs` 本次没有必须新增的方法/字段；仅同步 fixture 来源和注释。
不要把 Agent 已完成的 Store/Write 修复再复制到 TUI。

## 55.15 文档

新增：

```text
migration-v0.1-to-v0.2.md
backend.md
protocol.md
```

---

# 56. 必须删除的旧代码/概念

```text
SessionInfo.instance_id
TurnRef.instance_id
TurnRef.turn_id
TranscriptPage
ConversationSeq cursor
TurnTerminal history block
SessionStatus::Closing
Session model immutable文案
Model selector创建新Session的唯一语义
Running Composer disabled
Steering unsupported文案
Restart repairs unfinished turn文案
Tool arguments/path/command renderer
Core durable Conversation假设
```

搜索门禁：

```bash
rg "instance_id|session\\.transcript|ConversationSeq|TurnTerminal|Closing|hot switch unsupported"
```

逐项人工确认引用含义：旧 wire DTO / 旧 RPC 调用不得保留；migration、负向测试和一般 UI 文案可解释旧概念。
例如本地 `Closing…` 提示是合法的，禁止的是旧 wire `SessionStatus::Closing`。
不要用宽泛 grep 把正确文档或合法关闭状态提示当成构建失败。

---

# 57. Protocol Fixtures

必须基于Agent 0.3真实进程输出生成并脱敏。

## 57.1 SessionState Running

覆盖：

```text
active_loop
request_index
config_revision
model
pending_interaction null
```

## 57.2 SessionState Blocked

覆盖：

```text
status=blocked
block_reason=persistence
```

## 57.3 RequestStarted

覆盖：

```text
request_index
config_revision
model
reasoning
```

## 57.4 History

至少包含：

```text
prompt
assistant request0 + tool_calls
tool_result
steering
assistant request1
```

## 57.5 TurnWait

覆盖：

```text
persisted completed
persisted cancelled
persisted failed
persistence failed
```

## 57.6 SessionUpdate

覆盖：

```text
active_revision number
active_revision null
```

---

# 58. 单元测试：Protocol

必须覆盖：

```text
Agent 0.3版本接受
Agent 0.2拒绝
Agent 0.4拒绝
SessionInfo无instance_id
TurnRef使用loop_id
SessionState五种状态
History四种Item
RequestStarted
TurnResult persistence
Usage optional fields
未知Response字段忽略
未知Reasoning拒绝
```

---

# 59. 单元测试：History Projection

## 59.1 单Request

```text
Prompt
Assistant
```

## 59.2 Tool Loop

```text
Prompt
Assistant tool call
ToolResult
Assistant final
```

Request index分别0、1。

## 59.3 Steering

```text
Prompt
Assistant req0
Steering A
Steering B
Assistant req1
```

## 59.4 Missing ToolCall

ToolResult无call：

```text
显示orphan tool
不panic
```

## 59.5 ToolCall无Result

显示：

```text
no result
```

## 59.6 Summary

显示折叠Summary。

## 59.7 无Terminal

不得创建Completed Terminal。

---

# 60. App Flow 测试

## 60.1 Bootstrap

```text
ping/list模型/profile/session
Ready
```

## 60.2 Send

```text
turn.send
Event先到
Response后到
绑定同Loop
立即wait
```

## 60.3 多Request

```text
request0
tool
request1
final
```

Text不错误合并到一个Request。

## 60.4 Steer

Model req0 阻塞时发送 Steer，验证 RPC ok 后显示“已接受，待历史确认”。
即使 req1 started，仍不虚构逐条 applied acknowledgement；History 出现对应 `kind=steering` 后才替换成已存卡片。
加入 request_started 先于 steer response 到达的顺序用例，不丢输入也不误判应用时点。

## 60.5 Multiple Steer

两条按FIFO进入History。

## 60.6 Queue Full

保留Composer文本。

## 60.7 Update Idle

```text
session.update(model/reasoning)
→ active_revision=null
→ 下一 Turn 的 request0 使用新配置
```

不得误将 null 解释成更新失败，也不得从 null 单独推断之前 Loop 的具体结束时刻。

## 60.8 Update Running Model：同一 Loop 的确定性验收

必须覆盖后端本次新增的测试场景，而不只是“更新完再发下一 Turn”：[S3]

```text
turn.send → Loop L
Model A 的 request0 在 gate 处暂停
session.update(model=B) → active_revision=r
gate释放 → A返回read ToolCall → ToolResult
同一 Loop L 的 request1 由 Model B 回答
turn.wait → requests=2、tool_rounds=1、final_config_revision=r
session.history → Prompt / A Assistant / ToolResult / B Assistant
```

断言 session_id 和 loop_id 不变；A/B 各请求一次；旧 Request标签不被改写；没有 cancel+send模拟。
再覆盖 RequestStarted(r) 先于 update response 到达，响应处理必须使用已知事件确认，而非永远 pending。

## 60.9 Update Running Tools

```text
当前Tool属于revision0
下一Request属于revision1
```

## 60.10 Update + Steer

```text
更新config
发送steer
下一Request同时采用新config和steer
```

## 60.11 Update不延长

```text
只update
当前final直接结束
下一Turn使用新config
```

## 60.12 Cancel

```text
cancel
wait cancelled persisted
History对齐
```

## 60.13 Persistence Failed

```text
wait: outcome completed + persistence failed
→ Session blocked/persistence
→ 保留Unsaved临时展示
→ 不清event gap
→ 新send/update禁用
```

不得把失败追加解释为磁盘必为空；reopen fixture分别覆盖没有新行、尾部不完整行、完整行可读三种情况。
TUI只采用Agent结果，不执行修复算法。

## 60.14 Event Gap

Persisted后History修复。

## 60.15 Agent Crash

未收到 wait completion 时显示“结果/保存状态未确认”，不伪造完成结果、不承诺全丢或全存。
已收到的成功/失败 result 不因 EOF 被改写；强制退出必须恢复终端。

## 60.16 Blocked 后错误 send 与重复 wait（新增）

精确复现：

```text
send1 → L1
state → blocked / persistence
send2 → -32004 session_blocked
wait(L1) → outcome completed、persistence failed
wait(L1) → 相同TurnResult
```

断言 TUI 没有清掉 L1、没有变成 turn_not_found、没有重复累计 Usage/渲染卡片，也没有自动close/open。
普通 UI 不主动制造 send2；此处通过 reducer/fixture 模拟竞态或先前排入的请求。

## 60.17 Internal Blocked 与关闭（新增）

wait 可以返回 RPC internal error；不得把它包装成正常保存失败结果。
close 也可能返回 Internal但已卸载。单次读取核对后正确显示closed/unknown，不无限重试，不隐藏错误。

## 60.18 Shutdown 取消原因与收尾（新增）

有 wait 在途时发送shutdown，模拟 wait(cancelled, reason=user) 先于最终shutdown响应。
断言它合法、只结束一次；没有强求reason=shutdown；readers持续工作至收尾。
再模拟 persistence=failed + shutdown ok，仍提示保存失败。

## 60.19 无效 SessionRecord（新增）

列表包含健康项但不含坏record；显式open坏ID返回store_error。
断言健康Session继续可用，TUI不推断旧格式、不修改文件、不偷偷补默认配置或删除坏项。
这类文件破坏注入由Agent测试拥有；TUI的普通测试用RPC fixture即可。

## 60.20 同步门限（新增）

所有gate/Notify/子进程等待必须有测试超时并在超时后清理任务。
不能用“某个很短sleep大概率结束”替代事件条件，也不能等待永远不会到达的gate。
这不改变Write Tool功能，是本次上游测试修复对应的测试质量约束。[S6]

# 61. Agent E2E 测试

## 61.1 目标

使用最新真实：

```text
minicore-agent dev binary
```

后端可以使用loopback OpenAI mock。

## 61.2 场景

### E2E-A：发现

```text
ping 0.3.x
models
profiles
sessions
```

### E2E-B：基本

```text
create
send
wait
history
```

### E2E-C：Tool

```text
read
第二Request
persisted
```

### E2E-D：Steer

```text
send
steer
RequestStarted index增加
history kind=steering
```

### E2E-E：Update

```text
session.update during loop
active_revision
RequestStarted新config
```

### E2E-F：Blocked

Agent test seam模拟append failure。

如果生产Binary无注入方式：

```text
作为Agent仓库集成测试完成
TUI用fixture/App测试
```

不要为TUI向Agent增加debug RPC。

## 61.3 r2 回归来源与 E2E 范围

上游已有对应测试，可用于理解期望，但不复制其私有注入接口到TUI：

```text
blocked_send_does_not_discard_previous_turn_result
blocked_send_does_not_discard_previous_turn_result_rpc
running_model_update_applies_to_next_request_in_same_loop
invalid_session_json_skipped_by_list_and_fails_open
```

TUI E2E 至少跑通同Loop换模和正常关闭；写入失败/worker panic由TUI fixture覆盖，
同时记录上游基线CI证据。无真实二进制或凭据时写明“未执行”，不能宣称这些TUI验收项通过。
不向生产Agent添加故障注入RPC。

## 61.4 环境

```text
MINICORE_AGENT_BIN
MINICORE_AGENT_CONFIG
```

测试默认ignored。

---

# 62. Render Snapshots

保留旧Pi视觉快照并新增：

```text
live multi-request
steering queued
steering recorded / unconfirmed
model update pending
request_started config change
finishing/saving
blocked persistence
unsaved turn
history steering
history tool without arguments
event gap unsaved
blocked repeat wait（不重复显示）
close cancelled reason=user
store_error不误诊旧格式
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

---

# 63. 性能

## 63.1 Render

```text
最大30fps
Spinner 10fps
Idle不持续draw
```

## 63.2 Delta

每条Delta更新State，不每条draw。

## 63.3 Multi Request

LiveRequest最多受Runtime Request/tool round limits约束。

不额外设置小限制截断正确数据。

UI render预览有界。

## 63.4 History

每页20。

逐页渲染。

不一次请求100大项。

## 63.5 History Cache

item index稳定。

缓存：

```text
index
width
theme
expanded
content revision
```

---

# 64. Migration 实施阶段

## Phase 0：冻结旧视觉基线

提交：

```text
test(ui): preserve the pi-style visual baseline
```

完成：

- 旧视觉snapshot备份；
- Terminal/Theme/Composer测试；
- 不修改后端状态。

## Phase 1：Protocol Reset

提交：

```text
refactor(protocol): target minicore-agent v0.3
```

完成：

- Agent 0.3 version gate；
- 新DTO；
- IDs；
- Session state；
- History；
- TurnResult；
- RequestStarted；
- Update/Steer；
- protocol fixtures。

旧Protocol必须删除。

## Phase 2：State Reset

提交：

```text
refactor(state): model sessions as one-shot loops and requests
```

完成：

- HistoryState；
- LiveLoop；
- LiveRequest；
- PendingSteer；
- PendingConfigUpdate；
- UnsavedLoop；
- Blocked。

## Phase 3：History Migration

提交：

```text
feat(history): replace transcript reconciliation with session history
```

完成：

- offset pagination；
- projection；
- no terminal；
- tool without args；
- persisted reconciliation。

## Phase 4：Runtime Control UI

提交：

```text
feat(control): support steering and request-boundary session updates
```

完成：

- running Composer steer；
- `turn.steer`；
- `session.update`；
- revision UI；
- selector语义。

## Phase 5：Persistence UI

提交：

```text
feat(session): surface finishing blocked and unsaved turn states
```

完成：

- persistence failed；
- blocked；
- unsaved；
- safe close/reopen说明。

## Phase 6：Pi Visual Adaptation

提交：

```text
feat(ui): render multi-request loops steering and persistence states
```

保留Pi布局，新增最新状态。

## Phase 7：E2E 与文档

提交：

```text
test: verify agent v0.3 steering update and persistence flows
docs: replace the obsolete v0.1 backend contract
```

---

## 64.1 已完成上一版迁移的 r2 路径

不重新执行整套重构，只用以下小提交：

```text
1. docs: pin agent bugfix baseline and correct persistence contract
2. fix(state): preserve blocked completion and handle repeat waits idempotently
3. test: cover same-loop update, store rejection, and shutdown result ordering
4. docs: record backend compatibility and remaining verification gaps
```

如果现有TUI代码已经符合某项，只补验证和文档，不制造空重构。
样式、终端库版本、布局和无审批范围不因本次Agent bugfix改变。

---

# 65. 不应修改 Agent/Runtime

本阶段不要向Agent提PR来增加：

```text
ToolCall arguments
historical Loop outcome RPC
byte-bounded history pagination
Event replay
persistent unsaved loop
TUI-specific labels
```

这些可能是未来合理能力，但不是当前迁移阻塞项。

先按现有协议完成TUI。

---

# 66. 已知能力缺口

## 66.1 Historical Outcome

Session reopen后History不包含：

```text
Completed/Cancelled/Failed terminal
```

TUI只能显示History内容。

当前进程中的TurnResult可显示Outcome。

## 66.2 Tool Arguments

不能显示命令/path。

## 66.3 Persistence Failed 的结果与内容恢复

Agent 没有 retry-persist RPC。它保留当前 loaded Session 中 blocked Turn的完成结果，可再次wait，
但wire TurnResult没有完整 appended History，因此重复wait不能恢复丢失的文本/工具内容。
显式重开按磁盘可读记录恢复；不能预先断言该失败轮次的文件状态。

## 66.4 Approval

TUI不实现。

## 66.5 Compaction

Agent明确未实现。

## 66.6 Old Store Migration 与错误分类

旧格式没有迁移支持，保持不变。`store_error` 是一般错误分类，不是 `unsupported_format` 的独占证据。
TUI必须显示一般读取/校验失败，不假造新增错误码，也不通过扫描用户文件补充推断。

## 66.7 结果保留范围

blocked completion的保留仅限仍loaded的Session与尚存的handle/completion。
不是跨Session重新打开的历史结果接口。未保存正文、逐条Steer应用ID、旧Loop terminal列表仍不可通过新方法获取。

# 67. README 必须说明

```text
minicore-tui v0.2 / Spec r2 的测试后端为指定 Agent b2e2393…、Runtime 87f3cf9…
协议范围0.3.x；同样0.3.0不能证明包含本次bugfix
只通过stdio JSON-RPC；Pi风格视觉范围不变
支持turn.steer与session.update，请求边界生效，当前工具批次不被更新打断
一次Loop可有多个Request；真实采用的Model/Reasoning来自RequestStarted/History
Steer的ok只表示接受，没有逐条应用回执
History按offset分页；工具arguments不在RPC中
persisted只确认Agent当前进程的追加成功，不承诺事务、fsync或崩溃不丢
追加失败阻塞Session；误发新send不清掉原completion
重复wait读相同结果，不会重新执行或重新保存
失败追加可能留下部分或完整行，重开以Agent实际读取为准
不完整尾行best-effort修复不等于恢复Active Loop
正常退出显式agent.shutdown并排空已注册wait结果
close/shutdown取消当前Loop可报告reason=user
shutdown成功不是所有Loop保存成功证明
坏record在列表中可被跳过；显式open失败不被静默修复
通用store_error不直接诊断为旧格式
没有审批UI、Compaction、Plugin、MCP、Subagent或自动恢复
Bash不是Sandbox，工具副作用与History保存不是事务
```

---

# 68. 验收矩阵

## 68.1 基线

| ID | 验收 |
|---|---|
| MIG-001 | 固定Agent实际HEAD |
| MIG-002 | 固定Runtime实际HEAD |
| MIG-003 | Agent 0.3.x version gate |
| MIG-004 | Agent 0.2拒绝 |
| MIG-005 | Agent 0.4拒绝 |
| MIG-006 | 不依赖Agent Rust crate |
| MIG-007 | 不依赖Runtime Rust crate |
| MIG-008 | 无双协议兼容层 |

## 68.2 旧语义删除

| ID | 验收 |
|---|---|
| MIG-009 | 无instance_id |
| MIG-010 | 无session.transcript |
| MIG-011 | 无ConversationSeq |
| MIG-012 | 无durable TurnTerminal DTO |
| MIG-013 | 无SessionStatus Closing |
| MIG-014 | 无unfinished Turn repair文案 |
| MIG-015 | 无Model不可变文案 |
| MIG-016 | 无Steering unsupported文案 |
| MIG-017 | 无Tool argument推断 |

## 68.3 Protocol

| ID | 验收 |
|---|---|
| MIG-018 | TurnRef使用loop_id |
| MIG-019 | SessionInfo新shape |
| MIG-020 | SessionState五状态 |
| MIG-021 | LoopState解析 |
| MIG-022 | ConfigRevision解析 |
| MIG-023 | RequestStarted解析 |
| MIG-024 | OutputDelta request_index |
| MIG-025 | Tool Event request_index |
| MIG-026 | History Page解析 |
| MIG-027 | History四种Item |
| MIG-028 | TurnResult persistence |
| MIG-029 | Usage optional字段 |
| MIG-030 | session.update |
| MIG-031 | turn.steer |
| MIG-032 | 新错误码解析 |

## 68.4 History

| ID | 验收 |
|---|---|
| MIG-033 | offset分页 |
| MIG-034 | 每页20 |
| MIG-035 | index连续校验 |
| MIG-036 | Prompt User Card |
| MIG-037 | Steering Card |
| MIG-038 | Assistant按request分组 |
| MIG-039 | ToolCall无arguments可渲染 |
| MIG-040 | ToolResult关联 |
| MIG-041 | Orphan ToolResult不丢 |
| MIG-042 | Summary可渲染 |
| MIG-043 | 不创建fake Terminal |
| MIG-044 | Persisted后增量对齐 |
| MIG-045 | Reopen加载History |

## 68.5 Live Loop

| ID | 验收 |
|---|---|
| MIG-046 | 一个Session最多一个LiveLoop |
| MIG-047 | LiveLoop多个LiveRequest |
| MIG-048 | RequestStarted创建Request |
| MIG-049 | Delta按request_index |
| MIG-050 | Tool按request_index |
| MIG-051 | RequestStarted丢失时lazy request |
| MIG-052 | Event早于send response |
| MIG-053 | send后立即wait |
| MIG-054 | wait乱序Response |
| MIG-055 | current request model/reasoning显示 |

## 68.6 Update

| ID | 验收 |
|---|---|
| MIG-056 | Idle session.update |
| MIG-057 | Active session.update |
| MIG-058 | active_revision显示 |
| MIG-059 | 同Loop RequestStarted确认实际配置，支持事件先于update响应 |
| MIG-060 | current Tool保持旧revision |
| MIG-061 | 下一Request使用新revision |
| MIG-062 | active_revision null解释正确 |
| MIG-063 | Update不延长Loop |
| MIG-064 | 无静默Reasoning降级 |
| MIG-065 | Update blocked时错误 |

## 68.7 Steering

| ID | 验收 |
|---|---|
| MIG-066 | Running Composer为Steer |
| MIG-067 | Idle Composer为Prompt |
| MIG-068 | Steer成功清空Composer |
| MIG-069 | Steer失败保留Composer |
| MIG-070 | QueueFull可见 |
| MIG-071 | 多Steer FIFO显示 |
| MIG-072 | 后续RequestStarted不冒充逐条Steer applied回执 |
| MIG-073 | Persisted History含Steering |
| MIG-074 | 区分History未记录与保存未确认，不自动重发Steer |
| MIG-075 | WaitingForInput不自动steer |
| MIG-076 | Finishing不发送steer |
| MIG-077 | Steer可使final Loop继续 |

## 68.8 Persistence

| ID | 验收 |
|---|---|
| MIG-078 | turn.wait检查persistence |
| MIG-079 | Persisted后对齐History；未完成前保留Live |
| MIG-080 | Failed不假设本Loop已在内存History，也不推断磁盘必为空 |
| MIG-081 | Failed保留UnsavedLoop |
| MIG-082 | Failed显示event gap风险 |
| MIG-083 | Failed后Session Blocked |
| MIG-084 | Blocked禁用send |
| MIG-085 | Blocked禁用steer |
| MIG-086 | Blocked禁用update |
| MIG-087 | Blocked保留结果，显式close/open不保证数据一定丢失或恢复 |
| MIG-088 | 不自动重试persist |
| MIG-089 | Finishing显示Saving |
| MIG-090 | TurnFinished event非权威 |

## 68.9 Restart

| ID | 验收 |
|---|---|
| MIG-091 | Active Loop不宣称恢复 |
| MIG-092 | Graceful shutdown排空wait，取消reason=user合法 |
| MIG-093 | Agent crash Fatal |
| MIG-094 | Reopen只见persisted History |
| MIG-095 | Store错误可理解且不根据通用kind断定旧格式 |
| MIG-096 | TUI不直接迁移Store |

## 68.10 Pi UI

| ID | 验收 |
|---|---|
| MIG-097 | Fullscreen Transcript+Dock |
| MIG-098 | User背景卡 |
| MIG-099 | Steering特殊卡 |
| MIG-100 | Assistant Markdown |
| MIG-101 | Reasoning灰色斜体 |
| MIG-102 | Tool三状态背景 |
| MIG-103 | Unsaved红色卡 |
| MIG-104 | Request配置dim显示 |
| MIG-105 | Composer reasoning边框 |
| MIG-106 | Pending update显示 |
| MIG-107 | Footer双行 |
| MIG-108 | Dark主题 |
| MIG-109 | Light主题 |
| MIG-110 | 无Pi品牌资产 |

## 68.11 Terminal/RPC

| ID | 验收 |
|---|---|
| MIG-111 | Ratatui 0.29 |
| MIG-112 | Crossterm 0.28 |
| MIG-113 | Rust 1.85 |
| MIG-114 | 单stdin writer |
| MIG-115 | 单stdout reader |
| MIG-116 | stderr有界 |
| MIG-117 | 32MiB frame上限 |
| MIG-118 | Terminal正常恢复 |
| MIG-119 | Panic best-effort恢复 |
| MIG-120 | Agent shutdown超时kill |

## 68.12 测试

| ID | 验收 |
|---|---|
| MIG-121 | Protocol fixtures通过 |
| MIG-122 | App send flow |
| MIG-123 | Multi-request Tool flow |
| MIG-124 | Steer flow |
| MIG-125 | Update flow |
| MIG-126 | Update+Steer flow |
| MIG-127 | Cancel flow |
| MIG-128 | Persistence failed flow |
| MIG-129 | Blocked flow |
| MIG-130 | Event gap flow |
| MIG-131 | Reopen flow |
| MIG-132 | 60x16 snapshot |
| MIG-133 | 80x24 snapshot |
| MIG-134 | 120x40 snapshot |
| MIG-135 | CJK snapshot |
| MIG-136 | Agent 0.3 E2E存在 |
| MIG-137 | 默认测试离线 |
| MIG-138 | Linux CI |
| MIG-139 | macOS CI |
| MIG-140 | Windows CI |

## 68.13 r2 增量验收（新增，原编号不变）

| ID | 验收 |
|---|---|
| MIG-141 | Agent基线更新至b2e2393…；Runtime仍为87f3cf9…；docs/backend记录实际二进制来源 |
| MIG-142 | 版本仍0.3.0，TUI不把ping当成补丁SHA证明，不新增RPC字段 |
| MIG-143 | blocked/send2错误不清除L1 TurnRef、wait或临时输出 |
| MIG-144 | 对保留的blocked Turn重复wait幂等，不重复卡片、Usage或History副作用 |
| MIG-145 | blocked/internal的wait RPC错误不被伪造为正常persistence failed结果 |
| MIG-146 | close返回Internal但已卸载的情形可单次读取核对，不无限重试 |
| MIG-147 | shutdown期间持续读取；之前注册wait结果在最终响应前得到处理 |
| MIG-148 | close/shutdown取消reason=user被接受，不要求独立shutdown原因 |
| MIG-149 | shutdown ok不掩盖已知persistence failed；强杀标记未确认 |
| MIG-150 | persisted文案不承诺事务、fsync、掉电或端到端崩溃持久性 |
| MIG-151 | failed追加的文件可能无行/片段/完整行；TUI不作固定假设 |
| MIG-152 | 不完整尾行修复与Active Loop恢复严格区分，TUI不修写Store |
| MIG-153 | 同Loop Model A→Tool→Model B保持session_id/loop_id与requests=2、tool_rounds=1 |
| MIG-154 | RequestStarted先于update响应仍正确确认；旧Request不被改标 |
| MIG-155 | Steer无逐条应用回执；后续RequestStarted不统一标全部applied |
| MIG-156 | History从本地连续加载末尾推进，不从尚未加载的total跳页 |
| MIG-157 | 坏record列表跳过、显式open报错；健康Session继续可用 |
| MIG-158 | store_error不被固定诊断成旧格式；不自动补默认值/删Tool/改文件 |
| MIG-159 | 所有gate与子进程等待有测试超时和回收；不复制上游私有故障RPC |
| MIG-160 | 最新Agent CI和TUI实际测试分别记录；未执行的E2E/Live验证不得写通过 |

---

# 69. 测试命令

每个提交：

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
cargo tree -d
cargo tree -p crossterm
```

Snapshots：

```bash
cargo insta test
cargo insta review
```

Agent E2E：

```bash
MINICORE_AGENT_BIN=/path/to/minicore-agent \
MINICORE_AGENT_CONFIG=/path/to/agent.toml \
cargo test --locked agent_e2e -- --ignored --nocapture
```

---

# 70. 代码质量要求

## 70.1 优先

```text
具体struct
普通enum
直接match
单App::update
小型render函数
行为测试
```

## 70.2 禁止

```text
AgentV02Adapter
Protocol compatibility framework
SessionManager service graph
TuiBackend trait
Event sourcing
Redux
Plugin registry
UI-specific Agent fork
```

## 70.3 不过度兜底

直接暴露：

```text
Steer queue full
Session blocked
History too large
Invalid state
Agent version mismatch
Unsupported old store
```

不得：

```text
自动新建Session
自动换Model
自动降Reasoning
自动重跑Loop
自动恢复unsaved output
自动忽略persistence failed
```

---

# 71. 完成定义

本次迁移只有在以下条件全部满足时完成：

```text
旧Agent v0.2协议完全移除
TUI只支持Agent v0.3.x
Protocol使用loop_id且无instance_id
session.history完全替代session.transcript
History按offset分页和渲染
一个Loop支持多Request UI
RequestStarted驱动Model/Reasoning/Revision显示
session.update可在Idle和Running使用
配置只在下一个Request Boundary生效
当前Tool Batch保持旧config
Running Composer支持真正turn.steer
Steer在History中显示为Steering User item
turn.wait检查persistence
persistence failed保留UnsavedLoop并显示Blocked
Agent Event仍为best-effort
Persisted结果通过History对齐
Active Loop不跨Agent重启恢复
Tool Card不伪造arguments/path/command
Pi风格视觉、Composer、Selector和Theme保持
Rust 1.85及三平台CI通过
MIG-001～MIG-160逐项验收，未执行项明确标记，不以Agent CI替代TUI验证
```

最终边界：

```text
minicore-tui:
    Terminal、交互、RPC客户端、Live/History UI

minicore-agent:
    Session、History、Persistence、Update、Steer、Model、Tool

minicore-runtime:
    一次性AgentLoop、Request Boundary、ExecutionConfig、LoopReport
```

---

# 72. 给代码 Agent 的最终执行指令

请根据本文档迁移或重构 `minicore-tui`。

要求：

1. 固定 `minicore-agent` 实际 dev HEAD；
2. 固定其依赖的 `minicore-runtime` HEAD；
3. 将旧TUI v0.1及上一版v0.2迁移Spec标记为被本完整r2替代；
4. 不实现Agent v0.2/v0.3双协议；
5. 保留可复用的Terminal、Theme、Composer和Pi视觉组件；
6. 仅仍基于旧v0.1的实现需要重写Protocol/History/LiveLoop；已完成上一版迁移的实现按第64.1节做r2增量修改；
7. 删除instance_id、session.transcript和durable terminal假设；
8. 支持request_started和多Request Loop；
9. 支持session.update，并正确展示request-boundary语义；
10. 支持turn.steer，并在Running时让Composer进入Steer模式；
11. turn.send成功后立即注册turn.wait；
12. turn.wait必须检查persistence；
13. persistence failed不得用当前History假装恢复本Loop；保留旧completion；
14. Blocked时禁止send/steer/update；
15. Event gap只能在实际History对齐后清除；失败/未知结果保持标记；
16. 不修改Agent或Runtime来迁就旧UI；
17. 不实现Approval、Compaction、Plugin、MCP或Subagent；
18. Tool arguments未暴露时不得猜测；
19. 默认测试全部离线；
20. 最终报告：
    - 起始和最终HEAD；
    - 保留的旧模块；
    - 删除的旧模块；
    - 新Protocol DTO；
    - 新App状态；
    - RPC方法覆盖；
    - MIG-001～MIG-160结果；
    - CI结果；
    - Agent E2E结果；
    - 已知能力缺口；
    - r2的5个后端提交影响核对；
    - 保存确认/关闭原因/Store错误文案核对；
    - 未执行测试及原因。


---

# 73. 本次修订证据与维护说明

以下均为固定commit或固定CI run，便于后续代码Agent复核。仓库中的叙述性文档与具体实现不一致时，
以实际wire序列化、实现与行为测试为依据，并记录差异，不偷偷增加协议能力。

- [S1]：旧基线至新基线的完整比较，确认5个提交/9个文件及未变更的协议定义、Cargo依赖。
- [S2]：blocked completion保留修复及library/RPC测试。
- [S3]：同Loop请求边界换模的确定性测试。
- [S4]：SessionRecord校验修复及list/open测试。
- [S5]：shutdown、持久化与尾行修复边界说明。
- [S6]：Write测试gate/deadline修正，没有生产工具行为变更。
- [S7]：当前RPC文档。
- [S8]：当前Session执行、completion和close实现。
- [S9]：当前Store实现。
- [S10]：当前基线CI run，观察到completed/success；不代表TUI已通过。

[S1]: https://github.com/zqcli/minicore-agent/compare/edd1cb670dc72f61cb94f44bfdff8ca38b5a4999...b2e23938d073ab21c2775faa623561ba929a5ed1
[S2]: https://github.com/zqcli/minicore-agent/commit/bac2b715f7bee3a5865fc581f133dd60acadd1bc
[S3]: https://github.com/zqcli/minicore-agent/commit/e511d9e29c75f7d6a7476baec09fc55ca5fcd379
[S4]: https://github.com/zqcli/minicore-agent/commit/cc9ddf7436b49d2360ce5fde16b76e81cd52ef92
[S5]: https://github.com/zqcli/minicore-agent/commit/c362446a156dbcc5854930d0dbaac97bb612ba19
[S6]: https://github.com/zqcli/minicore-agent/commit/b2e23938d073ab21c2775faa623561ba929a5ed1
[S7]: https://github.com/zqcli/minicore-agent/blob/b2e23938d073ab21c2775faa623561ba929a5ed1/docs/rpc.md
[S8]: https://github.com/zqcli/minicore-agent/blob/b2e23938d073ab21c2775faa623561ba929a5ed1/src/sessions.rs
[S9]: https://github.com/zqcli/minicore-agent/blob/b2e23938d073ab21c2775faa623561ba929a5ed1/src/store.rs
[S10]: https://github.com/zqcli/minicore-agent/actions/runs/33897540665
