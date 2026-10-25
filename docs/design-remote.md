# Remote mode design

Remote mode lets a phone drive this TUI instance. The TUI stays the only host:
it owns the Agent child, serves a small web client and one WebSocket, and turns
what the phone asks for into the same reducer operations the keyboard uses. The
Agent and the Runtime do not change.

This document is the contract for the implementation. Where the code and this
document disagree after the feature lands, the code wins and this file is fixed.

## Decisions

- **Intents in, projection out.** The phone never sends Agent RPC. It sends a
  closed set of intents that the reducer executes like local actions, and it
  receives a projection of the resident session built from `App` state:
  a snapshot on connect, then changed blocks (by block revision) and state
  changes. Turn lifecycle, reconciliation, approvals, follow-ups and titles stay
  in the Rust reducer only.
- **One controller.** Remote mode is `Off`, `Waiting` (listening, local input
  frozen) or `Connected` (one phone in control). A second device is refused; a
  new connection from the *same* device replaces the old one (a phone that lost
  its network reconnects before the old socket times out).
- **Same session.** The phone and the TUI always show the same resident
  session. Opening or creating a session from the phone changes the TUI too.
- **Best effort.** No event log, no replay, no automatic resend. Reconnecting
  gives a fresh snapshot.
- **Web client first.** The TUI serves a single-file web client (PWA-capable).
  The protocol below is versioned (`v: 1`) and is the interface a future native
  or Flutter client uses too, so it carries semantic data, never rendered rows.
- **No built-in TLS.** Default bind is loopback. Across networks use Tailscale,
  WireGuard, an SSH tunnel or a TLS reverse proxy; `url` names the address the
  phone uses.
- **Not in scope:** TLS, sleep prevention, device revocation UI, push
  notifications, delete/move/upload in the directory browser, Git clone,
  `agent.reload`, `agent.shutdown`, `session.delete` from the phone.

## Configuration and command line

`config.toml`:

```toml
[remote]
bind = "127.0.0.1:8765"        # default; host:port
url = "https://mac.tail1234.ts.net:8765"   # optional; the address put in the QR code
roots = ["~/Develops", "/srv/work"]        # default: the user's home directory
allow_insecure = false         # required (true) for a non-loopback bind
```

- Validated when `config.toml` loads, reported as `file:line: [remote] key: reason`
  with exit status 2 before the screen changes:
  - `bind` parses as a socket address; a non-loopback IP needs `allow_insecure = true`
    ("a non-loopback bind sends traffic unencrypted; set allow_insecure = true").
  - `roots` entries expand a leading `~`, must exist, must be directories, and are
    stored canonicalized. An empty list is an error.
  - `url` must start with `http://` or `https://`; no trailing `/`.
- `--remote` starts remote mode at startup. The listener is bound before the
  terminal is taken over; a bind failure prints the error and exits with status 1.
- Paired devices live in `<TUI dir>/remote-devices.json`:
  `{"devices":[{"name":"iPhone Safari","token_sha256":"<hex>","paired_at":"<RFC 3339>"}]}`,
  written atomically, mode `0600` on Unix. Only token hashes are stored. Deleting
  the file forgets every device.

Slash commands (always allowed, also in remote mode):

| Command | Effect |
|---|---|
| `/remote` or `/remote status` | Open the Remote sheet: mode, URL, pairing QR and code, connected device |
| `/remote on` | Bind and enter `Waiting`; a fresh pairing code is made; opens the sheet |
| `/remote off` | Say `bye` to the phone, close it, stop listening, unfreeze local input |
| `/remote pair` | While listening, make a new pairing code (the old one stops working); opens the sheet |

When Off, `/remote pair` asks the user to run `/remote on` first; it never
starts a listener or freezes local input as an implicit side effect.

## Local behaviour in remote mode (`Waiting` or `Connected`)

- The transcript, tool output and status keep updating. The composer keeps its
  draft and slash input, but cannot send.
- Local `on_action` is **deny by default**. Allowed: `SetTheme`, `ToggleTheme`,
  `CopyLastAnswer`, `Export`, `EditDraft`, `LoadOlder`, `RecallQueued`, `Quit`,
  every `/remote` command and panels that only show (help, session, context,
  diff, the sessions picker; picking a session is `OpenSession`, which is denied).
  A denied action shows the flash `Remote control is on · /remote off to take back`
  and changes nothing.
- `Esc` still interrupts a running turn or cancels a compaction: a safety valve
  for the person at the computer. (It is not routed through `on_action` today and
  stays allowed.)
- A pending approval does not take the local keyboard and is not drawn as a
  sheet; the status line says `Waiting for approval` and the phone answers it.
- Entering remote mode moves queued follow-ups back into the composer
  (`restore_draft` of the joined text), so no local message waits to be sent.
- Quitting asks twice: the first request (Ctrl+C, `/quit`) flashes
  `Remote control is on · quit again to end it`; a second within 3 s quits.
  Quitting says `bye` to the phone first.
- When the phone disconnects the mode goes back to `Waiting`; local input stays
  frozen until `/remote off`.

## Transport

One TCP listener on `bind`. Each connection's request head (max 8 KiB, 10 s) is
peeked:

- `GET /` → the web client (`text/html; charset=utf-8`).
- `GET /manifest.webmanifest`, `GET /icon.svg` → the PWA manifest and icon.
- `GET /ws` with `Upgrade: websocket` → the WebSocket. If an `Origin` header is
  present its host:port must equal the `Host` header, else `403`. No `Origin`
  (a non-browser client) is allowed; authentication still applies.
- Anything else → `404`. Only `GET`.

Every HTTP response carries `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`,
`Referrer-Policy: no-referrer` and
`Content-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:`.

WebSocket rules: text frames of JSON objects with a `"t"` type field; max
message 1 MiB; the first message must arrive within 10 s; at most 8
unauthenticated connections at once; the server sends a WebSocket ping every
20 s and closes a connection that has sent nothing (pong included) for 60 s.
Each authenticated connection has an outbound queue of 512 messages; when it is
full the TUI drops the connection (the phone reconnects and gets a snapshot).
After `bye`, the server sends a WebSocket close frame with a bounded handshake.
Process exit allows at most 500 ms for the connection task to flush and close.

## Pairing and authentication

- `/remote on` and `/remote pair` make a pairing code: 8 characters from
  `ABCDEFGHJKLMNPQRSTUVWXYZ23456789`, shown as `ABCD-EFGH`, valid 10 minutes,
  single use. Five failed pair attempts invalidate it; stopping the listener
  invalidates its code too. Token generation precedes reducer admission, but
  consumption and persistence follow acceptance. A `busy` rejection leaves the
  code valid and writes no device.
- The QR code and the shown link are `<base>/#pair=ABCDEFGH`. The fragment never
  reaches the server or its logs. `<base>` is `url` when set, else
  `http://<bind>`, with an unspecified bind host (`0.0.0.0`, `::`) replaced by the
  machine's primary LAN address.
- The web client reads the code from the fragment, removes the fragment with
  `history.replaceState`, pairs, and keeps the returned token in `localStorage`
  (`minicore.token`). Later visits authenticate with the token.
- Tokens are 32 random bytes (OS CSPRNG), base64url without padding. The server
  compares SHA-256 hashes.

## Protocol v1

### Handshake

Device names contain 1–64 Unicode characters and no control characters; invalid
names are `bad_request` and do not spend a pairing attempt.

Client's first message, one of:

```json
{"t":"pair","v":1,"code":"ABCDEFGH","device":"iPhone Safari"}
{"t":"hello","v":1,"token":"<base64url>"}
```

Server answers, in order:

```json
{"t":"paired","token":"<base64url>"}                      // only after a pair
{"t":"welcome","v":1,"version":"0.5.5","device":"iPhone Safari"}
{"t":"snapshot","state":{...},"blocks":[...]}
```

or one error and closes: `{"t":"error","code":"unauthorized"|"busy"|"version"|"bad_request","message":"..."}`.
`busy`: another device is in control. `version`: `v` is not 1.

### Client → server (after the handshake)

```json
{"t":"intent","id":"c17","kind":"submit","text":"..."}
{"t":"intent","id":"c18","kind":"queue","text":"..."}
{"t":"intent","id":"c19","kind":"cancel"}
{"t":"intent","id":"c20","kind":"answer","interaction_id":"...","choice":"allow"}
{"t":"intent","id":"c21","kind":"load_older"}
{"t":"intent","id":"c22","kind":"open_session","session_id":"ses_..."}
{"t":"intent","id":"c23","kind":"new_session","workspace":"/Users/me/Develops/app","profile":"coding"}
{"t":"intent","id":"c24","kind":"set_model","model":"gpt-6-luna"}
{"t":"intent","id":"c25","kind":"set_reasoning","level":"high"}
{"t":"fs_roots","id":"c26"}
{"t":"fs_list","id":"c27","path":"/Users/me/Develops","hidden":false}
{"t":"fs_mkdir","id":"c28","parent":"/Users/me/Develops","name":"new-app"}
```

`id` is a client string of at most 64 bytes, echoed in the answer. Unknown `t`
or `kind`, or missing fields, answer `ack`/`fs` with `ok:false` and
`error:"bad_request"`; the connection stays open. An invalid or oversized `id`
is returned as the empty string, never echoed without its 64-byte bound.

Intent semantics (each is acknowledged once the reducer has accepted and
dispatched it, or refused it):

| kind | Behaviour | Refused when |
|---|---|---|
| `submit` | No session: create one in the draft workspace with this text as the first message (like the first Enter). Idle: send. Running: steer. | empty text; a session is opening or being created |
| `queue` | Running: queue a follow-up. Otherwise as `submit`. | as `submit` |
| `cancel` | Interrupt the running turn, else cancel a running compaction, else nothing. | — |
| `answer` | Approvals accept only `"allow"`, `"allow_session"`, or `"deny"`; choice prompts accept only an in-range row index. Goes through the local answer path, so session allowance is remembered. | no pending interaction or id differs (`"stale"`); wrong answer kind or index (`"bad_request"`), leaving the interaction untouched |
| `load_older` | Load an older history page into the shared transcript. | no session |
| `open_session` | Open that session (it becomes resident in the TUI too). | unknown id; its workspace is outside `roots` |
| `new_session` | Leave the session for a draft in `workspace` (default: the current draft workspace) with optional `profile`; the next `submit` creates it. No empty session is created. | workspace relative, outside `roots`, missing or not a directory; unknown profile |
| `set_model` / `set_reasoning` | As `/model` and `/reasoning` (session or draft). | unknown model or level |

`submit` and `queue` trim trailing whitespace like local sends.
A refused `submit` or `queue` text is never put into the local composer. When the
Agent later refuses a send (the path that calls `restore_draft` today) while a
phone is connected, the text goes back to the phone as `returned`.

### Server → client

```json
{"t":"ack","id":"c17","ok":true}
{"t":"ack","id":"c17","ok":false,"error":"Remote text is empty"}
{"t":"snapshot","state":State,"blocks":[Block, ...]}
{"t":"blocks","len":42,"upserts":[{"i":40,"block":Block},{"i":41,"block":Block}]}
{"t":"state","state":State}
{"t":"notice","level":"info"|"error","text":"..."}
{"t":"returned","text":"..."}
{"t":"fs","id":"c26","ok":true,"roots":[{"path":"/Users/me/Develops","name":"Develops"}]}
{"t":"fs","id":"c27","ok":true,"listing":Listing}
{"t":"fs","id":"c28","ok":true,"created":"/Users/me/Develops/new-app"}
{"t":"fs","id":"c27","ok":false,"error":"outside_roots","message":"..."}
{"t":"bye","reason":"remote_off"|"tui_exit"|"replaced"}
```

`blocks`: the transcript now has `len` blocks; the client truncates its list to
`len`, then sets each `upserts[k].block` at index `upserts[k].i`. `snapshot`
replaces everything. Every `app.notice` is forwarded as `notice`.

`State` (sent whole whenever any field changed):

```json
{
  "session": null | {"id":"ses_...","title":"Fix parser","workspace":"/abs","profile":"coding",
                     "model":"gpt-6-luna","reasoning":"high"},
  "draft": {"workspace":"/abs","profile":"coding"|null,"model":"..."|null,"reasoning":"..."|null},
  "status": {"label":"Thinking"|null,"busy":false,"started_ms":1761300000000|null,
             "interrupting":false,"compacting":false,"loading":false},
  "usage": {"input":0,"output":0,"cached":0},
  "context_percent": 41.5 | null,
  "interaction": null | {"id":"...","kind":"approval"|"choice","tool":"bash","detail":"cargo test",
                         "input":"..."|null,"prompt":"...","options":["Allow once","Deny","Allow bash this session"]},
  "queue": [{"kind":"steer"|"follow","text":"..."}],
  "older": true,
  "sessions": [{"id":"ses_...","title":"...","workspace":"/abs","updated_at":"<RFC 3339>","current":true}],
  "models": [{"id":"gpt-6-luna","reasoning":["low","medium","high"]}],
  "profiles": ["coding","ask"]
}
```

- `label` is `App::status_label()`. `started_ms` is the Unix time the live turn
  started (stored on `LiveTurn` when it is created, not derived from `Instant`,
  so the state does not change by itself). `loading` is `App::loading_session`.
- `older`: an older page can be loaded. `sessions`: the catalog filtered to
  workspaces under `roots`, newest first, at most 50. `context_percent` is the
  footer's figure, never an estimate. The pure calculation lives in
  `app::session::context_percent` and `ui::views::context` re-exports it as
  `percent`, so the projection does not depend on `ui`.

`Block` (one per transcript block, same order and indices):

```json
{"type":"user","kind":"prompt"|"steering"|...,"text":"...","at":1761300000000|null}
{"type":"assistant","loop_id":"...","request":1,"reasoning":"...","text":"...","streaming":true}
{"type":"tool","call_id":"...","name":"bash","detail":"cargo test","input":"..."|null,"input_lines":3|null,
 "state":"pending"|"running"|"succeeded"|"failed"|"denied"|"cancelled","exit_code":0|null,
 "duration_ms":1200|null,"output":["..."],"output_dropped":0,"output_lines":120|null,"truncated":false}
{"type":"turn_end","loop_id":"...","outcome":"completed"|...,"duration_ms":8000|null,"requests":2,"tools":3,
 "usage":{"input":0,"output":0,"cached":0},"changes":{"files":2,"added":10|null,"removed":3|null}|null}
{"type":"compaction","before":32100|null,"after":2700|null,"summary":"..."|null}
{"type":"earlier","hidden_items":12}
```

Enum values (`kind`, `outcome`) use the names the Agent protocol already uses.
A user block uses `kind:"steering"`; the pending queue uses `kind:"steer"`.
Assistant `text` is raw Markdown; clients render it.

### Projection rules

- After every main-loop iteration that handled input, an Agent event or a
  message, `App::sync_remote()` runs. With no connected phone it returns at once.
- A residency change (`Session::epoch` differs from the last one sent, or a
  session appeared or went away) or a new connection sends `snapshot`.
- Otherwise the transcript's revisions are compared index by index with the
  revisions last sent: every index whose revision differs, or that is new, is an
  upsert; `len` is the current length. Prepending older pages therefore resends
  the window, which is correct and rare.
- `State` is built, serialized and compared with the last one sent; only a
  difference sends `state`. Elapsed time is computed by the client from
  `started_ms`, so a running turn does not send a `state` every frame.
- An idle TUI with a connected phone does nothing: no timers in the main loop.

## Host directory interface

Only for the phone's directory picker; the model cannot call it. Pure, blocking
functions in `remote::fs`, run off the main loop by the connection task (they do
not need `App`). Only the accepted controller can request them. Replacing or
stopping that controller revokes queued filesystem work and suppresses replies
from work already in progress. A filesystem operation already started while
authorized may finish; it is not rolled back.

Operations:

- `roots`: the configured roots, with `name` = last path component.
- `list(path, hidden)`: `path` is canonicalized and must be under a root
  (`Path::starts_with` on canonical paths). Returns only directories (symlinks
  whose canonical target is a directory under a root; the entry `path` is that
  target), names starting with `.` only when `hidden`, sorted case-insensitively,
  at most 1000 (`truncated`). Unreadable entries and broken, looping or protected
  symlinks are skipped; failure to open the requested directory is still an error.
  `parent` is the canonical parent when it is still
  under a root, else `null`.
  `Listing = {"path":"/abs","parent":"/abs"|null,"entries":[{"name":"src","path":"/abs/src"}],"truncated":false}`
- `mkdir(parent, name)`: `name` is one non-empty component of at most 255 bytes,
  not `.` or `..`, without `/`, `\`, NUL or a drive prefix. On every platform,
  trailing dots/spaces and Windows device names (`con`, `nul`, `aux`, `prn`,
  `com1`–`com9`, `lpt1`–`lpt9`, case-insensitive, with or without an extension)
  are rejected. The canonical
  `parent` must be under a root; `create_dir` (never `create_dir_all`) and an
  existing entry is `exists`. Returns the canonical new path.
- Errors: `outside_roots`, `not_found`, `not_a_directory`, `invalid_name`,
  `exists`, `io` (message has the OS error).
- On Windows returned paths drop the `\\?\` verbatim prefix; both forms are
  accepted as input.
- Root checks for sessions compare `SessionInfo::workspace` (the Agent stores it
  canonical) with the canonical roots by `Path::starts_with`, without file IO.
  `new_session` canonicalizes its `workspace` once.

The directory listing is a host convenience and the roots are a UI boundary, not
a sandbox: whoever controls the session can run any command through the Agent's
tools.

## Workspace follows the session

`App::workspace` is the draft workspace: where the next new session is created.
Opening or creating a session sets it to that session's workspace; `new_session`
from the phone sets it to the chosen directory. `--continue` still uses the
launch workspace.

## Code layout

| Path | Contents |
|---|---|
| `src/remote/mod.rs` | `RemoteMode`, the handle `App` keeps (start/stop, outbound sender, pairing code, URL), `RemoteEvent` posted to the main loop as `Msg::Remote` |
| `src/remote/server.rs` | Listener, HTTP, WebSocket upgrade, handshake, pings, per-connection tasks |
| `src/remote/auth.rs` | Pairing codes, token generation and hashing, the device file |
| `src/remote/wire.rs` | Serde types of every message above |
| `src/remote/view.rs` | `State`/`Block` projection from `&App` and the diff tracker |
| `src/remote/fs.rs` | Host directory functions |
| `src/remote/web/` | `index.html` (the whole client: HTML, CSS, JS), `manifest.webmanifest`, `icon.svg`, embedded with `include_str!` |
| `src/app/remote.rs` | `App` side: `/remote` commands, mode changes, local gating, intents, `sync_remote` |
| `src/ui/…` | Remote sheet (status, URL, QR code, pairing code), composer and footer indicators |

`remote` depends on `app` and `protocol`, never on `ui`. Remote messages reach
the reducer through the existing `Msg` channel (`Msg::Remote`), so the main loop
gains no new branch beyond calling `sync_remote` once per iteration.

## Limits

| Limit | Value |
|---|---|
| Request head | 8 KiB, 10 s |
| WebSocket message in | 1 MiB |
| First message | 10 s |
| Unauthenticated connections | 8 |
| Outbound queue per connection | 512 messages, then the connection is dropped |
| Ping / idle close | 20 s / 60 s |
| Directory entries per listing | 1000 |
| Sessions in `State` | 50 |
| Pairing code | 10 min, single use, 5 failures |
