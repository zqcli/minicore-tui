# Configuration

Version 0.12.4 uses one JSON configuration directory for the embedded Host and
terminal frontend. Host owns `host.json`, Agent declarations and reusable Roles.
The TUI reads and writes only `tui.json`; it passes the resolved directory to
Host's Rust startup API.

## Paths and first run

`--config-dir <DIR>` selects the configuration directory. The default is
`~/.minicore/config`. Relative paths are resolved from the startup working
directory. `--data-root <DIR>` independently selects runtime storage; its default
is `~/.minicore/data`, even when only `--config-dir` is overridden.

When `host.json` is missing, initialization creates only missing example files,
even if empty `agents/` or `roles/` directories already exist. These include one
shared Host starter configuration, one Role, one Agent,
`tui.json` and `commands/`. Setup prints the files and exits with status 1.
The sample Agent records the absolute `--workspace` directory, or the current
working directory, at initialization. Its example provider endpoint must be
replaced before use. Argument and configuration failures exit with status 2
before fullscreen startup.

| Path | Owner and purpose |
|---|---|
| `<config-dir>/host.json` | Host resources, framework instructions, logging and network settings |
| `<config-dir>/roles/*.json` | Reusable Role values referenced by Agent declarations |
| `<config-dir>/agents/*.json` | Named Agent declarations with stable keys and Session defaults |
| `<config-dir>/tui.json` | Local theme, editor and bell preferences |
| `<config-dir>/commands/*.md` | Local slash-command templates |
| `<data-root>/` | Host identities, committed configuration, Sessions and history |
| `<data-root>/tui/history.jsonl` | Confirmed TUI input recall |

Host reads its directory on every startup and on explicit `/reload`. Saved
records preserve identity and history; current file declarations supply Host
and managed Agent configuration. Existing Sessions retain their own committed
configuration. Files are not watched automatically.

## Terminal preferences

`tui.json` uses standard JSON:

```json
{
  "ui": {
    "theme": "dark",
    "editor": ["code", "--wait"],
    "bell": true
  }
}
```

The theme is `dark` or `light`. An omitted file or field uses the terminal's
defaults. Unknown fields and invalid values are errors. JSON comments are not
supported. `/theme` and F4 save the theme while preserving editor and bell.
Host does not read this file; a UI error does not invalidate Host configuration.

The editor is an executable and argument vector. If it is empty, Ctrl+G uses
`VISUAL`, then `EDITOR`, and appends the temporary draft path. An empty edited
file clears the draft. Editor failure or unreadable output preserves the draft
and reports its recovery path. `NO_COLOR` disables colors. Input enters recall
history only after Host confirms acceptance.

## Host resources

Edit the generated `host.json` using native Host configuration fields:

| Field | Meaning |
|---|---|
| `name` | Human-readable Host name |
| `framework_instructions` | Optional shared framework text; empty by default |
| `models` | Explicit model resources and declared capabilities |
| `tools` | Tool bindings available from this Host |
| `workspace_roots` | Allowed workspace catalog roots |
| `network` | Native network settings; embedded TUI requires `listen: null` |
| `logging` | Diagnostic level and destination |

Each model has a full reference, explicit API, complete endpoint, provider model
name, credential source and declared capabilities. A model entry looks like:

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
endpoint. Host does not infer the API from the endpoint or fall back to another
protocol. `/model` and `/reasoning` use the Host catalog and its actual declared
capabilities. `api_key_env` names an environment variable; it never contains the
credential itself. Production resource preparation resolves that variable.

The starter enables `read`, `write`, `edit`, `apply_patch`, `bash` and `agent`.
Agent and Session configuration select tools and execution policy. Host owns
resource validation and execution.

The starter logs to `~/.minicore/logs/host.log` using an absolute native-home
path; initialization creates its parent so diagnostics cannot corrupt the terminal. Keep that path or choose another file whose parent exists. Logs may
not target Host metadata or Session history. Stderr logging is accepted only
when stderr is redirected away from the terminal.

`network.listen` must be `null`. This executable runs one local embedded Host.
It exposes no browser server, phone protocol or remote connection interface.
Web deployment uses its own `web.json` and is independent of this TUI directory.

## Roles, Agents and Sessions

Each `roles/<role>.json` contains `name`, `description` and optional
`instructions`. Each Agent file contains:

| Field | Meaning |
|---|---|
| `key` | Stable unique configuration identity, such as `coding` |
| `name` | Display name, such as `Coding assistant` |
| `role` | Role filename stem, such as `coding` for `roles/coding.json` |
| `instructions` | Optional Agent-specific text; empty by default |
| `session_defaults` | Complete native defaults for model, reasoning, tools, execution and workspace |

Retain the complete generated `session_defaults` object when editing values.
`role` names exactly one file stem in `roles/`. It cannot contain a path.
Missing Roles, duplicate Agent keys, unknown fields and invalid resources fail
validation before publication.

The stable `key` binds a declaration to one persistent Agent. Changing its
filename or display name preserves the Agent ID and existing default Session.
Host creates new declared Agents and their default Sessions. Removing a bound
declaration makes startup or reload fail before any publication or record write.
A failed reload keeps the existing in-memory configuration; a restart cannot
recover deleted defaults from Session history or stored fingerprints.

File-managed Agent name/default updates require editing the declaration and
`/reload`. Explicit deletion still uses the native exact-scope operation. To
remove such an Agent permanently, remove its declaration and use `/agent-delete`
before reloading. Deletion never edits source files; a retained declaration
creates a new Agent on a later reload. Start/stop, default Session selection and
Session updates keep their native behavior.

`/agent-new <name>` copies the currently selected Agent's native configuration
into a new dynamic Agent. It copies Role, Agent instructions and Session defaults;
it does not copy file management, Session overrides or history. With no selected
Agent, add an Agent declaration and `/reload` first. `/new` creates a Session
from the selected Agent's current defaults, with the current `--workspace`
override. Opening an existing Session preserves its saved workspace.

Host assembles nonempty prompt layers in order: framework, Role, Agent, workspace
and history. Empty instruction strings add no placeholder prompt. Role name and
description do not substitute for instructions. Updating Role or Agent files
changes future Session defaults; existing Sessions and already running Turns
keep their committed snapshots. `/model`, `/reasoning` and `/approval` update
the current Session explicitly. Startup model and reasoning flags apply once.

## Reload

`/reload` sends only an operation ID and the expected native configuration
revision. Host rereads its bound directory, validates the complete candidate,
prepares resources and reports real owner progress. The TUI does not send JSON
candidates or choose another directory during reload.

After full or partial application, the TUI refreshes the actual Agent, Session
and model catalogs and reloads local command templates. Partial application
means the Host configuration was published and some Agent operations failed;
the notice identifies failed declarations. Completed changes remain applied,
and the TUI does not retry automatically. Existing Sessions are not rewritten.

Restart-required changes, including logging and the network listener, leave
both Host and Agent configuration unchanged. Restart with the same directory
to load those changes. Preparation or validation failures leave the active
configuration unchanged. UI preferences are independent of Host reload; edit
`tui.json` and restart to load editor or bell changes.

## Prompt templates

Put Markdown in `<config-dir>/commands/`; each filename becomes a slash
command. Built-in command names cannot be shadowed. Optional front matter gives
`description` and `argument-hint`:

```markdown
---
description: Review the selected change
argument-hint: <area>
---
Review $1 for correctness and tests. Additional instructions: $ARGUMENTS
```

`$1`, `$2`, `$@`, `$ARGUMENTS` and `${1:-default}` expand arguments as text.
Templates load at startup and after full or partial Host application.

## Removed configuration paths

`--home`, `--host-config`, `MINICORE_HOME`, `config.toml`, `agent-defaults.json`
and `/agent-defaults` are removed. There is no conversion or compatibility
loader. Create a new JSON directory and explicitly transfer desired provider
settings and instructions. The old standalone Agent store is not imported.

A data root has one owning Host process. Opening it in a second TUI is not a
remote connection and cannot bypass the exclusive owner lease.

## Storage format 3

Host metadata is `data/host.json`, Agent metadata is `data/agents/<id>.json`, and
all Session folders are `data/sessions/<id>/`. Each Session retains `session.json`
and `history.jsonl`. File-managed Host/Agent records contain references and
fingerprints, not duplicate configuration bodies; dynamic Agents retain inline
configuration. Existing Sessions keep independent effective settings.

The embedded Host rejects old object schemas and nested storage without migration
or automatic rewriting. Back up old roots and use a fresh `--data-root` for the
new format. Moving the old files alone is not a supported conversion.
