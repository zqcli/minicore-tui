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
- **Same session.** The phone and the TUI always share the resident session.
  Opening or creating a session from the phone changes it for the TUI too.
- **The TUI only reports.** While remote mode is on, the TUI does not show the
  conversation: its transcript area becomes a status screen saying that remote
  control is on, how to connect, which phone is in control, which session and
  workspace are open and what the Agent is doing now. The reducer keeps the full
  session state for the phone; turning remote mode off shows the transcript again.
- **Password sign-in.** Turning remote mode on needs nothing. A phone signs in
  with the connection password the status screen shows, once per device; after
  that it reconnects with a remembered token.
- **Best effort.** No event log, no replay, no automatic resend. Reconnecting
  gives a fresh snapshot.
- **Web client first.** The TUI serves a single-file web client (PWA-capable).
  The protocol below is versioned (`v: 1`) and is the interface a future native
  or Flutter client uses too, so it carries semantic data, never rendered rows.
- **No built-in TLS.** Default bind is loopback. Across networks use Tailscale,
  WireGuard, an SSH tunnel or a TLS reverse proxy; `url` names the address the
  phone uses.
- **Not in scope:** TLS, sleep prevention, per-device revocation, push
  notifications, delete/move/upload in the directory browser, Git clone,
  `agent.reload`, `agent.shutdown`, `session.delete` from the phone.

## Configuration and command line

`config.toml`:

```toml
[remote]
bind = "127.0.0.1:8765"        # default; host:port
url = "https://mac.tail1234.ts.net:8765"   # optional; the address the status screen shows
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
- The password and the remembered phones live in `<TUI dir>/remote.json`,
  written atomically, mode `0600` on Unix:
  `{"password":"k7mq-x2pa-n9dt","devices":[{"name":"iPhone Safari","token_sha256":"<hex>","paired_at":"<RFC 3339>"}]}`.
  The first start of remote mode creates the file with a generated password.
  Only token hashes are stored; at most 20 phones are remembered (the oldest
  goes first). Deleting the file forgets every phone and makes a new password.

Slash commands (always allowed, also in remote mode):

| Command | Effect |
|---|---|
| `/remote on` | Bind and enter `Waiting`; the status screen replaces the transcript |
| `/remote off` | Say `bye` to the phone, close it, stop listening, show the transcript and unfreeze local input |
| `/remote` or `/remote status` | When on: nothing to open, the status screen already shows everything (close any open picker or view). When off: the info notice `Remote control is off · /remote on starts it` |
| `/remote password` | Generate a new password |
| `/remote password <text>` | Use `<text>` (at least 8 characters, spaces and dashes not counted) |

Changing the password signs every phone out: the remembered devices are
forgotten, a connected phone gets `bye signed_out`, and the notice
`New remote password · phones must sign in again` is shown. While remote mode is
off the password still changes (it is in the file). A `/remote password …`
line is never written to the prompt history.

## Local behaviour in remote mode (`Waiting` or `Connected`)

- **The status screen** replaces the transcript area (a full-screen view such as
  help or diff, opened from the allowed commands, still draws over it). It is
  read-only, not selectable, and the wheel and clicks over it do nothing. Rows,
  label column dim, values in text colour:

  ```text
   Remote control                                         ● phone connected
                                                          (○ waiting for a phone, amber)

     Address     http://192.168.20.207:8765
     Password    k7mq-x2pa-n9dt
     Phone       iPhone Safari · connected at 14:02
                 (none yet · open the address on your phone and enter the password)

     Session     Fix the parser          (New session · starts with the phone's first message)
     Workspace   ~/Develops/app
     Model       gpt-6-luna · max
     Now         ⠋ Running bash · cargo test · 12s
                 (⠋ Thinking · 4s / ⠋ Waiting for approval on the phone · bash · rm -rf build /
                  ⠋ Compacting context / Idle · last turn completed in 7.8s / Idle)
     Queue       2 messages waiting                       (only when the queue is not empty)
     Sign-in     3 wrong passwords · paused for 30 s      (only after a failure; amber)

     /remote off take back control · esc stop the turn · /remote password new password
  ```

  The home directory reads `~`. Values are cut to the width (never wrapped);
  on a narrow terminal the label column shrinks to the widest label. `Now` uses
  the same spinner and elapsed format as the Working line and animates only
  while the TUI already animates (`App::animating`); an idle screen does not tick.
- The dock in remote mode shows notices, the editor and the footer only: no
  Working line, no queue rows and no approval sheet (the status screen says it).
  The editor keeps its draft and slash input but cannot send; an empty editor
  shows the faint placeholder `Remote control is on · /remote off to take back`.
  The footer shows `remote` (waiting) or `remote · <device>` (connected).
- Local `on_action` is **deny by default**. Allowed: `SetTheme`, `ToggleTheme`,
  `CopyLastAnswer`, `Export`, `EditDraft`, `LoadOlder`, `RecallQueued`, `Quit`,
  every `/remote` command and panels that only show (help, session, context,
  diff, the sessions picker; picking a session is `OpenSession`, which is denied).
  A denied action shows the flash `Remote control is on · /remote off to take back`
  and changes nothing.
- `Esc` still interrupts a running turn or cancels a compaction: a safety valve
  for the person at the computer.
- A pending approval does not take the local keyboard; the phone answers it.
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
- `GET /manifest.webmanifest`, `GET /icon.svg`, `GET /icon.png` → the PWA
  manifest and icons (`icon.png` is 180×180 for the iOS home screen; served
  with `image/png` from `include_bytes!`).
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

## Password sign-in

- A generated password is 12 characters from `abcdefghjkmnpqrstuvwxyz23456789`
  (OS CSPRNG, rejection sampling), stored and shown in groups of four:
  `k7mq-x2pa-n9dt`. Comparison ignores whitespace and `-` on both sides and is
  case-sensitive, so `k7mq x2pa n9dt` and `k7mqx2pan9dt` also work.
- Five wrong passwords in a row pause sign-in for 30 s; every further five
  double the pause, up to 15 min. A right password resets the count. While
  paused even the right password is refused with `locked`. Remembered phones
  (`hello` with a token) are never paused.
- Each refusal is reported to the reducer (`RemoteEvent::LoginRefused`), which
  counts it for the status screen's `Sign-in` row (reset by `/remote on` and by a
  password change) and shows the info notice `Remote: wrong password` or
  `Remote: sign-in paused for 30 s`.
- A right password makes a token (32 random bytes, base64url without padding).
  The phone is stored only after the reducer admits it (a `busy` refusal stores
  nothing). The web client keeps the token in `localStorage` (`minicore.token`)
  and later signs in with `hello`. The server compares SHA-256 hashes.

## Protocol v1

### Handshake

Device names contain 1–64 Unicode characters and no control characters; invalid
names are `bad_request` and do not count as a wrong password.

Client's first message, one of:

```json
{"t":"login","v":1,"password":"k7mq-x2pa-n9dt","device":"iPhone Safari"}
{"t":"hello","v":1,"token":"<base64url>"}
```

Server answers, in order:

```json
{"t":"authorized","token":"<base64url>"}                  // only after a login
{"t":"welcome","v":1,"version":"0.5.5","device":"iPhone Safari","now_ms":1761300003000}
{"t":"snapshot","state":{...},"blocks":[...]}
```

`welcome.now_ms` is the host's Unix time in milliseconds when that message is
built. The client samples its own clock on receipt to estimate the host/phone
offset, refreshing that offset on each handshake, and uses host-adjusted time
with `status.started_ms` for elapsed time. `now_ms` appears only in `welcome`,
never in snapshots or `State`, so clock passage alone causes no state updates.

On failure, the server sends one error and closes:
`{"t":"error","code":"wrong_password"|"locked"|"unauthorized"|"busy"|"version"|"bad_request","message":"..."}`.
`wrong_password`: the login password is wrong. `locked`: sign-in is paused; the
error carries `"retry_after":<whole seconds, rounded up>`. `unauthorized`: the
`hello` token is unknown (signed out); the client forgets it and asks for the
password. `busy`: another device is in control. `version`: `v` is not 1.

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
{"t":"bye","reason":"remote_off"|"tui_exit"|"replaced"|"signed_out"}
```

`blocks`: the transcript now has `len` blocks; the client truncates its list to
`len`, then applies each upsert at index `i`. An upsert is exactly one of:

```json
{"i":40,"block":Block}
{"i":41,"append":{"text":"new suffix","reasoning":"new thinking suffix"}}
```

`block` replaces the whole block. `append` concatenates each supplied string to
the existing assistant block's corresponding field; `text` and `reasoning` are
independently optional. The server sends `append` only when the last block sent
at that index on this connection and the current block are both streaming
assistants with the same `loop_id` and `request`, and neither text length has
shrunk. Live assistant content is append-only. New, moved, replaced, or finished
blocks use the full `block` form; when streaming ends the full block is sent once.
Snapshots always contain full blocks and replace everything. Every `app.notice`
is forwarded as `notice`.

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

- `draft.model` and `draft.reasoning` use the footer's resolved values
  (`App::model_id()` and `App::reasoning()`), including profile defaults when no
  explicit override is set. `draft.profile` is the launch profile or default
  profile. Unresolved values remain `null`.
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

- `App::sync_remote()` runs after each paced frame (at most once per FRAME),
  and once before exit. When the terminal guard is inactive the same frame-pacing
  branch still synchronizes the phone and clears the pending frame. No extra
  timer is introduced; with no connected phone sync returns at once.
- A residency change (`Session::epoch` differs from the last one sent, or a
  session appeared or went away) or a new connection sends `snapshot`.
- Otherwise the transcript's revisions are compared index by index with the
  revisions last sent: every index whose revision differs, or that is new, is an
  upsert; `len` is the current length. Prepending older pages therefore resends
  the window, which is correct and rare.
- The catalog portions of `State` are cached by the catalog generation, which
  changes on catalog assignment, merge or deletion (and listener generation for
  configured roots). Current-session marks are applied without rebuilding or
  sorting catalogs. The tracker keeps streaming keys and byte lengths, not copies
  of assistant content.
- `State` is built, serialized and compared with the last one sent; only a
  difference sends `state`. Elapsed time is computed by the client from
  `started_ms`, so a running turn does not send a `state` every frame.## Host directory interface

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
| `src/remote/mod.rs` | `RemoteMode`, the handle `App` keeps (start/stop, outbound sender, password access, URL, sign-in refusals), `RemoteEvent` posted to the main loop as `Msg::Remote` |
| `src/remote/server.rs` | Listener, HTTP, WebSocket upgrade, handshake, pings, per-connection tasks |
| `src/remote/auth.rs` | The password, sign-in pauses, token generation and hashing, `remote.json` |
| `src/remote/wire.rs` | Serde types of every message above |
| `src/remote/view.rs` | `State`/`Block` projection from `&App` and the diff tracker |
| `src/remote/fs.rs` | Host directory functions |
| `src/remote/web/` | `index.html` (the whole client: HTML, CSS, JS), `manifest.webmanifest`, `icon.svg`, `icon.png`, embedded with `include_str!` / `include_bytes!` |
| `src/app/remote.rs` | `App` side: `/remote` commands, mode changes, local gating, intents, `sync_remote` |
| `src/ui/views/remote.rs` | The remote status screen; composer placeholder and footer mark live with the composer and footer |

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
| Wrong passwords | 5 in a row → 30 s pause, doubling per further 5, at most 15 min |
| Remembered phones | 20 |
