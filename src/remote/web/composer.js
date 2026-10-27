import * as MC from './core.js';
import { HOST, S, ask, cur, hostNow, intent, online, pendingKind, repaint, send, sending, st } from './net.js';
import { prefs } from './platform.js';
import { openInfo, openNav, openNew, rename } from './sessions.js';
import { compactNow, exportSession, openChanges, openContext, openHelp, openModel, openSettings } from './sheets.js';
import { $, actions, announce, copyText, h, hover, icon, menuRow, part, set, sheet, spin, toast, wide } from './ui.js';

// "Enter sends" is stored once chosen; until then the pointer default follows the device.
const apple = /Mac|iPhone|iPad/.test(navigator.userAgent), ALT = apple ? '⌥' : 'Alt+', tips = () => prefs.enter ? 'Enter' : (apple ? '⌘' : 'Ctrl+') + 'Enter';
// ---- dock: status strip, queue, composer ----
const ta = $('ta');
const stripPart = part($('strip'), x => [spin(), h('div', 'stx', h('div', 'st1', h('b', '', x.label), x.started && h('span', 'el', MC.fmtElapsed(hostNow() - x.started)), x.detail && h('span', 'mono', x.detail))),
  x.ctx && h('button', { class: 'ctxb' + (x.hi ? ' hi' : ''), 'aria-label': x.ctx + ' of context used', onclick: () => openContext() }, x.ctx)]);
function renderStrip() {
  const s = st(), on = !!(s.busy || s.compacting || s.loading);
  set($('strip'), 'hidden', !on);
  if (!on) return;
  const x = MC.statusLine(S.state, S.blocks, hostNow());
  stripPart({
    label: x.label, detail: x.detail, started: (s.busy || s.compacting) && s.started_ms, ctx: S.state.context_percent != null ? Math.round(S.state.context_percent) + '%' : '',
    hi: S.state.context_percent >= 80,
  });
}
export function stop() { if (MC.canStop(st()) && !st().interrupting && !pendingKind('cancel')) intent(MC.intents.cancel()); }
const trayPart = part($('tray'), t => [
  t.lost.map(([id, text]) => h('div', { class: 'lost', role: 'alert' }, icon('warn', 'sm'),
    h('div', '', h('div', '', 'Not sent · it may or may not have arrived'), h('div', 'lt', text)),
    h('button', { class: 'btn sm', 'data-k': 'again' + id, disabled: !t.on, onclick: () => { send(MC.sendAgain(S, id)); repaint(); } }, icon('retry', 'xs'), 'Send again'),
    h('button', { class: 'ib s', 'aria-label': 'Edit instead', onclick: () => { S.intents.delete(id); fillComposer(MC.mergeReturned(ta.value, text)); repaint(); } }, icon('edit', 'sm')))),
  t.q.follows && h('div', 'qcap', 'Follow-ups send when this turn ends'),
  t.q.items.map(q => q.kind === 'steer' ? h('div', 'qp steer', icon('steer', 'xs'), h('span', 'qk', 'Steer · waiting for the next step'), h('span', 'qt', q.text))
    : h('div', 'qp', icon('follow', 'xs'), h('span', 'qk', 'Follow-up ' + q.n), h('button', { class: 'qt', 'data-k': 'q' + q.index, disabled: !t.on, 'aria-label': 'Edit follow-up ' + q.n + ': ' + q.text, onclick: () => unqueue(q, false) }, q.text),
      h('button', { class: 'ib', disabled: !t.on, 'aria-label': 'Remove follow-up ' + q.n, onclick: () => unqueue(q, true) }, icon('x', 'sm')))),
  t.q.long && h('button', { class: 'qmore', 'data-k': 'qmore', 'aria-expanded': String(qAll), onclick: () => { qAll = !qAll; renderTray(); } }, t.q.hidden ? '+' + t.q.hidden + ' more' : 'Show less'),
]);
let qAll = false;
// A queued follow-up comes back from the host: into the composer to edit it, or dropped.
export const edits = new Set();
function unqueue(q, drop) {
  if (!online()) return;
  if (!drop) edits.add(q.text);
  send(MC.unqueue(S, q.index, q.text, drop));
  repaint();
}
function renderTray() {
  const q = MC.queueView(S.state.queue, qAll);
  trayPart({ lost: MC.unconfirmed(S).map(it => [it.id, it.text]), q: { ...q, follows: S.state.queue.some(x => x.kind === 'follow') }, on: online() });
}
// The bottom row of the composer: + and the draft's folder and profile on the left, then the slot (the model as plain text,
// or Follow-up while a turn runs with text typed) and the primary button, which keeps its place and changes face.
const PRIMARY = {
  send: { icon: 'up', label: 'Send' }, steer: { icon: 'steer', label: 'Steer' },
  stop: { icon: 'stop', label: 'Stop the turn' }, stopping: { icon: null, label: 'Stopping' },
};
let primary = '', stopAt = 0, press = null;
const composerState = () => {
  const s = st(), lock = sending();
  return { busy: !!s.busy, compacting: !!s.compacting, text: ta.value, stopping: MC.canStop(s) && (!!s.interrupting || pendingKind('cancel')), lock };
};
const chipsPart = part($('chips'), c => [h('div', 'cl',
  h('button', { class: 'chip icon', 'data-k': 'plus', 'aria-label': 'Commands, prompts and files', 'aria-haspopup': 'dialog', onclick: openPlus }, icon('plus', 'sm')),
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
  const s = st(), c = cur(), d = S.state.draft, cs = composerState(), kind = MC.primaryKind(cs), typed = cs.text.trim() !== '';
  set(ta, 'readOnly', cs.lock);
  set(ta, 'placeholder', s.busy ? (hover.matches ? tips() + ' to steer · ' + ALT + 'Enter to queue' : 'Steer, or queue a follow-up…') : S.state.session ? 'Message MiniCore · / for commands' : 'Describe a task to start');
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
export function loadDraft() { if (!sending()) { ta.value = localStorage.getItem(draftKey()) || ''; composerInput(); } }
export function fillComposer(text) { ta.value = text; recall = -1; composerInput(); ta.focus(); ta.setSelectionRange(text.length, text.length); }
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
    case 'find': return actions.find();
    case 'session': return openInfo();
    case 'sessions': return openNav();
    case 'copy': { const b = [...S.blocks].reverse().find(x => x.type === 'assistant' && x.text); return b ? copyText(b.text) : toast('No answer to copy yet'); }
    case 'settings': return openSettings();
    case 'help': return openHelp();
  }
}
ta.addEventListener('input', () => { recall = -1; composerInput(); });
ta.addEventListener('click', updateMenu);

// The / command and @ file list above the composer; keyboard and touch navigable.
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
  if (e.isComposing) return;
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
  const act = MC.composerKey({ key: e.key, shift: e.shiftKey, alt: e.altKey, mod: e.metaKey || e.ctrlKey }, { busy: !!st().busy, text: ta.value, popup: !!(menu && menu.items.length), enter: prefs.enter });
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


// Back from the host: an acknowledged send leaves the editor, a returned message comes back into it.
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
