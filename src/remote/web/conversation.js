import * as MC from './core.js';
import { HOST, S, abandon, ask, hostNow, intent, online, pendingKind, repaint, send, sending, turnStatus, tz, viewed } from './net.js';
import { compactNow, exportSession, openApprovals, openChanges, openContext, openHelp, openInfo, openModel, openNav, openNew, openOutput, openSettings, rename } from './sessions.js';
import { $, actions, announce, apple, copyText, diffView, findSheet, h, hover, icon, inField, keepFocus, kids, menuRow, notify, part, prefs, reduced, set, share, sheet, spin, toast, topModal, wide } from './ui.js';

// ---- composer ----
// "Enter sends" is stored once chosen; until then the pointer default follows the device.
const ALT = apple ? '⌥' : 'Alt+', tips = () => prefs.enter ? 'Enter' : (apple ? '⌘' : 'Ctrl+') + 'Enter';
// ---- dock: status strip, queue and composer ----
const ta = $('ta');
const stripPart = part($('strip'), x => [spin('sm'), h('b', '', x.label),
  x.started != null && h('span', 'el', MC.fmtElapsed(hostNow() - x.started)),
  x.detail && h('span', 'mono', x.detail)]);
function renderStrip() {
  const s = turnStatus(), on = !!(s.busy || s.compacting || s.loading);
  set($('strip'), 'hidden', !on);
  if (on) stripPart({ ...MC.statusLine(S.state, S.blocks), started: s.started_ms });
}
export function stop() { if (MC.canStop(turnStatus()) && !turnStatus().interrupting && !pendingKind('cancel')) intent(MC.intents.cancel()); }
const trayPart = part($('tray'), t => [
  t.lost.map(([id, text]) => h('div', { class: 'lost', role: 'alert' }, icon('warn', 'sm'),
    h('div', '', h('div', '', 'Not sent · it may or may not have arrived'), h('div', 'lt', text)),
    h('button', { class: 'btn sm', 'data-k': 'again' + id, disabled: !t.on }, icon('retry', 'xs'), 'Send again'),
    h('button', { class: 'ib s', 'aria-label': 'Edit instead', 'data-k': 'edit' + id }, icon('edit', 'sm')))),
  t.q.items.length > 0 && h('div', 'qcap', 'Sent in order · steers join this turn; follow-ups start the next'),
  t.q.items.map(q => h('div', { class: 'qp ' + q.kind, 'data-index': q.index, 'data-text': q.text },
    h('div', 'qbody', h('div', 'qhead', h('span', 'qk', icon(q.kind, 'xs'), (q.kind === 'steer' ? 'Steer ' : 'Follow-up ') + q.n),
      q.sent && h('span', 'qsent', 'Sent · applies at the next step')), h('span', 'qt', q.text)),
    !q.sent && [h('button', { class: 'ib', 'data-k': 'qedit', disabled: !t.on, 'aria-label': 'Edit in composer', title: 'Edit in composer' }, icon('edit', 'sm')),
      h('button', { class: 'ib', 'data-k': 'qdelete', disabled: !t.on, 'aria-label': 'Delete', title: 'Delete' }, icon('x', 'sm'))])),
  t.q.long && h('button', { class: 'qmore', 'data-k': 'qmore', 'aria-expanded': String(qAll) }, t.q.hidden ? '+' + t.q.hidden + ' more' : 'Show less'),
]);
let qAll = false;
const edits = new Set();
export const unqueueRefused = text => edits.delete(text);
$('tray').addEventListener('click', e => {
  const button = e.target.closest('[data-k]');
  if (!button) return;
  const k = button.dataset.k;
  if (k === 'qmore') { qAll = !qAll; renderTray(); }
  else if (k === 'qedit' || k === 'qdelete') {
    const row = button.closest('.qp'), edit = k === 'qedit', text = row.dataset.text;
    if (online()) {
      if (edit) edits.add(text);
      intent(MC.intents.unqueue(+row.dataset.index, text, edit));
    }
  } else if (k.startsWith('again')) { send(MC.sendAgain(S, k.slice(5))); repaint(); }
  else if (k.startsWith('edit')) {
    const id = k.slice(4), text = S.intents.get(id).text;
    abandon(id); fillComposer(MC.mergeReturned(ta.value, text)); repaint();
  }
});
function renderTray() {
  const q = MC.queueView(S.state.queue, qAll);
  trayPart({ lost: MC.unconfirmed(S).map(it => [it.id, it.text]), q, on: online() });
}
// The bottom row of the composer: + and the draft's folder and profile on the left, then the slot (the model as plain text,
// or Follow-up while a turn runs with text typed) and the primary button, which keeps its place and changes face.
const PRIMARY = {
  send: { icon: 'up', label: 'Send' }, steer: { icon: 'steer', label: 'Steer' },
  stop: { icon: 'stop', label: 'Stop the turn' }, stopping: { icon: null, label: 'Stopping' },
};
let primary = '', stopAt = 0, press = null;
const composerState = () => {
  const s = turnStatus(), lock = sending();
  return { busy: !!s.busy, compacting: !!s.compacting, text: ta.value, stopping: MC.canStop(s) && (!!s.interrupting || pendingKind('cancel')), lock };
};
const chipsPart = part($('chips'), c => [h('div', 'cl',
  h('button', { class: 'chip icon', 'data-k': 'plus', 'aria-label': 'Commands, prompts and files', 'aria-haspopup': 'dialog', onclick: openPlus }, icon('plus', 'sm')),
  h('button', { class: 'chip icon', 'data-k': 'appr', 'aria-label': 'Approvals', 'aria-haspopup': 'dialog', onclick: openApprovals }, icon('shield', 'sm'), c.allowed && h('i', { class: 'sdot', 'aria-hidden': 'true' })),
  c.ctx && h('button', { class: 'chip ctx' + (c.hi ? ' hi' : ''), 'data-k': 'ctx', 'aria-label': c.ctx + ' of context used', onclick: openContext }, c.ctx),
  c.draft && h('button', { class: 'chip', 'data-k': 'folder', 'aria-label': 'Folder ' + c.draft.folder + ', change', onclick: () => openNew() }, icon('folder', 'xs'), h('span', '', c.draft.folder)),
  c.draft && c.draft.profiles && h('button', { class: 'chip', 'data-k': 'profile', 'aria-label': 'Profile ' + c.draft.profile + ', change', onclick: openProfile }, h('span', '', c.draft.profile || 'Profile'))),
  c.followUp
    ? h('button', { class: 'tbtn acc', 'data-k': 'follow', title: c.tips && 'Queue as follow-up · ' + ALT + 'Enter', 'aria-label': 'Queue as a follow-up', disabled: c.disabled, onclick: () => submit('queue') }, icon('follow', 'sm'), 'Follow-up')
    : h('button', { class: 'tbtn', 'data-k': 'model', 'aria-label': 'Model ' + c.model + ', change', onclick: openModel }, h('span', '', c.modelShort), c.level && h('span', 'rz', '· ' + c.level), icon('chevd', 'xs')),
  h('button', { id: 'pri', class: 'pri ' + c.kind, type: 'button', 'data-k': 'pri', title: c.tip, 'aria-label': PRIMARY[c.kind].label, disabled: c.disabled,
    onpointerdown: priDown, onpointermove: priMove, onpointerup: priEnd, onpointercancel: priEnd, oncontextmenu: priMenu, onclick: priTap },
    c.kind === 'stopping' ? spin() : icon(PRIMARY[c.kind].icon)),
]);
function renderChips() {
  const s = turnStatus(), c = viewed(), d = S.state.draft, cs = composerState(), kind = MC.primaryKind(cs), typed = cs.text.trim() !== '';
  set(ta, 'readOnly', cs.lock);
  set(ta, 'placeholder', MC.placeholder(s.busy, !!S.state.session, hover.matches && { send: tips(), alt: ALT }));
  if (kind !== primary) {
    if (kind !== 'steer') closePriMenu();
    if (kind === 'stop') stopAt = Date.now();
    // Only the turn-related faces are announced, so typing does not chatter.
    if (kind !== 'send' && primary !== '') announce(PRIMARY[kind].label);
    primary = kind;
  }
  const tip = !hover.matches ? null : kind === 'send' ? 'Send · ' + tips() : kind === 'stop' ? 'Stop the turn · Esc'
    : kind === 'steer' ? 'Steer · ' + tips() + ' · right-click for more' : null;
  chipsPart({
    kind, tip, tips: hover.matches && tips(), followUp: cs.busy && typed && kind === 'steer', draft: !S.state.session && { folder: MC.basename(d.workspace), profile: d.profile, profiles: S.state.profiles.length > 1 },
    allowed: !!(S.state.session && S.state.session.allowed.length), ctx: S.state.context_percent != null ? Math.round(S.state.context_percent) + '%' : '', hi: S.state.context_percent >= 80,
    model: c.model || '', modelShort: c.model ? MC.midTrunc(c.model, wide.matches ? 28 : 16) : 'Model', level: c.reasoning,
    disabled: cs.lock || !online() || kind === 'stopping' || (kind === 'send' && !typed),
  });
}
// A tap on the primary asks MC what it means from the live text. A long press or right click opens the menu while text is
// typed during a turn, which is where Stop is otherwise out of reach.
function priDown(e) {
  press = { x: e.clientX, y: e.clientY, moved: 0, fired: false, timer: 0 };
  if (e.pointerType !== 'mouse') press.timer = setTimeout(() => { press.fired = true; openPriMenu(); }, 450);
}
function priMove(e) {
  if (!press) return;
  press.moved = Math.max(press.moved, Math.hypot(e.clientX - press.x, e.clientY - press.y));
  if (press.moved > MC.TAP_SLOP) clearTimeout(press.timer);
}
function priEnd() { if (press) clearTimeout(press.timer); }
function priMenu(e) { e.preventDefault(); openPriMenu(); }
function priTap(e) {
  const p = press;
  press = null;
  if (p && p.fired) return;
  const act = MC.primaryTap({ ...composerState(), stopAt: primary === 'stop' ? stopAt : Date.now() }, Date.now(), p && e.detail ? p.moved : 0);
  if (act === 'stop') stop(); else if (act) submit(act);
}
function openPriMenu() {
  if ($('pmenu') || MC.primaryKind(composerState()) !== 'steer') return;
  const item = (ic, label, key, fn) => h('button', { class: 'pm', role: 'menuitem', onclick: () => { closePriMenu(); fn(); } }, icon(ic, 'sm'), h('span', '', label), h('span', 'pk', key));
  $('comp').append(h('div', { id: 'pmenu', role: 'menu', 'aria-label': 'Turn actions' },
    item('steer', 'Steer now', tips(), () => submit('steer')), item('follow', 'Queue as follow-up', ALT + 'Enter', () => submit('queue')), item('stop', 'Stop the turn', 'Esc', stop)));
  $('pmenu').firstChild.focus({ preventScroll: true });
}
export function closePriMenu() { const m = $('pmenu'); if (m) m.remove(); }
document.addEventListener('pointerdown', e => { if (!e.target.closest('#pmenu')) closePriMenu(); }, true);
export function renderDock() { renderStrip(); renderTray(); renderChips(); }
function grow() {
  ta.style.height = 'auto';
  ta.style.height = ta.scrollHeight + 'px';
}
const draftKey = () => 'minicore.draft.' + (S.sid || 'new');
function composerInput() {
  grow(); renderChips(); updateMenu();
  const lit = MC.literal(ta.value);
  ta.setAttribute('autocorrect', lit ? 'off' : 'on');
  ta.spellcheck = !lit;
  if (ta.value) localStorage.setItem(draftKey(), ta.value); else localStorage.removeItem(draftKey());
}
function loadDraft() { if (!sending()) { ta.value = localStorage.getItem(draftKey()) || ''; composerInput(); } }
function fillComposer(text) { ta.value = text; recall = -1; composerInput(); ta.focus(); ta.setSelectionRange(text.length, text.length); }
function submit(kind) {
  const text = ta.value;
  if (!text.trim() || sending()) return;
  const c = MC.parseSlash(text, S.state.commands);
  if (c && c.builtin) { ta.value = ''; composerInput(); return builtin(c); }
  if (c && c.unknown) return toast('Unknown command /' + c.unknown, 'error');
  if (!online()) return;
  intent(kind === 'queue' ? MC.intents.queue(text) : MC.intents.submit(text));
  setMenu(null);
}
function builtin(c) {
  const s = S.state.session, need = () => { toast('Send a first message to start the session'); };
  switch (c.builtin) {
    case 'new': return openNew();
    case 'model': return openModel();
    case 'rename': return !s ? need() : c.arg ? intent(MC.intents.renameSession(s.id, c.arg)) : rename(s.id);
    case 'compact': return !s ? need() : compactNow();
    case 'context': return !s ? need() : openContext();
    case 'changes': return !s ? need() : openChanges();
    case 'export': return !s ? need() : exportSession();
    case 'find': return openFind();
    case 'session': return openInfo();
    case 'sessions': return openNav();
    case 'copy': { const b = [...S.blocks].reverse().find(x => x.type === 'assistant' && x.text); return b ? copyText(b.text) : toast('No answer to copy yet'); }
    case 'settings': return openSettings();
    case 'help': return openHelp();
  }
}
ta.addEventListener('input', () => { recall = -1; composerInput(); });
ta.addEventListener('click', updateMenu);

let menu = null, filesT = 0, recall = -1;
const recentKey = 'minicore.recent.' + HOST;
let recent = JSON.parse(localStorage.getItem(recentKey) || '[]');
const popPart = part($('pop'), m => m && (m.items.length ? m.items.map((x, k) => h('button', {
  class: 'opt', role: 'option', id: 'opt' + k, 'aria-selected': String(k === m.sel), tabindex: '-1',
}, m.kind === 'cmd' ? [h('span', 'on', '/' + x.name), h('span', 'od', x.description), h('span', 'tag', x.hint || (x.builtin ? '' : 'template'))]
  : [icon('file', 'sm'), h('span', 'od file', x)])) : h('div', 'muted', m.busy ? 'Searching…' : m.kind === 'cmd' ? 'No matching commands' : 'No matching files')));
// The input keeps its focus through a press on an option.
$('pop').addEventListener('mousedown', e => { if (e.target.closest('.opt')) e.preventDefault(); });
$('pop').addEventListener('click', e => { const o = e.target.closest('.opt'); if (o) accept(+o.id.slice(3)); });
function setMenu(m) {
  menu = m;
  $('pop').hidden = !m;
  ta.setAttribute('aria-expanded', String(!!m));
  if (m && m.items.length) ta.setAttribute('aria-activedescendant', 'opt' + m.sel); else ta.removeAttribute('aria-activedescendant');
  popPart(m && { kind: m.kind, items: m.items, sel: m.sel, busy: m.busy });
  const o = m && $('opt' + m.sel);
  if (o) o.scrollIntoView({ block: 'nearest' });
}
function updateMenu() {
  if (!S.state) return;
  const text = ta.value, cmds = MC.slashMenu(text, S.state.commands);
  if (cmds) return setMenu({ kind: 'cmd', items: cmds, sel: 0 });
  const tok = ta.selectionStart === ta.selectionEnd && MC.atToken(text, ta.selectionStart);
  if (!tok || !S.state.session) return setMenu(null);
  if (menu && menu.kind === 'file' && menu.tok.query === tok.query) { menu.tok = tok; return; }
  setMenu({ kind: 'file', items: menu && menu.kind === 'file' ? menu.items : [], sel: 0, tok, busy: true });
  clearTimeout(filesT);
  filesT = setTimeout(() => ask(MC.queries.files(tok.query), r => {
    if (!menu || menu.kind !== 'file' || menu.tok.query !== tok.query) return;
    setMenu({ ...menu, items: r.ok ? r.data.paths : [], busy: false, sel: 0 });
    if (!r.ok) toast(MC.errorText(r.error), 'error');
  }), 150);
}
function accept(k) {
  const m = menu, x = m.items[k];
  if (m.kind === 'file') {
    const r = MC.insertMention(ta.value, m.tok, x);
    ta.value = r.text; ta.setSelectionRange(r.caret, r.caret);
  } else if (x.builtin && x.name !== 'rename') { ta.value = ''; setMenu(null); composerInput(); return builtin({ builtin: x.name, arg: '' }); }
  else ta.value = '/' + x.name + ' ';
  ta.focus();
  composerInput();
}
ta.addEventListener('keydown', e => {
  // Safari can clear isComposing before the candidate-confirming Enter.
  if (e.isComposing || e.keyCode === 229) return;
  if (menu && menu.items.length && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
    e.preventDefault();
    setMenu({ ...menu, sel: (menu.sel + (e.key === 'ArrowDown' ? 1 : menu.items.length - 1)) % menu.items.length });
    return;
  }
  if (menu && menu.items.length && (e.key === 'Tab' || (e.key === 'Enter' && !e.shiftKey && !e.metaKey && !e.ctrlKey))) { e.preventDefault(); accept(menu.sel); return; }
  if (menu && e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); setMenu(null); return; }
  // Hardware keyboards walk the recent prompts with ↑/↓ from an empty composer.
  if ((e.key === 'ArrowUp' || e.key === 'ArrowDown') && !e.altKey && !e.metaKey && !e.ctrlKey && !e.shiftKey && (ta.value === '' || ta.value === recent[recall])) {
    const k = recall + (e.key === 'ArrowUp' ? 1 : -1);
    if (k >= recent.length || (k < 0 && recall < 0)) return;
    e.preventDefault();
    recall = k;
    ta.value = k < 0 ? '' : recent[k];
    composerInput();
    return;
  }
  const act = MC.composerKey({ key: e.key, shift: e.shiftKey, alt: e.altKey, mod: e.metaKey || e.ctrlKey }, { busy: !!turnStatus().busy, text: ta.value, popup: !!(menu && menu.items.length), enter: prefs.enter });
  if (act) { e.preventDefault(); submit(act); }
});
ta.addEventListener('blur', () => setTimeout(() => { if (document.activeElement !== ta) setMenu(null); }, 150));
function remember(text) { recent = MC.pushRecent(recent, text); localStorage.setItem(recentKey, JSON.stringify(recent)); }
export function openPlus() {
  sheet({
    key: 'plus', title: 'Add to your message', deps: () => [S.state.commands, recent.slice(0, 8), !!S.state.session],
    body: m => {
      const cmds = S.state.commands.map(c => ({ ...c, builtin: false })).concat(MC.BUILTINS);
      return [S.state.session && [h('h3', 'sec2', 'Insert'), h('div', 'list', menuRow(m, 'at', 'File', insertAt, { sub: 'Mention a file in this workspace as @path' }))],
        recent.length > 0 && [h('h3', 'sec2', 'Recent prompts'), h('div', 'list', recent.slice(0, 8).map(t => menuRow(m, 'history', t.length > 120 ? t.slice(0, 120) + '…' : t, () => fillComposer(t))))],
        h('h3', 'sec2', 'Commands'), h('div', 'list', cmds.map(c => menuRow(m, c.builtin ? 'term' : 'sparkle', '/' + c.name + (c.hint ? ' ' + c.hint : ''),
          () => { setMenu({ kind: 'cmd', items: [c], sel: 0 }); accept(0); }, { sub: c.description, mono: true })))];
    },
  });
}
function insertAt() {
  const p = ta.selectionStart, v = ta.value, pre = p > 0 && !/\s$/.test(v.slice(0, p)) ? ' @' : '@';
  ta.value = v.slice(0, p) + pre + v.slice(p);
  ta.focus();
  ta.setSelectionRange(p + pre.length, p + pre.length);
  composerInput();
}
function openProfile() {
  const d = S.state.draft;
  const m = sheet({
    key: 'profile', cls: 'dialog', title: 'Profile', deps: () => [S.state.draft.profile, S.state.profiles],
    body: () => h('div', { class: 'list', role: 'radiogroup', 'aria-label': 'Profile' }, S.state.profiles.map(p => h('button', {
      class: 'row flat', role: 'radio', 'aria-checked': String(p === S.state.draft.profile),
      onclick: () => { if (p !== S.state.draft.profile) intent(MC.intents.newSession(d.workspace, p)); m.close(); },
    }, h('span', 'rt', h('span', 'rm', p)), p === S.state.draft.profile && icon('check', 'ck')))),
  });
}


export function sent(text) { remember(text); ta.value = MC.retireText(ta.value, text); composerInput(); }
export function returned(text) {
  const mine = edits.delete(text);
  fillComposer(MC.mergeReturned(ta.value, text));
  if (!mine) toast('Message returned to the composer');
}
// An unconfirmed send lives in its card only, so it is never in two places to send twice.
export function unconfirmedHome() {
  for (const it of MC.unconfirmed(S)) ta.value = MC.retireText(ta.value, it.text);
  edits.clear();
}

// ---- transcript ----
// `opened` holds the rows the reader opened or closed by key; a key not in it starts as `startsOpen` says.
const log = $('log'), box = $('blocks'), nodes = [], dirty = new Set(), opened = new Map(), thinkTimes = new Map();
// The first block of the turn in progress: its finished steps read dimmer than the running one.
let liveFrom = Infinity;
const MAX_MOUNT = 600;
let lo = 0, hi = 0, win = MC.WINDOW, pinned = true, stick = true, anchor = null, raf = 0, liveIdx = -1;
export const schedule = () => { if (!raf) raf = requestAnimationFrame(flush); };
const atBottom = () => log.scrollHeight - log.scrollTop - log.clientHeight < 80;
// Tool rows start open when they failed and thinking when the setting says so; folds, groups, messages and compactions start closed.
const startsOpen = (k, b) => k[0] === 't' ? b.state === 'failed' : k[0] === 'r' && prefs.thinking;
const isOpen = (k, b) => opened.has(k) ? opened.get(k) : startsOpen(k, b);
export function onNewSession() { stick = true; pinned = true; win = MC.WINDOW; opened.clear(); thinkTimes.clear(); loadDraft(); }
export function resetTranscript() { nodes.forEach(n => n.remove()); nodes.length = 0; lo = hi = 0; win = MC.WINDOW; pinned = true; opened.clear(); dirty.clear(); }
function redrawAll() { for (let i = lo; i < hi; i++) dirty.add(i); schedule(); }
// The element with data-k `k` keeps its place on screen through the next flush.
function anchorTo(el, k) { anchor = { k: k || (el && el.dataset.k), y: el ? el.getBoundingClientRect().top : 0 }; }
// One listener opens and closes every row. data-tg names the key when the button's data-k is not it ("Hide" closes its fold);
// the thinking preview acts as its button.
box.addEventListener('click', e => {
  const preview = e.target.closest('.tp');
  if (preview) return preview.previousSibling.click();
  const t = e.target.closest('.tr,.tk,.more2');
  if (!t) return;
  const k = t.dataset.tg || t.dataset.k, i = +t.closest('.b').dataset.i;
  opened.set(k, !isOpen(k, S.blocks[i]));
  dirty.add(i); anchorTo(t, k); schedule();
});

function inlineInto(tokens, p) {
  for (const t of tokens) {
    if (t.t === 'text') p.append(t.s);
    else if (t.t === 'code') p.append(h('code', '', t.s));
    else if (t.t === 'a') { const a = h('a', { href: t.href, target: '_blank', rel: 'noopener noreferrer' }); inlineInto(t.c, a); p.append(a); }
    else { const n = h(t.t === 'b' ? 'strong' : t.t === 's' ? 'del' : 'em'); inlineInto(t.c, n); p.append(n); }
  }
}
function mdToNode(t) {
  if (t.t === 'p' || t.t === 'h') { const n = h(t.t === 'p' ? 'p' : 'h' + t.n); inlineInto(t.c, n); return n; }
  if (t.t === 'list') {
    const n = h(t.ord ? 'ol' : 'ul', t.ord && t.start !== 1 ? { start: t.start } : null);
    for (const it of t.items) { const li = h('li'); inlineInto(it.c, li); it.sub.forEach(s => li.append(mdToNode(s))); n.append(li); }
    return n;
  }
  if (t.t === 'code') return codeBlock(t.lang || 'code', t.text);
  if (t.t === 'table') {
    const cell = (tag, c, k) => { const n = h(tag, t.align[k] ? { style: 'text-align:' + t.align[k] } : null); inlineInto(c, n); return n; };
    return h('div', { class: 'tw', tabindex: '0', role: 'region', 'aria-label': 'Table' }, h('table', '', h('thead', '', h('tr', '', t.head.map((c, k) => cell('th', c, k)))),
      h('tbody', '', t.rows.map(r => h('tr', '', t.head.map((_, k) => cell('td', r[k] || [], k)))))));
  }
  if (t.t === 'quote') return h('blockquote', '', t.c.map(mdToNode));
  return h('hr');
}
// The Wrap toggle of a code or output box; the box starts wrapped or not by the setting.
function wrapButton(box) {
  const w = h('button', { 'aria-pressed': String(prefs.wrap), onclick: () => w.setAttribute('aria-pressed', String(box.classList.toggle('wrap'))) }, icon('wrap', 'xs'), 'Wrap');
  return w;
}
function codeBlock(lang, text) {
  const n = h('div', 'cb' + (prefs.wrap ? ' wrap' : ''));
  const c = h('button', { 'aria-label': 'Copy code', onclick: () => copyText(text, c) }, icon('copy', 'xs'), 'Copy');
  n.append(h('div', 'cbh', h('span', '', lang), wrapButton(n), c), h('pre', '', h('code', '', text)));
  return n;
}
function caretInto(body) {
  let t = body;
  while (t.lastElementChild && t.lastElementChild.matches('ul,ol,li,blockquote')) t = t.lastElementChild;
  const last = t.lastElementChild;
  (last && last.matches('p,h1,h2,h3,h4,h5,h6') ? last : t).append(h('span', { class: 'caret', 'aria-hidden': 'true' }));
}
const KIND_ICON = { cmd: 'term', edit: 'edit', write: 'edit', read: 'file', search: 'search', web: 'search', ask: 'info', other: 'tool' };
function stateIcon(b, f) {
  if (f.run) return h('span', 'si run', spin('sm'));
  if (b.state === 'failed') return h('span', 'si er', icon('alert', 'sm'));
  if (b.state === 'denied' || b.state === 'cancelled') return h('span', 'si wa', icon('slash', 'sm'));
  return h('span', 'si', icon(KIND_ICON[f.kind], 'sm'));
}
function toolMeta(f) {
  return h('span', 'mt', f.added != null && (f.added || f.removed) ? [h('span', 'add', '+' + f.added), ' ', h('span', 'del', '−' + f.removed), f.dur || f.exit || f.note ? ' · ' : ''] : '',
    f.exit && h('span', 'er', f.exit), f.exit && (f.dur || f.note) ? ' · ' : '', f.note && h('span', f.note === 'failed' ? 'er' : 'wa', f.note), f.note && f.dur ? ' · ' : '', f.dur);
}
const TAIL = 20;
function toolBody(b, f) {
  const out = b.output || [], tail = out.slice(-TAIL), earlier = (b.output_dropped || 0) + out.length - tail.length;
  const input = b.input || '', files = f.kind === 'edit' && input ? MC.parseDiff(input) : [];
  const total = b.output_lines != null ? b.output_lines : out.length + (b.output_dropped || 0);
  const shown = tail.length > 0 || earlier > 0, pre = shown && h('pre', { class: 'out' + (prefs.wrap ? ' wrap' : ''), tabindex: '0' }, tail.join('\n'));
  const copy = shown && h('button', { onclick: () => copyText(out.join('\n'), copy) }, icon('copy', 'xs'), 'Copy');
  return h('div', 'tb',
    f.kind === 'cmd' && h('pre', 'cmd', '$ ' + (input || b.detail)),
    files.length > 0 ? diffView(files, prefs.wrap) : f.kind === 'write' && input && h('pre', '', input.split('\n').slice(0, 400).join('\n')),
    shown && [
      h('div', 'lbl', h('span', '', 'Output'), earlier > 0 && h('span', '', 'last ' + tail.length + ' of ' + MC.fmtNum(total) + ' lines')),
      pre,
      h('div', 'tbar', wrapButton(pre), copy, h('button', { onclick: () => openOutput(b) }, icon('open', 'xs'), 'Open'), b.truncated && h('span', 'hint', 'Output truncated'))],
    !shown && !files.length && f.kind !== 'cmd' && h('div', 'hint', f.run ? 'Running…' : 'No output'),
    !shown && f.kind === 'cmd' && !f.run && h('div', 'hint', 'No output'));
}
function toolNode(b) {
  const k = 't' + b.call_id, f = MC.toolInfo(b), o = isOpen(k, b);
  const d = decided.get(b.call_id), dec = d && (d.you ? d.text + ' · you' : b.state === 'denied' && 'Denied on another device');
  return h('div', 'tl', h('button', { class: 'tr' + (f.run ? ' run' : ''), 'data-k': k, 'aria-expanded': String(o) },
    stateIcon(b, f), h('span', 'vb', f.verb), h('span', 'tg', f.target), toolMeta(f), icon('chev', 'xs cv')),
    dec && h('div', 'dec', icon('slash', 'xs'), dec), o && toolBody(b, f));
}
function thinkNode(b, k, thinking, o) {
  const now = Date.now(), w = thinkTimes.get(k);
  if (thinking && !w) thinkTimes.set(k, { start: now });
  else if (!thinking && w && !w.end) w.end = now;
  const t = thinkTimes.get(k), secs = t && t.end ? Math.max(1, Math.round((t.end - t.start) / 1000)) : 0;
  const tk = h('button', { class: 'tk' + (thinking ? ' live' : ''), 'data-k': k, 'aria-expanded': String(o) },
    icon('sparkle', 'xs'), h('span', '', thinking ? 'Thinking…' : secs ? 'Thought for ' + secs + ' s' : 'Thought'), icon('chev', 'xs cv'));
  const line = o ? h('div', 'rsn' + (thinking ? ' live' : ''), b.reasoning) : h('span', '', MC.thinkingPreview(b.reasoning));
  const n = h('div', 'think', tk, o ? line : h('div', { class: 'tp', 'aria-hidden': 'true' }, line));
  n._text = line; n._open = o;
  return n;
}
function thinkText(n, text, live) {
  const to = n._open ? text : MC.thinkingPreview(text), el = n._text, was = el.textContent;
  if (to === was) return;
  if (n._open) {
    // The open live text grows by its new suffix and follows the end only while the reader is at it.
    const end = el.scrollHeight - el.scrollTop - el.clientHeight < 8;
    if (to.startsWith(was)) el.append(to.slice(was.length)); else el.textContent = to;
    if (end) el.scrollTop = el.scrollHeight;
    return;
  }
  el.textContent = to;
  if (live && !n._open && was && !to.startsWith(was) && !reduced.matches) { el.classList.remove('up'); void el.offsetWidth; el.classList.add('up'); }
}
function thinkUpdate(a, b) {
  const k = 'r' + b.loop_id + ':' + b.request, thinking = a.live && !b.text, o = isOpen(k, b), sig = b.reasoning ? k + thinking + o : '';
  if (sig === a.thinkSig) return sig && thinkText(a.think, b.reasoning, thinking);
  a.thinkSig = sig;
  const n = sig ? thinkNode(b, k, thinking, o) : h('div');
  keepFocus(a.think, () => a.think.replaceWith(a.think = n), n);
  if (thinking && o) n._text.scrollTop = n._text.scrollHeight;
}
function asstNode(b, i) {
  const live = i === liveIdx, n = h('div', 'a');
  n._asst = { key: b.loop_id + ':' + b.request, live, thinkSig: '', md: { cache: null, toks: [], els: [] }, body: h('div', 'md'), think: h('div') };
  n.append(n._asst.think, n._asst.body);
  updateAsst(n, b);
  if (!live && b.text) n.append(answerActs(b, i));
  return n;
}
function updateAsst(n, b) {
  const a = n._asst, m = a.md, r = MC.mdInc(m.cache, b.text);
  n.hidden = MC.quiet(b);
  thinkUpdate(a, b);
  m.cache = r.cache;
  let k = 0;
  while (k < m.toks.length && k < r.blocks.length && m.toks[k] === r.blocks[k]) k++;
  for (let j = m.els.length - 1; j >= k; j--) m.els[j].remove();
  m.els.length = m.toks.length = k;
  const old = a.body.querySelector('.caret');
  if (old) old.remove();
  for (let j = k; j < r.blocks.length; j++) { const e = mdToNode(r.blocks[j]); a.body.append(e); m.els.push(e); m.toks.push(r.blocks[j]); }
  a.body.hidden = !b.text;
  if (a.live && b.text) caretInto(a.body);
}
function answerActs(b, i) {
  const c = h('button', { class: 'ib', 'aria-label': 'Copy answer', onclick: () => copyText(b.text, c) }, icon('copy', 'sm'));
  const s = h('button', { class: 'ib', 'aria-label': 'Share answer', onclick: () => share('MiniCore answer', b.text, s) }, icon('share', 'sm'));
  let j = i + 1;
  while (j < S.blocks.length && S.blocks[j].type !== 'turn_end' && S.blocks[j].type !== 'user') j++;
  const at = S.blocks[j] && S.blocks[j].type === 'turn_end' && turnClock(j);
  return h('div', 'acts', c, s, at && h('time', '', MC.stamp(at, hostNow(), tz())));
}
function userNode(b, i) {
  const k = 'u' + i, lines = b.text.split('\n').length, long = lines > 12 || b.text.length > 900, o = isOpen(k);
  const more = h('button', { class: 'ib', 'aria-label': 'Message actions', 'aria-haspopup': 'dialog', onclick: () => userActions(b) }, icon('more', 'sm'));
  const n = h('div', 'u', b.kind === 'steering' && h('div', 'ulbl', icon('steer', 'xs'), 'Steer'),
    h('div', { class: 'bubble' + (long && !o ? ' clip' : '') }, b.text),
    h('div', 'acts', long && h('button', { class: 'more2', 'data-k': k }, o ? 'Show less' : 'Show more'),
      b.at && h('time', '', MC.stamp(b.at, hostNow(), tz())), more));
  n.addEventListener('contextmenu', e => { if (!hover.matches) { e.preventDefault(); userActions(b); } });
  return n;
}
function userActions(b) {
  sheet({
    key: 'uact', cls: 'dialog', head: hid => h('h2', { id: hid }, 'Message', b.at && h('span', 'sub2', MC.stamp(b.at, hostNow(), tz()))),
    body: m => h('div', 'list', menuRow(m, 'copy', 'Copy', () => copyText(b.text)), menuRow(m, 'edit', 'Edit in composer', () => fillComposer(b.text)),
      menuRow(m, 'retry', 'Send again', () => { if (!sending()) intent(turnStatus().busy ? MC.intents.queue(b.text) : MC.intents.submit(b.text)); })),
  });
}
function turnClock(i) {
  const b = S.blocks[i];
  for (let j = i - 1; j >= 0; j--) {
    const x = S.blocks[j];
    if (x.type === 'turn_end') return null;
    if (x.type === 'user' && x.kind !== 'steering') return x.at == null ? null : x.at + (b.duration_ms || 0);
  }
  return null;
}
// The failed turn's header with what the host said under it: the hint, the provider's own message clamped to three lines
// (its toggle shows when the text overflows, see `place`) and the button that helps.
function failureNode(head, f, i) {
  const k = 'e' + i, o = isOpen(k), copy = f.message && h('button', { class: 'fbtn', onclick: () => copyText(f.message, copy) }, icon('copy', 'xs'), 'Copy');
  const act = f.action && h('button', { class: 'btn sm', onclick: f.action === 'compact' ? () => compactNow() : openModel }, f.action === 'compact' ? 'Compact now' : 'Switch model');
  return h('div', 'fail', head, f.hint && h('p', 'fhint', f.hint),
    f.message && h('blockquote', 'fmsg' + (o ? '' : ' clip'), h('div', 'fmt', f.message)),
    (f.message || act) && h('div', 'facts', f.message && h('button', { class: 'more2', 'data-k': k, hidden: !(o || f.multiline), 'aria-expanded': String(o) }, o ? 'Show less' : 'Show full message'), copy, act));
}
function blockNode(b, i) {
  switch (b.type) {
    case 'user': return userNode(b, i);
    case 'assistant': return asstNode(b, i);
    case 'tool': return toolNode(b);
    case 'turn_end': {
      const t = MC.turnEnd(b, turnClock(i), tz()), f = t.failure;
      const head = h('div', 'te' + (t.ok ? '' : ' bad'), icon(t.ok ? 'check' : 'warn', 'xs'), h('span', 'oc', t.parts[0]), f && f.ids && h('span', 'fid', f.ids),
        t.parts.slice(1).map(p => h('span', '', '· ' + p)),
        t.changes && h('button', { class: 'chg', 'aria-label': 'Changes: ' + MC.changesText(t.changes), onclick: () => openChanges() }, icon('diff', 'xs'), MC.changesText(t.changes), icon('chev', 'xs')));
      return f ? failureNode(head, f, i) : head;
    }
    case 'compaction': {
      const k = 'c' + i, o = isOpen(k);
      return h('div', 'cmpx', h('button', { class: 'tr', 'data-k': k, disabled: !b.summary, 'aria-expanded': b.summary ? String(o) : null },
        h('span', 'si', icon('layers', 'sm')), h('span', 'vb', MC.compactionText(b)), h('span', 'tg', ''), b.summary && icon('chev', 'xs cv')), o && h('div', 'rsn', b.summary));
    }
    case 'earlier': return h('div', 'ear', loadEarlierBtn(MC.plural(b.hidden_items, 'earlier item')));
    default: return h('div', 'x');
  }
}
function renderBlock(b, i) {
  const n = blockNode(b, i), r = roles[i - lo];
  let w = n;
  if (r) {
    const f = r.f, g = r.g, fo = f && !isOpen(f.key), go = g && isOpen(g.key);
    const rows = [f && i === f.at && foldRow(f, !fo), g && i === g.head && !fo && groupRow(g, go)];
    w = h('div', n.className, rows, n, f && !fo && i === f.last && h('button', { class: 'tr fold', 'data-k': f.key + 'h', 'data-tg': f.key },
      h('span', 'si', icon('chevu', 'sm')), h('span', 'vb', 'Hide')));
    n.className = '';
    n.hidden = n.hidden || !!fo || !!(g && !go);
    w.hidden = n.hidden && !kids(rows).length && !(f && !fo && i === f.last);
  }
  w.classList.add('b');
  if (i >= liveFrom) w.classList.add('live');
  w.dataset.i = i;
  return w;
}
function foldRow(f, open) {
  return h('button', { class: 'tr fold', 'data-k': f.key, 'aria-expanded': String(open) },
    h('span', 'si', icon('layers', 'sm')), h('span', 'vb', MC.foldText(f)), h('span', 'tg', MC.plural(f.tools, 'tool')),
    toolMeta({ added: f.added, removed: f.removed, dur: '', exit: f.failed ? f.failed + ' failed' : '', note: '' }), icon('chev', 'xs cv'));
}
function groupRow(g, open) {
  const last = S.blocks[g.members[g.members.length - 1]];
  return h('button', { class: 'tr' + (g.run ? ' run' : ''), 'data-k': g.key, 'aria-expanded': String(open) },
    g.run ? h('span', 'si run', spin('sm')) : h('span', 'si', icon('search', 'sm')), h('span', 'vb', g.label), h('span', 'tg', open ? '' : last.detail), icon('chev', 'xs cv'));
}
// Fold and group roles of the mounted blocks; a block whose role changed is rendered again.
let roles = [], roleSigs = [];
function relayout() {
  const bl = S.blocks, next = [];
  for (const f of MC.foldTurns(bl, lo, hi)) for (const j of f.hidden) (next[j - lo] = next[j - lo] || {}).f = f;
  for (const g of MC.groupTools(bl, lo, hi)) for (const j of g.members) (next[j - lo] = next[j - lo] || {}).g = g;
  const ns = next.map(r => r && JSON.stringify([r.f && [r.f.key, r.f.at, r.f.last, isOpen(r.f.key), r.f.tools, r.f.failed], r.g && [r.g.key, r.g.head, r.g.label, r.g.run, isOpen(r.g.key), r.g.members.length]]));
  for (let k = 0; k < Math.max(ns.length, roleSigs.length); k++) if (ns[k] !== roleSigs[k]) dirty.add(lo + k);
  roles = next; roleSigs = ns;
}
function place(n, old) {
  if (old) keepFocus(old, () => old.replaceWith(n), n);
  for (const p of n.querySelectorAll('pre.out')) p.scrollTop = p.scrollHeight;
  for (const q of n.querySelectorAll('.fmsg.clip .fmt')) if (q.scrollHeight > q.clientHeight + 1) n.querySelector('.more2').hidden = false;
}
function flush() {
  raf = 0;
  if (!S.state) return;
  // Tool progress can change without a state message.
  renderStrip();
  // Only the block being written shows the caret.
  const live = MC.liveBlock(S.blocks, S.state.status.busy);
  if (live !== liveIdx) { for (const i of [liveIdx, live]) if (i >= 0) dirty.add(i); liveIdx = live; }
  const bl = S.blocks, n = bl.length, lo0 = lo, prevHi = hi;
  liveFrom = S.state.status.busy ? bl.findLastIndex(b => b.type === 'turn_end') + 1 : Infinity;
  const wasStick = stick && pinned, fromBottom = log.scrollHeight - log.scrollTop, h0 = log.scrollHeight;
  const grew = n > prevHi || dirty.has(n - 1);
  const serverPrepend = n > prevHi && dirty.has(0) && prevHi > 0;
  // The mounted window [lo, hi): the newest `win` blocks while pinned to the end.
  const newHi = pinned ? n : Math.min(hi, n), newLo = Math.max(0, newHi - win);
  while (lo + nodes.length > newHi) nodes.pop().remove();
  while (lo < newLo && nodes.length) { nodes.shift().remove(); lo++; }
  if (!nodes.length) lo = newLo;
  if (lo > newLo) {
    const fr = document.createDocumentFragment(), add = [];
    for (let k = newLo; k < lo; k++) { const x = renderBlock(bl[k], k); add.push(x); fr.append(x); dirty.delete(k); }
    nodes.unshift(...add); box.prepend(fr); lo = newLo;
    for (const x of add) place(x);
  }
  hi = newHi;
  if (lo !== lo0) roleSigs = [];
  relayout();
  for (let i = lo; i < hi; i++) {
    if (!dirty.has(i) && nodes[i - lo]) continue;
    const old = nodes[i - lo], b = bl[i];
    if (old && old._asst && !roles[i - lo] && b.type === 'assistant' && old._asst.key === b.loop_id + ':' + b.request && old._asst.live === (i === liveIdx)) { updateAsst(old, b); continue; }
    const x = renderBlock(b, i);
    if (!old) box.append(x);
    place(x, old);
    nodes[i - lo] = x;
  }
  dirty.clear();
  renderTop(); renderEmpty();
  const topDelta = lo === lo0 ? 0 : log.scrollHeight - h0;
  const a = anchor && anchor.k && box.querySelector('[data-k="' + anchor.k + '"]');
  if (a) log.scrollTop += a.getBoundingClientRect().top - anchor.y;
  else if (serverPrepend && !wasStick) log.scrollTop = log.scrollHeight - fromBottom;
  else if (wasStick && !anchor) { log.scrollTop = log.scrollHeight; newOut(false); }
  else { log.scrollTop += topDelta; if (grew && !anchor && !wasStick) newOut(true); }
  // Newer blocks are unmounted while reading far back: the pill leads back to them.
  if (!pinned) newOut(true);
  anchor = null;
}
function newOut(on) { set($('newout'), 'hidden', !on); }
function loadEarlierBtn(label) {
  const busy = pendingKind('load_older');
  return h('button', { class: 'pill', disabled: busy, 'data-k': 'earlier', onclick: e => loadEarlier(e.currentTarget) }, busy ? spin('sm') : icon('history', 'sm'), busy ? 'Loading…' : 'Load earlier' + (label ? ' · ' + label : ''));
}
function loadEarlier(el) {
  if (lo > 0) {
    win += MC.WINDOW;
    // The window stays bounded: past the cap the newest blocks are unmounted until the user returns to them.
    if (win > MAX_MOUNT) { hi -= win - MAX_MOUNT; win = MAX_MOUNT; pinned = false; }
    anchorTo(el); schedule();
  } else if (!pendingKind('load_older')) intent(MC.intents.loadOlder());
}
function toLatest() {
  const was = pinned;
  pinned = true; win = MC.WINDOW; stick = true;
  if (!was) schedule();
  log.scrollTo({ top: log.scrollHeight, behavior: reduced.matches || !was ? 'auto' : 'smooth' });
  newOut(false);
}
const topPart = part($('top'), x => x && loadEarlierBtn(x[0]));
export function renderTop() {
  const first = S.blocks[lo], older = lo > 0 || (S.state.older && !(first && first.type === 'earlier'));
  topPart(older && [lo > 0 ? MC.plural(lo, 'block') : '', pendingKind('load_older')]);
}
const SUGG = [['Explain this project', 'Structure, entry points and how it fits together'], ['Run the tests and fix failures', 'Find what breaks and repair it'],
  ['Review my uncommitted changes', 'Spot bugs and risky edits'], ['Find TODOs', 'List open TODO and FIXME notes']];
const emptyPart = part($('empty'), e => e.loading ? h('div', 'loadrow', spin(), 'Opening session…') : e.session ? h('div', 'muted', 'No messages yet.') : [
  h('img', { class: 'appicon', src: '/icon.svg', alt: '' }), h('h2', '', 'What should we work on?'),
  h('button', { class: 'where', 'aria-label': 'Change folder, ' + e.ws, onclick: () => openNew() }, icon('folder', 'sm'), h('span', '', MC.basename(e.ws)), icon('chevd', 'xs')),
  h('div', 'hint mt6', MC.tilde(e.ws)),
  h('div', 'sugg', SUGG.map(([t, d]) => h('button', { onclick: () => fillComposer(t) }, t, h('small', '', d))))]);
export function renderEmpty() {
  const s = S.state, e = $('empty');
  set(e, 'hidden', S.blocks.length > 0);
  if (!e.hidden) emptyPart({ loading: s.status.loading, session: !!s.session, ws: s.draft.workspace });
}
log.addEventListener('scroll', () => { stick = atBottom(); if (stick && pinned) newOut(false); }, { passive: true });
$('newout').addEventListener('click', toLatest);

export function markDirty(indexes) { for (const i of indexes) dirty.add(i); schedule(); }
// A denied tool row says who denied it: this device with the reason it gave, or, without `text`, another one.
const decided = new Map();
function markDenied(call, text) {
  if (text == null && decided.has(call)) return;
  decided.set(call, text == null ? { you: false } : { you: true, text });
  const i = S.blocks.findIndex(b => b.type === 'tool' && b.call_id === call);
  if (i >= 0) markDirty([i]);
}
// Find in the loaded transcript: matches are highlighted in place (CSS highlights), folds holding one open.
const finder = { on: false, q: '', ranges: [], k: 0 };
let findTimer = 0;
const findPart = part($('findbar'), on => on && findBar());
function renderFind() { findPart(finder.on && [finder.q, finder.ranges.length, finder.k]); }
export function openFind() {
  finder.on = true;
  renderFind();
  const i = $('findq');
  i.focus(); i.select();
}
function closeFind() {
  finder.on = false; finder.q = ''; finder.ranges = [];
  if (window.CSS && CSS.highlights) { CSS.highlights.delete('find'); CSS.highlights.delete('findc'); }
  renderFind();
}
function findBar() {
  const n = finder.ranges.length;
  return h('div', { class: 'banner find', role: 'search' }, icon('search', 'sm'),
    h('input', { id: 'findq', type: 'search', class: 'fq', value: finder.q, placeholder: 'Find in session', 'aria-label': 'Find in session', 'data-k': 'findq', autocomplete: 'off', enterKeyHint: 'search',
      oninput: e => { finder.q = e.target.value; clearTimeout(findTimer); findTimer = setTimeout(runFind, 150); },
      onkeydown: e => { if (e.key === 'Enter') { e.preventDefault(); findStep(e.shiftKey ? -1 : 1); } else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closeFind(); } } }),
    h('span', { class: 'fc', 'aria-live': 'polite' }, finder.q ? (n ? finder.k + 1 + '/' + n : 'No matches') : ''),
    h('button', { class: 'ib s', 'aria-label': 'Previous match', disabled: !n, onclick: () => findStep(-1) }, icon('chevu', 'sm')),
    h('button', { class: 'ib s', 'aria-label': 'Next match', disabled: !n, onclick: () => findStep(1) }, icon('chevd', 'sm')),
    h('button', { class: 'ib s', 'aria-label': 'Close find', onclick: closeFind }, icon('x', 'sm')));
}
function runFind() {
  for (const i of MC.findBlocks(S.blocks, finder.q, lo, hi)) {
    const r = roles[i - lo], b = S.blocks[i];
    if (r && r.f) opened.set(r.f.key, true);
    if (r && r.g) opened.set(r.g.key, true);
    if (b.type === 'tool' && !b.detail.toLowerCase().includes(finder.q.toLowerCase())) opened.set('t' + b.call_id, true);
    dirty.add(i);
  }
  flush();
  const q = finder.q.toLowerCase(), ranges = [];
  if (q) {
    const w = document.createTreeWalker(box, NodeFilter.SHOW_TEXT, { acceptNode: t => t.parentElement.closest('[hidden],.tp') ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT });
    while (w.nextNode()) {
      const t = w.currentNode, s = t.data.toLowerCase();
      for (let k = s.indexOf(q); k >= 0; k = s.indexOf(q, k + q.length)) { const r = new Range(); r.setStart(t, k); r.setEnd(t, k + q.length); ranges.push(r); }
    }
  }
  finder.ranges = ranges; finder.k = Math.max(0, ranges.length - 1);
  paintFind();
}
function findStep(d) { if (finder.ranges.length) { finder.k = (finder.k + d + finder.ranges.length) % finder.ranges.length; paintFind(); } }
function paintFind() {
  const r = finder.ranges[finder.k];
  if (window.CSS && CSS.highlights) {
    CSS.highlights.set('find', new Highlight(...finder.ranges));
    if (r) CSS.highlights.set('findc', new Highlight(r)); else CSS.highlights.delete('findc');
  }
  if (r) { const y = r.getBoundingClientRect().top - log.getBoundingClientRect().top; log.scrollTop += y - log.clientHeight / 3; }
  renderFind();
}
export function followEnd() { if (stick && pinned) log.scrollTop = log.scrollHeight; }
actions.find = openFind;
actions.redraw = redrawAll;

// ---- approvals and questions ----
let apprId = null, apprAt = 0, lastKey = 0;
export function syncApproval() {
  const a = S.state.interaction, m = findSheet('appr');
  if (!a) {
    // Answered elsewhere: a denied tool row says so.
    if (apprId && reply.call) markDenied(reply.call);
    apprId = null; reply.call = null;
    if (m) m.close(true);
    renderApprBar();
    return;
  }
  if (a.id !== apprId) {
    apprId = a.id; apprAt = Date.now();
    const t = [...S.blocks].reverse().find(b => b.type === 'tool' && b.state === 'pending');
    reply.call = t ? t.call_id : null;
    announce(a.kind === 'approval' ? 'Approval needed' : 'Question: ' + (a.prompt || ''));
    notify(a.kind === 'approval' ? 'Approval needed: ' + MC.approvalLine(a) : a.prompt || 'A question is waiting', S.sid);
    if (!m) openApproval(a);
  }
  renderApprBar();
}
// The sheet's own state: the denial form being filled in and the tool row the question belongs to.
const reply = { deny: false, reason: '', call: null };
function openApproval() {
  reply.deny = false; reply.reason = '';
  const m = sheet({
    key: 'appr', closeIcon: 'chevd', closeLabel: 'Minimize', footCls: 'col', deps: () => [S.state.interaction, pendingKind('answer'), online(), reply.deny],
    head: hid => {
      const a = S.state.interaction, t = a.kind === 'approval' && MC.approvalTitle(a);
      return h('div', 'ahead', h('div', 'eicon', icon(t ? 'shield' : 'info')), h('div', '',
        h('h2', { id: hid, tabindex: '-1', 'data-focus': true }, t ? [t.text, t.code ? [' ', h('code', '', t.code)] : ''] : a.prompt || 'Question'),
        t && a.prompt && h('p', '', a.prompt)));
    },
    body: () => {
      const a = S.state.interaction, p = MC.pending(S, 'answer');
      if (a.kind !== 'approval') return h('div', 'opts', MC.approvalOptions(a).map(o => h('button', { class: 'btn', 'data-k': 'o' + o.n, disabled: !online() || !!p, onclick: () => answer(o.choice) },
        p && p.spec.choice === o.choice ? spin() : h('b', '', String(o.n)), h('span', '', o.label))));
      const x = MC.approvalBody(a);
      return [x.cmd && [h('pre', 'cmdbox', '$ ' + x.cmd), h('div', 'where2', 'in ' + MC.tilde(viewed().workspace))], x.diff && diffView(x.diff, prefs.wrap), x.text && h('pre', 'cmdbox', x.text)];
    },
    foot: () => {
      const a = S.state.interaction, p = MC.pending(S, 'answer'), dis = !online() || !!p;
      if (a.kind !== 'approval') return hover.matches && h('div', 'akeys', 'Press 1–' + a.options.length + ' to answer');
      const ans = (o, cls, fn) => h('button', { class: 'btn ' + cls, 'data-k': o.choice, disabled: dis, onclick: fn || (() => answer(o.choice)) }, p && p.spec.choice === o.choice && spin(), o.label);
      const [allow, session, deny] = MC.approvalOptions(a);
      if (reply.deny) return [h('form', { class: 'reason', onsubmit: e => { e.preventDefault(); answer('deny', reply.reason.trim()); } },
        h('input', { class: 'tf', id: 'why', 'data-k': 'why', value: reply.reason, placeholder: 'Tell the agent why (optional)', 'aria-label': 'Reason for denying', autocomplete: 'off', enterKeyHint: 'send', oninput: e => { reply.reason = e.target.value; } })),
        h('div', 'row2', h('button', { class: 'btn', onclick: () => { reply.deny = false; m.refresh(); } }, 'Back'), ans(deny, 'dangerfill', () => answer('deny', reply.reason.trim())))];
      return [ans(allow, 'primary'), session && ans(session, ''), ans({ ...deny, label: deny.label + '…' }, 'danger', () => { reply.deny = true; m.refresh(); m.el.querySelector('#why').focus(); }),
        hover.matches && h('div', 'akeys', 'Y allow · A allow for this session · N deny')];
    },
    onClose: () => renderApprBar(),
  });
}
// Only a denial is recorded on its tool row, with the reason: it explains why the tool did not run.
function answer(choice, message) {
  const a = S.state.interaction, call = reply.call;
  if (!a || pendingKind('answer')) return;
  intent(MC.intents.answer(a.id, choice, message || undefined), ack => {
    if (!ack.ok) return toast(MC.errorText(ack.error), ack.error === 'stale' ? null : 'error');
    if (call && choice === 'deny') markDenied(call, 'Denied' + (message ? ': “' + message + '”' : ''));
  });
}
const apprPart = part($('apprbar'), b => b && [icon('hand', 'sm'), h('span', '', b), h('button', { class: 'btn sm', onclick: () => openApproval() }, 'Review')]);
function renderApprBar() {
  const a = S.state && S.state.interaction, show = !!a && !findSheet('appr');
  set($('apprbar'), 'hidden', !show);
  apprPart(show ? (a.kind === 'approval' ? 'Approval needed · ' + MC.approvalLine(a) : 'Question · ' + (a.prompt || '')) : null);
}
document.addEventListener('keydown', e => {
  const m = topModal(), a = S.state && S.state.interaction, armedFrom = Math.max(apprAt, lastKey);
  lastKey = Date.now();
  if (!a || !m || m.key !== 'appr' || e.repeat) return;
  const c = MC.approvalKey(a, e.key, e.ctrlKey || e.metaKey || e.altKey, armedFrom, lastKey, inField(e.target));
  if (c != null) { e.preventDefault(); answer(c); }
}, true);
