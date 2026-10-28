# minicore-tui

`minicore-tui` is a fullscreen terminal frontend for [`minicore-agent`]: a
scrolling transcript, a status line, a composer and a footer. The Agent is built
into the same executable: the TUI starts itself as `minicore-tui agent` in a
child process and talks to it with stdio NDJSON JSON-RPC (Protocol v1), so one
file is the whole program. Version 0.4 is a rewrite of the v0.3 frontend; see
[CHANGELOG.md](CHANGELOG.md).

## Requirements

- current stable Rust (edition 2024) to build; no older toolchain is tested;
- a terminal with an alternate screen, raw mode and bracketed paste. Mouse
  reporting is used for the wheel, clicks and selection. `Shift+Enter` and
  `Ctrl+Enter` need a terminal with the kitty keyboard protocol; `Ctrl+J` always
  inserts a newline. `Alt+Enter` needs the terminal to pass Alt (on macOS, Option
  as Meta or Esc+); see [docs/keybindings.md](docs/keybindings.md).

Built-in backend: Agent 0.8.2, which links Runtime 0.8.0; `Cargo.toml` pins the
Agent by its release tag and `Cargo.lock` fixes both. An
external Agent can still be named with `--agent`. At startup the TUI requires
Protocol v1 and these Agent capabilities, and refuses an Agent that lacks one: `session.read`, `session.read.display`, `session.context`,
`turn.result`, `tool.read`, `tool.read.display`,
`workspace.files`, `changes.list`, `changes.diff`.

## Build and run

```bash
cargo build --locked --release
export MINICORE_HOME=~/minicore                  # the TUI directory
target/release/minicore-tui --workspace ./project   # first run writes starter files
```

Everything lives in one *TUI directory*: `--home`, else `$MINICORE_HOME`, else
the directory of the executable.
Providers and models are in `models.toml`, profiles and preferences in
`config.toml`, the system prompt in `prompt/SYSTEM_PROMPT.md`, and every session
in `session/`. On the first run the TUI writes commented starters, says what to
fill in and exits; after that it validates both files, writes the Agent config
to `session/agent.toml` and starts the built-in Agent with it
(`minicore-tui agent --config session/agent.toml --stdio`) before the screen
switches, so a bad configuration is an ordinary terminal error
naming the file, line and key. The TUI then opens the session `--session` or
`--continue` names. Otherwise it starts on an empty conversation with the editor
and the footer, and creates the session only when you send the first message, so
quitting first leaves no empty session behind. `--profile`, `--model`,
`--reasoning`, `/model`, `/reasoning` and `/new <profile>` set what that session
starts with. A draft typed meanwhile is kept. Full reference: [docs/configuration.md](docs/configuration.md).

Prebuilt packages: every CI run uploads one per platform (Linux x86_64, macOS
arm64 and x86_64, Windows x86_64) as a workflow artifact,
`minicore-tui-<version>-<target>`. Each holds `bin/minicore-tui` (`.exe` on
Windows), the Agent included, and `BUILD.txt` naming the three revisions. Unpack
it anywhere and run `bin/minicore-tui --home <DIR>` (or set `MINICORE_HOME`) from
the project directory; without either, the TUI directory is `bin/` itself. On
macOS a downloaded binary may need `xattr -d com.apple.quarantine bin/*` first.

### Options

| Option | Meaning |
|---|---|
| `--home <DIR>` | The TUI directory. Default: `$MINICORE_HOME`, else the executable's directory. |
| `--agent <BIN>` | Run this external Agent executable instead of the built-in one. Default: `[agent] binary` in `config.toml`, if set. |
| `--agent-config <FILE>` | Use this Agent config as is; no composition, no `models.toml`. For tests and advanced use. Replaces `--config`. |
| `--workspace <DIR>` | Workspace of a new session. Default: the current directory. |
| `--session <ID>` | Open this session. |
| `--continue` | Open the most recent session of the workspace; with none, start on an empty conversation. Cannot be combined with `--session`. |
| `--profile <ID>` | Profile of a new session (created by the first message). |
| `--model <ID>` | Model for the session; applied to an opened session with `session.update`. |
| `--reasoning <LEVEL>` | `auto`, `disabled`, `low`, `medium`, `high`, `xhigh`, `max` or `ultra`; applied like `--model`. |
| `--theme <dark\|light>` | Palette for this run. |
| `--remote` | Start [remote mode](#remote-mode) at startup. |
| `-h`, `--help`, `-V`, `--version` | Usage, version. |

Bad arguments print the usage and exit with status 2.

### Configuration

`config.toml` (profiles, `[ui]`, `[agent]`, `[loop]`, `[compaction]`) and
`models.toml` (providers and models, never keys: only `api_key_env`) replace the
0.4 `~/.config/minicore-tui/config.toml`, which is no longer read.

```toml
# config.toml
default_profile = "coding"

[profiles.coding]
model = "gpt-6-luna"               # a model id from models.toml
reasoning = "max"
approval = "auto"                  # ask | auto | read_only

[ui]
theme = "dark"                     # /theme writes this key back
editor = ["nvim"]                  # Ctrl+G; default $VISUAL, then $EDITOR
bell = true                        # ring when a turn over 10 s ends or asks approval
```

```toml
# models.toml
[providers.luna]
api = "open_ai_responses"
base_url = "https://api.openai.com/v1"
api_key_env = "LUNA_API_KEY"       # the NAME of the variable

[models.gpt-6-luna]
provider = "luna"
context_window = 64000
max_output_tokens = 20000
reasoning = ["auto", "low", "medium", "high", "max"]
```

Command-line options win. A malformed file, an unknown key or a dangling
reference stops the start with `file:line: key: reason`; nothing is ignored
silently. `/theme` edits only the `theme` key and keeps comments. `/reload`
composes the Agent config again, so editing `SYSTEM_PROMPT.md` or a profile and
running `/reload` applies it, to open sessions too from their next turn. With `NO_COLOR` set the UI uses no colors. Prompt
history is `history.jsonl` in the TUI directory.

## Screen

```text

 minicore-tui v0.7.3                                  (welcome header)

 esc interrupt · ctrl+c clear · / commands · @ files · ctrl+o expand · ctrl+r sessions

 ~/work/minicore-tui

 transcript (scrollable, 1-column gutter, ▎ rails)
 …
 ↳ follow-up 1  also update the README
 ⠋ Working (esc to interrupt) · Thinking · 1m12s · request 7 · 12 tools

 ▎ editor band (4–12 rows; the / and @ lists open inside it)
 ▸ minicore-tui@main · gpt 6 luna · max · working · 1m · queued   ↑50.5k ↓1.8k R33.8k · ctx 41.00%
```

The look follows pi-rail-ui. While something is in flight a Working line sits
above the editor with the phase (`Thinking`, `Running <tool> <detail>`,
`Waiting for approval`, `Compacting context`, …), elapsed time, request and
tool counts. Queued messages show in send order as `↗ steer` / `↳ follow-up N`
rows above it (`sent` marks steers the Agent already has), with a hint
for the keys that act on them.
The latest notice is a railed line in the band: info fades after 5 seconds,
errors stay until the next key press.

The footer shows the workspace and git branch, the model and reasoning level,
the state while not idle, the current or last turn's minutes, `queued`, token
totals since the session was opened and `ctx N%` — the context usage the Agent
reported for its last request (amber from 70 %), `ctx ?` once tokens were used
without a report. The TUI never estimates it.

## Features

- **Conversation.** User messages and Markdown answers stream with an incremental
  renderer. User messages are tinted cards; thinking has a purple rail and
  folds to its newest three rows under `... (N earlier lines, ctrl+o to expand)`;
  `Ctrl+T` collapses it to one `Thinking...` row.
- **Tool cards** start folded to three rows on a state-coloured band (running,
  done, failed): tool name, target (`$ cmd`, `path:range`, `pattern in path`)
  and a hidden-line hint (`running…` while a running call has no output yet),
  with duration, exit code or line changes on the right. `Ctrl+O` expands all
  cards and thinking, and a click toggles one. Expanded cards show a plain
  hidden-line count where output is cut: above the tail for `bash`, below the
  head for other tools. Any row of an expanded card collapses it on click;
  dragging over its output selects without collapsing.
- **Turn end** is one dim line: outcome, duration, requests, tools and, when files
  changed, `N files changed (+a −b)`. A cancelled turn shows its reason. A failed
  turn names the cause in plain words with the provider's status and code
  (`failed · rate limited · 429 rate_limit_exceeded`), then what to do about it,
  then the provider's own message behind a `│` rail, three lines until `Ctrl+O`
  opens it whole; a selection copies the message only.
  The reply an interrupt cut short stays above that line, also after the
  session is reopened.
- **Sessions.** None exists until the first message: the first `Enter` creates
  it with the profile, model and reasoning picked so far and sends the text.
  `/new` leaves the open session and returns to that state. A session you leave
  keeps running in the background and comes back instantly, as you left it,
  from `Ctrl+R`; the footer counts the ones working or waiting for approval, and
  a notice says when one finishes or needs you. Until then the
  commands that read a session (`/compact`, `/context`, `/diff`,
  `/rename`, `/export`, `/copy`, `/session`) and the `@` file picker say
  `No session yet: send a message first`.
- **Steering and queue.** `Enter` queues a steer; `Alt+Enter` (or `Ctrl+Enter`)
  queues a follow-up. Messages dispatch in send order: steers behind a follow-up
  wait until it starts the next turn. `Alt+↑` takes all unsent messages back;
  the web tray offers Edit and Delete. Sent steers cannot be withdrawn.
  `Esc` interrupts. A message sent while a compaction after a turn is
  running waits in the queue and is sent when the session is idle.
- **Compaction** shows as a status phase and leaves a `[compaction]` box with
  the token counts and the summary behind `Ctrl+O`; loaded content is never
  removed. `/compact` runs it manually.
- **History.** Opening a session loads the latest items; scrolling to the top
  loads older pages, including the history behind a compaction summary.
  A card whose output is not loaded shows its line count.
- **Views** replace the transcript and keep the footer: Git diff and help.
  `/diff` shows the workspace's live tracked changes and untracked files,
  also when opened by clicking a turn-end line. `Esc` returns; the
  composer is dimmed meanwhile, because the view owns the keys.
- **Completion and selectors.** `/` and `@` open a list inside the editor that
  filters as you type (`Tab` inserts, `Enter` runs, `Esc` closes and keeps the
  text). Sessions with delete (`Ctrl+R`), model (`Ctrl+L`), reasoning
  (`Shift+Tab` cycles), find (`Ctrl+F`), session info (`/session`), context usage
  (`/context`) and approval prompts replace the editor with a bordered sheet.
- **Copy and export.** Dragging copies the selection on release (native
  clipboard, OSC 52 fallback) and flashes `Copied!`; double-click selects a
  word, triple-click a line. `/copy` copies the last answer, `/export [path]` writes the
  conversation as Markdown (default `./minicore-<session>-<date>.md`).
- **Composer.** Grapheme-safe editing, undo and redo, prompt history, paste
  tokens for large pastes, external editor on `Ctrl+G`.

## Remote mode

`--remote`, or `/remote on` in a running TUI, lets a phone drive the open session
from its browser. The transcript area turns into a status screen with an address
and a password: open the address on the phone and enter the password once; the
phone remembers the sign-in. Each connected device views and drives its own
session or new-session draft; switching on one never switches the others.
Devices viewing the same session see the same conversation. The status screen
names each phone's session and shows what the last active device's Agent is
doing. Local sending is frozen and the footer shows `remote`; `/remote off`
takes the keyboard back and shows that foreground transcript again. `/remote password` makes a new password and
signs phones out. Without `--remote` nothing listens.

The server listens on `127.0.0.1:8765` and `[::1]:8765` by default, which only
the same machine can reach. A phone on the same LAN needs a non-loopback `bind` and either `tls = true` with
your own certificate (`tls_cert`, `tls_key`) or `allow_insecure = true`, in which
case the password crosses the network unencrypted. Across networks, keep the
loopback bind and publish it through Tailscale, an SSH tunnel or a TLS reverse
proxy, or listen with TLS on a forwarded port, then set
`[remote] url` to the address the phone should open; a Cloudflare Tunnel to
`http://127.0.0.1:8765` works as is. On iPhone, Safari's **Share → Add to Home
Screen** installs the client as an app (it asks for the password once more);
over HTTPS it opens even while the computer is asleep and reconnects when it
wakes. The `[remote]` settings, Cloudflare notes and install steps are
in [docs/configuration.md](docs/configuration.md) and the protocol in
[docs/design-remote.md](docs/design-remote.md).

Whoever signs in controls the session, can have the Agent run commands on this
machine and answers its approvals: keep the password to yourself, change it with
`/remote password` if it leaks, and run `/remote off` when done.

## Keys and commands

The complete map is [docs/keybindings.md](docs/keybindings.md). The essentials:

- `Enter` send or steer, `Alt+Enter` follow-up, `Shift+Enter` / `Ctrl+J` newline,
  `Esc` interrupt (idle: clear the draft), `Ctrl+C` clear / quit, `Ctrl+D` quit;
- `Ctrl+R` sessions, `Ctrl+L` model, `Shift+Tab` reasoning, `Ctrl+O` tool cards,
  `Ctrl+T` reasoning text, `Ctrl+F` find, `F1` help;
- `PageUp` / `PageDown`, `Ctrl+Home` / `End`, `Ctrl+↑` / `Ctrl+↓` and the
  wheel navigate; `/` opens the command palette and `@` the file picker;
- `/new [profile]`, `/sessions` (`/resume`), `/session`, `/rename`, `/model [id]`,
  `/reasoning`, `/compact`, `/diff`, `/context`, `/copy`, `/export`,
  `/theme`, `/reload`, `/remote`, `/help`, `/quit`, and a command per prompt template in
  `<home>/prompt/commands/*.md`; `Ctrl+P` cycles models.

## Troubleshooting

- **Agent not found:** the external Agent named by `--agent` or `[agent] binary`
  does not exist; drop both to use the built-in one.
- **Fatal screen:** the Agent exited, sent an unreadable frame or lacks a
  required capability. The screen shows the reason and the last 20 lines of the
  Agent's stderr; `q` or `Esc` quits.
- **Waiting for compaction:** a compaction after the last turn is still running;
  your message is queued and sent when it finishes.
- **Older history does not load:** the history changed while it was read. The
  read is retried once on its own; scroll up again if a notice says so.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
```

Reducer tests drive the app through a scripted in-process Agent, and
`TestBackend` snapshot tests cover the main screen states; only
`tests/built_in_agent.rs` starts the real built-in Agent, with no provider.
CI runs Clippy and the tests on Linux, macOS and Windows with stable Rust.
Module layout and invariants are in [docs/architecture.md](docs/architecture.md);
coding agents should read [AGENTS.md](AGENTS.md).

## License

Licensed under either the Apache License, Version 2.0 or the MIT license, at
your option.

[`minicore-agent`]: https://github.com/zqcli/minicore-agent
