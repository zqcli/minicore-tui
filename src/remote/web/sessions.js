import * as MC from './core.js';
import { HOST, S, ask, fsCall, hostNow, intent, isHalted, isMain, online, signOut, stampAt, turnStatus, tz, viewed } from './net.js';
import { $, actions, announce, apple, closeAll, copyButton, copyText, diffView, errBox, fill, findSheet, h, hover, icon, installApp, installable, layout, loading, menuRow, notify, part, popModal, prefs, pushModal, refreshSheets, rowText, set, setPref, share, sheet, spin, standalone, toast, wide } from './ui.js';

// ---- sheets ----
function confirmSheet(o) {
  const m = sheet({
    key: 'confirm', cls: 'dialog', title: o.title, deps: () => [o.busy, o.err, online()],
    body: () => [o.text && h('p', 'hint big', o.text), o.err && errBox(o.err, 'mt12')],
    foot: () => [h('button', { class: 'btn', 'data-k': 'no', onclick: () => m.close() }, 'Cancel'),
      h('button', { class: 'btn ' + (o.danger ? 'dangerfill' : 'primary'), 'data-k': 'ok', disabled: o.busy || (o.needsOnline && !online()), onclick: () => o.ok(m) }, o.busy && spin(), o.label)],
  });
  return m;
}

const SIZES = [['Small', .9], ['Default', 1], ['Large', 1.1], ['Larger', 1.2]];
function seg(label, items, value, set) {
  return h('div', { class: 'seg', role: 'radiogroup', 'aria-label': label }, items.map(([l, v]) => h('button', { role: 'radio', 'data-k': label + l, 'aria-checked': String(v === value), onclick: () => set(v) }, l)));
}
function switchRow(label, sub, on, set, disabled) {
  return h('label', 'sw', h('span', '', label, sub && h('small', '', sub)), h('input', { type: 'checkbox', role: 'switch', 'data-k': label, checked: on, disabled, onchange: e => set(e.target.checked) }));
}
export function openSettings() {
  sheet({
    key: 'settings', cls: 'full', title: 'Settings',
    deps: () => [prefs, S.state.devices, S.device, online(), installable(), typeof Notification === 'undefined' ? 'none' : Notification.permission],
    body: () => {
      const s = S.state, inst = MC.installOffer(standalone(), installable(), MC.deviceName(navigator.userAgent, navigator.maxTouchPoints), localStorage.getItem('minicore.installHint'));
      const where = d => d.session ? (s.sessions.find(x => x.id === d.session) || {}).title || 'a session' : 'a new session';
      const canNotify = typeof Notification !== 'undefined' && 'serviceWorker' in navigator && isSecureContext;
      return [
        h('h3', 'sec2', 'Appearance'), seg('Appearance', [['System', 'system'], ['Light', 'light'], ['Dark', 'dark']], prefs.theme, v => setPref('theme', v)),
        h('h3', 'sec2', 'Text size'), seg('Text size', SIZES, prefs.text, v => setPref('text', v)),
        h('h3', 'sec2', 'Reading'), h('div', 'list',
          switchRow('Wrap code', 'Long lines in code and output wrap instead of scrolling', prefs.wrap, v => setPref('wrap', v)),
          switchRow('Show thinking expanded', null, prefs.thinking, v => setPref('thinking', v))),
        h('h3', 'sec2', 'Composer'), h('div', 'list', switchRow('Enter sends', 'For hardware keyboards; Shift+Enter adds a line', prefs.enter, v => setPref('enter', v))),
        h('h3', 'sec2', 'While a turn runs'), h('div', 'list',
          switchRow('Keep screen awake', 'wakeLock' in navigator ? null : 'Not available in this browser', prefs.wake, v => setPref('wake', v), !('wakeLock' in navigator)),
          switchRow('Notifications', canNotify ? (Notification.permission === 'denied' ? 'Blocked in the browser settings' : 'Approvals and finished turns while this page is in the background') : 'Not available on this page',
            prefs.notify && canNotify && Notification.permission === 'granted', v => setNotify(v), !canNotify || Notification.permission === 'denied')),
        h('h3', 'sec2', 'Connected devices'), h('div', 'list', s.devices.map((d, k) => h('div', 'row flat', icon(/iPhone|Android/.test(d.name) ? 'tabs' : 'term', 'ri'),
          rowText(d.name + (d.name === S.device && k === s.devices.findIndex(x => x.name === S.device) ? ' (this device)' : ''), 'Viewing ' + where(d))))),
        h('h3', 'sec2', 'About'), h('div', 'list',
          h('div', 'row flat', rowText('Host'), h('span', 'val', HOST)),
          h('div', 'row flat', rowText('MiniCore'), h('span', 'val', (S.version || '?') + ' · protocol ' + MC.V)),
          inst === 'prompt' && h('button', { class: 'row flat', onclick: installApp }, icon('down', 'ri'), rowText('Install app', 'Open MiniCore from your home screen')),
          inst === 'ios' && h('div', 'row flat', icon('share', 'ri'), rowText('Install app', 'Share → Add to Home Screen'),
            h('button', { class: 'ib s', 'aria-label': 'Dismiss', onclick: () => { localStorage.setItem('minicore.installHint', '1'); refreshSheets(); } }, icon('x', 'sm')))),
        h('div', 'mt24', h('button', { class: 'btn block danger', onclick: () => confirmSheet({ title: 'Sign out of this device?', text: 'You will need the password shown on your computer to connect again.', label: 'Sign out', danger: true, ok: m => { m.close(true); signOut(); } }) }, icon('logout', 'sm'), 'Sign out')),
      ];
    },
  });
}
function setNotify(on) {
  if (!on) return setPref('notify', false);
  Notification.requestPermission().then(p => setPref('notify', p === 'granted'));
}
// ---- model sheet ----
// The model and reasoning level asked for and not yet confirmed by the host.
const modelPending = { model: null, level: null };
// A rejected or dropped change stops showing as pending.
export function modelSettled(kind) { if (kind !== 'set_reasoning') modelPending.model = null; if (kind !== 'set_model') modelPending.level = null; }
export function openModel() {
  if (!S.state.models.length) return toast('No models are configured');
  sheet({
    key: 'model', title: 'Model', deps: () => [viewed(), S.state.models, modelPending, online()],
    body: () => {
      const s = S.state, c = viewed(), model = modelPending.model || c.model, m = s.models.find(x => x.id === model), level = modelPending.level || c.reasoning;
      return [h('div', { class: 'list', role: 'radiogroup', 'aria-label': 'Model' }, s.models.map(x => h('button', {
        class: 'row flat', role: 'radio', 'data-k': 'm' + x.id, 'aria-checked': String(x.id === model), disabled: !online(),
        onclick: () => { if (x.id !== model && intent(MC.intents.setModel(x.id))) modelPending.model = x.id; refreshSheets(); },
      }, rowText(x.id, x.reasoning.length ? x.reasoning.join(' · ') : 'No reasoning levels'),
      x.id === model && (modelPending.model ? spin('sm') : icon('check', 'ck'))))),
      m && m.reasoning.length > 0 && [h('h3', 'sec2', 'Reasoning'), seg('Reasoning', m.reasoning.map(l => [l, l]), level, l => { if (l !== level && intent(MC.intents.setReasoning(l))) modelPending.level = l; refreshSheets(); })]];
    },
  });
}

export function openChanges() {
  const c = { files: null, branch: null, truncated: false, err: '', path: null, diff: null, wrap: prefs.wrap, busy: false };
  const load = () => { c.busy = true; c.err = ''; m.refresh(); ask(MC.queries.changes(), r => { c.busy = false; if (r.ok) Object.assign(c, r.data); else c.err = MC.errorText(r.error); m.refresh(); }); };
  const file = path => { c.path = path; c.diff = null; c.err = ''; m.refresh(); ask(MC.queries.diff(path), r => { if (c.path !== path) return; if (r.ok) c.diff = r.data; else c.err = MC.errorText(r.error); m.refresh(); }); };
  const back = h('button', { class: 'ib', 'aria-label': 'All changes', onclick: () => { c.path = null; m.refresh(); } }, icon('chev', 'flip'));
  const m = sheet({
    key: 'changes', cls: 'full', side: true, deps: () => [c, online()],
    head: hid => [c.path && back, h('h2', { id: hid }, c.path ? h('span', 'mono', MC.basename(c.path)) : 'Changes',
      h('span', 'sub2', c.path ? c.path : [c.branch, c.files && MC.plural(c.files.length, 'file')].filter(Boolean).join(' · '))),
      !c.path && h('button', { class: 'ib', 'aria-label': 'Refresh', disabled: c.busy, onclick: load }, c.busy ? spin() : icon('retry'))],
    body: () => {
      if (c.err) return errBox(c.err);
      if (c.path) return c.diff ? [h('div', 'tbar', h('button', { 'aria-pressed': String(c.wrap), onclick: () => { c.wrap = !c.wrap; m.refresh(); } }, icon('wrap', 'xs'), 'Wrap'),
        c.diff.truncated && h('span', 'hint', 'Diff truncated')), diffView(MC.parseDiff(c.diff.lines.join('\n')).map(f => ({ ...f, path: f.path || c.path })), c.wrap)] : loading('Loading diff…');
      if (!c.files) return loading('Loading changes…');
      if (!c.files.length) return h('div', 'muted', 'No changes in this workspace');
      return [h('div', 'list', c.files.map(f => { const [l, cls] = MC.changeStatus(f.status), dir = f.path.slice(0, -MC.basename(f.path).length);
        return h('button', { class: 'row flat', 'data-k': f.path, 'aria-label': f.status + ' ' + f.path, onclick: () => file(f.path) }, h('span', 'st ' + cls, l),
          rowText(MC.basename(f.path), dir, true), icon('chev', 'sm ri')); })),
        c.truncated && h('p', 'hint mt8', 'Only the first 200 files are listed.')];
    },
  });
  pull(m, () => { if (!c.path) load(); });
  load();
}
function pull(m, fn) {
  const b = m.el.querySelector('.sbody');
  let y0 = null;
  b.addEventListener('touchstart', e => { y0 = b.scrollTop <= 0 ? e.touches[0].clientY : null; }, { passive: true });
  b.addEventListener('touchend', e => { if (y0 != null && e.changedTouches[0].clientY - y0 > 80) fn(); y0 = null; });
}
export function openContext() {
  const c = { d: null, err: '' };
  const load = () => ask(MC.queries.context(), r => { if (r.ok) c.d = r.data; else c.err = MC.errorText(r.error); m.refresh(); });
  const m = sheet({
    key: 'context', side: true, title: 'Context', deps: () => [c, turnStatus().busy, turnStatus().compacting, online()],
    body: () => {
      if (c.err) return errBox(c.err);
      if (!c.d) return loading('Loading…');
      const x = MC.contextView(c.d);
      return [h('div', 'cbar', h('b', '', Math.round(x.percent) + '%'), h('span', '', x.used)),
        h('div', { class: 'meter', role: 'meter', 'aria-valuenow': String(Math.round(x.percent)), 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-label': 'Context used' },
          h('i', { style: 'width:' + x.percent + '%' + (x.percent >= 80 ? ';background:var(--wac)' : '') }), x.trigger != null && h('em', { style: 'left:' + x.trigger + '%', title: 'Compacts here' }), x.target != null && h('em', { class: 't', style: 'left:' + x.target + '%', title: 'Compacts down to here' })),
        h('dl', 'kv mt16', x.rows.map(([k, val]) => [h('dt', '', k), h('dd', '', val)])),
        x.failure && errBox(x.failure, 'mt12', false)];
    },
    foot: () => h('button', { class: 'btn block', disabled: !online() || turnStatus().busy || turnStatus().compacting, onclick: () => compactNow(() => { c.d = null; setTimeout(load, 2000); m.refresh(); }) },
      turnStatus().compacting ? [spin(), 'Compacting…'] : [icon('layers', 'sm'), 'Compact now']),
  });
  load();
}
export function openOutput(b) {
  const f = MC.toolInfo(b), out = b.output, total = b.output_lines != null ? b.output_lines : out.length + b.output_dropped;
  const o = { text: out.join('\n'), full: !(b.output_dropped || b.truncated), loading: false, err: '', wrap: prefs.wrap, capped: false, shown: -1 };
  const m = sheet({
    key: 'out', cls: 'full', side: true, deps: () => [o.full, o.loading, o.err, o.wrap, o.text.length],
    head: hid => h('h2', { id: hid }, h('span', 'mono', f.verb + ' ' + f.target), h('span', 'sub2', MC.fmtNum(total) + ' lines' + (o.full ? '' : ' · showing the last ' + MC.fmtNum(out.length)))),
    body: () => [o.err && errBox(o.err), o.capped && h('p', 'hint', 'Showing the first 4 MB.'),
      h('pre', { class: 'out view' + (o.wrap ? ' wrap' : ''), tabindex: '0' }, o.text)],
    foot: () => [h('button', { class: 'btn sm', 'aria-pressed': String(o.wrap), onclick: () => { o.wrap = !o.wrap; m.refresh(); } }, icon('wrap', 'xs'), 'Wrap'),
      copyButton(o.text, 'Copy', { class: 'btn sm' }),
      !o.full && h('button', { class: 'btn sm primary', 'data-k': 'full', disabled: o.loading, onclick: () => loadFull(b, o, m) }, o.loading && spin('sm'), o.loading ? 'Loading…' : 'Load full output')],
    after: body => { if (o.shown !== o.text.length) { o.shown = o.text.length; requestAnimationFrame(() => { body.scrollTop = body.scrollHeight; }); } },
  });
}
function loadFull(b, o, m) {
  const parts = [];
  let off = 0, pages = 0;
  o.loading = true; o.err = ''; m.refresh();
  const next = () => ask(MC.queries.toolOutput(b.call_id, off), r => {
    if (!r.ok) { o.loading = false; o.err = MC.errorText(r.error); return m.refresh(); }
    parts.push(r.data.text); off = r.data.next_offset; pages++;
    if (!r.data.eof && pages < 64) return next();
    Object.assign(o, { text: parts.join('').replace(/\n$/, ''), full: true, loading: false, capped: !r.data.eof });
    m.refresh();
  });
  next();
}
export function compactNow(done) {
  confirmSheet({ title: 'Compact the context now?', text: 'MiniCore summarises earlier turns to free context. The transcript stays as it is.', label: 'Compact', needsOnline: true,
    ok: m => { m.close(true); intent(MC.intents.compact(), a => { if (!a.ok) toast(MC.errorText(a.error), 'error'); else if (done) done(); }); } });
}
export function exportSession() { share(S.state.session.title || 'MiniCore session', MC.exportMd(S.state, S.blocks)); }
const KEYS = [['Esc', 'Close the top sheet or stop the turn'], ['Enter', 'Send, or steer a running turn (“Enter sends”)'], ['⌘/Ctrl Enter', 'Send, or steer a running turn'], ['⌥/Alt Enter', 'Queue a follow-up while a turn runs'], ['↑ ↓', 'Recent prompts in an empty composer'],
  ['⌘/Ctrl K', 'Commands and prompts'], ['⌘/Ctrl F', 'Find in session'], ['⌘/Ctrl ⇧ F', 'Search sessions'], ['Alt ↑ ↓', 'Previous or next session'], ['Y A N', 'Allow, allow for session, deny'], ['End', 'Jump to the latest message'], ['?', 'This help']];
export function openHelp() {
  sheet({ key: 'help', cls: 'dialog', title: 'Keyboard shortcuts', body: () => h('dl', 'kv keys', KEYS.map(([k, d]) => [h('dt', '', h('kbd', '', k)), h('dd', '', d)])) });
}

// What the approval prompts have allowed so far; the host has no other policy to show.
export function openApprovals() {
  sheet({ key: 'approvals', cls: 'dialog', title: 'Approvals', deps: () => [S.state.session && S.state.session.allowed],
    body: () => [h('p', 'hint big', MC.allowedText(S.state.session && S.state.session.allowed)),
      h('p', 'hint', 'Choose “Allow for this session” on an approval to add a tool here.')] });
}

// ---- sessions ----
// Sessions whose last turn failed, until the reader opens them.
const failed = new Set();
function markFailed(id) { failed.add(id); renderNav(); }
export function clearFailed() { for (const id of failed) if (S.state.sessions.some(x => x.id === id && x.current)) failed.delete(id); }

export const openLive = id => { if (intent(MC.intents.openSession(id))) pendingRow = id; };
// ---- state ----
let tick = 0;
// Only active work in this view needs elapsed-time updates.
export function syncTick() {
  const s = S.state, x = s && s.status;
  const want = !!s && !!(x.started_ms && (x.busy || x.compacting));
  if (want && !tick) tick = setInterval(onTick, 1000);
  if (!want && tick) { clearInterval(tick); tick = 0; }
}
function onTick() {
  const x = turnStatus();
  if (x.started_ms) for (const el of $('strip').querySelectorAll('.el')) el.textContent = MC.fmtElapsed(hostNow() - x.started_ms);
}
// ---- navigation: drawer on phones, sidebar on wide screens ----
const nav = $('nav'), navModal = { el: nav, close: () => closeNav() };
let pendingRow = null, query = '', frozen = null, navLook = false;
// Workspace groups the reader folded (kept across visits) and the ones opened past their first rows (this page only).
const CLOSED = 'minicore.nav.closed', closedWs = new Set(JSON.parse(localStorage.getItem(CLOSED) || '[]')), moreWs = new Set();
const navShown = () => wide.matches || nav.classList.contains('on');
// The device's own working set, newest pin first: no host or protocol change, so another device keeps its own.
const PINS = 'minicore.pins';
let pins = JSON.parse(localStorage.getItem(PINS) || '[]'), pinsSeen = null;
const savePins = () => localStorage.setItem(PINS, JSON.stringify(pins));
function togglePin(id) {
  pins = pins.includes(id) ? pins.filter(x => x !== id) : [id, ...pins];
  savePins();
  // A pin is an explicit action: the row moves at once, even while the open list holds its order.
  if (frozen) frozen.delete(id);
  renderNav();
}
const pinRow = (m, id) => menuRow(m, 'pin', pins.includes(id) ? 'Unpin' : 'Pin to top', () => togglePin(id));
export function buildNav() {
  const q = h('input', { type: 'search', placeholder: hover.matches ? 'Search' : 'Search sessions', 'aria-label': 'Search sessions', autocomplete: 'off', enterKeyHint: 'search', oninput: () => { query = q.value; renderNav(); } });
  fill(nav, h('div', 'nhead', h('img', { src: '/icon.svg', alt: '' }), h('div', '', h('b', '', 'MiniCore'), h('small', '', HOST)),
    h('button', { class: 'ib', 'aria-label': 'Settings', onclick: openSettings }, icon('gear')),
    h('button', { class: 'ib nclose', 'aria-label': 'Close', onclick: closeNav }, icon('x'))),
    h('button', { class: 'btn primary nnew', 'data-focus': true, onclick: () => openNew() }, icon('plus', 'sm'), 'New session'),
    h('label', 'search', icon('search', 'sm'), q, hover.matches && h('kbd', 'kh', apple ? '⌘⇧F' : 'Ctrl⇧F')), h('div', 'nlist'), h('div', 'nfoot'));
  const list = nav.querySelector('.nlist');
  listPart = part(list, renderList);
  // One listener for every row: data-k is "s<id>" on a row's open button, "m<id>" on its actions, "g<path>" on a workspace header
  // and "x<path>" on its "Show more" (paths are URI-encoded, since the key is also used in a selector).
  list.addEventListener('click', e => {
    const b = e.target.closest('[data-k]');
    if (!b) return;
    const kind = b.dataset.k[0], v = b.dataset.k.slice(1);
    if (kind === 'g') { const ws = decodeURIComponent(v); if (!closedWs.delete(ws)) closedWs.add(ws); localStorage.setItem(CLOSED, JSON.stringify([...closedWs])); renderNav(); }
    else if (kind === 'x') { moreWs.add(decodeURIComponent(v)); renderNav(); }
    else if (kind === 'm') sessionActions(v);
    else if (b.closest('.srow').classList.contains('cur')) closeNav();
    else if (!pendingRow && intent(MC.intents.openSession(v))) { pendingRow = v; renderNav(); }
  });
  footPart = part(nav.querySelector('.nfoot'), f => [h('span', 'cdot' + (f.on ? '' : ' off')), h('span', '', f.text)]);
}
let listPart, footPart;
export function openNav() {
  if (wide.matches) return nav.querySelector('input').focus();
  frozen = null;
  nav.classList.add('on');
  pushModal(navModal);
  renderNav(); syncTick();
}
export function closeNav() {
  if (!nav.classList.contains('on')) return;
  nav.classList.remove('on');
  popModal(navModal);
  frozen = null;
  syncTick();
}
// The list order stays put while it is in front of the user.
nav.addEventListener('pointerenter', () => { navLook = true; });
nav.addEventListener('pointerleave', () => { navLook = false; if (!nav.contains(document.activeElement)) { frozen = null; renderNav(); } });
nav.addEventListener('focusout', () => setTimeout(() => { if (!navLook && !nav.contains(document.activeElement) && wide.matches) { frozen = null; renderNav(); } }));
export function renderNav() {
  if (!navShown() || !S.state) return;
  const s = S.state;
  if (s.sessions !== pinsSeen) { pinsSeen = s.sessions; const kept = MC.prunePins(pins, s.sessions); if (kept !== pins) { pins = kept; savePins(); } }
  const lay = MC.sessionLayout(s.sessions, query, frozen, pins), searching = query.trim() !== '', now = hostNow();
  if (nav.classList.contains('on') || navLook || nav.contains(document.activeElement)) frozen = lay.order;
  // Only a workspace group marks recent sessions, and not the open one: the others already stand out and that one is in front.
  const row = recent => x => [x.id, x.title, x.workspace, x.current, x.live && x.status, x.unseen, MC.relTime(Date.parse(x.updated_at), now, tz()), failed.has(x.id), recent && !x.current && MC.recent(x, now)];
  listPart({
    groups: lay.groups.map(g => {
      const v = MC.groupRows(g.items, g.level < 3 || searching || moreWs.has(g.ws));
      return { label: g.label, ws: g.ws, level: g.level, closed: !searching && closedWs.has(g.ws), count: g.items.length, hidden: v.hidden, rows: v.shown.map(row(g.level === 3)) };
    }), pendingRow, query: !!query,
  });
  renderNavFoot();
}
function renderNavFoot() {
  if (!footPart || !S.state) return;
  const n = online() ? MC.otherDevices(S.state.devices, S.device).length : 0;
  footPart({ on: online(), text: online() ? (S.device || 'This device') + (n ? ' · ' + MC.plural(n, 'other device') : '') : 'Reconnecting…' });
}
function renderList(l) {
  if (!l.groups.length) return h('div', 'muted', l.query ? 'No matching sessions' : 'No sessions yet');
  return l.groups.map(g => g.level === 0 || g.level === 2 ? [h('div', 'sec' + (g.level === 0 ? ' warn' : ''), g.label), g.rows.map(r => sessionRow(r, true))] : [
    h('button', { class: 'sec gh', 'data-k': 'g' + encodeURIComponent(g.ws), title: g.level === 1 ? '' : MC.tilde(g.ws), 'aria-expanded': String(!g.closed) },
      icon(g.level === 1 ? 'pin' : 'folder', 'sm'), h('span', 'gn', g.label), g.closed && h('span', 'gc', String(g.count)), icon('chev', 'xs cv')),
    !g.closed && g.rows.map(r => sessionRow(r, false, g.level === 1)),
    !g.closed && g.hidden > 0 && h('button', { class: 'gmore', 'data-k': 'x' + encodeURIComponent(g.ws) }, 'Show ' + g.hidden + ' more')]);
}
// Rows of Needs you and Running show no time; the ones in Pinned have no folder header, so theirs is in the tooltip.
function sessionRow([id, title, ws, current, status, unseen, rel, bad, recent], pinned, inPins) {
  const shape = status === 'approval' ? h('span', { class: 'sst warn', title: 'Needs you' }, icon('hand', 'sm'))
    : status === 'running' || status === 'compacting' ? h('span', 'sst', spin('sm'))
    : bad ? h('span', { class: 'sst er', title: 'Last turn failed' }, icon('alert', 'sm'))
    : unseen ? h('span', { class: 'sst', title: 'New result' }, h('i', 'ndot'))
    : recent ? h('span', { class: 'sst', title: MC.finishedText(rel) }, h('i', 'rdot')) : h('span', 'sst');
  return h('div', 'srow' + (current ? ' cur' : ''), h('button', {
    class: 'sopen', 'data-k': 's' + id, 'aria-current': current ? 'page' : null, title: inPins ? MC.tilde(ws) : null,
    'aria-label': MC.titled(title) + ', ' + MC.basename(ws) + (status === 'approval' ? ', needs you' : status === 'running' ? ', running' : recent ? ', ' + MC.finishedText(rel) : ''),
  }, pendingRow === id ? h('span', 'sst', spin('sm')) : shape, rowText(MC.titled(title)), h('span', 'stime' + (pinned ? ' workspace' : '') + (recent ? ' recent' : ''), rel)),
  h('button', { class: 'ib more', 'data-k': 'm' + id, 'aria-label': 'Actions for ' + MC.titled(title), 'aria-haspopup': 'dialog' }, icon('more')));
}
(() => {
  let x0 = null;
  nav.addEventListener('touchstart', e => { x0 = wide.matches ? null : e.touches[0].clientX; }, { passive: true });
  nav.addEventListener('touchmove', e => { if (x0 != null && x0 - e.touches[0].clientX > 70) { x0 = null; closeNav(); } }, { passive: true });
})();
wide.addEventListener('change', () => { if (wide.matches) closeNav(); layout(); renderNav(); });

// ---- session menu, info, rename, delete ----
function sessionActions(id) {
  const x = S.state.sessions.find(y => y.id === id), current = x.current;
  sheet({
    key: 'actions', cls: 'dialog', title: MC.titled(x.title), sub: MC.tilde(x.workspace),
    body: m => h('div', 'list',
      !current && menuRow(m, 'open', 'Open', () => openLive(id), { focus: true }),
      pinRow(m, id),
      menuRow(m, 'edit', 'Rename', () => rename(id), { focus: current }),
      menuRow(m, 'copy', 'Copy session id', () => copyText(id)),
      menuRow(m, 'trash', 'Delete session', () => remove(id), { cls: 'bad', disabled: current, sub: current && 'Open another session first' })),
  });
}
export function rename(id) {
  const x = S.state.sessions.find(y => y.id === id) || S.state.session, d = { value: x.title || '', busy: false, err: '' };
  const save = e => {
    e.preventDefault();
    if (d.busy) return;
    d.err = '';
    if (intent(MC.intents.renameSession(id, d.value.trim()), a => { d.busy = false; if (a.ok) m.close(); else { d.err = MC.errorText(a.error); m.refresh(); } })) d.busy = true;
    m.refresh();
  };
  const m = sheet({
    key: 'rename', cls: 'dialog', title: 'Rename session', focusField: true, deps: () => [d.busy, d.err, online()],
    body: () => h('form', { novalidate: true, onsubmit: save, id: 'renf' },
      h('input', { class: 'tf', type: 'text', value: d.value, maxLength: 200, readOnly: d.busy, 'data-focus': true, 'data-k': 'ren', 'aria-label': 'Session title',
        placeholder: 'Session title', autocomplete: 'off', enterKeyHint: 'done', oninput: e => { d.value = e.target.value; } }),
      h('p', 'hint mt8', 'Leave empty to use the first message'),
      d.err && errBox(d.err, 'mt8')),
    foot: () => [h('button', { class: 'btn', onclick: () => m.close() }, 'Cancel'),
      h('button', { class: 'btn primary', type: 'submit', form: 'renf', 'data-k': 'save', disabled: d.busy || !online() }, d.busy && spin(), d.busy ? 'Saving…' : 'Save')],
  });
  const i = m.el.querySelector('input');
  // Focus inside the tap so iOS opens the keyboard.
  i.focus({ preventScroll: true }); i.select();
}
function remove(id) {
  const x = S.state.sessions.find(y => y.id === id), o = { title: 'Delete “' + MC.titled(x.title) + '”?', text: "This can't be undone.", label: 'Delete', danger: true, needsOnline: true, busy: false, err: '' };
  // The host never deletes the session a device views: this device moves to a new session in that folder first.
  const here = id === S.sid;
  if (here) o.text = 'You move to a new session in this folder. This can’t be undone.';
  const fail = a => { o.busy = false; o.err = MC.errorText(a.error).replace(/: open it and press Esc$/, ''); m.refresh(); };
  const del = () => intent(MC.intents.deleteSession(id), a => { if (a.ok) m.close(); else fail(a); });
  let m;
  o.ok = mm => {
    m = mm; o.err = '';
    if (here ? intent(MC.intents.newSession(x.workspace), a => { if (!a.ok) fail(a); else if (!del()) fail({}); }) : del()) o.busy = true;
    m.refresh();
  };
  confirmSheet(o);
}
export function openSessionMenu() {
  const s = S.state.session;
  sheet({
    key: 'menu', cls: 'dialog', title: MC.titled(s.title), sub: MC.tilde(s.workspace),
    body: m => h('div', 'list',
      pinRow(m, s.id), menuRow(m, 'edit', 'Rename', () => rename(s.id)), menuRow(m, 'diff', 'Changes', openChanges), menuRow(m, 'gauge', 'Context', openContext),
      menuRow(m, 'layers', 'Compact now', () => compactNow()), menuRow(m, 'search', 'Find in session', () => actions.find()), menuRow(m, 'info', 'Session info', openInfo),
      menuRow(m, 'copy', 'Copy session id', () => copyText(s.id)), menuRow(m, 'share', 'Share or export', exportSession),
      menuRow(m, 'trash', 'Delete session', () => remove(s.id), { cls: 'bad' })),
  });
}
export function openInfo() {
  const s = S.state.session;
  if (!s) return openModel();
  sheet({
    key: 'info', title: 'Session', deps: () => [S.state.session, S.state.usage, S.state.devices, S.state.context_percent],
    body: () => {
      const x = S.state.session, u = S.state.usage, cat = S.state.sessions.find(y => y.id === x.id);
      const others = MC.otherDevices(S.state.devices, S.device).filter(d => d.session === x.id).map(d => d.name);
      const kv = (k, v) => v && [h('dt', '', k), h('dd', '', v)];
      return [h('h3', 'stitle', MC.titled(x.title)),
        h('dl', 'kv', kv('Session', h('span', 'mono', x.id)), kv('Workspace', MC.tilde(x.workspace)), kv('Branch', x.branch), kv('Profile', x.profile), kv('Allowed for this session', x.allowed.join(', ')),
          kv('Model', [x.model, x.reasoning].filter(Boolean).join(' · ')), kv('Updated', cat && stampAt(Date.parse(cat.updated_at))),
          kv('Usage', (u.input || u.output) ? MC.fmtTok(u.input) + ' in · ' + MC.fmtTok(u.output) + ' out' + (u.cached ? ' · ' + MC.fmtTok(u.cached) + ' cached' : '') : ''),
          kv('Context', S.state.context_percent != null ? Math.round(S.state.context_percent) + '% used' : ''), kv('Also viewing', others.join(', '))),
        h('div', 'sacts', copyButton(x.id, 'Copy id', { class: 'btn sm' }))];
    },
  });
}

// ---- new session ----
let picker = null;
export function openNew() {
  const s = S.state;
  picker = { roots: null, listing: null, sel: s.draft.workspace, hidden: false, err: '', busy: true, mk: false, mkName: '', profile: s.draft.profile, starting: false, last: '' };
  const me = picker;
  sheet({
    key: 'new', cls: 'full', title: 'New session', onClose: () => { if (picker === me) picker = null; }, deps: () => [picker && { ...picker, roots: !!picker.roots }, online(), S.state.profiles],
    body: renderNew, foot: () => h('button', { class: 'btn primary block', 'data-k': 'start', disabled: !picker.sel || picker.starting || !online(), onclick: () => {
      if (intent(MC.intents.newSession(picker.sel, picker.profile), a => { if (!picker) return; picker.starting = false; if (a.ok) closeAll(); else { picker.err = MC.errorText(a.error); refreshSheets(); } })) picker.starting = true;
      refreshSheets();
    } }, picker.starting && spin(), picker.sel ? 'Start in ' + MC.basename(picker.sel) : 'Choose a folder'),
  });
  fsCall(MC.fs.roots(), r => {
    if (picker !== me) return;
    picker.busy = false;
    if (!r.ok) { picker.err = MC.fsError(r); return refreshSheets(); }
    picker.roots = r.roots;
    const w = s.draft.workspace;
    if (w && r.roots.some(x => MC.within(w, x.path))) list(w); else refreshSheets();
  });
}
function list(path) {
  const me = picker, id = picker.last = path + '|' + picker.hidden;
  picker.busy = true; picker.err = '';
  fsCall(MC.fs.list(path, picker.hidden), r => {
    if (picker !== me || picker.last !== id) return;
    picker.busy = false;
    if (r.ok) { picker.listing = r.listing; picker.sel = r.listing.path; picker.mk = false; } else picker.err = MC.fsError(r);
    refreshSheets();
  });
  refreshSheets();
}
function mkdir() {
  const name = picker.mkName.trim(), me = picker;
  if (!name || !picker.listing) return;
  picker.busy = true; picker.err = '';
  fsCall(MC.fs.mkdir(picker.listing.path, name), r => {
    if (picker !== me) return;
    picker.busy = false;
    if (r.ok) { picker.mkName = ''; list(r.created); } else { picker.err = MC.fsError(r); refreshSheets(); }
  });
  refreshSheets();
}
function renderNew() {
  const s = S.state, L = picker.listing, roots = picker.roots || [], recent = MC.recentWorkspaces(s.sessions);
  const folderRow = (name, sub, path, nav, ic) => h('button', { class: 'row', 'data-k': (nav ? 'n' : 's') + path, disabled: picker.starting,
    onclick: () => nav ? list(path) : (picker.sel = path, refreshSheets()) }, icon(ic || 'folder', 'ri'), rowText(name, sub),
    nav ? icon('chev', 'sm ri') : picker.sel === path && icon('check', 'ck'));
  const crumbs = L && MC.crumbs(L.path, roots);
  const profiles = s.profiles;
  return [
    profiles.length > 1 && [h('h3', 'sec2', 'Profile'), seg('Profile', profiles.map(p => [p, p]), picker.profile, p => { picker.profile = p; refreshSheets(); })],
    recent.length > 0 && [h('h3', 'sec2', 'Recent workspaces'), h('div', 'list', recent.map(w => folderRow(MC.basename(w), MC.tilde(w), w, false)))],
    h('h3', 'sec2', h('span', '', 'Browse'), picker.busy && spin('sm')),
    L && h('nav', { class: 'crumbs', 'aria-label': 'Folder path' }, h('button', { onclick: () => { picker.listing = null; picker.mk = false; refreshSheets(); } }, 'Roots'),
      crumbs.map((c, k) => [icon('chev', 'xs'), h('button', { 'aria-current': k === crumbs.length - 1 ? 'location' : null, onclick: () => list(c.path) }, c.name)])),
    L && h('div', 'tools', h('label', 'sw', h('span', '', 'Show hidden'), h('input', { type: 'checkbox', role: 'switch', checked: picker.hidden, onchange: e => { picker.hidden = e.target.checked; list(L.path); } })),
      h('button', { class: 'btn ghost sm', 'aria-expanded': String(picker.mk), onclick: () => { picker.mk = !picker.mk; refreshSheets(); if (picker.mk) findSheet('new').el.querySelector('#mkname').focus(); } }, icon('folderplus', 'sm'), 'New folder')),
    picker.mk && L && h('div', 'mkrow',
      h('input', { id: 'mkname', class: 'tf', 'data-k': 'mk', value: picker.mkName, placeholder: 'Folder name', 'aria-label': 'New folder name', autocapitalize: 'none', autocomplete: 'off', spellcheck: 'false', enterKeyHint: 'done',
        oninput: e => { picker.mkName = e.target.value; }, onkeydown: e => { if (e.key === 'Enter') { e.preventDefault(); mkdir(); } } }),
      h('button', { class: 'btn primary', disabled: picker.busy, onclick: mkdir }, 'Create')),
    picker.err && errBox(picker.err),
    L ? h('div', 'list', L.parent != null && folderRow(MC.basename(L.parent), 'Parent folder', L.parent, true, 'up'), L.entries.map(x => folderRow(x.name, null, x.path, true)),
      !L.entries.length && !picker.busy && h('div', 'muted', 'No folders here'))
      : roots.length > 0 && h('div', 'list', roots.map(r => MC.isDrive(r.path) ? folderRow(r.name, null, r.path, true, 'disk') : folderRow(r.name, MC.tilde(r.path), r.path, true))),
    L && L.truncated && h('div', 'hint', 'Only the first 1000 folders are shown.'),
  ];
}
export function stepSession(d) {
  const list = MC.sessionLayout(S.state.sessions, '', frozen, pins).groups.flatMap(g => g.items), i = list.findIndex(x => x.current), x = list[i + d];
  if (x && !pendingRow && intent(MC.intents.openSession(x.id))) pendingRow = x.id;
}
export function settle(id) { const was = pendingRow === id; if (was) pendingRow = null; return was; }
export const clearPending = () => { pendingRow = null; };

// ---- attention ----
const recentAttn = new Map(), dismissed = new Set();
const bannersPart = part($('banners'), b => [
  b.attn && h('div', { class: 'banner warn', role: 'status' }, icon('hand', 'sm'), h('span', '', '“' + b.attn.title + '” ' + (b.attn.kind === 'choice' ? 'has a question' : 'needs approval')),
    h('button', { class: 'btn sm', onclick: () => openLive(b.attn.id) }, 'Review'),
    h('button', { class: 'ib s', 'aria-label': 'Dismiss', onclick: () => { dismissed.add(b.attn.id); renderBanners(); } }, icon('x', 'sm'))),
  b.conn && h('div', { class: 'banner', role: 'status' }, spin(), h('span', '', b.conn)),
]);
export function renderBanners() {
  const s = S.state, x = s.sessions.find(y => y.live && !y.current && y.status === 'approval' && !dismissed.has(y.id));
  bannersPart({
    attn: x ? { id: x.id, title: MC.titled(x.title) } : null,
    conn: online() || isHalted() ? '' : navigator.onLine === false ? 'Offline · waiting for network' : 'Reconnecting…',
  });
}
// ---- badge, title, notices ----
let badgeN = -1;
export function renderAttention() {
  const s = S.state, n = isMain() && s ? MC.attentionCount(s) : 0;
  const t = s && s.session ? MC.titled(s.session.title) : 'MiniCore';
  set(document, 'title', (n ? '(' + n + ') ' : '') + (t === 'MiniCore' ? t : t + ' · MiniCore'));
  if (n !== badgeN && navigator.setAppBadge) { badgeN = n; (n ? navigator.setAppBadge(n) : navigator.clearAppBadge()).catch(() => {}); }
}
export function sessionEvent(e) {
  const label = MC.titled(e.title);
  recentAttn.set(label, Date.now());
  dismissed.delete(e.id);
  if (e.kind === 'finished') toast('“' + label + '” finished', null, { label: 'Open', fn: () => openLive(e.id) });
  else announce('“' + label + '” needs approval');
  notify(e.kind === 'finished' ? '“' + label + '” finished' : '“' + label + '” needs approval', e.id);
}
export function onNotice(n) {
  const ev = MC.noticeEvent(n.text), seen = () => ev && Date.now() - (recentAttn.get(ev.label) || 0) < 5000;
  if (ev && ev.kind === 'failed') { const x = S.state.sessions.find(y => y.title === ev.label && !y.current); if (x) markFailed(x.id); }
  if (!ev) return toast(n.text, n.level);
  // The state carrying the same event can arrive just after its notice.
  if (!seen()) setTimeout(() => { if (!seen()) toast(n.text, n.level); }, 1500);
}
