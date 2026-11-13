# Configuration

Version 0.11 uses the native Host 0.11 configuration types. Terminal preferences,
Host resources, Agent defaults and saved Session settings have distinct owners.
The TUI does not compose an Agent TOML file or import an old Agent store.

## Paths and first run

The TUI home is `--home`, otherwise `$MINICORE_HOME`, otherwise the executable's
directory. Relative command-line paths are resolved from the working directory.
`--data-root` selects the Host data directory; the default is `<home>/host`.
`--host-config` selects a native JSON candidate; its default is
`<home>/host-config.json`.

A fresh home writes `config.toml`, `host-config.json`, `agent-defaults.json` and
creates `prompt/commands/`. It prints the files to edit and exits with status 1.
The starter endpoint uses an invalid example domain: it must be replaced with
your real provider configuration. Subsequent configuration or argument errors
exit with status 2 before the terminal switches to fullscreen.

| Scope | Configuration source | When it applies |
|---|---|---|
| TUI | `config.toml` `[ui]` | Startup; theme changes are saved immediately |
| Host | Saved native Host configuration, or initial `host-config.json` for a new root | Host startup; explicit `/reload` for changes |
| New Agent | `agent-defaults.json` | Agent creation; explicit `/agent-defaults` to update the selected Agent |
| New Session | Selected Agent's saved Session defaults plus explicit overrides | Session creation |
| Existing Session | Its saved native Session configuration | Reopening; explicit model, reasoning or approval update |

Saved Host configuration wins over the candidate file at startup. An existing
Host does not parse the default candidate until `/reload`; an explicit
`--host-config` is still parsed at startup to report argument errors. Editing a
file does not watch, reload or rewrite existing objects. Opening a Session does
not reapply new-Agent defaults. Startup `--model` and `--reasoning` apply to the
startup Session once; later navigation keeps the selected Session's settings.

## Terminal preferences

```toml
[ui]
theme = "dark"                     # dark | light
bell = true
editor = ["code", "--wait"]         # optional; otherwise $VISUAL, then $EDITOR
```

`/theme` and F4 update only the theme key while preserving comments. The editor
is an executable and argument vector; Ctrl+G appends the temporary draft path.
An empty edited file clears the draft. A failed editor or unreadable result
keeps the original draft and reports the temporary path for recovery.
`NO_COLOR` disables colors. Prompt recall is stored in `history.jsonl`; input
enters it only after verified acceptance.

Only `[ui]` belongs in this file. Legacy `[agent]`, `[profiles]`, `[loop]`,
`[compaction]`, remote settings and `default_profile` are rejected rather than
silently interpreted as native Host settings.

## Native Host resources

Edit the generated `host-config.json`. It has these required top-level fields:

| Field | Meaning |
|---|---|
| `name` | Human-readable Host name |
| `framework_instructions` | Shared framework instructions |
| `models` | Explicit model resources and declared capabilities |
| `tools` | Built-in tool bindings made available by this Host |
| `network` | Native network settings; embedded TUI requires `listen: null` |
| `logging` | Native diagnostic level and destination |

Each model entry contains a full native reference such as `provider/chat`, an
explicit API, a complete endpoint, the provider's model name, a credential
source, context-window declaration, supported reasoning levels and tool support.
For example, replace the corresponding entry in the generated file with:

```json
{
  "model": "provider/chat",
  "api": "chat_completions",
  "endpoint": "https://provider.example.invalid/v1/chat/completions",
  "provider_model": "replace-with-provider-model",
  "api_key_env": "MINICORE_MODEL_KEY",
  "context_window": 128000,
  "supported_reasoning": ["auto"],
  "supports_tools": true,
  "request_timeout_ms": 120000
}
```

For Responses select `"api": "responses"` and the provider's full Responses
endpoint. Endpoint text does not choose the API and there is no fallback from
one protocol to another. Use only capabilities the actual provider/model
supports. `/model` and `/reasoning` display the Host catalog; the TUI does not
invent reasoning levels or silently substitute another model.

`api_key_env` is an environment-variable **name**, never the credential itself.
The native resource preparation step resolves its value. The TUI does not copy
it into an Agent child, config dump or frontend state.

The generated tool bindings are `read`, `write`, `edit`, `apply_patch`, `bash`
and `agent`. The selected Agent and Session configuration determine the enabled
set and execution policy. Tools execute in Host's resource/runtime layer.

### Logs and networking

The starter uses an absolute file path `<home>/minicore-tui.log`:

```json
{
  "level": "info",
  "output": { "kind": "file", "path": "/absolute/path/minicore-tui.log" }
}
```

Keep the generated platform-native path, or choose another absolute path whose
parent exists. Logs cannot target Host metadata or Session history. Native
stderr logging is accepted only when stderr is redirected away from the
terminal, so diagnostics cannot interleave with fullscreen output.

`network.listen` must remain `null`. This executable exposes no HTTP, WS, WSS,
phone pairing, browser authentication or Webpage assets. Independent Host
network deployment belongs to the Host application, not this embedded TUI.

## Agent and Session configuration

`agent-defaults.json` is a native `AgentConfig`, with `role` and
`session_defaults`. Edit the generated Role name, description and instructions,
and the Session model, reasoning, tools and execution policy. The old
`prompt/SYSTEM_PROMPT.md` composition path is replaced by explicit native
instructions.

A new Host creates the first Agent and its default Session. `/agent-new <name>`
creates another Agent from this file's currently loaded defaults. `/new` creates
a Session under the selected Agent, inheriting that Agent's **saved** defaults.
`--workspace` supplies a new Session's workspace; switching an existing Session
reads its own workspace.

`/agent-defaults` rereads this file and performs a revision-checked native Agent
update. It affects future Sessions. Existing Sessions keep their saved
configuration; `/model`, `/reasoning` and `/approval ask|auto|read_only` update
the current Session explicitly. A running Turn keeps its prepared execution
snapshot; changes become available to subsequent work according to Host's
native configuration semantics.

`/default-session` sets the selected Session as its Agent's actual default.
Selecting a row in the TUI alone does not change that default.

## Reload

`/reload` reads the selected Host candidate file, checks the current native
configuration revision and submits one reload request. Preparing and applying
are progress states. Only native `applied` refreshes the model catalog and
publishes the reloaded local command templates/new-Agent defaults.

A failed request shows its failure. A restart-required result lists the native
fields and leaves the effective configuration unchanged. Network listener and
logging changes cannot be applied live. The candidate file is not a way to
silently replace the saved configuration on the next start; use Host's native
configuration/deployment workflow for restart-only changes, or initialize a
new data root with the intended startup configuration.

Existing Agents and Sessions are not reset to the new-Agent defaults by
`/reload`. `/agent-defaults` is the explicit Agent-default update operation.

## Prompt templates

Put a Markdown file in `prompt/commands/`; its filename becomes a slash command.
A built-in command name cannot be shadowed. Optional front matter supplies
`description` and `argument-hint`.

```markdown
---
description: Review the selected change
argument-hint: <area>
---
Review $1 for correctness and tests. Additional instructions: $ARGUMENTS
```

`$1`, `$2`, `$@`, `$ARGUMENTS` and `${1:-default}` expand arguments. These are
text templates, not shell commands. Templates load at startup and after a
successful `/reload`.

## Upgrading from 0.9.x

Use a new home and let the current version generate its native starter files.
Copy the provider endpoint/model declarations, credential-variable names and
desired instructions into the new files explicitly. Reuse prompt template
Markdown if desired.
The old `models.toml`, profiles, composed `session/agent.toml` and Agent Session
store are not automatically translated, opened or modified.

`--agent`, `--agent-config`, `--profile`, `--remote` and `minicore-tui agent` no
longer exist. Webpage/phone configuration and assets have been removed. The
native Host directory must have one owning Host process; attaching a second TUI
to that same directory is not a remote connection.
