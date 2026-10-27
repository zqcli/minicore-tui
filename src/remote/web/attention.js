import * as MC from './core.js';
import { S, online } from './net.js';
import { notify } from './platform.js';
import { isHalted, isMain } from './connection.js';
import { markFailed, openLive } from './sessions.js';
import { $, announce, h, icon, part, set, spin, toast } from './ui.js';

// Banners, the app badge and tab title, and what a background session asks of this device.
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