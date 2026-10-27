import * as MC from './core.js';
import { mdInc } from './markdown.js';
import { fillComposer, loadDraft } from './composer.js';
import { S, hostNow, intent, pendingKind, sending, st, tz } from './net.js';
import { prefs } from './platform.js';
import { openNew } from './sessions.js';
import { openChanges, openOutput } from './sheets.js';
import { $, actions, copyText, diffView, fill, h, hover, icon, keepFocus, kids, menuRow, part, reduced, share, sheet, spin } from './ui.js';

// ---- transcript ----
// `opened` holds the rows the reader opened or closed by key; a key not in it starts as `startsOpen` says.
const log = $('log'), box = $('blocks'), nodes = [], dirty = new Set(), opened = new Map(), thinkTimes = new Map();
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
  return h('div', 'tl', h('button', { class: 'tr', 'data-k': k, 'aria-expanded': String(o) },
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
  const a = n._asst, m = a.md, r = mdInc(m.cache, b.text);
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
      menuRow(m, 'retry', 'Send again', () => { if (!sending()) intent(st().busy ? MC.intents.queue(b.text) : MC.intents.submit(b.text)); })),
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
function blockNode(b, i) {
  switch (b.type) {
    case 'user': return userNode(b, i);
    case 'assistant': return asstNode(b, i);
    case 'tool': return toolNode(b);
    case 'turn_end': {
      const t = MC.turnEnd(b, turnClock(i), tz());
      return h('div', 'te' + (t.ok ? '' : ' bad'), icon(t.ok ? 'check' : 'warn', 'xs'), h('span', 'oc', t.parts[0]), t.parts.slice(1).map(p => h('span', '', '· ' + p)),
        t.changes && h('button', { class: 'chg', 'aria-label': 'Changes: ' + MC.changesText(t.changes), onclick: () => openChanges() }, icon('diff', 'xs'), MC.changesText(t.changes), icon('chev', 'xs')));
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
  return h('button', { class: 'tr', 'data-k': g.key, 'aria-expanded': String(open) },
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
}
function flush() {
  raf = 0;
  if (!S.state) return;
  // Only the block being written shows the caret.
  const live = MC.liveBlock(S.blocks, S.state.status.busy);
  if (live !== liveIdx) { for (const i of [liveIdx, live]) if (i >= 0) dirty.add(i); liveIdx = live; }
  const bl = S.blocks, n = bl.length, lo0 = lo, prevHi = hi;
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
function newOut(on) { $('newout').hidden = !on; }
function loadEarlierBtn(label) {
  const busy = pendingKind('load_older');
  return h('button', { class: 'pill', disabled: busy, 'data-k': 'earlier', onclick: e => loadEarlier(e.currentTarget) }, busy ? spin('sm') : icon('history', 'sm'), busy ? 'Loading…' : 'Load earlier' + (label ? ' · ' + label : ''));
}
// Mount more of what this page has, else ask the host for an older page.
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
export function renderTop() {
  const first = S.blocks[lo], older = lo > 0 || (S.state.older && !(first && first.type === 'earlier'));
  fill($('top'), older && loadEarlierBtn(lo > 0 ? MC.plural(lo, 'block') : ''));
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
  e.hidden = S.blocks.length > 0;
  if (!e.hidden) emptyPart({ loading: s.status.loading, session: !!s.session, ws: s.draft.workspace });
}
log.addEventListener('scroll', () => { stick = atBottom(); if (stick && pinned) newOut(false); }, { passive: true });
$('newout').addEventListener('click', toLatest);

export const decided = new Map();
export function markCall(call) { const i = S.blocks.findIndex(b => b.type === 'tool' && b.call_id === call); if (i >= 0) { dirty.add(i); schedule(); } }
// Find in the loaded transcript: matches are highlighted in place (CSS highlights), folds holding one open.
const fd = { on: false, q: '', ranges: [], k: 0 };
let findT = 0;
const findPart = part($('findbar'), on => on && findBar());
function renderFind() { findPart(fd.on && [fd.q, fd.ranges.length, fd.k]); }
function openFind() {
  fd.on = true;
  renderFind();
  const i = $('findq');
  i.focus(); i.select();
}
function closeFind() {
  fd.on = false; fd.q = ''; fd.ranges = [];
  if (window.CSS && CSS.highlights) { CSS.highlights.delete('find'); CSS.highlights.delete('findc'); }
  renderFind();
}
function findBar() {
  const n = fd.ranges.length;
  return h('div', { class: 'banner find', role: 'search' }, icon('search', 'sm'),
    h('input', { id: 'findq', type: 'search', class: 'fq', value: fd.q, placeholder: 'Find in session', 'aria-label': 'Find in session', 'data-k': 'findq', autocomplete: 'off', enterKeyHint: 'search',
      oninput: e => { fd.q = e.target.value; clearTimeout(findT); findT = setTimeout(runFind, 150); },
      onkeydown: e => { if (e.key === 'Enter') { e.preventDefault(); findStep(e.shiftKey ? -1 : 1); } else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closeFind(); } } }),
    h('span', { class: 'fc', 'aria-live': 'polite' }, fd.q ? (n ? fd.k + 1 + '/' + n : 'No matches') : ''),
    h('button', { class: 'ib s', 'aria-label': 'Previous match', disabled: !n, onclick: () => findStep(-1) }, icon('chevu', 'sm')),
    h('button', { class: 'ib s', 'aria-label': 'Next match', disabled: !n, onclick: () => findStep(1) }, icon('chevd', 'sm')),
    h('button', { class: 'ib s', 'aria-label': 'Close find', onclick: closeFind }, icon('x', 'sm')));
}
function runFind() {
  for (const i of MC.findBlocks(S.blocks, fd.q, lo, hi)) {
    const r = roles[i - lo], b = S.blocks[i];
    if (r && r.f) opened.set(r.f.key, true);
    if (r && r.g) opened.set(r.g.key, true);
    if (b.type === 'tool' && !b.detail.toLowerCase().includes(fd.q.toLowerCase())) opened.set('t' + b.call_id, true);
    dirty.add(i);
  }
  flush();
  const q = fd.q.toLowerCase(), ranges = [];
  if (q) {
    const w = document.createTreeWalker(box, NodeFilter.SHOW_TEXT, { acceptNode: t => t.parentElement.closest('[hidden],.tp') ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT });
    while (w.nextNode()) {
      const t = w.currentNode, s = t.data.toLowerCase();
      for (let k = s.indexOf(q); k >= 0; k = s.indexOf(q, k + q.length)) { const r = new Range(); r.setStart(t, k); r.setEnd(t, k + q.length); ranges.push(r); }
    }
  }
  fd.ranges = ranges; fd.k = Math.max(0, ranges.length - 1);
  paintFind();
}
function findStep(d) { if (fd.ranges.length) { fd.k = (fd.k + d + fd.ranges.length) % fd.ranges.length; paintFind(); } }
function paintFind() {
  const r = fd.ranges[fd.k];
  if (window.CSS && CSS.highlights) {
    CSS.highlights.set('find', new Highlight(...fd.ranges));
    if (r) CSS.highlights.set('findc', new Highlight(r)); else CSS.highlights.delete('findc');
  }
  if (r) { const y = r.getBoundingClientRect().top - log.getBoundingClientRect().top; log.scrollTop += y - log.clientHeight / 3; }
  renderFind();
}
export function followEnd() { if (stick && pinned) log.scrollTop = log.scrollHeight; }
actions.find = openFind;
actions.redraw = redrawAll;

export { dirty };
