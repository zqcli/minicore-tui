# minicore-tui

`minicore-tui` is a fullscreen terminal frontend for
[minicore-host](https://github.com/zqcli/minicore-host). Version 0.12.16 embeds
Host 0.12.16 in the same executable and uses its typed Rust client through
`HostClient::in_process`. Host owns Agents, Sessions, execution, tools,
credentials, history and persistence; the TUI owns terminal interaction and
presentation.

The backend is pinned to an immutable Git revision in `Cargo.toml`.
`Cargo.lock` fixes the complete dependency graph. Run `/remote`
inside the TUI to expose the embedded Host over WSS (with configured PEM TLS
files) or local WS (IPv4/IPv6, e.g. behind a reverse proxy or Cloudflare orange
cloud) for `minicore-web` using a one-time in-memory access key.

## Build and first run

Use current stable Rust and a terminal supporting an alternate screen, raw
mode and bracketed paste. Mouse reporting enables selection and scrolling.
`Ctrl+J` always inserts a newline; modified Enter keys depend on terminal support.
On macOS, enable Option as Meta / +Esc in the terminal profile for
Option+Return follow-ups. See the [terminal setup guide](docs/keybindings.md#optionreturn-on-macos)
for Terminal.app, iTerm2, Ghostty and kitty, including a mapping for just that key.

```bash
cargo build --locked --release
target/release/minicore-tui --config-dir ./config --workspace ./project
```

A fresh configuration directory gets JSON starter files and setup instructions, then
exits. Edit the generated native Host model configuration with the provider's
full endpoint, explicit `chat_completions` or `responses` API, model name and
capabilities. Export the credential using the environment-variable name in
`api_key_env`, then run the same command again. No live provider or credential
is needed to run the automated tests.

| File or directory | Owner and purpose |
|---|---|
| `tui.json` | TUI preferences under `ui`: theme, editor, bell |
| `host.json` | Native Host configuration, read at startup and explicit `/reload` |
| `roles/*.json` | Reusable Role names, descriptions and instructions |
| `agents/*.json` | Agent declarations with stable keys and Session defaults |
| `data/` | Persistent Host data, including `tui/history.jsonl` for input recall |
| `commands/*.md` | Local slash-command prompt templates |

Host creates declared Agents and their default Sessions. Startup rereads the
configuration directory while preserving existing Host/Agent identities,
Sessions and history. An Agent's default Session opens unless `--session` or
`--continue` selects another, or it belongs to another workspace: then the
latest Session in the startup workspace opens, or the first input creates one
there. `/new` creates a Session in the startup workspace from the selected
Agent's current defaults with explicit overrides. Existing Sessions keep their saved
configuration. See [configuration](docs/configuration.md) for declaration
ownership, JSON examples and removed configuration paths.

## Options

| Option | Meaning |
|---|---|
| `--config-dir <DIR>` | JSON configuration directory; defaults to `~/.minicore/config` |
| `--data-root <DIR>` | Runtime storage; independently defaults to `~/.minicore/data` |
| `--agent-id <ID>` | Select this existing Host Agent |
| `--workspace <DIR>`, `[WORKSPACE]` | Workspace override for a newly created Session (e.g. `minicore .`); defaults to the current directory |
| `--session <ID>` | Open this Session; an explicit Agent must match its actual owner |
| `--continue` | Open the most recently active Session in the workspace, respecting an explicit Agent selection |
| `--model <REF>` | Full native model reference, applied once to the startup Session or used when creating one |
| `--reasoning <LEVEL>` | `auto`, `disabled`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra`; the selected model must support it |
| `--theme <dark\|light>` | Theme for this invocation |
| `-h`, `--help`, `-V`, `--version` | Usage or version |

`--session` and `--continue` cannot be combined. Removed options such as
`--home`, `--host-config`, `--agent`, `--agent-config`, `--profile`, `--remote`,
and the `agent` subcommand
are rejected. Argument and configuration errors are printed before fullscreen
startup.

## Terminal workflow

The rail layout, Markdown rendering, folding, mouse selection, incremental
layout cache, multiline editor, paste handling, prompt recall, external editor
and light/dark themes remain available. Idle rendering has no periodic tick.
`Ctrl+G` hands terminal input to the external editor and restores the edited draft
on return. If terminal restoration fails, the error names the retained draft file.
Find and the Sessions, Models and Agents filters accept terminal paste without
changing the draft. Forward Delete and nonempty Ctrl+D are separate undoable
edits; Ctrl+Z and Ctrl+Y restore or replay them.

- **Conversation:** stream answer and reasoning text, retain interrupted partial
  output, show native Turn outcome and persistence state. Separate Responses
  reasoning parts keep their line breaks during streaming and after reopening.
  Model retry phase and attempt information come from Host. `/reasoning` opens
  the picker without an argument; an invalid explicit level reports an error
  and preserves the setting.
- **Errors:** failed Turns retain Host's upstream message in the conversation,
  reopened history and `/export`. `Ctrl+O` expands failure details; clicking a
  failure opens a scrollable view with `c` to copy the complete safe message.
  `/error` recalls the last operation error after its notice is dismissed,
  including its original Session identity when the operation has one. Host
  redacts bound credentials; the TUI removes terminal control sequences when
  displaying or copying details. Startup, reload and shutdown retain the cause.
- **Tools:** display complete arguments, progress, output, result, failures and
  native delegation references. Argument generation is not a separate progress
  phase. Expanded cards and `/diff` expose details without changing execution.
  Folded tools are one row each (state glyph, name, subject, facts) with one
  blank row between every visible step; each run of a Turn's work folds as it
  goes into `▸ Working · N tools` / `▸ Worked …` with its newest step underneath.
  Blank lines inside tool output remain intact. See
  [docs/ui-spec.md](docs/ui-spec.md) for the visual rules.
- **Unified popups, sheets and split workspaces:** `/` commands and `@` files
  float directly above the pinned composer (keeping the Activity Line visible
  during long runs) with structured argument hints, active-value/hotkey badges,
  `PgUp`/`PgDn` paging and double-click confirmation. All pickers share one
  bottom-sheet style (`▎` selection highlight, unified `/ ` filter with `Ctrl+U`
  clear, single-click select / double-click confirm). `/diff` shares the exact
  two-pane workspace engine as `Ctrl+B` (compact 1-line searchable file list,
  `List ↔ Detail` focus, narrow-screen toggle, live `r`/`Ctrl+R` refresh,
  scrollbar and dual-pane mouse support), and `F1` Help, `/error`, `/context`
  and `/session` share one scrollable page layout.
- **Async tasks and delegated Agents:** `agent` dispatch calls stay compact
  single-line sync steps inside `Working`/`Worked`, while internal `agent completed`
  banners stay hidden from the main conversation so the parent flow stays clean.
  The single-line Activity Line above the editor reports live async counts
  (`⚠ N needs input · M running · K failed · C completed · ctrl+b`). Press
  `Ctrl+B` (`/tasks`) or click any dispatch row to open the
  full-screen searchable Async Tasks workspace (two-pane on wide terminals,
  single-pane list ↔ conversation on narrow ones): filter tasks by typing or
  pasting, scroll and fold the child's real `Working`/`Worked` conversation with
  `→`/`Tab` and `↑↓`/`PgUp`/`PgDn`/`Ctrl+O`, message the selected child directly
  from the isolated bottom composer (`Enter`), copy with `Ctrl+Y`, jump to
  origin with `Ctrl+G`, or open the full child Session with `Ctrl+E`.
- **Input:** Enter sends steering input; Alt+Enter or Ctrl+Enter queues a
  follow-up. Accepted `next_step` and `next_turn` entries come from Host. Esc
  cancels the captured Turn and preserves queued input; `/pause` prevents later
  queue execution and `/continue` allows it again. A mutation receipt alone never
  marks a Turn complete or a queue entry delivered. Asynchronous compaction
  failures show the native fault once in the conversation notice without moving
  the reading position; background failures name their originating Session.
- **Questions:** approvals support allow once or deny. Tool questions support
  native text and single-choice answers. Each answer is bound to its original
  Session, Load, Turn and interaction. Use `/approval ask|auto` for a
  real Session execution policy; in `ask` mode mutating and outside-workspace
  tool calls prompt for approval, while `auto` permits inside- and
  outside-workspace calls directly. There is no fabricated per-tool allow cache.
- **Agents and Sessions:** `/agents` selects a Host Agent, `/agent-new` copies
  its native configuration into a new dynamic Agent, and the Session picker shows
  actual owning Agents, lifecycle states and a stable short Session ID on every
  row. Untitled Sessions keep that ID as their name even after you open them.
  The ID remains visible when long titles are clipped.
  Background Sessions keep running when terminal selection changes. Rename,
  close, delete, load, unload, pause and resume use native Host operations.
  Renaming refreshes the original Session's list and detail metadata even after
  navigation. Agent deletion confirms the exact child-Session scope before submission.
- **History:** reopening reads a recent native history window; older pages use
  the same opaque cursor and fixed upper bound. Live output and canonical
  records deduplicate by native identity and position. `/export` streams the
  complete selected Session history independently of what is loaded onscreen.
- **Context and workspace:** `/context` shows Host's actual history and usage
  facts. Last-response token usage is not presented as a fabricated context
  percentage. File completion, branch status, changes and paged diffs are read
  through the Session's Host workspace API.
- **Uncertain input:** F8 retains a send whose acceptance could not be verified.
  It keeps its original target and requires explicit possible-duplicate
  confirmation before resending with a fresh Message ID. It is never resent
  automatically.

Full commands and key bindings: [docs/keybindings.md](docs/keybindings.md).

## Configuration and shutdown

`/reload` asks Host to reread its bound JSON directory and follows the native
receipt through preparation and owner application. Full and partial results
refresh actual catalogs and local command templates. Partial application,
restart-required changes and failures remain explicit. Agent file changes
supply defaults for future Sessions; existing Sessions retain their settings.
The TUI reads and saves its own `tui.json` independently of Host configuration.

The generated configuration writes diagnostics to a file so Host logs cannot
corrupt the terminal. Embedded mode requires `network.listen` to be `null`.
A configuration sending logs to terminal stderr is rejected before fullscreen;
choose a file or redirect stderr.

Quitting restores the terminal, flushes local preferences and prompt history,
and requests graceful Host shutdown. The process waits for actual execution,
cleanup and persistence to finish. Paused work or a save fault can keep shutdown
pending; the terminal explains the state. Only an explicit Ctrl+C at that
shutdown prompt requests force shutdown. Restarting the program reads durable
history; it does not reconstruct the old process's active Turn.

## Checks and packages

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
```

Reducer tests use a real in-process Host with explicit model test doubles.
Integration tests run the production resource builder and local HTTP fixtures
for both provider protocols, persistence, reopening and shutdown. Rendering
snapshots and platform-neutral terminal-input tests remain separate from
execution evidence. Optional frame-cost checks are available with
`cargo test --locked --lib perf_tests -- --ignored --nocapture`.

CI checks Linux, macOS and Windows and produces Linux x86_64, macOS arm64,
macOS x86_64 and Windows x86_64 packages. Each contains `bin/minicore-tui`
(`.exe` on Windows), guides and `BUILD.txt` identifying the TUI and pinned Host
revisions. Use `--config-dir` and optional `--data-root` to keep configuration
and data outside the package.

See [architecture](docs/architecture.md), [migration design](docs/design-host.md),
[0.11.0 performance and size](docs/performance-0.11.0.md) and
[CHANGELOG.md](CHANGELOG.md).
