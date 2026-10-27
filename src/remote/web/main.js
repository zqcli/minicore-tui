import * as MC from './core.js';
import { syncApproval } from './approval.js';
import { closePriMenu, edits, openPlus, renderDock, returned, sent, stop, unconfirmedHome } from './composer.js';
import { HOST, KEY, S, bind, cur, hangUp, online, open, pump, socketGone, st } from './net.js';
import { notify, syncWake } from './platform.js';
import { buildNav, clearPending, closeNav, failed, openInfo, openLive, openNav, openNew, openSessionMenu, renderNav, renderNavFoot, settle, stepSession, syncTick } from './sessions.js';
import { modelSettled, openHelp } from './sheets.js';
import { dirty, followEnd, onNewSession, renderEmpty, renderTop, resetTranscript, schedule } from './transcript.js';
import { $, actions, announce, closeAll, fill, h, icon, inField, part, reduced, refreshSheets, spin, stack, toast, topModal } from './ui.js';

let halted = false, attempt = 0, retryT = 0, how = 'hello', hiddenAt = 0, wasBusy = false;
const teardown = () => { clearTimeout(retryT); hangUp(); };
function start(first, kind) { teardown(); how = kind; open(first); }
const recentAttn = new Map();

document.addEventListener('mouseover', e => { const b = e.target.closest('button[aria-label]'); if (b && !b.title) b.title = b.getAttribute('aria-label'); });
// ---- screens ----
let screen = '';
function show(name) {
  screen = name;
  $('signin').hidden = name !== 'signin';
  $('gate').hidden = name !== 'gate';
  $('app').hidden = name !== 'main';
  if (name !== 'main') closeAll();
}
function gateScreen(g) {
  const eicon = h('div', 'eicon' + (g.icon === 'warn' ? ' warn' : ''), icon(g.icon, 'lg'));
  fill($('gate'), h('div', 'col', g.spinner ? h('img', { class: 'appicon', src: '/icon.svg', alt: '' }) : eicon, h('h1', '', g.title), g.text && h('p', 'sub', g.text),
    g.spinner && spin(), g.action && h('div', 'gate-acts', h('button', { class: 'btn primary block', onclick: () => gateAction(g.action) }, g.label),
      g.signin && h('button', { class: 'btn ghost', onclick: signOut }, 'Sign in with a password'))));
  show('gate');
}
function gateAction(a) {
  if (a === 'reload') location.reload();
  else { halted = false; attempt = 0; connecting(); connect(); }
}
function connecting() {
  if (S.state) return show('main');
  gateScreen(attempt < 2
    ? { spinner: true, title: 'Connecting…', text: HOST }
    : { icon: 'cloudoff', title: MC.unreachable(HOST), text: 'Check that MiniCore is running with remote control on and that this device can reach your computer. Retrying…', action: 'retry', label: 'Retry now', signin: true });
}

// ---- sign-in ----
let lockUntil = 0, lockT = 0;
function signinScreen(o = {}) {
  show('signin');
  $('shost').textContent = HOST;
  const note = $('snote');
  note.hidden = !o.notice;
  if (o.notice) note.lastChild.textContent = o.notice;
  signinBusy(false);
  if (o.locked) lock(o.locked);
  else if (o.error) signinError(o.error, o);
  else if (!lockT) signinError('');
  const pw = $('pw');
  if (o.shake) { const f = $('pwf'); f.classList.remove('shake'); void f.offsetWidth; f.classList.add('shake'); }
  if (o.error || o.shake) { pw.focus(); pw.select(); } else if (!o.notice) pw.focus({ preventScroll: true });
}
function signinError(text, o = {}) {
  const e = $('serr');
  $('pwf').classList.toggle('bad', !!text && !o.warn && !o.retry);
  e.hidden = !text;
  e.className = 'ferr' + (o.warn ? ' warn' : '');
  fill(e, text && icon(o.warn ? 'info' : 'warn', 'sm'), h('span', '', text), o.retry && h('button', { onclick: doLogin }, 'Retry'));
}
function signinBusy(on) {
  const b = $('sgo');
  b.firstChild.hidden = !on;
  b.lastChild.textContent = on ? 'Connecting…' : 'Connect';
  b.disabled = on || Date.now() < lockUntil;
  $('pw').readOnly = on;
}
function lock(sec) {
  lockUntil = Date.now() + sec * 1000;
  clearInterval(lockT);
  const step = () => {
    const left = Math.ceil((lockUntil - Date.now()) / 1000);
    if (left <= 0) { clearInterval(lockT); lockT = 0; lockUntil = 0; signinError('You can try again now.', { warn: true }); signinBusy(false); return; }
    signinError(MC.lockText(left), { warn: true });
    $('sgo').disabled = true;
  };
  lockT = setInterval(step, 1000);
  step();
}
function doLogin() {
  const pw = $('pw').value;
  if (!pw.trim()) { signinError('Enter the password shown on your computer.'); $('pw').focus(); return; }
  if (Date.now() < lockUntil) return;
  signinError('');
  signinBusy(true);
  halted = false;
  start(() => MC.login(pw, MC.deviceName(navigator.userAgent, navigator.maxTouchPoints)), 'login');
}
$('sf').addEventListener('submit', e => { e.preventDefault(); doLogin(); });
$('pw').addEventListener('input', () => { if (!lockT) signinError(''); });
$('pweye').addEventListener('click', () => {
  const pw = $('pw'), b = $('pweye'), vis = pw.type === 'password';
  pw.type = vis ? 'text' : 'password';
  b.setAttribute('aria-pressed', vis);
  b.setAttribute('aria-label', vis ? 'Hide password' : 'Show password');
  b.firstChild.firstChild.setAttribute('href', vis ? '#i-eyeoff' : '#i-eye');
});
function signOut() {
  localStorage.removeItem(KEY);
  halted = true;
  teardown();
  resetSession();
  signinScreen({ notice: 'Signed out of this device. Enter the password to connect again.' });
}

function connect() {
  const token = localStorage.getItem(KEY);
  if (!token) return signinScreen();
  start(() => MC.hello(token), 'hello');
  renderConn();
}
function closed() {
  if (halted) return;
  if (how === 'login') { signinBusy(false); signinError(MC.unreachable(HOST), { retry: true }); return; }
  attempt++;
  if (!S.state) connecting();
  renderConn();
  retryT = setTimeout(connect, MC.backoff(attempt - 1));
}
function wake(force) {
  if (halted || screen === 'signin' || !localStorage.getItem(KEY)) return;
  if (force || socketGone()) { clearTimeout(retryT); attempt = 0; connect(); }
}
function onGate(g) {
  halted = true;
  teardown();
  if (g.forget) { localStorage.removeItem(KEY); resetSession(); }
  if (g.view === 'signin') signinScreen(g);
  else gateScreen(g);
}
function resetSession() {
  Object.assign(S, MC.createStore(), { nid: S.nid });
  resetTranscript();
}

function onMessage(msg, fx) {
  if (fx.token) { localStorage.setItem(KEY, fx.token); how = 'hello'; }
  if (fx.welcome) { attempt = 0; signinBusy(false); }
  if (fx.gate) return onGate(fx.gate);
  if (fx.snapshot && screen !== 'main') { show('main'); $('pw').value = ''; }
  if (fx.newSession) onNewSession();
  if (fx.dirty) { fx.dirty.forEach(i => dirty.add(i)); schedule(); }
  if (fx.state) renderState();
  if (fx.snapshot || fx.welcome) { renderConn(); pump(); }
  if (fx.ack) onAck(fx.ack);
  if (fx.returned != null) returned(fx.returned);
  if (fx.events) fx.events.forEach(attention);
  if (fx.notice) onNotice(fx.notice);
}
function onAck(a) {
  const k = a.kind;
  if ((k === 'submit' || k === 'queue') && a.ok) sent(a.text);
  if (k === 'unqueue' && !a.ok) edits.delete(a.spec.text);
  if (k === 'set_model' || k === 'set_reasoning') modelSettled(k);
  if (k === 'open_session' && settle(a.spec.session_id) && a.ok) closeNav();
  const own = a.spec.own;
  if (own) own(a);
  else if (!a.ok) toast(MC.errorText(a.error), a.error === 'stale' ? null : 'error');
  renderState();
}

function connectionLost() {
  unconfirmedHome();
  clearPending();
  modelSettled();
  if (S.state) renderState();
}

// ---- top bar and banners ----
function renderBar() {
  const s = S.state, c = cur(), badge = s.background.approval;
  $('title').textContent = s.session ? s.session.title || 'Untitled session' : 'New session';
  $('sub').textContent = [MC.basename(c.workspace), s.session && s.session.branch].filter(Boolean).join(' · ');
  const m = $('menu'), old = m.querySelector('.dotn');
  if (badge && (!old || old.textContent !== String(badge))) {
    const d = h('span', 'dotn' + (old && +old.textContent < badge ? ' pop' : ''), String(badge));
    if (old) old.replaceWith(d); else m.append(d);
  } else if (!badge && old) old.remove();
  m.setAttribute('aria-label', badge ? 'Sessions, ' + MC.plural(badge, 'session') + ' need you' : 'Sessions');
  $('moreb').disabled = !s.session;
}
const bannersPart = part($('banners'), b => [
  b.attn && h('div', { class: 'banner warn', role: 'status' }, icon('hand', 'sm'), h('span', '', '“' + b.attn.title + '” ' + (b.attn.kind === 'choice' ? 'has a question' : 'needs approval')),
    h('button', { class: 'btn sm', onclick: () => openLive(b.attn.id) }, 'Review'),
    h('button', { class: 'ib s', 'aria-label': 'Dismiss', onclick: () => { dismissed.add(b.attn.id); renderBanners(); } }, icon('x', 'sm'))),
  b.conn && h('div', { class: 'banner', role: 'status' }, spin(), h('span', '', b.conn)),
]);
const dismissed = new Set();
function renderBanners() {
  const s = S.state, x = s.sessions.find(y => y.live && !y.current && y.status === 'approval' && !dismissed.has(y.id));
  bannersPart({
    attn: x ? { id: x.id, title: x.title || 'Untitled session' } : null,
    conn: online() || halted ? '' : navigator.onLine === false ? 'Offline · waiting for network' : 'Reconnecting…',
  });
}
function renderConn() {
  if (screen !== 'main' || !S.state) return;
  renderBanners();
  renderNavFoot();
  renderDock();
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

// ---- attention: badge, title, notices ----
let badgeN = -1;
function renderAttention() {
  const s = S.state, n = screen === 'main' && s ? MC.attentionCount(s) : 0;
  const t = s && s.session ? s.session.title || 'Untitled session' : 'MiniCore';
  document.title = (n ? '(' + n + ') ' : '') + (t === 'MiniCore' ? t : t + ' · MiniCore');
  if (n !== badgeN && navigator.setAppBadge) { badgeN = n; (n ? navigator.setAppBadge(n) : navigator.clearAppBadge()).catch(() => {}); }
}
function attention(e) {
  const label = e.title || 'Untitled session';
  recentAttn.set(label, Date.now());
  dismissed.delete(e.id);
  if (e.kind === 'finished') toast('“' + label + '” finished', null, { label: 'Open', fn: () => openLive(e.id) });
  else announce('“' + label + '” needs approval');
  notify(e.kind === 'finished' ? '“' + label + '” finished' : '“' + label + '” needs approval', e.id);
}
function onNotice(n) {
  const ev = MC.noticeEvent(n.text), seen = () => ev && Date.now() - (recentAttn.get(ev.label) || 0) < 5000;
  if (ev && ev.kind === 'failed') { const x = S.state.sessions.find(y => y.title === ev.label && !y.current); if (x) { failed.add(x.id); renderNav(); } }
  if (!ev) return toast(n.text, n.level);
  // The state carrying the same event can arrive just after its notice.
  if (!seen()) setTimeout(() => { if (!seen()) toast(n.text, n.level); }, 1500);
}
if ('serviceWorker' in navigator) navigator.serviceWorker.addEventListener('message', e => { const id = e.data && e.data.open; if (id && S.state && id !== S.sid) openLive(id); });

function renderState() {
  if (!S.state || screen !== 'main') return;
  const s = S.state;
  for (const id of failed) if (s.sessions.some(x => x.id === id && x.current)) failed.delete(id);
  renderBar(); renderBanners(); renderNav(); renderDock(); syncApproval();
  if (!!s.status.busy !== wasBusy) {
    wasBusy = !!s.status.busy;
    announce(wasBusy ? 'Working' : 'Turn ended');
    if (!wasBusy && s.session) notify('“' + (s.session.title || 'Untitled session') + '” finished', s.session.id);
    schedule();
  }
  syncWake();
  renderTop(); renderEmpty(); refreshSheets(); renderAttention(); syncTick();
}

// ---- keys ----
document.addEventListener('keydown', e => {
  if (screen !== 'main') return;
  const m = topModal();
  if (e.key === 'Escape') {
    if ($('pmenu')) { e.preventDefault(); closePriMenu(); return; }
    if (m) { e.preventDefault(); m.close(); return; }
    const p = stack.find(x => x.panel);
    if (p) { e.preventDefault(); p.close(); return; }
    if (MC.canStop(st())) { e.preventDefault(); stop(); }
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
addEventListener('online', () => { renderConn(); wake(false); });
addEventListener('offline', renderConn);
if ('serviceWorker' in navigator && isSecureContext) navigator.serviceWorker.register('/sw.js');
if (window.visualViewport) {
  // iOS keeps the layout viewport under the keyboard; sheets are lifted by the part it covers.
  const fit = () => {
    if (window.scrollY) scrollTo(0, 0);
    const root = document.documentElement, kb = Math.max(0, Math.round(innerHeight - visualViewport.height - visualViewport.offsetTop));
    root.style.setProperty('--h', visualViewport.height + 'px');
    root.style.setProperty('--kb', kb + 'px');
    root.classList.toggle('kb', kb > 80);
    followEnd();
  };
  visualViewport.addEventListener('resize', fit);
  visualViewport.addEventListener('scroll', fit);
  fit();
}
bind({ message: onMessage, lost: connectionLost, closed, render: renderState });
buildNav();
if (localStorage.getItem(KEY)) { connecting(); connect(); } else signinScreen();
actions.signOut = signOut;
