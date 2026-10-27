import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import * as core from '../src/remote/web/core.js';
import * as markdown from '../src/remote/web/markdown.js';

const read = name => readFileSync(new URL(`../src/remote/web/${name}`, import.meta.url), 'utf8');
const MC = { ...core, ...markdown };
// Round-trip through JSON so values compare against plain objects.
const plain = v => JSON.parse(JSON.stringify(v));

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

test('the pure modules have no DOM, network or timer access', () => {
  // Property access such as the context reply's `window` field is not a global.
  for (const name of ['core.js', 'markdown.js']) {
    assert.doesNotMatch(read(name), /(?<!\.)\b(document|window|localStorage|WebSocket|navigator|setTimeout|setInterval)\b/, name);
  }
});

test('the page preloads every module and the modules import each other without a cycle', () => {
  const files = readdirSync(new URL('../src/remote/web/', import.meta.url)).filter(f => f.endsWith('.js') && f !== 'sw.js' && f !== 'theme.js');
  const html = read('index.html');
  const imports = Object.fromEntries(files.map(f => [f, [...read(f).matchAll(/from '\.\/(\w+\.js)'/g)].map(m => m[1])]));
  for (const f of files) {
    assert.ok(html.includes(`<link rel="modulepreload" href="/${f}">`), `${f} is not preloaded`);
    for (const dep of imports[f]) assert.ok(files.includes(dep), `${f} imports ${dep}, which does not exist`);
  }
  const walk = (f, path) => {
    assert.ok(!path.includes(f), `import cycle: ${[...path, f].join(' → ')}`);
    for (const dep of imports[f]) walk(dep, [...path, f]);
  };
  files.forEach(f => walk(f, []));
});

test('login and hello follow the handshake contract with a bounded device name', () => {
  assert.deepEqual(plain(MC.login('k7mq-x2pa-n9dt', 'iPhone Safari')), { t: 'login', v: 2, password: 'k7mq-x2pa-n9dt', device: 'iPhone Safari' });
  assert.deepEqual(plain(MC.hello('tok')), { t: 'hello', v: 2, token: 'tok' });
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
  const s = MC.createStore(), I = MC.intents;
  const cases = [
    [I.submit('send'), { kind: 'submit', text: 'send' }],
    [I.queue('later'), { kind: 'queue', text: 'later' }],
    [I.cancel(), { kind: 'cancel' }],
    [I.answer('approval', 'allow'), { kind: 'answer', interaction_id: 'approval', choice: 'allow' }],
    [I.answer('approval', 'allow_session'), { kind: 'answer', interaction_id: 'approval', choice: 'allow_session' }],
    [I.answer('approval', 'deny'), { kind: 'answer', interaction_id: 'approval', choice: 'deny' }],
    [I.answer('choice', 1), { kind: 'answer', interaction_id: 'choice', choice: 1 }],
    [I.answer('approval', 'deny', 'use the helper instead'), { kind: 'answer', interaction_id: 'approval', choice: 'deny', message: 'use the helper instead' }],
    [I.compact(), { kind: 'compact' }],
    [I.unqueue(1, 'later'), { kind: 'unqueue', index: 1, text: 'later' }],
    [I.loadOlder(), { kind: 'load_older' }],
    [I.openSession('ses_one'), { kind: 'open_session', session_id: 'ses_one' }],
    [I.newSession('/work', 'coding'), { kind: 'new_session', workspace: '/work', profile: 'coding' }],
    [I.newSession(), { kind: 'new_session' }],
    [I.newSession('/work', null), { kind: 'new_session', workspace: '/work' }],
    [I.setModel('model'), { kind: 'set_model', model: 'model' }],
    [I.setReasoning('high'), { kind: 'set_reasoning', level: 'high' }],
    [I.renameSession('ses_one', 'Fix the parser'), { kind: 'rename_session', session_id: 'ses_one', title: 'Fix the parser' }],
    [I.renameSession('ses_one', ''), { kind: 'rename_session', session_id: 'ses_one', title: '' }],
    [I.deleteSession('ses_two'), { kind: 'delete_session', session_id: 'ses_two' }],
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
  const Q = MC.queries, qs = [
    [Q.context(), { kind: 'context' }], [Q.changes(), { kind: 'changes' }], [Q.diff('src/a.rs'), { kind: 'diff', path: 'src/a.rs' }],
    [Q.toolOutput('call', 65536), { kind: 'tool_output', call_id: 'call', offset: 65536 }], [Q.files('pars'), { kind: 'files', query: 'pars' }],
  ];
  for (const [spec, fields] of qs) {
    const msg = plain(MC.query(s, spec));
    assert.deepEqual(msg, { t: 'query', id: msg.id, ...fields });
    ids.add(msg.id);
  }
  assert.equal(ids.size, cases.length + fs.length + qs.length);
});

test('at most four queries are in flight and results are matched to their id once', () => {
  const s = MC.createStore(), sent = [];
  while (MC.queryRoom(s)) sent.push(MC.query(s, MC.queries.changes()));
  assert.equal(sent.length, 4);
  const reply = { t: 'result', id: sent[0].id, ok: true, data: { branch: 'main', files: [], truncated: false } };
  assert.deepEqual(plain(MC.reduce(s, reply, 0).result), reply);
  assert.deepEqual(plain(MC.reduce(s, reply, 0)), {});
  assert.equal(MC.queryRoom(s), true);
  // A stale reply (unknown id) is ignored, and a dropped connection forgets the rest.
  assert.deepEqual(plain(MC.reduce(s, { t: 'result', id: 'c999', ok: false, error: 'busy' }, 0)), {});
  MC.disconnect(s);
  assert.equal(s.q.size, 0);
  assert.equal(MC.errorText('busy'), 'MiniCore is busy · try again in a moment');
  assert.equal(MC.errorText('Open another session first'), 'Open another session first');
});

test('a removed queue pill drops the text the host returns, an edited one refills the composer', () => {
  const s = MC.createStore();
  const drop = MC.unqueue(s, 0, 'later', true);
  assert.deepEqual(plain(drop), { t: 'intent', id: drop.id, kind: 'unqueue', index: 0, text: 'later' });
  assert.deepEqual(plain(MC.reduce(s, { t: 'returned', text: 'later' }, 0)), {});
  assert.deepEqual(plain(MC.reduce(s, { t: 'ack', id: drop.id, ok: true }, 0)).ack.ok, true);
  MC.unqueue(s, 0, 'edit me', false);
  assert.deepEqual(plain(MC.reduce(s, { t: 'returned', text: 'edit me' }, 0)), { returned: 'edit me' });
  // A refused removal keeps nothing armed: a later identical return refills the composer.
  const stale = MC.unqueue(s, 3, 'gone', true);
  MC.reduce(s, { t: 'ack', id: stale.id, ok: false, error: 'stale' }, 0);
  assert.deepEqual(plain(MC.reduce(s, { t: 'returned', text: 'gone' }, 0)), { returned: 'gone' });
  // Unqueue is not a send: a dropped connection never offers it again.
  MC.unqueue(s, 0, 'x', true);
  MC.disconnect(s);
  assert.equal(MC.unconfirmed(s).length, 0);
  assert.deepEqual(plain(MC.reduce(s, { t: 'returned', text: 'x' }, 0)), { returned: 'x' });
});

test('filesystem replies are matched to their request id once', () => {
  const s = MC.createStore(), req = MC.request(s, MC.fs.roots());
  const reply = { t: 'fs', id: req.id, ok: true, roots: [{ path: '/work', name: 'work' }] };
  assert.deepEqual(plain(MC.reduce(s, reply, 0).fs), reply);
  assert.deepEqual(plain(MC.reduce(s, reply, 0)), {});
  assert.deepEqual(plain(MC.reduce(s, { t: 'fs', id: 'c999', ok: false, error: 'io' }, 0)), {});
});

test('snapshot blocks with append and len truncation and state keep protocol fields', () => {
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
  MC.reduce(s, { t: 'blocks', len: 3, upserts: [{ i: 1, block: done }, { i: 2, block: structuredClone(blocks[2]) }] }, 0);
  assert.deepEqual(plain(s.blocks), [blocks[0], done, blocks[2]]);
  // Text appended to a user block is ignored. Tool output continues the line being written, then adds lines.
  r = MC.reduce(s, { t: 'blocks', len: 3, upserts: [{ i: 0, append: { text: 'x' } }] }, 0);
  assert.deepEqual(plain(r.dirty), []);
  assert.equal(s.blocks[0].text, 'now');
  r = MC.reduce(s, { t: 'blocks', len: 3, upserts: [{ i: 2, append: { output: ['a', 'b'] } }, { i: 2, append: { output: ['c'] } }] }, 0);
  assert.deepEqual(plain(r.dirty), [2, 2]);
  assert.deepEqual(plain(s.blocks[2].output), ['/worka', 'bc']);
  s.blocks[2].output = [];
  MC.reduce(s, { t: 'blocks', len: 3, upserts: [{ i: 2, append: { output: ['run'] } }, { i: 2, append: { output: ['ning ... ok', ''] } }] }, 0);
  assert.deepEqual(plain(s.blocks[2].output), ['running ... ok', '']);
  const idle = { ...state, status: { ...state.status, busy: false, started_ms: null }, interaction: null };
  assert.equal(MC.reduce(s, { t: 'state', state: idle }, 0).state, true);
  assert.deepEqual(plain(s.state), idle);
  // The same session again only re-renders what changed (the tool lost its appended lines); another session resets.
  const again = MC.reduce(s, { t: 'snapshot', state: idle, blocks: structuredClone([blocks[0], done, blocks[2]]) }, 0);
  assert.equal(again.newSession, false);
  assert.deepEqual(plain(again.dirty), [2]);
  const draft = MC.reduce(s, { t: 'snapshot', state: { ...idle, session: null }, blocks: [] }, 0);
  assert.equal(draft.newSession, true);
  assert.equal(s.sid, null);
  assert.deepEqual(plain(s.blocks), []);
});

test('welcome sets the host clock offset and handshake messages map to screens', () => {
  const s = MC.createStore();
  assert.deepEqual(plain(MC.reduce(s, { t: 'authorized', token: 'tok' }, 0)), { token: 'tok' });
  MC.reduce(s, { t: 'welcome', v: 2, version: '0.7.0', device: 'iPhone Safari', now_ms: 1761300003000 }, 1761300000000);
  assert.equal(s.skew, 3000);
  assert.equal(s.version, '0.7.0');
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
  assert.equal(MC.retireText('draft', 'draft'), '');
  assert.equal(MC.retireText('draft\nreturned', 'draft'), 'returned');
  assert.equal(MC.retireText('returned\ndraft', 'draft'), 'returned');
  assert.equal(MC.retireText('edited draft', 'draft'), 'edited draft');
  assert.equal(MC.mergeReturned('', 'back'), 'back');
  assert.equal(MC.mergeReturned('next', 'back'), 'next\nback');
  // returned arrives while the send is still pending, then the ack retires only the sent text.
  assert.equal(MC.retireText(MC.mergeReturned('first', 'old'), 'first'), 'old');
});

test('other devices read per-device views and drop just one entry of this device name', () => {
  const devices = [
    { name: 'iPhone Safari', session: 'x' },
    { name: 'Mac Edge', session: 'y' },
    { name: 'iPhone Safari', session: null },
    { name: 'iPad Safari', session: 'x' },
  ];
  const before = structuredClone(devices);
  assert.deepEqual(plain(MC.otherDevices(devices, 'iPhone Safari')), [devices[1], devices[2], devices[3]]);
  assert.deepEqual(plain(MC.otherDevices(devices.slice(0, 1), 'iPhone Safari')), []);
  assert.deepEqual(plain(MC.otherDevices(devices.slice(1, 2), 'iPhone Safari')).map(d => d.name), ['Mac Edge']);
  assert.deepEqual(plain(MC.otherDevices([], 'iPhone Safari')), []);
  assert.deepEqual(devices, before);
});

test('markdown tokens keep HTML and unsafe links as plain text', () => {
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
  const tz = -480; // UTC+8
  const now = Date.parse('2026-10-25T01:00:00Z'); // 09:00 local
  const at = iso => ({ id: iso, title: iso, workspace: '/w', updated_at: iso, current: false });
  const sessions = [at('2026-10-24T17:00:00Z'), at('2026-10-24T15:30:00Z'), at('2026-10-20T00:00:00Z'), at('2026-09-01T00:00:00Z')];
  assert.deepEqual(plain(MC.sessionLayout(sessions, now, tz, '').groups).map(g => [g.label, g.items.length]),
    [['Today', 1], ['Yesterday', 1], ['Previous 7 days', 1], ['Older', 1]]);
  // In UTC the first two are both yesterday; a 3 s host offset moves nothing across a day here.
  assert.deepEqual(plain(MC.sessionLayout(sessions, now, 0, '').groups).map(g => g.label), ['Yesterday', 'Previous 7 days', 'Older']);
  // The host clock offset decides: the phone is a day behind but the host says it is the next day.
  const skew = 86400000;
  assert.equal(MC.dayGroup(Date.parse('2026-10-25T00:30:00Z'), now - 86400000 + skew, tz), 'Today');
  assert.equal(MC.relTime(now - 30000, now, tz), 'now');
  assert.equal(MC.relTime(now - 5 * 60000, now, tz), '5m');
  assert.equal(MC.relTime(now - 3 * 3600000, now, tz), '3h');
  assert.equal(MC.relTime(Date.parse('2026-09-01T00:00:00Z'), now, tz), 'Sep 1');
  assert.deepEqual(plain(MC.sessionLayout([{ ...sessions[0], title: 'Fix parser' }, sessions[1]], now, tz, 'PARSER').groups).map(g => g.items[0].title), ['Fix parser']);
});

test('elapsed status line and summaries read like the TUI', () => {
  assert.equal(MC.fmtElapsed(12000), '12s');
  assert.equal(MC.fmtElapsed(65000), '1m 05s');
  assert.equal(MC.fmtElapsed(3725000), '1h 02m');
  assert.equal(MC.fmtElapsed(-500), '0s');
  const tz = -480, at = Date.parse('2026-10-25T06:02:00Z');
  assert.deepEqual(plain(MC.turnEnd(blocks[3], at, tz)), { ok: true, parts: ['Completed', '7.8 s', '14:02', '12 tokens'], changes: { files: 2, added: 10, removed: 3 } });
  assert.equal(MC.changesText(blocks[3].changes), '2 files +10 −3');
  assert.equal(MC.turnEnd({ ...blocks[3], changes: { files: 0, added: 0, removed: 0 } }, null, tz).changes, null);
  const stopped = plain(MC.turnEnd({ ...blocks[3], outcome: 'cancelled', usage: { input: 0, output: 0, cached: 0 } }, null, tz));
  assert.deepEqual(stopped.parts, ['Stopped', '7.8 s']);
  assert.equal(stopped.ok, false);
  assert.equal(MC.compactionText(blocks[4]), 'Context compacted · 32.1k → 2.7k tokens');
  const running = { ...blocks[2], state: 'running', detail: 'cargo test' };
  const line = plain(MC.statusLine(state, [blocks[0], running], state.status.started_ms + 12000));
  assert.deepEqual(line, { label: 'Thinking', detail: 'bash · cargo test', elapsed: '12s', ctx: '42% context' });
  const stopping = plain(MC.statusLine({ ...state, status: { ...state.status, interrupting: true } }, [running], state.status.started_ms));
  assert.equal(stopping.label, 'Stopping…');
  assert.equal(stopping.detail, '');
  assert.equal(MC.statusLine({ ...state, status: { ...state.status, busy: false, started_ms: null } }, [], 0).elapsed, '');
  // Stop is offered while a turn runs or a compaction is under way, and only then.
  assert.equal(MC.canStop(state.status), true);
  assert.equal(MC.canStop({ ...state.status, busy: false, compacting: true }), true);
  assert.equal(MC.canStop({ ...state.status, busy: false }), false);
  assert.equal(MC.fsError({ error: 'exists' }), 'A folder with that name already exists.');
  assert.equal(MC.fsError({ error: 'io', message: 'Permission denied' }), 'Permission denied');
  assert.deepEqual(plain(MC.recentWorkspaces([{ workspace: '/a' }, { workspace: '/b' }, { workspace: '/a' }, { workspace: '/c' }], 2)), ['/a', '/b']);
});

test('rename keeps the title exactly, including an empty one that clears it', () => {
  const s = MC.createStore();
  for (const title of ['', '  spaced  ', 'Ünïcödé — 修复解析器 🚀', 'x'.repeat(200)]) {
    const msg = plain(MC.intent(s, MC.intents.renameSession('ses_one', title)));
    assert.deepEqual(Object.keys(msg).sort(), ['id', 'kind', 'session_id', 't', 'title']);
    assert.equal(msg.title, title);
    assert.ok(Buffer.byteLength(msg.id) <= 64);
  }
  // Rename and delete carry no text, so a dropped connection forgets them instead of offering Send again.
  MC.intent(s, MC.intents.deleteSession('ses_two'));
  MC.disconnect(s);
  assert.equal(s.intents.size, 0);
  assert.equal(MC.unconfirmed(s).length, 0);
});

test('the appearance choice overrides the OS scheme, System follows it, and the bar colours and text size follow along', () => {
  const run = (stored, dark) => {
    const root = { dataset: {}, style: { props: {}, setProperty(k, v) { this.props[k] = v; } } }, metas = { tc: {}, tcd: {} };
    const context = vm.createContext({
      localStorage: { getItem: () => stored },
      matchMedia: () => ({ matches: dark, addEventListener() {} }),
      document: { documentElement: root, getElementById: id => metas[id] },
    });
    vm.runInContext(read('theme.js'), context);
    return { theme: root.dataset.theme, size: root.style.props['--ts'], bars: [metas.tc.content, metas.tcd.content] };
  };
  const light = '#ffffff', dark = '#0e0e10';
  assert.deepEqual(run('{"theme":"light"}', true), { theme: 'light', size: 1, bars: [light, light] });
  assert.deepEqual(run('{"theme":"dark","text":1.2}', false), { theme: 'dark', size: 1.2, bars: [dark, dark] });
  assert.deepEqual(run('{"theme":"system"}', true), { theme: 'dark', size: 1, bars: [light, dark] });
  assert.deepEqual(run('{"theme":"system"}', false), { theme: 'light', size: 1, bars: [light, dark] });
  for (const stored of [null, '{}', '{"theme":"auto"}', '{"theme":"DARK"}']) {
    assert.equal(run(stored, true).theme, 'dark');
    assert.equal(run(stored, false).theme, 'light');
  }
});

test('install help is the browser prompt when offered, the Share hint on iOS until dismissed, and nothing once installed', () => {
  const iphone = MC.deviceName('Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) Version/18.0 Mobile/15E148 Safari/604.1');
  const ipad = MC.deviceName('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Version/18.0 Safari/605.1.15', 5);
  const android = MC.deviceName('Mozilla/5.0 (Linux; Android 15) Chrome/140.0 Mobile Safari/537.36');
  assert.equal(MC.installOffer(false, false, iphone, null), 'ios');
  assert.equal(MC.installOffer(false, false, ipad, null), 'ios');
  assert.equal(MC.installOffer(false, false, iphone, '1'), null);
  assert.equal(MC.installOffer(false, true, android, null), 'prompt');
  assert.equal(MC.installOffer(false, false, android, null), null);
  assert.equal(MC.installOffer(true, true, iphone, null), null);
});

const live = (id, status, extra = {}) => ({ id, title: id, workspace: '/w', updated_at: '2026-10-25T00:00:00Z', current: false, live: true, status, started_ms: null, unseen: false, ...extra });
const idle = id => ({ id, title: id, workspace: '/w', updated_at: '2026-10-24T00:00:00Z', current: false, live: false });

test('the session list puts sessions that need you first, then running ones, then days', () => {
  const now = Date.parse('2026-10-25T12:00:00Z');
  const sessions = [
    idle('old'),
    live('late', 'running', { started_ms: 3000 }),
    live('fg', 'idle', { current: true }),
    live('ask', 'approval', { started_ms: 5000 }),
    live('early', 'running', { started_ms: 1000 }),
    live('pack', 'compacting'),
    live('done', 'idle', { unseen: true }),
  ];
  const g = plain(MC.sessionLayout(sessions, now, 0, '').groups);
  assert.deepEqual(g.map(x => [x.label, x.items.map(y => y.id)]), [
    ['Needs you', ['ask']], ['Running', ['early', 'late', 'pack']], ['Today', ['fg', 'done']], ['Yesterday', ['old']]]);
  // Sessions from a server without live fields are only grouped by day; the query filters by title or workspace.
  assert.deepEqual(plain(MC.sessionLayout(state.sessions, now, 0, '').groups).map(x => x.label), ['Older']);
  assert.deepEqual(plain(MC.sessionLayout(sessions, now, 0, 'ear').groups).map(x => x.items.map(y => y.id)), [['early']]);
});

test('an open session list keeps its order while statuses change and puts new sessions on top of their group', () => {
  const now = Date.parse('2026-10-25T12:00:00Z');
  const first = MC.sessionLayout([live('a', 'running', { started_ms: 1 }), live('b', 'approval'), idle('c')], now, 0, '');
  const frozen = first.order;
  // b's approval is answered and a starts waiting; c is gone; d is new and needs you.
  const later = MC.sessionLayout([live('d', 'approval'), live('a', 'approval', { started_ms: 1 }), live('b', 'running', { started_ms: 2 })], now, 0, '', frozen);
  assert.equal(later.order, frozen);
  assert.deepEqual(plain(later.groups).map(x => [x.label, x.items.map(y => y.id)]), [['Needs you', ['d', 'b']], ['Running', ['a']]]);
  // Opened again: sorted afresh.
  const again = MC.sessionLayout([live('d', 'approval'), live('a', 'approval', { started_ms: 1 }), live('b', 'running', { started_ms: 2 })], now, 0, '');
  assert.deepEqual(plain(again.groups).map(x => [x.label, x.items.map(y => y.id)]), [['Needs you', ['a', 'd']], ['Running', ['b']]]);
});

test('attention counts other sessions that need you plus this view, and notices name their session', () => {
  assert.equal(MC.attentionCount({ ...state, background: { running: 2, approval: 1 } }), 2);
  assert.equal(MC.attentionCount({ ...state, interaction: null, background: { running: 2, approval: 0 } }), 0);
  assert.deepEqual({ ...MC.noticeEvent('"Fix parser" needs approval · ctrl+r') }, { label: 'Fix parser', kind: 'approval' });
  assert.deepEqual({ ...MC.noticeEvent('"Fix parser" finished in 3m 12s') }, { label: 'Fix parser', kind: 'finished' });
  assert.deepEqual({ ...MC.noticeEvent('"Say "hi"" failed in 4s') }, { label: 'Say "hi"', kind: 'failed' });
  assert.equal(MC.noticeEvent('"Fix parser": model error'), null);
  assert.equal(MC.noticeEvent('Session renamed'), null);
});

test('state transitions report background approvals and unseen finishes once', () => {
  const ev = (a, b) => plain(MC.sessionTransitions(a, b));
  const before = [live('a', 'running', { started_ms: 1 }), live('b', 'running', { started_ms: 2 }), live('fg', 'running', { current: true }), idle('c')];
  const after = [live('a', 'approval', { started_ms: 1 }), live('b', 'idle', { unseen: true }), live('fg', 'approval', { current: true }), idle('c')];
  assert.deepEqual(ev(before, after), [{ kind: 'approval', id: 'a', title: 'a' }, { kind: 'finished', id: 'b', title: 'b' }]);
  // Nothing changed: nothing to report, so a repeated state does not toast again.
  assert.deepEqual(ev(after, after), []);
  // A turn that started and ended between two states still counts as finished.
  assert.deepEqual(ev([live('d', 'idle')], [live('d', 'idle', { unseen: true })]), [{ kind: 'finished', id: 'd', title: 'd' }]);
  // The foreground answers its own approval in the sheet; sessions new to the list or not live are ignored.
  assert.deepEqual(ev([], [live('e', 'approval')]), []);
  assert.deepEqual(ev([idle('f')], [live('f', 'approval')]), []);
  // Approval answered and the turn goes on: no event.
  assert.deepEqual(ev([live('g', 'approval')], [live('g', 'running')]), []);
});

test('snapshots and states carry session transition events against the previous state', () => {
  const s = MC.createStore();
  const withSessions = sessions => structuredClone({ ...state, sessions, background: { running: 1, approval: 0 } });
  assert.deepEqual(plain(MC.reduce(s, { t: 'snapshot', state: withSessions([live('a', 'running')]), blocks: [] }, 0).events), []);
  const fx = plain(MC.reduce(s, { t: 'state', state: withSessions([live('a', 'approval')]) }, 0));
  assert.deepEqual(fx.events, [{ kind: 'approval', id: 'a', title: 'a' }]);
  assert.deepEqual(plain(MC.reduce(s, { t: 'state', state: withSessions([live('a', 'approval')]) }, 0).events), []);
});

test('device locations do not switch this client and attention excludes its own current session', () => {
  const s = MC.createStore();
  const view = (other, status) => structuredClone({
    ...state, session: { ...state.session, id: 'mine' },
    devices: [{ name: 'iPhone Safari', session: 'mine' }, { name: 'Mac Edge', session: other }],
    sessions: [live('mine', status, { current: true }), live('other', status)],
  });
  MC.reduce(s, { t: 'snapshot', state: view('other', 'running'), blocks: [] }, 0);
  const fx = plain(MC.reduce(s, { t: 'state', state: view(null, 'approval') }, 0));
  assert.equal(s.sid, 'mine');
  assert.equal(s.state.session.id, 'mine');
  assert.deepEqual(fx.events, [{ kind: 'approval', id: 'other', title: 'other' }]);
  assert.deepEqual(plain(MC.otherDevices(s.state.devices, 'iPhone Safari')).map(d => d.name), ['Mac Edge']);
});

test('only the streaming tail of a busy transcript is the block being written', () => {
  const said = (text, streaming = true) => ({ type: 'assistant', loop_id: 'l', request: 1, reasoning: '', text, streaming });
  const tool = { type: 'tool', call_id: 'c', name: 'bash', detail: 'ls', state: 'succeeded' };
  // The TUI keeps every assistant block of the running turn streaming; only the tail is still being written.
  const turn = [blocks[0], said('first'), tool, said('second')];
  assert.equal(MC.liveBlock(turn, true), 3);
  assert.equal(MC.liveBlock(turn.slice(0, 3), true), -1, 'a tool at the tail: nothing is being written');
  assert.equal(MC.liveBlock(turn, false), -1, 'idle: nothing is being written');
  assert.equal(MC.liveBlock([...turn.slice(0, 3), said('done', false)], true), -1);
  // A freshly started, still empty request is the tail: the earlier answer is done even though it is not shown as live.
  assert.equal(MC.liveBlock([said('first'), said('')], true), 1);
  assert.equal(MC.liveBlock([], true), -1);
});

test('tool rows read as a past-tense verb, a target and meta, present tense while running', () => {
  const t = (name, extra) => plain(MC.toolInfo({ ...blocks[2], name, ...extra }));
  assert.deepEqual(t('bash', { detail: 'cargo test', duration_ms: 1200 }), { kind: 'cmd', run: false, verb: 'Ran', target: 'cargo test', dur: '1.2 s', exit: '', added: null, removed: null, note: '' });
  assert.equal(t('bash', { state: 'running' }).verb, 'Running');
  assert.equal(t('bash', { state: 'failed', exit_code: 101 }).exit, 'exit 101');
  assert.equal(t('bash', { state: 'denied' }).note, 'denied');
  assert.equal(t('read', { detail: 'src/a.rs', duration_ms: 40 }).dur, '');
  const edit = t('apply_patch', { detail: 'src/a.rs', input: '*** Begin Patch\n*** Update File: src/a.rs\n@@\n-a\n+b\n+c\n*** End Patch' });
  assert.deepEqual([edit.verb, edit.added, edit.removed], ['Edited', 2, 1]);
  assert.deepEqual([t('write', { input_lines: 640 }).verb, t('write', { input_lines: 640 }).added], ['Wrote', 640]);
  assert.deepEqual([t('todo_write', { detail: '3 items' }).verb, t('todo_write', { detail: '3 items' }).target], ['todo_write', '3 items']);
  assert.equal(MC.midTrunc('gpt-6-luna'), 'gpt-6-luna');
  assert.equal(MC.midTrunc('claude-opus-5-preview-2026-10-01'), 'claude-o…26-10-01');
});

test('diffs parse git hunks with line numbers and apply_patch envelopes without them', () => {
  const git = plain(MC.parseDiff('diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -10,3 +10,4 @@ fn main\n ctx\n-old\n+new\n+more\n ctx2\ndiff --git a/b.md b/b.md\n--- /dev/null\n+++ b/b.md\n@@ -0,0 +1 @@\n+# B'));
  assert.deepEqual(git.map(f => [f.path, f.added, f.removed]), [['src/a.rs', 2, 1], ['b.md', 1, 0]]);
  assert.deepEqual(git[0].hunks[0].lines.map(l => [l.t, l.s, l.a, l.b]), [['ctx', 'ctx', 10, 10], ['del', 'old', 11, null], ['add', 'new', null, 11], ['add', 'more', null, 12], ['ctx', 'ctx2', 12, 13]]);
  // A content line that looks like a header inside a counted hunk stays content.
  const tricky = plain(MC.parseDiff('--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n--- not a header\n+++ still content\n same'));
  assert.deepEqual(tricky.map(f => [f.path, f.added, f.removed]), [['x', 1, 1]]);
  const patch = plain(MC.parseDiff('*** Begin Patch\n*** Update File: src/p.rs\n@@ fn split\n-a\n+b\n c\n*** Add File: new.txt\n+hello\n*** End Patch'));
  assert.deepEqual(patch.map(f => [f.path, f.op, f.added, f.removed]), [['src/p.rs', 'update', 1, 1], ['new.txt', 'add', 1, 0]]);
  assert.equal(patch[0].hunks[0].head, 'fn split');
  assert.deepEqual(patch[0].hunks[0].lines.map(l => [l.t, l.a]), [['del', null], ['add', null], ['ctx', null]]);
  assert.deepEqual(plain(MC.parseDiff('just text\nno diff')), []);
  assert.deepEqual(plain(MC.diffStats('*** Update File: a\n@@\n-x\n+y\n+z')), { added: 2, removed: 1 });
});

test('approval keys answer only once armed, never from a text field or with a modifier', () => {
  const appr = state.interaction, choice = { ...appr, kind: 'choice', options: ['GitHub', 'GitLab'] };
  assert.equal(MC.approvalKey(appr, 'y', false, 1000, 1600, false), 'allow');
  assert.equal(MC.approvalKey(appr, 'A', false, 1000, 1600, false), 'allow_session');
  assert.equal(MC.approvalKey(appr, 'n', false, 1000, 1600, false), 'deny');
  // Within 600 ms of the sheet appearing or of the last key typed nothing answers.
  assert.equal(MC.approvalKey(appr, 'y', false, 1000, 1599, false), null);
  assert.equal(MC.approvalKey(appr, 'y', false, 1000, 1600, true), null);
  assert.equal(MC.approvalKey(appr, 'y', true, 1000, 1600, false), null);
  assert.equal(MC.approvalKey(appr, ' ', false, 1000, 1600, false), null);
  assert.equal(MC.approvalKey(appr, 'Enter', false, 1000, 1600, false), null);
  assert.equal(MC.approvalKey(choice, '2', false, 1000, 1600, false), 1);
  assert.equal(MC.approvalKey(choice, '3', false, 1000, 1600, false), null);
  assert.equal(MC.approvalKey(choice, 'y', false, 1000, 1600, false), null);
});

test('timestamps read as a clock today and carry the day otherwise', () => {
  const tz = -480, now = Date.parse('2026-10-25T06:00:00Z');
  assert.equal(MC.stamp(Date.parse('2026-10-25T01:05:00Z'), now, tz), '09:05');
  assert.equal(MC.stamp(Date.parse('2026-10-24T01:05:00Z'), now, tz), 'Yesterday 09:05');
  assert.equal(MC.stamp(Date.parse('2026-10-03T01:05:00Z'), now, tz), 'Oct 3, 09:05');
  assert.equal(MC.tilde('/Users/me/Develops/app'), '~/Develops/app');
  assert.equal(MC.tilde('/home/me'), '~');
  assert.equal(MC.tilde('/srv/work'), '/srv/work');
  assert.equal(MC.fmtNum(48610), '48,610');
});

const commands = [{ name: 'fix-tests', description: 'Run the tests and fix failures', hint: '[path]' }, { name: 'review', description: 'Review', hint: '' }];

test('the slash list filters by name with prefix matches first and templates before built-ins', () => {
  const names = text => plain(MC.slashMenu(text, commands)).map(c => c.name);
  assert.deepEqual(names('/').slice(0, 3), ['fix-tests', 'review', 'new']);
  assert.ok(names('/').includes('settings'));
  assert.deepEqual(names('/re'), ['review', 'rename']);
  assert.deepEqual(names('/ss'), ['session', 'sessions']);
  assert.deepEqual(names('/s').slice(0, 3), ['session', 'sessions', 'settings']);
  assert.deepEqual(names('/fix'), ['fix-tests']);
  assert.deepEqual(names('/zzz'), []);
  // Only while the composer holds just the command word.
  assert.equal(MC.slashMenu('/fix-tests src', commands), null);
  assert.equal(MC.slashMenu('hello /fix', commands), null);
  assert.equal(plain(MC.slashMenu('/', commands)).find(c => c.name === 'review').builtin, false);
});

test('a slash line is a built-in run here, a template sent as text, or refused as unknown', () => {
  assert.deepEqual(plain(MC.parseSlash('/compact', commands)), { builtin: 'compact', arg: '' });
  assert.deepEqual(plain(MC.parseSlash('  /rename Fix the parser  ', commands)), { builtin: 'rename', arg: 'Fix the parser' });
  assert.deepEqual(plain(MC.parseSlash('/diff', commands)), { builtin: 'changes', arg: '' });
  assert.deepEqual(plain(MC.parseSlash('/fix-tests src/parser', commands)), { template: 'fix-tests', arg: 'src/parser' });
  assert.deepEqual(plain(MC.parseSlash('/nope', commands)), { unknown: 'nope' });
  // Text the host sends as it is: a word with more text, paths, an escaped slash, a lone slash.
  for (const t of ['/nope more text', '/Users/me/app is broken', '/Users/me/app', '//compact', '/ hello']) assert.equal(MC.parseSlash(t, commands), null, t);
  assert.equal(MC.parseSlash('fix the /tmp path', commands), null);
  assert.equal(MC.literal('/fix'), true);
  assert.equal(MC.literal('@src'), true);
  assert.equal(MC.literal('Fix /tmp'), false);
});

test('an @ mention is found at the caret and replaced by the chosen path', () => {
  assert.deepEqual(plain(MC.atToken('see @par', 8)), { start: 4, end: 8, query: 'par' });
  assert.deepEqual(plain(MC.atToken('@', 1)), { start: 0, end: 1, query: '' });
  // The caret inside the word: the whole word is replaced.
  assert.deepEqual(plain(MC.atToken('see @parser now', 6)), { start: 4, end: 11, query: 'p' });
  assert.equal(MC.atToken('mail me@host', 12), null);
  assert.equal(MC.atToken('see @par now', 12), null);
  assert.deepEqual(plain(MC.insertMention('see @par now', { start: 4, end: 8 }, 'src/parser.rs')), { text: 'see @src/parser.rs now', caret: 19 });
  assert.deepEqual(plain(MC.insertMention('@', { start: 0, end: 1 }, 'a.rs')), { text: '@a.rs ', caret: 6 });
});

test('recent prompts keep the newest first without repeats and at most fifty', () => {
  let list = [];
  for (const t of ['a', 'b', 'a', 'c']) list = MC.pushRecent(list, t);
  assert.deepEqual(plain(list), ['c', 'a', 'b']);
  for (let k = 0; k < 60; k++) list = MC.pushRecent(list, 'p' + k);
  assert.equal(list.length, 50);
  assert.equal(list[0], 'p59');
});

const SAMPLES = [
  '# Title\n\nIntro with **bold** and `code`.\n\n- a\n  - b\n- c\n\n3. x\n4. y\n\n```rust\nfn main() {\n\n    println!("hi");\n}\n```\n\n| A | B |\n|:--|--:|\n| 1 | **2** |\n| 3 | 4 |\n\n> quote\n> more\n\n---\n\nThe end.',
  'Para one\nstill one\n\n- item\n\n- loose item\n  continued\n\nafter\n\n```\nunclosed fence\n\nwith blank lines',
  'Text\r\n\r\n- crlf list\r\n- two\r\n\r\n## Heading\r\nbody',
  '1. one\n2. two\n\n   indented para\n\n~~~\ntilde fence\n~~~\n\n- \n- x\n\n> q\n\nplain\n|not|a table\n\n|a|b|\n|-|-|\n|1|2|\n\nend',
];

test('incremental markdown equals a full parse for random splits and keeps finished blocks', () => {
  let seed = 7;
  const rnd = n => { seed = (seed * 1103515245 + 12345) % 2147483648; return seed % n; };
  for (const doc of SAMPLES) {
    for (let run = 0; run < 40; run++) {
      let cache = null, at = 0, kept = 0;
      while (at < doc.length) {
        at = Math.min(doc.length, at + 1 + rnd(run % 3 ? 12 : 3));
        const prev = cache, r = MC.mdInc(cache, doc.slice(0, at));
        assert.deepEqual(plain(r.blocks), plain(MC.parseMd(doc.slice(0, at))), JSON.stringify(doc.slice(0, at)));
        // Blocks frozen earlier are handed back as the same objects.
        if (prev) for (let k = 0; k < prev.head.length; k++) assert.equal(r.blocks[k], prev.head[k]);
        kept = Math.max(kept, r.cache.head.length);
        cache = r.cache;
      }
      assert.ok(kept > 0 || doc.length < 40);
    }
  }
  // A text that is not an extension of the last one starts over.
  const a = MC.mdInc(null, 'one\n\ntwo\n\nthree\n\nfour\n'), b = MC.mdInc(a.cache, 'other\n\ntext');
  assert.deepEqual(plain(b.blocks), plain(MC.parseMd('other\n\ntext')));
});

const tool = (call_id, name, extra = {}) => ({ type: 'tool', call_id, name, detail: call_id, input: null, input_lines: null, state: 'succeeded', exit_code: 0, duration_ms: 10, output: [], output_dropped: 0, output_lines: 0, truncated: false, ...extra });
const said = (text, reasoning = '', streaming = false) => ({ type: 'assistant', loop_id: 'l', request: 1, reasoning, text, streaming });
const ask = text => ({ type: 'user', kind: 'prompt', text, at: 0 });
const end = (loop_id, extra = {}) => ({ type: 'turn_end', loop_id, outcome: 'completed', duration_ms: 47000, requests: 2, tools: 3, usage: { input: 0, output: 0, cached: 0 }, changes: { files: 1, added: 12, removed: 4 }, ...extra });

test('a finished turn folds tools and intermediate thinking before the final answer; the running turn does not', () => {
  const bl = [ask('go'), said('', 'plan'), tool('a', 'bash'), said('checking'), tool('b', 'read', { state: 'failed' }), said(''), { type: 'user', kind: 'steering', text: 'also', at: 0 }, tool('c', 'edit'), said('Done.', 'final thought'), end('t1'),
    ask('next'), tool('d', 'bash'), said('working', '', true)];
  const f = plain(MC.foldTurns(bl, 0, bl.length));
  assert.equal(f.length, 1);
  assert.deepEqual(f[0], { key: 'ft1', at: 1, last: 7, hidden: [1, 2, 3, 4, 7], tools: 3, failed: 1, ms: 47000, added: 12, removed: 4 });
  assert.equal(MC.foldText(f[0]), 'Worked 47.0 s');
  // No answer after the last tool: everything up to the turn end folds.
  const noAnswer = plain(MC.foldTurns([ask('go'), said('first'), tool('a', 'bash'), end('t2', { changes: null })], 0, 4));
  assert.deepEqual(noAnswer[0].hidden, [1, 2]);
  assert.equal(noAnswer[0].added, null);
  // A turn of only an answer has nothing to fold; a window that starts mid-turn folds from its start.
  assert.deepEqual(plain(MC.foldTurns([ask('hi'), said('hello'), end('t3')], 0, 3)), []);
  assert.deepEqual(plain(MC.foldTurns(bl, 3, 10))[0].hidden, [3, 4, 7]);
});

test('consecutive reads and searches group into one row with a count', () => {
  const bl = [tool('a', 'read'), said(''), tool('b', 'read'), tool('c', 'grep'), tool('d', 'bash'), tool('e', 'read'), tool('f', 'glob'), said('x'), tool('g', 'read'), tool('h', 'read', { state: 'running' }), tool('i', 'grep'), tool('j', 'grep')];
  const g = plain(MC.groupTools(bl, 0, 9));
  assert.deepEqual(g.map(x => [x.key, x.members, x.label]), [['ga', [0, 2, 3], 'Explored 2 files · 1 search'], ['ge', [5, 6], 'Explored 1 file · 1 search']]);
  assert.deepEqual(plain(MC.groupTools(bl, 8, 10)).map(x => [x.label, x.run]), [['Reading 2 files', true]]);
  assert.deepEqual(plain(MC.groupTools(bl, 10, 12)).map(x => x.label), ['Searched 2 times']);
  assert.deepEqual(plain(MC.groupTools([tool('a', 'read')], 0, 1)), []);
});

test('find matches messages, answers, thinking, tool targets and output case-insensitively', () => {
  const bl = [ask('Fix the Parser'), said('The parser is fixed', 'think about PARSER'), tool('a', 'bash', { detail: 'cargo test', output: ['parser ok'] }), end('t')];
  assert.deepEqual(plain(MC.findBlocks(bl, 'parser', 0, 4)), [0, 1, 2]);
  assert.deepEqual(plain(MC.findBlocks(bl, 'cargo', 1, 4)), [2]);
  assert.deepEqual(plain(MC.findBlocks(bl, '', 0, 4)), []);
});

test('approvals are titled by what is asked and show the command, diff or content first', () => {
  const a = (tool, detail, input) => ({ id: 'x', kind: 'approval', tool, detail, input, prompt: '', options: ['Allow once', 'Deny', 'Allow ' + tool + ' this session'] });
  assert.deepEqual(plain(MC.approvalTitle(a('bash', 'cargo test', 'cargo test'))), { text: 'Run command' });
  const patch = '*** Begin Patch\n*** Update File: src/x.rs\n@@\n-a\n+b\n*** End Patch';
  assert.deepEqual(plain(MC.approvalTitle(a('apply_patch', 'src/x.rs', patch))), { text: 'Edit', code: 'src/x.rs' });
  assert.deepEqual(plain(MC.approvalTitle(a('apply_patch', '2 files', patch + '\n*** Add File: y.md\n+hi'))), { text: 'Edit 2 files' });
  assert.deepEqual(plain(MC.approvalTitle(a('write', 'README.md', '# Hi'))), { text: 'Write', code: 'README.md' });
  assert.deepEqual(plain(MC.approvalTitle(a('web_fetch', 'https://x.test', null))), { text: 'Fetch', code: 'https://x.test' });
  assert.deepEqual(plain(MC.approvalTitle(a('mcp_thing', 'arg', null))), { text: 'Use', code: 'mcp_thing' });
  assert.deepEqual(plain(MC.approvalBody(a('bash', 'ls', null))), { cmd: 'ls' });
  assert.equal(plain(MC.approvalBody(a('apply_patch', 'src/x.rs', patch))).diff[0].path, 'src/x.rs');
  const w = plain(MC.approvalBody(a('write', 'README.md', '# Hi\nthere'))).diff[0];
  assert.deepEqual([w.path, w.added, w.hunks[0].lines.map(l => [l.t, l.s, l.b])], ['README.md', 2, [['add', '# Hi', 1], ['add', 'there', 2]]]);
  assert.deepEqual(plain(MC.approvalBody(a('mcp_thing', 'arg', 'arg'))), { text: '' });
});

test('approval buttons use the host labels, allow first and deny last; choices are numbered', () => {
  assert.deepEqual(plain(MC.approvalOptions(state.interaction)), [
    { choice: 'allow', label: 'Allow once' }, { choice: 'allow_session', label: 'Allow bash this session' }, { choice: 'deny', label: 'Deny' }]);
  assert.deepEqual(plain(MC.approvalOptions({ kind: 'choice', options: ['GitHub', 'GitLab'] })), [{ choice: 0, label: 'GitHub', n: 1 }, { choice: 1, label: 'GitLab', n: 2 }]);
});

test('the context view reads the reply as a bar with markers and rows for the parts present', () => {
  const full = { tokens: 46800, window: 200000, percent: 23.4, budget: 180000, history_tokens: 38500, trigger_tokens: 160000, target_tokens: 60000, compacting: false,
    coverage: { turns: 14, items: 212, kept: 58 }, last_compaction: { origin: 'manual', status: 'completed', before: 182400, after: 9100 }, failure: null };
  const v = plain(MC.contextView(full));
  assert.deepEqual([v.percent, v.used, v.trigger, v.target, v.failure], [23.4, '46.8k of 200.0k tokens', 80, 30, '']);
  assert.deepEqual(v.rows, [['History', '38.5k tokens'], ['Compacts at', '160.0k → 60.0k'], ['Covers', '14 turns · 212 items · 58 kept'], ['Last compaction', 'Manual · completed · 182.4k → 9.1k']]);
  const bare = plain(MC.contextView({ tokens: 10, window: 100, percent: 10, history_tokens: null, trigger_tokens: null, target_tokens: null, coverage: null, last_compaction: null, failure: 'model refused', compacting: true }));
  assert.deepEqual([bare.rows, bare.trigger, bare.failure, bare.compacting], [[], null, 'model refused', true]);
});

test('changed files map their status to a badge and the transcript exports as Markdown', () => {
  assert.deepEqual(plain(['added', 'modified', 'deleted', 'renamed', 'conflict', 'unknown', 'other'].map(MC.changeStatus)).map(x => x[0]).join(''), 'AMDR!??');
  const md = MC.exportMd(state, [blocks[0], { ...blocks[1], streaming: false }, blocks[2], blocks[3], blocks[4]]);
  assert.equal(md, '# One\n\n`/work` · model\n\n**You (steer):** now\n\n**hello**\n\n- Ran `pwd`\n\n_Completed · 7.8 s · 12 tokens_\n\n_Context compacted · 32.1k → 2.7k tokens_\n');
});

test('composer keys: Enter and Cmd/Ctrl+Enter steer or send, Alt+Enter queues, Tab and Shift+Enter are left alone', () => {
  const base = { key: 'Enter', shift: false, alt: false, mod: false };
  const ctx = { busy: true, text: 'next', popup: false, enter: true };
  // [name, key, expected as [busy, idle] for "Enter sends" on, then off]
  const table = [
    ['Enter', {}, ['steer', 'send'], [null, null]],
    ['Shift+Enter', { shift: true }, [null, null], [null, null]],
    ['Alt+Enter', { alt: true }, ['queue', null], ['queue', null]],
    ['Cmd/Ctrl+Enter', { mod: true }, ['steer', 'send'], ['steer', 'send']],
    ['Tab', { key: 'Tab' }, [null, null], [null, null]],
    ['Alt+Tab', { key: 'Tab', alt: true }, [null, null], [null, null]],
    ['a', { key: 'a' }, [null, null], [null, null]],
  ];
  for (const [name, mods, on, off] of table) {
    for (const [enter, want] of [[true, on], [false, off]]) {
      for (const [i, busy] of [true, false].entries()) {
        assert.equal(MC.composerKey({ ...base, ...mods }, { ...ctx, busy, enter }), want[i], `${name} busy=${busy} enter=${enter}`);
      }
    }
  }
  // An open list keeps Enter and Alt+Enter for completion; Cmd/Ctrl+Enter still steers or sends.
  for (const [name, mods, want] of [['Enter', {}, null], ['Shift+Enter', { shift: true }, null], ['Alt+Enter', { alt: true }, null],
    ['Tab', { key: 'Tab' }, null], ['Cmd/Ctrl+Enter', { mod: true }, 'steer']]) {
    assert.equal(MC.composerKey({ ...base, ...mods }, { ...ctx, popup: true }), want, name + ' with a list');
  }
  // Nothing typed: there is nothing to queue.
  assert.equal(MC.composerKey({ ...base, alt: true }, { ...ctx, text: '  \n' }), null);
});

test('the primary button is send when idle, stop while working with nothing typed, steer with text typed during a turn', () => {
  const face = (busy, compacting, text, stopping = false) => MC.primaryKind({ busy, compacting, text, stopping });
  // [busy, compacting, text, stopping] -> face
  const table = [
    [false, false, '', false, 'send'], [false, false, 'hi', false, 'send'], [false, false, '  \n', false, 'send'],
    [true, false, '', false, 'stop'], [true, false, '  ', false, 'stop'], [true, false, 'hi', false, 'steer'],
    [false, true, '', false, 'stop'], [false, true, 'hi', false, 'send'],
    [true, false, '', true, 'stopping'], [true, false, 'hi', true, 'stopping'], [false, true, '', true, 'stopping'],
  ];
  for (const [busy, compacting, text, stopping, want] of table) assert.equal(face(busy, compacting, text, stopping), want, JSON.stringify([busy, compacting, text, stopping]));
});

test('a tap on the primary decides from the live text and ignores double taps on Send and scrolls', () => {
  const tap = (c, now = 10000, moved = 0) => MC.primaryTap({ busy: true, compacting: false, text: '', stopping: false, stopAt: 0, ...c }, now, moved);
  assert.equal(tap({}), 'stop');
  assert.equal(tap({ compacting: true, busy: false }), 'stop');
  // The button still shows Stop but text was typed since: the tap steers, it never stops.
  assert.equal(tap({ text: 'next' }), 'steer');
  assert.equal(tap({ text: 'next', busy: false }), 'send');
  assert.equal(tap({ busy: false }), null, 'nothing to send');
  assert.equal(tap({ stopping: true }), null);
  // A tap within 600 ms after the button became Stop is the second half of a double tap on Send.
  assert.equal(tap({ stopAt: 9401 }), null);
  assert.equal(tap({ stopAt: 9400 }), 'stop');
  assert.equal(tap({ stopAt: 9401, text: 'next' }), 'steer', 'the guard only protects Stop');
  // A pointer that travelled over 10 px was a scroll.
  assert.equal(tap({}, 10000, 11), null);
  assert.equal(tap({}, 10000, 10), 'stop');
  assert.equal(tap({ text: 'next' }, 10000, 30), null);
});

test('the queue shows follow-ups numbered in order, waiting steers, and the first three until expanded', () => {
  const q = [{ kind: 'steer', text: 's' }, { kind: 'follow', text: 'a' }, { kind: 'follow', text: 'b' }, { kind: 'follow', text: 'c' }, { kind: 'follow', text: 'd' }];
  const v = plain(MC.queueView(q, false));
  assert.deepEqual(v.items.map(x => [x.kind, x.index, x.n]), [['steer', 0, 0], ['follow', 1, 1], ['follow', 2, 2]]);
  assert.deepEqual([v.hidden, v.long], [2, true]);
  const all = plain(MC.queueView(q, true));
  assert.deepEqual(all.items.map(x => x.n), [0, 1, 2, 3, 4]);
  assert.deepEqual([all.hidden, all.long], [0, true]);
  assert.deepEqual(plain(MC.queueView(q.slice(0, 3), false)), { items: plain(MC.queueView(q.slice(0, 3), true)).items, hidden: 0, long: false });
  assert.deepEqual(plain(MC.queueView([], false)), { items: [], hidden: 0, long: false });
  // A steer sent after a follow-up is still listed first and keeps its wire index.
  const mixed = plain(MC.queueView([{ kind: 'follow', text: 'a' }, { kind: 'steer', text: 'b' }, { kind: 'follow', text: 'c' }], true));
  assert.deepEqual(mixed.items.map(x => [x.kind, x.index, x.n]), [['steer', 1, 0], ['follow', 0, 1], ['follow', 2, 2]]);
});

test('the thinking preview is the newest sentence without Markdown markers', () => {
  const p = MC.thinkingPreview;
  assert.equal(p(''), '');
  assert.equal(p('Check the parser'), 'Check the parser');
  assert.equal(p('First line\nSecond line\n\n  \n'), 'Second line');
  // A long paragraph shows its newest sentence, also while that sentence is still being written.
  assert.equal(p('The user reports a bug. I should read the code. Then run the'), 'Then run the');
  assert.equal(p('The user reports a bug. I should read the code.'), 'I should read the code.');
  assert.equal(p('先读代码。然后运行测试'), '然后运行测试');
  assert.equal(p('Does it fail? Yes it does!'), 'Yes it does!');
  // Paths and versions with dots are not sentence ends.
  assert.equal(p('Open src/ui/mod.rs and v1.2.3 now'), 'Open src/ui/mod.rs and v1.2.3 now');
  assert.equal(p('**Considering the fix**'), 'Considering the fix');
  assert.equal(p('# Plan\n- read `split_path` in __main__\n'), 'read split_path in main');
  assert.equal(p('1. Run [the tests](https://x.test/a) first'), 'Run the tests first');
  assert.equal(p('> quoted ~~old~~ idea'), 'quoted old idea');
  assert.equal(p('thinking\n---\n'), '---');
  // Only the tail is read: a huge text costs the same and still ends with the newest line.
  assert.equal(p('x\n'.repeat(100000) + 'last words here'), 'last words here');
});

test('Windows paths split into drive, folder and UNC crumbs and accept either separator', () => {
  const parts = path => plain(MC.splitPath(path)).map(c => [c.name, c.path]);
  assert.deepEqual(parts('C:\\'), [['C:', 'C:\\']]);
  assert.deepEqual(parts('C:\\Users\\x'), [['C:', 'C:\\'], ['Users', 'C:\\Users'], ['x', 'C:\\Users\\x']]);
  assert.deepEqual(parts('D:\\work'), [['D:', 'D:\\'], ['work', 'D:\\work']]);
  assert.deepEqual(parts('\\\\server\\share\\dir'), [['server', '\\\\server'], ['share', '\\\\server\\share'], ['dir', '\\\\server\\share\\dir']]);
  // `/` inside a drive path is the same separator; the crumbs come back in the `\` form the host uses.
  assert.deepEqual(parts('C:/Users/x'), parts('C:\\Users\\x'));
  assert.deepEqual(parts('C:\\Users/x\\src'), parts('C:\\Users\\x\\src'));
  assert.deepEqual(parts('/work/app'), [['/', '/'], ['work', '/work'], ['app', '/work/app']]);
  assert.deepEqual(parts('C:'), [['C:', 'C:\\']]);
  assert.equal(MC.basename('C:\\'), 'C:');
  assert.equal(MC.basename('D:\\work\\app'), 'app');
  assert.equal(MC.basename('\\\\server\\share'), 'share');
  assert.equal(MC.isDrive('C:\\'), true);
  assert.equal(MC.isDrive('d:'), true);
  assert.equal(MC.isDrive('C:\\Users'), false);
  assert.equal(MC.isDrive('/'), false);
  assert.equal(MC.isDrive('\\\\server\\share'), false);
});

test('Windows paths compare by folder and without case, and never match another drive or a longer name', () => {
  assert.equal(MC.within('C:\\', 'C:\\'), true);
  assert.equal(MC.within('C:\\Users\\x', 'C:\\'), true);
  assert.equal(MC.within('C:\\Users\\x', 'C:\\Users'), true);
  assert.equal(MC.within('C:\\Users\\x', 'C:\\Users\\x'), true);
  assert.equal(MC.within('c:\\users\\X', 'C:\\Users'), true);
  assert.equal(MC.within('C:/Users/x', 'C:\\Users'), true);
  assert.equal(MC.within('C:\\Users2', 'C:\\Users'), false);
  assert.equal(MC.within('D:\\work', 'C:\\'), false);
  assert.equal(MC.within('C:\\work', 'D:\\'), false);
  assert.equal(MC.within('\\\\server\\share\\dir', '\\\\server\\share'), true);
  assert.equal(MC.within('\\\\server\\share2', '\\\\server\\share'), false);
  assert.equal(MC.within('/work/app', 'C:\\'), false);
  assert.equal(MC.within('C:\\work', '/work'), false);
  assert.equal(MC.within('/work', '/'), true);
});

test('crumbs start at the drive root or share root that holds the folder, one tap from Roots', () => {
  const roots = [{ path: 'C:\\', name: 'C:' }, { path: 'D:\\', name: 'D:' }, { path: 'D:\\work', name: 'work' }];
  const c = (path, r = roots) => plain(MC.crumbs(path, r)).map(x => [x.name, x.path]);
  assert.deepEqual(c('C:\\'), [['C:', 'C:\\']]);
  assert.deepEqual(c('C:\\Users\\x'), [['C:', 'C:\\'], ['Users', 'C:\\Users'], ['x', 'C:\\Users\\x']]);
  // The innermost root wins, also when the host's casing differs.
  assert.deepEqual(c('D:\\work\\app'), [['work', 'D:\\work'], ['app', 'D:\\work\\app']]);
  assert.deepEqual(c('d:\\Work\\app'), [['work', 'D:\\work'], ['app', 'd:\\Work\\app']]);
  assert.deepEqual(c('D:\\other'), [['D:', 'D:\\'], ['other', 'D:\\other']]);
  assert.deepEqual(c('C:\\Users\\x', [{ path: '\\\\nas\\share', name: 'share' }]).map(x => x[0]), ['C:', 'Users', 'x']);
  assert.deepEqual(c('\\\\nas\\share\\proj\\src', [{ path: '\\\\nas\\share', name: 'share' }]), [['share', '\\\\nas\\share'], ['proj', '\\\\nas\\share\\proj'], ['src', '\\\\nas\\share\\proj\\src']]);
});

test('home folders read as ~ on Windows too, and recent workspaces on other drives keep their path', () => {
  assert.equal(MC.tilde('C:\\Users\\me\\work\\app'), '~\\work\\app');
  assert.equal(MC.tilde('c:\\users\\me'), '~');
  assert.equal(MC.tilde('C:/Users/me/work'), '~/work');
  assert.equal(MC.tilde('D:\\work\\app'), 'D:\\work\\app');
  assert.equal(MC.tilde('C:\\Usersx\\me'), 'C:\\Usersx\\me');
  assert.equal(MC.tilde('\\\\nas\\share\\Users\\me'), '\\\\nas\\share\\Users\\me');
  assert.equal(MC.tilde('/Users/me/work'), '~/work');
  assert.equal(MC.tilde(null), '');
  const sessions = [{ workspace: 'C:\\Users\\me\\app' }, { workspace: 'D:\\work\\api' }, { workspace: 'C:\\Users\\me\\app' }, { workspace: '\\\\nas\\share\\site' }];
  const recent = plain(MC.recentWorkspaces(sessions));
  assert.deepEqual(recent.map(w => [MC.basename(w), MC.tilde(w)]), [['app', '~\\app'], ['api', 'D:\\work\\api'], ['site', '\\\\nas\\share\\site']]);
});
