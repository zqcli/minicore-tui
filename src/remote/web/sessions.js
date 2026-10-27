import * as MC from './core.js';
import { HOST, S, fsCall, hostNow, intent, online, st, tz } from './net.js';
import { compactNow, confirmSheet, exportSession, openChanges, openContext, openModel, openSettings, seg } from './sheets.js';
import { $, actions, closeAll, copyText, errBox, fill, findSheet, h, icon, layout, menuRow, part, popModal, pushModal, refreshSheets, rowText, sheet, spin, stack, wide } from './ui.js';

export const failed = new Set();

export const openLive = id => { if (intent(MC.intents.openSession(id))) pendingRow = id; };
// ---- state ----
let tick = 0;
// One 1 s timer, only while this view's turn runs or a visible session list shows a running session's elapsed time.
export function syncTick() {
  const s = S.state, x = s && s.status;
  const want = !!s && (!!(x.started_ms && (x.busy || x.compacting)) || (navShown() && s.sessions.some(y => y.live && y.status === 'running' && y.started_ms)));
  if (want && !tick) tick = setInterval(onTick, 1000);
  if (!want && tick) { clearInterval(tick); tick = 0; }
}
function onTick() {
  const el = $('strip').querySelector('.el'), x = st();
  if (el && x.started_ms) el.textContent = MC.fmtElapsed(hostNow() - x.started_ms);
  if (navShown()) for (const e of $('nav').querySelectorAll('[data-start]')) e.textContent = MC.fmtElapsed(hostNow() - +e.dataset.start);
}
// ---- navigation: drawer on phones, sidebar on wide screens ----
const nav = $('nav');
let pendingRow = null, query = '', frozen = null, navLook = false;
const navShown = () => wide.matches || nav.classList.contains('on');
export function buildNav() {
  const q = h('input', { type: 'search', placeholder: 'Search sessions', 'aria-label': 'Search sessions', autocomplete: 'off', enterKeyHint: 'search', oninput: () => { query = q.value; renderNav(); } });
  fill(nav, h('div', 'nhead', h('img', { src: '/icon.svg', alt: '' }), h('div', '', h('b', '', 'MiniCore'), h('small', '', HOST)),
    h('button', { class: 'ib', 'aria-label': 'Settings', onclick: openSettings }, icon('gear')),
    h('button', { class: 'ib nclose', 'aria-label': 'Close', onclick: closeNav }, icon('x'))),
    h('button', { class: 'btn primary nnew', 'data-focus': true, onclick: () => openNew() }, icon('plus', 'sm'), 'New session'),
    h('label', 'search', icon('search', 'sm'), q), h('div', 'nlist'), h('div', 'nfoot'));
  listPart = part(nav.querySelector('.nlist'), renderList);
  footPart = part(nav.querySelector('.nfoot'), f => [h('span', 'cdot' + (f.on ? '' : ' off')), h('span', '', f.text)]);
}
let listPart, footPart;
export function openNav() {
  if (wide.matches) return nav.querySelector('input').focus();
  frozen = null;
  nav.classList.add('on');
  pushModal({ key: 'nav', el: nav, close: closeNav });
  renderNav(); syncTick();
}
export function closeNav() {
  if (!nav.classList.contains('on')) return;
  nav.classList.remove('on');
  popModal(stack.find(m => m.el === nav));
  frozen = null;
  syncTick();
}
// The list order stays put while it is in front of the user.
nav.addEventListener('pointerenter', () => { navLook = true; });
nav.addEventListener('pointerleave', () => { navLook = false; if (!nav.contains(document.activeElement)) { frozen = null; renderNav(); } });
nav.addEventListener('focusout', () => setTimeout(() => { if (!navLook && !nav.contains(document.activeElement) && wide.matches) { frozen = null; renderNav(); } }));
export function renderNav() {
  if (!navShown() || !S.state) return;
  const s = S.state, lay = MC.sessionLayout(s.sessions, hostNow(), tz(), query, frozen);
  if (nav.classList.contains('on') || navLook || nav.contains(document.activeElement)) frozen = lay.order;
  listPart({ groups: lay.groups.map(g => ({ label: g.label, items: g.items.map(x => [x.id, x.title, x.workspace, x.current, x.live && x.status, x.started_ms, x.unseen, MC.relTime(Date.parse(x.updated_at), hostNow(), tz()), failed.has(x.id)]) })), pendingRow, query: !!query });
  renderNavFoot();
}
export function renderNavFoot() {
  if (!footPart || !S.state) return;
  const n = online() ? MC.otherDevices(S.state.devices, S.device).length : 0;
  footPart({ on: online(), text: online() ? (S.device || 'This device') + (n ? ' · ' + MC.plural(n, 'other device') : '') : 'Reconnecting…' });
}
function renderList(l) {
  if (!l.groups.length) return h('div', 'muted', l.query ? 'No matching sessions' : 'No sessions yet');
  return l.groups.map(g => [h('div', 'sec' + (g.label === 'Needs you' ? ' warn' : ''), g.label), g.items.map(sessionRow)]);
}
function sessionRow([id, title, ws, current, status, started, unseen, rel, bad]) {
  const shape = status === 'approval' ? h('span', { class: 'sst warn', title: 'Needs you' }, icon('hand', 'sm'))
    : status === 'running' || status === 'compacting' ? h('span', 'sst', spin('sm'))
    : bad ? h('span', { class: 'sst er', title: 'Last turn failed' }, icon('alert', 'sm'))
    : unseen ? h('span', { class: 'sst', title: 'New result' }, h('i', 'ndot')) : h('span', 'sst');
  const what = status === 'approval' ? h('span', 'warn', 'Needs you') : status === 'running' ? (started ? h('span', { 'data-start': started }, MC.fmtElapsed(hostNow() - started)) : 'Running')
    : status === 'compacting' ? 'Compacting' : bad ? h('span', 'er', 'Failed') : unseen ? 'New result' : rel;
  return h('div', 'srow' + (current ? ' cur' : ''), h('button', {
    class: 'sopen', 'data-k': 's' + id, 'aria-current': current ? 'page' : null,
    'aria-label': MC.titled(title) + ', ' + MC.basename(ws) + (status === 'approval' ? ', needs you' : status === 'running' ? ', running' : ''),
    onclick: () => { if (current) return closeNav(); if (!pendingRow && intent(MC.intents.openSession(id))) { pendingRow = id; renderNav(); } },
  }, shape, rowText(MC.titled(title), [what, ' · ' + MC.basename(ws)]), pendingRow === id && spin('sm')),
  h('button', { class: 'ib more', 'data-k': 'm' + id, 'aria-label': 'Actions for ' + MC.titled(title), 'aria-haspopup': 'dialog', onclick: () => sessionActions(id) }, icon('more')));
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
    key: 'actions', cls: 'dialog', head: hid => h('h2', { id: hid }, MC.titled(x.title), h('span', 'sub2', MC.tilde(x.workspace))),
    body: m => h('div', 'list',
      !current && menuRow(m, 'open', 'Open', () => openLive(id), { focus: true }),
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
    key: 'menu', cls: 'dialog', head: hid => h('h2', { id: hid }, MC.titled(s.title), h('span', 'sub2', MC.tilde(s.workspace))),
    body: m => h('div', 'list',
      menuRow(m, 'edit', 'Rename', () => rename(s.id)), menuRow(m, 'diff', 'Changes', openChanges), menuRow(m, 'gauge', 'Context', openContext),
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
          kv('Model', [x.model, x.reasoning].filter(Boolean).join(' · ')), kv('Updated', cat && MC.stamp(Date.parse(cat.updated_at), hostNow(), tz())),
          kv('Usage', (u.input || u.output) ? MC.fmtTok(u.input) + ' in · ' + MC.fmtTok(u.output) + ' out' + (u.cached ? ' · ' + MC.fmtTok(u.cached) + ' cached' : '') : ''),
          kv('Context', S.state.context_percent != null ? Math.round(S.state.context_percent) + '% used' : ''), kv('Also viewing', others.join(', '))),
        h('div', 'sacts', h('button', { class: 'btn sm', onclick: e => copyText(x.id, e.currentTarget) }, icon('copy', 'xs'), 'Copy id'))];
    },
  });
}

// ---- new session ----
let ns = null;
export function openNew() {
  const s = S.state;
  ns = { roots: null, listing: null, sel: s.draft.workspace, hidden: false, err: '', busy: true, mk: false, mkName: '', profile: s.draft.profile, starting: false, last: '' };
  const me = ns;
  sheet({
    key: 'new', cls: 'full', title: 'New session', onClose: () => { if (ns === me) ns = null; }, deps: () => [ns && { ...ns, roots: !!ns.roots }, online(), S.state.profiles],
    body: renderNew, foot: () => h('button', { class: 'btn primary block', 'data-k': 'start', disabled: !ns.sel || ns.starting || !online(), onclick: () => {
      if (intent(MC.intents.newSession(ns.sel, ns.profile), a => { if (!ns) return; ns.starting = false; if (a.ok) closeAll(); else { ns.err = MC.errorText(a.error); refreshSheets(); } })) ns.starting = true;
      refreshSheets();
    } }, ns.starting && spin(), ns.sel ? 'Start in ' + MC.basename(ns.sel) : 'Choose a folder'),
  });
  fsCall(MC.fs.roots(), r => {
    if (ns !== me) return;
    ns.busy = false;
    if (!r.ok) { ns.err = MC.fsError(r); return refreshSheets(); }
    ns.roots = r.roots;
    const w = s.draft.workspace;
    if (w && r.roots.some(x => MC.within(w, x.path))) list(w); else refreshSheets();
  });
}
function list(path) {
  const me = ns, id = ns.last = path + '|' + ns.hidden;
  ns.busy = true; ns.err = '';
  fsCall(MC.fs.list(path, ns.hidden), r => {
    if (ns !== me || ns.last !== id) return;
    ns.busy = false;
    if (r.ok) { ns.listing = r.listing; ns.sel = r.listing.path; ns.mk = false; } else ns.err = MC.fsError(r);
    refreshSheets();
  });
  refreshSheets();
}
function mkdir() {
  const name = ns.mkName.trim(), me = ns;
  if (!name || !ns.listing) return;
  ns.busy = true; ns.err = '';
  fsCall(MC.fs.mkdir(ns.listing.path, name), r => {
    if (ns !== me) return;
    ns.busy = false;
    if (r.ok) { ns.mkName = ''; list(r.created); } else { ns.err = MC.fsError(r); refreshSheets(); }
  });
  refreshSheets();
}
function renderNew() {
  const s = S.state, L = ns.listing, roots = ns.roots || [], recent = MC.recentWorkspaces(s.sessions);
  const folderRow = (name, sub, path, nav, ic) => h('button', { class: 'row', 'data-k': (nav ? 'n' : 's') + path, disabled: ns.starting,
    onclick: () => nav ? list(path) : (ns.sel = path, refreshSheets()) }, icon(ic || 'folder', 'ri'), rowText(name, sub),
    nav ? icon('chev', 'sm ri') : ns.sel === path && icon('check', 'ck'));
  const crumbs = L && MC.crumbs(L.path, roots);
  const profiles = s.profiles;
  return [
    profiles.length > 1 && [h('h3', 'sec2', 'Profile'), seg('Profile', profiles.map(p => [p, p]), ns.profile, p => { ns.profile = p; refreshSheets(); })],
    recent.length > 0 && [h('h3', 'sec2', 'Recent workspaces'), h('div', 'list', recent.map(w => folderRow(MC.basename(w), MC.tilde(w), w, false)))],
    h('h3', 'sec2', h('span', '', 'Browse'), ns.busy && spin('sm')),
    L && h('nav', { class: 'crumbs', 'aria-label': 'Folder path' }, h('button', { onclick: () => { ns.listing = null; ns.mk = false; refreshSheets(); } }, 'Roots'),
      crumbs.map((c, k) => [icon('chev', 'xs'), h('button', { 'aria-current': k === crumbs.length - 1 ? 'location' : null, onclick: () => list(c.path) }, c.name)])),
    L && h('div', 'tools', h('label', 'sw', h('span', '', 'Show hidden'), h('input', { type: 'checkbox', role: 'switch', checked: ns.hidden, onchange: e => { ns.hidden = e.target.checked; list(L.path); } })),
      h('button', { class: 'btn ghost sm', 'aria-expanded': String(ns.mk), onclick: () => { ns.mk = !ns.mk; refreshSheets(); if (ns.mk) findSheet('new').el.querySelector('#mkname').focus(); } }, icon('folderplus', 'sm'), 'New folder')),
    ns.mk && L && h('div', 'mkrow',
      h('input', { id: 'mkname', class: 'tf', 'data-k': 'mk', value: ns.mkName, placeholder: 'Folder name', 'aria-label': 'New folder name', autocapitalize: 'none', autocomplete: 'off', spellcheck: 'false', enterKeyHint: 'done',
        oninput: e => { ns.mkName = e.target.value; }, onkeydown: e => { if (e.key === 'Enter') { e.preventDefault(); mkdir(); } } }),
      h('button', { class: 'btn primary', disabled: ns.busy, onclick: mkdir }, 'Create')),
    ns.err && errBox(ns.err),
    L ? h('div', 'list', L.parent != null && folderRow(MC.basename(L.parent), 'Parent folder', L.parent, true, 'up'), L.entries.map(x => folderRow(x.name, null, x.path, true)),
      !L.entries.length && !ns.busy && h('div', 'muted', 'No folders here'))
      : roots.length > 0 && h('div', 'list', roots.map(r => MC.isDrive(r.path) ? folderRow(r.name, null, r.path, true, 'disk') : folderRow(r.name, MC.tilde(r.path), r.path, true))),
    L && L.truncated && h('div', 'hint', 'Only the first 1000 folders are shown.'),
  ];
}
export function stepSession(d) {
  const list = MC.sessionLayout(S.state.sessions, hostNow(), tz(), '', frozen).groups.flatMap(g => g.items), i = list.findIndex(x => x.current), x = list[i + d];
  if (x && !pendingRow && intent(MC.intents.openSession(x.id))) pendingRow = x.id;
}
// Whether `id` was the row being opened, now answered.
export function settle(id) { const was = pendingRow === id; if (was) pendingRow = null; return was; }
export const clearPending = () => { pendingRow = null; };
