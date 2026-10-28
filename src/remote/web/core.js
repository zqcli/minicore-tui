// ---- markdown ----
// Markdown as plain token trees, never HTML; the UI builds DOM nodes from them.
const SAFE_URL = /^https?:\/\//i;
function inline(s) {
  const out = [];
  let buf = '', i = 0, m;
  const flush = () => { if (buf) out.push({ t: 'text', s: buf }); buf = ''; };
  while (i < s.length) {
    const c = s[i];
    if (c === '\\' && /^[\\`*_{}[\]()#+\-.!|<>~]$/.test(s[i + 1] || '')) { buf += s[i + 1]; i += 2; }
    else if (c === '`') {
      let n = 1;
      while (s[i + n] === '`') n++;
      const e = s.indexOf('`'.repeat(n), i + n);
      if (e < 0) { buf += '`'.repeat(n); i += n; } else { flush(); out.push({ t: 'code', s: s.slice(i + n, e) }); i = e + n; }
    } else if (c === '*' || c === '_' || (c === '~' && s[i + 1] === '~')) {
      const dbl = s[i + 1] === c, w = dbl ? 2 : 1, e = s.indexOf(c.repeat(w), i + w);
      const open = s[i + w] && !/\s/.test(s[i + w]) && (c !== '_' || !/\w/.test(s[i - 1] || ''));
      if (open && e > i + w && !/\s/.test(s[e - 1]) && (c !== '_' || !/\w/.test(s[e + w] || ''))) {
        flush(); out.push({ t: c === '~' ? 's' : dbl ? 'b' : 'i', c: inline(s.slice(i + w, e)) }); i = e + w;
      } else { buf += c; i++; }
    } else if (c === '[' && (m = /^\[([^\]]*)\]\(\s*([^)\s]+)(?:\s+"[^"]*")?\s*\)/.exec(s.slice(i)))) {
      flush();
      out.push(SAFE_URL.test(m[2]) ? { t: 'a', href: m[2], c: inline(m[1]) } : { t: 'text', s: m[1] });
      i += m[0].length;
    } else if (c === 'h' && !/\w/.test(s[i - 1] || '') && (m = /^https?:\/\/[^\s<>]+/i.exec(s.slice(i)))) {
      const u = m[0].replace(/[.,;:!?)\]'"]+$/, '');
      flush(); out.push({ t: 'a', href: u, c: [{ t: 'text', s: u }] }); i += u.length;
    } else { buf += c; i++; }
  }
  flush();
  return out;
}
const cells = l => l.trim().replace(/^\|/, '').replace(/\|$/, '').replace(/\\\|/g, '\0').split('|').map(x => x.trim().replace(/\0/g, '|'));
const TSEP = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;
function nest(items, k, ind) {
  const list = { t: 'list', ord: items[k].ord != null, start: items[k].ord || 1, items: [] };
  while (k < items.length && items[k].ind >= ind) {
    const it = items[k];
    if (it.ind > ind && list.items.length) { const r = nest(items, k, it.ind); list.items[list.items.length - 1].sub.push(r.list); k = r.k; }
    else if ((it.ord != null) !== list.ord) break;
    else { list.items.push({ c: inline(it.text), sub: [] }); k++; }
  }
  return { list, k };
}
function parseLines(lines, cps) {
  const out = [], LI = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)/;
  let para = [], i = 0, m;
  const li = k => { const x = LI.exec(lines[k] || ''); return x && (k < lines.length - 1 || x[3].trim()) ? x : null; };
  const flush = () => { if (para.length) out.push({ t: 'p', c: inline(para.join('\n')) }); para = []; };
  while (i < lines.length) {
    const l = lines[i];
    if (cps && !para.length) cps.push([i, out.length]);
    if ((m = /^ {0,3}(`{3,}|~{3,})\s*([^\s`]*)/.exec(l))) {
      flush();
      const fence = m[1], body = [];
      for (i++; i < lines.length; i++) {
        const t = lines[i].trim();
        if (t.length >= fence.length && t === fence[0].repeat(t.length)) break;
        body.push(lines[i]);
      }
      out.push({ t: 'code', lang: m[2], text: body.join('\n') });
      i++;
    } else if ((m = /^ {0,3}(#{1,6})\s+(.*?)(?:\s+#+)?\s*$/.exec(l))) {
      flush(); out.push({ t: 'h', n: m[1].length, c: inline(m[2]) }); i++;
    } else if (/^ {0,3}([-*_])( *\1){2,} *$/.test(l)) {
      flush(); out.push({ t: 'hr' }); i++;
    } else if (/^ {0,3}>/.test(l)) {
      flush();
      const q = [];
      for (; i < lines.length && /^ {0,3}>/.test(lines[i]); i++) q.push(lines[i].replace(/^ {0,3}> ?/, ''));
      out.push({ t: 'quote', c: parseMd(q.join('\n')) });
    } else if (li(i)) {
      flush();
      const items = [];
      while (i < lines.length) {
        if ((m = li(i))) { items.push({ ind: m[1].replace(/\t/g, '    ').length, ord: /\d/.test(m[2]) ? parseInt(m[2], 10) : null, text: m[3] }); i++; }
        else if (items.length && /^\s+\S/.test(lines[i])) { items[items.length - 1].text += '\n' + lines[i].trim(); i++; }
        else if (!lines[i].trim() && li(i + 1)) i++;
        else break;
      }
      for (let k = 0; k < items.length;) { const r = nest(items, k, items[k].ind); out.push(r.list); k = r.k; }
    } else if (l.includes('|') && TSEP.test(lines[i + 1] || '') && (lines[i + 1] || '').includes('-')) {
      flush();
      const align = cells(lines[i + 1]).map(x => /^:-+:$/.test(x) ? 'center' : /-:$/.test(x) ? 'right' : '');
      const head = cells(l).map(inline), rows = [];
      for (i += 2; i < lines.length && lines[i].includes('|') && lines[i].trim(); i++) rows.push(cells(lines[i]).map(inline));
      out.push({ t: 'table', align, head, rows });
    } else {
      if (l.trim()) para.push(l); else flush();
      i++;
    }
  }
  flush();
  return out;
}
const norm = src => src.replace(/\r\n?/g, '\n');
const parseMd = src => parseLines(norm(src).split('\n'), null);
// Streaming Markdown: blocks before the last stable checkpoint are kept and only the tail is parsed again.
function mdInc(c, src) {
  src = norm(src);
  if (!c || !src.startsWith(c.src)) c = { src: '', at: 0, head: [] };
  const lines = src.slice(c.at).split('\n'), cps = [], tail = parseLines(lines, cps);
  let cut = null;
  for (const cp of cps) if (cp[0] > 0 && cp[0] + 2 <= lines.length - 1) cut = cp;
  const next = { src, at: c.at, head: c.head };
  if (cut) {
    let off = 0;
    for (let k = 0; k < cut[0]; k++) off += lines[k].length + 1;
    next.at += off;
    next.head = c.head.concat(tail.slice(0, cut[1]));
  }
  return { blocks: c.head.concat(tail), cache: next };
}


// ---- core ----

const V = 2, WINDOW = 300, MAX_QUERIES = 4;
const backoff = n => Math.min(15000, 1000 * 2 ** n);
const basename = p => (p || '').split(/[\\/]/).filter(Boolean).pop() || p || '';
const plural = (n, one, many) => n + ' ' + (n === 1 ? one : many || one + 's');
const cap = s => s ? s[0].toUpperCase() + s.slice(1) : s;
const titled = title => title || 'Untitled session';
// "gpt-6-…-preview": long names keep both ends.
const midTrunc = (s, n = 17) => s.length <= n ? s : s.slice(0, (n - 1) >> 1) + '…' + s.slice(-((n - 1) >> 1));

// What is left in the editor once an acked submission is retired; text the user or `returned` added stays.
function retireText(editor, sent) {
  if (editor === sent) return '';
  if (editor.startsWith(sent + '\n')) return editor.slice(sent.length + 1);
  if (editor.endsWith('\n' + sent)) return editor.slice(0, -sent.length - 1);
  return editor;
}
const mergeReturned = (cur, text) => cur ? cur + '\n' + text : text;
function otherDevices(devices, me) {
  const rest = devices.slice(), i = rest.findIndex(d => d.name === me);
  if (i >= 0) rest.splice(i, 1);
  return rest;
}

function deviceName(ua, touchPoints = 0) {
  const os = /iPhone/.test(ua) ? 'iPhone' : /iPad/.test(ua) || (/Macintosh/.test(ua) && touchPoints > 1) ? 'iPad'
    : /Android/.test(ua) ? 'Android' : /Macintosh/.test(ua) ? 'Mac' : /Windows/.test(ua) ? 'Windows'
    : /CrOS/.test(ua) ? 'Chromebook' : /Linux/.test(ua) ? 'Linux' : 'Device';
  const br = /EdgiOS|EdgA|Edg\//.test(ua) ? 'Edge' : /OPR\/|OPiOS/.test(ua) ? 'Opera' : /SamsungBrowser/.test(ua) ? 'Samsung Internet'
    : /FxiOS|Firefox/.test(ua) ? 'Firefox' : /CriOS|Chrome/.test(ua) ? 'Chrome' : /Safari/.test(ua) ? 'Safari' : 'Browser';
  return Array.from(os + ' ' + br).slice(0, 64).join('');
}

// ---- messages ----
const login = (password, device) => ({ t: 'login', v: V, password, device });
const hello = token => ({ t: 'hello', v: V, token });
const intents = {
  submit: text => ({ kind: 'submit', text }),
  queue: text => ({ kind: 'queue', text }),
  cancel: () => ({ kind: 'cancel' }),
  answer: (interaction_id, choice, message) => Object.assign({ kind: 'answer', interaction_id, choice }, message && { message }),
  compact: () => ({ kind: 'compact' }),
  unqueue: (index, text, edit) => ({ kind: 'unqueue', index, text, edit }),
  loadOlder: () => ({ kind: 'load_older' }),
  openSession: session_id => ({ kind: 'open_session', session_id }),
  newSession: (workspace, profile) => Object.assign({ kind: 'new_session' }, workspace && { workspace }, profile && { profile }),
  setModel: model => ({ kind: 'set_model', model }),
  setReasoning: level => ({ kind: 'set_reasoning', level }),
  renameSession: (session_id, title) => ({ kind: 'rename_session', session_id, title }),
  deleteSession: session_id => ({ kind: 'delete_session', session_id }),
};
const queries = {
  context: () => ({ kind: 'context' }),
  changes: () => ({ kind: 'changes' }),
  diff: path => ({ kind: 'diff', path }),
  toolOutput: (call_id, offset) => ({ kind: 'tool_output', call_id, offset }),
  files: query => ({ kind: 'files', query }),
};
// The install help to offer: none once installed, the browser's prompt, or on iOS the Share-sheet hint until dismissed.
const installOffer = (standalone, prompt, device, dismissed) =>
  standalone ? null : prompt ? 'prompt' : /^(iPhone|iPad) /.test(device) && !dismissed ? 'ios' : null;
const fs = {
  roots: () => ({ t: 'fs_roots' }),
  list: (path, hidden) => ({ t: 'fs_list', path, hidden: !!hidden }),
  mkdir: (parent, name) => ({ t: 'fs_mkdir', parent, name }),
};

// ---- store and reducer ----
const createStore = () => ({ state: null, blocks: [], intents: new Map(), fs: new Set(), q: new Set(), sid: undefined, nid: 0, skew: 0, device: null, version: '' });
const nextId = s => 'c' + ++s.nid;
const SENDS = { submit: 1, queue: 1 };
// Records the intent as pending and returns the message to send; it is never sent again by itself.
function intent(s, spec, own) {
  const id = nextId(s);
  s.intents.set(id, { id, spec, own, kind: spec.kind, text: SENDS[spec.kind] ? spec.text : undefined, st: 'pending' });
  return { t: 'intent', id, ...spec };
}
function request(s, body) { const id = nextId(s); s.fs.add(id); return { ...body, id }; }
// At most MAX_QUERIES are in flight per connection; the caller waits for room.
const queryRoom = s => s.q.size < MAX_QUERIES;
function query(s, spec) { const id = nextId(s); s.q.add(id); return { t: 'query', id, ...spec }; }
const pending = (s, kind) => { for (const it of s.intents.values()) if (it.st === 'pending' && (!kind || it.kind === kind)) return it; return null; };
// A dropped socket leaves pending sends unconfirmed; other intents show in the next snapshot.
function disconnect(s) {
  s.fs.clear(); s.q.clear();
  for (const it of [...s.intents.values()]) {
    if (it.st !== 'pending') continue;
    if (it.text != null) it.st = 'unconfirmed'; else s.intents.delete(it.id);
  }
}
// An explicit user action: a new intent with a new id.
function sendAgain(s, id) {
  const it = s.intents.get(id);
  s.intents.delete(id);
  return intent(s, it.spec);
}
const unconfirmed = s => [...s.intents.values()].filter(it => it.st === 'unconfirmed');

function diffBlocks(old, neu) {
  const out = [];
  for (let i = 0; i < neu.length; i++) if (JSON.stringify(old[i]) !== JSON.stringify(neu[i])) out.push(i);
  return out;
}
// An upsert is {i, block} or {i, append}, adding text/reasoning to an assistant or output to a tool (the first output string continues the open line).
function applyBlocks(blocks, len, upserts) {
  blocks.length = len;
  const out = [];
  for (const u of upserts) {
    const b = blocks[u.i], a = u.append;
    if (!a) blocks[u.i] = u.block;
    else if (b.type === 'tool' && a.output) {
      const o = b.output;
      if (o.length) o[o.length - 1] += a.output[0]; else o.push(a.output[0]);
      o.push(...a.output.slice(1));
    }
    else if (b.type === 'assistant') {
      if (a.text) b.text += a.text;
      if (a.reasoning) b.reasoning += a.reasoning;
    } else continue;
    out.push(u.i);
  }
  return out;
}

function reduce(s, msg, nowMs) {
  switch (msg.t) {
    case 'authorized': return { token: msg.token };
    case 'welcome':
      s.skew = msg.now_ms == null ? 0 : msg.now_ms - nowMs;
      s.device = msg.device; s.version = msg.version || '';
      return { welcome: true };
    case 'snapshot': {
      const events = s.state ? sessionTransitions(s.state.sessions, msg.state.sessions) : [];
      const dirty = diffBlocks(s.blocks, msg.blocks), sid = msg.state.session ? msg.state.session.id : null;
      const newSession = sid !== s.sid;
      s.blocks = msg.blocks; s.state = msg.state; s.sid = sid;
      return { dirty, state: true, newSession, snapshot: true, events };
    }
    case 'blocks': return { dirty: applyBlocks(s.blocks, msg.len, msg.upserts) };
    case 'state': {
      const events = s.state ? sessionTransitions(s.state.sessions, msg.state.sessions) : [];
      s.state = msg.state;
      return { state: true, events };
    }
    case 'ack': {
      const it = s.intents.get(msg.id);
      if (!it) return {};
      s.intents.delete(msg.id);
      return { ack: { ...it, ok: msg.ok, error: msg.error } };
    }
    case 'returned': return { returned: msg.text };
    case 'result': return s.q.delete(msg.id) ? { result: msg } : {};
    case 'notice': return { notice: { level: msg.level === 'error' ? 'error' : 'info', text: msg.text } };
    case 'fs': return s.fs.delete(msg.id) ? { fs: msg } : {};
    case 'error': case 'bye': return { gate: explain(msg) };
    default: return {};
  }
}

// ---- handshake outcomes ----
const fmtClock = sec => Math.floor(sec / 60) + ':' + String(sec % 60).padStart(2, '0');
const lockText = sec => 'Too many attempts · try again in ' + fmtClock(Math.max(0, Math.ceil(sec)));
const unreachable = host => "Can't reach MiniCore at " + host;
function explain(msg) {
  const key = msg.t === 'bye' ? msg.reason : msg.code;
  switch (key) {
    case 'wrong_password': return { view: 'signin', error: 'Wrong password. Check the password on your computer and try again.', shake: true };
    case 'locked': return { view: 'signin', locked: msg.retry_after || 30 };
    case 'unauthorized': return { view: 'signin', forget: true, notice: 'Please sign in to continue.' };
    case 'signed_out': return { view: 'signin', forget: true, notice: 'The password was changed on the computer. Sign in with the new password.' };
    case 'version': return { view: 'status', icon: 'retry', title: 'Update needed', text: "This page and MiniCore on your computer don't speak the same protocol version. Reload to get the current page.", action: 'reload', label: 'Reload' };
    case 'remote_off': return { view: 'status', icon: 'power', title: 'Remote control is off', text: 'Remote control was turned off on the computer. Turn it back on with /remote on, then retry.', action: 'retry', label: 'Retry' };
    case 'tui_exit': return { view: 'status', icon: 'term', title: 'MiniCore quit', text: 'MiniCore quit on the computer. Start it again with remote control on, then retry.', action: 'retry', label: 'Retry' };
    case 'replaced': return { view: 'status', icon: 'tabs', title: 'Opened in another tab', text: 'MiniCore is open in another tab on this device, and one tab per device stays connected.', action: 'retry', label: 'Use here' };
    case 'bad_request': return { view: 'status', icon: 'warn', title: "Couldn't connect", text: msg.message || 'MiniCore did not accept this request.', action: 'retry', label: 'Retry' };
    default: return { view: 'status', icon: 'warn', title: 'Disconnected', text: msg.message || 'The connection to MiniCore was closed.', action: 'retry', label: 'Retry' };
  }
}
const FS_ERR = {
  outside_roots: 'This folder is outside the folders MiniCore allows.', not_found: 'That folder no longer exists.',
  not_a_directory: 'That is not a folder.', invalid_name: "That name can't be used for a folder.",
  exists: 'A folder with that name already exists.', bad_request: 'MiniCore did not accept this request.',
};
const fsError = r => r.error === 'io' ? r.message || 'The folder could not be read.' : FS_ERR[r.error] || r.message || r.error || 'Something went wrong.';
const ERR = {
  busy: 'MiniCore is busy · try again in a moment', stale: 'Already answered', unknown_command: 'Unknown command',
  bad_request: 'MiniCore did not accept this request',
};
const errorText = e => ERR[e] || e || 'Request failed';

// ---- time ----
const MON = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const pad = n => String(n).padStart(2, '0');
// tz is Date#getTimezoneOffset() in minutes.
const localDay = (ms, tz) => Math.floor((ms - tz * 60000) / 86400000);
const clock = (ms, tz) => { const d = new Date(ms - tz * 60000); return pad(d.getUTCHours()) + ':' + pad(d.getUTCMinutes()); };
function relTime(ms, now, tz) {
  const m = Math.floor((now - ms) / 60000);
  if (m < 1) return 'now';
  if (m < 60) return m + 'm';
  if (m < 1440) return Math.floor(m / 60) + 'h';
  const days = localDay(now, tz) - localDay(ms, tz);
  if (days < 7) return days + 'd';
  const d = new Date(ms - tz * 60000), same = new Date(now - tz * 60000).getUTCFullYear() === d.getUTCFullYear();
  return MON[d.getUTCMonth()] + ' ' + d.getUTCDate() + (same ? '' : ', ' + d.getUTCFullYear());
}
// "14:02" today, "Yesterday 14:02", else "Oct 3, 14:02".
function stamp(ms, now, tz) {
  const days = localDay(now, tz) - localDay(ms, tz);
  if (days <= 0) return clock(ms, tz);
  if (days === 1) return 'Yesterday ' + clock(ms, tz);
  const d = new Date(ms - tz * 60000);
  return MON[d.getUTCMonth()] + ' ' + d.getUTCDate() + ', ' + clock(ms, tz);
}
const busyStatus = st => st === 'running' || st === 'approval' || st === 'compacting';
// The drawer: Needs you (longest waiting first), Running (oldest first), then one group per workspace, the one with the newest
// session first and each group's sessions newest first. `frozen` is the order captured on open: rows keep their place (and
// group), and new sessions go to the top of their group. Runs on every state while the list shows, so each date parses once.
function sessionLayout(sessions, query, frozen) {
  const rows = sessions.map((x, index) => ({ x, index, pin: x.live && x.status === 'approval' ? 0 : x.live && busyStatus(x.status) ? 1 : 2, at: Date.parse(x.updated_at) }));
  const wsRows = new Map();
  for (const r of rows.filter(r => r.pin === 2).sort((a, b) => b.at - a.at || a.index - b.index)) {
    if (!wsRows.has(r.x.workspace)) wsRows.set(r.x.workspace, []);
    wsRows.get(r.x.workspace).push(r);
  }
  let rank = 0;
  for (const list of wsRows.values()) for (const r of list) r.rank = rank++;
  for (const r of rows) {
    const f = frozen && frozen.get(r.x.id);
    if (f) { r.pin = f[0]; r.order = f[1]; } else r.order = frozen ? r.index - 1e9 : r.pin < 2 ? (r.x.started_ms == null ? Infinity : r.x.started_ms) : r.rank;
  }
  rows.sort((a, b) => a.pin - b.pin || a.order - b.order || a.index - b.index);
  const all = [], byWs = new Map(), pins = new Map();
  for (const r of rows) {
    pins.set(r.x.id, r.pin);
    let g = r.pin < 2 ? all.find(y => y.pinned === r.pin) : byWs.get(r.x.workspace);
    if (!g) {
      g = r.pin < 2 ? { label: PINNED[r.pin], pinned: r.pin, items: [] } : { label: basename(r.x.workspace), ws: r.x.workspace, items: [] };
      all.push(g);
      if (r.pin === 2) byWs.set(r.x.workspace, g);
    }
    g.items.push(r.x);
  }
  const order = new Map(all.flatMap(g => g.items).map((x, k) => [x.id, [pins.get(x.id), k]]));
  const q = (query || '').trim().toLowerCase();
  const groups = !q ? all : all.map(g => ({ ...g, items: g.items.filter(x => ((x.title || '') + '\n' + x.workspace).toLowerCase().includes(q)) })).filter(g => g.items.length);
  return { groups, order: frozen || order };
}
const PINNED = ['Needs you', 'Running'], GROUP_ROWS = 5;
// A workspace group shows its newest rows until opened with "Show N more"; a search shows every match.
const groupRows = (items, open) => open ? { shown: items, hidden: 0 } : { shown: items.slice(0, GROUP_ROWS), hidden: Math.max(0, items.length - GROUP_ROWS) };
function fmtElapsed(ms) {
  const s = Math.max(0, Math.floor(ms / 1000)), m = Math.floor(s / 60);
  if (m < 1) return s + 's';
  return m < 60 ? m + 'm ' + pad(s % 60) + 's' : Math.floor(m / 60) + 'h ' + pad(m % 60) + 'm';
}
const fmtDur = ms => ms < 1000 ? ms + ' ms' : ms < 60000 ? (ms / 1000).toFixed(1) + ' s' : fmtElapsed(ms);
const fmtTok = n => n == null ? '?' : n >= 1e6 ? (n / 1e6).toFixed(1) + 'M' : n >= 1000 ? (n / 1000).toFixed(1) + 'k' : String(n);
const fmtNum = n => String(n).replace(/\B(?=(\d{3})+(?!\d))/g, ',');

// ---- transcript text ----
function toolKind(name) {
  if (/^(bash|shell|sh|exec|run|terminal|command)$/.test(name)) return 'cmd';
  if (/^(write|create)/.test(name)) return 'write';
  if (/^(edit|multi_edit|apply_patch|patch|str_replace)/.test(name)) return 'edit';
  if (/^(read|view|cat|open)/.test(name)) return 'read';
  if (/^(grep|glob|find|search|ls|list)/.test(name)) return 'search';
  if (/^(web|fetch|http|browse)/.test(name)) return 'web';
  if (/^ask/.test(name)) return 'ask';
  return 'other';
}
const VERB = { cmd: ['Ran', 'Running'], edit: ['Edited', 'Editing'], write: ['Wrote', 'Writing'], read: ['Read', 'Reading'], search: ['Searched', 'Searching'], web: ['Fetched', 'Fetching'], ask: ['Asked', 'Asking'] };
function toolInfo(b) {
  const k = toolKind(b.name), run = b.state === 'running' || b.state === 'pending', v = VERB[k];
  const d = k === 'edit' && b.input ? diffStats(b.input) : k === 'write' && b.input_lines ? { added: b.input_lines, removed: 0 } : null;
  return {
    kind: k, run, verb: v ? v[run ? 1 : 0] : b.name, target: (v ? b.detail || b.name : b.detail) || '',
    dur: b.duration_ms >= 100 ? fmtDur(b.duration_ms) : '', exit: b.state === 'failed' && b.exit_code != null ? 'exit ' + b.exit_code : '',
    added: d ? d.added : null, removed: d ? d.removed : null,
    note: b.state === 'denied' ? 'denied' : b.state === 'cancelled' ? 'cancelled' : b.state === 'failed' && b.exit_code == null ? 'failed' : '',
  };
}
// Unified diffs and apply_patch envelopes as files of hunks of lines {t: add|del|ctx, s, a: old line, b: new line}; null numbers when absent.
function parseDiff(text) {
  const files = [];
  let f = null, h = null, a = 0, b = 0, left = null, m;
  const file = path => { f = { path, op: '', hunks: [], added: 0, removed: 0 }; files.push(f); h = null; };
  const hunk = head => { if (!f) file(''); h = { head, lines: [] }; f.hunks.push(h); };
  for (const l of text.split('\n')) {
    if ((m = /^\*\*\* (Update|Add|Delete) File: (.+)/.exec(l))) { file(m[2].trim()); f.op = m[1].toLowerCase(); left = null; continue; }
    if ((m = /^\*\*\* Move to: (.+)/.exec(l)) && f) { f.path += ' → ' + m[1].trim(); continue; }
    if (/^\*\*\* (Begin Patch|End Patch|End of File)/.test(l)) continue;
    if ((m = /^diff --git a\/.+ b\/(.+)$/.exec(l))) { file(m[1]); continue; }
    if ((m = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(l))) {
      hunk(l); a = +m[1]; b = +m[3]; left = [m[2] == null ? 1 : +m[2], m[4] == null ? 1 : +m[4]];
      continue;
    }
    if (l.startsWith('@@')) { if (!f) file(''); hunk(l.slice(2).trim()); left = null; continue; }
    const counted = left && h;
    if (!h || (counted && left[0] <= 0 && left[1] <= 0)) {
      if ((m = /^\+\+\+ (?:b\/)?(.+)$/.exec(l))) { if (!f || f.hunks.length) file(m[1]); else if (m[1] !== '/dev/null') f.path = m[1]; h = null; continue; }
      if ((m = /^--- (?:a\/)?(.+)$/.exec(l))) { if (!f || f.hunks.length) file(m[1]); h = null; continue; }
      if (f && f.op === 'add' && l[0] === '+') hunk(''); else continue;
    }
    const c = l[0], s = l.slice(1);
    if (c === '+') { h.lines.push({ t: 'add', s, a: null, b: counted ? b++ : null }); f.added++; if (counted) left[1]--; }
    else if (c === '-') { h.lines.push({ t: 'del', s, a: counted ? a++ : null, b: null }); f.removed++; if (counted) left[0]--; }
    else if (c === ' ' || l === '') { h.lines.push({ t: 'ctx', s, a: counted ? a++ : null, b: counted ? b++ : null }); if (counted) { left[0]--; left[1]--; } }
  }
  return files;
}
function diffStats(text) {
  let added = 0, removed = 0;
  for (const f of parseDiff(text)) { added += f.added; removed += f.removed; }
  return { added, removed };
}
const quiet = b => b.type === 'assistant' && !b.text && !b.reasoning;
// Consecutive reads and searches (empty assistant blocks between them do not count) group into one row.
function groupTools(blocks, from, to) {
  const out = [];
  let run = [];
  const end = () => {
    if (run.length > 1) {
      let reads = 0, live = false;
      for (const i of run) { if (toolKind(blocks[i].name) === 'read') reads++; if (blocks[i].state === 'running' || blocks[i].state === 'pending') live = true; }
      const searches = run.length - reads;
      out.push({
        key: 'g' + blocks[run[0]].call_id, head: run[0], members: run, run: live,
        label: !searches ? (live ? 'Reading ' : 'Read ') + plural(reads, 'file') : !reads ? (live ? 'Searching · ' : 'Searched ') + plural(searches, 'time')
          : (live ? 'Exploring ' : 'Explored ') + plural(reads, 'file') + ' · ' + plural(searches, 'search', 'searches'),
      });
    }
    run = [];
  };
  for (let i = from; i < to; i++) {
    const b = blocks[i];
    if (b.type === 'tool' && /^(read|search)$/.test(toolKind(b.name))) run.push(i);
    else if (!quiet(b)) end();
  }
  end();
  return out;
}
// Steering messages split finished work so folding never moves steps across a user's instruction.
function foldTurns(blocks, from, to) {
  const out = [];
  let start = from, promptAt = null;
  for (let i = from; i < to; i++) {
    const b = blocks[i];
    if ((b.type === 'user' && b.kind !== 'steering') || b.type === 'earlier') { start = i + 1; promptAt = b.type === 'user' ? b.at : null; continue; }
    if (b.type !== 'turn_end') continue;
    let fin = i;
    for (let j = i - 1; j >= start && blocks[j].type !== 'tool'; j--) if (blocks[j].type === 'assistant' && blocks[j].text) { fin = j; break; }
    let hidden = [], tools = 0, failed = 0, at = promptAt, segment = start, steered = false;
    const finish = until => {
      if (hidden.length) {
        const c = !steered && b.changes;
        out.push({ key: 'f' + b.loop_id + ':' + segment, at: hidden[0], last: hidden[hidden.length - 1], hidden, tools, failed,
          ms: at != null && until != null ? until - at : null,
          added: c && c.files && segment === start ? c.added : null, removed: c && c.files && segment === start ? c.removed : null });
      }
      hidden = []; tools = failed = 0;
    };
    for (let j = start; j < i; j++) {
      const x = blocks[j];
      if (x.type === 'user' && x.kind === 'steering') {
        steered = true; finish(x.at); at = x.at; segment = j + 1;
      } else if (j < fin) {
        if (x.type === 'tool') { tools++; if (x.state === 'failed') failed++; }
        if ((x.type === 'tool' || x.type === 'assistant') && !quiet(x)) hidden.push(j);
      }
    }
    finish(promptAt != null && b.duration_ms != null ? promptAt + b.duration_ms : null);
    start = i + 1; promptAt = null;
  }
  return out;
}
function findBlocks(blocks, q, from, to) {
  const n = q.toLowerCase(), out = [];
  for (let i = from; i < to && n; i++) {
    const b = blocks[i];
    const t = b.type === 'tool' ? b.detail + '\n' + b.output.join('\n') : b.type === 'user' ? b.text : b.type === 'assistant' ? b.text + '\n' + b.reasoning : '';
    if (t.toLowerCase().includes(n)) out.push(i);
  }
  return out;
}
const foldText = f => 'Worked' + (f.ms != null ? ' ' + fmtDur(f.ms) : '');
const OUTCOME = { completed: 'Completed', cancelled: 'Stopped', interrupted: 'Stopped', failed: 'Failed' };
// A failed turn as the host explains it: the title and hint are the TUI's words; `ids` is the provider's status and code
// (the type only when it adds to the code).
function failureView(f) {
  const name = f.code && f.type && f.code !== f.type ? f.code + '/' + f.type : f.code || f.type;
  return {
    title: cap(f.title), ids: [f.status, name].filter(x => x != null && x !== '').join(' · '), hint: f.hint, message: f.message || '',
    multiline: !!f.message && f.message.split('\n').length > 3, action: f.action || null,
  };
}
function turnEnd(b, at, tz) {
  const u = b.usage, tok = u && u.input + u.output, failure = b.failure && failureView(b.failure);
  return {
    ok: b.outcome === 'completed',
    parts: [failure ? failure.title : OUTCOME[b.outcome] || cap(String(b.outcome).replace(/_/g, ' ')), b.duration_ms != null && fmtDur(b.duration_ms),
      at != null && clock(at, tz), tok > 0 && fmtTok(tok) + ' tokens'].filter(Boolean),
    changes: b.changes && b.changes.files > 0 ? b.changes : null,
    failure,
  };
}
const changesText = c => plural(c.files, 'file') + (c.added != null ? ' +' + c.added : '') + (c.removed != null ? ' −' + c.removed : '');
const compactionText = b => 'Context compacted' + (b.before != null || b.after != null ? ' · ' + fmtTok(b.before) + ' → ' + fmtTok(b.after) + ' tokens' : '');
function statusLine(state, blocks) {
  const st = state.status, last = blocks[blocks.length - 1];
  const label = st.loading ? 'Opening session…' : st.interrupting ? 'Stopping…' : st.compacting ? 'Compacting context' : st.label || 'Working';
  const tool = !st.interrupting && last && last.type === 'tool' && last.state === 'running' ? last : null;
  return {
    label, detail: !tool ? '' : label.includes(tool.name) ? tool.detail || '' : [tool.name, tool.detail].filter(Boolean).join(' · '),
  };
}
// What the composer says before anything is typed; `keys` names Enter and Alt on a device with a keyboard.
const placeholder = (busy, session, keys) => !busy ? (session ? 'Message MiniCore · / for commands' : 'Describe a task to start')
  : keys ? keys.send + ' steers · ' + keys.alt + 'Enter queues a follow-up' : 'Type to steer or queue a follow-up';
const allowedText = allowed => allowed && allowed.length ? 'Allowed for this session: ' + allowed.join(', ') : 'Every tool asks first';
// Stop shows while a turn runs or a compaction is under way, and stays until the host confirms it stopped.
const canStop = st => !!(st.busy || st.compacting);
// The composer's primary button changes face, never place: send when idle, stop while a turn or compaction runs with
// nothing typed, steer with text typed during a turn. c = { busy, compacting, text, stopping }.
const primaryKind = c => c.stopping ? 'stopping' : c.text.trim() ? (c.busy ? 'steer' : 'send') : c.busy || c.compacting ? 'stop' : 'send';
// What a tap on it does. The kind comes from the live text, so typing then tapping never stops. A tap within STOP_GUARD of
// the button becoming Stop (a double tap on Send as the composer empties) or from a pointer that travelled over TAP_SLOP px
// (a scroll) does nothing. c adds stopAt: when the button became Stop. Returns 'send' | 'steer' | 'stop' | null.
const STOP_GUARD = 600, TAP_SLOP = 10;
function primaryTap(c, now, moved) {
  const kind = primaryKind(c);
  if (moved > TAP_SLOP || kind === 'stopping' || (kind === 'send' && !c.text.trim())) return null;
  return kind === 'stop' && now - c.stopAt < STOP_GUARD ? null : kind;
}
// The block being written: the last block when it is a streaming assistant in a busy session.
const liveBlock = (blocks, busy) => {
  const i = blocks.length - 1, b = blocks[i];
  return busy && b && b.type === 'assistant' && b.streaming ? i : -1;
};

// ---- approvals ----
function approvalTitle(a) {
  const k = toolKind(a.tool), files = k === 'edit' && a.input ? parseDiff(a.input) : [];
  if (k === 'cmd') return { text: 'Run command' };
  if (files.length > 1) return { text: 'Edit ' + files.length + ' files' };
  if (k === 'edit' || k === 'write') return { text: k === 'edit' ? 'Edit' : 'Write', code: files.length ? files[0].path : a.detail };
  if (k === 'read') return { text: 'Read', code: a.detail };
  if (k === 'web') return { text: 'Fetch', code: a.detail };
  return { text: 'Use', code: a.tool };
}
// The approval as one line: "Run command", "Edit `src/x.rs`".
function approvalLine(a) {
  const t = approvalTitle(a);
  return t.text + (t.code ? ' ' + t.code : '');
}
// The host's labels come in the order [allow, deny, allow for session]; allow shows first and deny last.
const APPROVAL_CHOICES = ['allow', 'deny', 'allow_session'];
function approvalOptions(a) {
  if (a.kind !== 'approval') return a.options.map((label, choice) => ({ choice, label, n: choice + 1 }));
  const o = a.options.map((label, k) => ({ choice: APPROVAL_CHOICES[k], label }));
  return [o[0], o[2], o[1]].filter(Boolean);
}
// What the sheet shows first: the command, a diff, the content to write, or the raw input.
function approvalBody(a) {
  const k = toolKind(a.tool), input = a.input || '';
  if (k === 'cmd') return { cmd: input || a.detail };
  const diff = k === 'edit' ? parseDiff(input) : [];
  if (diff.length) return { diff };
  if (k === 'write' && input) {
    const lines = input.split('\n');
    return { diff: [{ path: a.detail, op: 'add', added: lines.length, removed: 0, hunks: [{ head: '', lines: lines.map((s, i) => ({ t: 'add', s, a: null, b: i + 1 })) }] }] };
  }
  return { text: input && input !== a.detail ? input : '' };
}
// Keys answer only after 600 ms without typing since the sheet appeared, never from a text field or with a modifier.
const ARM_MS = 600, APPROVAL_KEYS = { y: 'allow', a: 'allow_session', n: 'deny' };
function approvalKey(interaction, key, mods, armedFrom, now, inField) {
  if (inField || mods || now - armedFrom < ARM_MS) return null;
  if (interaction.kind === 'approval') return APPROVAL_KEYS[key.toLowerCase()] || null;
  const k = /^[1-9]$/.test(key) ? +key - 1 : -1;
  return k >= 0 && k < interaction.options.length ? k : null;
}

// ---- paths ----
// Windows paths (drive letters, UNC) take `\` or `/` and compare without case.
const WIN = /^([A-Za-z]:([\\/]|$)|\\\\)/;
const canon = p => WIN.test(p) ? p.replace(/\//g, '\\') : p;
const pkey = p => WIN.test(p) ? canon(p).toLowerCase().replace(/\\+$/, '') : p;
const isDrive = p => /^[A-Za-z]:\\?$/.test(canon(p));
function splitPath(path) {
  const p = canon(path), win = WIN.test(p), sep = win ? '\\' : '/', parts = p.split(sep).filter(Boolean);
  const out = parts.map((name, k) => {
    let q = (p.startsWith('\\\\') ? '\\\\' : p.startsWith(sep) ? sep : '') + parts.slice(0, k + 1).join(sep);
    if (/^[A-Za-z]:$/.test(q)) q += sep;
    return { name, path: q };
  });
  if (p.startsWith('/')) out.unshift({ name: '/', path: '/' });
  return out;
}
function within(p, root) {
  if (!WIN.test(root)) return p === root || p.startsWith(root.endsWith('/') ? root : root + '/');
  const a = pkey(p), b = pkey(root);
  return a === b || a.startsWith(b + '\\');
}
function crumbs(path, roots) {
  const all = splitPath(path), root = roots.filter(r => within(path, r.path)).sort((a, b) => b.path.length - a.path.length)[0];
  if (!root) return all;
  const k = all.findIndex(c => pkey(c.path) === pkey(root.path));
  return k < 0 ? all : [{ name: root.name, path: root.path }, ...all.slice(k + 1)];
}
// "~/Develops/app" (Windows: "~\work") for a home directory path.
const tilde = p => (p || '').replace(/^(?:\/(?:Users|home)\/[^/]+|[A-Za-z]:[\\/]Users[\\/][^\\/]+)(?=[\\/]|$)/i, '~');

// ---- attention ----
function sessionTransitions(prev, next) {
  const out = [], before = new Map(prev.map(x => [x.id, x]));
  for (const x of next) {
    const p = before.get(x.id);
    if (!x.live || x.current || !p || !p.live) continue;
    if (x.status === 'approval' && p.status !== 'approval') out.push({ kind: 'approval', id: x.id, title: x.title });
    else if (x.status === 'idle' && x.unseen && !(p.status === 'idle' && p.unseen)) out.push({ kind: 'finished', id: x.id, title: x.title });
  }
  return out;
}
function noticeEvent(text) {
  const m = /^"(.+)" (needs approval|finished|failed|interrupted|cancelled|stopped)\b/.exec(text);
  return m ? { label: m[1], kind: m[2] === 'needs approval' ? 'approval' : m[2] === 'failed' ? 'failed' : 'finished' } : null;
}
const attentionCount = state => state.background.approval + (state.interaction ? 1 : 0);

// ---- composer ----
const BUILTINS = [
  ['new', 'Start a new session'], ['model', 'Choose the model and reasoning'], ['rename', 'Rename this session', '[title]'],
  ['compact', 'Compact the context now'], ['context', 'Show what fills the context'], ['changes', 'Show the changed files'],
  ['export', 'Share the transcript as Markdown'], ['find', 'Find in this session'], ['session', 'Show session details'],
  ['sessions', 'Open the session list'], ['copy', 'Copy the last answer'], ['settings', 'Open settings'], ['help', 'Keyboard shortcuts'],
].map(([name, description, hint]) => ({ name, description, hint: hint || '', builtin: true }));
const ALIAS = { diff: 'changes', resume: 'sessions', reasoning: 'model' };
// Every command the composer offers: the host's templates, then the built-ins the client runs itself.
const commandList = commands => commands.map(c => ({ ...c, builtin: false })).concat(BUILTINS);
// The command list while the composer holds only "/word": prefix matches first, templates before built-ins.
function slashMenu(text, commands) {
  const m = /^\/(\S*)$/.exec(text);
  if (!m) return null;
  const q = m[1].toLowerCase(), all = commandList(commands);
  const hit = all.filter(c => c.name.toLowerCase().includes(q));
  return hit.filter(c => c.name.toLowerCase().startsWith(q)).concat(hit.filter(c => !c.name.toLowerCase().startsWith(q)));
}
// What a "/name args" line is: a built-in (run here), a template (sent), a bare unknown "/word" (refused), or null for plain text.
function parseSlash(text, commands) {
  const m = /^\/(\S+)(?:\s+([\s\S]*))?$/.exec(text.trim());
  if (!m) return null;
  const name = ALIAS[m[1]] || m[1], arg = (m[2] || '').trim();
  if (BUILTINS.some(c => c.name === name)) return { builtin: name, arg };
  if (commands.some(c => c.name === m[1])) return { template: m[1], arg };
  return !arg && /^[\w-]+$/.test(m[1]) ? { unknown: m[1] } : null;
}
function atToken(text, caret) {
  const m = /(^|\s)@([^\s@]*)$/.exec(text.slice(0, caret));
  if (!m) return null;
  const tail = /^[^\s@]*/.exec(text.slice(caret))[0];
  return { start: caret - m[2].length - 1, end: caret + tail.length, query: m[2] };
}
function insertMention(text, tok, path) {
  const ins = '@' + path + ' ', rest = text.slice(tok.end).replace(/^ /, '');
  return { text: text.slice(0, tok.start) + ins + rest, caret: tok.start + ins.length };
}
const pushRecent = (list, text, max = 50) => [text, ...list.filter(x => x !== text)].slice(0, max);
// Commands and paths are not prose: no autocorrect while the text starts with one.
const literal = text => /^[/@]/.test(text);
// Enter (when "Enter sends" is on) or ⌘/Ctrl+Enter steers a running turn and sends when idle; Alt/Option+Enter queues a
// follow-up. An open list keeps Enter. null leaves the key to the browser.
function composerKey(k, c) {
  const typed = c.text.trim() !== '';
  if (c.popup && !k.mod) return null;
  if (k.key !== 'Enter') return null;
  if (k.alt && !k.mod) return c.busy && typed ? 'queue' : null;
  return k.mod || (c.enter && !k.shift && !k.alt) ? (c.busy ? 'steer' : 'send') : null;
}
function queueView(queue, all, max = 3) {
  const counts = { steer: 0, follow: 0 };
  const items = queue.map((q, index) => ({ ...q, index, n: ++counts[q.kind] }));
  return { items: all ? items : items.slice(0, max), hidden: all ? 0 : Math.max(0, items.length - max), long: items.length > max };
}

// The newest sentence of the reasoning text, Markdown stripped; only the tail is read.
function thinkingPreview(text) {
  const lines = norm(text.slice(-1200)).split('\n').map(l => l.replace(/^\s*(?:#{1,6}\s+|>\s*|[-*+]\s+|\d{1,9}[.)]\s+)+/, '').replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1').replace(/[`*~]+|__/g, '').trim()).filter(Boolean);
  const sentences = (lines.pop() || '').split(/(?<=[.!?。！？])\s+|(?<=[。！？])/).filter(Boolean);
  return sentences.pop() || '';
}

// ---- changes, context, export ----
const STATUS = { added: ['A', 'ok'], modified: ['M', ''], deleted: ['D', 'er'], renamed: ['R', ''], conflict: ['!', 'er'], unknown: ['?', ''] };
const changeStatus = s => STATUS[s] || STATUS.unknown;
// The context reply as a bar and rows; the reply's optional parts show only when present.
function contextView(d) {
  const pct = v => v == null || !d.window ? null : Math.min(100, v / d.window * 100);
  const c = d.coverage, l = d.last_compaction;
  return {
    percent: d.percent, used: fmtTok(d.tokens) + ' of ' + fmtTok(d.window) + ' tokens', trigger: pct(d.trigger_tokens), target: pct(d.target_tokens),
    rows: [
      d.history_tokens != null && ['History', fmtTok(d.history_tokens) + ' tokens'],
      d.trigger_tokens != null && ['Compacts at', fmtTok(d.trigger_tokens) + (d.target_tokens != null ? ' → ' + fmtTok(d.target_tokens) : '')],
      c && ['Covers', plural(c.turns, 'turn') + ' · ' + plural(c.items, 'item') + ' · ' + c.kept + ' kept'],
      l && ['Last compaction', cap(l.origin) + ' · ' + l.status + (l.before != null ? ' · ' + fmtTok(l.before) + ' → ' + fmtTok(l.after) : '')],
    ].filter(Boolean),
    failure: d.failure || '', compacting: !!d.compacting,
  };
}
function exportMd(state, blocks) {
  const s = state.session, out = ['# ' + titled(s.title), '', '`' + s.workspace + '`' + (s.model ? ' · ' + s.model : ''), ''];
  for (const b of blocks) {
    if (b.type === 'user') out.push(b.kind === 'steering' ? '**You (steer):** ' + b.text : '## You\n\n' + b.text, '');
    else if (b.type === 'assistant' && b.text) out.push(b.text, '');
    else if (b.type === 'tool') { const f = toolInfo(b); out.push('- ' + f.verb + ' `' + f.target + '`' + (f.exit ? ' (' + f.exit + ')' : f.note ? ' (' + f.note + ')' : ''), ''); }
    else if (b.type === 'turn_end') {
      const t = turnEnd(b, null, 0), f = t.failure, parts = f && f.ids ? [t.parts[0], f.ids, ...t.parts.slice(1)] : t.parts;
      out.push('_' + parts.join(' · ') + '_', '');
      if (f && f.message) out.push(f.message.split('\n').map(l => ('> ' + l).trimEnd()).join('\n'), '');
    }
    else if (b.type === 'compaction') out.push('_' + compactionText(b) + '_', '');
  }
  return out.join('\n').replace(/\n{3,}/g, '\n\n').trim() + '\n';
}

function recentWorkspaces(sessions, max = 6) {
  const out = [];
  for (const x of sessions) if (!out.includes(x.workspace) && out.push(x.workspace) >= max) break;
  return out;
}

export {
  V, WINDOW, inline, parseMd, mdInc, backoff, basename, plural, titled, quiet, midTrunc, retireText, mergeReturned, otherDevices, deviceName,
  installOffer, login, hello, intents, queries, fs, createStore, intent, request, query, queryRoom, pending, disconnect,
  sendAgain, unconfirmed, reduce, lockText, unreachable, fsError, errorText, stamp, relTime, sessionLayout, groupRows,
  fmtElapsed, fmtTok, fmtNum, groupTools, foldTurns, findBlocks, foldText, toolInfo, parseDiff, diffStats, liveBlock, turnEnd,
  changesText, compactionText, statusLine, placeholder, allowedText, canStop, primaryKind, primaryTap, TAP_SLOP, approvalKey, approvalTitle,
  approvalOptions, approvalBody, approvalLine, changeStatus, contextView, exportMd, slashMenu, parseSlash, atToken,
  insertMention, pushRecent, literal, composerKey, commandList, queueView, thinkingPreview, splitPath, within, crumbs, tilde, isDrive,
  recentWorkspaces, sessionTransitions, noticeEvent, attentionCount,
};
