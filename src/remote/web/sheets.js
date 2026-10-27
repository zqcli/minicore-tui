import * as MC from './core.js';
import { HOST, S, ask, intent, online, turnStatus, viewed } from './net.js';
import { installApp, installable, prefs, setPref, standalone } from './platform.js';
import { actions, copyText, diffView, errBox, h, icon, refreshSheets, rowText, share, sheet, spin, toast } from './ui.js';

export function confirmSheet(o) {
  const m = sheet({
    key: 'confirm', cls: 'dialog', title: o.title, deps: () => [o.busy, o.err, online()],
    body: () => [o.text && h('p', 'hint big', o.text), o.err && errBox(o.err, 'mt12')],
    foot: () => [h('button', { class: 'btn', 'data-k': 'no', onclick: () => m.close() }, 'Cancel'),
      h('button', { class: 'btn ' + (o.danger ? 'dangerfill' : 'primary'), 'data-k': 'ok', disabled: o.busy || (o.needsOnline && !online()), onclick: () => o.ok(m) }, o.busy && spin(), o.label)],
  });
  return m;
}

const SIZES = [['Small', .9], ['Default', 1], ['Large', 1.1], ['Larger', 1.2]];
export function seg(label, items, value, set) {
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
        h('div', 'mt24', h('button', { class: 'btn block danger', onclick: () => confirmSheet({ title: 'Sign out of this device?', text: 'You will need the password shown on your computer to connect again.', label: 'Sign out', danger: true, ok: m => { m.close(true); actions.signOut(); } }) }, icon('logout', 'sm'), 'Sign out')),
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
        c.diff.truncated && h('span', 'hint', 'Diff truncated')), diffView(MC.parseDiff(c.diff.lines.join('\n')).map(f => ({ ...f, path: f.path || c.path })), c.wrap)] : h('div', 'loadrow', spin(), 'Loading diff…');
      if (!c.files) return h('div', 'loadrow', spin(), 'Loading changes…');
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
      if (!c.d) return h('div', 'loadrow', spin(), 'Loading…');
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
      h('button', { class: 'btn sm', onclick: e => copyText(o.text, e.currentTarget) }, icon('copy', 'xs'), 'Copy'),
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
const KEYS = [['Esc', 'Close the top sheet, or stop the turn'], ['Enter', 'Send, or steer a running turn (“Enter sends”)'], ['⌘/Ctrl Enter', 'Send, or steer a running turn'], ['⌥/Alt Enter', 'Queue a follow-up while a turn runs'], ['↑ ↓', 'Recent prompts in an empty composer'],
  ['⌘/Ctrl K', 'Commands and prompts'], ['⌘/Ctrl F', 'Find in session'], ['Alt ↑ ↓', 'Previous or next session'], ['Y A N', 'Allow, allow for session, deny'], ['?', 'This help']];
export function openHelp() {
  sheet({ key: 'help', cls: 'dialog', title: 'Keyboard shortcuts', body: () => h('dl', 'kv keys', KEYS.map(([k, d]) => [h('dt', '', h('kbd', '', k)), h('dd', '', d)])) });
}

