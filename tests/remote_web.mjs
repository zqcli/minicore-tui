import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const html = readFileSync(new URL('../src/remote/web/index.html', import.meta.url), 'utf8');
const core = html.match(/<script id="core">([\s\S]*?)<\/script>/)[1];

// Only the pure core runs here: no DOM, no network, no timers in its context.
function load() {
  const context = vm.createContext({});
  vm.runInContext(core, context);
  // Round-trip through JSON so values compare against plain objects from this realm.
  const MC = context.MC;
  return { MC, plain: v => JSON.parse(JSON.stringify(v)) };
}

const state = {
  session: { id: 'ses_one', title: 'One', workspace: '/work', profile: 'coding', model: 'model', reasoning: 'high' },
  draft: { workspace: '/work', profile: null, model: null, reasoning: null },
  status: { label: 'Thinking', busy: true, started_ms: 1761300000000, interrupting: false, compacting: false, loading: false },
  usage: { input: 10, output: 2, cached: 1 }, context_percent: 41.5,
  interaction: { id: 'approval', kind: 'approval', tool: 'bash', detail: 'pwd', input: 'pwd', prompt: 'Allow?', options: ['Allow once', 'Deny', 'Allow bash this session'] },
  queue: [{ kind: 'steer', text: 'now' }, { kind: 'follow', text: 'later' }], older: true,
  sessions: [{ id: 'ses_one', title: 'One', workspace: '/work', updated_at: '2026-01-01T00:00:00Z', current: true }],
  models: [{ id: 'model', reasoning: ['low', 'high'] }], profiles: ['coding'],
};
const blocks = [
  { type: 'user', kind: 'steering', text: 'now', at: 1761300000000 },
  { type: 'assistant', loop_id: 'loop', request: 1, reasoning: 'think', text: '**hello**', streaming: true },
  { type: 'tool', call_id: 'call', name: 'bash', detail: 'pwd', input: 'pwd', input_lines: 1, state: 'succeeded', exit_code: 0, duration_ms: 20, output: ['/work'], output_dropped: 0, output_lines: 1, truncated: false },
  { type: 'turn_end', loop_id: 'loop', outcome: 'completed', duration_ms: 7800, requests: 2, tools: 1, usage: { input: 10, output: 2, cached: 1 }, changes: { files: 2, added: 10, removed: 3 } },
  { type: 'compaction', before: 32100, after: 2700, summary: 'summary' },
  { type: 'earlier', hidden_items: 12 },
];

test('the core script has no DOM or network access', () => {
  assert.doesNotMatch(core, /\b(document|window|localStorage|WebSocket|navigator|setTimeout|setInterval)\b/);
});

test('login and hello follow the handshake contract with a bounded device name', () => {
  const { MC, plain } = load();
  assert.deepEqual(plain(MC.login('k7mq-x2pa-n9dt', 'iPhone Safari')), { t: 'login', v: 1, password: 'k7mq-x2pa-n9dt', device: 'iPhone Safari' });
  assert.deepEqual(plain(MC.hello('tok')), { t: 'hello', v: 1, token: 'tok' });
  const ua = {
    'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1': 'iPhone Safari',
    'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/130.0 Mobile/15E148 Safari/604.1': 'iPhone Chrome',
    'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Mobile Safari/537.36': 'Android Chrome',
    'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15': 'Mac Safari',
  };
  for (const [agent, name] of Object.entries(ua)) assert.equal(MC.deviceName(agent), name);
  assert.equal(MC.deviceName(Object.keys(ua)[3], 5), 'iPad Safari');
  for (const agent of [...Object.keys(ua), 'x'.repeat(500), '']) {
    const name = MC.deviceName(agent);
    assert.ok(Array.from(name).length >= 1 && Array.from(name).length <= 64);
    assert.doesNotMatch(name, /[\u0000-\u001f\u007f]/);
  }
});

test('every intent and filesystem request has the contract shape and a bounded unique id', () => {
  const { MC, plain } = load();
  const s = MC.createStore(), I = MC.intents;
  const cases = [
    [I.submit('send'), { kind: 'submit', text: 'send' }],
    [I.queue('later'), { kind: 'queue', text: 'later' }],
    [I.cancel(), { kind: 'cancel' }],
    [I.answer('approval', 'allow'), { kind: 'answer', interaction_id: 'approval', choice: 'allow' }],
    [I.answer('approval', 'allow_session'), { kind: 'answer', interaction_id: 'approval', choice: 'allow_session' }],
    [I.answer('approval', 'deny'), { kind: 'answer', interaction_id: 'approval', choice: 'deny' }],
    [I.answer('choice', 1), { kind: 'answer', interaction_id: 'choice', choice: 1 }],
    [I.loadOlder(), { kind: 'load_older' }],
    [I.openSession('ses_one'), { kind: 'open_session', session_id: 'ses_one' }],
    [I.newSession('/work', 'coding'), { kind: 'new_session', workspace: '/work', profile: 'coding' }],
    [I.newSession(), { kind: 'new_session' }],
    [I.newSession('/work', null), { kind: 'new_session', workspace: '/work' }],
    [I.setModel('model'), { kind: 'set_model', model: 'model' }],
    [I.setReasoning('high'), { kind: 'set_reasoning', level: 'high' }],
  ];
  const ids = new Set();
  for (const [spec, fields] of cases) {
    const msg = plain(MC.intent(s, spec));
    assert.deepEqual(msg, { t: 'intent', id: msg.id, ...fields });
    assert.equal(typeof msg.id, 'string');
    assert.ok(msg.id.length >= 1 && Buffer.byteLength(msg.id) <= 64);
    ids.add(msg.id);
  }
  const fs = [
    [MC.fs.roots(), { t: 'fs_roots' }],
    [MC.fs.list('/work', false), { t: 'fs_list', path: '/work', hidden: false }],
    [MC.fs.list('/work', true), { t: 'fs_list', path: '/work', hidden: true }],
    [MC.fs.mkdir('/work', 'new-app'), { t: 'fs_mkdir', parent: '/work', name: 'new-app' }],
  ];
  for (const [body, fields] of fs) {
    const msg = plain(MC.request(s, body));
    assert.deepEqual(msg, { ...fields, id: msg.id });
    assert.ok(Buffer.byteLength(msg.id) <= 64);
    ids.add(msg.id);
  }
  assert.equal(ids.size, cases.length + fs.length);
});

test('filesystem replies are matched to their request id once', () => {
  const { MC, plain } = load();
  const s = MC.createStore(), req = MC.request(s, MC.fs.roots());
  const reply = { t: 'fs', id: req.id, ok: true, roots: [{ path: '/work', name: 'work' }] };
  assert.deepEqual(plain(MC.reduce(s, reply, 0).fs), reply);
  assert.deepEqual(plain(MC.reduce(s, reply, 0)), {});
  assert.deepEqual(plain(MC.reduce(s, { t: 'fs', id: 'c999', ok: false, error: 'io' }, 0)), {});
});

test('snapshot blocks with append and len truncation and state keep protocol fields', () => {
  const { MC, plain } = load();
  const s = MC.createStore();
  const fx = MC.reduce(s, { t: 'snapshot', state: structuredClone(state), blocks: structuredClone(blocks) }, 0);
  assert.equal(fx.newSession, true);
  assert.deepEqual(plain(fx.dirty), [0, 1, 2, 3, 4, 5]);
  assert.deepEqual(plain(s.state), state);
  assert.deepEqual(plain(s.blocks), blocks);
  // append concatenates each supplied field; len truncates first.
  let r = MC.reduce(s, { t: 'blocks', len: 2, upserts: [{ i: 1, append: { text: ' world' } }, { i: 1, append: { reasoning: ' more' } }] }, 0);
  assert.deepEqual(plain(r.dirty), [1, 1]);
  assert.equal(s.blocks.length, 2);
  assert.equal(s.blocks[1].text, '**hello** world');
  assert.equal(s.blocks[1].reasoning, 'think more');
  // A full block replaces, and new indices extend the list.
  const done = { ...blocks[1], text: 'finished', streaming: false };
  MC.reduce(s, { t: 'blocks', len: 3, upserts: [{ i: 1, block: done }, { i: 2, block: blocks[2] }] }, 0);
  assert.deepEqual(plain(s.blocks), [blocks[0], done, blocks[2]]);
  // append to a non-assistant block is ignored.
  r = MC.reduce(s, { t: 'blocks', len: 3, upserts: [{ i: 0, append: { text: 'x' } }] }, 0);
  assert.deepEqual(plain(r.dirty), []);
  assert.equal(s.blocks[0].text, 'now');
  const idle = { ...state, status: { ...state.status, busy: false, started_ms: null }, interaction: null };
  assert.equal(MC.reduce(s, { t: 'state', state: idle }, 0).state, true);
  assert.deepEqual(plain(s.state), idle);
  // The same session again only re-renders what changed; another session resets.
  const again = MC.reduce(s, { t: 'snapshot', state: idle, blocks: structuredClone([blocks[0], done, blocks[2]]) }, 0);
  assert.equal(again.newSession, false);
  assert.deepEqual(plain(again.dirty), []);
  const draft = MC.reduce(s, { t: 'snapshot', state: { ...idle, session: null }, blocks: [] }, 0);
  assert.equal(draft.newSession, true);
  assert.equal(s.sid, null);
  assert.deepEqual(plain(s.blocks), []);
});

test('welcome sets the host clock offset and handshake messages map to screens', () => {
  const { MC, plain } = load();
  const s = MC.createStore();
  assert.deepEqual(plain(MC.reduce(s, { t: 'authorized', token: 'tok' }, 0)), { token: 'tok' });
  MC.reduce(s, { t: 'welcome', v: 1, version: '0.5.5', device: 'iPhone Safari', now_ms: 1761300003000 }, 1761300000000);
  assert.equal(s.skew, 3000);
  assert.equal(s.device, 'iPhone Safari');
  const gate = msg => plain(MC.reduce(MC.createStore(), msg, 0).gate);
  const wrong = gate({ t: 'error', code: 'wrong_password', message: 'x' });
  assert.equal(wrong.view, 'signin'); assert.equal(wrong.shake, true); assert.match(wrong.error, /Wrong password/); assert.ok(!wrong.forget);
  const locked = gate({ t: 'error', code: 'locked', retry_after: 27, message: 'x' });
  assert.equal(locked.view, 'signin'); assert.equal(locked.locked, 27); assert.ok(!locked.forget);
  for (const msg of [{ t: 'error', code: 'unauthorized' }, { t: 'bye', reason: 'signed_out' }]) {
    const g = gate(msg);
    assert.equal(g.view, 'signin'); assert.equal(g.forget, true); assert.ok(g.notice);
  }
  assert.match(gate({ t: 'bye', reason: 'signed_out' }).notice, /password was changed/);
  const status = {
    busy: [{ t: 'error', code: 'busy' }, /another device/i, 'retry'],
    version: [{ t: 'error', code: 'version' }, /update needed/i, 'reload'],
    bad_request: [{ t: 'error', code: 'bad_request', message: 'bad device name' }, /couldn't connect/i, 'retry'],
    remote_off: [{ t: 'bye', reason: 'remote_off' }, /remote control is off/i, 'retry'],
    tui_exit: [{ t: 'bye', reason: 'tui_exit' }, /quit/i, 'retry'],
    replaced: [{ t: 'bye', reason: 'replaced' }, /another tab/i, 'retry'],
  };
  for (const [key, [msg, title, action]] of Object.entries(status)) {
    const g = gate(msg);
    assert.equal(g.view, 'status', key); assert.match(g.title, title, key); assert.equal(g.action, action, key); assert.ok(!g.forget, key);
  }
  assert.equal(gate({ t: 'bye', reason: 'replaced' }).label, 'Use here');
  assert.equal(gate({ t: 'error', code: 'bad_request', message: 'bad device name' }).text, 'bad device name');
  assert.equal(MC.lockText(27), 'Too many attempts · try again in 0:27');
  assert.equal(MC.lockText(90), 'Too many attempts · try again in 1:30');
  assert.equal(MC.lockText(900), 'Too many attempts · try again in 15:00');
  assert.equal(MC.lockText(0.2), 'Too many attempts · try again in 0:01');
  assert.equal(MC.unreachable('192.168.1.2:8765'), "Can't reach MiniCore at 192.168.1.2:8765");
});

test('a dropped connection leaves sends unconfirmed and never resends them', () => {
  const { MC, plain } = load();
  const s = MC.createStore();
  const sent = MC.intent(s, MC.intents.submit('hello'));
  MC.intent(s, MC.intents.cancel());
  MC.request(s, MC.fs.roots());
  MC.disconnect(s);
  assert.deepEqual(plain(MC.unconfirmed(s).map(it => it.text)), ['hello']);
  assert.equal(s.intents.size, 1);
  assert.equal(s.fs.size, 0);
  // Reconnecting gives only a fresh snapshot; nothing in the reducer produces outgoing messages.
  for (const msg of [{ t: 'welcome', v: 1, version: '1', device: 'd', now_ms: 0 }, { t: 'snapshot', state: structuredClone(state), blocks: structuredClone(blocks) }]) {
    const fx = MC.reduce(s, msg, 0);
    assert.ok(!('send' in fx) && !('resend' in fx));
  }
  assert.equal(MC.unconfirmed(s).length, 1);
  assert.equal(MC.pending(s, 'submit'), null);
  // A late ack for the old id after reconnect does not clear the card either.
  assert.deepEqual(Object.keys(MC.reduce(s, { t: 'ack', id: 'c999', ok: true }, 0)), []);
  // Send again is an explicit new intent with a new id.
  const again = MC.sendAgain(s, sent.id);
  assert.notEqual(again.id, sent.id);
  assert.equal(again.kind, 'submit');
  assert.equal(again.text, 'hello');
  assert.equal(MC.unconfirmed(s).length, 0);
  assert.ok(MC.pending(s, 'submit'));
  const ack = MC.reduce(s, { t: 'ack', id: again.id, ok: true }, 0).ack;
  assert.equal(ack.ok, true); assert.equal(ack.text, 'hello');
  assert.equal(s.intents.size, 0);
});

test('retireText removes only the acked text and keeps returned or newly typed text', () => {
  const { MC } = load();
  assert.equal(MC.retireText('draft', 'draft'), '');
  assert.equal(MC.retireText('draft\nreturned', 'draft'), 'returned');
  assert.equal(MC.retireText('returned\ndraft', 'draft'), 'returned');
  assert.equal(MC.retireText('edited draft', 'draft'), 'edited draft');
  assert.equal(MC.mergeReturned('', 'back'), 'back');
  assert.equal(MC.mergeReturned('next', 'back'), 'next\nback');
  // returned arrives while the send is still pending, then the ack retires only the sent text.
  assert.equal(MC.retireText(MC.mergeReturned('first', 'old'), 'first'), 'old');
});

test('markdown tokens keep HTML and unsafe links as plain text', () => {
  const { MC, plain } = load();
  const texts = [];
  const walk = v => {
    if (Array.isArray(v)) return v.forEach(walk);
    if (!v || typeof v !== 'object') return;
    if (v.t === 'text' || (v.t === 'code' && 's' in v)) texts.push(v.s);
    if (v.t === 'code' && 'text' in v) texts.push(v.text);
    assert.notEqual(v.t, 'html');
    if (v.t === 'a') assert.match(v.href, /^https?:\/\//);
    Object.values(v).forEach(walk);
  };
  const src = '<img src=x onerror=alert(1)> and <script>alert(2)</script>\n\n[click](javascript:alert(3)) [ok](https://example.com/a) http://x.test/b.\n\n```html\n<script>bad()</script>\n```';
  const tree = plain(MC.parseMd(src));
  walk(tree);
  const all = texts.join('|');
  assert.ok(all.includes('<img src=x onerror=alert(1)>'));
  assert.ok(all.includes('<script>alert(2)</script>'));
  assert.ok(all.includes('click'));
  assert.ok(!all.includes('javascript:'));
  assert.ok(all.includes('<script>bad()</script>'));
  const links = [];
  const find = v => { if (Array.isArray(v)) v.forEach(find); else if (v && typeof v === 'object') { if (v.t === 'a') links.push(v.href); Object.values(v).forEach(find); } };
  find(tree);
  assert.deepEqual(links, ['https://example.com/a', 'http://x.test/b']);
  for (const src of ['[x](JavaScript:alert(1))', '[x](data:text/html,hi)', '[x]( javascript:void(0) )']) {
    const tokens = plain(MC.inline(src));
    assert.ok(tokens.every(t => t.t === 'text'), src);
    assert.ok(!tokens.map(t => t.s).join('').toLowerCase().includes('javascript:') && !tokens.map(t => t.s).join('').includes('data:'), src);
  }
});

test('markdown parses headings nested lists code tables and quotes', () => {
  const { MC, plain } = load();
  const tree = plain(MC.parseMd('# Title\n\n- a\n  - b\n- c\n\n3. x\n4. y\n\n```rust\nfn main() {}\n```\n\n| A | B |\n|:--|--:|\n| 1 | **2** |\n\n> quote'));
  assert.deepEqual(tree.map(t => t.t), ['h', 'list', 'list', 'code', 'table', 'quote']);
  assert.equal(tree[1].ord, false);
  assert.equal(tree[1].items.length, 2);
  assert.equal(tree[1].items[0].sub[0].items[0].c[0].s, 'b');
  assert.equal(tree[2].ord, true); assert.equal(tree[2].start, 3);
  assert.deepEqual(tree[3], { t: 'code', lang: 'rust', text: 'fn main() {}' });
  assert.deepEqual(tree[4].align, ['', 'right']);
  assert.equal(tree[4].rows[0][1][0].t, 'b');
  // A streaming list marker without text stays a paragraph until its text arrives.
  assert.equal(plain(MC.parseMd('intro\n- '))[0].t, 'p');
});

test('breadcrumbs preserve Unix drive and UNC paths and start at the root', () => {
  const { MC, plain } = load();
  for (const path of ['/work/app', 'C:\\work\\app', '\\\\server\\share\\app']) assert.equal(plain(MC.splitPath(path)).at(-1).path, path);
  assert.deepEqual(plain(MC.splitPath('/work/app')).map(c => c.path), ['/', '/work', '/work/app']);
  assert.deepEqual(plain(MC.splitPath('C:\\work\\app')).map(c => c.path), ['C:\\', 'C:\\work', 'C:\\work\\app']);
  const unc = plain(MC.splitPath('\\\\server\\share\\app'));
  assert.equal(unc[1].path, '\\\\server\\share');
  assert.equal(MC.within(unc[2].path, unc[1].path), true);
  assert.equal(MC.within('/work2', '/work'), false);
  assert.equal(MC.within('C:\\work\\app', 'C:\\'), true);
  const roots = [{ path: '/Users/me/Develops', name: 'Develops' }, { path: '/Users/me', name: 'me' }];
  assert.deepEqual(plain(MC.crumbs('/Users/me/Develops/app/src', roots)).map(c => c.name), ['Develops', 'app', 'src']);
  assert.deepEqual(plain(MC.crumbs('D:\\code\\app', [{ path: 'D:\\code', name: 'code' }])).map(c => c.path), ['D:\\code', 'D:\\code\\app']);
  assert.deepEqual(plain(MC.crumbs('\\\\nas\\share\\proj', [{ path: '\\\\nas\\share', name: 'share' }])).map(c => c.path), ['\\\\nas\\share', '\\\\nas\\share\\proj']);
  assert.deepEqual(plain(MC.crumbs('C:\\a', [{ path: 'C:\\', name: 'C:' }])).map(c => c.path), ['C:\\', 'C:\\a']);
});

test('sessions group by the local calendar day of host-adjusted time', () => {
  const { MC, plain } = load();
  const tz = -480; // UTC+8
  const now = Date.parse('2026-10-25T01:00:00Z'); // 09:00 local
  const at = iso => ({ id: iso, title: iso, workspace: '/w', updated_at: iso, current: false });
  const sessions = [at('2026-10-24T17:00:00Z'), at('2026-10-24T15:30:00Z'), at('2026-10-20T00:00:00Z'), at('2026-09-01T00:00:00Z')];
  assert.deepEqual(plain(MC.groupSessions(sessions, now, tz, '')).map(g => [g.label, g.items.length]),
    [['Today', 1], ['Yesterday', 1], ['Previous 7 days', 1], ['Older', 1]]);
  // In UTC the first two are both yesterday; a 3 s host offset moves nothing across a day here.
  assert.deepEqual(plain(MC.groupSessions(sessions, now, 0, '')).map(g => g.label), ['Yesterday', 'Previous 7 days', 'Older']);
  // The host clock offset decides: the phone is a day behind but the host says it is the next day.
  const skew = 86400000;
  assert.equal(MC.dayGroup(Date.parse('2026-10-25T00:30:00Z'), now - 86400000 + skew, tz), 'Today');
  assert.equal(MC.relTime(now - 30000, now, tz), 'now');
  assert.equal(MC.relTime(now - 5 * 60000, now, tz), '5m');
  assert.equal(MC.relTime(now - 3 * 3600000, now, tz), '3h');
  assert.equal(MC.relTime(Date.parse('2026-09-01T00:00:00Z'), now, tz), 'Sep 1');
  assert.deepEqual(plain(MC.groupSessions([{ ...sessions[0], title: 'Fix parser' }, sessions[1]], now, tz, 'PARSER')).map(g => g.items[0].title), ['Fix parser']);
});

test('elapsed status line and summaries read like the TUI', () => {
  const { MC, plain } = load();
  assert.equal(MC.fmtElapsed(12000), '12s');
  assert.equal(MC.fmtElapsed(65000), '1m 05s');
  assert.equal(MC.fmtElapsed(3725000), '1h 02m');
  assert.equal(MC.fmtElapsed(-500), '0s');
  assert.equal(MC.turnEndText(blocks[3]), 'Completed · 7.8 s · 2 requests · 1 tool · 2 files +10 −3');
  assert.equal(MC.turnEndText({ ...blocks[3], changes: { files: 0, added: 0, removed: 0 } }), 'Completed · 7.8 s · 2 requests · 1 tool');
  assert.equal(MC.turnEndText({ ...blocks[3], outcome: 'cancelled', changes: null, tools: 0 }), 'Stopped · 7.8 s · 2 requests');
  assert.equal(MC.compactionText(blocks[4]), 'Context compacted · 32.1k → 2.7k tokens');
  const tool = (name, detail) => ({ type: 'tool', call_id: name + detail, name, detail });
  assert.equal(MC.activitySummary([tool('bash', 'a'), tool('bash', 'b'), tool('bash', 'c'), tool('edit', 'x.rs'), tool('apply_patch', 'y.rs'), tool('edit', 'x.rs')]),
    'Ran 3 commands · edited 2 files');
  assert.equal(MC.activitySummary([tool('read', 'a'), tool('grep', 'b')]), 'Read 1 file · ran 1 search');
  assert.equal(MC.diffClass('+added'), 'add');
  assert.equal(MC.diffClass('-gone'), 'del');
  assert.equal(MC.diffClass('+++ b/x'), 'meta');
  assert.equal(MC.diffClass('@@ -1 +1 @@'), 'hunk');
  const running = { ...blocks[2], state: 'running', detail: 'cargo test' };
  const line = plain(MC.statusLine(state, [blocks[0], running], state.status.started_ms + 12000));
  assert.deepEqual(line, { label: 'Thinking', detail: 'bash · cargo test', elapsed: '12s', ctx: '42% context' });
  const stopping = plain(MC.statusLine({ ...state, status: { ...state.status, interrupting: true } }, [running], state.status.started_ms));
  assert.equal(stopping.label, 'Stopping…');
  assert.equal(stopping.detail, '');
  assert.equal(MC.statusLine({ ...state, status: { ...state.status, busy: false, started_ms: null } }, [], 0).elapsed, '');
  assert.equal(MC.fsError({ error: 'exists' }), 'A folder with that name already exists.');
  assert.equal(MC.fsError({ error: 'io', message: 'Permission denied' }), 'Permission denied');
  assert.deepEqual(plain(MC.recentWorkspaces([{ workspace: '/a' }, { workspace: '/b' }, { workspace: '/a' }, { workspace: '/c' }], 2)), ['/a', '/b']);
});
