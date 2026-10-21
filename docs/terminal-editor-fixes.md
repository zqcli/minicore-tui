# Terminal, editor and local-time fixes

## Rendering and the cursor

All platforms share the sequence: hide the hardware cursor, paint the frame,
move to the editor caret, then show it. The renderer returns the desired caret
rather than asking Ratatui to show it before moving it. Paint or positioning
failures do not expose an output cursor in the transcript or Working line.

Windows ANSI/VT output additionally buffers the whole sequence before physical
output. Intermediate backend flushes remain in memory; frame submission writes
synchronized-update begin, frame bytes and synchronized-update end, followed by
one flush. Legacy Windows console commands can bypass `Write`, so that path is
not batched. Linux/macOS retain the unbatched shared drawing path.

ANSI support does not prove synchronized-update (DEC 2026) support. Unsupported
terminals may ignore the markers, and a single `write_all` may contain multiple
underlying writes. The offline transport tests verify command ordering, flush
boundaries and error cleanup, not a live IME composition window.

The editor requests a standard **steady block** cursor on compatible terminals.
It occupies a character cell rather than a thin bar and does not periodically
blink. The terminal default is restored on exit, panic cleanup and external
editor handoff, then the block is re-applied on resume. No terminal profile or
OS-wide cursor setting is changed. A steady cursor does not remove the need for
correct drawing order or the Windows whole-frame mitigation.

## Editor interaction (shared across platforms)

The renderer records only the actual editor body as a hit target. Mouse clicks
and drags share rendering geometry: padding, soft wraps, viewport scrolling,
Unicode display widths and paste-chip projection. Full-screen views and approval
sheets do not allow editing the draft behind them; inline completion rows are
not draft text.

Selections have a fixed anchor and moving caret, independently of transcript
selection. Mouse drag/Shift+click and Shift+navigation extend or shrink them.
Typing, paste and newline replace the range; deletion and undo/redo preserve
whole graphemes and atomic paste chips. Editor drags retain their highlight on
release instead of copying transcript text. Ctrl+C copies a selection and Ctrl+X
cuts it; with no selection their existing clear/quit and copy-answer behaviors
remain. See [keybindings](keybindings.md).

## Paste framing and fallback

Native `Event::Paste` contents are inserted into the draft without interpreting
newlines, trailing Enter or slash commands as actions. Raw bracketed-paste
markers are decoded too: the Crossterm Windows console-record backend otherwise
exposes those markers and the payload as separate keys. Once framed paste starts,
payload gaps do not make its Enter or shortcut-like contents executable.

Windows console-record input additionally collects unframed text/Enter bursts
until a **30 ms quiet window**. A multiline burst becomes one paste, including its
trailing newline. Ordinary Windows characters may consequently wait about 30 ms;
extremely fast typing followed by Enter can conservatively become a draft
newline. Unix input already decodes native paste, so it does not enable this
extra burst buffer or delay ordinary characters.

This is not a perfect detector for arbitrary legacy input: a slow paste without
any boundary markers is indistinguishable from manual typing. Actual terminal
paste shortcuts still require validation. Do not claim that timing alone makes
all unsupported terminals safe.

## System-local user timestamps

Stored timestamps remain UTC instants. Display converts the instant to the
system time zone using Jiff, with no hard-coded UTC+8 adjustment. Windows reports
a zone name but normally has no Unix zoneinfo installation; disabling Jiff's
bundled database made `TimeZone::system()` silently fall back to UTC.

`tzdb-bundle-platform` supplies IANA data where needed (notably Windows); Unix
continues to use system zoneinfo, with concatenated zoneinfo enabled for platforms
such as Android. The integration test renders a production user-message card
without the unit-test UTC snapshot override. It also checks Shanghai UTC+8/date
rollover and New York summer/winter offsets.

## UI simplification and preserved behavior

The empty-editor hint, running-card spinner/live timer, Working elapsed timer,
and footer profile/redundant run states are removed. Running-card animation no
longer invalidates cached cards on every tick; real tool events still do.

The main Working spinner and tool details, completed tool durations, footer
elapsed time/model/reasoning/usage/context, queue and allowed-tool indicators
remain. Actual profiles, approval sheets, authorization and model/network
configuration are unchanged.

## Verification and remaining platform checks

Windows GNU-host checks cover the full offline test suite, formatting, linting,
production local-zone integration, cursor ordering and synchronized-frame error
recovery. Tests do not require model/network requests or a live IME.

Linux/macOS builds and live terminal/IME checks were not performed locally.
Shared editor logic is not Windows-only, but passing its tests on Windows is not
evidence of live correctness on another terminal. Verify mouse targeting,
selection, multiline paste with a trailing newline, steady cursor shape, system
local time and Chinese composition while Working on each intended terminal.
