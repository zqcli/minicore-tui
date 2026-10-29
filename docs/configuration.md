# Configuration

**Breaking change in 0.5.** The old `~/.config/minicore-tui/config.toml`
(`agent`, `agent_config`, `theme`, `editor`, `bell`), `$MINICORE_TUI_CONFIG`,
`$XDG_CONFIG_HOME`, `~/.config/minicore-tui/prompts/` and `--config <agent.toml>`
are gone. Everything now lives in one directory, the *TUI directory*, and the
Agent config is generated from two files in it. Move templates from `prompts/`
to `prompt/commands/`, and the Agent settings you had in your own `agent.toml`
into `config.toml` and `models.toml` as described below.

## The TUI directory

Resolved in this order:

1. `--home DIR`
2. `$MINICORE_HOME`
3. the directory of the running executable. A release bundle runs as
   `bin/minicore-tui`, so without a flag or variable its files would be written
   inside `bin/`; give it `--home DIR` or `MINICORE_HOME`.

On Linux the executable path is the resolved one, so a symlink in `bin/` leads
to the symlink's target directory; use `--home` or `MINICORE_HOME` there.

```text
<home>/
  config.toml                system settings: profiles, ui, agent, loop, compaction
  models.toml                providers and models
  prompt/
    SYSTEM_PROMPT.md         the system prompt of every profile without its own
    commands/*.md            prompt templates: /name expands to the file
  session/                   the Agent's data_dir: all sessions and Agent stores
    agent.toml               generated Agent config (mode 0600), rewritten each start
  history.jsonl              prompt history (↑ in the composer)
```

`history.jsonl` stays outside `session/` on purpose: it belongs to the composer,
not to the Agent, so `rm -r session` clears every conversation and keeps the
history, and a stale Agent store never touches it.

## First run

When neither `config.toml` nor `models.toml` exists the TUI writes commented
starters (and `prompt/SYSTEM_PROMPT.md`, unless one is already there), prints
where they are and what to fill in (the model id, `base_url` and `api_key_env`),
and exits with status 1. Uncomment the example in `models.toml`, export the
variable `api_key_env` names, and start again. With only one of the two files
present the TUI reports the missing one instead of scaffolding.

## Startup

Before the terminal switches to the alternate screen the TUI:

1. reads and validates `models.toml`, then `config.toml`;
2. checks that every profile's model exists, every model's provider exists, and
   that each profile's system prompt file exists, is non-empty and is within the
   Agent's limits (UTF-8, at most 128 KiB, no control characters);
3. checks that `--profile` and `--model` name something defined and that the
   environment variable for the model about to be used is set and non-empty;
4. writes `session/agent.toml` atomically (mode 0600) with
   `system_prompt = { file = … }` pointing at the absolute prompt path, so the
   Agent reads the prompt itself;
5. starts the built-in Agent, `minicore-tui agent --config
   <home>/session/agent.toml --stdio`, or the external one from `--agent` or
   `[agent] binary`.

Errors name the file, the line when the parser knows it, and the key, and exit
with status 2 before any screen change:

```text
/opt/mct/config.toml:5: [profiles.coding] model: "ghost" is not defined in models.toml (defined: gpt-6-luna)
/opt/mct/models.toml:4: [providers.luna] api_key_env must be the NAME of an environment variable such as OPENAI_API_KEY, not a key; secrets never go in this file
```

`/reload` composes the file again before asking the Agent to reload, so editing
`SYSTEM_PROMPT.md`, a profile or a model and running `/reload` is enough. An
open session takes its profile's new system prompt from its next turn, as does
a session reopened after a restart; the new prompt replaces the old one. A
rejected edit leaves the running Agent untouched and shows the error as a notice.
`[ui]` and `[agent]` are read at start only; `/theme` writes `[ui] theme` back
into `config.toml` without disturbing the rest of the file or its comments.

## models.toml

Providers hold what endpoints share; models hold what one model needs. Secrets
never go here: `api_key_env` is the *name* of an environment variable.

```toml
# models.toml
[providers.luna]
api = "open_ai_responses"          # required; the only API the Agent implements
base_url = "https://api.openai.com/v1"   # required; http(s) URL, no credentials or query
api_key_env = "LUNA_API_KEY"       # required; a variable NAME, never the key
request_timeout_seconds = 900      # optional default for this provider's models

[models.gpt-6-luna]                # the id profiles and --model use
provider = "luna"                  # required; a [providers.*] name
model = "gpt-6-luna"               # name sent to the API; default: the id
context_window = 64000             # required; tokens
max_output_tokens = 20000          # required; must be > 0
safety_margin_tokens = 4000        # default 4000; context_window must exceed
                                   # max_output_tokens + safety_margin_tokens
reasoning = ["auto", "low", "medium", "high", "xhigh", "max"]   # default ["auto"]
tools = true                       # default true
request_timeout_seconds = 600      # overrides the provider's value

[models."gpt-6.1-sol"]             # quote ids with dots, colons or slashes
provider = "luna"
context_window = 372000
max_output_tokens = 32768
reasoning = ["auto", "low", "medium", "high"]
```

| models.toml | Agent `[models.ID]` |
| --- | --- |
| `providers.P.api` | `provider` |
| `providers.P.base_url` | `base_url` |
| `providers.P.api_key_env` | `api_key_env` |
| `models.ID.model` (default `ID`) | `model` |
| `models.ID.context_window` | `physical_context_window` |
| `models.ID.max_output_tokens` | `output_budget_tokens` |
| `models.ID.safety_margin_tokens` | `safety_margin_tokens` |
| `models.ID.reasoning` | `supported_reasoning` |
| `models.ID.tools` | `supports_tools` |
| `models.ID.request_timeout_seconds`, else the provider's | `request_timeout_seconds` |

Reasoning levels: `auto`, `disabled`, `low`, `medium`, `high`, `xhigh`, `max`,
`ultra` (use `ultra` only for an endpoint that documents it). The Agent has no
per-provider request headers, so there is no `headers` key.

## config.toml

```toml
# config.toml
default_profile = "coding"         # required; a profile below

[profiles.coding]
model = "gpt-6-luna"               # required; a model id from models.toml
reasoning = "max"                  # default "auto"; must be in the model's list
approval = "auto"                  # ask (default) | auto | read_only
# tools defaults to all five: read, write, edit, apply_patch, bash

[profiles.review]
model = "gpt-6-luna"
reasoning = "high"
approval = "read_only"
tools = ["read"]                   # [] disables tools; required if the model has tools = false
prompt = "prompt/review.md"        # relative to the TUI directory; default prompt/SYSTEM_PROMPT.md

[ui]
theme = "dark"                     # dark | light (/theme writes this key)
bell = true                        # ring when a long turn ends or waits for approval
editor = ["code", "--wait"]        # Ctrl+G; default $VISUAL, then $EDITOR

[agent]
binary = "bin/minicore-agent"      # an external Agent, relative to the TUI directory
                                   # (or absolute); default: the built-in Agent

[compaction]                       # Agent-wide; unset keys keep the Agent's defaults
enabled = true
trigger_percent = 90               # 0 < target_percent < trigger_percent <= 100
target_percent = 50

[loop]                             # Agent-wide, optional
model_timeout_seconds = 900
model_retry_attempts = 2
model_retry_base_delay_millis = 1000
```

`[loop]` keys: `event_capacity`, `prompt_timeout_seconds`,
`model_timeout_seconds`, `policy_timeout_seconds`, `tool_timeout_seconds`,
`model_retry_attempts`, `model_retry_base_delay_millis`. The Agent checks their
ranges when it starts.

Unknown keys are errors in both files, so a typo is reported with its line
instead of being ignored.

## Remote settings

```toml
[remote]
bind = ["127.0.0.1:8765", "[::1]:8765"] # default; one address or a list, numeric IP and port
# url = "https://host.example:8765" # optional address used by the phone
roots = ["~"]                    # default: HOME on Unix, USERPROFILE on Windows
# roots = ['C:\', 'D:\']          # Windows: whole drives (single quotes keep the backslash)
allow_insecure = false            # needed for a non-loopback bind without TLS
tls = false                       # serve HTTPS and WSS with the certificate below
# tls_cert = "~/certs/fullchain.pem" # PEM chain, leaf first
# tls_key = "~/certs/privkey.pem"    # PEM private key (PKCS#8, PKCS#1 or SEC1)
```

Remote settings are validated at startup, including with `--agent-config`.
`bind` is one address or a list; IPv6 addresses are written in brackets
(`"[::]:8765"`), and an entry may not repeat. By default the TUI listens on the
IPv4 and the IPv6 loopback with the same port, so `http://localhost:8765` works
whichever address `localhost` resolves to first. IPv6 sockets accept only IPv6,
so `"0.0.0.0:8765"` and `"[::]:8765"` can be listed together to reach every
interface on both families. An address the computer cannot listen on, such as
IPv6 on a machine without it or an IP it no longer has, is skipped with the
notice `Remote: not listening on [::1]:8765 (…)` as long as another address
listens; a port that is taken or needs privileges is an error, so the address
never changes behind your back.
A non-loopback bind without TLS requires explicit consent (`allow_insecure`),
because the password and the session then cross the network unencrypted. Use
TLS (below), a private network, a tunnel or a TLS reverse proxy for access
across networks.
A reverse proxy must preserve the browser-facing `Host` authority, including its
port when present: the WebSocket Origin check compares that host and port with
`Origin`. Replacing `Host` with the backend address causes a 403 response.
`url` must start with `http://` or `https://` and have no trailing slash.
Roots must be a non-empty list of existing directories; a leading `~/` expands
from the user's home, and paths are canonicalized. These roots restrict the
phone's directory picker and session selection, not the Agent's tools.
The picker lists only directories under these roots (up to 1000 per listing),
optionally including hidden names, and can create a single child directory.
It cannot read file contents, upload, delete or move entries.
On Windows a root may be a whole drive: `roots = ['C:\', 'D:\']`. In TOML use
single quotes, because `"C:\"` is an invalid escape in a basic string (`"C:\\"`
works too). `C:`, `C:/` and lowercase spellings mean the drive root `C:\`, never
the current directory of drive C. The picker's Roots level then lists each drive
as `C:\`, and the phone switches between them there.
Errors name `config.toml`, the line and the `[remote]` key.

Start with `--remote` or `/remote on`; the transcript area then shows the
remote status screen with the address and the password to enter on the phone,
and the addresses it listens on.
The default URL is `http://<bind>` (`https://` with TLS) for the first address
that listens; an
unspecified bind host is replaced by the address the OS selects for LAN routing.
Set `url` explicitly when using a tunnel, reverse proxy or a machine with
multiple networks.

### Over TLS

With `tls = true` the TUI serves `https://` and `wss://` itself, with a
certificate you provide; `tls_cert` and `tls_key` are then required, expand a
leading `~/`, and may stay in the file while `tls = false`. Both files are read
when `config.toml` loads, so a wrong path, a file that is not PEM or a key that
does not belong to the certificate stops the start with
`config.toml:LINE: [remote] tls_cert: <path>: <reason>` (or `tls_key`). They
are read again each time remote control starts, so after renewing a
certificate (for example from Let's Encrypt) `/remote off` and `/remote on`
pick it up. A non-loopback bind with TLS needs no `allow_insecure`. The status
screen's `Listening` row ends with `TLS`.

Use it when the phone reaches the computer directly, without a proxy that
terminates TLS: across a LAN or a forwarded port, with a certificate whose name
the phone opens (set `url` to that name). Over HTTPS the client also installs
and opens offline. A self-signed certificate must be trusted on the phone, or
browsers refuse `wss:` and the offline service worker: on iPhone, install it as
a profile, then enable full trust in **Settings → General → About → Certificate
Trust Settings**. Plain `http://` sent to a TLS port gets no answer. There is
no HTTP-to-HTTPS redirect.

### Through Cloudflare

The recommended public route is a Cloudflare Tunnel: keep the default loopback
`bind` (no `allow_insecure`), add a public hostname to the tunnel whose service
is `http://127.0.0.1:8765`, and set `url = "https://minicore.example.com"`.
Cloudflare serves the hostname over HTTPS, so the phone gets `https://` and
`wss://` and the client can install and open offline.

- Leave the tunnel's **HTTP Host Header** empty. cloudflared then forwards the
  public `Host`, which must match the browser's `Origin`; overriding it makes
  the WebSocket fail with 403.
- WebSockets must stay on (**Network → WebSockets**, on by default).
- Cloudflare Access in front of the hostname is optional; it adds its own
  sign-in before the password.
- A proxied DNS record (orange cloud) to a forwarded port also works with a
  non-loopback `bind` and a port Cloudflare proxies (such as 8443 or 2053 for
  HTTPS). Turn on `tls` with a Cloudflare Origin CA certificate for the
  hostname and set the zone's SSL/TLS mode to **Full (strict)**, so the hop from
  Cloudflare to the computer is encrypted and verified. Without TLS it needs
  `allow_insecure = true` and an HTTP port (such as 8080 or 2052), and that hop
  is plain HTTP across the internet.
- Behind a tunnel TLS on the computer is optional, since cloudflared already
  encrypts the way to Cloudflare. If you enable it, the tunnel's service must be
  `https://127.0.0.1:8765`, and a certificate not issued for `127.0.0.1` needs
  the tunnel's **Origin Server Name** (`originServerName`) set to a name in the
  certificate, or **No TLS Verify** (`noTLSVerify`).

Responses carry `Cache-Control: no-cache, no-transform`, so Cloudflare does
not rewrite the page (email obfuscation, Rocket Loader, injected scripts) and
no zone setting needs changing. The server gzips the page itself and gives
every file an `ETag`, so a slow hop between Cloudflare and the computer only
makes the first load slower: a reload revalidates each file with a small `304`,
and a reply is cut off only when it stops moving for 10 seconds. The server pings the phone every 20 seconds,
which keeps an idle connection open through Cloudflare.

### On the phone's home screen

On iPhone, open the address in Safari, then **Share → Add to Home Screen**; on
Android Chrome or a desktop Chromium browser, use **Install app** in the
drawer or the browser menu. The installed app has its own storage on iPhone, so
it asks for the password once more. Over `https://` the app opens even while the
computer is unreachable and reconnects when it is back; over plain `http://` on
a LAN it still works but needs the network to open, since browsers keep offline
copies only for secure origins.

The password and the remembered phones are stored in `<TUI dir>/remote.json`,
atomically and with mode 0600 on Unix. Windows inherits the directory's
permissions; the TUI does not harden Windows ACLs, so restrict access to the TUI
directory yourself. The first start of remote mode creates the file with a
generated password such as `k7mq-x2pa-n9dt`; spaces and dashes do not count when
it is typed, letter case does. A phone that signs in gets a token and reconnects
with it later; only SHA-256 token hashes are stored, and at most 20 phones are
remembered (the oldest is forgotten first). Five wrong passwords in a row pause
sign-in for 30 seconds, each further five double the pause up to 15 minutes, and
remembered phones are never paused.

`/remote password` makes a new password and `/remote password <text>` sets one
(at least 8 characters, spaces and dashes not counted), also while remote mode is
off. Either forgets every phone and signs out every connected one. Deleting
`remote.json` forgets every phone and makes a new password at the next start.

Several devices can be connected at once, each viewing and driving its own
session or new-session draft. Switching sessions or picking draft settings on
one device does not move the others; devices on the same session see the same
data. The status screen names each device's session and each client shows who
else is connected. The computer follows the device that acted last, and
`/remote off` shows that foreground transcript. A device connects once: opening
the client in a second tab, or reconnecting with the same token, replaces its
previous connection.
`/remote off` disconnects every phone, stops listening and restores local input;
phones disconnecting alone leave remote mode Waiting.

## Command line

| Option | Meaning |
| --- | --- |
| `--home DIR` | the TUI directory (see above) |
| `--agent BIN` | an external Agent executable instead of the built-in one; beats `[agent] binary` |
| `--agent-config FILE` | use this Agent config as is: no composition, no first-run files, no `models.toml`. `config.toml` is still read for `[ui]`, `[agent]` and `[remote]` if it exists. For tests and advanced use. |
| `--profile ID`, `--model ID`, `--reasoning LEVEL` | settings of the next new session, which the first message creates |
| `--theme dark\|light` | theme for this run |
| `--remote` | start remote control; bind before taking the terminal (bind failure exits with status 1) |
| `--workspace DIR`, `--session ID`, `--continue` | which workspace and session; with neither of the last two the TUI creates no session until the first message |

`--config` was removed; the parser says so instead of reporting an unknown option.

## Prompt templates

`prompt/commands/NAME.md` becomes `/NAME`. The optional front matter
(`description`, `argument-hint`) and the `$1`, `$@`, `${1:-default}` arguments are
unchanged from 0.4. A file named like a built-in command is ignored.
