"""Owned native iTerm2 + script PTY terminal-restoration check; no Agent/data."""
import json
from pathlib import Path
import shlex
import subprocess
import time

root = Path('/tmp/minicore-026.cJPXXO')
log = root / 'native-pty.log'
test = Path('/Users/zzq/Develops/minicore-tui/target/debug/deps/terminal_restore-b364a675eadfc028')
def osa(source):
    return subprocess.check_output(['osascript', '-e', source], text=True).strip()
window = int(osa('tell application "iTerm2"\nactivate\nset w to (create window with default profile)\nreturn id of w\nend tell'))
body = '[ -t 0 ] && [ -t 1 ] && [ -t 2 ] || exit 2; printf "ALL_STDIO_TTY=yes\\n"; before=$(stty -g); '
body += shlex.quote(str(test)) + ' real_pty_enter_and_restore_round_trip --ignored --exact --nocapture; rc=$?; '
body += '[ "$before" = "$(stty -g)" ] || rc=3; printf "\\nTTY_RESULT=%s\\n" "$rc"; exit "$rc"'
command = '/usr/bin/script -q ' + shlex.quote(str(log)) + ' /bin/sh -c ' + shlex.quote(body)
osa(f'tell application "iTerm2" to tell current session of window id {window} to write text {json.dumps(command)}')
deadline = time.monotonic() + 30
while time.monotonic() < deadline:
    text = log.read_text(errors='replace') if log.exists() else ''
    if '\nTTY_RESULT=0' in text:
        assert 'ALL_STDIO_TTY=yes' in text and '1 passed; 0 failed' in text
        print('PASS: real iTerm2; all three stdio handles are TTYs; terminal-restoration test passed; stty unchanged')
        osa(f'tell application "iTerm2" to close window id {window}')
        break
    time.sleep(0.2)
else:
    raise RuntimeError(f'TTY check did not finish successfully; owned window {window}, log {log}')
