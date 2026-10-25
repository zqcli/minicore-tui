import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const html = readFileSync(new URL('../src/remote/web/index.html', import.meta.url), 'utf8');
const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map(match => match[1]);

function client() {
  const sent = [], effects = [], elements = new Map(), storage = new Map();
  const makeElement = () => ({
    value: '', style: {}, children: [],
    append(...children) { this.children.push(...children); },
    replaceChildren(...children) { this.children = children; },
    get firstChild() { return this.children[0]; },
  });
  const element = id => {
    if (!elements.has(id)) elements.set(id, makeElement());
    return elements.get(id);
  };
  class Socket {
    readyState = 1;
    send(message) { sent.push(JSON.parse(message)); }
    close() { this.readyState = 3; }
  }
  const context = vm.createContext({
    document: { getElementById: element, createElement: makeElement, addEventListener() {} },
    location: { protocol: 'https:', host: 'host.example', hash: '', pathname: '/', search: '' },
    window: {}, addEventListener() {}, URLSearchParams,
    navigator: { userAgent: 'iPhone Safari' },
    localStorage: {
      getItem: key => storage.get(key) ?? null,
      setItem: (key, value) => storage.set(key, value),
      removeItem: key => storage.delete(key),
    },
    WebSocket: Socket,
    setTimeout() { return 1; }, clearTimeout() {}, effects,
  });
  const run = source => vm.runInContext(source, context);
  // Load every inline script; browser events and rendering are stubbed, not removed by markers.
  run(scripts.join('\n'));
  run(`
    renderTray = renderAppr = updateComposer = renderState = schedule = grow = setConn = () => {};
    toast = (...args) => effects.push(['notice', ...args]);
    gate = (...args) => effects.push(['gate', ...args]);
  `);
  return { run, sent, effects, storage, element,
    value: expression => JSON.parse(run(`JSON.stringify(${expression})`)),
    message: message => run(`onMessage(${JSON.stringify(message)})`),
  };
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
  { type: 'turn_end', loop_id: 'loop', outcome: 'completed', duration_ms: 1000, requests: 1, tools: 1, usage: { input: 10, output: 2, cached: 1 }, changes: { files: 1, added: 2, removed: 0 } },
  { type: 'compaction', before: 1000, after: 100, summary: 'summary' },
  { type: 'earlier', hidden_items: 12 },
];

test('pair credentials are stored and subsequent connections send hello v1', () => {
  const c = client();
  c.run("pairCode = 'ABCDEFGH'; connect(); ws.onopen()");
  assert.deepEqual(c.sent[0], { t: 'pair', v: 1, code: 'ABCDEFGH', device: 'iPhone Safari' });
  c.message({ t: 'paired', token: 'test-token' });
  assert.equal(c.storage.get('minicore.token'), 'test-token');
  c.message({ t: 'welcome', v: 1, version: '0.5.5', device: 'iPhone Safari' });
  c.run('connect(); ws.onopen()');
  assert.deepEqual(c.sent[1], { t: 'hello', v: 1, token: 'test-token' });
});

test('every intent uses the protocol kind and fields with bounded string ids', () => {
  const c = client();
  c.run('ws = new WebSocket(); ready = true');
  const intents = [
    ['submit', { text: 'send' }], ['queue', { text: 'later' }], ['cancel', {}],
    ['answer', { interaction_id: 'approval', choice: 'allow' }],
    ['answer', { interaction_id: 'approval', choice: 'allow_session' }],
    ['answer', { interaction_id: 'approval', choice: 'deny' }],
    ['answer', { interaction_id: 'choice', choice: 1 }],
    ['load_older', {}], ['open_session', { session_id: 'ses_one' }],
    ['new_session', { workspace: '/work', profile: 'coding' }],
    ['set_model', { model: 'model' }], ['set_reasoning', { level: 'high' }],
  ];
  intents.forEach(([kind, fields], index) => {
    c.run(`intent(${JSON.stringify(kind)}, ${JSON.stringify(fields)})`);
    assert.deepEqual(c.sent[index], { t: 'intent', id: `c${index + 1}`, kind, ...fields });
    assert.ok(c.sent[index].id.length <= 64);
  });
});

test('filesystem requests correlate roots listing creation and errors by id', () => {
  const c = client();
  c.run('ws = new WebSocket(); ready = true; globalThis.replies = []');
  const cases = [
    [{ t: 'fs_roots' }, { ok: true, roots: [{ path: '/work', name: 'work' }] }],
    [{ t: 'fs_list', path: '/work', hidden: false }, { ok: true, listing: { path: '/work', parent: null, entries: [{ path: '/work/app', name: 'app' }], truncated: false } }],
    [{ t: 'fs_mkdir', parent: '/work', name: 'new' }, { ok: true, created: '/work/new' }],
    [{ t: 'fs_list', path: '/outside', hidden: true }, { ok: false, error: 'outside_roots', message: 'Path is outside the configured roots' }],
  ];
  cases.forEach(([request, fields], index) => {
    c.run(`fsCall(${JSON.stringify(request)}, r => replies.push(r))`);
    const id = `c${index + 1}`;
    assert.deepEqual(c.sent[index], { ...request, id });
    const reply = { t: 'fs', id, ...fields };
    c.message(reply);
    assert.deepEqual(c.value(`replies[${index}]`), reply);
  });
  assert.equal(c.value('fsReq.size'), 0);
});

test('snapshot blocks and state retain protocol fields and honor truncation', () => {
  const c = client();
  c.message({ t: 'snapshot', state, blocks });
  assert.deepEqual(c.value('m.state'), state);
  assert.deepEqual(c.value('m.blocks'), blocks);
  const changed = { ...blocks[1], text: 'finished', streaming: false };
  c.message({ t: 'blocks', len: 2, upserts: [{ i: 1, block: changed }] });
  assert.deepEqual(c.value('m.blocks'), [blocks[0], changed]);
  const idle = { ...state, status: { ...state.status, busy: false, started_ms: null }, interaction: null };
  c.message({ t: 'state', state: idle });
  assert.deepEqual(c.value('m.state'), idle);
  c.message({ t: 'snapshot', state: { ...idle, session: null }, blocks: [] });
  assert.deepEqual(c.value('m.blocks'), []);
  assert.equal(c.value('m.sid'), null);
  assert.equal(c.run('LABEL.steering'), 'Steering');
  assert.equal(c.run('LABEL.steer'), 'Steering');
});

test('ack returned notice and bye preserve best-effort send semantics', () => {
  const c = client();
  c.run('ws = new WebSocket(); ready = true');
  c.element('ta').value = 'draft';
  c.run("intent('submit', {text: 'draft'})");
  c.message({ t: 'ack', id: 'c1', ok: false, error: 'refused' });
  assert.equal(c.element('ta').value, 'draft');
  c.run("intent('submit', {text: 'draft'})");
  c.message({ t: 'ack', id: 'c2', ok: true });
  assert.equal(c.element('ta').value, '');
  c.element('ta').value = 'next';
  c.message({ t: 'returned', text: 'draft' });
  assert.equal(c.element('ta').value, 'next\ndraft');
  assert.equal(c.value('m.intents.size'), 0);
  c.message({ t: 'notice', level: 'error', text: 'agent refused' });
  assert.deepEqual([...c.effects.at(-1)], ['notice', 'agent refused', 'error']);
  for (const reason of ['remote_off', 'tui_exit', 'replaced']) {
    c.message({ t: 'bye', reason });
    assert.equal(c.effects.at(-1)[0], 'gate');
    assert.equal(c.effects.at(-1)[3], reason !== 'replaced');
  }
});

test('unauthorized removes the token and other handshake errors retain it', () => {
  const c = client();
  c.storage.set('minicore.token', 'test-token');
  for (const code of ['busy', 'version', 'bad_request']) {
    c.message({ t: 'error', code, message: code });
    assert.equal(c.effects.at(-1)[0], 'gate');
    assert.equal(c.storage.get('minicore.token'), 'test-token');
  }
  c.message({ t: 'error', code: 'unauthorized', message: 'unauthorized' });
  assert.equal(c.storage.has('minicore.token'), false);
});

test('breadcrumbs preserve Unix drive and UNC paths returned by the server', () => {
  const c = client();
  for (const path of ['/work/app', 'C:\\work\\app', '\\\\server\\share\\app']) {
    const crumbs = c.value(`splitPath(${JSON.stringify(path)})`);
    assert.equal(crumbs.at(-1).path, path);
  }
  const unc = c.value(`splitPath(${JSON.stringify('\\\\server\\share\\app')})`);
  assert.equal(unc[1].path, '\\\\server\\share');
  assert.equal(c.run(`within(${JSON.stringify(unc[2].path)}, ${JSON.stringify(unc[1].path)})`), true);
});
