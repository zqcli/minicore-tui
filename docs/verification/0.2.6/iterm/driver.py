"""One-off native iTerm2 verification; exclusively synthetic temporary data."""
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(tempfile.mkdtemp(prefix='minicore-026-iterm-'))
WORK = ROOT / 'workspace'
OUT = ROOT / 'evidence'
WORK.mkdir()
OUT.mkdir()
REPO = Path('/Users/zzq/Develops/minicore-tui')
TUI = Path(os.environ.get('MINICORE_VERIFY_TUI', str(REPO / 'target/debug/minicore-tui')))
AGENT = REPO.parent / 'minicore-agent/target/debug/minicore-agent'
sys.dont_write_bytecode = True
sys.path.insert(0, str(REPO / 'scripts'))
from stage7_loopback_model import completed_event
requests = []
release_alpha = threading.Event()
release_busy = threading.Event()
LONG_TEXT = '\n\n'.join(f'## Performance section {i}\n\n' + '中文性能验证 mixed width 👩🏽‍💻 👨‍👩‍👧‍👦 🇨🇳 e\u0301 **样式** `code` [链接](https://example.invalid). ' * 20 for i in range(80)) + '\n\nLONG_HISTORY_END'

class Provider(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        inputs = body.get('input', [])
        serialized = json.dumps(inputs)
        record = {'model': body.get('model'), 'effort': body.get('reasoning', {}).get('effort'),
                  'alpha': 'steer-alpha' in serialized, 'beta': 'steer-beta' in serialized,
                  'alpha_answer': 'DONE_ALPHA' in serialized}
        requests.append(record)
        number = len(requests) - 1
        if number == 0:
            summaries = ['Planning phase', 'Detailing scope', 'Analyzing constraints']
            reasoning = {'type': 'reasoning', 'id': 'rs_native', 'status': 'completed',
                         'summary': [{'type': 'summary_text', 'text': text} for text in summaries]}
            command = "printf 'TOOL_RAN\\n' > tool-ran; while [ ! -f release-tool ]; do sleep 0.1; done; printf 'TOOL_DONE\\n'"
            arguments = json.dumps({'command': command})
            tool = {'type': 'function_call', 'id': 'fc_native', 'call_id': 'native-bash',
                    'name': 'bash', 'arguments': arguments, 'status': 'completed'}
            events = [{'type': 'response.output_item.added', 'output_index': 0,
                       'item': dict(reasoning, status='in_progress', summary=[])}]
            for summary_index, text, metadata in [(0, 'Plan', True), (0, 'ning', False),
                                                  (0, ' phase', True), (1, summaries[1], True),
                                                  (2, summaries[2], True)]:
                event = {'type': 'response.reasoning_summary_text.delta', 'delta': text}
                if metadata:
                    event.update(item_id='rs_native', output_index=0, summary_index=summary_index)
                events.append(event)
            events += [
                {'type': 'response.output_item.done', 'output_index': 0, 'item': reasoning},
                {'type': 'response.output_item.added', 'output_index': 1, 'item': dict(tool, arguments='', status='in_progress')},
                {'type': 'response.function_call_arguments.delta', 'output_index': 1, 'item_id': 'fc_native', 'delta': arguments},
                {'type': 'response.function_call_arguments.done', 'output_index': 1, 'arguments': arguments},
                {'type': 'response.output_item.done', 'output_index': 1, 'item': tool},
                completed_event(9, [reasoning, tool]),
            ]
        else:
            if number == 1 and not release_alpha.wait(45):
                raise RuntimeError('native test did not release request alpha')
            if number == 4 and not release_busy.wait(55):
                raise RuntimeError('native test did not release busy request')
            text = 'DONE_ALPHA' if number == 1 else ('DONE_BETA' if number == 2 else ('BUSY_COMPLETE' if number == 4 else LONG_TEXT))
            message = {'type': 'message', 'id': f'msg_{number}', 'role': 'assistant',
                       'status': 'completed', 'content': [{'type': 'output_text', 'text': text, 'annotations': []}]}
            events = [
                {'type': 'response.output_item.added', 'output_index': 0, 'item': dict(message, status='in_progress', content=[])},
                {'type': 'response.output_text.delta', 'output_index': 0, 'content_index': 0, 'item_id': message['id'], 'delta': text},
                {'type': 'response.output_item.done', 'output_index': 0, 'item': message},
                completed_event(0, [message]),
            ]
        payload = ''.join('data: ' + json.dumps(e) + '\n\n' for e in events).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Connection', 'close')
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def osa(script):
    return subprocess.check_output(['osascript', '-e', script], text=True).rstrip('\n')

server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
threading.Thread(target=server.serve_forever, daemon=True).start()
config = 'data_dir = ' + json.dumps(str(ROOT / 'agent-data')) + '\ndefault_profile = "coding"\n'
config += '[profiles.coding]\nmodel = "verify-a"\nreasoning = "high"\nsystem_prompt = "Synthetic native UI verification."\ntools = ["bash"]\nmax_tool_rounds = 4\napproval = "auto"\n'
config += f'''\n[models.verify-a]
provider = "open_ai_responses"
model = "verify-a"
base_url = "http://127.0.0.1:{server.server_port}/v1"
api_key_env = "MINICORE_NATIVE_TEST_KEY"
physical_context_window = 512000
output_budget_tokens = 16384
safety_margin_tokens = 1000
supported_reasoning = ["high", "max"]
supports_tools = true
request_timeout_seconds = 60
'''
(ROOT / 'agent.toml').write_text(config)
window = int(osa('tell application "iTerm2"\nactivate\nset w to (create window with default profile)\nreturn id of w\nend tell'))


def send(text):
    value = ' & '.join(f'(character id {ord(c)})' for c in text)
    osa(f'tell application "iTerm2" to tell current session of window id {window} to write text ({value}) newline NO')


def screen():
    rows = int(osa(f'tell application "iTerm2" to tell current session of window id {window} to get rows'))
    contents = osa(f'tell application "iTerm2" to tell current session of window id {window} to get contents')
    # Resizing can move earlier alternate-screen rows into iTerm scrollback.
    return '\n'.join(contents.splitlines()[-rows:])


def wait(predicate, label, seconds=15):
    deadline = time.monotonic() + seconds
    last = ''
    while time.monotonic() < deadline:
        last = screen()
        if predicate(last):
            return last
        time.sleep(0.1)
    (OUT / 'failure-screen.txt').write_text(last)
    raise AssertionError(label + ': timeout')


def has(*words):
    return lambda text: all(word in text for word in words)


def capture(name):
    text = screen()
    (OUT / (name + '.txt')).write_text(text)
    dimensions = osa(f'tell application "iTerm2" to tell current session of window id {window} to get {{columns, rows}}')
    return {'name': name, 'dimensions': dimensions}

checks = []
captures = []
result = {'terminal': 'iTerm2 ' + osa('tell application "iTerm2" to get version'),
          'window_id': window, 'evidence_dir': str(OUT),
          'tui_sha256': hashlib.sha256(TUI.read_bytes()).hexdigest(),
          'agent_sha256': hashlib.sha256(AGENT.read_bytes()).hexdigest(),
          'pixel_screenshots': 'UNVERIFIED; protected iTerm windows, no screenshot attempt or replacement'}
try:
    osa(f'tell application "iTerm2" to tell current session of window id {window}\nset columns to 100\nset rows to 32\nset name to "MiniCore 0.2.6 native verification"\nend tell')
    command = f'cd {shlex.quote(str(WORK))} && env MINICORE_NATIVE_TEST_KEY=loopback-placeholder {TUI} --agent-bin {AGENT} --agent-config {ROOT / "agent.toml"} --workspace {WORK} --theme dark; rc=$?; printf "\\nVERIFY_EXIT=%s\\n" "$rc"'
    send('/bin/sh -c ' + shlex.quote(command) + '\r')
    wait(has('Create session'), 'new session')
    send('\t\t\t\t\t\r')
    wait(has('verify a', 'ready', 'ctx'), 'active session')
    send('initial verification\r')
    text = wait(lambda t: (WORK / 'tool-ran').exists() and 'Analyzing constraints' in t, 'reasoning and real bash')
    rows = text.splitlines()
    positions = [next(i for i, row in enumerate(rows) if title in row)
                 for title in ('Planning phase', 'Detailing scope', 'Analyzing constraints')]
    assert positions == list(range(positions[0], positions[0] + 3)), positions
    checks.append('same-part tokens concatenate; distinct summary parts without raw newlines occupy three native rows')
    captures.append(capture('01-thinking-part-boundaries'))
    send('steer-alpha\rsteer-beta\r')
    text = wait(has('steer-alpha', 'steer-beta', 'queued 2'), 'both rapid steers admitted')
    assert len(requests) == 1, requests
    rows = text.splitlines()
    working = next(i for i, row in enumerate(rows) if 'Running bash' in row)
    queued = [i for i, row in enumerate(rows) if 'Steering' in row and ('steer-alpha' in row or 'steer-beta' in row)]
    assert len(queued) == 2 and max(queued) < working
    assert any(not rows[i].strip() for i in range(max(queued) + 1, working)), rows
    assert '↪ steer-alpha' not in text and '↪ steer-beta' not in text
    checks.append('two rapid steers appear in dock queue above Working, with a blank gap, not as User cards')
    captures.append(capture('02-two-queued-steers'))
    send('\x1b[1;3A')
    wait(lambda t: 'queued 1' in t and 'Steering: steer-beta' not in t and 'steer-beta' in t, 'withdraw next unsent into editor')
    send('\r')
    wait(has('queued 2', 'steer-beta'), 'withdrawn steer re-admitted')
    checks.append('Option+Up withdraws unsent beta; resubmission keeps both entries and changes only local identity')
    osa(f'tell application "iTerm2" to tell current session of window id {window}\nset columns to 60\nset rows to 16\nend tell')
    wait(has('Steering', 'steer-alpha', 'steer-beta', 'Running bash'), 'narrow queue')
    captures.append(capture('03-queued-60x16'))
    osa(f'tell application "iTerm2" to tell current session of window id {window}\nset columns to 100\nset rows to 32\nend tell')
    (WORK / 'release-tool').touch()
    wait(lambda t: len(requests) >= 2 and 'Steering (accepted): steer-beta' in t, 'alpha consumed and beta accepted for later request')
    assert requests[1]['alpha'] and not requests[1]['beta'], requests
    captures.append(capture('04-alpha-applied-beta-queued'))
    release_alpha.set()
    text = wait(has('DONE_ALPHA', 'DONE_BETA', 'ready'), 'both instructions answered', seconds=25)
    text = wait(lambda t: 'DONE_BETA' in t and 'queued' not in t and 'Steering' not in t, 'queue reconciled')
    assert len(requests) == 3 and requests[2]['beta'] and requests[2]['alpha_answer'], requests
    assert text.count('steer-alpha') == 1 and text.count('steer-beta') == 1, text
    checks.append('request alpha excludes beta; request beta follows alpha answer; both history entries appear exactly once')
    captures.append(capture('05-both-completed'))
    send('/reasoning\r')
    wait(has('Select reasoning', 'max'), 'reasoning selector')
    send('\x1b[B\r')
    wait(has('· max ·', 'ready'), 'footer updates immediately')
    assert len(requests) == 3
    checks.append('existing immediate-footer max selection remains intact without another user turn')
    captures.append(capture('06-footer-max'))
    send('long history performance fixture\r')
    wait(has('LONG_HISTORY_END', 'ready'), 'long history landed', seconds=30)
    assert len(requests) == 4 and requests[3]['effort'] == 'max'
    captures.append(capture('07-long-history-tail'))
    pid_rows = subprocess.check_output(['ps', '-ww', '-axo', 'pid=,ppid=,command='], text=True).splitlines()
    owned_rows = [row for row in pid_rows if str(ROOT / 'agent.toml') in row]
    (OUT / 'owned-processes.txt').write_text('\n'.join(owned_rows))
    own_pids = [int(row.split()[1]) for row in owned_rows if len(row.split()) > 2 and Path(row.split()[2]).resolve() == AGENT.resolve()]
    assert len(own_pids) == 1, own_pids
    own_agents = [int(row.split()[0]) for row in owned_rows if len(row.split()) > 2 and Path(row.split()[2]).resolve() == AGENT.resolve()]
    assert len(own_agents) == 1 and own_pids[0] != 15561 and own_agents[0] != 15562
    def cpu_time(pid=None):
        value = subprocess.check_output(['ps', '-p', str(pid or own_pids[0]), '-o', 'time='], text=True).strip()
        seconds = 0.0
        for part in value.split(':'):
            seconds = seconds * 60 + float(part)
        return seconds
    def cpu_sample():
        tui_start = cpu_time()
        agent_start = cpu_time(own_agents[0])
        started = time.monotonic()
        time.sleep(8)
        elapsed = time.monotonic() - started
        tui_seconds = round(cpu_time() - tui_start, 3)
        agent_seconds = round(cpu_time(own_agents[0]) - agent_start, 3)
        return {'wall_seconds':round(elapsed, 3), 'tui_cpu_seconds':tui_seconds,
                'agent_cpu_seconds':agent_seconds, 'tui_cpu_percent_one_core':round(tui_seconds / elapsed * 100, 3)}
    osa(f'tell application "iTerm2" to tell current session of window id {window}\nset columns to 220\nset rows to 55\nend tell')
    send('RESIZE_READY')
    wait(has('RESIZE_READY'), 'wide resize and input drained')
    send('\x7f' * len('RESIZE_READY'))
    time.sleep(0.5)
    result['cpu_dimensions'] = osa(f'tell application "iTerm2" to tell current session of window id {window} to get {{columns, rows}}')
    captures.append(capture('08-cpu-idle-wide'))
    result['idle_cpu'] = cpu_sample()
    send('busy timer performance fixture\r')
    busy_before = wait(lambda t: len(requests) == 5 and 'working' in t, 'provider held while Working animates')
    time.sleep(0.5)
    result['busy_cpu'] = [cpu_sample() for _ in range(3)]
    busy_after = screen()
    assert busy_before != busy_after, 'Working state must visibly advance during the held request'
    captures.append(capture('09-cpu-busy-wide'))
    subprocess.run(['sample', str(own_pids[0]), '3', '1', '-file', str(OUT / 'busy-sample.txt')],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
    send('BUSY_INPUT_READY')
    wait(has('BUSY_INPUT_READY'), 'input remains responsive during held request')
    send('\x7f' * len('BUSY_INPUT_READY'))
    release_busy.set()
    wait(has('BUSY_COMPLETE', 'ready'), 'held request completes normally')
    checks.append('220x55 requested: idle 8s; held real request 3x8s with changing Working display, CPU measured only for owned TUI/Agent; input remains responsive')
    osa(f'tell application "iTerm2" to tell current session of window id {window}\nset columns to 100\nset rows to 32\nend tell')
    send('NARROW_READY')
    wait(has('NARROW_READY'), 'return to scroll fixture dimensions')
    send('\x7f' * len('NARROW_READY'))
    def mouse(code, x, y, count=1, release=False):
        suffix = 'm' if release else 'M'
        sequence = f'[<{code};{x};{y}{suffix}'
        osa(f'tell application "iTerm2" to tell current session of window id {window}\nset payload to ""\nrepeat {count} times\nset payload to payload & (character id 27) & "{sequence}"\nend repeat\nwrite text payload newline NO\nend tell')
    marker = 'PERF_INPUT_READY'
    before_cpu = cpu_time()
    before = time.monotonic()
    mouse(35, 5, 5, count=200)
    send(marker)
    wait(has(marker), 'input after hover burst', seconds=30)
    result['hover_200_input_seconds'] = round(time.monotonic() - before, 3)
    result['hover_200_cpu_seconds'] = round(cpu_time() - before_cpu, 3)
    send('\x7f' * len(marker))
    before_cpu = cpu_time()
    before = time.monotonic()
    mouse(64, 5, 5, count=100)
    send(marker)
    text = wait(lambda t: marker in t and '↑ scroll position' in t and 'LONG_HISTORY_END' not in t, 'wheel scrolling long history', seconds=30)
    result['wheel_100_visible_seconds'] = round(time.monotonic() - before, 3)
    result['wheel_100_cpu_seconds'] = round(cpu_time() - before_cpu, 3)
    send('\x7f' * len(marker))
    captures.append(capture('08-long-history-scrolled'))
    thumb_rows = [i for i, row in enumerate(text.splitlines()) if row.rstrip().endswith('█')]
    assert thumb_rows, text
    mouse(0, 100, thumb_rows[0] + 1)
    mouse(32, 100, 1)
    time.sleep(0.15)
    mouse(0, 100, 1, release=True)
    wait(has('initial verification'), 'scrollbar drag to beginning', seconds=20)
    captures.append(capture('09-long-history-drag-top'))
    checks.append('long history: 200 hover events preserve responsive input; 100 wheel events scroll; scrollbar drag reaches beginning')
    send('\x03')
    time.sleep(0.2)
    send('\x03')
    wait(has('VERIFY_EXIT=0'), 'native clean exit')
    checks.append('native TUI exits zero and restores its shell')
    assert result['tui_sha256'] == hashlib.sha256(TUI.read_bytes()).hexdigest()
    assert result['agent_sha256'] == hashlib.sha256(AGENT.read_bytes()).hexdigest()
    result['status'] = 'PASS'
except Exception as error:
    result['status'] = 'FAIL'
    result['error'] = str(error)
    raise
finally:
    (WORK / 'release-tool').touch()
    release_alpha.set()
    release_busy.set()
    if result.get('status') != 'PASS':
        try:
            for _ in range(3):
                send('\x03')
                time.sleep(0.2)
        except subprocess.CalledProcessError:
            pass
    server.shutdown()
    server.server_close()
    result.update(checks=checks, captures=captures, requests=requests,
                  tui_version=subprocess.check_output([str(TUI), '--version'], text=True).strip(),
                  note='Native iTerm2 screen text and input + real Agent/loopback. No user config or Store; no external provider.')
    (OUT / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))
    if result.get('status') == 'PASS':
        osa(f'tell application "iTerm2" to close window id {window}')
