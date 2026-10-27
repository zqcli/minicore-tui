const NS = 'http://www.w3.org/2000/svg';
// Late-bound calls upward in the import graph: the module that defines one sets it, callers below it use it here.
export const actions = {};

const $ = id => document.getElementById(id);
// Children are nodes and non-empty strings only: 0, false and null never render.
export const kids = k => k.flat(Infinity).filter(x => typeof x === 'string' ? x !== '' : x instanceof Node);
export function h(tag, p, ...c) {
  const n = document.createElement(tag);
  if (tag === 'button') n.type = 'button';
  if (typeof p === 'string') n.className = p;
  else if (p) for (const k in p) {
    const v = p[k];
    if (v == null || v === false) continue;
    if (k === 'class') n.className = v;
    else if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
    else if (k in n && !k.includes('-') && k !== 'spellcheck' && k !== 'list') n[k] = v;
    else n.setAttribute(k, v === true ? '' : v);
  }
  n.append(...kids(c));
  return n;
}
export function icon(name, cls) {
  const s = document.createElementNS(NS, 'svg'), u = document.createElementNS(NS, 'use');
  s.setAttribute('class', 'ic' + (cls ? ' ' + cls : ''));
  s.setAttribute('aria-hidden', 'true');
  u.setAttribute('href', '#i-' + name);
  s.append(u);
  return s;
}
export const fill = (n, ...k) => n.replaceChildren(...kids(k));
export const spin = cls => h('span', 'spin' + (cls ? ' ' + cls : ''));
const mq = q => matchMedia(q);
export const wide = mq('(min-width: 900px)'), hover = mq('(hover: hover)'), reduced = mq('(prefers-reduced-motion: reduce)');
const panelWide = mq('(min-width: 1200px)');
export const inField = n => !!(n && n.closest && n.closest('input,textarea,[contenteditable=true]'));
// A region that re-renders only when its input changes; focus inside it survives through data-k.
export function part(host, render) {
  let last;
  return input => {
    const k = JSON.stringify(input);
    if (k !== last) { last = k; swap(host, render(input)); }
  };
}
// Runs `change`, which replaces what is inside `host`; a focused [data-k] element comes back in `into`.
export function keepFocus(host, change, into = host) {
  const a = document.activeElement, k = host.contains(a) && a.dataset.k;
  change();
  const n = k && into.querySelector('[data-k="' + k + '"]');
  if (n) n.focus({ preventScroll: true });
}
const swap = (host, nodes) => keepFocus(host, () => fill(host, nodes));
export const errBox = (text, cls, alert = true) => h('div', { class: 'serr' + (cls ? ' ' + cls : ''), role: alert && 'alert' }, icon('warn', 'sm'), h('span', '', text));
export const rowText = (label, sub, mono) => h('span', 'rt', h('span', 'rm' + (mono ? ' mono' : ''), label), sub && h('span', 'rs', sub));
// A row of a menu sheet: it closes the sheet first, then acts.
export const menuRow = (m, name, label, fn, o = {}) => h('button', { class: 'row flat' + (o.cls ? ' ' + o.cls : ''), disabled: o.disabled, 'data-focus': o.focus, onclick: () => { m.close(true); fn(); } },
  icon(name, 'ri'), rowText(label, o.sub, o.mono));

// ---- copy, share, toasts, announcements ----
// navigator.clipboard is missing on plain-http LAN pages.
export function copyText(text, btn) {
  // The button shows a check; a toast only where no button is left to show it.
  const done = () => {
    if (!btn) return toast('Copied');
    const u = btn.querySelector('use'), was = u.getAttribute('href');
    u.setAttribute('href', '#i-check');
    btn.classList.add('done');
    setTimeout(() => { u.setAttribute('href', was); btn.classList.remove('done'); }, 1500);
  };
  const legacy = () => {
    const t = h('textarea', { class: 'sink', value: text, readOnly: true });
    document.body.append(t);
    t.select(); t.setSelectionRange(0, text.length);
    let ok = false;
    try { ok = document.execCommand('copy'); } catch { ok = false; }
    t.remove();
    ok ? done() : toast("Couldn't copy on this page", 'error');
  };
  if (navigator.clipboard && window.isSecureContext) navigator.clipboard.writeText(text).then(done, legacy); else legacy();
}
export function share(title, text, btn) {
  if (navigator.share) navigator.share({ title, text }).catch(() => {}); else copyText(text, btn);
}
// Errors stay until dismissed; an action ("Open") removes the toast and runs.
export function toast(text, level, act) {
  const box = $('toasts'), sticky = level === 'error';
  const n = h('div', { class: 'toast' + (sticky ? ' error' : ''), role: sticky ? 'alert' : 'status' }, sticky && icon('warn', 'sm'), h('span', '', text),
    act && h('button', { class: 'tact', onclick: () => { n.remove(); act.fn(); } }, act.label),
    sticky && h('button', { class: 'ib', 'aria-label': 'Dismiss', onclick: () => n.remove() }, icon('x', 'sm')));
  box.append(n);
  while (box.children.length > 3) box.firstChild.remove();
  if (!sticky) setTimeout(() => n.remove(), act ? 6000 : 3000);
}
export const announce = text => { $('live').textContent = text; };

// ---- modal manager: the drawer and every sheet ----
// Focus moves in and back, everything below is inert, Esc and swipe down close.
export const stack = [];
export function pushModal(m) {
  m.opener = document.activeElement;
  stack.push(m);
  layout();
  requestAnimationFrame(() => {
    const f = m.el.querySelector('[data-focus]');
    (f && (hover.matches || m.focusField) ? f : m.el).focus({ preventScroll: true });
  });
}
export function popModal(m) {
  const i = stack.indexOf(m);
  if (i < 0) return;
  stack.splice(i, 1);
  layout();
  if (m.opener && m.opener.isConnected && !m.opener.closest('[inert]')) m.opener.focus({ preventScroll: true });
}
export function layout() {
  const modal = stack.filter(m => !m.panel), top = modal[modal.length - 1], nav = $('nav');
  $('main').inert = !!top;
  nav.inert = wide.matches ? !!top : !top || top.el !== nav;
  stack.forEach((m, k) => { m.el.inert = !!top && m !== top; m.el.style.zIndex = m.el === nav ? 20 : 30 + 2 * k; });
  const scrim = $('scrim');
  scrim.classList.toggle('on', !!top);
  if (top) scrim.style.zIndex = top.el === nav ? 19 : 29 + 2 * stack.indexOf(top);
  document.documentElement.classList.toggle('haspanel', stack.some(m => m.panel));
}
export const topModal = () => stack.filter(m => !m.panel).pop();
export function closeAll() { for (const m of [...stack].reverse()) m.close(); }
$('scrim').addEventListener('click', () => { const m = topModal(); if (m) m.close(); });

let sid = 0;
// A sheet: persistent header, body and footer re-rendered when deps() changes. cls: '' | 'full' | 'dialog';
// `side` sheets become a non-modal right-hand panel from 1200 px.
export function sheet(o) {
  const old = stack.find(m => m.key === o.key);
  if (old) old.close(true);
  const id = 'sh' + ++sid, el = h('section', { class: 'sheet ' + (o.cls || ''), role: 'dialog', 'aria-modal': 'true', 'aria-labelledby': id, tabindex: '-1' });
  const head = h('header', 'shead'), body = h('div', 'sbody'), foot = h('div', 'sfoot' + (o.footCls ? ' ' + o.footCls : ''));
  const m = { key: o.key, el, last: undefined, focusField: o.focusField };
  m.panel = !!o.side && panelWide.matches;
  if (m.panel) { el.classList.add('panel'); el.setAttribute('aria-modal', 'false'); for (const p of stack.filter(x => x.panel)) p.close(true); }
  m.refresh = force => {
    const k = JSON.stringify(o.deps ? o.deps() : 0);
    if (k === m.last && !force) return;
    m.last = k;
    const top = body.scrollTop;
    fill(head, o.head ? o.head(id) : h('h2', { id }, o.title), h('button', { class: 'ib', 'aria-label': o.closeLabel || 'Close', onclick: () => m.close() }, icon(o.closeIcon || 'x')));
    swap(body, o.body(m));
    const f = o.foot && o.foot();
    foot.hidden = !f;
    if (f) swap(foot, f);
    body.scrollTop = top;
    if (o.after) o.after(body);
  };
  m.close = quiet => {
    if (!stack.includes(m)) return;
    popModal(m);
    if (quiet || reduced.matches || m.panel) el.remove();
    else { el.classList.add('out'); setTimeout(() => el.remove(), 180); }
    if (o.onClose) o.onClose(quiet);
  };
  el.append(h('div', 'grab'), head, body, foot);
  swipeClose(el, m);
  m.refresh(true);
  $('layer').append(el);
  pushModal(m);
  return m;
}
export const findSheet = key => stack.find(m => m.key === key);
export function refreshSheets() { for (const m of stack) if (m.refresh) m.refresh(); }
function swipeClose(el, m) {
  let y0 = null;
  el.addEventListener('touchstart', e => { y0 = e.target.closest('.grab,.shead,.ahead') ? e.touches[0].clientY : null; }, { passive: true });
  el.addEventListener('touchmove', e => {
    if (y0 == null) return;
    const dy = e.touches[0].clientY - y0;
    el.style.transform = dy > 0 ? 'translateY(' + dy + 'px)' : '';
  }, { passive: true });
  el.addEventListener('touchend', e => {
    if (y0 == null) return;
    const dy = e.changedTouches[0].clientY - y0;
    y0 = null;
    el.style.transform = '';
    if (dy > 80) m.close();
  });
}
export function diffView(files, wrap) {
  return files.map(f => h('div', 'dv' + (wrap ? ' wrap' : ''),
    h('div', 'dvh', h('span', '', f.path || 'Changes'), f.added > 0 && h('span', 'add', '+' + f.added), f.removed > 0 && h('span', 'del', '−' + f.removed)),
    f.hunks.map(hk => {
      const nums = hk.lines.some(l => l.a != null || l.b != null);
      return h('div', 'hk', hk.head && h('div', 'hkh', hk.head), hk.lines.map(l => h('div', 'dl ' + (l.t === 'add' ? 'a' : l.t === 'del' ? 'd' : '') + (nums ? '' : ' nn'),
        nums && h('span', 'n', l.a == null ? '' : String(l.a)), nums && h('span', 'n', l.b == null ? '' : String(l.b)),
        !nums && h('span', 'sgn', l.t === 'add' ? '+' : l.t === 'del' ? '−' : ' '), h('span', 'c', l.s || ' '))));
    })));
}

export { $ };
