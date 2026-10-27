import * as MC from './core.js';
import { sessionEvent, onNotice, renderAttention, renderBanners } from './attention.js';
import { syncApproval } from './approval.js';
import { closePriMenu, openPlus, renderDock, returned, sent, stop, unconfirmedHome, unqueueRefused } from './composer.js';
import { boot, closed, handshake, isMain, wake } from './connection.js';
import { S, bind, pump, repaint, turnStatus, viewed } from './net.js';
import { notify, syncWake } from './platform.js';
import { buildNav, clearFailed, clearPending, closeNav, openInfo, openLive, openNav, openNew, openSessionMenu, renderNav, settle, stepSession, syncTick } from './sessions.js';
import { modelSettled, openHelp } from './sheets.js';
import { followEnd, markDirty, onNewSession, renderEmpty, renderTop, schedule } from './transcript.js';
import { $, actions, announce, h, inField, reduced, refreshSheets, set, toast, topModal, topPanel } from './ui.js';

let hiddenAt = 0, wasBusy = false;

document.addEventListener('mouseover', e => { const b = e.target.closest('button[aria-label]'); if (b && !b.title) b.title = b.getAttribute('aria-label'); });

function onMessage(msg, fx) {
  if (!handshake(fx)) return;
  if (fx.newSession) onNewSession();
  if (fx.dirty) markDirty(fx.dirty);
  if (fx.state || fx.welcome) renderState();
  if (fx.snapshot || fx.welcome) pump();
  if (fx.ack) onAck(fx.ack);
  if (fx.returned != null) returned(fx.returned);
  if (fx.events) fx.events.forEach(sessionEvent);
  if (fx.notice) onNotice(fx.notice);
}
function onAck(a) {
  const k = a.kind;
  if ((k === 'submit' || k === 'queue') && a.ok) sent(a.text);
  if (k === 'unqueue' && !a.ok) unqueueRefused(a.spec.text);
  if (k === 'set_model' || k === 'set_reasoning') modelSettled(k);
  if (k === 'open_session' && settle(a.spec.session_id) && a.ok) closeNav();
  if (a.own) a.own(a);
  else if (!a.ok) toast(MC.errorText(a.error), a.error === 'stale' ? null : 'error');
  renderState();
}

function connectionLost() {
  unconfirmedHome();
  clearPending();
  modelSettled();
  if (S.state) renderState();
}

// ---- top bar ----
function renderBar() {
  const s = S.state, c = viewed(), badge = s.background.approval;
  set($('title'), 'textContent', s.session ? MC.titled(s.session.title) : 'New session');
  set($('sub'), 'textContent', [MC.basename(c.workspace), s.session && s.session.branch].filter(Boolean).join(' · '));
  const m = $('menu'), old = m.querySelector('.dotn');
  if (badge && (!old || old.textContent !== String(badge))) {
    const d = h('span', 'dotn' + (old && +old.textContent < badge ? ' pop' : ''), String(badge));
    if (old) old.replaceWith(d); else m.append(d);
  } else if (!badge && old) old.remove();
  m.setAttribute('aria-label', badge ? 'Sessions, ' + MC.plural(badge, 'session') + ' need you' : 'Sessions');
  set($('moreb'), 'disabled', !s.session);
}
$('log').addEventListener('scroll', () => $('bar').classList.toggle('edge', $('log').scrollTop > 4), { passive: true });
$('title').addEventListener('click', () => {
  // A tap while scrolled away from the top scrolls up; at the top it opens the session details.
  const log = $('log');
  if (log.scrollTop > 40) log.scrollTo({ top: 0, behavior: reduced.matches ? 'auto' : 'smooth' });
  else openInfo();
});
$('title').addEventListener('dblclick', () => $('log').scrollTo({ top: 0 }));
$('menu').addEventListener('click', openNav);
$('newb').addEventListener('click', () => openNew());
$('moreb').addEventListener('click', openSessionMenu);

if ('serviceWorker' in navigator) navigator.serviceWorker.addEventListener('message', e => { const id = e.data && e.data.open; if (id && S.state && id !== S.sid) openLive(id); });

function renderState() {
  if (!S.state || !isMain()) return;
  const s = S.state;
  clearFailed();
  renderBar(); renderBanners(); renderNav(); renderDock(); syncApproval();
  if (!!s.status.busy !== wasBusy) {
    wasBusy = !!s.status.busy;
    announce(wasBusy ? 'Working' : 'Turn ended');
    if (!wasBusy && s.session) notify('“' + MC.titled(s.session.title) + '” finished', s.session.id);
    schedule();
  }
  syncWake();
  renderTop(); renderEmpty(); refreshSheets(); renderAttention(); syncTick();
}

// ---- keys ----
document.addEventListener('keydown', e => {
  if (!isMain()) return;
  const m = topModal();
  if (e.key === 'Escape') {
    if ($('pmenu')) { e.preventDefault(); closePriMenu(); return; }
    if (m) { e.preventDefault(); m.close(); return; }
    const p = topPanel();
    if (p) { e.preventDefault(); p.close(); return; }
    if (MC.canStop(turnStatus())) { e.preventDefault(); stop(); }
    return;
  }
  const mod = e.metaKey || e.ctrlKey, k = e.key.toLowerCase();
  if (!m && mod && k === 'k') { e.preventDefault(); openPlus(); return; }
  if (!m && mod && k === 'f' && S.state.session) { e.preventDefault(); actions.find(); return; }
  if (!m && e.altKey && (k === 'arrowup' || k === 'arrowdown')) { e.preventDefault(); stepSession(k === 'arrowup' ? -1 : 1); return; }
  if (!m && e.key === '?' && !inField(e.target)) { e.preventDefault(); openHelp(); return; }
  if (!m || e.key !== 'Tab') return;
  const f = [...m.el.querySelectorAll('button,input,textarea,a[href],[tabindex="0"]')].filter(x => !x.disabled && x.offsetParent);
  if (!f.length) return;
  const first = f[0], last = f[f.length - 1];
  if (e.shiftKey && (document.activeElement === first || document.activeElement === m.el)) { e.preventDefault(); last.focus(); }
  else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
});

// ---- lifecycle ----
document.addEventListener('visibilitychange', () => { if (document.hidden) hiddenAt = Date.now(); else wake(Date.now() - hiddenAt > 10000); syncWake(); });
// A page restored from the back/forward cache lost its socket.
addEventListener('pageshow', e => { if (e.persisted) wake(true); });
addEventListener('focus', () => wake(false));
addEventListener('online', () => { repaint(); wake(false); });
addEventListener('offline', repaint);
if ('serviceWorker' in navigator && isSecureContext) navigator.serviceWorker.register('/sw.js');
if (window.visualViewport) {
  // iOS keeps the layout viewport under the keyboard; sheets are lifted by the part it covers.
  // Panning fires scroll events without a change, and every write to these properties restyles the whole page.
  let height = -1, lifted = -1;
  const fit = () => {
    if (window.scrollY) scrollTo(0, 0);
    const root = document.documentElement, kb = Math.max(0, Math.round(innerHeight - visualViewport.height - visualViewport.offsetTop));
    if (visualViewport.height !== height) root.style.setProperty('--h', (height = visualViewport.height) + 'px');
    if (kb !== lifted) { root.style.setProperty('--kb', (lifted = kb) + 'px'); root.classList.toggle('kb', kb > 80); }
    followEnd();
  };
  visualViewport.addEventListener('resize', fit);
  visualViewport.addEventListener('scroll', fit);
  fit();
}
bind({ message: onMessage, lost: connectionLost, closed, render: renderState });
buildNav();
boot();
