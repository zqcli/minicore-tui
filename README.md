# minicore-tui

`minicore-tui` is a fullscreen terminal frontend for
[minicore-host](https://github.com/zqcli/minicore-host). Version 0.11.0 embeds
Host 0.11.0 in the same executable and uses its typed Rust client through
`HostClient::in_process`. Host owns Agents, Sessions, execution, tools,
credentials, history and persistence; the TUI owns terminal interaction and
presentation.

The backend is pinned to immutable commit
[`32f074c57868533af81b0c6491440bae24b872c8`](https://github.com/zqcli/minicore-host/commit/32f074c57868533af81b0c6491440bae24b872c8).
`Cargo.lock` fixes the complete dependency graph. There is no Agent child
process, stdio RPC adapter, Webpage server, browser client or phone protocol.
This executable runs a local embedded Host; connecting to a separately deployed
Host is outside this version's interface.

## Build and first run

Use current stable Rust and a terminal supporting an alternate screen, raw
mode and bracketed paste. Mouse reporting enables selection and scrolling.
`Ctrl+J` always inserts a newline; modified Enter keys depend on terminal support.

```bash
cargo build --locked --release
target/release/minicore-tui --home ./minicore-home --workspace ./project
```

A fresh home gets starter configuration files and setup instructions, then
exits. Edit the generated native Host model configuration with the provider's
full endpoint, explicit `chat_completions` or `responses` API, model name and
capabilities. Export the credential using the environment-variable name in
`api_key_env`, then run the same command again. No live provider or credential
is needed to run the automated tests.

| File or directory | Owner and purpose |
|---|---|
| `config.toml` | TUI preferences under `[ui]`: theme, editor, bell |
| `host-config.json` | Native initial Host configuration and explicit `/reload` candidate |
| `agent-defaults.json` | Native Role and Session defaults used to create an Agent |
| `host/` | Persistent Host data; read and written by Host |
| `prompt/commands/*.md` | Local slash-command prompt templates |
| `history.jsonl` | Confirmed local input recall history |
| `minicore-tui.log` | Default generated diagnostic destination |

On a new Host the TUI creates an Agent and its native default Session. Later
starts use saved Host, Agent and Session configuration. An existing Agent's
default Session opens unless `--session` or `--continue` selects another.
`/new` explicitly creates a Session under the selected Agent. Session creation
inherits that Agent's defaults, with explicit new-Session overrides applied by
Host. Opening or switching a Session preserves its saved configuration.

The old `minicore-agent` store is not imported or modified. Use a separate TUI
home when moving from 0.9.x; see [configuration](docs/configuration.md).

## Options

| Option | Meaning |
|---|---|
| `--home <DIR>` | TUI directory; otherwise `$MINICORE_HOME`, then the executable's directory |
| `--data-root <DIR>` | Host persistence root; defaults to `<home>/host` |
| `--host-config <FILE>` | Initial native Host JSON and `/reload` candidate; saved Host configuration takes precedence at startup |
| `--agent-id <ID>` | Select this existing Host Agent |
| `--workspace <DIR>` | Workspace override for a newly created Session; defaults to the current directory |
| `--session <ID>` | Open this Session; an explicit Agent must match its actual owner |
| `--continue` | Open the most recently active Session in the workspace, respecting an explicit Agent selection |
| `--model <REF>` | Full native model reference, applied once to the startup Session or used when creating one |
| `--reasoning <LEVEL>` | `auto`, `disabled`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra`; the selected model must support it |
| `--theme <dark\|light>` | Theme for this invocation |
| `-h`, `--help`, `-V`, `--version` | Usage or version |

`--session` and `--continue` cannot be combined. Removed options such as
`--agent`, `--agent-config`, `--profile`, `--remote`, and the `agent` subcommand
are rejected. Argument and configuration errors are printed before fullscreen
startup.

## Terminal workflow

The rail layout, Markdown rendering, folding, mouse selection, incremental
layout cache, multiline editor, paste handling, prompt recall, external editor
and light/dark themes remain available. Idle rendering has no periodic tick.
`Ctrl+G` hands terminal input to the external editor and restores the edited draft
on return. If terminal restoration fails, the error names the retained draft file.

- **Conversation:** stream answer and reasoning text, retain interrupted partial
  output, show native Turn outcome and persistence state. Model retry phase and
  attempt information come from Host. `/reasoning` opens the picker without an
  argument; an invalid explicit level reports an error and preserves the setting.
- **Tools:** display complete arguments, progress, output, result, failures and
  native delegation references. Argument generation is not a separate progress
  phase. Expanded cards and `/diff` expose details without changing execution.
- **Input:** Enter sends steering input; Alt+Enter or Ctrl+Enter queues a
  follow-up. Accepted `next_step` and `next_turn` entries come from Host. Esc
  cancels the captured Turn and preserves queued input; `/pause` prevents later
  queue execution and `/resume` allows it again. A mutation receipt alone never
  marks a Turn complete or a queue entry delivered.
- **Questions:** approvals support allow once or deny. Tool questions support
  native text and single-choice answers. Each answer is bound to its original
  Session, Load, Turn and interaction. Use `/approval ask|auto|read_only` for a
  real Session execution policy; there is no fabricated per-tool allow cache.
- **Agents and Sessions:** `/agents` selects a Host Agent, `/agent-new` creates
  one, and the Session picker shows actual owning Agents and lifecycle states.
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

`/reload` loads the selected Host JSON candidate, follows the native reload
receipt through preparation and application, and refreshes the model catalog
only after actual application. Restart-required and failed outcomes remain
explicit. It also refreshes local prompt templates and new-Agent defaults on
successful application. `/agent-defaults` explicitly applies the defaults file
to the selected Agent; it does not rewrite existing Sessions.

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
revisions. Use `--home` or `MINICORE_HOME` to put data outside the package.

See [architecture](docs/architecture.md), [migration design](docs/design-host.md),
[0.11.0 performance and size](docs/performance-0.11.0.md) and
[CHANGELOG.md](CHANGELOG.md).
