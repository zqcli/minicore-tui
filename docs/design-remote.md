# Remote mode design

Remote mode lets phones drive this TUI instance. The TUI stays the only host:
it owns the Agent child, serves a small web client and one WebSocket per
connected device, and turns what a phone asks for into the same reducer
operations the keyboard uses. The Agent and the Runtime do not change.

This document is the contract for the implementation. Where the code and this
document disagree after the feature lands, the code wins and this file is fixed.

## Decisions

- **Intents in, projection out.** The phone never sends Agent RPC. It sends a
  closed set of intents that the reducer executes like local actions and a
  closed set of queries the host relays as bounded Agent reads for its own view,
  and it receives a projection of its own session or draft built from `App` state:
  a snapshot on connect or view change, then changed blocks (by block revision) and state
  changes. Turn lifecycle, reconciliation, approvals, follow-ups and titles stay
  in the Rust reducer only.
- **Several devices.** Remote mode is `Off`, `Waiting` (listening, local input
  frozen) or `Connected` (at least one phone connected). Every signed-in device
  is admitted and every connected device has its own view. A new connection from
  the *same* device (the same
  token) replaces that device's older one with `bye replaced`, so a phone that
  lost its network reconnects before the old socket times out and a second tab
  does not count twice. There is no separate cap: at most 20 devices are
  remembered, which bounds the distinct devices that can be connected.
- **Independent views.** Admission copies the current foreground: its session
  id, or its draft workspace and launch settings (profile, model and reasoning).
  Opening or creating a session and starting a draft change only that device's
  view. Devices never follow each other; two devices viewing the same session
  see the same data and may both act on it.
- **Focus before acting.** One helper brings the acting device's view to the
  foreground, then runs the existing reducer paths. A live background session
  switches instantly; a draft parks the foreground and loads that device's
  workspace and launch settings. After a draft intent those settings are saved
  back to the device and `App::launch` is cleared, so an opened session can never
  adopt another device's draft overrides. The foreground draft keeps a copy for
  the status screen and for returning to local control. Non-live opens remember
  the requesting connection; adoption changes only its view. Creation does the
  same and sends the first message to the newly adopted session.
  The TUI foreground is the view of the device that acted last; an asynchronous
  open or create takes the foreground when it completes. `/remote off` shows
  that foreground, not a separate local session.
- **The TUI only reports.** While remote mode is on, the TUI does not show the
  conversation: its transcript area becomes a status screen saying that remote
  control is on, how to connect, which phones are connected, which session and
  workspace are open and what the Agent is doing now. The reducer keeps the full
  session state for the phones; turning remote mode off shows the transcript again.
- **Password sign-in.** Turning remote mode on needs nothing. A phone signs in
  with the connection password the status screen shows, once per device; after
  that it reconnects with a remembered token.
- **Best effort.** No event log, no replay, no automatic resend. Reconnecting
  gives a fresh snapshot.
- **Web client first.** The TUI serves a web client (plain files, no build step) that installs
  as a home-screen app and opens offline from a service worker cache.
  The protocol below is versioned (`v: 2`) and is the interface a future native
  or Flutter client uses too, so it carries semantic data, never rendered rows.
- **Optional TLS with a user certificate.** Default bind is loopback on both
  families. `tls = true` serves HTTPS and WSS with a PEM certificate and key the
  user provides (Let's Encrypt, a Cloudflare Origin CA certificate, a
  self-signed one the phone trusts); the TUI never makes or renews
  certificates. Otherwise use Tailscale, WireGuard, an SSH tunnel or a TLS
  reverse proxy across networks; `url` names the address the phone uses.
- **Not in scope:** certificate issuance or renewal, HSTS and HTTP-to-HTTPS
  redirects, sleep prevention, per-device revocation, push
  notifications, delete/move/upload in the directory browser, Git clone,
  `agent.reload` and `agent.shutdown` from the phone.

## Configuration and command line

`config.toml`:

```toml
[remote]
bind = ["127.0.0.1:8765", "[::1]:8765"]   # default; one host:port or a list
url = "https://mac.tail1234.ts.net:8765"   # optional; the address the status screen shows
roots = ["~/Develops", "/srv/work"]        # default: the user's home directory
allow_insecure = false         # required (true) for a non-loopback bind without TLS
tls = false                    # HTTPS and WSS with the files below
tls_cert = "~/certs/fullchain.pem"         # PEM chain, leaf first
tls_key = "~/certs/privkey.pem"            # PEM key: PKCS#8, PKCS#1 or SEC1
```

- Validated when `config.toml` loads, reported as `file:line: [remote] key: reason`
  with exit status 2 before the screen changes:
  - `bind` is a string or a non-empty list of strings without repeats; each
    parses as a socket address. A non-loopback IP without TLS needs
    `allow_insecure = true` ("<address>: a non-loopback bind sends traffic
    unencrypted; set tls = true or allow_insecure = true").
  - `tls = true` needs `tls_cert` and `tls_key` ("needs tls_cert and tls_key");
    both expand a leading `~` like `roots`, and are loaded and parsed here, so a
    missing file, a file without PEM items or a key that does not match the
    certificate is `[remote] tls_cert: <path>: <reason>` or `tls_key: …`. With
    `tls = false` the paths are ignored.
  - `roots` entries expand a leading `~`, must exist, must be directories, and are
    stored canonicalized. An empty list is an error.
  - `url` must start with `http://` or `https://`; no trailing `/`.
- `--remote` starts remote mode at startup. The listener is bound before the
  terminal is taken over; a bind failure prints the error and exits with status 1.
  Skipped addresses (below) are shown as notices once the screen is up.
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
| `/remote off` | Say `bye` to every phone, close them, stop listening, show the transcript and unfreeze local input |
| `/remote` or `/remote status` | When on: nothing to open, the status screen already shows everything (close any open picker or view). When off: the info notice `Remote control is off · /remote on starts it` |
| `/remote password` | Generate a new password |
| `/remote password <text>` | Use `<text>` (at least 8 characters, spaces and dashes not counted) |

Changing the password signs every phone out: the remembered devices are
forgotten, every connected phone gets `bye signed_out`, and the notice
`New remote password · phones must sign in again` is shown. While remote mode is
off the password still changes (it is in the file). A `/remote password …`
line is never written to the prompt history.

## Local behaviour in remote mode (`Waiting` or `Connected`)

- **The status screen** replaces the transcript area (a full-screen view such as
  help or diff, opened from the allowed commands, still draws over it). It is
  read-only, not selectable, and the wheel and clicks over it do nothing. Rows,
  label column dim, values in text colour:

  ```text
   Remote control                                 ● iPhone Safari connected
                                                  (● 2 phones connected /
                                                   ○ waiting for a phone, amber)

     Address     http://192.168.20.207:8765
     Listening   0.0.0.0:8765 · [::]:8765
     Password    k7mq-x2pa-n9dt
     Phones      iPhone Safari · Fix the parser · connected at 14:02
                 Mac Edge · new session · connected at 14:30
                 (none yet · open the address on your phone and enter the password)

     Session     Fix the parser          (New session · starts with the phone's first message)
     Workspace   ~/Develops/app
     Model       gpt-6-luna · max
     Now         ⠋ Running bash · cargo test · 12s
                 (⠋ Thinking · 4s / ⠋ Waiting for approval on the phone · bash · rm -rf build /
                  ⠋ Compacting context / Idle · last turn completed in 7.8s / Idle)
     Queue       2 messages waiting                       (only when the queue is not empty)
     Others      2 running · 1 needs approval             (only when background sessions work)
     Sign-in     3 wrong passwords · paused for 30 s      (only after a failure; amber)

     /remote off take back control · esc stop the turn · /remote password new password
  ```

  `Listening` names every address the listener accepts on, joined with ` · `,
  then `TLS` when it serves TLS. `Phones` has one row per connected device, in admission order, with its
  session label (or `new session`) and the local time it was admitted
  (a clock time, not a running duration, so the
  idle screen stays still); the header names the one device or counts them.
  The home directory reads `~`. Values are cut to the width (never wrapped);
  the label column is the widest label plus two cells. Hints that do not fit
  give way whole, from the end. `Now` uses the Working line's spinner and the
  turn's elapsed whole seconds (`4s`, `3m07s`), and animates only while the TUI
  already animates (`App::animating`); an idle screen does not tick, so the
  sign-in pause countdown refreshes with the next frame. `Session`, `Workspace`,
  `Model` and `Now` still describe the foreground, the last device that acted.
- The dock in remote mode shows notices, the editor and the footer only: no
  Working line, no queue rows and no approval sheet (the status screen says it).
  The editor keeps its draft and slash input but cannot send; an empty editor
  shows the faint placeholder `Remote control is on · /remote off to take back`.
  The footer shows `remote` (waiting), `remote · <device>` (one phone) or
  `remote · 2 phones`.
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
  Quitting says `bye` to every phone first.
- When the last phone disconnects the mode goes back to `Waiting`; local input
  stays frozen until `/remote off`.

## Transport

One TCP listener per `bind` address, all served by one accept loop with the
limits below. IPv6 sockets are v6-only (`IPV6_V6ONLY`), so `0.0.0.0:P` and
`[::]:P` coexist; Unix sockets set `SO_REUSEADDR` as std does, Windows ones do
not (there it would let another process take the port). A bind failing with
`AddrInUse` or `PermissionDenied` aborts the start: the port must stay the one
configured. Any other failure (no IPv6, an address the host no longer has)
skips that address with the info notice
`Remote: not listening on <address> (<reason>)`, as long as one address listens;
otherwise the first error, naming its address, fails the start. The default URL
uses the first address that listens.

With TLS (rustls with the ring provider, passed to the server configuration
rather than installed process-wide; TLS 1.2 and 1.3; ALPN `http/1.1`; no client
certificates) every accepted connection completes its handshake within 10 s
first; a failed handshake, such as plain HTTP to the TLS port, closes the
connection without a reply. The certificate and key are read again at every
start (`--remote`, `/remote on`), so `/remote off` and `/remote on` pick up a
renewed certificate; files are not watched. The default URL then starts with
`https://`.

Each connection's request head (max 8 KiB, 10 s) is read up to its blank line;
plain TCP and TLS streams go through the same handler:

- `GET /` → the web client's page (`text/html; charset=utf-8`): markup only.
- `GET /app.css` → its styles (`text/css`); `GET /theme.js` → the small classic
  script that sets the theme before the first paint; `GET /main.js` and the
  ES modules it imports (`core.js`, `ui.js`, `net.js`, … `text/javascript`),
  all listed with `modulepreload` in the page so they load in parallel.
- `GET /sw.js` → the service worker (`text/javascript`).
- `GET /manifest.webmanifest` → the app manifest (`application/manifest+json`).
- `GET /icon.svg`, `/icon-192.png`, `/icon-512.png`, `/icon-maskable.png`
  (full-bleed for Android masks) and `/apple-touch-icon.png` (180×180, opaque,
  for the iOS home screen) → the icons, embedded with `include_bytes!`.
- `GET /ws` with `Upgrade: websocket` → the WebSocket; the bytes already read
  are replayed to the WebSocket handshake. If an `Origin` header is
  present its host:port must equal the `Host` header, else `403`. No `Origin`
  (a non-browser client) is allowed; authentication still applies.
- Anything else → `404`. Only `GET`. Paths are matched without their query
  string, so `/?source=pwa` is the page and `/ws?x` the socket.

Every HTTP response carries `X-Content-Type-Options: nosniff`,
`Referrer-Policy: no-referrer` and
`Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'`.
The client has no inline script, `<style>` or `style=` attribute, so nothing
inline is allowed. `'self'` covers the service worker and the manifest through
`default-src`, and the same-host `ws:` and `wss:` socket in current browsers
(WebKit since 2022).

The client's files carry `Cache-Control: no-cache, no-transform` and a strong
`ETag`: the first 16 hex digits of the SHA-256 of the embedded bytes, computed
once. A request whose `If-None-Match` lists that validator (a `W/` prefix is
accepted) gets `304 Not Modified` with the headers and no body, so reloading
over a slow link costs one small round trip per file instead of the files. The
gzipped copy is another representation and has its own validator (`"<hash>-gzip"`).
The socket's upgrade reply and the `403` and `404` replies carry
`Cache-Control: no-store, no-transform`. `no-transform` stops proxies such as
Cloudflare from rewriting the page (email obfuscation, Rocket Loader, injected
beacons) or recompressing it, so the server compresses itself: the page, service
worker, manifest and SVG icon go out gzipped (made once, on first request) to
clients whose `Accept-Encoding` allows gzip, with `Vary: Accept-Encoding`; PNGs are
sent as they are. A reply is written in 4 KiB pieces and dropped only when one
piece makes no progress for 10 s, so a slow proxy hop can take minutes without
being cut off. A connection stays open for the next request (HTTP/1.1
keep-alive): requests are answered in turn until one carries `Connection: close`,
is not a `GET` or comes over HTTP/1.0 (that reply says `Connection: close` and
ends the connection), or the client sends nothing for 10 s. A socket upgrade may
follow plain requests on the same connection. So a first load reuses a browser's
few connections instead of opening a TCP (and TLS) handshake per file; the 32
unauthenticated connections the server admits still cover the worst case.

The service worker (`sw.js`, secure contexts only: `https://` or `localhost`)
precaches every client file except itself, and answers same-origin `GET`s network
first: a successful response refreshes the cache, while a network failure or an
error status (a tunnel's error page while the computer sleeps) serves the cached
copy. The next online load therefore always runs what the TUI serves; a client
older than the server still meets the `v` check in the handshake. It never
handles `/ws` or other origins, activates at once and deletes older caches.

### Through Cloudflare

A Cloudflare Tunnel is the simplest public route: `cloudflared` sends the
public hostname to `http://127.0.0.1:8765`, so the default loopback bind stays
and `allow_insecure` is not needed; set `url` to the `https://` hostname. The
browser's `Origin` is `https://<hostname>` and cloudflared forwards
`Host: <hostname>`, so the Origin check passes. Overriding the tunnel's HTTP
Host Header breaks it (`403`); `X-Forwarded-*` headers are never trusted.
A proxied DNS record to a forwarded port needs a non-loopback bind and a port
Cloudflare proxies; with `tls` and a Cloudflare Origin CA certificate the zone's
**Full (strict)** mode encrypts and verifies the hop to the computer, without
it `allow_insecure = true` is needed and that hop is plain HTTP. Behind a
tunnel TLS is optional; enabled, the tunnel's service is `https://127.0.0.1:8765`
with `originServerName` (or `noTLSVerify`) when the certificate does not name
`127.0.0.1`. The 20 s ping keeps idle sockets well inside
Cloudflare's WebSocket idle timeout.

WebSocket rules: text frames of JSON objects with a `"t"` type field; max
message 1 MiB; the first message must arrive within 10 s; at most 8
unauthenticated connections at once; the server sends a WebSocket ping every
20 s and closes a connection that has sent nothing (pong included) for 60 s.
Each authenticated connection has an outbound queue of 512 messages; when it is
full the TUI drops that connection alone (the phone reconnects and gets a
snapshot); the others are unaffected.
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
  The phone is stored only after the reducer admits it. The reducer refuses
  only a handshake that reaches a listener being stopped; that connection gets
  `bye remote_off` and nothing is stored. The web client keeps the token in `localStorage` (`minicore.token`)
  and later signs in with `hello`. The server compares SHA-256 hashes.

## Protocol v2

### Handshake

Device names contain 1–64 Unicode characters and no control characters; invalid
names are `bad_request` and do not count as a wrong password.

Client's first message, one of:

```json
{"t":"login","v":2,"password":"k7mq-x2pa-n9dt","device":"iPhone Safari"}
{"t":"hello","v":2,"token":"<base64url>"}
```

Server answers, in order:

```json
{"t":"authorized","token":"<base64url>"}                  // only after a login
{"t":"welcome","v":2,"version":"0.6.0","device":"iPhone Safari","now_ms":1761300003000}
{"t":"snapshot","state":{...},"blocks":[...]}
```

`welcome.now_ms` is the host's Unix time in milliseconds when that message is
built. The client samples its own clock on receipt to estimate the host/phone
offset, refreshing that offset on each handshake, and uses host-adjusted time
with `status.started_ms` for elapsed time. `now_ms` appears only in `welcome`,
never in snapshots or `State`, so clock passage alone causes no state updates.

On failure, the server sends one error and closes:
`{"t":"error","code":"wrong_password"|"locked"|"unauthorized"|"version"|"bad_request","message":"..."}`.
`wrong_password`: the login password is wrong. `locked`: sign-in is paused; the
error carries `"retry_after":<whole seconds, rounded up>`. `unauthorized`: the
`hello` token is unknown (signed out); the client forgets it and asks for the
password. `version`: `v` is not 2 (a v1 client must reload). Another connected device never refuses a
sign-in.

### Client → server (after the handshake)

```json
{"t":"intent","id":"c17","kind":"submit","text":"..."}
{"t":"intent","id":"c18","kind":"queue","text":"..."}
{"t":"intent","id":"c19","kind":"cancel"}
{"t":"intent","id":"c20","kind":"answer","interaction_id":"...","choice":"allow"}
{"t":"intent","id":"c31","kind":"answer","interaction_id":"...","choice":"deny","message":"Use the staging bucket"}
{"t":"intent","id":"c32","kind":"compact"}
{"t":"intent","id":"c33","kind":"unqueue","index":1,"text":"run the tests too"}
{"t":"intent","id":"c21","kind":"load_older"}
{"t":"intent","id":"c22","kind":"open_session","session_id":"ses_..."}
{"t":"intent","id":"c23","kind":"new_session","workspace":"/Users/me/Develops/app","profile":"coding"}
{"t":"intent","id":"c24","kind":"set_model","model":"gpt-6-luna"}
{"t":"intent","id":"c25","kind":"set_reasoning","level":"high"}
{"t":"intent","id":"c29","kind":"rename_session","session_id":"ses_...","title":"Fix the parser"}
{"t":"intent","id":"c30","kind":"delete_session","session_id":"ses_..."}
{"t":"fs_roots","id":"c26"}
{"t":"fs_list","id":"c27","path":"/Users/me/Develops","hidden":false}
{"t":"fs_mkdir","id":"c28","parent":"/Users/me/Develops","name":"new-app"}
{"t":"query","id":"q1","kind":"context"}
{"t":"query","id":"q2","kind":"changes"}
{"t":"query","id":"q3","kind":"diff","path":"src/parser.rs"}
{"t":"query","id":"q4","kind":"tool_output","call_id":"call_...","offset":0}
{"t":"query","id":"q5","kind":"files","query":"pars"}
```

`id` is a client string of at most 64 bytes, echoed in the answer. Unknown `t`
or `kind`, or missing fields, answer `ack`/`fs`/`result` with `ok:false` and
`error:"bad_request"`; the connection stays open. An invalid or oversized `id`
is returned as the empty string, never echoed without its 64-byte bound.

Intent semantics (each is acknowledged once the reducer has accepted and
dispatched it, or refused it):

| kind | Behaviour | Refused when |
|---|---|---|
| `submit` | In this device's draft: create one in its workspace with its launch settings and this first message. Idle session: send. Running: steer. Only the creating device changes view on adoption. The text is read as the composer reads Enter (below). | a built-in command or a bare unknown `/word` (`"unknown_command"`); empty text, also after expansion; pending-open/create rules below |
| `queue` | In this device's running session: queue a follow-up. Otherwise as `submit`. | as `submit` |
| `cancel` | Interrupt the running turn, else cancel a running compaction, else nothing. | pending-create rule below |
| `compact` | `/compact`'s path: `session.compact` for this device's session. | a draft (`"No session"`); a running turn or a compaction under way (`"busy"`); pending-create rule below |
| `unqueue` | Removes the queued follow-up at `index` of `state.queue` and sends its text back as `returned` to the requester only (never to the local composer). | the entry is not a follow-up (steers already sent cannot be taken back), is missing, or its text differs from `text`, and in a draft (`"stale"`); pending-create rule below |
| `answer` | Approvals accept only `"allow"`, `"allow_session"`, or `"deny"`; choice prompts accept only an in-range row index. Goes through the local answer path, so session allowance is remembered. An optional `message` with `"deny"` is steered (`turn.steer`, as a running turn's Enter does, trailing whitespace trimmed) into the same turn right after `interaction.answer`, so the agent hears why. | no pending interaction or id differs (`"stale"`); wrong answer kind or index, or a `message` that is blank or beside anything but an approval's `"deny"` (`"bad_request"`), leaving the interaction untouched; pending-create rule below |
| `load_older` | Load an older history page into this device's session transcript. | no session; pending-create rule below |
| `open_session` | Change only this device's view: a live session switches at once (no `session.open`), another is opened and assigned on adoption. | unknown id; workspace outside `roots`; pending-open/create rules below |
| `new_session` | Change only this device's view to a draft in `workspace` (default: its viewed workspace), with optional `profile` and that profile's/default settings; the next `submit` creates it. No empty session is created. | workspace relative, outside `roots`, missing or not a directory; unknown profile; pending-open/create rules below |
| `set_model` / `set_reasoning` | As `/model` and `/reasoning`: a session update persists for that session; a draft changes only this device's launch settings. Draft notices go only to this device. | unknown model or level; pending-create rule below |
| `rename_session` | Rename any listed session through the sessions picker's rename path (`session.rename` with its id); the title is trimmed, and an empty title clears it so the label falls back to the first prompt or short id. The catalog, the live session and the phones' states follow; no notice is sent, the new title is the confirmation. | unknown id; workspace outside `roots`; title over 200 characters or containing control characters; pending-create rule below |
| `delete_session` | Delete permanently through the local path (`session.close` when loaded, then `session.delete`); it leaves the catalog and the lists. The phone confirms before sending. | unknown id; workspace outside `roots`; viewed by the requester (`"Open another session before deleting this one"`) or another device (`Open on "<device name>"`); the existing foreground or working-live-session refusal; pending-create rule below |

`loading_session` remains global. While an open is pending, another non-live
open, `new_session`, or draft `submit`/`queue` is refused; live-session intents
and live opens still work. While a create's `first_message` is pending, **all
intents** are refused, including live-session intents: the first text and the
local composer are global until adoption, so focusing another view would steal
the creating draft. Filesystem requests are unaffected. The owner receives
`"A session is opening or being created"`; other devices receive
`"Another device is opening a session"`. Only the owner's `status.loading` is
true. A failed create returns its text only to its connected creator and keeps
that device's draft settings.

Deletion cannot normally invalidate a view: viewed sessions cannot be deleted,
and `/remote off` disconnects every phone before local deletion is possible.
There is one in-flight race: a device can open a session after its deletion
starts. Successful deletion resets any such views to drafts with the app's
workspace and default launch settings, resets their trackers, and removes the
live session even if it became the foreground meanwhile. If a non-live open
reply arrives after its target was deleted, it is discarded with the notice
`Session deleted while opening`; the device keeps its previous view.

`submit` and `queue` read the text with the composer's rules and code
(`commands::parse`/`execute`, `App::prompt_text`): a line whose first word
names a built-in command (`/compact`, `/model x`, `/rename`, also without its
argument) is refused with `unknown_command`, since the client runs those
itself; `/name args` naming a prompt template is expanded on the host (`$1`,
`$@`, `$ARGUMENTS`, `${1:-default}`, quoted arguments); `//text` sends `/text`;
a bare `/word` naming nothing (`/reveiw`) is refused with `unknown_command`,
where the composer warns once. Anything else is sent unchanged: a path
(`/usr/bin is…`), `/word` followed by other text (`/reveiw the parser`), a slash
later in the text, `/9lives`, `/ hello`. Then trailing whitespace is trimmed
like local sends.
A refused `submit` or `queue` text is never put into the local composer. When the
Agent later refuses a send (the path that calls `restore_draft` today) while a
phone is connected, the text goes back as `returned` (see the routing below).

Any admitted connection may send intents and filesystem requests; requests
from a connection that is no longer admitted (replaced, dropped, or from a
stopped listener) are ignored. Two devices answering the same approval: the
first answer wins, the second gets `ack` with `error:"stale"` and the client
says `Already answered` while the next `state` closes its sheet.

### Queries

A query relays one bounded Agent read for the requesting device's own view (its
session; a draft answers `"No session"`) and is answered only to that
connection as `result`, once the Agent replies. Queries do not focus the view,
so they never move the TUI foreground, and the pending-open/create rules do
not apply to them. The reducer counts each connection's queries waiting on the
Agent: a fifth one is answered `"busy"` at once, and each reply frees its slot,
also when its connection has gone (the reply is then dropped). A query whose
session left the device's view still answers; the client ignores stale
replies by id. Agent errors are passed on as their message.

```json
{"t":"result","id":"q1","ok":true,"data":{...}}
{"t":"result","id":"q1","ok":false,"error":"busy"|"stale"|"No session"|"bad_request"|"<Agent message>"}
```

| kind | fields | Agent RPC | refused when |
|---|---|---|---|
| `context` | — | `session.context` | — |
| `changes` | — | `changes.list` with `/diff`'s parameters (`scope:"workspace"`, `limit:200`, `max_bytes:262144`) | — |
| `diff` | `path` | that same `changes.list`, then `changes.diff` (paged to the end, as the diff view reads) for every record of `path` | `path` is not in the list (`"stale"`) |
| `tool_output` | `call_id`, `offset` (UTF-8 byte offset) | `tool.output` with the call's full identity from the viewed transcript, `stream:"output"`, `max_bytes:65536` | no tool block with that `call_id` in the viewed transcript (`"stale"`, before any RPC) |
| `files` | `query` | `workspace.files` with the `@` list's parameters (`recursive:true`, `max_bytes:262144`) and `query`, `limit:50` | — |

`data` per kind:

```json
// context: the figures the /context view shows. tokens/window/percent: the
// last request's report (session.presentation; percent of the input budget,
// else of the window, as the footer); the rest from session.context.
{"tokens":31200|null,"window":64000|null,"percent":78.0|null,"budget":40000|null,
 "history_tokens":370|null,"trigger_tokens":32000|null,"target_tokens":16000|null,
 "compacting":false,"coverage":{"turns":6,"items":12,"kept":3}|null,
 "last_compaction":{"origin":"automatic"|"manual"|null,
                    "status":"compacted"|"noop"|"failed"|"unknown_write"|"unknown",
                    "before":4200|null,"after":900|null}|null,
 "failure":"context_uncompressible"|null}
// changes: one row per path in list order; branch = state.session.branch;
// truncated = more records follow (next_cursor).
{"branch":"main"|null,"files":[{"path":"src/a.rs",
  "status":"added"|"modified"|"deleted"|"renamed"|"conflict"|"unknown"}],"truncated":false}
// diff: the diff view's rows without the line-number gutter: "@@ -a,b +c,d @@",
// "+text", "-text", " text", "" between records, and notes ("── change 1 of 2 ──",
// "No differences.", "Binary file: the diff is not shown.", "… the diff continues").
// added/removed count lines as the diff view's file column; truncated: cut at
// 256 KiB of line text or truncated by the Agent.
{"path":"src/a.rs","lines":["@@ -1,1 +1,2 @@","-old","+new","+more"],"added":2,"removed":1,"truncated":false}
// tool_output: one page; offset is where text starts (past bytes the Agent no
// longer keeps); continue with next_offset until eof.
{"text":"...","offset":0,"next_offset":65536,"eof":false,"truncated":false}
// files: regular files only, workspace-relative, at most 50; truncated: the
// Agent's page is partial, more may match.
{"paths":["src/main.rs"],"truncated":false}
```

The Agent reports no per-category token breakdown, so `context` carries the
`/context` figures instead. `changes` lists no line counts: they need one
`changes.diff` per file, which the `diff` query of the opened file returns
(`added`, `removed`).

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
{"t":"result","id":"q1","ok":true,"data":{...}}
{"t":"bye","reason":"remote_off"|"tui_exit"|"replaced"|"signed_out"}
```

Routing, with several devices connected:

| Message | Goes to |
|---|---|
| `snapshot`, `blocks`, `state` | Each connection's own viewed session or draft, through its tracker |
| Synchronous intent `notice` | Only the requesting connection |
| Asynchronous session `notice` | Plain text to devices viewing that session; `"<label>": text` to other devices |
| Background-attention `notice` (approval or turn end) | Only devices not viewing that session, with its session label |
| Sign-in attempt `notice` (wrong password, sign-in paused) | Every connection |
| Any other TUI notice (key hints, local refusals, `/remote` status, config) | The TUI only |
| `ack`, `fs`, `result` | Only the connection that sent the request |
| `returned` | The connection that last sent `submit`/`queue` **to that session**, if connected; otherwise devices viewing it; otherwise kept in `Session::returned` for local control |
| `bye` | `replaced`: only the older connection of that device; `remote_off`, `tui_exit`, `signed_out`: every connection |

`returned` follows the latest text intent for that session, not the refused
text itself: when two devices send to the same session in quick succession and
the first send is refused, the text goes to the second. A later text intent to
another session cannot take it. Session-retained text stays out of the hidden
local composer during remote focus changes and is restored on local return.

`blocks`: the transcript now has `len` blocks; the client truncates its list to
`len`, then applies each upsert at index `i`. An upsert is exactly one of:

```json
{"i":40,"block":Block}
{"i":41,"append":{"text":"new suffix","reasoning":"new thinking suffix"}}
{"i":42,"append":{"output":["rest of the open line","next line",""]}}
```

`block` replaces the whole block. `append` concatenates each supplied string to
the existing assistant block's corresponding field; `text` and `reasoning` are
independently optional. The server sends `append` only when the last block sent
at that index on this connection and the current block are both streaming
assistants with the same `loop_id` and `request`, and neither text length has
shrunk. Live assistant content is append-only. New, moved, replaced, or finished
blocks use the full `block` form; when streaming ends the full block is sent once.
A running tool's `append.output` continues its `output` the way the TUI's card
grows: the first string is concatenated to the client's last output line (the
line still being written, possibly `""`), every further string is pushed as a
new line. The server sends it only when the last block sent at that index and
the current one are both `running` tools whose `output_dropped` and every other
field (call identity, name, detail, input, exit code, duration, line count,
`truncated`) are unchanged; the tracker keeps a hash of those fields and the
output's line count and last-line length, never the output. When the bounded
tail drops lines from its start, anything else changes, or the tool finishes,
the full `block` is sent; the finished block is always sent once in full.
Snapshots always contain full blocks and replace everything. `app.notice`
uses the routing above; local background notices keep their label prefix.

`State` (sent whole whenever any field changed):

```json
{
  "session": null | {"id":"ses_...","title":"Fix parser","workspace":"/abs","profile":"coding",
                     "model":"gpt-6-luna","reasoning":"high","branch":"main"|null,
                     "allowed":["bash","edit"]},
  "draft": {"workspace":"/abs","profile":"coding"|null,"model":"..."|null,"reasoning":"..."|null},
  "status": {"label":"Thinking"|null,"busy":false,"started_ms":1761300000000|null,
             "interrupting":false,"compacting":false,"loading":false},
  "usage": {"input":0,"output":0,"cached":0},
  "context_percent": 41.5 | null,
  "interaction": null | {"id":"...","kind":"approval"|"choice","tool":"bash","detail":"cargo test",
                         "input":"..."|null,"prompt":"...","options":["Allow once","Deny","Allow bash this session"]},
  "queue": [{"kind":"steer"|"follow","text":"..."}],
  "older": true,
  "sessions": [{"id":"ses_...","title":"...","workspace":"/abs","updated_at":"<RFC 3339>","current":true,
                "live":true,"status":"running"|"approval"|"compacting"|"idle","started_ms":1761300000000|null,
                "unseen":false}],
  "background": {"running":1,"approval":0},
  "devices": [{"name":"iPhone Safari","session":"ses_..."},{"name":"Mac Edge","session":null}],
  "models": [{"id":"gpt-6-luna","reasoning":["low","medium","high"]}],
  "profiles": ["coding","ask"],
  "commands": [{"name":"review","description":"Review staged changes","hint":"[focus]"}]
}
```

- `session`, `draft`, `status`, `usage`, `context_percent`, `interaction`, `queue`
  and `older` describe this device's view, never the foreground by accident.
  `draft.model` and `draft.reasoning` use the same resolved settings as the
  footer, including profile defaults; `draft.profile` is the launch/default
  profile, or the viewed session's profile. Unresolved values remain `null`.
- `label` uses the viewed `Session::status_label()`. `started_ms` is the Unix
  time stored once on its live turn, not derived from `Instant`, so the state
  does not change by itself. `loading` is true only for the opening/creating
  device. `background` counts live sessions other than this device's session,
  including the TUI foreground when it differs: running/compacting and approval.
- `devices`: connected device names and viewed session ids (`null` for a draft)
  in admission order, the receiving one included (the web client's drawer reads `Also connected: Mac Edge`, dropping one entry of its
  own name). Two devices may carry the same name.
- `sessions[].current` marks this device's session. Live marks and catalogs are
  shared. `unseen` clears when any device opens the session, and a turn ending
  in a session any device views does not mark it unseen; the foreground also
  stays seen as before.
- `session.allowed` is the sorted list of tool names the user allowed for this
  session (`allow_session`); their approvals are answered without a notice.
  It is `[]` when there are none.
- `session.branch` is the Git branch the footer shows for that session
  (`Session::branch`: the last `session.presentation` after a
  `workspace.status`, blank names dropped), or `null`. A draft has no session
  and the TUI knows no branch for it.
- `commands` are the prompt templates (`prompt/commands/*.md`): name, the
  description line and the `argument-hint`, sorted by name, shared by every
  device and built with the cached catalog. Sending `/name args` expands one.
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
  branch still synchronizes the phones and clears the pending frame. No extra
  timer is introduced; with no connected phone sync returns at once.
- Each connection has its own tracker (`view::View`): the revisions, streaming
  offsets and serialized `State` last sent to it. One sync refreshes the shared
  catalog cache, groups devices by session id or equal draft settings (and
  loading ownership), then builds and serializes `State` once per distinct
  view. Each tracker diffs the viewed session found through `App::live`, whether
  foreground or background; draft views have no blocks.
- A device view change resets its tracker. A viewed session epoch change or a
  newly admitted connection also sends a fresh `snapshot`. Another device's
  switch never resets this tracker.
- Otherwise the transcript's revisions are compared index by index with the
  revisions last sent: every index whose revision differs, or that is new, is an
  upsert; `len` is the current length. Prepending older pages therefore resends
  the window, which is correct and rare.
- The catalog portions of `State` are cached by the catalog generation, which
  changes on catalog assignment, merge or deletion (and listener generation for
  configured roots). Current-session marks are applied without rebuilding or
  sorting catalogs. The tracker keeps streaming keys, byte and line counts, not copies
  of assistant text or tool output.
- `State` is built, serialized and compared with the last one sent; only a
  difference sends `state`. Elapsed time is computed by the client from
  `started_ms`, so a running turn does not send a `state` every frame.

## Host directory interface

Only for the phone's directory picker; the model cannot call it. Pure, blocking
functions in `remote::fs`, run off the main loop by the connection task (they do
not need `App`). Any admitted connection can request them, and the reply goes
to that connection only. Replacing, dropping or stopping a connection revokes
its own queued filesystem work and suppresses replies from its work already in
progress; other connections are unaffected. A filesystem operation already started while
authorized may finish; it is not rolled back.

Operations:

- `roots`: the configured roots, with `name` = last path component; a root with
  none (`/`, a Windows drive `C:\`) is named like its path.
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
  accepted as input. A drive root is `C:\` (`name` and `path`), its listing has
  `parent: null`, and `mkdir` works directly in it. A `roots` entry `C:`, `C:/`
  or `C:\` is normalized to `C:\` before canonicalization.
- Root checks for sessions compare `SessionInfo::workspace` (the Agent stores it
  canonical) with the canonical roots by `Path::starts_with`, without file IO.
  `new_session` canonicalizes its `workspace` once.

The directory listing is a host convenience and the roots are a UI boundary, not
a sandbox: whoever controls the session can run any command through the Agent's
tools.

## Workspace follows the session

Each phone's draft owns its workspace and launch settings. Focusing that view
loads them into `App`; focusing or adopting a session sets `App::workspace` to
its workspace. `new_session` defaults to the requesting device's viewed
workspace, never another device's. `App::workspace` still supplies the local
draft workspace when remote mode ends. `--continue` uses the original launch
workspace.

## Code layout

| Path | Contents |
|---|---|
| `src/remote/mod.rs` | `RemoteMode`, the handle `App` keeps (start/stop, the admitted phones with their trackers, routed sends, password access, URL, sign-in refusals), `RemoteEvent` posted to the main loop as `Msg::Remote` |
| `src/remote/mod.rs` (`Bound`) | Binding every configured address, skipping unavailable ones, the default URL |
| `src/remote/server.rs` | Accept loop over the listeners, TLS handshake, HTTP, WebSocket upgrade, handshake, pings, per-connection tasks |
| `src/remote/tls.rs` | Loading the PEM certificate chain and key into a rustls server configuration |
| `src/remote/auth.rs` | The password, sign-in pauses, token generation and hashing, `remote.json` |
| `src/remote/wire.rs` | Serde types of every message above |
| `src/remote/view.rs` | `State`/`Block` projection from `&App`, the shared catalog cache and the per-connection diff tracker |
| `src/remote/fs.rs` | Host directory functions |
| `src/remote/query.rs` | Query relays: the Agent reads per kind and the `data` mapping, run off the reducer |
| `src/remote/web/` | The client: `index.html`, `app.css`, `theme.js`, the ES modules, `sw.js`, `manifest.webmanifest` and the icons, embedded with `include_bytes!` (layout in [design-remote-ui.md](design-remote-ui.md#code-layout)) |
| `src/app/remote.rs` | `App` side: `/remote` commands, mode changes, local gating, intents, `sync_remote` |
| `src/ui/views/remote.rs` | The remote status screen; composer placeholder and footer mark live with the composer and footer |

`remote` depends on `app` and `protocol`, never on `ui`. Remote messages reach
the reducer through the existing `Msg` channel (`Msg::Remote`), so the main loop
gains no new branch beyond calling `sync_remote` once per iteration.

## Limits

| Limit | Value |
|---|---|
| TLS handshake | 10 s |
| Request head | 8 KiB, 10 s |
| HTTP reply | dropped after 10 s without progress |
| WebSocket message in | 1 MiB |
| First message | 10 s |
| Unauthenticated connections | 32 |
| Outbound queue per connection | 512 messages, then the connection is dropped |
| Ping / idle close | 20 s / 60 s |
| Directory entries per listing | 1000 |
| Queries waiting on the Agent per connection | 4, then `busy` |
| `diff` text / `tool_output` page / `files` names | 256 KiB / 64 KiB / 50 |
| `changes` records listed | 200 (one `changes.list` page) |
| Sessions in `State` | 50 |
| Wrong passwords | 5 in a row → 30 s pause, doubling per further 5, at most 15 min |
| Remembered phones | 20 (so at most 20 devices connected, one connection each) |
