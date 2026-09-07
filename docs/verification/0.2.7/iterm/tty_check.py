"""Real iTerm2/PTTY checks of remotely compiled normal and panic restoration."""
import json
from pathlib import Path
import shlex
import subprocess
import time

root = Path('/tmp/minicore-027.RXdxEP')
test = root / 'final-artifacts/terminal-restore-0.2.7-macos'
marker = 'UNFLUSHED_TUI_FRAME_MUST_NOT_LEAK'
def osa(source):
    return subprocess.check_output(['osascript', '-e', source], text=True).strip()
window = int(osa('tell application "iTerm2"\nactivate\nset w to (create window with default profile)\nreturn id of w\nend tell'))
results = []
for mode, name, expected in [('normal', 'real_pty_enter_and_restore_round_trip', 0), ('panic', 'child_test', 101)]:
    log = root / f'native-pty-final-{mode}.log'
    body = '[ -t 0 ] && [ -t 1 ] && [ -t 2 ] || exit 2; printf "ALL_STDIO_TTY=yes\\n"; before=$(stty -g); '
    if mode == 'panic':
        body += 'MINICORE_TUI_TERMINAL_RESTORE_CHILD=panic '
    body += shlex.quote(str(test)) + ' ' + name + ' --exact --nocapture' + (' --ignored' if mode == 'normal' else '') + '; rc=$?; '
    body += '[ "$before" = "$(stty -g)" ] || rc=3; printf "\\nTTY_RESULT=%s\\n" "$rc"; exit "$rc"'
    command = '/usr/bin/script -q ' + shlex.quote(str(log)) + ' /bin/sh -c ' + shlex.quote(body)
    osa(f'tell application "iTerm2" to tell current session of window id {window} to write text {json.dumps(command)}')
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        text = log.read_text(errors='replace') if log.exists() else ''
        if f'\nTTY_RESULT={expected}' in text:
            assert 'ALL_STDIO_TTY=yes' in text
            assert marker not in text, f'{mode}: stale frame leaked after restore'
            assert '\x1b[?1049l' in text, 'alternate screen must be left'
            assert 'panic in a destructor during cleanup' not in text
            if mode == 'normal':
                assert '1 passed; 0 failed' in text
            else:
                assert 'panic hook unwind regression child' in text
            results.append({'mode':mode, 'exit':expected, 'stty_unchanged':True, 'unflushed_frame_absent':True})
            break
        time.sleep(0.2)
    else:
        raise RuntimeError(f'TTY check did not finish: window {window}, log {log}')
    time.sleep(0.5)
osa(f'tell application "iTerm2" to close window id {window}')
result = {'status':'PASS', 'all_stdio_tty':True, 'results':results, 'compiled':'remote x86_64 macOS Rust 1.98.0, Clang/LLD 19'}
(root/'native-pty-final-result.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result,indent=2))
